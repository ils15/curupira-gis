//! **Exchange do fluxo de monitoramento** — formato próprio e mínimo
//! `curupira-monitoring-exchange` **v1** (JSON, documento único).
//!
//! Este módulo é o DONO do modelo serde do exchange (formato, validação e
//! erros tipados). Portabilidade é por **NOMES estáveis** — nunca por UUIDs:
//! nome de projeto, de fase, de camada, `cell_id`, nome de classe, nome+versão
//! de ruleset e nome de cena são as chaves de referência entre seções.
//! Geometria trafega em **WKT, EPSG:4674** (armazenamento canônico); o
//! exchange é **JSON puro** — sem GDAL obrigatório.
//!
//! Seções do documento (todas opcionais exceto cabeçalho e `project`; um
//! exchange válido pode ter seções vazias — projeto recém-criado):
//!
//! | Seção         | Conteúdo                                                |
//! |---------------|---------------------------------------------------------|
//! | `project`     | nome, descrição, `active`, `min_scale`, `max_scale`, `max_cells` |
//! | `phases`      | fases do projeto (`name` é a chave)                     |
//! | `aoi_layers`  | camadas AOI com as features (WKT + attrs)               |
//! | `cell_layers` | grades: resolução/SRS/origem, `cells` e `blocked`       |
//! | `class_groups`| grupos de classes                                       |
//! | `classes`     | classes (`group_name`/`parent_name` por nome)           |
//! | `rulesets`    | rulesets versionados com suas `rules`                   |
//! | `draft_layers`| rascunhos com as decisões (`draft_cells`)               |
//! | `measures`    | medidas por célula (proveniência `scene_name` opcional) |
//! | `scenes`      | cenas vinculadas ao projeto (footprint em WKT)          |
//!
//! A importação recria as seções na **ordem de dependência** (cenas antes de
//! medidas, pois a medida referencia a cena); a validação de predicados
//! (`ta-engine::parse_predicate`) acontece ANTES de gravar qualquer regra.

use std::str::FromStr;

use geo_types::{Geometry, MultiPolygon, Polygon};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wkt::{ToWkt, Wkt};

/// Valor da chave `"format"` de todo documento v1.
pub const EXCHANGE_FORMAT: &str = "curupira-monitoring-exchange";

/// Versão do formato que este build exporta e importa.
pub const EXCHANGE_VERSION: u32 = 1;

/// Erros do exchange — tipados e acionáveis (apontam a seção/valor culpado).
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    /// Documento sem a chave `format` esperada — provavelmente não é um
    /// exchange do Curupira GIS.
    #[error(
        "formato de exchange inválido `{got}`: o documento deve declarar \
         `\"format\": \"{EXCHANGE_FORMAT}\"`"
    )]
    WrongFormat {
        /// Valor encontrado no documento.
        got: String,
    },

    /// `version` diferente da suportada por este build.
    #[error(
        "versão de exchange não suportada: {got} (este build fala a versão \
         {EXCHANGE_VERSION}); gere o arquivo com uma versão compatível"
    )]
    UnsupportedVersion {
        /// Valor encontrado no documento.
        got: u32,
    },

    /// JSON malformado — `serde_json` já aponta linha/coluna.
    #[error("JSON do exchange inválido: {0}")]
    Json(#[from] serde_json::Error),

    /// WKT que não decodifica — o `context` diz a seção/valor de origem.
    #[error("WKT inválido em {context}: {reason}")]
    Wkt {
        /// Onde o WKT estava (ex.: `cell_layers[0].cells[2].wkt`).
        context: String,
        /// Motivo devolvido pelo parser.
        reason: String,
    },

    /// Footprint de cena precisa ser (Multi)Polygon — outra geometria é
    /// recusada antes de chegar ao banco (coluna `geometry(MultiPolygon,4674)`).
    #[error(
        "footprint de cena inválido em {context}: esperado POLYGON/MULTIPOLYGON, \
         recebido {got}; informe POLYGON ou MULTIPOLYGON em EPSG:4674"
    )]
    FootprintNotPolygonal {
        /// Onde o WKT estava.
        context: String,
        /// Variante da geometria recebida (para a mensagem).
        got: &'static str,
    },

    /// Timestamp ISO-8601 que não parseia (`acquisition_on`/`decided_on`).
    #[error(
        "timestamp inválido em {context}: `{value}` (use ISO-8601, ex.: 2026-01-31T12:00:00Z ou 2026-01-31)"
    )]
    BadTimestamp {
        /// Onde o timestamp estava.
        context: String,
        /// Valor recebido.
        value: String,
    },
}

/// Formato de data aceito em `acquisition_on`: RFC-3339 completo
/// (`2026-01-31T12:00:00Z`) ou só a data (`2026-01-31` → 00:00 UTC).
pub fn parse_iso8601(context: &str, value: &str) -> Result<OffsetDateTime, ExchangeError> {
    if let Ok(dt) =
        OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339)
    {
        return Ok(dt);
    }
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    let bad = |context: &str, value: &str| ExchangeError::BadTimestamp {
        context: context.to_owned(),
        value: value.to_owned(),
    };
    let date = time::Date::parse(value.trim(), &fmt).map_err(|_| bad(context, value))?;
    date.with_hms(0, 0, 0)
        .map(|dt| dt.assume_utc())
        .map_err(|_| bad(context, value))
}

/// Converte WKT (EPSG:4674) em geometria; `context` entra na mensagem de erro.
pub fn parse_wkt(context: &str, text: &str) -> Result<Geometry<f64>, ExchangeError> {
    let bad = |reason: String| ExchangeError::Wkt {
        context: context.to_owned(),
        reason,
    };
    let parsed = Wkt::from_str(text.trim()).map_err(|reason| bad(reason.to_owned()))?;
    Geometry::<f64>::try_from(parsed).map_err(|e| bad(e.to_string()))
}

/// Serializa geometria em WKT (texto canônico do exchange).
#[must_use]
pub fn geometry_to_wkt(geom: &Geometry<f64>) -> String {
    geom.to_wkt().to_string()
}

/// WKT → footprint de cena (MultiPolygon; POLYGON simples é envolvido).
pub fn scene_footprint_from_wkt(
    context: &str,
    text: &str,
) -> Result<MultiPolygon<f64>, ExchangeError> {
    match parse_wkt(context, text)? {
        Geometry::Polygon(p) => Ok(MultiPolygon::new(vec![p])),
        Geometry::MultiPolygon(mp) => Ok(mp),
        other => Err(ExchangeError::FootprintNotPolygonal {
            context: context.to_owned(),
            got: geometry_kind(&other),
        }),
    }
}

/// Rótulo da variante da geometria (mensagens acionáveis).
fn geometry_kind(geom: &Geometry<f64>) -> &'static str {
    match geom {
        Geometry::Point(_) => "POINT",
        Geometry::Line(_) => "LINE",
        Geometry::LineString(_) => "LINESTRING",
        Geometry::Polygon(_) => "POLYGON",
        Geometry::MultiPoint(_) => "MULTIPOINT",
        Geometry::MultiLineString(_) => "MULTILINESTRING",
        Geometry::MultiPolygon(_) => "MULTIPOLYGON",
        Geometry::GeometryCollection(_) => "GEOMETRYCOLLECTION",
        Geometry::Rect(_) => "RECT",
        Geometry::Triangle(_) => "TRIANGLE",
    }
}

/// Converte Polygon em MultiPolygon (células vêm como Polygon; cena guarda
/// MultiPolygon).
#[must_use]
pub fn polygon_to_multipolygon(p: &Polygon<f64>) -> MultiPolygon<f64> {
    MultiPolygon::new(vec![p.clone()])
}

// ---------------------------------------------------------------------------
// Documento e seções
// ---------------------------------------------------------------------------

/// Documento exchange completo (`curupira-monitoring-exchange` v1).
///
/// Campos desconhecidos são RECUSADOS (`deny_unknown_fields`): um erro de
/// digitação em uma seção não pode perder dados silenciosamente no round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeDoc {
    /// Sempre `"curupira-monitoring-exchange"`.
    pub format: String,
    /// Sempre `1` neste build.
    pub version: u32,
    /// Momento da exportação (RFC-3339 UTC); informativo.
    pub exported_on: String,
    /// Projeto-alvo do exchange (obrigatório).
    #[serde(default)]
    pub project: ExchangeProject,
    /// Fases do projeto.
    #[serde(default)]
    pub phases: Vec<ExchangePhase>,
    /// Camadas AOI (com features).
    #[serde(default)]
    pub aoi_layers: Vec<ExchangeAoiLayer>,
    /// Grades de células (com células e bloqueios).
    #[serde(default)]
    pub cell_layers: Vec<ExchangeCellLayer>,
    /// Grupos de classes.
    #[serde(default)]
    pub class_groups: Vec<ExchangeClassGroup>,
    /// Classes (referências por nome).
    #[serde(default)]
    pub classes: Vec<ExchangeClass>,
    /// Rulesets versionados (com regras).
    #[serde(default)]
    pub rulesets: Vec<ExchangeRuleset>,
    /// Rascunhos (com decisões).
    #[serde(default)]
    pub draft_layers: Vec<ExchangeDraftLayer>,
    /// Medidas por célula.
    #[serde(default)]
    pub measures: Vec<ExchangeMeasure>,
    /// Cenas vinculadas ao projeto.
    #[serde(default)]
    pub scenes: Vec<ExchangeScene>,
}

/// Seção `project` — o único campo verdadeiramente obrigatório é `name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeProject {
    /// Nome do projeto (chave de portabilidade).
    pub name: String,
    /// Descrição livre (persistida em `projects.description`).
    #[serde(default)]
    pub description: String,
    /// Projeto ativo (default `true`).
    #[serde(default = "default_true")]
    pub active: bool,
    /// Escala mínima de edição (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_scale: Option<f64>,
    /// Escala máxima de edição (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_scale: Option<f64>,
    /// Teto de células por grade (default 1.000.000).
    #[serde(default = "default_max_cells")]
    pub max_cells: i32,
}

fn default_true() -> bool {
    true
}

fn default_max_cells() -> i32 {
    1_000_000
}

impl Default for ExchangeProject {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            active: true,
            min_scale: None,
            max_scale: None,
            max_cells: default_max_cells(),
        }
    }
}

/// Seção `phases` — `name` é único no projeto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangePhase {
    /// Nome da fase (chave de referência das seções seguintes).
    pub name: String,
    /// Descrição livre.
    #[serde(default)]
    pub description: String,
    /// A fase bloqueia células? (default `false`).
    #[serde(default)]
    pub cell_blocker: bool,
    /// Ordem de exibição (default `0`).
    #[serde(default)]
    pub sort_order: i32,
}

/// Feição de uma camada AOI: geometria em WKT 4674 + atributos livres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeFeature {
    /// Geometria em WKT (EPSG:4674).
    pub wkt: String,
    /// Atributos livres (objeto JSON; `{}` quando ausente).
    #[serde(default)]
    pub attrs: serde_json::Value,
}

/// Seção `aoi_layers` — camada vetorial eleita AOI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeAoiLayer {
    /// Nome da camada vetorial (única no projeto; é criada na importação).
    pub layer_name: String,
    /// Fase dona (opcional; referência por nome).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    /// Coluna de atributo que identifica a feição da AOI (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_column: Option<String>,
    /// Features da camada.
    #[serde(default)]
    pub features: Vec<ExchangeFeature>,
}

/// Célula de uma grade: id determinístico + índices + geometria WKT 4674.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeCell {
    /// Identificador `"<col>_<row>"` (0-based).
    pub cell_id: String,
    /// Índice de linha.
    pub row_index: i32,
    /// Índice de coluna.
    pub col_index: i32,
    /// Polígono da célula em WKT (EPSG:4674).
    pub wkt: String,
}

/// Célula bloqueada de uma grade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeBlocked {
    /// Célula bloqueada (`"<col>_<row>"`).
    pub cell_id: String,
    /// Motivo livre (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Seção `cell_layers` — grade regular derivada de uma AOI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeCellLayer {
    /// Nome da grade (único no projeto).
    pub name: String,
    /// Fase dona (opcional; referência por nome).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    /// Resolução NAS UNIDADES de `srid` (graus se geográfico, metros se
    /// projetado — Decisão 4).
    pub resolution: f64,
    /// EPSG em que `resolution`/`origin_x`/`origin_y` são expressos.
    #[serde(default = "default_srid")]
    pub srid: i32,
    /// Origem X do alinhamento.
    pub origin_x: f64,
    /// Origem Y do alinhamento.
    pub origin_y: f64,
    /// AOI que originou a grade (referência por NOME de camada; `cell_layers.aoi_layer_id`
    /// é NOT NULL no banco — sem ela a grade não pode ser recriada).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aoi_layer_name: Option<String>,
    /// Células da grade.
    #[serde(default)]
    pub cells: Vec<ExchangeCell>,
    /// Células bloqueadas.
    #[serde(default)]
    pub blocked: Vec<ExchangeBlocked>,
}

fn default_srid() -> i32 {
    4674
}

/// Seção `class_groups`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeClassGroup {
    /// Nome do grupo (único no projeto).
    pub name: String,
    /// Ordem de exibição.
    #[serde(default)]
    pub sort_order: i32,
}

/// Seção `classes` — `group_name`/`parent_name` por nome (criados antes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeClass {
    /// Nome da classe (único no projeto).
    pub name: String,
    /// Grupo (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// Classe-mãe (opcional; referência por nome).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_name: Option<String>,
    /// Cor `#RRGGBB` (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Ordem de exibição.
    #[serde(default)]
    pub sort_order: i32,
    /// Classe ativa (default `true`; hoje é atributo de exibição).
    #[serde(default = "default_true")]
    pub is_active: bool,
}

/// Regra dentro de um ruleset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRule {
    /// Nome da regra (único no ruleset).
    pub name: String,
    /// Classe decidida (referência por nome).
    pub class_name: String,
    /// Prioridade (maior vence; empate pelo menor nome).
    #[serde(default)]
    pub priority: i32,
    /// Predicado na gramática do motor (`ndvi >= 0.6 and nbr < 0.1`).
    pub predicate: String,
    /// Regra ativa (inativa é ignorada na avaliação; default `true`).
    #[serde(default = "default_true")]
    pub is_active: bool,
}

/// Seção `rulesets` — versão explícita preservada no import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRuleset {
    /// Nome do ruleset (a linhagem é versionada por nome).
    pub name: String,
    /// Versão (> 0) — preservada na importação.
    pub version: i32,
    /// Descrição livre (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ruleset ativo do projeto (no máximo um por projeto — índice parcial
    /// único; na importação o primeiro ativo do documento é ativado).
    #[serde(default)]
    pub is_active: bool,
    /// Regras do ruleset.
    #[serde(default)]
    pub rules: Vec<ExchangeRule>,
}

/// Decisão de classificação dentro de um rascunho.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeDraftCell {
    /// Célula classificada.
    pub cell_id: String,
    /// Grade da célula (referência por nome).
    pub cell_layer_name: String,
    /// Classe decidida (referência por nome).
    pub class_name: String,
    /// Ruleset que decidiu (referência por nome).
    pub ruleset_name: String,
    /// Versão do ruleset que decidiu.
    pub ruleset_version: i32,
    /// Momento da decisão (ISO-8601; ausente → agora).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_on: Option<String>,
}

/// Seção `draft_layers` — rascunho com suas decisões.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeDraftLayer {
    /// Nome do rascunho (único no projeto).
    pub name: String,
    /// Fase dona (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    /// Grade associada (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_layer_name: Option<String>,
    /// Estado `draft` | `consolidated` (default `draft`).
    #[serde(default = "default_state")]
    pub state: String,
    /// Decisões do rascunho.
    #[serde(default)]
    pub cells: Vec<ExchangeDraftCell>,
}

fn default_state() -> String {
    "draft".to_owned()
}

/// Seção `measures` — medida observada por célula (upsert last-write-wins).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeMeasure {
    /// Grade da célula (referência por nome).
    pub cell_layer_name: String,
    /// Célula medida.
    pub cell_id: String,
    /// Nome da medida (mesma gramática dos predicados).
    pub measure: String,
    /// Valor observado (finito).
    pub value: f64,
    /// Proveniência livre (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Cena de origem (opcional; referência por nome — criada antes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
}

/// Seção `scenes` — cena de sensoriamento vinculada ao projeto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeScene {
    /// Nome da cena (único no banco — cenas são globais).
    pub name: String,
    /// Aquisição (ISO-8601; opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_on: Option<String>,
    /// Footprint em WKT (Multi)Polygon EPSG:4674 (opcional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint_wkt: Option<String>,
    /// Metadados livres (objeto JSON; `{}` quando ausente).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ExchangeDoc {
    /// Documento vazio (todas as seções vazias) para o projeto dado — o
    /// export de um projeto recém-criado é um documento válido.
    #[must_use]
    pub fn empty(project: ExchangeProject) -> Self {
        Self {
            format: EXCHANGE_FORMAT.to_owned(),
            version: EXCHANGE_VERSION,
            exported_on: OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned()),
            project,
            phases: Vec::new(),
            aoi_layers: Vec::new(),
            cell_layers: Vec::new(),
            class_groups: Vec::new(),
            classes: Vec::new(),
            rulesets: Vec::new(),
            draft_layers: Vec::new(),
            measures: Vec::new(),
            scenes: Vec::new(),
        }
    }

    /// Faz o parse do JSON **e** valida cabeçalho (`format`/`version`).
    pub fn from_json(text: &str) -> Result<Self, ExchangeError> {
        let doc: Self = serde_json::from_str(text)?;
        doc.validate()?;
        Ok(doc)
    }

    /// Valida o cabeçalho do documento (`format` e `version`).
    ///
    /// # Errors
    /// [`ExchangeError::WrongFormat`] quando `format` difere de
    /// [`EXCHANGE_FORMAT`]; [`ExchangeError::UnsupportedVersion`] quando
    /// `version` difere de [`EXCHANGE_VERSION`].
    pub fn validate(&self) -> Result<(), ExchangeError> {
        if self.format != EXCHANGE_FORMAT {
            return Err(ExchangeError::WrongFormat {
                got: self.format.clone(),
            });
        }
        if self.version != EXCHANGE_VERSION {
            return Err(ExchangeError::UnsupportedVersion { got: self.version });
        }
        Ok(())
    }

    /// Serializa em JSON pretty (uma linha por campo — diff-friendly).
    ///
    /// # Errors
    /// Propaga falha de serialização (não deve ocorrer para este modelo).
    pub fn to_json_pretty(&self) -> Result<String, ExchangeError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_doc() -> ExchangeDoc {
        let mut doc = ExchangeDoc::empty(ExchangeProject {
            name: "proj_a".into(),
            description: "projeto de teste".into(),
            active: true,
            min_scale: Some(1000.0),
            max_scale: None,
            max_cells: 500_000,
        });
        doc.phases.push(ExchangePhase {
            name: "monitoramento 2026".into(),
            description: "fase única".into(),
            cell_blocker: true,
            sort_order: 1,
        });
        doc.aoi_layers.push(ExchangeAoiLayer {
            layer_name: "aoi".into(),
            phase_name: Some("monitoramento 2026".into()),
            id_column: Some("cod".into()),
            features: vec![ExchangeFeature {
                wkt: "POLYGON((-52 -12,-52 -11,-51 -11,-51 -12,-52 -12))".into(),
                attrs: json!({"cod": 1}),
            }],
        });
        doc.cell_layers.push(ExchangeCellLayer {
            name: "grade".into(),
            phase_name: Some("monitoramento 2026".into()),
            resolution: 0.005,
            srid: 4674,
            origin_x: -52.0,
            origin_y: -12.0,
            aoi_layer_name: Some("aoi".into()),
            cells: vec![ExchangeCell {
                cell_id: "0_0".into(),
                row_index: 0,
                col_index: 0,
                wkt: "POLYGON((-52 -12,-52 -11.995,-51.995 -11.995,-51.995 -12,-52 -12))".into(),
            }],
            blocked: vec![ExchangeBlocked {
                cell_id: "0_0".into(),
                reason: Some("nuvem".into()),
            }],
        });
        doc.class_groups.push(ExchangeClassGroup {
            name: "cobertura".into(),
            sort_order: 0,
        });
        doc.classes.push(ExchangeClass {
            name: "floresta".into(),
            group_name: Some("cobertura".into()),
            parent_name: None,
            color: Some("#00AA00".into()),
            sort_order: 1,
            is_active: true,
        });
        doc.classes.push(ExchangeClass {
            name: "palmeira".into(),
            group_name: None,
            parent_name: Some("floresta".into()),
            color: None,
            sort_order: 2,
            is_active: false,
        });
        doc.rulesets.push(ExchangeRuleset {
            name: "regras v1".into(),
            version: 2,
            description: Some("segunda escrita".into()),
            is_active: true,
            rules: vec![ExchangeRule {
                name: "ndvi alto".into(),
                class_name: "floresta".into(),
                priority: 10,
                predicate: "ndvi >= 0.6 and nbr < 0.1".into(),
                is_active: true,
            }],
        });
        doc.draft_layers.push(ExchangeDraftLayer {
            name: "rascunho regras v1".into(),
            phase_name: Some("monitoramento 2026".into()),
            cell_layer_name: Some("grade".into()),
            state: "draft".into(),
            cells: vec![ExchangeDraftCell {
                cell_id: "0_0".into(),
                cell_layer_name: "grade".into(),
                class_name: "floresta".into(),
                ruleset_name: "regras v1".into(),
                ruleset_version: 2,
                decided_on: Some("2026-10-19T12:00:00Z".into()),
            }],
        });
        doc.measures.push(ExchangeMeasure {
            cell_layer_name: "grade".into(),
            cell_id: "0_0".into(),
            measure: "ndvi".into(),
            value: 0.72,
            source: Some("sentinel-2".into()),
            scene_name: Some("cena S2 A".into()),
        });
        doc.scenes.push(ExchangeScene {
            name: "cena S2 A".into(),
            acquisition_on: Some("2026-01-31".into()),
            footprint_wkt: Some("POLYGON((-52 -12,-52 -11,-51 -11,-51 -12,-52 -12))".into()),
            metadata: json!({"satellite": "S2"}),
        });
        doc
    }

    /// Round-trip do modelo serde: doc de exemplo → JSON → doc IGUAL.
    #[test]
    fn round_trip_serde_preserva_o_documento() {
        let doc = sample_doc();
        let json = doc.to_json_pretty().expect("serializa");
        let back = ExchangeDoc::from_json(&json).expect("reparse");
        assert_eq!(back, doc);
    }

    /// `float_roundtrip`: f64 não-beco (ex.: coordenada/média de medida)
    /// sobrevive a export→parse→re-export com os MESMOS bytes — sem a feature
    /// do serde_json, o formato curto pode alterar o último dígito.
    #[test]
    fn float_roundtrip_e_byte_a_byte_no_reexport() {
        let valor: f64 = 9_643_762.620_782_405;
        let mut doc = ExchangeDoc::empty(ExchangeProject {
            name: "floats".into(),
            ..ExchangeProject::default()
        });
        doc.measures.push(ExchangeMeasure {
            cell_layer_name: "grade".into(),
            cell_id: "0_0".into(),
            measure: "ndvi".into(),
            value: valor,
            source: None,
            scene_name: None,
        });
        let json1 = doc.to_json_pretty().expect("1º export");
        // O dígito completo está no texto (não foi encurtado na serialização).
        assert!(
            json1.contains("9643762.620782405"),
            "valor encurtado no JSON: {json1}"
        );
        let doc2 = ExchangeDoc::from_json(&json1).expect("reparse");
        assert_eq!(doc2.measures[0].value, valor, "f64 alterado no parse");
        let json2 = doc2.to_json_pretty().expect("2º export");
        assert_eq!(json1, json2, "re-export difere byte a byte do original");
    }

    /// Documento com TODAS as seções vazias é válido (projeto novo).
    #[test]
    fn secoes_vazias_sao_validas() {
        let doc = ExchangeDoc::empty(ExchangeProject {
            name: "vazio".into(),
            ..ExchangeProject::default()
        });
        doc.validate().expect("documento vazio válido");
        let json = doc.to_json_pretty().expect("serializa");
        let back = ExchangeDoc::from_json(&json).expect("reparse");
        assert!(
            back.phases.is_empty()
                && back.aoi_layers.is_empty()
                && back.cell_layers.is_empty()
                && back.classes.is_empty()
                && back.rulesets.is_empty()
                && back.draft_layers.is_empty()
                && back.measures.is_empty()
                && back.scenes.is_empty()
        );
    }

    /// `format` errado é recusado com erro que mostra o valor recebido.
    #[test]
    fn formato_errado_e_recusado() {
        let mut doc = sample_doc();
        doc.format = "outro-exchange".into();
        let json = doc.to_json_pretty().expect("serializa");
        let err = ExchangeDoc::from_json(&json).expect_err("formato errado recusado");
        assert!(err.to_string().contains("outro-exchange"), "{err}");
        assert!(err.to_string().contains(EXCHANGE_FORMAT), "{err}");
    }

    /// `version` diferente de 1 é recusado com erro acionável.
    #[test]
    fn versao_desconhecida_e_recusada() {
        let mut doc = sample_doc();
        doc.version = 2;
        let json = doc.to_json_pretty().expect("serializa");
        let err = ExchangeDoc::from_json(&json).expect_err("versão recusada");
        assert!(err.to_string().contains('2'), "{err}");
    }

    /// Seção desconhecida (typo) é recusada — sem perda silenciosa de dados.
    #[test]
    fn secao_desconhecida_e_recusada() {
        let json = r#"{
            "format": "curupira-monitoring-exchange",
            "version": 1,
            "exported_on": "2026-10-19T12:00:00Z",
            "project": {"name": "p"},
            "ruleset": []
        }"#;
        let err = ExchangeDoc::from_json(json).expect_err("campo desconhecido recusado");
        let msg = err.to_string();
        assert!(msg.contains("line"), "erro deve apontar a linha: {msg}");
    }

    /// WKT inválido é recusado com o contexto do erro.
    #[test]
    fn wkt_invalido_e_recusado() {
        let err = parse_wkt("cells[0].wkt", "POLYGON((").expect_err("parse falha");
        assert!(err.to_string().contains("cells[0].wkt"), "{err}");
        assert!(
            parse_wkt("x", "NOT A WKT").is_err(),
            "texto qualquer não é WKT"
        );
    }

    /// Footprint de cena: POLYGON é aceito (envolvido em MultiPolygon),
    /// POINT é recusado.
    #[test]
    fn footprint_exige_poligono() {
        let mp = scene_footprint_from_wkt("scenes[0]", "POLYGON((0 0,1 0,1 1,0 0))")
            .expect("polygon aceito");
        assert_eq!(mp.0.len(), 1);
        scene_footprint_from_wkt("scenes[0]", "MULTIPOLYGON(((0 0,1 0,1 1,0 0)))")
            .expect("multipolygon aceito");
        let err = scene_footprint_from_wkt("scenes[0]", "POINT(0 0)").expect_err("point recusado");
        assert!(err.to_string().contains("POINT"), "{err}");
    }

    /// Timestamps: RFC-3339 completo e data simples são aceitos; lixo é recusado.
    #[test]
    fn timestamps_iso8601() {
        let a = parse_iso8601("t", "2026-01-31T12:00:00Z").expect("rfc3339");
        assert_eq!(a.year(), 2026);
        let b = parse_iso8601("t", "2026-01-31").expect("data simples");
        assert_eq!(b.year(), 2026);
        assert_eq!(b.month(), time::Month::January);
        let err = parse_iso8601("scenes[0].acquisition_on", "31/01/2026")
            .expect_err("formato BR recusado");
        assert!(err.to_string().contains("31/01/2026"), "{err}");
    }

    /// WKT gerado por [`geometry_to_wkt`] reparseia para a mesma geometria.
    #[test]
    fn wkt_do_motor_reparseia_igual() {
        let original = parse_wkt("t", "MULTIPOLYGON(((0 0,1 0,1 1,0 0)))").expect("parse");
        let text = geometry_to_wkt(&original);
        let back = parse_wkt("t", &text).expect("reparse");
        assert_eq!(original, back);
    }
}
