//! Orquestração do **relatório de área** (`ta_flow::area::report`) — **gated**
//! por `TA_TEST_PG=1` (harness de `scripts/test-pg.sh`).
//!
//! Cobertura: resolução do SRS efetivo (default 5880 sem `area_settings`;
//! override; override geográfico recusado), rascunho resolvido por NOME no
//! projeto da grade e rascunho inexistente com erro acionável.

use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use ta_core::aoi::AoiLayer;
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::classification::{Class, DraftLayer};
use ta_core::feature::Feature;
use ta_core::project::Project;
use ta_core::rule::RuleSet;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::repo_class::{ClassRepo, DraftLayerRepo, PgClassRepo, PgDraftLayerRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::PgTaskRepo;
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
use ta_db::sqlx::PgPool;
use ta_engine::{aoi_bbox, generate_grid};
use ta_flow::error::FlowError;
use ta_flow::queue::ensure_cell_layer_in_project;
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";
static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

async fn init_schema() {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = ta_db::sqlx::PgPool::connect(&dsn)
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
    ta_db::sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness")
}

fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

/// Grade pequena e determinística em 4674 (3×2 células de 0.01°).
async fn make_grid(pool: &PgPool, prefix: &str) -> (Project, String, Vec<String>) {
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name(prefix),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");
    let layer = PgLayerRepo::new(pool.clone())
        .add(&ta_core::layer::Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: ta_core::layer::GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://fase5-flow".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area = MultiPolygon::new(vec![Polygon::new(
        LineString::from(vec![
            Coord {
                x: -52.20,
                y: -3.20,
            },
            Coord {
                x: -52.17,
                y: -3.20,
            },
            Coord {
                x: -52.17,
                y: -3.18,
            },
            Coord {
                x: -52.20,
                y: -3.18,
            },
            Coord {
                x: -52.20,
                y: -3.20,
            },
        ]),
        vec![],
    )]);
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                area.clone().into(),
                serde_json::json!({"nome": "aoi"}),
            )],
        )
        .await
        .expect("insere AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");
    let [origin_x, origin_y, ..] = aoi_bbox(&area).expect("bbox");
    let spec = GridSpec::new(0.01, origin_x, origin_y, TARGET_SRID as i32);
    let generated = generate_grid(&area, &spec).expect("grade pura");
    assert_eq!(generated.len(), 6);
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
    let saved = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("grade"),
            &spec,
        ))
        .await
        .expect("cria camada de células");
    PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, TARGET_SRID as i32)
        .await
        .expect("insere células");
    let ids: Vec<String> = cells.into_iter().map(|c| c.cell_id).collect();
    (project, saved.id, ids)
}

async fn cleanup(pool: &PgPool, project_id: &str) {
    ta_db::sqlx::query(
        "DELETE FROM draft_cells WHERE draft_layer_id IN \
         (SELECT id FROM draft_layers WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .execute(pool)
    .await
    .expect("cleanup de draft_cells");
    ta_db::sqlx::query("DELETE FROM rulesets WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de rulesets");
    PgProjectRepo::new(pool.clone())
        .delete(project_id)
        .await
        .expect("cleanup do projeto");
}

/// Rascunho classificado (2 células numa classe) com nome conhecido.
async fn make_draft(
    pool: &PgPool,
    project_id: &str,
    cell_layer_id: &str,
    ids: &[String],
) -> String {
    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(project_id, &unique_name("classe")))
        .await
        .expect("cria classe");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(project_id, &unique_name("rs")))
        .await
        .expect("cria ruleset");
    PgRuleRepo::new(pool.clone())
        .create(&ta_core::rule::Rule {
            id: String::new(),
            ruleset_id: ruleset.id.clone(),
            class_id: class.id.clone(),
            name: unique_name("regra"),
            priority: 1,
            predicate: "ndvi >= 0.6".to_owned(),
            is_active: true,
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria regra");
    let name = unique_name("rasc");
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(project_id, &name).with_cell_layer(Some(cell_layer_id)))
        .await
        .expect("cria rascunho");
    let rows: Vec<(String, String, String)> = ids[..2]
        .iter()
        .map(|c| (cell_layer_id.to_owned(), c.clone(), class.id.clone()))
        .collect();
    let mut tx = pool.begin().await.expect("tx");
    PgTaskRepo::draft_upsert_on(&mut *tx, &draft.id, &ruleset.id, &rows)
        .await
        .expect("grava decisões");
    tx.commit().await.expect("commit");
    name
}

/// Sem `area_settings` e sem `--srs`, o SRS efetivo é o DEFAULT da Decisão 4
/// (EPSG:5880, métrico) e o relatório volta coerente; com rascunho classificado,
/// a área por classe entra no relatório.
#[tokio::test]
async fn report_usa_o_default_5880_e_traz_por_classe() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, ids) = make_grid(&pool, "f5-flow").await;
    let draft_name = make_draft(&pool, &project.id, &grid_id, &ids).await;

    let report = ta_flow::area::report(&pool, &grid_id, Some(&draft_name), None)
        .await
        .expect("relatório default");
    assert_eq!(report.srs, 5880, "default da Decisão 4 (get_or_default)");
    assert_eq!(report.srs_units, "metros");
    assert_eq!(report.cell_layer_id, grid_id);
    assert_eq!(report.summary.cells_total, 6);
    assert_eq!(report.by_class.len(), 1, "uma classe decidida");
    assert_eq!(report.by_class[0].cells, 2);
    assert!(report.by_class[0].area_m2 > 0.0);

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// `--srs` sobrepõe a configuração do projeto (EPSG métrico válido).
#[tokio::test]
async fn report_aceita_override_metrico() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, _) = make_grid(&pool, "f5-ovr").await;

    let report = ta_flow::area::report(&pool, &grid_id, None, Some(31983))
        .await
        .expect("override métrico (SIRGAS 2000 UTM 23S)");
    assert_eq!(report.srs, 31983);
    assert_eq!(report.summary.cells_total, 6);
    assert!(report.by_class.is_empty(), "sem --draft, sem por-classe");

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// Erros acionáveis: SRS geográfico recusado (área nunca em graus), draft
/// inexistente e grade inexistente — cada um com a mensagem certa.
#[tokio::test]
async fn report_recusa_srs_geografico_draft_e_grade_inexistentes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, _) = make_grid(&pool, "f5-err").await;

    let err = ta_flow::area::report(&pool, &grid_id, None, Some(4674))
        .await
        .expect_err("área em graus é recusada");
    assert!(
        matches!(
            err,
            FlowError::Db(ta_db::Error::GeographicSridForArea(4674))
        ),
        "SRS geográfico vira erro tipado, obtido: {err}"
    );

    let err = ta_flow::area::report(&pool, &grid_id, Some("rascunho fantasma"), None)
        .await
        .expect_err("draft inexistente");
    match &err {
        FlowError::DraftNotFound { name, project_id } => {
            assert_eq!(name, "rascunho fantasma");
            assert_eq!(project_id, &project.id);
        }
        other => panic!("erro de draft inexistente esperado, obtido: {other:?}"),
    }

    let err = ta_flow::area::report(&pool, "00000000-0000-0000-0000-000000000000", None, None)
        .await
        .expect_err("grade inexistente");
    assert!(matches!(err, FlowError::CellLayerNotFound(_)), "{err}");

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// O helper de vinculação de grade ao projeto (usado por create_task e pela
/// CLI) mantém as mensagens da Fase 3: inexistente e de outro projeto.
#[tokio::test]
async fn ensure_cell_layer_valida_existencia_e_projeto() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, _) = make_grid(&pool, "f5-ensure").await;
    let (other, other_grid, _) = make_grid(&pool, "f5-ensure2").await;

    let ok = ensure_cell_layer_in_project(&pool, &project.id, &grid_id)
        .await
        .expect("grade do próprio projeto");
    assert_eq!(ok, grid_id);

    let err = ensure_cell_layer_in_project(&pool, &project.id, &other_grid)
        .await
        .expect_err("grade de outro projeto");
    match &err {
        FlowError::CellLayerOtherProject { id, project_id } => {
            assert_eq!(id, &other_grid);
            assert_eq!(project_id, &other.id);
        }
        other_err => panic!("erro de projeto errado esperado, obtido: {other_err:?}"),
    }

    cleanup(&pool, &project.id).await;
    cleanup(&pool, &other.id).await;
    pool.close().await;
}
