//! Escritor vetorial via OGR (feature `gdal`): Shapefile, GeoPackage,
//! GeoJSON e o fallback FlatGeobuf de lotes mistos.
//!
//! Detalhes que vivem aqui:
//! - recriação limpa do destino (`remove_existing`, com sidecars SHP);
//! - rejeição de lote misto em SHP (limitação do formato, erro acionável);
//! - schema de campos inferido dos atributos (`writer::infer_columns`);
//! - SRID gravado na camada via `SpatialRef::from_epsg`.

use std::path::Path;

use serde_json::Value;
use ta_core::feature::Feature;

use crate::error::{Error, Result};
use crate::writer::{ColKind, WriteFormat, WriteOptions, column_value, infer_columns};

/// Escrita via OGR (feature `gdal`): Shapefile e GeoPackage (e GeoJSON
/// como fallback caso alguém compile só com gdal).
#[cfg(feature = "gdal")]
pub(crate) fn ogr_write(
    features: &[Feature],
    path: &Path,
    format: WriteFormat,
    opts: &WriteOptions,
) -> Result<()> {
    let driver_name = match format {
        WriteFormat::Shapefile => "ESRI Shapefile",
        WriteFormat::GeoPackage => "GPKG",
        WriteFormat::GeoJson => "GeoJSON",
        _ => {
            return Err(Error::UnsupportedFormat(format!(
                "formato {} não é OGR",
                format.name()
            )));
        }
    };

    // ESRI Shapefile tem UM tipo de geometria por arquivo (ponto OU linha OU
    // polígono…). Lote misto → erro com direção, nunca arquivo corrompido.
    if format == WriteFormat::Shapefile
        && crate::normalizer::dominant_geom_type(features)
            .is_some_and(|t| t == ta_core::layer::GeomType::GeometryCollection)
    {
        return Err(Error::Write(
            "Shapefile não suporta tipos de geometria mistos no mesmo arquivo; \
             separe o lote por tipo ou exporte GPKG/GeoJSON/FGB"
                .into(),
        ));
    }

    remove_existing(path, format == WriteFormat::Shapefile)?;
    ogr_write_driver(
        features,
        path,
        driver_name,
        &layer_name_for(path, format),
        opts.target_srid,
    )
}

/// Núcleo OGR: cria dataset/layer/campos e escreve as features (usado por
/// SHP/GPKG/GeoJSON e pelo fallback FGB de lotes mistos).
#[cfg(feature = "gdal")]
pub(crate) fn ogr_write_driver(
    features: &[Feature],
    path: &Path,
    driver_name: &str,
    layer_name: &str,
    target_srid: u32,
) -> Result<()> {
    use gdal::DriverManager;
    use gdal::vector::{FieldDefn, FieldValue, LayerAccess, LayerOptions, OGRFieldType};

    let driver = DriverManager::get_driver_by_name(driver_name)?;
    let mut ds = driver.create_vector_only(path)?;

    let homogeneous = crate::normalizer::dominant_geom_type(features)
        .map(|t| t != ta_core::layer::GeomType::GeometryCollection)
        .unwrap_or(false);
    let ty = crate::ogr::ogr_type_of(&features[0].geom, homogeneous);

    let srs = gdal::spatial_ref::SpatialRef::from_epsg(target_srid)?;
    let name = layer_name.to_string();
    let layer = ds.create_layer(LayerOptions {
        name: &name,
        srs: Some(&srs),
        ty,
        options: None,
    })?;

    let cols = infer_columns(features);
    for (name, kind) in &cols {
        let ogr_type = match kind {
            ColKind::Int => OGRFieldType::OFTInteger64,
            ColKind::Real => OGRFieldType::OFTReal,
            ColKind::Text => OGRFieldType::OFTString,
        };
        let fd = FieldDefn::new(name, ogr_type)?;
        if *kind == ColKind::Text {
            fd.set_width(254); // limite DBF; GPKG ignora
        }
        fd.add_to_layer(&layer)?;
    }

    for f in features {
        let mut feat = gdal::vector::Feature::new(layer.defn())?;
        feat.set_geometry(crate::ogr::to_gdal(&f.geom)?)?;
        for (i, (name, kind)) in cols.iter().enumerate() {
            match column_value(f, name, *kind) {
                None => feat.set_field_null(i)?,
                Some(Value::Number(n)) => match kind {
                    ColKind::Int => {
                        feat.set_field(i, &FieldValue::Integer64Value(n.as_i64().unwrap_or(0)))?
                    }
                    _ => feat.set_field(i, &FieldValue::RealValue(n.as_f64().unwrap_or(0.0)))?,
                },
                Some(Value::String(s)) => feat.set_field(i, &FieldValue::StringValue(s))?,
                Some(Value::Bool(b)) => {
                    feat.set_field(i, &FieldValue::IntegerValue(i32::from(b)))?
                }
                Some(other) => feat.set_field(i, &FieldValue::StringValue(other.to_string()))?,
            }
        }
        feat.create(&layer)?;
    }
    ds.flush_cache()?;
    Ok(())
}

/// Remove o arquivo (e sidecars SHP) antes de criar — drivers OGR falham
/// se o destino existe; reexportação deve sobrescrever de forma limpa.
fn remove_existing(path: &Path, shapefile: bool) -> Result<()> {
    let mut targets = vec![path.to_path_buf()];
    if shapefile {
        for ext in ["shx", "dbf", "prj", "cpg", "qix", "shp.xml"] {
            targets.push(path.with_extension(ext));
        }
    }
    for t in targets {
        match std::fs::remove_file(&t) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Write(format!(
                    "não foi possível remover {} antes de recriar: {e}",
                    t.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn layer_name_for(path: &Path, format: WriteFormat) -> String {
    match format {
        // SHP/GeoJSON: nome é o arquivo; GPKG: nome do layer = stem.
        WriteFormat::GeoPackage | WriteFormat::GeoJson | WriteFormat::Shapefile => path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("layer")
            .to_string(),
        _ => "layer".to_string(),
    }
}
