//! Repositórios de **cenas** (Fase 4): registro global de cenas e o vínculo
//! N:N com projetos (`project_scenes`).
//!
//! A cena é **global** (`UNIQUE(name)` no banco): o mesmo nome de cena não
//! pode existir duas vezes, então `create` de nome existente é erro tipado
//! ([`Error::SceneDuplicate`]) e o import do exchange REAPROVEITA a cena
//! existente pelo nome (portabilidade por nomes).
//!
//! Bind-only em todas as queries; os SQL são literais fixos. O footprint entra
//! como EWKB hexagonal (ponto único de conversão [`ta_core::geo`]) com SRID
//! 4674 embutido — a coluna `geometry(MultiPolygon,4674)` é o enforce final.

use async_trait::async_trait;
use geo_types::{Geometry, MultiPolygon};
use sqlx::PgPool;
use sqlx::Postgres;
use ta_core::scene::Scene;

use crate::error::check_id;
use crate::{Error, Result};

type SceneRow = (
    String,                       // id
    String,                       // name
    Option<time::OffsetDateTime>, // acquisition_on
    Option<Vec<u8>>,              // footprint (EWKB)
    serde_json::Value,            // metadata
    time::OffsetDateTime,         // created_on
    time::OffsetDateTime,         // updated_on
);

fn footprint_from_ewkb(ewkb: &[u8]) -> Result<MultiPolygon<f64>> {
    let (geom, _) = ta_core::geo::from_ewkb(ewkb)?;
    match geom {
        Geometry::MultiPolygon(mp) => Ok(mp),
        // O typmod `geometry(MultiPolygon,4674)` é o enforce; esta braço é
        // defesa em profundidade para um banco fora da garantia do schema.
        _ => Err(Error::Postgis(
            "cena com footprint que não é MultiPolygon (violação do typmod; \
             banco fora da garantia do schema)"
                .to_owned(),
        )),
    }
}

fn scene_from_row(row: SceneRow) -> Result<Scene> {
    let footprint = match row.3 {
        None => None,
        Some(ewkb) => Some(footprint_from_ewkb(&ewkb)?),
    };
    Ok(Scene {
        id: row.0,
        name: row.1,
        acquisition_on: row.2,
        footprint,
        metadata: row.4,
        created_on: row.5,
        updated_on: row.6,
    })
}

/// Contrato do repositório de cenas.
#[async_trait]
pub trait SceneRepo: Send + Sync {
    /// Cria a cena; nome existente é [`Error::SceneDuplicate`].
    async fn create(&self, scene: &Scene) -> Result<Scene>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<Scene>>;
    /// Busca por nome (`None` = ausente).
    async fn get_by_name(&self, name: &str) -> Result<Option<Scene>>;
    /// Cenas, opcionalmente só as vinculadas a um projeto (ordem por nome).
    async fn list(&self, project_id: Option<&str>) -> Result<Vec<Scene>>;
    /// Remove a cena (vínculos caem em CASCADE; medidas desvinculam por
    /// SET NULL). `Ok(false)` = cena não existia.
    async fn delete(&self, id: &str) -> Result<bool>;
    /// Vincula a cena ao projeto (idempotente). `Ok(true)` = novo vínculo.
    async fn link(&self, project_id: &str, scene_id: &str) -> Result<bool>;
    /// Desvincula a cena do projeto. `Ok(false)` = não estava vinculada.
    async fn unlink(&self, project_id: &str, scene_id: &str) -> Result<bool>;
    /// Projetos que usam a cena (nome) — pré-checagem do `scene remove`.
    async fn projects_using(&self, scene_id: &str) -> Result<Vec<String>>;
    /// Medidas com proveniência desta cena — contagem do `scene remove`.
    async fn measures_using(&self, scene_id: &str) -> Result<i64>;
}

/// Cenas sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgSceneRepo {
    pool: PgPool,
}

impl PgSceneRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `create` sobre o **executor** dado (pool ou transação) — o import do
    /// exchange grava cenas dentro da transação da seção.
    ///
    /// Nome existente NÃO sobrescreve: devolve [`Error::SceneDuplicate`] com
    /// o caminho de saída (reaproveitar pelo nome é política do import).
    pub async fn create_on<'e, E>(exec: E, scene: &Scene) -> Result<Scene>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        let name = scene.name.trim();
        if name.is_empty() {
            return Err(Error::Postgis(
                "nome de cena vazio: informe --name (ex.: `scene add --name cena_S2_A`)".to_owned(),
            ));
        }
        let footprint_hex = match &scene.footprint {
            None => None,
            Some(mp) => Some(crate::spatial::bytes_to_hex(&ta_core::geo::to_ewkb(
                &Geometry::from(mp.clone()),
                crate::spatial::TARGET_SRID,
            )?)),
        };
        let row: Option<SceneRow> = sqlx::query_as(
            "INSERT INTO scenes (id, name, acquisition_on, footprint, metadata) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2, $3, \
                     $4::geometry, $5) \
             ON CONFLICT (name) DO NOTHING \
             RETURNING id::text, name, acquisition_on, ST_AsEWKB(footprint), metadata, \
                       created_on, updated_on",
        )
        .bind(&scene.id)
        .bind(name)
        .bind(scene.acquisition_on)
        .bind(footprint_hex)
        .bind(&scene.metadata)
        .fetch_optional(exec)
        .await?;
        row.map(scene_from_row)
            .transpose()?
            .ok_or_else(|| Error::SceneDuplicate {
                name: name.to_owned(),
            })
    }
    /// `link` sobre o **executor** dado (pool ou transação) — o import do
    /// exchange vincula cenas dentro da transação da seção.
    pub async fn link_on<'e, E>(exec: E, project_id: &str, scene_id: &str) -> Result<bool>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(project_id)?;
        check_id(scene_id)?;
        let res = sqlx::query(
            "INSERT INTO project_scenes (project_id, scene_id) \
             VALUES ($1::uuid, $2::uuid) ON CONFLICT DO NOTHING",
        )
        .bind(project_id)
        .bind(scene_id)
        .execute(exec)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// `get_by_name` sobre o **executor** dado (pool ou transação) — o import
    /// do exchange lê a cena DENTRO da transação única (a cena criada nas
    /// seções anteriores ainda não está visível fora dela).
    pub async fn get_by_name_on<'e, E>(exec: E, name: &str) -> Result<Option<Scene>>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        let row: Option<SceneRow> = sqlx::query_as(
            "SELECT id::text, name, acquisition_on, ST_AsEWKB(footprint), metadata, \
                    created_on, updated_on \
             FROM scenes WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(exec)
        .await?;
        row.map(scene_from_row).transpose()
    }
}

#[async_trait]
impl SceneRepo for PgSceneRepo {
    async fn create(&self, scene: &Scene) -> Result<Scene> {
        Self::create_on(&self.pool, scene).await
    }

    async fn get(&self, id: &str) -> Result<Option<Scene>> {
        check_id(id)?;
        let row: Option<SceneRow> = sqlx::query_as(
            "SELECT id::text, name, acquisition_on, ST_AsEWKB(footprint), metadata, \
                    created_on, updated_on \
             FROM scenes WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(scene_from_row).transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Scene>> {
        Self::get_by_name_on(&self.pool, name).await
    }

    async fn list(&self, project_id: Option<&str>) -> Result<Vec<Scene>> {
        let rows: Vec<SceneRow> = match project_id {
            Some(project_id) => {
                check_id(project_id)?;
                sqlx::query_as(
                    "SELECT s.id::text, s.name, s.acquisition_on, ST_AsEWKB(s.footprint), \
                            s.metadata, s.created_on, s.updated_on \
                     FROM scenes s \
                     JOIN project_scenes ps ON ps.scene_id = s.id \
                     WHERE ps.project_id = $1::uuid ORDER BY s.name",
                )
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT id::text, name, acquisition_on, ST_AsEWKB(footprint), metadata, \
                            created_on, updated_on \
                     FROM scenes ORDER BY name",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter().map(scene_from_row).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM scenes WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn link(&self, project_id: &str, scene_id: &str) -> Result<bool> {
        check_id(project_id)?;
        check_id(scene_id)?;
        let res = sqlx::query(
            "INSERT INTO project_scenes (project_id, scene_id) \
             VALUES ($1::uuid, $2::uuid) ON CONFLICT DO NOTHING",
        )
        .bind(project_id)
        .bind(scene_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn unlink(&self, project_id: &str, scene_id: &str) -> Result<bool> {
        check_id(project_id)?;
        check_id(scene_id)?;
        let res = sqlx::query(
            "DELETE FROM project_scenes WHERE project_id = $1::uuid AND scene_id = $2::uuid",
        )
        .bind(project_id)
        .bind(scene_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn projects_using(&self, scene_id: &str) -> Result<Vec<String>> {
        check_id(scene_id)?;
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT p.name FROM projects p \
             JOIN project_scenes ps ON ps.project_id = p.id \
             WHERE ps.scene_id = $1::uuid ORDER BY p.name",
        )
        .bind(scene_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(names)
    }

    async fn measures_using(&self, scene_id: &str) -> Result<i64> {
        check_id(scene_id)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cell_measures WHERE scene_id = $1::uuid")
                .bind(scene_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }
}
