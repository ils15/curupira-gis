//! Barra de menus completa + toolbars agrupadas (v0.2 — paridade de layout
//! com a referência visual: grupos de ações separados por divisórias).
//!
//! Regras:
//! - **Persona filtra**: menus/painéis fora do preset da persona não são
//!   renderizados (ver [`crate::persona`]); Área de Trabalho, Mapa, Ajuda e
//!   Arquivo (mínimo) ficam sempre — sem elas o usuário perdia a saída ou a
//!   troca de persona.
//! - **Ações reais, fonte única**: menus e toolbars **não implementam
//!   comportamento** — consomem o registro de comandos
//!   ([`crate::command_registry`], padrão RustGIS "toda ação é um comando").
//!   Guardas de habilitação (com motivo no tooltip) e atalhos exibidos vêm
//!   do mesmo lugar que a paleta Ctrl+K e a caixa de ferramentas.
//! - 100% dos rótulos via i18n; disparo real de atalhos continua no
//!   [`crate::app::TerraApp::handle_shortcuts`].

use eframe::egui;
use egui::{Align, Layout};

use crate::app::TerraApp;
use crate::app_state::EditMode;
use crate::command_registry as reg;
use crate::command_registry::Command;
use crate::i18n::Key as L;
use crate::persona::Persona;

/// Barra de menus (TopPanel próprio, acima das toolbars).
pub fn menu_bar(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::top("ta_menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            file_menu(ui, app);
            if app.state.flags.editing {
                edit_menu(ui, app);
            }
            workspace_menu(ui, app);
            map_menu(ui, app);
            if app.state.flags.tools {
                tools_menu(ui, app);
            }
            if app.state.flags.project_mgmt {
                project_menu(ui, app);
            }
            if app.state.flags.admin {
                admin_menu(ui, app);
            }
            if app.state.flags.plugins {
                plugins_menu(ui, app);
            }
            if app.state.flags.processing {
                processing_menu(ui, app);
            }
            help_menu(ui, app);
        });
    });
}

// ---------------------------------------------------------------------------
// Renderização a partir do registro — menus não sabem COMO executar nada.
// ---------------------------------------------------------------------------

/// Renderiza os comandos de uma categoria (ordem do registro).
fn menu_commands(ui: &mut egui::Ui, app: &mut TerraApp, cat: reg::Category) {
    menu_commands_if(ui, app, |c| c.category == cat);
}

/// Variante com predicado (menu Arquivo divide em torno de "Abrir recente";
/// Área de Trabalho renderiza persona/tema/idioma como submenus).
fn menu_commands_if(ui: &mut egui::Ui, app: &mut TerraApp, keep: impl Fn(&Command) -> bool) {
    for cmd in reg::all().iter().filter(|c| keep(c)) {
        if !cmd.is_visible(app) {
            continue;
        }
        if cmd.sep {
            ui.separator();
        }
        let label = app.t(cmd.label);
        match cmd.toggle {
            // Comando-marca (grade, barra de escala, modos): checkbox com
            // estado real; o run alterna.
            Some(is_on) => {
                let mut on = is_on(app);
                if ui.checkbox(&mut on, label).changed() {
                    (cmd.run)(app, ui.ctx());
                }
            }
            None => {
                let mut resp = ui.add_enabled(cmd.enabled(app), egui::Button::new(label));
                // Motivo da desabilitação no hover (a11y — não é só cinza).
                if let Some(reason) = cmd.reason(app) {
                    resp = resp.on_disabled_hover_text(app.t(reason));
                }
                if resp.clicked() {
                    (cmd.run)(app, ui.ctx());
                    ui.close();
                }
            }
        }
    }
}

fn file_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuFile), |ui| {
        menu_commands_if(ui, app, |c| {
            c.category == reg::Category::File && c.id != "file.quit"
        });
        if app.state.flags.import_export {
            open_recent_menu(ui, app);
        }
        menu_commands_if(ui, app, |c| c.id == "file.quit");
    });
}

/// "Abrir recente ▸": lista clicável; entrada que sumiu do disco é removida
/// na hora (on-click) — v0.2.1. (Submenu bespoke: itens dinâmicos do disco;
/// a ação de importar continua a única do app.)
fn open_recent_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::FileBrowserOpenRecent), |ui| {
        let recents = app.workbench.recent_files.clone();
        if recents.is_empty() {
            ui.weak(app.t(L::FileBrowserRecentEmpty));
            return;
        }
        for item in recents {
            let exists = std::path::Path::new(&item.path).exists();
            let icon = crate::file_browser::EntryKind::from_name(
                &std::path::Path::new(&item.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                false,
            )
            .icon();
            let label = format!("{icon} {}", item.path);
            if ui.button(label).clicked() {
                ui.close();
                if exists {
                    app.start_import_from_path(item.path);
                } else {
                    app.workbench.remove_recent_file(&item.path);
                    app.workbench_save();
                }
            }
        }
    });
}

fn edit_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuEdit), |ui| {
        menu_commands(ui, app, reg::Category::Edit);
    });
}

fn workspace_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuWorkspace), |ui| {
        // v0.2.2: paleta + caixa de ferramentas + reabrir bem-vindo (o resto
        // da categoria — persona/tema/idioma — vira submenu com marcador).
        menu_commands_if(ui, app, |c| {
            c.category == reg::Category::Workspace
                && !c.id.contains("persona.")
                && !c.id.contains("theme.")
                && !c.id.contains("lang.")
        });
        ui.separator();
        // Personas (marcador na corrente; ação única = comando do registro).
        for (persona, key, id) in [
            (Persona::Sig, L::PersonaSig, "workspace.persona.sig"),
            (
                Persona::Assistant,
                L::PersonaAssistant,
                "workspace.persona.assistant",
            ),
            (
                Persona::Operator,
                L::PersonaOperator,
                "workspace.persona.operator",
            ),
            (
                Persona::Administrator,
                L::PersonaAdmin,
                "workspace.persona.administrator",
            ),
        ] {
            let current = app.state.persona == persona;
            if ui.selectable_label(current, app.t(key)).clicked() {
                reg::run(app, ui.ctx(), id);
                ui.close();
            }
        }
        ui.separator();
        // Tema (ação única = comando do registro).
        ui.menu_button(app.t(L::Theme), |ui| {
            for (key, id) in [
                (L::ThemeDark, "workspace.theme.dark"),
                (L::ThemeLight, "workspace.theme.light"),
            ] {
                if ui.button(app.t(key)).clicked() {
                    reg::run(app, ui.ctx(), id);
                    ui.close();
                }
            }
        });
        // Idioma (marcador na corrente; ação única = comando do registro).
        ui.menu_button(app.t(L::Language), |ui| {
            for (lang, id) in [
                (crate::i18n::Lang::PtBr, "workspace.lang.pt_br"),
                (crate::i18n::Lang::En, "workspace.lang.en"),
            ] {
                let current = app.state.i18n.lang == lang;
                if ui.selectable_label(current, lang.display_name()).clicked() {
                    reg::run(app, ui.ctx(), id);
                    ui.close();
                }
            }
        });
    });
}

fn map_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuMap), |ui| {
        menu_commands(ui, app, reg::Category::Map);
    });
}

fn tools_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuTools), |ui| {
        menu_commands(ui, app, reg::Category::Tools);
    });
}

fn project_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuProjectMgmt), |ui| {
        menu_commands(ui, app, reg::Category::Project);
    });
}

fn admin_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuAdmin), |ui| {
        menu_commands(ui, app, reg::Category::Admin);
    });
}

fn plugins_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuPlugins), |ui| {
        menu_commands(ui, app, reg::Category::Plugins);
    });
}

fn processing_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuProcessing), |ui| {
        menu_commands(ui, app, reg::Category::Processing);
        let ready = app.state.conn.is_db() && app.state.project.is_some();
        if !ready {
            ui.weak(app.t(L::ProcNeedsDb));
        }
    });
}

fn help_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuHelp), |ui| {
        menu_commands(ui, app, reg::Category::Help);
    });
}

// ---------------------------------------------------------------------------
// Toolbars — duas linhas densas com grupos separados (paridade visual).
// Botões pequenos com glifos unicode + tooltip i18n (com atalho quando
// existente); toggles com estado hover/ativo consistentes (v0.2.2).
// ---------------------------------------------------------------------------

/// Painel de toolbars (uma única TopPanel com duas linhas).
pub fn toolbars(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::top("ta_toolbars").show(ui, |ui| {
        ui.horizontal(|ui| {
            toolbar_file_row(ui, app);
        });
        ui.separator();
        ui.horizontal(|ui| {
            toolbar_map_row(ui, app);
        });
    });
}

/// Botão de toolbar acionando um comando do registro: visibilidade por
/// persona, guarda (desabilitado **com motivo** no hover) e tooltip i18n.
fn reg_toolbar(ui: &mut egui::Ui, app: &mut TerraApp, id: &str, glyph: &str, tip: L) {
    let Some(cmd) = reg::find(id) else {
        return;
    };
    if !cmd.is_visible(app) {
        return;
    }
    let mut resp = ui.add_enabled(cmd.enabled(app), egui::Button::new(glyph).small());
    if let Some(reason) = cmd.reason(app) {
        resp = resp.on_disabled_hover_text(app.t(reason));
    }
    if resp.on_hover_text(app.t(tip)).clicked() {
        (cmd.run)(app, ui.ctx());
    }
}

/// Toggle de toolbar (grade, barra, modos) com visual único: `toggle_value`
/// dá o mesmo estado hover/ativo para todos — v0.2.2 (antes era misto de
/// toggle/selectable). O run alterna o estado; o visual lê do registro.
fn reg_toolbar_toggle(ui: &mut egui::Ui, app: &mut TerraApp, id: &str, glyph: &str, tip: L) {
    let Some(cmd) = reg::find(id) else {
        return;
    };
    if !cmd.is_visible(app) {
        return;
    }
    let mut visual = cmd.toggle.map(|f| f(app)).unwrap_or(false);
    if ui
        .toggle_value(&mut visual, glyph)
        .on_hover_text(app.t(tip))
        .clicked()
    {
        (cmd.run)(app, ui.ctx());
    }
}

fn toolbar_file_row(ui: &mut egui::Ui, app: &mut TerraApp) {
    reg_toolbar(ui, app, "file.new_project", "🗎", L::NewProject);
    reg_toolbar(ui, app, "file.open_project", "📂", L::HintOpenProject);
    if app.state.flags.import_export {
        ui.separator();
        reg_toolbar(ui, app, "file.import", "📥", L::HintImport);
        reg_toolbar(ui, app, "file.browser", "🗂", L::FileBrowserUseEmbedded);
        reg_toolbar(ui, app, "file.export_layer", "📤", L::ExportActiveLayer);
    }
    if app.state.flags.tools {
        ui.separator();
        reg_toolbar(ui, app, "tools.connect_db", "🔌", L::ConnectDb);
        reg_toolbar(ui, app, "tools.disconnect_db", "⏏", L::DisconnectDb);
    }
}

fn toolbar_map_row(ui: &mut egui::Ui, app: &mut TerraApp) {
    // Zoom
    reg_toolbar(ui, app, "map.zoom_in", "＋", L::ZoomIn);
    reg_toolbar(ui, app, "map.zoom_out", "－", L::ZoomOut);
    reg_toolbar(ui, app, "map.zoom_to_extent", "⤢", L::ZoomToExtent);
    // Overlays (toggles consistentes com os modos do mouse)
    ui.separator();
    reg_toolbar_toggle(ui, app, "map.toggle_grid", "⌗", L::Grid);
    reg_toolbar_toggle(ui, app, "map.toggle_scale_bar", "▭", L::ScaleBar);
    // Modos do mouse (persona com edição apenas).
    if app.state.flags.editing {
        ui.separator();
        reg_toolbar_toggle(ui, app, "map.mode_navigate", "✛", L::RailIdentify);
        reg_toolbar_toggle(ui, app, "map.mode_new_point", "📍", L::RailNewPoint);
        ui.separator();
        reg_toolbar(ui, app, "edit.undo", "↶", L::Undo);
        reg_toolbar(ui, app, "edit.redo", "↷", L::Redo);
        reg_toolbar(ui, app, "edit.delete_feature", "🗑", L::DeleteFeatureTitle);
    }
    // Dica do modo ativo à direita (mesma linha, sem painel extra).
    if app.state.edit_mode == EditMode::NewPoint {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.weak(app.t(L::NewPointHint));
        });
    }
}
