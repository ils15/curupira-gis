//! Estilo mínimo de renderização por camada (serializável no projeto).

use serde::{Deserialize, Serialize};

fn default_color() -> String {
    "#3388ffcc".to_string()
}

fn default_stroke_width() -> f32 {
    1.0
}

fn default_opacity() -> f32 {
    1.0
}

/// Estilo visual mínimo de uma camada.
///
/// Cores em `#RRGGBB[AA]` (hex CSS). Mantido deliberadamente pequeno — o
/// editor de estilos completo não é escopo da Fase 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Cor de preenchimento/linha (`#RRGGBB[AA]`).
    #[serde(default = "default_color")]
    pub color: String,
    /// Espessura do traço, em pixels.
    #[serde(default = "default_stroke_width")]
    pub stroke_width: f32,
    /// Opacidade geral, `0.0..=1.0`.
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Camada visível no mapa.
    #[serde(default)]
    pub visible: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: default_color(),
            stroke_width: default_stroke_width(),
            opacity: default_opacity(),
            visible: true,
        }
    }
}
