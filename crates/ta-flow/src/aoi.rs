//! **Preparo do fluxo — AOI** (extraído do `ta-cli` na Fase 5b).
//!
//! AOI não é geometria nova: é uma **camada vetorial existente** eleita como
//! área de interesse (`aoi_layers`), opcionalmente com a coluna de atributo
//! que identifica cada feição e com uma fase do MESMO projeto. A validação de
//! projeto/fase e a escrita ficam aqui; a CLI e a GUI só resolvem o projeto e
//! exibem o resultado.

use ta_core::aoi::AoiLayer;
use ta_db::repo::{LayerRepo, PgLayerRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::sqlx::PgPool;

use crate::error::FlowError;
use crate::phase;

/// AOI criada + dados de exibição (o vínculo não carrega o nome da camada, e
/// a CLI/GUI mostram nome **e** id).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkedAoi {
    /// Vínculo persistido (`aoi_layers`).
    pub aoi: AoiLayer,
    /// Id da camada vetorial vinculada.
    pub layer_id: String,
    /// Nome da camada vetorial vinculada.
    pub layer_name: String,
}

/// Vincula a camada `layer_id` como AOI do projeto `project_id`.
///
/// Regras (as mesmas da CLI desde a Fase 1):
/// - a camada precisa existir e ser do projeto (erro acionável);
/// - `phase_id`, quando informado, precisa existir e ser do MESMO projeto;
/// - `id_column` é opcional e gravado como veio (sem trim — a coluna é dado).
pub async fn create(
    pool: &PgPool,
    project_id: &str,
    layer_id: &str,
    id_column: Option<String>,
    phase_id: Option<&str>,
) -> Result<LinkedAoi, FlowError> {
    let layer = PgLayerRepo::new(pool.clone())
        .get(layer_id)
        .await?
        .ok_or_else(|| FlowError::LayerNotFound(layer_id.to_owned()))?;
    if layer.project_id != project_id {
        return Err(FlowError::LayerOtherProject {
            id: layer_id.to_owned(),
            project_id: layer.project_id,
        });
    }
    let phase_id = match phase_id {
        Some(id) => Some(phase::ensure_in_project(pool, project_id, id).await?),
        None => None,
    };
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(project_id, &layer.id, id_column).with_phase(phase_id.as_deref()))
        .await
        .map_err(FlowError::AoiCreate)?;
    Ok(LinkedAoi {
        aoi,
        layer_id: layer.id,
        layer_name: layer.name,
    })
}
