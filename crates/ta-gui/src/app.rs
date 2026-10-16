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
use crate::import_job::{ImportJob, ImportMsg, PendingImport};
use crate::map_view::MapView;
use crate::panels;
use crate::raster_view;

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
        Self {
            state: AppState::new(),
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
        }
    }

    pub(crate) fn t(&self, key: L) -> &'static str {
        self.state.i18n.t(key)
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
            if ctrl && i.key_pressed(Key::O) {
                self.request_open_project();
            }
            if ctrl && i.key_pressed(Key::I) {
                self.show_import = true;
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
                DbEvt::Failed(msg) => self.error = Some(msg),
            }
        }
    }

    fn drain_import(&mut self) {
        let Some(job) = &self.import_job else {
            return;
        };
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
                    self.register_raster(path, meta);
                }
                ImportMsg::Failed(e) => {
                    self.import_job = None;
                    self.error = Some(format!("{}: {e}", self.t(L::ImportFailed)));
                }
            }
        }
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

    pub fn zoom_to_extent(&mut self) {
        if let Some(ext) = self.state.visible_extent() {
            self.map
                .zoom_to(crate::map_view::Bbox::new(ext[0], ext[1], ext[2], ext[3]));
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
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.drain_db_events();
        self.drain_import();
        self.drain_band_loads();
        // Banda carregando: repinta em ritmo baixo até o canal entregar
        // (sem isso a imagem só apareceria no próximo evento de input).
        if self.band_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.handle_map_click();

        // Ordem dos painéis = ordem de "escultura" da área; o canvas é o
        // último (consumir o resto).
        panels::top_menu(ui, self);
        panels::layers_dock(ui, self);
        panels::inspector(ui, self);
        attribute_table(ui, self);
        panels::status_bar(ui, self);
        panels::central_canvas(ui, self);
        dialogs(ui, self);
    }
}
