//! Matemática dos overlays do canvas (v0.2 — paridade com a barra de mapa
//! da referência): grade de coordenadas, barra de escala e presets de zoom.
//!
//! Tudo **puro e headless** (mesmo padrão de `viewport.rs`): o desenho em si
//! mora em `panels::central_canvas` via painter do egui; aqui só existe a
//! conversão escala ↔ pixels/labels, coberta por testes unitários.
//!
//! Convenções herdadas do viewport (SRID geográfico EPSG:4674):
//! 1° ≈ 111 320 m no equador; pixel de referência 0,28 mm (OGC 02-058).

/// Denominadores dos presets do dropdown "Scale" da status bar
/// (série cartográfica comum: 1:1k … 1:5M).
pub const SCALE_PRESETS: &[f64] = &[
    1_000.0,
    2_500.0,
    5_000.0,
    10_000.0,
    25_000.0,
    50_000.0,
    100_000.0,
    250_000.0,
    500_000.0,
    1_000_000.0,
    5_000_000.0,
];

const METERS_PER_DEGREE: f64 = 111_320.0;
const METERS_PER_PIXEL: f64 = 0.000_28;

/// `px_per_unit` que produz o denominador de escala pedido.
///
/// Inverso exato de `Viewport::scale_denominator` — o roundtrip é coberto
/// por teste. O clamp de limites fica por conta de `set_px_per_unit`.
pub fn px_per_unit_for_denominator(denominator: f64) -> f64 {
    if !denominator.is_finite() || denominator <= 0.0 {
        return 1.0;
    }
    denominator / (METERS_PER_DEGREE * METERS_PER_PIXEL)
}

/// Passo "bonito" (série 1-2-5) da grade de coordenadas para o zoom atual.
///
/// `min_px` é o espaçamento mínimo desejado em pixels entre linhas — o passo
/// em graus é o menor 1-2-5 que ocupa ao menos esse espaço.
pub fn grid_step(px_per_unit: f64, min_px: f64) -> f64 {
    if !px_per_unit.is_finite() || px_per_unit <= 0.0 || !min_px.is_finite() || min_px <= 0.0 {
        return 1.0;
    }
    let raw = min_px / px_per_unit;
    let mag = 10f64.powf(raw.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        let step = m * mag;
        if step >= raw {
            return step;
        }
    }
    10.0 * mag
}

/// Linhas da grade dentro do recorte visível (meridianos, paralelos).
///
/// Devolve `(meridianos_x, paralelos_y)` alinhados a múltiplos do passo —
/// começa no primeiro múltiplo ≥ mín e para no último ≤ máx (nunca iterando
/// fora do recorte, mesmo com janelas enormes).
pub fn grid_lines(
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    step: f64,
) -> (Vec<f64>, Vec<f64>) {
    let axis = |min: f64, max: f64| -> Vec<f64> {
        let mut out = Vec::new();
        if !step.is_finite() || step <= 0.0 || !min.is_finite() || !max.is_finite() || max <= min {
            return out;
        }
        let first = (min / step).ceil() * step;
        let mut v = first;
        // Guarda de iteração: no máx. (span/step + 2) elementos.
        let cap = ((max - min) / step).floor() as usize + 2;
        while v <= max && out.len() < cap {
            out.push(v);
            v += step;
        }
        out
    };
    (axis(min_x, max_x), axis(min_y, max_y))
}

/// Comprimento "bonito" da barra de escala para o denominador atual.
///
/// Devolve `(metros, pixels, rótulo)` — o metro mais próximo da série
/// 1-2-5 cuja barra fique perto de `target_px` (o desenho ancora essa barra
/// no canto do canvas). Rótulo em m (< 1000) ou km.
pub fn scale_bar(denominator: f64, target_px: f64) -> (f64, f64, String) {
    if !denominator.is_finite() || denominator <= 0.0 || !target_px.is_finite() || target_px <= 0.0
    {
        return (0.0, 0.0, "—".into());
    }
    let meters_per_px = denominator * METERS_PER_PIXEL;
    let raw = target_px * meters_per_px;
    let mag = 10f64.powf(raw.log10().floor());
    // Escolhe o 1-2-5 mais próximo do alvo (não o menor que cobre).
    let mut best = (mag, f64::MAX);
    for m in [1.0, 2.0, 5.0, 10.0] {
        let cand = m * mag;
        let err = (cand - raw).abs();
        if err < best.1 {
            best = (cand, err);
        }
    }
    let meters = best.0;
    let px = meters / meters_per_px;
    let label = if meters >= 1000.0 {
        format!("{} km", trim(meters / 1000.0))
    } else {
        format!("{} m", trim(meters))
    };
    (meters, px, label)
}

/// Sem casas decimais sobrando (10.0 → "10", 0.5 → "0,5" não — ponto fixo).
fn trim(v: f64) -> String {
    if (v - v.round()).abs() < f64::EPSILON * v.abs().max(1.0) {
        format!("{}", v.round() as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEN_TOLERANCE: f64 = 1e-6;

    #[test]
    fn presets_tem_roundtrip_com_denominador() {
        for &den in SCALE_PRESETS {
            let ppu = px_per_unit_for_denominator(den);
            // Inverso da fórmula do viewport (mesmas constantes).
            let back = ppu * METERS_PER_DEGREE * METERS_PER_PIXEL;
            assert!(
                (back - den).abs() / den < DEN_TOLERANCE,
                "preset {den} não roundtripa (voltou {back})"
            );
        }
    }

    #[test]
    fn denominador_invalido_nao_panica() {
        assert_eq!(px_per_unit_for_denominator(0.0), 1.0);
        assert_eq!(px_per_unit_for_denominator(f64::NAN), 1.0);
        assert_eq!(px_per_unit_for_denominator(-5.0), 1.0);
        assert_eq!(grid_step(0.0, 80.0), 1.0);
        assert_eq!(scale_bar(0.0, 80.0).2, "—");
    }

    #[test]
    fn grid_step_eh_serie_1_2_5_e_cresce_com_zoom_out() {
        let mantissa = |s: f64| s / 10f64.powf(s.log10().floor());
        let s = grid_step(1e-2, 80.0);
        assert!(
            [1.0, 2.0, 5.0].contains(&mantissa(s)),
            "passo {s} fora da série 1-2-5"
        );
        // Zoom out (ppu menor) → passo maior ou igual.
        assert!(grid_step(1e-4, 80.0) >= grid_step(1e-2, 80.0));
        // Zoom in → passo menor.
        assert!(grid_step(1e5, 80.0) < grid_step(1e2, 80.0));
    }

    #[test]
    fn grid_lines_alinhadas_ao_passo_sem_exceder_recorte() {
        let (xs, ys) = grid_lines(-0.3, 0.4, 5.2, 9.9, 1.0);
        assert_eq!(xs, vec![0.0]); // único múltiplo de 1 no intervalo
        assert_eq!(ys, vec![6.0, 7.0, 8.0, 9.0]);
        assert!(xs.iter().all(|&x| (-0.3..=0.4).contains(&x)));
        assert!(ys.iter().all(|&y| (5.2..=9.9).contains(&y)));
        // Recorte degenerado/lixo → vazio, sem loop infinito.
        assert_eq!(grid_lines(1.0, 1.0, 0.0, 1.0, 1.0).0, Vec::<f64>::new());
        assert_eq!(grid_lines(0.0, 1.0, 0.0, 1.0, 0.0).1, Vec::<f64>::new());
    }

    #[test]
    fn scale_bar_escolhe_1_2_5_proximo_e_rotula() {
        // Denominador 100k → 28 m/px; alvo 80 px → ~2240 m → "2 km".
        let (meters, px, label) = scale_bar(100_000.0, 80.0);
        assert_eq!(label, "2 km");
        assert!((meters - 2000.0).abs() < 1e-9);
        assert!((px - meters / (100_000.0 * METERS_PER_PIXEL)).abs() < 1e-9);
        // Escala grande (zoom in): denominador 1k → 0.28 m/px → alvo ~22.4 m → "20 m".
        let (meters, _, label) = scale_bar(1_000.0, 80.0);
        assert_eq!(label, "20 m");
        assert!((meters - 20.0).abs() < 1e-9);
        // Zoom muito out → km.
        let (_, _, label) = scale_bar(10_000_000.0, 80.0);
        assert!(label.ends_with("km"), "esperava km, veio {label}");
        // A barra resultante fica numa faixa útil em px (não some, não explode).
        let (_, px, _) = scale_bar(100_000.0, 80.0);
        assert!(px > 20.0 && px < 200.0, "barra com {px} px fora da faixa");
    }

    #[test]
    fn trim_nao_emite_zero_flutuante() {
        assert_eq!(trim(10.0), "10");
        assert_eq!(trim(0.5), "0.5");
    }
}
