//! `ta-db` — persistência do terraamazon-cleanroom-rust.
//!
//! - [`pool`]: pool de conexões PostgreSQL/PostGIS (sqlx, tokio).
//! - [`repo`]: traits de repositório async — um só contrato para PostGIS
//!   (padrão) e GPKG offline (feature `offline-gpkg`, opt-in).
//! - [`repo_phase`], [`repo_aoi`], [`repo_cell`]: repositórios do fluxo de
//!   monitoramento (fase, AOI/área, camadas de células/células/bloqueios).
//! - [`repo_class`], [`repo_ruleset`]: classes/grupos/rascunhos e
//!   rulesets versionados/regras (Fase 2).
//! - [`spatial`]: utilitários espaciais do banco (DDL, COPY BINARY, MVT).
//! - [`ops`]: processamento espacial server-side (buffer/interseção/união —
//!   camadas derivadas, v0.2 GUI).
//!
//! Banco primário: **PostgreSQL 16 + PostGIS 3.4**, SRID do projeto **4674**
//! (coluna `geometry(Geometry,4674)` + índice GIST + CHECK de SRID — Fase 1).

pub mod error;
pub mod ops;
pub mod pool;
pub mod repo;
pub mod repo_aoi;
pub mod repo_cell;
pub mod repo_class;
pub mod repo_phase;
pub mod repo_ruleset;
pub mod spatial;

pub use error::{Error, Result};
