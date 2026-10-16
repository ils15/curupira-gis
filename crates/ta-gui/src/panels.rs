//! Painéis principais: menu, dock de camadas, inspetor, canvas e status bar.
//!
//! Acessibilidade: todos os controles têm rótulo textual (i18n), a ordem de
//! tabulação é a ordem de criação (padrão egui/accesskit) e nenhum controle
//! depende só de cor (ícones têm texto). Contraste: cores de texto/quadro
//! vêm do tema egui (dark/light verificados nos dois).

use eframe::egui;
use egui::{Align, Color32, Label, Layout, Sense, ThemePreference};

use crate::app::TerraApp;
use crate::app_state::EditMode;
use crate::i18n::Key as L;
use crate::map_view::{MapView, RenderCtx};

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

/// Menu superior.
pub fn top_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::top("ta_menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(app.t(L::MenuFile), |ui| {
                if ui.button(app.t(L::ConnectDb)).clicked() {
                    app.show_connect = true;
                    ui.close();
                }
                if ui.button(app.t(L::OpenProject)).clicked() {
                    app.request_open_project();
                    ui.close();
                }
                if ui.button(app.t(L::Import)).clicked() {
                    app.show_import = true;
                    ui.close();
                }
                ui.separator();
                if ui.button(app.t(L::Quit)).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(app.t(L::MenuView), |ui| {
                ui.menu_button(app.t(L::Theme), |ui| {
                    if ui.button(app.t(L::ThemeDark)).clicked() {
                        ui.ctx().set_theme(ThemePreference::Dark);
                    }
                    if ui.button(app.t(L::ThemeLight)).clicked() {
                        ui.ctx().set_theme(ThemePreference::Light);
                    }
                });
                ui.menu_button(app.t(L::Language), |ui| {
                    if ui.button("Português (BR)").clicked() {
                        app.state.i18n.lang = crate::i18n::Lang::PtBr;
                    }
                    if ui.button("English").clicked() {
                        app.state.i18n.lang = crate::i18n::Lang::En;
                    }
                });
            });
            ui.menu_button(app.t(L::MenuLayer), |ui| {
                if ui.button(app.t(L::ZoomToExtent)).clicked() {
                    app.zoom_to_extent();
                    ui.close();
                }
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
                    ui.close();
                }
            });
            ui.menu_button(app.t(L::MenuHelp), |ui| {
                if ui.button(app.t(L::About)).clicked() {
                    app.show_about = true;
                    ui.close();
                }
            });
        });
    });
}

/// Dock esquerdo: árvore de camadas com visibilidade/ordem/ativa.
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
                    let (name, visible, is_active, n_feats) = {
                        let g = &app.state.layers[idx];
                        (
                            g.layer.name.clone(),
                            g.visible,
                            app.state.active_or_first() == Some(idx),
                            g.features.len(),
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
                        let sel = ui.selectable_label(is_active, name).clicked();
                        if sel {
                            app.state.active_layer = Some(idx);
                        }
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
                            app.state.layers.remove(idx);
                            app.state.active_layer = None;
                            app.state.selection = None;
                            app.map.set_raster(None);
                            app.state.sync_map(&mut app.map);
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
            // Estilo da camada ativa (cor/espessura/opacidade).
            let active = app.state.active_or_first();
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
            });
        });
}

/// Canvas central: toolbar + mapa.
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
        // Toolbar do mapa.
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
            if app.state.edit_mode == EditMode::NewPoint {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(Label::new(app.t(L::NewPointHint)).wrap());
                });
            }
        });
        ui.separator();

        // Canvas do mapa (contrato MapView — o app não desenha nada direto).
        let rect = ui.available_rect_before_wrap();
        let mut rctx = RenderCtx {
            egui: ui.ctx(),
            origin_px: (rect.left(), rect.top()),
            viewport_px: (rect.width(), rect.height()),
        };
        app.map.render(&mut rctx);
        // Garante que o painel reserve a área (evita layout oscilante).
        ui.allocate_exact_size(rect.size(), Sense::hover());
    });
}

/// Barra de status: cursor em EPSG:4674, escala, SRID e modo.
pub fn status_bar(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::bottom("ta_status").show(ui, |ui| {
        ui.horizontal(|ui| {
            let (cx, cy) = app.map.cursor_world().unwrap_or((f64::NAN, f64::NAN));
            let coords = if cx.is_nan() {
                "—".to_owned()
            } else {
                format!("lat {cy:.6}, lon {cx:.6}")
            };
            ui.label(format!(
                "{}: {coords} (EPSG:4674)",
                app.t(L::StatusBarCoords)
            ));
            ui.separator();
            let den = app.map.viewport().scale_denominator();
            ui.label(format!(
                "{}: 1:{}",
                app.t(L::StatusBarScale),
                format_compact(den)
            ));
            ui.separator();
            let srid = app.state.project.as_ref().map_or(4674, |p| p.srid);
            ui.label(format!("{}: {srid}", app.t(L::StatusBarSrid)));
            ui.separator();
            let mode = if app.state.conn.is_db() {
                app.t(L::StatusBarModeDb)
            } else {
                app.t(L::StatusBarModeLocal)
            };
            ui.label(mode);
            if let Some(info) = app.info.clone() {
                ui.separator();
                ui.weak(info);
            }
        });
    });
}

/// Formata números grandes de forma compacta (1:12M etc.).
fn format_compact(v: f64) -> String {
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
    }
}
