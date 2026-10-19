//! Cena de sensoriamento — imagem/observação de origem das medidas.
//!
//! Uma **cena** é o registro de origem de uma observação (ex.: passagem de
//! satélite): nome único no banco, aquisição opcional, footprint opcional
//! (MultiPolygon em EPSG:4674 — armazenamento canônico) e metadados livres.
//! O vínculo com projetos é N:N (`project_scenes`) e as medidas de célula
//! referenciam a cena por proveniência (`cell_measures.scene_id`, nullable —
//! uma medida pode existir sem cena).
//!
//! Os nomes de tabela/coluna são próprios do clean-room (Decisão 2 do plano).

use geo_types::MultiPolygon;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Uma cena registrada no banco (global; `UNIQUE(name)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Nome da cena, único no banco (`UNIQUE(name)`).
    pub name: String,
    /// Momento da aquisição (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_on: Option<OffsetDateTime>,
    /// Footprint da cena em EPSG:4674 (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<MultiPolygon<f64>>,
    /// Metadados livres (objeto JSON; `{}` quando ausente).
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_on: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_on: OffsetDateTime,
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

impl Scene {
    /// Cria uma cena ainda não persistida (id vazio → o banco gera o UUID).
    #[must_use]
    pub fn new(name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            name: name.to_owned(),
            acquisition_on: None,
            footprint: None,
            metadata: default_metadata(),
            created_on: now,
            updated_on: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cena_nova_sem_acessorios() {
        let s = Scene::new("cena S2 A");
        assert_eq!(s.name, "cena S2 A");
        assert!(s.acquisition_on.is_none());
        assert!(s.footprint.is_none());
        assert_eq!(s.metadata, serde_json::json!({}));
    }

    #[test]
    fn serde_de_cena_preenche_defaults() {
        let json = r#"{"id":"x","name":"c1","created_on":"2026-10-19T12:00:00Z","updated_on":"2026-10-19T12:00:00Z"}"#;
        let s: Scene = serde_json::from_str(json).expect("parse sem footprint/metadata");
        assert_eq!(s.metadata, serde_json::json!({}));
        assert!(s.acquisition_on.is_none());
    }
}
