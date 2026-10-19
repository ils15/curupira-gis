//! Avaliação de **regras de classificação** — motor puro, determinístico e
//! testável sem banco ou GUI.
//!
//! # Gramática (EBNF)
//!
//! ```text
//! predicado  = expr ;
//! expr       = and_expr { "or" and_expr } ;
//! and_expr   = not_expr { "and" not_expr } ;
//! not_expr   = "not" not_expr | primario ;
//! primario   = "(" expr ")" | comparacao ;
//! comparacao = medida operador numero ;
//! medida     = [a-z_] { [a-z0-9_] } ;   (* case-insensitive; normalizada *)
//! operador   = ">" | ">=" | "<" | "<=" | "==" | "!=" ;
//! numero     = [ "+" | "-" ] ( digito { digito } [ "." { digito } ]
//!                            | "." digito { digito } ) ;
//! ```
//!
//! `and` liga mais forte que `or`; `not` liga mais forte que ambos. Só há
//! comparação **medida × número** (não há aritmética nem números à esquerda).
//! A posição nos erros é **1-based em bytes** do texto original. O `numero`
//! aceita as formas `.5`, `5.`, `+.5` (com pelo menos um dígito no total);
//! literais não finitos (`inf` por overflow) ou com mais de 17 dígitos
//! significativos — além da precisão distinguível de `f64` — são rejeitados
//! com [`ta_core::rule::RuleError::NumberOutOfRange`].
//!
//! # Semântica
//!
//! - **Medida ausente:** a comparação é *falsa* (não é erro) e o nome entra em
//!   [`Decision::missing_measures`]. **A ausência avaliada propaga para a
//!   regra inteira, inclusive sob `not`/`or`: regra com comparação de medida
//!   ausente NUNCA casa.** Exemplo: `NOT NDVI >= 1` com a célula **sem**
//!   `ndvi` NÃO casa — em monitoramento, célula sem observação não pode ser
//!   classificada por negação. A ausência é de medidas **efetivamente
//!   avaliadas**: o curto-circuito de `and`/`or` pode ocultar medidas à
//!   direita (elas não entram em `missing_measures` nem impedem o casamento —
//!   a decisão vem só do que foi avaliado). Isso mantém a avaliação total:
//!   qualquer célula pode ser avaliada, mesmo sem todas as bandas/índices.
//! - **Regra inativa** (`is_active = false`) é ignorada pelo motor. O chamador
//!   pode passar todas as regras do ruleset; o filtro é feito aqui (uma única
//!   regra, documentada).
//! - **Predicado inválido** vindo do banco é ignorado defensivamente (a CLI
//!   valida na escrita com [`parse_predicate`]); o motor nunca entra em
//!   pânico — e a contagem/nome das ignoradas sai em
//!   [`Decision::skipped_invalid`]/[`Decision::skipped_invalid_names`] para
//!   dar rastro ao operador.
//! - **Desempate determinístico:** maior `priority` vence; em empate, o **menor
//!   `name`** (ordem lexicográfica de bytes) vence. A ordem de entrada não
//!   altera a decisão.
//!
//! # API
//!
//! [`evaluate`] é a API de avaliação (regras → decisão). `Predicate`
//! expõe apenas `matches_collecting` (o antigo `matches` foi removido como
//! código morto — nenhum chamador de produção o usava).

use std::collections::{BTreeMap, BTreeSet};

use ta_core::rule::{Rule, RuleError};

/// Operador de comparação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

impl CmpOp {
    fn apply(self, left: f64, right: f64) -> bool {
        match self {
            Self::Gt => left > right,
            Self::Ge => left >= right,
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Eq => left == right,
            Self::Ne => left != right,
        }
    }
}

/// Árvore sintática de um predicado.
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Comparison {
        measure: String,
        op: CmpOp,
        value: f64,
    },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

/// Predicado já validado, pronto para ser avaliado contra medidas.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    expr: Expr,
}

impl Predicate {
    /// Avalia e acumula em `missing` os nomes de medidas referenciadas mas
    /// ausentes em `measures` (ordem de avaliação, pode repetir).
    ///
    /// Política de ausência (ver semântica do módulo): a ausência **propaga
    /// para a regra inteira** — se qualquer comparação da expressão referenciou
    /// medida ausente, o resultado é `false`, mesmo que a lógica booleana
    /// (`not`/`or`) pudesse casar. Célula sem observação não é classificada
    /// por negação.
    ///
    /// Nota: o teste de ausência cobre o que **esta chamada** acumulou em
    /// `missing`; reutilize o vetor entre predicados só esvaziando-o antes.
    #[must_use]
    pub fn matches_collecting(
        &self,
        measures: &BTreeMap<String, f64>,
        missing: &mut Vec<String>,
    ) -> bool {
        let before = missing.len();
        let matched = eval_expr(&self.expr, measures, missing);
        matched && missing.len() == before
    }
}

fn eval_expr(expr: &Expr, measures: &BTreeMap<String, f64>, missing: &mut Vec<String>) -> bool {
    match expr {
        Expr::Comparison { measure, op, value } => match measures.get(measure) {
            Some(actual) => op.apply(*actual, *value),
            None => {
                missing.push(measure.clone());
                false
            }
        },
        Expr::And(left, right) => {
            eval_expr(left, measures, missing) && eval_expr(right, measures, missing)
        }
        Expr::Or(left, right) => {
            eval_expr(left, measures, missing) || eval_expr(right, measures, missing)
        }
        Expr::Not(inner) => !eval_expr(inner, measures, missing),
    }
}

/// Interpreta um predicado na gramática documentada no topo do módulo.
///
/// # Errors
/// Devolve [`RuleError`] com a posição (1-based) do primeiro problema.
pub fn parse_predicate(input: &str) -> Result<Predicate, RuleError> {
    if input.trim().is_empty() {
        return Err(RuleError::Empty);
    }
    let mut parser = Parser::new(input);
    let expr = parser.parse_expr()?;
    parser.finish()?;
    Ok(Predicate { expr })
}

/// Dígitos significativos máximos aceitos num literal — `f64` distingue até
/// 17 (garantia de round-trip); além disso o valor gravado não é o escrito.
const MAX_SIGNIFICANT_DIGITS: usize = 17;

/// Conta dígitos significativos do literal: ignora sinal, ponto e zeros à
/// esquerda (a parte inteira conta; zeros de borda na fração final não).
fn significant_digits(text: &str) -> usize {
    let digits: Vec<char> = text.chars().filter(char::is_ascii_digit).collect();
    let first_significant = digits
        .iter()
        .position(|&d| d != '0')
        .unwrap_or(digits.len());
    digits[first_significant..].len()
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.rest().chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            self.pos += ch.len_utf8();
        }
    }

    /// Posição 1-based para mensagens (0 é sempre inválido na entrada).
    fn human_pos(&self) -> usize {
        self.pos + 1
    }

    fn parse_expr(&mut self) -> Result<Expr, RuleError> {
        let mut left = self.parse_and()?;
        while self.consume_keyword("or") {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, RuleError> {
        let mut left = self.parse_not()?;
        while self.consume_keyword("and") {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, RuleError> {
        if self.consume_keyword("not") {
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, RuleError> {
        self.skip_ws();
        if self.rest().starts_with('(') {
            self.pos += 1;
            let inner = self.parse_expr()?;
            self.skip_ws();
            if !self.rest().starts_with(')') {
                return Err(self.expected("`)`"));
            }
            self.pos += 1;
            return Ok(inner);
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, RuleError> {
        let measure = self.parse_measure()?;
        let op = self.parse_operator()?;
        let value = self.parse_number()?;
        Ok(Expr::Comparison { measure, op, value })
    }

    fn parse_measure(&mut self) -> Result<String, RuleError> {
        self.skip_ws();
        let Some(first) = self.rest().chars().next() else {
            return Err(RuleError::UnexpectedEnd {
                expected: "nome de medida",
                pos: self.human_pos(),
            });
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(self.expected("nome de medida"));
        }
        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let name = self.input[start..self.pos].to_ascii_lowercase();
        if matches!(name.as_str(), "and" | "or" | "not") {
            return Err(self.expected("nome de medida"));
        }
        Ok(name)
    }

    fn parse_operator(&mut self) -> Result<CmpOp, RuleError> {
        self.skip_ws();
        let start = self.pos;
        let rest = self.rest();
        let (op, len) = if rest.starts_with(">=") {
            (CmpOp::Ge, 2)
        } else if rest.starts_with("<=") {
            (CmpOp::Le, 2)
        } else if rest.starts_with("==") {
            (CmpOp::Eq, 2)
        } else if rest.starts_with("!=") {
            (CmpOp::Ne, 2)
        } else if rest.starts_with('>') {
            (CmpOp::Gt, 1)
        } else if rest.starts_with('<') {
            (CmpOp::Lt, 1)
        } else if rest.starts_with('=') {
            return Err(RuleError::InvalidOperator { pos: start + 1 });
        } else {
            return Err(self.expected("operador (>, >=, <, <=, ==, !=)"));
        };
        self.pos += len;
        Ok(op)
    }

    fn parse_number(&mut self) -> Result<f64, RuleError> {
        self.skip_ws();
        let start = self.pos;
        let rest = self.rest();
        let Some(first) = rest.chars().next() else {
            return Err(RuleError::UnexpectedEnd {
                expected: "número",
                pos: self.human_pos(),
            });
        };
        if !(first.is_ascii_digit() || first == '+' || first == '-' || first == '.') {
            return Err(self.expected("número"));
        }
        if first == '+' || first == '-' {
            self.pos += 1;
        }
        let mut digits = 0usize;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_ascii_digit() {
                digits += 1;
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.rest().starts_with('.') {
            self.pos += 1;
            while let Some(ch) = self.rest().chars().next() {
                if ch.is_ascii_digit() {
                    digits += 1;
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        if digits == 0 {
            return Err(RuleError::InvalidNumber {
                text: self.input[start..self.pos].to_owned(),
                pos: start + 1,
            });
        }
        let text = &self.input[start..self.pos];
        let value: f64 = text.parse::<f64>().map_err(|_| RuleError::InvalidNumber {
            text: text.to_owned(),
            pos: start + 1,
        })?;
        // Overflow para `inf` (ex.: 400 dígitos) ou literal com mais dígitos
        // significativos do que `f64` distingue (ex.: 32 noves): a comparação
        // deixaria de ser a que o operador escreveu. Fora de intervalo.
        if !value.is_finite() || significant_digits(text) > MAX_SIGNIFICANT_DIGITS {
            return Err(RuleError::NumberOutOfRange {
                text: text.to_owned(),
                pos: start + 1,
            });
        }
        Ok(value)
    }

    /// Consome `kw` (case-insensitive) se for a próxima palavra.
    fn consume_keyword(&mut self, kw: &str) -> bool {
        self.skip_ws();
        let rest = self.rest();
        let Some(prefix) = rest.get(..kw.len()) else {
            return false;
        };
        if !prefix.eq_ignore_ascii_case(kw) {
            return false;
        }
        let after = rest.as_bytes().get(kw.len()).copied();
        let boundary = after.is_none_or(|b| !(b.is_ascii_alphanumeric() || b == b'_'));
        if boundary {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    fn finish(&mut self) -> Result<(), RuleError> {
        self.skip_ws();
        if self.pos < self.input.len() {
            return Err(self.expected("fim da expressão ou operador lógico"));
        }
        Ok(())
    }

    fn expected(&self, expected: &'static str) -> RuleError {
        RuleError::Expected {
            expected,
            pos: self.human_pos(),
        }
    }
}

/// Resultado determinístico da avaliação de um conjunto de regras.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decision {
    /// Classe vencedora (identificador opaco — `classes.id`), se alguma regra
    /// verdadeira venceu.
    pub class_id: Option<String>,
    /// Regra vencedora (`rules.id`), se houve.
    pub rule_id: Option<String>,
    /// Quantidade de regras **ativas e com predicado válido** avaliadas.
    pub evaluated: usize,
    /// Medidas **efetivamente avaliadas e ausentes** (o curto-circuito de
    /// `and`/`or` pode ocultar medidas à direita), únicas e em ordem
    /// lexicográfica — explicação da decisão.
    pub missing_measures: Vec<String>,
    /// Quantidade de regras ativas ignoradas por **predicado inválido**
    /// (defensivo: a CLI valida na escrita; o banco de procedência duvidosa
    /// não derruba o motor).
    pub skipped_invalid: usize,
    /// Nomes (`rules.name`) das regras ignoradas por predicado inválido, em
    /// ordem — rastro para o operador corrigir a regra de origem.
    pub skipped_invalid_names: Vec<String>,
}

/// Conjunto de regras **já parseadas** — o parse acontece uma vez
/// (em [`prepare_rules`]), nunca por célula; a avaliação reutiliza este
/// conjunto ([`evaluate_prepared`]). Regras inativas são descartadas no
/// prepare; predicados inválidos são contados/nomeados uma única vez.
pub(crate) struct PreparedRules {
    /// Regras ativas com predicado válido, na ordem de entrada.
    rules: Vec<(Rule, Predicate)>,
    /// Quantidade de regras ativas ignoradas por predicado inválido.
    skipped_invalid: usize,
    /// Nomes das regras ignoradas (ordenados; rastro para o operador).
    skipped_invalid_names: Vec<String>,
}

/// Parseia `rules` **uma vez** para avaliação em lote — o custo do parser é
/// pago por lote, não por célula. Regras inativas não entram.
pub(crate) fn prepare_rules(rules: &[Rule]) -> PreparedRules {
    let mut prepared = Vec::new();
    let mut skipped_invalid = 0usize;
    let mut skipped_names = Vec::new();
    for rule in rules {
        if !rule.is_active {
            continue;
        }
        match parse_predicate(&rule.predicate) {
            Ok(predicate) => prepared.push((rule.clone(), predicate)),
            Err(_) => {
                skipped_invalid += 1;
                skipped_names.push(rule.name.clone());
            }
        }
    }
    // Nomes em ordem, sem depender da ordem de entrada das regras.
    skipped_names.sort();
    PreparedRules {
        rules: prepared,
        skipped_invalid,
        skipped_invalid_names: skipped_names,
    }
}

/// Avalia um conjunto já parseado contra as medidas de UMA célula — mesma
/// semântica de [`evaluate`] (maior prioridade vence; empate → menor nome),
/// sem repetir o parse das regras.
pub(crate) fn evaluate_prepared(
    prepared: &PreparedRules,
    measures: &BTreeMap<String, f64>,
) -> Decision {
    let mut winner: Option<&Rule> = None;
    let mut missing = BTreeSet::new();

    for (rule, predicate) in &prepared.rules {
        let mut rule_missing = Vec::new();
        if predicate.matches_collecting(measures, &mut rule_missing) {
            let wins = winner.is_none_or(|current| {
                rule.priority > current.priority
                    || (rule.priority == current.priority && rule.name < current.name)
            });
            if wins {
                winner = Some(rule);
            }
        }
        missing.extend(rule_missing);
    }

    Decision {
        class_id: winner.map(|rule| rule.class_id.clone()),
        rule_id: winner.map(|rule| rule.id.clone()),
        evaluated: prepared.rules.len(),
        missing_measures: missing.into_iter().collect(),
        skipped_invalid: prepared.skipped_invalid,
        skipped_invalid_names: prepared.skipped_invalid_names.clone(),
    }
}

/// Avalia `rules` contra `measures` e devolve a decisão de classe.
///
/// Regras inativas e predicados inválidos são ignorados (ver semântica no topo
/// do módulo). Maior `priority` vence; empate → menor `name`.
#[must_use]
pub fn evaluate(rules: &[Rule], measures: &BTreeMap<String, f64>) -> Decision {
    // Parse uma única vez; a semântica é idêntica ao caminho preparado.
    evaluate_prepared(&prepare_rules(rules), measures)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ta_core::rule::Rule;

    use super::{Predicate, evaluate, parse_predicate};

    fn measures(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    /// Aparato de teste: `Predicate::matches` foi removida da API (F11 —
    /// código morto; `evaluate` é a API de avaliação), então os testes usam
    /// `matches_collecting` diretamente.
    fn is_match(pred: &Predicate, pairs: &[(&str, f64)]) -> bool {
        let mut missing = Vec::new();
        pred.matches_collecting(&measures(pairs), &mut missing)
    }

    fn rule(name: &str, class: &str, priority: i32, predicate: &str) -> Rule {
        let mut rule = Rule::new("rs1", class, name, predicate).with_priority(priority);
        rule.id = name.to_owned();
        rule
    }

    /// `and` liga mais forte que `or`: `a or b and c` = `a or (b and c)`.
    #[test]
    fn and_tem_precedencia_sobre_or() {
        let pred = parse_predicate("a == 1 or b == 1 and c == 1").expect("predicado válido");
        assert!(is_match(&pred, &[("a", 1.0), ("b", 1.0), ("c", 0.0)]));
        // Se `or` ligasse mais forte, (a|b)&c seria falso aqui.
        let pred = parse_predicate("(a == 1 or b == 1) and c == 1").expect("predicado válido");
        assert!(!is_match(&pred, &[("a", 1.0), ("b", 1.0), ("c", 0.0)]));
    }

    #[test]
    fn parenteses_mudam_o_agrupamento() {
        let pred =
            parse_predicate("(ndvi >= 0.6 or nbr < 0.1) and water == 1").expect("predicado válido");
        assert!(!is_match(
            &pred,
            &[("ndvi", 0.8), ("nbr", 0.5), ("water", 0.0)]
        ));
        assert!(is_match(
            &pred,
            &[("ndvi", 0.8), ("nbr", 0.5), ("water", 1.0)]
        ));
    }

    #[test]
    fn not_inverte_a_comparacao() {
        let pred = parse_predicate("not (ndvi >= 0.6)").expect("predicado válido");
        assert!(is_match(&pred, &[("ndvi", 0.2)]));
        assert!(!is_match(&pred, &[("ndvi", 0.6)]));
    }

    /// F3 — ausência propaga para a regra INTEIRA, inclusive sob `not`:
    /// célula sem observação não pode ser classificada por negação.
    #[test]
    fn not_com_medida_ausente_nunca_casa() {
        let pred = parse_predicate("not ndvi >= 1").expect("predicado válido");
        assert!(
            !is_match(&pred, &[]),
            "`not` de comparação com medida ausente não pode virar verdadeiro"
        );
        let mut missing = Vec::new();
        assert!(!pred.matches_collecting(&measures(&[]), &mut missing));
        assert_eq!(missing, vec!["ndvi".to_owned()]);
    }

    /// F3 — idem sob `or`: o lado de medida ausente **avaliado** envenena a
    /// regra inteira; o lado à direita ocultado por curto-circuito não
    /// (medidas "efetivamente avaliadas", ver `missing_measures`).
    #[test]
    fn or_com_medida_ausente_nunca_casa() {
        // Ausente à ESQUERDA (avaliada): `or` com o outro lado verdadeiro
        // não pode mascarar a ausência.
        let pred = parse_predicate("agua == 1 or ndvi >= 0.6").expect("predicado válido");
        assert!(!is_match(&pred, &[("ndvi", 0.9)]));
        assert!(is_match(&pred, &[("ndvi", 0.9), ("agua", 1.0)]));

        // Ausente à DIREITA ocultada pelo curto-circuito: a decisão vem só
        // das comparações efetivamente avaliadas e casa.
        let pred = parse_predicate("ndvi >= 0.6 or agua == 1").expect("predicado válido");
        assert!(is_match(&pred, &[("ndvi", 0.9)]));
        let mut missing = Vec::new();
        assert!(pred.matches_collecting(&measures(&[("ndvi", 0.9)]), &mut missing));
        assert!(missing.is_empty(), "à direita não avaliada: {missing:?}");
    }

    /// F3 — o caso da revisão: regra que decidia `agua` por `not` sem ter a
    /// medida vira "nenhuma classe" no `evaluate` (API real).
    #[test]
    fn regra_not_sem_medida_nao_decide_classe() {
        let rules = vec![rule("agua", "c-agua", 10, "not ndvi >= 1")];
        let decision = evaluate(&rules, &measures(&[("nbr", 0.05)]));
        assert_eq!(decision.class_id, None, "nenhuma classe sem observação");
        assert_eq!(decision.rule_id, None);
        assert_eq!(decision.evaluated, 1);
        assert_eq!(decision.missing_measures, vec!["ndvi".to_owned()]);
    }

    #[test]
    fn todos_os_operadores_e_numeros_com_sinal() {
        let cases = [
            ("x > -0.5", 0.0, true),
            ("x >= -0.5", -0.5, true),
            ("x < 1.25", 1.0, true),
            ("x <= 1.25", 1.25, true),
            ("x == 2", 2.0, true),
            ("x != 2", 2.0, false),
        ];
        for (expr, value, expected) in cases {
            let pred = parse_predicate(expr).expect("predicado válido");
            assert_eq!(
                is_match(&pred, &[("x", value)]),
                expected,
                "expressão `{expr}` com x={value}"
            );
        }
    }

    #[test]
    fn identificadores_sao_case_insensitive_e_normalizados() {
        let pred = parse_predicate("NDVI >= 0.6 AND Not NBR <= 0.1").expect("válido");
        assert!(is_match(&pred, &[("ndvi", 0.7), ("nbr", 0.2)]));
        assert!(!is_match(&pred, &[("ndvi", 0.7), ("nbr", 0.05)]));
    }

    #[test]
    fn sintaxe_invalida_traz_a_posicao() {
        let err = parse_predicate("ndvi 0.6").expect_err("falta operador");
        assert!(
            err.to_string().contains("posição 6"),
            "mensagem sem posição: {err}"
        );

        let err = parse_predicate("ndvi >=").expect_err("falta número");
        assert!(err.to_string().contains("incompleta"), "{err}");

        let err = parse_predicate("ndvi = 0.6").expect_err("operador inválido");
        assert!(err.to_string().contains("operador inválido"), "{err}");

        let err = parse_predicate("ndvi >= 0.6 and").expect_err("final incompleto");
        assert!(err.to_string().contains("incompleta"), "{err}");

        let err = parse_predicate("   ").expect_err("vazio");
        assert!(err.to_string().contains("predicado vazio"), "{err}");
    }

    #[test]
    fn formas_de_numero_aceitas_pelo_parser() {
        // F9 — `.5`, `5.` e `+.5` são aceitos (a EBNF do módulo documenta).
        let cases = [
            (".5", 0.5, true),
            ("5.", 5.0, true),
            ("+.5", 0.5, true),
            ("-5.", -5.0, true),
            ("007", 7.0, true),
            ("0.0001", 0.0001, true),
        ];
        for (text, value, ok) in cases {
            let parsed = parse_predicate(&format!("x == {text}"));
            assert_eq!(parsed.is_ok(), ok, "literal `{text}`");
            if let Ok(pred) = parsed {
                assert!(is_match(&pred, &[("x", value)]), "valor de `{text}`");
            }
        }
        // Ponto sozinho (nenhum dígito) continua inválido.
        assert!(parse_predicate("x == .").is_err());
    }

    /// F10 — literal além da precisão/intervalo de `f64` é rejeitado com
    /// erro tipado ("número fora de intervalo"), não silenciosamente
    /// arredondado/overflowado.
    #[test]
    fn numeros_fora_de_intervalo_sao_rejeitados() {
        // 32 dígitos: finito, mas além dos 17 que f64 distingue.
        let n32 = "9".repeat(32);
        let err = parse_predicate(&format!("ndvi >= {n32}")).expect_err("32 dígitos");
        assert!(
            err.to_string().contains("fora de intervalo"),
            "mensagem acionável ausente: {err}"
        );

        // 400 dígitos: overflow para inf — precisa ser rejeitado também.
        let n400 = "9".repeat(400);
        let err = parse_predicate(&format!("ndvi < {n400}")).expect_err("overflow");
        assert!(err.to_string().contains("fora de intervalo"), "{err}");

        // Zero à esquerda não conta como significativo: segue válido.
        let zeros = format!("0.{}", "0".repeat(20));
        assert!(parse_predicate(&format!("ndvi >= {zeros}")).is_ok());

        // 17 dígitos significativos: no limite, aceito.
        let n17 = "9".repeat(17);
        assert!(parse_predicate(&format!("ndvi >= {n17}")).is_ok());
    }

    #[test]
    fn medida_ausente_torna_a_comparacao_falsa_e_e_registrada() {
        let pred = parse_predicate("ndvi >= 0.6 and nbr < 0.1").expect("válido");
        let mut missing = Vec::new();
        let matched = pred.matches_collecting(&measures(&[("ndvi", 0.8)]), &mut missing);
        assert!(!matched, "nbr ausente → comparação falsa");
        assert_eq!(missing, vec!["nbr".to_owned()]);
    }

    #[test]
    fn prioridade_maior_vence_independente_da_ordem() {
        let rules = vec![
            rule("baixa", "c-baixa", 10, "ndvi >= 0.5"),
            rule("alta", "c-alta", 20, "ndvi >= 0.5"),
        ];
        let decision = evaluate(&rules, &measures(&[("ndvi", 0.7)]));
        assert_eq!(decision.class_id.as_deref(), Some("c-alta"));
        assert_eq!(decision.rule_id.as_deref(), Some("alta"));
        assert_eq!(decision.evaluated, 2);
    }

    #[test]
    fn empate_de_prioridade_desempata_pelo_menor_nome() {
        let rules = vec![
            rule("zebra", "c-zebra", 5, "ndvi >= 0.5"),
            rule("alfa", "c-alfa", 5, "ndvi >= 0.5"),
        ];
        let decision = evaluate(&rules, &measures(&[("ndvi", 0.7)]));
        assert_eq!(decision.class_id.as_deref(), Some("c-alfa"));
        assert_eq!(decision.rule_id.as_deref(), Some("alfa"));
    }

    #[test]
    fn sem_regra_verdadeira_nao_ha_decisao() {
        let rules = vec![rule("r1", "c1", 0, "ndvi >= 0.9")];
        let decision = evaluate(&rules, &measures(&[("ndvi", 0.1)]));
        assert_eq!(decision.class_id, None);
        assert_eq!(decision.rule_id, None);
        assert_eq!(decision.evaluated, 1);
        assert!(decision.missing_measures.is_empty());
    }

    #[test]
    fn lista_vazia_devolve_decisao_vazia() {
        let decision = evaluate(&[], &measures(&[("ndvi", 0.7)]));
        assert_eq!(decision.class_id, None);
        assert_eq!(decision.evaluated, 0);
    }

    #[test]
    fn regra_inativa_e_ignorada() {
        let mut inactive = rule("inativa", "c-inativa", 99, "ndvi >= 0.0");
        inactive.is_active = false;
        let decision = evaluate(
            &[inactive, rule("ativa", "c-ativa", 0, "ndvi >= 0.0")],
            &measures(&[("ndvi", 0.7)]),
        );
        assert_eq!(decision.class_id.as_deref(), Some("c-ativa"));
        assert_eq!(decision.evaluated, 1, "inativa não conta como avaliada");
    }

    #[test]
    fn avaliacao_e_deterministica_inclusive_com_ordem_trocada() {
        let rules = vec![
            rule("b", "c-b", 5, "ndvi >= 0.5"),
            rule("a", "c-a", 5, "ndvi >= 0.5"),
        ];
        let m = measures(&[("ndvi", 0.7)]);
        let first = evaluate(&rules, &m);
        let second = evaluate(&rules, &m);
        assert_eq!(first, second);

        let mut reversed = rules.clone();
        reversed.reverse();
        assert_eq!(
            evaluate(&reversed, &m).class_id,
            first.class_id,
            "a ordem de entrada não pode mudar a decisão"
        );
    }

    #[test]
    fn predicado_invalido_no_banco_nao_derruba_o_motor() {
        // A CLI valida na escrita; o motor é defensivo e apenas ignora a regra
        // — mas deixa RASTRO na decisão (F4).
        let decision = evaluate(
            &[rule("quebrada", "c1", 9, "ndvi >>>")],
            &measures(&[("ndvi", 0.7)]),
        );
        assert_eq!(decision.class_id, None);
        assert_eq!(decision.evaluated, 0);
        assert_eq!(decision.skipped_invalid, 1);
        assert_eq!(decision.skipped_invalid_names, vec!["quebrada".to_owned()]);
    }

    /// F4 — regra válida e inválida juntas: a válida decide, a inválida sai
    /// nomeada e ordenada na explicação.
    #[test]
    fn regras_invalidas_saem_contadas_e_nomeadas() {
        let rules = vec![
            rule("zebra quebrada", "c1", 1, "ndvi >>> 0.5"),
            rule("boa", "c2", 1, "ndvi >= 0.5"),
            rule("alfa quebrada", "c3", 1, "lixo"),
        ];
        let decision = evaluate(&rules, &measures(&[("ndvi", 0.7)]));
        assert_eq!(decision.class_id.as_deref(), Some("c2"));
        assert_eq!(decision.skipped_invalid, 2);
        assert_eq!(
            decision.skipped_invalid_names,
            vec!["alfa quebrada".to_owned(), "zebra quebrada".to_owned()]
        );
    }

    #[test]
    fn medidas_ausentes_saem_unicas_e_ordenadas() {
        let rules = vec![
            rule("r1", "c1", 0, "nbr < 0.1"),
            rule("r2", "c2", 0, "ndvi >= 0.5 and nbr < 0.1"),
        ];
        let decision = evaluate(&rules, &measures(&[("swir", 0.3)]));
        assert_eq!(
            decision.missing_measures,
            vec!["nbr".to_owned(), "ndvi".to_owned()]
        );
    }
}
