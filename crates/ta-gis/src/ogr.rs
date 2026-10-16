//! Ponte GDAL (OGR) ↔ `geo_types` + reprojeção via OGR/PROJ.
//!
//! Tudo por cima da API **segura** do crate `gdal` — nenhum `unsafe` neste
//! crate (o wrapper já encapsula a FFI). A conversão é recursiva e cobre os
//! tipos 2D do modelo de domínio; eixos Z/M são descartados na leitura
//! (flatten) e a geometria de saída é sempre 2D.
//!
//! A reprojeção usa `CoordTransform` (OGR, que fala com o PROJ do sistema) —
//! o mesmo motor do `ogr2ogr`, com os grids instalados no host.

use gdal::spatial_ref::{CoordTransform, SpatialRef};
use gdal::vector::{Geometry as GdalGeom, OGRwkbGeometryType as Wkb, geometry_type_flatten};
use geo_types::{
    Coord, Geometry, GeometryCollection, Line, LineString, MultiLineString, MultiPoint,
    MultiPolygon, Point, Polygon,
};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// gdal → geo_types (leitura)
// ---------------------------------------------------------------------------

/// Converte uma geometria OGR em `geo_types`, achatando para 2D.
pub fn from_gdal(g: &GdalGeom) -> Result<Geometry<f64>> {
    let ty = geometry_type_flatten(g.geometry_type());
    match ty {
        Wkb::wkbPoint => {
            let (x, y, _z) = g.get_point(0);
            Ok(Point::new(x, y).into())
        }
        Wkb::wkbLineString => line_from_gdal(g).map(Geometry::LineString),
        Wkb::wkbPolygon => polygon_from_gdal(g).map(Geometry::Polygon),
        Wkb::wkbMultiPoint => {
            let mut pts = Vec::with_capacity(g.geometry_count());
            for i in 0..g.geometry_count() {
                let sub = g.get_geometry(i);
                let (x, y, _z) = sub.get_point(0);
                pts.push(Point::new(x, y));
            }
            Ok(MultiPoint::new(pts).into())
        }
        Wkb::wkbMultiLineString => {
            let mut lines = Vec::with_capacity(g.geometry_count());
            for i in 0..g.geometry_count() {
                lines.push(line_from_gdal(&g.get_geometry(i))?);
            }
            Ok(MultiLineString::new(lines).into())
        }
        Wkb::wkbMultiPolygon => {
            let mut polys = Vec::with_capacity(g.geometry_count());
            for i in 0..g.geometry_count() {
                polys.push(polygon_from_gdal(&g.get_geometry(i))?);
            }
            Ok(MultiPolygon::new(polys).into())
        }
        Wkb::wkbGeometryCollection | Wkb::wkbUnknown => {
            let mut parts = Vec::with_capacity(g.geometry_count());
            for i in 0..g.geometry_count() {
                parts.push(from_gdal(&g.get_geometry(i))?);
            }
            Ok(Geometry::GeometryCollection(GeometryCollection::from(
                parts,
            )))
        }
        other => Err(Error::Write(format!(
            "tipo de geometria OGR não suportado na conversão: {}",
            gdal::vector::geometry_type_to_name(other)
        ))),
    }
}

fn coords_from_gdal(g: &GdalGeom) -> Vec<Coord<f64>> {
    let mut raw = Vec::new();
    g.get_points(&mut raw);
    raw.into_iter()
        .map(|(x, y, _z)| Coord::from((x, y)))
        .collect()
}

fn line_from_gdal(g: &GdalGeom) -> Result<LineString<f64>> {
    let coords = coords_from_gdal(g);
    if coords.len() < 2 {
        return Err(Error::Write(format!(
            "LineString OGR com {0} ponto(s): mínimo 2",
            coords.len()
        )));
    }
    Ok(LineString::new(coords))
}

fn polygon_from_gdal(g: &GdalGeom) -> Result<Polygon<f64>> {
    if g.geometry_count() == 0 {
        return Err(Error::Write("polígono OGR sem anéis".into()));
    }
    let exterior = line_from_gdal(&g.get_geometry(0))?;
    let mut interiors = Vec::with_capacity(g.geometry_count() - 1);
    for i in 1..g.geometry_count() {
        interiors.push(line_from_gdal(&g.get_geometry(i))?);
    }
    Ok(Polygon::new(exterior, interiors))
}

// ---------------------------------------------------------------------------
// geo_types → gdal (escrita)
// ---------------------------------------------------------------------------

/// Converte `geo_types` em geometria OGR (sempre 2D).
pub fn to_gdal(g: &Geometry<f64>) -> Result<GdalGeom> {
    match g {
        Geometry::Point(p) => {
            let mut out = GdalGeom::empty(Wkb::wkbPoint)?;
            out.set_point_2d(0, (p.x(), p.y()));
            Ok(out)
        }
        Geometry::LineString(ls) => line_to_gdal(ls, Wkb::wkbLineString),
        Geometry::Line(l) => {
            let mut out = GdalGeom::empty(Wkb::wkbLineString)?;
            out.add_point_2d((l.start.x, l.start.y));
            out.add_point_2d((l.end.x, l.end.y));
            Ok(out)
        }
        Geometry::Polygon(poly) => polygon_to_gdal(poly),
        Geometry::Rect(r) => polygon_to_gdal(&r.to_polygon()),
        Geometry::Triangle(t) => polygon_to_gdal(&t.to_polygon()),
        Geometry::MultiPoint(mp) => {
            let mut out = GdalGeom::empty(Wkb::wkbMultiPoint)?;
            for p in &mp.0 {
                let mut part = GdalGeom::empty(Wkb::wkbPoint)?;
                part.set_point_2d(0, (p.x(), p.y()));
                out.add_geometry(part)?;
            }
            Ok(out)
        }
        Geometry::MultiLineString(ml) => {
            let mut out = GdalGeom::empty(Wkb::wkbMultiLineString)?;
            for ls in &ml.0 {
                out.add_geometry(line_to_gdal(ls, Wkb::wkbLineString)?)?;
            }
            Ok(out)
        }
        Geometry::MultiPolygon(mp) => {
            let mut out = GdalGeom::empty(Wkb::wkbMultiPolygon)?;
            for p in &mp.0 {
                out.add_geometry(polygon_to_gdal(p)?)?;
            }
            Ok(out)
        }
        Geometry::GeometryCollection(gc) => {
            let mut out = GdalGeom::empty(Wkb::wkbGeometryCollection)?;
            for part in &gc.0 {
                out.add_geometry(to_gdal(part)?)?;
            }
            Ok(out)
        }
    }
}

fn line_to_gdal(ls: &LineString<f64>, ty: Wkb::Type) -> Result<GdalGeom> {
    let mut out = GdalGeom::empty(ty)?;
    for c in &ls.0 {
        out.add_point_2d((c.x, c.y));
    }
    Ok(out)
}

fn polygon_to_gdal(poly: &Polygon<f64>) -> Result<GdalGeom> {
    let mut out = GdalGeom::empty(Wkb::wkbPolygon)?;
    out.add_geometry(line_to_gdal(poly.exterior(), Wkb::wkbLinearRing)?)?;
    for hole in poly.interiors() {
        out.add_geometry(line_to_gdal(hole, Wkb::wkbLinearRing)?)?;
    }
    Ok(out)
}

/// Tipo OGR correspondente (para o `LayerOptions.ty` na criação de camada).
/// Conjuntos heterogêneos viram `wkbUnknown` (aceito por SHP/GPKG).
pub fn ogr_type_of(geom: &Geometry<f64>, homogeneous: bool) -> Wkb::Type {
    if !homogeneous {
        return Wkb::wkbUnknown;
    }
    use Geometry::*;
    match geom {
        Point(_) => Wkb::wkbPoint,
        LineString(_) | Line(_) => Wkb::wkbLineString,
        Polygon(_) | Rect(_) | Triangle(_) => Wkb::wkbPolygon,
        MultiPoint(_) => Wkb::wkbMultiPoint,
        MultiLineString(_) => Wkb::wkbMultiLineString,
        MultiPolygon(_) => Wkb::wkbMultiPolygon,
        GeometryCollection(_) => Wkb::wkbGeometryCollection,
    }
}

// ---------------------------------------------------------------------------
// Reprojeção (OGR/PROJ)
// ---------------------------------------------------------------------------

/// Cria o transformador `from → to` (EPSG).
pub fn make_transform(from_srid: u32, to_srid: u32) -> Result<CoordTransform> {
    let mut from = SpatialRef::from_epsg(from_srid)
        .map_err(|e| Error::Write(format!("SRS de origem EPSG:{from_srid} inválido: {e}")))?;
    let mut to = SpatialRef::from_epsg(to_srid)
        .map_err(|e| Error::Write(format!("SRS de destino EPSG:{to_srid} inválido: {e}")))?;
    // Axis order tradicional (lon, lat) — o clean-room trabalha em x/y
    // matemático, não na ordem EPSG "lat, lon".
    from.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
    to.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
    CoordTransform::new(&from, &to).map_err(|e| {
        Error::Write(format!(
            "transformação {from_srid}→{to_srid} indisponível: {e}"
        ))
    })
}

/// Reprojeção de todos os coeficientes da geometria (lotes por primitiva).
pub fn reproject(geom: &mut Geometry<f64>, tr: &CoordTransform) -> Result<()> {
    match geom {
        Geometry::Point(p) => {
            let (x, y) = transform_one(tr, p.x(), p.y())?;
            *p = Point::new(x, y);
        }
        Geometry::LineString(ls) => {
            let mut owned = ls.clone();
            transform_coords(tr, &mut owned.0)?;
            *ls = owned;
        }
        Geometry::Line(l) => {
            let start = transform_point(tr, Point::from(l.start))?;
            let end = transform_point(tr, Point::from(l.end))?;
            *l = Line::new(start.0, end.0);
        }
        // Rect/Triangle perdem a garantia de alinhamento sob reprojeção —
        // degradam para Polygon (documentado).
        Geometry::Rect(_) | Geometry::Triangle(_) => {
            let poly = match &*geom {
                Geometry::Rect(r) => r.to_polygon(),
                Geometry::Triangle(t) => t.to_polygon(),
                _ => unreachable!("arm exclusivo"),
            };
            let mut p = Geometry::Polygon(poly);
            reproject(&mut p, tr)?;
            *geom = p;
        }
        Geometry::Polygon(poly) => {
            let (ext, interiors) = poly.clone().into_inner();
            let mut ext = ext;
            transform_coords(tr, &mut ext.0)?;
            let mut rings = Vec::with_capacity(interiors.len());
            for mut ring in interiors {
                transform_coords(tr, &mut ring.0)?;
                rings.push(ring);
            }
            *poly = Polygon::new(ext, rings);
        }
        Geometry::MultiPoint(mp) => {
            let mut owned = mp.clone();
            for p in &mut owned.0 {
                let (x, y) = transform_one(tr, p.x(), p.y())?;
                *p = Point::new(x, y);
            }
            *mp = owned;
        }
        Geometry::MultiLineString(ml) => {
            let mut owned = ml.clone();
            for ls in &mut owned.0 {
                transform_coords(tr, &mut ls.0)?;
            }
            *ml = owned;
        }
        Geometry::MultiPolygon(mp) => {
            let mut owned = mp.clone();
            for poly in &mut owned.0 {
                let (ext, interiors) = poly.clone().into_inner();
                let mut ext = ext;
                transform_coords(tr, &mut ext.0)?;
                let mut rings = Vec::with_capacity(interiors.len());
                for mut ring in interiors {
                    transform_coords(tr, &mut ring.0)?;
                    rings.push(ring);
                }
                *poly = Polygon::new(ext, rings);
            }
            *mp = owned;
        }
        Geometry::GeometryCollection(gc) => {
            let mut owned = gc.clone();
            for part in &mut owned.0 {
                reproject(part, tr)?;
            }
            *gc = owned;
        }
    }
    Ok(())
}

fn transform_point(tr: &CoordTransform, p: Point<f64>) -> Result<Point<f64>> {
    let (x, y) = transform_one(tr, p.x(), p.y())?;
    Ok(Point::new(x, y))
}

fn transform_one(tr: &CoordTransform, x: f64, y: f64) -> Result<(f64, f64)> {
    let mut xs = [x];
    let mut ys = [y];
    let mut zs = [0.0];
    tr.transform_coords(&mut xs, &mut ys, &mut zs)?;
    Ok((xs[0], ys[0]))
}

fn transform_coords(tr: &CoordTransform, coords: &mut [Coord<f64>]) -> Result<()> {
    if coords.is_empty() {
        return Ok(());
    }
    let mut xs: Vec<f64> = coords.iter().map(|c| c.x).collect();
    let mut ys: Vec<f64> = coords.iter().map(|c| c.y).collect();
    let mut zs = vec![0.0; coords.len()];
    tr.transform_coords(&mut xs, &mut ys, &mut zs)?;
    for (c, (x, y)) in coords.iter_mut().zip(xs.into_iter().zip(ys)) {
        c.x = x;
        c.y = y;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::Rect;

    #[test]
    fn round_trip_point_line_poly() {
        let poly = Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 3.0),
                (0.0, 3.0),
                (0.0, 0.0),
            ]),
            vec![LineString::from(vec![
                (1.0, 1.0),
                (1.0, 2.0),
                (2.0, 2.0),
                (1.0, 1.0),
            ])],
        );
        let geoms: Vec<Geometry<f64>> = vec![
            Point::new(-51.9, -12.4).into(),
            LineString::from(vec![(0.0, 0.0), (1.5, 2.5), (-3.0, 4.0)]).into(),
            poly.clone().into(),
            MultiPoint::new(vec![Point::new(1.0, 2.0), Point::new(3.0, 4.0)]).into(),
            MultiPolygon::new(vec![poly]).into(),
        ];
        for g in &geoms {
            let ggdal = to_gdal(g).expect("to_gdal");
            let back = from_gdal(&ggdal).expect("from_gdal");
            assert_eq!(&back, g);
        }
    }

    #[test]
    fn round_trip_collection() {
        let coll: Geometry<f64> = Geometry::GeometryCollection(GeometryCollection::from(vec![
            Geometry::from(Point::new(1.0, 2.0)),
            Geometry::from(LineString::from(vec![(0.0, 0.0), (1.0, 1.0)])),
        ]));
        let ggdal = to_gdal(&coll).unwrap();
        let back = from_gdal(&ggdal).unwrap();
        assert_eq!(back, coll);
    }

    /// Reprojeção ida-e-volta 4674 → 3857 → 4674 deve ser idempotente
    /// (tolerância 1e-9 grau) — prova o motor OGR/PROJ sem precisar de
    /// valores de referência gravados.
    #[test]
    fn reprojecao_round_trip_4674_3857_4674() {
        let to_3857 = make_transform(4674, 3857).expect("transform 4674→3857");
        let to_4674 = make_transform(3857, 4674).expect("transform 3857→4674");
        let original: Geometry<f64> = Point::new(-51.9253, -12.4340).into();
        let mut mid = original.clone();
        reproject(&mut mid, &to_3857).expect("ida");
        reproject(&mut mid, &to_4674).expect("volta");
        let (Geometry::Point(a), Geometry::Point(b)) = (&original, &mid) else {
            panic!("tipos inesperados")
        };
        assert!(
            (a.x() - b.x()).abs() < 1e-9,
            "x divergiu: {} vs {}",
            a.x(),
            b.x()
        );
        assert!(
            (a.y() - b.y()).abs() < 1e-9,
            "y divergiu: {} vs {}",
            a.y(),
            b.y()
        );
    }

    /// Sanity: EPSG:3857 tem alcance ±20037508 m ↔ ±85.06° na borda.
    #[test]
    fn mercator_sul_e_norte_batem() {
        let to_3857 = make_transform(4674, 3857).unwrap();
        let mut top: Geometry<f64> = Point::new(0.0, 85.0).into();
        reproject(&mut top, &to_3857).unwrap();
        if let Geometry::Point(p) = &top {
            // 85° N → ~19 971 868,88 m (o teto 20 037 508 m é só em 85.0511°).
            assert!(
                (p.y() - 19_971_868.88).abs() < 10.0,
                "y esperado ~19971868 m, veio {}",
                p.y()
            );
        } else {
            panic!("não é ponto");
        }
    }

    #[test]
    fn rect_para_polygon_round_trip_curto() {
        // Usado por import-raster: envelope do raster vira polígono.
        let r = Rect::new(Coord::from((-73.0, -10.0)), Coord::from((-50.0, 2.0)));
        let poly: Geometry<f64> = Polygon::new(
            LineString::from(vec![
                (r.min().x, r.min().y),
                (r.max().x, r.min().y),
                (r.max().x, r.max().y),
                (r.min().x, r.max().y),
                (r.min().x, r.min().y),
            ]),
            vec![],
        )
        .into();
        let back = from_gdal(&to_gdal(&poly).unwrap()).unwrap();
        assert_eq!(back, poly);
    }
}
