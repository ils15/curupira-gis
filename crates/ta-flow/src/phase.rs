//! **Preparo do fluxo — fase** (extraído do `ta-cli` na Fase 5b).
//!
//! A criação de fase é regra de domínio do fluxo (nome único no projeto,
//! descrição, ordem de exibição, papel de bloqueio de células): mora aqui para
//! que a CLI e o painel Monitoramento da GUI chamem **o mesmo** caminho. O
//! chamador resolve o projeto (nome/id é camada de apresentação) e formata a
//! saída; este módulo só escreve e devolve o registro persistido.
//!
//! Mensagens de erro: idênticas às da CLI desde a Fase 1 (os testes de CLI
//! afirmam o texto), incluindo o caso acionável de nome duplicado.

use ta_core::phase::Phase;
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::sqlx::PgPool;

use crate::error::FlowError;

/// Cria uma fase no projeto (id resolvido pelo chamador).
///
/// Nome duplicado vira [`FlowError::PhaseNameTaken`] — a única colisão
/// esperada é o `UNIQUE(project_id, name)`; o resto preserva o erro do banco
/// com o contexto `criar fase`.
pub async fn create(
    pool: &PgPool,
    project_id: &str,
    name: &str,
    description: String,
    sort_order: i32,
    cell_blocker: bool,
) -> Result<Phase, FlowError> {
    let mut phase = Phase::new(project_id, name);
    phase.description = description;
    phase.sort_order = sort_order;
    phase.cell_blocker = cell_blocker;
    PgPhaseRepo::new(pool.clone())
        .create(&phase)
        .await
        .map_err(|err| {
            if is_unique_violation(&err) {
                FlowError::PhaseNameTaken {
                    name: name.to_owned(),
                }
            } else {
                FlowError::PhaseCreate {
                    name: name.to_owned(),
                    source: err,
                }
            }
        })
}

/// `true` quando o banco recusou por unicidade (o PG devolve SQLSTATE 23505).
pub(crate) fn is_unique_violation(err: &ta_db::Error) -> bool {
    matches!(
        err,
        ta_db::Error::Sql(ta_db::sqlx::Error::Database(db)) if db.is_unique_violation()
    )
}

/// Resolve e valida que uma fase pertence ao projeto; devolve o id canônico.
///
/// Erro acionável quando a fase não existe ou é de outro projeto — a mesma
/// regra para AOI (`aoi create --phase`) e grade (`grid create --phase`).
pub async fn ensure_in_project(
    pool: &PgPool,
    project_id: &str,
    phase_id: &str,
) -> Result<String, FlowError> {
    let phase = PgPhaseRepo::new(pool.clone())
        .get(phase_id)
        .await?
        .ok_or_else(|| FlowError::PhaseNotFound(phase_id.to_owned()))?;
    if phase.project_id != project_id {
        return Err(FlowError::PhaseOtherProject {
            id: phase.id,
            project_id: phase.project_id,
        });
    }
    Ok(phase.id)
}
