//! Diálogos de trabalho da v0.2: console SQL (somente-leitura), opções,
//! processamento PostGIS, info do banco, plugins (stub), propriedades de
//! camada, criar/renomear/deletar projeto, renomear camada, exportação e
//! atalhos. Todos modais com rótulos i18n e sem panic em path de UI.

use eframe::egui;

use crate::app::{ProcKind, TerraApp};
use crate::db_worker::DbCmd;
use crate::i18n::Key as L;
use crate::panels::geom_icon;

/// Despacha os diálogos de trabalho abertos (chamado uma vez por frame).
pub fn work_dialogs(ui: &mut egui::Ui, app: &mut TerraApp) {
    let ctx = ui.ctx().clone();
    if app.dlg.welcome {
        crate::welcome::welcome_dialog(&ctx, app);
    }
    if app.dlg.wizard && app.state.flags.wizard {
        crate::welcome::wizard_dialog(&ctx, app);
    }
    if app.dlg.options {
        options_dialog(&ctx, app);
    }
    if app.dlg.shortcuts {
        shortcuts_dialog(&ctx, app);
    }
    if app.dlg.plugins {
        plugins_dialog(&ctx, app);
    }
    if app.dlg.sql {
        sql_console(&ctx, app);
    }
    if app.dlg.db_info {
        db_info_dialog(&ctx, app);
    }
    if app.dlg.create_project {
        create_project_dialog(&ctx, app);
    }
    if app.dlg.rename_project {
        rename_project_dialog(&ctx, app);
    }
    if app.dlg.delete_project {
        delete_project_dialog(&ctx, app);
    }
    if let Some(idx) = app.dlg.rename_layer {
        rename_layer_dialog(&ctx, app, idx);
    }
    if let Some(idx) = app.dlg.layer_props {
        layer_props_dialog(&ctx, app, idx);
    }
    if app.dlg.export {
        export_dialog(&ctx, app);
    }
    if let Some(kind) = app.dlg.processing {
        processing_dialog(&ctx, app, kind);
    }
}

fn options_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_options")).show(ctx, |ui| {
        ui.heading(app.t(L::Options));
        ui.separator();
        // Persona (atalho da engrenagem — mesmo preset do menu).
        ui.label(app.t(L::MenuWorkspace));
        for (persona, key) in [
            (crate::persona::Persona::Sig, L::PersonaSig),
            (crate::persona::Persona::Assistant, L::PersonaAssistant),
            (crate::persona::Persona::Operator, L::PersonaOperator),
            (crate::persona::Persona::Administrator, L::PersonaAdmin),
        ] {
            let current = app.state.persona == persona;
            if ui.selectable_label(current, app.t(key)).clicked() {
                app.apply_persona(persona);
            }
        }
        ui.separator();
        // Idioma (persistido no workbench.json).
        ui.label(app.t(L::Language));
        ui.horizontal(|ui| {
            for lang in [crate::i18n::Lang::PtBr, crate::i18n::Lang::En] {
                let current = app.state.i18n.lang == lang;
                if ui.selectable_label(current, lang.display_name()).clicked() {
                    app.set_language(lang);
                }
            }
        });
        ui.separator();
        let mut show = app.workbench.show_welcome;
        if ui
            .checkbox(&mut show, app.t(L::WelcomeShowAtStart))
            .changed()
        {
            app.workbench.show_welcome = show;
            app.workbench_save();
        }
        ui.add_space(4.0);
        if ui.button(app.t(L::Close)).clicked() {
            app.dlg.options = false;
        }
    });
}

fn shortcuts_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_shortcuts")).show(ctx, |ui| {
        ui.heading(app.t(L::Shortcuts));
        ui.separator();
        ui.label(app.t(L::ShortcutsBody));
        ui.add_space(4.0);
        if ui.button(app.t(L::Close)).clicked() {
            app.dlg.shortcuts = false;
        }
    });
}

fn plugins_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_plugins")).show(ctx, |ui| {
        ui.heading(app.t(L::PluginsTitle));
        ui.separator();
        ui.weak(app.t(L::PluginsStub));
        ui.add_space(4.0);
        if ui.button(app.t(L::Close)).clicked() {
            app.dlg.plugins = false;
        }
    });
}

/// Console SQL somente-leitura: SELECT/WITH (guard na UI **e** no worker),
/// resultados como JSON-texto por linha (máx. 200 — ver `db_worker`).
fn sql_console(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_sql")).show(ctx, |ui| {
        ui.heading(app.t(L::SqlConsole));
        ui.weak(app.t(L::SqlReadOnlyHint));
        let response = ui.add(
            egui::TextEdit::multiline(&mut app.dlg.sql_text)
                .desired_width(480.0)
                .desired_rows(4)
                .code_editor(),
        );
        ui.horizontal(|ui| {
            let sql = app.dlg.sql_text.trim().to_owned();
            let ok = crate::db_worker::is_read_only_sql(&sql);
            if ui
                .add_enabled(ok, egui::Button::new(app.t(L::SqlRun)))
                .clicked()
            {
                app.dlg.sql_rows = None;
                app.state.conn.send(DbCmd::Query { sql });
            }
            if !ok && !app.dlg.sql_text.trim().is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(0xE7, 0x4C, 0x3C),
                    app.t(L::SqlOnlySelect),
                );
            }
        });
        if let Some(rows) = &app.dlg.sql_rows {
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    egui::Grid::new("ta_sql_rows").striped(true).show(ui, |ui| {
                        for row in rows {
                            for cell in row {
                                ui.monospace(cell);
                            }
                            ui.end_row();
                        }
                    });
                });
            ui.weak(format!(
                "{}: {} {}",
                app.t(L::SqlExecuted),
                rows.len(),
                app.t(L::SelectedRows)
            ));
        }
        ui.horizontal(|ui| {
            if ui.button(app.t(L::Close)).clicked() {
                app.dlg.sql = false;
            }
            let _ = &response; // foco: manter a caixa de texto viva no frame
        });
    });
}

fn db_info_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_dbinfo")).show(ctx, |ui| {
        ui.heading(app.t(L::DbInfo));
        ui.separator();
        match app.dlg.db_info_text.as_deref() {
            Some(text) => {
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.monospace(text);
                    });
            }
            None => {
                ui.label(app.t(L::Loading));
            }
        }
        ui.add_space(4.0);
        if ui.button(app.t(L::Close)).clicked() {
            app.dlg.db_info = false;
        }
    });
}

fn create_project_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_newproject")).show(ctx, |ui| {
        ui.heading(app.t(L::CreateProject));
        egui::Grid::new("ta_newproject_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::ProjectName));
                ui.add(
                    egui::TextEdit::singleline(&mut app.dlg.new_project_name).desired_width(240.0),
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let name = app.dlg.new_project_name.trim().to_owned();
            let can = !name.is_empty() && app.state.conn.is_db();
            if ui
                .add_enabled(can, egui::Button::new(app.t(L::CreateAction)))
                .clicked()
            {
                app.state.conn.send(DbCmd::CreateProject { name });
                app.dlg.create_project = false;
            }
            if !app.state.conn.is_db() {
                ui.weak(app.t(L::LocalModeHint));
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.create_project = false;
            }
        });
    });
}

fn rename_project_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_renproject")).show(ctx, |ui| {
        ui.heading(app.t(L::RenameProject));
        egui::Grid::new("ta_renproject_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::ProjectName));
                ui.add(
                    egui::TextEdit::singleline(&mut app.dlg.rename_project_name)
                        .desired_width(240.0),
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if let Some(project) = app.state.project.clone() {
                let name = app.dlg.rename_project_name.trim().to_owned();
                let changed = !name.is_empty() && name != project.name;
                if ui
                    .add_enabled(changed, egui::Button::new(app.t(L::RenameAction)))
                    .clicked()
                {
                    app.state.conn.send(DbCmd::RenameProject {
                        project_id: project.id,
                        new_name: name,
                    });
                    app.dlg.rename_project = false;
                }
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.rename_project = false;
            }
        });
    });
}

fn delete_project_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_delproject")).show(ctx, |ui| {
        ui.heading(app.t(L::DeleteProject));
        ui.label(app.t(L::DeleteProjectConfirmBody));
        ui.horizontal(|ui| {
            if let Some(project) = app.state.project.clone() {
                if ui.button(app.t(L::DeleteConfirm)).clicked() {
                    app.state.conn.send(DbCmd::DeleteProject {
                        project_id: project.id,
                    });
                    app.dlg.delete_project = false;
                }
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.delete_project = false;
            }
        });
    });
}

fn rename_layer_dialog(ctx: &egui::Context, app: &mut TerraApp, idx: usize) {
    egui::Modal::new(egui::Id::unique("ta_renlayer")).show(ctx, |ui| {
        ui.heading(app.t(L::RenameLayer));
        egui::Grid::new("ta_renlayer_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::PropName));
                ui.add(
                    egui::TextEdit::singleline(&mut app.dlg.rename_layer_name).desired_width(240.0),
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let name = app.dlg.rename_layer_name.trim().to_owned();
            let db_id = app
                .state
                .layers
                .get(idx)
                .map(|g| g.layer.id.clone())
                .unwrap_or_default();
            let changed = !name.is_empty() && !db_id.is_empty();
            if ui
                .add_enabled(changed, egui::Button::new(app.t(L::RenameAction)))
                .clicked()
            {
                app.state.conn.send(DbCmd::RenameLayer {
                    layer_id: db_id,
                    new_name: name,
                });
                app.dlg.rename_layer = None;
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.rename_layer = None;
            }
        });
    });
}

/// Propriedades da camada: nome, geometria, CRS, contagem e origem
/// (read-only — espelha o "Propriedades" do context menu).
fn layer_props_dialog(ctx: &egui::Context, app: &mut TerraApp, idx: usize) {
    egui::Modal::new(egui::Id::unique("ta_layerprops")).show(ctx, |ui| {
        ui.heading(app.t(L::LayerProperties));
        ui.separator();
        let Some(g) = app.state.layers.get(idx) else {
            ui.label(app.t(L::NoProject));
            return;
        };
        egui::Grid::new("ta_layerprops_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label(app.t(L::PropName));
                ui.label(g.layer.name.clone());
                ui.end_row();
                ui.label(app.t(L::PropGeom));
                let gt = g.layer.geom_type;
                ui.label(format!("{} {:?}", geom_icon(gt), gt));
                ui.end_row();
                ui.label(app.t(L::InspectorCrs));
                ui.label(format!("EPSG:{}", g.layer.srid));
                ui.end_row();
                ui.label(app.t(L::PropFeatures));
                ui.label(g.features.len().to_string());
                ui.end_row();
                ui.label(app.t(L::PropSource));
                ui.label(g.layer.source_uri.clone());
                ui.end_row();
            });
        ui.add_space(4.0);
        if ui.button(app.t(L::Close)).clicked() {
            app.dlg.layer_props = None;
        }
    });
}

fn export_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(egui::Id::unique("ta_export")).show(ctx, |ui| {
        ui.heading(app.t(L::ExportActiveLayer));
        ui.weak(app.t(L::ImportDialogPathHint));
        egui::Grid::new("ta_export_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::ImportDialogPath));
                ui.add(
                    egui::TextEdit::singleline(&mut app.dlg.export_path)
                        .desired_width(320.0)
                        .hint_text("/dados/saida.geojson"),
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let path = app.dlg.export_path.trim().to_owned();
            let has_features = app
                .state
                .active_or_first()
                .is_some_and(|i| !app.state.layers[i].features.is_empty());
            if ui
                .add_enabled(
                    !path.is_empty() && has_features,
                    egui::Button::new(app.t(L::Save)),
                )
                .clicked()
            {
                app.start_export(path);
                app.dlg.export = false;
            }
            if !has_features {
                ui.weak(app.t(L::AttrNoLayer));
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.export = false;
            }
        });
    });
}

/// Buffer / Interseção / União — parâmetros por operador, execução via
/// worker (PostGIS server-side; resultado vira camada derivada).
fn processing_dialog(ctx: &egui::Context, app: &mut TerraApp, kind: ProcKind) {
    egui::Modal::new(egui::Id::unique("ta_proc")).show(ctx, |ui| {
        let title = match kind {
            ProcKind::Buffer => L::ProcBuffer,
            ProcKind::Intersect => L::ProcIntersect,
            ProcKind::Union => L::ProcUnion,
        };
        ui.heading(app.t(title));
        let layers = app.layer_names();
        egui::Grid::new("ta_proc_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::ProcLayerA));
                egui::ComboBox::from_id_salt("ta_proc_a")
                    .selected_text(app.dlg.proc_source_a.clone().unwrap_or_else(|| "—".into()))
                    .show_ui(ui, |ui| {
                        for name in &layers {
                            if ui
                                .selectable_label(
                                    app.dlg.proc_source_a.as_deref() == Some(name.as_str()),
                                    name,
                                )
                                .clicked()
                            {
                                app.dlg.proc_source_a = Some(name.clone());
                            }
                        }
                    });
                ui.end_row();
                if kind == ProcKind::Intersect {
                    ui.label(app.t(L::ProcLayerB));
                    egui::ComboBox::from_id_salt("ta_proc_b")
                        .selected_text(app.dlg.proc_source_b.clone().unwrap_or_else(|| "—".into()))
                        .show_ui(ui, |ui| {
                            for name in &layers {
                                if ui
                                    .selectable_label(
                                        app.dlg.proc_source_b.as_deref() == Some(name.as_str()),
                                        name,
                                    )
                                    .clicked()
                                {
                                    app.dlg.proc_source_b = Some(name.clone());
                                }
                            }
                        });
                    ui.end_row();
                }
                if kind == ProcKind::Buffer {
                    ui.label(app.t(L::ProcDistance));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.dlg.proc_distance).desired_width(120.0),
                    );
                    ui.end_row();
                }
                ui.label(app.t(L::ProcTargetLayer));
                ui.add(egui::TextEdit::singleline(&mut app.dlg.proc_target).desired_width(200.0));
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let ready = app.dlg.proc_source_a.is_some()
                && (kind != ProcKind::Intersect || app.dlg.proc_source_b.is_some())
                && !app.dlg.proc_target.trim().is_empty()
                && (kind != ProcKind::Buffer || app.dlg.proc_distance.parse::<f64>().is_ok());
            if ui
                .add_enabled(ready, egui::Button::new(app.t(L::ProcRun)))
                .clicked()
            {
                app.run_processing(kind);
                app.dlg.processing = None;
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.dlg.processing = None;
            }
        });
    });
}
