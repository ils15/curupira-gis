//! Classificação **em lote** — motor puro, determinístico e sem IO.
//!
//! [`classify_batch`] avalia um conjunto de células contra as regras e devolve
//! uma decisão por célula, **na ordem de entrada**. É o coração do executor da
//! fila de processos (Fase 3): o estado (fila, status, log) é responsabilidade
//! da persistência — aqui não há IO, status nem efeito colateral.
//!
//! Contrato:
//! - **determinístico:** mesma entrada (células + regras) → mesma saída; a
//!   decisão por célula é exatamente a de [`evaluate`] (maior prioridade vence,
//!   empate → menor nome; medidas ausentes nunca decidem);
//! - **parse uma vez por lote:** as regras são parseadas no início do lote
//!   ([`ta_engine::rule`] usa o mesmo caminho preparado), nunca por célula;
//! - **ordem de entrada preservada:** `output[i]` corresponde a `cells[i]`;
//! - lote vazio → saída vazia.

use std::collections::BTreeMap;

use ta_core::rule::Rule;

use crate::rule::{Decision, evaluate_prepared, prepare_rules};

/// Uma célula de entrada do lote: identificador + medidas observadas.
#[derive(Debug, Clone, PartialEq)]
pub struct CellInput {
    /// Identificador determinístico da célula (`"<col>_<row>"`).
    pub cell_id: String,
    /// Medidas observadas (nomes na gramática dos predicados).
    pub measures: BTreeMap<String, f64>,
}

impl CellInput {
    /// Cria a entrada de uma célula.
    #[must_use]
    pub fn new(cell_id: &str, measures: BTreeMap<String, f64>) -> Self {
        Self {
            cell_id: cell_id.to_owned(),
            measures,
        }
    }
}

/// Decisão de UMA célula do lote, na posição de entrada.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassOutput {
    /// Identificador da célula (eco da entrada — o chamador não precisa
    /// confiar na ordem para saber de quem é a decisão).
    pub cell_id: String,
    /// Decisão do motor para a célula (`class_id: None` = nenhuma classe).
    pub decision: Decision,
}

/// Avalia um lote de células contra `rules`.
///
/// Determinístico, ordem de entrada preservada; regras inativas e predicados
/// inválidos são ignorados com rastro no [`Decision`] (mesma semântica de
/// [`evaluate`](crate::evaluate) por célula, com parse único por lote).
#[must_use]
pub fn classify_batch(cells: &[CellInput], rules: &[Rule]) -> Vec<ClassOutput> {
    // Parse UMA vez por lote — nunca por célula (risco apontado na revisão).
    let prepared = prepare_rules(rules);
    cells
        .iter()
        .map(|cell| ClassOutput {
            cell_id: cell.cell_id.clone(),
            decision: evaluate_prepared(&prepared, &cell.measures),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ta_core::rule::Rule;

    use super::{CellInput, classify_batch};

    fn rule(name: &str, class: &str, priority: i32, predicate: &str) -> Rule {
        let mut rule = Rule::new("rs1", class, name, predicate).with_priority(priority);
        rule.id = format!("rule_{name}");
        rule
    }

    fn cell(cell_id: &str, pairs: &[(&str, f64)]) -> CellInput {
        CellInput::new(
            cell_id,
            pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
        )
    }

    /// Mesma entrada → mesma saída, decisão a decisão (determinismo do lote).
    #[test]
    fn lote_e_deterministico() {
        let rules = [
            rule("floresta", "c-floresta", 10, "ndvi >= 0.6"),
            rule("agua", "c-agua", 5, "water == 1"),
        ];
        let cells = [
            cell("0_0", &[("ndvi", 0.8), ("water", 0.0)]),
            cell("0_1", &[("ndvi", 0.2), ("water", 1.0)]),
            cell("1_0", &[("ndvi", 0.3), ("water", 0.0)]),
        ];
        let first = classify_batch(&cells, &rules);
        let second = classify_batch(&cells, &rules);
        assert_eq!(first, second);
        assert_eq!(first[0].decision.class_id.as_deref(), Some("c-floresta"));
        assert_eq!(first[1].decision.class_id.as_deref(), Some("c-agua"));
        assert_eq!(first[2].decision.class_id, None);
    }

    /// `output[i]` corresponde a `cells[i]`, mesmo com decisões distintas.
    #[test]
    fn ordem_de_entrada_e_preservada() {
        let rules = [rule("r", "c1", 1, "ndvi >= 0.6")];
        let cells = [
            cell("2_1", &[("ndvi", 0.1)]),
            cell("0_0", &[("ndvi", 0.9)]),
            cell("1_2", &[]),
        ];
        let out = classify_batch(&cells, &rules);
        let ids: Vec<&str> = out.iter().map(|o| o.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["2_1", "0_0", "1_2"]);
        assert_eq!(out[0].decision.class_id, None);
        assert_eq!(out[1].decision.class_id.as_deref(), Some("c1"));
    }

    /// Célula sem medidas → "nenhuma classe" + medidas ausentes explicadas.
    /// `or` não tem curto-circuito quando o lado esquerdo é falso: ambas as
    /// medidas ausentes aparecem (semântica documentada no motor).
    #[test]
    fn celula_sem_medidas_nao_decide_e_explica_ausentes() {
        let rules = [rule("r", "c1", 1, "ndvi >= 0.6 or nbr < 0.1")];
        let out = classify_batch(&[cell("0_0", &[])], &rules);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].decision.class_id, None);
        assert_eq!(
            out[0].decision.missing_measures,
            vec!["nbr".to_owned(), "ndvi".to_owned()]
        );
    }

    /// Lote vazio → saída vazia (e regras nem são parseadas com efeito).
    #[test]
    fn lote_vazio_devolve_vazio() {
        let rules = [rule("r", "c1", 1, "ndvi >= 0.6")];
        assert!(classify_batch(&[], &rules).is_empty());
    }

    /// Regras duplicadas (mesmo nome/prioridade) não mudam a decisão nem
    /// tornam o lote não determinístico: a primeira da ordem de entrada vence
    /// o empate, em qualquer célula do lote.
    #[test]
    fn regras_duplicadas_mantem_decisao_deterministica() {
        let rules = [
            rule("dup", "c-primeira", 5, "ndvi >= 0.5"),
            rule("dup", "c-segunda", 5, "ndvi >= 0.5"),
        ];
        let cells = [cell("0_0", &[("ndvi", 0.9)]), cell("0_1", &[("ndvi", 0.2)])];
        let out = classify_batch(&cells, &rules);
        assert_eq!(out[0].decision.class_id.as_deref(), Some("c-primeira"));
        assert_eq!(out[1].decision.class_id, None);
        assert_eq!(out[0].decision.evaluated, 2);
    }
}
