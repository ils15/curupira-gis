//! Erros do módulo de IO geoespacial.

/// Erro de IO geoespacial.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Formato sem suporte no MVP (KML, NetCDF, DWG, …). Variante tipada de
    /// propósito: a UI precisa distinguir "formato inválido" de "falha de
    /// leitura".
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// Fonte sem CRS identificável (nem EPSG, nem WKT). A política do
    /// clean-room é recusar: dado sem datum entra como coordenada sem
    /// significado — corrija a origem (ex.: gere o .prj) e reimporte.
    #[error("fonte sem CRS: {path} (gerar .prj/sidecar com o SRS e reimportar)")]
    MissingCrs {
        /// Caminho da fonte que não declarou CRS.
        path: String,
    },

    /// Geometria inválida detectada na normalização — rejeitada com o motivo
    /// e o índice da feature (política: nunca passar inválida adiante).
    #[error("geometria inválida na feature {index}: {reason}")]
    InvalidGeometry {
        /// Índice (0-based) da feature rejeitada no lote.
        index: usize,
        /// Motivo legível (coordenada não finita, anel aberto, …).
        reason: String,
    },

    /// Reprojeção pedida sem a feature `gdal` habilitada (motor OGR/PROJ).
    #[error(
        "reprojeção requer a feature `gdal` (link dinâmico libgdal ≥ 3.8); \
         habilite `--features gdal` ou importe dados já em EPSG:4674"
    )]
    ReprojectionUnavailable,

    /// Conjunto de features vazio — nada a escrever/exportar.
    #[error("conjunto de features vazio: nada a escrever")]
    EmptyFeatureSet,

    /// Ferramenta externa (gdal_translate/gdaladdo) ausente ou falhou.
    #[error("ferramenta externa `{program}` falhou: {message}")]
    ExternalTool {
        /// Executável invocado (ex.: `gdal_translate`).
        program: String,
        /// Diagnóstico (stderr ou erro de spawn).
        message: String,
    },

    /// Erro de parser CSV-XY com posição (linha/coluna) para o usuário.
    #[error("CSV-XY: linha {line}, coluna {column}: {message}")]
    CsvCell {
        /// Linha do arquivo (1-based, contando o cabeçalho).
        line: u64,
        /// Nome da coluna problemática.
        column: String,
        /// Descrição do problema.
        message: String,
    },

    /// Falha na escrita de arquivo (formato/driver).
    #[error("falha de escrita: {0}")]
    Write(String),

    /// Erro de cliente/cache WMS.
    #[error("WMS: {0}")]
    Wms(String),

    #[error(transparent)]
    Core(#[from] ta_core::Error),

    /// Erro da biblioteca GDAL (feature `gdal`).
    #[error(transparent)]
    #[cfg(feature = "gdal")]
    Gdal(#[from] gdal::errors::GdalError),

    /// Erro do crate CSV (feature `csv`).
    #[error(transparent)]
    #[cfg(feature = "csv")]
    Csv(#[from] csv::Error),

    /// Erro do crate geojson (feature `geojson`).
    #[error(transparent)]
    #[cfg(feature = "geojson")]
    GeoJson(#[from] geojson::Error),

    /// Erro do flatgeobuf (feature `flatgeobuf`).
    #[error(transparent)]
    #[cfg(feature = "flatgeobuf")]
    FlatGeobuf(#[from] flatgeobuf::Error),

    /// Erro geozero (feature `flatgeobuf`/`geozero`).
    #[error(transparent)]
    #[cfg(any(feature = "geozero", feature = "flatgeobuf"))]
    Geozero(#[from] geozero::error::GeozeroError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// Erro da persistência (feature `db` — export consome FeatureRepo).
    #[error(transparent)]
    #[cfg(feature = "db")]
    Db(#[from] ta_db::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Alias de resultado padrão do módulo.
pub type Result<T, E = Error> = std::result::Result<T, E>;
