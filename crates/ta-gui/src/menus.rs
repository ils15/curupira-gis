//! Barra de menus completa + toolbars agrupadas (v0.2 — paridade de layout
//! com a referência visual: grupos de ações separados por divisórias).
//!
//! Regras:
//! - **Persona filtra**: menus/painéis fora do preset da persona não são
//!   renderizados (ver [`crate::persona`]); Área de Trabalho, Mapa, Ajuda e
//!   Arquivo (mínimo) ficam sempre — sem elas o usuário perdia a saída ou a
//!   troca de persona.
//! - **Ações reais**: todo item chama a mesma ação do painel equivalente —
//!   nada decorativo.
//! - 100% dos rótulos via i18n; atalhos de teclado continuam no
//!   [`crate::app::TerraApp::handle_shortcuts`].

use eframe::egui;
use egui::{Align, Layout, ThemePreference};

use crate::app::TerraApp;
use crate::app_state::EditMode;
use crate::db_worker::DbCmd;
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

fn file_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuFile), |ui| {
        if app.state.flags.project_mgmt && ui.button(app.t(L::NewProject)).clicked() {
            app.dlg.new_project_name.clear();
            app.dlg.create_project = true;
            ui.close();
        }
        if ui.button(app.t(L::OpenProject)).clicked() {
            app.request_open_project();
            ui.close();
        }
        if app.state.flags.import_export {
            if ui.button(app.t(L::Import)).clicked() {
                app.show_import = true;
                ui.close();
            }
            // v0.2.1: navegador embutido (Ctrl+Shift+O).
            if ui.button(app.t(L::FileBrowserUseEmbedded)).clicked() {
                let seed = app.browser.cwd.clone();
                app.browser.open_at(seed);
                ui.close();
            }
            if ui.button(app.t(L::ExportActiveLayer)).clicked() {
                app.dlg.export = true;
                ui.close();
            }
            open_recent_menu(ui, app);
        }
        ui.separator();
        if ui.button(app.t(L::Quit)).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    });
}

/// "Abrir recente ▸": lista clicável; entrada que sumiu do disco é removida
/// na hora (on-click) — v0.2.1.
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
        let can_undo = !app.state.undo_stack.is_empty();
        if ui
            .add_enabled(can_undo, egui::Button::new(app.t(L::Undo)))
            .clicked()
        {
            app.state.undo();
            app.state.sync_map(&mut app.map);
            ui.close();
        }
        let can_redo = !app.state.redo_stack.is_empty();
        if ui
            .add_enabled(can_redo, egui::Button::new(app.t(L::Redo)))
            .clicked()
        {
            app.state.redo();
            app.state.sync_map(&mut app.map);
            ui.close();
        }
        ui.separator();
        let has_sel = app.state.selection.is_some();
        if ui
            .add_enabled(has_sel, egui::Button::new(app.t(L::DeleteFeatureTitle)))
            .clicked()
        {
            app.confirm_delete = true;
            ui.close();
        }
    });
}

fn workspace_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuWorkspace), |ui| {
        // Personas (marcador na corrente; trocar aplica o preset + persiste).
        for (persona, key) in [
            (Persona::Sig, L::PersonaSig),
            (Persona::Assistant, L::PersonaAssistant),
            (Persona::Operator, L::PersonaOperator),
            (Persona::Administrator, L::PersonaAdmin),
        ] {
            let current = app.state.persona == persona;
            if ui.selectable_label(current, app.t(key)).clicked() {
                app.apply_persona(persona);
                ui.close();
            }
        }
        ui.separator();
        if ui.button(app.t(L::ReopenWelcome)).clicked() {
            app.dlg.welcome = true;
            app.dlg.welcome_show_at_start = app.workbench.show_welcome;
            ui.close();
        }
        ui.separator();
        ui.menu_button(app.t(L::Theme), |ui| {
            if ui.button(app.t(L::ThemeDark)).clicked() {
                ui.ctx().set_theme(ThemePreference::Dark);
            }
            if ui.button(app.t(L::ThemeLight)).clicked() {
                ui.ctx().set_theme(ThemePreference::Light);
            }
        });
        ui.menu_button(app.t(L::Language), |ui| {
            for lang in [crate::i18n::Lang::PtBr, crate::i18n::Lang::En] {
                let current = app.state.i18n.lang == lang;
                if ui.selectable_label(current, lang.display_name()).clicked() {
                    app.set_language(lang);
                    ui.close();
                }
            }
        });
    });
}

fn map_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuMap), |ui| {
        if ui.button(app.t(L::ZoomIn)).clicked() {
            app.map.zoom_step(1.25);
            ui.close();
        }
        if ui.button(app.t(L::ZoomOut)).clicked() {
            app.map.zoom_step(0.8);
            ui.close();
        }
        if ui.button(app.t(L::ZoomToExtent)).clicked() {
            app.zoom_to_extent();
            ui.close();
        }
        ui.separator();
        // Overlays: toggles persistentes na sessão (estado em AppState).
        let mut grid = app.state.grid_on;
        if ui.checkbox(&mut grid, app.t(L::Grid)).changed() {
            app.state.grid_on = grid;
        }
        let mut bar = app.state.scale_bar_on;
        if ui.checkbox(&mut bar, app.t(L::ScaleBar)).changed() {
            app.state.scale_bar_on = bar;
        }
    });
}

fn tools_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuTools), |ui| {
        if ui.button(app.t(L::ConnectDb)).clicked() {
            app.show_connect = true;
            ui.close();
        }
        let connected = app.state.conn.is_db();
        if ui
            .add_enabled(connected, egui::Button::new(app.t(L::DisconnectDb)))
            .clicked()
        {
            app.state.conn.send(DbCmd::Disconnect);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(connected, egui::Button::new(app.t(L::SqlConsole)))
            .clicked()
        {
            app.dlg.sql = true;
            ui.close();
        }
        if ui.button(app.t(L::Options)).clicked() {
            app.dlg.options = true;
            ui.close();
        }
    });
}

fn project_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuProjectMgmt), |ui| {
        if ui.button(app.t(L::CreateProject)).clicked() {
            app.dlg.new_project_name.clear();
            app.dlg.create_project = true;
            ui.close();
        }
        let has_project = app.state.project.is_some() && app.state.conn.is_db();
        if ui
            .add_enabled(has_project, egui::Button::new(app.t(L::RenameProject)))
            .clicked()
        {
            app.dlg.rename_project_name = app
                .state
                .project
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            app.dlg.rename_project = true;
            ui.close();
        }
        if ui
            .add_enabled(has_project, egui::Button::new(app.t(L::DeleteProject)))
            .clicked()
        {
            app.dlg.delete_project = true;
            ui.close();
        }
        ui.separator();
        let has_active = app.state.active_or_first().is_some();
        let rename_ok = has_active
            && app
                .state
                .active_or_first()
                .is_some_and(|i| !app.state.layers[i].layer.id.is_empty())
            && app.state.conn.is_db();
        if ui
            .add_enabled(rename_ok, egui::Button::new(app.t(L::RenameLayer)))
            .clicked()
        {
            app.open_rename_layer();
            ui.close();
        }
        if ui
            .add_enabled(has_active, egui::Button::new(app.t(L::LayerRemove)))
            .clicked()
        {
            app.remove_active_layer();
            ui.close();
        }
    });
}

fn admin_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuAdmin), |ui| {
        let connected = app.state.conn.is_db();
        if ui
            .add_enabled(connected, egui::Button::new(app.t(L::RunMigrations)))
            .clicked()
        {
            app.state.conn.send(DbCmd::RunMigrations);
            ui.close();
        }
        if ui
            .add_enabled(connected, egui::Button::new(app.t(L::DbInfo)))
            .clicked()
        {
            app.state.conn.send(DbCmd::DbInfo);
            ui.close();
        }
    });
}

fn plugins_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuPlugins), |ui| {
        if ui.button(app.t(L::PluginsTitle)).clicked() {
            app.dlg.plugins = true;
            ui.close();
        }
    });
}

fn processing_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuProcessing), |ui| {
        let ready = app.state.conn.is_db() && app.state.project.is_some();
        for (kind, key) in [
            (crate::app::ProcKind::Buffer, L::ProcBuffer),
            (crate::app::ProcKind::Intersect, L::ProcIntersect),
            (crate::app::ProcKind::Union, L::ProcUnion),
        ] {
            if ui
                .add_enabled(ready, egui::Button::new(app.t(key)))
                .clicked()
            {
                app.open_processing(kind);
                ui.close();
            }
        }
        if !ready {
            ui.weak(app.t(L::ProcNeedsDb));
        }
    });
}

fn help_menu(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.menu_button(app.t(L::MenuHelp), |ui| {
        if ui.button(app.t(L::Shortcuts)).clicked() {
            app.dlg.shortcuts = true;
            ui.close();
        }
        if ui.button(app.t(L::About)).clicked() {
            app.show_about = true;
            ui.close();
        }
    });
}

// ---------------------------------------------------------------------------
// Toolbars — duas linhas densas com grupos separados (paridade visual).
// Botões pequenos com glifos unicode + tooltip i18n (a11y: nada só de cor).
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

fn toolbar_file_row(ui: &mut egui::Ui, app: &mut TerraApp) {
    if app.state.flags.project_mgmt
        && ui
            .small_button("🗎")
            .on_hover_text(app.t(L::NewProject))
            .clicked()
    {
        app.dlg.new_project_name.clear();
        app.dlg.create_project = true;
    }
    if ui
        .small_button("📂")
        .on_hover_text(app.t(L::OpenProject))
        .clicked()
    {
        app.request_open_project();
    }
    if app.state.flags.import_export {
        ui.separator();
        if ui
            .small_button("📥")
            .on_hover_text(app.t(L::Import))
            .clicked()
        {
            app.show_import = true;
        }
        // v0.2.1: navegador de arquivos embutido na toolbar.
        if ui
            .small_button("🗂")
            .on_hover_text(app.t(L::FileBrowserUseEmbedded))
            .clicked()
        {
            let seed = app.browser.cwd.clone();
            app.browser.open_at(seed);
        }
        if ui
            .small_button("📤")
            .on_hover_text(app.t(L::ExportActiveLayer))
            .clicked()
        {
            app.dlg.export = true;
        }
    }
    if app.state.flags.tools {
        ui.separator();
        if ui
            .small_button("🔌")
            .on_hover_text(app.t(L::ConnectDb))
            .clicked()
        {
            app.show_connect = true;
        }
        if ui
            .add_enabled(app.state.conn.is_db(), egui::Button::new("⏏").small())
            .on_hover_text(app.t(L::DisconnectDb))
            .clicked()
        {
            app.state.conn.send(DbCmd::Disconnect);
        }
    }
}

fn toolbar_map_row(ui: &mut egui::Ui, app: &mut TerraApp) {
    // Zoom
    if ui
        .small_button("＋")
        .on_hover_text(app.t(L::ZoomIn))
        .clicked()
    {
        app.map.zoom_step(1.25);
    }
    if ui
        .small_button("－")
        .on_hover_text(app.t(L::ZoomOut))
        .clicked()
    {
        app.map.zoom_step(0.8);
    }
    if ui
        .small_button("⤢")
        .on_hover_text(app.t(L::ZoomToExtent))
        .clicked()
    {
        app.zoom_to_extent();
    }
    // Overlays
    ui.separator();
    let mut grid = app.state.grid_on;
    if ui
        .toggle_value(&mut grid, "⌗")
        .on_hover_text(app.t(L::Grid))
        .changed()
    {
        app.state.grid_on = grid;
    }
    let mut bar = app.state.scale_bar_on;
    if ui
        .toggle_value(&mut bar, "▭")
        .on_hover_text(app.t(L::ScaleBar))
        .changed()
    {
        app.state.scale_bar_on = bar;
    }
    // Modos do mouse (persona com edição apenas).
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
    // Undo/redo de atributos (persona com edição).
    if app.state.flags.editing {
        ui.separator();
        let can_undo = !app.state.undo_stack.is_empty();
        if ui
            .add_enabled(can_undo, egui::Button::new("↶").small())
            .on_hover_text(app.t(L::Undo))
            .clicked()
        {
            app.state.undo();
            app.state.sync_map(&mut app.map);
        }
        let can_redo = !app.state.redo_stack.is_empty();
        if ui
            .add_enabled(can_redo, egui::Button::new("↷").small())
            .on_hover_text(app.t(L::Redo))
            .clicked()
        {
            app.state.redo();
            app.state.sync_map(&mut app.map);
        }
        let has_sel = app.state.selection.is_some();
        if ui
            .add_enabled(has_sel, egui::Button::new("🗑").small())
            .on_hover_text(app.t(L::DeleteFeatureTitle))
            .clicked()
        {
            app.confirm_delete = true;
        }
    }
    // Dica do modo ativo à direita (mesma linha, sem painel extra).
    if app.state.edit_mode == EditMode::NewPoint {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.weak(app.t(L::NewPointHint));
        });
    }
}
