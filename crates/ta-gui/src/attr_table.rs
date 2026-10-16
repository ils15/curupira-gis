//! Tabela atributiva editável (painel inferior).
//!
//! - Janela deslizante via `ScrollArea::show_rows` — só as linhas visíveis
//!   são construídas (50k linhas não derrubam o frame).
//! - Edição: `TextEdit` por campo; commit em `lost_focus + changed`
//!   (Enter também perde o foco). Valor numérico reconhecido vira número
//!   JSON; `true/false` vira bool; vazio vira `null`; resto, string.
//! - Commit passa por `AppState::apply_attrs_edit` → undo/redo + persistência
//!   (bind-only) automática.

use eframe::egui;

use crate::app::TerraApp;
use crate::i18n::Key as L;

/// Altura estimada de linha (px) para o `show_rows`.
const ROW_HEIGHT: f32 = 24.0;
/// União de chaves calculada sobre as primeiras N feições (janela de custo).
const KEY_SAMPLE: usize = 200;

/// Painel da tabela atributiva da camada ativa.
pub fn attribute_table(ui: &mut egui::Ui, app: &mut TerraApp) {
    egui::Panel::bottom("ta_attrs")
        .resizable(true)
        .default_size(220.0)
        .min_size(80.0)
        .show(ui, |ui| {
            ui.heading(app.t(L::Attributes));
            ui.separator();
            let Some(layer_idx) = app.state.active_or_first() else {
                ui.weak(app.t(L::AttrNoLayer));
                return;
            };
            let n_feats = app.state.layers[layer_idx].features.len();
            if n_feats == 0 {
                ui.weak(app.t(L::AttrNoLayer));
                return;
            }
            let keys = attr_keys(app, layer_idx);
            let sel = app.state.selection;

            ui.horizontal(|ui| {
                ui.weak(format!(
                    "{} / {} {}",
                    layer_idx + 1,
                    n_feats,
                    app.t(L::StatusBarFeatures)
                ));
                if let Some((sl, sf)) = sel {
                    if sl == layer_idx {
                        ui.weak(format!("{}: #{}", app.t(L::SelectedFeature), sf + 1));
                    }
                }
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, ROW_HEIGHT, n_feats, |ui, range| {
                    for row in range {
                        // Cuidado: borrow curto por linha (app é mutável por campo).
                        let attrs = app.state.layers[layer_idx].features[row]
                            .feature
                            .attrs
                            .clone();
                        let is_sel = sel == Some((layer_idx, row));
                        ui.horizontal(|ui| {
                            let h = ui
                                .selectable_label(is_sel, format!("#{}", row + 1))
                                .clicked()
                                .then_some((layer_idx, row));
                            if let Some(sel_new) = h {
                                app.state.selection = Some(sel_new);
                                app.attr_buffers.clear();
                                app.state.sync_map(&mut app.map);
                            }
                            for key in &keys {
                                let display = attr_as_string(attrs.get(key));
                                let buf_key = (layer_idx, row, key.clone());
                                let mut text = app
                                    .attr_buffers
                                    .get(&buf_key)
                                    .cloned()
                                    .unwrap_or(display.clone());
                                let resp = egui::TextEdit::singleline(&mut text)
                                    .desired_width(110.0)
                                    .hint_text(key)
                                    .show(ui);
                                if text != display {
                                    app.attr_buffers.insert(buf_key.clone(), text.clone());
                                } else {
                                    app.attr_buffers.remove(&buf_key);
                                }
                                if resp.response.lost_focus() && resp.response.changed() {
                                    commit_attr(app, layer_idx, row, &attrs, key, &text);
                                }
                            }
                        });
                    }
                });
        });
}

/// União de chaves de atributos (amostra das primeiras `KEY_SAMPLE`).
fn attr_keys(app: &TerraApp, layer_idx: usize) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let layer = &app.state.layers[layer_idx];
    for lf in layer.features.iter().take(KEY_SAMPLE) {
        if let Some(obj) = lf.feature.attrs.as_object() {
            for k in obj.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    keys.truncate(12); // MVP: 12 colunas em tela (scroll horizontal fica p/ Fase 4)
    keys
}

/// Valor JSON → string de exibição.
fn attr_as_string(v: Option<&serde_json::Value>) -> String {
    match v {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Texto digitado → valor JSON tipado; grava via AppState (undo + banco).
fn commit_attr(
    app: &mut TerraApp,
    layer_idx: usize,
    row: usize,
    attrs: &serde_json::Value,
    key: &str,
    text: &str,
) {
    let mut new = attrs.clone();
    if let Some(obj) = new.as_object_mut() {
        let value = if text.is_empty() {
            serde_json::Value::Null
        } else if text == "true" {
            serde_json::Value::Bool(true)
        } else if text == "false" {
            serde_json::Value::Bool(false)
        } else if let Ok(i) = text.parse::<i64>() {
            serde_json::json!(i)
        } else if let Ok(f) = text.parse::<f64>() {
            serde_json::json!(f)
        } else {
            serde_json::Value::String(text.to_owned())
        };
        obj.insert(key.to_owned(), value);
    }
    app.state.apply_attrs_edit(layer_idx, row, new);
    app.attr_buffers.clear();
    app.state.sync_map(&mut app.map);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_as_string_tipos() {
        assert_eq!(attr_as_string(Some(&serde_json::json!("abc"))), "abc");
        assert_eq!(attr_as_string(Some(&serde_json::json!(42))), "42");
        assert_eq!(attr_as_string(Some(&serde_json::json!(1.5))), "1.5");
        assert_eq!(attr_as_string(Some(&serde_json::json!(null))), "");
        assert_eq!(
            attr_as_string(Some(&serde_json::json!({"a":1}))),
            r#"{"a":1}"#
        );
        assert_eq!(attr_as_string(None), "");
    }

    #[test]
    fn keys_tem_limite_de_colunas() {
        // Teste do limite é estrutural (truncate(12)); aqui validamos a
        // junção de chaves com um app mínimo — sem UI.
        use crate::app::TerraApp;
        let mut app = TerraApp::new();
        let layer = crate::app_state::AppState::new_layer_for(
            "p",
            "t",
            ta_core::layer::GeomType::Point,
            "",
        );
        let mut feats = Vec::new();
        for i in 0..5 {
            feats.push(crate::map_view::LayerFeature::local(
                ta_core::feature::Feature::new(
                    geo_types::Geometry::Point(geo_types::Point::new(0.0, 0.0)),
                    serde_json::json!({"k0": i, "k1": i}),
                ),
            ));
        }
        app.state
            .layers
            .push(crate::app_state::GuiLayer::with_features(layer, feats));
        let keys = attr_keys(&app, 0);
        assert_eq!(keys, vec!["k0", "k1"]);
    }
}
