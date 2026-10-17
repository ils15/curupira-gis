//! Caixa de ferramentas de processamento — padrão QGIS (toolbox com busca),
//! implementado como janela flutuante/redimensionável (dockável sem a crate
//! de docking — **nenhuma dependência nova**).
//!
//! A grade de ferramentas **reusa o registro de comandos**: cada
//! [`ProcessingTool`] aponta para o `command` (id) que executa a ação — o
//! comportamento (guardas, diálogos, pré-preenchimento) continua tendo um
//! único caminho em [`crate::command_registry`]. A toolbox só adiciona o
//! catálogo buscável com o *schema* de parâmetros.
//!
//! # Como adicionar uma ferramenta nova (prepara o plugin system)
//!
//! 1. **Ação**: registre um `Command` em `command_registry::ALL` com
//!    `category: Category::Processing` (a ação morre no registro, nunca
//!    aqui — menus/paleta/toobox passam a vê-la de graça).
//! 2. **Catálogo**: adicione um `ProcessingTool` em [`ALL_TOOLS`] com
//!    `command: "processing.seu_id"` e o `params_schema`.
//! 3. **i18n**: chaves de rótulo/descrição/parâmetros (pt-BR + en).
//!
//! O teste de integridade falha se o `command` não existir no registro —
//! a caixa nunca lista ferramenta morta.

use eframe::egui;
use egui::{RichText, TextEdit, TextWrapMode};

use crate::app::TerraApp;
use crate::command_registry as reg;
use crate::i18n::Key as L;

/// Tipo de um parâmetro declarado no schema (documental por ora: o diálogo
/// de parâmetros continua sendo o existente, pré-preenchido pelo comando).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Camada A (origem).
    LayerA,
    /// Camada B (segunda origem — interseção).
    LayerB,
    /// Distância numérica (buffer, graus).
    Distance,
    /// Nome da camada de saída.
    TargetName,
    /// Caminho de arquivo (import/export).
    FilePath,
    /// SRID de destino (import).
    Srid,
}

/// Um parâmetro declarado da ferramenta.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub id: &'static str,
    pub label: L,
    pub kind: ParamKind,
}

/// Ferramenta do catálogo — casca de catálogo sobre um comando do registro.
#[derive(Debug, Clone, Copy)]
pub struct ProcessingTool {
    /// Id da FERRAMENTA na caixa ("tool.buffer") — distinto do id do comando.
    pub id: &'static str,
    pub label: L,
    pub description: L,
    pub params_schema: &'static [ParamSpec],
    /// Id do comando do registro que executa a ferramenta (integridade
    /// garantida por teste).
    pub command: &'static str,
}

/// Catálogo atual: as 3 ops PostGIS + import/export (via registro).
pub const ALL_TOOLS: &[ProcessingTool] = &[
    ProcessingTool {
        id: "tool.buffer",
        label: L::ProcBuffer,
        description: L::ProcBufferDesc,
        command: "processing.buffer",
        params_schema: &[
            ParamSpec {
                id: "layer_a",
                label: L::ProcLayerA,
                kind: ParamKind::LayerA,
            },
            ParamSpec {
                id: "distance",
                label: L::ProcDistance,
                kind: ParamKind::Distance,
            },
            ParamSpec {
                id: "target",
                label: L::ProcTargetLayer,
                kind: ParamKind::TargetName,
            },
        ],
    },
    ProcessingTool {
        id: "tool.intersect",
        label: L::ProcIntersect,
        description: L::ProcIntersectDesc,
        command: "processing.intersect",
        params_schema: &[
            ParamSpec {
                id: "layer_a",
                label: L::ProcLayerA,
                kind: ParamKind::LayerA,
            },
            ParamSpec {
                id: "layer_b",
                label: L::ProcLayerB,
                kind: ParamKind::LayerB,
            },
            ParamSpec {
                id: "target",
                label: L::ProcTargetLayer,
                kind: ParamKind::TargetName,
            },
        ],
    },
    ProcessingTool {
        id: "tool.union",
        label: L::ProcUnion,
        description: L::ProcUnionDesc,
        command: "processing.union",
        params_schema: &[
            ParamSpec {
                id: "layer_a",
                label: L::ProcLayerA,
                kind: ParamKind::LayerA,
            },
            ParamSpec {
                id: "target",
                label: L::ProcTargetLayer,
                kind: ParamKind::TargetName,
            },
        ],
    },
    ProcessingTool {
        id: "tool.import",
        label: L::Import,
        description: L::ImportToolDesc,
        command: "file.import",
        params_schema: &[
            ParamSpec {
                id: "path",
                label: L::ImportDialogPath,
                kind: ParamKind::FilePath,
            },
            ParamSpec {
                id: "srid",
                label: L::ImportDialogSrid,
                kind: ParamKind::Srid,
            },
        ],
    },
    ProcessingTool {
        id: "tool.export",
        label: L::ExportActiveLayer,
        description: L::ExportToolDesc,
        command: "file.export_layer",
        params_schema: &[ParamSpec {
            id: "path",
            label: L::ImportDialogPath,
            kind: ParamKind::FilePath,
        }],
    },
];

/// Busca fuzzy do catálogo (rótulo i18n + último segmento do id, ASCII).
/// Vazia → todas, na ordem do catálogo.
pub fn search_tools(app: &TerraApp, query: &str) -> Vec<&'static ProcessingTool> {
    let mut scored: Vec<(&'static ProcessingTool, usize)> = ALL_TOOLS
        .iter()
        .filter_map(|t| {
            let short_id = t.id.rsplit('.').next().unwrap_or(t.id);
            let by_label = reg::fuzzy_score(query, app.t(t.label));
            let by_id = reg::fuzzy_score(query, short_id);
            let best = match (by_label, by_id) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }?;
            Some((t, best))
        })
        .collect();
    scored.sort_by_key(|(_, s)| *s);
    scored.into_iter().map(|(t, _)| t).collect()
}

/// Janela da caixa de ferramentas (flutuante, movível, redimensionável —
/// chamada uma vez por frame quando `dlg.toolbox`).
pub fn toolbox_window(ctx: &egui::Context, app: &mut TerraApp) {
    let mut open = app.dlg.toolbox;
    egui::Window::new(app.t(L::ToolboxTitle))
        .open(&mut open)
        .default_width(320.0)
        .default_height(420.0)
        .vscroll(true)
        .show(ctx, |ui| {
            toolbox_body(ui, app);
        });
    if !open {
        app.dlg.toolbox = false;
    }
}

fn toolbox_body(ui: &mut egui::Ui, app: &mut TerraApp) {
    let hint = app.t(L::ToolboxSearchHint);
    ui.add(
        TextEdit::singleline(&mut app.dlg.toolbox_query)
            .hint_text(hint)
            .desired_width(ui.available_width()),
    );
    ui.separator();

    let tools = search_tools(app, &app.dlg.toolbox_query);
    if tools.is_empty() {
        ui.weak(app.t(L::ToolboxEmpty));
        return;
    }
    for tool in tools {
        tool_row(ui, app, tool);
        ui.add_space(4.0);
    }
}

/// Uma ferramenta: ação + descrição + schema de parâmetros. A guarda de
/// habilitação vem do **comando** do registro (uma só fonte).
fn tool_row(ui: &mut egui::Ui, app: &mut TerraApp, tool: &'static ProcessingTool) {
    let cmd = reg::find(tool.command);
    let enabled = cmd.is_some_and(|c| c.enabled(app));

    let mut run: Option<&'static str> = None;
    ui.vertical(|ui| {
        let resp = ui
            .add_enabled(enabled, egui::Button::new(app.t(tool.label)).frame(false))
            .on_hover_text(app.t(tool.description));
        if !enabled {
            if let Some(reason) = cmd.and_then(|c| c.reason(app)) {
                resp.on_disabled_hover_text(app.t(reason));
            }
        } else if resp.clicked() {
            run = Some(tool.command);
        }

        ui.add(
            egui::Label::new(RichText::new(app.t(tool.description)).small().weak())
                .wrap_mode(TextWrapMode::Wrap),
        );

        // Schema documental (prepara o plugin system): nomes dos parâmetros.
        let params: Vec<&str> = tool.params_schema.iter().map(|p| app.t(p.label)).collect();
        if !params.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "{}: {}",
                        app.t(L::ToolboxParams),
                        params.join(", ")
                    ))
                    .small()
                    .weak(),
                )
                .wrap_mode(TextWrapMode::Wrap),
            );
        }
    });

    if let Some(id) = run {
        reg::run(app, ui.ctx(), id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::Persona;

    fn app() -> TerraApp {
        let mut app = TerraApp::new();
        app.state.apply_persona(Persona::Sig);
        app
    }

    #[test]
    fn ids_de_ferramentas_sao_unicos() {
        let mut seen = std::collections::HashSet::new();
        for t in ALL_TOOLS {
            assert!(seen.insert(t.id), "tool id duplicado: {}", t.id);
        }
    }

    #[test]
    fn toda_ferramenta_aponta_para_comando_existente() {
        // Integridade: a caixa nunca lista ferramenta morta.
        for t in ALL_TOOLS {
            let cmd = reg::find(t.command).unwrap_or_else(|| {
                panic!("tool {} aponta p/ comando inexistente {}", t.id, t.command)
            });
            assert!(
                matches!(
                    cmd.category,
                    reg::Category::Processing | reg::Category::File
                ),
                "categorias esperadas no catálogo: {} tem {:?}",
                t.id,
                cmd.category
            );
        }
        // E as 3 ops de processamento do registro estão TODAS catalogadas.
        for cmd in reg::of_category(reg::Category::Processing) {
            assert!(
                ALL_TOOLS.iter().any(|t| t.command == cmd.id),
                "comando {} do registro sem ferramenta na caixa",
                cmd.id
            );
        }
    }

    #[test]
    fn busca_do_catalogo_filtra_e_ordenacao_e_estavel() {
        let app = app();
        assert_eq!(search_tools(&app, "").len(), ALL_TOOLS.len());
        let buf = search_tools(&app, "buf");
        assert!(!buf.is_empty());
        assert_eq!(buf[0].id, "tool.buffer", "'buf' deve casar Buffer primeiro");
        // Match por id ASCII também funciona ("inter" → interseção).
        assert!(
            search_tools(&app, "inter")
                .iter()
                .any(|t| t.id == "tool.intersect")
        );
        assert!(search_tools(&app, "xyz").is_empty());
    }

    #[test]
    fn schemas_declaram_parametros_minimos() {
        let schema = |id: &str| ALL_TOOLS.iter().find(|t| t.id == id).unwrap().params_schema;
        assert!(
            schema("tool.buffer")
                .iter()
                .any(|p| p.kind == ParamKind::Distance)
        );
        assert!(
            schema("tool.intersect")
                .iter()
                .any(|p| p.kind == ParamKind::LayerB)
        );
        assert!(
            schema("tool.import")
                .iter()
                .any(|p| p.kind == ParamKind::FilePath)
        );
    }
}
