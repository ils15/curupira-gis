//! Erro unificado do domínio (`ta-core`).

use crate::rule::RuleError;

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

    /// Falha de contrato no motor de grade (AOI/especificação inválidas).
    #[error(transparent)]
    Grid(#[from] GridError),

    /// Predicado de regra inválido (sintaxe; inclui a posição do erro).
    #[error(transparent)]
    Rule(#[from] RuleError),

    /// Estado de rascunho desconhecido lido da persistência.
    #[error("estado de rascunho inválido: {0:?} (esperado `draft` ou `consolidated`)")]
    InvalidDraftState(String),
}

/// Erros do motor de grade (`ta-engine`), tipados — nunca uma `String` solta.
///
/// O motor é puro: recebe geometria + especificação e devolve erro de
/// contrato quando a entrada não permite uma grade bem definida.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GridError {
    /// Resolução não finita (NaN/∞).
    #[error("resolução da grade não é finita: {0}")]
    NonFiniteResolution(f64),

    /// Resolução nula ou negativa.
    #[error("resolução da grade deve ser > 0 (recebido {0})")]
    NonPositiveResolution(f64),

    /// Origem não finita (NaN/∞) em x ou y.
    #[error("origem da grade não é finita (x={x}, y={y})")]
    NonFiniteOrigin { x: f64, y: f64 },

    /// SRID da grade não positivo (`srid <= 0`).
    ///
    /// O motor é **numericamente agnóstico** ao SRID (não reprojeta nada), mas
    /// exige um EPSG positivo: o valor define as unidades de `resolution` e da
    /// origem grau/metro. A existência do EPSG é validada na fronteira de IO
    /// (`ta-db`, contra `spatial_ref_sys`).
    #[error("SRID da grade deve ser > 0 (recebido {0})")]
    NonPositiveSrid(i32),

    /// AOI vazia ou com envelope degenerado (largura/altura nula).
    #[error("AOI degenerada ou vazia: envelope [{min_x}, {min_y}]–[{max_x}, {max_y}] sem área")]
    DegenerateAoi {
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    },

    /// AOI **válida** (envelope com área), mas o recorte pela regra do centro
    /// não manteve nenhuma célula — tipicamente porque a resolução da grade é
    /// maior que a própria AOI (ou a origem desalinha os centros). É distinto
    /// de [`GridError::DegenerateAoi`]: aqui a AOI é boa, a grade é que é
    /// grosseira demais para ela.
    #[error(
        "nenhuma célula gerada: a AOI é menor que a resolução da grade ({resolution} nas \
         unidades do SRS da grade); reduza --resolution para uma célula mais fina \
         (ou revise --origin-x/--origin-y)"
    )]
    EmptyGrid { resolution: f64 },

    /// A grade candidata excede o teto de células permitido.
    #[error("grade grande demais: {needed} células candidatas excedem o limite de {limit}")]
    TooManyCells { limit: u64, needed: u64 },

    /// Índice de linha/coluna fora do intervalo representável (`i32`).
    #[error("índice de célula fora do intervalo suportado: {0}")]
    IndexOutOfRange(i64),
}

/// Alias de resultado padrão do domínio.
pub type Result<T, E = Error> = std::result::Result<T, E>;
