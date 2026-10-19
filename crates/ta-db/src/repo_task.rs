//! Repositórios da **fila de processos** (Fase 3): tasks, processos por
//! célula, log por célula, medidas por célula e gravação de rascunho.
//!
//! Concorrência e retomada (risco declarado do plano — modelo simples e
//! observável):
//! - **Enfileiramento idempotente:** um processo por (task, célula) —
//!   `ON CONFLICT DO NOTHING` sobre `UNIQUE(task_id, cell_layer_id, cell_id)`.
//!   Células bloqueadas NÃO são enfileiradas (política: bloqueada = fora do
//!   processamento).
//! - **Claim seguro para múltiplos workers:** `claim_on` faz
//!   `UPDATE … WHERE id IN (SELECT … FOR UPDATE SKIP LOCKED LIMIT $n)` — dois
//!   workers nunca reclamam o mesmo processo; o que sobra fica `pending`.
//! - **Retomada:** o lote é transacional (claim → trabalho → finish no MESMO
//!   commit); um worker que morre no meio do lote deixa tudo `pending` de
//!   novo. Processo `error` NÃO é reprocessado automaticamente — só com
//!   `retry_errors` (o `task retry` explícito do operador).
//! - **Estado da task derivado dos processos** (`refresh_status`): work
//!   restante → `pending`; tudo done → `done`; erro sem trabalho restante →
//!   `failed`; `cancelled` nunca é sobrescrito (cancelamento é terminal).
//!
//! Bind-only em todas as queries; os SQL são literais fixos.

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::postgres::PgConnection;
use ta_core::classification::CellMeasure;
use ta_core::task::{CellLogEntry, Process, ProcessStatus, Task, TaskKind, TaskStatus};

use crate::error::check_id;
use crate::{Error, Result};

/// Contagem de processos da task por estado — a leitura observável da fila.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCounts {
    /// Total de processos da task.
    pub total: i64,
    /// Pendentes (inclui células nunca processadas).
    pub pending: i64,
    /// Em execução (reclamados por algum worker).
    pub running: i64,
    /// Concluídos.
    pub done: i64,
    /// Com erro (aguardam `task retry` explícito).
    pub error: i64,
}

/// Resultado do enfileiramento — quantas células elegíveis existem e quantas
/// processos NOVOS foram criados (0 nas re-execuções = idempotência).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnqueueReport {
    /// Processos efetivamente inseridos nesta chamada.
    pub inserted: i64,
    /// Células elegíveis da camada (não bloqueadas) — o total da fila.
    pub eligible: i64,
}

/// Processo reclamado por um worker (`claim_on`) — o mínimo para executar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedProcess {
    /// Id do processo (para `finish_on`).
    pub id: String,
    /// Célula alvo.
    pub cell_id: String,
    /// Tentativas anteriores concluídas (done/error).
    pub attempts: i32,
}

type TaskRow = (
    String,               // id
    String,               // project_id
    String,               // kind
    String,               // name
    serde_json::Value,    // params
    Option<String>,       // cell_layer_id
    Option<String>,       // ruleset_id
    Option<String>,       // draft_layer_id
    String,               // status
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn task_kind_from_text(kind: &str) -> Result<TaskKind> {
    kind.parse()
        .map_err(|_| Error::Core(ta_core::Error::InvalidTaskKind(kind.to_owned())))
}

fn task_status_from_text(status: &str) -> Result<TaskStatus> {
    status
        .parse()
        .map_err(|_| Error::Core(ta_core::Error::InvalidTaskStatus(status.to_owned())))
}

fn process_status_from_text(status: &str) -> Result<ProcessStatus> {
    status
        .parse()
        .map_err(|_| Error::Core(ta_core::Error::InvalidProcessStatus(status.to_owned())))
}

fn task_from_row(row: TaskRow) -> Result<Task> {
    let kind = task_kind_from_text(&row.2)?;
    let status = task_status_from_text(&row.8)?;
    Ok(Task {
        id: row.0,
        project_id: row.1,
        kind,
        name: row.3,
        params: row.4,
        cell_layer_id: row.5,
        ruleset_id: row.6,
        draft_layer_id: row.7,
        status,
        created_at: row.9,
        updated_at: row.10,
    })
}

/// Linha de `processes` na ordem dos SELECTs — decodificada uma vez.
type ProcessRow = (
    String,               // id
    String,               // task_id
    String,               // cell_layer_id
    String,               // cell_id
    String,               // status
    i32,                  // attempts
    Option<String>,       // last_error
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn process_from_row(row: ProcessRow) -> Result<Process> {
    Ok(Process {
        id: row.0,
        task_id: row.1,
        cell_layer_id: row.2,
        cell_id: row.3,
        status: process_status_from_text(&row.4)?,
        attempts: row.5,
        last_error: row.6,
        created_at: row.7,
        updated_at: row.8,
    })
}

/// Contrato do repositório de tasks.
#[async_trait]
pub trait TaskRepo: Send + Sync {
    /// Cria a task; id vazio → UUID do banco.
    async fn create(&self, task: &Task) -> Result<Task>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<Task>>;
    /// Tasks do projeto (todas, quando `project_id` é `None`), mais recentes primeiro.
    async fn list(&self, project_id: Option<&str>) -> Result<Vec<Task>>;
    /// Grava o estado da task (`Ok(false)` = task ausente).
    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<bool>;
    /// Contagem de processos por estado (leitura observável da fila).
    async fn counts(&self, id: &str) -> Result<TaskCounts>;
    /// Recalcula o estado da task a partir dos processos e o devolve.
    async fn refresh_status(&self, id: &str) -> Result<Option<TaskStatus>>;
}

/// Medidas agrupadas por célula (`cell_id → medida → valor`) — insumo direto
/// do `classify_batch`.
pub type MeasuresByCell =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, f64>>;

/// Fila de processos sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgTaskRepo {
    pool: PgPool,
}

impl PgTaskRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Cria a task sobre o **executor** dado (pool ou transação), para compor
    /// criação + enfileiramento na MESMA transação (task sem processo não
    /// existe). Id vazio → UUID do banco.
    ///
    /// Só `classify` existe nesta fase — outro kind é erro tipado (o CHECK do
    /// banco é extensível, mas não inventamos kinds: YAGNI).
    pub async fn create_on<'e, E>(exec: E, task: &Task) -> Result<Task>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(&task.project_id)?;
        if !matches!(task.kind, TaskKind::Classify) {
            return Err(Error::Core(ta_core::Error::InvalidTaskKind(
                task.kind.as_str().to_owned(),
            )));
        }
        let params = if task.params.is_null() {
            serde_json::json!({})
        } else {
            task.params.clone()
        };
        let row: TaskRow = sqlx::query_as(
            "INSERT INTO tasks \
                 (id, project_id, kind, name, params, cell_layer_id, ruleset_id, \
                  draft_layer_id, status) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4, $5, \
                     NULLIF($6, '')::uuid, NULLIF($7, '')::uuid, NULLIF($8, '')::uuid, $9) \
             RETURNING id::text, project_id::text, kind, name, params, \
                       cell_layer_id::text, ruleset_id::text, draft_layer_id::text, status, \
                       created_at, updated_at",
        )
        .bind(&task.id)
        .bind(&task.project_id)
        .bind(task.kind.as_str())
        .bind(&task.name)
        .bind(&params)
        .bind(task.cell_layer_id.as_deref().unwrap_or(""))
        .bind(task.ruleset_id.as_deref().unwrap_or(""))
        .bind(task.draft_layer_id.as_deref().unwrap_or(""))
        .bind(task.status.as_str())
        .fetch_one(exec)
        .await?;
        task_from_row(row)
    }

    /// **Enfileira** um processo por célula NÃO bloqueada da camada —
    /// idempotente (`ON CONFLICT DO NOTHING`): rodar duas vezes produz as
    /// mesmas contagens. Devolve quantos processos NOVOS foram criados e o
    /// total de células elegíveis. Sempre dentro da transação de criação da
    /// task (mesmo padrão de `bulk_insert_on`).
    ///
    /// Política documentada: célula bloqueada está FORA do processamento —
    /// não gera processo nem log de erro.
    pub async fn enqueue_on(
        conn: &mut PgConnection,
        task_id: &str,
        cell_layer_id: &str,
    ) -> Result<EnqueueReport> {
        check_id(task_id)?;
        check_id(cell_layer_id)?;
        let inserted = sqlx::query(
            "INSERT INTO processes (task_id, cell_layer_id, cell_id) \
             SELECT $1::uuid, $2::uuid, c.cell_id FROM cells c \
             WHERE c.cell_layer_id = $2::uuid \
               AND NOT EXISTS (SELECT 1 FROM blocked_cells b \
                               WHERE b.cell_layer_id = c.cell_layer_id \
                                 AND b.cell_id = c.cell_id) \
             ON CONFLICT (task_id, cell_layer_id, cell_id) DO NOTHING",
        )
        .bind(task_id)
        .bind(cell_layer_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        // Contagem real de elegíveis (não confia no rows_affected para o total:
        // re-enfileirar uma task já populada insere 0 e continua válida).
        let eligible: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cells c \
             WHERE c.cell_layer_id = $1::uuid \
               AND NOT EXISTS (SELECT 1 FROM blocked_cells b \
                               WHERE b.cell_layer_id = c.cell_layer_id \
                                 AND b.cell_id = c.cell_id)",
        )
        .bind(cell_layer_id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(EnqueueReport {
            inserted: inserted as i64,
            eligible,
        })
    }

    /// **Claim de trabalho**: marca até `limit` processos `pending` da task
    /// como `running` e os devolve, ordenados por `cell_id`.
    ///
    /// `FOR UPDATE SKIP LOCKED` garante que DOIS workers simultâneos nunca
    /// reclamam o mesmo processo (o segundo pega o próximo disponível). Tasks
    /// canceladas não entregam trabalho (cancelamento cooperativo é checado
    /// aqui e entre lotes pelo executor). Só reclama `pending`: `error` volta
    /// a ser processável apenas por `retry_errors` (decisão documentada).
    pub async fn claim_on<'e, E>(exec: E, task_id: &str, limit: i64) -> Result<Vec<ClaimedProcess>>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(task_id)?;
        let rows: Vec<(String, String, i32)> = sqlx::query_as(
            "WITH picked AS ( \
                SELECT id FROM processes \
                WHERE task_id = $1::uuid AND status = 'pending' \
                  AND EXISTS (SELECT 1 FROM tasks t \
                              WHERE t.id = processes.task_id \
                                AND t.status <> 'cancelled') \
                ORDER BY cell_id \
                LIMIT $2 \
                FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE processes p SET status = 'running', updated_at = now() \
             FROM picked WHERE p.id = picked.id \
             RETURNING p.id::text, p.cell_id, p.attempts",
        )
        .bind(task_id)
        .bind(limit)
        .fetch_all(exec)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, cell_id, attempts)| ClaimedProcess {
                id,
                cell_id,
                attempts,
            })
            .collect())
    }

    /// Claim de UMA célula específica (`task_id`, `cell_id`) — usado pelo
    /// caminho degradado (célula a célula) quando o lote falha e o erro precisa
    /// ser isolado num processo só. `None` = processo não está `pending`
    /// (outro worker chegou primeiro).
    pub async fn claim_cell_on(
        conn: &mut PgConnection,
        task_id: &str,
        cell_id: &str,
    ) -> Result<Option<ClaimedProcess>> {
        check_id(task_id)?;
        let row: Option<(String, String, i32)> = sqlx::query_as(
            "UPDATE processes SET status = 'running', updated_at = now() \
             WHERE task_id = $1::uuid AND cell_id = $2 AND status = 'pending' \
             RETURNING id::text, cell_id, attempts",
        )
        .bind(task_id)
        .bind(cell_id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id, cell_id, attempts)| ClaimedProcess {
            id,
            cell_id,
            attempts,
        }))
    }

    /// Fecha com erro o processo de UMA célula específica — utilitário do
    /// caminho degradado: claim + fechamento com o motivo na MESMA transação
    /// (nenhuma janela em que a célula fica `pending` com erro "perdido").
    /// `None` = processo não estava `pending` (outro worker chegou antes).
    pub async fn claim_and_error_on(
        conn: &mut PgConnection,
        task_id: &str,
        cell_id: &str,
        last_error: &str,
    ) -> Result<Option<String>> {
        let Some(claimed) = Self::claim_cell_on(conn, task_id, cell_id).await? else {
            return Ok(None);
        };
        Self::finish_on(conn, &claimed.id, ProcessStatus::Error, Some(last_error)).await?;
        Ok(Some(claimed.id))
    }

    /// **Fecha** um processo reclamado: `done` ou `error` + `attempts++` +
    /// `last_error`. Idempotente o bastante para o caminho de retomada: grava
    /// o estado final do processo dono da chamada.
    pub async fn finish_on<'e, E>(
        exec: E,
        process_id: &str,
        status: ProcessStatus,
        last_error: Option<&str>,
    ) -> Result<()>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(process_id)?;
        match status {
            ProcessStatus::Done => {
                sqlx::query(
                    "UPDATE processes SET status = 'done', last_error = NULL, \
                            attempts = attempts + 1, updated_at = now() \
                     WHERE id = $1::uuid",
                )
                .bind(process_id)
                .execute(exec)
                .await?;
            }
            ProcessStatus::Error => {
                sqlx::query(
                    "UPDATE processes SET status = 'error', last_error = $2, \
                            attempts = attempts + 1, updated_at = now() \
                     WHERE id = $1::uuid",
                )
                .bind(process_id)
                .bind(last_error)
                .execute(exec)
                .await?;
            }
            // `running`/`pending` não são estados FINAIS: fechar processo é
            // done ou error, sempre.
            ProcessStatus::Pending | ProcessStatus::Running => {
                return Err(Error::Core(ta_core::Error::InvalidProcessStatus(
                    status.as_str().to_owned(),
                )));
            }
        }
        Ok(())
    }

    /// Repõe em `pending` todos os processos `error` da task (o
    /// `task retry` explícito). Devolve quantos foram repostos.
    pub async fn retry_errors_on<'e, E>(exec: E, task_id: &str) -> Result<i64>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(task_id)?;
        let res = sqlx::query(
            "UPDATE processes SET status = 'pending', updated_at = now() \
             WHERE task_id = $1::uuid AND status = 'error'",
        )
        .bind(task_id)
        .execute(exec)
        .await?;
        Ok(res.rows_affected() as i64)
    }

    /// Grava o log por célula em lote (níveis validados contra o CHECK).
    pub async fn append_logs_on<'e, E>(exec: E, entries: &[CellLogEntry]) -> Result<u64>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        if entries.is_empty() {
            return Ok(0);
        }
        let task_id = &entries[0].task_id;
        check_id(task_id)?;
        let mut layers = Vec::with_capacity(entries.len());
        let mut cells = Vec::with_capacity(entries.len());
        let mut levels = Vec::with_capacity(entries.len());
        let mut messages = Vec::with_capacity(entries.len());
        for entry in entries {
            check_id(&entry.task_id)?;
            if entry.task_id != *task_id {
                return Err(Error::InvalidId(entry.task_id.clone()));
            }
            check_id(&entry.cell_layer_id)?;
            layers.push(entry.cell_layer_id.clone());
            cells.push(entry.cell_id.clone());
            levels.push(entry.level.as_str().to_owned());
            messages.push(entry.message.clone());
        }
        let res = sqlx::query(
            "INSERT INTO cell_log (task_id, cell_layer_id, cell_id, level, message) \
             SELECT $1::uuid, u.layer, u.cell, u.level, u.msg \
             FROM UNNEST($2::uuid[], $3::text[], $4::text[], $5::text[]) \
                  AS u(layer, cell, level, msg)",
        )
        .bind(task_id)
        .bind(&layers)
        .bind(&cells)
        .bind(&levels)
        .bind(&messages)
        .execute(exec)
        .await?;
        Ok(res.rows_affected())
    }

    /// Grava as decisões de classificação do lote no rascunho — **upsert**:
    /// reclassificar a mesma célula no MESMO rascunho não duplica. Toda linha
    /// recebe o `ruleset_id` de proveniência e `decided_on = now()`.
    pub async fn draft_upsert_on<'e, E>(
        exec: E,
        draft_layer_id: &str,
        ruleset_id: &str,
        rows: &[(String, String, String)],
    ) -> Result<u64>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        if rows.is_empty() {
            return Ok(0);
        }
        check_id(draft_layer_id)?;
        check_id(ruleset_id)?;
        let mut layers = Vec::with_capacity(rows.len());
        let mut cells = Vec::with_capacity(rows.len());
        let mut classes = Vec::with_capacity(rows.len());
        for (cell_layer_id, cell_id, class_id) in rows {
            check_id(cell_layer_id)?;
            check_id(class_id)?;
            layers.push(cell_layer_id.clone());
            cells.push(cell_id.clone());
            classes.push(class_id.clone());
        }
        let res = sqlx::query(
            "INSERT INTO draft_cells \
                 (draft_layer_id, cell_layer_id, cell_id, class_id, ruleset_id, decided_on) \
             SELECT $1::uuid, u.layer, u.cell, u.class, $2::uuid, now() \
             FROM UNNEST($3::uuid[], $4::text[], $5::uuid[]) AS u(layer, cell, class) \
             ON CONFLICT (draft_layer_id, cell_layer_id, cell_id) \
             DO UPDATE SET class_id = EXCLUDED.class_id, \
                           ruleset_id = EXCLUDED.ruleset_id, decided_on = now()",
        )
        .bind(draft_layer_id)
        .bind(ruleset_id)
        .bind(&layers)
        .bind(&cells)
        .bind(&classes)
        .execute(exec)
        .await?;
        Ok(res.rows_affected())
    }

    /// Grava medidas por célula em lote — **upsert** (regravar a mesma medida
    /// atualiza valor/source). Valor não finito é recusado ANTES do banco com
    /// erro tipado acionável (o CHECK `isfinite` do banco é o backstop).
    pub async fn upsert_measures_on<'e, E>(exec: E, measures: &[CellMeasure]) -> Result<u64>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        if measures.is_empty() {
            return Ok(0);
        }
        let mut layers = Vec::with_capacity(measures.len());
        let mut cells = Vec::with_capacity(measures.len());
        let mut names = Vec::with_capacity(measures.len());
        let mut values = Vec::with_capacity(measures.len());
        let mut sources = Vec::with_capacity(measures.len());
        for measure in measures {
            check_id(&measure.cell_layer_id)?;
            if !measure.value.is_finite() {
                return Err(Error::NonFiniteMeasure {
                    measure: measure.measure.clone(),
                    value: measure.value,
                });
            }
            layers.push(measure.cell_layer_id.clone());
            cells.push(measure.cell_id.clone());
            names.push(measure.measure.clone());
            values.push(measure.value);
            sources.push(measure.source.clone());
        }
        let res = sqlx::query(
            "INSERT INTO cell_measures (cell_layer_id, cell_id, measure, value, source) \
             SELECT u.layer, u.cell, u.measure, u.value, u.source \
             FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::float8[], $5::text[]) \
                  AS u(layer, cell, measure, value, source) \
             ON CONFLICT (cell_layer_id, cell_id, measure) \
             DO UPDATE SET value = EXCLUDED.value, source = EXCLUDED.source, \
                           updated_on = now()",
        )
        .bind(&layers)
        .bind(&cells)
        .bind(&names)
        .bind(&values)
        .bind(&sources)
        .execute(exec)
        .await?;
        Ok(res.rows_affected())
    }

    /// Medidas da camada (opcionalmente de uma célula), ordenadas por
    /// célula/medida — leitura do `measure list`.
    pub async fn list_measures(
        &self,
        cell_layer_id: &str,
        cell_id: Option<&str>,
    ) -> Result<Vec<CellMeasure>> {
        check_id(cell_layer_id)?;
        let rows: Vec<(
            String,
            String,
            String,
            f64,
            Option<String>,
            time::OffsetDateTime,
        )> = sqlx::query_as(
            "SELECT cell_layer_id::text, cell_id, measure, value, source, updated_on \
                 FROM cell_measures \
                 WHERE cell_layer_id = $1::uuid AND ($2::text IS NULL OR cell_id = $2) \
                 ORDER BY cell_id, measure",
        )
        .bind(cell_layer_id)
        .bind(cell_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(cell_layer_id, cell_id, measure, value, source, updated_on)| CellMeasure {
                    cell_layer_id,
                    cell_id,
                    measure,
                    value,
                    source,
                    updated_on,
                },
            )
            .collect())
    }

    /// Medidas de um conjunto de células (uma claim), agrupadas por célula —
    /// insumo direto do `classify_batch` no executor da task. Aceita pool ou
    /// transação (mesmo executor das escritas do lote).
    pub async fn measures_for_cells_on<'e, E>(
        exec: E,
        cell_layer_id: &str,
        cell_ids: &[String],
    ) -> Result<MeasuresByCell>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        if cell_ids.is_empty() {
            return Ok(MeasuresByCell::new());
        }
        check_id(cell_layer_id)?;
        let rows: Vec<(String, String, f64)> = sqlx::query_as(
            "SELECT cell_id, measure, value FROM cell_measures \
             WHERE cell_layer_id = $1::uuid AND cell_id = ANY($2) \
             ORDER BY cell_id, measure",
        )
        .bind(cell_layer_id)
        .bind(cell_ids)
        .fetch_all(exec)
        .await?;
        let mut grouped = MeasuresByCell::new();
        for (cell_id, measure, value) in rows {
            grouped.entry(cell_id).or_default().insert(measure, value);
        }
        Ok(grouped)
    }

    /// Remove medidas da camada (opcionalmente de uma célula). Devolve quantas
    /// linhas caíram.
    pub async fn clear_measures(&self, cell_layer_id: &str, cell_id: Option<&str>) -> Result<u64> {
        check_id(cell_layer_id)?;
        let res = sqlx::query(
            "DELETE FROM cell_measures \
             WHERE cell_layer_id = $1::uuid AND ($2::text IS NULL OR cell_id = $2)",
        )
        .bind(cell_layer_id)
        .bind(cell_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Log por célula (opcionalmente filtrado por task), mais recente por último.
    pub async fn list_logs(
        &self,
        cell_layer_id: &str,
        cell_id: &str,
        task_id: Option<&str>,
    ) -> Result<Vec<CellLogEntry>> {
        check_id(cell_layer_id)?;
        if let Some(task_id) = task_id {
            check_id(task_id)?;
        }
        let rows: Vec<(String, String, String, String, String, time::OffsetDateTime)> =
            sqlx::query_as(
                "SELECT task_id::text, cell_layer_id::text, cell_id, level, message, created_on \
             FROM cell_log \
             WHERE cell_layer_id = $1::uuid AND cell_id = $2 \
               AND ($3::uuid IS NULL OR task_id = $3::uuid) \
             ORDER BY created_on, id",
            )
            .bind(cell_layer_id)
            .bind(cell_id)
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(
                |(task_id, cell_layer_id, cell_id, level, message, created_on)| {
                    Ok(CellLogEntry {
                        task_id,
                        cell_layer_id,
                        cell_id,
                        level: level.parse().map_err(|_| {
                            Error::Core(ta_core::Error::InvalidLogLevel(level.clone()))
                        })?,
                        message,
                        created_on,
                    })
                },
            )
            .collect()
    }

    /// Processos da task (opcionalmente filtrados por estado), por célula —
    /// leitura do `process list`.
    pub async fn list_processes(
        &self,
        task_id: &str,
        status: Option<ProcessStatus>,
    ) -> Result<Vec<Process>> {
        check_id(task_id)?;
        let rows: Vec<ProcessRow> = sqlx::query_as(
            "SELECT id::text, task_id::text, cell_layer_id::text, cell_id, status, attempts, \
                    last_error, created_at, updated_at \
             FROM processes \
             WHERE task_id = $1::uuid AND ($2::text IS NULL OR status = $2) \
             ORDER BY cell_id",
        )
        .bind(task_id)
        .bind(status.map(ProcessStatus::as_str))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(process_from_row).collect()
    }
}

#[async_trait]
impl TaskRepo for PgTaskRepo {
    async fn create(&self, task: &Task) -> Result<Task> {
        Self::create_on(&self.pool, task).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        check_id(id)?;
        let row: Option<TaskRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, kind, name, params, \
                    cell_layer_id::text, ruleset_id::text, draft_layer_id::text, status, \
                    created_at, updated_at \
             FROM tasks WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(task_from_row).transpose()
    }

    async fn list(&self, project_id: Option<&str>) -> Result<Vec<Task>> {
        if let Some(project_id) = project_id {
            check_id(project_id)?;
        }
        let rows: Vec<TaskRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, kind, name, params, \
                    cell_layer_id::text, ruleset_id::text, draft_layer_id::text, status, \
                    created_at, updated_at \
             FROM tasks \
             WHERE ($1::uuid IS NULL OR project_id = $1::uuid) \
             ORDER BY created_at DESC, id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(task_from_row).collect()
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<bool> {
        check_id(id)?;
        let res =
            sqlx::query("UPDATE tasks SET status = $2, updated_at = now() WHERE id = $1::uuid")
                .bind(id)
                .bind(status.as_str())
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn counts(&self, id: &str) -> Result<TaskCounts> {
        check_id(id)?;
        let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT count(*), \
                    count(*) FILTER (WHERE status = 'pending'), \
                    count(*) FILTER (WHERE status = 'running'), \
                    count(*) FILTER (WHERE status = 'done'), \
                    count(*) FILTER (WHERE status = 'error') \
             FROM processes WHERE task_id = $1::uuid",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(TaskCounts {
            total: counts.0,
            pending: counts.1,
            running: counts.2,
            done: counts.3,
            error: counts.4,
        })
    }

    async fn refresh_status(&self, id: &str) -> Result<Option<TaskStatus>> {
        check_id(id)?;
        let status: Option<String> = sqlx::query_scalar(
            "UPDATE tasks t SET \
                    status = CASE \
                        -- Cancelamento é terminal: nunca é sobrescrito.
                        WHEN t.status = 'cancelled' THEN 'cancelled' \
                        WHEN EXISTS (SELECT 1 FROM processes p \
                                     WHERE p.task_id = t.id \
                                       AND p.status IN ('pending', 'running')) \
                            THEN 'pending' \
                        WHEN EXISTS (SELECT 1 FROM processes p \
                                     WHERE p.task_id = t.id AND p.status = 'error') \
                            THEN 'failed' \
                        ELSE 'done' \
                    END, \
                    updated_at = now() \
             WHERE t.id = $1::uuid AND t.status <> 'cancelled' \
             RETURNING status",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        status.as_deref().map(task_status_from_text).transpose()
    }
}
