//! Leitura de fontes vetoriais/raster para o modelo de domínio.
//!
//! - Vetores **SHP/GeoJSON/GPKG/FGB** → OGR (feature `gdal`, link dinâmico).
//! - **CSV-XY** puro-Rust (feature `csv`) — ver [`csv_xy`].
//! - Raster (GeoTIFF/COG) → só metadados via OGR ([`read_raster_meta`]).
//!
//! Toda leitura devolve [`VectorData`] com geometrias `geo_types` e
//! atributos `serde_json::Value`. O CRS é extraído da fonte (EPSG/WKT);
//! fonte sem CRS falha com [`Error::MissingCrs`] — a política do clean-room
//! é não advinhar datum.

use std::path::Path;

use ta_core::feature::Feature;

use crate::detect::SourceFormat;
use crate::error::Result;

/// Resultado de uma leitura vetorial: features + CRS reportado.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorData {
    /// Feições na SRID de origem (normalização é etapa separada — ver
    /// [`crate::normalizer`]).
    pub features: Vec<Feature>,
    /// EPSG identificado na fonte (`None` = só WKT conhecido).
    pub srid: Option<u32>,
    /// WKT do CRS, para relatório/diagnóstico.
    pub crs_wkt: Option<String>,
    /// Nome da camada de origem (nome do layer OGR ou stem do arquivo).
    pub layer_name: String,
}

/// Opções de leitura vetorial.
#[derive(Debug, Clone, Default)]
pub struct ReadOptions {
    /// SRID declarado pelo usuário quando a fonte não carrega CRS próprio
    /// (CSV-XY e afins). Não sobrescreve CRS lido do arquivo.
    pub srid: Option<u32>,
    /// Opções finas de CSV-XY (ignoradas nos demais formatos).
    pub csv: CsvXyOptions,
}

/// Opções do leitor CSV-XY (tipo puro — a leitura fica na feature `csv`).
#[derive(Debug, Clone)]
pub struct CsvXyOptions {
    /// Nome da coluna de latitude/Y (padrão: `lat`).
    pub lat_col: Option<String>,
    /// Nome da coluna de longitude/X (padrão: `lon`).
    pub lon_col: Option<String>,
    /// Separador explícito; `None` → sniff entre `,` `;` `\t`.
    pub sep: Option<char>,
    /// SRID das coordenadas; `None` → **4326** (lon/lat WGS84).
    pub srid: Option<u32>,
    /// Encoding do arquivo.
    pub encoding: CsvEncoding,
}

impl Default for CsvXyOptions {
    fn default() -> Self {
        Self {
            lat_col: None,
            lon_col: None,
            sep: None,
            srid: None,
            encoding: CsvEncoding::Utf8,
        }
    }
}

/// Encoding suportado do CSV-XY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CsvEncoding {
    /// UTF-8 (padrão).
    #[default]
    Utf8,
    /// Latin-1 (ISO-8859-1) — comum em bases de legado; decodificado
    /// byte→char 1:1 para UTF-8 antes do parse.
    Latin1,
}

/// Lê qualquer formato vetorial suportado, detectando pelo arquivo.
///
/// Formatos OGR (SHP/GeoJSON/GPKG/FGB) exigem a feature `gdal`; CSV-XY
/// exige a feature `csv`. Sem a feature correspondente, falha com
/// [`Error::UnsupportedFormat`] apontando a feature — nunca entra em
/// panic nem lê parcialmente.
pub fn read_vector(path: &Path, opts: &ReadOptions) -> Result<VectorData> {
    let _ = &opts; // opções só são consumidas pelo leitor CSV-XY (feature `csv`)
    let fmt = crate::detect::detect_source_format(path)?;
    match fmt {
        SourceFormat::CsvXy => {
            #[cfg(feature = "csv")]
            return crate::csv_xy::read_csv_xy(path, &opts.csv);
            #[cfg(not(feature = "csv"))]
            Err(crate::error::Error::UnsupportedFormat(format!(
                "CSV-XY requer a feature `csv` deste crate ({}): recompile \
                 com `--features csv`",
                path.display()
            )))
        }
        SourceFormat::Shapefile
        | SourceFormat::GeoJson
        | SourceFormat::GeoPackage
        | SourceFormat::FlatGeobuf => {
            #[cfg(feature = "gdal")]
            return crate::vector_gdal::read_vector_gdal(path);
            #[cfg(not(feature = "gdal"))]
            Err(crate::error::Error::UnsupportedFormat(format!(
                "{} requer a feature `gdal` (libgdal dinâmica): recompile com \
                 `--features gdal` ({})",
                fmt.name(),
                path.display()
            )))
        }
    }
}

/// Lê metadados de um raster (GeoTIFF/COG) — **não carrega pixels**.
///
/// A importação de raster no clean-room registra dimensões, geotransform e
/// CRS; os pixels permanecem no arquivo (`source_uri` da camada).
#[cfg(feature = "gdal")]
pub fn read_raster_meta(path: &Path) -> Result<crate::raster::RasterMeta> {
    crate::raster::read_raster_meta(path)
}
