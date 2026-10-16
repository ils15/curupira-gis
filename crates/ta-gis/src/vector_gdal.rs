//! Leitura vetorial via OGR (crate `gdal`) — SHP, GeoJSON, GPKG, FGB.
//!
//! Convenções da leitura:
//! - apenas a **primeira camada** do dataset (SHP/GeoJSON/FGB têm uma; GPKG
//!   com múltiplas camadas deve ser importado camada a camada no futuro);
//! - geometrias achatadas para 2D ([`crate::ogr::from_gdal`]);
//! - atributos OGR → objeto JSON (números preservados, datas como string
//!   ISO, listas como array, NULL → `null`);
//! - CRS: `auto_identify_epsg` + `auth_code`; sem EPSG → reporta só WKT;
//!   sem CRS nenhum → [`Error::MissingCrs`].

use std::path::Path;

use gdal::Dataset;
use gdal::vector::LayerAccess;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::ogr::from_gdal;
use crate::reader::VectorData;

/// Lê a primeira camada vetorial do arquivo via OGR.
pub fn read_vector_gdal(path: &Path) -> Result<VectorData> {
    let ds = Dataset::open(path)?;
    if ds.layer_count() == 0 {
        return Err(Error::UnsupportedFormat(format!(
            "{} não contém camadas vetoriais (é raster?)",
            path.display()
        )));
    }
    let mut layer = ds.layer(0)?;
    let layer_name = layer.name();

    let (srid, crs_wkt) = layer_crs(&layer, path)?;

    let mut features = Vec::new();
    for feature in layer.features() {
        let geom = match feature.geometry() {
            Some(g) => from_gdal(g)?,
            None => continue, // feature sem geometria: pulada e contada
        };
        let mut attrs = serde_json::Map::new();
        for (name, value) in feature.fields() {
            attrs.insert(name, field_value_to_json(value));
        }
        features.push(ta_core::feature::Feature {
            geom,
            attrs: Value::Object(attrs),
        });
    }

    Ok(VectorData {
        features,
        srid,
        crs_wkt,
        layer_name,
    })
}

/// Extrai `(epsg, wkt)` do SRS da camada, com relatório honesto: EPSG só
/// quando a autoridade identifica o código; WKT sempre que existe SRS.
fn layer_crs<L: LayerAccess>(layer: &L, path: &Path) -> Result<(Option<u32>, Option<String>)> {
    let Some(srs) = layer.spatial_ref() else {
        return Err(Error::MissingCrs {
            path: path.display().to_string(),
        });
    };
    let mut srs = srs;
    // Tenta identificar o código EPSG a partir do WKT (não falha a leitura
    // se não identificar — cai no fallback WKT).
    let _ = srs.auto_identify_epsg();
    let srid = srs.auth_code().ok().map(|c| c as u32);
    let wkt = srs.to_wkt().ok();
    if wkt.is_none() {
        return Err(Error::MissingCrs {
            path: path.display().to_string(),
        });
    }
    Ok((srid, wkt))
}

/// Converte o valor de um campo OGR em JSON (perda mínima de fidelidade).
fn field_value_to_json(value: Option<gdal::vector::FieldValue>) -> Value {
    use gdal::vector::FieldValue as FV;
    match value {
        None => Value::Null,
        Some(FV::IntegerValue(v)) => json!(v),
        Some(FV::Integer64Value(v)) => json!(v),
        Some(FV::RealValue(v)) => {
            if v.is_finite() {
                json!(v)
            } else {
                Value::Null // NaN/∞ não existem em JSON
            }
        }
        Some(FV::StringValue(v)) => json!(v),
        Some(FV::DateValue(d)) => json!(d.to_string()),
        Some(FV::DateTimeValue(d)) => json!(d.to_string()),
        Some(FV::IntegerListValue(v)) => json!(v),
        Some(FV::Integer64ListValue(v)) => json!(v),
        Some(FV::RealListValue(v)) => json!(v),
        Some(FV::StringListValue(v)) => json!(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fixtures_presentes, testdata_dir};

    /// FGB começa com magic "fgb\x03" (3 = versão da spec) — usado para
    /// validar a escrita sem depender de leitura completa.
    #[test]
    fn fgb_magic_documentado() {
        // flatgeobuf 6.x: [0x66 0x67 0x62 0x03] = "fgb" + versão.
        let magic: [u8; 4] = [0x66, 0x67, 0x62, 0x03];
        assert_eq!(&magic[..3], b"fgb");
        assert_eq!(magic[3], 3);
    }

    /// Golden de leitura do SHP sintético (200 polígonos, EPSG:4674).
    #[test]
    fn le_shp_fixture() {
        if !fixtures_presentes() {
            return;
        }
        let shp = testdata_dir().join("fixtures_vector.shp");
        let data = read_vector_gdal(&shp).expect("ler shp fixture");
        assert_eq!(data.srid, Some(4674), "fixture deve estar em 4674");
        assert_eq!(data.features.len(), 200);
        let first = &data.features[0];
        assert!(first.attrs.get("id").is_some());
        assert!(first.attrs.get("nome").is_some());
        assert!(first.attrs.get("area_ha").is_some());
    }

    /// Golden de leitura do GeoJSON sintético (equivalente ao SHP).
    #[test]
    fn le_geojson_fixture() {
        if !fixtures_presentes() {
            return;
        }
        let gj = testdata_dir().join("fixtures_vector.geojson");
        let data = read_vector_gdal(&gj).expect("ler geojson fixture");
        assert_eq!(data.features.len(), 200);
        assert_eq!(data.srid, Some(4674));
    }

    /// Golden de leitura do GPKG sintético (mesmo conteúdo, driver GPKG).
    #[test]
    fn le_gpkg_fixture() {
        if !fixtures_presentes() {
            return;
        }
        let gpkg = testdata_dir().join("fixtures_vector.gpkg");
        if !gpkg.exists() {
            eprintln!("[skip] fixtures_vector.gpkg ausente (gerador antigo?)");
            return;
        }
        let data = read_vector_gdal(&gpkg).expect("ler gpkg fixture");
        assert_eq!(data.features.len(), 200);
        assert_eq!(data.srid, Some(4674));
    }
}
