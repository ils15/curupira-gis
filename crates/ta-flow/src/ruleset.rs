//! **Preparo do fluxo — ativação de ruleset** (extraído do `ta-cli` na 5b).
//!
//! O ruleset ATIVO do projeto é o default do `task create` e, portanto, do
//! botão “Classificar” da GUI. A regra “no máximo um ativo por projeto” é do
//! banco (índice parcial único da migração `0013`); aqui ficam a resolução por
//! `(nome, versão)` e a mensagem acionável quando ele não existe.

use ta_core::rule::RuleSet;
use ta_db::repo_ruleset::{PgRuleSetRepo, RuleSetRepo};
use ta_db::sqlx::PgPool;

use crate::error::FlowError;

/// Torna o ruleset `(name, version)` o ATIVO do projeto (desativa os demais).
///
/// `version = None` ativa a versão mais recente da linhagem. `project_name`
/// entra só na mensagem acionável (o chamador já o tem resolvido). A regra de
/// unicidade do ativo é garantida pelo índice parcial do banco.
pub async fn activate(
    pool: &PgPool,
    project_id: &str,
    project_name: &str,
    name: &str,
    version: Option<i32>,
) -> Result<RuleSet, FlowError> {
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .find(project_id, name, version)
        .await?
        .ok_or_else(|| FlowError::RulesetNotInProject {
            name: name.to_owned(),
            version,
            project: project_name.to_owned(),
        })?;
    PgRuleSetRepo::new(pool.clone())
        .activate(&ruleset.id)
        .await
        .map_err(FlowError::RulesetActivate)?
        .ok_or_else(|| FlowError::RulesetVanished(ruleset.id))
}
