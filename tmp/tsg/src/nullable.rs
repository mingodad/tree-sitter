//! Which rules can match the empty string.
//!
//! `tree-sitter` rejects such a rule unless it is the grammar's start rule —
//! "Tree-sitter does not support syntactic rules that match the empty string
//! unless they are used only as the grammar's start rule" — and the fix is
//! always at the *use* sites (`block` → `block?`). By the time the parser
//! generator can say that, `grammar.json` is all it has: no rule spans, no
//! source. So the check runs here, where both are available, and points at the
//! alternative that makes the rule empty.

use std::collections::{HashMap, HashSet};

use crate::lexer::Span;
use crate::parser::{Expr, ExprKind, GrammarAst};

/// Rules that can derive nothing, and for each the alternative responsible.
pub struct Nullable {
    pub rules: HashMap<String, Span>,
    /// Every rule name referenced from some rule body, with the reference spans.
    pub references: HashMap<String, Vec<Span>>,
}

/// Smallest fixpoint: iterate until nothing new becomes nullable. Only `true`
/// results are recorded, so an unrecognised construct is treated as non-empty
/// and never produces a false diagnostic.
#[must_use]
pub fn analyse(ast: &GrammarAst) -> Nullable {
    let mut nullable: HashMap<String, Span> = HashMap::new();
    loop {
        let known: HashSet<String> = nullable.keys().cloned().collect();
        let mut added: Vec<(String, Span)> = Vec::new();
        for (name, body) in &ast.rules {
            if known.contains(name) || added.iter().any(|(seen, _)| seen == name) {
                continue;
            }
            if let Some(span) = empty_production(body, &known) {
                added.push((name.clone(), span));
            }
        }
        if added.is_empty() {
            break;
        }
        for (name, span) in added {
            nullable.insert(name, span);
        }
    }

    let mut references: HashMap<String, Vec<Span>> = HashMap::new();
    for (_, body) in &ast.rules {
        collect_references(body, &mut references);
    }
    Nullable {
        rules: nullable,
        references,
    }
}

/// The span of the first top-level alternative that produces no tokens.
fn empty_production(body: &Expr, nullable: &HashSet<String>) -> Option<Span> {
    match &body.kind {
        ExprKind::Choice(items) => items
            .iter()
            .find(|item| is_empty(item, nullable))
            .map(|item| item.span),
        _ => is_empty(body, nullable).then_some(body.span),
    }
}

fn is_empty(expr: &Expr, nullable: &HashSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => false,
        ExprKind::Symbol(name) => nullable.contains(name),
        ExprKind::Seq(items) => items.iter().all(|item| is_empty(item, nullable)),
        ExprKind::Choice(items) => items.iter().any(|item| is_empty(item, nullable)),
        ExprKind::Optional(_) | ExprKind::Repeat(_) => true,
        ExprKind::Repeat1(inner) => is_empty(inner, nullable),
        ExprKind::Call { name, args } => match name.as_str() {
            // `eof()` is zero-width but it is not "matches the empty string":
            // it pins the production to the end of input, which is exactly the
            // condition that makes such a production legal.
            "eof" => false,
            "blank" => true,
            "optional" | "repeat" => true,
            "repeat1" | "token" | "token.immediate" => {
                args.first().is_some_and(|arg| is_empty(arg, nullable))
            }
            "prec" | "prec.left" | "prec.right" | "prec.dynamic" | "field" | "reserved" => {
                args.get(1).is_some_and(|arg| is_empty(arg, nullable))
            }
            "alias" => args.first().is_some_and(|arg| is_empty(arg, nullable)),
            "seq" => args.iter().all(|arg| is_empty(arg, nullable)),
            "choice" => args.iter().any(|arg| is_empty(arg, nullable)),
            // `sym` and an empty argument list mean something special to the
            // generator; stay silent rather than risk a wrong message.
            _ => false,
        },
    }
}

fn collect_references(expr: &Expr, out: &mut HashMap<String, Vec<Span>>) {
    match &expr.kind {
        ExprKind::Symbol(name) => out.entry(name.clone()).or_default().push(expr.span),
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                collect_references(item, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            collect_references(inner, out);
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                collect_references(arg, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => {}
    }
}
