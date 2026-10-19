//! A **máquina de estados da fila de processos** (Fase 3, extraída na 5a).
//!
//! O executor é **transacional por lote**: claim → medidas →
//! `classify_batch` (motor puro) → draft_upsert + log + finish no MESMO
//! commit — worker morto no meio do lote deixa tudo `pending` de novo
//! (retomada limpa). Quando o lote falha (ex.: gatilho de integridade), o
//! executor degrada para célula a célula para isolar o processo culpado:
//! célula boa é concluída, célula ruim vira `error` com `last_error` acionável
//! e volta só por `retry_task`.
//!
//! Regras de negócio (também nos comentários do repo):
//! - célula bloqueada NÃO é enfileirada (bloqueada = fora do processamento);
//! - cancelamento é cooperativo (checado entre lotes; `claim` não entrega
//!   trabalho de task cancelada);
//! - regras do ruleset são lidas UMA vez por run; o parse do predicado é uma
//!   vez por lote (dentro do `classify_batch`), nunca por célula;
//! - o estado final da task é derivado dos processos (`refresh_status`).
//!
//! **Saída de progresso:** nada de `println!` aqui — o executor emite
//! [`RunEvent`]s para um callback e devolve um [`RunReport`]. A CLI imprime as
//! mesmas linhas de antes; a GUI converte eventos em barra de progresso.

use std::time::{Duration, Instant};

use ta_core::classification::DraftLayer;
use ta_core::project::Project;
use ta_core::rule::Rule;
use ta_core::task::{CellLogEntry, LogLevel, ProcessStatus, Task, TaskKind, TaskStatus};
use ta_db::repo_cell::{CellLayerRepo, PgCellLayerRepo};
use ta_db::repo_class::{DraftLayerRepo, PgDraftLayerRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::{ClaimedProcess, EnqueueReport, PgTaskRepo, TaskCounts, TaskRepo};
use ta_db::sqlx::PgPool;
use ta_engine::{CellInput, ClassOutput, classify_batch};

use crate::error::FlowError;

/// Converte os erros de conexão/transação (`sqlx::Error` direto de
/// `begin`/`commit`/`rollback`) na variante `Db` — mesmo caminho dos erros
/// que já saem de `ta_db::Error` (que os embrulha).
fn db_err(err: ta_db::sqlx::Error) -> FlowError {
    FlowError::Db(err.into())
}

/// Tamanho do lote do executor (células por transação). Suficientemente grande
/// para pagar o parse das regras uma vez só; pequeno o bastante para o
/// degradar-para-célula não custar caro num lote ruim.
pub const DEFAULT_BATCH: i64 = 500;

// ---------------------------------------------------------------------------
// create — enfileiramento idempotente
// ---------------------------------------------------------------------------

/// Task criada, com o contexto que a CLI/GUI exibem junto (`task criada: …`).
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedTask {
    /// A task persistida (status `pending`).
    pub task: Task,
    /// Nome do ruleset resolvido (explícito ou o ativo do projeto).
    pub ruleset_name: String,
    /// Versão do ruleset resolvido.
    pub ruleset_version: i32,
    /// Nome do rascunho (reaproveitado ou criado).
    pub draft_name: String,
    /// Resultado do enfileiramento (novos × elegíveis).
    pub report: EnqueueReport,
}

/// `create_task` — cria a task e enfileira 1 processo por célula NÃO bloqueada
/// da camada (idempotente; bloqueadas ficam FORA). Task + enfileiramento
/// acontecem numa única transação: task sem processo não existe.
///
/// Ruleset: `ruleset_id` explícito ou o ATIVO do projeto. Rascunho:
/// `draft_layer_name` ou `rascunho <ruleset>`; existente no projeto é
/// reaproveitado (reclassificação é upsert).
///
/// O projeto chega RESOLVIDO (id + nome) — resolver id-ou-nome é camada de
/// apresentação (CLI); a GUI já tem o projeto aberto.
pub async fn create_task(
    pool: &PgPool,
    project: &Project,
    cell_layer_id: &str,
    ruleset_id: Option<&str>,
    draft_layer_name: Option<&str>,
    task_name: Option<&str>,
) -> Result<CreatedTask, FlowError> {
    let layer_id = ensure_cell_layer_in_project(pool, &project.id, cell_layer_id).await?;

    // Ruleset explícito ou o ativo do projeto.
    let ruleset = match ruleset_id {
        Some(id) => PgRuleSetRepo::new(pool.clone())
            .get(id)
            .await?
            .ok_or_else(|| FlowError::RulesetNotFound(id.to_owned()))?,
        None => PgRuleSetRepo::new(pool.clone())
            .find_active(&project.id)
            .await?
            .ok_or_else(|| FlowError::NoActiveRuleset(project.name.clone()))?,
    };
    if ruleset.project_id != project.id {
        return Err(FlowError::RulesetOtherProject {
            ruleset_id: ruleset.id.clone(),
            project_id: ruleset.project_id.clone(),
        });
    }

    // Rascunho: existente é reaproveitado; inexistente é criado vinculado à grade.
    let draft_name = draft_layer_name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rascunho {}", ruleset.name));
    let drafts = PgDraftLayerRepo::new(pool.clone());
    let draft = match drafts
        .list_for_project(&project.id)
        .await?
        .into_iter()
        .find(|d| d.name == draft_name)
    {
        Some(existing) => existing,
        None => drafts
            .create(&DraftLayer::new(&project.id, &draft_name).with_cell_layer(Some(&layer_id)))
            .await
            .map_err(|source| FlowError::DraftCreate {
                name: draft_name.clone(),
                source,
            })?,
    };
    if let Some(draft_layer) = draft.cell_layer_id.as_deref() {
        if draft_layer != layer_id {
            return Err(FlowError::CreateDraftLayerMismatch {
                draft_name: draft_name.clone(),
                draft_layer: draft_layer.to_owned(),
                layer_id: layer_id.clone(),
            });
        }
    }

    let task_name = task_name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("classificar {}", layer_id));
    let mut task = Task::new(&project.id, TaskKind::Classify, &task_name);
    task.cell_layer_id = Some(layer_id.clone());
    task.ruleset_id = Some(ruleset.id.clone());
    task.draft_layer_id = Some(draft.id.clone());

    // Task + enfileiramento numa transação: task sem processo não existe.
    let mut tx = pool.begin().await.map_err(db_err)?;
    let saved = PgTaskRepo::create_on(&mut *tx, &task).await?;
    let report = PgTaskRepo::enqueue_on(&mut tx, &saved.id, &layer_id).await?;
    tx.commit().await.map_err(db_err)?;

    Ok(CreatedTask {
        task: saved,
        ruleset_name: ruleset.name,
        ruleset_version: ruleset.version,
        draft_name: draft.name,
        report,
    })
}

/// Garante que a camada de células existe e pertence ao projeto; devolve o id.
///
/// Ponto único da regra (a CLI delega aqui; a GUI usará direto).
pub async fn ensure_cell_layer_in_project(
    pool: &PgPool,
    project_id: &str,
    cell_layer_id: &str,
) -> Result<String, FlowError> {
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await?
        .ok_or_else(|| FlowError::CellLayerNotFound(cell_layer_id.to_owned()))?;
    if layer.project_id != project_id {
        return Err(FlowError::CellLayerOtherProject {
            id: cell_layer_id.to_owned(),
            project_id: layer.project_id,
        });
    }
    Ok(layer.id)
}

// ---------------------------------------------------------------------------
// run — o executor
// ---------------------------------------------------------------------------

/// Evento de progresso do [`run_task`] — o que a CLI imprime linha a linha e a
/// GUI assina para barra de progresso. Razões de erro já vêm encurtadas à
/// primeira linha (`shorten_error`), na forma exibida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    /// Lote commitado com sucesso.
    BatchOk {
        /// Células do lote.
        cells: u64,
        /// Linhas de rascunho gravadas (upsert).
        draft_rows: u64,
        /// Entradas de log gravadas.
        logs: u64,
    },
    /// Lote falhou (rollback devolveu tudo a `pending`); executor degrada
    /// para célula a célula. `reason` é a primeira linha do erro do banco.
    BatchFailed { reason: String },
    /// Célula individual falhou no caminho degradado (marcada `error` com
    /// `last_error`; volta só com `retry_task`).
    CellError { cell_id: String, reason: String },
}

/// Resultado consolidado de um `run_task` concluído (o epílogo da CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// Células processadas nesta invocação (novas concluídas).
    pub processed: u64,
    /// Células que falharam no caminho degradado.
    pub errors: u64,
    /// Tempo total do run.
    pub elapsed: Duration,
    /// Tempo dentro do `classify_batch` (o trecho de CPU).
    pub classify_time: Duration,
    /// Cancelamento cooperativo observado ENTRE lotes.
    pub cancelled: bool,
    /// Estado final após `refresh_status` (`None` = task cancelada, estado
    /// terminal não é sobrescrito).
    pub final_status: Option<TaskStatus>,
    /// Contagens por estado no fim do run.
    pub counts: TaskCounts,
}

/// Desfecho do [`run_task`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// Task já `done`: nada a processar (a CLI imprime o aviso e sai 0).
    AlreadyDone { task_id: String },
    /// O run rodou (total, parcial por `--limit`, cancelado ou com erros).
    Completed(RunReport),
}

/// `run_task` — processa a task em lotes transacionais.
///
/// `limit` processa no máximo N células nesta invocação (o resto continua
/// `pending`; rodar de novo retoma — nenhum `done` é reprocessado). Eventos de
/// progresso saem por `on_event` (entre o claim e o commit do lote NADA é
/// emitido: só sucesso commitado vira evento).
pub async fn run_task(
    pool: &PgPool,
    task_id: &str,
    limit: Option<u64>,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<RunOutcome, FlowError> {
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await?
        .ok_or_else(|| FlowError::TaskNotFound(task_id.to_owned()))?;
    match task.status {
        TaskStatus::Cancelled => {
            return Err(FlowError::TaskCancelledForRun(task.id));
        }
        TaskStatus::Done => {
            return Ok(RunOutcome::AlreadyDone { task_id: task.id });
        }
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Failed => {}
    }
    let cell_layer_id = task
        .cell_layer_id
        .clone()
        .ok_or_else(|| FlowError::TaskWithoutCellLayer(task.id.clone()))?;
    let ruleset_id = task
        .ruleset_id
        .clone()
        .ok_or_else(|| FlowError::TaskWithoutRuleset(task.id.clone()))?;
    let draft_layer_id = task
        .draft_layer_id
        .clone()
        .ok_or_else(|| FlowError::TaskWithoutDraft(task.id.clone()))?;

    // Pré-voo do rascunho: precisa existir e ser da MESMA grade da task —
    // decisões de `draft_cells` de outra grade nunca são válidas (o rascunho
    // é vinculado à grade na criação; mismatch só ocorre com banco editado
    // fora do fluxo ou task enfileirada para outra grade).
    let draft = PgDraftLayerRepo::new(pool.clone())
        .get(&draft_layer_id)
        .await?
        .ok_or_else(|| FlowError::TaskDraftMissing {
            task_id: task.id.clone(),
            draft_id: draft_layer_id.clone(),
        })?;
    if let Some(draft_layer) = draft.cell_layer_id.as_deref() {
        if draft_layer != cell_layer_id {
            return Err(FlowError::RunDraftLayerMismatch {
                draft_name: draft.name,
                draft_layer: draft_layer.to_owned(),
                cell_layer: cell_layer_id.clone(),
            });
        }
    }

    // Regras lidas UMA vez por run (ativas; o motor re-filtra por robustez).
    let all_rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await?;
    let rules: Vec<Rule> = all_rules.iter().filter(|r| r.is_active).cloned().collect();
    if rules.is_empty() {
        return Err(FlowError::NoActiveRules(ruleset_id));
    }

    tasks.set_status(&task.id, TaskStatus::Running).await?;

    let started = Instant::now();
    let mut processed: u64 = 0;
    let mut errors: u64 = 0;
    let mut classify_time = Duration::ZERO;
    let mut cancelled = false;

    loop {
        // Cancelamento cooperativo: checado ENTRE lotes; o lote em voo termina.
        if let Some(current) = tasks.get(&task.id).await? {
            if current.status == TaskStatus::Cancelled {
                cancelled = true;
                break;
            }
        }
        // Limite desta invocação: o que falta no teto, sem passar do lote.
        let batch = match limit {
            Some(total) => {
                let remaining = total.saturating_sub(processed);
                if remaining == 0 {
                    break;
                }
                i64::try_from(remaining)
                    .unwrap_or(DEFAULT_BATCH)
                    .min(DEFAULT_BATCH)
            }
            None => DEFAULT_BATCH,
        };

        let mut tx = pool.begin().await.map_err(db_err)?;
        let claimed = PgTaskRepo::claim_on(&mut *tx, &task.id, batch).await?;
        if claimed.is_empty() {
            tx.commit().await.map_err(db_err)?;
            break; // fila vazia: fim limpo
        }
        let cell_ids: Vec<String> = claimed.iter().map(|c| c.cell_id.clone()).collect();
        let measures =
            PgTaskRepo::measures_for_cells_on(&mut *tx, &cell_layer_id, &cell_ids).await?;
        let inputs: Vec<CellInput> = claimed
            .iter()
            .map(|c| {
                CellInput::new(
                    &c.cell_id,
                    measures.get(&c.cell_id).cloned().unwrap_or_default(),
                )
            })
            .collect();

        let classify_started = Instant::now();
        let outputs = classify_batch(&inputs, &rules);
        classify_time += classify_started.elapsed();

        // Plano do lote: decisões → rascunho; log e finish por processo.
        let plan = plan_batch(&task.id, &cell_layer_id, &claimed, &outputs);

        let written = run_batch_writes(
            &mut tx,
            &draft_layer_id,
            &ruleset_id,
            &plan.draft_rows,
            &plan.logs,
            &plan.finishes,
        )
        .await;
        match written {
            Ok(rows) => {
                // Commit único do lote: claim + rascunho + log + finishes.
                tx.commit().await.map_err(db_err)?;
                processed += claimed.len() as u64;
                on_event(RunEvent::BatchOk {
                    cells: claimed.len() as u64,
                    draft_rows: rows.0,
                    logs: rows.1,
                });
            }
            Err(batch_error) => {
                // Lote falhou (ex.: gatilho de integridade): o rollback devolve
                // TODOS os processos a `pending`. Degrada para célula a célula
                // para isolar o culpado — bom vira done, ruim vira error.
                tx.rollback().await.map_err(db_err)?;
                on_event(RunEvent::BatchFailed {
                    reason: shorten_error(&batch_error.to_string()),
                });
                for cell_id in &cell_ids {
                    match run_single_cell(
                        pool,
                        &task.id,
                        &cell_layer_id,
                        &draft_layer_id,
                        &ruleset_id,
                        &rules,
                        cell_id,
                    )
                    .await
                    {
                        Ok(()) => processed += 1,
                        Err(cell_error) => {
                            errors += 1;
                            on_event(RunEvent::CellError {
                                cell_id: cell_id.clone(),
                                reason: shorten_error(&cell_error),
                            });
                        }
                    }
                }
            }
        }
    }

    let final_status = tasks.refresh_status(&task.id).await?;
    let counts = tasks.counts(&task.id).await?;
    Ok(RunOutcome::Completed(RunReport {
        processed,
        errors,
        elapsed: started.elapsed(),
        classify_time,
        cancelled,
        final_status,
        counts,
    }))
}

/// Plano de escritas de um lote (puro, testável sem banco): decisões → linhas
/// de rascunho; log Info/Warn e finish `Done` por processo. Célula sem classe
/// NÃO é erro: conclui com log explicativo (nenhuma regra casou / medidas
/// ausentes) — a retomada não a reprocessa.
struct BatchPlan {
    draft_rows: Vec<(String, String, String)>,
    logs: Vec<CellLogEntry>,
    finishes: Vec<(String, ProcessStatus, Option<String>)>,
}

fn plan_batch(
    task_id: &str,
    cell_layer_id: &str,
    claimed: &[ClaimedProcess],
    outputs: &[ClassOutput],
) -> BatchPlan {
    let mut plan = BatchPlan {
        draft_rows: Vec::new(),
        logs: Vec::new(),
        finishes: Vec::new(),
    };
    for (proc, output) in claimed.iter().zip(outputs.iter()) {
        match output.decision.class_id.clone() {
            Some(class_id) => {
                plan.draft_rows.push((
                    cell_layer_id.to_owned(),
                    output.cell_id.clone(),
                    class_id.clone(),
                ));
                plan.logs.push(CellLogEntry::new(
                    task_id,
                    cell_layer_id,
                    &output.cell_id,
                    LogLevel::Info,
                    format!(
                        "classe={class_id} regra={}",
                        output.decision.rule_id.as_deref().unwrap_or("?")
                    ),
                ));
                plan.finishes
                    .push((proc.id.clone(), ProcessStatus::Done, None));
            }
            None => {
                let missing = if output.decision.missing_measures.is_empty() {
                    "nenhuma regra casou".to_owned()
                } else {
                    format!(
                        "medidas ausentes: {}",
                        output.decision.missing_measures.join(", ")
                    )
                };
                plan.logs.push(CellLogEntry::new(
                    task_id,
                    cell_layer_id,
                    &output.cell_id,
                    LogLevel::Warn,
                    format!("nenhuma classe decidida ({missing})"),
                ));
                plan.finishes
                    .push((proc.id.clone(), ProcessStatus::Done, None));
            }
        }
    }
    plan
}

/// Escritas do lote (rascunho + log + finishes) no MESMO commit.
async fn run_batch_writes(
    tx: &mut ta_db::sqlx::PgConnection,
    draft_layer_id: &str,
    ruleset_id: &str,
    draft_rows: &[(String, String, String)],
    logs: &[CellLogEntry],
    finishes: &[(String, ProcessStatus, Option<String>)],
) -> Result<(u64, u64), ta_db::Error> {
    let draft_written =
        PgTaskRepo::draft_upsert_on(&mut *tx, draft_layer_id, ruleset_id, draft_rows).await?;
    let logs_written = PgTaskRepo::append_logs_on(&mut *tx, logs).await?;
    for (process_id, status, last_error) in finishes {
        PgTaskRepo::finish_on(&mut *tx, process_id, *status, last_error.as_deref()).await?;
    }
    Ok((draft_written, logs_written))
}

/// Caminho degradado: processa UMA célula na própria transação.
///
/// - Ok: célula concluída (done) no commit único;
/// - Err(msg): célula marcada `error` com `msg` em `last_error` — transação
///   separada para o erro não engolir a marcação. `msg` é o texto do erro do
///   banco (vai para `processes.last_error`, por isso String aqui).
async fn run_single_cell(
    pool: &PgPool,
    task_id: &str,
    cell_layer_id: &str,
    draft_layer_id: &str,
    ruleset_id: &str,
    rules: &[Rule],
    cell_id: &str,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| cell_err(e.into()))?;
    let Some(proc) = PgTaskRepo::claim_cell_on(&mut tx, task_id, cell_id)
        .await
        .map_err(cell_err)?
    else {
        // Outro worker chegou primeiro; nada a fazer.
        return Ok(());
    };
    let measures =
        PgTaskRepo::measures_for_cells_on(&mut *tx, cell_layer_id, &[cell_id.to_owned()])
            .await
            .map_err(cell_err)?;
    let input = CellInput::new(cell_id, measures.get(cell_id).cloned().unwrap_or_default());
    let outputs = classify_batch(std::slice::from_ref(&input), rules);
    let plan = plan_batch(
        task_id,
        cell_layer_id,
        std::slice::from_ref(&proc),
        &outputs,
    );

    // Escritas do processo (rascunho + log + finish) nesta transação; o commit
    // fica fora do bloco para o rollback do erro manter `tx` utilizável.
    let write: Result<(), ta_db::Error> = async {
        PgTaskRepo::draft_upsert_on(&mut *tx, draft_layer_id, ruleset_id, &plan.draft_rows).await?;
        PgTaskRepo::append_logs_on(&mut *tx, &plan.logs).await?;
        for (process_id, status, last_error) in &plan.finishes {
            PgTaskRepo::finish_on(&mut *tx, process_id, *status, last_error.as_deref()).await?;
        }
        Ok(())
    }
    .await;
    match write {
        Ok(()) => tx.commit().await.map_err(|e| cell_err(e.into()))?,
        Err(db_error) => {
            let message = db_error.to_string();
            tx.rollback().await.map_err(|e| cell_err(e.into()))?;
            // Marca o processo como error numa transação própria (o rollback
            // devolveu-o a `pending`; reclamamos e fechamos com o motivo —
            // claim + finish na MESMA transação, sem janela de corrida).
            let mut etx = pool.begin().await.map_err(|e| cell_err(e.into()))?;
            PgTaskRepo::claim_and_error_on(&mut etx, task_id, cell_id, &message)
                .await
                .map_err(cell_err)?;
            etx.commit().await.map_err(|e| cell_err(e.into()))?;
            return Err(message);
        }
    }
    Ok(())
}

fn cell_err(err: ta_db::Error) -> String {
    err.to_string()
}

/// Erros de banco são longos; o progresso da CLI mostra a primeira linha.
fn shorten_error(message: &str) -> String {
    message.lines().next().unwrap_or(message).to_owned()
}

// ---------------------------------------------------------------------------
// cancel / retry
// ---------------------------------------------------------------------------

/// Desfecho do cancelamento cooperativo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// A task já estava cancelada (idempotente: nada muda).
    AlreadyCancelled { task_id: String },
    /// Task marcada cancelada; `done` são os processos já concluídos (o
    /// cancelamento é para trabalho EM VOO; `done` é terminal e permanece).
    Cancelled { task_id: String, done: i64 },
}

/// `cancel_task` — cancelamento cooperativo: a task nunca mais entrega
/// trabalho; o lote em voo termina (quem roda, conclui).
pub async fn cancel_task(pool: &PgPool, task_id: &str) -> Result<CancelOutcome, FlowError> {
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await?
        .ok_or_else(|| FlowError::TaskNotFound(task_id.to_owned()))?;
    if task.status == TaskStatus::Cancelled {
        return Ok(CancelOutcome::AlreadyCancelled { task_id: task.id });
    }
    // Task concluída NÃO vira `cancelled`: done é terminal (o cancelamento
    // é para trabalho EM VOO; flipar um histórico de sucesso corromperia a
    // leitura da fila). Sem pendentes/running/error não há nada a cancelar.
    let counts = tasks.counts(&task.id).await?;
    if counts.pending + counts.running + counts.error == 0 {
        return Err(FlowError::NothingToCancel(task.id));
    }
    tasks.set_status(&task.id, TaskStatus::Cancelled).await?;
    let counts = tasks.counts(&task.id).await?;
    Ok(CancelOutcome::Cancelled {
        task_id: task.id,
        done: counts.done,
    })
}

/// Resultado do `retry_task`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryReport {
    /// Processos `error` repostos em `pending`.
    pub reset: i64,
    /// Estado da task após o `refresh_status` (`None` = cancelada).
    pub status: Option<TaskStatus>,
}

/// `retry_task` — repõe os processos `error` em `pending` (reprocessamento é
/// SEMPRE explícito; `done` nunca é reprocessado).
pub async fn retry_task(pool: &PgPool, task_id: &str) -> Result<RetryReport, FlowError> {
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await?
        .ok_or_else(|| FlowError::TaskNotFound(task_id.to_owned()))?;
    if task.status == TaskStatus::Cancelled {
        return Err(FlowError::TaskCancelledForRetry(task.id));
    }
    let mut tx = pool.begin().await.map_err(db_err)?;
    let reset = PgTaskRepo::retry_errors_on(&mut *tx, &task.id).await?;
    tx.commit().await.map_err(db_err)?;
    let status = tasks.refresh_status(&task.id).await?;
    Ok(RetryReport { reset, status })
}

// ---------------------------------------------------------------------------
// testes puros (sem banco) — o plano do lote e o encurtador de erro
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ta_engine::Decision;

    fn claimed(cell_id: &str) -> ClaimedProcess {
        ClaimedProcess {
            id: format!("proc-{cell_id}"),
            cell_id: cell_id.to_owned(),
            attempts: 0,
        }
    }

    fn output(cell_id: &str, class_id: Option<&str>, missing: &[&str]) -> ClassOutput {
        ClassOutput {
            cell_id: cell_id.to_owned(),
            decision: Decision {
                class_id: class_id.map(str::to_owned),
                rule_id: class_id.map(|_| "rule-1".to_owned()),
                evaluated: 1,
                missing_measures: missing.iter().map(|m| (*m).to_owned()).collect(),
                skipped_invalid: 0,
                skipped_invalid_names: Vec::new(),
            },
        }
    }

    /// Célula decidida → linha de rascunho + log Info com classe/regra + done.
    #[test]
    fn plano_do_lote_decidida_gera_rascunho_log_e_done() {
        let claimed = [claimed("0_0")];
        let outputs = [output("0_0", Some("cls-floresta"), &[])];
        let plan = plan_batch("t1", "grade-1", &claimed, &outputs);
        assert_eq!(
            plan.draft_rows,
            vec![("grade-1".into(), "0_0".into(), "cls-floresta".into())]
        );
        assert_eq!(plan.logs.len(), 1);
        assert_eq!(plan.logs[0].level, LogLevel::Info);
        assert_eq!(plan.logs[0].message, "classe=cls-floresta regra=rule-1");
        assert_eq!(
            plan.finishes,
            vec![("proc-0_0".to_owned(), ProcessStatus::Done, None)]
        );
    }

    /// Célula sem classe NÃO é erro: done com log Warn explicativo (e sem
    /// linha de rascunho — a célula volta só com medidas novas + task nova).
    #[test]
    fn plano_do_lote_sem_classe_gera_warn_e_done_sem_rascunho() {
        let claimed = [claimed("1_2")];
        let sem_medida = plan_batch("t1", "g", &claimed, &[output("1_2", None, &["ndvi"])]);
        assert!(sem_medida.draft_rows.is_empty());
        assert_eq!(sem_medida.logs.len(), 1);
        assert_eq!(sem_medida.logs[0].level, LogLevel::Warn);
        assert_eq!(
            sem_medida.logs[0].message,
            "nenhuma classe decidida (medidas ausentes: ndvi)"
        );
        assert_eq!(
            sem_medida.finishes,
            vec![("proc-1_2".to_owned(), ProcessStatus::Done, None)]
        );

        let sem_regra = plan_batch("t1", "g", &claimed, &[output("1_2", None, &[])]);
        assert_eq!(
            sem_regra.logs[0].message,
            "nenhuma classe decidida (nenhuma regra casou)"
        );
    }

    /// O progresso mostra só a primeira linha do erro de banco (mensagens do
    /// PG são multiline; a fila continua legível).
    #[test]
    fn erro_encurtado_mantem_apenas_a_primeira_linha() {
        let longo = "erro: gatilho de integridade\nDETAIL: alguma coisa\nCONTEXT: mais";
        assert_eq!(shorten_error(longo), "erro: gatilho de integridade");
        assert_eq!(shorten_error("uma linha só"), "uma linha só");
        assert_eq!(shorten_error(""), "");
    }
}
