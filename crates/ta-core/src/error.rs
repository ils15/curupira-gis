//! Erro unificado do domínio (`ta-core`).

/// Erro compartilhado por todo o domínio.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Funcionalidade planejada, mas ainda não implementada (stub de Fase 0).
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),

    /// Formato de dados sem suporte (ex.: KML, NetCDF, DWG — fora do MVP).
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// SRID inesperado — o clean-room normaliza tudo para 4674.
    #[error("invalid SRID: expected {expected}, got {got}")]
    SridMismatch { expected: u32, got: u32 },

    /// SRID fora do intervalo suportado (PostGIS usa i32).
    #[error("SRID inválido: {0}")]
    InvalidSrid(u32),

    /// Erro do motor de conversão geoespacial (geozero).
    #[error(transparent)]
    Geo(#[from] geozero::error::GeozeroError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Alias de resultado padrão do domínio.
pub type Result<T, E = Error> = std::result::Result<T, E>;
