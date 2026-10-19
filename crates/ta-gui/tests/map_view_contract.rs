//! Teste de **contrato** da trait `MapView` (Fase 3 — estende o esqueleto da
//! Fase 0): um mock registra as chamadas e um driver genérico exercita os
//! **5 métodos** contra qualquer implementação — mock e `MapViewEgui` têm de
//! obedecer ao mesmo contrato. A renderização egui roda headless
//! (`Context::run_pass` — sem GPU/display, seguro no CI).

use eframe::egui;
use geo_types::{Geometry, Point};
use ta_core::feature::{Feature, FeatureId};
use ta_core::layer::{GeomType, Layer};
use ta_core::style::Style;
use ta_gui::map_view::{Bbox, LayerFeature, LayerHandle, MapView, MapViewEgui, RenderCtx};

/// Mock que registra o uso do contrato (nada de render real).
#[derive(Default)]
struct MockMapView {
    layers: usize,
    srid: u32,
    renders: usize,
    picks: Vec<Option<FeatureId>>,
    zooms: Vec<Bbox>,
}

impl MapView for MockMapView {
    fn set_layers(&mut self, layers: Vec<LayerHandle>) {
        self.layers = layers.len();
    }
    fn set_crs(&mut self, srid: u32) {
        self.srid = srid;
    }
    fn render(&mut self, _ctx: &mut RenderCtx<'_>) {
        self.renders += 1;
    }
    fn pick(&self, screen_pos: (f32, f32)) -> Option<FeatureId> {
        Some((u64::from(screen_pos.0 as u32) << 16) | u64::from(screen_pos.1 as u32))
    }
    fn zoom_to(&mut self, bbox: Bbox) {
        self.zooms.push(bbox);
    }
}

/// Camada de pontos padrão dos testes.
fn point_layer(name: &str, pts: &[(f64, f64)]) -> LayerHandle {
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
        features: pts
            .iter()
            .map(|&(x, y)| {
                LayerFeature::local(Feature::new(
                    Geometry::Point(Point::new(x, y)),
                    serde_json::json!({"x": x, "y": y}),
                ))
            })
            .collect(),
    }
}

/// Driver do contrato: os 5 métodos, na sequência de uso real da UI.
fn exercise_contract(v: &mut dyn MapView) {
    v.set_crs(4674);
    v.set_layers(vec![point_layer("camada", &[(-60.0, -5.0)])]);
    v.render(&mut RenderCtx {
        egui: &egui::Context::default(),
        origin_px: (0.0, 0.0),
        viewport_px: (800.0, 600.0),
    });
    let _ = v.pick((100.0, 100.0));
    v.zoom_to(Bbox::new(-73.0, -10.0, -50.0, 2.0));
}

#[test]
fn mock_obedece_ao_contrato() {
    let mut mock = MockMapView::default();
    exercise_contract(&mut mock);
    assert_eq!(mock.srid, 4674);
    assert_eq!(mock.layers, 1);
    assert_eq!(mock.renders, 1);
    assert_eq!(
        mock.picks.len(),
        0,
        "pick do mock não registra estado (puro)"
    );
    assert_eq!(mock.zooms.len(), 1);
    assert_eq!(mock.zooms[0], Bbox::new(-73.0, -10.0, -50.0, 2.0));
}

#[test]
fn egui_headless_render_produz_shapes_sem_gpu() {
    let mut view = MapViewEgui::new();
    view.set_crs(4674);
    view.set_layers(vec![point_layer(
        "pontos",
        &(0..50)
            .map(|i| (-60.0 + f64::from(i) * 0.01, -5.0))
            .collect::<Vec<_>>(),
    )]);

    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_pass(raw, |ctx| {
        let mut rctx = RenderCtx {
            egui: ctx,
            origin_px: (0.0, 0.0),
            viewport_px: (800.0, 600.0),
        };
        view.render(&mut rctx);
    });
    assert!(
        !output.shapes.is_empty(),
        "render headless deve produzir shapes (fundo/pontos)"
    );
    // A tesselação em CPU funciona sem renderer (o bench usa o mesmo caminho).
    let ppp = output.pixels_per_point;
    let shapes = std::mem::take(&mut output.shapes);
    // Deltas de textura (fonte) são do backend — headless descarta com a
    // API oficial (sem o assert de Drop).
    output.drop_without_applying_deltas();
    let prims = ctx.tessellate(shapes, ppp);
    assert!(!prims.is_empty());
}

#[test]
fn pick_da_feature_certa_apos_render_headless() {
    let mut view = MapViewEgui::new();
    let pts: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), f64::from(i))).collect();
    view.set_layers(vec![point_layer("pontos", &pts)]);

    // O fit precisa da viewport MEDIDA: um frame headless define o tamanho.
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    let output = ctx.run_pass(raw, |ctx| {
        // Ordem importa: o primeiro render mede a viewport; o zoom usa o
        // tamanho real (não o 0×0 do primeiro frame).
        let mut rctx = RenderCtx {
            egui: ctx,
            origin_px: (0.0, 0.0),
            viewport_px: (800.0, 600.0),
        };
        view.render(&mut rctx);
        view.zoom_to(Bbox::new(-1.0, -1.0, 11.0, 11.0));
    });
    output.drop_without_applying_deltas();

    // Ponto 7: (7, 7) → tela → pick.
    let (sx, sy) = view.viewport().world_to_screen(7.0, 7.0);
    assert_eq!(
        view.pick((sx, sy)),
        Some(ta_gui::map_view::encode_feature_uid(0, 7)),
        "pick deve achar a feature 7 no centro do símbolo"
    );
    // Canto vazio do extent (longe de qualquer ponto) → None.
    let (fx, fy) = view.viewport().world_to_screen(4.5, 0.5);
    assert_eq!(view.pick((fx, fy)), None);
}

/// Contrato de renderização do overlay (Fase 5b), sem snapshot gráfico:
/// cor de classe vem de `classes.color`, bloqueio tem precedência e o pick
/// do canvas devolve a célula certa (base do "clicar para bloquear").
#[test]
fn overlay_colorido_por_classe_e_bloqueio_e_pick_da_celula() {
    use geo_types::{LineString, Polygon};
    use ta_core::classification::Class;
    use ta_db::repo_cell::CellViewRow;
    use ta_gui::monitoring::{BLOCKED_FILL, UNCLASSIFIED_FILL, overlay};

    fn cell(id: &str, min_x: f64, class: Option<&str>, blocked: bool) -> CellViewRow {
        CellViewRow {
            cell_id: id.to_owned(),
            geom: Polygon::new(
                LineString::from(vec![
                    (min_x, 0.0),
                    (min_x + 1.0, 0.0),
                    (min_x + 1.0, 1.0),
                    (min_x, 1.0),
                    (min_x, 0.0),
                ]),
                vec![],
            ),
            class_id: class.map(str::to_owned),
            blocked,
        }
    }

    let mut class = Class::new("p1", "floresta");
    class.id = "floresta".into();
    let class = class.with_color(Some("#12AB34"));
    let cells = vec![
        cell("0_0", 0.0, Some("floresta"), false),
        cell("1_0", 1.0, Some("floresta"), true),
        cell("2_0", 2.0, None, false),
    ];
    let ov = overlay(
        &cells,
        std::slice::from_ref(&class),
        Vec::new(),
        Some("1_0"),
    );

    assert_eq!(ov.cells[0].fill, ta_gui::monitoring::class_fill(&class));
    assert_eq!(ov.cells[1].fill, BLOCKED_FILL, "bloqueio vence a classe");
    assert_eq!(ov.cells[2].fill, UNCLASSIFIED_FILL);
    assert!(ov.cells[1].blocked);
    assert_eq!(ov.selected.as_deref(), Some("1_0"));

    let mut view = MapViewEgui::new();
    view.set_cell_overlay(Some(ov));
    assert_eq!(view.cell_at(0.5, 0.5).as_deref(), Some("0_0"));
    assert_eq!(
        view.cell_at(1.5, 0.5).as_deref(),
        Some("1_0"),
        "célula bloqueada continua clicável (desbloquear)"
    );
    assert_eq!(view.cell_at(9.0, 9.0), None);
}
