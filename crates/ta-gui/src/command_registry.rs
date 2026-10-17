//! Registro de comandos — **toda ação da UI é um comando scriptável** aqui.
//!
//! Padrão adotado de GIS sério (referências: RustGIS, Apache-2.0 — "toda
//! ação é um comando"; QGIS — caixa de processamento com busca): menus,
//! toolbars, paleta (Ctrl+K) e caixa de ferramentas **consomem este
//! registro** — o comportamento mora num lugar só, sem duplicação.
//!
//! Como funciona:
//! - [`Command`] descreve a ação: id estável, rótulo i18n, categoria,
//!   atalho exibível, guardas (`guard` → motivo de desabilitar) e `run`.
//! - `run` recebe `(&mut TerraApp, &egui::Context)` — comandos que só
//!   mexem em estado ignoram o contexto.
//! - `guard` devolve `Some(chave_i18n)` = ação desabilitada **com motivo**
//!   (mostrado na paleta/tooltips); `None` = habilitada.
//! - [`fuzzy_score`] é o match da paleta: subsequência case-insensitive
//!   com ranking por posição (letras consecutivas e início cedo vencem).
//!   Implementação própria (~30 linhas) — **nenhuma dependência nova**.

use eframe::egui;

use crate::app::{ProcKind, TerraApp};
use crate::db_worker::DbCmd;
use crate::i18n::{Key as L, Lang};

/// Categoria de um comando — 1:1 com os menus (rótulos reusados do i18n).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    File,
    Edit,
    Workspace,
    Map,
    Tools,
    Project,
    Admin,
    Plugins,
    Processing,
    Help,
}

impl Category {
    /// Chave i18n do rótulo da categoria (reusa as chaves dos menus).
    pub fn key(self) -> L {
        match self {
            Category::File => L::MenuFile,
            Category::Edit => L::MenuEdit,
            Category::Workspace => L::MenuWorkspace,
            Category::Map => L::MenuMap,
            Category::Tools => L::MenuTools,
            Category::Project => L::MenuProjectMgmt,
            Category::Admin => L::MenuAdmin,
            Category::Plugins => L::MenuPlugins,
            Category::Processing => L::MenuProcessing,
            Category::Help => L::MenuHelp,
        }
    }
}

/// Guarda de habilitação: `None` = habilitado; `Some(chave)` = motivo.
pub type Guard = fn(&TerraApp) -> Option<L>;
/// Ação do comando (contexto disponível p/ tema/viewport — geralmente `_`).
pub type Run = fn(&mut TerraApp, &egui::Context);
/// Visibilidade (filtragem por persona — item some do menu/paleta).
pub type Visible = fn(&TerraApp) -> bool;
/// Estado de comandos-marca (toggles): valor atual para o ✓/realce.
pub type IsOn = fn(&TerraApp) -> bool;

/// Uma ação da UI, única e scriptável.
#[derive(Clone, Copy)]
pub struct Command {
    /// Id estável ("categoria.nome") — único no registro (teste garante).
    pub id: &'static str,
    pub label: L,
    pub category: Category,
    /// Atalho exibível ("Ctrl+K") — o disparo real vive em
    /// `TerraApp::handle_shortcuts` (egui não tem aceleradores nativos).
    pub shortcut: Option<&'static str>,
    /// Renderiza separador antes deste item nos menus.
    pub sep: bool,
    pub visible: Visible,
    pub guard: Guard,
    /// Toggle (grade, barra de escala, modos): estado atual p/ ✓.
    pub toggle: Option<IsOn>,
    pub run: Run,
}

impl Command {
    /// Habilitada? (`guard` devolve `None`.)
    pub fn enabled(&self, app: &TerraApp) -> bool {
        (self.guard)(app).is_none()
    }

    /// Motivo da desabilitação (`None` = habilitada) — tooltip/paleta.
    pub fn reason(&self, app: &TerraApp) -> Option<L> {
        (self.guard)(app)
    }

    /// Visível no menu/paleta da persona corrente?
    pub fn is_visible(&self, app: &TerraApp) -> bool {
        (self.visible)(app)
    }

    /// Estado de comandos-marca (toggles); `false` em ações simples.
    pub fn is_on(&self, app: &TerraApp) -> bool {
        self.toggle.map(|f| f(app)).unwrap_or(false)
    }
}

// -- guardas reutilizáveis --------------------------------------------------

fn ok(_: &TerraApp) -> Option<L> {
    None
}

fn always(_: &TerraApp) -> bool {
    true
}

fn needs_db(app: &TerraApp) -> Option<L> {
    (!app.state.conn.is_db()).then_some(L::DisabledNoDb)
}

fn needs_db_project(app: &TerraApp) -> Option<L> {
    needs_db(app).or_else(|| app.state.project.is_none().then_some(L::DisabledNoProject))
}

fn needs_selection(app: &TerraApp) -> Option<L> {
    app.state
        .selection
        .is_none()
        .then_some(L::DisabledNoSelection)
}

fn needs_undo(app: &TerraApp) -> Option<L> {
    app.state.undo_stack.is_empty().then_some(L::DisabledNoUndo)
}

fn needs_redo(app: &TerraApp) -> Option<L> {
    app.state.redo_stack.is_empty().then_some(L::DisabledNoRedo)
}

fn needs_layer(app: &TerraApp) -> Option<L> {
    app.state
        .active_or_first()
        .is_none()
        .then_some(L::DisabledNoLayer)
}

/// Camada ativa cadastrada no banco (renomear camada — fluxo antigo).
fn needs_db_layer(app: &TerraApp) -> Option<L> {
    needs_db(app).or_else(|| {
        let named = app
            .state
            .active_or_first()
            .and_then(|i| app.state.layers.get(i))
            .is_some_and(|g| !g.layer.id.is_empty());
        (!named).then_some(L::DisabledNoLayer)
    })
}

// -- filtros de persona (mesmos flags dos menus) ----------------------------

fn vis_editing(app: &TerraApp) -> bool {
    app.state.flags.editing
}
fn vis_import_export(app: &TerraApp) -> bool {
    app.state.flags.import_export
}
fn vis_project_mgmt(app: &TerraApp) -> bool {
    app.state.flags.project_mgmt
}
fn vis_tools(app: &TerraApp) -> bool {
    app.state.flags.tools
}
fn vis_admin(app: &TerraApp) -> bool {
    app.state.flags.admin
}
fn vis_plugins(app: &TerraApp) -> bool {
    app.state.flags.plugins
}
fn vis_processing(app: &TerraApp) -> bool {
    app.state.flags.processing
}

// -- toggles ----------------------------------------------------------------

fn on_grid(app: &TerraApp) -> bool {
    app.state.grid_on
}
fn on_scale_bar(app: &TerraApp) -> bool {
    app.state.scale_bar_on
}
fn on_navigate(app: &TerraApp) -> bool {
    app.state.edit_mode == crate::app_state::EditMode::Navigate
}
fn on_new_point(app: &TerraApp) -> bool {
    app.state.edit_mode == crate::app_state::EditMode::NewPoint
}

/// Todos os comandos — **a** fonte única das ações da UI (ordem = ordem de
/// menu/paleta). Novo recurso → nova entrada aqui e as chaves i18n.
static ALL: &[Command] = &[
    // ---- Arquivo ----------------------------------------------------------
    Command {
        id: "file.new_project",
        label: L::NewProject,
        category: Category::File,
        shortcut: None,
        sep: false,
        visible: vis_project_mgmt,
        guard: ok,
        toggle: None,
        run: |app, _| {
            app.dlg.new_project_name.clear();
            app.dlg.create_project = true;
        },
    },
    Command {
        id: "file.open_project",
        label: L::OpenProject,
        category: Category::File,
        shortcut: Some("Ctrl+O"),
        sep: false,
        visible: always,
        guard: needs_db,
        toggle: None,
        run: |app, _| app.request_open_project(),
    },
    Command {
        id: "file.import",
        label: L::Import,
        category: Category::File,
        shortcut: Some("Ctrl+I"),
        sep: false,
        visible: vis_import_export,
        guard: ok,
        toggle: None,
        run: |app, _| app.show_import = true,
    },
    Command {
        id: "file.browser",
        label: L::FileBrowserUseEmbedded,
        category: Category::File,
        shortcut: Some("Ctrl+Shift+O"),
        sep: false,
        visible: vis_import_export,
        guard: ok,
        toggle: None,
        run: |app, _| {
            let seed = app.browser.cwd.clone();
            app.browser.open_at(seed);
        },
    },
    Command {
        id: "file.export_layer",
        label: L::ExportActiveLayer,
        category: Category::File,
        shortcut: None,
        sep: false,
        visible: vis_import_export,
        guard: needs_layer,
        toggle: None,
        run: |app, _| app.dlg.export = true,
    },
    Command {
        id: "file.quit",
        label: L::Quit,
        category: Category::File,
        shortcut: None,
        sep: true,
        visible: always,
        guard: ok,
        toggle: None,
        run: |_, ctx| ctx.send_viewport_cmd(egui::ViewportCommand::Close),
    },
    // ---- Editar -----------------------------------------------------------
    Command {
        id: "edit.undo",
        label: L::Undo,
        category: Category::Edit,
        shortcut: Some("Ctrl+Z"),
        sep: false,
        visible: vis_editing,
        guard: needs_undo,
        toggle: None,
        run: |app, _| {
            app.state.undo();
            app.state.sync_map(&mut app.map);
        },
    },
    Command {
        id: "edit.redo",
        label: L::Redo,
        category: Category::Edit,
        shortcut: Some("Ctrl+Y"),
        sep: false,
        visible: vis_editing,
        guard: needs_redo,
        toggle: None,
        run: |app, _| {
            app.state.redo();
            app.state.sync_map(&mut app.map);
        },
    },
    Command {
        id: "edit.delete_feature",
        label: L::DeleteFeatureTitle,
        category: Category::Edit,
        shortcut: None,
        sep: true,
        visible: vis_editing,
        guard: needs_selection,
        toggle: None,
        run: |app, _| app.confirm_delete = true,
    },
    // ---- Área de Trabalho ---------------------------------------------------
    Command {
        id: "workspace.palette",
        label: L::MenuPalette,
        category: Category::Workspace,
        shortcut: Some("Ctrl+K"),
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.dlg.palette = true,
    },
    Command {
        id: "workspace.toolbox",
        label: L::ToolboxTitle,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: Some(|app| app.dlg.toolbox),
        run: |app, _| app.dlg.toolbox = !app.dlg.toolbox,
    },
    Command {
        id: "workspace.reopen_welcome",
        label: L::ReopenWelcome,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| {
            app.dlg.welcome = true;
            app.dlg.welcome_show_at_start = app.workbench.show_welcome;
        },
    },
    Command {
        id: "workspace.persona.sig",
        label: L::PersonaSig,
        category: Category::Workspace,
        shortcut: None,
        sep: true,
        visible: always,
        guard: ok,
        toggle: Some(|app| app.state.persona == crate::persona::Persona::Sig),
        run: |app, _| app.apply_persona(crate::persona::Persona::Sig),
    },
    Command {
        id: "workspace.persona.assistant",
        label: L::PersonaAssistant,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: Some(|app| app.state.persona == crate::persona::Persona::Assistant),
        run: |app, _| app.apply_persona(crate::persona::Persona::Assistant),
    },
    Command {
        id: "workspace.persona.operator",
        label: L::PersonaOperator,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: Some(|app| app.state.persona == crate::persona::Persona::Operator),
        run: |app, _| app.apply_persona(crate::persona::Persona::Operator),
    },
    Command {
        id: "workspace.persona.administrator",
        label: L::PersonaAdmin,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: Some(|app| app.state.persona == crate::persona::Persona::Administrator),
        run: |app, _| app.apply_persona(crate::persona::Persona::Administrator),
    },
    Command {
        id: "workspace.theme.dark",
        label: L::ThemeDark,
        category: Category::Workspace,
        shortcut: None,
        sep: true,
        visible: always,
        guard: ok,
        toggle: None,
        run: |_, ctx| ctx.set_theme(egui::ThemePreference::Dark),
    },
    Command {
        id: "workspace.theme.light",
        label: L::ThemeLight,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |_, ctx| ctx.set_theme(egui::ThemePreference::Light),
    },
    Command {
        id: "workspace.lang.pt_br",
        label: L::LangPtBr,
        category: Category::Workspace,
        shortcut: None,
        sep: true,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.set_language(Lang::PtBr),
    },
    Command {
        id: "workspace.lang.en",
        label: L::LangEn,
        category: Category::Workspace,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.set_language(Lang::En),
    },
    // ---- Mapa ---------------------------------------------------------------
    Command {
        id: "map.zoom_in",
        label: L::ZoomIn,
        category: Category::Map,
        shortcut: Some("+"),
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.map.zoom_step(1.25),
    },
    Command {
        id: "map.zoom_out",
        label: L::ZoomOut,
        category: Category::Map,
        shortcut: Some("−"),
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.map.zoom_step(0.8),
    },
    Command {
        id: "map.zoom_to_extent",
        label: L::ZoomToExtent,
        category: Category::Map,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.zoom_to_extent(),
    },
    Command {
        id: "map.toggle_grid",
        label: L::Grid,
        category: Category::Map,
        shortcut: None,
        sep: true,
        visible: always,
        guard: ok,
        toggle: Some(on_grid),
        run: |app, _| app.state.grid_on = !app.state.grid_on,
    },
    Command {
        id: "map.toggle_scale_bar",
        label: L::ScaleBar,
        category: Category::Map,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: Some(on_scale_bar),
        run: |app, _| app.state.scale_bar_on = !app.state.scale_bar_on,
    },
    Command {
        id: "map.mode_navigate",
        label: L::RailIdentify,
        category: Category::Map,
        shortcut: Some("Esc"),
        sep: true,
        visible: vis_editing,
        guard: ok,
        toggle: Some(on_navigate),
        run: |app, _| app.state.edit_mode = crate::app_state::EditMode::Navigate,
    },
    Command {
        id: "map.mode_new_point",
        label: L::RailNewPoint,
        category: Category::Map,
        shortcut: None,
        sep: false,
        visible: vis_editing,
        guard: ok,
        toggle: Some(on_new_point),
        run: |app, _| {
            app.state.edit_mode = if app.state.edit_mode == crate::app_state::EditMode::NewPoint {
                crate::app_state::EditMode::Navigate
            } else {
                crate::app_state::EditMode::NewPoint
            };
        },
    },
    // ---- Ferramentas ---------------------------------------------------------
    Command {
        id: "tools.connect_db",
        label: L::ConnectDb,
        category: Category::Tools,
        shortcut: None,
        sep: false,
        visible: vis_tools,
        guard: ok,
        toggle: None,
        run: |app, _| app.show_connect = true,
    },
    Command {
        id: "tools.disconnect_db",
        label: L::DisconnectDb,
        category: Category::Tools,
        shortcut: None,
        sep: false,
        visible: vis_tools,
        guard: needs_db,
        toggle: None,
        run: |app, _| {
            app.state.conn.send(DbCmd::Disconnect);
        },
    },
    Command {
        id: "tools.sql_console",
        label: L::SqlConsole,
        category: Category::Tools,
        shortcut: None,
        sep: true,
        visible: vis_tools,
        guard: needs_db,
        toggle: None,
        run: |app, _| app.dlg.sql = true,
    },
    Command {
        id: "tools.options",
        label: L::Options,
        category: Category::Tools,
        shortcut: None,
        sep: false,
        visible: vis_tools,
        guard: ok,
        toggle: None,
        run: |app, _| app.dlg.options = true,
    },
    // ---- Gerência de projeto --------------------------------------------------
    Command {
        id: "project.create",
        label: L::CreateProject,
        category: Category::Project,
        shortcut: None,
        sep: false,
        visible: vis_project_mgmt,
        guard: ok,
        toggle: None,
        run: |app, _| {
            app.dlg.new_project_name.clear();
            app.dlg.create_project = true;
        },
    },
    Command {
        id: "project.rename",
        label: L::RenameProject,
        category: Category::Project,
        shortcut: None,
        sep: false,
        visible: vis_project_mgmt,
        guard: needs_db_project,
        toggle: None,
        run: |app, _| {
            app.dlg.rename_project_name = app
                .state
                .project
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            app.dlg.rename_project = true;
        },
    },
    Command {
        id: "project.delete",
        label: L::DeleteProject,
        category: Category::Project,
        shortcut: None,
        sep: false,
        visible: vis_project_mgmt,
        guard: needs_db_project,
        toggle: None,
        run: |app, _| app.dlg.delete_project = true,
    },
    Command {
        id: "project.rename_layer",
        label: L::RenameLayer,
        category: Category::Project,
        shortcut: None,
        sep: true,
        visible: vis_project_mgmt,
        guard: needs_db_layer,
        toggle: None,
        run: |app, _| app.open_rename_layer(),
    },
    Command {
        id: "project.remove_layer",
        label: L::LayerRemove,
        category: Category::Project,
        shortcut: None,
        sep: false,
        visible: vis_project_mgmt,
        guard: needs_layer,
        toggle: None,
        run: |app, _| app.remove_active_layer(),
    },
    // ---- Administração --------------------------------------------------------
    Command {
        id: "admin.run_migrations",
        label: L::RunMigrations,
        category: Category::Admin,
        shortcut: None,
        sep: false,
        visible: vis_admin,
        guard: needs_db,
        toggle: None,
        run: |app, _| {
            app.state.conn.send(DbCmd::RunMigrations);
        },
    },
    Command {
        id: "admin.db_info",
        label: L::DbInfo,
        category: Category::Admin,
        shortcut: None,
        sep: false,
        visible: vis_admin,
        guard: needs_db,
        toggle: None,
        run: |app, _| {
            app.state.conn.send(DbCmd::DbInfo);
        },
    },
    // ---- Plugins ---------------------------------------------------------------
    Command {
        id: "plugins.manage",
        label: L::PluginsTitle,
        category: Category::Plugins,
        shortcut: None,
        sep: false,
        visible: vis_plugins,
        guard: ok,
        toggle: None,
        run: |app, _| app.dlg.plugins = true,
    },
    // ---- Processamento ----------------------------------------------------------
    Command {
        id: "processing.buffer",
        label: L::ProcBuffer,
        category: Category::Processing,
        shortcut: None,
        sep: false,
        visible: vis_processing,
        guard: needs_db_project,
        toggle: None,
        run: |app, _| app.open_processing(ProcKind::Buffer),
    },
    Command {
        id: "processing.intersect",
        label: L::ProcIntersect,
        category: Category::Processing,
        shortcut: None,
        sep: false,
        visible: vis_processing,
        guard: needs_db_project,
        toggle: None,
        run: |app, _| app.open_processing(ProcKind::Intersect),
    },
    Command {
        id: "processing.union",
        label: L::ProcUnion,
        category: Category::Processing,
        shortcut: None,
        sep: false,
        visible: vis_processing,
        guard: needs_db_project,
        toggle: None,
        run: |app, _| app.open_processing(ProcKind::Union),
    },
    // ---- Ajuda -------------------------------------------------------------------
    Command {
        id: "help.shortcuts",
        label: L::Shortcuts,
        category: Category::Help,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.dlg.shortcuts = true,
    },
    Command {
        id: "help.about",
        label: L::About,
        category: Category::Help,
        shortcut: None,
        sep: false,
        visible: always,
        guard: ok,
        toggle: None,
        run: |app, _| app.show_about = true,
    },
];

/// Todos os comandos na ordem de declaração (menus/paleta seguem esta ordem).
pub fn all() -> &'static [Command] {
    ALL
}

/// Busca por id estável (scriptabilidade/toolbox).
pub fn find(id: &str) -> Option<&'static Command> {
    ALL.iter().find(|c| c.id == id)
}

/// Executa um comando por id (`false` = id desconhecido — nunca panic).
pub fn run(app: &mut TerraApp, ctx: &egui::Context, id: &str) -> bool {
    match find(id) {
        Some(cmd) => {
            (cmd.run)(app, ctx);
            true
        }
        None => false,
    }
}

/// Comandos visíveis de uma categoria (ordem do registro).
pub fn of_category(cat: Category) -> impl Iterator<Item = &'static Command> {
    ALL.iter().filter(move |c| c.category == cat)
}

/// Penalidade de casar só na categoria (match fraco vence do label, mas
/// perde para label direto).
const CATEGORY_PENALTY: usize = 4;

/// Match fuzzy: subsequência case-insensitive com custo.
///
/// `None` = não casa. `Some(custo)` — **menor é melhor**: letras em
/// posições consecutivas custam 0, buracos custam o tamanho do buraco,
/// começar tarde custa a posição inicial. Query vazia casa tudo (custo 0).
pub fn fuzzy_score(query: &str, target: &str) -> Option<usize> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = target.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let mut score = 0usize;
    let mut prev: Option<usize> = None;
    let mut from = 0usize;
    for &qc in &q {
        let pos = t.iter().skip(from).position(|&tc| tc == qc)? + from;
        score += match prev {
            None => pos,
            Some(p) if pos == p + 1 => 0,
            Some(p) => pos - p - 1,
        };
        prev = Some(pos);
        from = pos + 1;
    }
    Some(score)
}

/// Busca da paleta: ranqueia comandos **visíveis** por `fuzzy_score` sobre
/// o rótulo (no idioma corrente) e a categoria (com penalidade).
/// Empate → ordem do registro (estável). Vazia → tudo, em ordem.
pub fn search(app: &TerraApp, query: &str) -> Vec<(&'static Command, usize)> {
    let mut out: Vec<(&'static Command, usize)> = ALL
        .iter()
        .filter(|c| (c.visible)(app))
        .filter_map(|c| {
            let label = fuzzy_score(query, app.t(c.label));
            let cat = fuzzy_score(query, app.t(c.category.key())).map(|s| s + CATEGORY_PENALTY);
            match (label, cat) {
                (Some(a), Some(b)) => Some((c, a.min(b))),
                (Some(a), None) => Some((c, a)),
                (None, Some(b)) => Some((c, b)),
                (None, None) => None,
            }
        })
        .collect();
    out.sort_by_key(|(_, score)| *score);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::Persona;

    fn app() -> TerraApp {
        let mut app = TerraApp::new();
        app.state.apply_persona(Persona::Sig);
        app.dlg.welcome = false;
        app
    }

    #[test]
    fn ids_sao_unicos() {
        let mut seen = std::collections::HashSet::new();
        for cmd in all() {
            assert!(seen.insert(cmd.id), "id duplicado: {}", cmd.id);
        }
        assert!(all().len() >= 40, "registro deveria nascer populado");
    }

    #[test]
    fn todo_comando_tem_categoria_e_run_executa_sem_panic() {
        // Smoke: cada comando roda headless (Local, sem projeto) sem panic.
        let mut app = app();
        app.workbench_path = None; // não polui config do host
        let ctx = egui::Context::default();
        for cmd in all() {
            assert!(run(&mut app, &ctx, cmd.id), "{} deve executar", cmd.id);
        }
        assert!(!run(&mut app, &ctx, "id.inexistente"));
    }

    #[test]
    fn fuzzy_subsequencia_e_ranking_por_posicao() {
        // Não-casamento → None.
        assert_eq!(fuzzy_score("xyz", "Buffer…"), None);
        assert_eq!(fuzzy_score("proj", "Sair"), None);

        // Consecutivo e cedo vence: "buf" em "buffer" (custo 0) bate
        // "bfr" (buracos) e a mesma palavra tardia.
        let consec = fuzzy_score("buf", "buffer").unwrap();
        let holes = fuzzy_score("bfr", "buffer").unwrap();
        let late = fuzzy_score("buf", "aa buffer").unwrap();
        assert!(consec < holes, "{consec} < {holes}");
        assert!(consec < late, "{consec} < {late}");

        // Case-insensitive.
        assert_eq!(fuzzy_score("BUF", "Buffer…"), Some(0));
        // Vazia casa tudo.
        assert_eq!(fuzzy_score("", "qualquer"), Some(0));
    }

    #[test]
    fn busca_da_paleta_ranqueia_e_filtra_visibilidade() {
        let app = app();
        let hits = search(&app, "proj");
        assert!(
            hits.iter().any(|(c, _)| c.id == "file.open_project"),
            "abrir projeto deve casar com 'proj'"
        );
        assert!(
            !hits.iter().any(|(c, _)| c.id == "file.quit"),
            "'proj' não deve casar 'Sair'"
        );
        // Ranking determinístico: custos não-descrescentes.
        for w in hits.windows(2) {
            assert!(w[0].1 <= w[1].1);
        }
    }

    #[test]
    fn guardas_e_visibilidade_por_persona() {
        // SIG, modo local: processamento desabilita com motivo; zoom livre.
        let sig = app();
        let buffer = find("processing.buffer").unwrap();
        assert_eq!(buffer.reason(&sig), Some(L::DisabledNoDb));
        assert!(!buffer.enabled(&sig));
        assert_eq!(find("map.zoom_in").unwrap().reason(&sig), None);

        // Importar: some p/ Administrador (persona sem import/export).
        let mut admin = sig;
        admin.state.apply_persona(Persona::Administrator);
        assert!(!find("file.import").unwrap().is_visible(&admin));
        assert!(!find("processing.buffer").unwrap().is_visible(&admin));
        assert!(find("admin.db_info").unwrap().is_visible(&admin));

        // Operador: sem gerência de projeto nem administração.
        let mut op = app();
        op.state.apply_persona(Persona::Operator);
        assert!(!find("project.create").unwrap().is_visible(&op));
        assert!(!find("admin.run_migrations").unwrap().is_visible(&op));
        assert!(find("file.import").unwrap().is_visible(&op));

        // Paleta esconde comandos invisíveis da persona.
        assert!(
            !search(&admin, "import")
                .iter()
                .any(|(c, _)| c.id == "file.import")
        );

        // Undo sem edições: desabilitado com motivo; após uma edição, livre.
        let fresh = app();
        assert_eq!(
            find("edit.undo").unwrap().reason(&fresh),
            Some(L::DisabledNoUndo)
        );
        let mut edited = app();
        edited.state.undo_stack.push(crate::app_state::AttrsEdit {
            layer_idx: 0,
            feat_idx: 0,
            old: serde_json::json!({}),
            new: serde_json::json!({}),
        });
        assert_eq!(find("edit.undo").unwrap().reason(&edited), None);
    }

    #[test]
    fn toggles_refletem_o_estado() {
        let mut app = app();
        let grid = find("map.toggle_grid").unwrap();
        assert!(!grid.is_on(&app));
        (grid.run)(&mut app, &egui::Context::default());
        assert!(app.state.grid_on);
        assert!(grid.is_on(&app));

        let nav = find("map.mode_navigate").unwrap();
        assert!(nav.is_on(&app));
        let pt = find("map.mode_new_point").unwrap();
        (pt.run)(&mut app, &egui::Context::default());
        assert!(!nav.is_on(&app));
        assert!(pt.is_on(&app));
    }
}
