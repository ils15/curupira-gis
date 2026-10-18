//! Repositórios de **camadas de células**, **células** e **células bloqueadas**.
//!
//! - `cell_layers` — a grade regular (`resolution` + origem nas unidades de
//!   `cell_layers.srid`) ligada a uma AOI.
//! - `cells` — um polígono 4674 por célula, com id determinístico
//!   (`"<col>_<row>"`) e índices linha/coluna.
//! - `blocked_cells` — células marcadas como bloqueadas (motivo livre).
//!
//! A geometria é convertida **exclusivamente** por [`ta_core::geo`] (ponto único
//! do projeto). O insert em lote usa `INSERT … SELECT FROM UNNEST` (bind-only),
//! reaproveitando o padrão batched já usado como fallback em `repo.rs`, e é
//! `ON CONFLICT DO NOTHING` sobre `UNIQUE(cell_layer_id, cell_id)` — o que torna
//! a gravação de uma grade **idempotente**. `bulk_insert` roda em **uma
//! transação**, para que falha em qualquer lote não deixe a grade parcial.
//!
//! **Reprojeção (Decisão 4, retificada em 2026-10-18):** as células chegam
//! geradas no SRS da camada (`cell_layers.srid` — graus ou metros). O
//! armazenamento é **sempre** EPSG:4674; quando o SRS da grade difere, o insert
//! aplica `ST_Transform(ST_SetSRID(geom, srid), 4674)`. Quando a grade já é
//! 4674, nenhuma transformação é aplicada (evita round-trip desnecessário).

use async_trait::async_trait;
use geo_types::Geometry;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::postgres::PgConnection;
use ta_core::cell::{BlockedCell, Cell, CellLayer};
use ta_core::geo;

use crate::error::check_id;
use crate::spatial::{TARGET_SRID, bytes_to_hex};
use crate::{Error, Result};

/// Contrato do repositório de camadas de células.
#[async_trait]
pub trait CellLayerRepo: Send + Sync {
    /// Cria a camada de células; id vazio → UUID do banco.
    async fn create(&self, layer: &CellLayer) -> Result<CellLayer>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<CellLayer>>;
    /// Camadas de células de um projeto, por nome.
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<CellLayer>>;
}

/// Contrato do repositório de células.
#[async_trait]
pub trait CellRepo: Send + Sync {
    /// Insere as células em lote (idempotente por `UNIQUE(cell_layer_id, cell_id)`).
    ///
    /// `srid` é o EPSG em que as geometrias de `cells` estão expressas e
    /// **precisa** ser o mesmo de `cell_layers.srid`; divergência é erro
    /// ([`Error::SridLayerMismatch`]). Devolve o número de linhas
    /// **efetivamente** inseridas.
    async fn bulk_insert(&self, cell_layer_id: &str, cells: &[Cell], srid: i32) -> Result<u64>;
    /// Células da camada, em ordem espacial (`row_index`, `col_index`).
    async fn list(&self, cell_layer_id: &str) -> Result<Vec<Cell>>;
    /// Quantidade de células da camada.
    async fn count(&self, cell_layer_id: &str) -> Result<u64>;
    /// A célula existe nesta camada?
    async fn exists(&self, cell_layer_id: &str, cell_id: &str) -> Result<bool>;
    /// Envelope (`min_x, min_y, max_x, max_y`) via `ST_Extent`; `None` = vazia.
    async fn extent(&self, cell_layer_id: &str) -> Result<Option<[f64; 4]>>;
}

/// Contrato do repositório de células bloqueadas.
#[async_trait]
pub trait BlockedCellRepo: Send + Sync {
    /// Marca a célula como bloqueada (insere/atualiza o motivo).
    async fn block(&self, blocked: &BlockedCell) -> Result<BlockedCell>;
    /// Células bloqueadas da camada, por `cell_id`.
    async fn list(&self, cell_layer_id: &str) -> Result<Vec<BlockedCell>>;
    /// Remove o bloqueio. `Ok(false)` = não estava bloqueada.
    ///
    /// **Consumo:** Fase 5 (UI de células bloqueadas); sem chamador na CLI
    /// atual (só `block`/`list`). API mantida.
    async fn unblock(&self, cell_layer_id: &str, cell_id: &str) -> Result<bool>;
}

type CellLayerRow = (
    String,               // id
    String,               // project_id
    String,               // aoi_layer_id
    String,               // name
    f64,                  // resolution
    i32,                  // srid
    f64,                  // origin_x
    f64,                  // origin_y
    Option<String>,       // phase_id
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn cell_layer_from_row(row: CellLayerRow) -> CellLayer {
    CellLayer {
        id: row.0,
        project_id: row.1,
        aoi_layer_id: row.2,
        name: row.3,
        resolution: row.4,
        srid: row.5,
        origin_x: row.6,
        origin_y: row.7,
        phase_id: row.8,
        created_at: row.9,
        updated_at: row.10,
    }
}

/// Camadas de células sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgCellLayerRepo {
    pool: PgPool,
}

impl PgCellLayerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `create` sobre o **executor** dado (pool ou transação), para compor a
    /// criação da camada + o insert das células em UMA transação.
    pub async fn create_on<'e, E>(exec: E, layer: &CellLayer) -> Result<CellLayer>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        check_id(&layer.project_id)?;
        check_id(&layer.aoi_layer_id)?;
        // `srid` = SRS em que resolução/origem foram expressos (graus/metros).
        // Não é o SRID de persistência (células ficam em `TARGET_SRID` = 4674).
        // A existência do EPSG é garantida pela FK `cell_layers.srid →
        // spatial_ref_sys(srid)`; a mensagem acionável fica em
        // `spatial::srid_units` (usada pela CLI).
        let row: CellLayerRow = sqlx::query_as(
            "INSERT INTO cell_layers \
                 (id, project_id, aoi_layer_id, name, resolution, srid, origin_x, origin_y, phase_id) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), \
                     $2::uuid, $3::uuid, $4, $5, $6, $7, $8, $9::uuid) \
             RETURNING id::text, project_id::text, aoi_layer_id::text, name, resolution, \
                       srid, origin_x, origin_y, phase_id::text, created_at, updated_at",
        )
        .bind(&layer.id)
        .bind(&layer.project_id)
        .bind(&layer.aoi_layer_id)
        .bind(&layer.name)
        .bind(layer.resolution)
        .bind(layer.srid)
        .bind(layer.origin_x)
        .bind(layer.origin_y)
        .bind(&layer.phase_id)
        .fetch_one(exec)
        .await?;
        Ok(cell_layer_from_row(row))
    }
}

#[async_trait]
impl CellLayerRepo for PgCellLayerRepo {
    async fn create(&self, layer: &CellLayer) -> Result<CellLayer> {
        Self::create_on(&self.pool, layer).await
    }

    async fn get(&self, id: &str) -> Result<Option<CellLayer>> {
        check_id(id)?;
        let row: Option<CellLayerRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, aoi_layer_id::text, name, resolution, \
                    srid, origin_x, origin_y, phase_id::text, created_at, updated_at \
             FROM cell_layers WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(cell_layer_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<CellLayer>> {
        check_id(project_id)?;
        let rows: Vec<CellLayerRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, aoi_layer_id::text, name, resolution, \
                    srid, origin_x, origin_y, phase_id::text, created_at, updated_at \
             FROM cell_layers WHERE project_id = $1::uuid ORDER BY name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(cell_layer_from_row).collect())
    }
}

/// Células sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgCellRepo {
    pool: PgPool,
}

/// Tamanho do lote do `INSERT … UNNEST` (mesmo racional do fallback de features).
const CHUNK: usize = 1000;

impl PgCellRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `bulk_insert` sobre a **conexão** dada (de um pool ou de uma transação).
    ///
    /// Loop em lotes de [`CHUNK`] reutilizando a mesma conexão — o executor é
    /// consumido por referência (`&mut PgConnection`), então a mesma conexão
    /// serve todos os lotes.
    ///
    /// `srid` declara em que EPSG as geometrias de `cells` estão expressas e é
    /// **validado** contra `cell_layers.srid` (o SRS em que a grade foi
    /// definida): expressar a célula em outro SRS faria o `ST_Transform`
    /// reprojetar lixo sem erro. O armazenamento é **sempre** `TARGET_SRID`
    /// (4674): quando o SRS da grade difere, aplica `ST_Transform`; quando é
    /// 4674, grava direto (sem transformação).
    pub async fn bulk_insert_on(
        conn: &mut PgConnection,
        cell_layer_id: &str,
        cells: &[Cell],
        srid: i32,
    ) -> Result<u64> {
        if cells.is_empty() {
            return Ok(0);
        }
        check_id(cell_layer_id)?;
        let srid_u32 = u32::try_from(srid).map_err(|_| Error::UnknownSrid(srid))?;
        // SRS da grade: define as unidades das coordenadas recebidas. Divergir
        // aqui não é erro de arredondamento — é dado no SRS errado.
        let layer_srid: i32 =
            sqlx::query_scalar("SELECT srid FROM cell_layers WHERE id = $1::uuid")
                .bind(cell_layer_id)
                .fetch_one(&mut *conn)
                .await?;
        if srid != layer_srid {
            return Err(Error::SridLayerMismatch {
                input: srid,
                layer: layer_srid,
            });
        }
        let transform = srid_u32 != TARGET_SRID;
        // Pré-codifica a geometria no formato do PostGIS (hex EWKB), no SRS da
        // grade (o EWKB carrega esse SRID).
        let mut ids = Vec::with_capacity(cells.len());
        let mut geoms = Vec::with_capacity(cells.len());
        let mut rows_idx = Vec::with_capacity(cells.len());
        let mut cols_idx = Vec::with_capacity(cells.len());
        for cell in cells {
            let ewkb = geo::to_ewkb(&Geometry::Polygon(cell.geom.clone()), srid_u32)?;
            ids.push(cell.cell_id.clone());
            geoms.push(bytes_to_hex(&ewkb));
            rows_idx.push(cell.row_index);
            cols_idx.push(cell.col_index);
        }

        // Quando o SRS da grade já é o de armazenamento (4674), a expressão é o
        // próprio EWKB (sem `ST_Transform`) — nenhum round-trip desnecessário.
        // Fora disso, reprojeta para 4674 (o SRID de entrada é reforçado por
        // `ST_SetSRID`). Nenhuma interpolação de dado: só o literal/`$6`.
        let sql = if transform {
            "INSERT INTO cells (cell_layer_id, cell_id, geom, row_index, col_index) \
             SELECT $1::uuid, x.cid, \
                    ST_Transform(ST_SetSRID(x.g::geometry, $6::int4), 4674), \
                    x.r, x.c \
             FROM UNNEST($2::text[], $3::text[], $4::int4[], $5::int4[]) AS x(cid, g, r, c) \
             ON CONFLICT (cell_layer_id, cell_id) DO NOTHING"
        } else {
            "INSERT INTO cells (cell_layer_id, cell_id, geom, row_index, col_index) \
             SELECT $1::uuid, x.cid, x.g::geometry, x.r, x.c \
             FROM UNNEST($2::text[], $3::text[], $4::int4[], $5::int4[]) AS x(cid, g, r, c) \
             ON CONFLICT (cell_layer_id, cell_id) DO NOTHING"
        };

        let mut total = 0u64;
        for start in (0..cells.len()).step_by(CHUNK) {
            let end = (start + CHUNK).min(cells.len());
            let mut query = sqlx::query(sql)
                .bind(cell_layer_id)
                .bind(&ids[start..end])
                .bind(&geoms[start..end])
                .bind(&rows_idx[start..end])
                .bind(&cols_idx[start..end]);
            if transform {
                query = query.bind(srid);
            }
            let res = query.execute(&mut *conn).await?;
            total += res.rows_affected();
        }
        Ok(total)
    }
}

#[async_trait]
impl CellRepo for PgCellRepo {
    async fn bulk_insert(&self, cell_layer_id: &str, cells: &[Cell], srid: i32) -> Result<u64> {
        // Um lote grande abrange vários `CHUNK`s; sem transação, uma falha no
        // meio deixaria a grade parcial (e o seed, que decide idempotência por
        // `count > 0`, nunca repararia). Mesmo padrão de `persist_grid`.
        let mut tx = self.pool.begin().await?;
        let inserted = Self::bulk_insert_on(&mut tx, cell_layer_id, cells, srid).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn list(&self, cell_layer_id: &str) -> Result<Vec<Cell>> {
        check_id(cell_layer_id)?;
        let rows: Vec<(i64, String, Vec<u8>, i32, i32)> = sqlx::query_as(
            // Ordem **espacial** (linha, coluna), não lexical: `cell_id` é
            // texto, então "10_0" viria antes de "2_0" e o list perderia a
            // localidade de varredura da qual a Fase 3 depende.
            "SELECT id, cell_id, ST_AsEWKB(geom), row_index, col_index \
             FROM cells WHERE cell_layer_id = $1::uuid \
             ORDER BY row_index, col_index, cell_id",
        )
        .bind(cell_layer_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, cell_id, ewkb, row_index, col_index) in rows {
            let (geom, srid) = geo::from_ewkb(&ewkb)?;
            if let Some(srid) = srid {
                if srid != TARGET_SRID {
                    return Err(ta_core::Error::SridMismatch {
                        expected: TARGET_SRID,
                        got: srid,
                    }
                    .into());
                }
            }
            let geom = match geom {
                Geometry::Polygon(p) => p,
                other => {
                    return Err(Error::Postgis(format!(
                        "célula {cell_id} não é polígono: {other:?}"
                    )));
                }
            };
            out.push(Cell {
                id,
                cell_layer_id: cell_layer_id.to_owned(),
                cell_id,
                geom,
                row_index,
                col_index,
            });
        }
        Ok(out)
    }

    async fn count(&self, cell_layer_id: &str) -> Result<u64> {
        check_id(cell_layer_id)?;
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cells WHERE cell_layer_id = $1::uuid")
                .bind(cell_layer_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    async fn exists(&self, cell_layer_id: &str, cell_id: &str) -> Result<bool> {
        check_id(cell_layer_id)?;
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM cells WHERE cell_layer_id = $1::uuid AND cell_id = $2)",
        )
        .bind(cell_layer_id)
        .bind(cell_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(found)
    }

    async fn extent(&self, cell_layer_id: &str) -> Result<Option<[f64; 4]>> {
        check_id(cell_layer_id)?;
        let row: Option<(f64, f64, f64, f64)> = sqlx::query_as(
            "SELECT ST_XMin(e), ST_YMin(e), ST_XMax(e), ST_YMax(e) \
             FROM (SELECT ST_Extent(geom) AS e FROM cells WHERE cell_layer_id = $1::uuid) s \
             WHERE e IS NOT NULL",
        )
        .bind(cell_layer_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(min_x, min_y, max_x, max_y)| [min_x, min_y, max_x, max_y]))
    }
}

/// Células bloqueadas sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgBlockedCellRepo {
    pool: PgPool,
}

impl PgBlockedCellRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BlockedCellRepo for PgBlockedCellRepo {
    async fn block(&self, blocked: &BlockedCell) -> Result<BlockedCell> {
        check_id(&blocked.cell_layer_id)?;
        let row: (i64, String, String, Option<String>, time::OffsetDateTime) = sqlx::query_as(
            "INSERT INTO blocked_cells (cell_layer_id, cell_id, reason) \
             VALUES ($1::uuid, $2, $3) \
             ON CONFLICT (cell_layer_id, cell_id) DO UPDATE \
                 SET reason = EXCLUDED.reason, updated_at = now() \
             RETURNING id, cell_layer_id::text, cell_id, reason, created_at",
        )
        .bind(&blocked.cell_layer_id)
        .bind(&blocked.cell_id)
        .bind(&blocked.reason)
        .fetch_one(&self.pool)
        .await?;
        Ok(BlockedCell {
            id: row.0,
            cell_layer_id: row.1,
            cell_id: row.2,
            reason: row.3,
            created_at: row.4,
        })
    }

    async fn list(&self, cell_layer_id: &str) -> Result<Vec<BlockedCell>> {
        check_id(cell_layer_id)?;
        let rows: Vec<(i64, String, String, Option<String>, time::OffsetDateTime)> =
            sqlx::query_as(
                "SELECT id, cell_layer_id::text, cell_id, reason, created_at \
             FROM blocked_cells WHERE cell_layer_id = $1::uuid ORDER BY cell_id",
            )
            .bind(cell_layer_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, cell_layer_id, cell_id, reason, created_at)| BlockedCell {
                    id,
                    cell_layer_id,
                    cell_id,
                    reason,
                    created_at,
                },
            )
            .collect())
    }

    async fn unblock(&self, cell_layer_id: &str, cell_id: &str) -> Result<bool> {
        check_id(cell_layer_id)?;
        let res = sqlx::query(
            "DELETE FROM blocked_cells WHERE cell_layer_id = $1::uuid AND cell_id = $2",
        )
        .bind(cell_layer_id)
        .bind(cell_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}
