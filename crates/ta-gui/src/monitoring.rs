//! **Monitoramento** (Fase 5b): estado e UI da visualização de AOI, células e
//! classes, do fluxo de classificação e da área consolidada.
//!
//! Regra arquitetural da fase: a GUI **orquestra** — lê pelo worker
//! ([`crate::db_worker`]), dispara `ta_flow` (fila/área) e desenha. Nada de
//! regra de domínio aqui: a decisão de classe vem de `draft_cells`
//! (persistida pelo executor), o bloqueio de `blocked_cells` e a área de
//! `ta_flow::area` — os mesmos caminhos da CLI.
//!
//! O módulo concentra três coisas testáveis **sem display**:
//! - [`MonitoringState`]: listas do projeto, seleção e estado da task (puro);
//! - cores/legenda e recorte de viewport ([`cell_fill`], [`legend_items`],
//!   [`needs_reload`]) — contrato de renderização sem snapshot gráfico;
//! - a janela egui ([`window`]) e a legenda do canvas ([`draw_legend`]).

use std::path::{Path, PathBuf};

use eframe::egui;
use egui::Color32;
use geo_types::{Geometry, Polygon};

use ta_core::aoi::AoiLayer;
use ta_core::area::AreaReport;
use ta_core::cell::CellLayer;
use ta_core::classification::{Class, DraftLayer};
use ta_core::layer::GeomType;
use ta_core::phase::Phase;
use ta_core::rule::RuleSet;
use ta_core::task::TaskStatus;
use ta_db::repo_cell::CellViewRow;
use ta_flow::measures::MeasuresImportReport;
use ta_flow::queue::{RunEvent, RunOutcome};

use crate::app::TerraApp;
use crate::app_state::GuiLayer;
use crate::db_worker::ExchangeMode;
use crate::i18n::Key as L;
use crate::map_view::{CellOverlay, CellShape};

/// Fator m² → hectare (mesma exibição do `area report` da CLI: 10 000 m²).
pub const M2_PER_HA: f64 = 10_000.0;

/// Listas de um projeto para o painel "Monitoramento" (uma leitura só).
#[derive(Debug, Clone, Default)]
pub struct MonitoringData {
    /// Projeto consultado (UUID) — descarta evento de projeto antigo.
    pub project_id: String,
    pub phases: Vec<Phase>,
    pub aois: Vec<AoiLayer>,
    pub cell_layers: Vec<CellLayer>,
    pub drafts: Vec<DraftLayer>,
    pub classes: Vec<Class>,
    /// Linhagens de ruleset do projeto (a ativa marca qual o `Classificar` usa).
    pub rulesets: Vec<RuleSet>,
    /// Ruleset ativo (`RuleSetRepo::find_active`) — `None` = classificar falha
    /// com mensagem acionável (nunca escolha silenciosa).
    pub active_ruleset: Option<RuleSet>,
}

/// Estado observável da task de classificação disparada pela GUI.
///
/// O contador vem dos [`RunEvent`]s do executor (`BatchOk`/`CellError`) — a
/// GUI não estima progresso por conta própria.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum TaskUi {
    /// Nenhuma task disparada nesta sessão.
    #[default]
    Idle,
    /// Executando (ou enfileirada): `total` = células elegíveis do `create`.
    Running {
        task_id: Option<String>,
        draft_name: Option<String>,
        total: Option<u64>,
        processed: u64,
        errors: u64,
        draft_rows: u64,
        last_error: Option<String>,
    },
    /// Concluída (o resumo da execução).
    Done {
        processed: u64,
        errors: u64,
        draft_rows: u64,
        /// Estado terminal do banco (`done`/`cancelled`/`failed`) — a janela
        /// traduz via [`status_key`] em vez de imprimir o texto cru.
        status: TaskStatus,
    },
    /// Falhou antes/durante a execução (mensagem do `ta_flow`/banco).
    Failed(String),
}

impl TaskUi {
    /// Há task em voo? A UI bloqueia um segundo "Classificar" enquanto `true`.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Contadores de uma task recém-criada (`TaskCreated`).
    pub fn begin(&mut self, task_id: String, draft_name: Option<String>, total: u64) {
        *self = Self::Running {
            task_id: Some(task_id),
            draft_name,
            total: Some(total),
            processed: 0,
            errors: 0,
            draft_rows: 0,
            last_error: None,
        };
    }

    /// Aplica um evento de progresso do executor.
    pub fn record(&mut self, event: &RunEvent) {
        let Self::Running {
            processed,
            errors,
            draft_rows,
            last_error,
            ..
        } = self
        else {
            return;
        };
        match event {
            RunEvent::BatchOk {
                cells,
                draft_rows: rows,
                ..
            } => {
                *processed += cells;
                *draft_rows += rows;
            }
            RunEvent::BatchFailed { reason } => *last_error = Some(reason.clone()),
            RunEvent::CellError { cell_id, reason } => {
                *errors += 1;
                *last_error = Some(format!("{cell_id}: {reason}"));
            }
        }
    }

    /// Fração concluída (`None` = total desconhecido/zero).
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Self::Running {
                total: Some(total),
                processed,
                ..
            } if *total > 0 => Some((*processed as f32 / *total as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    /// Encerra a task com o desfecho do executor, preservando as linhas de
    /// rascunho acumuladas no progresso (o `RunReport` não as repete).
    pub fn finish(&mut self, outcome: &RunOutcome) {
        let (draft_rows, previous_errors) = match self {
            Self::Running {
                draft_rows, errors, ..
            } => (*draft_rows, *errors),
            _ => (0, 0),
        };
        *self = match outcome {
            RunOutcome::Completed(report) => Self::Done {
                processed: report.processed,
                errors: report.errors,
                draft_rows,
                // `None` = cancelada: o estado terminal não é sobrescrito.
                status: report.final_status.unwrap_or(TaskStatus::Cancelled),
            },
            RunOutcome::AlreadyDone { .. } => Self::Done {
                processed: 0,
                errors: previous_errors,
                draft_rows,
                status: TaskStatus::Done,
            },
        };
    }
}

/// Chave i18n do estado terminal da task (nunca o texto cru do banco).
#[must_use]
pub fn status_key(status: TaskStatus) -> L {
    match status {
        TaskStatus::Pending => L::MonitoringStatusPending,
        TaskStatus::Running => L::MonitoringStatusRunning,
        TaskStatus::Done => L::MonitoringStatusDone,
        TaskStatus::Cancelled => L::MonitoringStatusCancelled,
        TaskStatus::Failed => L::MonitoringStatusFailed,
    }
}

/// Estado de UI do monitoramento (seleções + cache da viewport + área/task).
#[derive(Debug, Clone, Default)]
pub struct MonitoringState {
    /// Janela aberta?
    pub open: bool,
    pub data: MonitoringData,
    pub selected_aoi: Option<String>,
    pub selected_cell_layer: Option<String>,
    pub selected_draft: Option<String>,
    /// Células da viewport corrente (consulta por bbox da GUI).
    pub cells: Vec<CellViewRow>,
    /// A viewport devolveu mais células que o LIMIT (aviso na UI).
    pub cells_truncated: bool,
    /// Célula clicada (alvo de bloquear/desbloquear).
    pub selected_cell: Option<String>,
    /// Relatório de área da grade/rascunho corrente.
    pub area: Option<AreaReport>,
    pub task: TaskUi,
    /// Último bbox pedido e a chave (grade, rascunho) da consulta.
    pub last_bbox: Option<[f64; 4]>,
    pub last_key: Option<(Option<String>, Option<String>)>,
    /// Sequência da consulta de células (descarta resposta atrasada).
    pub view_seq: u64,
    /// Sequência da consulta de área (descarta resposta atrasada).
    pub area_seq: u64,
    // ---- Preparo do fluxo pela GUI (B1) --------------------------------
    /// Fase nova: nome e descrição (vazio = sem descrição).
    pub new_phase_name: String,
    pub new_phase_description: String,
    /// Camada vetorial eleita AOI e coluna de id opcional.
    pub aoi_layer_id: Option<String>,
    pub aoi_id_column: String,
    /// Grade nova: nome, resolução, EPSG (texto — validado ao clicar).
    pub new_grid_name: String,
    pub new_grid_resolution: String,
    pub new_grid_srid: String,
    /// Total de células da última grade criada (feedback acionável).
    pub grid_cells: Option<u64>,
    /// Ruleset escolhido para ativar (nome + versão).
    pub ruleset_name: String,
    pub ruleset_version: String,
    /// Caminho do CSV de medidas e o relatório do último import.
    pub measures_path: String,
    pub measures_report: Option<MeasuresImportReport>,
    /// Cancelamento já pedido (botão desabilitado até a task parar).
    pub cancel_requested: bool,
    /// Clique no canvas escolhe célula? (só com a janela aberta; M3)
    pub pick_cells: bool,
    /// Campo de caminho do exchange (export/import).
    pub exchange_path: String,
    /// Nome do projeto NOVO do import do exchange.
    pub exchange_into: String,
    /// Um exchange está em execução (botões desabilitados).
    pub exchange_busy: bool,
}

impl MonitoringState {
    /// Substitui as listas e reconcilia as seleções (ids ausentes → default).
    pub fn apply(&mut self, data: MonitoringData) {
        let keep_layer = data
            .cell_layers
            .iter()
            .any(|l| Some(&l.id) == self.selected_cell_layer.as_ref());
        let keep_aoi = data
            .aois
            .iter()
            .any(|a| Some(&a.id) == self.selected_aoi.as_ref());
        let keep_draft = data
            .drafts
            .iter()
            .any(|d| Some(&d.id) == self.selected_draft.as_ref());
        self.data = data;
        if !keep_aoi {
            self.selected_aoi = self.data.aois.first().map(|a| a.id.clone());
        }
        if !keep_layer {
            self.selected_cell_layer = self.data.cell_layers.first().map(|l| l.id.clone());
            self.selected_draft = None;
        }
        if !keep_draft || self.draft_of_layer().is_none() {
            self.selected_draft = self
                .data
                .drafts
                .iter()
                .find(|d| d.cell_layer_id == self.selected_cell_layer)
                .map(|d| d.id.clone());
        }
        // Ruleset a ativar: mantém a escolha se ainda existe; senão o ativo
        // (ou a primeira linhagem). A lista vem ordenada por nome/versão.
        let keep_ruleset =
            self.data.rulesets.iter().any(|r| {
                r.name == self.ruleset_name && r.version.to_string() == self.ruleset_version
            });
        if !keep_ruleset {
            match self
                .data
                .active_ruleset
                .clone()
                .or_else(|| self.data.rulesets.first().cloned())
            {
                Some(ruleset) => {
                    self.ruleset_name = ruleset.name;
                    self.ruleset_version = ruleset.version.to_string();
                }
                None => {
                    self.ruleset_name.clear();
                    self.ruleset_version.clear();
                }
            }
        }
    }

    /// Grade selecionada (atributo do projeto).
    #[must_use]
    pub fn cell_layer(&self) -> Option<&CellLayer> {
        let id = self.selected_cell_layer.as_ref()?;
        self.data.cell_layers.iter().find(|l| &l.id == id)
    }

    /// Rascunho selecionado, **se** pertence à grade selecionada.
    ///
    /// Exige grade selecionada (`selected_cell_layer.is_some()`) e compara a
    /// coluna `cell_layer_id` como `Option` (m4): rascunho órfão
    /// (`cell_layer_id = None`) nunca "pertence" à grade.
    #[must_use]
    pub fn draft_of_layer(&self) -> Option<&DraftLayer> {
        let layer_id = self.selected_cell_layer.as_ref()?;
        let draft_id = self.selected_draft.as_ref()?;
        let draft = self.data.drafts.iter().find(|d| &d.id == draft_id)?;
        (draft.cell_layer_id.as_ref() == Some(layer_id)).then_some(draft)
    }

    /// Clique no canvas pode escolher célula? (M3)
    ///
    /// Só com a janela ABERTA, o toggle “selecionar células” ligado e uma
    /// grade de overlay ativa; fora disso o clique é pick de feição normal.
    #[must_use]
    pub fn cell_pick_enabled(&self) -> bool {
        self.open && self.pick_cells && self.selected_cell_layer.is_some()
    }

    /// Rascunho em execução (nome vindo do `create`; n2 — visível na janela).
    #[must_use]
    pub fn running_draft(&self) -> Option<&str> {
        match &self.task {
            TaskUi::Running { draft_name, .. } => draft_name.as_deref(),
            _ => None,
        }
    }

    /// Nome do rascunho (o `ta_flow::area` resolve por NOME dentro do projeto).
    #[must_use]
    pub fn draft_name(&self) -> Option<&str> {
        self.draft_of_layer().map(|d| d.name.as_str())
    }

    /// AOI selecionada.
    #[must_use]
    pub fn aoi(&self) -> Option<&AoiLayer> {
        let id = self.selected_aoi.as_ref()?;
        self.data.aois.iter().find(|a| &a.id == id)
    }

    /// Seleciona a grade; o rascunho cai para o primeiro dela (ou nenhum).
    pub fn select_cell_layer(&mut self, id: &str) {
        self.selected_cell_layer = Some(id.to_owned());
        self.selected_draft = self
            .data
            .drafts
            .iter()
            .find(|d| d.cell_layer_id == self.selected_cell_layer)
            .map(|d| d.id.clone());
        self.selected_cell = None;
    }

    /// Seleciona a AOI (contorno no canvas).
    pub fn select_aoi(&mut self, id: &str) {
        self.selected_aoi = Some(id.to_owned());
    }

    /// Seleciona o rascunho (cores/área por classe).
    pub fn select_draft(&mut self, id: &str) {
        self.selected_draft = Some(id.to_owned());
    }

    /// Chave da consulta de células (grade, rascunho).
    #[must_use]
    pub fn view_key(&self) -> (Option<String>, Option<String>) {
        (
            self.selected_cell_layer.clone(),
            self.draft_of_layer().map(|d| d.id.clone()),
        )
    }

    /// Célula selecionada está bloqueada no cache atual?
    #[must_use]
    pub fn selected_cell_blocked(&self) -> bool {
        let Some(id) = self.selected_cell.as_ref() else {
            return false;
        };
        self.cells
            .iter()
            .find(|c| &c.cell_id == id)
            .is_some_and(|c| c.blocked)
    }

    /// Atualiza o flag de bloqueio da célula no cache (resposta do worker).
    pub fn set_cell_blocked(&mut self, cell_id: &str, blocked: bool) {
        if let Some(cell) = self.cells.iter_mut().find(|c| c.cell_id == cell_id) {
            cell.blocked = blocked;
        }
    }
}

// ---------------------------------------------------------------------------
// Contrato de renderização (cores/legenda) — puro, testável sem display
// ---------------------------------------------------------------------------

/// Cinza de célula bloqueada (precedência visual sobre a classe decidida).
pub const BLOCKED_FILL: Color32 = Color32::from_rgb(0x6b, 0x6b, 0x6b);

/// Cinza de célula sem decisão (nem classe, nem bloqueio).
pub const UNCLASSIFIED_FILL: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);

/// Alfa do preenchimento das células (o contorno da AOI/vetores continua
/// legível por cima).
pub const CELL_FILL_ALPHA: u8 = 150;

/// Cor de preenchimento de uma classe: **vem de `classes.color`** (`#RRGGBB`)
/// com o alfa do canvas; ausente/ inválida → cinza de "sem classe".
#[must_use]
pub fn class_fill(class: &Class) -> Color32 {
    let Some(hex) = class.color.as_deref() else {
        return UNCLASSIFIED_FILL;
    };
    // `Color32` guarda componentes **premultiplicados**: ler `.r()/.g()/.b()`
    // escureceria a cor; a leitura correta é `to_srgba_unmultiplied`.
    let [r, g, b, _] = crate::panels::hex_to_color32(hex).to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, CELL_FILL_ALPHA)
}

/// Preenchimento de uma célula: **bloqueio tem precedência**; depois a cor da
/// classe decidida no rascunho; sem decisão → cinza.
#[must_use]
pub fn cell_fill(cell: &CellViewRow, classes: &[Class]) -> Color32 {
    if cell.blocked {
        return BLOCKED_FILL;
    }
    cell.class_id
        .as_ref()
        .and_then(|id| classes.iter().find(|c| &c.id == id))
        .map_or(UNCLASSIFIED_FILL, class_fill)
}

/// Item da legenda do canvas (classe presente nas células, ou bloqueio).
#[derive(Debug, Clone, PartialEq)]
pub enum LegendItem {
    /// Classe decidida (nome de exibição + cor de `classes.color`).
    Class { name: String, color: Color32 },
    /// Células bloqueadas (cinza, precedência visual).
    Blocked { color: Color32 },
}

/// Legenda derivada das células visíveis: classes na ordem do cadastro e, por
/// último, o bloqueio — só entra o que está de fato no canvas.
#[must_use]
pub fn legend_items(cells: &[CellViewRow], classes: &[Class]) -> Vec<LegendItem> {
    let mut out = Vec::new();
    for class in classes {
        if cells
            .iter()
            .any(|c| !c.blocked && c.class_id.as_deref() == Some(class.id.as_str()))
        {
            out.push(LegendItem::Class {
                name: class.name.clone(),
                color: class_fill(class),
            });
        }
    }
    if cells.iter().any(|c| c.blocked) {
        out.push(LegendItem::Blocked {
            color: BLOCKED_FILL,
        });
    }
    out
}

/// Monta o overlay do canvas: contorno da AOI + células coloridas/bloqueadas.
#[must_use]
pub fn overlay(
    cells: &[CellViewRow],
    classes: &[Class],
    aoi: Vec<Polygon<f64>>,
    selected: Option<&str>,
) -> CellOverlay {
    CellOverlay {
        aoi,
        cells: cells
            .iter()
            .map(|c| CellShape {
                cell_id: c.cell_id.clone(),
                polygon: c.geom.clone(),
                fill: cell_fill(c, classes),
                blocked: c.blocked,
            })
            .collect(),
        selected: selected.map(str::to_owned),
    }
}

/// Polígonos das feições de uma camada de AOI (para o contorno do canvas).
#[must_use]
pub fn aoi_polygons(layers: &[GuiLayer], aoi: Option<&AoiLayer>) -> Vec<Polygon<f64>> {
    let Some(aoi) = aoi else {
        return Vec::new();
    };
    let Some(layer) = layers.iter().find(|g| g.layer.id == aoi.layer_id) else {
        return Vec::new();
    };
    layer
        .features
        .iter()
        .flat_map(|lf| polygons_of(&lf.feature.geom))
        .collect()
}

/// Extrai polígonos de uma geometria (as demais variantes não têm área).
fn polygons_of(geom: &Geometry<f64>) -> Vec<Polygon<f64>> {
    match geom {
        Geometry::Polygon(p) => vec![p.clone()],
        Geometry::MultiPolygon(mp) => mp.0.clone(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Viewport: quando pedir as células
// ---------------------------------------------------------------------------

/// A viewport mudou o bastante para justificar nova consulta?
///
/// Tolerância generosa (20% do centro, 25% do tamanho) evita uma query por
/// frame de pan/zoom; troca de grade/rascunho força a consulta. O recorte é
/// sempre server-side (bbox + LIMIT) — a GUI nunca carrega a grade inteira.
#[must_use]
pub fn needs_reload(previous: Option<[f64; 4]>, next: [f64; 4], key_changed: bool) -> bool {
    if key_changed {
        return true;
    }
    if ![next[0], next[1], next[2], next[3]]
        .iter()
        .all(|v| v.is_finite())
        || next[0] >= next[2]
        || next[1] >= next[3]
    {
        return false;
    }
    let Some(prev) = previous else {
        return true;
    };
    let prev_cx = (prev[0] + prev[2]) / 2.0;
    let prev_cy = (prev[1] + prev[3]) / 2.0;
    let next_cx = (next[0] + next[2]) / 2.0;
    let next_cy = (next[1] + next[3]) / 2.0;
    let prev_span = (prev[2] - prev[0])
        .max(prev[3] - prev[1])
        .max(f64::MIN_POSITIVE);
    let next_span = (next[2] - next[0])
        .max(next[3] - next[1])
        .max(f64::MIN_POSITIVE);
    let center_moved = ((next_cx - prev_cx).hypot(next_cy - prev_cy)) > 0.20 * next_span;
    let span_changed = (next_span / prev_span - 1.0).abs() > 0.25;
    center_moved || span_changed
}

/// Área em hectares com 2 casas (mesma formatação do `area report`).
#[must_use]
pub fn format_ha(area_m2: f64) -> String {
    format!("{:.2}", area_m2 / M2_PER_HA)
}

// ---------------------------------------------------------------------------
// Exchange: invocação do MESMO binário da CLI (sem duplicar a orquestração)
// ---------------------------------------------------------------------------

/// Candidatos a binário do `ta-cli`, do mais específico ao mais genérico:
/// `CURUPIRA_TA_CLI`, ao lado do executável, no diretório pai (caso do
/// `target/debug/deps/` dos testes) e, por fim, o `PATH`.
#[must_use]
pub fn cli_candidates(exe: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("CURUPIRA_TA_CLI") {
        out.push(PathBuf::from(path));
    }
    if let Some(dir) = exe.parent() {
        out.push(dir.join("ta-cli"));
        if let Some(parent) = dir.parent() {
            out.push(parent.join("ta-cli"));
        }
    }
    out
}

/// Resolve o `ta-cli` executável (o exchange roda no binário existente).
///
/// `CURUPIRA_TA_CLI` é **override explícito**: quando definido, é usado como
/// veio (mesmo inexistente — o erro de spawn cita o caminho, em vez de cair
/// em silêncio para outro binário). Sem override: candidatos locais e, por
/// último, `ta-cli` do `PATH`.
#[must_use]
pub fn resolve_cli(exe: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("CURUPIRA_TA_CLI") {
        return PathBuf::from(path);
    }
    cli_candidates(exe)
        .into_iter()
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("ta-cli"))
}

// ---------------------------------------------------------------------------
// Janela "Monitoramento" (egui)
// ---------------------------------------------------------------------------

/// Ações coletadas dentro da janela e aplicadas depois (evita empréstimo
/// conflitante de `TerraApp` dentro do closure do egui).
enum Action {
    Refresh,
    Classify,
    CancelTask(String),
    SelectAoi(String),
    SelectCellLayer(String),
    SelectDraft(String),
    SelectRuleset(String, i32),
    ZoomAoi,
    ToggleBlock,
    CreatePhase,
    LinkAoi,
    CreateGrid,
    ActivateRuleset,
    ImportMeasures,
    Export,
    Import,
}

/// Janela flutuante do Monitoramento (Fase 5b).
pub fn window(ui: &mut egui::Ui, app: &mut TerraApp) {
    if !app.monitoring.open {
        // Janela fechada: o overlay de células não fica no canvas (e o clique
        // volta a escolher feições normalmente — M3).
        if app.map.overlay().is_some() {
            app.map.set_cell_overlay(None);
        }
        return;
    }
    let mut open = app.monitoring.open;
    let mut actions: Vec<Action> = Vec::new();
    // Camadas elegíveis a AOI (poligonais): a GUI não deixa vincular raster.
    let vector_layers: Vec<(String, String)> = app
        .state
        .layers
        .iter()
        .filter(|g| {
            matches!(
                g.layer.geom_type,
                GeomType::Polygon | GeomType::MultiPolygon | GeomType::GeometryCollection
            )
        })
        .map(|g| (g.layer.id.clone(), g.layer.name.clone()))
        .collect();
    egui::Window::new(app.t(L::MonitoringTitle))
        .open(&mut open)
        .default_width(360.0)
        .resizable(true)
        .show(ui.ctx(), |ui| {
            let Some(project) = app.state.project.clone() else {
                ui.weak(app.t(L::MonitoringNoProject));
                return;
            };
            ui.horizontal(|ui| {
                ui.label(&project.name);
                if ui.button(app.t(L::MonitoringRefresh)).clicked() {
                    actions.push(Action::Refresh);
                }
                let has_layer = app.monitoring.selected_cell_layer.is_some();
                let ready =
                    app.state.conn.is_db() && has_layer && !app.monitoring.task.is_running();
                let mut resp =
                    ui.add_enabled(ready, egui::Button::new(app.t(L::MonitoringClassify)));
                if app.monitoring.task.is_running() {
                    resp = resp.on_disabled_hover_text(app.t(L::MonitoringTaskRunning));
                } else if !has_layer {
                    resp = resp.on_disabled_hover_text(app.t(L::DisabledNoCellLayer));
                }
                if resp.clicked() {
                    actions.push(Action::Classify);
                }
                // M1: cancelamento cooperativo visível enquanto a task roda.
                if let TaskUi::Running {
                    task_id: Some(task_id),
                    ..
                } = &app.monitoring.task
                {
                    let task_id = task_id.clone();
                    if app.monitoring.cancel_requested {
                        ui.add_enabled(false, egui::Button::new(app.t(L::MonitoringCancelPending)));
                    } else if ui.button(app.t(L::MonitoringCancel)).clicked() {
                        actions.push(Action::CancelTask(task_id));
                    }
                }
            });
            ui.horizontal(|ui| {
                // M3: só com este toggle (e a janela aberta) o clique escolhe
                // célula; desligado, o clique é pick de feição normal.
                let pick_label = app.t(L::MonitoringPickCells);
                ui.checkbox(&mut app.monitoring.pick_cells, pick_label);
                match &app.monitoring.data.active_ruleset {
                    Some(active) => {
                        ui.weak(format!(
                            "{}: {} v{}",
                            app.t(L::MonitoringRulesetActive),
                            active.name,
                            active.version
                        ));
                    }
                    None => {
                        ui.colored_label(
                            Color32::from_rgb(0xD4, 0xAC, 0x0D),
                            app.t(L::MonitoringNoActiveRuleset),
                        );
                    }
                }
            });
            ui.separator();

            // Fases (leitura; a seleção de fase fica na CLI nesta fase).
            ui.label(app.t(L::MonitoringPhases));
            if app.monitoring.data.phases.is_empty() {
                ui.weak(app.t(L::MonitoringEmpty));
            }
            for phase in &app.monitoring.data.phases {
                let blocker = if phase.cell_blocker { " ⛔" } else { "" };
                ui.weak(format!("• {}{blocker}", phase.name));
            }
            // Nova fase (B1: o fluxo inteiro pela GUI, sem CLI).
            ui.label(app.t(L::MonitoringNewPhase));
            ui.horizontal(|ui| {
                ui.label(app.t(L::MonitoringPhaseName));
                ui.text_edit_singleline(&mut app.monitoring.new_phase_name);
            });
            ui.horizontal(|ui| {
                ui.label(app.t(L::MonitoringPhaseDescription));
                ui.text_edit_singleline(&mut app.monitoring.new_phase_description);
            });
            if ui
                .add_enabled(
                    app.state.conn.is_db(),
                    egui::Button::new(app.t(L::MonitoringPhaseCreate)),
                )
                .on_disabled_hover_text(app.t(L::DisabledNoDb))
                .clicked()
            {
                actions.push(Action::CreatePhase);
            }

            ui.separator();
            ui.label(app.t(L::MonitoringAoiLayers));
            if app.monitoring.data.aois.is_empty() {
                ui.weak(app.t(L::MonitoringEmpty));
            }
            for aoi in &app.monitoring.data.aois {
                let name = app
                    .state
                    .layers
                    .iter()
                    .find(|g| g.layer.id == aoi.layer_id)
                    .map_or_else(|| short_id(&aoi.layer_id), |g| g.layer.name.clone());
                let selected = app.monitoring.selected_aoi.as_deref() == Some(aoi.id.as_str());
                if ui.selectable_label(selected, format!("▲ {name}")).clicked() {
                    actions.push(Action::SelectAoi(aoi.id.clone()));
                }
            }
            if ui.button(app.t(L::MonitoringZoomAoi)).clicked() {
                actions.push(Action::ZoomAoi);
            }

            // Vincular uma camada vetorial existente como AOI (B1).
            ui.label(app.t(L::MonitoringNewAoi));
            if vector_layers.is_empty() {
                ui.weak(app.t(L::MonitoringAoiNoLayer));
            } else {
                if app.monitoring.aoi_layer_id.is_none() {
                    app.monitoring.aoi_layer_id = vector_layers.first().map(|(id, _)| id.clone());
                }
                let selected = app
                    .monitoring
                    .aoi_layer_id
                    .as_ref()
                    .and_then(|id| vector_layers.iter().find(|(lid, _)| lid == id))
                    .map_or_else(|| app.t(L::MonitoringAoiLayer), |(_, name)| name.as_str());
                egui::ComboBox::from_id_salt("monitoring_aoi_layer")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (id, name) in &vector_layers {
                            let is_selected =
                                app.monitoring.aoi_layer_id.as_deref() == Some(id.as_str());
                            if ui.selectable_label(is_selected, name).clicked() {
                                app.monitoring.aoi_layer_id = Some(id.clone());
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label(app.t(L::MonitoringAoiIdColumn));
                    ui.text_edit_singleline(&mut app.monitoring.aoi_id_column);
                });
                let ready = app.state.conn.is_db() && app.monitoring.aoi_layer_id.is_some();
                if ui
                    .add_enabled(ready, egui::Button::new(app.t(L::MonitoringAoiLink)))
                    .on_disabled_hover_text(app.t(L::DisabledNoDb))
                    .clicked()
                {
                    actions.push(Action::LinkAoi);
                }
            }

            ui.separator();
            ui.label(app.t(L::MonitoringCellLayers));
            if app.monitoring.data.cell_layers.is_empty() {
                ui.weak(app.t(L::MonitoringEmpty));
            }
            for layer in &app.monitoring.data.cell_layers {
                let selected =
                    app.monitoring.selected_cell_layer.as_deref() == Some(layer.id.as_str());
                if ui
                    .selectable_label(selected, format!("▦ {}", layer.name))
                    .on_hover_text(format!(
                        "{}: {} · EPSG:{}",
                        app.t(L::MonitoringResolution),
                        layer.resolution,
                        layer.srid
                    ))
                    .clicked()
                {
                    actions.push(Action::SelectCellLayer(layer.id.clone()));
                }
            }

            // Gerar grade sobre a AOI selecionada (B1).
            ui.label(app.t(L::MonitoringNewGrid));
            if app.monitoring.selected_aoi.is_none() {
                ui.weak(app.t(L::MonitoringGridHint));
            } else {
                ui.horizontal(|ui| {
                    ui.label(app.t(L::MonitoringGridName));
                    ui.text_edit_singleline(&mut app.monitoring.new_grid_name);
                });
                ui.horizontal(|ui| {
                    ui.label(app.t(L::MonitoringGridResolution));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.monitoring.new_grid_resolution)
                            .desired_width(70.0),
                    );
                    ui.label(app.t(L::MonitoringGridSrid));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.monitoring.new_grid_srid)
                            .desired_width(60.0),
                    );
                });
                if ui
                    .add_enabled(
                        app.state.conn.is_db(),
                        egui::Button::new(app.t(L::MonitoringGridCreate)),
                    )
                    .on_disabled_hover_text(app.t(L::DisabledNoDb))
                    .clicked()
                {
                    actions.push(Action::CreateGrid);
                }
                if let Some(cells) = app.monitoring.grid_cells {
                    ui.weak(format!(
                        "{}: {cells} {}",
                        app.t(L::MonitoringGridCreated),
                        app.t(L::MonitoringTaskCells)
                    ));
                }
            }

            ui.separator();
            ui.label(app.t(L::MonitoringDrafts));
            let drafts: Vec<_> = app
                .monitoring
                .data
                .drafts
                .iter()
                .filter(|d| d.cell_layer_id == app.monitoring.selected_cell_layer)
                .cloned()
                .collect();
            if drafts.is_empty() {
                ui.weak(app.t(L::MonitoringNoDraft));
            }
            for draft in drafts {
                let selected = app.monitoring.selected_draft.as_deref() == Some(draft.id.as_str());
                if ui
                    .selectable_label(selected, format!("✎ {}", draft.name))
                    .clicked()
                {
                    actions.push(Action::SelectDraft(draft.id.clone()));
                }
            }

            // Rulesets do projeto: escolher a linhagem/versão e ativar (B1).
            ui.separator();
            ui.label(app.t(L::MonitoringRulesets));
            if app.monitoring.data.rulesets.is_empty() {
                ui.weak(app.t(L::MonitoringRulesetNone));
            }
            for ruleset in &app.monitoring.data.rulesets {
                let selected = app.monitoring.ruleset_name == ruleset.name
                    && app.monitoring.ruleset_version == ruleset.version.to_string();
                let active = app
                    .monitoring
                    .data
                    .active_ruleset
                    .as_ref()
                    .is_some_and(|a| a.id == ruleset.id);
                let suffix = if active {
                    format!(" ({})", app.t(L::MonitoringRulesetActive))
                } else {
                    String::new()
                };
                if ui
                    .selectable_label(
                        selected,
                        format!("⚖ {} v{}{suffix}", ruleset.name, ruleset.version),
                    )
                    .clicked()
                {
                    actions.push(Action::SelectRuleset(ruleset.name.clone(), ruleset.version));
                }
            }
            let can_activate = app.state.conn.is_db() && !app.monitoring.ruleset_name.is_empty();
            if ui
                .add_enabled(
                    can_activate,
                    egui::Button::new(app.t(L::MonitoringRulesetActivate)),
                )
                .on_disabled_hover_text(if app.monitoring.data.rulesets.is_empty() {
                    app.t(L::MonitoringRulesetNone)
                } else {
                    app.t(L::MonitoringRulesetSelectHint)
                })
                .clicked()
            {
                actions.push(Action::ActivateRuleset);
            }

            // Import de medidas do CSV LONG (mesma regra do `measure` da CLI).
            ui.separator();
            ui.label(app.t(L::MonitoringMeasures));
            ui.horizontal(|ui| {
                ui.label(app.t(L::MonitoringMeasuresPath));
                ui.text_edit_singleline(&mut app.monitoring.measures_path);
            });
            let can_import = app.state.conn.is_db()
                && app.monitoring.selected_cell_layer.is_some()
                && !app.monitoring.measures_path.trim().is_empty();
            if ui
                .add_enabled(
                    can_import,
                    egui::Button::new(app.t(L::MonitoringMeasuresImport)),
                )
                .on_disabled_hover_text(if app.monitoring.selected_cell_layer.is_none() {
                    app.t(L::DisabledNoCellLayer)
                } else {
                    app.t(L::MonitoringMeasuresHint)
                })
                .clicked()
            {
                actions.push(Action::ImportMeasures);
            }
            if let Some(report) = &app.monitoring.measures_report {
                ui.weak(format!(
                    "{}: {} {} · {} {} · {} {}",
                    app.t(L::MonitoringMeasuresImported),
                    report.read,
                    app.t(L::MonitoringMeasuresRead),
                    report.written,
                    app.t(L::MonitoringMeasuresWritten),
                    report.skipped,
                    app.t(L::MonitoringMeasuresSkipped),
                ));
                for issue in ta_flow::measures::issues_for_display(report, 5) {
                    ui.weak(format!("  ⚠ {}: {}", issue.line, issue.reason));
                }
            }

            ui.separator();
            ui.label(app.t(L::MonitoringClasses));
            for class in &app.monitoring.data.classes {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 2.0, class_fill(class).gamma_multiply(2.0));
                    let suffix = if class.is_active {
                        String::new()
                    } else {
                        format!(" ({})", app.t(L::MonitoringInactive))
                    };
                    ui.label(format!("{}{suffix}", class.name));
                });
            }

            // Célula selecionada → bloqueio (persiste em `blocked_cells`).
            ui.separator();
            match app.monitoring.selected_cell.clone() {
                Some(cell_id) => {
                    ui.label(format!("{}: {cell_id}", app.t(L::MonitoringCellSelected)));
                    let blocked = app.monitoring.selected_cell_blocked();
                    let label = if blocked {
                        app.t(L::MonitoringUnblock)
                    } else {
                        app.t(L::MonitoringBlock)
                    };
                    if ui.button(label).clicked() {
                        actions.push(Action::ToggleBlock);
                    }
                }
                None => {
                    ui.weak(app.t(L::MonitoringNoCellSelected));
                }
            }

            // Progresso da task (contador do executor; sem barra falsa).
            if !matches!(app.monitoring.task, TaskUi::Idle) {
                ui.separator();
                match &app.monitoring.task {
                    TaskUi::Idle => {}
                    TaskUi::Running {
                        processed,
                        errors,
                        total,
                        ..
                    } => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!(
                                "{} {processed}{}",
                                app.t(L::MonitoringTaskRunning),
                                total.map(|t| format!("/{t}")).unwrap_or_default()
                            ));
                            if *errors > 0 {
                                ui.colored_label(
                                    Color32::from_rgb(0xE7, 0x4C, 0x3C),
                                    format!("{} {errors}", app.t(L::MonitoringTaskErrors)),
                                );
                            }
                        });
                        // n2: nome do rascunho em execução (vem do `create`).
                        if let Some(draft) = app.monitoring.running_draft() {
                            ui.weak(format!("{}: {draft}", app.t(L::MonitoringTaskDraft)));
                        }
                        if app.monitoring.cancel_requested {
                            ui.weak(app.t(L::MonitoringCancelRequested));
                        }
                        if let Some(fraction) = app.monitoring.task.fraction() {
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .text(format!("{:.0}%", fraction * 100.0)),
                            );
                        }
                    }
                    TaskUi::Done {
                        processed,
                        errors,
                        draft_rows,
                        status,
                    } => {
                        ui.label(format!(
                            "{}: {processed} · {} {errors} · {} {draft_rows} ({})",
                            app.t(L::MonitoringTaskDone),
                            app.t(L::MonitoringTaskErrors),
                            app.t(L::MonitoringTaskCells),
                            app.t(status_key(*status)),
                        ));
                    }
                    TaskUi::Failed(message) => {
                        ui.colored_label(
                            Color32::from_rgb(0xE7, 0x4C, 0x3C),
                            format!("{}: {message}", app.t(L::MonitoringTaskFailed)),
                        );
                    }
                }
            }

            // Área consolidada (números de `ta_flow::area`; só formata em ha).
            ui.separator();
            ui.label(app.t(L::MonitoringArea));
            match &app.monitoring.area {
                Some(report) => {
                    ui.weak(format!("EPSG:{} ({})", report.srs, report.srs_units));
                    egui::Grid::new("monitoring_area_grid")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label(app.t(L::MonitoringAreaTotal));
                            ui.label(format!(
                                "{} ({} {})",
                                format_ha(report.summary.total_m2),
                                report.summary.cells_total,
                                app.t(L::MonitoringTaskCells)
                            ));
                            ui.end_row();
                            ui.label(app.t(L::MonitoringAreaBlocked));
                            ui.label(format!(
                                "{} ({} {})",
                                format_ha(report.summary.blocked_m2),
                                report.summary.cells_blocked,
                                app.t(L::MonitoringTaskCells)
                            ));
                            ui.end_row();
                            ui.label(app.t(L::MonitoringAreaAvailable));
                            ui.label(format_ha(report.summary.available_m2));
                            ui.end_row();
                        });
                    if report.by_class.is_empty() {
                        ui.weak(app.t(L::MonitoringAreaEmpty));
                    } else {
                        ui.label(app.t(L::MonitoringAreaByClass));
                        for class in &report.by_class {
                            ui.label(format!(
                                "  {} · {} {} · {} ha",
                                class.class_name,
                                class.cells,
                                app.t(L::MonitoringTaskCells),
                                format_ha(class.area_m2)
                            ));
                        }
                    }
                }
                None => {
                    ui.weak(app.t(L::MonitoringAreaEmpty));
                }
            }

            // Aviso honesto do LIMIT da viewport.
            if app.monitoring.cells_truncated {
                ui.colored_label(
                    Color32::from_rgb(0xD4, 0xAC, 0x0D),
                    app.t(L::MonitoringTruncated),
                );
            }

            // Exchange: chama o binário da CLI (mesma orquestração/resultado).
            ui.separator();
            ui.label(app.t(L::MonitoringExchange));
            ui.horizontal(|ui| {
                ui.label(app.t(L::MonitoringExchangePath));
                ui.text_edit_singleline(&mut app.monitoring.exchange_path);
            });
            ui.horizontal(|ui| {
                let ready = !app.monitoring.exchange_busy
                    && !app.monitoring.exchange_path.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(app.t(L::MonitoringExchangeExport)))
                    .clicked()
                {
                    actions.push(Action::Export);
                }
                if ui
                    .add_enabled(ready, egui::Button::new(app.t(L::MonitoringExchangeImport)))
                    .clicked()
                {
                    actions.push(Action::Import);
                }
            });
            ui.horizontal(|ui| {
                ui.label(app.t(L::MonitoringExchangeInto));
                ui.text_edit_singleline(&mut app.monitoring.exchange_into);
            });
        });
    app.monitoring.open = open;

    for action in actions {
        match action {
            Action::Refresh => app.request_monitoring(),
            Action::Classify => app.classify_selected(),
            Action::CancelTask(task_id) => app.cancel_task(&task_id),
            Action::SelectAoi(id) => {
                app.monitoring.select_aoi(&id);
                app.refresh_monitoring_overlay();
            }
            Action::SelectCellLayer(id) => {
                app.monitoring.select_cell_layer(&id);
                app.on_monitoring_selection_changed();
            }
            Action::SelectDraft(id) => {
                app.monitoring.select_draft(&id);
                app.on_monitoring_selection_changed();
            }
            Action::SelectRuleset(name, version) => {
                app.monitoring.ruleset_name = name;
                app.monitoring.ruleset_version = version.to_string();
            }
            Action::ZoomAoi => app.zoom_to_aoi(),
            Action::ToggleBlock => app.toggle_selected_cell_block(),
            Action::CreatePhase => app.create_phase(),
            Action::LinkAoi => app.link_aoi(),
            Action::CreateGrid => app.create_grid(),
            Action::ActivateRuleset => app.activate_ruleset(),
            Action::ImportMeasures => app.import_measures(),
            Action::Export => {
                if let Some(project) = app.state.project.clone() {
                    app.run_exchange(ExchangeMode::Export, project.name, None);
                }
            }
            Action::Import => {
                let into = app.monitoring.exchange_into.trim().to_owned();
                if into.is_empty() {
                    app.error = Some(app.t(L::MonitoringExchangeIntoHint).to_owned());
                } else {
                    app.run_exchange(ExchangeMode::Import, String::new(), Some(into));
                }
            }
        }
    }
}

/// Desenha a legenda do canvas (canto inferior direito) — só quando há
/// overlay de células; itens derivados do que está VISÍVEL.
pub fn draw_legend(painter: &egui::Painter, app: &TerraApp, rect: egui::Rect) {
    let items = legend_items(&app.monitoring.cells, &app.monitoring.data.classes);
    if items.is_empty() {
        return;
    }
    let visuals = painter.ctx().global_style().visuals.clone();
    let line_h = 16.0;
    let width = 210.0;
    let height = line_h * (items.len() as f32 + 1.0) + 8.0;
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - width - 8.0, rect.bottom() - height - 8.0),
        egui::vec2(width, height),
    );
    painter.rect_filled(box_rect, 4.0, visuals.extreme_bg_color.gamma_multiply(0.85));
    painter.text(
        egui::pos2(box_rect.left() + 8.0, box_rect.top() + 4.0),
        egui::Align2::LEFT_TOP,
        app.t(L::MonitoringLegend),
        egui::FontId::proportional(12.0),
        visuals.text_color(),
    );
    for (idx, item) in items.iter().enumerate() {
        let y = box_rect.top() + line_h * (idx as f32 + 1.0) + 4.0;
        let (label, color) = match item {
            LegendItem::Class { name, color } => (name.clone(), *color),
            LegendItem::Blocked { color } => (app.t(L::MonitoringBlocked).to_owned(), *color),
        };
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(box_rect.left() + 8.0, y + 2.0),
                egui::vec2(10.0, 10.0),
            ),
            1.0,
            color,
        );
        painter.text(
            egui::pos2(box_rect.left() + 24.0, y),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::proportional(12.0),
            visuals.text_color(),
        );
    }
}

/// Id curto (8 primeiros caracteres) para objetos sem nome de exibição.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{Coord, LineString};

    fn cell(id: &str, class: Option<&str>, blocked: bool) -> CellViewRow {
        CellViewRow {
            cell_id: id.to_owned(),
            geom: Polygon::new(
                LineString::from(vec![
                    Coord { x: 0.0, y: 0.0 },
                    Coord { x: 1.0, y: 0.0 },
                    Coord { x: 1.0, y: 1.0 },
                    Coord { x: 0.0, y: 0.0 },
                ]),
                vec![],
            ),
            class_id: class.map(str::to_owned),
            blocked,
        }
    }

    fn class(id: &str, color: &str) -> Class {
        let mut c = Class::new("p1", id);
        c.id = id.to_owned();
        c.with_color(Some(color))
    }

    /// Contrato de renderização: a cor de uma célula classificada é a de
    /// `classes.color`.
    #[test]
    fn cor_da_celula_classificada_vem_de_classes_color() {
        let classes = [class("floresta", "#12AB34")];
        let fill = cell_fill(&cell("0_0", Some("floresta"), false), &classes);
        // A célula classificada usa exatamente a cor da classe (alfa do canvas).
        assert_eq!(fill, class_fill(&classes[0]));
        assert_eq!(fill.a(), CELL_FILL_ALPHA);
        // E a cor é dado de `classes.color`: trocar a cor muda o preenchimento.
        let outra = [class("floresta", "#FF0000")];
        assert_ne!(
            cell_fill(&cell("0_0", Some("floresta"), false), &outra),
            fill
        );
        // Fidelidade: recuperar os componentes devolve o hex original (só o
        // arredondamento do alfa premultiplicado varia alguns níveis).
        let [r, g, b, _] = fill.to_srgba_unmultiplied();
        for (got, want) in [(r, 0x12i32), (g, 0xAB), (b, 0x34)] {
            assert!(
                (i32::from(got) - want).abs() <= 3,
                "componente {got:#x} ≠ {want:#x} (cor da classe escurecida?)"
            );
        }
    }

    /// Bloqueio tem precedência visual: cinza mesmo com classe decidida.
    #[test]
    fn celula_bloqueada_tem_precedencia_sobre_a_classe() {
        let classes = [class("floresta", "#12AB34")];
        let blocked = cell_fill(&cell("0_0", Some("floresta"), true), &classes);
        assert_eq!(blocked, BLOCKED_FILL);
        assert_ne!(
            blocked,
            class_fill(&classes[0]),
            "bloqueio não pode usar a cor da classe"
        );
    }

    /// Sem decisão/sem cadastro de cor → cinza de "sem classe" (nunca panic).
    #[test]
    fn celula_sem_decisao_ou_sem_cor_cai_no_cinza() {
        let sem_classe = cell_fill(&cell("0_0", None, false), &[]);
        assert_eq!(sem_classe, UNCLASSIFIED_FILL);
        let sem_cor = [Class::new("p1", "sem-cor")];
        assert_eq!(
            cell_fill(&cell("0_0", Some("sem-cor"), false), &sem_cor),
            UNCLASSIFIED_FILL
        );
    }

    /// A legenda lista só classes presentes + bloqueio (sem entradas mortas).
    #[test]
    fn legenda_deriva_das_celulas_visiveis() {
        let classes = [class("floresta", "#00FF00"), class("agua", "#0000FF")];
        let cells = [
            cell("0_0", Some("floresta"), false),
            cell("1_0", Some("floresta"), true),
        ];
        let items = legend_items(&cells, &classes);
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], LegendItem::Class { name, .. } if name == "floresta"));
        assert!(matches!(&items[1], LegendItem::Blocked { .. }));
    }

    /// Viewport: força reload na troca de grade/rascunho e evita query por
    /// frame de pan pequeno; zoom/pan grande pede de novo.
    #[test]
    fn recorte_de_viewport_evita_query_por_frame() {
        let bbox = [-52.2, -3.2, -52.1, -3.1];
        assert!(needs_reload(None, bbox, false));
        assert!(!needs_reload(Some(bbox), bbox, false));
        assert!(
            needs_reload(Some(bbox), bbox, true),
            "troca de camada força"
        );
        // Pan de 1% do span: não recarrega.
        let small = [-52.199, -3.2, -52.099, -3.1];
        assert!(!needs_reload(Some(bbox), small, false));
        // Zoom out de 2×: recarrega.
        let wide = [-52.3, -3.3, -52.0, -3.0];
        assert!(needs_reload(Some(bbox), wide, false));
        // bbox inválida não dispara consulta.
        assert!(!needs_reload(Some(bbox), [0.0, 0.0, 0.0, 1.0], false));
    }

    /// Área só formata (m² → ha) — o número vem de `ta_flow::area`.
    #[test]
    fn area_e_formatada_em_hectares_como_na_cli() {
        assert_eq!(format_ha(10_000.0), "1.00");
        assert_eq!(format_ha(44_500_321.5), "4450.03");
        assert_eq!(format_ha(0.0), "0.00");
    }

    /// Progresso lido dos eventos do executor (sem estimativa na GUI).
    #[test]
    fn progresso_da_task_vem_dos_run_events() {
        let mut task = TaskUi::default();
        assert!(!task.is_running());
        task.begin("t1".into(), Some("r".into()), 10);
        assert!(task.is_running());
        assert_eq!(task.fraction(), Some(0.0));
        task.record(&RunEvent::BatchOk {
            cells: 4,
            draft_rows: 4,
            logs: 4,
        });
        task.record(&RunEvent::CellError {
            cell_id: "9_9".into(),
            reason: "boom".into(),
        });
        assert_eq!(task.fraction(), Some(0.4));
        if let TaskUi::Running {
            processed,
            errors,
            draft_rows,
            last_error,
            ..
        } = &task
        {
            assert_eq!((*processed, *errors, *draft_rows), (4, 1, 4));
            assert!(last_error.as_deref().is_some_and(|m| m.contains("9_9")));
        } else {
            panic!("task deveria continuar Running");
        }
    }

    /// Seleção reconcilia rascunho da MESMA grade e descarta ids sumidos.
    #[test]
    fn selecao_reconcilia_rascunho_com_a_grade() {
        let spec = ta_core::cell::GridSpec::new(1.0, 0.0, 0.0, 4674);
        let mut g1 = CellLayer::new("p1", "aoi", "g1", &spec);
        g1.id = "g1".into();
        let mut g2 = CellLayer::new("p1", "aoi", "g2", &spec);
        g2.id = "g2".into();
        let mut draft_a = DraftLayer::new("p1", "a");
        draft_a.id = "da".into();
        draft_a.cell_layer_id = Some("g1".into());
        let mut draft_b = DraftLayer::new("p1", "b");
        draft_b.id = "db".into();
        draft_b.cell_layer_id = Some("g2".into());
        let mut aoi = AoiLayer::new("p1", "l1", None);
        aoi.id = "aoi".into();
        let mut st = MonitoringState::default();
        st.apply(MonitoringData {
            project_id: "p1".into(),
            phases: Vec::new(),
            aois: vec![aoi],
            cell_layers: vec![g1.clone(), g2.clone()],
            drafts: vec![draft_a, draft_b],
            classes: Vec::new(),
            rulesets: Vec::new(),
            active_ruleset: None,
        });
        // Default: primeira grade e o rascunho DESSA grade.
        assert_eq!(st.selected_cell_layer.as_deref(), Some("g1"));
        assert_eq!(st.selected_draft.as_deref(), Some("da"));
        assert_eq!(st.draft_name(), Some("a"));

        // Trocar de grade reconcilia o rascunho (nunca cruza grades).
        st.select_cell_layer("g2");
        assert_eq!(st.selected_draft.as_deref(), Some("db"));
        assert_eq!(st.draft_name(), Some("b"));

        // Id sumido no refresh → volta para o primeiro da grade.
        st.selected_draft = Some("sumiu".into());
        st.apply(MonitoringData {
            project_id: "p1".into(),
            phases: Vec::new(),
            aois: Vec::new(),
            cell_layers: vec![g1, g2],
            drafts: vec![],
            classes: Vec::new(),
            rulesets: Vec::new(),
            active_ruleset: None,
        });
        assert_eq!(st.selected_draft, None);
        assert!(st.draft_of_layer().is_none());
    }

    /// m4: sem grade selecionada ou com rascunho órfão (`None`), não há
    /// rascunho "da grade" — nunca comparar `None` com id por engano.
    #[test]
    fn draft_of_layer_exige_grade_e_compara_option() {
        let spec = ta_core::cell::GridSpec::new(1.0, 0.0, 0.0, 4674);
        let mut grid = CellLayer::new("p1", "aoi", "g", &spec);
        grid.id = "g".into();
        let mut orphan = DraftLayer::new("p1", "orfao");
        orphan.id = "d".into();
        orphan.cell_layer_id = None;
        let mut st = MonitoringState {
            selected_cell_layer: Some("g".into()),
            selected_draft: Some("d".into()),
            ..MonitoringState::default()
        };
        st.data.cell_layers = vec![grid];
        st.data.drafts = vec![orphan];
        assert!(
            st.draft_of_layer().is_none(),
            "rascunho sem grade não pertence a grade nenhuma"
        );
        st.selected_cell_layer = None;
        assert!(st.draft_of_layer().is_none(), "sem grade não há rascunho");
    }

    /// M3: o clique só escolhe célula com a janela aberta, o toggle ligado e
    /// uma grade de overlay selecionada.
    #[test]
    fn clique_de_celula_exige_janela_toggle_e_grade() {
        let mut st = MonitoringState {
            selected_cell_layer: Some("g".into()),
            ..MonitoringState::default()
        };
        assert!(!st.cell_pick_enabled(), "janela fechada não escolhe célula");
        st.open = true;
        assert!(
            !st.cell_pick_enabled(),
            "toggle desligado não escolhe célula"
        );
        st.pick_cells = true;
        assert!(st.cell_pick_enabled());
        st.selected_cell_layer = None;
        assert!(!st.cell_pick_enabled(), "sem overlay ativo não há célula");
    }

    /// Ruleset a ativar: escolha preservada enquanto existe; sumiu → o ativo
    /// (ou a primeira linhagem), nunca um nome fora do projeto.
    #[test]
    fn selecao_reconcilia_ruleset_escolhido_com_o_ativo() {
        let mut chosen = RuleSet::new("p1", "floresta");
        chosen.id = "r1".into();
        chosen.version = 2;
        let mut other = RuleSet::new("p1", "agua");
        other.id = "r2".into();
        other.version = 1;
        let mut st = MonitoringState::default();
        st.apply(MonitoringData {
            project_id: "p1".into(),
            rulesets: vec![chosen.clone(), other.clone()],
            active_ruleset: Some(chosen.clone()),
            ..MonitoringData::default()
        });
        assert_eq!(st.ruleset_name, "floresta");
        assert_eq!(st.ruleset_version, "2");

        // Escolha manual sobrevive ao refresh enquanto o par existe.
        st.ruleset_name = "agua".into();
        st.ruleset_version = "1".into();
        st.apply(MonitoringData {
            project_id: "p1".into(),
            rulesets: vec![chosen, other.clone()],
            active_ruleset: Some(other),
            ..MonitoringData::default()
        });
        assert_eq!(
            (st.ruleset_name.as_str(), st.ruleset_version.as_str()),
            ("agua", "1")
        );

        // Projeto sem ruleset: campos vazios (o botão Ativar desabilita).
        st.apply(MonitoringData {
            project_id: "p1".into(),
            ..MonitoringData::default()
        });
        assert!(st.ruleset_name.is_empty() && st.ruleset_version.is_empty());
    }

    /// Desfecho do executor vira `Done` preservando as linhas de rascunho
    /// acumuladas no progresso (o relatório não as repete).
    #[test]
    fn finish_mapeia_desfecho_e_preserva_rascunho() {
        use ta_db::repo_task::TaskCounts;
        use ta_flow::queue::RunReport;

        let mut task = TaskUi::default();
        task.begin("t1".into(), Some("rascunho".into()), 10);
        task.record(&RunEvent::BatchOk {
            cells: 4,
            draft_rows: 7,
            logs: 4,
        });
        task.record(&RunEvent::CellError {
            cell_id: "0_1".into(),
            reason: "fora da regra".into(),
        });
        assert_eq!(task.fraction(), Some(0.4));

        let report = RunReport {
            processed: 10,
            errors: 1,
            elapsed: std::time::Duration::ZERO,
            classify_time: std::time::Duration::ZERO,
            cancelled: false,
            final_status: Some(TaskStatus::Done),
            counts: TaskCounts::default(),
        };
        task.finish(&RunOutcome::Completed(report.clone()));
        assert_eq!(
            task,
            TaskUi::Done {
                processed: 10,
                errors: 1,
                draft_rows: 7,
                status: TaskStatus::Done,
            }
        );
        assert!(!task.is_running());

        // Reexecução de task já concluída: nada processado, mas os erros e o
        // rascunho observados nesta sessão continuam visíveis.
        let mut again = TaskUi::default();
        again.begin("t1".into(), None, 10);
        again.record(&RunEvent::BatchOk {
            cells: 2,
            draft_rows: 3,
            logs: 2,
        });
        again.record(&RunEvent::CellError {
            cell_id: "0_2".into(),
            reason: "falhou".into(),
        });
        again.finish(&RunOutcome::AlreadyDone {
            task_id: "t1".into(),
        });
        assert_eq!(
            again,
            TaskUi::Done {
                processed: 0,
                errors: 1,
                draft_rows: 3,
                status: TaskStatus::Done,
            }
        );

        // Cancelada (`final_status` nulo) não herda status terminal inventado.
        let mut cancelled = TaskUi::default();
        cancelled.begin("t2".into(), None, 1);
        cancelled.finish(&RunOutcome::Completed(RunReport {
            final_status: None,
            ..report
        }));
        match cancelled {
            TaskUi::Done { status, .. } => assert_eq!(status, TaskStatus::Cancelled),
            other => panic!("esperava Done, veio {other:?}"),
        }
    }

    /// Candidatos do binário do exchange: env → ao lado → diretório pai.
    #[test]
    fn candidatos_do_ta_cli_priorizam_o_vizinho_do_executavel() {
        let exe = Path::new("/opt/curupira/bin/curupira-gis");
        let candidates = cli_candidates(exe);
        assert!(candidates.contains(&PathBuf::from("/opt/curupira/bin/ta-cli")));
        assert!(candidates.contains(&PathBuf::from("/opt/curupira/ta-cli")));
    }
}
