//! Fluxo da **fila de processos pela CLI** (`measure`/`ruleset activate`/
//! `task`/`process`/`cell log`) — **gated** por `TA_TEST_PG=1`, no harness de
//! `scripts/test-pg.sh`.
//!
//! Executa o binário real (`CARGO_BIN_EXE_ta-cli`) e confere o efeito no banco
//! e as mensagens acionáveis do stderr. O projeto/grade de apoio é criado via
//! `ta-db` (o teste não está testando `grid create`); os comandos da FASE 3
//! são todos executados pelo binário.

use std::process::{Command, Output};

use sqlx::PgPool;
use ta_db::spatial::{is_postgis_ready, run_migrations};

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

fn run_ok(args: &[&str], ctx: &str) {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "{ctx} falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_out(args: &[&str], ctx: &str) -> String {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "{ctx} falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_cli_fails(args: &[&str], needle: &str) {
    let out = run_cli(args);
    assert!(
        !out.status.success(),
        "esperava falha, mas passou:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(needle),
        "stderr sem a mensagem `{needle}`:\n{stderr}"
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

async fn migrate_and_pool() -> PgPool {
    run_ok(&["migrate", "--to", &dsn()], "migrate");
    let pool = PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness");
    if !is_postgis_ready(&pool).await.expect("pg_extension") {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS no harness");
    }
    run_migrations(&pool).await.expect("migrações aplicam");
    pool
}

/// Projeto + grade 2×1 (duas células: `0_0` e `1_0`) criados via `ta-db`.
async fn make_project_grid(pool: &PgPool, prefix: &str) -> (String, String, Vec<String>) {
    use geo_types::{Coord, LineString, MultiPolygon, Polygon};
    use ta_core::aoi::AoiLayer;
    use ta_core::cell::{Cell, CellLayer, GridSpec};
    use ta_core::feature::Feature;
    use ta_core::project::Project;
    use ta_db::repo::{
        FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo,
    };
    use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
    use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
    use ta_db::spatial::TARGET_SRID;

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
            source_uri: "test://fase3-cli".into(),
        })
        .await
        .expect("cria camada de AOI");
    let (min_x, min_y) = (-52.20, -3.20);
    let area = MultiPolygon::new(vec![Polygon::new(
        LineString::from(vec![
            Coord { x: min_x, y: min_y },
            Coord {
                x: min_x + 0.02,
                y: min_y,
            },
            Coord {
                x: min_x + 0.02,
                y: min_y + 0.01,
            },
            Coord {
                x: min_x,
                y: min_y + 0.01,
            },
            Coord { x: min_x, y: min_y },
        ]),
        vec![],
    )]);
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(area.clone().into(), serde_json::json!({}))],
        )
        .await
        .expect("insere AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("vínculo de AOI");
    let spec = GridSpec::new(0.01, min_x, min_y, TARGET_SRID as i32);
    let generated = ta_engine::generate_grid(&area, &spec).expect("grade pura");
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
        .expect("camada de células");
    PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, TARGET_SRID as i32)
        .await
        .expect("células");
    let mut ids: Vec<String> = cells.iter().map(|c| c.cell_id.clone()).collect();
    ids.sort();
    (project.id, saved.id, ids)
}

async fn cleanup(pool: &PgPool, project_id: &str) {
    sqlx::query("DELETE FROM tasks WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de tasks");
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
    sqlx::query(
        "DELETE FROM cell_measures WHERE cell_layer_id IN \
         (SELECT id FROM cell_layers WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .execute(pool)
    .await
    .expect("cleanup de cell_measures");
    sqlx::query("DELETE FROM classes WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de classes");
    sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup do projeto");
}

/// Classe + ruleset v1 + regra (`ndvi >= 0.6` → classe) + ATIVAÇÃO.
async fn make_active_ruleset(pool: &PgPool, project_id: &str, class_name: &str) -> String {
    use ta_core::classification::Class;
    use ta_core::rule::{Rule, RuleSet};
    use ta_db::repo_class::{ClassRepo, PgClassRepo};
    use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};

    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(project_id, class_name))
        .await
        .expect("classe");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(project_id, &unique_name("rs")))
        .await
        .expect("ruleset");
    let rule = Rule {
        id: String::new(),
        ruleset_id: ruleset.id.clone(),
        class_id: class.id.clone(),
        name: "classifica".to_owned(),
        priority: 1,
        predicate: "ndvi >= 0.6".to_owned(),
        is_active: true,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    PgRuleRepo::new(pool.clone())
        .create(&rule)
        .await
        .expect("regra");
    run_ok(
        &[
            "ruleset",
            "activate",
            "--project",
            project_id,
            "--name",
            &ruleset.name,
            "--version",
            "1",
        ],
        "ruleset activate",
    );
    ruleset.id
}

#[tokio::test]
async fn fluxo_create_run_show_com_retomada_por_limite() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, grid_id, cell_ids) = make_project_grid(&pool, "cli_f3").await;
    let ruleset_id = make_active_ruleset(&pool, &project_id, "floresta").await;

    // Medidas via CLI: só a primeira célula classifica.
    run_ok(
        &[
            "measure",
            "set",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[0],
            "--values",
            "ndvi=0.8",
            "--source",
            "cli-teste",
        ],
        "measure set",
    );
    let listed = run_out(
        &[
            "measure",
            "list",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[0],
        ],
        "measure list",
    );
    assert!(
        listed.contains("ndvi") && listed.contains("cli-teste"),
        "measure list sem a medida gravada: {listed}"
    );

    // task create classify → enfileira 2 processos (grade 2×1).
    let created = run_out(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "task create",
    );
    let task_id = created
        .lines()
        .find_map(|l| l.split_whitespace().nth(2))
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task na saída do create");
    assert!(
        created.contains("2 novos / 2 células elegíveis"),
        "create deveria enfileirar 2 processos: {created}"
    );

    // run --limit 1 → retomada: 1 done, 1 pending, task pendente.
    let out = run_out(&["task", "run", &task_id, "--limit", "1"], "task run limit");
    assert!(out.contains("processadas=1"), "run parcial: {out}");
    let shown = run_out(&["task", "show", &task_id], "task show");
    assert!(shown.contains("pendentes=1"), "show pós-limite: {shown}");
    assert!(shown.contains("concluídos=1"), "show pós-limite: {shown}");

    // run completo → done; o done NÃO é reprocessado.
    let out = run_out(&["task", "run", &task_id], "task run completo");
    assert!(out.contains("status da task: done"), "run completo: {out}");
    let shown = run_out(&["task", "show", &task_id], "task show final");
    assert!(shown.contains("status=done"), "show final: {shown}");
    assert!(shown.contains("erros=0"), "show final: {shown}");

    // draft_cells no banco com proveniência.
    let draft_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_cells WHERE class_id IN \
         (SELECT id FROM classes WHERE project_id = $1::uuid) \
         AND ruleset_id = $2::uuid AND cell_id = $3",
    )
    .bind(&project_id)
    .bind(&ruleset_id)
    .bind(&cell_ids[0])
    .fetch_one(&pool)
    .await
    .expect("draft_cells");
    assert_eq!(
        draft_rows, 1,
        "célula classificada deveria estar no rascunho"
    );

    // cell log legível pela CLI.
    let log = run_out(
        &[
            "cell",
            "log",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[0],
        ],
        "cell log",
    );
    assert!(log.contains("classe="), "log sem a decisão: {log}");
    let log_warn = run_out(
        &[
            "cell",
            "log",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[1],
        ],
        "cell log da sem medida",
    );
    assert!(
        log_warn.contains("nenhuma classe"),
        "log da célula sem medida: {log_warn}"
    );

    // process list legível.
    let procs = run_out(&["process", "list", "--task", &task_id], "process list");
    assert!(procs.contains("done"), "process list: {procs}");

    // measure clear via CLI.
    run_ok(
        &[
            "measure",
            "clear",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[0],
        ],
        "measure clear",
    );
    let listed = run_out(
        &[
            "measure",
            "list",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_ids[0],
        ],
        "measure list pós-clear",
    );
    assert!(listed.contains("(nenhuma"), "clear não removeu: {listed}");

    cleanup(&pool, &project_id).await;
}

#[tokio::test]
async fn task_cancel_e_cooperativo_e_mensagens_acionaveis() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, grid_id, _) = make_project_grid(&pool, "cli_f3cancel").await;
    let _ = make_active_ruleset(&pool, &project_id, "floresta").await;

    let created = run_out(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "task create",
    );
    let task_id = created
        .lines()
        .find_map(|l| l.split_whitespace().nth(2))
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task");

    run_ok(&["task", "cancel", &task_id], "task cancel");
    let shown = run_out(&["task", "show", &task_id], "task show cancelada");
    assert!(shown.contains("status=cancelled"), "show: {shown}");

    // Run de task cancelada é recusado com mensagem acionável.
    assert_cli_fails(
        &["task", "run", &task_id],
        "está cancelada (cancelamento é terminal)",
    );
    // Cancelar duas vezes é idempotente na CLI.
    run_ok(&["task", "cancel", &task_id], "task cancel 2×");
    // Retry de task cancelada é recusado.
    assert_cli_fails(&["task", "retry", &task_id], "cancelada");

    cleanup(&pool, &project_id).await;
}

#[tokio::test]
async fn mensagens_acionaveis_sem_ruleset_ativo_e_celula_inexistente() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, grid_id, _cell_ids) = make_project_grid(&pool, "cli_f3err").await;

    // Sem ruleset ativo e sem --ruleset → erro acionável.
    assert_cli_fails(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "nenhum ruleset ATIVO no projeto",
    );

    // Com ruleset ativo mas SEM regras → run recusa com mensagem acionável.
    let ruleset_id = make_active_ruleset(&pool, &project_id, "classe_sem_regras").await;
    sqlx::query("DELETE FROM rules WHERE ruleset_id = $1::uuid")
        .bind(&ruleset_id)
        .execute(&pool)
        .await
        .expect("remove regras");
    let created = run_out(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "task create sem regras",
    );
    let task_id = created
        .lines()
        .find_map(|l| l.split_whitespace().nth(2))
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task");
    assert_cli_fails(&["task", "run", &task_id], "nenhuma regra ATIVA no ruleset");

    // Medida em célula inexistente → erro acionável (não FK crua).
    assert_cli_fails(
        &[
            "measure",
            "set",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            "9_9",
            "--values",
            "ndvi=0.7",
        ],
        "não existe na camada",
    );

    cleanup(&pool, &project_id).await;
}

// ---------------------------------------------------------------------------
// Fechamento da revisão Themis da Fase 3 (2026-10-18): deletes conscientes
// de `draft_cells`, cancel de task concluída e rascunho de outra grade.
// ---------------------------------------------------------------------------

/// Fixture do fechamento: projeto + grade 2×1 + ruleset ativo com UMA regra
/// (`ndvi >= 0.6`), medidas nas DUAS células e task rodada até `done` —
/// sobram 2 `draft_cells` decididas pela classe e pelo ruleset.
async fn setup_classificado(
    pool: &PgPool,
    prefix: &str,
) -> (String, String, String, String, String) {
    let (project_id, grid_id, cell_ids) = make_project_grid(pool, prefix).await;
    let ruleset_id = make_active_ruleset(pool, &project_id, "floresta").await;
    for cell_id in &cell_ids {
        run_ok(
            &[
                "measure",
                "set",
                "--cell-layer",
                &grid_id,
                "--cell-id",
                cell_id,
                "--values",
                "ndvi=0.8",
            ],
            "measure set",
        );
    }
    let created = run_out(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "task create",
    );
    let task_id = created
        .lines()
        .find_map(|l| l.split_whitespace().nth(2))
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task");
    let out = run_out(&["task", "run", &task_id], "task run");
    assert!(out.contains("status da task: done"), "run: {out}");
    let draft_cells: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_cells WHERE draft_layer_id = \
         (SELECT draft_layer_id FROM tasks WHERE id = $1::uuid)",
    )
    .bind(&task_id)
    .fetch_one(pool)
    .await
    .expect("draft_cells");
    assert_eq!(draft_cells, 2, "duas células classificadas");
    (
        project_id,
        grid_id,
        ruleset_id,
        task_id,
        "floresta".to_owned(),
    )
}

/// M1.2 — `class delete` com decisões em rascunho: sem `--force` BLOQUEIA
/// com contagem e nomes dos rascunhos (nada apagado); com `--force`, as
/// três contagens (draft_cells + regras + classe) saem numa transação única.
#[tokio::test]
async fn class_delete_com_draft_cells_bloqueia_e_force_reporta_tres_contagens() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, _grid_id, _ruleset_id, _task_id, class_name) =
        setup_classificado(&pool, "cli_f3clsdel").await;

    // Sem --force: bloqueia com contagem + nome do rascunho; nada apagado.
    assert_cli_fails(
        &[
            "class",
            "delete",
            "--project",
            &project_id,
            "--class",
            &class_name,
        ],
        "decide 2 célula(s) em 1 rascunho(s): rascunho ",
    );
    assert_cli_fails(
        &[
            "class",
            "delete",
            "--project",
            &project_id,
            "--class",
            &class_name,
        ],
        "apague o(s) rascunho(s) com `draft delete`",
    );
    let still: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM classes WHERE project_id = $1::uuid), \
            (SELECT count(*) FROM rules r JOIN rulesets rs ON rs.id = r.ruleset_id \
             WHERE rs.project_id = $1::uuid), \
            (SELECT count(*) FROM draft_cells WHERE draft_layer_id IN \
             (SELECT id FROM draft_layers WHERE project_id = $1::uuid))",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("estado após bloqueio");
    assert_eq!(still, (1, 1, 2), "bloqueio não pode apagar nada");

    // Com --force: as três contagens reportadas e tudo some na transação.
    let out = run_out(
        &[
            "class",
            "delete",
            "--project",
            &project_id,
            "--class",
            &class_name,
            "--force",
        ],
        "class delete --force",
    );
    assert!(
        out.contains("decisões em rascunho apagadas: 2") && out.contains("regras apagadas: 1"),
        "force sem as contagens: {out}"
    );
    let left: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM classes WHERE project_id = $1::uuid), \
            (SELECT count(*) FROM rules r JOIN rulesets rs ON rs.id = r.ruleset_id \
             WHERE rs.project_id = $1::uuid), \
            (SELECT count(*) FROM draft_cells WHERE draft_layer_id IN \
             (SELECT id FROM draft_layers WHERE project_id = $1::uuid))",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("estado final");
    assert_eq!(left, (0, 0, 0), "force apaga os três atomicamente");

    cleanup(&pool, &project_id).await;
}

/// M1.3 — `ruleset delete` com decisões em rascunho: sem `--force` BLOQUEIA
/// (contagem + nomes); com `--force`, draft_cells + regras + ruleset numa
/// única transação com as três contagens.
#[tokio::test]
async fn ruleset_delete_com_draft_cells_bloqueia_e_force_reporta_tres_contagens() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, _grid_id, ruleset_id, _task_id, _class) =
        setup_classificado(&pool, "cli_f3rsdel").await;
    let ruleset_name: String = sqlx::query_scalar("SELECT name FROM rulesets WHERE id = $1::uuid")
        .bind(&ruleset_id)
        .fetch_one(&pool)
        .await
        .expect("nome do ruleset");
    let draft_name: String = sqlx::query_scalar(
        "SELECT dl.name FROM draft_cells dc \
         JOIN draft_layers dl ON dl.id = dc.draft_layer_id \
         WHERE dc.ruleset_id = $1::uuid LIMIT 1",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("rascunho que decide");

    // Sem --force: bloqueia com contagem + nomes; nada apagado.
    assert_cli_fails(
        &[
            "ruleset",
            "delete",
            "--project",
            &project_id,
            "--name",
            &ruleset_name,
        ],
        &format!(
            "decide 2 célula(s) em 1 rascunho(s): {draft_name}; apague o(s) \
             rascunho(s) com `draft delete`"
        ),
    );
    let still: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM rulesets WHERE id = $1::uuid), \
            (SELECT count(*) FROM rules WHERE ruleset_id = $1::uuid), \
            (SELECT count(*) FROM draft_cells WHERE ruleset_id = $1::uuid)",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("estado após bloqueio");
    assert_eq!(still, (1, 1, 2), "bloqueio não pode apagar nada");

    // Com --force: as três contagens e tudo some na transação.
    let out = run_out(
        &[
            "ruleset",
            "delete",
            "--project",
            &project_id,
            "--name",
            &ruleset_name,
            "--force",
        ],
        "ruleset delete --force",
    );
    assert!(
        out.contains("decisões em rascunho apagadas: 2") && out.contains("regras apagadas: 1"),
        "force sem as contagens: {out}"
    );
    let left: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM rulesets WHERE id = $1::uuid), \
            (SELECT count(*) FROM rules WHERE ruleset_id = $1::uuid), \
            (SELECT count(*) FROM draft_cells WHERE ruleset_id = $1::uuid)",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("estado final");
    assert_eq!(left, (0, 0, 0), "force apaga os três atomicamente");

    cleanup(&pool, &project_id).await;
}

/// M1.1 — `draft delete`: sem `--yes` BLOQUEIA mostrando a contagem de
/// células classificadas; com `--yes`, apaga o rascunho e as decisões
/// (CASCADE) reportando a contagem.
#[tokio::test]
async fn draft_delete_bloqueia_sem_yes_e_apaga_com_yes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, _grid_id, ruleset_id, _task_id, _class) =
        setup_classificado(&pool, "cli_f3drdel").await;
    let draft_name: String = sqlx::query_scalar(
        "SELECT dl.name FROM draft_cells dc \
         JOIN draft_layers dl ON dl.id = dc.draft_layer_id \
         WHERE dc.ruleset_id = $1::uuid LIMIT 1",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("rascunho classificado");

    // Sem --yes: bloqueia com a contagem; nada apagado.
    assert_cli_fails(
        &[
            "draft",
            "delete",
            "--project",
            &project_id,
            "--name",
            &draft_name,
        ],
        "guarda 2 célula(s) classificada(s)",
    );
    assert_cli_fails(
        &[
            "draft",
            "delete",
            "--project",
            &project_id,
            "--name",
            &draft_name,
        ],
        "repita com --yes",
    );
    let still: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM draft_layers WHERE project_id = $1::uuid AND name = $2), \
            (SELECT count(*) FROM draft_cells WHERE draft_layer_id IN \
             (SELECT id FROM draft_layers WHERE project_id = $1::uuid AND name = $2))",
    )
    .bind(&project_id)
    .bind(&draft_name)
    .fetch_one(&pool)
    .await
    .expect("estado após bloqueio");
    assert_eq!(still, (1, 2), "bloqueio não pode apagar nada");

    // Com --yes: apaga rascunho + decisões (CASCADE), reportando a contagem.
    let out = run_out(
        &[
            "draft",
            "delete",
            "--project",
            &project_id,
            "--name",
            &draft_name,
            "--yes",
        ],
        "draft delete --yes",
    );
    assert!(
        out.contains("células classificadas removidas: 2"),
        "delete sem a contagem: {out}"
    );
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM draft_layers WHERE project_id = $1::uuid AND name = $2), \
            (SELECT count(*) FROM draft_cells WHERE draft_layer_id IN \
             (SELECT id FROM draft_layers WHERE project_id = $1::uuid AND name = $2))",
    )
    .bind(&project_id)
    .bind(&draft_name)
    .fetch_one(&pool)
    .await
    .expect("estado final");
    assert_eq!(left, (0, 0), "rascunho e decisões deveriam ter sumido");

    cleanup(&pool, &project_id).await;
}

/// m2 — `task cancel` de task JÁ CONCLUÍDA é recusado: done é terminal e
/// não vira `cancelled`.
#[tokio::test]
async fn task_cancel_de_task_concluida_e_recusado() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, _grid_id, _ruleset_id, task_id, _class) =
        setup_classificado(&pool, "cli_f3cdone").await;

    assert_cli_fails(
        &["task", "cancel", &task_id],
        "já concluída; nada a cancelar",
    );
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = $1::uuid")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .expect("status da task");
    assert_eq!(status, "done", "done não pode virar cancelled");

    cleanup(&pool, &project_id).await;
}

/// m5 — rascunho de outra grade: recusado no `task create` (com
/// `--draft-layer` explícito) E no `task run`.
#[tokio::test]
async fn task_com_rascunho_de_outra_grade_e_rejeitada_no_create_e_no_run() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    use ta_core::cell::{CellLayer, GridSpec};
    use ta_db::repo_cell::{CellLayerRepo, PgCellLayerRepo};
    use ta_db::spatial::TARGET_SRID;

    let pool = migrate_and_pool().await;
    let (project_id, grid_id, _ruleset_id, _task_id, _class) =
        setup_classificado(&pool, "cli_f3grade").await;

    // Segunda grade no MESMO projeto (sem células — os refuses acontecem
    // antes de tocar a fila).
    let aoi_id: String =
        sqlx::query_scalar("SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid LIMIT 1")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("aoi do projeto");
    let other_grid = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project_id,
            &aoi_id,
            &unique_name("outra_grade"),
            &GridSpec::new(0.01, -51.0, -2.0, TARGET_SRID as i32),
        ))
        .await
        .expect("segunda grade");
    let draft_name = unique_name("rascunho_outra_grade");
    run_ok(
        &[
            "draft",
            "create",
            "--project",
            &project_id,
            "--name",
            &draft_name,
            "--cell-layer",
            &other_grid.id,
        ],
        "draft create na outra grade",
    );

    // create com --draft-layer de outra grade → recusa acionável.
    let created = run_out(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
        ],
        "task create na grade certa (rascunho default)",
    );
    let task_id = created
        .lines()
        .find_map(|l| l.split_whitespace().nth(2))
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task");
    assert_cli_fails(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &project_id,
            "--cell-layer",
            &grid_id,
            "--draft-layer",
            &draft_name,
        ],
        &format!(
            "o rascunho `{draft_name}` pertence à grade `{}`, mas a task usa a \
             grade `{grid_id}`",
            other_grid.id
        ),
    );

    // run com task apontando para rascunho de outra grade → recusa acionável.
    let task_draft: String = sqlx::query_scalar(
        "SELECT dl.name FROM tasks t \
         JOIN draft_layers dl ON dl.id = t.draft_layer_id \
         WHERE t.id = $1::uuid",
    )
    .bind(&task_id)
    .fetch_one(&pool)
    .await
    .expect("rascunho da task");
    sqlx::query("UPDATE tasks SET cell_layer_id = $2::uuid WHERE id = $1::uuid")
        .bind(&task_id)
        .bind(&other_grid.id)
        .execute(&pool)
        .await
        .expect("simula task enfileirada para outra grade");
    assert_cli_fails(
        &["task", "run", &task_id],
        &format!(
            "o rascunho `{task_draft}` pertence à grade `{grid_id}`, mas a task \
             usa a grade `{}`",
            other_grid.id
        ),
    );

    cleanup(&pool, &project_id).await;
}
