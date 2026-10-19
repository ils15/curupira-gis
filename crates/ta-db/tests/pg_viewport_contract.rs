//! **Contrato da viewport de células** (M4 da revisão da Fase 5b) — gated por
//! `TA_TEST_PG=1` (mesmo harness de `scripts/test-pg.sh`, porta 55432).
//!
//! A GUI nunca carrega a grade inteira: `viewport_on` recebe bbox + LIMIT e
//! devolve `(células, truncated)`. Estes testes fixam o contrato no banco:
//!
//! 1. `limit = 1` com ≥ 2 células na bbox → **1 linha** e `truncated = true`
//!    (o `limit + 1` detecta truncamento sem segunda query);
//! 2. bbox invertida/inválida → `Err` **acionável** (nunca resultado vazio);
//! 3. `limit = 1_000_000` é **clampado** em `MAX_VIEWPORT_CELLS` (5.000):
//!    com 5.041 células na bbox, devolve 5.000 e `truncated = true`.
//!
//! Sem `TA_TEST_PG=1` imprime "skipped" e passa (economia de CI).

use geo_types::{LineString, Polygon};
use sqlx::PgPool;
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{LayerRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellLayerRepo, CellRepo, MAX_VIEWPORT_CELLS, PgCellLayerRepo, PgCellRepo};
use ta_db::spatial::run_migrations;
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

fn dsn() -> String {
    std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned())
}

async fn init_schema() {
    let pool = sqlx::PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness (scripts/test-pg.sh)");
    run_migrations(&pool).await.expect("migrações aplicam");
    pool.close().await;
}

async fn setup() {
    INIT.get_or_init(init_schema).await;
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

/// Projeto + AOI + grade, com as células informadas (linha × coluna regular).
async fn seed_grid(pool: &PgPool, cells: Vec<Cell>) -> (Project, CellLayer) {
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("viewport"),
            srid: 4674,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");
    let aoi_layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: 4674,
            source_uri: "test://viewport".into(),
        })
        .await
        .expect("cria camada de AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&ta_core::aoi::AoiLayer::new(
            &project.id,
            &aoi_layer.id,
            None,
        ))
        .await
        .expect("cria vínculo de AOI");
    let spec = GridSpec::new(0.001, -52.30, -3.30, 4674);
    let layer = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("grade"),
            &spec,
        ))
        .await
        .expect("cria grade");
    PgCellRepo::new(pool.clone())
        .bulk_insert(&layer.id, &cells, 4674)
        .await
        .expect("insere células");
    (project, layer)
}

fn cell(col: i32, row: i32) -> Cell {
    let step = 0.001;
    let x = -52.30 + f64::from(col) * step;
    let y = -3.30 + f64::from(row) * step;
    Cell {
        id: 0,
        cell_layer_id: String::new(),
        cell_id: format!("{col}_{row}"),
        geom: rect(x, y, x + step, y + step),
        row_index: row,
        col_index: col,
    }
}

#[test]
fn contrato_da_viewport_de_celulas() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn())
            .await
            .expect("conectar no PG do harness");
        let cells_repo = PgCellRepo::new(pool.clone());

        // ---- 1/2) limit=1 com 2 células + bbox inválida -------------------
        let (small_project, small) =
            seed_grid(&pool, vec![cell(0, 0), cell(1, 0), cell(0, 1)]).await;
        let bbox = [-52.31, -3.31, -52.27, -3.28];
        let (rows, truncated) = cells_repo
            .viewport_on(&small.id, None, bbox, 1)
            .await
            .expect("viewport com limit=1");
        assert_eq!(rows.len(), 1, "limit=1 devolve UMA linha");
        assert!(
            truncated,
            "há mais células na bbox: truncated tem de ser true"
        );

        // Sem truncamento quando o limite cobre a bbox.
        let (rows, truncated) = cells_repo
            .viewport_on(&small.id, None, bbox, 100)
            .await
            .expect("viewport com limit=100");
        assert_eq!(rows.len(), 3);
        assert!(!truncated);

        // bbox invertida: erro acionável (não lista vazia silenciosa).
        let err = cells_repo
            .viewport_on(&small.id, None, [-52.27, -3.28, -52.31, -3.31], 100)
            .await
            .expect_err("bbox invertida precisa falhar");
        let message = err.to_string();
        assert!(
            message.contains("bbox de viewport inválida"),
            "mensagem acionável: {message}"
        );

        // bbox infinita também é recusada antes de chegar ao PostGIS.
        assert!(
            cells_repo
                .viewport_on(&small.id, None, [f64::NEG_INFINITY, -3.3, -52.2, -3.2], 10)
                .await
                .is_err()
        );

        // ---- 3) limit absurdo é clampado em MAX_VIEWPORT_CELLS ------------
        // 71×71 = 5.041 > 5.000: sem clamp viriam 5.041 linhas e
        // `truncated=false`; com clamp vêm 5.000 e truncated.
        let mut many = Vec::with_capacity(71 * 71);
        for row in 0..71 {
            for col in 0..71 {
                many.push(cell(col, row));
            }
        }
        let (large_project, large) = seed_grid(&pool, many).await;
        let (rows, truncated) = cells_repo
            .viewport_on(&large.id, None, [-52.31, -3.31, -52.20, -3.20], 1_000_000)
            .await
            .expect("viewport com limit absurdo");
        assert_eq!(
            rows.len(),
            usize::try_from(MAX_VIEWPORT_CELLS).expect("constante cabe em usize"),
            "limit=1_000_000 precisa ser clampado em {MAX_VIEWPORT_CELLS}"
        );
        assert!(truncated, "5.041 células na bbox: truncada");

        for project in [small_project, large_project] {
            assert!(
                PgProjectRepo::new(pool.clone())
                    .delete(&project.id)
                    .await
                    .expect("cleanup")
            );
        }
        pool.close().await;
    });
}
