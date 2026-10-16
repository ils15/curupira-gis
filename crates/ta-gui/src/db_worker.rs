//! Worker de banco em thread dedicada — a UI **nunca bloqueia** em I/O.
//!
//! Modelo: um `std::thread` dono de um runtime tokio single-thread (sqlx
//! precisa de reactor); a GUI conversa por canais `std::sync::mpsc`:
//! comandos [`DbCmd`] entram, eventos [`DbEvt`] saem. O pool vive SOMENTE
//! na thread do worker — nada de `PgPool` no estado da UI.
//!
//! Segurança: toda query passa pelos repositórios de `ta-db` (**bind-only**
//! — gate themis). A senha nunca aparece em evento/log (só o DSN montado no
//! diálogo, que a UI também não imprime).

use std::sync::mpsc::{Receiver, Sender};
use std::time::SystemTime;

use ta_core::feature::Feature;
use ta_core::layer::Layer;
use ta_core::project::{DEFAULT_SRID, Project};
use ta_core::style::Style;
use ta_db::pool::{DbConfig, connect};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::spatial::{ensure_postgis, run_migrations};

use crate::map_view::{LayerFeature, LayerHandle};

/// `created_at` do domínio (`time` RFC3339) — o schema usa `DEFAULT now()`;
/// o valor local é só o que a struct `Project` exige para o `create`.
fn now_offsetdatetime() -> time::OffsetDateTime {
    let unix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(unix).unwrap_or(0))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

/// Comando da UI para o worker.
#[derive(Debug)]
pub enum DbCmd {
    /// Conecta, aplica migrações e garante PostGIS. Falha → `ConnectFailed`.
    Connect { url: String },
    /// Fecha o pool e volta ao modo local.
    Disconnect,
    /// Lista projetos (diálogo "Abrir projeto").
    ListProjects,
    /// Cria projeto novo com nome dado (SRID 4674 fixo).
    CreateProject { name: String },
    /// Carrega camadas+features do projeto (handles prontos para o mapa).
    OpenProject { project_id: String },
    /// (Re)carrega uma camada — usado após inserir feição nova.
    OpenLayer { layer_id: String },
    /// Registra a camada (raster) e grava o bbox do envelope.
    AddLayer { layer: Layer, envelope: [f64; 4] },
    /// Bulk insert (COPY) das features de um import.
    BulkInsert {
        layer_id: String,
        features: Vec<Feature>,
    },
    /// Edição de atributos (substitui o JSONB inteiro).
    UpdateAttrs {
        db_id: i64,
        attrs: serde_json::Value,
    },
    /// Exclusão da feição.
    DeleteFeature { db_id: i64 },
}

/// Evento do worker para a UI.
#[derive(Debug)]
pub enum DbEvt {
    Connected,
    ConnectFailed(String),
    Disconnected,
    Projects(Vec<Project>),
    ProjectCreated(Project),
    ProjectOpened {
        project: Project,
        handles: Vec<LayerHandle>,
    },
    /// Camada recarregada (features novas inclusas).
    LayerRefreshed(LayerHandle),
    LayerAdded(Layer),
    Inserted {
        layer_id: String,
        count: u64,
    },
    AttrsUpdated {
        db_id: i64,
    },
    FeatureDeleted {
        db_id: i64,
    },
    Failed(String),
}

/// Par de canais da UI: envia comandos, recebe eventos.
pub struct DbWorker {
    tx: Sender<DbCmd>,
    rx: Receiver<DbEvt>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for DbWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbWorker")
            .field(
                "alive",
                &self.alive.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl DbWorker {
    /// Sobe a thread do worker.
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DbCmd>();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<DbEvt>();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let alive_thread = std::sync::Arc::clone(&alive);
        std::thread::Builder::new()
            .name("ta-db-worker".into())
            .spawn(move || {
                run_worker(cmd_rx, evt_tx);
                alive_thread.store(false, std::sync::atomic::Ordering::Relaxed);
            })
            .expect("spawn da thread de banco (recursos de thread do SO)");
        Self {
            tx: cmd_tx,
            rx: evt_rx,
            alive,
        }
    }

    /// Enfileira um comando (nunca bloqueia — canal ilimitado).
    pub fn send(&self, cmd: DbCmd) {
        // Falha só se o worker morreu; a UI reporta como erro de sessão.
        if self.tx.send(cmd).is_err() {
            // Sem panic em path de UI: o usuário verá "worker indisponível"
            // na próxima checagem de eventos (canal rx fica sem emissões e o
            // app detecta via `is_alive`).
        }
    }

    /// Drena os eventos disponíveis (não bloqueia).
    pub fn drain(&self) -> Vec<DbEvt> {
        let mut out = Vec::new();
        while let Ok(evt) = self.rx.try_recv() {
            out.push(evt);
        }
        out
    }

    /// A thread do worker está viva?
    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Loop principal do worker. Erros de operação viram `DbEvt::Failed` com
/// mensagem legível; o worker nunca entra em panic por erro de banco.
fn run_worker(cmd_rx: Receiver<DbCmd>, evt_tx: Sender<DbEvt>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = evt_tx.send(DbEvt::Failed(format!(
                "não foi possível iniciar o runtime de banco: {e}"
            )));
            return;
        }
    };

    // Estado de conexão (pool dentro da thread).
    let mut pool: Option<sqlx::PgPool> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            DbCmd::Connect { url } => {
                let cfg = DbConfig {
                    url: url.clone(),
                    max_connections: 4,
                };
                match rt.block_on(async {
                    let pool: sqlx::PgPool = connect(&cfg).await?;
                    ensure_postgis(&pool).await?;
                    run_migrations(&pool).await?;
                    Ok::<sqlx::PgPool, ta_db::Error>(pool)
                }) {
                    Ok(p) => {
                        pool = Some(p);
                        let _ = evt_tx.send(DbEvt::Connected);
                    }
                    Err(e) => {
                        // Mensagem legível; sem ecoar a senha do DSN.
                        pool = None;
                        let _ = evt_tx.send(DbEvt::ConnectFailed(format!(
                            "{e}\n(verifique host/porta/usuário/senha e se o \
                             PostGIS 3.4 está instalado no servidor)"
                        )));
                    }
                }
            }
            DbCmd::Disconnect => {
                if let Some(p) = pool.take() {
                    rt.block_on(p.close());
                }
                let _ = evt_tx.send(DbEvt::Disconnected);
            }
            DbCmd::ListProjects => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgProjectRepo::new(p);
                match rt.block_on(repo.list()) {
                    Ok(list) => {
                        let _ = evt_tx.send(DbEvt::Projects(list));
                    }
                    Err(e) => fail(&evt_tx, "listar projetos", e),
                }
            }
            DbCmd::CreateProject { name } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgProjectRepo::new(p);
                let project = Project {
                    id: String::new(),
                    name,
                    srid: DEFAULT_SRID,
                    created_at: now_offsetdatetime(),
                };
                match rt.block_on(repo.create(&project)) {
                    Ok(saved) => {
                        let _ = evt_tx.send(DbEvt::ProjectCreated(saved));
                    }
                    Err(e) => fail(&evt_tx, "criar projeto", e),
                }
            }
            DbCmd::OpenProject { project_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let projects = PgProjectRepo::new(p.clone());
                let layers_repo = PgLayerRepo::new(p.clone());
                let features = PgFeatureRepo::new(p);
                match rt.block_on(async {
                    let project = projects
                        .get(&project_id)
                        .await?
                        .ok_or_else(|| ta_db::Error::InvalidId(project_id.clone()))?;
                    let layers = layers_repo.list_for_project(&project_id).await?;
                    let mut handles = Vec::with_capacity(layers.len());
                    for layer in layers {
                        let rows = features.list_with_ids(&layer.id).await?;
                        handles.push(LayerHandle {
                            visible: true,
                            style: Style::default(),
                            features: rows
                                .into_iter()
                                .map(|(db_id, feature)| LayerFeature {
                                    feature,
                                    db_id: Some(db_id),
                                })
                                .collect(),
                            layer,
                        });
                    }
                    Ok::<_, ta_db::Error>((project, handles))
                }) {
                    Ok((project, handles)) => {
                        let _ = evt_tx.send(DbEvt::ProjectOpened { project, handles });
                    }
                    Err(e) => fail(&evt_tx, "abrir projeto", e),
                }
            }
            DbCmd::OpenLayer { layer_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let layers_repo = PgLayerRepo::new(p.clone());
                let features = PgFeatureRepo::new(p);
                match rt.block_on(async {
                    let layer = layers_repo
                        .get(&layer_id)
                        .await?
                        .ok_or_else(|| ta_db::Error::InvalidId(layer_id.clone()))?;
                    let rows = features.list_with_ids(&layer_id).await?;
                    Ok::<_, ta_db::Error>(LayerHandle {
                        visible: true,
                        style: Style::default(),
                        features: rows
                            .into_iter()
                            .map(|(db_id, feature)| LayerFeature {
                                feature,
                                db_id: Some(db_id),
                            })
                            .collect(),
                        layer,
                    })
                }) {
                    Ok(handle) => {
                        let _ = evt_tx.send(DbEvt::LayerRefreshed(handle));
                    }
                    Err(e) => fail(&evt_tx, "recarregar camada", e),
                }
            }
            DbCmd::AddLayer { layer, envelope } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgLayerRepo::new(p);
                match rt.block_on(async {
                    let saved = repo.add(&layer).await?;
                    repo.set_bbox(
                        &saved.id,
                        envelope[0],
                        envelope[1],
                        envelope[2],
                        envelope[3],
                    )
                    .await?;
                    Ok::<_, ta_db::Error>(saved)
                }) {
                    Ok(saved) => {
                        let _ = evt_tx.send(DbEvt::LayerAdded(saved));
                    }
                    Err(e) => fail(&evt_tx, "registrar camada", e),
                }
            }
            DbCmd::BulkInsert {
                layer_id,
                features: feats,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgFeatureRepo::new(p);
                match rt.block_on(repo.bulk_insert(&layer_id, feats)) {
                    Ok(count) => {
                        let _ = evt_tx.send(DbEvt::Inserted { layer_id, count });
                    }
                    Err(e) => fail(&evt_tx, "gravar feições", e),
                }
            }
            DbCmd::UpdateAttrs { db_id, attrs } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgFeatureRepo::new(p);
                match rt.block_on(repo.update_attrs(db_id, &attrs)) {
                    Ok(true) => {
                        let _ = evt_tx.send(DbEvt::AttrsUpdated { db_id });
                    }
                    Ok(false) => fail_msg(&evt_tx, format!("feição {db_id} não existe no banco")),
                    Err(e) => fail(&evt_tx, "salvar atributos", e),
                }
            }
            DbCmd::DeleteFeature { db_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgFeatureRepo::new(p);
                match rt.block_on(repo.delete_feature(db_id)) {
                    Ok(true) => {
                        let _ = evt_tx.send(DbEvt::FeatureDeleted { db_id });
                    }
                    Ok(false) => fail_msg(&evt_tx, format!("feição {db_id} não existe no banco")),
                    Err(e) => fail(&evt_tx, "remover feição", e),
                }
            }
        }
    }
}

fn not_connected(evt_tx: &Sender<DbEvt>) {
    let _ = evt_tx.send(DbEvt::Failed(
        "não conectado: use “Conectar banco” antes desta operação".into(),
    ));
}

fn fail<E: std::fmt::Display>(evt_tx: &Sender<DbEvt>, ctx: &str, e: E) {
    fail_msg(evt_tx, format!("{ctx}: {e}"))
}

fn fail_msg(evt_tx: &Sender<DbEvt>, msg: String) {
    let _ = evt_tx.send(DbEvt::Failed(msg));
}
