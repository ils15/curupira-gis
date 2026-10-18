//! Célula, camada de células e a especificação da grade.
//!
//! Uma **camada de células** (`cell_layers`) é a grade regular derivada de uma
//! AOI: resolução, origem e o ponteiro para a AOI vínculo (`aoi_layers`). Cada
//! **célula** (`cells`) é uma [`geo_types::Polygon`] em EPSG:4674 com um
//! identificador determinístico (`"<col>_<row>"`, 0-based) e os índices de
//! linha/coluna que a geraram.
//!
//! [`GridSpec`] é a entrada pura do motor de grade (`ta-engine`): a resolução e
//! a origem do alinhamento são expressas **nas unidades do SRS escolhido**
//! (graus se geográfico, ex.: 4674; metros se projetado, ex.: 5880) — Decisão 4
//! retificada pelo usuário em 2026-10-18: a grade aceita **qualquer** EPSG.
//! O armazenamento das células continua canônico em EPSG:4674.

use geo_types::Polygon;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::project::DEFAULT_SRID;

/// SRID de persistência padrão das células (4674, SIRGAS 2000).
pub const DEFAULT_CELL_SRID: i32 = DEFAULT_SRID as i32;

/// Especificação de uma grade regular: SRS, passo e origem (alinhamento).
///
/// A grade é alinhada à **origem** e caminha em passos de `resolution`
/// **nas unidades do SRS `srid`** — graus se o EPSG for geográfico (ex.: 4674),
/// metros se for projetado (ex.: 5880), ou o que o EPSG definir. A célula
/// `(col, row)` cobre `[origin_x + col·r, origin_x + (col+1)·r] ×
/// [origin_y + row·r, origin_y + (row+1)·r]` **nessas unidades**.
///
/// O motor de grade (`ta-engine`) é **numericamente agnóstico ao SRID**: ele
/// não reprojeta nada e só exige `srid > 0`. A reprojeção para o armazenamento
/// canônico (EPSG:4674) acontece **na fronteira de IO**, com PostGIS
/// `ST_Transform` (`ta-db`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridSpec {
    /// Passo da grade **nas unidades de `srid`** (> 0, finito).
    pub resolution: f64,
    /// Coordenada X da origem do alinhamento (nas unidades de `srid`).
    pub origin_x: f64,
    /// Coordenada Y da origem do alinhamento (nas unidades de `srid`).
    pub origin_y: f64,
    /// EPSG do SRS em que `resolution`/`origin_x`/`origin_y` são expressos.
    #[serde(default = "default_srid")]
    pub srid: i32,
}

impl GridSpec {
    /// Nova especificação. `srid` define as unidades de `resolution`/origem; a
    /// validade (`resolution`/origem finitas, `srid > 0`) é checada pelo motor
    /// (`ta-engine`).
    #[must_use]
    pub fn new(resolution: f64, origin_x: f64, origin_y: f64, srid: i32) -> Self {
        Self {
            resolution,
            origin_x,
            origin_y,
            srid,
        }
    }
}

/// Camada de células (grade regular sobre uma AOI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellLayer {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da grade.
    pub project_id: String,
    /// AOI (`aoi_layers.id`) que originou a grade.
    pub aoi_layer_id: String,
    /// Nome da camada, único dentro do projeto (`UNIQUE(project_id, name)`).
    pub name: String,
    /// Passo da grade **nas unidades de [`Self::srid`]**.
    pub resolution: f64,
    /// EPSG do SRS em que `resolution`/`origin_x`/`origin_y` são expressos:
    /// graus se geográfico (ex.: 4674), metros se projetado (ex.: 5880), ou o
    /// que o EPSG definir. A **resolução** é expressa nestas unidades.
    ///
    /// Não é o SRID de persistência das células: essas são **sempre** gravadas
    /// em EPSG:4674 (`spatial::TARGET_SRID`) via `ST_Transform`. Este campo diz
    /// como a grade foi **definida** (Decisão 4, retificada em 2026-10-18).
    #[serde(default = "default_srid")]
    pub srid: i32,
    pub origin_x: f64,
    pub origin_y: f64,
    /// Fase dona da grade (`phases.id` textual); `None` = sem fase (`ON DELETE
    /// SET NULL` — apagar a fase desvincula, não apaga a grade).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn default_srid() -> i32 {
    DEFAULT_CELL_SRID
}

impl CellLayer {
    /// Cria uma camada de células ainda não persistida.
    ///
    /// O `srid` da camada vem de `spec.srid` (as unidades de `resolution` e da
    /// origem) — não há parâmetro separado para evitar divergência.
    #[must_use]
    pub fn new(project_id: &str, aoi_layer_id: &str, name: &str, spec: &GridSpec) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            aoi_layer_id: aoi_layer_id.to_owned(),
            name: name.to_owned(),
            resolution: spec.resolution,
            srid: spec.srid,
            origin_x: spec.origin_x,
            origin_y: spec.origin_y,
            phase_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Vincula a grade a uma fase (`phases.id`); o chamador valida o projeto.
    #[must_use]
    pub fn with_phase(mut self, phase_id: Option<&str>) -> Self {
        self.phase_id = phase_id.map(str::to_owned);
        self
    }

    /// A especificação de grade correspondente a esta camada.
    #[must_use]
    pub fn spec(&self) -> GridSpec {
        GridSpec::new(self.resolution, self.origin_x, self.origin_y, self.srid)
    }
}

/// Uma célula persistida de uma camada de células.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Id de linha do banco (`BIGSERIAL`; 0 = não persistido).
    pub id: i64,
    /// Camada de células dona.
    pub cell_layer_id: String,
    /// Identificador determinístico da célula (`"<col>_<row>"`, 0-based).
    pub cell_id: String,
    /// Polígono da célula. Em memória (recém-gerada) está no SRS da grade;
    /// lido do banco está **sempre em EPSG:4674** (armazenamento canônico).
    pub geom: Polygon<f64>,
    /// Índice de linha (0-based, alinhado à origem).
    pub row_index: i32,
    /// Índice de coluna (0-based, alinhado à origem).
    pub col_index: i32,
}

/// Uma célula marcada como bloqueada em uma camada de células.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedCell {
    /// Id de linha do banco (`BIGSERIAL`; 0 = não persistido).
    pub id: i64,
    /// Camada de células dona.
    pub cell_layer_id: String,
    /// Identificador da célula bloqueada — **precisa existir** em `cells` da
    /// mesma camada (FK composta `blocked_cells_cell_fk`); não aceita id
    /// externo à grade.
    pub cell_id: String,
    /// Motivo do bloqueio (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl BlockedCell {
    /// Marca um bloqueio ainda não persistido.
    #[must_use]
    pub fn new(cell_layer_id: &str, cell_id: &str, reason: Option<String>) -> Self {
        Self {
            id: 0,
            cell_layer_id: cell_layer_id.to_owned(),
            cell_id: cell_id.to_owned(),
            reason,
            created_at: OffsetDateTime::now_utc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_spec_arredonda_ida_e_volta_pela_camada() {
        // Spec métrica (metros, EPSG:5880): o srid sobrevive ao round-trip.
        let spec = GridSpec::new(500.0, 100.0, 200.0, 5880);
        let layer = CellLayer::new("p1", "a1", "grade", &spec);
        assert_eq!(layer.srid, 5880, "o srid da camada vem do spec");
        assert_eq!(layer.spec(), spec);
    }

    #[test]
    fn cell_id_e_apenas_indices_quando_nao_persistida() {
        let b = BlockedCell::new("cl1", "3_4", Some("nuvem".into()));
        assert_eq!(b.id, 0);
        assert_eq!(b.reason.as_deref(), Some("nuvem"));
    }

    #[test]
    fn serde_de_cell_layer_preenche_srid_default() {
        let json = r#"{"id":"cl1","project_id":"p1","aoi_layer_id":"a1","name":"g",
            "resolution":0.01,"origin_x":0.0,"origin_y":0.0,
            "created_at":"2026-10-18T12:00:00Z","updated_at":"2026-10-18T12:00:00Z"}"#;
        let cl: CellLayer = serde_json::from_str(json).unwrap();
        assert_eq!(cl.srid, 4674);
    }
}
