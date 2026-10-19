//! Subcomandos da **fila de processos e classificação** (Fase 3): medidas por
//! célula, ativação de ruleset, tasks (create/run/list/show/cancel/retry),
//! processos e log por célula.
//!
//! **Camada fina (Fase 5a):** o EXECUTOR da fila mora em `ta_flow` (crate
//! próprio, compartilhado com a GUI da Fase 5b — a lógica NÃO se duplica).
//! Aqui fica só parse de argumentos e formatação de saída: as funções de
//! create/run/cancel/retry chamam `ta_flow` e imprimem as MESMAS linhas de
//! antes (erros tipados de `ta_flow::FlowError` viram `String` de CLI na
//! borda). As leituras/listagens (list/show/process/cell log) e as medidas
//! seguem sendo um comando de repositório + formatação — sem lógica de fila.

use ta_core::classification::CellMeasure;
use ta_core::task::ProcessStatus;
use ta_db::repo::PgProjectRepo;
use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::repo_ruleset::{PgRuleSetRepo, RuleSetRepo};
use ta_db::repo_scene::{PgSceneRepo, SceneRepo};
use ta_db::repo_task::{EnqueueReport, PgTaskRepo, TaskRepo};
use ta_flow::queue::{
    CancelOutcome, RunEvent, RunOutcome, cancel_task, create_task, retry_task, run_task,
};

use crate::flow::parse_observations;
use crate::resolve_project;

// ---------------------------------------------------------------------------
// measure — medidas observadas por célula
// ---------------------------------------------------------------------------

/// `measure set` — grava (upsert) as medidas de UMA célula.
///
/// `scene` (opcional) resolve a proveniência por NOME de cena; erro acionável
/// se não existir (cenas são globais — veja `scene add`). Regrar a mesma
/// (célula, medida) é **last-write-wins**: a última escrita vence, inclusive a
/// proveniência (medida regravada sem `--scene` perde a cena anterior).
pub(crate) async fn measure_set(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: &str,
    values: &str,
    source: Option<String>,
    scene: Option<&str>,
) -> Result<(), String> {
    let measures = parse_observations(values)?;
    let pool = crate::open_pool(dsn).await?;
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("camada de células `{cell_layer_id}` não existe"))?;
    if !PgCellRepo::new(pool.clone())
        .exists(&layer.id, cell_id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "célula `{cell_id}` não existe na camada `{}` (ids são `\"<col>_<row>\"`; \
             veja `grid info`)",
            layer.name
        ));
    }
    let scene_id = match scene {
        None => None,
        Some(name) => Some(
            PgSceneRepo::new(pool.clone())
                .get_by_name(name)
                .await
                .map_err(|e| e.to_string())?
                .map(|s| s.id)
                .ok_or_else(|| {
                    format!(
                        "cena `{name}` não existe; crie com `ta-cli scene add --name {name}` \
                         ou liste as existentes com `ta-cli scene list`"
                    )
                })?,
        ),
    };
    let now = time::OffsetDateTime::now_utc();
    let rows: Vec<CellMeasure> = measures
        .into_iter()
        .map(|(measure, value)| CellMeasure {
            cell_layer_id: layer.id.clone(),
            cell_id: cell_id.to_owned(),
            measure,
            value,
            source: source.clone(),
            scene_id: scene_id.clone(),
            updated_on: now,
        })
        .collect();
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let written = PgTaskRepo::upsert_measures_on(&mut *tx, &rows)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    match scene {
        Some(scene_name) => println!(
            "medidas gravadas: {written}\tcamada={}\tcélula={cell_id}\tcena={scene_name}",
            layer.id
        ),
        None => println!(
            "medidas gravadas: {written}\tcamada={}\tcélula={cell_id}",
            layer.id
        ),
    }
    pool.close().await;
    Ok(())
}

/// `measure list` — lista as medidas da camada (opcionalmente de uma célula).
pub(crate) async fn measure_list(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let repo = PgTaskRepo::new(pool.clone());
    let measures = repo
        .list_measures(cell_layer_id, cell_id)
        .await
        .map_err(|e| e.to_string())?;
    if measures.is_empty() {
        println!("(nenhuma medida gravada na camada {cell_layer_id})");
    }
    for m in &measures {
        println!(
            "{}\t{}\t{}\tsource={}",
            m.cell_id,
            m.measure,
            m.value,
            m.source.as_deref().unwrap_or("(sem source)")
        );
    }
    pool.close().await;
    Ok(())
}

/// `measure clear` — remove as medidas da camada (opcionalmente de uma célula).
pub(crate) async fn measure_clear(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let removed = PgTaskRepo::new(pool.clone())
        .clear_measures(cell_layer_id, cell_id)
        .await
        .map_err(|e| e.to_string())?;
    match cell_id {
        Some(cell_id) => println!("medidas removidas: {removed} (célula {cell_id})"),
        None => println!("medidas removidas: {removed} (camada inteira)"),
    }
    pool.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// ruleset activate
// ---------------------------------------------------------------------------

/// `ruleset activate` — torna este ruleset o ATIVO do projeto (desativa os
/// demais; o índice parcial único no banco garante no máximo um).
pub(crate) async fn ruleset_activate(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    version: Option<i32>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .find(&project.id, name, version)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "ruleset `{name}`{} não existe no projeto `{}`; crie com `ruleset create`",
                version
                    .map(|v| format!(" (versão {v})"))
                    .unwrap_or_default(),
                project.name
            )
        })?;
    let activated = PgRuleSetRepo::new(pool.clone())
        .activate(&ruleset.id)
        .await
        .map_err(|e| format!("ativar ruleset: {e}"))?
        .ok_or_else(|| format!("ruleset `{}` sumiu durante a ativação", ruleset.id))?;
    println!(
        "ruleset ativo: {}\t{}\tversao={}\tprojeto={}",
        activated.id, activated.name, activated.version, project.name
    );
    pool.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// task create / run / list / show / cancel / retry — camada fina sobre ta_flow
// ---------------------------------------------------------------------------

/// `task create classify` — parse/formato sobre [`ta_flow::queue::create_task`].
pub(crate) async fn task_create(
    dsn: Option<&str>,
    project: &str,
    cell_layer_id: &str,
    ruleset_id: Option<&str>,
    draft_layer_name: Option<&str>,
    task_name: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let created = create_task(
        &pool,
        &project,
        cell_layer_id,
        ruleset_id,
        draft_layer_name,
        task_name,
    )
    .await
    .map_err(|e| e.to_string())?;
    println!(
        "task criada: {}\tkind={}\tstatus=pending\truleset={} (v{})\trascunho={}",
        created.task.id,
        created.task.kind,
        created.ruleset_name,
        created.ruleset_version,
        created.draft_name
    );
    print_enqueue_report(&created.report);
    pool.close().await;
    Ok(())
}

fn print_enqueue_report(report: &EnqueueReport) {
    println!(
        "processos enfileirados: {} novos / {} células elegíveis (bloqueadas fora)",
        report.inserted, report.eligible
    );
}

/// `task run` — formatação do executor [`ta_flow::queue::run_task`].
///
/// `--limit N` processa no máximo N células nesta invocação (o resto continua
/// `pending`; rodar de novo retoma — nenhum `done` é reprocessado).
pub(crate) async fn task_run(
    dsn: Option<&str>,
    task_id: &str,
    limit: Option<u64>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    // Os eventos reproduzem as linhas de progresso de sempre; o epílogo vem
    // no RunReport (mesmas linhas, mesmas ordens — contrato da CLI da fila).
    let outcome = run_task(&pool, task_id, limit, &mut |event| match event {
        RunEvent::BatchOk {
            cells,
            draft_rows,
            logs,
        } => println!("lote ok: {cells} células ({draft_rows} linhas de rascunho, {logs} logs)"),
        RunEvent::BatchFailed { reason } => {
            println!("lote falhou ({reason}); degradando para célula a célula…")
        }
        RunEvent::CellError { cell_id, reason } => {
            eprintln!("  erro na célula {cell_id}: {reason}")
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    match outcome {
        RunOutcome::AlreadyDone { task_id } => {
            println!("task `{task_id}` já está concluída; nada a processar");
        }
        RunOutcome::Completed(report) => {
            println!(
                "run concluído: processadas={} erros={} tempo={:?} (classificação {:?})",
                report.processed, report.errors, report.elapsed, report.classify_time
            );
            if report.cancelled {
                println!("task cancelada entre lotes; processos pendentes ficam na fila");
            }
            print_task_counts(&report.counts);
            println!(
                "status da task: {}",
                ta_flow::error::status_label(report.final_status)
            );
        }
    }
    pool.close().await;
    Ok(())
}

/// `task list` — tasks do projeto (ou todas), mais recentes primeiro.
pub(crate) async fn task_list(dsn: Option<&str>, project: Option<&str>) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project_id = match project {
        Some(p) => {
            let project = resolve_project(&PgProjectRepo::new(pool.clone()), p).await?;
            Some(project.id)
        }
        None => None,
    };
    let tasks = PgTaskRepo::new(pool.clone())
        .list(project_id.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    if tasks.is_empty() {
        println!("(nenhuma task enfileirada)");
    }
    for task in &tasks {
        println!(
            "{}\tkind={}\tstatus={}\t{}",
            task.id, task.kind, task.status, task.name
        );
    }
    pool.close().await;
    Ok(())
}

fn print_task_counts(counts: &ta_db::repo_task::TaskCounts) {
    println!(
        "processos: total={} pendentes={} executando={} concluídos={} erros={}",
        counts.total, counts.pending, counts.running, counts.done, counts.error
    );
}

/// `task show` — task + contagens por estado (a leitura observável da fila).
pub(crate) async fn task_show(dsn: Option<&str>, task_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task `{task_id}` não existe; veja `task list`"))?;
    println!(
        "task: {}\tkind={}\tstatus={}\t{}",
        task.id, task.kind, task.status, task.name
    );
    println!(
        "  camada={}\truleset={}\trascunho={}",
        task.cell_layer_id.as_deref().unwrap_or("(desvinculada)"),
        task.ruleset_id.as_deref().unwrap_or("(desvinculado)"),
        task.draft_layer_id.as_deref().unwrap_or("(desvinculado)")
    );
    let counts = tasks.counts(&task.id).await.map_err(|e| e.to_string())?;
    print_task_counts(&counts);
    pool.close().await;
    Ok(())
}

/// `task cancel` — formatação sobre [`ta_flow::queue::cancel_task`].
pub(crate) async fn task_cancel(dsn: Option<&str>, task_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let outcome = cancel_task(&pool, task_id)
        .await
        .map_err(|e| e.to_string())?;
    match outcome {
        CancelOutcome::AlreadyCancelled { task_id } => {
            println!("task `{task_id}` já estava cancelada");
        }
        CancelOutcome::Cancelled { task_id, done } => {
            println!("task cancelada: {task_id}\t(processos já concluídos permanecem: {done})");
        }
    }
    pool.close().await;
    Ok(())
}

/// `task retry` — formatação sobre [`ta_flow::queue::retry_task`].
pub(crate) async fn task_retry(dsn: Option<&str>, task_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let report = retry_task(&pool, task_id)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "task retry: {} processo(s) error → pending; status={}",
        report.reset,
        ta_flow::error::status_label(report.status)
    );
    pool.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// process list / cell log
// ---------------------------------------------------------------------------

/// `process list` — processos da task (opcionalmente por estado).
pub(crate) async fn process_list(
    dsn: Option<&str>,
    task_id: &str,
    status: Option<&str>,
) -> Result<(), String> {
    let status = match status {
        Some(text) => Some(
            text.parse::<ProcessStatus>()
                .map_err(|e| format!("{e}; use pending|running|done|error"))?,
        ),
        None => None,
    };
    let pool = crate::open_pool(dsn).await?;
    let processes = PgTaskRepo::new(pool.clone())
        .list_processes(task_id, status)
        .await
        .map_err(|e| e.to_string())?;
    if processes.is_empty() {
        println!("(nenhum processo)");
    }
    for process in &processes {
        println!(
            "{}\t{}\t{}\tattempts={}\t{}",
            process.cell_id,
            process.status,
            process.id,
            process.attempts,
            process.last_error.as_deref().unwrap_or("(sem erro)")
        );
    }
    pool.close().await;
    Ok(())
}

/// `cell log` — log por célula (opcionalmente filtrado por task).
pub(crate) async fn cell_log(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: &str,
    task_id: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let entries = PgTaskRepo::new(pool.clone())
        .list_logs(cell_layer_id, cell_id, task_id)
        .await
        .map_err(|e| e.to_string())?;
    if entries.is_empty() {
        println!("(nenhum log para a célula {cell_id})");
    }
    for entry in &entries {
        println!("{}\t{}\t{}", entry.created_on, entry.level, entry.message);
    }
    pool.close().await;
    Ok(())
}
