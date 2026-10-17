//! Config de área de trabalho persistida (`workbench.json`).
//!
//! Caminho (sem crate nova — std puro):
//! `$XDG_CONFIG_HOME/curupira/workbench.json` ou, sem XDG,
//! `$HOME/.config/curupira/workbench.json`. Sem HOME/XDG (host exótico) a
//! persistência é desligada: o app usa os defaults e `save` é no-op.
//!
//! Arquivo ausente, corrompido ou com persona desconhecida → defaults
//! (**nunca panic**: o app precisa abrir em qualquer estado do disco).
//! Persona default = SIG com a janela de boas-vindas habilitada — igual ao
//! comportamento de 1ª abertura da referência de layout.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::Lang;
use crate::persona::Persona;

/// Limite de arquivos recentes (v0.2.1).
pub const MAX_RECENT_FILES: usize = 15;
/// Limite de diretórios recentes (v0.2.1).
pub const MAX_RECENT_DIRS: usize = 8;

/// Entrada de "recentes": caminho + timestamp Unix (segundos) do último uso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentItem {
    pub path: String,
    pub at: i64,
}

/// Unix agora (segundos). Host sem relógio coerente → 0 (nunca panic).
fn now_unix() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

/// Insere/move `path` para o topo da lista, deduplica e trunca em `max`.
/// Função pura (recebe o "agora") — testável sem relógio.
fn push_recent(list: &mut Vec<RecentItem>, path: &str, max: usize, now: i64) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    list.retain(|item| item.path != path);
    list.insert(
        0,
        RecentItem {
            path: path.to_owned(),
            at: now,
        },
    );
    list.truncate(max);
}

/// Remove a entrada `path`; devolve `true` se existia.
fn remove_recent(list: &mut Vec<RecentItem>, path: &str) -> bool {
    let before = list.len();
    list.retain(|item| item.path != path.trim());
    list.len() != before
}

/// Config persistida da área de trabalho.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchConfig {
    /// Persona aplicada na última sessão.
    pub persona: String,
    /// "Exibir esta janela na abertura" (diálogo de boas-vindas).
    pub show_welcome: bool,
    /// Idioma da UI ("pt-BR" | "en").
    pub lang: String,
    /// Arquivos abertos recentemente (mais recente primeiro; máx.
    /// [`MAX_RECENT_FILES`]). v0.2.1 — `default` mantém JSONs antigos.
    #[serde(default)]
    pub recent_files: Vec<RecentItem>,
    /// Diretórios visitados no navegador (máx. [`MAX_RECENT_DIRS`]). v0.2.1.
    #[serde(default)]
    pub recent_dirs: Vec<RecentItem>,
}

impl Default for WorkbenchConfig {
    fn default() -> Self {
        Self {
            persona: Persona::default().slug().to_owned(),
            show_welcome: true,
            lang: Lang::default().slug().to_owned(),
            recent_files: Vec::new(),
            recent_dirs: Vec::new(),
        }
    }
}

impl WorkbenchConfig {
    /// Persona configurada; slug desconhecido → SIG (default).
    pub fn persona(&self) -> Persona {
        Persona::from_slug(&self.persona).unwrap_or_default()
    }

    /// Idioma configurado; slug desconhecido → pt-BR (default).
    pub fn lang(&self) -> Lang {
        Lang::from_slug(&self.lang).unwrap_or_default()
    }

    // -- v0.2.1: recentes ------------------------------------------------
    /// Registra uso de arquivo (dedupe + topo + trim [`MAX_RECENT_FILES`]).
    pub fn push_recent_file(&mut self, path: &str) {
        push_recent(&mut self.recent_files, path, MAX_RECENT_FILES, now_unix());
    }

    /// Registra visita a diretório (dedupe + topo + trim [`MAX_RECENT_DIRS`]).
    pub fn push_recent_dir(&mut self, path: &str) {
        push_recent(&mut self.recent_dirs, path, MAX_RECENT_DIRS, now_unix());
    }

    /// Remove um arquivo recente (entrada inválida ao clicar). `true` se
    /// existia.
    pub fn remove_recent_file(&mut self, path: &str) -> bool {
        remove_recent(&mut self.recent_files, path)
    }
}

impl Lang {
    /// Slug estável para persistência.
    pub fn slug(self) -> &'static str {
        match self {
            Lang::PtBr => "pt-BR",
            Lang::En => "en",
        }
    }

    /// Parse do slug persistido.
    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "pt-BR" => Some(Lang::PtBr),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Resolve o diretório-base de config a partir dos valores de ambiente
/// (`XDG_CONFIG_HOME` primeiro, depois `HOME/.config`). `None` = sem
/// persistência. Função pura — testável sem tocar no ambiente real.
pub fn config_dir_from(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|s| !s.trim().is_empty()) {
        return Some(PathBuf::from(xdg));
    }
    home.filter(|s| !s.trim().is_empty()).map(|h| {
        let mut p = PathBuf::from(h);
        p.push(".config");
        p
    })
}

/// Caminho do arquivo de config no host real. `None` = sem persistência.
pub fn config_file() -> Option<PathBuf> {
    let dir = config_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )?;
    Some(dir.join("curupira").join("workbench.json"))
}

/// Lê a config de `path`; qualquer problema (ausente/JSON inválido/campos
/// errados) devolve os defaults — abertura do app nunca falha por config.
pub fn load_from(path: &std::path::Path) -> WorkbenchConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Carrega a config do host real (ou defaults, sem persistência).
pub fn load() -> WorkbenchConfig {
    config_file().map_or_else(WorkbenchConfig::default, |p| load_from(&p))
}

/// Grava a config (cria diretórios). Falha de I/O retorna erro — o chamador
/// decide ignorar (persistência é best-effort; a UI não bloqueia).
pub fn save_to(path: &std::path::Path, cfg: &WorkbenchConfig) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // pretty: o arquivo é pequeno e legível pelo usuário.
    let json = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".into());
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn resolver_prefere_xdg_e_cai_para_home() {
        // XDG vence.
        assert_eq!(
            config_dir_from(Some("/xdg"), Some("/home/u")),
            Some(PathBuf::from("/xdg"))
        );
        // Sem XDG: HOME/.config.
        assert_eq!(
            config_dir_from(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config"))
        );
        // Vazios contam como ausentes.
        assert_eq!(
            config_dir_from(Some("  "), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config"))
        );
        // Sem nada: sem persistência.
        assert_eq!(config_dir_from(None, None), None);
        // Caminho final.
        assert_eq!(
            config_file_for(Some("/c"), Some("/h")),
            Some(PathBuf::from("/c/curupira/workbench.json"))
        );
    }

    /// Variante pura de `config_file` para o teste (não lê env real).
    fn config_file_for(xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
        config_dir_from(xdg, home).map(|d| d.join("curupira").join("workbench.json"))
    }

    #[test]
    fn roundtrip_persistencia() {
        let path = std::env::temp_dir().join(format!(
            "curupira-wb-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cfg = WorkbenchConfig {
            persona: Persona::Operator.slug().into(),
            show_welcome: false,
            lang: "en".into(),
            recent_files: Vec::new(),
            recent_dirs: Vec::new(),
        };
        save_to(&path, &cfg).expect("grava config");

        let loaded = load_from(&path);
        assert_eq!(loaded, cfg);
        assert_eq!(loaded.persona(), Persona::Operator);
        assert_eq!(loaded.lang(), Lang::En);
        assert!(!loaded.show_welcome);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn arquivo_ausente_ou_corrompido_dá_default() {
        assert_eq!(
            load_from(Path::new("/não/existe/workbench.json")),
            WorkbenchConfig::default()
        );

        let path = std::env::temp_dir().join(format!(
            "curupira-wb-bad-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "{ json quebrado").unwrap();
        assert_eq!(load_from(&path), WorkbenchConfig::default());
        // Persona desconhecida cai no default (SIG).
        std::fs::write(
            &path,
            r#"{"persona":"root","show_welcome":false,"lang":"xx"}"#,
        )
        .unwrap();
        let cfg = load_from(&path);
        assert_eq!(cfg.persona(), Persona::Sig);
        assert_eq!(cfg.lang(), Lang::PtBr);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn defaults_da_1a_abertura() {
        let cfg = WorkbenchConfig::default();
        assert!(cfg.show_welcome, "1ª abertura mostra o bem-vindo");
        assert_eq!(cfg.persona(), Persona::Sig);
        assert_eq!(cfg.lang(), Lang::PtBr);
        assert!(cfg.recent_files.is_empty() && cfg.recent_dirs.is_empty());
    }

    // ---- v0.2.1: recentes ----------------------------------------------

    #[test]
    fn recentes_deduplicam_movem_para_o_topo_e_truncam_em_15() {
        let mut wb = WorkbenchConfig::default();
        for i in 0..(MAX_RECENT_FILES + 3) {
            wb.push_recent_file(&format!("/d/f{i}.shp"));
        }
        assert_eq!(wb.recent_files.len(), MAX_RECENT_FILES, "trim em 15");
        assert_eq!(
            wb.recent_files[0].path, "/d/f17.shp",
            "mais recente primeiro"
        );

        // Reabrir um arquivo antigo move para o topo (dedupe, sem duplicar).
        let old = "/d/f5.shp";
        assert!(wb.recent_files.iter().any(|r| r.path == old));
        wb.push_recent_file(old);
        assert_eq!(wb.recent_files[0].path, old);
        assert_eq!(wb.recent_files.iter().filter(|r| r.path == old).count(), 1);
        assert_eq!(wb.recent_files.len(), MAX_RECENT_FILES);

        // Caminho vazio/whitespace é ignorado.
        wb.push_recent_file("   ");
        assert_eq!(wb.recent_files.len(), MAX_RECENT_FILES);
    }

    #[test]
    fn recentes_de_diretorios_truncam_em_8() {
        let mut wb = WorkbenchConfig::default();
        for i in 0..12 {
            wb.push_recent_dir(&format!("/d/dir{i}"));
        }
        assert_eq!(wb.recent_dirs.len(), MAX_RECENT_DIRS);
        assert_eq!(wb.recent_dirs[0].path, "/d/dir11");
    }

    #[test]
    fn remover_recente_invalido_so_toca_a_entrada_certa() {
        let mut wb = WorkbenchConfig::default();
        wb.push_recent_file("/d/a.shp");
        wb.push_recent_file("/d/b.geojson");
        assert!(wb.remove_recent_file("/d/a.shp"));
        assert!(!wb.recent_files.iter().any(|r| r.path == "/d/a.shp"));
        assert_eq!(wb.recent_files.len(), 1);
        assert!(
            !wb.remove_recent_file("/d/sumiu.shp"),
            "inexistente → false"
        );
    }

    #[test]
    fn workbench_antigo_sem_recents_carrega_com_defaults_serde() {
        let mut wb = WorkbenchConfig::default();
        wb.push_recent_file("/d/x.tif");
        // Roundtrip completo preserva os recentes.
        let back: WorkbenchConfig =
            serde_json::from_str(&serde_json::to_string(&wb).unwrap()).unwrap();
        assert_eq!(back.recent_files.len(), 1);
        // JSON da v0.2 (sem as chaves novas) → defaults vazios, sem erro.
        let old: WorkbenchConfig =
            serde_json::from_str(r#"{"persona":"sig","show_welcome":true,"lang":"pt-BR"}"#)
                .unwrap();
        assert!(old.recent_files.is_empty() && old.recent_dirs.is_empty());
    }
}
