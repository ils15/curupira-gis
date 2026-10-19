//! **Relatório de área consolidado** (Fase 5) — a orquestração que resolve a
//! grade, o SRS de cálculo e o rascunho, e devolve um [`AreaReport`] pronto
//! para tabela (CLI) ou visualização (GUI 5b).
//!
//! **Decisão 4 (regra de domínio, documentada também em `ta-db::repo_area`):**
//! a área NUNCA é calculada em 4674 (graus). O SRS de cálculo é, em ordem:
//! 1. `--srs` explícito do chamador (validado: precisa existir em
//!    `spatial_ref_sys` e ser métrico);
//! 2. `area_settings.srs_id` do projeto da grade;
//! 3. default **EPSG:5880** (Policônica SIRGAS 2000) quando o projeto não tem
//!    linha em `area_settings` — `AreaSettingsRepo::get_or_default`.
//!
//! A validação acontece aqui (mensagem antes do relatório) E no repositório
//! (`ensure_metric_srs` antes de cada query) — a invariante vale para
//! qualquer chamador futuro, não só para este caminho.

use ta_core::area::AreaReport;
use ta_db::repo_aoi::{AreaSettingsRepo, PgAreaSettingsRepo};
use ta_db::repo_area::{AreaRepo, PgAreaRepo};
use ta_db::repo_cell::{CellLayerRepo, PgCellLayerRepo};
use ta_db::repo_class::{DraftLayerRepo, PgDraftLayerRepo};
use ta_db::spatial::srid_units;
use ta_db::sqlx::PgPool;

use crate::error::FlowError;

/// Monta o relatório de área da grade `cell_layer_id` no SRS efetivo
/// (override → configuração do projeto → default 5880). Com `draft` (NOME do
/// rascunho, resolvido dentro do projeto da grade), acrescenta a área por
/// classe — rascunho sem decisões devolve `by_class` vazio; rascunho
/// inexistente é erro acionável ([`FlowError::DraftNotFound`]).
pub async fn report(
    pool: &PgPool,
    cell_layer_id: &str,
    draft: Option<&str>,
    srs_override: Option<i32>,
) -> Result<AreaReport, FlowError> {
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await?
        .ok_or_else(|| FlowError::CellLayerNotFound(cell_layer_id.to_owned()))?;

    // SRS efetivo do cálculo (Decisão 4): override > area_settings > 5880.
    let settings = PgAreaSettingsRepo::new(pool.clone())
        .get_or_default(&layer.project_id)
        .await?;
    let srs = srs_override.unwrap_or(settings.srs_id);
    // Validação antecipada: EPSG inexistente → UnknownSrid (acionável, vindo
    // de srid_units); SRS geográfico → GeographicSridForArea (área nunca em
    // graus). O repositório revalida antes de cada query (defesa no dado).
    let srs_units = srid_units(pool, srs).await?;
    if srs_units == "graus" {
        return Err(FlowError::Db(ta_db::Error::GeographicSridForArea(srs)));
    }

    let areas = PgAreaRepo::new(pool.clone());
    let summary = areas.area_summary_on(&layer.id, srs).await?;

    let by_class = match draft {
        None => Vec::new(),
        Some(name) => {
            let found = PgDraftLayerRepo::new(pool.clone())
                .list_for_project(&layer.project_id)
                .await?
                .into_iter()
                .find(|d| d.name == name);
            let draft_layer = found.ok_or_else(|| FlowError::DraftNotFound {
                name: name.to_owned(),
                project_id: layer.project_id.clone(),
            })?;
            areas.area_by_class_on(&draft_layer.id, srs).await?
        }
    };

    Ok(AreaReport {
        cell_layer_id: layer.id,
        cell_layer_name: layer.name,
        srs,
        srs_units,
        summary,
        by_class,
    })
}
