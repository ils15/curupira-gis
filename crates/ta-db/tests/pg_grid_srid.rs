//! Grade configurável em **qualquer EPSG** — **gated** por `TA_TEST_PG=1`.
//!
//! Cluster de referência: `scripts/test-pg.sh` (PG16 + PostGIS 3.4, porta
//! 55432); DSN configurável por `TA_PG_DSN`. Sem a variável, o teste imprime
//! "skipped" e passa (economia: não exige banco).
//!
//! Cobre a Decisão 4 **retificada pelo usuário em 2026-10-18**: a grade aceita
//! qualquer SRS para `resolution`/origem (graus OU metros), mas as células são
//! **sempre** persistidas em EPSG:4674 (armazenamento canônico). O caso métrico
//! real (EPSG:5880, 500 m) confere a contagem e o envelope contra o cálculo
//! **puro** do motor e prova que todo `cells.geom` está em 4674. O caso
//! geográfico padrão (4674, 118 células do fixture e default inalterado) fica em
//! `dev_up_integration.rs`, para não duplicar.

use std::collections::HashMap;

use geo_types::{Geometry, LineString, MultiPolygon, Polygon};
use serde_json::json;
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations, transform_geometry};
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

/// Tolerância da comparação motor↔banco, em **metros**.
///
/// A comparação é feita no SRS da grade (5880): a geometria pura do motor (em
/// 5880) contra a célula gravada (4674) **trazida de volta** a 5880
/// (`ST_Transform`). O erro medido desse round-trip 5880→4674→5880 é da ordem
/// de 1e-8 m (Hausdorff), então 1e-6 m ainda é folga ~100× e pega qualquer
/// erro real de reprojeção/ordem de coordenadas.
const TOLERANCE_M: f64 = 1e-6;

static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

async fn init_schema() {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness (scripts/test-pg.sh)");
    if !is_postgis_ready(&pool)
        .await
        .expect("consultar pg_extension")
    {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS disponível no harness");
    }
    run_migrations(&pool).await.expect("migrações aplicam");
    pool.close().await;
}

async fn test_pool() -> PgPool {
    INIT.get_or_init(init_schema).await;
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness")
}

fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

/// Retângulo sintético como polígono fechado.
fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::from(vec![
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
            (min_x, min_y),
        ]),
        vec![],
    )
}

/// Envelope `[min_x, min_y, max_x, max_y]` das células do motor puro.
fn extent_of(cells: &[ta_engine::GridCell]) -> [f64; 4] {
    let mut extent = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for cell in cells {
        for coord in &cell.geom.exterior().0 {
            extent[0] = extent[0].min(coord.x);
            extent[1] = extent[1].min(coord.y);
            extent[2] = extent[2].max(coord.x);
            extent[3] = extent[3].max(coord.y);
        }
    }
    extent
}

/// Grade **métrica** (EPSG:5880, 500 m) persistida em 4674.
///
/// Prova os três lados da Decisão 4 retificada: contagem = cálculo puro,
/// `cell_layers.srid` faz round-trip e **toda** célula gravada está em 4674,
/// com o envelope reprojetado batendo com o motor dentro da tolerância.
#[tokio::test]
async fn grade_metrica_em_5880_persiste_celulas_em_4674() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;

    // Projeto + camada de AOI (retângulo em 4674) + vínculo.
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("srid-proj"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://grid-srid".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area_4674 = rect(-52.20, -3.20, -52.14, -3.16);
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                area_4674.clone().into(),
                json!({"nome": "AOI metrica"}),
            )],
        )
        .await
        .expect("insere a geometria da AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");

    // AOI no SRS MÉTRICO da grade (o recorte acontece nesse SRS).
    let geom = transform_geometry(
        &pool,
        &Geometry::MultiPolygon(MultiPolygon::new(vec![area_4674])),
        TARGET_SRID,
        5880,
    )
    .await
    .expect("reprojetа a AOI 4674 → 5880");
    let area_5880 = match geom {
        Geometry::MultiPolygon(mp) => mp,
        other => panic!("esperava MultiPolygon reprojetado, veio {other:?}"),
    };

    // Grade PURA em metros: 500 m, origem no canto (min_x, min_y) da AOI.
    let [min_x, min_y, ..] = ta_engine::aoi_bbox(&area_5880).expect("bbox da AOI em 5880");
    let spec = GridSpec::new(500.0, min_x, min_y, 5880);
    let generated = ta_engine::generate_grid(&area_5880, &spec).expect("grade pura em 5880");
    assert!(
        generated.len() > 1,
        "a AOI métrica deveria render algumas células de 500 m, veio {}",
        generated.len()
    );
    let pure_extent = extent_of(&generated);

    // Persistência: camada com srid 5880 + células (o repositório reprojeta p/ 4674).
    let cells: Vec<Cell> = generated
        .iter()
        .map(|c| Cell {
            id: 0,
            cell_layer_id: String::new(),
            cell_id: c.cell_id.clone(),
            geom: c.geom.clone(),
            row_index: c.row_index,
            col_index: c.col_index,
        })
        .collect();
    let layer_repo = PgCellLayerRepo::new(pool.clone());
    let saved = layer_repo
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("metrica"),
            &spec,
        ))
        .await
        .expect("cria camada de células");
    assert_eq!(saved.srid, 5880, "o srid da camada é o SRS da grade");
    assert_eq!(saved.resolution, 500.0);

    let inserted = PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, 5880)
        .await
        .expect("insere células");
    assert_eq!(inserted, generated.len() as u64);

    // Round-trip do srid: a leitura devolve o SRS da grade (5880).
    let reloaded = layer_repo
        .get(&saved.id)
        .await
        .expect("get")
        .expect("camada existe");
    assert_eq!(reloaded.srid, 5880);
    assert_eq!(reloaded.spec(), spec, "o spec completo faz round-trip");

    // Contagem confere com o cálculo puro.
    let repo = PgCellRepo::new(pool.clone());
    assert_eq!(
        repo.count(&saved.id).await.expect("count"),
        generated.len() as u64
    );

    // TODA célula persistida está em EPSG:4674 (armazenamento canônico).
    let fora: i64 = sqlx::query_scalar(
        "SELECT count(*)::int8 FROM cells \
         WHERE cell_layer_id = $1::uuid AND ST_SRID(geom) <> 4674",
    )
    .bind(&saved.id)
    .fetch_one(&pool)
    .await
    .expect("srid das células gravadas");
    assert_eq!(fora, 0, "toda célula deve estar em EPSG:4674");

    // Envelope reprojetado: a extensão gravada (4674), trazida de volta a 5880,
    // bate com o envelope puro do motor em 5880 (tolerância de 1e-6 m).
    let db_extent: (f64, f64, f64, f64) = sqlx::query_as(
        "SELECT ST_XMin(e), ST_YMin(e), ST_XMax(e), ST_YMax(e) \
         FROM (SELECT ST_Extent(ST_Transform(geom, 5880)) AS e \
               FROM cells WHERE cell_layer_id = $1::uuid) s \
         WHERE e IS NOT NULL",
    )
    .bind(&saved.id)
    .fetch_one(&pool)
    .await
    .expect("extensão reprojetada");
    let db_extent = [db_extent.0, db_extent.1, db_extent.2, db_extent.3];
    for (i, lado) in ["min_x", "min_y", "max_x", "max_y"].iter().enumerate() {
        assert!(
            (db_extent[i] - pure_extent[i]).abs() < TOLERANCE_M,
            "{lado} divergiu além de {TOLERANCE_M} m: banco={db_extent:?} vs motor={pure_extent:?}"
        );
    }

    // Célula a célula: mesma `cell_id`, geometria gravada (4674) reprojetada de
    // volta a 5880 vs. o cálculo PURO do motor. A comparação é vértice a
    // vértice — o `ST_Transform` preserva quantidade e ordem dos vértices —,
    // o que é mais estrito que Hausdorff e custa uma query só.
    let stored: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT cell_id, ST_AsEWKB(ST_Transform(geom, 5880)) \
         FROM cells WHERE cell_layer_id = $1::uuid",
    )
    .bind(&saved.id)
    .fetch_all(&pool)
    .await
    .expect("geometrias das células reprojetadas de volta a 5880");
    assert_eq!(
        stored.len(),
        generated.len(),
        "o banco deve ter exatamente as células do motor"
    );
    let stored: HashMap<String, Polygon<f64>> = stored
        .into_iter()
        .map(|(cell_id, ewkb)| {
            let (geom, _) = ta_core::geo::from_ewkb(&ewkb).expect("EWKB da célula");
            match geom {
                Geometry::Polygon(p) => (cell_id, p),
                other => panic!("célula {cell_id} não é polígono: {other:?}"),
            }
        })
        .collect();
    for cell in &generated {
        let db = stored
            .get(&cell.cell_id)
            .unwrap_or_else(|| panic!("célula {} ausente no banco", cell.cell_id));
        let pure = &cell.geom.exterior().0;
        let round_trip = &db.exterior().0;
        assert_eq!(
            round_trip.len(),
            pure.len(),
            "célula {}: número de vértices divergiu",
            cell.cell_id
        );
        for (i, (a, b)) in round_trip.iter().zip(pure.iter()).enumerate() {
            let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            assert!(
                d < TOLERANCE_M,
                "célula {} vértice {i}: banco e motor divergiram {d} m (> {TOLERANCE_M} m)",
                cell.cell_id
            );
        }
    }

    // Cleanup: apagar o projeto derruba camadas/features/AOI/grade (CASCADE).
    assert!(
        PgProjectRepo::new(pool.clone())
            .delete(&project.id)
            .await
            .expect("delete projeto")
    );
    pool.close().await;
}
