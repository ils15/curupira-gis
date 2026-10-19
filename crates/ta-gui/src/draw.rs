//! Rotinas egui de desenho do mapa — consumidas por `MapViewEgui::render`.
//!
//! Decisões documentadas (ver também `map_view`):
//! - **Rasterização direta no painter do egui** (sem tiny-skia): o painter já
//!   tessela em CPU com clipping; buffer intermediário seria uma cópia a mais
//!   por frame sem ganho no MVP.
//! - **Preenchimento apenas para anéis convexos**: limitação pública do
//!   epaint (`PathShape` — "fill is only supported for convex polygons").
//!   Anéis côncavos recebem contorno; a convexidade é testada por produto
//!   vetorial com sinal único (barato após a decimação do LOD).
//! - **LOD**: anéis com diagonal subpixel são pulados; anéis maiores são
//!   decimados por stride (ver `viewport::lod_stride`) antes da conversão
//!   tela↔mundo.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Shape, Stroke, epaint::PathShape};
use geo_types::{Coord, Geometry};

use crate::map_view::RasterLayerView;
use crate::viewport::{Viewport, decimate_indices, lod_stride};

/// Paleta default por camada (até 6 camadas distintas, depois cicla).
/// Cores de tons médios/saturados com contraste garantido sobre o fundo
/// escuro E o claro do egui (ambos os temas são primeiro-cidadão).
pub fn default_palette() -> [Color32; 6] {
    [
        Color32::from_rgb(0x33, 0x88, 0xff), // azul
        Color32::from_rgb(0xe6, 0x7e, 0x22), // laranja
        Color32::from_rgb(0x27, 0xae, 0x60), // verde
        Color32::from_rgb(0x9b, 0x59, 0xb6), // roxo
        Color32::from_rgb(0xd4, 0xac, 0x0d), // amarelo-ocre
        Color32::from_rgb(0xe7, 0x4c, 0x3c), // vermelho
    ]
}

/// Estilo efetivo de uma feição no frame.
#[derive(Debug, Clone, Copy)]
pub struct GeomStyle {
    pub color: Color32,
    pub stroke_width: f32,
    pub opacity: f32,
    pub selected: bool,
}

impl GeomStyle {
    fn stroke(&self) -> Stroke {
        let width = if self.selected {
            self.stroke_width.max(1.0) + 1.5
        } else {
            self.stroke_width.max(1.0)
        };
        Stroke::new(width, with_opacity(self.color, self.opacity))
    }

    fn fill(&self) -> Color32 {
        with_opacity(
            self.color,
            self.opacity * if self.selected { 0.55 } else { 0.25 },
        )
    }

    fn outline(&self) -> Stroke {
        if self.selected {
            Stroke::new(2.5, Color32::YELLOW)
        } else {
            self.stroke()
        }
    }
}

/// Aplica opacidade `0..=1` sobre a cor base.
fn with_opacity(c: Color32, opacity: f32) -> Color32 {
    let a = (opacity.clamp(0.0, 1.0) * f32::from(c.a())) as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Mundo → tela (pixels da JANELA: soma a origem do canvas — o painter pinta
/// em coordenadas de janela).
fn world_to_canvas(canvas: Rect, vp: &Viewport, x: f64, y: f64) -> Pos2 {
    let (sx, sy) = vp.world_to_screen(x, y);
    Pos2::new(canvas.left() + sx, canvas.top() + sy)
}

/// Desenha a camada raster: a textura da banda ativa escalada ao bbox de
/// mundo (o downsample por frame é o próprio draw da textura — O(viewport)).
pub fn draw_raster(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    raster: &RasterLayerView,
    texture: egui::TextureId,
) {
    let a = world_to_canvas(canvas, vp, raster.bbox.min_x, raster.bbox.max_y);
    let b = world_to_canvas(canvas, vp, raster.bbox.max_x, raster.bbox.min_y);
    let rect = Rect::from_two_pos(a, b);
    if !rect.intersects(canvas) {
        return;
    }
    painter.image(
        texture,
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Desenha uma célula da grade no canvas (Fase 5b): preenchimento pela classe
/// decidida (ou cinza de bloqueio), contorno fino e um "X" quando bloqueada.
///
/// Células são quadriláteros (convexos) — `convex_polygon` é o caminho barato
/// do epaint. O "X" usa os vértices 0–2 e 1–3 (dentro do quadrilátero), o que
/// dá a marca de hachura sem depender de clipping de padrão.
pub fn draw_cell(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    poly: &geo_types::Polygon<f64>,
    fill: Color32,
    blocked: bool,
    selected: bool,
) {
    let mut pts: Vec<Pos2> = poly
        .exterior()
        .0
        .iter()
        .map(|c| world_to_canvas(canvas, vp, c.x, c.y))
        .collect();
    // Anel do banco vem fechado (último == primeiro); o `convex_polygon`
    // fecha sozinho e um vértice duplicado degenera triângulos.
    if pts.len() > 3 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.len() < 3 {
        return;
    }
    let bounds = Rect::from_points(&pts);
    if !bounds.intersects(canvas) {
        return; // célula fora da viewport (pan antes da nova consulta)
    }
    let stroke = if selected {
        Stroke::new(2.5, Color32::YELLOW)
    } else {
        Stroke::new(0.5, Color32::from_rgba_unmultiplied(40, 40, 40, 120))
    };
    painter.add(Shape::convex_polygon(pts.clone(), fill, stroke));
    if blocked && pts.len() >= 4 {
        let hatch = Stroke::new(1.0, Color32::from_rgba_unmultiplied(30, 30, 30, 200));
        painter.line_segment([pts[0], pts[2]], hatch);
        painter.line_segment([pts[1], pts[3]], hatch);
    }
}

/// Contorno de um polígono (AOI selecionada) — sem preenchimento, para não
/// competir com as cores das células.
pub fn draw_outline(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    poly: &geo_types::Polygon<f64>,
    color: Color32,
    width: f32,
) {
    let ring: Vec<Pos2> = poly
        .exterior()
        .0
        .iter()
        .map(|c| world_to_canvas(canvas, vp, c.x, c.y))
        .collect();
    if ring.len() < 2 {
        return;
    }
    painter.add(Shape::closed_line(ring, Stroke::new(width, color)));
}

/// Desenha uma geometria qualquer (todas as variantes de `Geometry`).
pub fn draw_geometry(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    geom: &Geometry<f64>,
    style: GeomStyle,
) {
    match geom {
        Geometry::Point(p) => draw_point(painter, canvas, vp, p.x(), p.y(), &style),
        Geometry::MultiPoint(mp) => {
            for p in &mp.0 {
                draw_point(painter, canvas, vp, p.x(), p.y(), &style);
            }
        }
        Geometry::Line(l) => {
            let pts = [l.start, l.end];
            draw_polyline(painter, canvas, vp, &pts, false, &style);
        }
        Geometry::LineString(ls) => draw_polyline(painter, canvas, vp, &ls.0, false, &style),
        Geometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                draw_polyline(painter, canvas, vp, &ls.0, false, &style);
            }
        }
        Geometry::Polygon(poly) => {
            draw_ring_filled(painter, canvas, vp, &poly.exterior().0, &style);
            for ring in poly.interiors() {
                // Buracos: contorno apenas (o fill do exterior já contrasta).
                draw_polyline(painter, canvas, vp, &ring.0, true, &style);
            }
        }
        Geometry::MultiPolygon(mp) => {
            for poly in &mp.0 {
                draw_ring_filled(painter, canvas, vp, &poly.exterior().0, &style);
            }
        }
        Geometry::Rect(r) => {
            let p = r.to_polygon();
            draw_ring_filled(painter, canvas, vp, &p.exterior().0, &style);
        }
        Geometry::Triangle(t) => {
            let p = t.to_polygon();
            draw_ring_filled(painter, canvas, vp, &p.exterior().0, &style);
        }
        Geometry::GeometryCollection(gc) => {
            for g in &gc.0 {
                draw_geometry(painter, canvas, vp, g, style);
            }
        }
    }
}

fn draw_point(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    x: f64,
    y: f64,
    style: &GeomStyle,
) {
    let p = world_to_canvas(canvas, vp, x, y);
    if !canvas.expand(8.0).contains(p) {
        return; // culling de ponto fora do canvas
    }
    let r = if style.selected { 5.5 } else { 3.5 };
    if style.selected {
        painter.circle_filled(p, r, style.fill());
        painter.circle_stroke(p, r, style.outline());
    } else {
        // Ponto comum: só o disco (metade das shapes — decisão de perf da
        // Fase 3; contorno só na seleção).
        painter.circle_filled(p, r, style.stroke().color);
    }
}

/// Linha aberta ou fechada (anel), com LOD por stride.
fn draw_polyline(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    coords: &[Coord<f64>],
    closed: bool,
    style: &GeomStyle,
) {
    let Some(mut pts) = ring_to_screen(canvas, vp, coords) else {
        return;
    };
    if pts.len() < 2 {
        return;
    }
    if closed {
        // Garante o fechamento visual (a decimação preserva o último ponto;
        // se a fonte não fecha o anel, fechamos aqui).
        if pts.first() != pts.last() {
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
        }
    }
    painter.add(Shape::line(pts, style.stroke()));
}

/// Polígono: contorno sempre; fill apenas quando o anel é convexo
/// (limitação do epaint documentada no cabeçalho do módulo).
fn draw_ring_filled(
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    ring: &[Coord<f64>],
    style: &GeomStyle,
) {
    let Some(screen) = ring_to_screen(canvas, vp, ring) else {
        return;
    };
    if screen.len() < 3 {
        return;
    }
    let fill = style.fill();
    if fill.a() > 0 && is_convex(&screen) {
        painter.add(Shape::Path(PathShape {
            points: screen,
            closed: true,
            fill,
            stroke: style.outline().into(),
        }));
    } else {
        draw_polyline(painter, canvas, vp, ring, true, style);
    }
}

/// Converte anel de mundo → pontos de tela com LOD:
/// 1) diagonal do anel em pixels mede a complexidade visual;
/// 2) `lod_stride` decide pular (subpixel) ou decimar;
/// 3) decimação preserva o último vértice (anéis fecham).
///
/// Retorna `None` quando o anel é subpixel ou vazio.
fn ring_to_screen(canvas: Rect, vp: &Viewport, ring: &[Coord<f64>]) -> Option<Vec<Pos2>> {
    if ring.is_empty() {
        return None;
    }
    let first = world_to_canvas(canvas, vp, ring[0].x, ring[0].y);
    let mut min_x = first.x;
    let mut max_x = first.x;
    let mut min_y = first.y;
    let mut max_y = first.y;
    let mut pts = Vec::with_capacity(ring.len());
    for c in ring {
        let p = world_to_canvas(canvas, vp, c.x, c.y);
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
        pts.push(p);
    }
    let diag = ((max_x - min_x).mul_add(max_x - min_x, (max_y - min_y) * (max_y - min_y))).sqrt();
    let stride = lod_stride(diag)?;
    if stride <= 1 {
        return Some(pts);
    }
    let kept = decimate_indices(pts.len(), stride);
    Some(kept.into_iter().map(|i| pts[i]).collect())
}

/// Teste de convexidade por produto vetorial com sinal único (colineares
/// ignorados). Em coordenadas de tela (y invertido) só a consistência do
/// sinal importa, não o sentido.
fn is_convex(pts: &[Pos2]) -> bool {
    if pts.len() < 4 {
        return true;
    }
    let n = pts.len();
    let mut sign = 0i8;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let c = pts[(i + 2) % n];
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross.abs() < f32::EPSILON {
            continue;
        }
        let s = if cross > 0.0 { 1 } else { -1 };
        if sign == 0 {
            sign = s;
        } else if s != sign {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::LineString;

    fn vp() -> Viewport {
        let mut v = Viewport::new();
        v.set_size_px(400.0, 300.0);
        v.set_px_per_unit(10.0);
        v
    }

    fn canvas() -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(400.0, 300.0))
    }

    #[test]
    fn world_to_canvas_soma_a_origem_do_painel() {
        let c = canvas();
        let v = vp();
        let p = world_to_canvas(c, &v, 0.0, 0.0);
        // centro do mundo → centro do canvas (200,150 relativo) + origem.
        assert!((p.x - (10.0 + 200.0)).abs() < 1e-4);
        assert!((p.y - (20.0 + 150.0)).abs() < 1e-4);
    }

    #[test]
    fn anel_subpixel_retorna_none() {
        let v = vp();
        // Anel minúsculo: 0.05 unidades * 10 px/un = 0.5 px → pula.
        let ring = LineString::new(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 0.05, y: 0.0 },
            Coord { x: 0.0, y: 0.05 },
            Coord { x: 0.0, y: 0.0 },
        ])
        .0;
        assert!(ring_to_screen(canvas(), &v, &ring).is_none());
    }

    #[test]
    fn anel_grande_nao_decima_e_pequeno_decima() {
        let mut v = vp();
        v.set_px_per_unit(1.0);
        let big = LineString::new(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 50.0, y: 0.0 },
            Coord { x: 50.0, y: 50.0 },
            Coord { x: 0.0, y: 50.0 },
            Coord { x: 0.0, y: 0.0 },
        ])
        .0;
        let screen = ring_to_screen(canvas(), &v, &big).expect("anel grande visível");
        assert_eq!(screen.len(), big.len(), "zoom ok → stride 1");

        // Zoom-out extremo: mesmo anel com ~5 px de diagonal → decima.
        v.set_px_per_unit(0.1);
        let screen = ring_to_screen(canvas(), &v, &big).expect("ainda visível");
        assert!(
            screen.len() < big.len(),
            "LOD decimou ({} < {})",
            screen.len(),
            big.len()
        );
    }

    #[test]
    fn convexidade() {
        let square = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        assert!(is_convex(&square));
        let star = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        assert!(!is_convex(&star));
    }
}
