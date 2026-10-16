//! Detecção de formato de origem por **extensão e magic bytes**.
//!
//! A detecção é a porta de entrada do pipeline de import
//! ([`crate::normalizer::prepare_import`]) e é deliberadamente estrita:
//! formatos fora do MVP (KML, NetCDF, DWG, …) falham com
//! [`Error::UnsupportedFormat`] tipado — nunca com um erro genérico de
//! parse no meio da leitura.

use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};

/// Formato vetorial de origem suportado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    /// ESRI Shapefile (+ .shx/.dbf/.prj).
    Shapefile,
    /// GeoJSON (RFC 7946).
    GeoJson,
    /// GeoPackage (SQLite + extensões GPKG).
    GeoPackage,
    /// FlatGeobuf (colunar, streaming).
    FlatGeobuf,
    /// CSV com colunas X/Y configuráveis (não é OGR).
    CsvXy,
}

impl SourceFormat {
    /// Nome legível do formato (mensagens de erro/relatórios).
    pub fn name(self) -> &'static str {
        match self {
            SourceFormat::Shapefile => "Shapefile",
            SourceFormat::GeoJson => "GeoJSON",
            SourceFormat::GeoPackage => "GeoPackage",
            SourceFormat::FlatGeobuf => "FlatGeobuf",
            SourceFormat::CsvXy => "CSV-XY",
        }
    }
}

/// Detecta o formato vetorial pela extensão **e** magic bytes.
///
/// Ordem de decisão:
/// 1. magic bytes inequívocos (SHP, SQLite/GPKG, HDF5/NetCDF, DWG, KML);
/// 2. extensão do arquivo;
/// 3. sem correspondência → [`Error::UnsupportedFormat`].
pub fn detect_source_format(path: &Path) -> Result<SourceFormat> {
    if let Some(fmt) = sniff_magic(path)? {
        return Ok(fmt);
    }
    from_extension(path)
}

/// Lê os primeiros bytes e deduz o formato quando o magic é inequívoco.
/// KML/NetCDF/DWG são reconhecidos e **rejeitados na hora** com
/// [`Error::UnsupportedFormat`] (mensagem explica que não há suporte).
fn sniff_magic(path: &Path) -> Result<Option<SourceFormat>> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        Error::UnsupportedFormat(format!(
            "não foi possível abrir {} para detecção de formato: {e}",
            path.display()
        ))
    })?;
    let mut head = [0u8; 16];
    let n = read_prefix(&mut file, &mut head)?;
    if n < 4 {
        return Ok(None);
    }
    let head = &head[..n];

    // ESRI Shapefile: file code 9994 big-endian = 00 00 27 0A.
    if head[0..4] == [0x00, 0x00, 0x27, 0x0A] {
        return Ok(Some(SourceFormat::Shapefile));
    }
    // SQLite (GeoPackage): "SQLite format 3\0".
    if head.starts_with(b"SQLite format 3") {
        return Ok(Some(SourceFormat::GeoPackage));
    }
    // HDF5 (NetCDF-4/HDF5): \x89HDF\r\n\x1a\n — raster científico sem
    // suporte no MVP.
    if head.starts_with(&[0x89, b'H', b'D', b'F', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Err(unsupported_raster_hdf(path));
    }
    // NetCDF classic (CDF\x01 / CDF\x02).
    if head.starts_with(b"CDF\x01") || head.starts_with(b"CDF\x02") {
        return Err(unsupported_netcdf(path));
    }
    // DWG: versões iniciam com "AC10…" (ASCII).
    if &head[0..4] == b"AC10" {
        return Err(Error::UnsupportedFormat(format!(
            "DWG ({}): CAD binário sem suporte no MVP; converta para \
             Shapefile/GPKG (ex.: ODA File Converter) e reimporte",
            path.display()
        )));
    }
    // KML é XML — sniff do conteúdo inicial.
    if looks_like_kml(head) {
        return Err(Error::UnsupportedFormat(format!(
            "KML ({}): sem suporte no MVP; converta para GeoJSON \
             (ex.: `ogr2ogr -f GeoJSON saida.geojson entrada.kml`) e reimporte",
            path.display()
        )));
    }
    Ok(None)
}

fn read_prefix(file: &mut std::fs::File, buf: &mut [u8; 16]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// XML inicial que contenha a tag `<kml` (case-insensitive) nos primeiros
/// bytes lidos é tratado como KML. Declaração XML sozinha NÃO é KML (GML e
/// metadados também começam com `<?xml`) — só a tag `<kml` decide.
fn looks_like_kml(head: &[u8]) -> bool {
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    lower.windows(4).any(|w| w == b"<kml")
}

fn unsupported_raster_hdf(path: &Path) -> Error {
    Error::UnsupportedFormat(format!(
        "HDF5/NetCDF-4 ({}): raster científico sem suporte no MVP; \
         converta para GeoTIFF/COG (ex.: `gdal_translate -of COG`) e use \
         import-raster",
        path.display()
    ))
}

fn unsupported_netcdf(path: &Path) -> Error {
    Error::UnsupportedFormat(format!(
        "NetCDF classic ({}): sem suporte no MVP; converta para GeoTIFF/COG \
         e use import-raster",
        path.display()
    ))
}

/// Fallback por extensão (lowercase).
fn from_extension(path: &Path) -> Result<SourceFormat> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "shp" => Ok(SourceFormat::Shapefile),
        "geojson" | "json" => Ok(SourceFormat::GeoJson),
        "gpkg" => Ok(SourceFormat::GeoPackage),
        "fgb" => Ok(SourceFormat::FlatGeobuf),
        "csv" | "txt" => Ok(SourceFormat::CsvXy),
        // Raster conhecido — erro com direção clara para o caminho certo.
        "tif" | "tiff" | "cog" => Err(Error::UnsupportedFormat(format!(
            "GeoTIFF/COG ({}): é raster — use `ta-gis::reader::read_raster_meta` \
             (CLI: `ta-cli import-raster`)",
            path.display()
        ))),
        "kml" | "kmz" => Err(Error::UnsupportedFormat(format!(
            "KML/KMZ ({}): sem suporte no MVP; converta para GeoJSON e reimporte",
            path.display()
        ))),
        "nc" | "nc4" | "netcdf" | "hdf" | "h5" => Err(unsupported_netcdf(path)),
        "dwg" | "dgn" => Err(Error::UnsupportedFormat(format!(
            "CAD ({}): DWG/DGN sem suporte no MVP; converta para Shapefile/GPKG",
            path.display()
        ))),
        other => Err(Error::UnsupportedFormat(format!(
            "extensão `.{other}` sem formato correspondente ({})",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Arquivo temporário único (sem dependência de crate de testes).
    fn temp_with(bytes: &[u8], ext: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let p = dir.join(format!("ta-gis-detect-{}-{n}.{ext}", std::process::id()));
        std::fs::write(&p, bytes).expect("escrever temp");
        p
    }

    #[test]
    fn magic_shapefile_vence_extensao() {
        let p = temp_with(&[0x00, 0x00, 0x27, 0x0A, 0x00, 0x00, 0x00, 0x00], "bin");
        assert_eq!(detect_source_format(&p).unwrap(), SourceFormat::Shapefile);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn magic_sqlite_e_gpkg() {
        let mut bytes = b"SQLite format 3\0".to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 16));
        let p = temp_with(&bytes, "bin");
        assert_eq!(detect_source_format(&p).unwrap(), SourceFormat::GeoPackage);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn kml_por_magic_e_rejeitado_tipado() {
        let mut bytes = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<kml xmlns=\"http://www.opengis.net/kml/2.2\">".to_vec();
        bytes.extend(std::iter::repeat_n(b' ', 32));
        let p = temp_with(&bytes, "kml");
        let err = detect_source_format(&p).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("KML")),
            "erro errado: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn netcdf_classic_e_rejeitado_tipado() {
        let p = temp_with(b"CDF\x01\x00\x00\x00\x02", "nc");
        let err = detect_source_format(&p).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("NetCDF")),
            "erro errado: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn hdf5_e_rejeitado_tipado() {
        let p = temp_with(&[0x89, b'H', b'D', b'F', 0x0D, 0x0A, 0x1A, 0x0A], "nc4");
        let err = detect_source_format(&p).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("HDF5")),
            "erro errado: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dwg_e_rejeitado_tipado() {
        let p = temp_with(b"AC1032xxxx", "dwg");
        let err = detect_source_format(&p).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("DWG")),
            "erro errado: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extensoes_conhecidas() {
        let dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        for (ext, expect) in [
            ("shp", SourceFormat::Shapefile),
            ("geojson", SourceFormat::GeoJson),
            ("json", SourceFormat::GeoJson),
            ("gpkg", SourceFormat::GeoPackage),
            ("fgb", SourceFormat::FlatGeobuf),
            ("csv", SourceFormat::CsvXy),
        ] {
            let p = dir.join(format!("ta-gis-ext-{n}.{ext}"));
            std::fs::write(&p, b"qualquer coisa").expect("temp");
            assert_eq!(detect_source_format(&p).unwrap(), expect, "ext {ext}");
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn tif_aponta_para_o_caminho_raster() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("ta-gis-tif-{n}.tif"));
        std::fs::write(&p, b"II*\0").expect("temp");
        let err = detect_source_format(&p).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("import-raster")),
            "erro errado: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extensao_desconhecida_e_erro_tipado() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("ta-gis-x-{n}.xyz"));
        std::fs::write(&p, b"\x01\x02\x03\x04").expect("temp");
        assert!(matches!(
            detect_source_format(&p),
            Err(Error::UnsupportedFormat(_))
        ));
        let _ = std::fs::remove_file(&p);
    }
}
