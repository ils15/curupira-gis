//! Leitor CSV-XY puro-Rust (feature `csv`) — pontos a partir de colunas X/Y.
//!
//! - **Sniff de delimitador**: `,` `;` `\t` — vence o mais frequente nas
//!   primeiras linhas (empate → ordem `,` → `;` → `\t`).
//! - **Encoding**: UTF-8 ou Latin-1 (byte→char 1:1, comum em bases de
//!   legado Windows-1252 puras).
//! - Cada linha vira `Point(lon, lat)`; SRID **4326** por padrão ou o
//!   `--srid` informado (não há CRS embutido em CSV).
//! - Erros de parse carregam **linha e coluna** para o usuário corrigir a
//!   origem — nada de pular linha silenciosamente.
//!
//! Demais colunas viram atributos JSON com inferência leve de tipo
//! (`int` → número, `float` → número, vazio → `null`, resto → string).

use std::io::Cursor;
use std::path::Path;

use geo_types::Point;
use serde_json::{Value, json};

use ta_core::feature::Feature;

use crate::error::{Error, Result};
use crate::reader::{CsvEncoding, CsvXyOptions, VectorData};

const DEFAULT_LAT_COL: &str = "lat";
const DEFAULT_LON_COL: &str = "lon";
const DEFAULT_SRID: u32 = 4326;

/// Lê um CSV-XY e devolve as feições ponto + SRID reportado.
pub fn read_csv_xy(path: &Path, opts: &CsvXyOptions) -> Result<VectorData> {
    let raw = std::fs::read(path)?;
    let text = decode(&raw, opts.encoding)?;
    let sep = match opts.sep {
        Some(c) => c,
        None => sniff_delimiter(&text).unwrap_or(','),
    };
    let lon_col = opts.lon_col.as_deref().unwrap_or(DEFAULT_LON_COL);
    let lat_col = opts.lat_col.as_deref().unwrap_or(DEFAULT_LAT_COL);
    let srid = opts.srid.unwrap_or(DEFAULT_SRID);

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(sep as u8)
        .flexible(false)
        .from_reader(Cursor::new(text.as_bytes()));

    let headers = reader.headers()?.clone();
    let lon_idx = column_index(&headers, lon_col)?;
    let lat_idx = column_index(&headers, lat_col)?;

    let mut features = Vec::new();
    for record in reader.records() {
        let record = record?;
        let line = record.position().map(|p| p.line()).unwrap_or(0);
        let lon = parse_f64(&record, lon_idx, line, &headers[lon_idx])?;
        let lat = parse_f64(&record, lat_idx, line, &headers[lat_idx])?;
        if !lon.is_finite() || !lat.is_finite() {
            return Err(Error::CsvCell {
                line,
                column: format!("{lon_col}/{lat_col}"),
                message: format!("coordenada não finita: ({lon}, {lat})"),
            });
        }

        let mut attrs = serde_json::Map::new();
        for (i, header) in headers.iter().enumerate() {
            if i == lon_idx || i == lat_idx {
                continue;
            }
            let raw_value = record.get(i).unwrap_or_default();
            attrs.insert(header.to_string(), infer_value(raw_value));
        }
        features.push(Feature {
            geom: Point::new(lon, lat).into(),
            attrs: Value::Object(attrs),
        });
    }

    let layer_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("csv")
        .to_string();

    Ok(VectorData {
        features,
        srid: Some(srid),
        crs_wkt: None,
        layer_name,
    })
}

/// Decodifica bytes para UTF-8; Latin-1 mapeia byte→char 1:1 (nunca falha).
fn decode(raw: &[u8], encoding: CsvEncoding) -> Result<String> {
    match encoding {
        CsvEncoding::Utf8 => String::from_utf8(raw.to_vec()).map_err(|e| Error::CsvCell {
            line: 0,
            column: "(arquivo)".into(),
            message: format!("não é UTF-8 válido: {e}; tente --encoding latin1"),
        }),
        CsvEncoding::Latin1 => Ok(raw.iter().map(|&b| b as char).collect()),
    }
}

/// Sniff do delimitador: conta candidatos nas até 5 primeiras linhas não
/// vazias, fora de aspas; vence o de maior contagem total
/// (empate → ordem `,` → `;` → `\t`).
pub(crate) fn sniff_delimiter(text: &str) -> Option<char> {
    const CANDIDATES: [char; 3] = [',', ';', '\t'];
    let mut counts = [0usize; 3];
    let mut lines = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut quoted = false;
        for ch in trimmed.chars() {
            match ch {
                '"' => quoted = !quoted,
                c if !quoted => {
                    if let Some(i) = CANDIDATES.iter().position(|&x| x == c) {
                        counts[i] += 1;
                    }
                }
                _ => {}
            }
        }
        lines += 1;
        if lines >= 5 {
            break;
        }
    }
    let best = counts
        .iter()
        .enumerate()
        .max_by_key(|(i, c)| (**c, std::cmp::Reverse(*i)))
        .map(|(i, _)| i);
    best.filter(|&i| counts[i] > 0).map(|i| CANDIDATES[i])
}

fn column_index(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
        .ok_or_else(|| Error::CsvCell {
            line: 1,
            column: name.to_string(),
            message: format!(
                "coluna `{name}` não encontrada; cabeçalho: [{}]",
                headers.iter().collect::<Vec<_>>().join(", ")
            ),
        })
}

fn parse_f64(record: &csv::StringRecord, idx: usize, line: u64, col: &str) -> Result<f64> {
    let raw = record.get(idx).unwrap_or_default().trim();
    raw.parse::<f64>().map_err(|_| Error::CsvCell {
        line,
        column: col.to_string(),
        message: format!("`{raw}` não é número"),
    })
}

/// Inferência leve: int → float → null (vazio) → string.
fn infer_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if f.is_finite() {
            return json!(f);
        }
    }
    json!(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_csv(content: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("ta-gis-csv-{}-{n}.csv", std::process::id()));
        std::fs::write(&p, content).expect("temp csv");
        p
    }

    #[test]
    fn sniff_esperto_entre_virgula_ponto_virgula_e_tab() {
        assert_eq!(sniff_delimiter("a,b,c\n1,2,3\n4,5,6"), Some(','));
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3\n4;5;6"), Some(';'));
        assert_eq!(sniff_delimiter("a\tb\tc\n1\t2\t3\n4\t5\t6"), Some('\t'));
        // Vírgula dentro de texto não vence o `;` dominante.
        assert_eq!(
            sniff_delimiter("nome;nota\n\"silva, joao\";9\npereira;7"),
            Some(';')
        );
        assert_eq!(sniff_delimiter("apenas texto"), None);
    }

    #[test]
    fn le_csv_virgula_default_4326() {
        let p = temp_csv("id,lon,lat,nome\n1,-51.9,-12.4,cuiaba\n2,-48.5,-1.4,belem\n");
        let data = read_csv_xy(&p, &CsvXyOptions::default()).expect("ler");
        assert_eq!(data.srid, Some(4326));
        assert_eq!(data.features.len(), 2);
        let f = &data.features[0];
        assert_eq!(f.geom, geo_types::Point::new(-51.9, -12.4).into());
        assert_eq!(f.attrs["id"], json!(1)); // inferido como int
        assert_eq!(f.attrs["nome"], json!("cuiaba"));
        assert!(f.attrs.get("lon").is_none() && f.attrs.get("lat").is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sniff_ponto_virgula_e_colunas_custom() {
        let p = temp_csv("x;y;valor\n-45.1;-10.2;3\n-45.2;-10.3;4\n");
        let opts = CsvXyOptions {
            lon_col: Some("x".into()),
            lat_col: Some("y".into()),
            ..Default::default()
        };
        let data = read_csv_xy(&p, &opts).expect("ler");
        assert_eq!(
            data.features[0].geom,
            geo_types::Point::new(-45.1, -10.2).into()
        );
        assert_eq!(data.features[0].attrs["valor"], json!(3));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn latin1_decodifica_acentos() {
        // "São Paulo" em Latin-1: ã = 0xE3.
        let mut bytes = b"id;lon;lat;cidade\n1;-46.6;-23.5;S".to_vec();
        bytes.push(0xE3);
        bytes.extend_from_slice(b"o Paulo\n");
        let p = std::env::temp_dir().join(format!("ta-gis-latin1-{}.csv", std::process::id()));
        std::fs::write(&p, &bytes).expect("temp");
        let opts = CsvXyOptions {
            encoding: CsvEncoding::Latin1,
            sep: Some(';'),
            ..Default::default()
        };
        let data = read_csv_xy(&p, &opts).expect("ler latin1");
        assert_eq!(data.features[0].attrs["cidade"], json!("São Paulo"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn erro_de_parse_tem_linha_e_coluna() {
        let p = temp_csv("lon,lat\n-45.0,-10.0\nfoo,-11.0\n");
        let err = read_csv_xy(&p, &CsvXyOptions::default()).unwrap_err();
        match err {
            Error::CsvCell {
                line,
                column,
                message,
            } => {
                assert_eq!(line, 3, "linha 3 = header + 2 linhas de dados");
                assert_eq!(column, "lon");
                assert!(message.contains("foo"), "mensagem: {message}");
            }
            other => panic!("erro inesperado: {other}"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn coluna_ausente_lista_cabecalho() {
        let p = temp_csv("longitude,latitude\n-45.0,-10.0\n");
        let err = read_csv_xy(&p, &CsvXyOptions::default()).unwrap_err();
        assert!(
            matches!(err, Error::CsvCell { ref column, ref message, .. }
                if column == "lon" && message.contains("longitude")),
            "erro: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn srid_customizada_e_respeitada() {
        let p = temp_csv("lon,lat\n-45.0,-10.0\n");
        let opts = CsvXyOptions {
            srid: Some(4674),
            ..Default::default()
        };
        let data = read_csv_xy(&p, &opts).expect("ler");
        assert_eq!(data.srid, Some(4674));
        let _ = std::fs::remove_file(&p);
    }
}
