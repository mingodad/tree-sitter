//! Lexical analysis of a grammar: which rules are tokens, in what order, and
//! what inside a token is not lexical.
//!
//! tree-sitter has no token section of its own: a rule is a token when its body
//! is built only from strings and patterns, which the generator decides by
//! analysis. A `tokens { … }` block lets the author *say* it, and this module is
//! where the claim is checked against the body — so a rule that is not lexical
//! is reported at the offending element instead of as a position-free
//! `UnexpectedRule` after the JSON exists.
//!
//! The other half is priority. Equal-length matches are resolved by declaration
//! order — measured, not guessed: `a: /ab/; b: /a./;` lexes `ab` as `a`, and
//! swapping the two lines lexes it as `b`. So [`Lexing::order`] is the file's
//! rule order, and `--explain-lexing` prints it as the priority list rather than
//! leaving it implicit.
//!
//! Per-parse-state lexer *states* are a different thing and are not computed
//! here: the generator derives one DFA start state per distinct set of valid
//! tokens (`ts_lex_modes[state].lex_state`), which is why a grammar this size
//! needs two of them and no user-declared modes at all.

use std::collections::HashSet;

use crate::{
    lexer::Span,
    parser::{Expr, ExprKind, GrammarAst},
};

/// One rule, in declaration order.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// The rule's name.
    pub span: Span,
    /// The rule's body.
    pub body: Span,
    /// Declared in a `tokens { … }` block.
    pub declared: bool,
    /// The body carries `@immediate`.
    pub immediate: bool,
}

/// What the lexer will be asked to do.
#[derive(Debug, Default)]
pub struct Lexing {
    /// Every rule whose body is lexical, and so is a single token.
    pub tokens: HashSet<String>,
    /// Every rule, in declaration order — which is token priority order.
    pub order: Vec<Entry>,
}

impl Lexing {
    pub fn is_token(&self, name: &str) -> bool {
        self.tokens.contains(name)
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.order.iter().find(|entry| entry.name == name)
    }
}

/// Whether a rule body is *one* token, rather than a sequence or choice of
/// tokens.
///
/// `identifier: /[a-z]+/;` is one token. `pair: 'a' 'b';` is not: it is a
/// nonterminal that matches two anonymous tokens, even though both are lexical.
/// The `tokens` block and `token(…)` make the difference explicit by wrapping
/// the body, which is why the parser desugars the block to a `token(…)` call.
fn token_shape(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } => true,
        ExprKind::Call { name, .. } => matches!(name.as_str(), "token" | "token.immediate"),
        _ => false,
    }
}

/// Read `grammar` and return the token set plus the priority order.
pub fn analyse(grammar: &GrammarAst) -> Lexing {
    let mut lexing = Lexing::default();
    // The start rule is the first rule *not* declared in `tokens`: a block may
    // sit before the start rule, and `compile` moves that rule to the front of
    // `grammar.json`. Resolve it here too, so `--explain-modes` can read the
    // source AST without duplicating the reordering.
    let start = grammar
        .rules
        .iter()
        .position(|(name, _)| !grammar.tokens.iter().any(|rule| rule.name == *name))
        .unwrap_or(0);
    for (index, (name, body)) in grammar.rules.iter().enumerate() {
        let declared = grammar.tokens.iter().find(|rule| rule.name == *name);
        // The start rule's body may be lexical, but the rule itself is the
        // entry point, not a token: `file: /x/;` is a nonterminal that happens
        // to match one anonymous token. The generator says so too (`%word file`
        // is rejected as a non-terminal word token).
        if index != start && non_lexical(body).is_none() && token_shape(body) {
            lexing.tokens.insert(name.clone());
        }
        lexing.order.push(Entry {
            name: name.clone(),
            span: declared.map_or(body.span, |rule| rule.span),
            body: body.span,
            declared: declared.is_some(),
            immediate: declared.is_some_and(|rule| rule.immediate),
        });
    }
    // Externals are tokens by declaration, even though their body lives in C.
    for external in &grammar.externals {
        if let ExprKind::Symbol(name) = &external.kind {
            lexing.tokens.insert(name.clone());
        }
    }
    lexing
}

/// The first element of `expr` that makes it non-lexical, with a phrase naming
/// it: `Some((span, "the rule `expr`"))`.
///
/// Only two things disqualify a body, because that is all the generator
/// objects to: a reference to another rule (tokens are built from strings and
/// patterns, never from other tokens — `UnexpectedRule` in `parse_grammar.rs`)
/// and the empty production. Repetition, `prec(…)`, `alias(…)` and `field(…)`
/// are all fine inside a token; the DFA has loops.
pub fn non_lexical(expr: &Expr) -> Option<(Span, String)> {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } => None,
        ExprKind::Symbol(name) => Some((expr.span, format!("the rule `{name}`"))),
        ExprKind::Seq(items) | ExprKind::Choice(items) => items.iter().find_map(non_lexical),
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            non_lexical(inner)
        }
        ExprKind::Number(_) => Some((expr.span, "a bare number".to_owned())),
        ExprKind::Call { name, args } => match name.as_str() {
            "blank" => Some((expr.span, "`blank()`".to_owned())),
            "field" => args.get(1).and_then(non_lexical),
            _ => args.iter().find_map(non_lexical),
        },
    }
}

/// The first place where an `%extra` rule starts with something that is not a
/// token, with a phrase naming it: `Some((span, "the rule `x`"))`.
///
/// The generator accepts a non-terminal extra as long as each of its
/// productions starts with a terminal — `comment: _line_comment |
/// _block_comment;` is fine, `x: y ...;` with a non-terminal `y` is not. So the
/// top-level extra symbol is expanded to its body, but a symbol found *inside*
/// that body is not expanded again.
pub fn extra_problem(expr: &Expr, grammar: &GrammarAst, lexing: &Lexing) -> Option<(Span, String)> {
    let body = match &expr.kind {
        ExprKind::Symbol(name) => {
            if lexing.is_token(name) {
                return None;
            }
            grammar
                .rules
                .iter()
                .find_map(|(candidate, body)| (candidate == name).then_some(body))
                .unwrap_or(expr)
        }
        _ => expr,
    };
    first_problem(body, lexing)
}

fn first_problem(expr: &Expr, lexing: &Lexing) -> Option<(Span, String)> {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } => None,
        ExprKind::Symbol(name) => {
            (!lexing.is_token(name)).then(|| (expr.span, format!("the rule `{name}`")))
        }
        ExprKind::Seq(items) => items.first().and_then(|item| first_problem(item, lexing)),
        ExprKind::Choice(items) => items.iter().find_map(|item| first_problem(item, lexing)),
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            first_problem(inner, lexing)
        }
        ExprKind::Number(_) => Some((expr.span, "a bare number".to_owned())),
        ExprKind::Call { name, args } => match name.as_str() {
            "blank" => Some((expr.span, "`blank()`".to_owned())),
            "seq" => args.first().and_then(|arg| first_problem(arg, lexing)),
            "choice" => args.iter().find_map(|arg| first_problem(arg, lexing)),
            "optional" | "repeat" | "repeat1" => {
                args.first().and_then(|arg| first_problem(arg, lexing))
            }
            "token" | "token.immediate" | "alias" => {
                args.first().and_then(|arg| first_problem(arg, lexing))
            }
            "field" | "prec" | "prec.left" | "prec.right" | "prec.dynamic" | "reserved" => {
                args.get(1).and_then(|arg| first_problem(arg, lexing))
            }
            // `sym(name)` is already resolved by the parser/lowerer; an
            // unresolved one is not a token we can vouch for.
            "sym" => Some((expr.span, "`sym(…)`".to_owned())),
            _ => Some((expr.span, format!("`{name}(…)`"))),
        },
    }
}

/// A span where `expr` can match nothing, if it is statically obvious.
///
/// This is deliberately small: it catches empty strings, empty patterns,
/// `blank()`, and the constructs that make emptiness (`optional`, `repeat`,
/// `?`, `*`) so `tokens { X: ''; }` and `%extra x; x: 'a'?;` get a caret at the
/// source. It does not try to decide whether an arbitrary regex can match the
/// empty string.
#[must_use]
pub fn empty_span(expr: &Expr) -> Option<Span> {
    match &expr.kind {
        ExprKind::Str(value) if value.is_empty() => Some(expr.span),
        ExprKind::Pattern { value, .. } if value.is_empty() => Some(expr.span),
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => None,
        ExprKind::Symbol(_) => None,
        ExprKind::Optional(_) | ExprKind::Repeat(_) => Some(expr.span),
        ExprKind::Repeat1(inner) => empty_span(inner),
        ExprKind::Seq(items) => {
            if items.iter().all(|item| empty_span(item).is_some()) {
                items.first().map(|item| item.span).or(Some(expr.span))
            } else {
                None
            }
        }
        ExprKind::Choice(items) => items.iter().find_map(empty_span),
        ExprKind::Call { name, args } => match name.as_str() {
            "blank" => Some(expr.span),
            "optional" | "repeat" => Some(expr.span),
            "repeat1" => args.first().and_then(empty_span),
            "seq" => {
                if args.iter().all(|arg| empty_span(arg).is_some()) {
                    args.first().and_then(empty_span).or(Some(expr.span))
                } else {
                    None
                }
            }
            "choice" => args.iter().find_map(empty_span),
            "token" | "token.immediate" | "alias" => args.first().and_then(empty_span),
            "field" | "prec" | "prec.left" | "prec.right" | "prec.dynamic" | "reserved" => {
                args.get(1).and_then(empty_span)
            }
            // `eof()` is zero-width by design; the other calls are opaque.
            _ => None,
        },
    }
}

/// Resolve an extra's top-level symbol to its rule body, so that `%extra
/// comment;` is checked as `choice(_line_comment, _block_comment)`.
pub fn extra_body<'a>(expr: &'a Expr, grammar: &'a GrammarAst) -> &'a Expr {
    if let ExprKind::Symbol(name) = &expr.kind {
        if let Some((_, body)) = grammar
            .rules
            .iter()
            .find(|(candidate, _)| candidate == name)
        {
            return body;
        }
    }
    expr
}

/// One line of source for a span, whitespace collapsed, `…` when long.
fn snippet(src: &str, span: Span) -> String {
    let text = src.get(span.start..span.end).unwrap_or("").trim();
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 56 {
        let head: String = collapsed.chars().take(53).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

/// The `--explain-lexing` / `--explain-modes` report: the token priority list,
/// then what the order means.
pub fn report(src: &str, grammar: &GrammarAst, lexing: &Lexing) -> String {
    let mut shown: Vec<&Entry> = lexing
        .order
        .iter()
        .filter(|e| lexing.is_token(&e.name))
        .collect();
    shown.sort_by_key(|entry| {
        lexing
            .order
            .iter()
            .position(|candidate| candidate.name == entry.name)
            .unwrap_or(usize::MAX)
    });

    let markers_for =
        |entry: &Entry| {
            let mut markers: Vec<&str> = Vec::new();
            if grammar
                .word
                .as_ref()
                .is_some_and(|(word, _)| *word == entry.name)
            {
                markers.push("word");
            }
            if entry.immediate {
                markers.push("immediate");
            }
            if grammar
                .extras
                .iter()
                .any(|extra| matches!(&extra.kind, ExprKind::Symbol(name) if *name == entry.name))
            {
                markers.push("extra");
            }
            if grammar.externals.iter().any(
                |external| matches!(&external.kind, ExprKind::Symbol(name) if *name == entry.name),
            ) {
                markers.push("external");
            }
            if markers.is_empty() {
                "—".to_owned()
            } else {
                markers.join(" ")
            }
        };

    let mut out = String::new();
    out.push_str("lexer modes: token priority (equal-length matches: the earlier rule wins)\n");
    let width = shown
        .iter()
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    for (index, entry) in shown.iter().enumerate() {
        out.push_str(&format!(
            "  {:>2}  {:<width$}  {:<12}  {}\n",
            index + 1,
            entry.name,
            markers_for(entry),
            snippet(src, entry.body),
            width = width
        ));
    }

    let mut nonterminals: Vec<&str> = lexing
        .order
        .iter()
        .filter(|entry| !lexing.is_token(&entry.name))
        .map(|entry| entry.name.as_str())
        .collect();
    nonterminals.sort_unstable();
    out.push_str(&format!(
        "\n{} token{}, {} nonterminal{}; {} declared in `tokens`\n",
        shown.len(),
        if shown.len() == 1 { "" } else { "s" },
        nonterminals.len(),
        if nonterminals.len() == 1 { "" } else { "s" },
        grammar.tokens.len()
    ));
    out.push_str(
        "The order above is the grammar's rule order. Per-parse-state lexer states\n\
         (`ts_lex_modes[state].lex_state`) are derived by the generator from the set\n\
         of valid tokens, so they are not written here — and not written at all.\n\
         This report names the lexical inputs those states are built from.\n",
    );
    out
}

/// Parse `src` and produce the lexical report. Kept separate from [`report`] so
/// tests can inspect a hand-built [`GrammarAst`].
pub fn describe(src: &str) -> crate::lexer::Res<String> {
    let grammar = crate::parser::Parser::new(src)?.parse()?;
    let lexing = analyse(&grammar);
    Ok(report(src, &grammar, &lexing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn lexing(src: &str) -> (GrammarAst, Lexing) {
        let grammar = Parser::new(src).expect("lexes").parse().expect("parses");
        let lexing = analyse(&grammar);
        (grammar, lexing)
    }

    #[test]
    fn a_pattern_rule_is_a_token_without_being_declared() {
        let (_, lexing) = lexing("%name t;\nr: a 'y';\na: /x/;");
        assert!(lexing.is_token("a"));
        assert!(!lexing.is_token("r"));
    }

    #[test]
    fn a_choice_needs_token_to_become_one_token() {
        let (_, bare) = lexing("%name t;\nr: x z;\nx: 'a' | 'b';\ny: 'c';\nz: y;");
        assert!(!bare.is_token("x"), "a bare choice is still a nonterminal");
        assert!(!bare.is_token("z"), "`z: y;` is a nonterminal");

        let (_, wrapped) = lexing("%name t;\nr: x z;\nx: token('a' | 'b');\ny: 'c';\nz: y;");
        assert!(wrapped.is_token("x"), "`token(…)` is one token");
    }

    #[test]
    fn a_tokens_block_rule_is_one_token_and_keeps_its_position() {
        let (grammar, lexing) = lexing("%name t;\na: 'a';\ntokens { B: 'b'; }\nr: a B;");
        assert_eq!(grammar.tokens.len(), 1);
        assert!(lexing.is_token("B"));
        let order: Vec<&str> = lexing.order.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, vec!["a", "B", "r"], "file order is preserved");
        let entry = lexing.entry("B").expect("B");
        assert!(entry.declared);
    }

    #[test]
    fn a_tokens_block_before_the_start_rule_is_still_all_tokens() {
        let (_, lexing) = lexing("%name t;\ntokens { B: 'b'; }\nr: B;");
        assert!(
            lexing.is_token("B"),
            "a leading block is not the start rule"
        );
        assert!(!lexing.is_token("r"));
        let order: Vec<&str> = lexing.order.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(order, vec!["B", "r"], "the report keeps file order");
    }

    #[test]
    fn a_rule_reference_inside_a_token_is_named() {
        let (_, lexing) = lexing("%name t;\ntokens { FUSED: a 'x'; }\na: /a/;\nr: FUSED;");
        assert!(!lexing.is_token("FUSED"));
    }

    #[test]
    fn the_report_lists_tokens_in_priority_order() {
        let src = "%name t;\nr: a B;\na: 'a';\ntokens {\n  B: /b/ @immediate;\n}\n%extra B;";
        let (grammar, lexing) = lexing(src);
        let text = report(src, &grammar, &lexing);
        assert!(text.contains("token priority"), "{text}");
        assert!(text.contains("immediate extra"), "{text}");
        let a = text.find(" a ").expect("a is listed");
        let b = text.find(" B ").expect("B is listed");
        assert!(a < b, "priority order is declaration order:\n{text}");
    }

    #[test]
    fn the_start_rule_is_not_a_token_even_when_its_body_is_lexical() {
        let (_, lexing) = lexing("%name t;\nfile: /x/;\nr: file;");
        assert!(
            !lexing.is_token("file"),
            "`file` is the entry point, not a token: `%word file` is invalid"
        );
    }

    #[test]
    fn a_nonterminal_extra_is_fine_when_its_productions_start_with_tokens() {
        let (grammar, lexing) = lexing(
            "%name t;\nr: comment;\ncomment: line | block;\nline: /\\/\\/[^\\n]*/;\nblock: /\\/\\*[^*]*\\*\\//;\n%extra comment;",
        );
        assert!(extra_problem(&grammar.extras[0], &grammar, &lexing).is_none());
    }

    #[test]
    fn a_nonterminal_extra_that_starts_with_a_nonterminal_is_named() {
        let (grammar, lexing) = lexing("%name t;\nr: x;\n%extra x;\nx: y 'b';\ny: z;\nz: 'a';");
        let (_, what) = extra_problem(&grammar.extras[0], &grammar, &lexing)
            .expect("the first symbol of `x` is `y`, which is a nonterminal");
        assert!(what.contains("`y`"), "{what}");
    }

    #[test]
    fn obvious_empty_token_bodies_are_found() {
        let (grammar, _) = lexing("%name t;\nr: X;\ntokens { X: ''; }");
        let body = grammar
            .rules
            .iter()
            .find_map(|(name, body)| (name == "X").then_some(body))
            .expect("X");
        assert!(empty_span(body).is_some());
    }
}
