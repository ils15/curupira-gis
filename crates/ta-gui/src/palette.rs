//! Paleta de comandos (Ctrl+K) — padrão de polimento de GIS sério
//! (RustGIS: "toda ação é um comando"; VS Code/QGIS: paleta com busca).
//!
//! Busca fuzzy (subsequência + ranking, [`crate::command_registry::search`]),
//! ↑/↓ navegam, Enter executa, Esc fecha. Ações desabilitadas aparecem
//! esmaecidas **com o motivo** (hover). 100% i18n.

use eframe::egui;
use egui::{Align, Key, Label, Layout, RichText, TextWrapMode};

use crate::app::TerraApp;
use crate::command_registry as reg;
use crate::i18n::Key as L;

/// Altura máxima da lista antes do scroll.
const MAX_VISIBLE: f32 = 380.0;

/// Modal da paleta (chamado uma vez por frame quando `dlg.palette`).
pub fn command_palette(ctx: &egui::Context, app: &mut TerraApp) {
    // Teclado global do modal (lido via contexto, não pelo widget).
    let (down, up, enter, esc) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowDown),
            i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Escape),
        )
    });
    let results = reg::search(app, &app.dlg.palette_query);
    let n = results.len();

    if down && n > 0 {
        app.dlg.palette_selected = (app.dlg.palette_selected + 1) % n;
    }
    if up && n > 0 {
        app.dlg.palette_selected = (app.dlg.palette_selected + n - 1) % n;
    }
    if esc {
        close(app);
        return;
    }

    // Seleção sempre dentro dos limites da busca corrente.
    let selected = app.dlg.palette_selected.min(n.saturating_sub(1));
    app.dlg.palette_selected = selected;

    // Enter executa a ação selecionada — se estiver habilitada.
    let mut run_id: Option<&'static str> = None;
    if enter && n > 0 {
        let (cmd, _) = results[selected];
        if cmd.enabled(app) {
            run_id = Some(cmd.id);
        }
    }

    let modal = egui::Modal::new(egui::Id::unique("ta_command_palette")).show(ctx, |ui| {
        ui.set_min_width(540.0);
        // Campo de busca com foco automático (a paleta NASCE digitando).
        let hint = app.t(L::PalettePlaceholder);
        let query = ui.add(
            egui::TextEdit::singleline(&mut app.dlg.palette_query)
                .hint_text(hint)
                .desired_width(ui.available_width()),
        );
        query.request_focus();
        if query.changed() {
            app.dlg.palette_selected = 0;
        }
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(MAX_VISIBLE)
            .show(ui, |ui| {
                if n == 0 {
                    ui.weak(app.t(L::PaletteEmpty));
                    return;
                }
                for (i, (cmd, _score)) in results.iter().enumerate() {
                    if command_row(ui, app, cmd, i == selected) {
                        run_id = Some(cmd.id);
                    }
                }
            });

        ui.separator();
        ui.weak(app.t(L::PaletteHint));
    });

    // Clique no backdrop também fecha (padrão dos demais modais).
    if let Some(id) = run_id {
        reg::run(app, ctx, id);
        close(app);
    } else if modal.should_close() {
        close(app);
    }
}

/// Uma linha da paleta. `true` = clique pedido na ação (sempre habilitada —
/// desabilitadas não recebem clique, só o hover com motivo).
fn command_row(
    ui: &mut egui::Ui,
    app: &TerraApp,
    cmd: &'static reg::Command,
    selected: bool,
) -> bool {
    let enabled = cmd.enabled(app);
    let mut label = RichText::new(app.t(cmd.label));
    if !enabled {
        label = label.weak();
    }
    let mut clicked = false;

    // Linha: ação à esquerda, categoria + atalho à direita (fonte de
    // verdade = registro — nada duplicado na UI).
    ui.horizontal(|ui| {
        let resp = ui
            .add_enabled(
                enabled,
                egui::Button::new(label)
                    .selected(selected)
                    .frame(false)
                    .wrap_mode(TextWrapMode::Truncate),
            )
            .on_hover_text(app.t(cmd.category.key()));
        if !enabled {
            // Motivo da desabilitação: hover na ação esmaecida (a11y — não
            // depende só da cor).
            if let Some(reason) = cmd.reason(app) {
                resp.clone().on_disabled_hover_text(app.t(reason));
            }
        }
        clicked = resp.clicked();

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(shortcut) = cmd.shortcut {
                ui.weak(RichText::new(shortcut).small());
            }
            let cat = RichText::new(app.t(cmd.category.key())).small();
            let cat = if enabled { cat.weak() } else { cat };
            ui.add(Label::new(cat).wrap_mode(TextWrapMode::Truncate));
        });
    });

    enabled && clicked
}

fn close(app: &mut TerraApp) {
    app.dlg.palette = false;
    app.dlg.palette_query.clear();
    app.dlg.palette_selected = 0;
}
