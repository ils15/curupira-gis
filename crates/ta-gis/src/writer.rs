//! Escrita vetorial a partir do modelo de domínio.
//!
//! Formatos e motores:
//! | Formato       | Motor                    | Feature        |
//! |---------------|--------------------------|----------------|
//! | Shapefile     | OGR (`gdal`)             | `gdal`         |
//! | GeoPackage    | OGR (`gdal`)             | `gdal`         |
//! | GeoJSON       | puro (`geojson`+serde)   | `geojson`      |
//! | FlatGeobuf    | puro (`flatgeobuf`)      | `flatgeobuf`   |
//! | CSV-XY        | puro (`csv`)             | `csv`          |
//!
//! Pré-condição: as features já estão no SRID-alvo e **válidas** (rodar
//! [`crate::normalizer::normalize`] antes é responsabilidade do chamador —
//! a escrita não repropara silenciosamente).

use std::path::Path;

use serde_json::Value;
use ta_core::feature::Feature;
use ta_core::project::DEFAULT_SRID;

use crate::error::{Error, Result};

/// Formatos de escrita vetorial suportados no MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteFormat {
    GeoJson,
    GeoPackage,
    FlatGeobuf,
    CsvXy,
    Shapefile,
}

impl WriteFormat {
    /// Parse do flag `--format` da CLI.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "geojson" | "json" => Some(Self::GeoJson),
            "gpkg" | "geopackage" => Some(Self::GeoPackage),
            "fgb" | "flatgeobuf" => Some(Self::FlatGeobuf),
            "csv" | "csvxy" => Some(Self::CsvXy),
            "shp" | "shapefile" => Some(Self::Shapefile),
            _ => None,
        }
    }

    /// Nome canônico (relatórios/erros).
    pub fn name(self) -> &'static str {
        match self {
            Self::GeoJson => "GeoJSON",
            Self::GeoPackage => "GeoPackage",
            Self::FlatGeobuf => "FlatGeobuf",
            Self::CsvXy => "CSV-XY",
            Self::Shapefile => "Shapefile",
        }
    }
}

/// Opções de escrita vetorial.
#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// SRID alvo do arquivo de saída (padrão: 4674). Reprojeção para SRID
    /// ≠ 4674 exige a feature `gdal` (OGR/PROJ).
    pub target_srid: u32,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            target_srid: DEFAULT_SRID,
        }
    }
}

/// Escreve features em `path` no formato pedido. Devolve o número de
/// features escritas.
pub fn write_vector(
    features: &[Feature],
    path: &Path,
    format: WriteFormat,
    opts: &WriteOptions,
) -> Result<usize> {
    if features.is_empty() {
        return Err(Error::EmptyFeatureSet);
    }
    let features = ensure_srid(features, opts.target_srid)?;

    match format {
        WriteFormat::Shapefile | WriteFormat::GeoPackage => {
            #[cfg(feature = "gdal")]
            return crate::ogr_writer::ogr_write(&features, path, format, opts)
                .map(|_| features.len());
            #[cfg(not(feature = "gdal"))]
            Err(unsupported_feature(format, "gdal"))
        }
        WriteFormat::GeoJson => {
            #[cfg(feature = "geojson")]
            return geojson_write(&features, path).map(|_| features.len());
            #[cfg(not(feature = "geojson"))]
            Err(unsupported_feature(format, "geojson"))
        }
        WriteFormat::FlatGeobuf => {
            let mixed = crate::normalizer::dominant_geom_type(&features)
                .is_none_or(|t| t == ta_core::layer::GeomType::GeometryCollection);
            // Lote misto: o FgbWriter 6.x trava o tipo da feature no primeiro
            // visto (sticky `type_` sob Unknown) → fallback driver OGR "FlatGeobuf"
            // (desvio registrado em STATUS.md; mesma limitação da crate).
            #[cfg(feature = "gdal")]
            if mixed {
                return crate::ogr_writer::ogr_write_driver(
                    &features,
                    path,
                    "FlatGeobuf",
                    &crate::ogr_writer::layer_name_for(path, WriteFormat::GeoPackage),
                    opts.target_srid,
                )
                .map(|_| features.len());
            }
            #[cfg(feature = "flatgeobuf")]
            return crate::fgb_writer::fgb_write(&features, path, opts.target_srid)
                .map(|_| features.len());
            #[cfg(not(all(feature = "flatgeobuf", feature = "gdal")))]
            {
                if mixed {
                    return Err(Error::Write(
                        "lote misto em FlatGeobuf requer as features `flatgeobuf`                          e `gdal` (fallback OGR); lotes homogêneos funcionam só                          com `flatgeobuf`"
                            .into(),
                    ));
                }
                Err(unsupported_feature(format, "flatgeobuf"))
            }
        }
        WriteFormat::CsvXy => {
            #[cfg(feature = "csv")]
            return csv_write(&features, path).map(|_| features.len());
            #[cfg(not(feature = "csv"))]
            Err(unsupported_feature(format, "csv"))
        }
    }
}

// Só é alcançado quando alguma feature de escrita está desligada.
#[allow(dead_code)]
fn unsupported_feature(format: WriteFormat, feature: &str) -> Error {
    Error::UnsupportedFormat(format!(
        "escrita {} requer a feature `{feature}` deste crate: recompile com \
         `--features {feature}`",
        format.name()
    ))
}

/// SRID-alvo ≠ 4674: reprojeção via OGR (feature `gdal`); sem gdal, erro.
fn ensure_srid(features: &[Feature], target: u32) -> Result<Vec<Feature>> {
    if target == DEFAULT_SRID {
        return Ok(features.to_vec());
    }
    #[cfg(feature = "gdal")]
    {
        let tr = crate::ogr::make_transform(DEFAULT_SRID, target)?;
        let mut out = features.to_vec();
        for f in &mut out {
            crate::ogr::reproject(&mut f.geom, &tr)?;
        }
        Ok(out)
    }
    #[cfg(not(feature = "gdal"))]
    {
        let _ = target;
        Err(Error::ReprojectionUnavailable)
    }
}

// ---------------------------------------------------------------------------
// Schema de atributos (compartilhado pelos escritores)
// ---------------------------------------------------------------------------

/// Tipo de coluna inferido dos atributos JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColKind {
    /// Só inteiros aparecem na coluna.
    Int,
    /// Algum float aparece (int é absorvido).
    Real,
    /// Texto/bool/estrutura aninhada (JSON serializado).
    Text,
}

/// Deduz colunas estáveis: chaves ordenadas (determinismo) + tipo dominante.
pub(crate) fn infer_columns(features: &[Feature]) -> Vec<(String, ColKind)> {
    let mut keys: Vec<String> = Vec::new();
    for f in features {
        if let Value::Object(map) = &f.attrs {
            for k in map.keys() {
                if !keys.iter().any(|existing| existing == k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    keys.sort();
    keys.into_iter()
        .map(|k| {
            let mut kind = ColKind::Int;
            for f in features {
                match f.attrs.get(&k) {
                    Some(Value::Number(n)) => {
                        if n.is_f64() {
                            kind = ColKind::Real;
                            break;
                        }
                    }
                    Some(Value::Null) => {}
                    Some(_) => {
                        kind = ColKind::Text;
                        break;
                    }
                    None => {}
                }
            }
            (k, kind)
        })
        .collect()
}

/// Valor serializável da coluna para uma feature (`None` = ausente/null).
pub(crate) fn column_value(f: &Feature, key: &str, kind: ColKind) -> Option<Value> {
    match f.attrs.get(key)? {
        Value::Null => None,
        v => match kind {
            ColKind::Text => Some(match v {
                Value::String(s) => Value::String(s.clone()),
                other => Value::String(other.to_string()), // bool/objeto → JSON/texto
            }),
            _ => Some(v.clone()),
        },
    }
}

// ---------------------------------------------------------------------------
// Escritores
// ---------------------------------------------------------------------------

/// GeoJSON puro (feature `geojson`): sem GDAL, determinístico.
#[cfg(feature = "geojson")]
fn geojson_write(features: &[Feature], path: &Path) -> Result<()> {
    use std::io::Write;

    let mut gj_features = Vec::with_capacity(features.len());
    for f in features {
        let geom_value: geojson::GeometryValue = (&f.geom).into();
        let mut gj = geojson::Feature::from(geojson::Geometry::new(geom_value));
        gj.properties = Some(match &f.attrs {
            Value::Object(map) => map.clone(),
            other => [("attrs".to_string(), other.clone())].into_iter().collect(),
        });
        gj_features.push(gj);
    }
    let fc = geojson::FeatureCollection {
        bbox: None,
        features: gj_features,
        foreign_members: None,
    };
    let mut out = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(&mut out, &fc)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// CSV-XY puro (feature `csv`): só geometrias Point; lon/lat nas duas
/// primeiras colunas, atributos nas demais (chaves ordenadas).
#[cfg(feature = "csv")]
fn csv_write(features: &[Feature], path: &Path) -> Result<()> {
    use std::io::BufWriter;

    let cols = infer_columns(features);
    let file = std::fs::File::create(path)?;
    let mut w = csv::Writer::from_writer(BufWriter::new(file));

    let mut header = vec!["lon".to_string(), "lat".to_string()];
    header.extend(cols.iter().map(|(name, _)| name.clone()));
    w.write_record(&header)?;

    for f in features {
        let (x, y) = match f.geom {
            geo_types::Geometry::Point(p) => (p.x(), p.y()),
            _ => {
                return Err(Error::Write(
                    "CSV-XY suporta apenas geometrias Point; converta o lote \
                     (ST_CollectionExtract/ogr2ogr) ou exporte outro formato"
                        .into(),
                ));
            }
        };
        let mut row: Vec<String> = vec![fmt_f64(x), fmt_f64(y)];
        for (name, kind) in &cols {
            row.push(match column_value(f, name, *kind) {
                Some(Value::String(s)) => s,
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(other) => other.to_string(),
                None => String::new(),
            });
        }
        w.write_record(&row)?;
    }
    w.flush()?;
    Ok(())
}

/// f64 estável com precisão suficiente p/ graus (12 casas ≈ 0.1 mm).
#[cfg(feature = "csv")]
fn fmt_f64(v: f64) -> String {
    format!("{v:.12}")
}

// ---------------------------------------------------------------------------
// Export do banco (feature `db`)
// ---------------------------------------------------------------------------

/// Exporta todas as features de uma camada do banco para `out_path`.
///
/// Consome o [`ta_db::repo::FeatureRepo`] (bind-only, a cargo do impl Pg) e
/// escreve com [`write_vector`]. Devolve o número de features exportadas.
#[cfg(feature = "db")]
pub async fn export<R>(
    repo: &R,
    layer: &ta_core::layer::Layer,
    format: WriteFormat,
    out_path: &Path,
    opts: &WriteOptions,
) -> Result<usize>
where
    R: ta_db::repo::FeatureRepo + ?Sized,
{
    let features = repo.list(&layer.id).await?;
    write_vector(&features, out_path, format, opts)?;
    Ok(features.len())
}
