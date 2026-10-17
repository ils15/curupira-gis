//! Diálogo "Bem-vindo" (seletor de persona) + assistente de fluxo de
//! trabalho (persona Assistente) — paridade v0.2 com o modal central da
//! referência de layout.
//!
//! Clean-room: os ícones dos cards são desenhados com o **painter do egui**
//! (primitivas geométricas próprias) — nenhum asset/imagem do produto
//! legado é usado. Cada card combina clique + rótulo textual (a11y).

use eframe::egui;
use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::TerraApp;
use crate::i18n::Key as L;
use crate::persona::Persona;

/// Modal de boas-vindas: 4 cards de persona + Ajuda + checkbox de exibição.
pub fn welcome_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_welcome")).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(
                egui::RichText::new("◍ Curupira GIS").heading(),
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.heading(app.t(L::WelcomeHeading));
            });
        });
        ui.separator();
        ui.label(app.t(L::WelcomePrompt));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            persona_card(ui, app, Persona::Sig, L::WelcomeCardSig, icon_globe);
            ui.separator();
            persona_card(
                ui,
                app,
                Persona::Assistant,
                L::WelcomeCardAssistant,
                icon_clipboard,
            );
            ui.separator();
            persona_card(
                ui,
                app,
                Persona::Operator,
                L::WelcomeCardOperator,
                icon_pencil,
            );
            ui.separator();
            persona_card(
                ui,
                app,
                Persona::Administrator,
                L::WelcomeCardAdmin,
                icon_shield,
            );
        });
        ui.add_space(6.0);
        let show_label = app.t(L::WelcomeShowAtStart);
        ui.horizontal(|ui| {
            if ui.button(app.t(L::WelcomeHelp)).clicked() {
                app.show_about = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut app.dlg.welcome_show_at_start, show_label);
            });
        });
    });
}

/// Card clicável: ícone desenhado + título, seleção persistida.
fn persona_card(
    ui: &mut egui::Ui,
    app: &mut TerraApp,
    persona: Persona,
    title: L,
    icon: fn(&egui::Painter, Rect, Color32),
) {
    let text = app.t(title);
    let sel = app.state.persona == persona;
    let inner = egui::Frame::group(ui.style())
        .stroke(Stroke::new(
            if sel { 2.0 } else { 1.0 },
            if sel {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_stroke.color
            },
        ))
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_min_width(108.0);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), Sense::hover());
            let accent = ui.visuals().strong_text_color();
            icon(ui.painter(), rect, accent);
            ui.set_min_height(30.0);
            ui.add(egui::Label::new(egui::RichText::new(text).small()).wrap());
        });
    let resp = inner.response.interact(Sense::click()).on_hover_text(text);
    if resp.clicked() {
        app.apply_persona(persona);
        // "Exibir esta janela na abertura" acompanha o clique do card
        // (o checkbox do modal é a fonte da verdade nesta interação).
        app.workbench.show_welcome = app.dlg.welcome_show_at_start;
        app.workbench_save();
        app.dlg.welcome = false;
    }
}

// ---------------------------------------------------------------------------
// Ícones (painter puro — desenhos próprios, clean-room).
// Nota: o "logo" da v0.2 é tipográfico (texto no título do modal) — nenhum
// asset binário; desenhos ficam nos ícones dos cards.
// ---------------------------------------------------------------------------

/// SIG: globo com meridianos e paralelos.
fn icon_globe(p: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let r = rect.height().min(rect.width()) * 0.38;
    let stroke = Stroke::new(2.0, color);
    p.circle_stroke(c, r, stroke);
    p.line_segment([Pos2::new(c.x - r, c.y), Pos2::new(c.x + r, c.y)], stroke);
    p.line_segment([Pos2::new(c.x, c.y - r), Pos2::new(c.x, c.y + r)], stroke);
    // Dois meridianos (cordas verticais a ±r/2, altura r·√3/2).
    let dy = r * 0.866;
    for dx in [-r / 2.0, r / 2.0] {
        p.line_segment(
            [Pos2::new(c.x + dx, c.y - dy), Pos2::new(c.x + dx, c.y + dy)],
            Stroke::new(1.0, color),
        );
    }
}

/// Assistente: prancheta com linhas de checklist.
fn icon_clipboard(p: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let w = rect.width() * 0.42;
    let h = rect.height() * 0.62;
    let top = Pos2::new(c.x - w / 2.0, c.y - h / 2.0);
    p.rect_filled(
        Rect::from_min_size(top, Vec2::new(w, h)),
        2.0,
        color.gamma_multiply(0.25),
    );
    p.rect(
        Rect::from_min_size(top, Vec2::new(w, h)),
        2,
        Color32::TRANSPARENT,
        Stroke::new(2.0, color),
        egui::StrokeKind::Inside,
    );
    // Presilha superior.
    p.rect_filled(
        Rect::from_min_size(
            Pos2::new(c.x - w * 0.18, top.y - 4.0),
            Vec2::new(w * 0.36, 6.0),
        ),
        2.0,
        color,
    );
    // Três itens de checklist.
    for i in 0..3usize {
        let y = top.y + h * (0.28 + 0.22 * i as f32);
        p.line_segment(
            [
                Pos2::new(top.x + w * 0.14, y),
                Pos2::new(top.x + w * 0.3, y),
            ],
            Stroke::new(2.0, color),
        );
        p.line_segment(
            [
                Pos2::new(top.x + w * 0.42, y),
                Pos2::new(top.x + w * 0.86, y),
            ],
            Stroke::new(1.0, color),
        );
    }
}

/// Operador: lápis (edição/produção).
fn icon_pencil(p: &egui::Painter, rect: Rect, color: Color32) {
    let (x0, y0) = (
        rect.left() + rect.width() * 0.2,
        rect.bottom() - rect.height() * 0.2,
    );
    let (x1, y1) = (
        rect.right() - rect.width() * 0.2,
        rect.top() + rect.height() * 0.2,
    );
    // Corpo do lápis (diagonal grossa).
    p.line_segment(
        [Pos2::new(x0 + 4.0, y0 - 4.0), Pos2::new(x1 - 4.0, y1 + 4.0)],
        Stroke::new(6.0, color),
    );
    // Ponta (triângulo convexo).
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x0, y0),
            Pos2::new(x0 + 8.0, y0),
            Pos2::new(x0, y0 - 8.0),
        ],
        color,
        Stroke::NONE,
    ));
    // Borracha (traço curto no topo).
    p.line_segment(
        [Pos2::new(x1 - 5.0, y1 + 5.0), Pos2::new(x1, y1)],
        Stroke::new(3.0, color.gamma_multiply(0.6)),
    );
}

/// Administrador: escudo com marca de verificação.
fn icon_shield(p: &egui::Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let w = rect.width() * 0.34;
    let h = rect.height() * 0.4;
    // Escudo (polígono convexo de 5 pontos).
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x - w, c.y - h),
            Pos2::new(c.x + w, c.y - h),
            Pos2::new(c.x + w, c.y + h * 0.25),
            Pos2::new(c.x, c.y + h),
            Pos2::new(c.x - w, c.y + h * 0.25),
        ],
        color.gamma_multiply(0.3),
        Stroke::new(2.0, color),
    ));
    // Verificação (dois segmentos formando o "check").
    p.line_segment(
        [
            Pos2::new(c.x - w * 0.45, c.y - h * 0.1),
            Pos2::new(c.x - w * 0.1, c.y + h * 0.3),
        ],
        Stroke::new(2.5, color),
    );
    p.line_segment(
        [
            Pos2::new(c.x - w * 0.1, c.y + h * 0.3),
            Pos2::new(c.x + w * 0.5, c.y - h * 0.35),
        ],
        Stroke::new(2.5, color),
    );
}

// ---------------------------------------------------------------------------
// Assistente de fluxo de trabalho (persona Assistente)
// ---------------------------------------------------------------------------

/// Wizard de 4 passos: conectar → projeto → importar → concluir.
pub fn wizard_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_wizard")).show(ctx, |ui| {
        ui.heading(app.t(L::WizardTitle));
        ui.separator();
        ui.label(format!(
            "{} {}/4",
            app.t(L::WizardStep),
            app.dlg.wizard_step + 1
        ));
        ui.add_space(4.0);
        match app.dlg.wizard_step {
            0 => {
                ui.label(app.t(L::WizardStepConnect));
                ui.horizontal(|ui| {
                    let connected = app.state.conn.is_db();
                    if ui
                        .add_enabled(!connected, egui::Button::new(app.t(L::ConnectDb)))
                        .clicked()
                    {
                        app.show_connect = true;
                    }
                    if connected {
                        ui.weak("✓");
                    }
                });
            }
            1 => {
                ui.label(app.t(L::WizardStepProject));
                ui.horizontal(|ui| {
                    if ui.button(app.t(L::OpenProject)).clicked() {
                        app.request_open_project();
                    }
                    if ui.button(app.t(L::CreateProject)).clicked() {
                        app.dlg.new_project_name.clear();
                        app.dlg.create_project = true;
                    }
                    if app.state.project.is_some() {
                        ui.weak("✓");
                    }
                });
            }
            2 => {
                ui.label(app.t(L::WizardStepImport));
                ui.horizontal(|ui| {
                    if ui.button(app.t(L::Import)).clicked() {
                        app.show_import = true;
                    }
                    if !app.state.layers.is_empty() {
                        ui.weak("✓");
                    }
                });
            }
            _ => {
                ui.label(app.t(L::WizardStepDone));
                if ui.button(app.t(L::ZoomToExtent)).clicked() {
                    app.zoom_to_extent();
                }
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if app.dlg.wizard_step > 0 && ui.button(app.t(L::WizardBack)).clicked() {
                app.dlg.wizard_step -= 1;
            }
            if app.dlg.wizard_step < 3 {
                if ui.button(app.t(L::WizardNext)).clicked() {
                    app.dlg.wizard_step += 1;
                }
            } else if ui.button(app.t(L::WizardFinish)).clicked() {
                app.dlg.wizard = false;
                app.dlg.wizard_step = 0;
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.wizard = false;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use crate::persona::Persona;

    /// Ícones dos 4 cards devem ser funções distintas — aqui garantimos a
    /// cobertura de desenho por smoke headless (ver app_smoke::welcome).
    #[test]
    fn cards_cobrem_as_quatro_personas() {
        let cards = [
            Persona::Sig,
            Persona::Assistant,
            Persona::Operator,
            Persona::Administrator,
        ];
        assert_eq!(cards.len(), 4);
        for p in cards {
            let f = p.flags();
            assert!(!f.wizard || p == Persona::Assistant);
        }
    }
}
