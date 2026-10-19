//! AOI — área de interesse e os parâmetros de cálculo de área.
//!
//! A AOI **não** é uma geometria nova: é uma **camada vetorial existente**
//! (`layers`) que o projeto elege como área de interesse. [`AoiLayer`] registra
//! esse vínculo e qual coluna de atributo identifica cada feição da AOI.
//!
//! [`AreaSettings`] guarda como a **área** do projeto é calculada — SRID de
//! cálculo (Decisão 4: EPSG:5880, Policônica SIRGAS2000, ou `geography`) e a
//! unidade de saída. Geometria é **persistida** em 4674; a **área** usa o SRID
//! métrico de [`AreaSettings::srs_id`] e a **grade** tem SRS próprio
//! ([`crate::cell::GridSpec`]) — não há escolha excludente entre graus e
//! metros (Decisão 4 retificada em 2026-10-18).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::project::DEFAULT_SRID;

/// SRID de cálculo de área padrão: **EPSG:5880** (Policônica SIRGAS2000),
/// projeção métrica adequada ao território brasileiro (Decisão 4).
pub const DEFAULT_AREA_SRID: i32 = 5880;

/// Unidade de área padrão (hectare).
pub const DEFAULT_AREA_UNIT: &str = "ha";

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Vínculo entre um projeto e uma camada que representa a sua AOI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AoiLayer {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da AOI.
    pub project_id: String,
    /// Camada (`layers.id`) que contém a geometria da AOI.
    pub layer_id: String,
    /// Coluna de atributo que identifica cada feição da AOI (`None` = não usa).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aoi_id_column: Option<String>,
    /// Fase dona da AOI (`phases.id` textual); `None` = sem fase (`ON DELETE
    /// SET NULL` — apagar a fase desvincula, não apaga a AOI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default = "now", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "now", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl AoiLayer {
    /// Cria um vínculo ainda não persistido (sem fase).
    #[must_use]
    pub fn new(project_id: &str, layer_id: &str, aoi_id_column: Option<String>) -> Self {
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            layer_id: layer_id.to_owned(),
            aoi_id_column,
            phase_id: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    /// Vincula a AOI a uma fase (`phases.id`); o chamador valida o projeto.
    #[must_use]
    pub fn with_phase(mut self, phase_id: Option<&str>) -> Self {
        self.phase_id = phase_id.map(str::to_owned);
        self
    }
}

/// Parâmetros de cálculo de área de um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaSettings {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono das configurações (uma por projeto).
    pub project_id: String,
    /// SRID métrico usado no cálculo de área (padrão 5880).
    #[serde(default = "default_area_srid")]
    pub srs_id: i32,
    /// Unidade de saída da área (padrão `ha`).
    #[serde(default = "default_unit")]
    pub unit: String,
    #[serde(default = "now", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "now", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn default_area_srid() -> i32 {
    DEFAULT_AREA_SRID
}

fn default_unit() -> String {
    DEFAULT_AREA_UNIT.to_owned()
}

impl AreaSettings {
    /// Configurações padrão para um projeto (SIRGAS 2000 Policônica, hectare).
    #[must_use]
    pub fn new(project_id: &str) -> Self {
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            srs_id: DEFAULT_AREA_SRID,
            unit: DEFAULT_AREA_UNIT.to_owned(),
            created_at: now(),
            updated_at: now(),
        }
    }
}

/// SRID de persistência da geometria da AOI (4674, SIRGAS 2000).
#[must_use]
pub fn aoi_storage_srid() -> u32 {
    DEFAULT_SRID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_de_area_em_5880_e_hectare() {
        let s = AreaSettings::new("p1");
        assert_eq!(s.srs_id, 5880);
        assert_eq!(s.unit, "ha");
    }

    #[test]
    fn aoi_aceita_coluna_de_id_opcional() {
        let a = AoiLayer::new("p1", "l1", None);
        assert!(a.aoi_id_column.is_none());
        let b = AoiLayer::new("p1", "l1", Some("cod_aoi".into()));
        assert_eq!(b.aoi_id_column.as_deref(), Some("cod_aoi"));
    }

    #[test]
    fn serde_de_area_settings_preenche_defaults() {
        let json = r#"{"id":"x","project_id":"p1","created_at":"2026-10-18T12:00:00Z","updated_at":"2026-10-18T12:00:00Z"}"#;
        let s: AreaSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.srs_id, 5880);
        assert_eq!(s.unit, "ha");
    }
}
