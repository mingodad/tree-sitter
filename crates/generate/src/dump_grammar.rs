//! Dump the grammar that the parse table is *actually* built from.
//!
//! This is not the grammar an author writes. By the time the table builder runs,
//! the EBNF sugar has been lowered: `repeat(x)` and `repeat1(x)` became
//! left-recursive auxiliary rules called `{rule}_repeat{N}`, hidden variables
//! that are pure repetitions became binary trees in place, and the token/rule
//! split happened. Precedence also changed shape: it is carried on individual
//! production *steps* rather than on symbols, which is why a conflict can be
//! reported "in" a rule whose productions look harmless in the source.
//!
//! When `generate` reports
//!
//! ```text
//! SELECT '(' a_expr •  ','
//!   `expr_list`
//! ```
//!
//! this dump is how to see the productions behind `expr_list`, aux rules and
//! all. It deliberately invents no names: every identifier printed is one the
//! generator itself would use.

use std::fmt::Write as _;

use crate::{
    Diagnostic, GenerateError, GenerateResult,
    PreparedGrammar,
    grammars::{InlinedProductionMap, LexicalGrammar, ProductionStep, SyntaxGrammar, VariableType},
    parse_grammar::parse_grammar,
    prepare_grammar::prepare_grammar,
    rules::{Associativity, Precedence, Symbol, SymbolView},
    strpool::{StrId, StrPool},
};

/// Write the expanded (sugar-free, token-split) grammar as BNF.
///
/// The output is meant to be read and diffed: one production per line, so
/// `grep '^expr_list:' expanded.bnf` shows exactly the alternatives that the
/// table builder sees.
pub fn write_expanded_grammar(
    grammar_json: &str,
    out: &mut dyn std::io::Write,
    diagnostics: &mut Vec<Diagnostic>,
) -> GenerateResult<()> {
    let input_grammar = parse_grammar(grammar_json, diagnostics)?;
    let grammar_name = input_grammar.pool.resolve(input_grammar.name).to_string();
    let prepared = prepare_grammar(input_grammar, diagnostics)?;
    let text = format_expanded_grammar(&grammar_name, &prepared);
    out.write_all(text.as_bytes()).map_err(|error| {
        GenerateError::IO(crate::IoError {
            error,
            path: None,
        })
    })
}

fn format_expanded_grammar(grammar_name: &str, prepared: &PreparedGrammar) -> String {
    let PreparedGrammar {
        syntax_grammar,
        lexical_grammar,
        inlines,
        str_pool,
        ..
    } = prepared;
    let mut out = String::new();
    let mut counts = Counts::default();
    for variable in &syntax_grammar.variables {
        counts.add(variable.kind);
    }

    let _ = writeln!(out, "# Expanded grammar `{grammar_name}` — after EBNF lowering,");
    let _ = writeln!(
        out,
        "# before the token/rule split is collapsed into a parse table."
    );
    let _ = writeln!(
        out,
        "# {} variables: {} named, {} anonymous, {} hidden, {} auxiliary (`*_repeatN`)",
        syntax_grammar.variables.len(),
        counts.named,
        counts.anonymous,
        counts.hidden,
        counts.auxiliary,
    );
    let let_prods = syntax_grammar.productions.len();
    let _ = writeln!(out, "# {let_prods} productions in total");

    write_header(&mut out, syntax_grammar, str_pool);
    write_tokens(&mut out, lexical_grammar, str_pool);
    write_variables(&mut out, syntax_grammar, lexical_grammar, str_pool, inlines);
    out
}

#[derive(Default)]
struct Counts {
    named: usize,
    anonymous: usize,
    hidden: usize,
    auxiliary: usize,
}

impl Counts {
    fn add(&mut self, kind: VariableType) {
        match kind {
            VariableType::Named => self.named += 1,
            VariableType::Anonymous => self.anonymous += 1,
            VariableType::Hidden => self.hidden += 1,
            VariableType::Auxiliary => self.auxiliary += 1,
        }
    }
}

fn write_header(out: &mut String, grammar: &SyntaxGrammar, pool: &StrPool) {
    let extras: Vec<String> = grammar
        .extra_symbols
        .iter()
        .map(|s| symbol_name(*s, grammar, None, pool))
        .collect();
    if !extras.is_empty() {
        let _ = writeln!(out, "%extra {}", extras.join(" "));
    }
    if let Some(word) = grammar.word_token {
        let _ = writeln!(out, "%word {}", symbol_name(word, grammar, None, pool));
    }
    if !grammar.variables_to_inline.is_empty() {
        let names: Vec<String> = grammar
            .variables_to_inline
            .iter()
            .map(|s| symbol_name(*s, grammar, None, pool))
            .collect();
        let _ = writeln!(out, "%inline {}", names.join(" "));
    }
    if !grammar.external_tokens.is_empty() {
        let names: Vec<&str> = grammar
            .external_tokens
            .iter()
            .map(|t| pool.resolve(t.name))
            .collect();
        let _ = writeln!(out, "%external {}", names.join(" "));
    }
    for conflict in &grammar.expected_conflicts {
        let names: Vec<String> = conflict
            .iter()
            .map(|s| symbol_name(*s, grammar, None, pool))
            .collect();
        let _ = writeln!(out, "%conflicts {}", names.join(" "));
    }
    for ordering in &grammar.precedence_orderings {
        let names: Vec<String> = ordering
            .iter()
            .map(|entry| match entry {
                crate::grammars::PrecedenceEntry::Name(id) => pool.resolve(*id).to_string(),
                crate::grammars::PrecedenceEntry::Symbol(id) => {
                    format!("'{}'", pool.resolve(*id))
                }
            })
            .collect();
        let _ = writeln!(out, "%precedence-order {}", names.join(" > "));
    }
}

fn write_tokens(out: &mut String, lexical: &LexicalGrammar, pool: &StrPool) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "# {} tokens, in the order the lexer tries them at equal length",
        lexical.variables.len()
    );
    let mut tokens: Vec<(usize, String, i32)> = lexical
        .variables
        .iter()
        .enumerate()
        .map(|(index, variable)| (index, pool.resolve(variable.name).to_string(), variable.implicit_precedence))
        .collect();
    tokens.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    for (index, name, precedence) in tokens {
        let mut line = format!("token {name}");
        if lexical.variables[index].kind == VariableType::Anonymous {
            line.push_str(" (anonymous)");
        }
        let _ = write!(out, "{line}");
        if lexical.variables[index].implicit_precedence != 0 {
            let _ = write!(out, " prec={precedence}");
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "# `token` names come from the lexical grammar; two tokens can share one pattern,"
    );
    let _ = writeln!(
        out,
        "# which is why the parse table has to choose between them per state."
    );
}

fn write_variables(
    out: &mut String,
    syntax: &SyntaxGrammar,
    lexical: &LexicalGrammar,
    pool: &StrPool,
    inlines: &InlinedProductionMap,
) {
    let _ = writeln!(out);
    for (index, variable) in syntax.variables.iter().enumerate() {
        let name = pool.resolve(variable.name);
        let mark = match variable.kind {
            VariableType::Named => "",
            VariableType::Anonymous => "  # anonymous",
            VariableType::Hidden => "  # hidden",
            VariableType::Auxiliary => "  # auxiliary rule, invented by EBNF lowering",
        };
        let ids: Vec<u32> = syntax.variable_prod_ids(index).collect();
        if ids.is_empty() {
            let _ = writeln!(out, "{name}: %empty ;{mark}");
            continue;
        }
        for production_id in ids {
            let production = syntax.production(production_id);
            let mut line = format!("{name}:");
            if production.steps.is_empty() {
                line.push_str(" %empty");
            }
            for step in production.steps {
                line.push(' ');
                line.push_str(&step_name(*step, syntax, lexical, pool));
            }
            if production.dynamic_precedence != 0 {
                let _ = write!(line, " %dprec({})", production.dynamic_precedence);
            }
            if production.requires_eof_lookahead {
                line.push_str(" %require-eof");
            }
            let inlined = inlines
                .fully_inlined_prod_ids(production_id)
                .is_some()
                .then_some("  # inlined")
                .unwrap_or("");
            let _ = writeln!(out, "{line} ;{mark}{inlined}");
        }
    }
}

fn step_name(
    step: ProductionStep,
    syntax: &SyntaxGrammar,
    lexical: &LexicalGrammar,
    pool: &StrPool,
) -> String {
    let mut out = String::new();
    if let Some(field) = step.field() {
        let _ = write!(out, "{}:", pool.resolve(field));
    }
    out.push_str(&symbol_name(step.symbol(), syntax, Some(lexical), pool));
    if let Some(alias) = step.alias() {
        let _ = write!(out, "@{}", pool.resolve(alias.value));
    }
    let precedence = step.precedence();
    let associativity = step.associativity();
    if precedence != Precedence::None || associativity.is_some() {
        let precedence = match precedence {
            Precedence::None => String::new(),
            Precedence::Integer(value) => value.to_string(),
            Precedence::Name(id) => pool.resolve(id).to_string(),
        };
        let associativity = match associativity {
            None => "",
            Some(Associativity::Left) => " left",
            Some(Associativity::Right) => " right",
        };
        let _ = write!(out, " %prec({precedence}{associativity})");
    }
    if step.reserved != 0 && step.reserved != ProductionStep::NO_RESERVED_WORDS {
        let _ = write!(out, "%reserved({})", step.reserved);
    }
    out
}

fn symbol_name(
    symbol: Symbol,
    syntax: &SyntaxGrammar,
    lexical: Option<&LexicalGrammar>,
    pool: &StrPool,
) -> String {
    match symbol.view() {
        SymbolView::Terminal(index) => {
            let index = usize::from(index);
            match lexical.and_then(|lexical| lexical.variables.get(index)) {
                Some(variable) => {
                    let name = pool.resolve(variable.name);
                    if variable.kind == VariableType::Anonymous && needs_quotes(name) {
                        format!("'{name}'")
                    } else {
                        name.to_string()
                    }
                }
                None => format!("terminal-{index}"),
            }
        }
        SymbolView::External(index) => {
            let name: StrId = syntax.external_tokens[usize::from(index)].name;
            pool.resolve(name).to_string()
        }
        SymbolView::NonTerminal(index) => {
            pool.resolve(syntax.variables[usize::from(index)].name).to_string()
        }
        SymbolView::End => "$".to_string(),
        SymbolView::EndOfNonTerminalExtra => "<END_OF_NONTERMINAL_EXTRA>".to_string(),
    }
}

fn needs_quotes(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

#[cfg(test)]
mod tests {
    use super::write_expanded_grammar;

    fn dump(grammar_json: &str) -> String {
        let mut diagnostics = Vec::new();
        let mut out = Vec::new();
        write_expanded_grammar(grammar_json, &mut out, &mut diagnostics).unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        String::from_utf8(out).unwrap()
    }

    /// `repeat` disappears into an auxiliary rule named after its parent, so
    /// `grep '^a:'` cannot see the repetition at all: it lives in `a_repeat1`.
    #[test]
    fn repeat_becomes_a_named_auxiliary_rule() {
        let out = dump(
            r#"{"name":"t","rules":{
                "source_file":{"type":"SYMBOL","name":"a"},
                "a":{"type":"REPEAT","content":{"type":"STRING","value":"x"}}
            },"extras":[],"conflicts":[],"precedences":[],"externals":[],"inline":[],"supertypes":[]}"#,
        );
        assert!(
            out.contains("# 3 variables: 2 named, 0 anonymous, 0 hidden, 1 auxiliary"),
            "{out}"
        );
        assert!(out.contains("a: a_repeat1 ;"), "{out}");
        assert!(out.contains("a: %empty ;"), "{out}");
        assert!(
            out.contains("a_repeat1: a_repeat1 a_repeat1 ;  # auxiliary rule"),
            "{out}"
        );
        assert!(out.contains("a_repeat1: x ;"), "{out}");
    }

    /// Precedence is per production step, not per symbol, and that is where the
    /// difference from a yacc grammar shows: `joined_table: a b` carries the
    /// level on `b` even though a yacc rule would inherit it from `b` implicitly.
    #[test]
    fn precedence_is_shown_on_the_steps_that_carry_it() {
        let out = dump(
            r#"{"name":"t","rules":{
                "source_file":{"type":"SYMBOL","name":"a"},
                "a":{"type":"PREC_LEFT","value":3,"content":{"type":"SEQ","members":[
                    {"type":"STRING","value":"x"},
                    {"type":"FIELD","name":"rhs","content":{"type":"STRING","value":"y"}}
                ]}}
            },"extras":[],"conflicts":[],"precedences":[],"externals":[],"inline":[],"supertypes":[]}"#,
        );
        // The parent `prec_left` is pushed down onto every step, and the field
        // label is printed where it applies.
        assert!(out.contains("a: x %prec(3 left) rhs:y %prec(3 left) ;"), "{out}");
        assert_eq!(out.matches("%prec(3 left)").count(), 2, "{out}");
    }

    /// A conflict the table builder reports names a variable; the dump is how to
    /// see the production behind the name, including the declared ones.
    #[test]
    fn declared_conflicts_are_listed_with_their_symbols() {
        let out = dump(
            r#"{"name":"t","rules":{
                "source_file":{"type":"CHOICE","members":[
                    {"type":"SYMBOL","name":"a"},{"type":"SYMBOL","name":"b"}]},
                "a":{"type":"STRING","value":"x"},
                "b":{"type":"STRING","value":"x"}
            },"extras":[],"conflicts":[["a","b"]],"precedences":[],"externals":[],"inline":[],"supertypes":[]}"#,
        );
        assert!(out.contains("%conflicts a b"), "{out}");
    }
}
