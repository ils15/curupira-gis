//! Barra de status completa (v0.2 — paridade com a referência):
//! `Linhas selecionadas: N` · chip SRS (verde com projeto / vermelho sem) ·
//! coordenadas do cursor · dropdown **Scale** (presets 1:N) · modo ·
//! engrenagem (opções).
//!
//! Todos os valores são **binds reais** ao estado (seleção/projeto/viewport)
//! — nada decorativo. O chip SRS distingue por TEXTO (a11y: nunca só cor).

use eframe::egui;
use egui::Color32;

use crate::app::TerraApp;
use crate::i18n::Key as L;
use crate::overlays::{SCALE_PRESETS, px_per_unit_for_denominator};

/// Painel de status (BottomPanel).
pub fn status_bar(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::bottom("ta_status").show(ui, |ui| {
        ui.horizontal(|ui| {
            // Linhas selecionadas (bind real à seleção).
            ui.label(format!(
                "{}: {}",
                app.t(L::SelectedRows),
                app.state.selection_count()
            ));
            ui.separator();

            // Chip SRS: ponto colorido + TEXTO (verde com projeto ativo,
            // vermelho sem projeto — "Unknown SRS" da referência).
            let (dot, text) = match app.state.project.as_ref() {
                Some(p) => (
                    Color32::from_rgb(0x4C, 0xAF, 0x50),
                    format!("EPSG:{}", p.srid),
                ),
                None => (
                    Color32::from_rgb(0xE7, 0x4C, 0x3C),
                    app.t(L::UnknownSrs).to_owned(),
                ),
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, dot);
            ui.label(text);
            ui.separator();

            // Coordenadas do cursor (EPSG:4674).
            let (cx, cy) = app.map.cursor_world().unwrap_or((f64::NAN, f64::NAN));
            let coords = if cx.is_nan() {
                "—".to_owned()
            } else {
                format!("lat {cy:.6}, lon {cx:.6}")
            };
            ui.label(format!("{}: {coords}", app.t(L::StatusBarCoords)));
            ui.separator();

            // Dropdown Scale: mostra o 1:N atual; presets aplicam a escala.
            scale_combo(ui, app);
            ui.separator();

            // Modo LOCAL/BANCO + infos transitórias.
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

            // Engrenagem (opções) ancorada à direita.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("⚙")
                    .on_hover_text(app.t(L::Options))
                    .clicked()
                {
                    app.dlg.options = true;
                }
            });
        });
    });
}

/// ComboBox da escala: seleção atual + presets cartográficos.
fn scale_combo(ui: &mut egui::Ui, app: &mut TerraApp) {
    let den = app.map.viewport().scale_denominator();
    let current = format!("Scale 1:{}", crate::panels::format_compact(den));
    egui::ComboBox::from_id_salt("ta_scale_preset")
        .selected_text(current)
        .width(130.0)
        .show_ui(ui, |ui| {
            // Entrada "livre" = escala atual (nenhum preset selecionado).
            let sel_cur = true;
            if ui
                .selectable_label(sel_cur, format!("1:{}", crate::panels::format_compact(den)))
                .clicked()
            {
                let ppu = px_per_unit_for_denominator(den);
                app.map.viewport_mut().set_px_per_unit(ppu);
            }
            ui.separator();
            for &preset in SCALE_PRESETS {
                let sel = (den - preset).abs() / preset < 0.02;
                if ui
                    .selectable_label(sel, format!("1:{}", crate::panels::format_compact(preset)))
                    .clicked()
                {
                    let ppu = px_per_unit_for_denominator(preset);
                    app.map.viewport_mut().set_px_per_unit(ppu);
                }
            }
        })
        .response
        .on_hover_text(app.t(L::ScalePresetHint));
}

#[cfg(test)]
mod tests {
    use crate::overlays::px_per_unit_for_denominator;

    #[test]
    fn preset_aplica_escala_coerente() {
        // Simula o fluxo do combo: escolhe 1:100 000 e confere o denominador.
        let den = 100_000.0;
        let ppu = px_per_unit_for_denominator(den);
        let mut vp = crate::map_view::MapViewEgui::new();
        vp.viewport_mut().set_px_per_unit(ppu);
        let got = vp.viewport().scale_denominator();
        assert!(
            (got - den).abs() / den < 1e-6,
            "denominador voltou {got}, esperado {den}"
        );
    }
}
