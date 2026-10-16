//! i18n — **todos** os rótulos da UI passam por aqui (pt-BR padrão, en).
//!
//! Esqueleto da Fase 3: chaves tipadas (`Key`) + tabela estática. As duas
//! línguas são exaustivas por construção (o `match` de [`tr`] precisa cobrir
//! toda a enum) e o teste unitário garante que nenhuma entrada é vazia e
//! que pt-BR e en divergem em pelo menos uma chave (ou seria tradução morta).
//!
//! Regra do projeto (restrição da Fase 3): nenhum literal de UI fora deste
//! módulo — textos dinâmicos (nomes de camada, caminhos) são dados, não
//! rótulos, e entram via `format!` nos pontos de uso.

/// Idioma ativo da UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// Português do Brasil (padrão do projeto).
    #[default]
    PtBr,
    /// English (fallback/esqueleto).
    En,
}

impl Lang {
    /// Alterna entre os dois idiomas (menu Ver).
    pub fn toggle(self) -> Self {
        match self {
            Lang::PtBr => Lang::En,
            Lang::En => Lang::PtBr,
        }
    }

    /// Nome exibível do idioma (na própria língua).
    pub fn display_name(self) -> &'static str {
        match self {
            Lang::PtBr => "Português (BR)",
            Lang::En => "English",
        }
    }
}

/// Chaves de rótulo da UI. Ordem livre; nova UI → nova chave aqui.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    AppTitle,
    AppSubtitle,
    MenuFile,
    MenuView,
    MenuLayer,
    MenuHelp,
    ConnectDb,
    DisconnectDb,
    OpenProject,
    Import,
    Quit,
    About,
    AboutBody,
    AboutRepo,
    Language,
    Theme,
    ThemeDark,
    ThemeLight,
    Layers,
    LayerVisible,
    LayerUp,
    LayerDown,
    LayerRemove,
    LayerActive,
    ZoomToExtent,
    NewPointMode,
    NewPointHint,
    CancelEditMode,
    Attributes,
    AttrNoLayer,
    AttrNoSelectionHint,
    Inspector,
    InspectorCrs,
    InspectorStyle,
    InspectorColor,
    InspectorStrokeWidth,
    InspectorOpacity,
    InspectorBands,
    InspectorBand,
    RasterBandLoading,
    StatusBarCoords,
    StatusBarScale,
    StatusBarSrid,
    StatusBarModeLocal,
    StatusBarModeDb,
    StatusBarFeatures,
    DbDialogTitle,
    DbDialogEnv,
    DbDialogUseEnv,
    DbDialogHost,
    DbDialogPort,
    DbDialogName,
    DbDialogUser,
    DbDialogPassword,
    DbDialogConnect,
    DbDialogCancel,
    DbConnected,
    DbConnectFailed,
    ImportDialogTitle,
    ImportDialogPath,
    ImportDialogPathHint,
    ImportDialogSrid,
    ImportDialogStart,
    ImportStageReading,
    ImportStageNormalizing,
    ImportStageInserting,
    ImportDone,
    ImportFailed,
    ImportRasterLoaded,
    ProjectsTitle,
    ProjectsEmpty,
    ProjectsOpen,
    ProjectsClose,
    DeleteFeatureTitle,
    DeleteFeatureBody,
    DeleteConfirm,
    DeleteCancel,
    Undo,
    Redo,
    ErrorTitle,
    ErrorDismiss,
    Loading,
    NoProject,
    LocalModeHint,
    Save,
    Close,
    SelectedFeature,
    ZoomIn,
    ZoomOut,
}

/// Todas as chaves — o teste de completude itera esta lista.
pub const ALL_KEYS: &[Key] = &[
    Key::AppTitle,
    Key::AppSubtitle,
    Key::MenuFile,
    Key::MenuView,
    Key::MenuLayer,
    Key::MenuHelp,
    Key::ConnectDb,
    Key::DisconnectDb,
    Key::OpenProject,
    Key::Import,
    Key::Quit,
    Key::About,
    Key::AboutBody,
    Key::AboutRepo,
    Key::Language,
    Key::Theme,
    Key::ThemeDark,
    Key::ThemeLight,
    Key::Layers,
    Key::LayerVisible,
    Key::LayerUp,
    Key::LayerDown,
    Key::LayerRemove,
    Key::LayerActive,
    Key::ZoomToExtent,
    Key::NewPointMode,
    Key::NewPointHint,
    Key::CancelEditMode,
    Key::Attributes,
    Key::AttrNoLayer,
    Key::AttrNoSelectionHint,
    Key::Inspector,
    Key::InspectorCrs,
    Key::InspectorStyle,
    Key::InspectorColor,
    Key::InspectorStrokeWidth,
    Key::InspectorOpacity,
    Key::InspectorBands,
    Key::InspectorBand,
    Key::RasterBandLoading,
    Key::StatusBarCoords,
    Key::StatusBarScale,
    Key::StatusBarSrid,
    Key::StatusBarModeLocal,
    Key::StatusBarModeDb,
    Key::StatusBarFeatures,
    Key::DbDialogTitle,
    Key::DbDialogEnv,
    Key::DbDialogUseEnv,
    Key::DbDialogHost,
    Key::DbDialogPort,
    Key::DbDialogName,
    Key::DbDialogUser,
    Key::DbDialogPassword,
    Key::DbDialogConnect,
    Key::DbDialogCancel,
    Key::DbConnected,
    Key::DbConnectFailed,
    Key::ImportDialogTitle,
    Key::ImportDialogPath,
    Key::ImportDialogPathHint,
    Key::ImportDialogSrid,
    Key::ImportDialogStart,
    Key::ImportStageReading,
    Key::ImportStageNormalizing,
    Key::ImportStageInserting,
    Key::ImportDone,
    Key::ImportFailed,
    Key::ImportRasterLoaded,
    Key::ProjectsTitle,
    Key::ProjectsEmpty,
    Key::ProjectsOpen,
    Key::ProjectsClose,
    Key::DeleteFeatureTitle,
    Key::DeleteFeatureBody,
    Key::DeleteConfirm,
    Key::DeleteCancel,
    Key::Undo,
    Key::Redo,
    Key::ErrorTitle,
    Key::ErrorDismiss,
    Key::Loading,
    Key::NoProject,
    Key::LocalModeHint,
    Key::Save,
    Key::Close,
    Key::SelectedFeature,
    Key::ZoomIn,
    Key::ZoomOut,
];

/// Tradução de `key` em `lang`. Exaustivo por construção.
#[must_use]
pub fn tr(lang: Lang, key: Key) -> &'static str {
    match (lang, key) {
        (Lang::PtBr, Key::AppTitle) => "Curupira GIS",
        (Lang::PtBr, Key::AppSubtitle) => {
            "SIG desktop clean-room para monitoramento florestal da Amazônia"
        }
        (Lang::PtBr, Key::MenuFile) => "Arquivo",
        (Lang::PtBr, Key::MenuView) => "Ver",
        (Lang::PtBr, Key::MenuLayer) => "Camada",
        (Lang::PtBr, Key::MenuHelp) => "Ajuda",
        (Lang::PtBr, Key::ConnectDb) => "Conectar banco…",
        (Lang::PtBr, Key::DisconnectDb) => "Desconectar banco",
        (Lang::PtBr, Key::OpenProject) => "Abrir projeto… (Ctrl+O)",
        (Lang::PtBr, Key::Import) => "Importar… (Ctrl+I)",
        (Lang::PtBr, Key::Quit) => "Sair",
        (Lang::PtBr, Key::About) => "Sobre",
        (Lang::PtBr, Key::AboutBody) => {
            "Reimplementação clean-room (MIT OR Apache-2.0). Nada deriva do \
             produto legado — ver CONTRIBUTING.md."
        }
        (Lang::PtBr, Key::AboutRepo) => "Repositório: https://github.com/ils15/curupira-gis",
        (Lang::PtBr, Key::Language) => "Idioma",
        (Lang::PtBr, Key::Theme) => "Tema",
        (Lang::PtBr, Key::ThemeDark) => "Escuro",
        (Lang::PtBr, Key::ThemeLight) => "Claro",
        (Lang::PtBr, Key::Layers) => "Camadas",
        (Lang::PtBr, Key::LayerVisible) => "Visível",
        (Lang::PtBr, Key::LayerUp) => "Subir camada",
        (Lang::PtBr, Key::LayerDown) => "Descer camada",
        (Lang::PtBr, Key::LayerRemove) => "Remover camada",
        (Lang::PtBr, Key::LayerActive) => "Ativa",
        (Lang::PtBr, Key::ZoomToExtent) => "Zoom para a extensão",
        (Lang::PtBr, Key::NewPointMode) => "Novo ponto (clique no mapa)",
        (Lang::PtBr, Key::NewPointHint) => {
            "Modo novo ponto: clique no mapa para criar. Esc cancela."
        }
        (Lang::PtBr, Key::CancelEditMode) => "Cancelar edição (Esc)",
        (Lang::PtBr, Key::Attributes) => "Tabela atributiva",
        (Lang::PtBr, Key::AttrNoLayer) => "Selecione uma camada na árvore à esquerda.",
        (Lang::PtBr, Key::AttrNoSelectionHint) => {
            "Clique numa linha para selecionar a feição no mapa."
        }
        (Lang::PtBr, Key::Inspector) => "Inspetor",
        (Lang::PtBr, Key::InspectorCrs) => "CRS",
        (Lang::PtBr, Key::InspectorStyle) => "Estilo",
        (Lang::PtBr, Key::InspectorColor) => "Cor",
        (Lang::PtBr, Key::InspectorStrokeWidth) => "Espessura",
        (Lang::PtBr, Key::InspectorOpacity) => "Opacidade",
        (Lang::PtBr, Key::InspectorBands) => "Bandas",
        (Lang::PtBr, Key::InspectorBand) => "Banda ativa",
        (Lang::PtBr, Key::RasterBandLoading) => "Carregando banda…",
        (Lang::PtBr, Key::StatusBarCoords) => "Cursor",
        (Lang::PtBr, Key::StatusBarScale) => "Escala",
        (Lang::PtBr, Key::StatusBarSrid) => "SRID",
        (Lang::PtBr, Key::StatusBarModeLocal) => "LOCAL",
        (Lang::PtBr, Key::StatusBarModeDb) => "BANCO",
        (Lang::PtBr, Key::StatusBarFeatures) => "feições",
        (Lang::PtBr, Key::DbDialogTitle) => "Conectar ao banco (PostGIS)",
        (Lang::PtBr, Key::DbDialogEnv) => "TA_DATABASE_URL detectado no ambiente",
        (Lang::PtBr, Key::DbDialogUseEnv) => "Usar TA_DATABASE_URL",
        (Lang::PtBr, Key::DbDialogHost) => "Host",
        (Lang::PtBr, Key::DbDialogPort) => "Porta",
        (Lang::PtBr, Key::DbDialogName) => "Banco",
        (Lang::PtBr, Key::DbDialogUser) => "Usuário",
        (Lang::PtBr, Key::DbDialogPassword) => "Senha",
        (Lang::PtBr, Key::DbDialogConnect) => "Conectar",
        (Lang::PtBr, Key::DbDialogCancel) => "Cancelar",
        (Lang::PtBr, Key::DbConnected) => "Conectado ao banco; migrações aplicadas.",
        (Lang::PtBr, Key::DbConnectFailed) => "Falha ao conectar",
        (Lang::PtBr, Key::ImportDialogTitle) => "Importar camada",
        (Lang::PtBr, Key::ImportDialogPath) => "Arquivo",
        (Lang::PtBr, Key::ImportDialogPathHint) => {
            "SHP/GeoJSON/GPKG/CSV-XY/GeoTIFF — caminho completo no disco"
        }
        (Lang::PtBr, Key::ImportDialogSrid) => "SRID (se a fonte não declarar CRS)",
        (Lang::PtBr, Key::ImportDialogStart) => "Importar",
        (Lang::PtBr, Key::ImportStageReading) => "Lendo arquivo…",
        (Lang::PtBr, Key::ImportStageNormalizing) => "Normalizando para EPSG:4674…",
        (Lang::PtBr, Key::ImportStageInserting) => "Gravando no banco…",
        (Lang::PtBr, Key::ImportDone) => "Importação concluída",
        (Lang::PtBr, Key::ImportFailed) => "Falha na importação",
        (Lang::PtBr, Key::ImportRasterLoaded) => "Raster carregado (referência ao arquivo)",
        (Lang::PtBr, Key::ProjectsTitle) => "Projetos",
        (Lang::PtBr, Key::ProjectsEmpty) => {
            "Nenhum projeto no banco. Crie pela CLI ou importe em modo local."
        }
        (Lang::PtBr, Key::ProjectsOpen) => "Abrir",
        (Lang::PtBr, Key::ProjectsClose) => "Fechar",
        (Lang::PtBr, Key::DeleteFeatureTitle) => "Remover feição",
        (Lang::PtBr, Key::DeleteFeatureBody) => {
            "Remover a feição selecionada? A ação não pode ser desfeita."
        }
        (Lang::PtBr, Key::DeleteConfirm) => "Remover",
        (Lang::PtBr, Key::DeleteCancel) => "Cancelar",
        (Lang::PtBr, Key::Undo) => "Desfazer (Ctrl+Z)",
        (Lang::PtBr, Key::Redo) => "Refazer (Ctrl+Y)",
        (Lang::PtBr, Key::ErrorTitle) => "Erro",
        (Lang::PtBr, Key::ErrorDismiss) => "Fechar",
        (Lang::PtBr, Key::Loading) => "Carregando…",
        (Lang::PtBr, Key::NoProject) => "Sem projeto aberto",
        (Lang::PtBr, Key::LocalModeHint) => {
            "Modo local (sem banco): as camadas ficam em memória. Conecte um \
             banco para persistir."
        }
        (Lang::PtBr, Key::Save) => "Salvar",
        (Lang::PtBr, Key::Close) => "Fechar",
        (Lang::PtBr, Key::SelectedFeature) => "Feição selecionada",
        (Lang::PtBr, Key::ZoomIn) => "Aproximar (+)",
        (Lang::PtBr, Key::ZoomOut) => "Afastar (−)",

        (Lang::En, Key::AppTitle) => "Curupira GIS",
        (Lang::En, Key::AppSubtitle) => "Clean-room desktop GIS for Amazon forest monitoring",
        (Lang::En, Key::MenuFile) => "File",
        (Lang::En, Key::MenuView) => "View",
        (Lang::En, Key::MenuLayer) => "Layer",
        (Lang::En, Key::MenuHelp) => "Help",
        (Lang::En, Key::ConnectDb) => "Connect database…",
        (Lang::En, Key::DisconnectDb) => "Disconnect database",
        (Lang::En, Key::OpenProject) => "Open project… (Ctrl+O)",
        (Lang::En, Key::Import) => "Import… (Ctrl+I)",
        (Lang::En, Key::Quit) => "Quit",
        (Lang::En, Key::About) => "About",
        (Lang::En, Key::AboutBody) => {
            "Clean-room reimplementation (MIT OR Apache-2.0). Nothing derives \
             from the legacy product — see CONTRIBUTING.md."
        }
        (Lang::En, Key::AboutRepo) => "Repository: https://github.com/ils15/curupira-gis",
        (Lang::En, Key::Language) => "Language",
        (Lang::En, Key::Theme) => "Theme",
        (Lang::En, Key::ThemeDark) => "Dark",
        (Lang::En, Key::ThemeLight) => "Light",
        (Lang::En, Key::Layers) => "Layers",
        (Lang::En, Key::LayerVisible) => "Visible",
        (Lang::En, Key::LayerUp) => "Move layer up",
        (Lang::En, Key::LayerDown) => "Move layer down",
        (Lang::En, Key::LayerRemove) => "Remove layer",
        (Lang::En, Key::LayerActive) => "Active",
        (Lang::En, Key::ZoomToExtent) => "Zoom to extent",
        (Lang::En, Key::NewPointMode) => "New point (click on map)",
        (Lang::En, Key::NewPointHint) => "New-point mode: click on the map to create. Esc cancels.",
        (Lang::En, Key::CancelEditMode) => "Cancel edit (Esc)",
        (Lang::En, Key::Attributes) => "Attribute table",
        (Lang::En, Key::AttrNoLayer) => "Select a layer in the tree on the left.",
        (Lang::En, Key::AttrNoSelectionHint) => "Click a row to select the feature on the map.",
        (Lang::En, Key::Inspector) => "Inspector",
        (Lang::En, Key::InspectorCrs) => "CRS",
        (Lang::En, Key::InspectorStyle) => "Style",
        (Lang::En, Key::InspectorColor) => "Color",
        (Lang::En, Key::InspectorStrokeWidth) => "Stroke width",
        (Lang::En, Key::InspectorOpacity) => "Opacity",
        (Lang::En, Key::InspectorBands) => "Bands",
        (Lang::En, Key::InspectorBand) => "Active band",
        (Lang::En, Key::RasterBandLoading) => "Loading band…",
        (Lang::En, Key::StatusBarCoords) => "Cursor",
        (Lang::En, Key::StatusBarScale) => "Scale",
        (Lang::En, Key::StatusBarSrid) => "SRID",
        (Lang::En, Key::StatusBarModeLocal) => "LOCAL",
        (Lang::En, Key::StatusBarModeDb) => "DATABASE",
        (Lang::En, Key::StatusBarFeatures) => "features",
        (Lang::En, Key::DbDialogTitle) => "Connect to database (PostGIS)",
        (Lang::En, Key::DbDialogEnv) => "TA_DATABASE_URL detected in environment",
        (Lang::En, Key::DbDialogUseEnv) => "Use TA_DATABASE_URL",
        (Lang::En, Key::DbDialogHost) => "Host",
        (Lang::En, Key::DbDialogPort) => "Port",
        (Lang::En, Key::DbDialogName) => "Database",
        (Lang::En, Key::DbDialogUser) => "User",
        (Lang::En, Key::DbDialogPassword) => "Password",
        (Lang::En, Key::DbDialogConnect) => "Connect",
        (Lang::En, Key::DbDialogCancel) => "Cancel",
        (Lang::En, Key::DbConnected) => "Connected; migrations applied.",
        (Lang::En, Key::DbConnectFailed) => "Connection failed",
        (Lang::En, Key::ImportDialogTitle) => "Import layer",
        (Lang::En, Key::ImportDialogPath) => "File",
        (Lang::En, Key::ImportDialogPathHint) => {
            "SHP/GeoJSON/GPKG/CSV-XY/GeoTIFF — full path on disk"
        }
        (Lang::En, Key::ImportDialogSrid) => "SRID (if the source has no CRS)",
        (Lang::En, Key::ImportDialogStart) => "Import",
        (Lang::En, Key::ImportStageReading) => "Reading file…",
        (Lang::En, Key::ImportStageNormalizing) => "Normalizing to EPSG:4674…",
        (Lang::En, Key::ImportStageInserting) => "Writing to database…",
        (Lang::En, Key::ImportDone) => "Import finished",
        (Lang::En, Key::ImportFailed) => "Import failed",
        (Lang::En, Key::ImportRasterLoaded) => "Raster loaded (file by reference)",
        (Lang::En, Key::ProjectsTitle) => "Projects",
        (Lang::En, Key::ProjectsEmpty) => {
            "No projects in the database. Create via CLI or import locally."
        }
        (Lang::En, Key::ProjectsOpen) => "Open",
        (Lang::En, Key::ProjectsClose) => "Close",
        (Lang::En, Key::DeleteFeatureTitle) => "Delete feature",
        (Lang::En, Key::DeleteFeatureBody) => "Delete the selected feature? This cannot be undone.",
        (Lang::En, Key::DeleteConfirm) => "Delete",
        (Lang::En, Key::DeleteCancel) => "Cancel",
        (Lang::En, Key::Undo) => "Undo (Ctrl+Z)",
        (Lang::En, Key::Redo) => "Redo (Ctrl+Y)",
        (Lang::En, Key::ErrorTitle) => "Error",
        (Lang::En, Key::ErrorDismiss) => "Close",
        (Lang::En, Key::Loading) => "Loading…",
        (Lang::En, Key::NoProject) => "No project open",
        (Lang::En, Key::LocalModeHint) => {
            "Local mode (no database): layers live in memory. Connect a \
             database to persist."
        }
        (Lang::En, Key::Save) => "Save",
        (Lang::En, Key::Close) => "Close",
        (Lang::En, Key::SelectedFeature) => "Selected feature",
        (Lang::En, Key::ZoomIn) => "Zoom in (+)",
        (Lang::En, Key::ZoomOut) => "Zoom out (−)",
    }
}

/// Acesso conveniente aos rótulos no idioma corrente.
#[derive(Debug, Clone, Copy, Default)]
pub struct I18n {
    pub lang: Lang,
}

impl I18n {
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    /// Rótulo no idioma corrente.
    #[must_use]
    pub fn t(&self, key: Key) -> &'static str {
        tr(self.lang, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todas_as_chaves_tem_traducao_nao_vazia_nos_dois_idiomas() {
        for &key in ALL_KEYS {
            for lang in [Lang::PtBr, Lang::En] {
                let s = tr(lang, key);
                assert!(!s.trim().is_empty(), "chave {key:?} vazia em {lang:?}");
            }
        }
    }

    #[test]
    fn traducoes_divergem_em_alguma_chave() {
        // Se pt-BR == en em TUDO, a tabela en é cópia morta.
        let diverge = ALL_KEYS
            .iter()
            .any(|&k| tr(Lang::PtBr, k) != tr(Lang::En, k));
        assert!(diverge, "pt-BR e en idênticos em todas as chaves");
    }

    #[test]
    fn lista_all_keys_cobre_a_enum_completa() {
        // Guarda de completude: se alguém adicionar variante em Key e esquecer
        // de ALL_KEYS, os testes acima não a percorrem. Aqui forçamos uma
        // contagem esperada — atualize os dois juntos.
        assert_eq!(ALL_KEYS.len(), 89, "Key e ALL_KEYS devem crescer juntos");
    }

    #[test]
    fn toggle_e_display() {
        assert_eq!(Lang::default(), Lang::PtBr);
        assert_eq!(Lang::PtBr.toggle(), Lang::En);
        assert_eq!(Lang::En.toggle(), Lang::PtBr);
        assert!(!Lang::PtBr.display_name().is_empty());
    }
}
