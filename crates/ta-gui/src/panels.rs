//! Painéis principais: trilho de ícones, dock de camadas, inspetor, canvas
//! (com overlays de grade/barra de escala) e utilitários de estilo.
//!
//! Menus → [`crate::menus`]; status bar → [`crate::status_bar`]; diálogos →
//! [`crate::dialogs`]/[`crate::work_dialogs`]/[`crate::welcome`].
//!
//! Acessibilidade: todos os controles têm rótulo textual (i18n), a ordem de
//! tabulação é a ordem de criação (padrão egui/accesskit) e nenhum controle
//! depende só de cor (ícones têm texto). Contraste: cores de texto/quadro
//! vêm do tema egui (dark/light verificados nos dois).

use eframe::egui;
use egui::{Color32, Sense, Stroke};

use crate::app::TerraApp;
use crate::app_state::EditMode;
use crate::i18n::Key as L;
use crate::map_view::{MapView, RenderCtx};
use crate::overlays::{grid_lines, grid_step, scale_bar};
use ta_core::layer::GeomType;

/// Conversão utilitária `#RRGGBB[AA]` ↔ Color32 (estilo da camada).
///
/// egui armazena Color32 **premultiplicado**: leitura de componentes usa
/// `to_srgba_unmultiplied` — hex da UI é sempre não-premultiplicado.
pub fn hex_to_color32(hex: &str) -> Color32 {
    let h = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("88"), 16).unwrap_or(0x88);
    let a = if h.len() >= 8 { byte(6) } else { 0xCC };
    Color32::from_rgba_unmultiplied(byte(0), byte(2), byte(4), a)
}

pub fn color32_to_hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
}

/// Formata números grandes de forma compacta (1:12M etc.) — status bar.
pub fn format_compact(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

/// Glifo por tipo de geometria (árvore de camadas — texto, nunca só cor).
pub fn geom_icon(gt: GeomType) -> &'static str {
    match gt {
        GeomType::Point | GeomType::MultiPoint => "●",
        GeomType::LineString | GeomType::MultiLineString => "─",
        GeomType::Polygon | GeomType::MultiPolygon => "▲",
        GeomType::GeometryCollection => "◈",
        GeomType::Raster => "▦",
    }
}

// ---------------------------------------------------------------------------
// Trilho de ícones (borda esquerda, paridade com a referência visual)
// ---------------------------------------------------------------------------

/// Trilho vertical fino: adicionar camada, conectar banco, zoom extent e
/// toggles de modo do mouse (identificar / novo ponto).
pub fn rail(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::left("ta_rail")
        .resizable(false)
        .default_size(36.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            if app.state.flags.import_export
                && ui
                    .small_button("📥")
                    .on_hover_text(app.t(L::RailAddLayer))
                    .clicked()
            {
                app.show_import = true;
            }
            if app.state.flags.tools
                && ui
                    .small_button("🔌")
                    .on_hover_text(app.t(L::RailConnect))
                    .clicked()
            {
                app.show_connect = true;
            }
            if ui
                .small_button("⤢")
                .on_hover_text(app.t(L::RailExtent))
                .clicked()
            {
                app.zoom_to_extent();
            }
            if app.state.flags.editing {
                ui.separator();
                let identify = app.state.edit_mode == EditMode::Navigate;
                if ui
                    .selectable_label(identify, "✛")
                    .on_hover_text(app.t(L::RailIdentify))
                    .clicked()
                {
                    app.state.edit_mode = EditMode::Navigate;
                }
                let new_point = app.state.edit_mode == EditMode::NewPoint;
                if ui
                    .selectable_label(new_point, "📍")
                    .on_hover_text(app.t(L::RailNewPoint))
                    .clicked()
                {
                    app.state.edit_mode = if new_point {
                        EditMode::Navigate
                    } else {
                        EditMode::NewPoint
                    };
                }
            }
        });
}

/// Dock esquerdo: árvore de camadas com visibilidade/ordem/ativa,
/// ícone por geometria e **context menu** (zoom, renomear, remover,
/// propriedades).
pub fn layers_dock(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::left("ta_layers")
        .resizable(true)
        .default_size(220.0)
        .show(ui, |ui| {
            ui.heading(app.t(L::Layers));
            ui.separator();
            if app.state.layers.is_empty() {
                ui.weak(app.t(L::LocalModeHint));
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Topo da árvore = camada de cima (última do vetor).
                for idx in (0..app.state.layers.len()).rev() {
                    let (name, visible, is_active, n_feats, icon) = {
                        let g = &app.state.layers[idx];
                        (
                            g.layer.name.clone(),
                            g.visible,
                            app.state.active_or_first() == Some(idx),
                            g.features.len(),
                            geom_icon(g.layer.geom_type),
                        )
                    };
                    ui.horizontal(|ui| {
                        // Visibilidade (rótulo acessível = nome da camada).
                        let mut vis = visible;
                        if ui
                            .checkbox(&mut vis, "")
                            .on_hover_text(format!("{}: {name}", app.t(L::LayerVisible)))
                            .changed()
                        {
                            app.state.layers[idx].visible = vis;
                            app.state.sync_map(&mut app.map);
                        }
                        let row = ui
                            .selectable_label(is_active, format!("{icon} {name}"))
                            .on_hover_text(format!("({n_feats})"));
                        if row.clicked() {
                            app.state.active_layer = Some(idx);
                        }
                        // Context menu (botão direito) — ações reais.
                        row.context_menu(|ui| {
                            if ui.button(app.t(L::LayerZoomTo)).clicked() {
                                if let Some(b) = app.state.layers[idx].bbox_of_layer() {
                                    app.map.zoom_to(crate::map_view::Bbox::new(
                                        b[0], b[1], b[2], b[3],
                                    ));
                                }
                                ui.close();
                            }
                            if app.state.conn.is_db()
                                && !app.state.layers[idx].layer.id.is_empty()
                                && ui.button(app.t(L::RenameLayer)).clicked()
                            {
                                app.open_rename_layer_at(idx);
                                ui.close();
                            }
                            if ui.button(app.t(L::LayerRemove)).clicked() {
                                app.remove_layer_at(idx);
                                ui.close();
                            }
                            if ui.button(app.t(L::LayerProperties)).clicked() {
                                app.dlg.layer_props = Some(idx);
                                ui.close();
                            }
                        });
                        ui.weak(format!("({n_feats})"));
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("▲")
                            .on_hover_text(app.t(L::LayerUp))
                            .clicked()
                            && idx + 1 < app.state.layers.len()
                        {
                            app.state.layers.swap(idx, idx + 1);
                            app.state.active_layer = Some(idx + 1);
                            app.state.sync_map(&mut app.map);
                        }
                        if ui
                            .small_button("▼")
                            .on_hover_text(app.t(L::LayerDown))
                            .clicked()
                            && idx > 0
                        {
                            app.state.layers.swap(idx, idx - 1);
                            app.state.active_layer = Some(idx - 1);
                            app.state.sync_map(&mut app.map);
                        }
                        if ui
                            .small_button("✕")
                            .on_hover_text(app.t(L::LayerRemove))
                            .clicked()
                        {
                            app.remove_layer_at(idx);
                        }
                    });
                    ui.separator();
                }
            });
        });
}

/// Painel direito: inspetor de CRS/estilo (+ seletor de banda de raster).
pub fn inspector(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::right("ta_inspector")
        .resizable(true)
        .default_size(240.0)
        .show(ui, |ui| {
            ui.heading(app.t(L::Inspector));
            ui.separator();
            egui::Grid::new("insp_grid").num_columns(2).show(ui, |ui| {
                ui.label(app.t(L::InspectorCrs));
                let srid = app.state.project.as_ref().map_or(4674, |p| p.srid);
                ui.label(format!("EPSG:{srid}"));
                ui.end_row();

                ui.label(app.t(L::StatusBarFeatures));
                let n: usize = app.state.layers.iter().map(|l| l.features.len()).sum();
                ui.label(n.to_string());
                ui.end_row();
            });

            ui.separator();
            ui.label(app.t(L::InspectorStyle));
            // Estilo da camada ativa (cor/espessura/opacidade) — edição só
            // com a persona liberando edição.
            let active = app
                .state
                .active_or_first()
                .filter(|_| app.state.flags.editing);
            if let Some(idx) = active {
                let mut color = hex_to_color32(&app.state.layers[idx].style.color);
                let mut width = app.state.layers[idx].style.stroke_width;
                let mut opacity = app.state.layers[idx].style.opacity;
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(app.t(L::InspectorColor));
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(app.t(L::InspectorStrokeWidth));
                    changed |= ui.add(egui::Slider::new(&mut width, 0.5..=6.0)).changed();
                });
                ui.horizontal(|ui| {
                    ui.label(app.t(L::InspectorOpacity));
                    changed |= ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0)).changed();
                });
                if changed {
                    let g = &mut app.state.layers[idx];
                    g.style.color = color32_to_hex(color);
                    g.style.stroke_width = width;
                    g.style.opacity = opacity;
                    app.state.sync_map(&mut app.map);
                }
            }

            // Raster: seletor de banda.
            if let Some(raster) = app.map.raster() {
                ui.separator();
                ui.label(format!(
                    "{}: {}",
                    app.t(L::InspectorBands),
                    raster.band_count.max(1)
                ));
                ui.label(app.t(L::InspectorBand));
                let path = raster.path.clone();
                let current = raster.band;
                let count = raster.band_count.max(1);
                ui.horizontal(|ui| {
                    for band in 0..count.min(8) {
                        let label = format!("{}", band + 1);
                        if ui.selectable_label(current == band, label).clicked() {
                            // Troca de banda em thread de fundo (a UI nunca
                            // bloqueia em GDAL); cache hit aplica na hora.
                            app.request_band(path.clone(), band);
                        }
                    }
                });
                if app.band_loading() {
                    ui.label(app.t(L::RasterBandLoading));
                }
            }

            ui.separator();
            ui.vertical(|ui| {
                if ui.button(app.t(L::ZoomToExtent)).clicked() {
                    app.zoom_to_extent();
                }
                if app.state.flags.editing {
                    let new_point = app.state.edit_mode == EditMode::NewPoint;
                    if ui
                        .selectable_label(new_point, app.t(L::NewPointMode))
                        .clicked()
                    {
                        app.state.edit_mode = if new_point {
                            EditMode::Navigate
                        } else {
                            EditMode::NewPoint
                        };
                    }
                    if new_point {
                        ui.weak(app.t(L::NewPointHint));
                        if ui.button(app.t(L::CancelEditMode)).clicked() {
                            app.state.edit_mode = EditMode::Navigate;
                        }
                    }
                    let has_sel = app.state.selection.is_some();
                    if ui
                        .add_enabled(has_sel, egui::Button::new(app.t(L::DeleteFeatureTitle)))
                        .clicked()
                    {
                        app.confirm_delete = true;
                    }
                }
            });
        });
}

/// Canvas central: banner de erro, toolbar do mapa e overlays.
///
/// O canvas NUNCA é escondido por persona (contrato da fase): este painel
/// é o último da "escultura" e consome toda a área restante.
pub fn central_canvas(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::CentralPanel::default().show(ui, |ui| {
        // Banner de erro (legível, dispensável — nada de panic na UI).
        if let Some(err) = app.error.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    Color32::from_rgb(0xE7, 0x4C, 0x3C),
                    format!("{}: {err}", app.t(L::ErrorTitle)),
                );
                if ui.small_button(app.t(L::ErrorDismiss)).clicked() {
                    app.error = None;
                }
            });
            ui.separator();
        }
        // Toolbar do mapa (linha fina dentro do canvas — ações de zoom/edição).
        ui.horizontal(|ui| {
            if ui.button("+").on_hover_text(app.t(L::ZoomIn)).clicked() {
                app.map.zoom_step(1.25);
            }
            if ui.button("−").on_hover_text(app.t(L::ZoomOut)).clicked() {
                app.map.zoom_step(0.8);
            }
            if ui.button(app.t(L::ZoomToExtent)).clicked() {
                app.zoom_to_extent();
            }
            if app.state.flags.editing {
                let new_point = app.state.edit_mode == EditMode::NewPoint;
                if ui
                    .selectable_label(new_point, app.t(L::NewPointMode))
                    .clicked()
                {
                    app.state.edit_mode = if new_point {
                        EditMode::Navigate
                    } else {
                        EditMode::NewPoint
                    };
                }
                ui.separator();
                let can_undo = !app.state.undo_stack.is_empty();
                if ui
                    .add_enabled(can_undo, egui::Button::new("↶"))
                    .on_hover_text(app.t(L::Undo))
                    .clicked()
                {
                    app.state.undo();
                    app.state.sync_map(&mut app.map);
                }
                let can_redo = !app.state.redo_stack.is_empty();
                if ui
                    .add_enabled(can_redo, egui::Button::new("↷"))
                    .on_hover_text(app.t(L::Redo))
                    .clicked()
                {
                    app.state.redo();
                    app.state.sync_map(&mut app.map);
                }
            }
        });
        ui.separator();

        // Canvas do mapa (contrato MapView — o app não desenha o mapa direto).
        let rect = ui.available_rect_before_wrap();
        let mut rctx = RenderCtx {
            egui: ui.ctx(),
            origin_px: (rect.left(), rect.top()),
            viewport_px: (rect.width(), rect.height()),
        };
        app.map.render(&mut rctx);
        // Fase 5b: consulta as células da viewport corrente (tolerância de
        // pan/zoom; troca de grade/rascunho força) — o worker responde.
        app.request_cell_view(false);
        // Overlays de UI (grade + barra de escala) — desenhados sobre o
        // mapa pelo painter do egui, sem tocar no contrato MapView.
        let painter = ui.painter_at(rect);
        if app.state.grid_on {
            draw_grid(&painter, app, rect);
        }
        if app.state.scale_bar_on {
            draw_scale_bar(&painter, app, rect);
        }
        // Legenda das classes/bloqueio do monitoramento (derivada das
        // células VISÍVEIS; sem entradas mortas).
        crate::monitoring::draw_legend(&painter, app, rect);
        // Garante que o painel reserve a área (evita layout oscilante).
        ui.allocate_exact_size(rect.size(), Sense::hover());
    });
}

/// Grade de meridianos/paralelos (passo 1-2-5 com ~80 px de espaçamento).
fn draw_grid(painter: &egui::Painter, app: &TerraApp, rect: egui::Rect) {
    let ppu = app.map.viewport().px_per_unit();
    let (wx0, wy0) = app.map.screen_to_world(0.0, 0.0); // canto sup. esq.
    let (max_x, min_y) = app.map.screen_to_world(rect.width(), rect.height());
    let step = grid_step(ppu, 80.0);
    let (xs, ys) = grid_lines(wx0, max_x.max(wx0), min_y.min(wy0), wy0, step);
    let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(128, 128, 128, 60));
    for x in xs {
        let sx = rect.left() + ((x - wx0) * ppu) as f32;
        painter.line_segment(
            [egui::pos2(sx, rect.top()), egui::pos2(sx, rect.bottom())],
            stroke,
        );
    }
    for y in ys {
        let sy = rect.top() + ((wy0 - y) * ppu) as f32;
        painter.line_segment(
            [egui::pos2(rect.left(), sy), egui::pos2(rect.right(), sy)],
            stroke,
        );
    }
}

/// Barra de escala (canto inferior esquerdo): traço + tiques + rótulo.
fn draw_scale_bar(painter: &egui::Painter, app: &TerraApp, rect: egui::Rect) {
    let den = app.map.viewport().scale_denominator();
    let (_, px, label) = scale_bar(den, 90.0);
    if !px.is_finite() || px <= 0.0 {
        return;
    }
    let y = rect.bottom() - 14.0;
    let x0 = rect.left() + 12.0;
    let x1 = x0 + px as f32;
    let color = Color32::from_rgba_unmultiplied(200, 200, 200, 230);
    let stroke = Stroke::new(2.0, color);
    painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
    painter.line_segment([egui::pos2(x0, y - 5.0), egui::pos2(x0, y + 5.0)], stroke);
    painter.line_segment([egui::pos2(x1, y - 5.0), egui::pos2(x1, y + 5.0)], stroke);
    painter.text(
        egui::pos2(x0 + (x1 - x0) / 2.0, y - 12.0),
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::monospace(11.0),
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let c = hex_to_color32("#FF8000CC");
        assert_eq!(c.to_srgba_unmultiplied(), [0xFF, 0x80, 0x00, 0xCC]);
        let hex = color32_to_hex(c);
        assert_eq!(hex, "#FF8000CC");
        // Entrada curta usa alfa padrão.
        let c2 = hex_to_color32("#FF8000");
        assert_eq!(c2.to_srgba_unmultiplied()[3], 0xCC);
        // Lixo cai em default razoável (sem panic).
        let _ = hex_to_color32("zzz");
    }

    #[test]
    fn compacto() {
        assert_eq!(format_compact(12_345_678.0), "12.3M");
        assert_eq!(format_compact(4567.0), "4.6k");
        assert_eq!(format_compact(42.0), "42");
        assert_eq!(format_compact(f64::NAN), "—");
    }

    #[test]
    fn icones_de_geometria_sao_distintos() {
        let all = [
            geom_icon(GeomType::Point),
            geom_icon(GeomType::MultiPoint),
            geom_icon(GeomType::LineString),
            geom_icon(GeomType::MultiLineString),
            geom_icon(GeomType::Polygon),
            geom_icon(GeomType::MultiPolygon),
            geom_icon(GeomType::GeometryCollection),
            geom_icon(GeomType::Raster),
        ];
        // Pares de mesma família compartilham glifo; famílias distintas não.
        assert_eq!(all[0], all[1]);
        assert_eq!(all[2], all[3]);
        assert_eq!(all[4], all[5]);
        assert_eq!(
            [all[0], all[2], all[4], all[6], all[7]]
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
    }
}
