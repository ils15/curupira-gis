//! Integração do **fluxo de monitoramento** (Fase 1) — **gated** por `TA_TEST_PG=1`.
//!
//! Sem a variável, o teste imprime "skipped" e passa (economia: não exige banco).
//! Cluster de referência: `scripts/test-pg.sh` (PG16 + PostGIS 3.4, porta 55432);
//! DSN configurável por `TA_PG_DSN`.
//!
//! Cobre o ciclo real: criar projeto → camada de AOI → vínculo `aoi_layers` →
//! grade gerada pelo motor [`ta_engine`] e persistida (`cell_layers` + `cells`)
//! → contagem e `ST_Extent` conferidos contra o cálculo **puro** do motor →
//! bloquear célula e listar. Nada de geometria duplicada entre unit e integração:
//! aqui só se confere que a persistência **bate** com o motor.

use geo_types::{LineString, Polygon};
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{BlockedCell, Cell, CellLayer, GridSpec};
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{BlockedCellRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

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

async fn setup() {
    INIT.get_or_init(init_schema).await;
}

async fn test_pool() -> PgPool {
    setup().await;
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

#[tokio::test]
async fn ciclo_aoi_grade_celulas_e_bloqueio() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let projects = PgProjectRepo::new(pool.clone());
    let layers = PgLayerRepo::new(pool.clone());
    let features = PgFeatureRepo::new(pool.clone());

    // 1) projeto + camada de AOI (polígono retangular) + 1 feature.
    let project = projects
        .create(&Project {
            id: String::new(),
            name: unique_name("monitor-proj"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");
    let aoi_layer = layers
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("camada-aoi"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://monitoring".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area_poly = rect(-52.20, -3.20, -52.14, -3.16); // 0.06 × 0.04 graus
    features
        .bulk_insert(
            &aoi_layer.id,
            vec![Feature::new(
                area_poly.clone().into(),
                serde_json::json!({}),
            )],
        )
        .await
        .expect("insere a geometria da AOI");

    // 2) vínculo aoi_layers.
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(
            &project.id,
            &aoi_layer.id,
            Some("cod".into()),
        ))
        .await
        .expect("cria vínculo de AOI");
    assert_eq!(aoi.project_id, project.id);

    // 3) grade: cálculo PURO do motor e persistência (camada + células).
    let spec = GridSpec::new(0.01, -52.20, -3.20, TARGET_SRID as i32);
    let area = geo_types::MultiPolygon::new(vec![area_poly]);
    let generated = ta_engine::generate_grid(&area, &spec).expect("grid do motor");
    let expected_count = generated.len() as u64;
    let expected_extent = ta_engine::aoi_bbox(&area).expect("bbox da AOI");
    // Extensão do motor = envelope das células; recalculada a partir delas.
    let expected_cell_extent = {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for cell in &generated {
            for coord in &cell.geom.exterior().0 {
                min_x = min_x.min(coord.x);
                min_y = min_y.min(coord.y);
                max_x = max_x.max(coord.x);
                max_y = max_y.max(coord.y);
            }
        }
        [min_x, min_y, max_x, max_y]
    };
    assert!(
        expected_cell_extent[0] >= expected_extent[0]
            && expected_cell_extent[2] <= expected_extent[2] + spec.resolution,
        "extensão independente do cálculo puro"
    );

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

    let cell_layer = CellLayer::new(&project.id, &aoi.id, &unique_name("grade"), &spec);
    let mut tx = pool.begin().await.expect("begin");
    let saved_layer = PgCellLayerRepo::create_on(&mut *tx, &cell_layer)
        .await
        .expect("cria camada de células");
    let inserted = PgCellRepo::bulk_insert_on(&mut tx, &saved_layer.id, &cells, spec.srid)
        .await
        .expect("insere células");
    tx.commit().await.expect("commit");
    assert_eq!(inserted, expected_count);

    // 4) contagem e ST_Extent batem com o cálculo puro.
    let repo = PgCellRepo::new(pool.clone());
    assert_eq!(
        repo.count(&saved_layer.id).await.expect("count"),
        expected_count
    );
    let extent = repo
        .extent(&saved_layer.id)
        .await
        .expect("extent")
        .expect("extensão não vazia");
    // ST_Extent bate com o envelope calculado pelo motor puro (os 4 lados,
    // incluindo min_y/max_y).
    assert!(
        (extent[0] - expected_cell_extent[0]).abs() < 1e-9,
        "min_x divergiu: ST_Extent={extent:?} vs motor={expected_cell_extent:?}"
    );
    assert!(
        (extent[1] - expected_cell_extent[1]).abs() < 1e-9,
        "min_y divergiu: ST_Extent={extent:?} vs motor={expected_cell_extent:?}"
    );
    assert!(
        (extent[2] - expected_cell_extent[2]).abs() < 1e-9,
        "max_x divergiu: ST_Extent={extent:?} vs motor={expected_cell_extent:?}"
    );
    assert!(
        (extent[3] - expected_cell_extent[3]).abs() < 1e-9,
        "max_y divergiu: ST_Extent={extent:?} vs motor={expected_cell_extent:?}"
    );

    // 5) bloquear uma célula e conferir o list.
    let blocked_repo = PgBlockedCellRepo::new(pool.clone());
    let target = &generated[0].cell_id;
    let blocked = blocked_repo
        .block(&BlockedCell::new(
            &saved_layer.id,
            target,
            Some("nuvem".into()),
        ))
        .await
        .expect("bloqueia célula");
    assert_eq!(&blocked.cell_id, target);
    let listed = blocked_repo
        .list(&saved_layer.id)
        .await
        .expect("lista bloqueadas");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].cell_id, *target);
    assert_eq!(listed[0].reason.as_deref(), Some("nuvem"));

    // Rebloquear a mesma célula atualiza o motivo (UNIQUE) — não duplica.
    blocked_repo
        .block(&BlockedCell::new(
            &saved_layer.id,
            target,
            Some("água".into()),
        ))
        .await
        .expect("rebloqueia");
    let relisted = blocked_repo.list(&saved_layer.id).await.expect("relist");
    assert_eq!(relisted.len(), 1);
    assert_eq!(relisted[0].reason.as_deref(), Some("água"));

    // Cleanup: apagar o projeto derruba camadas/features/AOI/grade (CASCADE).
    assert!(projects.delete(&project.id).await.expect("delete projeto"));
    pool.close().await;
}
