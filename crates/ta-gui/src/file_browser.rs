//! Navegador de arquivos embutido (egui) — v0.2.1.
//!
//! Motivação: o diálogo nativo (`rfd`) não existe mais como caminho único —
//! de fato **nunca existiu** neste repo (o diálogo de import sempre foi um
//! campo de texto). O navegador embutido é idêntico em Windows e Linux por
//! construção (é 100% egui) e resolve busca/navegação de primeira classe:
//!
//! - breadcrumb clicável + subir/home/projeto/testdata;
//! - campo de caminho editável (Enter navega, erro inline);
//! - filtro por substring (case-insensitive) na pasta atual, com toggle de
//!   busca em subpastas (walk std com depth limitado, cancelável);
//! - painel de detalhes (nome, tamanho, formato, modificação);
//! - seção "Recentes" (arquivos) compartilhada com o menu Arquivo;
//! - teclado: setas navegam, Enter abre, Esc fecha, Ctrl+F foca o filtro,
//!   F3 pula para a próxima ocorrência da busca em subpastas.
//!
//! Toda a lógica de sistema de arquivos é **função pura** testável headless;
//! a UI apenas orquestra. I/O pesado não existe aqui (listagem de um dir é
//! local; o walk é thread separada com cancel flag).

use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::SystemTime;

use eframe::egui;

use crate::app::TerraApp;
use crate::i18n::Key as L;
use crate::i18n::Lang;

// ===========================================================================
// Lógica pura (headless-testável)
// ===========================================================================

/// Classe de uma entrada (define ícone e se é importável).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    Shapefile,
    GeoJson,
    GeoPackage,
    FlatGeobuf,
    Raster,
    Csv,
    Other,
}

impl EntryKind {
    /// Classifica por nome + flag de diretório (sem tocar no disco).
    pub fn from_name(name: &str, is_dir: bool) -> Self {
        if is_dir {
            return Self::Dir;
        }
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "shp" => Self::Shapefile,
            "geojson" => Self::GeoJson,
            "gpkg" => Self::GeoPackage,
            "fgb" => Self::FlatGeobuf,
            "tif" | "tiff" => Self::Raster,
            "csv" => Self::Csv,
            _ => Self::Other,
        }
    }

    /// Glifo suplementar ao nome (a11y: o nome textual é a informação real).
    pub fn icon(self) -> &'static str {
        match self {
            Self::Dir => "📁",
            Self::Shapefile => "🔷",
            Self::GeoJson => "🌐",
            Self::GeoPackage => "🗃️",
            Self::FlatGeobuf => "🧱",
            Self::Raster => "🛰️",
            Self::Csv => "📊",
            Self::Other => "▫️",
        }
    }

    /// Importável pelo pipeline atual (mesmos formatos do `ta-gis::detect`
    /// + GeoTIFF do leitor de raster).
    pub fn is_importable(self) -> bool {
        !matches!(self, Self::Dir | Self::Other)
    }
}

/// Entrada listada pelo navegador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Bytes (arquivos; diretórios ficam sem tamanho).
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// Lista `dir` ordenado (pastas primeiro, depois nome case-insensitive).
/// Entradas ilegíveis são puladas (nunca panic).
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let is_dir = ft.is_dir();
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        out.push(FileEntry {
            kind: EntryKind::from_name(&name, is_dir),
            name: name.clone(),
            path: entry.path(),
            size: if is_dir {
                None
            } else {
                meta.as_ref().map(|m| m.len())
            },
            modified: meta.and_then(|m| m.modified().ok()),
        });
    }
    out.sort_by(
        |a, b| match (a.kind == EntryKind::Dir, b.kind == EntryKind::Dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        },
    );
    Ok(out)
}

/// Filtro case-insensitive por substring no nome (vazio = tudo passa).
pub fn name_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// Profundidade máxima da busca em subpastas (raiz = nível 0; 3 níveis
/// abaixo dela). Limite std — sem deps novas (sem jwalk/scanlex).
pub const WALK_MAX_DEPTH: usize = 3;

/// Teto de resultados da busca em subpastas (proteção de UI).
pub const MAX_WALK_RESULTS: usize = 500;

/// Job da busca em subpastas (thread + cancel flag compartilhada).
pub struct WalkJob {
    pub rx: Receiver<Vec<FileEntry>>,
    pub cancel: Arc<AtomicBool>,
}

impl WalkJob {
    /// Pede cancelamento cooperativo (checado por diretório).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Sobe a thread de busca (lote único ao final; cancelável entre dirs).
pub fn spawn_walk(root: PathBuf, query: String) -> WalkJob {
    spawn_walk_capped(root, query, MAX_WALK_RESULTS)
}

/// Variante com teto injetável (testes); produção usa [`MAX_WALK_RESULTS`].
fn spawn_walk_capped(root: PathBuf, query: String, max: usize) -> WalkJob {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = Arc::clone(&cancel);
    std::thread::Builder::new()
        .name("ta-browser-walk".into())
        .spawn(move || {
            let mut out = Vec::new();
            walk_search(&root, &query, WALK_MAX_DEPTH, max, &cancel_thread, &mut out);
            // Ordem determinística (mesma da listagem): pastas não existem
            // aqui; nome case-insensitive.
            out.sort_by_key(|e| e.name.to_lowercase());
            let _ = tx.send(out);
        })
        .expect("spawn da thread de busca do navegador");
    WalkJob { rx, cancel }
}

/// Walk recursivo com depth limitado (raiz = nível 0; desce até `depth`
/// níveis abaixo) e teto de resultados. Arquivos que casam
/// (case-insensitive) vão para `out`.
pub fn walk_search(
    dir: &Path,
    query: &str,
    depth: usize,
    max: usize,
    cancel: &AtomicBool,
    out: &mut Vec<FileEntry>,
) {
    if cancel.load(Ordering::Relaxed) || out.len() >= max {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // dir ilegível: pula silencioso (nunca panic)
    };
    for entry in entries.flatten() {
        if cancel.load(Ordering::Relaxed) || out.len() >= max {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            if depth > 0 {
                walk_search(&entry.path(), query, depth - 1, max, cancel, out);
            }
        } else if name_matches(&name, query) {
            let meta = entry.metadata().ok();
            out.push(FileEntry {
                kind: EntryKind::from_name(&name, false),
                name: name.clone(),
                path: entry.path(),
                size: meta.as_ref().map(|m| m.len()),
                modified: meta.and_then(|m| m.modified().ok()),
            });
        }
    }
}

/// Resultado da validação do campo de caminho.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidPath {
    Dir(PathBuf),
    File(PathBuf),
    NotFound,
}

/// Valida o texto digitado/colado (expande `~`; sem panic).
pub fn validate_path(input: &str) -> ValidPath {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ValidPath::NotFound;
    }
    let expanded = expand_tilde(trimmed);
    let p = PathBuf::from(&expanded);
    if p.is_dir() {
        ValidPath::Dir(p)
    } else if p.is_file() {
        ValidPath::File(p)
    } else {
        ValidPath::NotFound
    }
}

/// Expande `~/resto` (ou `~` puro) com o HOME fornecido — puro.
/// No Windows, `HOME` costuma não existir; `USERPROFILE` cobre.
pub fn expand_tilde_with(input: &str, home: Option<&str>) -> String {
    let Some(rest) = input.strip_prefix('~') else {
        return input.to_owned();
    };
    let Some(home) = home.filter(|h| !h.trim().is_empty()) else {
        return input.to_owned();
    };
    if rest.is_empty() {
        return home.to_owned();
    }
    // `~/…` e `~\…` (Win) expandem; `~user` NÃO (sem resolução de usuários).
    if rest.starts_with('/') || rest.starts_with('\\') {
        format!("{home}{rest}")
    } else {
        input.to_owned()
    }
}

/// HOME do ambiente real (Win: `USERPROFILE`).
pub fn home_dir_from_env() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|h| !h.trim().is_empty())
}

/// `expand_tilde` com o ambiente real.
pub fn expand_tilde(input: &str) -> String {
    expand_tilde_with(input, home_dir_from_env().as_deref())
}

/// Procura `testdata` do repo a partir do dir do binário (dev: ex.
/// `repo/target/debug` → `repo/testdata`). `None` = oculta o atalho.
pub fn testdata_dir_from(exe_dir: &Path) -> Option<PathBuf> {
    let mut p = exe_dir.to_path_buf();
    for _ in 0..4 {
        let cand = p.join("testdata");
        if cand.is_dir() {
            return Some(cand);
        }
        if !p.pop() {
            return None;
        }
    }
    None
}

/// Pasta "do projeto": diretório-pai do `source` da camada ativa quando é
/// um caminho de arquivo existente. Sem caminho útil → `None`.
pub fn project_dir_from(source: Option<&str>) -> Option<PathBuf> {
    let src = source?.trim();
    if src.is_empty() {
        return None;
    }
    let p = PathBuf::from(src);
    p.parent()
        .filter(|d| d.is_dir())
        .map(std::path::Path::to_path_buf)
}

/// Tamanho legível ("1,5 MB" em pt-BR; "1.5 MB" em en).
/// Unidades são iguais nos dois idiomas (dados, não rótulos).
pub fn format_size(bytes: u64, lang: Lang) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let sep = if lang == Lang::PtBr { ',' } else { '.' };
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    // 1 casa decimal com separador do idioma (Rust sempre imprime '.').
    let scaled = (value * 10.0).round() / 10.0;
    let int_part = scaled.trunc() as u64;
    let frac = ((scaled - int_part as f64) * 10.0).round() as u64;
    format!("{int_part}{sep}{frac} {}", UNITS[unit])
}

/// "AAAA-MM-DD HH:MM" (UTC) — algoritmo civil-from-days (Hinnant), sem deps.
pub fn format_mtime(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, mi) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// Dias desde a época → (ano, mês, dia). Algoritmo de Hinnant (domínio
/// público), versão compacta — testado contra datas conhecidas.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Plano do payload de drag & drop (puro, sem tocar no disco).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DropPlan {
    /// Suportados, na ordem recebida (1º pré-preenche o modal, resto fila).
    pub importable: Vec<PathBuf>,
    /// Ignorados por extensão não suportada.
    pub ignored: usize,
}

/// Classifica os caminhos soltos na janela: suportados entram, o resto
/// conta como ignorado. Diretórios soltos (sem extensão útil) caem fora.
pub fn plan_drop(paths: &[PathBuf]) -> DropPlan {
    let mut plan = DropPlan::default();
    for p in paths {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if EntryKind::from_name(&name, false).is_importable() {
            plan.importable.push(p.clone());
        } else {
            plan.ignored += 1;
        }
    }
    plan
}

// ===========================================================================
// Estado do navegador
// ===========================================================================

/// Estado do navegador de arquivos (um por app; campo `browser` do app).
#[derive(Default)]
pub struct BrowserState {
    /// Janela aberta?
    pub open: bool,
    /// Diretório corrente (fonte da listagem).
    pub cwd: PathBuf,
    /// Texto do campo de caminho editável.
    pub path_edit: String,
    /// Último texto validado era inexistente (erro inline).
    pub path_error: bool,
    /// Filtro por nome (substring case-insensitive).
    pub filter: String,
    /// Toggle "buscar em subpastas".
    pub search_subdirs: bool,
    /// Listagem da pasta corrente (sem filtro aplicado).
    entries: Vec<FileEntry>,
    /// Entrada selecionada (painel de detalhes).
    pub selected: Option<FileEntry>,
    /// Busca em subpastas em andamento (loading state).
    walk: Option<WalkJob>,
    /// Resultados acumulados da busca em subpastas.
    pub walk_results: Vec<FileEntry>,
    /// Query que originou `walk_results` (evita re-spawn a cada tecla).
    walk_query: Option<(String, PathBuf)>,
    /// 1º `open` semeia `cwd` (recente → home → cwd do processo).
    seeded: bool,
}

impl BrowserState {
    /// Abre o navegador em `dir` (ou no seed padrão se `dir` não existe).
    pub fn open_at(&mut self, dir: PathBuf) {
        let dir = if dir.is_dir() { dir } else { self.seed_dir() };
        self.open = true;
        self.navigate(dir);
    }

    /// Diretório inicial: mais recente válido → HOME → cwd do processo.
    fn seed_dir(&self) -> PathBuf {
        if let Some(cwd) = std::env::current_dir().ok().filter(|p| p.is_dir()) {
            return cwd;
        }
        PathBuf::from(MAIN_SEPARATOR.to_string())
    }

    /// Navega para `dir` (se for dir válido) e relista. Devolve sucesso.
    /// Recentes são registrados pelo chamador (precisa de `&mut TerraApp`).
    pub fn navigate(&mut self, dir: PathBuf) -> bool {
        match list_dir(&dir) {
            Ok(entries) => {
                self.cwd = dir;
                self.path_edit = self.cwd.to_string_lossy().to_string();
                self.path_error = false;
                self.entries = entries;
                self.selected = None;
                self.cancel_walk();
                true
            }
            Err(_) => {
                self.path_error = true;
                false
            }
        }
    }

    /// Cancela busca em andamento e limpa resultados.
    pub fn cancel_walk(&mut self) {
        if let Some(job) = &self.walk {
            job.cancel();
        }
        self.walk = None;
        self.walk_results.clear();
        self.walk_query = None;
    }

    /// Linhas visíveis: filtro local na pasta corrente, ou resultados da
    /// busca em subpastas quando o toggle está ligado.
    pub fn rows(&self) -> Vec<FileEntry> {
        if self.search_subdirs && !self.filter.trim().is_empty() {
            return self.walk_results.clone();
        }
        self.entries
            .iter()
            .filter(|e| name_matches(&e.name, &self.filter))
            .cloned()
            .collect()
    }

    /// Busca em subpastas em andamento?
    pub fn walking(&self) -> bool {
        self.walk.is_some()
    }

    /// Drena o canal do walk: acumula lotes e detecta o fim (sender
    /// dropado → `Disconnected`). `Empty` = thread ainda trabalhando.
    fn drain_walk(&mut self) {
        let Some(job) = &self.walk else { return };
        while let Ok(batch) = job.rx.try_recv() {
            self.walk_results.extend(batch);
        }
        if matches!(
            job.rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ) {
            self.walk = None;
        }
    }

    /// (Re)inicia a busca em subpastas se a query/dir mudou.
    fn ensure_walk(&mut self) {
        let query = self.filter.trim().to_owned();
        if query.is_empty() {
            self.cancel_walk();
            return;
        }
        let fresh = self
            .walk_query
            .as_ref()
            .is_none_or(|(q, dir)| *q != query || *dir != self.cwd);
        if fresh {
            self.cancel_walk();
            self.walk_query = Some((query.clone(), self.cwd.clone()));
            self.walk = Some(spawn_walk(self.cwd.clone(), query));
        }
    }
}

// ===========================================================================
// UI (egui) — idêntica em Windows/Linux por construção
// ===========================================================================

/// IDs de foco (Ctrl+F, setas × edição de texto).
const ID_SEARCH: &str = "ta_browser_search";
const ID_PATH: &str = "ta_browser_path";

/// Modal do navegador (chamado por frame quando `app.browser.open`).
pub fn file_browser_dialog(ctx: &egui::Context, app: &mut TerraApp) {
    handle_browser_keys(ctx, app);
    app.browser.drain_walk();
    app.browser.ensure_walk();
    if !app.browser.seeded {
        // 1ª abertura sem open_at explícito: semeia pelo dir atual/home.
        let seed = app.browser.seed_dir();
        app.browser.navigate(seed);
        app.browser.seeded = true;
    }
    egui::Modal::new(egui::Id::unique("ta_file_browser")).show(ctx, |ui| {
        ui.set_min_width(780.0);
        ui.heading(app.t(L::FileBrowserTitle));
        ui.separator();

        // -- linha de atalhos --------------------------------------------
        ui.horizontal(|ui| {
            let home = home_dir_from_env().map(PathBuf::from);
            if ui
                .add_enabled(home.is_some(), egui::Button::new("⌂"))
                .on_hover_text(app.t(L::FileBrowserHome))
                .clicked()
            {
                browser_navigate(app, home.unwrap_or_default());
            }
            let proj = project_dir_from(active_layer_source(app));
            if ui
                .add_enabled(
                    proj.is_some(),
                    egui::Button::new(app.t(L::FileBrowserProject)),
                )
                .on_hover_text(app.t(L::FileBrowserProject))
                .clicked()
            {
                browser_navigate(app, proj.unwrap_or_default());
            }
            // Atalho testdata: só aparece quando existe ao lado do binário.
            if let Some(testdata) = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().and_then(testdata_dir_from))
            {
                if ui
                    .button(app.t(L::FileBrowserTestdata))
                    .on_hover_text(app.t(L::FileBrowserTestdata))
                    .clicked()
                {
                    browser_navigate(app, testdata);
                }
            }
            if ui
                .button("↑")
                .on_hover_text(app.t(L::FileBrowserUp))
                .clicked()
            {
                if let Some(parent) = app.browser.cwd.parent().map(Path::to_path_buf) {
                    browser_navigate(app, parent);
                }
            }
            breadcrumb(ui, app);
        });

        // -- campo de caminho (Enter navega; erro inline) -----------------
        ui.horizontal(|ui| {
            ui.label(app.t(L::FileBrowserPath));
            let hint = app.t(L::FileBrowserPathHint);
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut app.browser.path_edit)
                        .id(egui::Id::unique(ID_PATH))
                        .desired_width(560.0)
                        .hint_text(hint),
                )
                .on_hover_text(app.t(L::FileBrowserPathHint));
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button("→").clicked() {
                submit_path(app);
            }
            if app.browser.path_error {
                ui.colored_label(egui::Color32::RED, app.t(L::FileBrowserInvalidPath));
            }
        });

        // -- filtro / busca ------------------------------------------------
        ui.horizontal(|ui| {
            let mut subdirs = app.browser.search_subdirs;
            let hint_search = app.t(L::FileBrowserSearchHint);
            ui.add(
                egui::TextEdit::singleline(&mut app.browser.filter)
                    .id(egui::Id::unique(ID_SEARCH))
                    .desired_width(360.0)
                    .hint_text(hint_search),
            );
            if ui
                .checkbox(&mut subdirs, app.t(L::FileBrowserSubdirs))
                .changed()
            {
                app.browser.search_subdirs = subdirs;
                app.browser.cancel_walk();
                // Re-semeia a query p/ re-disparar o walk no próximo frame.
                app.browser.walk_query = None;
            }
            if app.browser.walking() {
                ui.add(egui::Spinner::new().size(14.0));
                ui.weak(app.t(L::FileBrowserSearching));
                if ui.button(app.t(L::DeleteCancel)).clicked() {
                    app.browser.cancel_walk();
                }
            } else if app.browser.search_subdirs && !app.browser.filter.trim().is_empty() {
                ui.weak(format!(
                    "{}: {}",
                    app.t(L::FileBrowserSearching),
                    app.browser.walk_results.len()
                ));
            }
        });

        ui.separator();

        // -- corpo: listagem + detalhes ------------------------------------
        ui.horizontal(|ui| {
            listing(ui, app);
            ui.vertical(|ui| {
                ui.separator();
            });
            details_panel(ui, app);
        });
        ui.separator();

        // -- rodapé: Importar / Fechar -------------------------------------
        ui.horizontal(|ui| {
            let can_import = app
                .browser
                .selected
                .as_ref()
                .is_some_and(|e| e.kind.is_importable());
            if ui
                .add_enabled(can_import, egui::Button::new(app.t(L::FileBrowserImport)))
                .clicked()
            {
                if let Some(entry) = app.browser.selected.clone() {
                    let path = entry.path.to_string_lossy().to_string();
                    app.browser.open = false;
                    app.start_import_from_path(path);
                }
            }
            if ui.button(app.t(L::Close)).clicked() {
                close_browser(app);
            }
        });
    });
}

/// Fecha o navegador cancelando qualquer busca em andamento.
pub fn close_browser(app: &mut TerraApp) {
    app.browser.cancel_walk();
    app.browser.open = false;
}

/// Navega e registra o dir nos recentes (mesma ação para clique/teclado).
fn browser_navigate(app: &mut TerraApp, dir: PathBuf) -> bool {
    if app.browser.navigate(dir.clone()) {
        app.workbench.push_recent_dir(&dir.to_string_lossy());
        app.workbench_save();
        true
    } else {
        false
    }
}

/// Valida o texto do campo de caminho: dir navega; arquivo seleciona no
/// pai; inexistente marca erro inline.
fn submit_path(app: &mut TerraApp) {
    match validate_path(&app.browser.path_edit) {
        ValidPath::Dir(dir) => {
            browser_navigate(app, dir);
        }
        ValidPath::File(file) => {
            let parent = file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(MAIN_SEPARATOR.to_string()));
            if browser_navigate(app, parent) {
                app.browser.selected = Some(entry_from_path(&file));
            }
        }
        ValidPath::NotFound => app.browser.path_error = true,
    }
}

/// Constrói a entrada de um caminho único (usado ao colar um arquivo).
fn entry_from_path(path: &Path) -> FileEntry {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let meta = std::fs::metadata(path).ok();
    FileEntry {
        kind: EntryKind::from_name(&name, path.is_dir()),
        name: name.clone(),
        path: path.to_path_buf(),
        size: if path.is_dir() {
            None
        } else {
            meta.as_ref().map(|m| m.len())
        },
        modified: meta.and_then(|m| m.modified().ok()),
    }
}

/// Breadcrumb clicável do cwd.
fn breadcrumb(ui: &mut egui::Ui, app: &mut TerraApp) {
    let cwd = app.browser.cwd.clone();
    let text = cwd.to_string_lossy().to_string();
    let parts: Vec<&str> = text
        .split(MAIN_SEPARATOR)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        ui.weak(MAIN_SEPARATOR.to_string());
        return;
    }
    ui.weak("▸");
    // Raiz ("/" no Unix, "C:\" no Windows) — via components (std estável).
    let anchor = path_anchor(&cwd);
    let root = anchor.clone();
    let root_label = anchor.to_string_lossy().to_string();
    if ui
        .selectable_label(false, root_label.clone())
        .on_hover_text(root_label)
        .clicked()
    {
        browser_navigate(app, root);
    }
    let mut acc = anchor;
    for part in parts {
        acc.push(part);
        let target = acc.clone();
        if ui.selectable_label(true, part).clicked() {
            browser_navigate(app, target);
        }
        ui.weak("▸");
    }
}

/// Ancora do caminho: `"/"` no Unix, `"C:\"` no Windows (prefix + root);
/// caminho relativo cai na raiz. Sem `Path::anchor` (std estável local
/// não o expõe) — components.
fn path_anchor(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut comps = p.components();
    match comps.next() {
        Some(Component::Prefix(pre)) => {
            let mut anchor = PathBuf::from(pre.as_os_str());
            if comps.next() == Some(Component::RootDir) {
                anchor.push(MAIN_SEPARATOR_STR);
            }
            anchor
        }
        _ => PathBuf::from(MAIN_SEPARATOR_STR),
    }
}

/// Listagem (duplo clique entra/abre; seleção alimenta o painel).
fn listing(ui: &mut egui::Ui, app: &mut TerraApp) {
    let rows = app.browser.rows();
    egui::ScrollArea::vertical()
        .id_salt("ta_browser_list")
        .max_height(340.0)
        .max_width(520.0)
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.weak(app.t(L::FileBrowserEmpty));
                return;
            }
            for entry in rows {
                let selected = app
                    .browser
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.path == entry.path);
                let label = format!("{} {}", entry.kind.icon(), entry.name);
                let resp = ui.selectable_label(selected, label);
                if resp.clicked() {
                    app.browser.selected = Some(entry.clone());
                }
                if resp.double_clicked() {
                    if entry.kind == EntryKind::Dir {
                        let dir = entry.path.clone();
                        browser_navigate(app, dir);
                    } else if entry.kind.is_importable() {
                        let path = entry.path.to_string_lossy().to_string();
                        app.browser.open = false;
                        app.start_import_from_path(path);
                    }
                }
            }
        });
}

/// Painel lateral de detalhes + seção Recentes.
fn details_panel(ui: &mut egui::Ui, app: &mut TerraApp) {
    ui.group(|ui| {
        ui.set_min_width(220.0);
        ui.strong(app.t(L::FileBrowserDetails));
        if let Some(entry) = &app.browser.selected {
            egui::Grid::new("ta_browser_details")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.weak(app.t(L::FileBrowserName));
                    ui.label(entry.name.clone());
                    ui.end_row();
                    if let Some(size) = entry.size {
                        ui.weak(app.t(L::FileBrowserSize));
                        ui.label(format_size(size, app.state.i18n.lang));
                        ui.end_row();
                    }
                    ui.weak(app.t(L::FileBrowserModified));
                    ui.label(
                        entry
                            .modified
                            .map(format_mtime)
                            .unwrap_or_else(|| "—".into()),
                    );
                    ui.end_row();
                    ui.weak(app.t(L::FileBrowserFormat));
                    ui.label(format_label(entry));
                    ui.end_row();
                });
        } else {
            ui.weak(app.t(L::AttrNoSelectionHint));
        }
        ui.separator();
        ui.strong(app.t(L::FileBrowserOpenRecent));
        let recents = app.workbench.recent_files.clone();
        if recents.is_empty() {
            ui.weak(app.t(L::FileBrowserRecentEmpty));
        }
        for item in recents {
            let path = PathBuf::from(&item.path);
            let exists = path.exists();
            let icon = EntryKind::from_name(
                &path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                false,
            )
            .icon();
            let resp = ui.selectable_label(false, format!("{icon} {}", item.path));
            if resp.clicked() {
                if exists {
                    let dir = path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from(MAIN_SEPARATOR.to_string()));
                    app.browser.navigate(dir);
                    app.browser.selected = Some(entry_from_path(&path));
                } else {
                    // Recente sumiu do disco → remove na hora (on-click).
                    app.workbench.remove_recent_file(&item.path);
                    app.workbench_save();
                }
            }
        }
    });
}

/// Rótulo de formato do painel de detalhes: usa `ta_gis::detect` (extensão
/// + magic bytes) e cai para GeoTIFF/extensão quando não é vetorial.
fn format_label(entry: &FileEntry) -> String {
    match entry.kind {
        EntryKind::Dir => "—".into(),
        EntryKind::Raster => "GeoTIFF".into(),
        _ => ta_gis::detect::detect_source_format(&entry.path)
            .map(|f| f.name().to_owned())
            .unwrap_or_else(|_| {
                entry
                    .path
                    .extension()
                    .map(|e| e.to_string_lossy().to_uppercase())
                    .unwrap_or_else(|| "—".into())
            }),
    }
}

/// `source_uri` da camada ativa (para o atalho "Projeto").
fn active_layer_source(app: &TerraApp) -> Option<&str> {
    app.state
        .active_or_first()
        .and_then(|i| app.state.layers.get(i))
        .map(|g| g.layer.source_uri.as_str())
}

/// Teclado do navegador: Esc fecha, Ctrl+F foca o filtro, Alt+↑ sobe,
/// setas movem a seleção, F3 pula para a próxima ocorrência, Enter (no
/// filtro) abre o 1º resultado. Texto em edição vence (a11y).
fn handle_browser_keys(ctx: &egui::Context, app: &mut TerraApp) {
    let focused = ctx.memory(|m| m.focused());
    let editing_text = focused
        .map(|id| id == egui::Id::unique(ID_PATH) || id == egui::Id::unique(ID_SEARCH))
        .unwrap_or(false);
    ctx.input(|i| {
        let ctrl = i.modifiers.contains(egui::Modifiers::CTRL);
        let alt = i.modifiers.contains(egui::Modifiers::ALT);
        if i.key_pressed(egui::Key::Escape) {
            close_browser(app);
            return;
        }
        if ctrl && i.key_pressed(egui::Key::F) {
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique(ID_SEARCH)));
            return;
        }
        if alt && i.key_pressed(egui::Key::ArrowUp) {
            if let Some(parent) = app.browser.cwd.parent().map(Path::to_path_buf) {
                browser_navigate(app, parent);
            }
            return;
        }
        if editing_text {
            return; // setas/F3 pertencem ao campo de texto
        }
        if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowUp) {
            move_selection(app, i.key_pressed(egui::Key::ArrowDown));
        }
        if i.key_pressed(egui::Key::F3)
            || (i.key_pressed(egui::Key::Enter) && app.browser.walking())
        {
            jump_next_match(app);
        }
    });
}

/// Move a seleção na listagem visível (com wrap).
fn move_selection(app: &mut TerraApp, down: bool) {
    let rows = app.browser.rows();
    if rows.is_empty() {
        return;
    }
    let idx = app
        .browser
        .selected
        .as_ref()
        .and_then(|s| rows.iter().position(|e| e.path == s.path));
    let next = match idx {
        None => 0,
        Some(i) if down => (i + 1) % rows.len(),
        Some(0) => rows.len() - 1,
        Some(i) => i - 1,
    };
    app.browser.selected = rows.get(next).cloned();
}

/// F3: próxima ocorrência nos resultados da busca em subpastas (wrap).
fn jump_next_match(app: &mut TerraApp) {
    let results = app.browser.walk_results.clone();
    if results.is_empty() {
        return;
    }
    let idx = app
        .browser
        .selected
        .as_ref()
        .and_then(|s| results.iter().position(|e| e.path == s.path))
        .map_or(0, |i| (i + 1) % results.len());
    app.browser.selected = results.get(idx).cloned();
}

// ===========================================================================
// Testes (lógica pura — sem egui/display)
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("curupira-fb-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn classificacao_por_extensao_e_importabilidade() {
        assert_eq!(EntryKind::from_name("a.shp", false), EntryKind::Shapefile);
        assert_eq!(EntryKind::from_name("b.GeoJSON", false), EntryKind::GeoJson);
        assert_eq!(EntryKind::from_name("c.gpkg", false), EntryKind::GeoPackage);
        assert_eq!(EntryKind::from_name("d.fgb", false), EntryKind::FlatGeobuf);
        assert_eq!(EntryKind::from_name("e.TIF", false), EntryKind::Raster);
        assert_eq!(EntryKind::from_name("f.csv", false), EntryKind::Csv);
        assert_eq!(EntryKind::from_name("g.txt", false), EntryKind::Other);
        assert_eq!(EntryKind::from_name("qualquer", true), EntryKind::Dir);
        for k in [
            EntryKind::Shapefile,
            EntryKind::GeoJson,
            EntryKind::GeoPackage,
            EntryKind::FlatGeobuf,
            EntryKind::Raster,
            EntryKind::Csv,
        ] {
            assert!(k.is_importable(), "{k:?} deve ser importável");
        }
        assert!(!EntryKind::Dir.is_importable());
        assert!(!EntryKind::Other.is_importable());
    }

    #[test]
    fn list_dir_ordena_pastas_primeiro_e_coleta_metadados() {
        let root = tmp_root("list");
        std::fs::create_dir_all(root.join("zeta")).unwrap();
        std::fs::create_dir_all(root.join("alfa")).unwrap();
        std::fs::write(root.join("m.geojson"), b"{}").unwrap();
        std::fs::write(root.join("b.csv"), b"x,y\n1,2\n").unwrap();
        std::fs::write(root.join("a.txt"), b"oi").unwrap();

        let entries = list_dir(&root).unwrap();
        let dirs: Vec<_> = entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| e.name.clone())
            .collect();
        let files: Vec<_> = entries
            .iter()
            .filter(|e| e.kind != EntryKind::Dir)
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(dirs, vec!["alfa", "zeta"], "pastas antes, ordem alfabética");
        assert_eq!(files, vec!["a.txt", "b.csv", "m.geojson"]);

        let csv = entries.iter().find(|e| e.name == "b.csv").unwrap();
        assert_eq!(csv.size, Some(8));
        assert!(csv.modified.is_some(), "mtime coletado");
        let dir = entries.iter().find(|e| e.name == "alfa").unwrap();
        assert_eq!(dir.size, None, "dir não tem tamanho");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn filtro_case_insensitive_por_substring() {
        assert!(name_matches("Camada_SHP.shp", "shp"));
        assert!(name_matches("Camada_SHP.shp", "ada_s"));
        assert!(!name_matches("outro.csv", "shp"));
        assert!(name_matches("qualquer", ""), "filtro vazio = tudo");
    }

    #[test]
    fn walk_em_subpastas_respeita_depth_e_acha_matches() {
        let root = tmp_root("walk");
        std::fs::create_dir_all(root.join("n1/n2/n3/n4")).unwrap();
        std::fs::write(root.join("rasgo.tif"), b"x").unwrap();
        std::fs::write(root.join("n1/alvo.geojson"), b"{}").unwrap();
        std::fs::write(root.join("n1/n2/ALVO.gpkg"), b"sqlite").unwrap();
        std::fs::write(root.join("n1/n2/n3/alvo.csv"), b"a,b\n").unwrap();
        std::fs::write(root.join("n1/n2/n3/n4/fundo.csv"), b"a,b\n").unwrap();

        let cancel = AtomicBool::new(false);
        let mut out = Vec::new();
        walk_search(
            &root,
            "alvo",
            WALK_MAX_DEPTH,
            MAX_WALK_RESULTS,
            &cancel,
            &mut out,
        );
        let mut names: Vec<_> = out.iter().map(|e| e.name.clone()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        // depth 3 alcança n1, n1/n2 e n1/n2/n3; n4 fica fora.
        assert_eq!(names, vec!["alvo.csv", "alvo.geojson", "ALVO.gpkg"]);

        // Query vazia casa com tudo (arquivos), teto não atrapalha.
        // n4 (nível 4) fica fora do depth 3 → rasgo + alvo×3 = 4.
        let mut tudo = Vec::new();
        walk_search(
            &root,
            "",
            WALK_MAX_DEPTH,
            MAX_WALK_RESULTS,
            &cancel,
            &mut tudo,
        );
        assert_eq!(tudo.len(), 4);
        assert!(tudo.iter().all(|e| e.kind != EntryKind::Dir));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_cancelavel_e_limitado_em_resultados() {
        let root = tmp_root("walkcancel");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("x.csv"), b"0").unwrap();
        std::fs::write(root.join("a/x.csv"), b"1").unwrap();
        std::fs::write(root.join("a/b/y.csv"), b"2").unwrap();

        // Cancelada antes: nada sai.
        let cancel = AtomicBool::new(true);
        let mut out = Vec::new();
        walk_search(
            &root,
            "",
            WALK_MAX_DEPTH,
            MAX_WALK_RESULTS,
            &cancel,
            &mut out,
        );
        assert!(out.is_empty());

        // Teto injetável: 1º match consome a cota e interrompe o walk.
        let cancel = AtomicBool::new(false);
        let mut out = Vec::new();
        walk_search(&root, "", WALK_MAX_DEPTH, 1, &cancel, &mut out);
        assert_eq!(out.len(), 1, "teto de resultados interrompe o walk");

        // spawn_walk (thread): ordena por nome e entrega o lote final.
        let job = spawn_walk(root.clone(), "".into());
        let batch = job.rx.recv().expect("lote final");
        let names: Vec<_> = batch.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["x.csv", "x.csv", "y.csv"], "ordem estável");
        job.cancel(); // após o fim é no-op, mas não pode panic
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn validacao_de_caminho_e_expansao_de_tilde() {
        assert_eq!(validate_path("   "), ValidPath::NotFound);
        assert!(matches!(
            validate_path("/definitivamente/nao/existe"),
            ValidPath::NotFound
        ));

        // Dir e arquivo reais (tmpdir).
        let root = tmp_root("valid");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.csv"), b"x").unwrap();
        assert_eq!(
            validate_path(&root.join("sub").to_string_lossy()),
            ValidPath::Dir(root.join("sub"))
        );
        assert_eq!(
            validate_path(&root.join("sub/a.csv").to_string_lossy()),
            ValidPath::File(root.join("sub/a.csv"))
        );

        // `~` expande com o HOME injetado; `~user` não.
        assert_eq!(expand_tilde_with("~", Some("/h")), "/h");
        assert_eq!(expand_tilde_with("~/dados", Some("/h")), "/h/dados");
        assert_eq!(expand_tilde_with("~\\dados", Some("/h")), "/h\\dados");
        assert_eq!(expand_tilde_with("~root/x", Some("/h")), "~root/x");
        assert_eq!(
            expand_tilde_with("~/x", None),
            "~/x",
            "sem HOME não expande"
        );
        assert_eq!(expand_tilde_with("/puro", Some("/h")), "/puro");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn testdata_dir_sobe_ate_4_niveis_e_pula_ausentes() {
        let root = tmp_root("testdata");
        let exe = root.join("repo/target/debug/deps");
        std::fs::create_dir_all(root.join("repo/testdata")).unwrap();
        assert_eq!(
            testdata_dir_from(&exe),
            Some(root.join("repo/testdata")),
            "sobe até achar o testdata do repo"
        );
        let sem = root.join("vazio/bin");
        std::fs::create_dir_all(&sem).unwrap();
        assert_eq!(testdata_dir_from(&sem), None, "sem testdata → None (skip)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_dir_deriva_da_fonte_da_camada_ativa() {
        let root = tmp_root("projdir");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(project_dir_from(None), None);
        assert_eq!(project_dir_from(Some("")), None);
        assert_eq!(project_dir_from(Some("/não/existe.shp")), None);
        let src = root.join("camada.shp").to_string_lossy().to_string();
        assert_eq!(project_dir_from(Some(&src)), Some(root.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tamanhos_legiveis_no_idioma_certo() {
        use crate::i18n::Lang;
        assert_eq!(format_size(512, Lang::PtBr), "512 B");
        assert_eq!(format_size(1024, Lang::En), "1.0 kB");
        assert_eq!(format_size(1536, Lang::PtBr), "1,5 kB");
        assert_eq!(format_size(1536, Lang::En), "1.5 kB");
        assert_eq!(format_size(5 * 1024 * 1024, Lang::En), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024, Lang::PtBr), "3,0 GB");
    }

    #[test]
    fn mtime_formato_iso_curto() {
        // 2026-01-02 03:04 UTC = 1_767_323_040 s.
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_767_323_040);
        assert_eq!(format_mtime(t), "2026-01-02 03:04");
        // Época → 1970-01-01 00:00.
        assert_eq!(format_mtime(SystemTime::UNIX_EPOCH), "1970-01-01 00:00");
        // Virada de ano bissexto: 2024-02-29 23:59 = 1_709_251_140 s.
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_709_251_140);
        assert_eq!(format_mtime(t), "2024-02-29 23:59");
    }

    #[test]
    fn plan_drop_filtra_suportados_e_conta_ignorados() {
        let paths = vec![
            PathBuf::from("/d/a.shp"),
            PathBuf::from("/d/leia-me.txt"),
            PathBuf::from("/d/b.geojson"),
            PathBuf::from("/d/pasta"), // sem extensão → ignorado
            PathBuf::from("/d/c.TIF"),
        ];
        let plan = plan_drop(&paths);
        assert_eq!(
            plan.importable,
            vec![
                PathBuf::from("/d/a.shp"),
                PathBuf::from("/d/b.geojson"),
                PathBuf::from("/d/c.TIF"),
            ],
            "ordem preservada"
        );
        assert_eq!(plan.ignored, 2);
        assert!(plan_drop(&[]).importable.is_empty());
    }
}
