//! Contrato de visualização de mapa ([`MapView`]) e a implementação egui
//! ([`MapViewEgui`]) da Fase 3.
//!
//! # Por que uma trait?
//!
//! O MVP renderiza com **egui/eframe** (imediato, puro Rust, sem dependência
//! obrigatória de WebGL), mas o contrato abaixo é deliberadamente migrável:
//! se o bench da Fase 3 apontar limite do egui para raster grande (gatilho
//! objetivo da Fase 3b), a migração para Tauri+MapLibre troca **apenas a
//! implementação desta trait** — estado do app, projeto, camadas e IO não
//! mudam.
//!
//! # Decisões de implementação (documentadas por exigência da Fase 3)
//!
//! - **Render vetorial**: rasterização direta das coordenadas no painter do
//!   egui (`crate::draw`) — sem tiny-skia intermediário. Motivo: o painter do
//!   egui já é um tessellator 2D em CPU com clipping; um buffer intermediário
//!   adicionaria uma cópia por frame sem ganho no MVP. Preenchimento só é
//!   emitido para anéis convexos (limitação pública do `PathShape` do epaint:
//!   "fill is only supported for convex polygons"); anéis côncavos recebem
//!   contorno.
//! - **LOD client-side**: decimação de vértices por stride proporcional ao
//!   zoom ([`viewport::lod_stride`]) + descarte de geometrias subpixel.
//!   Documentação completa no módulo `viewport`.
//! - **Raster**: a banda ativa é carregada uma vez (downsample via GDAL,
//!   `crate::raster_view`) para uma `ColorImage` ≤ 2048 px e vira textura do
//!   egui; o downsample por frame é o draw da textura escalada ao bbox de
//!   mundo (O(viewport), não O(pixels do arquivo)). Seletor de banda fica no
//!   inspetor (`panels`).
//! - **Repaint**: contínuo **somente** durante interação (drag/zoom) —
//!   `Context::request_repaint` é chamado só nesses eventos; fora disso o
//!   egui dorme até input.
//!
//! # Mapeamento de ids de pick
//!
//! `FeatureId` (u64) do contrato é uid sintético estável:
//! `uid = (índice_da_camada << 32) | índice_da_feature` — decodificado por
//! [`decode_feature_uid`]; o id de linha do banco (`i64`) viaja em
//! [`LayerFeature::db_id`] e nunca vaza para a trait.

use eframe::egui;
use egui::{Color32, Id, LayerId, Order, Pos2, Rect, Sense, Vec2};
use geo_types::Polygon;

use ta_core::feature::{Feature, FeatureId};
use ta_core::layer::{GeomType, Layer};
use ta_core::project::DEFAULT_SRID;
use ta_core::style::Style;

use crate::draw;
use crate::raster_view::RasterBandImage;
use crate::viewport::{Viewport, geometry_bbox, hit_test};

/// Handle de camada no mapa: o backend mantém o estado de render (cache,
/// LOD, visibilidade) a partir do dado de domínio.
#[derive(Debug, Clone)]
pub struct LayerHandle {
    pub layer: Layer,
    pub visible: bool,
    /// Estilo de render da camada (cor/espessura/opacidade — inspetor).
    pub style: Style,
    /// Feições + id de linha do banco (None = camada local/arquivo).
    pub features: Vec<LayerFeature>,
}

/// Uma feição da camada com seu id de banco opcional.
#[derive(Debug, Clone)]
pub struct LayerFeature {
    pub feature: Feature,
    pub db_id: Option<i64>,
}

impl LayerFeature {
    pub fn local(feature: Feature) -> Self {
        Self {
            feature,
            db_id: None,
        }
    }
}

/// BBox em coordenadas do SRID corrente do mapa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bbox {
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }
}

/// Contorno da AOI selecionada no canvas (amarelo-âmbar do tema).
pub const AOI_OUTLINE: Color32 = Color32::from_rgb(0xF2, 0xC1, 0x4E);

/// Uma célula da grade pronta para pintar (Fase 5b).
///
/// `fill` já vem resolvido pela UI ([`crate::monitoring::cell_fill`]): cor de
/// `classes.color` ou cinza de bloqueio. O canvas não decide classificação.
#[derive(Debug, Clone)]
pub struct CellShape {
    pub cell_id: String,
    pub polygon: Polygon<f64>,
    pub fill: Color32,
    pub blocked: bool,
}

/// Overlay de monitoramento do canvas: contorno da AOI + células.
#[derive(Debug, Clone, Default)]
pub struct CellOverlay {
    /// Contornos da AOI selecionada (EPSG:4674, polígonos das feições).
    pub aoi: Vec<Polygon<f64>>,
    pub cells: Vec<CellShape>,
    /// Célula destacada (alvo de bloquear/desbloquear).
    pub selected: Option<String>,
}

/// Contexto de render entregue à implementação de [`MapView`].
///
/// Encapsula o backend de pintura: hoje o `egui::Context`; amanhã, o contexto
/// equivalente do backend que vier (MapLibre/WebView). O contrato da trait
/// permanece estável.
pub struct RenderCtx<'a> {
    pub egui: &'a egui::Context,
    /// Origem do canvas em pixels (canto superior esquerdo, coordenadas da
    /// janela — o painel central não cobre os docks).
    pub origin_px: (f32, f32),
    /// Tamanho do viewport em pixels.
    pub viewport_px: (f32, f32),
}

/// Contrato do canvas de mapa — independente de backend de render.
///
/// Única implementação do MVP: [`MapViewEgui`] (egui). O contrato permite
/// migrar para MapLibre (Fase 3b condicional) sem reescrever o aplicativo.
pub trait MapView {
    /// Substitui o conjunto de camadas renderizadas.
    fn set_layers(&mut self, layers: Vec<LayerHandle>);
    /// Define o SRID corrente do mapa (do projeto; 4674 no clean-room).
    fn set_crs(&mut self, srid: u32);
    /// Renderiza o frame corrente.
    fn render(&mut self, ctx: &mut RenderCtx<'_>);
    /// Pick: feature sob a posição de tela (pixels), se houver.
    fn pick(&self, screen_pos: (f32, f32)) -> Option<FeatureId>;
    /// Ajusta a viewport para o bbox dado (zoom-to-extent).
    fn zoom_to(&mut self, bbox: Bbox);
}

/// Camada raster carregada por referência (Fase 2: pixels ficam no arquivo).
#[derive(Debug, Clone)]
pub struct RasterLayerView {
    /// Arquivo GeoTIFF/COG (para recarregar outra banda).
    pub path: std::path::PathBuf,
    /// Bbox de mundo (do geotransform) — posiciona a textura no mapa.
    pub bbox: Bbox,
    /// Banda ativa (0-based).
    pub band: usize,
    /// Número de bandas do arquivo.
    pub band_count: usize,
    /// Imagem decimada da banda ativa (None = ainda não carregada).
    pub image: Option<RasterBandImage>,
}

/// uid sintético: (camada << 32) | feature — estável entre frames.
pub fn encode_feature_uid(layer_idx: usize, feat_idx: usize) -> FeatureId {
    ((layer_idx as u64) << 32) | (feat_idx as u64)
}

/// Decodifica o uid sintético de pick.
pub fn decode_feature_uid(uid: FeatureId) -> (usize, usize) {
    ((uid >> 32) as usize, (uid & 0xFFFF_FFFF) as usize)
}

/// Id estável do canvas dentro do egui.
fn canvas_id() -> Id {
    Id::unique("ta_map_canvas")
}

/// LOD client-side para camadas **pontuais**: acima de [`POINT_BUDGET`]
/// feições visíveis, pula com stride determinístico (i % stride) — pontos
/// densos em zoom-out viram "nuvem" de qualquer forma. Documentado como
/// decisão da Fase 3; o stride zera (todos desenhados) com zoom in.
pub const POINT_BUDGET: usize = 20_000;

/// Stride de decimação de pontos para `n` feições (1 = desenha todas).
pub fn point_stride(n: usize) -> usize {
    if n <= POINT_BUDGET {
        1
    } else {
        n.div_ceil(POINT_BUDGET)
    }
}

/// Implementação egui — a única do MVP (sem Debug: `TextureHandle` não é
/// Debug e o derive não é usado em UI).
#[derive(Default)]
pub struct MapViewEgui {
    layers: Vec<LayerHandle>,
    srid: u32,
    viewport: Viewport,
    /// BBox por camada (pré-computado no `set_layers` para pick O(n) barato).
    bboxes: Vec<Vec<Option<Bbox>>>,
    raster: Option<RasterLayerView>,
    raster_texture: Option<egui::TextureHandle>,
    raster_texture_key: Option<(std::path::PathBuf, usize)>,
    selection: Option<FeatureId>,
    pending_click: Option<((f32, f32), (f64, f64))>,
    cursor_world: Option<(f64, f64)>,
    dragging: bool,
    /// Paleta por índice de camada (determinística entre frames).
    palette: [Color32; 6],
    /// Overlay do monitoramento (AOI + células/classes) — Fase 5b.
    cell_overlay: Option<CellOverlay>,
}

impl MapViewEgui {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            srid: DEFAULT_SRID,
            viewport: Viewport::new(),
            bboxes: Vec::new(),
            raster: None,
            raster_texture: None,
            raster_texture_key: None,
            selection: None,
            pending_click: None,
            cursor_world: None,
            dragging: false,
            palette: draw::default_palette(),
            cell_overlay: None,
        }
    }

    /// SRID corrente do mapa.
    pub fn srid(&self) -> u32 {
        self.srid
    }

    /// Número de camadas carregadas.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Viewport corrente (centro/escala) — status bar e testes.
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Viewport mutável (preset de escala da status bar — v0.2).
    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    /// Última posição de mundo sob o cursor (status bar).
    pub fn cursor_world(&self) -> Option<(f64, f64)> {
        self.cursor_world
    }

    /// Consome o clique simples pendente (pick/novo ponto) — um por frame.
    /// Devolve `(tela_relativa_ao_canvas, mundo)`.
    pub fn take_click(&mut self) -> Option<((f32, f32), (f64, f64))> {
        self.pending_click.take()
    }

    /// Camada raster ativa (inspetor/panels).
    pub fn raster(&self) -> Option<&RasterLayerView> {
        self.raster.as_ref()
    }

    /// Camada raster mutável (troca de banda no inspetor).
    pub fn raster_mut(&mut self) -> Option<&mut RasterLayerView> {
        self.raster.as_mut()
    }

    /// Define/atualiza a camada raster (None remove).
    pub fn set_raster(&mut self, raster: Option<RasterLayerView>) {
        self.raster = raster;
        self.raster_texture = None;
        self.raster_texture_key = None;
    }

    /// Substitui a imagem da banda ativa (recarga do seletor de banda).
    pub fn set_raster_image(&mut self, image: RasterBandImage) {
        if let Some(r) = &mut self.raster {
            r.image = Some(image);
        }
        self.raster_texture = None;
    }

    /// Seleção destacada no canvas (uid de pick).
    pub fn set_selection(&mut self, selection: Option<FeatureId>) {
        self.selection = selection;
    }

    /// Define o overlay de monitoramento (AOI + células); `None` limpa.
    pub fn set_cell_overlay(&mut self, overlay: Option<CellOverlay>) {
        self.cell_overlay = overlay;
    }

    /// Destaca uma célula do overlay (sem exigir remontar o vetor).
    pub fn set_cell_selected(&mut self, cell_id: Option<&str>) {
        if let Some(overlay) = &mut self.cell_overlay {
            overlay.selected = cell_id.map(str::to_owned);
        }
    }

    /// Célula do overlay sob o ponto de mundo (hit-test do polígono).
    ///
    /// A grade é regular e não sobrepõe células; devolve a primeira que
    /// contém o ponto — `None` quando o clique cai fora da grade.
    pub fn cell_at(&self, x: f64, y: f64) -> Option<String> {
        let overlay = self.cell_overlay.as_ref()?;
        overlay
            .cells
            .iter()
            .find(|cell| crate::viewport::point_in_polygon(&cell.polygon, x, y))
            .map(|cell| cell.cell_id.clone())
    }

    /// Overlay corrente (legenda/inspeção em testes).
    pub fn overlay(&self) -> Option<&CellOverlay> {
        self.cell_overlay.as_ref()
    }

    /// Zoom pelos botões da toolbar (+/−) — âncora no centro.
    pub fn zoom_step(&mut self, factor: f64) {
        self.viewport.zoom_center(factor);
    }

    /// Ponto de mundo ↔ tela (usado pelo app para "novo ponto" e relatos).
    pub fn screen_to_world(&self, sx: f32, sy: f32) -> (f64, f64) {
        self.viewport.screen_to_world(sx, sy)
    }

    /// Reconstrói o cache de bbox por feature (chamar via `set_layers`).
    fn rebuild_bboxes(&mut self) {
        self.bboxes = self
            .layers
            .iter()
            .map(|h| {
                h.features
                    .iter()
                    .map(|lf| geometry_bbox(&lf.feature.geom))
                    .collect()
            })
            .collect();
    }

    fn handle_input(&mut self, egui_ctx: &egui::Context, rect: Rect, resp: &egui::Response) {
        // Pan por arraste.
        if resp.dragged() {
            let d = resp.drag_delta();
            self.viewport.pan_pixels(d.x, d.y);
            self.dragging = true;
            // Repaint contínuo SOMENTE durante interação (decisão documentada).
            egui_ctx.request_repaint();
        } else if self.dragging {
            self.dragging = false;
        }

        // Zoom pela roda (ancorado no cursor) — convenção GIS.
        if resp.hovered() {
            egui_ctx.input(|i| {
                let scroll = i.smooth_scroll_delta.y;
                if scroll.abs() > f32::EPSILON {
                    let factor = (f64::from(scroll) * 0.004).exp();
                    if let Some(pos) = i.pointer.hover_pos() {
                        let local = (pos.x - rect.left(), pos.y - rect.top());
                        self.viewport.zoom_at(local.0, local.1, factor);
                        egui_ctx.request_repaint();
                    }
                }
                // Cursor → mundo só dentro do canvas (status bar honesta).
                if let Some(pos) = i.pointer.hover_pos().filter(|p| rect.contains(*p)) {
                    self.cursor_world = Some(
                        self.viewport
                            .screen_to_world(pos.x - rect.left(), pos.y - rect.top()),
                    );
                } else {
                    self.cursor_world = None;
                }
            });
        }

        // Clique simples (sem arraste) → pick / novo ponto.
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let local = (pos.x - rect.left(), pos.y - rect.top());
                let world = self.viewport.screen_to_world(local.0, local.1);
                self.pending_click = Some((local, world));
            }
        }
    }

    fn ensure_raster_texture(&mut self, egui_ctx: &egui::Context) {
        let Some(raster) = &self.raster else {
            return;
        };
        let Some(image) = &raster.image else {
            return;
        };
        let key = (raster.path.clone(), raster.band);
        if self.raster_texture_key.as_ref() == Some(&key) && self.raster_texture.is_some() {
            return;
        }
        let handle = egui_ctx.load_texture(
            "ta_raster_band",
            image.image.clone(),
            egui::TextureOptions::LINEAR,
        );
        self.raster_texture = Some(handle);
        self.raster_texture_key = Some(key);
    }
}

impl MapView for MapViewEgui {
    fn set_layers(&mut self, layers: Vec<LayerHandle>) {
        self.layers = layers;
        self.rebuild_bboxes();
    }

    fn set_crs(&mut self, srid: u32) {
        self.srid = srid;
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_>) {
        self.viewport
            .set_size_px(ctx.viewport_px.0, ctx.viewport_px.1);
        let rect = Rect::from_min_size(
            Pos2::new(ctx.origin_px.0, ctx.origin_px.1),
            Vec2::new(ctx.viewport_px.0.max(1.0), ctx.viewport_px.1.max(1.0)),
        );

        // Área interativa do canvas: captura drag/scroll/click.
        let area_resp = egui::Area::new(canvas_id())
            .fixed_pos(rect.min)
            .order(Order::Middle)
            .sense(Sense::click_and_drag())
            .show(ctx.egui, |ui| {
                // 0.37: allocate_exact_size devolve (Rect, Response).
                ui.allocate_exact_size(rect.size(), Sense::click_and_drag())
                    .1
            });
        let resp = area_resp.inner;
        self.handle_input(ctx.egui, rect, &resp);

        // Painter com clip no retângulo do canvas: nada "vaza" para os docks.
        let painter = egui::Painter::new(
            ctx.egui.clone(),
            LayerId::new(Order::Middle, canvas_id()),
            rect,
        );
        // Fundo do mapa (cor do tema — contraste garantido nos dois temas).
        let bg = ctx.egui.global_style().visuals.extreme_bg_color;
        painter.rect_filled(rect, 0.0, bg);

        // Raster (banda ativa) sob o vetor.
        self.ensure_raster_texture(ctx.egui);
        if let (Some(raster), Some(texture)) = (&self.raster, &self.raster_texture) {
            draw::draw_raster(&painter, rect, &self.viewport, raster, texture.id());
        }

        // Camadas vetoriais na ordem da árvore (0 = fundo).
        for (layer_idx, handle) in self.layers.iter().enumerate() {
            if !handle.visible {
                continue;
            }
            let color = self.palette[layer_idx % self.palette.len()];
            let bboxes = self.bboxes.get(layer_idx);
            // LOD pontual: acima do orçamento, stride determinístico.
            let stride = match handle.layer.geom_type {
                GeomType::Point | GeomType::MultiPoint => point_stride(handle.features.len()),
                _ => 1,
            };
            for (feat_idx, lf) in handle.features.iter().enumerate() {
                if stride > 1 && feat_idx % stride != 0 {
                    continue; // LOD (documentado em `point_stride`)
                }
                if let Some(bbox) = bboxes.and_then(|b| b.get(feat_idx).copied().flatten()) {
                    if !intersects_viewport(&self.viewport, &bbox) {
                        continue;
                    }
                }
                let uid = encode_feature_uid(layer_idx, feat_idx);
                let selected = self.selection == Some(uid);
                draw::draw_geometry(
                    &painter,
                    rect,
                    &self.viewport,
                    &lf.feature.geom,
                    draw::GeomStyle {
                        color,
                        stroke_width: handle.style.stroke_width,
                        opacity: handle.style.opacity,
                        selected,
                    },
                );
            }
        }

        // Overlay do monitoramento (Fase 5b): células coloridas/bloqueadas e,
        // por cima, o contorno da AOI selecionada. O preenchimento já vem
        // resolvido por `monitoring::cell_fill` — o canvas não decide classe.
        if let Some(overlay) = &self.cell_overlay {
            for cell in &overlay.cells {
                let selected = overlay.selected.as_deref() == Some(cell.cell_id.as_str());
                draw::draw_cell(
                    &painter,
                    rect,
                    &self.viewport,
                    &cell.polygon,
                    cell.fill,
                    cell.blocked,
                    selected,
                );
            }
            for poly in &overlay.aoi {
                draw::draw_outline(&painter, rect, &self.viewport, poly, AOI_OUTLINE, 2.0);
            }
        }

        // Replica o cursor como mira no modo "novo ponto" (feedback a11y).
        if let Some(cursor) = self.cursor_world {
            if let Some((sx, sy)) = Some(self.viewport.world_to_screen(cursor.0, cursor.1)) {
                let p = Pos2::new(rect.left() + sx, rect.top() + sy);
                if rect.contains(p) {
                    painter.circle_stroke(p, 6.0, egui::Stroke::new(1.5, Color32::WHITE));
                }
            }
        }
    }

    fn pick(&self, screen_pos: (f32, f32)) -> Option<FeatureId> {
        let (wx, wy) = self.viewport.screen_to_world(screen_pos.0, screen_pos.1);
        // Tolerância de ~6 px convertida para unidades de mundo.
        let tol = 6.0 / self.viewport.px_per_unit().max(f64::EPSILON);
        for (layer_idx, handle) in self.layers.iter().enumerate().rev() {
            if !handle.visible {
                continue;
            }
            for (feat_idx, lf) in handle.features.iter().enumerate().rev() {
                // Filtro rápido pelo bbox (pré-computado) antes do teste fino.
                if let Some(bbox) = self
                    .bboxes
                    .get(layer_idx)
                    .and_then(|b| b.get(feat_idx))
                    .copied()
                    .flatten()
                {
                    let inside = wx >= bbox.min_x - tol
                        && wx <= bbox.max_x + tol
                        && wy >= bbox.min_y - tol
                        && wy <= bbox.max_y + tol;
                    if !inside {
                        continue;
                    }
                }
                if hit_test(&lf.feature.geom, wx, wy, tol) {
                    return Some(encode_feature_uid(layer_idx, feat_idx));
                }
            }
        }
        None
    }

    fn zoom_to(&mut self, bbox: Bbox) {
        self.viewport.zoom_to_bbox(&bbox, 0.05);
    }
}

/// O bbox da feature intersecta a viewport corrente (culling barato).
fn intersects_viewport(vp: &Viewport, bbox: &Bbox) -> bool {
    let (w, h) = vp.size_px();
    if w <= 1.0 || h <= 1.0 {
        return true; // viewport ainda não medida — desenha (primeiro frame)
    }
    let (min_x, min_y) = vp.screen_to_world(0.0, h);
    let (max_x, max_y) = vp.screen_to_world(w, 0.0);
    bbox.max_x >= min_x && bbox.min_x <= max_x && bbox.max_y >= min_y && bbox.min_y <= max_y
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{Geometry, Point};
    use ta_core::layer::GeomType;

    fn point_layer(name: &str, xs: &[f64]) -> LayerHandle {
        LayerHandle {
            layer: Layer {
                id: name.into(),
                project_id: "p".into(),
                name: name.into(),
                geom_type: GeomType::Point,
                srid: 4674,
                source_uri: String::new(),
            },
            visible: true,
            style: Style::default(),
            features: xs
                .iter()
                .map(|&x| {
                    LayerFeature::local(Feature::new(
                        Geometry::Point(Point::new(x, -x)),
                        serde_json::json!({"x": x}),
                    ))
                })
                .collect(),
        }
    }

    #[test]
    fn mapview_nasce_em_srid_4674() {
        let m = MapViewEgui::new();
        assert_eq!(m.srid(), 4674);
        assert_eq!(m.layer_count(), 0);
    }

    #[test]
    fn set_crs_e_layer_count_respeitam_contrato() {
        let mut m = MapViewEgui::new();
        m.set_crs(4326);
        assert_eq!(m.srid(), 4326);
        m.set_layers(vec![point_layer("l1", &[-60.0])]);
        assert_eq!(m.layer_count(), 1);
        // Pick em viewport vazia (sem render) ainda não acha nada.
        assert!(m.pick((0.0, 0.0)).is_none());
    }

    #[test]
    fn uid_codifica_camada_e_feature() {
        let uid = encode_feature_uid(3, 7);
        assert_eq!(decode_feature_uid(uid), (3, 7));
        let uid = encode_feature_uid(0, 0xFFFF_FFFF);
        assert_eq!(decode_feature_uid(uid), (0, 0xFFFF_FFFF));
    }

    #[test]
    fn zoom_to_centra_o_bbox() {
        let mut m = MapViewEgui::new();
        // Tamanho precisa estar definido (o render faz isso; no teste,
        // zoom_to sem viewport só centraliza).
        m.zoom_to(Bbox::new(-73.0, -10.0, -50.0, 2.0));
        let (cx, cy) = m.viewport().center();
        assert!((cx - (-61.5)).abs() < 1e-9);
        assert!((cy - (-4.0)).abs() < 1e-9);
    }
}
