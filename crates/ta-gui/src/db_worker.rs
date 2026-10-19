//! Worker de banco em thread dedicada — a UI **nunca bloqueia** em I/O.
//!
//! Modelo: um `std::thread` dono de um runtime tokio **multi-thread** (sqlx
//! precisa de reactor; operações longas rodam em `rt.spawn` para não ocupar o
//! loop de comandos); a GUI conversa por canais `std::sync::mpsc`: comandos
//! [`DbCmd`] entram, eventos [`DbEvt`] saem. O pool vive SOMENTE na thread do
//! worker — nada de `PgPool` no estado da UI.
//!
//! **Operações longas fora do loop** (Fase 5b, revisão do Themis): run de
//! task, exchange (subprocesso com timeout), import CSV de medidas e geração
//! de grade grande são `rt.spawn`ados. O loop continua drenando comandos, então
//! `CancelTask` e leituras de viewport respondem durante um run; `Disconnect`
//! responde na hora (o fechamento do pool também é assíncrono).
//!
//! **Coalescência de viewport:** o worker não enfileira leituras — cada
//! `LoadCellView` leva um `seq` que a UI usa para DESCARTAR respostas antigas
//! (`CellViewLoaded` com `seq` velho é ignorado). É coalescência por geração:
//! durante um pan rápido só a última resposta interessa.
//!
//! Segurança: toda query passa pelos repositórios de `ta-db` (**bind-only**
//! — gate themis). A senha nunca aparece em evento/log (só o DSN montado no
//! diálogo, que a UI também não imprime).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, SystemTime};

use ta_core::area::AreaReport;
use ta_core::cell::BlockedCell;
use ta_core::feature::Feature;
use ta_core::layer::Layer;
use ta_core::phase::Phase;
use ta_core::project::{DEFAULT_SRID, Project};
use ta_core::style::Style;
use ta_db::pool::{DbConfig, connect};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellViewRow, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_class::{ClassRepo, DraftLayerRepo, PgClassRepo, PgDraftLayerRepo};
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::repo_ruleset::{PgRuleSetRepo, RuleSetRepo};
use ta_db::spatial::{ensure_postgis, run_migrations};
use ta_flow::measures::MeasuresImportReport;
use ta_flow::queue::{RunEvent, RunOutcome};
use ta_flow::{aoi, area, grid, phase, queue, ruleset};

use crate::map_view::{LayerFeature, LayerHandle};
use crate::monitoring::MonitoringData;

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
    // ---- v0.2 (paridade de UI) -------------------------------------
    /// Reaplica as migrações no banco conectado (menu Administração).
    RunMigrations,
    /// Coleta `version()` + `postgis_full_version()` (menu Administração).
    DbInfo,
    /// Remove a camada (feições somem por CASCADE).
    DeleteLayer { layer_id: String },
    /// Remove o projeto (camadas + feições por CASCADE).
    DeleteProject { project_id: String },
    /// Renomeia o projeto.
    RenameProject {
        project_id: String,
        new_name: String,
    },
    /// Renomeia a camada.
    RenameLayer { layer_id: String, new_name: String },
    /// Processamento espacial server-side (PostGIS; camada derivada nova).
    ProcessOp {
        op: ProcOp,
        project_id: String,
        source_a: String,
        source_b: Option<String>,
        distance_deg: f64,
        target_name: String,
    },
    /// Consulta **somente-leitura** do console SQL (SELECT/WITH, 1 instrução).
    Query { sql: String },
    // ---- Fase 5b (monitoramento) -----------------------------------
    /// Listas do projeto para a janela de monitoramento (uma leitura só).
    LoadMonitoring { project_id: String },
    /// Células da viewport do canvas (bbox em 4674 + LIMIT server-side).
    LoadCellView {
        seq: u64,
        cell_layer_id: String,
        draft_layer_id: Option<String>,
        bbox: [f64; 4],
    },
    /// Relatório de área consolidado (`ta_flow::area`, mesmo da CLI).
    AreaReport {
        seq: u64,
        cell_layer_id: String,
        draft_name: Option<String>,
    },
    /// Cria a task (`ta_flow::queue`) e a executa em lotes, emitindo progresso.
    ClassifyTask {
        project: Project,
        cell_layer_id: String,
        draft_name: Option<String>,
        task_name: Option<String>,
    },
    /// Bloqueia/desbloqueia a célula em `blocked_cells`.
    SetCellBlocked {
        cell_layer_id: String,
        cell_id: String,
        blocked: bool,
    },
    /// Export/import do exchange no **mesmo binário da CLI** (subprocesso).
    Exchange(ExchangeRequest),
    // ---- Fase 5b: preparo do fluxo pela GUI (B1) -------------------
    /// Cria a fase (`ta_flow::phase::create`).
    CreatePhase {
        project_id: String,
        name: String,
        description: String,
    },
    /// Vincula uma camada vetorial existente como AOI (`ta_flow::aoi::create`).
    LinkAoi {
        project_id: String,
        layer_id: String,
        id_column: Option<String>,
    },
    /// Gera a grade e persiste camada + células (`ta_flow::grid::create`).
    CreateGrid {
        aoi_id: String,
        name: String,
        resolution: f64,
        srid: i32,
    },
    /// Ativa o ruleset escolhido (`ta_flow::ruleset::activate`).
    ActivateRuleset {
        project_id: String,
        project_name: String,
        name: String,
        version: i32,
    },
    /// Importa medidas do CSV LONG (`ta_flow::measures::import_csv`).
    ImportMeasures { cell_layer_id: String, path: String },
    /// Cancelamento cooperativo da task (`ta_flow::queue::cancel_task`).
    CancelTask { task_id: String },
}

/// Direção do exchange de monitoramento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeMode {
    Export,
    Import,
}

/// Pedido de exchange: `project` (export) ou `into` (import) + arquivo.
#[derive(Debug, Clone)]
pub struct ExchangeRequest {
    pub mode: ExchangeMode,
    pub project: String,
    pub path: String,
    pub into: Option<String>,
}

/// Operador de processamento (espelha `ta_db::ops::ProcOp` para a UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcOp {
    Buffer,
    Intersect,
    Union,
}

/// Guarda de somente-leitura do console SQL: aceita apenas uma instrução
/// `SELECT`/`WITH` — proíbe `;` (múltiplas instruções) por conservadorismo.
/// A execução usa prepared statement, que de qualquer forma rejeitaria
/// multi-statement; a guard é defesa em profundidade na borda da UI.
///
/// # Limitação conhecida (documentada, não corrigida em v0.2)
///
/// A validação é **string-matching** (prefixo `select`/`with`), não análise
/// sintática: um `WITH x AS (SELECT …) INSERT INTO … SELECT …` em
/// instrução única começa com `with`, passa nesta guard e o prepared
/// statement a executa (PG aceita CTE precedendo `INSERT`). É o bypass
/// conhecido do console; o console é ferramenta admin/poder-usuário e a
/// mitigação de verdade (transação `READ ONLY` ou `SET TRANSACTION
/// READ ONLY` na sessão do console) fica para o backlog.
pub fn is_read_only_sql(sql: &str) -> bool {
    let t = sql.trim();
    if t.is_empty() || t.contains(';') {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("select") || lower.starts_with("with")
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
    // ---- v0.2 (paridade de UI) -------------------------------------
    /// Migrações reaplicadas com sucesso.
    MigrationsApplied,
    /// Texto de `version()` + `postgis_full_version()`.
    DbInfo(String),
    /// Camada removida no banco.
    LayerDeleted {
        layer_id: String,
    },
    /// Projeto removido no banco.
    ProjectDeleted {
        project_id: String,
    },
    /// Projeto renomeado (registro atualizado).
    ProjectRenamed(Project),
    /// Camada renomeada (registro atualizado).
    LayerRenamed(Layer),
    /// Camada derivada criada pelo processamento.
    Processed(Layer),
    /// Linhas do console SQL (JSON-texto por linha, máx. 200).
    QueryResult(Vec<Vec<String>>),
    // ---- Fase 5b (monitoramento) -----------------------------------
    /// Listas do projeto (fases, AOIs, grades, rascunhos, classes).
    MonitoringLoaded(MonitoringData),
    /// Células da viewport (`seq` descarta resposta de viewport antiga).
    CellViewLoaded {
        seq: u64,
        cells: Vec<CellViewRow>,
        truncated: bool,
    },
    /// Relatório de área (`seq` descarta resposta antiga).
    AreaReported {
        seq: u64,
        result: Result<AreaReport, String>,
    },
    /// Task criada e pronta para executar (`total` = células elegíveis).
    TaskCreated {
        task_id: String,
        draft_name: String,
        total: u64,
    },
    /// Progresso do executor (`RunEvent` — a GUI não estima).
    TaskProgress(RunEvent),
    /// Execução concluída (ou task já `done`).
    TaskFinished(RunOutcome),
    /// Falha ao criar/executar a task (mensagem acionável do `ta_flow`).
    TaskFailed(String),
    /// Bloqueio persistido (a UI atualiza o cache da viewport).
    CellBlocked {
        cell_layer_id: String,
        cell_id: String,
        blocked: bool,
    },
    /// Exchange concluído (resumo do binário da CLI).
    ExchangeDone {
        mode: ExchangeMode,
        path: String,
        message: String,
    },
    /// Exchange falhou (stderr do binário, sem DSN).
    ExchangeFailed {
        mode: ExchangeMode,
        message: String,
    },
    // ---- Fase 5b: preparo do fluxo pela GUI (B1) -------------------
    /// Fase criada (a janela recarrega as listas).
    PhaseCreated(Phase),
    /// AOI vinculada (id + nome da camada para exibição).
    AoiLinked {
        aoi_id: String,
        layer_name: String,
    },
    /// Grade criada (`cells` = total após a inserção).
    GridCreated {
        layer_id: String,
        name: String,
        resolution: f64,
        srid: i32,
        cells: u64,
    },
    /// Ruleset ativo do projeto (nome + versão).
    RulesetActivated {
        name: String,
        version: i32,
    },
    /// Medidas importadas (relatório lidas/gravadas/ignoradas).
    MeasuresImported(MeasuresImportReport),
    /// Cancelamento pedido; `done` são os processos já concluídos.
    TaskCancelled {
        task_id: String,
        done: i64,
        /// A task já estava cancelada (idempotente — nada mudou).
        already: bool,
    },
    /// Falha ao PEDIR o cancelamento (a task segue como estava).
    CancelFailed(String),
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
/// Emissor de eventos de uma operação **spawnada**: só entrega enquanto a
/// conexão que originou a operação é a corrente (`epoch`) e há sessão viva.
///
/// Sem isso, um run/exchange que termina depois de `Disconnect`/reconexão
/// pintaria erro de uma sessão que o usuário já fechou (e um `TaskFinished`
/// de um pool antigo sobrescreveria o estado da task nova).
#[derive(Clone)]
struct Emitter {
    tx: Sender<DbEvt>,
    epoch: Arc<AtomicU64>,
    generation: u64,
    connected: Arc<AtomicBool>,
}

impl Emitter {
    fn new(tx: &Sender<DbEvt>, epoch: &Arc<AtomicU64>, connected: &Arc<AtomicBool>) -> Self {
        Self {
            tx: tx.clone(),
            epoch: Arc::clone(epoch),
            generation: epoch.load(Ordering::Relaxed),
            connected: Arc::clone(connected),
        }
    }

    fn send(&self, evt: DbEvt) {
        if self.connected.load(Ordering::Relaxed)
            && self.epoch.load(Ordering::Relaxed) == self.generation
        {
            let _ = self.tx.send(evt);
        }
    }
}

/// Loop principal do worker. Erros de operação viram `DbEvt::Failed` com
/// mensagem legível; o worker nunca entra em panic por erro de banco.
///
/// Runtime **multi-thread** com 2 workers: o loop de comandos fica livre
/// enquanto um run/import/grade/exchange roda em `rt.spawn` (M2 da revisão).
fn run_worker(cmd_rx: Receiver<DbCmd>, evt_tx: Sender<DbEvt>) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
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

    // Estado de conexão (pool dentro da thread). O DSN fica só aqui para o
    // exchange repassar aos subprocessos da CLI — nunca vai para evento/log.
    let mut pool: Option<sqlx::PgPool> = None;
    let mut dsn: Option<String> = None;
    // Geração da conexão: operações spawnadas de um pool antigo não emitem
    // eventos (ver `Emitter`).
    let epoch = Arc::new(AtomicU64::new(0));
    let connected = Arc::new(AtomicBool::new(false));

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            DbCmd::Connect { url } => {
                let cfg = DbConfig {
                    url: url.clone(),
                    max_connections: 8,
                };
                match rt.block_on(async {
                    let pool: sqlx::PgPool = connect(&cfg).await?;
                    ensure_postgis(&pool).await?;
                    run_migrations(&pool).await?;
                    Ok::<sqlx::PgPool, ta_db::Error>(pool)
                }) {
                    Ok(p) => {
                        pool = Some(p);
                        dsn = Some(url);
                        connected.store(true, Ordering::Relaxed);
                        epoch.fetch_add(1, Ordering::Relaxed);
                        let _ = evt_tx.send(DbEvt::Connected);
                    }
                    Err(e) => {
                        // Mensagem legível; sem ecoar a senha do DSN.
                        pool = None;
                        dsn = None;
                        connected.store(false, Ordering::Relaxed);
                        epoch.fetch_add(1, Ordering::Relaxed);
                        let _ = evt_tx.send(DbEvt::ConnectFailed(format!(
                            "{e}\n(verifique host/porta/usuário/senha e se o \
                             PostGIS 3.4 está instalado no servidor)"
                        )));
                    }
                }
            }
            DbCmd::Disconnect => {
                // Invalida operações em voo e NÃO espera o pool fechar: um run
                // longo não pode travar o `Disconnect` (M2).
                connected.store(false, Ordering::Relaxed);
                epoch.fetch_add(1, Ordering::Relaxed);
                if let Some(p) = pool.take() {
                    rt.spawn(async move { p.close().await });
                }
                dsn = None;
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
            DbCmd::RunMigrations => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(run_migrations(&p)) {
                    Ok(()) => {
                        let _ = evt_tx.send(DbEvt::MigrationsApplied);
                    }
                    Err(e) => fail(&evt_tx, "aplicar migrações", e),
                }
            }
            DbCmd::DbInfo => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(async {
                    let version: (String,) =
                        sqlx::query_as("SELECT version()").fetch_one(&p).await?;
                    let postgis: (String,) = sqlx::query_as("SELECT postgis_full_version()")
                        .fetch_one(&p)
                        .await?;
                    Ok::<_, ta_db::Error>(format!("{}\n{}", version.0, postgis.0))
                }) {
                    Ok(text) => {
                        let _ = evt_tx.send(DbEvt::DbInfo(text));
                    }
                    Err(e) => fail(&evt_tx, "consultar informações do banco", e),
                }
            }
            DbCmd::DeleteLayer { layer_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgLayerRepo::new(p);
                match rt.block_on(repo.delete(&layer_id)) {
                    Ok(true) => {
                        let _ = evt_tx.send(DbEvt::LayerDeleted { layer_id });
                    }
                    Ok(false) => fail_msg(&evt_tx, "camada não existe no banco".into()),
                    Err(e) => fail(&evt_tx, "remover camada", e),
                }
            }
            DbCmd::DeleteProject { project_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgProjectRepo::new(p);
                match rt.block_on(repo.delete(&project_id)) {
                    Ok(true) => {
                        let _ = evt_tx.send(DbEvt::ProjectDeleted { project_id });
                    }
                    Ok(false) => fail_msg(&evt_tx, "projeto não existe no banco".into()),
                    Err(e) => fail(&evt_tx, "remover projeto", e),
                }
            }
            DbCmd::RenameProject {
                project_id,
                new_name,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgProjectRepo::new(p);
                match rt.block_on(async {
                    if !repo.rename(&project_id, &new_name).await? {
                        return Err(ta_db::Error::InvalidId(project_id.clone()));
                    }
                    repo.get(&project_id)
                        .await?
                        .ok_or_else(|| ta_db::Error::InvalidId(project_id.clone()))
                }) {
                    Ok(updated) => {
                        let _ = evt_tx.send(DbEvt::ProjectRenamed(updated));
                    }
                    Err(e) => fail(&evt_tx, "renomear projeto", e),
                }
            }
            DbCmd::RenameLayer { layer_id, new_name } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgLayerRepo::new(p);
                match rt.block_on(async {
                    if !repo.rename(&layer_id, &new_name).await? {
                        return Err(ta_db::Error::InvalidId(layer_id.clone()));
                    }
                    repo.get(&layer_id)
                        .await?
                        .ok_or_else(|| ta_db::Error::InvalidId(layer_id.clone()))
                }) {
                    Ok(updated) => {
                        let _ = evt_tx.send(DbEvt::LayerRenamed(updated));
                    }
                    Err(e) => fail(&evt_tx, "renomear camada", e),
                }
            }
            DbCmd::ProcessOp {
                op,
                project_id,
                source_a,
                source_b,
                distance_deg,
                target_name,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                use ta_db::ops;
                match rt.block_on(async {
                    match (op, source_b) {
                        (ProcOp::Buffer, _) => {
                            ops::buffer(&p, &project_id, &source_a, distance_deg, &target_name)
                                .await
                        }
                        (ProcOp::Intersect, Some(b)) => {
                            ops::intersection(&p, &project_id, &source_a, &b, &target_name).await
                        }
                        (ProcOp::Intersect, None) => {
                            Err(ta_db::Error::Postgis("interseção requer a camada B".into()))
                        }
                        (ProcOp::Union, _) => {
                            ops::union(&p, &project_id, &source_a, &target_name).await
                        }
                    }
                }) {
                    Ok(layer) => {
                        let _ = evt_tx.send(DbEvt::Processed(layer));
                    }
                    Err(e) => fail(&evt_tx, "processar", e),
                }
            }
            DbCmd::Query { sql } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                // Guarda de somente-leitura na borda do worker (a UI também
                // filtra — defesa em profundidade).
                if !is_read_only_sql(&sql) {
                    fail_msg(&evt_tx, "recusado: somente SELECT/WITH de leitura".into());
                    continue;
                }
                // AssertSqlSafe: o SQL vem do operador, por decisão dele —
                // guardado por `is_read_only_sql` (SELECT/WITH, sem `;`) e
                // executado como prepared statement (multi-statement já é
                // rejeitado pelo protocolo). Envolvido em row_to_json para
                // serializar colunas de tipos arbitrários como texto.
                let wrapped = format!("SELECT row_to_json(t)::text FROM ({sql}) t LIMIT 200");
                match rt.block_on(async {
                    let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(wrapped))
                        .fetch_all(&p)
                        .await?;
                    Ok::<_, sqlx::Error>(rows.into_iter().map(|(r,)| vec![r]).collect())
                }) {
                    Ok(rows) => {
                        let _ = evt_tx.send(DbEvt::QueryResult(rows));
                    }
                    Err(e) => fail(&evt_tx, "executar consulta", e),
                }
            }
            DbCmd::LoadMonitoring { project_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(async {
                    let phases = PgPhaseRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let aois = PgAoiLayerRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let cell_layers = PgCellLayerRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let drafts = PgDraftLayerRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let classes = PgClassRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let rulesets = PgRuleSetRepo::new(p.clone())
                        .list_for_project(&project_id)
                        .await?;
                    let active_ruleset = PgRuleSetRepo::new(p).find_active(&project_id).await?;
                    Ok::<_, ta_db::Error>(MonitoringData {
                        project_id,
                        phases,
                        aois,
                        cell_layers,
                        drafts,
                        classes,
                        rulesets,
                        active_ruleset,
                    })
                }) {
                    Ok(data) => {
                        let _ = evt_tx.send(DbEvt::MonitoringLoaded(data));
                    }
                    Err(e) => fail(&evt_tx, "carregar monitoramento", e),
                }
            }
            DbCmd::LoadCellView {
                seq,
                cell_layer_id,
                draft_layer_id,
                bbox,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgCellRepo::new(p);
                match rt.block_on(repo.viewport_on(
                    &cell_layer_id,
                    draft_layer_id.as_deref(),
                    bbox,
                    ta_db::repo_cell::MAX_VIEWPORT_CELLS,
                )) {
                    Ok((cells, truncated)) => {
                        let _ = evt_tx.send(DbEvt::CellViewLoaded {
                            seq,
                            cells,
                            truncated,
                        });
                    }
                    Err(e) => fail(&evt_tx, "carregar células da viewport", e),
                }
            }
            DbCmd::AreaReport {
                seq,
                cell_layer_id,
                draft_name,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let result = rt.block_on(area::report(
                    &p,
                    &cell_layer_id,
                    draft_name.as_deref(),
                    None,
                ));
                let _ = evt_tx.send(DbEvt::AreaReported {
                    seq,
                    result: result.map_err(|e| e.to_string()),
                });
            }
            DbCmd::ClassifyTask {
                project,
                cell_layer_id,
                draft_name,
                task_name,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                // create + run em tarefa SEPARADA (M2): o loop de comandos
                // segue atendendo viewport/cancelamento durante o run; os
                // eventos de progresso saem pelo canal.
                let em = Emitter::new(&evt_tx, &epoch, &connected);
                rt.spawn(async move {
                    let created = match queue::create_task(
                        &p,
                        &project,
                        &cell_layer_id,
                        None,
                        draft_name.as_deref(),
                        task_name.as_deref(),
                    )
                    .await
                    {
                        Ok(created) => created,
                        Err(e) => {
                            em.send(DbEvt::TaskFailed(e.to_string()));
                            return;
                        }
                    };
                    let total = u64::try_from(created.report.eligible.max(0)).unwrap_or(0);
                    let task_id = created.task.id.clone();
                    em.send(DbEvt::TaskCreated {
                        task_id: task_id.clone(),
                        draft_name: created.draft_name.clone(),
                        total,
                    });
                    let event_em = em.clone();
                    let mut on_event = move |event: RunEvent| {
                        event_em.send(DbEvt::TaskProgress(event));
                    };
                    match queue::run_task(&p, &task_id, None, &mut on_event).await {
                        Ok(outcome) => em.send(DbEvt::TaskFinished(outcome)),
                        Err(e) => em.send(DbEvt::TaskFailed(e.to_string())),
                    }
                });
            }
            DbCmd::SetCellBlocked {
                cell_layer_id,
                cell_id,
                blocked,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                let repo = PgBlockedCellRepo::new(p);
                let result = rt.block_on(async {
                    if blocked {
                        repo.block(&BlockedCell::new(&cell_layer_id, &cell_id, None))
                            .await
                            .map(|_| ())
                    } else {
                        repo.unblock(&cell_layer_id, &cell_id).await.map(|_| ())
                    }
                });
                match result {
                    Ok(()) => {
                        let _ = evt_tx.send(DbEvt::CellBlocked {
                            cell_layer_id,
                            cell_id,
                            blocked,
                        });
                    }
                    Err(e) => fail(
                        &evt_tx,
                        if blocked {
                            "bloquear célula"
                        } else {
                            "desbloquear célula"
                        },
                        e,
                    ),
                }
            }
            DbCmd::Exchange(req) => {
                let Some(url) = dsn.clone() else {
                    let _ = evt_tx.send(DbEvt::ExchangeFailed {
                        mode: req.mode,
                        message: "não conectado: use “Conectar banco” antes do exchange".into(),
                    });
                    continue;
                };
                // Subprocesso bloqueante → pool de blocking do runtime; o
                // loop de comandos nunca espera o `ta-cli` terminar (M2).
                let em = Emitter::new(&evt_tx, &epoch, &connected);
                rt.spawn_blocking(move || match run_exchange_cli(&req, &url) {
                    Ok(message) => em.send(DbEvt::ExchangeDone {
                        mode: req.mode,
                        path: req.path,
                        message,
                    }),
                    Err(message) => em.send(DbEvt::ExchangeFailed {
                        mode: req.mode,
                        message,
                    }),
                });
            }
            DbCmd::CreatePhase {
                project_id,
                name,
                description,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(phase::create(&p, &project_id, &name, description, 0, false)) {
                    Ok(saved) => {
                        let _ = evt_tx.send(DbEvt::PhaseCreated(saved));
                    }
                    Err(e) => fail(&evt_tx, "criar fase", e),
                }
            }
            DbCmd::LinkAoi {
                project_id,
                layer_id,
                id_column,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(aoi::create(&p, &project_id, &layer_id, id_column, None)) {
                    Ok(linked) => {
                        let _ = evt_tx.send(DbEvt::AoiLinked {
                            aoi_id: linked.aoi.id,
                            layer_name: linked.layer_name,
                        });
                    }
                    Err(e) => fail(&evt_tx, "vincular AOI", e),
                }
            }
            DbCmd::CreateGrid {
                aoi_id,
                name,
                resolution,
                srid,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                // Geração/persistência de grade pode ser grande → tarefa
                // separada (M2); a UI recebe o resultado por evento.
                let em = Emitter::new(&evt_tx, &epoch, &connected);
                rt.spawn(async move {
                    match grid::create(
                        &p,
                        &aoi_id,
                        resolution,
                        None,
                        None,
                        srid,
                        Some(name.as_str()),
                        None,
                    )
                    .await
                    {
                        Ok(created) => em.send(DbEvt::GridCreated {
                            layer_id: created.layer.id,
                            name: created.layer.name,
                            resolution: created.layer.resolution,
                            srid: created.layer.srid,
                            cells: created.count,
                        }),
                        Err(e) => em.send(DbEvt::Failed(format!("gerar grade: {e}"))),
                    }
                });
            }
            DbCmd::ActivateRuleset {
                project_id,
                project_name,
                name,
                version,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                match rt.block_on(ruleset::activate(
                    &p,
                    &project_id,
                    &project_name,
                    &name,
                    Some(version),
                )) {
                    Ok(active) => {
                        let _ = evt_tx.send(DbEvt::RulesetActivated {
                            name: active.name,
                            version: active.version,
                        });
                    }
                    Err(e) => fail(&evt_tx, "ativar ruleset", e),
                }
            }
            DbCmd::ImportMeasures {
                cell_layer_id,
                path,
            } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                // CSV grande + upsert → tarefa separada (M2).
                let em = Emitter::new(&evt_tx, &epoch, &connected);
                rt.spawn(async move {
                    match ta_flow::measures::import_csv(&p, &cell_layer_id, &path).await {
                        Ok(report) => em.send(DbEvt::MeasuresImported(report)),
                        Err(e) => em.send(DbEvt::Failed(format!("importar medidas: {e}"))),
                    }
                });
            }
            DbCmd::CancelTask { task_id } => {
                let Some(p) = pool.clone() else {
                    not_connected(&evt_tx);
                    continue;
                };
                // Cancelamento cooperativo: só marca a task; o run em voo
                // (outra tarefa do runtime) termina o lote e para no próximo
                // check entre lotes. A conexão é própria desta operação.
                match rt.block_on(queue::cancel_task(&p, &task_id)) {
                    Ok(queue::CancelOutcome::Cancelled { task_id, done }) => {
                        let _ = evt_tx.send(DbEvt::TaskCancelled {
                            task_id,
                            done,
                            already: false,
                        });
                    }
                    Ok(queue::CancelOutcome::AlreadyCancelled { task_id }) => {
                        let _ = evt_tx.send(DbEvt::TaskCancelled {
                            task_id,
                            done: 0,
                            already: true,
                        });
                    }
                    Err(e) => {
                        let _ = evt_tx.send(DbEvt::CancelFailed(e.to_string()));
                    }
                }
            }
        }
    }
    // Sessão encerrada: dá tempo das operações em voo terminarem, sem travar
    // o fechamento do processo para sempre.
    rt.shutdown_timeout(Duration::from_secs(5));
}

/// Argumentos do `ta-cli exchange …` (export/import) — puro, testável.
pub(crate) fn exchange_args(req: &ExchangeRequest) -> Result<Vec<String>, String> {
    let mut args = vec!["exchange".to_owned()];
    match req.mode {
        ExchangeMode::Export => {
            args.extend([
                "export".to_owned(),
                "--project".to_owned(),
                req.project.clone(),
                "--out".to_owned(),
                req.path.clone(),
            ]);
        }
        ExchangeMode::Import => {
            let into = req.into.as_deref().unwrap_or("").trim();
            if into.is_empty() {
                return Err("import exige o nome do projeto novo (--into)".into());
            }
            args.extend([
                "import".to_owned(),
                "--in".to_owned(),
                req.path.clone(),
                "--into".to_owned(),
                into.to_owned(),
            ]);
        }
    }
    Ok(args)
}

/// Teto do exchange (m5): o import é atômico e o export só publica o arquivo
/// no fim, mas um subprocesso travado (lock no banco, GDAL) não pode prender o
/// worker para sempre.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Executa o exchange no binário da CLI (mesma orquestração/validação).
///
/// O DSN vai por **ambiente** (`TA_PG_DSN`) — não aparece na linha de comando
/// (`ps`) nem na mensagem de erro. `stderr`/`stdout` do subprocesso viram o
/// resultado exibido no log/status da UI.
///
/// O processo é acompanhado por [`EXCHANGE_TIMEOUT`] (120 s) com `try_wait`;
/// estourou → `kill` e erro acionável com o motivo. A saída é pequena (resumo
/// do exchange) e os pipes só são lidos no fim — um filho que encha o buffer
/// (64 KiB) aparece como timeout, não como deadlock silencioso.
fn run_exchange_cli(req: &ExchangeRequest, dsn: &str) -> Result<String, String> {
    use std::process::Stdio;

    let args = exchange_args(req)?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("não foi possível localizar o executável: {e}"))?;
    let cli = crate::monitoring::resolve_cli(&exe);
    let mut child = std::process::Command::new(&cli)
        .args(&args)
        .env("TA_PG_DSN", dsn)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("não foi possível executar {}: {e}", cli.display()))?;
    let deadline = std::time::Instant::now() + EXCHANGE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "timeout de {} s: `{}` não terminou; o processo foi                          encerrado (o import do exchange é atômico; o export só                          publica o arquivo no fim — rode de novo)",
                        EXCHANGE_TIMEOUT.as_secs(),
                        cli.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!(
                    "não foi possível acompanhar {}: {e}",
                    cli.display()
                ));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("não foi possível ler a saída de {}: {e}", cli.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(if stdout.is_empty() {
            "concluído".to_owned()
        } else {
            stdout
        })
    } else if stderr.is_empty() {
        Err(if stdout.is_empty() {
            format!("ta-cli terminou com {}", output.status)
        } else {
            stdout
        })
    } else {
        Err(stderr)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_sql_somente_leitura() {
        // SELECT/WITH simples passam.
        assert!(is_read_only_sql("SELECT * FROM features"));
        assert!(is_read_only_sql("  select 1"));
        assert!(is_read_only_sql("WITH c AS (SELECT 1) SELECT * FROM c"));
        // Qualquer segunda instrução ou `;` solto é barrado (conservador).
        assert!(!is_read_only_sql("SELECT 1; DROP TABLE features"));
        assert!(!is_read_only_sql("SELECT 1;"));
        assert!(!is_read_only_sql("DROP TABLE features"));
        assert!(!is_read_only_sql("INSERT INTO features VALUES (1)"));
        assert!(!is_read_only_sql("UPDATE features SET attrs = '{}'"));
        assert!(!is_read_only_sql("DELETE FROM features"));
        assert!(!is_read_only_sql("   "));
        assert!(!is_read_only_sql("-- comentário\nDROP TABLE x"));
    }

    #[test]
    fn exchange_mapeia_para_os_args_da_cli() {
        let export = ExchangeRequest {
            mode: ExchangeMode::Export,
            project: "demo".into(),
            path: "/tmp/demo.json".into(),
            into: None,
        };
        assert_eq!(
            exchange_args(&export).unwrap(),
            [
                "exchange",
                "export",
                "--project",
                "demo",
                "--out",
                "/tmp/demo.json"
            ]
        );
        let import = ExchangeRequest {
            mode: ExchangeMode::Import,
            project: String::new(),
            path: "/tmp/demo.json".into(),
            into: Some("copia".into()),
        };
        assert_eq!(
            exchange_args(&import).unwrap(),
            [
                "exchange",
                "import",
                "--in",
                "/tmp/demo.json",
                "--into",
                "copia"
            ]
        );
        // Import sem nome novo é recusado antes de spawnar o binário.
        let empty = ExchangeRequest {
            into: Some("  ".into()),
            ..import
        };
        assert!(exchange_args(&empty).is_err());
    }
}
