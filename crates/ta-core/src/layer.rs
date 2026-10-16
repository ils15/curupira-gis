//! Camada — entrada vetorial ou raster vinculada a um projeto.

use serde::{Deserialize, Serialize};

use crate::project::DEFAULT_SRID;

fn default_srid() -> u32 {
    DEFAULT_SRID
}

/// Tipo de geometria dominante da camada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeomType {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
    /// Camada raster (ex.: GeoTIFF/COG) — sem geometria vetorial.
    Raster,
}

/// Uma camada de um projeto: origem, tipo de geometria e SRID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da camada.
    pub project_id: String,
    pub name: String,
    pub geom_type: GeomType,
    /// SRID da origem da camada; a normalização para 4674 acontece no IO
    /// (`ta-gis::normalizer`).
    #[serde(default = "default_srid")]
    pub srid: u32,
    /// URI de origem: caminho de arquivo, URL ou DSN de banco.
    pub source_uri: String,
}
