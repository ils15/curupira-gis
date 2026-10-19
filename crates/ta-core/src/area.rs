//! Tipos do **cálculo de área consolidado** (Fase 5).
//!
//! **Decisão 4 (retificada em 2026-10-18):** a geometria é persistida em
//! **EPSG:4674** (graus), mas a área **NUNCA** é calculada em graus — sempre
//! `ST_Area(ST_Transform(geom, srs))` com o SRS **métrico** de
//! `area_settings.srs_id` (default **EPSG:5880**, Policônica SIRGAS 2000,
//! quando o projeto não tem linha em `area_settings`). Não há escolha
//! excludente entre graus e metros: graus é armazenamento; metro é cálculo.
//!
//! Aqui vivem apenas os tipos do resultado (sem lógica, sem IO); o SQL
//! agregado fica em `ta-db::repo_area` e a orquestração em `ta-flow::area`.

use serde::{Deserialize, Serialize};

/// Totais de área de UMA camada de células no SRS métrico do projeto.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AreaSummary {
    /// Área total da grade, em m² (SRS métrico).
    pub total_m2: f64,
    /// Área das células bloqueadas, em m² (SRS métrico).
    pub blocked_m2: f64,
    /// Área disponível (`total_m2 - blocked_m2`), em m² (SRS métrico).
    pub available_m2: f64,
    /// Total de células da grade.
    pub cells_total: i64,
    /// Células bloqueadas.
    pub cells_blocked: i64,
}

/// Área agregada por classe de um rascunho classificado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaByClass {
    /// Nome da classe (exibição; o `classes.id` não aparece no relatório).
    pub class_name: String,
    /// Células decididas para a classe.
    pub cells: i64,
    /// Área somada dessas células, em m² (SRS métrico).
    pub area_m2: f64,
}

/// Área de UMA célula — base do export CSV/JSON por célula.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaByCell {
    /// Identificador determinístico da célula (`"<col>_<row>"`).
    pub cell_id: String,
    /// A célula é bloqueada?
    pub blocked: bool,
    /// Área da célula, em m² (SRS métrico).
    pub area_m2: f64,
}

/// Relatório consolidado de área de uma grade (e, opcionalmente, das classes
/// de um rascunho) — o que `area report` imprime/serializa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaReport {
    /// Grade avaliada.
    pub cell_layer_id: String,
    /// Nome da grade avaliada (exibição).
    pub cell_layer_name: String,
    /// SRS métrico efetivamente usado (`--srs` ou `area_settings.srs_id`,
    /// com default 5880 — ver [`crate::aoi::DEFAULT_AREA_SRID`]).
    pub srs: i32,
    /// Rótulo de unidades do SRS (best-effort do PostGIS; ex.: "metros").
    pub srs_units: String,
    /// Totais da grade.
    pub summary: AreaSummary,
    /// Área por classe do rascunho (vazio sem `--draft` ou sem decisões).
    pub by_class: Vec<AreaByClass>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A coerência `available + blocked == total` é propriedade do produtor
    /// (repo/flow); o tipo só carrega os três campos independentes.
    #[test]
    fn summary_serializa_os_tres_totais_e_as_contagens() {
        let s = AreaSummary {
            total_m2: 44_500_321.5,
            blocked_m2: 385_110.25,
            available_m2: 44_115_211.25,
            cells_total: 118,
            cells_blocked: 12,
        };
        let json = serde_json::to_string(&s).expect("serializa");
        assert!(json.contains("\"total_m2\":44500321.5"));
        assert!(json.contains("\"cells_blocked\":12"));
        let back: AreaSummary = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back, s);
    }
}
