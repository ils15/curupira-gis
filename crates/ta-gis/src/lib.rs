//! `ta-gis` — IO geoespacial do terraamazon-cleanroom-rust.
//!
//! - [`detect`]: detecção de formato por extensão/magic (KML, NetCDF e DWG
//!   falham com [`Error::UnsupportedFormat`] tipado).
//! - [`reader`] / [`writer`]: leitura/escrita vetorial e raster por formato
//!   (features opcionais: `gdal`, `csv`, `geojson`, `flatgeobuf`).
//! - [`normalizer`]: pipeline de import — reprojeção para 4674 (OGR/PROJ) e
//!   política de validade (inválida é **rejeitada**, nunca reparada em
//!   silêncio).
//! - [`derived`]: COG+overviews (`gdal_translate`/`gdaladdo` sem shell) e
//!   FlatGeobuf; MVT vive no banco (`ta_db::spatial::mvt_tile`).
//! - [`wms`]: cliente WMS 1.3.0 com cache em disco (feature `wms`).
//!
//! O CRS é sempre extraído da fonte; fonte sem CRS falha com
//! [`Error::MissingCrs`] — datum não se adivinha.

pub mod derived;
pub mod detect;
pub mod error;
pub mod exchange;
pub mod normalizer;
pub mod reader;
pub mod writer;

#[cfg(feature = "csv")]
pub mod csv_xy;
#[cfg(feature = "flatgeobuf")]
mod fgb_writer;
#[cfg(feature = "gdal")]
pub mod ogr;
#[cfg(feature = "gdal")]
mod ogr_writer;
#[cfg(feature = "gdal")]
pub mod raster;
#[cfg(feature = "gdal")]
mod vector_gdal;
#[cfg(feature = "wms")]
pub mod wms;

#[cfg(test)]
pub(crate) mod testutil;

pub use error::{Error, Result};

/// Versão do GDAL carregada em runtime (ex.: `GDAL 3.8.4, released …`).
///
/// Existe só com a feature `gdal`; sem ela o binário não linka libgdal e o
/// `ta-cli doctor` reporta a ausência da feature em vez de falhar. Como o
/// `gdal` é link **dinâmico**, a chamada é também o probe de que a
/// biblioteca está de fato carregável neste host.
#[cfg(feature = "gdal")]
#[must_use]
pub fn gdal_version_summary() -> String {
    gdal::version::VersionInfo::version_summary()
}
