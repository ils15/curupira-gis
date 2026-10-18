//! `ta-core` — núcleo de domínio do terraamazon-cleanroom-rust.
//!
//! Tipos puros (projeto, camada, feature, estilo, fase, AOI, célula), o módulo
//! central de conversão WKB/EWKB e o erro compartilhado. Nenhuma dependência de
//! IO, banco ou GUI vive aqui.

pub mod aoi;
pub mod cell;
pub mod error;
pub mod feature;
pub mod geo;
pub mod layer;
pub mod phase;
pub mod project;
pub mod style;

pub use aoi::{AoiLayer, AreaSettings};
pub use cell::{BlockedCell, Cell, CellLayer, GridSpec};
pub use error::{Error, GridError, Result};
pub use phase::Phase;
