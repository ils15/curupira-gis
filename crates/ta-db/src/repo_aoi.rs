//! Repositórios de **AOI** (`aoi_layers`) e das **configurações de área**
//! (`area_settings`).
//!
//! A AOI é uma camada vetorial existente vinculada ao projeto; `aoi_layers`
//! guarda o ponteiro e a coluna de identificação. Bind-only em todas as queries.

use async_trait::async_trait;
use sqlx::PgPool;
use ta_core::aoi::{AoiLayer, AreaSettings};

use crate::Result;
use crate::error::check_id;

/// Contrato do repositório de vínculos de AOI.
#[async_trait]
pub trait AoiLayerRepo: Send + Sync {
    /// Cria o vínculo projeto↔camada de AOI; id vazio → UUID do banco.
    async fn create(&self, aoi: &AoiLayer) -> Result<AoiLayer>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<AoiLayer>>;
    /// AOIs de um projeto, por nome de camada.
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<AoiLayer>>;
}

/// Contrato das configurações de cálculo de área de um projeto.
///
/// **Consumo:** Fase 5 (cálculo de ÁREA consolidado, EPSG:5880 ou `geography`).
/// A tabela `area_settings` é criada na Fase 1, mas ainda não há chamador na
/// CLI/GUI — o contrato fica pronto para a Fase 5.
#[async_trait]
pub trait AreaSettingsRepo: Send + Sync {
    /// Grava (insere ou substitui) as configurações do projeto.
    async fn set(&self, settings: &AreaSettings) -> Result<AreaSettings>;
    /// Configurações do projeto (`None` = não definidas).
    async fn get(&self, project_id: &str) -> Result<Option<AreaSettings>>;
}

type AoiRow = (
    String,               // id
    String,               // project_id
    String,               // layer_id
    Option<String>,       // aoi_id_column
    Option<String>,       // phase_id
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn aoi_from_row(row: AoiRow) -> AoiLayer {
    AoiLayer {
        id: row.0,
        project_id: row.1,
        layer_id: row.2,
        aoi_id_column: row.3,
        phase_id: row.4,
        created_at: row.5,
        updated_at: row.6,
    }
}

/// Vínculos de AOI sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgAoiLayerRepo {
    pool: PgPool,
}

impl PgAoiLayerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `create` sobre o **executor** dado (pool ou transação) — o import do
    /// exchange grava camada+features+AOI numa única transação.
    pub async fn create_on<'e, E>(exec: E, aoi: &AoiLayer) -> Result<AoiLayer>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(&aoi.project_id)?;
        check_id(&aoi.layer_id)?;
        let row: AoiRow = sqlx::query_as(
            "INSERT INTO aoi_layers (id, project_id, layer_id, aoi_id_column, phase_id) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, $4, $5::uuid) \
             RETURNING id::text, project_id::text, layer_id::text, aoi_id_column, phase_id::text, \
                       created_at, updated_at",
        )
        .bind(&aoi.id)
        .bind(&aoi.project_id)
        .bind(&aoi.layer_id)
        .bind(&aoi.aoi_id_column)
        .bind(&aoi.phase_id)
        .fetch_one(exec)
        .await?;
        Ok(aoi_from_row(row))
    }
}

#[async_trait]
impl AoiLayerRepo for PgAoiLayerRepo {
    async fn create(&self, aoi: &AoiLayer) -> Result<AoiLayer> {
        check_id(&aoi.project_id)?;
        check_id(&aoi.layer_id)?;
        let row: AoiRow = sqlx::query_as(
            "INSERT INTO aoi_layers (id, project_id, layer_id, aoi_id_column, phase_id) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, $4, $5::uuid) \
             RETURNING id::text, project_id::text, layer_id::text, aoi_id_column, phase_id::text, \
                       created_at, updated_at",
        )
        .bind(&aoi.id)
        .bind(&aoi.project_id)
        .bind(&aoi.layer_id)
        .bind(&aoi.aoi_id_column)
        .bind(&aoi.phase_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(aoi_from_row(row))
    }

    async fn get(&self, id: &str) -> Result<Option<AoiLayer>> {
        check_id(id)?;
        let row: Option<AoiRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, layer_id::text, aoi_id_column, phase_id::text, \
                    created_at, updated_at \
             FROM aoi_layers WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(aoi_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<AoiLayer>> {
        check_id(project_id)?;
        let rows: Vec<AoiRow> = sqlx::query_as(
            "SELECT a.id::text, a.project_id::text, a.layer_id::text, a.aoi_id_column, \
                    a.phase_id::text, a.created_at, a.updated_at \
             FROM aoi_layers a JOIN layers l ON l.id = a.layer_id \
             WHERE a.project_id = $1::uuid ORDER BY l.name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(aoi_from_row).collect())
    }
}

type AreaRow = (
    String,               // id
    String,               // project_id
    i32,                  // srs_id
    String,               // unit
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn area_from_row(row: AreaRow) -> AreaSettings {
    AreaSettings {
        id: row.0,
        project_id: row.1,
        srs_id: row.2,
        unit: row.3,
        created_at: row.4,
        updated_at: row.5,
    }
}

/// Configurações de área sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgAreaSettingsRepo {
    pool: PgPool,
}

impl PgAreaSettingsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AreaSettingsRepo for PgAreaSettingsRepo {
    async fn set(&self, settings: &AreaSettings) -> Result<AreaSettings> {
        check_id(&settings.project_id)?;
        let row: AreaRow = sqlx::query_as(
            "INSERT INTO area_settings (id, project_id, srs_id, unit) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4) \
             ON CONFLICT (project_id) DO UPDATE \
                 SET srs_id = EXCLUDED.srs_id, unit = EXCLUDED.unit, updated_at = now() \
             RETURNING id::text, project_id::text, srs_id, unit, created_at, updated_at",
        )
        .bind(&settings.id)
        .bind(&settings.project_id)
        .bind(settings.srs_id)
        .bind(&settings.unit)
        .fetch_one(&self.pool)
        .await?;
        Ok(area_from_row(row))
    }

    async fn get(&self, project_id: &str) -> Result<Option<AreaSettings>> {
        check_id(project_id)?;
        let row: Option<AreaRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, srs_id, unit, created_at, updated_at \
             FROM area_settings WHERE project_id = $1::uuid",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(area_from_row))
    }
}
