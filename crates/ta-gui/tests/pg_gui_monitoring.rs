//! Comandos de banco do **monitoramento da GUI** (Fase 5b) — **gated** por
//! `TA_TEST_PG=1` (mesmo harness de `scripts/test-pg.sh`, porta 55432).
//!
//! Exercita o `db_worker` de ponta a ponta (thread real, runtime multi-thread,
//! canais reais) — os mesmos comandos que a janela Monitoramento envia:
//!
//! - **B1**: fluxo completo pela GUI (fase → AOI → grade → ruleset → medidas
//!   CSV → classificação → células do rascunho → área) comparado, número a
//!   número, com `ta-cli area report --json`;
//! - **M1/M2**: run longo não ocupa o loop (viewport responde durante o run) e
//!   o cancelamento cooperativo para entre lotes, deixando `pending`;
//! - **m7**: caminho feliz do worker (`TaskCreated{total}` → progresso →
//!   `Finished(Done)` → `draft_cells`);
//! - **m6**: exchange no binário da CLI (export real) e binário ausente
//!   (`CURUPIRA_TA_CLI` explícito) com erro acionável.
//!
//! Sem `TA_TEST_PG=1` os testes imprimem "skipped" e passam.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use geo_types::{Geometry, LineString, Polygon};
use serde_json::json;
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::area::AreaReport;
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::classification::Class;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_core::rule::{Rule, RuleSet};
use ta_core::task::TaskStatus;
use ta_db::repo::{
    FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectFlowSettings,
    ProjectRepo,
};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellViewRow, PgCellLayerRepo, PgCellRepo};
use ta_db::repo_class::{ClassRepo, PgClassRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::{PgTaskRepo, TaskRepo};
use ta_db::spatial::run_migrations;
use ta_flow::queue::{RunOutcome, RunReport};
use ta_gui::db_worker::{DbCmd, DbEvt, DbWorker, ExchangeMode, ExchangeRequest};
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

/// Arquivo temporário próprio por teste (sem colidir entre execuções).
fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{}-{name}", unique_name("ta-gui")))
}

/// `ta-cli` do workspace: `CARGO_BIN_EXE_ta-cli` (quando definido), env
/// explícita ou o binário ao lado do executável de teste (`target/debug`).
fn ta_cli_bin() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_ta-cli") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    if let Some(path) = std::env::var_os("TA_TEST_TA_CLI") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    let exe = std::env::current_exe().expect("executável do teste");
    // `target/debug/deps/<test>` → `target/debug/ta-cli`.
    for candidate in [
        exe.parent().map(|d| d.join("ta-cli")),
        exe.parent()
            .and_then(|d| d.parent())
            .map(|d| d.join("ta-cli")),
    ]
    .into_iter()
    .flatten()
    {
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "ta-cli não encontrado: rode `cargo test --workspace` (os bins do \
         workspace são construídos) ou defina TA_TEST_TA_CLI"
    );
}

/// Roda o `ta-cli area report --json` (contrato de números da CLI).
fn cli_area_report(cell_layer_id: &str, draft: &str) -> AreaReport {
    let output = std::process::Command::new(ta_cli_bin())
        .args([
            "area",
            "report",
            "--cell-layer",
            cell_layer_id,
            "--draft",
            draft,
            "--json",
        ])
        .env("TA_PG_DSN", dsn())
        .output()
        .expect("executar ta-cli area report");
    assert!(
        output.status.success(),
        "ta-cli area report falhou: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<AreaReport>(&output.stdout).expect("JSON do AreaReport")
}

/// Projeto com `flow_settings` (o exchange export exige as settings).
async fn make_project(pool: &PgPool, prefix: &str) -> Project {
    PgProjectRepo::new(pool.clone())
        .create_with_flow_settings(
            &Project {
                id: String::new(),
                name: unique_name(prefix),
                srid: 4674,
                created_at: time::OffsetDateTime::now_utc(),
            },
            &ProjectFlowSettings {
                active: false,
                min_scale: None,
                max_scale: None,
                max_cells: 1_000_000,
                description: String::new(),
            },
        )
        .await
        .expect("cria projeto")
}

/// Camada vetorial poligonal com UMA feição (a AOI crua, antes do vínculo).
async fn add_polygon_layer(pool: &PgPool, project: &Project, poly: Polygon<f64>) -> String {
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: 4674,
            source_uri: "test://gui-monitoring".into(),
        })
        .await
        .expect("cria camada de AOI");
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(Geometry::Polygon(poly), json!({}))],
        )
        .await
        .expect("insere feição da AOI");
    layer.id
}

/// Classe + ruleset v1 com `ndvi >= 0.5`, devolvendo `(class_id, nome)`.
async fn add_class_and_ruleset(pool: &PgPool, project: &Project) -> (String, String, i32) {
    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project.id, "floresta").with_color(Some("#12AB34")))
        .await
        .expect("cria classe");
    let name = unique_name("regras");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, &name))
        .await
        .expect("cria ruleset");
    PgRuleRepo::new(pool.clone())
        .create(&Rule::new(
            &ruleset.id,
            &class.id,
            "ndvi-alto",
            "ndvi >= 0.5",
        ))
        .await
        .expect("cria regra");
    (class.id, name, ruleset.version)
}

/// Vincula a AOI e cria a grade `cols × rows` (células `<col>_<row>`).
async fn add_grid(
    pool: &PgPool,
    project: &Project,
    aoi_layer_id: &str,
    cols: i32,
    rows: i32,
    resolution: f64,
) -> (String, CellLayer) {
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, aoi_layer_id, None))
        .await
        .expect("cria vínculo de AOI");
    let spec = GridSpec::new(resolution, -52.30, -3.30, 4674);
    let cell_layer = CellLayer::new(&project.id, &aoi.id, &unique_name("grade"), &spec);
    let mut cells = Vec::with_capacity(usize::try_from(cols * rows).unwrap_or(0));
    for row in 0..rows {
        for col in 0..cols {
            let x = -52.30 + f64::from(col) * resolution;
            let y = -3.30 + f64::from(row) * resolution;
            cells.push(Cell {
                id: 0,
                cell_layer_id: String::new(),
                cell_id: format!("{col}_{row}"),
                geom: rect(x, y, x + resolution, y + resolution),
                row_index: row,
                col_index: col,
            });
        }
    }
    let mut tx = pool.begin().await.expect("begin");
    let saved = PgCellLayerRepo::create_on(&mut *tx, &cell_layer)
        .await
        .expect("cria grade");
    PgCellRepo::bulk_insert_on(&mut tx, &saved.id, &cells, 4674)
        .await
        .expect("insere células");
    tx.commit().await.expect("commit");
    (aoi.id, saved)
}

/// CSV LONG de medidas (`cell_id,measure,value`) para as células informadas.
fn write_measures(path: &PathBuf, values: &[(String, f64)]) {
    let file = std::fs::File::create(path).expect("criar CSV");
    let mut out = std::io::BufWriter::new(file);
    writeln!(out, "cell_id,measure,value").expect("cabeçalho");
    for (cell_id, value) in values {
        writeln!(out, "{cell_id},ndvi,{value}").expect("linha");
    }
    out.flush().expect("flush");
}

/// Eventos do worker com **acumulação** (n4): um evento que não interessa a
/// um `wait_for` fica para o próximo, em vez de ser descartado; o timeout
/// lista o que estava acumulado (diagnóstico, não "timeout" seco).
struct WorkerEvents {
    worker: DbWorker,
    pending: Vec<DbEvt>,
}

impl WorkerEvents {
    fn new(worker: DbWorker) -> Self {
        Self {
            worker,
            pending: Vec::new(),
        }
    }

    fn wait_for(&mut self, what: &str, pred: impl Fn(&DbEvt) -> bool) -> DbEvt {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(pos) = self.pending.iter().position(&pred) {
                return self.pending.remove(pos);
            }
            self.pending.extend(self.worker.drain());
            assert!(
                Instant::now() < deadline,
                "timeout esperando {what}; eventos acumulados: {:#?}",
                self.pending
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Todos os eventos do run até `TaskFinished` (progresso incluso). Devolve
    /// também o `TaskCreated` (id + total), que interessa a quem disparou.
    fn run_to_finish(
        &mut self,
    ) -> (
        Option<(String, u64)>,
        Vec<ta_flow::queue::RunEvent>,
        RunOutcome,
    ) {
        let mut created = None;
        let mut progress = Vec::new();
        loop {
            let evt = self.wait_for("run da task terminar", |e| {
                matches!(
                    e,
                    DbEvt::TaskCreated { .. }
                        | DbEvt::TaskProgress(_)
                        | DbEvt::TaskFinished(_)
                        | DbEvt::TaskFailed(_)
                )
            });
            match evt {
                DbEvt::TaskProgress(event) => progress.push(event),
                DbEvt::TaskFinished(outcome) => return (created, progress, outcome),
                DbEvt::TaskFailed(msg) => panic!("run falhou: {msg}"),
                DbEvt::TaskCreated { task_id, total, .. } => {
                    created = Some((task_id, total));
                }
                other => unreachable!("evento inesperado: {other:?}"),
            }
        }
    }
}

fn completed(outcome: &RunOutcome) -> &RunReport {
    match outcome {
        RunOutcome::Completed(report) => report,
        other => panic!("esperava RunOutcome::Completed, veio {other:?}"),
    }
}

async fn count_draft_cells(pool: &PgPool, draft_layer_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid")
        .bind(draft_layer_id)
        .fetch_one(pool)
        .await
        .expect("contar draft_cells")
}

/// Cleanup pelo caminho da CLI (`project delete --yes`): o repositório só faz
/// `DELETE FROM projects` e o cascade de `projects → classes` pode colidir com
/// `rules.class_id` (FK sem cascade) — a CLI faz a deleção ordenada.
fn delete_projects(projects: &[&Project]) {
    for project in projects {
        let output = std::process::Command::new(ta_cli_bin())
            .args(["project", "delete", "--project", &project.name, "--yes"])
            .env("TA_PG_DSN", dsn())
            .output()
            .expect("executar ta-cli project delete");
        assert!(
            output.status.success(),
            "cleanup do projeto `{}` falhou: {}",
            project.name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------

#[test]
fn comandos_de_monitoramento_da_gui_ponta_a_ponta() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de setup");
    let (project, cell_layer) = rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn())
            .await
            .expect("conectar no PG do harness");
        let project = make_project(&pool, "gui-monitor").await;
        let aoi_layer =
            add_polygon_layer(&pool, &project, rect(-52.30, -3.30, -52.28, -3.29)).await;
        let (_aoi_id, cell_layer) = add_grid(&pool, &project, &aoi_layer, 2, 1, 0.01).await;
        // Classe sem ruleset: a classificação precisa falhar com mensagem.
        PgClassRepo::new(pool.clone())
            .create(&Class::new(&project.id, &unique_name("floresta")).with_color(Some("#12AB34")))
            .await
            .expect("cria classe");
        (project, cell_layer)
    });
    let pool = rt
        .block_on(sqlx::PgPool::connect(&dsn()))
        .expect("pool do assert");

    let mut events = WorkerEvents::new(DbWorker::spawn());
    events.worker.send(DbCmd::Connect { url: dsn() });
    events.wait_for("conectar", |e| {
        matches!(e, DbEvt::Connected | DbEvt::ConnectFailed(_))
    });

    // 1) Listas do projeto (o que a janela mostra).
    events.worker.send(DbCmd::LoadMonitoring {
        project_id: project.id.clone(),
    });
    let DbEvt::MonitoringLoaded(data) = events.wait_for("MonitoringLoaded", |e| {
        matches!(e, DbEvt::MonitoringLoaded(_))
    }) else {
        panic!("esperava MonitoringLoaded");
    };
    assert_eq!(data.project_id, project.id);
    assert!(data.cell_layers.iter().any(|l| l.id == cell_layer.id));
    assert_eq!(data.classes.len(), 1);
    assert_eq!(data.classes[0].color.as_deref(), Some("#12AB34"));
    assert!(data.active_ruleset.is_none());

    // 2) Células da viewport (bbox da GUI + LIMIT no servidor).
    events.worker.send(DbCmd::LoadCellView {
        seq: 7,
        cell_layer_id: cell_layer.id.clone(),
        draft_layer_id: None,
        bbox: [-52.31, -3.31, -52.27, -3.28],
    });
    let DbEvt::CellViewLoaded {
        seq,
        cells,
        truncated,
    } = events.wait_for("CellViewLoaded seq=7", |e| {
        matches!(e, DbEvt::CellViewLoaded { seq: 7, .. })
    })
    else {
        panic!("esperava CellViewLoaded");
    };
    assert_eq!(seq, 7);
    assert_eq!(cells.len(), 2);
    assert!(!truncated);
    assert!(cells.iter().all(|c| c.class_id.is_none() && !c.blocked));

    // 3) Bloqueio persistido e visível na consulta seguinte.
    events.worker.send(DbCmd::SetCellBlocked {
        cell_layer_id: cell_layer.id.clone(),
        cell_id: "0_0".into(),
        blocked: true,
    });
    assert!(matches!(
        events.wait_for("CellBlocked", |e| matches!(e, DbEvt::CellBlocked { .. })),
        DbEvt::CellBlocked { blocked: true, .. }
    ));
    events.worker.send(DbCmd::LoadCellView {
        seq: 8,
        cell_layer_id: cell_layer.id.clone(),
        draft_layer_id: None,
        bbox: [-52.31, -3.31, -52.27, -3.28],
    });
    let DbEvt::CellViewLoaded { cells, .. } = events.wait_for("CellViewLoaded seq=8", |e| {
        matches!(e, DbEvt::CellViewLoaded { seq: 8, .. })
    }) else {
        panic!("esperava a viewport seq=8");
    };
    let blocked = cells.iter().find(|c| c.cell_id == "0_0").expect("célula");
    assert!(blocked.blocked, "bloqueio vem de blocked_cells");

    // Desbloquear volta ao estado inicial.
    events.worker.send(DbCmd::SetCellBlocked {
        cell_layer_id: cell_layer.id.clone(),
        cell_id: "0_0".into(),
        blocked: false,
    });
    assert!(matches!(
        events.wait_for("desbloqueio", |e| matches!(e, DbEvt::CellBlocked { .. })),
        DbEvt::CellBlocked { blocked: false, .. }
    ));

    // 4) Área: MESMO `ta_flow::area` da CLI (ha só na formatação).
    events.worker.send(DbCmd::AreaReport {
        seq: 3,
        cell_layer_id: cell_layer.id.clone(),
        draft_name: None,
    });
    let DbEvt::AreaReported { result, .. } = events.wait_for("AreaReported seq=3", |e| {
        matches!(e, DbEvt::AreaReported { seq: 3, .. })
    }) else {
        panic!("esperava AreaReported seq=3");
    };
    let report = result.expect("relatório de área");
    assert!(report.summary.total_m2 > 0.0);
    assert!(report.by_class.is_empty(), "sem rascunho não há por classe");
    assert_eq!(report.srs, 5880, "default métrico da Decisão 4");

    // 5) Classificar sem ruleset ativo → erro acionável (não silêncio).
    events.worker.send(DbCmd::ClassifyTask {
        project: project.clone(),
        cell_layer_id: cell_layer.id.clone(),
        draft_name: None,
        task_name: None,
    });
    let DbEvt::TaskFailed(message) = events.wait_for("TaskFailed", |e| {
        matches!(e, DbEvt::TaskFailed(_) | DbEvt::TaskCreated { .. })
    }) else {
        panic!("esperava TaskFailed");
    };
    assert!(
        message.to_lowercase().contains("ruleset"),
        "mensagem acionável: {message}"
    );

    events.worker.send(DbCmd::Disconnect);
    let _ = events.wait_for("disconnect", |e| matches!(e, DbEvt::Disconnected));
    delete_projects(&[&project]);
    rt.block_on(pool.close());
}

/// **B1** — o fluxo inteiro pela GUI, com os números conferidos contra a CLI.
#[test]
fn fluxo_completo_pela_gui_bate_com_a_cli() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de setup");
    let (project, aoi_layer, class_id) = rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn()).await.expect("conectar");
        let project = make_project(&pool, "gui-b1").await;
        let aoi_layer =
            add_polygon_layer(&pool, &project, rect(-52.20, -3.20, -52.18, -3.18)).await;
        let (class_id, _ruleset, _version) = add_class_and_ruleset(&pool, &project).await;
        (project, aoi_layer, class_id)
    });
    let pool = rt.block_on(sqlx::PgPool::connect(&dsn())).expect("pool");
    let mut events = WorkerEvents::new(DbWorker::spawn());
    events.worker.send(DbCmd::Connect { url: dsn() });
    events.wait_for("conectar", |e| {
        matches!(e, DbEvt::Connected | DbEvt::ConnectFailed(_))
    });

    // 1) Fase (ta_flow::phase::create).
    events.worker.send(DbCmd::CreatePhase {
        project_id: project.id.clone(),
        name: "2026-10".into(),
        description: "fluxo B1".into(),
    });
    let DbEvt::PhaseCreated(phase) =
        events.wait_for("fase criada", |e| matches!(e, DbEvt::PhaseCreated(_)))
    else {
        panic!("esperava PhaseCreated");
    };
    assert_eq!(phase.name, "2026-10");
    assert_eq!(phase.project_id, project.id);

    // 2) AOI = camada vetorial existente.
    events.worker.send(DbCmd::LinkAoi {
        project_id: project.id.clone(),
        layer_id: aoi_layer.clone(),
        id_column: Some("id".into()),
    });
    let DbEvt::AoiLinked { aoi_id, layer_name } =
        events.wait_for("AOI vinculada", |e| matches!(e, DbEvt::AoiLinked { .. }))
    else {
        panic!("esperava AoiLinked");
    };
    assert!(!layer_name.is_empty());

    // 3) Grade: 0.02° / 0.01° = 2×2 = 4 células.
    events.worker.send(DbCmd::CreateGrid {
        aoi_id,
        name: "grade-b1".into(),
        resolution: 0.01,
        srid: 4674,
    });
    let DbEvt::GridCreated {
        layer_id,
        name,
        resolution,
        srid,
        cells,
    } = events.wait_for("grade criada", |e| matches!(e, DbEvt::GridCreated { .. }))
    else {
        panic!("esperava GridCreated");
    };
    assert_eq!((name.as_str(), resolution, srid), ("grade-b1", 0.01, 4674));
    assert_eq!(cells, 4, "0.02° × 0.02° com resolução 0.01° = 4 células");

    // 4) Ruleset criado fora da GUI (a GUI só ATIVA — como na CLI).
    let ruleset = rt.block_on(async {
        PgRuleSetRepo::new(pool.clone())
            .list_for_project(&project.id)
            .await
            .expect("listar rulesets")
            .into_iter()
            .find(|r| r.version == 1)
            .expect("ruleset v1")
    });
    events.worker.send(DbCmd::ActivateRuleset {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        name: ruleset.name.clone(),
        version: ruleset.version,
    });
    let DbEvt::RulesetActivated { name, version } = events.wait_for("ruleset ativado", |e| {
        matches!(e, DbEvt::RulesetActivated { .. })
    }) else {
        panic!("esperava RulesetActivated");
    };
    assert_eq!((name.as_str(), version), (ruleset.name.as_str(), 1));

    // 5) Medidas CSV: 2 válidas (0_0, 1_0), 1 célula inexistente e 1 valor
    //    inválido → relatório honesto (lidas/gravadas/ignoradas).
    let csv = temp_file("medidas-b1.csv");
    {
        let file = std::fs::File::create(&csv).expect("criar CSV");
        let mut out = std::io::BufWriter::new(file);
        writeln!(out, "cell_id,measure,value").expect("cabeçalho");
        writeln!(out, "0_0,ndvi,0.71").expect("linha");
        writeln!(out, "1_0,ndvi,0.65").expect("linha");
        writeln!(out, "9_9,ndvi,0.7").expect("linha");
        writeln!(out, "0_1,ndvi,muito").expect("linha");
        out.flush().expect("flush");
    }
    events.worker.send(DbCmd::ImportMeasures {
        cell_layer_id: layer_id.clone(),
        path: csv.to_string_lossy().into_owned(),
    });
    let DbEvt::MeasuresImported(report) = events.wait_for("medidas importadas", |e| {
        matches!(e, DbEvt::MeasuresImported(_))
    }) else {
        panic!("esperava MeasuresImported");
    };
    assert_eq!(
        (report.read, report.written, report.skipped),
        (4, 2, 2),
        "relatório do import: {report:?}"
    );
    assert_eq!(report.issues.len(), 2);

    // 6) Classificar: 4 células elegíveis; 2 casam `ndvi >= 0.5`.
    events.worker.send(DbCmd::ClassifyTask {
        project: project.clone(),
        cell_layer_id: layer_id.clone(),
        draft_name: Some("rascunho-b1".into()),
        task_name: None,
    });
    let (created, progress, outcome) = events.run_to_finish();
    let (_, total) = created.expect("TaskCreated antes do desfecho");
    let report = completed(&outcome);
    assert_eq!(total, 4, "todas as células não bloqueadas são elegíveis");
    assert_eq!(report.processed, 4);
    assert_eq!(report.errors, 0);
    assert_eq!(report.final_status, Some(TaskStatus::Done));
    let draft_rows: u64 = progress
        .iter()
        .map(|e| match e {
            ta_flow::queue::RunEvent::BatchOk { draft_rows, .. } => *draft_rows,
            _ => 0,
        })
        .sum();
    assert_eq!(draft_rows, 2, "só as células com medida casam a regra");

    // 7) Rascunho e células classificadas (o que o canvas pinta).
    events.worker.send(DbCmd::LoadMonitoring {
        project_id: project.id.clone(),
    });
    let DbEvt::MonitoringLoaded(data) = events.wait_for("MonitoringLoaded pós-run", |e| {
        matches!(e, DbEvt::MonitoringLoaded(_))
    }) else {
        panic!("esperava MonitoringLoaded");
    };
    let draft = data
        .drafts
        .iter()
        .find(|d| d.name == "rascunho-b1")
        .expect("rascunho criado pelo run");
    assert_eq!(draft.cell_layer_id.as_deref(), Some(layer_id.as_str()));
    assert_eq!(
        data.active_ruleset.as_ref().map(|r| r.name.as_str()),
        Some(ruleset.name.as_str())
    );
    assert_eq!(rt.block_on(count_draft_cells(&pool, &draft.id)), 2);

    events.worker.send(DbCmd::LoadCellView {
        seq: 21,
        cell_layer_id: layer_id.clone(),
        draft_layer_id: Some(draft.id.clone()),
        bbox: [-52.21, -3.21, -52.17, -3.17],
    });
    let DbEvt::CellViewLoaded { cells, .. } = events.wait_for("viewport com rascunho", |e| {
        matches!(e, DbEvt::CellViewLoaded { seq: 21, .. })
    }) else {
        panic!("esperava CellViewLoaded seq=21");
    };
    let classified: Vec<&CellViewRow> = cells
        .iter()
        .filter(|c| c.class_id.as_deref() == Some(class_id.as_str()))
        .collect();
    assert_eq!(classified.len(), 2, "2 células verdes no canvas");
    assert!(classified.iter().any(|c| c.cell_id == "0_0"));

    // 8) Área: GUI e CLI produzem o MESMO `AreaReport` (número a número).
    events.worker.send(DbCmd::AreaReport {
        seq: 31,
        cell_layer_id: layer_id.clone(),
        draft_name: Some("rascunho-b1".into()),
    });
    let DbEvt::AreaReported { result, .. } = events.wait_for("área da GUI", |e| {
        matches!(e, DbEvt::AreaReported { seq: 31, .. })
    }) else {
        panic!("esperava AreaReported seq=31");
    };
    let gui_report = result.expect("relatório da GUI");
    let cli_report = cli_area_report(&layer_id, "rascunho-b1");
    assert_eq!(gui_report, cli_report, "GUI e CLI no mesmo contrato");
    assert_eq!(gui_report.summary.cells_total, 4);
    assert_eq!(gui_report.by_class.len(), 1);
    assert_eq!(gui_report.by_class[0].class_name, "floresta");
    assert_eq!(gui_report.by_class[0].cells, 2);
    let half = gui_report.summary.total_m2 / 2.0;
    // Tolerância de 0,1%: as bordas das células são retas no SRS do cálculo
    // (paralelos curvos no elipsoide), então a área planar não é exatamente
    // metade — a diferença medida é ~5e-6 relativa.
    assert!(
        (gui_report.by_class[0].area_m2 - half).abs() <= half.abs() * 1e-3,
        "duas das quatro células (Δ {:e})",
        gui_report.by_class[0].area_m2 - half
    );
    assert_eq!(
        gui_report.summary.available_m2,
        gui_report.summary.total_m2 - gui_report.summary.blocked_m2
    );

    events.worker.send(DbCmd::Disconnect);
    let _ = events.wait_for("disconnect", |e| matches!(e, DbEvt::Disconnected));
    std::fs::remove_file(&csv).ok();
    delete_projects(&[&project]);
    rt.block_on(pool.close());
}

/// **m7** — caminho feliz do worker: total, progresso, desfecho e linhas.
#[test]
fn worker_classifica_duas_celulas_e_grava_duas_decisoes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de setup");
    let (project, cell_layer, class_id) = rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn()).await.expect("conectar");
        let project = make_project(&pool, "gui-m7").await;
        let aoi_layer =
            add_polygon_layer(&pool, &project, rect(-52.30, -3.30, -52.28, -3.29)).await;
        let (_, cell_layer) = add_grid(&pool, &project, &aoi_layer, 2, 1, 0.01).await;
        let (class_id, _, _) = add_class_and_ruleset(&pool, &project).await;
        (project, cell_layer, class_id)
    });
    let pool = rt.block_on(sqlx::PgPool::connect(&dsn())).expect("pool");
    rt.block_on(async {
        let ruleset = PgRuleSetRepo::new(pool.clone())
            .list_for_project(&project.id)
            .await
            .expect("rulesets")
            .into_iter()
            .next()
            .expect("ruleset");
        PgRuleSetRepo::new(pool.clone())
            .activate(&ruleset.id)
            .await
            .expect("ativar ruleset");
    });
    let csv = temp_file("medidas-m7.csv");
    write_measures(&csv, &[("0_0".into(), 0.7), ("1_0".into(), 0.9)]);
    rt.block_on(async {
        let report = ta_flow::measures::import_csv(&pool, &cell_layer.id, &csv.to_string_lossy())
            .await
            .expect("import de medidas");
        assert_eq!((report.read, report.written), (2, 2));
    });

    let mut events = WorkerEvents::new(DbWorker::spawn());
    events.worker.send(DbCmd::Connect { url: dsn() });
    events.wait_for("conectar", |e| {
        matches!(e, DbEvt::Connected | DbEvt::ConnectFailed(_))
    });
    events.worker.send(DbCmd::ClassifyTask {
        project: project.clone(),
        cell_layer_id: cell_layer.id.clone(),
        draft_name: Some("rascunho-m7".into()),
        task_name: None,
    });
    let (created, progress, outcome) = events.run_to_finish();
    let (_, total) = created.expect("TaskCreated antes do desfecho");
    assert_eq!(total, 2, "TaskCreated{{total:2}}");
    assert!(!progress.is_empty(), "progresso chegou antes do desfecho");
    let report = completed(&outcome);
    assert_eq!(report.processed, 2);
    assert_eq!(report.final_status, Some(TaskStatus::Done));

    let draft_id = rt.block_on(async {
        sqlx::query_scalar::<_, String>(
            "SELECT id::text FROM draft_layers WHERE project_id = $1::uuid AND name = $2",
        )
        .bind(&project.id)
        .bind("rascunho-m7")
        .fetch_one(&pool)
        .await
        .expect("rascunho do run")
    });
    assert_eq!(rt.block_on(count_draft_cells(&pool, &draft_id)), 2);
    let classes: Vec<String> = rt.block_on(async {
        sqlx::query_scalar("SELECT class_id::text FROM draft_cells WHERE draft_layer_id = $1::uuid")
            .bind(&draft_id)
            .fetch_all(&pool)
            .await
            .expect("decisões")
    });
    assert!(
        classes.iter().all(|c| c == &class_id),
        "as duas decisões são da classe da regra: {classes:?}"
    );

    events.worker.send(DbCmd::Disconnect);
    let _ = events.wait_for("disconnect", |e| matches!(e, DbEvt::Disconnected));
    std::fs::remove_file(&csv).ok();
    delete_projects(&[&project]);
    rt.block_on(pool.close());
}

/// **M1/M2** — run longo não bloqueia o loop; cancelar para entre lotes.
#[test]
fn run_longo_responde_a_viewport_e_cancela_entre_lotes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de setup");
    const SIDE: i32 = 200; // 200×200 = 40.000 células (run de segundos)
    let (project, cell_layer) = rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn()).await.expect("conectar");
        let project = make_project(&pool, "gui-long").await;
        let aoi_layer =
            add_polygon_layer(&pool, &project, rect(-52.30, -3.30, -52.10, -3.10)).await;
        let (_, cell_layer) = add_grid(&pool, &project, &aoi_layer, SIDE, SIDE, 0.001).await;
        let (_, _, _) = add_class_and_ruleset(&pool, &project).await;
        let ruleset = PgRuleSetRepo::new(pool.clone())
            .list_for_project(&project.id)
            .await
            .expect("rulesets")
            .into_iter()
            .next()
            .expect("ruleset");
        PgRuleSetRepo::new(pool.clone())
            .activate(&ruleset.id)
            .await
            .expect("ativar");
        (project, cell_layer)
    });
    let pool = rt.block_on(sqlx::PgPool::connect(&dsn())).expect("pool");
    // Medidas em TODAS as células: o run grava 40.000 decisões (segundos).
    let csv = temp_file("medidas-long.csv");
    {
        let file = std::fs::File::create(&csv).expect("criar CSV");
        let mut out = std::io::BufWriter::new(file);
        writeln!(out, "cell_id,measure,value").expect("cabeçalho");
        for row in 0..SIDE {
            for col in 0..SIDE {
                writeln!(out, "{col}_{row},ndvi,0.7").expect("linha");
            }
        }
        out.flush().expect("flush");
    }
    rt.block_on(async {
        let report = ta_flow::measures::import_csv(&pool, &cell_layer.id, &csv.to_string_lossy())
            .await
            .expect("import de medidas");
        assert_eq!(report.written, u64::try_from(SIDE * SIDE).unwrap());
    });

    let mut events = WorkerEvents::new(DbWorker::spawn());
    events.worker.send(DbCmd::Connect { url: dsn() });
    events.wait_for("conectar", |e| {
        matches!(e, DbEvt::Connected | DbEvt::ConnectFailed(_))
    });
    events.worker.send(DbCmd::ClassifyTask {
        project: project.clone(),
        cell_layer_id: cell_layer.id.clone(),
        draft_name: Some("rascunho-long".into()),
        task_name: None,
    });
    let (task_id, total) = match events.wait_for("TaskCreated", |e| {
        matches!(e, DbEvt::TaskCreated { .. } | DbEvt::TaskFailed(_))
    }) {
        DbEvt::TaskCreated { task_id, total, .. } => (task_id, total),
        other => panic!("run não começou: {other:?}"),
    };
    assert_eq!(total, u64::try_from(SIDE * SIDE).unwrap());

    // M2: com o run em voo, a viewport responde (loop de comandos livre).
    events.worker.send(DbCmd::LoadCellView {
        seq: 41,
        cell_layer_id: cell_layer.id.clone(),
        draft_layer_id: None,
        bbox: [-52.31, -3.31, -52.09, -3.09],
    });
    let view = events.wait_for("viewport durante o run", |e| {
        matches!(
            e,
            DbEvt::CellViewLoaded { seq: 41, .. } | DbEvt::TaskFinished(_)
        )
    });
    assert!(
        matches!(view, DbEvt::CellViewLoaded { .. }),
        "a viewport respondeu ANTES do fim do run (o run não ocupa o loop): {view:?}"
    );

    // M1: cancelamento cooperativo enquanto o run está em voo.
    events.worker.send(DbCmd::CancelTask {
        task_id: task_id.clone(),
    });
    let DbEvt::TaskCancelled { already, .. } = events.wait_for("cancelamento aceito", |e| {
        matches!(e, DbEvt::TaskCancelled { .. } | DbEvt::CancelFailed(_))
    }) else {
        panic!("esperava TaskCancelled");
    };
    assert!(!already, "a task estava em voo (não cancelada antes)");

    // O run termina sozinho, no próximo limite de lote.
    let (_, _, outcome) = events.run_to_finish();
    let report = completed(&outcome);
    assert!(report.cancelled, "cancelamento observado entre lotes");
    assert_eq!(
        report.final_status, None,
        "cancelada é terminal: o status não é sobrescrito"
    );
    let counts = rt.block_on(async {
        PgTaskRepo::new(pool.clone())
            .counts(&task_id)
            .await
            .expect("contagens")
    });
    assert!(counts.done > 0, "lotes já commitados permanecem done");
    assert!(
        counts.pending > 0,
        "o resto fica pending (nunca é reclamado): {counts:?}"
    );
    assert_eq!(counts.running, 0, "nenhum processo fica em running");

    // Idempotência: cancelar de novo não é erro.
    events.worker.send(DbCmd::CancelTask {
        task_id: task_id.clone(),
    });
    assert!(
        matches!(
            events.wait_for("cancelamento idempotente", |e| matches!(
                e,
                DbEvt::TaskCancelled { .. } | DbEvt::CancelFailed(_)
            )),
            DbEvt::TaskCancelled { already: true, .. }
        ),
        "segundo cancelamento é idempotente"
    );

    events.worker.send(DbCmd::Disconnect);
    let _ = events.wait_for("disconnect", |e| matches!(e, DbEvt::Disconnected));
    std::fs::remove_file(&csv).ok();
    delete_projects(&[&project]);
    rt.block_on(pool.close());
}

/// **m6** — exchange no binário da CLI (`CURUPIRA_TA_CLI`) e binário ausente.
#[test]
fn exchange_usa_o_binario_da_cli_e_falha_com_caminho_inexistente() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de setup");
    let project = rt.block_on(async {
        setup().await;
        let pool = sqlx::PgPool::connect(&dsn()).await.expect("conectar");
        let project = make_project(&pool, "gui-exchange").await;
        let aoi_layer =
            add_polygon_layer(&pool, &project, rect(-52.30, -3.30, -52.28, -3.29)).await;
        let (_aoi_id, cell_layer) = add_grid(&pool, &project, &aoi_layer, 2, 1, 0.01).await;
        let (_, _, _) = add_class_and_ruleset(&pool, &project).await;
        assert!(!cell_layer.id.is_empty());
        project
    });
    let pool = rt.block_on(sqlx::PgPool::connect(&dsn())).expect("pool");
    let out = temp_file("exchange.json");
    let cli = ta_cli_bin();

    // O `CURUPIRA_TA_CLI` é explícito: apontamos para o binário do workspace.
    unsafe { std::env::set_var("CURUPIRA_TA_CLI", &cli) };
    let import_name = unique_name("copia");
    let mut events = WorkerEvents::new(DbWorker::spawn());
    events.worker.send(DbCmd::Connect { url: dsn() });
    events.wait_for("conectar", |e| {
        matches!(e, DbEvt::Connected | DbEvt::ConnectFailed(_))
    });

    events.worker.send(DbCmd::Exchange(ExchangeRequest {
        mode: ExchangeMode::Export,
        project: project.name.clone(),
        path: out.to_string_lossy().into_owned(),
        into: None,
    }));
    let DbEvt::ExchangeDone { message, .. } = events.wait_for("exchange export", |e| {
        matches!(e, DbEvt::ExchangeDone { .. } | DbEvt::ExchangeFailed { .. })
    }) else {
        panic!("esperava ExchangeDone no export");
    };
    assert!(!message.trim().is_empty(), "resumo do binário: {message}");
    assert!(out.is_file(), "o export gravou {}", out.display());
    assert!(
        std::fs::metadata(&out).expect("metadata").len() > 0,
        "arquivo de exchange não vazio"
    );

    // Import cria o projeto novo (o binário valida o documento).
    events.worker.send(DbCmd::Exchange(ExchangeRequest {
        mode: ExchangeMode::Import,
        project: String::new(),
        path: out.to_string_lossy().into_owned(),
        into: Some(import_name.clone()),
    }));
    let DbEvt::ExchangeDone { message, .. } = events.wait_for("exchange import", |e| {
        matches!(e, DbEvt::ExchangeDone { .. } | DbEvt::ExchangeFailed { .. })
    }) else {
        panic!("esperava ExchangeDone no import");
    };
    assert!(message.contains("exchange importado"), "saída: {message}");
    let imported = rt.block_on(async {
        PgProjectRepo::new(pool.clone())
            .list()
            .await
            .expect("listar projetos")
            .into_iter()
            .find(|p| p.name == import_name)
            .expect("projeto importado existe")
    });

    // Binário ausente: overrides explícitos não caem em silêncio no vizinho.
    unsafe { std::env::set_var("CURUPIRA_TA_CLI", "/nao/existe/ta-cli") };
    events.worker.send(DbCmd::Exchange(ExchangeRequest {
        mode: ExchangeMode::Export,
        project: project.name.clone(),
        path: out.to_string_lossy().into_owned(),
        into: None,
    }));
    let DbEvt::ExchangeFailed { message, .. } = events.wait_for("exchange sem binário", |e| {
        matches!(e, DbEvt::ExchangeFailed { .. } | DbEvt::ExchangeDone { .. })
    }) else {
        panic!("esperava ExchangeFailed com binário inexistente");
    };
    assert!(
        message.contains("/nao/existe/ta-cli"),
        "a mensagem cita o caminho configurado: {message}"
    );
    unsafe { std::env::remove_var("CURUPIRA_TA_CLI") };

    events.worker.send(DbCmd::Disconnect);
    let _ = events.wait_for("disconnect", |e| matches!(e, DbEvt::Disconnected));
    std::fs::remove_file(&out).ok();
    delete_projects(&[&project, &imported]);
    rt.block_on(pool.close());
}
