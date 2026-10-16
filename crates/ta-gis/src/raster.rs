//! Metadados de raster (GeoTIFF/COG) via GDAL — **sem carregar pixels**.
//!
//! O clean-room registra rasters no banco por referência: `source_uri`
//! aponta o arquivo; bbox e geotransform ficam no registro da camada. Os
//! pixels nunca são copiados para o PostgreSQL (política Fase 2); derivados
//! (COG/overviews) são produzidos ao lado do arquivo por
//! [`crate::derived`].

use std::path::Path;

use gdal::Dataset;
use gdal::GeoTransformEx;

use crate::error::{Error, Result};

/// Metadados resumidos de um raster lido.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterMeta {
    /// Largura em pixels.
    pub width: u32,
    /// Altura em pixels.
    pub height: u32,
    /// Geotransform GDAL `[origin_x, pixel_w, rot_x, origin_y, rot_y, pixel_h]`.
    pub transform: [f64; 6],
    /// Número de bandas.
    pub bands: usize,
    /// Nodata declarado na banda 1 (`None` = sem nodata).
    pub nodata: Option<f64>,
    /// Tamanho de bloco/tile `(x, y)` da banda 1 (cache interno do driver).
    pub block_size: [usize; 2],
    /// EPSG identificado (`None` = só WKT conhecido).
    pub srid: Option<u32>,
    /// WKT do CRS (relatório/diagnóstico).
    pub crs_wkt: Option<String>,
    /// `[min_x, min_y, max_x, max_y]` na SRID do raster (cantos do transform).
    pub bbox: [f64; 4],
}

/// Lê metadados do raster. Falha com [`Error::MissingCrs`] quando o arquivo
/// não declara CRS — coordenada sem datum não entra no projeto.
pub fn read_raster_meta(path: &Path) -> Result<RasterMeta> {
    let ds = Dataset::open(path)?;
    let (width, height) = ds.raster_size();
    if ds.raster_count() == 0 {
        return Err(Error::UnsupportedFormat(format!(
            "{} não contém bandas raster (é vetorial?)",
            path.display()
        )));
    }

    let transform = ds.geo_transform().map_err(|_| {
        Error::UnsupportedFormat(format!(
            "{} sem geotransform: raster sem georreferência não é importável \
             (georreferencie com `gdal_translate -a_srs/-a_ullr` antes)",
            path.display()
        ))
    })?;

    let band = ds.rasterband(1)?;
    let block_size = band.block_size();
    let nodata = band.no_data_value();

    // CRS: mesmo caminho da leitura vetorial — EPSG identificado quando
    // possível, WKT para relatório, MissingCrs quando nada existe.
    let mut srs = ds.spatial_ref().map_err(|_| Error::MissingCrs {
        path: path.display().to_string(),
    })?;
    let _ = srs.auto_identify_epsg();
    let srid = srs.auth_code().ok().map(|c| c as u32);
    let crs_wkt = Some(srs.to_wkt().map_err(|_| Error::MissingCrs {
        path: path.display().to_string(),
    })?);

    let bbox = transform_bbox(&transform, width as u32, height as u32);

    Ok(RasterMeta {
        width: u32::try_from(width).unwrap_or(u32::MAX),
        height: u32::try_from(height).unwrap_or(u32::MAX),
        transform,
        bands: ds.raster_count(),
        nodata,
        block_size: [block_size.0, block_size.1],
        srid,
        crs_wkt,
        bbox,
    })
}

/// Bbox a partir do geotransform: os 4 cantos da imagem (transform pode ter
/// rotação), min/max em cada eixo.
pub fn transform_bbox(gt: &[f64; 6], width: u32, height: u32) -> [f64; 4] {
    let w = f64::from(width);
    let h = f64::from(height);
    let corners = [
        (gt.apply(0.0, 0.0)),
        (gt.apply(w, 0.0)),
        (gt.apply(0.0, h)),
        (gt.apply(w, h)),
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in corners {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    [min_x, min_y, max_x, max_y]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fixtures_presentes, testdata_dir};

    #[test]
    fn bbox_sem_rotacao() {
        // origin (-73, 2), pixel 0.01°, sem rotação; 100×50 px.
        let gt = [-73.0, 0.01, 0.0, 2.0, 0.0, -0.01];
        let bbox = transform_bbox(&gt, 100, 50);
        assert_eq!(bbox, [-73.0, 1.5, -72.0, 2.0]);
    }

    #[test]
    fn bbox_com_rotacao() {
        // 45°-ish: pixel (1,0) vai para (1,1).
        let gt = [0.0, 1.0, 1.0, 0.0, 1.0, -1.0];
        let bbox = transform_bbox(&gt, 10, 10);
        assert_eq!(bbox, [0.0, -10.0, 20.0, 10.0]);
    }

    #[test]
    fn meta_do_fixture_geotiff() {
        if !fixtures_presentes() {
            return;
        }
        let tif = testdata_dir().join("fixtures_raster.tif");
        let meta = read_raster_meta(&tif).expect("ler meta do tif");
        assert_eq!(meta.width, 256);
        assert_eq!(meta.height, 256);
        assert_eq!(meta.bands, 4);
        assert_eq!(meta.srid, Some(4674));
        assert_eq!(meta.nodata, Some(0.0));
        assert_eq!(meta.block_size, [64, 64]);
        assert!(meta.bbox[0] < meta.bbox[2] && meta.bbox[1] < meta.bbox[3]);
    }
}
