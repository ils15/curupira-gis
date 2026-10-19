//! Repositórios da **fila de processos** (Fase 3) — **gated** por
//! `TA_TEST_PG=1`.
//!
//! Cluster de referência: `scripts/test-pg.sh` (PG16 + PostGIS 3.4, porta
//! 55432); DSN configurável por `TA_PG_DSN`. Sem a variável, cada teste
//! imprime "skipped" e passa.
//!
//! Cobertura: enfileiramento idempotente com bloqueadas fora, run feliz ponta
//! a ponta (medidas → `draft_cells` com proveniência + `cell_log`), retomada
//! por lote sem reprocessar `done`, erro controlado com `last_error` +
//! `task retry`, claim concorrente (`FOR UPDATE SKIP LOCKED`), gatilhos F5 de
//! integridade multi-projeto e ativação de ruleset (índice parcial único).

use std::collections::BTreeMap;

use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use serde_json::json;
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::classification::{Class, DraftLayer};
use ta_core::feature::Feature;
use ta_core::project::Project;
use ta_core::rule::{Rule, RuleSet};
use ta_core::task::{LogLevel, ProcessStatus, Task, TaskKind, TaskStatus};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_class::{ClassRepo, DraftLayerRepo, PgClassRepo, PgDraftLayerRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::{PgTaskRepo, TaskRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
use ta_engine::{CellInput, classify_batch};
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
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> MultiPolygon<f64> {
    MultiPolygon::new(vec![Polygon::new(
        LineString::from(vec![
            Coord { x: min_x, y: min_y },
            Coord { x: max_x, y: min_y },
            Coord { x: max_x, y: max_y },
            Coord { x: min_x, y: max_y },
            Coord { x: min_x, y: min_y },
        ]),
        vec![],
    )])
}

/// Cleanup na ordem das RESTRICT (`draft_cells.ruleset/class`, `rules.class`):
/// tarefas primeiro (cascateiam processos/log), depois rascunhos, rulesets,
/// medidas e o projeto (cascateia o resto).
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
    PgProjectRepo::new(pool.clone())
        .delete(project_id)
        .await
        .expect("cleanup do projeto");
}

/// Grade mínima e determinística em 4674: `cols × rows` células de 0,01°.
/// Devolve o projeto, o id da camada e os ids de célula em ordem.
async fn make_grid(
    pool: &PgPool,
    prefix: &str,
    cols: u32,
    rows: u32,
) -> (Project, String, Vec<String>) {
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
            source_uri: "test://fase3".into(),
        })
        .await
        .expect("cria camada de AOI");
    let (min_x, min_y) = (-52.20, -3.20);
    let area = rect(
        min_x,
        min_y,
        min_x + 0.01 * f64::from(cols),
        min_y + 0.01 * f64::from(rows),
    );
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(area.clone().into(), json!({"nome": "aoi"}))],
        )
        .await
        .expect("insere a geometria da AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");

    let spec = GridSpec::new(0.01, min_x, min_y, TARGET_SRID as i32);
    let generated = ta_engine::generate_grid(&area, &spec).expect("grade pura");
    assert_eq!(
        generated.len(),
        (cols * rows) as usize,
        "grade de teste deveria ser exata"
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
    let saved = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("grade"),
            &spec,
        ))
        .await
        .expect("cria camada de células");
    let inserted = PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, TARGET_SRID as i32)
        .await
        .expect("insere células");
    assert_eq!(inserted, cells.len() as u64);
    let mut ids: Vec<String> = cells.iter().map(|c| c.cell_id.clone()).collect();
    ids.sort();
    (project, saved.id, ids)
}

/// Ruleset com UMA regra decidindo `class_name` por `ndvi >= 0.6`.
async fn make_ruleset_one_rule(
    pool: &PgPool,
    project_id: &str,
    class_name: &str,
    priority: i32,
) -> (String, String) {
    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(project_id, class_name))
        .await
        .expect("cria classe");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(project_id, &unique_name("rs")))
        .await
        .expect("cria ruleset");
    let rule = Rule {
        id: String::new(),
        ruleset_id: ruleset.id.clone(),
        class_id: class.id.clone(),
        name: "classifica".to_owned(),
        priority,
        predicate: "ndvi >= 0.6".to_owned(),
        is_active: true,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    PgRuleRepo::new(pool.clone())
        .create(&rule)
        .await
        .expect("cria regra");
    (ruleset.id, class.id)
}

/// Task classify criada + enfileirada para a grade (1 processo por célula).
async fn make_task(
    pool: &PgPool,
    project_id: &str,
    cell_layer_id: &str,
    ruleset_id: &str,
    draft_layer_id: &str,
) -> Task {
    let mut task = Task::new(project_id, TaskKind::Classify, &unique_name("task"));
    task.cell_layer_id = Some(cell_layer_id.to_owned());
    task.ruleset_id = Some(ruleset_id.to_owned());
    task.draft_layer_id = Some(draft_layer_id.to_owned());
    let saved = PgTaskRepo::create_on(pool, &task).await.expect("cria task");
    let mut tx = pool.begin().await.expect("tx");
    let report = PgTaskRepo::enqueue_on(&mut tx, &saved.id, cell_layer_id)
        .await
        .expect("enfileira");
    tx.commit().await.expect("commit enfileiramento");
    assert!(
        report.inserted > 0,
        "task sem processos não faz sentido aqui"
    );
    saved
}

/// Executor mínimo (o mesmo contrato do `task run` da CLI): um lote transacional
/// claim → medidas → classify → draft + log + finish. Devolve células processadas.
async fn run_batch(
    pool: &PgPool,
    task_id: &str,
    cell_layer_id: &str,
    draft_layer_id: &str,
    ruleset_id: &str,
    rules: &[Rule],
    limit: i64,
) -> ta_db::Result<usize> {
    let mut tx = pool.begin().await?;
    let claimed = PgTaskRepo::claim_on(&mut *tx, task_id, limit).await?;
    if claimed.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    let cell_ids: Vec<String> = claimed.iter().map(|c| c.cell_id.clone()).collect();
    let measures = PgTaskRepo::measures_for_cells_on(&mut *tx, cell_layer_id, &cell_ids).await?;
    let inputs: Vec<CellInput> = claimed
        .iter()
        .map(|c| {
            CellInput::new(
                &c.cell_id,
                measures.get(&c.cell_id).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let outputs = classify_batch(&inputs, rules);
    let mut draft_rows = Vec::new();
    let mut logs = Vec::new();
    for (_proc, output) in claimed.iter().zip(outputs.iter()) {
        if let Some(class_id) = output.decision.class_id.clone() {
            draft_rows.push((cell_layer_id.to_owned(), output.cell_id.clone(), class_id));
        }
        let level = if output.decision.class_id.is_some() {
            LogLevel::Info
        } else {
            LogLevel::Warn
        };
        logs.push(ta_core::task::CellLogEntry::new(
            task_id,
            cell_layer_id,
            &output.cell_id,
            level,
            "run de teste".to_owned(),
        ));
    }
    PgTaskRepo::draft_upsert_on(&mut *tx, draft_layer_id, ruleset_id, &draft_rows).await?;
    PgTaskRepo::append_logs_on(&mut *tx, &logs).await?;
    for proc in &claimed {
        PgTaskRepo::finish_on(&mut *tx, &proc.id, ProcessStatus::Done, None).await?;
    }
    tx.commit().await?;
    Ok(claimed.len())
}

#[tokio::test]
async fn enqueue_e_idempotente_e_bloqueadas_ficam_fora() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, grid, cell_ids) = make_grid(&pool, "f3-enq", 2, 2).await;

    // Bloqueia UMA célula: política = bloqueada fica fora do processamento.
    PgBlockedCellRepo::new(pool.clone())
        .block(&ta_core::cell::BlockedCell {
            id: 0,
            cell_layer_id: grid.clone(),
            cell_id: cell_ids[0].clone(),
            reason: Some("teste".into()),
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("bloqueia célula");

    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, &unique_name("rs")))
        .await
        .expect("ruleset");
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project.id, &unique_name("draft")))
        .await
        .expect("rascunho");
    let mut task = Task::new(&project.id, TaskKind::Classify, "t");
    task.cell_layer_id = Some(grid.clone());
    task.ruleset_id = Some(ruleset.id.clone());
    task.draft_layer_id = Some(draft.id.clone());
    let saved = PgTaskRepo::create_on(&pool, &task).await.expect("task");

    let mut tx = pool.begin().await.expect("tx");
    let first = PgTaskRepo::enqueue_on(&mut tx, &saved.id, &grid)
        .await
        .expect("enqueue 1");
    tx.commit().await.expect("commit");
    assert_eq!(first.inserted, 3, "4 células − 1 bloqueada");
    assert_eq!(first.eligible, 3);

    // 2ª vez: idempotente — mesmas contagens, nenhum processo novo.
    let mut tx = pool.begin().await.expect("tx");
    let second = PgTaskRepo::enqueue_on(&mut tx, &saved.id, &grid)
        .await
        .expect("enqueue 2");
    tx.commit().await.expect("commit");
    assert_eq!(second.inserted, 0);
    assert_eq!(second.eligible, 3);

    let counts = PgTaskRepo::new(pool.clone())
        .counts(&saved.id)
        .await
        .expect("counts");
    assert_eq!(counts.total, 3);
    assert_eq!(counts.pending, 3);

    // A bloqueada não está na fila.
    let processes = PgTaskRepo::new(pool.clone())
        .list_processes(&saved.id, None)
        .await
        .expect("processos");
    assert!(!processes.iter().any(|p| p.cell_id == cell_ids[0]));

    cleanup(&pool, &project.id).await;
}

#[tokio::test]
async fn run_feliz_ponta_a_ponta_com_proveniencia_e_log() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, grid, cell_ids) = make_grid(&pool, "f3-run", 2, 2).await;
    let (ruleset_id, class_id) = make_ruleset_one_rule(&pool, &project.id, "floresta", 1).await;
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project.id, &unique_name("draft")))
        .await
        .expect("rascunho");

    // Medidas: duas células classificam, duas ficam sem classe (sem medida).
    let tasks = PgTaskRepo::new(pool.clone());
    for cell_id in &cell_ids[..2] {
        let mut tx = pool.begin().await.expect("tx");
        PgTaskRepo::upsert_measures_on(
            &mut *tx,
            &[ta_core::classification::CellMeasure {
                cell_layer_id: grid.clone(),
                cell_id: cell_id.clone(),
                measure: "ndvi".into(),
                value: 0.8,
                source: Some("teste".into()),
                updated_on: time::OffsetDateTime::now_utc(),
            }],
        )
        .await
        .expect("medidas");
        tx.commit().await.expect("commit");
    }

    let task = make_task(&pool, &project.id, &grid, &ruleset_id, &draft.id).await;
    let all_rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .expect("regras");

    let mut total = 0;
    loop {
        let n = run_batch(
            &pool,
            &task.id,
            &grid,
            &draft.id,
            &ruleset_id,
            &all_rules,
            3,
        )
        .await
        .expect("lote");
        if n == 0 {
            break;
        }
        total += n;
    }
    assert_eq!(total, 4);

    let status = tasks
        .refresh_status(&task.id)
        .await
        .expect("refresh")
        .expect("task existe");
    assert_eq!(status, TaskStatus::Done);
    let counts = tasks.counts(&task.id).await.expect("counts");
    assert_eq!(counts.done, 4);
    assert_eq!(counts.pending, 0);

    // draft_cells com proveniência (class_id + ruleset_id) nas 2 classificadas.
    let draft_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT cell_id, class_id::text, ruleset_id::text FROM draft_cells \
         WHERE draft_layer_id = $1::uuid ORDER BY cell_id",
    )
    .bind(&draft.id)
    .fetch_all(&pool)
    .await
    .expect("draft_cells");
    assert_eq!(draft_rows.len(), 2);
    for (cell_id, saved_class, saved_ruleset) in &draft_rows {
        assert_eq!(saved_class, &class_id, "classe decidida pela regra");
        assert_eq!(saved_ruleset, &ruleset_id, "proveniência do ruleset");
        assert!(cell_ids.contains(cell_id));
    }

    // Log por célula: info nas classificadas, warn nas sem classe.
    let logs: Vec<(String, String)> = sqlx::query_as(
        "SELECT cell_id, level FROM cell_log WHERE task_id = $1::uuid ORDER BY cell_id",
    )
    .bind(&task.id)
    .fetch_all(&pool)
    .await
    .expect("logs");
    assert_eq!(logs.len(), 4);
    assert_eq!(logs.iter().filter(|(_, l)| l == "info").count(), 2);
    assert_eq!(logs.iter().filter(|(_, l)| l == "warn").count(), 2);

    cleanup(&pool, &project.id).await;
}

#[tokio::test]
async fn retomada_com_limite_nao_reprocessa_done() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, grid, _) = make_grid(&pool, "f3-resume", 3, 1).await;
    let (ruleset_id, _) = make_ruleset_one_rule(&pool, &project.id, "floresta", 1).await;
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project.id, &unique_name("draft")))
        .await
        .expect("rascunho");
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .expect("regras");
    let task = make_task(&pool, &project.id, &grid, &ruleset_id, &draft.id).await;
    let tasks = PgTaskRepo::new(pool.clone());

    // 1ª invocação: só 1 célula.
    let n = run_batch(&pool, &task.id, &grid, &draft.id, &ruleset_id, &rules, 1)
        .await
        .expect("lote 1");
    assert_eq!(n, 1);
    let counts = tasks.counts(&task.id).await.expect("counts");
    assert_eq!((counts.done, counts.pending), (1, 2));
    // Trabalho restante → task NÃO está done.
    assert_eq!(
        tasks.refresh_status(&task.id).await.expect("refresh"),
        Some(TaskStatus::Pending)
    );

    // 2ª invocação: termina o resto SEM reprocessar o done (attempts do 1º = 1).
    loop {
        if run_batch(&pool, &task.id, &grid, &draft.id, &ruleset_id, &rules, 2)
            .await
            .expect("lote 2")
            == 0
        {
            break;
        }
    }
    let counts = tasks.counts(&task.id).await.expect("counts finais");
    assert_eq!(counts.done, 3);
    assert_eq!(counts.error, 0);
    let attempts: Vec<i32> = sqlx::query_scalar(
        "SELECT attempts FROM processes WHERE task_id = $1::uuid ORDER BY cell_id",
    )
    .bind(&task.id)
    .fetch_all(&pool)
    .await
    .expect("attempts");
    assert!(
        attempts.iter().all(|a| *a == 1),
        "nenhum done reprocessado: {attempts:?}"
    );
    assert_eq!(
        tasks.refresh_status(&task.id).await.expect("refresh"),
        Some(TaskStatus::Done)
    );

    cleanup(&pool, &project.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_concorrente_nao_reclama_o_mesmo_processo() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, grid, _) = make_grid(&pool, "f3-claim", 3, 2).await; // 6 células
    let (ruleset_id, _) = make_ruleset_one_rule(&pool, &project.id, "floresta", 1).await;
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project.id, &unique_name("draft")))
        .await
        .expect("rascunho");
    let task = make_task(&pool, &project.id, &grid, &ruleset_id, &draft.id).await;

    // Dois workers SIMULTÂNEOS reclamam 3 processos cada: a transação fica
    // aberta durante o "trabalho" (sleep) para o SKIP LOCKED do segundo
    // worker ver as linhas bloqueadas do primeiro.
    let worker = |pool: PgPool, task_id: String| async move {
        let mut tx = pool.begin().await.expect("tx do worker");
        let claimed = PgTaskRepo::claim_on(&mut *tx, &task_id, 3)
            .await
            .expect("claim");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        tx.commit().await.expect("commit do claim");
        claimed
    };
    let (c1, c2) = tokio::join!(
        worker(pool.clone(), task.id.clone()),
        worker(pool.clone(), task.id.clone())
    );
    let claimed_1 = c1;
    let claimed_2 = c2;
    assert_eq!(claimed_1.len(), 3, "worker 1 sem lote cheio");
    assert_eq!(claimed_2.len(), 3, "worker 2 sem lote cheio");

    // Disjunção: nenhum processo reclamado 2× (o claim commitado marca
    // `running`; conferimos no banco que as 6 estão running).
    let ids_1: std::collections::BTreeSet<_> = claimed_1.iter().map(|c| c.id.clone()).collect();
    let ids_2: std::collections::BTreeSet<_> = claimed_2.iter().map(|c| c.id.clone()).collect();
    assert!(ids_1.is_disjoint(&ids_2), "mesmo processo reclamado 2×");
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processes WHERE task_id = $1::uuid AND status = 'running'",
    )
    .bind(&task.id)
    .fetch_one(&pool)
    .await
    .expect("running");
    assert_eq!(running, 6, "todas as 6 reclamadas e em execução");

    cleanup(&pool, &project.id).await;
}

#[tokio::test]
async fn erro_controlado_gera_last_error_e_retry_recupera() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project_a, grid, _) = make_grid(&pool, "f3-err-a", 2, 1).await;

    // Projeto B com a classe "invasora" (cross-project).
    let project_b = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("f3-err-b"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto B");
    let class_b = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project_b.id, "invasora"))
        .await
        .expect("classe B");

    let (ruleset_id, _) = make_ruleset_one_rule(&pool, &project_a.id, "floresta", 1).await;
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project_a.id, &unique_name("draft")).with_cell_layer(Some(&grid)))
        .await
        .expect("rascunho");

    // Injeção controlada (SQL direto): desativa o gatilho F5 de `rules`,
    // insere regra apontando para a classe de OUTRO projeto (prioridade 100,
    // vence tudo) e reativa o gatilho. O gatilho de `draft_cells` segue ativo.
    sqlx::query("ALTER TABLE rules DISABLE TRIGGER rules_class_project_chk")
        .execute(&pool)
        .await
        .expect("desativa trigger");
    let bad_rule: (String,) = sqlx::query_as(
        "INSERT INTO rules (ruleset_id, class_id, name, priority, predicate) \
         VALUES ($1::uuid, $2::uuid, 'invasora', 100, 'ndvi >= 0.0') RETURNING id::text",
    )
    .bind(&ruleset_id)
    .bind(&class_b.id)
    .fetch_one(&pool)
    .await
    .expect("regra invasora (trigger desligado)");
    sqlx::query("ALTER TABLE rules ENABLE TRIGGER rules_class_project_chk")
        .execute(&pool)
        .await
        .expect("reativa trigger");

    let task = make_task(&pool, &project_a.id, &grid, &ruleset_id, &draft.id).await;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .expect("regras");
    let tasks = PgTaskRepo::new(pool.clone());

    // Run: o draft_upsert fere o gatilho de draft_cells (classe do projeto B
    // num rascunho do projeto A) — o executor do teste isola e marca error.
    // O claim pega qualquer pending (ordem por cell_id): 2 células, 2 rodadas.
    for _ in 0..2 {
        let mut tx = pool.begin().await.expect("tx");
        let claimed = PgTaskRepo::claim_on(&mut *tx, &task.id, 1)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1);
        let input = CellInput::new(
            &claimed[0].cell_id,
            BTreeMap::from([("ndvi".to_owned(), 0.9)]),
        );
        let outputs = classify_batch(&[input], &rules);
        let class_id = outputs[0]
            .decision
            .class_id
            .clone()
            .expect("classe invasora");
        let write = PgTaskRepo::draft_upsert_on(
            &mut *tx,
            &draft.id,
            &ruleset_id,
            &[(grid.clone(), claimed[0].cell_id.clone(), class_id)],
        )
        .await;
        assert!(write.is_err(), "draft cross-project deveria ser recusado");
        tx.rollback().await.expect("rollback");
        // Caminho do executor: erro isolado no processo, com o motivo.
        let mut etx = pool.begin().await.expect("etx");
        let marked = PgTaskRepo::claim_and_error_on(
            &mut etx,
            &task.id,
            &claimed[0].cell_id,
            "draft_cells cross-project rejeitado",
        )
        .await
        .expect("marca erro");
        etx.commit().await.expect("commit erro");
        assert!(marked.is_some());
    }

    let counts = tasks.counts(&task.id).await.expect("counts");
    assert_eq!((counts.error, counts.done), (2, 0));
    let last_errors: Vec<Option<String>> =
        sqlx::query_scalar("SELECT last_error FROM processes WHERE task_id = $1::uuid")
            .bind(&task.id)
            .fetch_all(&pool)
            .await
            .expect("last_error");
    assert!(
        last_errors
            .iter()
            .all(|e| e.as_deref().unwrap_or("").contains("cross-project"))
    );
    assert_eq!(
        tasks.refresh_status(&task.id).await.expect("refresh"),
        Some(TaskStatus::Failed),
        "erro sem trabalho restante = failed"
    );

    // Correção: regra invasora removida; `task retry` repõe e o run conclui.
    sqlx::query("DELETE FROM rules WHERE id = $1::uuid")
        .bind(&bad_rule.0)
        .execute(&pool)
        .await
        .expect("remove invasora");
    let mut tx = pool.begin().await.expect("tx retry");
    let reset = PgTaskRepo::retry_errors_on(&mut *tx, &task.id)
        .await
        .expect("retry");
    tx.commit().await.expect("commit retry");
    assert_eq!(reset, 2);
    assert_eq!(
        tasks
            .refresh_status(&task.id)
            .await
            .expect("refresh pós-retry"),
        Some(TaskStatus::Pending)
    );

    let good_rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .expect("regras boas");
    loop {
        if run_batch(
            &pool,
            &task.id,
            &grid,
            &draft.id,
            &ruleset_id,
            &good_rules,
            2,
        )
        .await
        .expect("lote pós-retry")
            == 0
        {
            break;
        }
    }
    let counts = tasks.counts(&task.id).await.expect("counts finais");
    assert_eq!(counts.done, 2);
    assert_eq!(counts.error, 0);
    assert_eq!(
        tasks.refresh_status(&task.id).await.expect("refresh final"),
        Some(TaskStatus::Done)
    );

    cleanup(&pool, &project_a.id).await;
    cleanup(&pool, &project_b.id).await;
}

#[tokio::test]
async fn triggers_f5_rejeitam_cross_project_com_mensagem_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project_a, _, _) = make_grid(&pool, "f5-a", 1, 1).await;
    let project_b = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("f5-b"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto B");
    let class_a = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project_a.id, "classe_a"))
        .await
        .expect("classe A");
    let class_b = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project_b.id, "classe_b"))
        .await
        .expect("classe B");
    let group_id: (String,) = sqlx::query_as(
        "INSERT INTO class_groups (project_id, name) VALUES ($1::uuid, $2) RETURNING id::text",
    )
    .bind(&project_a.id)
    .bind(unique_name("grupo"))
    .fetch_one(&pool)
    .await
    .expect("grupo A");
    let ruleset_a = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project_a.id, &unique_name("rs")))
        .await
        .expect("ruleset A");

    // 1) `rules.class_id` de outro projeto → recusado pelo gatilho.
    let err = sqlx::query(
        "INSERT INTO rules (ruleset_id, class_id, name, predicate) \
         VALUES ($1::uuid, $2::uuid, 'x', 'ndvi >= 0.6')",
    )
    .bind(&ruleset_a.id)
    .bind(&class_b.id)
    .execute(&pool)
    .await
    .expect_err("regra cross-project recusada");
    assert!(
        err.to_string().contains("cross-project"),
        "mensagem acionável ausente: {err}"
    );

    // 2) `classes.class_group_id` de outro projeto → recusado.
    let err = sqlx::query(
        "INSERT INTO classes (project_id, class_group_id, name) \
         VALUES ($1::uuid, $2::uuid, $3)",
    )
    .bind(&project_b.id)
    .bind(&group_id.0)
    .bind(unique_name("classe_err"))
    .execute(&pool)
    .await
    .expect_err("grupo cross-project recusado");
    assert!(err.to_string().contains("cross-project"), "{err}");

    // 3) `classes.parent_id` de outro projeto → recusado.
    let err = sqlx::query(
        "INSERT INTO classes (project_id, parent_id, name) VALUES ($1::uuid, $2::uuid, $3)",
    )
    .bind(&project_b.id)
    .bind(&class_a.id)
    .bind(unique_name("classe_err2"))
    .execute(&pool)
    .await
    .expect_err("mãe cross-project recusada");
    assert!(err.to_string().contains("cross-project"), "{err}");

    // 4) Mesmo projeto passa (o gatilho não atrapalha o caminho legítimo).
    sqlx::query(
        "INSERT INTO rules (ruleset_id, class_id, name, predicate) \
         VALUES ($1::uuid, $2::uuid, 'ok', 'ndvi >= 0.6')",
    )
    .bind(&ruleset_a.id)
    .bind(&class_a.id)
    .execute(&pool)
    .await
    .expect("regra legítima passa pelo gatilho");

    cleanup(&pool, &project_a.id).await;
    cleanup(&pool, &project_b.id).await;
}

#[tokio::test]
async fn ruleset_activate_troca_ativo_e_indice_impede_duas() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("f3-activate"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto");
    let repo = PgRuleSetRepo::new(pool.clone());
    let a = repo
        .create(&RuleSet::new(&project.id, "alfa"))
        .await
        .expect("ruleset alfa");
    let b = repo
        .create(&RuleSet::new(&project.id, "beta"))
        .await
        .expect("ruleset beta");

    assert!(
        repo.find_active(&project.id)
            .await
            .expect("active")
            .is_none()
    );

    repo.activate(&a.id)
        .await
        .expect("ativa alfa")
        .expect("alfa");
    let active = repo
        .find_active(&project.id)
        .await
        .expect("active")
        .expect("ativo");
    assert_eq!(active.id, a.id);

    // Ativar beta DESATIVA alfa (uma ativa por projeto).
    repo.activate(&b.id)
        .await
        .expect("ativa beta")
        .expect("beta");
    let active = repo
        .find_active(&project.id)
        .await
        .expect("active")
        .expect("ativo");
    assert_eq!(active.id, b.id);
    let a_is_active: bool =
        sqlx::query_scalar("SELECT is_active FROM rulesets WHERE id = $1::uuid")
            .bind(&a.id)
            .fetch_one(&pool)
            .await
            .expect("is_active alfa");
    assert!(!a_is_active, "alfa deveria ter sido desativado");

    // SQL direto tentando DUAS ativas → índice parcial único recusa.
    let err = sqlx::query("UPDATE rulesets SET is_active = true WHERE id = $1::uuid")
        .bind(&a.id)
        .execute(&pool)
        .await
        .expect_err("segunda ativa recusada pelo índice parcial");
    assert!(
        err.to_string()
            .contains("rulesets_one_active_per_project_idx")
            || err.to_string().to_lowercase().contains("unique"),
        "esperava violação do índice parcial único: {err}"
    );

    cleanup(&pool, &project.id).await;
}

/// M1 — pré-checagem de `draft_cells` nos deletes de classe/ruleset (FK
/// RESTRICT da 0013): o delete simples RECUSA com erro tipado acionável e o
/// `delete_force` apaga decisões + regras + objeto numa única transação,
/// com AS TRÊS CONTAGENS reportadas.
#[tokio::test]
async fn deletes_com_draft_cells_recusam_e_force_reporta_tres_contagens() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, grid, cell_ids) = make_grid(&pool, "f3-del", 2, 1).await;
    let (ruleset_id, class_id) = make_ruleset_one_rule(&pool, &project.id, "floresta", 1).await;
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(&DraftLayer::new(&project.id, &unique_name("draft")))
        .await
        .expect("rascunho");

    // Ambas as células classificam → 2 draft_cells decididas pela classe e
    // pelo ruleset.
    for cell_id in &cell_ids {
        let mut tx = pool.begin().await.expect("tx");
        PgTaskRepo::upsert_measures_on(
            &mut *tx,
            &[ta_core::classification::CellMeasure {
                cell_layer_id: grid.clone(),
                cell_id: cell_id.clone(),
                measure: "ndvi".into(),
                value: 0.8,
                source: Some("teste".into()),
                updated_on: time::OffsetDateTime::now_utc(),
            }],
        )
        .await
        .expect("medidas");
        tx.commit().await.expect("commit");
    }
    let task = make_task(&pool, &project.id, &grid, &ruleset_id, &draft.id).await;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .expect("regras");
    loop {
        let n = run_batch(&pool, &task.id, &grid, &draft.id, &ruleset_id, &rules, 10)
            .await
            .expect("lote");
        if n == 0 {
            break;
        }
    }
    let draft_cells: i64 =
        sqlx::query_scalar("SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid")
            .bind(&draft.id)
            .fetch_one(&pool)
            .await
            .expect("draft_cells");
    assert_eq!(draft_cells, 2, "duas células classificadas no rascunho");

    // Pré-checagens do repositório: contagem por rascunho com NOME.
    let classes = PgClassRepo::new(pool.clone());
    let rulesets = PgRuleSetRepo::new(pool.clone());
    let drafts_da_classe = classes
        .count_drafts_for_class(&class_id)
        .await
        .expect("count_drafts_for_class");
    assert_eq!(
        drafts_da_classe,
        vec![(draft.name.clone(), 2)],
        "um rascunho, duas células"
    );
    let drafts_do_ruleset = rulesets
        .count_drafts_for_ruleset(&ruleset_id)
        .await
        .expect("count_drafts_for_ruleset");
    assert_eq!(drafts_do_ruleset, vec![(draft.name.clone(), 2)]);

    // Deletes simples RECUSAM (com regras E draft_cells, qualquer uma das
    // duas FKs RESTRICT pode disparar primeiro — ambas são acionáveis).
    let err = classes
        .delete(&class_id)
        .await
        .expect_err("classe com vínculos recusa");
    assert!(
        matches!(
            &err,
            ta_db::Error::ClassInUseByRules { .. } | ta_db::Error::ClassInUseByDraftCells { .. }
        ),
        "esperava erro tipado de uso, veio {err:?}"
    );
    assert!(err.to_string().contains("--force"));
    let err = rulesets
        .delete(&ruleset_id)
        .await
        .expect_err("ruleset com draft_cells recusa");
    match &err {
        ta_db::Error::RuleSetInUseByDraftCells { count, .. } => {
            assert_eq!(*count, 2);
        }
        other => panic!("esperava RuleSetInUseByDraftCells, veio {other:?}"),
    }
    assert!(err.to_string().contains("draft delete"));
    // Nada foi apagado pelos deletes recusados (contagens escopadas ao
    // fixture — o cluster é compartilhado entre testes em paralelo).
    let still: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid), \
            (SELECT count(*) FROM rules WHERE ruleset_id = $2::uuid), \
            (SELECT count(*) FROM classes WHERE id = $3::uuid), \
            (SELECT count(*) FROM rulesets WHERE id = $2::uuid)",
    )
    .bind(&draft.id)
    .bind(&ruleset_id)
    .bind(&class_id)
    .fetch_one(&pool)
    .await
    .expect("estado após recusas");
    assert_eq!(still, (2, 1, 1, 1), "recusa não pode apagar nada");

    // Discriminação da FK: sem regras, o bloqueio restante é SOMENTE o das
    // decisões de rascunho — o erro aponta `draft delete` como saída.
    sqlx::query("DELETE FROM rules WHERE class_id = $1::uuid")
        .bind(&class_id)
        .execute(&pool)
        .await
        .expect("remove regras para isolar a FK de draft_cells");
    let err = classes
        .delete(&class_id)
        .await
        .expect_err("classe com draft_cells recusa mesmo sem regras");
    match &err {
        ta_db::Error::ClassInUseByDraftCells { count, .. } => {
            assert_eq!(*count, 2, "contagem de draft_cells no erro");
        }
        other => panic!("esperava ClassInUseByDraftCells, veio {other:?}"),
    }
    assert!(err.to_string().contains("draft delete"));

    // `--force` do ruleset: draft_cells + ruleset numa transação (as regras
    // já saíram no passo de isolamento — contagem 0). A contagem de regras
    // no `--force` é provada ponta a ponta pelo teste CLI
    // `ruleset_delete_com_draft_cells_bloqueia_e_force_reporta_tres_contagens`.
    let report = rulesets
        .delete_force(&ruleset_id)
        .await
        .expect("ruleset delete_force");
    assert_eq!(
        report,
        ta_db::repo_ruleset::RuleSetForceDeleted {
            draft_cells: 2,
            rules: 0,
            ruleset: true
        }
    );
    // `--force` da classe: sobraram 0 decisões e 0 regras — apaga limpa.
    let report = classes.delete_force(&class_id).await.expect("class force");
    assert_eq!(
        report,
        ta_db::repo_class::ClassForceDeleted {
            draft_cells: 0,
            rules: 0,
            class: true
        }
    );
    let left: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid), \
            (SELECT count(*) FROM rules WHERE ruleset_id = $2::uuid), \
            (SELECT count(*) FROM classes WHERE id = $3::uuid), \
            (SELECT count(*) FROM rulesets WHERE id = $2::uuid)",
    )
    .bind(&draft.id)
    .bind(&ruleset_id)
    .bind(&class_id)
    .fetch_one(&pool)
    .await
    .expect("estado final");
    assert_eq!(left, (0, 0, 0, 0), "force apaga tudo atomicamente");

    cleanup(&pool, &project.id).await;
}
