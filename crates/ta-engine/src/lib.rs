//! `ta-engine` — motor de domínio **puro** do terraamazon-cleanroom-rust.
//!
//! É onde vive a paridade comportamental **testável**: entrada pura
//! (geometria + especificação), saída pura (dados), sem IO, banco ou GUI.
//! Depende apenas de [`ta_core`] e `geo-types` (Decisão 3 do plano aprovado).
//!
//! Contrato de pureza:
//! - **entrada:** geometria da AOI (`geo_types::MultiPolygon<f64>`) e uma
//!   [`ta_core::cell::GridSpec`] (resolução + origem **nas unidades do SRS
//!   `srid`**; a geometria deve estar no MESMO SRS);
//! - **saída:** `Vec<grid::GridCell>` determinístico — mesma entrada, mesma
//!   saída na mesma ordem — ou um [`ta_core::GridError`] tipado;
//! - **sem `unwrap`/`expect`/panic** fora de teste e **sem alocação
//!   descontrolada** (teto [`grid::MAX_CELLS`]).
//!
//! Módulos:
//! - [`grid`]: grade regular alinhada à origem, recortada pela AOI.

pub mod grid;

pub use grid::{GridCell, MAX_CELLS, aoi_bbox, candidate_count, generate_grid};
