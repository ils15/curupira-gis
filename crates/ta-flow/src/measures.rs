//! **Preparo do fluxo — import de medidas por CSV** (Fase 5b).
//!
//! Formato **LONG** (uma observação por linha), com cabeçalho opcional:
//!
//! ```text
//! cell_id,measure,value[,scene]
//! 0_0,ndvi,0.71,cena-2026-10
//! 0_0,nbr,0.12
//! ```
//!
//! Regras de domínio (as mesmas do `measure set`, sem duplicação):
//! - `cell_id` precisa existir na grade (ids são `"<col>_<row>"`);
//! - `measure` segue a gramática dos predicados (`[a-z_][a-z0-9_]*`);
//! - `value` precisa ser um número **finito** (NaN/inf seriam decisões
//!   silenciosas e nunca casariam regra);
//! - `scene` (opcional) resolve por NOME; cena inexistente é linha ignorada;
//! - o upsert é **idempotente** (`ON CONFLICT (cell_layer_id, cell_id,
//!   measure) DO UPDATE`): reimportar o mesmo arquivo reescreve os mesmos
//!   valores e não cria duplicatas.
//!
//! Linhas inválidas **não abortam o import**: entram no relatório como
//! ignoradas, com o número da linha e o motivo. Erros de IO/banco abortam.
//!
//! O parser é CSV **simples** (vírgula, sem aspas) — o formato é de máquina e
//! os campos (`cell_id`, nome de medida, número, nome de cena) não contêm
//! vírgula. Documentado no README; aspas seriam complexidade sem uso (YAGNI).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ta_core::classification::CellMeasure;
use ta_db::repo_cell::{CellLayerRepo, PgCellLayerRepo};
use ta_db::repo_scene::{PgSceneRepo, SceneRepo};
use ta_db::repo_task::PgTaskRepo;
use ta_db::sqlx::PgPool;

use crate::error::FlowError;

/// Linha ignorada do CSV (motivo acionável, 1-based como no editor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportIssue {
    /// Número da linha no arquivo (1-based).
    pub line: usize,
    /// Por que a linha não foi gravada.
    pub reason: String,
}

/// Relatório do import de medidas (`lidas`/`gravadas`/`ignoradas`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeasuresImportReport {
    /// Linhas de dados lidas (exclui cabeçalho e linhas em branco).
    pub read: usize,
    /// Linhas efetivamente gravadas (upsert).
    pub written: u64,
    /// Linhas lidas e não gravadas (`issues` traz uma entrada por linha).
    pub skipped: usize,
    /// Motivos das linhas ignoradas (mesma ordem do arquivo).
    pub issues: Vec<ImportIssue>,
}

/// Gramática do nome de medida (mesma dos predicados do motor e do
/// `measure set`): minúsculas/dígitos/`_`, começando por letra ou `_`.
#[must_use]
pub fn is_valid_measure_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Importa o CSV `path` para a grade `cell_layer_id`.
///
/// Cenas são resolvidas por NOME (globais, como no `measure set --scene`) e
/// células pela lista de ids da própria grade — uma consulta cada, sem N+1.
/// As linhas válidas são gravadas na MESMA transação (relatório só depois do
/// commit).
pub async fn import_csv(
    pool: &PgPool,
    cell_layer_id: &str,
    path: &str,
) -> Result<MeasuresImportReport, FlowError> {
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await?
        .ok_or_else(|| FlowError::CellLayerNotFound(cell_layer_id.to_owned()))?;

    let text = std::fs::read_to_string(Path::new(path)).map_err(|e| FlowError::MeasuresCsv {
        path: path.to_owned(),
        message: e.to_string(),
    })?;

    // Índices de validação: ids da grade + cenas por nome.
    let cell_ids: HashSet<String> = ta_db::repo_cell::PgCellRepo::new(pool.clone())
        .cell_ids(&layer.id)
        .await?;
    let scenes: HashMap<String, String> = PgSceneRepo::new(pool.clone())
        .list(None)
        .await?
        .into_iter()
        .map(|scene| (scene.name, scene.id))
        .collect();

    let mut report = MeasuresImportReport::default();
    let mut rows: Vec<CellMeasure> = Vec::new();
    let now = time::OffsetDateTime::now_utc();
    let mut header_seen = false;

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue; // branco/comentário: não é linha de dados
        }
        let fields: Vec<&str> = raw.split(',').map(str::trim).collect();
        // Cabeçalho opcional: só a primeira linha de dados pode ser o header.
        if !header_seen
            && fields
                .first()
                .is_some_and(|f| f.eq_ignore_ascii_case("cell_id"))
        {
            header_seen = true;
            continue;
        }
        header_seen = true;
        report.read += 1;

        let skipped = |report: &mut MeasuresImportReport, reason: String| {
            report.skipped += 1;
            report.issues.push(ImportIssue { line, reason });
        };
        if !(3..=4).contains(&fields.len()) {
            skipped(
                &mut report,
                "esperado cell_id,measure,value[,scene]".to_owned(),
            );
            continue;
        }
        let (cell_id, measure, value_text) = (fields[0], fields[1], fields[2]);
        if cell_id.is_empty() {
            skipped(&mut report, "cell_id vazio".to_owned());
            continue;
        }
        if !cell_ids.contains(cell_id) {
            skipped(
                &mut report,
                format!("célula `{cell_id}` não existe na grade `{}`", layer.name),
            );
            continue;
        }
        if !is_valid_measure_name(measure) {
            skipped(
                &mut report,
                format!(
                    "medida inválida `{measure}`: use letras minúsculas, dígitos e `_` \
                     (começando por letra ou `_`)"
                ),
            );
            continue;
        }
        let Ok(value) = value_text.parse::<f64>() else {
            skipped(
                &mut report,
                format!("valor inválido `{value_text}`: use um número (ex.: 0.7)"),
            );
            continue;
        };
        if !value.is_finite() {
            skipped(
                &mut report,
                format!("valor não finito `{value_text}`: use um número finito"),
            );
            continue;
        }
        let scene_id = match fields.get(3).copied().filter(|s| !s.is_empty()) {
            None => None,
            Some(name) => match scenes.get(name) {
                Some(id) => Some(id.clone()),
                None => {
                    skipped(
                        &mut report,
                        format!("cena `{name}` não existe; crie com `ta-cli scene add`"),
                    );
                    continue;
                }
            },
        };
        rows.push(CellMeasure {
            cell_layer_id: layer.id.clone(),
            cell_id: cell_id.to_owned(),
            measure: measure.to_owned(),
            value,
            source: None,
            scene_id,
            updated_on: now,
        });
    }

    if !rows.is_empty() {
        let mut tx = pool.begin().await?;
        let written = PgTaskRepo::upsert_measures_on(&mut *tx, &rows).await?;
        tx.commit().await?;
        report.written = written;
    }
    Ok(report)
}

/// Recorte dos motivos para exibição (CLI imprime os primeiros N; a GUI usa o
/// mesmo recorte para não estourar o painel). O total continua em `skipped`.
#[must_use]
pub fn issues_for_display(report: &MeasuresImportReport, max: usize) -> Vec<&ImportIssue> {
    report.issues.iter().take(max).collect()
}
