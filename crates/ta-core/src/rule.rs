//! Regras e **rulesets versionados**.
//!
//! Um `ruleset` agrupa regras nomeadas de um projeto; cada nova escrita do
//! mesmo `(project_id, name)` cria a **próxima versão** (`version` 1, 2, …),
//! preservando as anteriores. Uma [`Rule`] associa um predicado a uma classe:
//!
//! ```text
//! ndvi >= 0.6 and nbr < 0.1
//! ```
//!
//! A **gramática e o parser** do predicado vivem no motor puro (`ta-engine`);
//! aqui ficam só os tipos e os erros tipados ([`RuleError`]) que a persistência
//! e a CLI compartilham.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Um conjunto versionado de regras de um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono do ruleset.
    pub project_id: String,
    /// Nome do ruleset; junto com o projeto identifica a linhagem de versões.
    pub name: String,
    /// Versão (> 0), incrementada a cada nova escrita do mesmo nome.
    pub version: i32,
    /// Descrição livre (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl RuleSet {
    /// Cria um ruleset ainda não persistido. `version` é atribuída pelo
    /// repositório na escrita (a próxima da linhagem), não pelo chamador.
    #[must_use]
    pub fn new(project_id: &str, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            name: name.to_owned(),
            version: 0,
            description: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Define a descrição.
    #[must_use]
    pub fn with_description(mut self, description: Option<&str>) -> Self {
        self.description = description.map(str::to_owned);
        self
    }
}

/// Uma regra de classificação: predicado + classe vencedora.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Ruleset dono.
    pub ruleset_id: String,
    /// Classe decidida quando o predicado é verdadeiro.
    pub class_id: String,
    /// Nome da regra, único dentro do ruleset (`UNIQUE(ruleset_id, name)`).
    /// É o desempate lexicográfico quando duas regras têm a mesma prioridade.
    pub name: String,
    /// Prioridade: **maior vence**.
    #[serde(default)]
    pub priority: i32,
    /// Predicado na gramática do motor (ex.: `ndvi >= 0.6 and nbr < 0.1`).
    pub predicate: String,
    /// Regra inativa é ignorada na avaliação.
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn default_true() -> bool {
    true
}

impl Rule {
    /// Cria uma regra ainda não persistida (prioridade 0, ativa).
    #[must_use]
    pub fn new(ruleset_id: &str, class_id: &str, name: &str, predicate: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            ruleset_id: ruleset_id.to_owned(),
            class_id: class_id.to_owned(),
            name: name.to_owned(),
            priority: 0,
            predicate: predicate.to_owned(),
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// Define a prioridade (maior vence).
    #[must_use]
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
}

/// Erros do predicado de regra — tipados e **com posição** (1-based, em bytes
/// do texto original). Nunca uma `String` solta.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// Predicado vazio ou só com espaços.
    #[error("predicado vazio: informe uma expressão como `ndvi >= 0.6 and nbr < 0.1`")]
    Empty,

    /// A expressão terminou antes do esperado.
    #[error("expressão incompleta na posição {pos}: esperado {expected}")]
    UnexpectedEnd { expected: &'static str, pos: usize },

    /// Token presente, mas de outro tipo.
    #[error("esperado {expected} na posição {pos}")]
    Expected { expected: &'static str, pos: usize },

    /// Número malformado (ex.: `1.2.3`, `--1`).
    #[error("número inválido `{text}` na posição {pos}")]
    InvalidNumber { text: String, pos: usize },

    /// Número fora de intervalo: literal não finito (overflow para `inf`) ou
    /// com mais de 17 dígitos significativos — além do que `f64` distingue,
    /// a comparação deixaria de ser a que o operador escreveu.
    #[error(
        "número fora de intervalo `{text}` na posição {pos}: use um literal \
             finito com até 17 dígitos significativos (precisão f64)"
    )]
    NumberOutOfRange { text: String, pos: usize },

    /// Operador de comparação fora do conjunto suportado.
    #[error("operador inválido na posição {pos}: use >, >=, <, <=, == ou !=")]
    InvalidOperator { pos: usize },
}
