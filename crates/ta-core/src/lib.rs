//! `ta-core` — núcleo de domínio do terraamazon-cleanroom-rust.
//!
//! Tipos puros (projeto, camada, feature, estilo, fase, AOI, célula, classe,
//! regra), o módulo central de conversão WKB/EWKB e o erro compartilhado.
//! Nenhuma dependência de IO, banco ou GUI vive aqui.

pub mod aoi;
pub mod cell;
pub mod classification;
pub mod error;
pub mod feature;
pub mod geo;
pub mod layer;
pub mod phase;
pub mod project;
pub mod rule;
pub mod scene;
pub mod style;
pub mod task;

pub use aoi::{AoiLayer, AreaSettings};
pub use cell::{BlockedCell, Cell, CellLayer, GridSpec};
pub use classification::{CellMeasure, Class, ClassGroup, DraftCell, DraftLayer, DraftState};
pub use error::{Error, GridError, Result};
pub use phase::Phase;
pub use rule::{Rule, RuleError, RuleSet};
pub use scene::Scene;
pub use task::{CellLogEntry, LogLevel, Process, ProcessStatus, Task, TaskKind, TaskStatus};
