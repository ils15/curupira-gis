//! Diálogos modais: conexão, import, projetos, confirmação de exclusão e
//! sobre. Todos com `egui::Modal` (backdrop bloqueia input — sem cliques
//! fantasmas) e 100% rótulos i18n. A senha não é ecoada nem logada.

use eframe::egui;
use egui::Id;

use crate::app::TerraApp;
use crate::db_worker::DbCmd;
use crate::i18n::Key as L;

/// Despacha os modais abertos (chamado uma vez por frame).
pub fn dialogs(ui: &mut egui::Ui, app: &mut TerraApp) {
    let ctx = ui.ctx().clone();
    if app.show_connect {
        connect_dialog(&ctx, app);
    }
    if app.show_import {
        import_dialog(&ctx, app);
    }
    // v0.2.1: navegador de arquivos embutido.
    if app.browser.open {
        crate::file_browser::file_browser_dialog(&ctx, app);
    }
    if app.show_projects {
        projects_dialog(&ctx, app);
    }
    if app.confirm_delete {
        delete_confirm(&ctx, app);
    }
    if app.show_about {
        about_dialog(&ctx, app);
    }
    // Progresso do import (não é modal bloqueante longo — mostra estado).
    let stage = app.import_job.as_ref().map(|j| j.stage.clone());
    if let Some(stage) = stage {
        egui::Modal::new(Id::unique("ta_import_progress")).show(&ctx, |ui| {
            ui.heading(app.t(L::ImportDialogTitle));
            ui.add(egui::ProgressBar::new(0.5).animate(true));
            ui.weak(stage);
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                if let Some(job) = app.import_job.as_mut() {
                    job.cancel = true;
                    let _ = job.cancel_tx.send(());
                }
            }
        });
    }
    // v0.2.2: paleta de comandos (Ctrl+K) + caixa de ferramentas (janela).
    if app.dlg.palette {
        crate::palette::command_palette(&ctx, app);
    }
    if app.dlg.toolbox {
        crate::toolbox::toolbox_window(&ctx, app);
    }
    // v0.2: bem-vindo, wizard, ferramentas, gerência de projeto,
    // processamento, export etc. (módulo próprio para conter o escopo).
    crate::work_dialogs::work_dialogs(ui, app);
}

fn connect_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(Id::unique("ta_connect")).show(ctx, |ui| {
        ui.heading(app.t(L::DbDialogTitle));
        let mut close = false;
        if let Some(env) = app.conn.env_url.clone() {
            ui.weak(app.t(L::DbDialogEnv));
            if ui.button(app.t(L::DbDialogUseEnv)).clicked() {
                app.conn.url = env;
            }
        }
        // URL direta (TA_DATABASE_URL colado) tem prioridade sobre campos.
        egui::Grid::new("ta_connect_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("URL");
                ui.add(
                    egui::TextEdit::singleline(&mut app.conn.url)
                        .hint_text("postgres://ta:***@host:5432/ta"),
                );
                ui.end_row();
                ui.label(app.t(L::DbDialogHost));
                ui.add(egui::TextEdit::singleline(&mut app.conn.host));
                ui.end_row();
                ui.label(app.t(L::DbDialogPort));
                ui.add(egui::TextEdit::singleline(&mut app.conn.port));
                ui.end_row();
                ui.label(app.t(L::DbDialogName));
                ui.add(egui::TextEdit::singleline(&mut app.conn.db));
                ui.end_row();
                ui.label(app.t(L::DbDialogUser));
                ui.add(egui::TextEdit::singleline(&mut app.conn.user));
                ui.end_row();
                ui.label(app.t(L::DbDialogPassword));
                ui.add(egui::TextEdit::singleline(&mut app.conn.pass).password(true));
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui.button(app.t(L::DbDialogConnect)).clicked() {
                app.connect_clicked();
                close = true;
            }
            if ui.button(app.t(L::DbDialogCancel)).clicked() {
                close = true;
            }
        });
        if close {
            app.show_connect = false;
        }
    });
}

fn import_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(Id::unique("ta_import")).show(ctx, |ui| {
        ui.heading(app.t(L::ImportDialogTitle));
        ui.weak(app.t(L::ImportDialogPathHint));
        egui::Grid::new("ta_import_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(app.t(L::ImportDialogPath));
                ui.add(
                    egui::TextEdit::singleline(&mut app.import_path)
                        .desired_width(360.0)
                        .hint_text("/dados/camada.shp"),
                );
                ui.end_row();
                ui.label(app.t(L::ImportDialogSrid));
                ui.add(egui::TextEdit::singleline(&mut app.import_srid));
                ui.end_row();
            });
        // v0.2.1: fila do drag & drop multi-arquivo (hint informativo).
        if !app.import_queue.is_empty() {
            ui.weak(
                app.t(L::FileBrowserQueueHint)
                    .replace("{}", &app.import_queue.len().to_string()),
            );
        }
        ui.horizontal(|ui| {
            if ui.button(app.t(L::ImportDialogStart)).clicked() {
                let path = app.import_path.trim().to_owned();
                if path.is_empty() {
                    app.error = Some(app.t(L::ImportDialogPathHint).to_owned());
                } else {
                    // v0.2.1: fluxo único consolidado (valida + sobe thread).
                    app.start_import_from_path(path);
                }
            }
            // v0.2.1: toggle "usar navegador embutido" a partir do diálogo.
            if ui.button(app.t(L::FileBrowserUseEmbedded)).clicked() {
                let seed = app.browser.cwd.clone();
                app.show_import = false;
                app.browser.open_at(seed);
            }
            if ui.button(app.t(L::DbDialogCancel)).clicked() {
                // Cancelar descarta a fila do drop multi-arquivo.
                app.cancel_import_dialog();
            }
        });
    });
}

fn projects_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(Id::unique("ta_projects")).show(ctx, |ui| {
        ui.heading(app.t(L::ProjectsTitle));
        let mut close = false;
        if app.projects_cache.is_empty() {
            ui.weak(app.t(L::ProjectsEmpty));
        } else {
            let projects = app.projects_cache.clone();
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for p in projects {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} (EPSG:{})", p.name, p.srid));
                            if ui.button(app.t(L::ProjectsOpen)).clicked() {
                                app.state.conn.send(DbCmd::OpenProject {
                                    project_id: p.id.clone(),
                                });
                                close = true;
                            }
                        });
                    }
                });
        }
        // Criar projeto direto do diálogo (nome curto).
        ui.horizontal(|ui| {
            if ui
                .button("+")
                .on_hover_text(app.t(L::ProjectsTitle))
                .clicked()
                && app.state.project.is_none()
            {
                app.state.conn.send(DbCmd::CreateProject {
                    name: format!("projeto-{}", chrono_like_now()),
                });
            }
            if ui.button(app.t(L::ProjectsClose)).clicked() {
                close = true;
            }
        });
        if close {
            app.show_projects = false;
        }
    });
}

fn delete_confirm(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(Id::unique("ta_delete")).show(ctx, |ui| {
        ui.heading(app.t(L::DeleteFeatureTitle));
        ui.label(app.t(L::DeleteFeatureBody));
        ui.horizontal(|ui| {
            if ui.button(app.t(L::DeleteConfirm)).clicked() {
                if let Some((l, f)) = app.state.selection {
                    let db_id = app.state.remove_local(l, f);
                    if let Some(db_id) = db_id.filter(|&id| id >= 0) {
                        app.state
                            .conn
                            .send(crate::db_worker::DbCmd::DeleteFeature { db_id });
                    }
                    app.state.sync_map(&mut app.map);
                }
                app.confirm_delete = false;
            }
            if ui.button(app.t(L::DeleteCancel)).clicked() {
                app.confirm_delete = false;
            }
        });
    });
}

fn about_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    egui::Modal::new(Id::unique("ta_about")).show(ctx, |ui| {
        ui.heading(app.t(L::AppTitle));
        ui.weak(app.t(L::AppSubtitle));
        ui.separator();
        ui.label(app.t(L::AboutBody));
        // Repo público (mesma URL em ambos os idiomas).
        ui.hyperlink_to(app.t(L::AboutRepo), "https://github.com/ils15/curupira-gis");
        ui.separator();
        if ui.button(app.t(L::Close)).clicked() {
            app.show_about = false;
        }
    });
}

/// Sufixo de nome para projeto criado no diálogo (segundos desde a época).
fn chrono_like_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
