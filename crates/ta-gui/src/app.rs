//! Shell do aplicativo: menus, painéis, diálogos, atalhos e bombas de
//! eventos (worker de banco + thread de import).
//!
//! Regras da Fase 3 respeitadas aqui:
//! - **UI nunca bloqueia**: I/O de banco e de arquivo vive em threads
//!   (`db_worker`, [`import_job`]); a UI drena canais por frame.
//! - **Sem unwrap em path de UI**: tudo que pode falhar vira mensagem no
//!   banner de erro ([`TerraApp::error`]).
//! - **100% dos rótulos via `i18n`** ([`crate::i18n`]).
//! - Atalhos: Ctrl+O (abrir projeto), Ctrl+I (importar), +/− (zoom),
//!   Esc (cancela modo de edição), Ctrl+Z/Ctrl+Y (undo/redo de attrs).

use eframe::egui;
use egui::{Key, Modifiers};

use ta_core::feature::Feature;
use ta_core::layer::GeomType;
use ta_gis::raster::RasterMeta;

use crate::app_state::{AppState, Conn, EditMode};
use crate::attr_table::attribute_table;
use crate::db_worker::{DbCmd, DbEvt, DbWorker};
use crate::dialogs::dialogs;
use crate::i18n::Key as L;
use crate::import_job::{ImportJob, ImportMsg, PendingImport, spawn_import};
use crate::map_view::MapView;
use crate::panels;
use crate::raster_view;
use crate::workbench::WorkbenchConfig;

/// Operador de processamento disponível no diálogo (menu Processamento).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
    Buffer,
    Intersect,
    Union,
}

/// Estado dos diálogos/buffers da v0.2 (bem-vindo, wizard, ferramentas,
/// gerência de projeto, processamento, export). Agrupado para não inflar
/// [`TerraApp`] com 20 campos soltos.
#[derive(Debug, Default)]
pub struct UiDialogs {
    /// Janela "Bem-vindo" (aberta na 1ª execução e pelo menu).
    pub welcome: bool,
    /// "Exibir esta janela na abertura" (estado do checkbox do modal).
    pub welcome_show_at_start: bool,
    /// Assistente de fluxo de trabalho (persona Assistente).
    pub wizard: bool,
    pub wizard_step: usize,
    pub options: bool,
    pub shortcuts: bool,
    /// Plugins (stub informativo).
    pub plugins: bool,
    /// Console SQL (somente-leitura).
    pub sql: bool,
    pub sql_text: String,
    pub sql_rows: Option<Vec<Vec<String>>>,
    /// Informações do banco (preenchido por evento do worker).
    pub db_info: bool,
    pub db_info_text: Option<String>,
    // Gerência de projeto.
    pub create_project: bool,
    pub new_project_name: String,
    pub rename_project: bool,
    pub rename_project_name: String,
    pub delete_project: bool,
    // Camadas.
    pub rename_layer: Option<usize>,
    pub rename_layer_name: String,
    pub layer_props: Option<usize>,
    // Export (GeoJSON da camada ativa).
    pub export: bool,
    pub export_path: String,
    // Processamento.
    pub processing: Option<ProcKind>,
    pub proc_source_a: Option<String>,
    pub proc_source_b: Option<String>,
    pub proc_distance: String,
    pub proc_target: String,
    // ---- v0.2.2 (registro de comandos) ---------------------------------
    /// Paleta de comandos (Ctrl+K): aberta?
    pub palette: bool,
    /// Query corrente da paleta (busca fuzzy).
    pub palette_query: String,
    /// Índice selecionado na paleta (↑/↓/Enter).
    pub palette_selected: usize,
    /// Caixa de ferramentas de processamento (janela flutuante).
    pub toolbox: bool,
    /// Query corrente da busca da caixa de ferramentas.
    pub toolbox_query: String,
}

/// Aplicação principal. Mantém o `MapView` (contrato migrável de backend).
pub struct TerraApp {
    pub state: AppState,
    pub map: crate::map_view::MapViewEgui,
    /// Último erro (banner com fechar — nunca panic).
    pub error: Option<String>,
    /// Informação transitória (import concluído, conexão etc.).
    pub info: Option<String>,
    pub show_connect: bool,
    pub show_import: bool,
    pub show_projects: bool,
    pub confirm_delete: bool,
    pub show_about: bool,
    /// Campos do diálogo de conexão (senha nunca é logada nem impressa).
    pub conn: ConnFields,
    pub import_path: String,
    pub import_srid: String,
    pub import_job: Option<ImportJob>,
    /// Carregamento de banda raster em thread de fundo (UI nunca bloqueia
    /// em GDAL — ver [`crate::raster_view::spawn_band_load`]).
    pub band_job: Option<raster_view::BandLoadJob>,
    /// Cache em memória das bandas já carregadas: (arquivo, banda) → imagem.
    /// Invalidado ao trocar de arquivo (bandas do arquivo antigo não voltam).
    pub band_cache:
        std::collections::HashMap<(std::path::PathBuf, usize), raster_view::RasterBandImage>,
    /// Import vetorial aguardando cadastro de camada/inserção no banco.
    pub pending_import: Option<PendingImport>,
    pub projects_cache: Vec<ta_core::project::Project>,
    /// Buffers de edição da tabela atributiva: (camada, feição, campo) → texto.
    pub attr_buffers: std::collections::HashMap<(usize, usize, String), String>,
    // ---- v0.2 (paridade de UI) -------------------------------------
    /// Estado dos diálogos novos (agrupado — ver [`UiDialogs`]).
    pub dlg: UiDialogs,
    /// Config persistida da área de trabalho (`~/.config/curupira/…`).
    pub workbench: WorkbenchConfig,
    /// Caminho da config resolvido UMA vez (None = sem persistência).
    pub workbench_path: Option<std::path::PathBuf>,
    /// Export GeoJSON em background (a UI nunca bloqueia em I/O).
    pub export_job: Option<crate::export_job::ExportJob>,
    // ---- v0.2.1 (navegação de arquivos) ----------------------------
    /// Navegador de arquivos embutido (janela/modal + estado).
    pub browser: crate::file_browser::BrowserState,
    /// Fila de import do drag & drop multi-arquivo (1º pré-preenche o
    /// modal; o resto entra aqui e é consumido em sequência).
    pub import_queue: Vec<std::path::PathBuf>,
    /// Consome a fila automaticamente ao fim de cada import (ligado ao
    /// confirmar o import de um drop; desligado ao cancelar o diálogo).
    pub import_queue_auto: bool,
    // ---- v0.2.2 (registro de comandos) -------------------------------
    /// Último título de janela enviado (evita `ViewportCommand` por frame).
    pub last_title: Option<String>,
}

/// Campos do formulário de conexão.
#[derive(Debug, Default)]
pub struct ConnFields {
    /// TA_DATABASE_URL detectado no ambiente (pré-preenche o campo URL).
    pub env_url: Option<String>,
    pub url: String,
    pub host: String,
    pub port: String,
    pub db: String,
    pub user: String,
    pub pass: String,
}

impl Default for TerraApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TerraApp {
    pub fn new() -> Self {
        let env_url = std::env::var("TA_DATABASE_URL")
            .ok()
            .filter(|s| !s.is_empty());
        // Workbench persistido (persona/idioma/bem-vindo). Ausente ou
        // corrompido → defaults (SIG + bem-vindo na abertura).
        let workbench = crate::workbench::load();
        let mut state = AppState::new();
        state.apply_persona(workbench.persona());
        state.i18n.lang = workbench.lang();
        let show_welcome = workbench.show_welcome;
        Self {
            state,
            map: crate::map_view::MapViewEgui::new(),
            error: None,
            info: None,
            show_connect: false,
            show_import: false,
            show_projects: false,
            confirm_delete: false,
            show_about: false,
            conn: ConnFields {
                env_url: env_url.clone(),
                url: env_url.unwrap_or_default(),
                host: "localhost".into(),
                port: "5432".into(),
                db: "ta".into(),
                user: "ta".into(),
                pass: String::new(),
            },
            import_path: String::new(),
            import_srid: "4674".into(),
            import_job: None,
            band_job: None,
            band_cache: std::collections::HashMap::new(),
            pending_import: None,
            projects_cache: Vec::new(),
            attr_buffers: std::collections::HashMap::new(),
            dlg: UiDialogs {
                welcome: show_welcome,
                welcome_show_at_start: show_welcome,
                ..UiDialogs::default()
            },
            workbench,
            workbench_path: crate::workbench::config_file(),
            export_job: None,
            browser: crate::file_browser::BrowserState::default(),
            import_queue: Vec::new(),
            import_queue_auto: false,
            last_title: None,
        }
    }

    pub(crate) fn t(&self, key: L) -> &'static str {
        self.state.i18n.t(key)
    }

    // ------------------------------------------------------------------
    // Workbench: persona/idioma com persistência best-effort
    // ------------------------------------------------------------------

    /// Persiste a config (best-effort — falha de disco não atrapalha a UI).
    ///
    /// O caminho é resolvido UMA vez em [`TerraApp::new`] (`config_file()`)
    /// e fica em `self.workbench_path` — testes podem injetar um caminho
    /// temporário sem tocar em variáveis de ambiente.
    pub fn workbench_save(&mut self) {
        self.workbench.persona = self.state.persona.slug().to_owned();
        self.workbench.lang = self.state.i18n.lang.slug().to_owned();
        if let Some(path) = self.workbench_path.clone() {
            let _ = crate::workbench::save_to(&path, &self.workbench);
        }
    }

    /// Aplica persona (preset de flags) e persiste.
    pub fn apply_persona(&mut self, persona: crate::persona::Persona) {
        self.state.apply_persona(persona);
        self.workbench_save();
    }

    /// Troca o idioma e persiste.
    pub fn set_language(&mut self, lang: crate::i18n::Lang) {
        self.state.i18n.lang = lang;
        self.workbench_save();
    }

    // ------------------------------------------------------------------
    // Ações de camada (menu/contexto — um único caminho para cada efeito)
    // ------------------------------------------------------------------

    /// Abre o diálogo de renomear camada pré-preenchido (contexto da árvore).
    pub fn open_rename_layer_at(&mut self, idx: usize) {
        self.dlg.rename_layer_name = self
            .state
            .layers
            .get(idx)
            .map(|g| g.layer.name.clone())
            .unwrap_or_default();
        self.dlg.rename_layer = Some(idx);
    }

    /// Renomear a camada ativa (menu Gerenciamento de Projeto).
    pub fn open_rename_layer(&mut self) {
        if let Some(idx) = self.state.active_or_first() {
            self.open_rename_layer_at(idx);
        }
    }

    /// Remove a camada `idx` da sessão (local — no banco via `DeleteLayer`).
    pub fn remove_layer_at(&mut self, idx: usize) {
        if idx >= self.state.layers.len() {
            return;
        }
        let layer_id = self.state.layers[idx].layer.id.clone();
        let is_raster = self.state.layers[idx].layer.geom_type == GeomType::Raster;
        self.state.layers.remove(idx);
        self.state.active_layer = None;
        self.state.selection = None;
        if is_raster {
            self.map.set_raster(None);
        }
        if !layer_id.is_empty() && self.state.conn.is_db() {
            self.state.conn.send(DbCmd::DeleteLayer { layer_id });
        }
        self.state.sync_map(&mut self.map);
    }

    /// Remove a camada ativa (menu Gerenciamento de Projeto).
    pub fn remove_active_layer(&mut self) {
        if let Some(idx) = self.state.active_or_first() {
            self.remove_layer_at(idx);
        }
    }

    /// Nomes das camadas da sessão (combos de processamento).
    pub fn layer_names(&self) -> Vec<String> {
        self.state
            .layers
            .iter()
            .map(|g| g.layer.name.clone())
            .collect()
    }

    /// Abre o diálogo de processamento pré-preenchido.
    pub fn open_processing(&mut self, kind: ProcKind) {
        let source = self
            .state
            .active_or_first()
            .map(|i| self.state.layers[i].layer.name.clone());
        let target_suffix = match kind {
            ProcKind::Buffer => "buffer",
            ProcKind::Intersect => "intersecao",
            ProcKind::Union => "uniao",
        };
        let target = format!(
            "{}_{target_suffix}",
            source.clone().unwrap_or_else(|| "camada".into())
        );
        self.dlg.proc_source_a = source;
        self.dlg.proc_source_b = None;
        self.dlg.proc_distance = "0.01".into();
        self.dlg.proc_target = target;
        self.dlg.processing = Some(kind);
    }

    /// Executa o processamento via worker (PostGIS server-side).
    pub fn run_processing(&mut self, kind: ProcKind) {
        let Some(project) = self.state.project.clone() else {
            self.error = Some(self.t(L::ProcNeedsDb).to_owned());
            return;
        };
        let Some(source_a) = self.resolve_layer_id(self.dlg.proc_source_a.as_deref()) else {
            self.error = Some(self.t(L::ProcNeedsDb).to_owned());
            return;
        };
        let source_b = if kind == ProcKind::Intersect {
            match self.resolve_layer_id(self.dlg.proc_source_b.as_deref()) {
                Some(id) => Some(id),
                None => {
                    self.error = Some(self.t(L::ProcNeedsDb).to_owned());
                    return;
                }
            }
        } else {
            None
        };
        let distance = self.dlg.proc_distance.trim().parse::<f64>().unwrap_or(0.0);
        self.state.conn.send(DbCmd::ProcessOp {
            op: match kind {
                ProcKind::Buffer => crate::db_worker::ProcOp::Buffer,
                ProcKind::Intersect => crate::db_worker::ProcOp::Intersect,
                ProcKind::Union => crate::db_worker::ProcOp::Union,
            },
            project_id: project.id,
            source_a,
            source_b,
            distance_deg: distance,
            target_name: self.dlg.proc_target.trim().to_owned(),
        });
        self.info = Some(self.t(L::Loading).to_owned());
    }

    /// id da camada no banco a partir do nome (combos mostram nomes).
    fn resolve_layer_id(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        self.state
            .layers
            .iter()
            .find(|g| g.layer.name == name)
            .map(|g| g.layer.id.clone())
            .filter(|id| !id.is_empty())
    }

    /// Export GeoJSON da camada ativa em thread de fundo.
    pub fn start_export(&mut self, path: String) {
        let Some(idx) = self.state.active_or_first() else {
            return;
        };
        let features: Vec<Feature> = self.state.layers[idx]
            .features
            .iter()
            .map(|lf| lf.feature.clone())
            .collect();
        if features.is_empty() {
            self.error = Some(self.t(L::AttrNoLayer).to_owned());
            return;
        }
        self.export_job = Some(crate::export_job::spawn_export(
            features,
            std::path::PathBuf::from(path),
        ));
    }

    // ------------------------------------------------------------------
    // v0.2.1: import consolidado + navegador + drag & drop + recentes
    // ------------------------------------------------------------------

    /// **Único** caminho de import da UI (diálogo, navegador, drag & drop,
    /// recentes): valida, sobe a thread e fecha os modais de origem.
    ///
    /// Falha vira erro no banner — nunca panic.
    pub fn start_import_from_path(&mut self, path: String) {
        let trimmed = path.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(self.t(L::ImportDialogPathHint).to_owned());
            return;
        }
        if !std::path::Path::new(&trimmed).exists() {
            self.error = Some(self.t(L::FileBrowserInvalidPath).to_owned());
            return;
        }
        let srid = self.import_srid.trim().parse::<u32>().ok();
        self.import_job = Some(spawn_import(
            std::path::PathBuf::from(&trimmed),
            srid,
            self.t(L::ImportStageReading).to_owned(),
            self.t(L::ImportStageNormalizing).to_owned(),
        ));
        self.show_import = false;
        // Confirmou um import com fila pendente (drop multi-arquivo)?
        self.import_queue_auto = !self.import_queue.is_empty();
    }

    /// Consome o próximo da fila (se a fila automática estiver ativa e
    /// nenhum job roda). Chamado ao fim de `drain_import`.
    fn advance_import_queue(&mut self) {
        if self.import_job.is_some() || !self.import_queue_auto || self.import_queue.is_empty() {
            return;
        }
        let next = self.import_queue.remove(0);
        self.import_queue_auto = !self.import_queue.is_empty();
        let srid = self.import_srid.trim().parse::<u32>().ok();
        self.import_job = Some(spawn_import(
            next,
            srid,
            self.t(L::ImportStageReading).to_owned(),
            self.t(L::ImportStageNormalizing).to_owned(),
        ));
    }

    /// Cancelamento do diálogo de import descarta a fila do drop.
    pub fn cancel_import_dialog(&mut self) {
        self.show_import = false;
        self.import_queue.clear();
        self.import_queue_auto = false;
    }

    /// Payload de drag & drop → plano (suportados + ignorados).
    ///
    /// 1º suportado pré-preenche o diálogo (sem auto-start: o usuário vê o
    /// SRID e confirma); o resto entra na fila na ordem recebida.
    pub fn handle_dropped_paths(&mut self, paths: Vec<std::path::PathBuf>) {
        let plan = crate::file_browser::plan_drop(&paths);
        let Some(first) = plan.importable.first().cloned() else {
            if plan.ignored > 0 {
                self.error = Some(self.t(L::FileBrowserNoDroppable).to_owned());
            }
            return;
        };
        self.import_queue = plan.importable[1..].to_vec();
        self.import_path = first.to_string_lossy().to_string();
        self.show_import = true;
    }

    /// Registra arquivo (+ seu diretório) nos recentes e persiste
    /// (best-effort). Import/export/browser convergem aqui (v0.2.1).
    pub fn remember_recent_file(&mut self, path: &std::path::Path) {
        self.workbench.push_recent_file(&path.to_string_lossy());
        if let Some(parent) = path.parent() {
            self.workbench.push_recent_dir(&parent.to_string_lossy());
        }
        self.workbench_save();
    }

    // ------------------------------------------------------------------
    // Atalhos de teclado (a11y)
    // ------------------------------------------------------------------
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Enquanto um campo de texto tem foco, atalhos de tecla solta
        // (+/−/Esc) não disparam — digitação vem primeiro (a11y).
        let editing = ctx.egui_wants_keyboard_input();
        ctx.input(|i| {
            let ctrl = i.modifiers.contains(Modifiers::CTRL);
            // v0.2.1: Ctrl+Shift+O (navegador) vence Ctrl+O (projeto).
            if ctrl
                && i.key_pressed(Key::O)
                && i.modifiers.contains(Modifiers::SHIFT)
                && self.state.flags.import_export
            {
                let seed = self.browser.cwd.clone();
                self.browser.open_at(seed);
            } else if ctrl && i.key_pressed(Key::O) {
                self.request_open_project();
            }
            if ctrl && i.key_pressed(Key::I) && self.state.flags.import_export {
                self.show_import = true;
            }
            // v0.2.2: Ctrl+K alterna a paleta de comandos (funciona em
            // qualquer foco — é navegação, não digitação de campo).
            if ctrl && i.key_pressed(Key::K) {
                self.dlg.palette = !self.dlg.palette;
            }
            if ctrl && i.key_pressed(Key::Z) && !editing {
                self.state.undo();
                self.state.sync_map(&mut self.map);
            }
            if ctrl && i.key_pressed(Key::Y) && !editing {
                self.state.redo();
                self.state.sync_map(&mut self.map);
            }
            if !editing {
                if i.key_pressed(Key::Plus) || (ctrl && i.key_pressed(Key::Equals)) {
                    self.map.zoom_step(1.25);
                }
                if i.key_pressed(Key::Minus) {
                    self.map.zoom_step(0.8);
                }
                if i.key_pressed(Key::Escape) {
                    self.state.edit_mode = EditMode::Navigate;
                }
            }
        });
    }

    pub fn request_open_project(&mut self) {
        if self.state.conn.is_db() {
            if let Conn::Db(w) = &self.state.conn {
                w.send(DbCmd::ListProjects);
            }
            self.show_projects = true;
        } else {
            self.info = Some(self.t(L::LocalModeHint).to_owned());
        }
    }

    // ------------------------------------------------------------------
    // Conexão
    // ------------------------------------------------------------------
    pub(crate) fn connect_clicked(&mut self) {
        let url = if self.conn.url.trim().is_empty() {
            // Monta a URL dos campos, com componente URL-escaped (senha com
            // '@' ou '/' não quebra o DSN).
            let enc = percent_encoding::NON_ALPHANUMERIC;
            let user = percent_encoding::utf8_percent_encode(&self.conn.user, enc);
            let pass = percent_encoding::utf8_percent_encode(&self.conn.pass, enc);
            format!(
                "postgres://{}:{}@{}:{}/{}",
                user, pass, self.conn.host, self.conn.port, self.conn.db
            )
        } else {
            self.conn.url.trim().to_owned()
        };
        if self.state.conn.is_db() {
            // Já existe worker (ex.: reconexão) — reutiliza.
            self.state.conn.send(DbCmd::Connect { url });
        } else {
            let worker = DbWorker::spawn();
            worker.send(DbCmd::Connect { url });
            self.state.conn = Conn::Db(Box::new(worker));
        }
        self.show_connect = false;
    }

    // ------------------------------------------------------------------
    // Bomba de eventos (banco + import)
    // ------------------------------------------------------------------
    fn drain_db_events(&mut self) {
        let events = match &self.state.conn {
            Conn::Db(w) => {
                if !w.is_alive() {
                    self.error = Some(self.t(L::DbConnectFailed).to_owned());
                    Vec::new()
                } else {
                    w.drain()
                }
            }
            Conn::Local => Vec::new(),
        };
        for evt in events {
            match evt {
                DbEvt::Connected => {
                    self.info = Some(self.t(L::DbConnected).to_owned());
                }
                DbEvt::ConnectFailed(msg) => {
                    self.error = Some(format!("{}: {msg}", self.t(L::DbConnectFailed)));
                    self.state.conn = Conn::Local;
                }
                DbEvt::Disconnected => {
                    self.state.project = None;
                    self.state.layers.clear();
                    self.state.sync_map(&mut self.map);
                }
                DbEvt::Projects(list) => self.projects_cache = list,
                DbEvt::ProjectCreated(p) => {
                    self.state.project = Some(p);
                }
                DbEvt::ProjectOpened { project, handles } => {
                    // Converte handles → GuiLayers da sessão.
                    self.state.layers = handles
                        .into_iter()
                        .map(|h| crate::app_state::GuiLayer::with_features(h.layer, h.features))
                        .collect();
                    self.state.project = Some(project);
                    self.state.selection = None;
                    self.state.active_layer = None;
                    self.zoom_to_extent();
                }
                DbEvt::LayerRefreshed(handle) => {
                    if let Some(g) = self
                        .state
                        .layers
                        .iter_mut()
                        .find(|g| g.layer.id == handle.layer.id)
                    {
                        g.features = handle.features;
                        g.layer = handle.layer;
                    }
                }
                DbEvt::LayerAdded(layer) => {
                    // Camada registrada: avança o import pendente (bulk).
                    self.advance_pending_after_layer_added(&layer);
                }
                DbEvt::Inserted { layer_id, .. } => {
                    // Pede a recarga com ids de linha (mapeamento p/ edição).
                    self.state.conn.send(DbCmd::OpenLayer { layer_id });
                }
                DbEvt::AttrsUpdated { .. } | DbEvt::FeatureDeleted { .. } => {}
                // ---- v0.2 ----------------------------------------------
                DbEvt::MigrationsApplied => {
                    self.info = Some(self.t(L::MigrationsApplied).to_owned());
                }
                DbEvt::DbInfo(text) => {
                    self.dlg.db_info_text = Some(text);
                    self.dlg.db_info = true;
                }
                DbEvt::LayerDeleted { layer_id } => {
                    if let Some(pos) = self
                        .state
                        .layers
                        .iter()
                        .position(|g| g.layer.id == layer_id)
                    {
                        let was_raster = self.state.layers[pos].layer.geom_type == GeomType::Raster;
                        self.state.layers.remove(pos);
                        self.state.active_layer = None;
                        self.state.selection = None;
                        if was_raster {
                            self.map.set_raster(None);
                        }
                        self.state.sync_map(&mut self.map);
                    }
                }
                DbEvt::ProjectDeleted { project_id } => {
                    if self
                        .state
                        .project
                        .as_ref()
                        .is_some_and(|p| p.id == project_id)
                    {
                        self.state.project = None;
                        self.state.layers.clear();
                        self.state.selection = None;
                        self.state.active_layer = None;
                        self.state.sync_map(&mut self.map);
                    }
                    self.info = Some(self.t(L::DeleteProject).to_owned());
                }
                DbEvt::ProjectRenamed(p) => {
                    self.info = Some(format!("{}: {}", self.t(L::RenameAction), p.name));
                    if self
                        .state
                        .project
                        .as_ref()
                        .is_some_and(|cur| cur.id == p.id)
                    {
                        self.state.project = Some(p);
                    }
                }
                DbEvt::LayerRenamed(layer) => {
                    if let Some(g) = self
                        .state
                        .layers
                        .iter_mut()
                        .find(|g| g.layer.id == layer.id)
                    {
                        g.layer.name = layer.name.clone();
                    }
                    self.info = Some(format!("{}: {}", self.t(L::RenameAction), layer.name));
                }
                DbEvt::Processed(layer) => {
                    self.info = Some(format!("{}: {}", self.t(L::ProcDone), layer.name));
                    let layer_id = layer.id.clone();
                    self.state
                        .layers
                        .push(crate::app_state::GuiLayer::new(layer));
                    self.state.active_layer = Some(self.state.layers.len() - 1);
                    // Recarrega com as feições derivadas (worker → evento).
                    self.state.conn.send(DbCmd::OpenLayer { layer_id });
                    self.state.sync_map(&mut self.map);
                }
                DbEvt::QueryResult(rows) => {
                    self.dlg.sql_rows = Some(rows);
                }
                DbEvt::Failed(msg) => self.error = Some(msg),
            }
        }
    }

    fn drain_import(&mut self) {
        if let Some(job) = &self.import_job {
            let msgs: Vec<ImportMsg> = {
                let mut out = Vec::new();
                while let Ok(m) = job.rx.try_recv() {
                    out.push(m);
                }
                out
            };
            for msg in msgs {
                match msg {
                    ImportMsg::Stage(s) => {
                        if let Some(job) = &mut self.import_job {
                            job.stage = s;
                        }
                    }
                    ImportMsg::DoneVector {
                        name,
                        path,
                        features,
                        geom,
                        bbox,
                        report,
                    } => {
                        self.import_job = None;
                        self.info = Some(format!(
                            "{}: {} {}",
                            self.t(L::ImportDone),
                            report.n_features,
                            self.t(L::StatusBarFeatures)
                        ));
                        self.remember_recent_file(&path);
                        self.pending_import = Some(PendingImport {
                            name,
                            features,
                            geom,
                            bbox,
                            source: path.to_string_lossy().to_string(),
                            srid: None,
                        });
                        self.start_pending_import();
                    }
                    ImportMsg::DoneRaster { path, meta } => {
                        self.import_job = None;
                        self.remember_recent_file(&path);
                        self.register_raster(path, meta);
                    }
                    ImportMsg::Failed(e) => {
                        self.import_job = None;
                        self.error = Some(format!("{}: {e}", self.t(L::ImportFailed)));
                        // Falha interrompe a fila do drop (usuário decide).
                        self.import_queue.clear();
                        self.import_queue_auto = false;
                    }
                }
            }
        }
        self.advance_import_queue();
    }

    /// Registra o raster (local ou banco) e carrega a banda 0 no canvas.
    fn register_raster(&mut self, path: std::path::PathBuf, meta: RasterMeta) {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "raster".into());
        let layer = crate::app_state::AppState::new_layer_for(
            self.state.project.as_ref().map_or("", |p| &p.id),
            &name,
            GeomType::Raster,
            &path.to_string_lossy(),
        );
        let bbox =
            crate::map_view::Bbox::new(meta.bbox[0], meta.bbox[1], meta.bbox[2], meta.bbox[3]);
        if self.state.conn.is_db() {
            self.state.conn.send(DbCmd::AddLayer {
                layer,
                envelope: meta.bbox,
            });
        } else {
            self.state
                .layers
                .push(crate::app_state::GuiLayer::new(layer));
        }
        // Banda 0 em thread de fundo (a leitura GDAL nunca roda na UI
        // thread): a view nasce sem imagem e pinta quando o canal entrega.
        // Cache invalidado ao trocar de arquivo — bandas do arquivo antigo
        // não voltam.
        self.band_cache.retain(|(p, _), _| *p == path);
        self.map.set_raster(Some(crate::map_view::RasterLayerView {
            path: path.clone(),
            bbox,
            band: 0,
            band_count: meta.bands,
            image: None,
        }));
        self.request_band(path, 0);
        self.info = Some(self.t(L::ImportRasterLoaded).to_owned());
        self.state.sync_map(&mut self.map);
    }

    /// Pede a banda `band` de `path` para o canvas.
    ///
    /// Cache hit aplica direto (sem thread); cache miss sobe a thread de
    /// fundo ([`crate::raster_view::spawn_band_load`]) e a UI mostra
    /// "carregando banda…" até o canal entregar.
    pub fn request_band(&mut self, path: std::path::PathBuf, band: usize) {
        if let Some(img) = self.band_cache.get(&(path.clone(), band)) {
            let img = img.clone();
            if let Some(r) = self.map.raster_mut() {
                r.band = band;
            }
            self.map.set_raster_image(img);
            return;
        }
        // Pedido igual já em andamento: não sobe thread duplicada.
        if self
            .band_job
            .as_ref()
            .is_some_and(|j| j.path == path && j.band == band)
        {
            return;
        }
        self.band_job = Some(raster_view::spawn_band_load(path, band));
    }

    /// Job de banda em andamento? (UI mostra "carregando banda…".)
    pub fn band_loading(&self) -> bool {
        self.band_job.is_some()
    }

    /// Drena o canal do worker de banda (um pedido por job).
    fn drain_band_loads(&mut self) {
        let Some(job) = &self.band_job else {
            return;
        };
        let mut msgs: Vec<raster_view::BandMsg> = Vec::new();
        while let Ok(m) = job.rx.try_recv() {
            msgs.push(m);
        }
        if msgs.is_empty() {
            return;
        }
        self.band_job = None;
        for msg in msgs {
            let raster_view::BandMsg::Done { path, band, result } = msg;
            match result {
                Ok(img) => {
                    self.band_cache.insert((path.clone(), band), img.clone());
                    // Aplica só se o raster corrente ainda é o arquivo
                    // pedido (troca durante a carga descarta o resultado).
                    if self.map.raster().is_some_and(|r| r.path == path) {
                        if let Some(r) = self.map.raster_mut() {
                            r.band = band;
                        }
                        self.map.set_raster_image(img);
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }

    /// Import vetorial concluído: registra camada (banco) ou monta local.
    fn start_pending_import(&mut self) {
        let Some(pending) = self.pending_import.take() else {
            return;
        };
        let layer = crate::app_state::AppState::new_layer_for(
            self.state.project.as_ref().map_or("", |p| &p.id),
            &pending.name,
            pending.geom,
            &pending.source,
        );
        if self.state.conn.is_db() && self.state.project.is_some() {
            let envelope = pending.bbox;
            self.pending_import = Some(pending);
            self.state.conn.send(DbCmd::AddLayer { layer, envelope });
        } else {
            if self.state.conn.is_db() {
                self.info = Some(self.t(L::NoProject).to_owned());
            }
            self.state
                .layers
                .push(crate::app_state::GuiLayer::with_features(
                    layer,
                    to_layer_features(pending.features),
                ));
            let n = self.state.layers.len() - 1;
            self.state.active_layer = Some(n);
            self.zoom_to_extent();
            self.state.sync_map(&mut self.map);
        }
    }

    fn advance_pending_after_layer_added(&mut self, layer: &ta_core::layer::Layer) {
        if layer.geom_type == GeomType::Raster {
            self.state
                .layers
                .push(crate::app_state::GuiLayer::new(layer.clone()));
            self.state.active_layer = Some(self.state.layers.len() - 1);
            return;
        }
        let Some(pending) = self.pending_import.take() else {
            // Camada raster registrada via AddLayer cai aqui também.
            self.state
                .layers
                .push(crate::app_state::GuiLayer::new(layer.clone()));
            return;
        };
        let feats = pending.features.clone();
        self.state
            .layers
            .push(crate::app_state::GuiLayer::with_features(
                layer.clone(),
                Vec::new(),
            ));
        let idx = self.state.layers.len() - 1;
        self.state.active_layer = Some(idx);
        self.state.conn.send(DbCmd::BulkInsert {
            layer_id: layer.id.clone(),
            features: feats,
        });
        self.zoom_to_extent();
        self.state.sync_map(&mut self.map);
    }

    /// Drena o canal do export GeoJSON (thread de fundo → estado).
    fn drain_export(&mut self) {
        let Some(job) = &self.export_job else {
            return;
        };
        let mut msgs: Vec<crate::export_job::ExportMsg> = Vec::new();
        while let Ok(m) = job.rx.try_recv() {
            msgs.push(m);
        }
        if msgs.is_empty() {
            return;
        }
        self.export_job = None;
        for msg in msgs {
            match msg {
                crate::export_job::ExportMsg::Done(n) => {
                    self.info = Some(format!(
                        "{}: {n} {}",
                        self.t(L::ExportDone),
                        self.t(L::StatusBarFeatures)
                    ));
                    // Recentes: o export também alimenta a lista (v0.2.1).
                    let path = self.dlg.export_path.trim().to_owned();
                    if !path.is_empty() {
                        self.remember_recent_file(std::path::Path::new(&path));
                    }
                }
                crate::export_job::ExportMsg::Failed(e) => {
                    self.error = Some(format!("{}: {e}", self.t(L::ExportDone)));
                }
            }
        }
    }

    /// Drena os arquivos soltos na janela (egui raw drag-drop payload).
    /// Um frame processa apenas uma rodada de drop.
    fn drain_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        if !dropped.is_empty() {
            self.handle_dropped_paths(dropped);
        }
    }

    pub fn zoom_to_extent(&mut self) {
        if let Some(ext) = self.state.visible_extent() {
            self.map
                .zoom_to(crate::map_view::Bbox::new(ext[0], ext[1], ext[2], ext[3]));
        }
    }

    /// v0.2.2: título da janela com o projeto aberto ("Curupira GIS —
    /// projeto"). Envia `ViewportCommand::Title` só quando muda.
    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match &self.state.project {
            Some(p) => format!("{} — {}", self.t(L::AppTitle), p.name),
            None => self.t(L::AppTitle).to_owned(),
        };
        if self.last_title.as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = Some(title);
        }
    }

    // ------------------------------------------------------------------
    // Clique no mapa: pick ou novo ponto
    // ------------------------------------------------------------------
    fn handle_map_click(&mut self) {
        let Some((screen, world)) = self.map.take_click() else {
            return;
        };
        match self.state.edit_mode {
            EditMode::Navigate => {
                if let Some(uid) = self.map.pick(screen) {
                    self.state.selection = Some(crate::map_view::decode_feature_uid(uid));
                } else {
                    self.state.selection = None;
                }
                self.attr_buffers.clear();
                self.state.sync_map(&mut self.map);
            }
            EditMode::NewPoint => {
                let Some(layer_idx) = self.state.active_or_first() else {
                    return;
                };
                let feature = Feature::new(
                    geo_types::Geometry::Point(geo_types::Point::new(world.0, world.1)),
                    serde_json::json!({}),
                );
                match self.state.append_feature(layer_idx, feature.clone()) {
                    Some((l, _)) => {
                        // Em banco: persiste e recarrega a camada p/ mapear o id.
                        if let Some(layer_id) = self
                            .state
                            .layers
                            .get(l)
                            .filter(|g| !g.layer.id.is_empty())
                            .map(|g| g.layer.id.clone())
                            .filter(|_| self.state.conn.is_db())
                        {
                            self.state.conn.send(DbCmd::BulkInsert {
                                layer_id,
                                features: vec![feature],
                            });
                        }
                        self.state.sync_map(&mut self.map);
                    }
                    None => {
                        self.error = Some(self.t(L::NewPointHint).to_owned());
                    }
                }
                // Modo persiste até Esc (permite vários pontos).
            }
        }
    }
}

fn to_layer_features(features: Vec<Feature>) -> Vec<crate::map_view::LayerFeature> {
    features
        .into_iter()
        .map(crate::map_view::LayerFeature::local)
        .collect()
}

impl eframe::App for TerraApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl TerraApp {
    /// Corpo da UI (separado do `eframe::App` para permitir smoke test
    /// headless via `Context::run_ui` — o CI não tem display/GPU).
    ///
    /// Ordem dos painéis = ordem de "escultura" da área; o canvas é o
    /// último (consumir o resto). Personas filtram **apenas** painéis
    /// auxiliares — o canvas nunca é escondido.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.drain_db_events();
        self.drain_import();
        self.drain_export();
        self.drain_band_loads();
        // v0.2.1: drag & drop de arquivos do SO direto no canvas.
        self.drain_drops(&ctx);
        // v0.2.2: título da janela reflete o projeto aberto.
        self.update_window_title(&ctx);
        // Banda carregando: repinta em ritmo baixo até o canal entregar
        // (sem isso a imagem só apareceria no próximo evento de input).
        if self.band_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.handle_map_click();

        // Shell v0.2: menu completo → toolbars agrupadas → trilho de ícones
        // → camadas → inspetor → tabela → status → canvas → diálogos.
        crate::menus::menu_bar(ui, self);
        crate::menus::toolbars(ui, self);
        panels::rail(ui, self);
        panels::layers_dock(ui, self);
        if self.state.flags.inspector {
            panels::inspector(ui, self);
        }
        if self.state.flags.attr_table {
            attribute_table(ui, self);
        }
        crate::status_bar::status_bar(ui, self);
        panels::central_canvas(ui, self);
        dialogs(ui, self);
    }
}
