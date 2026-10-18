//! Repositório de **fases** (`phases`).
//!
//! Bind-only: todo valor entra por parâmetro (`$n`); os nomes de tabela/coluna
//! são literais fixos no código, nunca interpolados de input externo.

use async_trait::async_trait;
use sqlx::PgPool;
use ta_core::phase::Phase;

use crate::Result;
use crate::error::check_id;

/// Contrato do repositório de fases.
#[async_trait]
pub trait PhaseRepo: Send + Sync {
    /// Insere a fase; id vazio deixa o banco gerar o UUID.
    async fn create(&self, phase: &Phase) -> Result<Phase>;
    /// Busca por id (`None` = ausente).
    ///
    /// **Consumo:** Fase 5 (UI de fases); sem chamador na CLI atual. API mantida.
    async fn get(&self, id: &str) -> Result<Option<Phase>>;
    /// Fases de um projeto, em ordem de exibição (`sort_order`, `name`).
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Phase>>;
    /// Remove a fase. `Ok(false)` = ausente.
    ///
    /// **Consumo:** Fase 5 (UI de fases); sem chamador na CLI atual. API mantida.
    async fn delete(&self, id: &str) -> Result<bool>;
}

type PhaseRow = (
    String,               // id
    String,               // project_id
    String,               // name
    String,               // description
    bool,                 // cell_blocker
    i32,                  // sort_order
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn phase_from_row(row: PhaseRow) -> Phase {
    Phase {
        id: row.0,
        project_id: row.1,
        name: row.2,
        description: row.3,
        cell_blocker: row.4,
        sort_order: row.5,
        created_at: row.6,
        updated_at: row.7,
    }
}

/// Fases sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgPhaseRepo {
    pool: PgPool,
}

impl PgPhaseRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PhaseRepo for PgPhaseRepo {
    async fn create(&self, phase: &Phase) -> Result<Phase> {
        check_id(&phase.project_id)?;
        let row: PhaseRow = sqlx::query_as(
            "INSERT INTO phases (id, project_id, name, description, cell_blocker, sort_order) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4, $5, $6) \
             RETURNING id::text, project_id::text, name, description, cell_blocker, \
                       sort_order, created_at, updated_at",
        )
        .bind(&phase.id)
        .bind(&phase.project_id)
        .bind(&phase.name)
        .bind(&phase.description)
        .bind(phase.cell_blocker)
        .bind(phase.sort_order)
        .fetch_one(&self.pool)
        .await?;
        Ok(phase_from_row(row))
    }

    async fn get(&self, id: &str) -> Result<Option<Phase>> {
        check_id(id)?;
        let row: Option<PhaseRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, description, cell_blocker, \
                    sort_order, created_at, updated_at \
             FROM phases WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(phase_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Phase>> {
        check_id(project_id)?;
        let rows: Vec<PhaseRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, description, cell_blocker, \
                    sort_order, created_at, updated_at \
             FROM phases WHERE project_id = $1::uuid ORDER BY sort_order, name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(phase_from_row).collect())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM phases WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}
