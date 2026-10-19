//! Classificação: **classes**, **grupos de classes** and **camadas de rascunho**.
//!
//! Uma classe é o rótulo final que uma célula recebe pela avaliação de regras
//! (`ta-engine`). Classes podem ser organizadas em grupos (`class_groups`) e
//! hierarquizadas por `parent_id` (uma classe-mãe, N filhas). A **camada de
//! rascunho** (`draft_layers`) agrupa a classificação em andamento de uma fase
//! ou de uma camada de células; `consolidated` marca que o rascunho virou
//! resultado definitivo.
//!
//! Os nomes de tabela/coluna são próprios do clean-room (Decisão 2 do plano).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::Error;

/// Grupo de classes de um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassGroup {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono do grupo.
    pub project_id: String,
    /// Nome do grupo, único dentro do projeto (`UNIQUE(project_id, name)`).
    pub name: String,
    /// Ordem de exibição dentro do projeto.
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl ClassGroup {
    /// Cria um grupo ainda não persistido (id vazio → o banco gera o UUID).
    #[must_use]
    pub fn new(project_id: &str, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            name: name.to_owned(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Classe de classificação de células.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Class {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da classe.
    pub project_id: String,
    /// Grupo da classe (`class_groups.id`); `None` = sem grupo. Apagar o grupo
    /// desvincula (`ON DELETE SET NULL`), não apaga a classe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_group_id: Option<String>,
    /// Classe-mãe (`classes.id`); `None` = classe raiz. Apagar a mãe desvincula
    /// as filhas (`ON DELETE SET NULL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Nome da classe, único dentro do projeto (`UNIQUE(project_id, name)`).
    pub name: String,
    /// Cor de exibição no formato `#RRGGBB` (validado no banco e na CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Ordem de exibição dentro do projeto.
    #[serde(default)]
    pub sort_order: i32,
    /// Classe ativa/inativa — **hoje é atributo de EXIBIÇÃO** (decisão da
    /// revisão da Fase 2, 2026-10-18): o motor de regras não consulta
    /// `classes` (avalia apenas `rules`, cujo `is_active` é consultado), então
    /// desativar uma classe não altera decisões de classificação; a UI deve
    /// apresentá-la como desativada. Se a classificação passar a filtrar por
    /// classes inativas, isso será decisão explícita de outra fase.
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

impl Class {
    /// Cria uma classe ainda não persistida.
    #[must_use]
    pub fn new(project_id: &str, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            class_group_id: None,
            parent_id: None,
            name: name.to_owned(),
            color: None,
            sort_order: 0,
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// Define o grupo (o chamador valida que é do mesmo projeto).
    #[must_use]
    pub fn with_group(mut self, group_id: Option<&str>) -> Self {
        self.class_group_id = group_id.map(str::to_owned);
        self
    }

    /// Define a classe-mãe (o chamador valida que é do mesmo projeto).
    #[must_use]
    pub fn with_parent(mut self, parent_id: Option<&str>) -> Self {
        self.parent_id = parent_id.map(str::to_owned);
        self
    }

    /// Define a cor de exibição (`#RRGGBB`; a validação final é do banco/CLI).
    #[must_use]
    pub fn with_color(mut self, color: Option<&str>) -> Self {
        self.color = color.map(str::to_owned);
        self
    }
}

/// Estado de uma camada de rascunho.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    /// Classificação em andamento (editável).
    Draft,
    /// Classificação consolidada (resultado do fluxo).
    Consolidated,
}

impl DraftState {
    /// Texto persistido em `draft_layers.state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Consolidated => "consolidated",
        }
    }
}

impl fmt::Display for DraftState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DraftState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "consolidated" => Ok(Self::Consolidated),
            other => Err(Error::InvalidDraftState(other.to_owned())),
        }
    }
}

/// Camada de rascunho de classificação de um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftLayer {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono do rascunho.
    pub project_id: String,
    /// Fase (`phases.id`); apagar a fase desvincula (`ON DELETE SET NULL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// Camada de células (`cell_layers.id`); apagar a grade apaga o rascunho
    /// (`ON DELETE CASCADE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_layer_id: Option<String>,
    /// Nome do rascunho, único dentro do projeto (`UNIQUE(project_id, name)`).
    pub name: String,
    /// Estado (`draft` | `consolidated`).
    #[serde(default = "default_state")]
    pub state: DraftState,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn default_state() -> DraftState {
    DraftState::Draft
}

impl DraftLayer {
    /// Cria um rascunho ainda não persistido (estado `draft`).
    #[must_use]
    pub fn new(project_id: &str, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            phase_id: None,
            cell_layer_id: None,
            name: name.to_owned(),
            state: DraftState::Draft,
            created_at: now,
            updated_at: now,
        }
    }

    /// Associa a fase (o chamador valida que é do mesmo projeto).
    #[must_use]
    pub fn with_phase(mut self, phase_id: Option<&str>) -> Self {
        self.phase_id = phase_id.map(str::to_owned);
        self
    }

    /// Associa a camada de células (o chamador valida que é do mesmo projeto).
    #[must_use]
    pub fn with_cell_layer(mut self, cell_layer_id: Option<&str>) -> Self {
        self.cell_layer_id = cell_layer_id.map(str::to_owned);
        self
    }
}

/// Medida observada de uma célula (`cell_measures`) — entrada da classificação.
///
/// Uma linha por (célula, medida); regravar a mesma medida é upsert
/// (**last-write-wins** por `(célula, medida)` — a última escrita vence). O
/// valor é sempre finito (CHECK no banco; validação no repositório).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellMeasure {
    /// Camada de células da medição.
    pub cell_layer_id: String,
    /// Célula medida (`"<col>_<row>"`).
    pub cell_id: String,
    /// Nome da medida (mesma gramática dos predicados; ex.: `ndvi`).
    pub measure: String,
    /// Valor observado (finito).
    pub value: f64,
    /// Proveniência livre (ex.: cena/índice de origem).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Cena de origem (`scenes.id`, opcional — proveniência da observação;
    /// apagar a cena desvincula (`ON DELETE SET NULL`), não apaga a medida).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    /// Momento da última gravação (`cell_measures.updated_on`).
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_on: OffsetDateTime,
}

/// Uma decisão de classificação gravada em um rascunho (`draft_cells`).
///
/// Linha de leitura para consulta/exportação: a classe decidida para a célula
/// por qual ruleset, e quando. Referências são por id (UUID); o exchange
/// resolve os nomes na fronteira de IO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCell {
    /// Rascunho dono da decisão.
    pub draft_layer_id: String,
    /// Grade da célula.
    pub cell_layer_id: String,
    /// Célula classificada (`"<col>_<row>"`).
    pub cell_id: String,
    /// Classe decidida.
    pub class_id: String,
    /// Ruleset que decidiu (proveniência).
    pub ruleset_id: String,
    /// Momento da decisão.
    pub decided_on: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_state_faz_round_trip_pelo_texto_persistido() {
        for state in [DraftState::Draft, DraftState::Consolidated] {
            let parsed: DraftState = state.as_str().parse().expect("estado válido");
            assert_eq!(parsed, state);
        }
        assert!("published".parse::<DraftState>().is_err());
    }

    #[test]
    fn classe_nova_e_ativa_e_sem_vinculos() {
        let class = Class::new("p1", "floresta");
        assert!(class.is_active);
        assert_eq!(class.class_group_id, None);
        assert_eq!(class.parent_id, None);
        assert_eq!(class.color, None);
    }
}
