//! Projeto — raiz da sessão de trabalho do usuário.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// SRID padrão do projeto: **SIRGAS 2000 (EPSG:4674)**, datum geodésico
/// usado pela cartografia oficial brasileira.
pub const DEFAULT_SRID: u32 = 4674;

fn default_srid() -> u32 {
    DEFAULT_SRID
}

/// Um projeto de trabalho: agrupa camadas, estilos e a SRID-alvo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    pub name: String,
    /// SRID do projeto. Novos projetos nascem em 4674.
    #[serde(default = "default_srid")]
    pub srid: u32,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            srid: DEFAULT_SRID,
            created_at: OffsetDateTime::now_utc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn novo_projeto_nasce_em_srid_4674() {
        let p = Project::default();
        assert_eq!(p.srid, 4674);
        assert_eq!(p.srid, DEFAULT_SRID);
    }

    #[test]
    fn srid_tem_default_no_serde() {
        let json = r#"{"id": "p1", "name": "Teste", "created_at": "2026-10-16T12:00:00Z"}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.srid, 4674);
    }
}
