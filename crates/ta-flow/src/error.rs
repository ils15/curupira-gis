//! Erros tipados do executor do fluxo ([`FlowError`]).
//!
//! As mensagens reproduzem EXATAMENTE as que a CLI da fila emitia quando a
//! lógica morava em `ta-cli::flow_task` (comportamento EXACTO inalterado —
//! a Fase 5b consome este crate e a CLI imprime `e.to_string()`). Erros de
//! banco transitam como [`FlowError::Db`] (transparente para `ta_db::Error`).
//!
//! Impl de `Display`/`Error` manual (sem thiserror): o contrato de
//! dependências do crate é deliberadamente mínimo — o `cargo tree -p ta-flow
//! -e normal` mostra SÓ core/db/engine (ver `Cargo.toml`).

use std::fmt;

use ta_core::task::TaskStatus;

/// Erro das operações de fluxo (fila e área).
#[derive(Debug)]
pub enum FlowError {
    /// Erro da camada de dados (sqlx, gatilhos, id inválido…).
    Db(ta_db::Error),

    // --- grade / projeto ---------------------------------------------------
    /// Camada de células inexistente (mensagens idênticas às da Fase 3).
    CellLayerNotFound(String),

    /// A grade existe, mas é de outro projeto.
    CellLayerOtherProject { id: String, project_id: String },

    // --- task run ----------------------------------------------------------
    /// Task inexistente (`task run`/`cancel`/`retry`).
    TaskNotFound(String),

    /// Task cancelada ao tentar RODAR (cancelamento é terminal).
    TaskCancelledForRun(String),

    /// Task cancelada ao tentar RETRYAR (redação própria do `task retry`).
    TaskCancelledForRetry(String),

    /// Task sem grade de origem (apagada fora do fluxo).
    TaskWithoutCellLayer(String),

    /// Task sem ruleset de origem (apagado fora do fluxo).
    TaskWithoutRuleset(String),

    /// Task sem rascunho de origem (apagado fora do fluxo).
    TaskWithoutDraft(String),

    /// O rascunho apontado pela task sumiu do banco.
    TaskDraftMissing { task_id: String, draft_id: String },

    /// Pré-voo do `run`: rascunho vinculado a outra grade.
    RunDraftLayerMismatch {
        draft_name: String,
        draft_layer: String,
        cell_layer: String,
    },

    /// Regras ativas ausentes no ruleset da task.
    NoActiveRules(String),

    // --- task create -------------------------------------------------------
    /// Ruleset explícito inexistente.
    RulesetNotFound(String),

    /// Projeto sem ruleset ativo (e sem `--ruleset`).
    NoActiveRuleset(String),

    /// Ruleset de outro projeto.
    RulesetOtherProject {
        ruleset_id: String,
        project_id: String,
    },

    /// Falha ao criar o rascunho da task (erro do banco preservado).
    DraftCreate { name: String, source: ta_db::Error },

    /// Pré-voo do `create`: rascunho reaproveitado pertence a outra grade.
    CreateDraftLayerMismatch {
        draft_name: String,
        draft_layer: String,
        layer_id: String,
    },

    // --- task cancel -------------------------------------------------------
    /// Task sem trabalho cancelável (done é terminal).
    NothingToCancel(String),

    // --- área --------------------------------------------------------------
    /// Rascunho informado no relatório de área não existe no projeto.
    DraftNotFound { name: String, project_id: String },

    // --- preparo do fluxo (Fase 5b: GUI e CLI no MESMO caminho) ------------
    /// Nome de fase duplicado no projeto (`UNIQUE(project_id, name)`).
    PhaseNameTaken { name: String },

    /// Falha ao gravar a fase (erro do banco preservado).
    PhaseCreate { name: String, source: ta_db::Error },

    /// Fase inexistente (vinculada a AOI/grade).
    PhaseNotFound(String),

    /// Fase de outro projeto.
    PhaseOtherProject { id: String, project_id: String },

    /// Camada vetorial inexistente (AOI é uma camada existente).
    LayerNotFound(String),

    /// Camada vetorial de outro projeto.
    LayerOtherProject { id: String, project_id: String },

    /// Falha ao gravar o vínculo de AOI.
    AoiCreate(ta_db::Error),

    /// AOI (`aoi_layers.id`) inexistente.
    AoiNotFound(String),

    /// Falha ao ler as feições da camada de AOI.
    AoiGeometry(ta_db::Error),

    /// Camada de AOI sem nenhuma feição poligonal.
    AoiNoPolygon,

    /// Falha ao reprojetar a AOI para o SRS da grade.
    AoiReproject { srid: i32, message: String },

    /// Reprojeção devolveu geometria que não é `MultiPolygon`.
    AoiReprojectUnexpected(String),

    /// Teto de células do projeto (`projects.max_cells`) excedido.
    MaxCellsExceeded { needed: u64, max: i32 },

    /// Projeto inexistente (leitura de `projects`).
    ProjectNotFound(String),

    /// O motor puro (`ta-engine`) recusou a grade (resolução/AOI/teto).
    GridEngine(String),

    /// Nome de camada de células duplicado no projeto.
    CellLayerNameTaken { name: String },

    /// Falha ao criar a camada de células.
    CellLayerCreate(ta_db::Error),

    /// Falha nas escritas da grade (insert de células).
    CellsInsert(ta_db::Error),

    /// Falha de transação da grade (`step` = "iniciando transação"/"commit").
    GridTransaction { step: &'static str, message: String },

    /// Ruleset nome/versão inexistente no projeto (mensagem do `activate`).
    RulesetNotInProject {
        name: String,
        version: Option<i32>,
        project: String,
    },

    /// O ruleset sumiu entre a busca e a ativação.
    RulesetVanished(String),

    /// Falha ao ativar o ruleset.
    RulesetActivate(ta_db::Error),

    /// Falha ao ler o CSV de medidas (arquivo ausente/ilegível).
    MeasuresCsv { path: String, message: String },
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowError::Db(err) => write!(f, "{err}"),
            FlowError::CellLayerNotFound(id) => {
                write!(f, "camada de células `{id}` não existe")
            }
            FlowError::CellLayerOtherProject { id, project_id } => write!(
                f,
                "camada de células `{id}` pertence a outro projeto ({project_id}); \
                 use uma grade deste projeto"
            ),
            FlowError::TaskNotFound(id) => {
                write!(f, "task `{id}` não existe; veja `task list`")
            }
            FlowError::TaskCancelledForRun(id) => write!(
                f,
                "task `{id}` está cancelada (cancelamento é terminal); crie outra task"
            ),
            FlowError::TaskCancelledForRetry(id) => write!(
                f,
                "task `{id}` está cancelada; cancelamento é terminal — crie outra task"
            ),
            FlowError::TaskWithoutCellLayer(id) => write!(
                f,
                "task `{id}` sem camada de células (a grade de origem foi apagada); \
                 crie outra task"
            ),
            FlowError::TaskWithoutRuleset(id) => write!(
                f,
                "task `{id}` sem ruleset (o ruleset de origem foi apagado); crie outra \
                 task com `task create`"
            ),
            FlowError::TaskWithoutDraft(id) => write!(
                f,
                "task `{id}` sem rascunho (o rascunho de origem foi apagado); crie outra task"
            ),
            FlowError::TaskDraftMissing { task_id, draft_id } => write!(
                f,
                "task `{task_id}` aponta para o rascunho `{draft_id}`, que não existe \
                 (apagado?); crie outra task com `task create`"
            ),
            FlowError::RunDraftLayerMismatch {
                draft_name,
                draft_layer,
                cell_layer,
            } => write!(
                f,
                "o rascunho `{draft_name}` pertence à grade `{draft_layer}`, mas a task \
                 usa a grade `{cell_layer}`; crie outra task com `task create`"
            ),
            FlowError::NoActiveRules(ruleset_id) => write!(
                f,
                "nenhuma regra ATIVA no ruleset `{ruleset_id}`; adicione regras (`rule add`) \
                 ou ative-as (`rule update --active true`) antes de rodar"
            ),
            FlowError::RulesetNotFound(id) => write!(f, "ruleset `{id}` não existe"),
            FlowError::NoActiveRuleset(project_name) => write!(
                f,
                "nenhum ruleset ATIVO no projeto `{project_name}`; ative um com \
                 `ruleset activate --project {project_name} --name <n> --version <v>` \
                 ou passe --ruleset <id>"
            ),
            FlowError::RulesetOtherProject {
                ruleset_id,
                project_id,
            } => write!(
                f,
                "ruleset `{ruleset_id}` pertence a outro projeto ({project_id}); use um \
                 ruleset deste projeto"
            ),
            FlowError::DraftCreate { name, source } => {
                write!(f, "criar rascunho `{name}`: {source}")
            }
            FlowError::CreateDraftLayerMismatch {
                draft_name,
                draft_layer,
                layer_id,
            } => write!(
                f,
                "o rascunho `{draft_name}` pertence à grade `{draft_layer}`, mas a task \
                 usa a grade `{layer_id}`; informe outro --draft-layer ou reaproveite o \
                 rascunho da mesma grade"
            ),
            FlowError::NothingToCancel(id) => {
                write!(f, "task `{id}` já concluída; nada a cancelar")
            }
            FlowError::DraftNotFound { name, project_id } => {
                write!(
                    f,
                    "rascunho `{name}` não existe no projeto `{project_id}`; veja `draft list`"
                )
            }
            FlowError::PhaseNameTaken { name } => write!(
                f,
                "já existe uma fase com o nome `{name}` neste projeto; escolha outro nome em --name"
            ),
            FlowError::PhaseCreate { name, source } => {
                write!(f, "criar fase `{name}`: {source}")
            }
            FlowError::PhaseNotFound(id) => write!(f, "fase `{id}` não existe"),
            FlowError::PhaseOtherProject { id, project_id } => write!(
                f,
                "fase `{id}` pertence a outro projeto ({project_id}); use uma fase deste projeto"
            ),
            FlowError::LayerNotFound(id) => write!(f, "camada `{id}` não existe"),
            FlowError::LayerOtherProject { id, project_id } => {
                write!(f, "camada `{id}` pertence a outro projeto ({project_id})")
            }
            FlowError::AoiCreate(source) => write!(f, "criar AOI: {source}"),
            FlowError::AoiNotFound(id) => write!(f, "AOI `{id}` não existe"),
            FlowError::AoiGeometry(source) => {
                write!(f, "lendo a geometria da AOI: {source}")
            }
            FlowError::AoiNoPolygon => {
                write!(f, "a AOI precisa de pelo menos uma feição poligonal")
            }
            FlowError::AoiReproject { srid, message } => {
                write!(f, "reprojetando a AOI para EPSG:{srid}: {message}")
            }
            FlowError::AoiReprojectUnexpected(other) => {
                write!(
                    f,
                    "reprojeção da AOI produziu geometria inesperada: {other}"
                )
            }
            FlowError::MaxCellsExceeded { needed, max } => write!(
                f,
                "grade grande demais para o projeto: {needed} células candidatas \
                 excedem max_cells={max} (aumente max_cells ou a resolução)"
            ),
            FlowError::ProjectNotFound(id) => write!(f, "projeto `{id}` não existe"),
            FlowError::GridEngine(message) => write!(f, "{message}"),
            FlowError::CellLayerNameTaken { name } => write!(
                f,
                "já existe uma camada de células com o nome `{name}` neste projeto; \
                 escolha outro nome em --name"
            ),
            FlowError::CellLayerCreate(source) => {
                write!(f, "criando camada de células: {source}")
            }
            FlowError::CellsInsert(source) => write!(f, "gravando células: {source}"),
            FlowError::GridTransaction { step, message } => write!(f, "{step}: {message}"),
            FlowError::RulesetNotInProject {
                name,
                version,
                project,
            } => write!(
                f,
                "ruleset `{name}`{} não existe no projeto `{project}`; crie com `ruleset create`",
                version
                    .map(|v| format!(" (versão {v})"))
                    .unwrap_or_default()
            ),
            FlowError::RulesetVanished(id) => {
                write!(f, "ruleset `{id}` sumiu durante a ativação")
            }
            FlowError::RulesetActivate(source) => write!(f, "ativar ruleset: {source}"),
            FlowError::MeasuresCsv { path, message } => {
                write!(f, "lendo `{path}`: {message}")
            }
        }
    }
}

impl std::error::Error for FlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FlowError::Db(err) => Some(err),
            FlowError::DraftCreate { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ta_db::Error> for FlowError {
    fn from(err: ta_db::Error) -> Self {
        FlowError::Db(err)
    }
}

impl From<ta_db::sqlx::Error> for FlowError {
    fn from(err: ta_db::sqlx::Error) -> Self {
        FlowError::Db(err.into())
    }
}

/// Estado final de task quando não há `TaskStatus` (task cancelada: o
/// `refresh_status` não sobrescreve `cancelled`) — atalho de exibição.
pub fn status_label(status: Option<TaskStatus>) -> &'static str {
    status.map_or("cancelled", TaskStatus::as_str)
}
