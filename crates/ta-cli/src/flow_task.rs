//! Subcomandos da **fila de processos e classificação** (Fase 3): medidas por
//! célula, ativação de ruleset, tasks (create/run/list/show/cancel/retry),
//! processos e log por célula.
//!
//! O executor da task (`task run`) é **transacional por lote**: claim →
//! medidas → `classify_batch` (motor puro) → draft_upsert + log + finish no
//! MESMO commit — worker morto no meio do lote deixa tudo `pending` de novo
//! (retomada limpa). Quando o lote falha (ex.: gatilho de integridade), o
//! executor degrada para célula a célula para isolar o processo culpado:
//! célula boa é concluída, célula ruim vira `error` com `last_error` acionável
//! e volta só por `task retry`.
//!
//! Regras de negócio documentadas (também nos comentários do repo):
//! - célula bloqueada NÃO é enfileirada (bloqueada = fora do processamento);
//! - cancelamento é cooperativo (checado entre lotes; `claim` não entrega
//!   trabalho de task cancelada);
//! - regras do ruleset são lidas UMA vez por run; o parse do predicado é uma
//!   vez por lote (dentro do `classify_batch`), nunca por célula;
//! - o estado final da task é derivado dos processos (`refresh_status`).

use std::time::{Duration, Instant};

use sqlx::PgPool;
use ta_core::classification::CellMeasure;
use ta_core::classification::DraftLayer;
use ta_core::task::{CellLogEntry, LogLevel, ProcessStatus, Task, TaskKind, TaskStatus};
use ta_db::repo::PgProjectRepo;
use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::repo_class::{DraftLayerRepo, PgDraftLayerRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::{EnqueueReport, PgTaskRepo, TaskRepo};
use ta_engine::{CellInput, classify_batch};

use crate::flow::{ensure_cell_layer_in_project, parse_observations};
use crate::resolve_project;

/// Tamanho do lote do executor (células por transação). Suficientemente grande
/// para pagar o parse das regras uma vez só; pequeno o bastante para o
/// degradar-para-célula não custar caro num lote ruim.
pub(crate) const DEFAULT_BATCH: i64 = 500;

/// Erro do executor por célula: a mensagem acionável que vai para
/// `processes.last_error`.
type CellRunResult = Result<(), String>;

// ---------------------------------------------------------------------------
// measure — medidas observadas por célula
// ---------------------------------------------------------------------------

/// `measure set` — grava (upsert) as medidas de UMA célula.
pub(crate) async fn measure_set(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: &str,
    values: &str,
    source: Option<String>,
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
    let now = time::OffsetDateTime::now_utc();
    let rows: Vec<CellMeasure> = measures
        .into_iter()
        .map(|(measure, value)| CellMeasure {
            cell_layer_id: layer.id.clone(),
            cell_id: cell_id.to_owned(),
            measure,
            value,
            source: source.clone(),
            updated_on: now,
        })
        .collect();
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let written = PgTaskRepo::upsert_measures_on(&mut *tx, &rows)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    println!(
        "medidas gravadas: {written}\tcamada={}\tcélula={cell_id}",
        layer.id
    );
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
// task create / run / list / show / cancel / retry
// ---------------------------------------------------------------------------

/// `task create classify` — cria a task e enfileira 1 processo por célula NÃO
/// bloqueada da camada (idempotente; bloqueadas ficam FORA).
///
/// Ruleset: `--ruleset <id>` ou o ATIVO do projeto (`ruleset activate`).
/// Rascunho: `--draft-layer <nome>` ou `rascunho <ruleset>`; rascunho
/// existente no projeto é reaproveitado (reclassificação é upsert).
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
    let layer_id = ensure_cell_layer_in_project(&pool, &project.id, cell_layer_id).await?;

    // Ruleset explícito ou o ativo do projeto.
    let ruleset = match ruleset_id {
        Some(id) => PgRuleSetRepo::new(pool.clone())
            .get(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("ruleset `{id}` não existe"))?,
        None => PgRuleSetRepo::new(pool.clone())
            .find_active(&project.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!(
                    "nenhum ruleset ATIVO no projeto `{}`; ative um com \
                     `ruleset activate --project {} --name <n> --version <v>` \
                     ou passe --ruleset <id>",
                    project.name, project.name
                )
            })?,
    };
    if ruleset.project_id != project.id {
        return Err(format!(
            "ruleset `{}` pertence a outro projeto ({}); use um ruleset deste projeto",
            ruleset.id, ruleset.project_id
        ));
    }

    // Rascunho: existente é reaproveitado; inexistente é criado vinculado à grade.
    let draft_name = draft_layer_name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rascunho {}", ruleset.name));
    let drafts = PgDraftLayerRepo::new(pool.clone());
    let draft = match drafts
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|d| d.name == draft_name)
    {
        Some(existing) => existing,
        None => drafts
            .create(&DraftLayer::new(&project.id, &draft_name).with_cell_layer(Some(&layer_id)))
            .await
            .map_err(|e| format!("criar rascunho `{draft_name}`: {e}"))?,
    };
    if let Some(draft_layer) = draft.cell_layer_id.as_deref() {
        if draft_layer != layer_id {
            return Err(format!(
                "o rascunho `{draft_name}` pertence à grade `{draft_layer}`, mas \
                 a task usa a grade `{layer_id}`; informe outro --draft-layer ou \
                 reaproveite o rascunho da mesma grade"
            ));
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
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let saved = PgTaskRepo::create_on(&mut *tx, &task)
        .await
        .map_err(|e| e.to_string())?;
    let report = PgTaskRepo::enqueue_on(&mut tx, &saved.id, &layer_id)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;

    println!(
        "task criada: {}\tkind={}\tstatus=pending\truleset={} (v{})\trascunho={}",
        saved.id, saved.kind, ruleset.name, ruleset.version, draft.name
    );
    print_enqueue_report(&report);
    pool.close().await;
    Ok(())
}

fn print_enqueue_report(report: &EnqueueReport) {
    println!(
        "processos enfileirados: {} novos / {} células elegíveis (bloqueadas fora)",
        report.inserted, report.eligible
    );
}

/// `task run` — processa a task em lotes transacionais, com progresso e tempo.
///
/// `--limit N` processa no máximo N células nesta invocação (o resto continua
/// `pending`; rodar de novo retoma — nenhum `done` é reprocessado).
pub(crate) async fn task_run(
    dsn: Option<&str>,
    task_id: &str,
    limit: Option<u64>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task `{task_id}` não existe; veja `task list`"))?;
    match task.status {
        TaskStatus::Cancelled => {
            return Err(format!(
                "task `{}` está cancelada (cancelamento é terminal); crie outra task",
                task.id
            ));
        }
        TaskStatus::Done => {
            println!("task `{}` já está concluída; nada a processar", task.id);
            pool.close().await;
            return Ok(());
        }
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Failed => {}
    }
    let cell_layer_id = task.cell_layer_id.clone().ok_or_else(|| {
        format!(
            "task `{}` sem camada de células (a grade de origem foi apagada); \
             crie outra task",
            task.id
        )
    })?;
    let ruleset_id = task.ruleset_id.clone().ok_or_else(|| {
        format!(
            "task `{}` sem ruleset (o ruleset de origem foi apagado); crie outra task \
             com `task create`",
            task.id
        )
    })?;
    let draft_layer_id = task.draft_layer_id.clone().ok_or_else(|| {
        format!(
            "task `{}` sem rascunho (o rascunho de origem foi apagado); crie outra task",
            task.id
        )
    })?;

    // Pré-voo do rascunho: precisa existir e ser da MESMA grade da task —
    // decisões de `draft_cells` de outra grade nunca são válidas (o rascunho
    // é vinculado à grade na criação; mismatch só ocorre com banco editado
    // fora do fluxo ou task enfileirada para outra grade).
    let draft = PgDraftLayerRepo::new(pool.clone())
        .get(&draft_layer_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "task `{}` aponta para o rascunho `{draft_layer_id}`, que não \
                 existe (apagado?); crie outra task com `task create`",
                task.id
            )
        })?;
    if let Some(draft_layer) = draft.cell_layer_id.as_deref() {
        if draft_layer != cell_layer_id {
            return Err(format!(
                "o rascunho `{}` pertence à grade `{draft_layer}`, mas a task \
                 usa a grade `{cell_layer_id}`; crie outra task com `task create`",
                draft.name
            ));
        }
    }

    // Regras lidas UMA vez por run (ativas; o motor re-filtra por robustez).
    let all_rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset_id)
        .await
        .map_err(|e| e.to_string())?;
    let rules: Vec<_> = all_rules.iter().filter(|r| r.is_active).cloned().collect();
    if rules.is_empty() {
        return Err(format!(
            "nenhuma regra ATIVA no ruleset `{ruleset_id}`; adicione regras \
             (`rule add`) ou ative-as (`rule update --active true`) antes de rodar"
        ));
    }

    tasks
        .set_status(&task.id, TaskStatus::Running)
        .await
        .map_err(|e| e.to_string())?;

    let started = Instant::now();
    let mut processed: u64 = 0;
    let mut errors: u64 = 0;
    let mut classify_time = Duration::ZERO;
    let mut cancelled = false;

    loop {
        // Cancelamento cooperativo: checado ENTRE lotes; o lote em voo termina.
        if let Some(current) = tasks.get(&task.id).await.map_err(|e| e.to_string())? {
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

        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let claimed = PgTaskRepo::claim_on(&mut *tx, &task.id, batch)
            .await
            .map_err(|e| e.to_string())?;
        if claimed.is_empty() {
            tx.commit().await.map_err(|e| e.to_string())?;
            break; // fila vazia: fim limpo
        }
        let cell_ids: Vec<String> = claimed.iter().map(|c| c.cell_id.clone()).collect();
        let measures = PgTaskRepo::measures_for_cells_on(&mut *tx, &cell_layer_id, &cell_ids)
            .await
            .map_err(|e| e.to_string())?;
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
        let mut draft_rows: Vec<(String, String, String)> = Vec::new();
        let mut logs: Vec<CellLogEntry> = Vec::new();
        let mut finishes: Vec<(String, ProcessStatus, Option<String>)> = Vec::new();
        for (proc, output) in claimed.iter().zip(outputs.iter()) {
            match output.decision.class_id.clone() {
                Some(class_id) => {
                    draft_rows.push((
                        cell_layer_id.clone(),
                        output.cell_id.clone(),
                        class_id.clone(),
                    ));
                    logs.push(CellLogEntry::new(
                        &task.id,
                        &cell_layer_id,
                        &output.cell_id,
                        LogLevel::Info,
                        format!(
                            "classe={class_id} regra={}",
                            output.decision.rule_id.as_deref().unwrap_or("?")
                        ),
                    ));
                    finishes.push((proc.id.clone(), ProcessStatus::Done, None));
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
                    logs.push(CellLogEntry::new(
                        &task.id,
                        &cell_layer_id,
                        &output.cell_id,
                        LogLevel::Warn,
                        format!("nenhuma classe decidida ({missing})"),
                    ));
                    finishes.push((proc.id.clone(), ProcessStatus::Done, None));
                }
            }
        }

        let written = run_batch_writes(
            &mut tx,
            &draft_layer_id,
            &ruleset_id,
            &draft_rows,
            &logs,
            &finishes,
        )
        .await;
        match written {
            Ok(rows) => {
                // Commit único do lote: claim + rascunho + log + finishes.
                tx.commit().await.map_err(|e| e.to_string())?;
                processed += claimed.len() as u64;
                println!(
                    "lote ok: {} células ({} linhas de rascunho, {} logs)",
                    claimed.len(),
                    rows.0,
                    rows.1
                );
            }
            Err(batch_error) => {
                // Lote falhou (ex.: gatilho de integridade): o rollback devolve
                // TODOS os processos a `pending`. Degrada para célula a célula
                // para isolar o culpado — bom vira done, ruim vira error.
                tx.rollback().await.map_err(|e| e.to_string())?;
                println!(
                    "lote falhou ({}); degradando para célula a célula…",
                    shorten_error(&batch_error.to_string())
                );
                for cell_id in &cell_ids {
                    match run_single_cell(
                        &pool,
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
                            eprintln!("  erro na célula {cell_id}: {}", shorten_error(&cell_error));
                        }
                    }
                }
            }
        }
    }

    let final_status = tasks
        .refresh_status(&task.id)
        .await
        .map_err(|e| e.to_string())?;
    let counts = tasks.counts(&task.id).await.map_err(|e| e.to_string())?;
    println!(
        "run concluído: processadas={} erros={} tempo={:?} (classificação {:?})",
        processed,
        errors,
        started.elapsed(),
        classify_time
    );
    if cancelled {
        println!("task cancelada entre lotes; processos pendentes ficam na fila");
    }
    print_task_counts(&counts);
    println!(
        "status da task: {}",
        final_status.map_or("cancelled", TaskStatus::as_str)
    );
    pool.close().await;
    Ok(())
}

/// Escritas do lote (rascunho + log + finishes) no MESMO commit.
async fn run_batch_writes(
    tx: &mut sqlx::PgConnection,
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
///   separada para o erro não engolir a marcação.
async fn run_single_cell(
    pool: &PgPool,
    task_id: &str,
    cell_layer_id: &str,
    draft_layer_id: &str,
    ruleset_id: &str,
    rules: &[ta_core::rule::Rule],
    cell_id: &str,
) -> CellRunResult {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
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
    let output = &outputs[0];

    // Escritas do processo (rascunho + log + finish) nesta transação; o commit
    // fica fora do bloco para o rollback do erro manter `tx` utilizável.
    let write: Result<(), ta_db::Error> = async {
        if let Some(class_id) = output.decision.class_id.clone() {
            PgTaskRepo::draft_upsert_on(
                &mut *tx,
                draft_layer_id,
                ruleset_id,
                &[(cell_layer_id.to_owned(), cell_id.to_owned(), class_id)],
            )
            .await?;
        }
        let log = log_for_decision(task_id, cell_layer_id, cell_id, output);
        PgTaskRepo::append_logs_on(&mut *tx, std::slice::from_ref(&log)).await?;
        PgTaskRepo::finish_on(&mut *tx, &proc.id, ProcessStatus::Done, None).await?;
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

/// Log da decisão (mesma forma do lote) — usado pelo caminho degradado.
fn log_for_decision(
    task_id: &str,
    cell_layer_id: &str,
    cell_id: &str,
    output: &ta_engine::ClassOutput,
) -> CellLogEntry {
    match output.decision.class_id.clone() {
        Some(class_id) => CellLogEntry::new(
            task_id,
            cell_layer_id,
            cell_id,
            LogLevel::Info,
            format!(
                "classe={class_id} regra={}",
                output.decision.rule_id.as_deref().unwrap_or("?")
            ),
        ),
        None => {
            let missing = if output.decision.missing_measures.is_empty() {
                "nenhuma regra casou".to_owned()
            } else {
                format!(
                    "medidas ausentes: {}",
                    output.decision.missing_measures.join(", ")
                )
            };
            CellLogEntry::new(
                task_id,
                cell_layer_id,
                cell_id,
                LogLevel::Warn,
                format!("nenhuma classe decidida ({missing})"),
            )
        }
    }
}

fn cell_err(err: ta_db::Error) -> String {
    err.to_string()
}

/// Erros de banco são longos; o progresso da CLI mostra a primeira linha.
fn shorten_error(message: &str) -> String {
    message.lines().next().unwrap_or(message).to_owned()
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

/// `task cancel` — cancelamento cooperativo: a task nunca mais entrega
/// trabalho; o lote em voo termina (quem roda, conclui).
pub(crate) async fn task_cancel(dsn: Option<&str>, task_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task `{task_id}` não existe; veja `task list`"))?;
    if task.status == TaskStatus::Cancelled {
        println!("task `{}` já estava cancelada", task.id);
        pool.close().await;
        return Ok(());
    }
    // Task concluída NÃO vira `cancelled`: done é terminal (o cancelamento
    // é para trabalho EM VOO; flipar um histórico de sucesso corromperia a
    // leitura da fila). Sem pendentes/running/error não há nada a cancelar.
    let counts = tasks.counts(&task.id).await.map_err(|e| e.to_string())?;
    if counts.pending + counts.running + counts.error == 0 {
        pool.close().await;
        return Err(format!("task `{}` já concluída; nada a cancelar", task.id));
    }
    tasks
        .set_status(&task.id, TaskStatus::Cancelled)
        .await
        .map_err(|e| e.to_string())?;
    let counts = tasks.counts(&task.id).await.map_err(|e| e.to_string())?;
    println!(
        "task cancelada: {}\t(processos já concluídos permanecem: {})",
        task.id, counts.done
    );
    pool.close().await;
    Ok(())
}

/// `task retry` — repõe os processos `error` em `pending` (reprocessamento é
/// SEMPRE explícito; `done` nunca é reprocessado).
pub(crate) async fn task_retry(dsn: Option<&str>, task_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let tasks = PgTaskRepo::new(pool.clone());
    let task = tasks
        .get(task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task `{task_id}` não existe; veja `task list`"))?;
    if task.status == TaskStatus::Cancelled {
        return Err(format!(
            "task `{}` está cancelada; cancelamento é terminal — crie outra task",
            task.id
        ));
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let reset = PgTaskRepo::retry_errors_on(&mut *tx, &task.id)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    let status = tasks
        .refresh_status(&task.id)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "task retry: {} processo(s) error → pending; status={}",
        reset,
        status.map_or("cancelled", TaskStatus::as_str)
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
