//! Golden round-trip por formato: import → WKB → export → re-import.
//!
//! A equivalência geométrica é medida **depois de passar pelo ponto único
//! de conversão** ([`ta_core::geo::to_wkb`]/[`from_wkb`]) — exercita
//! exatamente o caminho que as features tomam ao entrar no PostGIS.
//! Formatos OGR (SHP/GPKG/FGB) usam a leitura GDAL; GeoJSON/CSV usam os
//! leitores puros. Requer fixtures? Não: este arquivo gera lotes próprios
//! em memória — fixtures versionados são cobertos nos módulos de leitura.

#![cfg(feature = "gdal")]

use std::path::PathBuf;

use geo_types::{Geometry, LineString, MultiPolygon, Point, Polygon};
use serde_json::json;
use ta_core::feature::Feature;
use ta_core::geo::{from_wkb, to_wkb};
use ta_gis::reader::{self, ReadOptions};
use ta_gis::writer::{WriteFormat, WriteOptions};

fn temp_out(tag: &str, ext: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("ta-gis-rt-{tag}-{}-{n}.{ext}", std::process::id()))
}

/// Lote de referência: um de cada tipo (2D), atributos típicos de camada
/// fundiária sintética.
fn reference_batch() -> Vec<Feature> {
    let poly = Polygon::new(
        LineString::from(vec![
            (-51.0, -12.0),
            (-50.9, -12.0),
            (-50.9, -11.9),
            (-51.0, -11.9),
            (-51.0, -12.0),
        ]),
        vec![LineString::from(vec![
            (-50.98, -11.98),
            (-50.98, -11.94),
            (-50.94, -11.94),
            (-50.98, -11.98),
        ])],
    );
    vec![
        Feature::new(
            Geometry::from(Point::new(-51.92, -12.43)),
            json!({"id": 1, "nome": "ponto_A", "area_ha": 12.5}),
        ),
        Feature::new(
            Geometry::from(LineString::from(vec![(-51.0, -12.0), (-50.5, -11.5)])),
            json!({"id": 2, "nome": "linha_rio", "area_ha": 0}),
        ),
        Feature::new(
            Geometry::from(poly.clone()),
            json!({"id": 3, "nome": "glebe_01", "area_ha": 82.75}),
        ),
        Feature::new(
            Geometry::from(MultiPolygon::new(vec![poly, square_two()])),
            json!({"id": 4, "nome": "glebe_multi", "area_ha": 82.75}),
        ),
    ]
}

/// Segunda parte do MultiPolygon (duas partes ⇒ SHP não rebaixa p/ Polygon).
fn square_two() -> Polygon<f64> {
    Polygon::new(
        LineString::from(vec![
            (10.0, 10.0),
            (10.2, 10.0),
            (10.2, 10.2),
            (10.0, 10.2),
            (10.0, 10.0),
        ]),
        vec![],
    )
}

/// Sub-lotes homogêneos (um por tipo) — exigência do formato SHP, que só
/// aceita UM tipo de geometria por arquivo.
fn homogeneous_batches() -> Vec<Vec<Feature>> {
    let batch = reference_batch();
    batch.iter().map(|f| vec![f.clone()]).collect()
}

/// Compara geometrias pós-WKB com epsilon (graus) — float drift esperado
/// entre drivers é aceito, desvio real não.
fn assert_geom_equiv(a: &Geometry<f64>, b: &Geometry<f64>, eps: f64) {
    let wa = to_wkb(a).unwrap();
    let wb = to_wkb(b).unwrap();
    if wa == wb {
        return;
    }
    let ga = from_wkb(&wa).unwrap();
    let gb = from_wkb(&wb).unwrap();
    assert_eq!(
        std::mem::discriminant(&ga),
        std::mem::discriminant(&gb),
        "tipos divergem: {ga:?} vs {gb:?}"
    );
    compare_coords(&ga, &gb, eps);
}

fn compare_coords(a: &Geometry<f64>, b: &Geometry<f64>, eps: f64) {
    let mut ca = coords_of_geom(a);
    let mut cb = coords_of_geom(b);
    assert_eq!(ca.len(), cb.len(), "contagem de coordenadas divergiu");
    // Ordem-insensível: drivers (SHP!) normalizam orientação/rotação de
    // anéis — equivalência de round-trip é sobre o MESMO conjunto de
    // vértices, não sobre a ordem deles.
    ca.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
    cb.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
    for (p, q) in ca.iter().zip(cb.iter()) {
        assert!(
            (p.0 - q.0).abs() < eps && (p.1 - q.1).abs() < eps,
            "coordenada divergiu além de {eps}: ({}, {}) vs ({}, {})",
            p.0,
            p.1,
            q.0,
            q.1
        );
    }
}

/// Achatamento local de coordenadas (sem depender do crate `geo`).
fn coords_of_geom(g: &Geometry<f64>) -> Vec<(f64, f64)> {
    use geo_types::Geometry::*;
    match g {
        Point(p) => vec![(p.x(), p.y())],
        Line(l) => vec![(l.start.x, l.start.y), (l.end.x, l.end.y)],
        LineString(ls) => ls.0.iter().map(|c| (c.x, c.y)).collect(),
        Rect(r) => r
            .to_polygon()
            .exterior()
            .0
            .iter()
            .map(|c| (c.x, c.y))
            .collect(),
        Triangle(t) => t
            .to_polygon()
            .exterior()
            .0
            .iter()
            .map(|c| (c.x, c.y))
            .collect(),
        Polygon(p) => p
            .exterior()
            .0
            .iter()
            .chain(p.interiors().iter().flat_map(|r| r.0.iter()))
            .map(|c| (c.x, c.y))
            .collect(),
        MultiPoint(mp) => mp.0.iter().map(|p| (p.x(), p.y())).collect(),
        MultiLineString(ml) => {
            ml.0.iter()
                .flat_map(|ls| ls.0.iter())
                .map(|c| (c.x, c.y))
                .collect()
        }
        MultiPolygon(mp) => {
            mp.0.iter()
                .flat_map(|p| {
                    p.exterior()
                        .0
                        .iter()
                        .chain(p.interiors().iter().flat_map(|r| r.0.iter()))
                })
                .map(|c| (c.x, c.y))
                .collect()
        }
        GeometryCollection(gc) => gc.0.iter().flat_map(coords_of_geom).collect(),
    }
}

/// Pipeline genérico: write → detect/read → compara via WKB.
///
/// O casamento é **por atributo `id`** (não por ordem): drivers com índice
/// espacial (FlatGeobuf/OGR) reordenam features na escrita.
fn round_trip_batch(format: WriteFormat, ext: &str, eps: f64, batch: &[Feature]) {
    let out = temp_out(format.name().to_lowercase().as_str(), ext);
    let written = ta_gis::writer::write_vector(batch, &out, format, &WriteOptions::default())
        .unwrap_or_else(|e| panic!("{}: escrita falhou: {e}", format.name()));
    assert_eq!(written, batch.len());

    let data = reader::read_vector(&out, &ReadOptions::default())
        .unwrap_or_else(|e| panic!("{}: re-leitura falhou: {e}", format.name()));
    assert_eq!(data.features.len(), batch.len(), "{}", format.name());

    let id_of = |f: &Feature| f.attrs["id"].as_i64().expect("attrs.id numérico");
    let mut by_id: std::collections::HashMap<i64, &Feature> =
        data.features.iter().map(|f| (id_of(f), f)).collect();
    assert_eq!(by_id.len(), batch.len(), "ids duplicados na releitura");
    for orig in batch {
        let back = by_id
            .remove(&id_of(orig))
            .unwrap_or_else(|| panic!("{}: feature id {} não voltou", format.name(), id_of(orig)));
        assert_geom_equiv(&orig.geom, &back.geom, eps);
    }
    let _ = std::fs::remove_file(&out);
}

fn round_trip(format: WriteFormat, ext: &str, eps: f64) {
    round_trip_batch(format, ext, eps, &reference_batch());
}

#[test]
fn golden_round_trip_shapefile() {
    // SHP/DBF arredonda reais para ~15 dígitos; 1e-9° ≈ 0.1 mm — folga
    // para o formato binário antigo. Um golden POR TIPO (o formato só
    // aceita um tipo de geometria por arquivo).
    for batch in homogeneous_batches() {
        round_trip_batch(WriteFormat::Shapefile, "shp", 1e-9, &batch);
    }
}

/// Lote misto em SHP é erro tipado e acionável (não arquivo corrompido).
#[test]
fn shapefile_rejeita_lote_misto() {
    let batch = reference_batch();
    let out = temp_out("misto", "shp");
    let err = ta_gis::writer::write_vector(
        &batch,
        &out,
        WriteFormat::Shapefile,
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, ta_gis::Error::Write(ref m) if m.contains("mistos")),
        "erro: {err}"
    );
}

#[test]
fn golden_round_trip_geopackage() {
    // GPKG guarda WKB full-precision: ida e volta bit-a-bit esperada.
    round_trip(WriteFormat::GeoPackage, "gpkg", 1e-12);
}

#[test]
fn golden_round_trip_flatgeobuf() {
    // FGB = doubles little-endian: perda esperada só de float puro.
    round_trip(WriteFormat::FlatGeobuf, "fgb", 1e-12);
}

#[test]
#[cfg(feature = "csv")]
fn golden_round_trip_csv_xy() {
    // CSV-XY só tem Point — lote dedicado; 12 casas decimais ⇒ 1e-9° folga.
    let batch = vec![
        Feature::new(
            Geometry::from(Point::new(-51.92, -12.43)),
            json!({"id": 1, "nome": "ponto_A", "area_ha": 12.5}),
        ),
        Feature::new(
            Geometry::from(Point::new(-48.5, -1.4)),
            json!({"id": 2, "nome": "ponto_B", "area_ha": 0}),
        ),
    ];
    round_trip_batch(WriteFormat::CsvXy, "csv", 1e-9, &batch);
}

/// GeoJSON tem leitura pura (feature `geojson`) — mas com gdal habilitado
/// a releitura pode ir por OGR também; aqui força o caminho OGR que é o
/// usado no import normal.
#[test]
#[cfg(feature = "geojson")]
fn golden_round_trip_geojson() {
    round_trip(WriteFormat::GeoJson, "geojson", 1e-9);
}

/// Atributos sobrevivem ao round-trip SHP (tipos DBF: int/real/texto).
#[test]
fn shp_attrs_sobrevivem_round_trip() {
    let reference = reference_batch();
    let out = temp_out("attrs", "shp");
    // Lote homogêneo (SHP é um tipo por arquivo) com attrs int/real/texto.
    let batch = vec![reference[2].clone(), {
        let mut f = reference[2].clone();
        f.attrs = json!({"id": 9, "nome": "glebe_02", "area_ha": 41.5});
        f
    }];
    ta_gis::writer::write_vector(
        &batch,
        &out,
        WriteFormat::Shapefile,
        &WriteOptions::default(),
    )
    .expect("escrever shp");
    let data = reader::read_vector(&out, &ReadOptions::default()).expect("reler");
    for (orig, back) in batch.iter().zip(data.features.iter()) {
        let id_orig = orig.attrs["id"].as_i64().unwrap();
        let id_back = back.attrs["id"].as_i64().expect("id volta numérico");
        assert_eq!(id_orig, id_back);
        let nome_orig = orig.attrs["nome"].as_str().unwrap();
        assert_eq!(back.attrs["nome"].as_str().unwrap(), nome_orig);
    }
    let _ = std::fs::remove_file(&out);
}

/// Escrever lote vazio é erro tipado (não cria arquivo vazio sem sentido).
#[test]
#[cfg(any(
    feature = "gdal",
    feature = "geojson",
    feature = "csv",
    feature = "flatgeobuf"
))]
fn lote_vazio_e_erro_tipado() {
    let err = ta_gis::writer::write_vector(
        &[],
        &temp_out("vazio", "gpkg"),
        WriteFormat::GeoPackage,
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ta_gis::Error::EmptyFeatureSet), "{err}");
}

/// Export de SRID ≠ 4674 sem gdal? gdal está ON neste binário — exporta
/// em 3857 e confere que as coordenadas saíram de graus (ordem 10⁶ m).
#[test]
fn export_reprojeta_para_srid_alvo() {
    let batch = vec![Feature::new(
        Geometry::from(Point::new(-51.92, -12.43)),
        json!({"id": 1}),
    )];
    let out = temp_out("srid", "gpkg");
    let opts = WriteOptions { target_srid: 3857 };
    ta_gis::writer::write_vector(&batch, &out, WriteFormat::GeoPackage, &opts)
        .expect("escrever 3857");
    let data = reader::read_vector(&out, &ReadOptions::default()).expect("reler");
    if let Geometry::Point(p) = &data.features[0].geom {
        assert!(
            p.x().abs() > 1e6,
            "esperava metros (WebMerc), veio {}",
            p.x()
        );
    } else {
        panic!("não é ponto");
    }
    let _ = std::fs::remove_file(&out);
}
