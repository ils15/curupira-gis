//! Feature — uma geometria + atributos livres (JSON).

use geo_types::Geometry;
use serde::{Deserialize, Serialize};

/// Identificador opaco de feature no contexto de render/pick (`ta-gui`).
///
/// É um uid do backend de mapa; o mapeamento para o id de banco é
/// responsabilidade da camada de acesso a dados.
pub type FeatureId = u64;

/// Uma feição geoespacial: geometria + atributos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    /// Geometria em coordenadas do SRID da camada dona.
    pub geom: Geometry<f64>,
    /// Atributos livres (objeto JSON; colunas do shapefile/DB).
    pub attrs: serde_json::Value,
}

impl Feature {
    pub fn new(geom: Geometry<f64>, attrs: serde_json::Value) -> Self {
        Self { geom, attrs }
    }
}
