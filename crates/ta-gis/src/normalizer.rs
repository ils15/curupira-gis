//! Normalização de SRID e política de validade geométrica.
//!
//! Tudo que entra no projeto é normalizado para o SRID-alvo (**4674 —
//! SIRGAS 2000**).
//!
//! **Política de validade** (documentada, alinhada ao trigger do schema):
//! geometria inválida é **rejeitada** com erro que aponta a feature e o
//! motivo — a ingestão **não repara silenciosamente**. Reparo é decisão
//! consciente na origem (`ST_MakeValid`, `ogr2ogr -makevalid`, ou edição).
//! Regras verificadas aqui (validação simples, equivalente ao que o
//! `ST_IsValid` cobre de forma barata no cliente):
//! - todas as coordenadas finitas (sem NaN/∞);
//! - anéis de polígono fechados (primeiro == último) e com ≥ 4 pontos;
//! - LineString com ≥ 2 pontos; geometrias não vazias.

use std::path::Path;

use geo_types::{Geometry, LineString};
use ta_core::feature::Feature;
use ta_core::layer::GeomType;
use ta_core::project::DEFAULT_SRID;

use crate::detect::{self, SourceFormat};
use crate::error::{Error, Result};
use crate::reader::{self, ReadOptions, VectorData};

/// SRID-alvo do clean-room.
pub const TARGET_SRID: u32 = DEFAULT_SRID;

/// Relatório de uma passada de normalização.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizationReport {
    /// SRID detectado/declarado na origem.
    pub input_srid: Option<u32>,
    /// Houve reprojeção para o SRID-alvo.
    pub reprojected: bool,
    /// Feições que passaram pela normalização.
    pub n_features: usize,
}

/// Valida uma geometria segundo a política do clean-room.
/// `Ok(())` = válida; `Err(motivo)` = rejeitar com mensagem acionável.
pub fn validate_geometry(geom: &Geometry<f64>) -> std::result::Result<(), String> {
    use Geometry::*;
    match geom {
        Point(p) => check_coord(p.x(), p.y()),
        MultiPoint(mp) => {
            if mp.0.is_empty() {
                return Err("MultiPoint vazio".into());
            }
            for p in &mp.0 {
                check_coord(p.x(), p.y())?;
            }
            Ok(())
        }
        LineString(ls) => check_linestring(ls),
        Line(l) => {
            check_coord(l.start.x, l.start.y)?;
            check_coord(l.end.x, l.end.y)
        }
        // Rect/Triangle são validados como o polígono equivalente.
        Rect(r) => check_ring(&r.to_polygon().exterior().clone()),
        Triangle(t) => check_ring(&t.to_polygon().exterior().clone()),
        MultiLineString(ml) => {
            if ml.0.is_empty() {
                return Err("MultiLineString vazio".into());
            }
            for ls in &ml.0 {
                check_linestring(ls)?;
            }
            Ok(())
        }
        Polygon(poly) => {
            check_ring(poly.exterior())?;
            for ring in poly.interiors() {
                check_ring(ring)?;
            }
            Ok(())
        }
        MultiPolygon(mp) => {
            if mp.0.is_empty() {
                return Err("MultiPolygon vazio".into());
            }
            for poly in &mp.0 {
                check_ring(poly.exterior())?;
                for ring in poly.interiors() {
                    check_ring(ring)?;
                }
            }
            Ok(())
        }
        GeometryCollection(gc) => {
            if gc.0.is_empty() {
                return Err("GeometryCollection vazio".into());
            }
            for part in &gc.0 {
                validate_geometry(part)?;
            }
            Ok(())
        }
    }
}

fn check_coord(x: f64, y: f64) -> std::result::Result<(), String> {
    if x.is_finite() && y.is_finite() {
        Ok(())
    } else {
        Err(format!("coordenada não finita: ({x}, {y})"))
    }
}

fn check_linestring(ls: &LineString<f64>) -> std::result::Result<(), String> {
    if ls.0.len() < 2 {
        return Err(format!("LineString com {} ponto(s); mínimo 2", ls.0.len()));
    }
    ls.0.iter().try_for_each(|c| check_coord(c.x, c.y))
}

fn check_ring(ring: &LineString<f64>) -> std::result::Result<(), String> {
    if ring.0.len() < 4 {
        return Err(format!(
            "anel de polígono com {} ponto(s); mínimo 4 (fechado)",
            ring.0.len()
        ));
    }
    let first = ring.0[0];
    // `len >= 4` foi verificado acima; sem `expect` no caminho da lib.
    let last = ring.0[ring.0.len() - 1];
    if first != last {
        return Err(format!(
            "anel de polígono aberto: início ({}, {}) ≠ fim ({}, {}); \
             feche o anel na origem (ST_MakeClosed/ editar o arquivo)",
            first.x, first.y, last.x, last.y
        ));
    }
    ring.0.iter().try_for_each(|c| check_coord(c.x, c.y))
}

/// Normaliza um lote de features para `target_srid` e aplica a política de
/// validade. Reprojeção usa OGR/PROJ (feature `gdal`); sem a feature, só
/// aceita lotes já no SRID-alvo.
///
/// Devolve as features validadas + [`NormalizationReport`]; primeira
/// geometria inválida interrompe com [`Error::InvalidGeometry`].
pub fn normalize(
    features: Vec<Feature>,
    input_srid: Option<u32>,
    target_srid: u32,
) -> Result<(Vec<Feature>, NormalizationReport)> {
    let input_srid = input_srid.ok_or_else(|| Error::MissingCrs {
        path: "(fonte sem SRID declarado: informe --srid ou corrija o CRS da origem)".into(),
    })?;

    let mut features = if input_srid == target_srid {
        features
    } else {
        reproject_all(features, input_srid, target_srid)?
    };

    for (i, f) in features.iter().enumerate() {
        validate_geometry(&f.geom).map_err(|reason| Error::InvalidGeometry { index: i, reason })?;
    }

    let report = NormalizationReport {
        input_srid: Some(input_srid),
        reprojected: input_srid != target_srid,
        n_features: features.len(),
    };
    features.shrink_to_fit();
    Ok((features, report))
}

/// Reprojeção de todos os lotes (feature `gdal` — OGR/PROJ).
fn reproject_all(mut features: Vec<Feature>, from: u32, to: u32) -> Result<Vec<Feature>> {
    #[cfg(feature = "gdal")]
    {
        let tr = crate::ogr::make_transform(from, to)?;
        for f in &mut features {
            crate::ogr::reproject(&mut f.geom, &tr)?;
        }
        Ok(features)
    }
    #[cfg(not(feature = "gdal"))]
    {
        let _ = &mut features;
        let _ = (from, to);
        Err(Error::ReprojectionUnavailable)
    }
}

/// Pipeline completo de import: **detecta → lê → normaliza → devolve** pronto
/// para `FeatureRepo::bulk_insert`.
///
/// O [`VectorData`] de saída vem em `TARGET_SRID`; o relatório diz o que
/// aconteceu (reprojeção? quantas features?).
pub fn prepare_import(
    path: &Path,
    opts: &ReadOptions,
) -> Result<(VectorData, NormalizationReport)> {
    let fmt = detect::detect_source_format(path)?;
    if !matches!(
        fmt,
        SourceFormat::Shapefile
            | SourceFormat::GeoJson
            | SourceFormat::GeoPackage
            | SourceFormat::CsvXy
            | SourceFormat::FlatGeobuf
    ) {
        return Err(Error::UnsupportedFormat(format!(
            "{}: formato vetorial não tratado no pipeline",
            path.display()
        )));
    }
    let data = reader::read_vector(path, opts)?;
    let input_srid = data.srid.or(opts.srid);
    let (features, report) = normalize(data.features, input_srid, TARGET_SRID)?;
    Ok((
        VectorData {
            features,
            srid: Some(TARGET_SRID),
            crs_wkt: data.crs_wkt,
            layer_name: data.layer_name,
        },
        report,
    ))
}

/// Tipo de geometria dominante do lote (para registrar a camada).
/// Conjuntos mistos → [`GeomType::GeometryCollection`]; lote vazio → `None`.
pub fn dominant_geom_type(features: &[Feature]) -> Option<GeomType> {
    let first = features.first()?.geom.clone();
    let base = base_type(&first);
    if features.iter().all(|f| base_type(&f.geom) == base) {
        Some(base)
    } else {
        Some(GeomType::GeometryCollection)
    }
}

fn base_type(geom: &Geometry<f64>) -> GeomType {
    match geom {
        Geometry::Point(_) => GeomType::Point,
        Geometry::LineString(_) | Geometry::Line(_) => GeomType::LineString,
        Geometry::Polygon(_) | Geometry::Rect(_) | Geometry::Triangle(_) => GeomType::Polygon,
        Geometry::MultiPoint(_) => GeomType::MultiPoint,
        Geometry::MultiLineString(_) => GeomType::MultiLineString,
        Geometry::MultiPolygon(_) => GeomType::MultiPolygon,
        Geometry::GeometryCollection(_) => GeomType::GeometryCollection,
    }
}

/// Bbox (min_x, min_y, max_x, max_y) do lote — usado no relatório da CLI e
/// para geometrias de envelope. Lote vazio → `None`.
pub fn bbox_of(features: &[Feature]) -> Option<[f64; 4]> {
    let mut it = features.iter().flat_map(|f| coords_of(&f.geom));
    let first = it.next()?;
    let mut bbox = [first.0, first.1, first.0, first.1];
    for (x, y) in it {
        bbox[0] = bbox[0].min(x);
        bbox[1] = bbox[1].min(y);
        bbox[2] = bbox[2].max(x);
        bbox[3] = bbox[3].max(y);
    }
    Some(bbox)
}

fn coords_of(geom: &Geometry<f64>) -> Box<dyn Iterator<Item = (f64, f64)> + '_> {
    use geo_types::Geometry::*;
    fn coords_iter<'a>(
        c: impl Iterator<Item = &'a geo_types::Coord<f64>> + 'a,
    ) -> Box<dyn Iterator<Item = (f64, f64)> + 'a> {
        Box::new(c.map(|c| (c.x, c.y)))
    }
    match geom {
        Point(p) => Box::new(std::iter::once((p.x(), p.y()))),
        LineString(ls) => coords_iter(ls.0.iter()),
        Line(l) => Box::new([(l.start.x, l.start.y), (l.end.x, l.end.y)].into_iter()),
        Polygon(poly) => Box::new(
            poly.exterior()
                .0
                .iter()
                .chain(poly.interiors().iter().flat_map(|r| r.0.iter()))
                .map(|c| (c.x, c.y)),
        ),
        Rect(r) => {
            let poly = r.to_polygon();
            Box::new(
                poly.exterior()
                    .0
                    .iter()
                    .map(|c| (c.x, c.y))
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        }
        Triangle(t) => {
            let poly = t.to_polygon();
            Box::new(
                poly.exterior()
                    .0
                    .iter()
                    .map(|c| (c.x, c.y))
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        }
        MultiPoint(mp) => coords_iter(mp.0.iter().map(|p| &p.0)),
        MultiLineString(ml) => coords_iter(ml.0.iter().flat_map(|ls| ls.0.iter())),
        MultiPolygon(mp) => Box::new(
            mp.0.iter()
                .flat_map(|p| {
                    p.exterior()
                        .0
                        .iter()
                        .chain(p.interiors().iter().flat_map(|r| r.0.iter()))
                })
                .map(|c| (c.x, c.y)),
        ),
        GeometryCollection(gc) => Box::new(gc.0.iter().flat_map(coords_of)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{LineString, Point, Polygon};
    use serde_json::json;

    fn square(x0: f64, y0: f64, side: f64) -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![
                (x0, y0),
                (x0 + side, y0),
                (x0 + side, y0 + side),
                (x0, y0 + side),
                (x0, y0),
            ]),
            vec![],
        )
    }

    fn feat(geom: Geometry<f64>) -> Feature {
        Feature::new(geom, json!({"id": 1}))
    }

    #[test]
    fn valida_poligono_bom() {
        assert!(validate_geometry(&Geometry::from(square(0.0, 0.0, 1.0))).is_ok());
    }

    /// geo-types 0.7.20 AUTO-FECHA anéis em `Polygon::new` — o caminho de
    /// rejeição de anel aberto é exercitado direto no `check_ring` (anéis
    /// abertos chegam por WKB/fontes externas, não por construtores).
    #[test]
    fn rejeita_anel_aberto_com_mensagem_acionavel() {
        let open = LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
        let err = check_ring(&open).unwrap_err();
        assert!(err.contains("aberto"), "motivo: {err}");
        assert!(err.contains("feche"), "{err}");
    }

    #[test]
    fn rejeita_coordenada_nao_finita() {
        let bad: Geometry<f64> = Point::new(f64::NAN, 1.0).into();
        let err = validate_geometry(&bad).unwrap_err();
        assert!(err.contains("não finita"), "{err}");
    }

    #[test]
    fn rejeita_linestring_de_um_ponto_e_anel_curto() {
        let one: Geometry<f64> = LineString::from(vec![(0.0, 0.0)]).into();
        assert!(validate_geometry(&one).is_err());
        let tiny = Polygon::new(
            LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 0.0)]),
            vec![],
        );
        assert!(validate_geometry(&Geometry::from(tiny)).is_err());
    }

    #[test]
    fn normalize_ja_no_alvo_nao_reprojeta() {
        let feats = vec![feat(Geometry::from(square(-51.0, -12.0, 0.1)))];
        let (out, report) = normalize(feats, Some(4674), TARGET_SRID).unwrap();
        assert!(!report.reprojected);
        assert_eq!(report.n_features, 1);
        assert_eq!(out[0].attrs, json!({"id": 1}));
    }

    #[test]
    fn normalize_sem_srid_falha_com_missing_crs() {
        let feats = vec![feat(Geometry::from(square(0.0, 0.0, 1.0)))];
        let err = normalize(feats, None, TARGET_SRID).unwrap_err();
        assert!(matches!(err, Error::MissingCrs { .. }), "{err}");
    }

    #[test]
    fn rejeita_invalida_com_indice() {
        let mut feats = vec![feat(Geometry::from(square(0.0, 0.0, 1.0)))];
        // NaN atravessa qualquer construtor — rejeição garantida na política.
        let nan_ring = Polygon::new(
            LineString::from(vec![
                (f64::NAN, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (f64::NAN, 1.0),
                (f64::NAN, 0.0),
            ]),
            vec![],
        );
        feats.push(feat(Geometry::from(nan_ring)));
        let err = normalize(feats, Some(4674), TARGET_SRID).unwrap_err();
        match err {
            Error::InvalidGeometry { index, reason } => {
                assert_eq!(index, 1);
                // NaN no primeiro/último ponto pode reportar "aberto" antes
                // de "não finita" (NaN != NaN) — ambos são rejeição correta.
                assert!(
                    reason.contains("não finita") || reason.contains("aberto"),
                    "{reason}"
                );
            }
            other => panic!("erro inesperado: {other}"),
        }
    }

    /// Reprojeção 4326 → 4674 com tolerância: WGS84 e SIRGAS 2000 diferem
    /// no nível de centímetros; em graus, 1e-6° ≈ 0.11 m cobre com folga.
    #[test]
    #[cfg(feature = "gdal")]
    fn reprojecao_4326_para_4674_dentro_da_tolerancia() {
        let lonlat: Geometry<f64> = Point::new(-51.9253, -12.4340).into();
        let feats = vec![feat(lonlat)];
        let (out, report) = normalize(feats, Some(4326), TARGET_SRID).expect("normalizar");
        assert!(report.reprojected);
        assert_eq!(report.input_srid, Some(4326));
        if let Geometry::Point(p) = &out[0].geom {
            assert!((p.x() - -51.9253).abs() < 1e-6, "lon: {}", p.x());
            assert!((p.y() - -12.4340).abs() < 1e-6, "lat: {}", p.y());
        } else {
            panic!("não é ponto");
        }
    }

    /// Sem gdal, reprojeção falha com erro tipado (não pânico).
    #[test]
    #[cfg(not(feature = "gdal"))]
    fn reprojecao_sem_gdal_e_erro_tipado() {
        let feats = vec![feat(Geometry::from(square(0.0, 0.0, 1.0)))];
        assert!(matches!(
            normalize(feats, Some(4326), TARGET_SRID),
            Err(Error::ReprojectionUnavailable)
        ));
    }

    #[test]
    fn dominant_geom_type_misto_vira_collection() {
        let feats = vec![
            feat(Geometry::from(Point::new(0.0, 0.0))),
            feat(Geometry::from(square(0.0, 0.0, 1.0))),
        ];
        assert_eq!(
            dominant_geom_type(&feats),
            Some(GeomType::GeometryCollection)
        );
        let uniforme = vec![
            feat(Geometry::from(square(0.0, 0.0, 1.0))),
            feat(Geometry::from(square(5.0, 5.0, 1.0))),
        ];
        assert_eq!(dominant_geom_type(&uniforme), Some(GeomType::Polygon));
        assert_eq!(dominant_geom_type(&[]), None);
    }

    #[test]
    fn bbox_do_lote() {
        let feats = vec![
            feat(Geometry::from(Point::new(-51.0, -12.0))),
            feat(Geometry::from(Point::new(-48.0, -1.0))),
            feat(Geometry::from(Point::new(-52.5, -2.0))),
        ];
        assert_eq!(bbox_of(&feats), Some([-52.5, -12.0, -48.0, -1.0]));
        assert_eq!(bbox_of(&[]), None);
    }
}
