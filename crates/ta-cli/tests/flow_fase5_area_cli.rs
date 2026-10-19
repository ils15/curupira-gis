//! CLI do **relatório de área** (`area report`, Fase 5) — **gated** por
//! `TA_TEST_PG=1` (harness de `scripts/test-pg.sh`).
//!
//! Executa o binário real (`CARGO_BIN_EXE_ta-cli`) contra uma grade criada
//! via `ta-db`: tabela legível em ha com o SRS usado, `--json` parseável,
//! por-classe com `--draft` e erros acionáveis (grade inexistente, rascunho
//! inexistente, SRS geográfico recusado — área nunca é calculada em graus).

use std::process::{Command, Output};

use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use sqlx::PgPool;
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
use ta_engine::{aoi_bbox, generate_grid};

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

fn dsn() -> String {
    std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned())
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ta-cli"))
        .args(args)
        .env("TA_PG_DSN", dsn())
        .output()
        .expect("executar o binário ta-cli")
}

fn run_ok(args: &[&str], ctx: &str) -> String {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "{ctx} falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn assert_cli_fails(args: &[&str], needle: &str, ctx: &str) {
    let out = run_cli(args);
    assert!(
        !out.status.success(),
        "{ctx}: esperava falha, mas passou:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(needle),
        "{ctx}: stderr sem a mensagem `{needle}`:\n{stderr}"
    );
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

async fn init_schema() {
    let pool = PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness");
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

/// Projeto + AOI + grade 3×2 (0.01°) criados via `ta-db` — o teste não está
/// testando `grid create`; só precisa de uma grade para o relatório.
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
            source_uri: "test://fase5-cli".into(),
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

async fn cleanup(pool: &PgPool, project_id: &str) {
    sqlx::query(
        "DELETE FROM draft_cells WHERE draft_layer_id IN \
         (SELECT id FROM draft_layers WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .execute(pool)
    .await
    .expect("cleanup de draft_cells");
    sqlx::query("DELETE FROM rulesets WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de rulesets");
    PgProjectRepo::new(pool.clone())
        .delete(project_id)
        .await
        .expect("cleanup do projeto");
}

/// Relatório legível: SRS impresso, totais em ha, por-classe com `--draft`.
#[tokio::test]
async fn area_report_legivel_com_por_classe() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    init_schema().await;
    let pool = PgPool::connect(&dsn()).await.expect("conectar");
    let (project, grid_id, ids) = make_grid(&pool, "f5-cli").await;
    let draft_name = make_draft(&pool, &project.id, &grid_id, &ids).await;

    let out = run_ok(
        &["area", "report", "--cell-layer", &grid_id],
        "report simples",
    );
    assert!(
        out.contains("srs de cálculo: EPSG:5880"),
        "SRS impresso:\n{out}"
    );
    assert!(out.contains("total:"), "total em ha:\n{out}");
    assert!(out.contains("ha"), "unidade hectare:\n{out}");
    assert!(out.contains("disponível:"), "disponível:\n{out}");

    let out = run_ok(
        &[
            "area",
            "report",
            "--cell-layer",
            &grid_id,
            "--draft",
            &draft_name,
        ],
        "report com draft",
    );
    assert!(out.contains("por classe"), "seção por classe:\n{out}");
    assert!(out.contains("2 células"), "células da classe:\n{out}");

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// `--json` é JSON parseável com o contrato do `AreaReport` (SRS, summary,
/// by_class) — consumo programático do quickstart.
#[tokio::test]
async fn area_report_json_e_parseavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    init_schema().await;
    let pool = PgPool::connect(&dsn()).await.expect("conectar");
    let (project, grid_id, ids) = make_grid(&pool, "f5-json").await;
    let draft_name = make_draft(&pool, &project.id, &grid_id, &ids).await;

    let out = run_ok(
        &[
            "area",
            "report",
            "--cell-layer",
            &grid_id,
            "--draft",
            &draft_name,
            "--json",
        ],
        "report --json",
    );
    let value: serde_json::Value = serde_json::from_str(&out).expect("saída --json é JSON válido");
    assert_eq!(value["srs"], 5880);
    assert_eq!(value["cell_layer_id"], grid_id.as_str());
    assert_eq!(value["summary"]["cells_total"], 6);
    let total = value["summary"]["total_m2"]
        .as_f64()
        .expect("total_m2 float");
    let blocked = value["summary"]["blocked_m2"].as_f64().expect("blocked_m2");
    let available = value["summary"]["available_m2"]
        .as_f64()
        .expect("available_m2");
    assert!(
        (available + blocked - total).abs() <= total.abs() * 1e-9,
        "JSON coerente: available + blocked == total"
    );
    assert_eq!(value["by_class"].as_array().map(Vec::len), Some(1));

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// Erros acionáveis: grade inexistente, rascunho inexistente e SRS geográfico
/// recusado (área nunca é calculada em graus — Decisão 4).
#[tokio::test]
async fn area_report_erros_sao_acionaveis() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    init_schema().await;
    let pool = PgPool::connect(&dsn()).await.expect("conectar");
    let (project, grid_id, _) = make_grid(&pool, "f5-err").await;

    assert_cli_fails(
        &[
            "area",
            "report",
            "--cell-layer",
            "00000000-0000-0000-0000-000000000000",
        ],
        "camada de células `00000000-0000-0000-0000-000000000000` não existe",
        "grade inexistente",
    );
    assert_cli_fails(
        &[
            "area",
            "report",
            "--cell-layer",
            &grid_id,
            "--draft",
            "fantasma",
        ],
        "rascunho `fantasma` não existe no projeto",
        "draft inexistente",
    );
    assert_cli_fails(
        &["area", "report", "--cell-layer", &grid_id, "--srs", "4674"],
        "nunca é calculada em graus",
        "SRS geográfico",
    );
    assert_cli_fails(
        &[
            "area",
            "report",
            "--cell-layer",
            &grid_id,
            "--srs",
            "999999",
        ],
        "não está em spatial_ref_sys",
        "SRS inexistente",
    );

    cleanup(&pool, &project.id).await;
    pool.close().await;
}
