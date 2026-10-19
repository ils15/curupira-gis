//! Repositórios do **cálculo de área consolidado** (Fase 5).
//!
//! **Decisão 4 (retificada em 2026-10-18), regra de domínio local:** a
//! geometria é persistida em EPSG:4674 (graus), mas a área **NUNCA** é
//! calculada em graus — todo número de área sai de
//! `ST_Area(ST_Transform(geom, srs))` com SRS **métrico** (o de
//! `area_settings.srs_id`; default EPSG:5880 quando o projeto não tem linha —
//! ver `AreaSettingsRepo::get_or_default`). O guarda [`ensure_metric_srs`]
//! roda ANTES de cada query agregada: EPSG inexistente vira
//! [`Error::UnknownSrid`] e SRS geográfico vira [`Error::GeographicSridForArea`]
//! — erros acionáveis em vez de °² sem sentido.
//!
//! Agregação **no banco, uma query por relatório** (sem N+1): o total vem de
//! um único `SUM` sobre `cells` com `LEFT JOIN blocked_cells`; por classe, um
//! `GROUP BY` sobre `draft_cells ⋈ classes ⋈ cells`. Bind-only em todas as
//! queries (o SRID é `$n`, nunca concatenação).
//!
//! Nomenclatura: o sufixo `_on` aqui é "sobre a camada/rascunho indicado"
//! (`area_summary_on(cell_layer_id, srs)`), diferente de `repo_task`, onde
//! `_on` significa "sobre a transação indicada".

use async_trait::async_trait;
use sqlx::PgPool;
use ta_core::area::{AreaByCell, AreaByClass, AreaSummary};

use crate::spatial::srid_units;
use crate::{Error, Result};

/// Limite padrão do export por célula (`area_by_cell_on`) — o relatório célula
/// a célula é para CSV/JSON; grades do fluxo têm milhares de células, não
/// milhões. Um teto explícito evita materializar o banco por descuido.
pub const MAX_CELLS_EXPORT: i64 = 10_000;

/// Valida o SRS de CÁLCULO DE ÁREA: precisa existir em `spatial_ref_sys`
/// (senão `Error::UnknownSrid`) e ser **métrico em metros** — SRS geográfico
/// (graus) é rejeitado com `Error::GeographicSridForArea` (Decisão 4: área
/// nunca em graus) e SRS em outra unidade linear (ex.: pés, EPSG:2263) com
/// `Error::NonMetricSridForArea` (área é reportada em m²; pés² rotulado m² é
/// erro de unidade silencioso). Reutiliza a classificação de unidades do
/// PostGIS (`srid_units`), que já dá a mensagem acionável do EPSG desconhecido.
pub async fn ensure_metric_srs(pool: &PgPool, srs: i32) -> Result<()> {
    let units = srid_units(pool, srs).await?;
    if units == "graus" {
        return Err(Error::GeographicSridForArea(srs));
    }
    if units != "metros" {
        return Err(Error::NonMetricSridForArea { srs, units });
    }
    Ok(())
}

/// Contrato do cálculo de área (só PostGIS: `ST_Area` métrico é server-side).
#[async_trait]
pub trait AreaRepo: Send + Sync {
    /// Totais de área da grade no SRS métrico `srs` — UMA query agregada
    /// (`SUM` sobre `cells` com `LEFT JOIN blocked_cells`, sem N+1).
    ///
    /// `available_m2` é calculado como `total_m2 - blocked_m2` (a invariante
    /// `available + blocked == total` é do produtor, exata por construção).
    async fn area_summary_on(&self, cell_layer_id: &str, srs: i32) -> Result<AreaSummary>;

    /// Área por classe das decisões do rascunho (`draft_cells ⋈ cells`),
    /// ordenada por nome de classe. Rascunho sem decisões → vetor vazio.
    async fn area_by_class_on(&self, draft_layer_id: &str, srs: i32) -> Result<Vec<AreaByClass>>;

    /// Área por célula (base do export CSV/JSON), ordenada por `cell_id` e
    /// limitada a `min(limit, MAX_CELLS_EXPORT)`.
    async fn area_by_cell_on(
        &self,
        cell_layer_id: &str,
        srs: i32,
        limit: i64,
    ) -> Result<Vec<AreaByCell>>;
}

/// Implementação PostGIS do cálculo de área.
pub struct PgAreaRepo {
    pool: PgPool,
}

impl PgAreaRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AreaRepo for PgAreaRepo {
    async fn area_summary_on(&self, cell_layer_id: &str, srs: i32) -> Result<AreaSummary> {
        ensure_metric_srs(&self.pool, srs).await?;
        crate::error::check_id(cell_layer_id)?;
        // UMA query: área total e área bloqueada no MESMO scan (o `CASE` do
        // bloqueio evita segunda passagem). `ST_Area` sobre geometria
        // transformada no SRS métrico — NUNCA sobre a geometria 4674 crua.
        let row: (f64, f64, i64, i64) = sqlx::query_as(
            "SELECT \
                 COALESCE(SUM(ST_Area(ST_Transform(c.geom, $2::int4))), 0::float8), \
                 COALESCE(SUM( \
                     CASE WHEN b.cell_id IS NOT NULL \
                          THEN ST_Area(ST_Transform(c.geom, $2::int4)) END), 0::float8), \
                 COUNT(*)::int8, \
                 COUNT(b.cell_id)::int8 \
             FROM cells c \
             LEFT JOIN blocked_cells b \
                    ON b.cell_layer_id = c.cell_layer_id AND b.cell_id = c.cell_id \
             WHERE c.cell_layer_id = $1::uuid",
        )
        .bind(cell_layer_id)
        .bind(srs)
        .fetch_one(&self.pool)
        .await?;
        let (total_m2, blocked_m2, cells_total, cells_blocked) = row;
        Ok(AreaSummary {
            total_m2,
            blocked_m2,
            available_m2: total_m2 - blocked_m2,
            cells_total,
            cells_blocked,
        })
    }

    async fn area_by_class_on(&self, draft_layer_id: &str, srs: i32) -> Result<Vec<AreaByClass>> {
        ensure_metric_srs(&self.pool, srs).await?;
        crate::error::check_id(draft_layer_id)?;
        // `draft_cells` tem PK (draft, cell_layer, cell): uma linha por célula
        // decidida — o join com `cells` não duplica e o COUNT(*) é nº de
        // células. Sem rascunho/decisões → vetor vazio (não é erro).
        let rows: Vec<(String, i64, f64)> = sqlx::query_as(
            "SELECT cl.name, \
                    COUNT(*)::int8, \
                    COALESCE(SUM(ST_Area(ST_Transform(c.geom, $2::int4))), 0::float8) \
             FROM draft_cells dc \
             JOIN classes cl ON cl.id = dc.class_id \
             JOIN cells c ON c.cell_layer_id = dc.cell_layer_id AND c.cell_id = dc.cell_id \
             WHERE dc.draft_layer_id = $1::uuid \
             GROUP BY cl.name \
             ORDER BY cl.name",
        )
        .bind(draft_layer_id)
        .bind(srs)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(class_name, cells, area_m2)| AreaByClass {
                class_name,
                cells,
                area_m2,
            })
            .collect())
    }

    async fn area_by_cell_on(
        &self,
        cell_layer_id: &str,
        srs: i32,
        limit: i64,
    ) -> Result<Vec<AreaByCell>> {
        ensure_metric_srs(&self.pool, srs).await?;
        crate::error::check_id(cell_layer_id)?;
        // Teto defensivo mesmo com LIMIT parametrizado: o chamador não
        // materializa a grade inteira por esquecimento do LIMIT.
        let limit = limit.clamp(0, MAX_CELLS_EXPORT);
        let rows: Vec<(String, bool, f64)> = sqlx::query_as(
            "SELECT c.cell_id, \
                    (b.cell_id IS NOT NULL), \
                    ST_Area(ST_Transform(c.geom, $2::int4)) \
             FROM cells c \
             LEFT JOIN blocked_cells b \
                    ON b.cell_layer_id = c.cell_layer_id AND b.cell_id = c.cell_id \
             WHERE c.cell_layer_id = $1::uuid \
             ORDER BY c.cell_id \
             LIMIT $3::int8",
        )
        .bind(cell_layer_id)
        .bind(srs)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(cell_id, blocked, area_m2)| AreaByCell {
                cell_id,
                blocked,
                area_m2,
            })
            .collect())
    }
}
