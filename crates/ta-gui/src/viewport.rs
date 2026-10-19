//! Viewport 2D do mapa — matemática **pura**, sem egui: transformações
//! tela↔mundo, LOD (decimação de vértices) e hit-test de pick.
//!
//! Isolada num módulo próprio para ser testada headless (`cargo test` no CI
//! não tem GPU/display) e reutilizável por qualquer implementação de
//! [`crate::map_view::MapView`].
//!
//! Convenções:
//! - **mundo**: coordenadas do SRID corrente do projeto (4674 — geográfico,
//!   graus; x = lon cresce p/ direita, y = lat cresce p/ cima);
//! - **tela**: pixels egui (origem no canto superior esquerdo; y cresce
//!   p/ baixo);
//! - escala = pixels por unidade de mundo (`px_per_unit`).

use geo_types::{Coord, Geometry, LineString, Polygon};

use crate::map_view::Bbox;

/// Escala mínima (zoom máximo p/ fora) — evita degenerar a 0.
pub const MIN_PX_PER_UNIT: f64 = 1e-9;
/// Escala máxima (zoom máximo p/ dentro).
pub const MAX_PX_PER_UNIT: f64 = 1e9;

/// Viewport do mapa: centro no mundo, escala e tamanho em pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    center: (f64, f64),
    px_per_unit: f64,
    size_px: (f32, f32),
}

impl Viewport {
    /// Viewport vazia (tamanho 0) centrada na origem.
    pub fn new() -> Self {
        Self {
            center: (0.0, 0.0),
            px_per_unit: 1.0,
            size_px: (0.0, 0.0),
        }
    }

    pub fn size_px(&self) -> (f32, f32) {
        self.size_px
    }

    /// Atualiza o tamanho em pixels (resize do painel central).
    pub fn set_size_px(&mut self, width: f32, height: f32) {
        self.size_px = (width.max(1.0), height.max(1.0));
    }

    pub fn center(&self) -> (f64, f64) {
        self.center
    }

    pub fn set_center(&mut self, x: f64, y: f64) {
        self.center = (x, y);
    }

    /// Escala corrente: pixels por unidade de mundo.
    pub fn px_per_unit(&self) -> f64 {
        self.px_per_unit
    }

    /// Define a escala diretamente (testes e presets de zoom).
    pub fn set_px_per_unit(&mut self, px_per_unit: f64) {
        self.px_per_unit = px_per_unit.clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT);
    }

    /// Mundo → tela.
    pub fn world_to_screen(&self, x: f64, y: f64) -> (f32, f32) {
        let (w, h) = self.size_px;
        let sx = (x - self.center.0).mul_add(self.px_per_unit, f64::from(w) / 2.0);
        // y do mundo cresce p/ cima; y de tela cresce p/ baixo.
        let sy = f64::from(h) / 2.0 - (y - self.center.1) * self.px_per_unit;
        (sx as f32, sy as f32)
    }

    /// Tela → mundo.
    pub fn screen_to_world(&self, sx: f32, sy: f32) -> (f64, f64) {
        let (w, h) = self.size_px;
        let x = (f64::from(sx) - f64::from(w) / 2.0) / self.px_per_unit + self.center.0;
        let y = (f64::from(h) / 2.0 - f64::from(sy)) / self.px_per_unit + self.center.1;
        (x, y)
    }

    /// Pan pela tela: desloca o centro na direção oposta ao arraste.
    pub fn pan_pixels(&mut self, dx_px: f32, dy_px: f32) {
        self.center.0 -= f64::from(dx_px) / self.px_per_unit;
        // Arraste p/ baixo (dy>0) move o conteúdo p/ baixo → centro sobe.
        self.center.1 += f64::from(dy_px) / self.px_per_unit;
    }

    /// Zoom com âncora: o ponto de mundo sob `(sx, sy)` permanece fixo.
    /// `factor` > 1 aproxima; clampado em [`MIN_PX_PER_UNIT`],
    /// [`MAX_PX_PER_UNIT`].
    pub fn zoom_at(&mut self, sx: f32, sy: f32, factor: f64) {
        let factor = factor.clamp(1e-6, 1e6);
        let (wx, wy) = self.screen_to_world(sx, sy);
        self.px_per_unit = (self.px_per_unit * factor).clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT);
        // Reposiciona o centro para manter (wx, wy) sob o mesmo pixel.
        let (w, h) = self.size_px;
        self.center.0 = wx - (f64::from(sx) - f64::from(w) / 2.0) / self.px_per_unit;
        self.center.1 = wy + (f64::from(sy) - f64::from(h) / 2.0) / self.px_per_unit;
    }

    /// Zoom no centro da viewport (botões +/−).
    pub fn zoom_center(&mut self, factor: f64) {
        let (w, h) = self.size_px;
        self.zoom_at(w / 2.0, h / 2.0, factor);
    }

    /// Ajusta a viewport ao bbox (zoom-to-extent) com margem fracionária.
    /// Bbox degenerado (linha/ponto) é expandido antes do fit.
    pub fn zoom_to_bbox(&mut self, bbox: &Bbox, margin: f64) {
        const MIN_SPAN: f64 = 1e-9;
        let mut min_x = bbox.min_x;
        let mut min_y = bbox.min_y;
        let mut max_x = bbox.max_x;
        let mut max_y = bbox.max_y;
        if (max_x - min_x) < MIN_SPAN {
            let pad = MIN_SPAN.max((max_x - min_x).abs()) * 0.5 + MIN_SPAN;
            min_x -= pad;
            max_x += pad;
        }
        if (max_y - min_y) < MIN_SPAN {
            let pad = MIN_SPAN.max((max_y - min_y).abs()) * 0.5 + MIN_SPAN;
            min_y -= pad;
            max_y += pad;
        }
        let (w, h) = self.size_px;
        if w <= 1.0 || h <= 1.0 {
            // Sem viewport definida ainda: só centraliza.
            self.center = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
            return;
        }
        let span_x = (max_x - min_x).max(MIN_SPAN);
        let span_y = (max_y - min_y).max(MIN_SPAN);
        let scale = (f64::from(w) / span_x).min(f64::from(h) / span_y) / (1.0 + margin);
        self.px_per_unit = scale.clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT);
        self.center = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
    }

    /// Denominador da escala `1:N` — **aproximação** para SRID geográfico
    /// (1° ≈ 111 320 m no equador) e pixel de referência de 0,28 mm
    /// (padrão OGC/OGC-02-058). Para SRIDs projetados em metros a conversão
    /// seria direta; hoje o clean-room é 4674 (geográfico).
    pub fn scale_denominator(&self) -> f64 {
        const METERS_PER_DEGREE: f64 = 111_320.0;
        const METERS_PER_PIXEL: f64 = 0.000_28;
        self.px_per_unit * METERS_PER_DEGREE * METERS_PER_PIXEL
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

/// LOD — stride de decimação a partir do tamanho do anel/linha em pixels.
///
/// Política documentada (Fase 3): geometrias cuja diagonal em tela é
/// subpixel (< 1.5 px) são **puladas** (`None`); acima disso o anel é
/// decimado mantendo stride vértices tal que o segmento médio fique
/// ≥ ~1 px — decimação proporcional ao zoom out. Zoom in (muito px)
/// volta a stride 1 (todos os vértices).
///
/// Retorna `None` quando a geometria é pequena demais para valer o desenho.
pub fn lod_stride(diagonal_px: f32) -> Option<usize> {
    if !(diagonal_px.is_finite()) || diagonal_px < 1.5 {
        return None;
    }
    // Alvo: ~1 px por vértice mantido. diag/stride ≈ n_vertices_visíveis…
    // como não temos n aqui, usamos: stride cresce quando a forma fica
    // pequena; a partir de 64 px de diagonal, stride 1.
    if diagonal_px >= 64.0 {
        return Some(1);
    }
    let stride = (64.0 / diagonal_px).ceil() as usize;
    Some(stride.clamp(2, 64))
}

/// Mantém 1 a cada `stride` índices, **sempre preservando o último**
/// (fecha anéis/linhas corretamente). `stride` 0/1 → todos.
pub fn decimate_indices(n: usize, stride: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    if stride <= 1 {
        return (0..n).collect();
    }
    let mut out = Vec::with_capacity(n / stride + 2);
    let mut i = 0;
    while i < n {
        out.push(i);
        i += stride;
    }
    let last = n - 1;
    if *out.last().expect("n > 0 garante out não vazio") != last {
        out.push(last);
    }
    out
}

/// Visita coordenadas por recursão sobre as variantes (sem Box<dyn>: as
/// variantes Rect/Triangle convertem para o polígono equivalente; o custo é
/// local a esta função, usada só na carga de dados).
fn for_each_coord<F: FnMut(&Coord<f64>)>(geom: &Geometry<f64>, f: &mut F) {
    match geom {
        Geometry::Point(p) => f(&p.0),
        Geometry::Line(l) => {
            f(&l.start);
            f(&l.end);
        }
        Geometry::LineString(ls) => {
            for c in &ls.0 {
                f(c);
            }
        }
        Geometry::Polygon(poly) => {
            for c in poly.exterior().0.iter() {
                f(c);
            }
        }
        Geometry::Rect(r) => {
            for c in r.to_polygon().exterior().0.iter() {
                f(c);
            }
        }
        Geometry::Triangle(t) => {
            for c in t.to_polygon().exterior().0.iter() {
                f(c);
            }
        }
        Geometry::MultiPoint(mp) => {
            for p in &mp.0 {
                f(&p.0);
            }
        }
        Geometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                for c in &ls.0 {
                    f(c);
                }
            }
        }
        Geometry::MultiPolygon(mp) => {
            for poly in &mp.0 {
                for c in poly.exterior().0.iter() {
                    f(c);
                }
            }
        }
        Geometry::GeometryCollection(gc) => {
            for g in &gc.0 {
                for_each_coord(g, f);
            }
        }
    }
}

/// BBox de uma geometria (para pick rápido e zoom-to-extent por feature).
pub fn geometry_bbox(geom: &Geometry<f64>) -> Option<Bbox> {
    let mut bbox: Option<Bbox> = None;
    for_each_coord(geom, &mut |c| match &mut bbox {
        None => bbox = Some(Bbox::new(c.x, c.y, c.x, c.y)),
        Some(b) => {
            b.min_x = b.min_x.min(c.x);
            b.min_y = b.min_y.min(c.y);
            b.max_x = b.max_x.max(c.x);
            b.max_y = b.max_y.max(c.y);
        }
    });
    bbox
}

/// BBox de um lote de geometrias (extent de camada — zoom-to-extent).
pub fn bbox_of_geoms<'a>(geoms: impl Iterator<Item = &'a Geometry<f64>>) -> Option<[f64; 4]> {
    let mut acc: Option<[f64; 4]> = None;
    for g in geoms {
        let Some(b) = geometry_bbox(g) else { continue };
        match &mut acc {
            None => acc = Some([b.min_x, b.min_y, b.max_x, b.max_y]),
            Some(a) => {
                a[0] = a[0].min(b.min_x);
                a[1] = a[1].min(b.min_y);
                a[2] = a[2].max(b.max_x);
                a[3] = a[3].max(b.max_y);
            }
        }
    }
    acc
}

/// O ponto de mundo `(x, y)` está dentro do polígono (exterior menos buracos)?
///
/// Usado pelo canvas para achar a célula clicada sem alocar uma
/// `Geometry` por célula (a grade pode ter milhares de células na viewport).
pub fn point_in_polygon(poly: &Polygon<f64>, x: f64, y: f64) -> bool {
    point_in_ring(poly.exterior(), x, y) && !poly.interiors().iter().any(|r| point_in_ring(r, x, y))
}

/// Hit-test de pick: o ponto de mundo `(x, y)` está sobre a geometria,
/// com tolerância `tol` em **unidades de mundo** (já dividida pela escala)?
pub fn hit_test(geom: &Geometry<f64>, x: f64, y: f64, tol: f64) -> bool {
    match geom {
        Geometry::Point(p) => distance(x, y, p.x(), p.y()) <= tol,
        Geometry::MultiPoint(mp) => mp.0.iter().any(|p| distance(x, y, p.x(), p.y()) <= tol),
        Geometry::Line(l) => dist_to_segment(x, y, l.start, l.end) <= tol,
        Geometry::LineString(ls) => hit_linestring(ls, x, y, tol),
        Geometry::MultiLineString(mls) => mls.0.iter().any(|ls| hit_linestring(ls, x, y, tol)),
        Geometry::Polygon(poly) => {
            point_in_ring(poly.exterior(), x, y)
                && !poly.interiors().iter().any(|r| point_in_ring(r, x, y))
        }
        Geometry::Rect(r) => {
            let p = r.to_polygon();
            point_in_ring(p.exterior(), x, y)
        }
        Geometry::Triangle(t) => point_in_ring(t.to_polygon().exterior(), x, y),
        Geometry::MultiPolygon(mp) => mp.0.iter().any(|poly| {
            point_in_ring(poly.exterior(), x, y)
                && !poly.interiors().iter().any(|r| point_in_ring(r, x, y))
        }),
        Geometry::GeometryCollection(gc) => gc.0.iter().any(|g| hit_test(g, x, y, tol)),
    }
}

fn hit_linestring(ls: &LineString<f64>, x: f64, y: f64, tol: f64) -> bool {
    if ls.0.len() < 2 {
        return ls
            .0
            .first()
            .is_some_and(|c| distance(x, y, c.x, c.y) <= tol);
    }
    ls.0.windows(2)
        .any(|w| dist_to_segment(x, y, w[0], w[1]) <= tol)
}

/// Ray-cast par/ímpar sobre um anel (não considera buracos — quem chama
/// trata interiores).
fn point_in_ring(ring: &LineString<f64>, x: f64, y: f64) -> bool {
    let pts = &ring.0;
    if pts.len() < 4 {
        return false;
    }
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = (pts[i].x, pts[i].y);
        let (xj, yj) = (pts[j].x, pts[j].y);
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn distance(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    ((x2 - x1).mul_add(x2 - x1, (y2 - y1) * (y2 - y1))).sqrt()
}

fn dist_to_segment(px: f64, py: f64, a: Coord<f64>, b: Coord<f64>) -> f64 {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len2 = abx.mul_add(abx, aby * aby);
    if len2 <= f64::EPSILON {
        return distance(px, py, a.x, a.y);
    }
    let t = (((px - a.x) * abx + (py - a.y) * aby) / len2).clamp(0.0, 1.0);
    let cx = a.x + t * abx;
    let cy = a.y + t * aby;
    distance(px, py, cx, cy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{Point, Polygon};

    fn viewport_800x600() -> Viewport {
        let mut v = Viewport::new();
        v.set_size_px(800.0, 600.0);
        v
    }

    #[test]
    fn round_trip_tela_mundo() {
        let mut v = viewport_800x600();
        v.set_center(-60.0, -5.0);
        v.set_px_per_unit(100.0);
        for (wx, wy) in [(-60.0, -5.0), (-60.5, -4.25), (-59.9, -5.1)] {
            let (sx, sy) = v.world_to_screen(wx, wy);
            let (bx, by) = v.screen_to_world(sx, sy);
            assert!((bx - wx).abs() < 1e-9, "x: {bx} vs {wx}");
            assert!((by - wy).abs() < 1e-9, "y: {by} vs {wy}");
        }
    }

    #[test]
    fn centro_da_tela_e_o_centro_do_mundo() {
        let mut v = viewport_800x600();
        v.set_center(-61.0, -6.0);
        v.set_px_per_unit(42.0);
        let (sx, sy) = v.world_to_screen(-61.0, -6.0);
        assert!((sx - 400.0).abs() < 1e-4);
        assert!((sy - 300.0).abs() < 1e-4);
    }

    #[test]
    fn zoom_na_ancora_mantem_o_ponto_fixo() {
        let mut v = viewport_800x600();
        v.set_center(-60.0, -5.0);
        v.set_px_per_unit(10.0);
        let (sx, sy) = v.world_to_screen(-59.7, -4.8);
        let before = v.screen_to_world(sx, sy);
        v.zoom_at(sx, sy, 2.0);
        let after = v.screen_to_world(sx, sy);
        assert!((after.0 - before.0).abs() < 1e-9);
        assert!((after.1 - before.1).abs() < 1e-9);
        assert!((v.px_per_unit() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn pan_move_o_centro_na_direcao_oposta() {
        let mut v = viewport_800x600();
        v.set_center(-60.0, -5.0);
        v.set_px_per_unit(100.0);
        v.pan_pixels(100.0, 0.0);
        assert!((v.center().0 - (-61.0)).abs() < 1e-9);
        assert!((v.center().1 - (-5.0)).abs() < 1e-9);
    }

    #[test]
    fn zoom_to_bbox_encaixa_e_centra() {
        let mut v = viewport_800x600();
        let bbox = Bbox::new(-73.0, -10.0, -50.0, 2.0);
        v.zoom_to_bbox(&bbox, 0.05);
        let (cx, cy) = v.center();
        assert!((cx - (-61.5)).abs() < 1e-9);
        assert!((cy - (-4.0)).abs() < 1e-9);
        // largura em px do bbox deve caber na viewport com margem
        let (x0, _) = v.world_to_screen(-73.0, 0.0);
        let (x1, _) = v.world_to_screen(-50.0, 0.0);
        assert!((x1 - x0) < 800.0 && (x1 - x0) > 700.0);
    }

    #[test]
    fn stride_lod_cresce_no_zoom_out() {
        assert_eq!(lod_stride(800.0), Some(1));
        assert_eq!(lod_stride(64.0), Some(1));
        assert!(lod_stride(30.0).unwrap() > 1);
        assert!(lod_stride(5.0).unwrap() > lod_stride(30.0).unwrap());
        assert_eq!(lod_stride(1.0), None, "subpixel é pulado");
        assert_eq!(lod_stride(0.0), None);
    }

    #[test]
    fn decimate_preserva_o_ultimo() {
        assert_eq!(decimate_indices(10, 3), vec![0, 3, 6, 9]);
        assert_eq!(decimate_indices(4, 10), vec![0, 3]);
        assert_eq!(decimate_indices(5, 1), vec![0, 1, 2, 3, 4]);
        assert!(decimate_indices(0, 5).is_empty());
    }

    #[test]
    fn hit_test_ponto_linha_poligono() {
        let pt = Geometry::Point(Point::new(-60.0, -5.0));
        assert!(hit_test(&pt, -60.0, -5.0, 1e-6));
        assert!(!hit_test(&pt, -59.0, -5.0, 1e-6));
        assert!(hit_test(&pt, -59.99995, -5.0, 1e-4)); // tolerância

        let ls = Geometry::LineString(LineString::new(vec![
            Coord { x: -60.0, y: -5.0 },
            Coord { x: -59.0, y: -5.0 },
        ]));
        assert!(hit_test(&ls, -59.5, -5.001, 1e-2));
        assert!(!hit_test(&ls, -59.5, -4.9, 1e-2));

        // Polígono com buraco: clique no buraco não pega.
        let exterior = LineString::new(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 10.0, y: 0.0 },
            Coord { x: 10.0, y: 10.0 },
            Coord { x: 0.0, y: 10.0 },
            Coord { x: 0.0, y: 0.0 },
        ]);
        let hole = LineString::new(vec![
            Coord { x: 4.0, y: 4.0 },
            Coord { x: 6.0, y: 4.0 },
            Coord { x: 6.0, y: 6.0 },
            Coord { x: 4.0, y: 6.0 },
            Coord { x: 4.0, y: 4.0 },
        ]);
        let poly = Geometry::Polygon(Polygon::new(exterior, vec![hole]));
        assert!(hit_test(&poly, 1.0, 1.0, 1e-9));
        assert!(!hit_test(&poly, 5.0, 5.0, 1e-9));
        // Buraco continua buraco mesmo com tolerância (a tolerância do pick
        // é aplicada às bordas externas; interno do furo não vira hit).
        assert!(!hit_test(&poly, 5.0, 5.0, 1.0));
    }

    #[test]
    fn bbox_de_geometria() {
        let g = Geometry::Point(Point::new(-60.0, -5.0));
        let b = geometry_bbox(&g).expect("point tem bbox");
        assert_eq!(
            (b.min_x, b.min_y, b.max_x, b.max_y),
            (-60.0, -5.0, -60.0, -5.0)
        );
    }

    #[test]
    fn denominador_de_escala_positivo() {
        let mut v = viewport_800x600();
        v.set_px_per_unit(1e-5);
        assert!(v.scale_denominator() > 0.0);
    }
}
