//! `ta-core` — núcleo de domínio do terraamazon-cleanroom-rust.
//!
//! Tipos puros (projeto, camada, feature, estilo), o módulo central de
//! conversão WKB/EWKB e o erro compartilhado. Nenhuma dependência de IO,
//! banco ou GUI vive aqui.

pub mod error;
pub mod feature;
pub mod geo;
pub mod layer;
pub mod project;
pub mod style;

pub use error::{Error, Result};
