//! Estado de sessão do app: camadas (locais/banco), seleção, modos de
//! edição e undo/redo de atributos.
//!
//! Módulo **sem egui**: lógica pura testável headless. A UI ([`crate::app`])
//! orquestra; daqui saem os `LayerHandle` prontos para o `MapView`.

use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_core::style::Style;

use crate::db_worker::DbWorker;
use crate::map_view::{LayerFeature, LayerHandle, MapView};

/// Modo de interação corrente do cursor no mapa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    /// Navegação (pan/pick).
    #[default]
    Navigate,
    /// Próximo clique no mapa cria um ponto na camada ativa.
    NewPoint,
}

/// Conexão de banco da sessão.
#[derive(Debug, Default)]
pub enum Conn {
    /// Sem banco — camadas só em memória.
    #[default]
    Local,
    /// Worker vivo (pool na thread dele).
    Db(Box<DbWorker>),
}

impl Conn {
    pub fn is_db(&self) -> bool {
        matches!(self, Conn::Db(_))
    }

    /// Envia comando se conectado; `false` = modo local (chamador decide).
    pub fn send(&self, cmd: crate::db_worker::DbCmd) -> bool {
        match self {
            Conn::Db(w) => {
                w.send(cmd);
                true
            }
            Conn::Local => false,
        }
    }
}

/// Comando de desfazer/refazer de edição de atributos (escopo da Fase 3:
/// **attrs** — geometria editável fica para a Fase 4).
#[derive(Debug, Clone, PartialEq)]
pub struct AttrsEdit {
    pub layer_idx: usize,
    pub feat_idx: usize,
    pub old: serde_json::Value,
    pub new: serde_json::Value,
}

/// Camada da sessão (dado de domínio + estado de UI).
#[derive(Debug, Clone)]
pub struct GuiLayer {
    pub layer: Layer,
    pub visible: bool,
    pub style: Style,
    pub features: Vec<LayerFeature>,
}

impl GuiLayer {
    pub fn new(layer: Layer) -> Self {
        Self {
            layer,
            visible: true,
            style: Style::default(),
            features: Vec::new(),
        }
    }

    pub fn with_features(layer: Layer, features: Vec<LayerFeature>) -> Self {
        Self {
            layer,
            visible: true,
            style: Style::default(),
            features,
        }
    }

    /// Converte para o handle do `MapView` (sem clonar os dados — move).
    pub fn to_handle(&self) -> LayerHandle {
        LayerHandle {
            layer: self.layer.clone(),
            visible: self.visible,
            style: self.style.clone(),
            features: self.features.clone(),
        }
    }

    pub fn bbox_of_layer(&self) -> Option<[f64; 4]> {
        crate::viewport::bbox_of_geoms(self.features.iter().map(|lf| &lf.feature.geom))
    }
}

/// Estado completo da sessão.
#[derive(Debug, Default)]
pub struct AppState {
    pub i18n: crate::i18n::I18n,
    pub conn: Conn,
    pub project: Option<Project>,
    pub layers: Vec<GuiLayer>,
    /// Camada ativa (recebe "novo ponto").
    pub active_layer: Option<usize>,
    /// Feição selecionada: (camada, índice).
    pub selection: Option<(usize, usize)>,
    pub edit_mode: EditMode,
    pub undo_stack: Vec<AttrsEdit>,
    pub redo_stack: Vec<AttrsEdit>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Espelha o estado no `MapView` (chamar após qualquer mudança de dados).
    pub fn sync_map(&self, map: &mut crate::map_view::MapViewEgui) {
        let handles: Vec<LayerHandle> = self.layers.iter().map(GuiLayer::to_handle).collect();
        map.set_layers(handles);
        map.set_crs(self.project.as_ref().map_or(4674, |p| p.srid));
        let sel = self
            .selection
            .map(|(l, f)| crate::map_view::encode_feature_uid(l, f));
        map.set_selection(sel);
    }

    /// Camada ativa (ou primeira, se nenhuma marcada).
    pub fn active_or_first(&self) -> Option<usize> {
        self.active_layer
            .filter(|&i| i < self.layers.len())
            .or(if self.layers.is_empty() {
                None
            } else {
                Some(0)
            })
    }

    /// Bbox global das camadas visíveis (zoom-to-extent).
    pub fn visible_extent(&self) -> Option<[f64; 4]> {
        let mut it = self
            .layers
            .iter()
            .filter(|l| l.visible)
            .filter_map(GuiLayer::bbox_of_layer);
        let first = it.next()?;
        let mut acc = first;
        for b in it {
            acc[0] = acc[0].min(b[0]);
            acc[1] = acc[1].min(b[1]);
            acc[2] = acc[2].max(b[2]);
            acc[3] = acc[3].max(b[3]);
        }
        Some(acc)
    }

    /// Registra edição de attrs (undo em memória + persistência se banco).
    /// Retorna `true` se houve mudança real.
    pub fn apply_attrs_edit(
        &mut self,
        layer_idx: usize,
        feat_idx: usize,
        new: serde_json::Value,
    ) -> bool {
        let Some(lf) = self
            .layers
            .get_mut(layer_idx)
            .and_then(|l| l.features.get_mut(feat_idx))
        else {
            return false;
        };
        if lf.feature.attrs == new {
            return false;
        }
        let old = lf.feature.attrs.clone();
        lf.feature.attrs = new.clone();
        let db_id = lf.db_id;
        self.undo_stack.push(AttrsEdit {
            layer_idx,
            feat_idx,
            old,
            new,
        });
        self.redo_stack.clear();
        if let Some(db_id) = db_id {
            // Persiste o estado ATUAL (novo) — fire and forget; falha vira
            // evento Failed exibido pela UI.
            let attrs = self
                .layers
                .get(layer_idx)
                .and_then(|l| l.features.get(feat_idx))
                .map(|lf| lf.feature.attrs.clone());
            if let Some(attrs) = attrs {
                self.conn
                    .send(crate::db_worker::DbCmd::UpdateAttrs { db_id, attrs });
            }
        }
        true
    }

    /// Desfaz a última edição de attrs (memória + banco se aplicável).
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo_stack.pop() else {
            return false;
        };
        self.push_and_persist(edit, false)
    }

    /// Refaz a última edição desfeita.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo_stack.pop() else {
            return false;
        };
        self.push_and_persist(edit, true)
    }

    fn push_and_persist(&mut self, edit: AttrsEdit, to_redo: bool) -> bool {
        let AttrsEdit {
            layer_idx,
            feat_idx,
            old,
            new,
        } = edit.clone();
        let Some(lf) = self
            .layers
            .get_mut(layer_idx)
            .and_then(|l| l.features.get_mut(feat_idx))
        else {
            return false;
        };
        let applied = if to_redo { new.clone() } else { old.clone() };
        if lf.feature.attrs == applied {
            return false;
        }
        let db_id = lf.db_id;
        lf.feature.attrs = applied.clone();
        if to_redo {
            self.undo_stack.push(edit);
        } else {
            self.redo_stack.push(edit);
        }
        if let Some(db_id) = db_id {
            self.conn.send(crate::db_worker::DbCmd::UpdateAttrs {
                db_id,
                attrs: applied,
            });
        }
        true
    }

    /// Remove a feição selecionada (memória; banco via `DbCmd::DeleteFeature`
    /// quando há `db_id`). Limpa undo/redo (índices deslocados invalidariam o
    /// histórico — política conservadora documentada) e a seleção.
    pub fn remove_local(&mut self, layer_idx: usize, feat_idx: usize) -> Option<i64> {
        let layer = self.layers.get_mut(layer_idx)?;
        if feat_idx >= layer.features.len() {
            return None;
        }
        let removed = layer.features.remove(feat_idx);
        // Conservador: a seleção sumiu com a feição e o histórico de attrs
        // apontaria para índices deslocados.
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        Some(removed.db_id.unwrap_or(-1))
    }

    /// Adiciona feição (novo ponto) à camada ativa. Devolve (camada, índice).
    pub fn append_feature(&mut self, layer_idx: usize, feature: Feature) -> Option<(usize, usize)> {
        let layer = self.layers.get_mut(layer_idx)?;
        if layer.layer.geom_type != GeomType::Point && layer.layer.geom_type != GeomType::MultiPoint
        {
            // MVP: modo "novo ponto" só em camadas pontuais (mensagem na UI).
            return None;
        }
        layer.features.push(LayerFeature::local(feature));
        Some((layer_idx, layer.features.len() - 1))
    }

    /// Cria a `Layer` de domínio para um import vetorial concluído.
    pub fn new_layer_for(project_id: &str, name: &str, geom: GeomType, source: &str) -> Layer {
        Layer {
            id: String::new(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            geom_type: geom,
            srid: 4674,
            source_uri: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::Point;
    use serde_json::json;

    fn point_feature(x: f64, attrs: serde_json::Value) -> Feature {
        Feature::new(geo_types::Geometry::Point(Point::new(x, x)), attrs)
    }

    fn layer_point(name: &str, n: usize) -> GuiLayer {
        let layer = Layer {
            id: String::new(),
            project_id: "p".into(),
            name: name.into(),
            geom_type: GeomType::Point,
            srid: 4674,
            source_uri: String::new(),
        };
        let feats = (0..n)
            .map(|i| LayerFeature::local(point_feature(i as f64, json!({"i": i}))))
            .collect();
        GuiLayer::with_features(layer, feats)
    }

    #[test]
    fn attrs_edit_empilha_undo_limpa_redo() {
        let mut st = AppState::new();
        st.layers.push(layer_point("l", 2));
        assert!(st.apply_attrs_edit(0, 0, json!({"i": 0, "novo": true})));
        assert_eq!(st.undo_stack.len(), 1);
        assert!(st.redo_stack.is_empty());
        assert_eq!(st.layers[0].features[0].feature.attrs["novo"], true);

        // Mesmo valor → sem mudança, sem push.
        assert!(!st.apply_attrs_edit(0, 0, json!({"i": 0, "novo": true})));
        assert_eq!(st.undo_stack.len(), 1);
    }

    #[test]
    fn undo_redo_vai_e_volta_com_historico() {
        let mut st = AppState::new();
        st.layers.push(layer_point("l", 1));
        st.apply_attrs_edit(0, 0, json!({"v": 2}));
        st.apply_attrs_edit(0, 0, json!({"v": 3}));
        assert_eq!(st.layers[0].features[0].feature.attrs["v"], 3);

        assert!(st.undo());
        assert_eq!(st.layers[0].features[0].feature.attrs["v"], 2);
        assert_eq!(st.redo_stack.len(), 1);
        // Segundo undo volta ao attrs ORIGINAL da feição ({"i": 0}).
        assert!(st.undo());
        assert_eq!(st.layers[0].features[0].feature.attrs["i"], 0);
        assert!(st.layers[0].features[0].feature.attrs.get("v").is_none());
        assert!(st.redo());
        assert_eq!(st.layers[0].features[0].feature.attrs["v"], 2);
        assert!(st.redo());
        assert_eq!(st.layers[0].features[0].feature.attrs["v"], 3);
        assert!(!st.redo(), "redo vazio");
        // Undo ainda tem as duas edições da sessão.
        assert!(st.undo());
        assert_eq!(st.layers[0].features[0].feature.attrs["v"], 2);
    }

    #[test]
    fn append_feature_só_em_camada_pontual() {
        let mut st = AppState::new();
        st.layers.push(layer_point("pontos", 0));
        let poly_layer = {
            let layer = Layer {
                id: String::new(),
                project_id: "p".into(),
                name: "polys".into(),
                geom_type: GeomType::Polygon,
                srid: 4674,
                source_uri: String::new(),
            };
            GuiLayer::new(layer)
        };
        st.layers.push(poly_layer);

        let f = point_feature(-60.0, json!({}));
        assert!(st.append_feature(0, f.clone()).is_some());
        assert!(
            st.append_feature(1, f).is_none(),
            "polígono não recebe ponto no MVP"
        );
    }

    #[test]
    fn extent_visible_soma_camadas() {
        let mut st = AppState::new();
        st.layers.push(layer_point("a", 1)); // (0,0)
        let mut b = layer_point("b", 1);
        b.features[0] = LayerFeature::local(point_feature(-70.0, json!({})));
        st.layers.push(b);
        let ext = st.visible_extent().expect("bbox de camadas visíveis");
        assert_eq!(ext[0], -70.0);
        assert_eq!(ext[2], 0.0);
    }

    #[test]
    fn new_layer_for_usa_4674() {
        let l = AppState::new_layer_for("p1", "nome", GeomType::Point, "/tmp/x");
        assert_eq!(l.srid, 4674);
        assert_eq!(l.project_id, "p1");
    }
}
