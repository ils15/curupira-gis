//! Escritor FlatGeobuf puro-Rust (feature `flatgeobuf`): `FgbWriter` +
//! processadores geozero sobre `geo_types::Geometry` (impl `GeozeroGeometry`
//! do geozero `with-geo`).
//!
//! Lotes mistos NÃO passam por aqui (o `type_` do FgbWriter 6.x é sticky
//! sob `Unknown`) — o dispatch em `writer.rs` usa o driver OGR "FlatGeobuf"
//! nesse caso (desvio registrado em STATUS.md).

use std::io::BufWriter;
use std::path::Path;

use flatgeobuf::{ColumnType, FgbCrs, FgbWriter, FgbWriterOptions, GeometryType};
use geozero::PropertyProcessor;
use serde_json::Value;
use ta_core::feature::Feature;

use crate::error::Result;
use crate::writer::{ColKind, column_value, infer_columns};

/// FlatGeobuf puro (feature `flatgeobuf`): FgbWriter + geozero.
#[cfg(feature = "flatgeobuf")]
pub(crate) fn fgb_write(features: &[Feature], path: &Path, srid: u32) -> Result<()> {
    use std::io::Write;

    let layer_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("layer")
        .to_string();

    // Tipo da camada: uniforme → tipo específico; misto → Unknown (o
    // FeatureWriter do FGB aceita qualquer tipo sob Unknown).
    let mixed = crate::normalizer::dominant_geom_type(features)
        .is_none_or(|t| t == ta_core::layer::GeomType::GeometryCollection);
    let geom_type = if mixed {
        GeometryType::Unknown
    } else {
        match features[0].geom {
            geo_types::Geometry::Point(_) => GeometryType::Point,
            geo_types::Geometry::LineString(_) => GeometryType::LineString,
            geo_types::Geometry::Polygon(_) => GeometryType::Polygon,
            geo_types::Geometry::MultiPoint(_) => GeometryType::MultiPoint,
            geo_types::Geometry::MultiLineString(_) => GeometryType::MultiLineString,
            geo_types::Geometry::MultiPolygon(_) => GeometryType::MultiPolygon,
            _ => GeometryType::Unknown,
        }
    };
    // Lote misto: Unknown com detect_type OFF — com detect ON o writer
    // trava no primeiro tipo visto (exatamente o bug que isso evita).
    let mut fgb = if mixed {
        FgbWriter::create_with_options(
            &layer_name,
            GeometryType::Unknown,
            FgbWriterOptions {
                write_index: true,
                detect_type: false,
                promote_to_multi: false,
                crs: FgbCrs {
                    org: Some("EPSG"),
                    code: srid as i32,
                    ..Default::default()
                },
                ..Default::default()
            },
        )?
    } else {
        FgbWriter::create_with_options(
            &layer_name,
            geom_type,
            FgbWriterOptions {
                write_index: true,
                detect_type: false,
                promote_to_multi: false,
                crs: FgbCrs {
                    org: Some("EPSG"),
                    code: srid as i32,
                    ..Default::default()
                },
                ..Default::default()
            },
        )?
    };

    let cols = infer_columns(features);
    for (name, kind) in &cols {
        let col_type = match kind {
            ColKind::Int => ColumnType::Long,
            ColKind::Real => ColumnType::Double,
            ColKind::Text => ColumnType::String,
        };
        fgb.add_column(name, col_type, |_, _| {});
    }

    for f in features {
        let geom = f.geom.clone();
        fgb.add_feature_geom(geom, |feat| {
            for (i, (name, kind)) in cols.iter().enumerate() {
                if let Some(value) = column_value(f, name, *kind) {
                    let cv = json_to_column_value(&value);
                    // Atributo fora do schema inferido é logado como erro
                    // de propriedade — nunca deve acontecer (infer_columns
                    // varreu o mesmo lote).
                    let _ = feat.property(i, name, &cv);
                }
            }
        })?;
    }

    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    fgb.write(&mut writer)?;
    writer.flush()?;
    Ok(())
}

/// JSON → ColumnValue (escritor FGB). Valores aninhados nunca chegam aqui:
/// `infer_columns` já os classificou como Text e `column_value` os
/// serializou para String antes.
#[cfg(feature = "flatgeobuf")]
fn json_to_column_value(v: &Value) -> geozero::ColumnValue<'_> {
    use geozero::ColumnValue;
    match v {
        Value::Null => ColumnValue::String(""),
        Value::Bool(b) => ColumnValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ColumnValue::Long(i)
            } else {
                ColumnValue::Double(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => ColumnValue::String(s),
        // Defensivo (não deveria ocorrer — ver doc):
        Value::Array(_) | Value::Object(_) => ColumnValue::String(""),
    }
}
