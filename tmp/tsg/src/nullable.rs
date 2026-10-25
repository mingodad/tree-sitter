//! Which rules can match the empty string, and which of them do so in a circle.
//!
//! A rule that matches nothing is a reduction with no children, which the parse
//! table handles: the start rule has always been able to do it, and every rule
//! can. What no table can handle is a *circle* of empty rules — `a: b?; b: ();`
//! reduces `b`, then `a`, then asks for `b` again at the same position, forever.
//! That is the one thing refused here.
//!
//! By the time the parser generator can complain, `grammar.json` is all it has:
//! no rule spans, no source, and (for a cycle) no names either. So the check
//! runs here, where both are available, and points at the reference that closes
//! the loop.

use std::collections::{HashMap, HashSet};

use crate::{
    lexer::Span,
    parser::{Expr, ExprKind, GrammarAst},
};

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

/// A rule that reaches itself without consuming input.
pub struct Cycle {
    /// Every rule on the way round, sorted, without repeats.
    pub rules: Vec<String>,
    /// The reference that closes the loop.
    pub span: Span,
}

/// Find the circles in which every step can match nothing.
///
/// The graph has an edge `a -> b` when `a` can hand over to `b` without
/// consuming a token: `b` appears in one of `a`'s alternatives and everything
/// around it can match nothing too. A rule that can reach itself that way is the
/// runaway case — `a: b?; b: ();` is `a -> b -> a`. Repeating something that can
/// match nothing (`a: b*; b: ();`) is the same edge back to the same rule,
/// because the helper rule the generator builds for `*` hands over to itself.
///
/// The answer is a *list* of cycles rather than a boolean so the message can
/// name the rules and point at one reference that closes the loop.
#[must_use]
pub fn epsilon_cycles(ast: &GrammarAst, nullable: &Nullable) -> Vec<Cycle> {
    let empty: HashSet<String> = nullable.rules.keys().cloned().collect();
    let mut names: Vec<String> = empty.iter().cloned().collect();
    names.sort_unstable();
    let indices: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let bodies: HashMap<&str, &Expr> = ast
        .rules
        .iter()
        .map(|(name, body)| (name.as_str(), body))
        .collect();
    let mut edges: Vec<Vec<(usize, Span)>> = vec![Vec::new(); names.len()];
    for (from, name) in names.iter().enumerate() {
        let Some(body) = bodies.get(name.as_str()) else {
            continue;
        };
        let mut targets = Vec::new();
        handovers(name, body, &empty, &mut targets);
        for (target, span) in targets {
            if let Some(&to) = indices.get(target.as_str()) {
                edges[from].push((to, span));
            }
        }
    }

    // Kosaraju's algorithm finds every strongly connected component in O(V+E).
    // A component is an epsilon cycle iff it has multiple vertices or a self-edge.
    let mut reverse = vec![Vec::new(); names.len()];
    for (from, targets) in edges.iter().enumerate() {
        for &(to, _) in targets {
            reverse[to].push(from);
        }
    }
    let mut visited = vec![false; names.len()];
    let mut finished = Vec::with_capacity(names.len());
    for root in 0..names.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut stack = vec![(root, 0)];
        while let Some((node, next)) = stack.last_mut() {
            if let Some(&(to, _)) = edges[*node].get(*next) {
                *next += 1;
                if !visited[to] {
                    visited[to] = true;
                    stack.push((to, 0));
                }
            } else {
                finished.push(*node);
                stack.pop();
            }
        }
    }
    let mut component_of = vec![usize::MAX; names.len()];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for &root in finished.iter().rev() {
        if component_of[root] != usize::MAX {
            continue;
        }
        let component = components.len();
        let mut members = Vec::new();
        let mut stack = vec![root];
        component_of[root] = component;
        while let Some(node) = stack.pop() {
            members.push(node);
            for &to in &reverse[node] {
                if component_of[to] == usize::MAX {
                    component_of[to] = component;
                    stack.push(to);
                }
            }
        }
        components.push(members);
    }

    let mut cycles = Vec::new();
    for mut members in components {
        let component = component_of[members[0]];
        let is_cycle =
            members.len() > 1 || edges[members[0]].iter().any(|(to, _)| *to == members[0]);
        if !is_cycle {
            continue;
        }
        members.sort_unstable();
        let span = members
            .iter()
            .flat_map(|&from| edges[from].iter())
            .find(|(to, _)| component_of[*to] == component)
            .map(|(_, span)| *span)
            .expect("a cyclic component has an internal edge");
        cycles.push(Cycle {
            rules: members
                .into_iter()
                .map(|index| names[index].clone())
                .collect(),
            span,
        });
    }
    cycles
}

/// Rules that `expr` can hand over to without consuming input, with the
/// reference span of each.
fn handovers(rule: &str, expr: &Expr, empty: &HashSet<String>, out: &mut Vec<(String, Span)>) {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => {}
        ExprKind::Symbol(name) => {
            if empty.contains(name) {
                out.push((name.clone(), expr.span));
            }
        }
        ExprKind::Seq(items) => {
            for (i, item) in items.iter().enumerate() {
                let rest_is_empty = items
                    .iter()
                    .enumerate()
                    .all(|(j, other)| j == i || is_empty(other, empty));
                if rest_is_empty {
                    handovers(rule, item, empty, out);
                }
            }
        }
        // A choice picks one alternative, so every alternative is a way through.
        ExprKind::Choice(items) => {
            for item in items {
                handovers(rule, item, empty, out);
            }
        }
        // `X?` is `choice(X, )`, so the empty branch is a way to reach whatever
        // `X` can reach without consuming. `X*` and `X+` are a helper rule whose
        // production `aux: aux X` hands over to itself when `X` can match
        // nothing — the one case where repeating can never advance.
        ExprKind::Optional(inner) => {
            if is_empty(inner, empty) {
                handovers(rule, inner, empty, out);
            }
        }
        ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            if is_empty(inner, empty) {
                out.push((rule.to_owned(), expr.span));
            }
        }
        ExprKind::Call { name, args } => match name.as_str() {
            "eof" | "blank" => {}
            "optional" => {
                if let Some(arg) = args.first().filter(|arg| is_empty(arg, empty)) {
                    handovers(rule, arg, empty, out);
                }
            }
            "repeat" | "repeat1" => {
                if args.first().is_some_and(|arg| is_empty(arg, empty)) {
                    out.push((rule.to_owned(), expr.span));
                }
            }
            "token" | "token.immediate" | "alias" => {
                if let Some(arg) = args.first() {
                    handovers(rule, arg, empty, out);
                }
            }
            "prec" | "prec.left" | "prec.right" | "prec.dynamic" | "field" | "reserved" => {
                if let Some(arg) = args.get(1) {
                    handovers(rule, arg, empty, out);
                }
            }
            "seq" => {
                for (i, arg) in args.iter().enumerate() {
                    let rest_is_empty = args
                        .iter()
                        .enumerate()
                        .all(|(j, other)| j == i || is_empty(other, empty));
                    if rest_is_empty {
                        handovers(rule, arg, empty, out);
                    }
                }
            }
            "choice" => {
                for arg in args {
                    handovers(rule, arg, empty, out);
                }
            }
            _ => {}
        },
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

#[cfg(test)]
mod tests {
    use super::{analyse, epsilon_cycles};
    use crate::parser::Parser;

    fn cycles(source: &str) -> Vec<Vec<String>> {
        let ast = Parser::new(source).expect("lexes").parse().expect("parses");
        let nullable = analyse(&ast);
        let mut cycles: Vec<_> = epsilon_cycles(&ast, &nullable)
            .into_iter()
            .map(|cycle| cycle.rules)
            .collect();
        cycles.sort();
        cycles
    }

    #[test]
    fn reports_disjoint_strongly_connected_components_once_each() {
        assert_eq!(
            cycles("%name t;\nroot: a;\na: b | ();\nb: a | ();\nx: y | ();\ny: x | ();"),
            vec![vec!["a".to_owned(), "b".to_owned()], vec!["x".to_owned(), "y".to_owned()]],
        );
    }

    #[test]
    fn a_nullable_acyclic_chain_is_not_a_cycle() {
        assert!(cycles("%name t;\nroot: a;\na: b;\nb: c;\nc: ();\n").is_empty());
    }

    #[test]
    fn a_repeat_of_nullable_content_is_a_single_rule_component() {
        assert_eq!(
            cycles("%name t;\nroot: a;\na: ('x'?)*;\n"),
            vec![vec!["a".to_owned()]],
        );
    }
}

pub(crate) fn collect_references(expr: &Expr, out: &mut HashMap<String, Vec<Span>>) {
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
