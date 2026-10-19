//! Fase — etapa de trabalho dentro de um projeto.
//!
//! Uma **fase** organiza o fluxo de monitoramento: agrupa as AOIs e as camadas
//! de células de um passo (ex.: recorte de um período) e permite marcar que a
//! fase **bloqueia células**. O nome das tabelas é próprio do clean-room
//! (`phases` — Decisão 2 do plano; nada é derivado do produto legado).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Uma fase de um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da fase.
    pub project_id: String,
    /// Nome da fase, único dentro do projeto (`UNIQUE(project_id, name)`).
    pub name: String,
    /// Descrição livre (opcional; persiste como `''`).
    #[serde(default)]
    pub description: String,
    /// Quando verdadeiro, células desta fase podem ser marcadas como bloqueadas.
    #[serde(default)]
    pub cell_blocker: bool,
    /// Ordem de exibição dentro do projeto.
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Phase {
    /// Cria uma fase ainda não persistida (id vazio → o banco gera o UUID).
    #[must_use]
    pub fn new(project_id: &str, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            cell_blocker: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}
