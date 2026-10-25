//! `%precedence` levels: the yacc-compatible surface.
//!
//! Yacc and Lemon give a production its precedence from its rightmost terminal
//! and let one table decide associativity, so a grammar states its precedence
//! once, as a table, instead of marking every production by hand:
//!
//! ```tsg
//! %precedence left  "add" = '+' '-';
//! %precedence right "pow" = '^';
//! ```
//!
//! `tree-sitter` has no such inference: `prec`/`prec.left`/`prec.right` wrap a
//! *production*. This module recovers the yacc behaviour by rewriting the AST
//! before lowering — each production whose rightmost terminal is declared in a
//! level is wrapped in the `prec` call the author would have written. Nothing
//! downstream changes: the emitted `grammar.json` is identical to the explicit
//! spelling, which is how the rewrite is tested (`lua53/`).

use std::collections::HashMap;

use crate::{
    lexer::{Error, Res, Span},
    parser::{Assoc, Expr, ExprKind, GrammarAst},
};

/// One declared level, in declaration order (highest precedence first).
pub struct Level {
    pub name: String,
    pub assoc: Option<Assoc>,
    /// The terminals as written, for `--precedences`.
    pub terminals: Vec<String>,
    pub span: Span,
}

/// A resolved level for one terminal key.
#[derive(Clone)]
struct Mark {
    name: String,
    assoc: Option<Assoc>,
}

/// The precedence table recovered from the directives, plus the projections the
/// rewrite and `--precedences` need.
pub struct Levels {
    by_terminal: HashMap<String, Mark>,
    pub levels: Vec<Level>,
    rules: HashMap<String, Expr>,
}

impl Levels {
    #[must_use]
    pub fn from_ast(ast: &GrammarAst) -> Self {
        let rules: HashMap<String, Expr> = ast
            .rules
            .iter()
            .map(|(name, body)| (name.clone(), body.clone()))
            .collect();

        let mut by_terminal = HashMap::new();
        let mut levels = Vec::new();
        for group in &ast.precedences {
            if !group.is_level() {
                continue;
            }
            // A level has exactly one name; the parser enforces that.
            let Some(name) = group.entries.first().and_then(|e| match &e.kind {
                ExprKind::Str(s) => Some(s.clone()),
                _ => None,
            }) else {
                continue;
            };
            for terminal in &group.terminals {
                for key in resolve_keys(terminal, &rules, 4) {
                    by_terminal.insert(
                        key,
                        Mark {
                            name: name.clone(),
                            assoc: group.assoc,
                        },
                    );
                }
            }
            levels.push(Level {
                name,
                assoc: group.assoc,
                terminals: group.terminals.iter().map(display_terminal).collect(),
                span: group.span,
            });
        }
        Self {
            by_terminal,
            levels,
            rules,
        }
    }

    /// Whether anything was declared that could be inherited; lets `compile`
    /// skip the rewrite entirely for grammars that never use the table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_terminal.is_empty()
    }

    /// The mark a production's rightmost terminal gives it, if any.
    #[must_use]
    pub fn mark_for(&self, production: &Expr) -> Option<(String, Option<Assoc>)> {
        if is_annotated(production) {
            return None;
        }
        let key = self.rightmost_terminal(production, 4)?;
        let mark = self.by_terminal.get(&key)?;
        Some((mark.name.clone(), mark.assoc))
    }

    /// Wrap every unannotated production whose rightmost terminal is in the
    /// table. Returns the number of marks applied.
    pub fn apply(&self, ast: &mut GrammarAst) -> usize {
        if self.is_empty() {
            return 0;
        }
        let mut applied = 0;
        for (_, body) in &mut ast.rules {
            // A token rule is one terminal already; wrapping its pattern in
            // `prec` would turn the terminal into a nonterminal. A level marks
            // the *productions* that use a terminal, not the terminal's own
            // definition — including when the level lists the rule by name.
            if is_lexical_body(body) {
                continue;
            }
            let mut marks: Vec<Option<Expr>> = Vec::new();
            match &body.kind {
                ExprKind::Choice(items) => {
                    for item in items {
                        marks.push(self.wrap(item, &mut applied));
                    }
                    if let ExprKind::Choice(items) = &mut body.kind {
                        for (slot, mark) in items.iter_mut().zip(marks) {
                            if let Some(wrapped) = mark {
                                *slot = wrapped;
                            }
                        }
                    }
                }
                _ => {
                    if let Some(wrapped) = self.wrap(body, &mut applied) {
                        *body = wrapped;
                    }
                }
            }
        }
        applied
    }

    /// The `prec` call for one production, or `None` when nothing is declared
    /// for its rightmost terminal.
    fn wrap(&self, production: &Expr, applied: &mut usize) -> Option<Expr> {
        let (name, assoc) = self.mark_for(production)?;
        *applied += 1;
        Some(Expr {
            span: production.span,
            kind: ExprKind::Call {
                name: function_for(assoc).to_owned(),
                args: vec![
                    Expr {
                        span: production.span,
                        kind: ExprKind::Str(name),
                    },
                    production.clone(),
                ],
            },
        })
    }

    /// The rightmost terminal of a production — the symbol yacc would take the
    /// precedence from. Conservative: anything that can be absent or ambiguous
    /// yields `None`, so a production is never marked by accident.
    fn rightmost_terminal(&self, expr: &Expr, depth: u8) -> Option<String> {
        if depth == 0 {
            return None;
        }
        match &expr.kind {
            ExprKind::Str(value) => Some(key_str(value)),
            ExprKind::Pattern { value, flags } => Some(key_pattern(value, flags)),
            // A rule reference counts only when that rule is a terminal rule.
            ExprKind::Symbol(name) => {
                let body = self.rules.get(name)?;
                match &body.kind {
                    ExprKind::Str(value) => Some(key_str(value)),
                    ExprKind::Pattern { .. } => self.rightmost_terminal(body, depth - 1),
                    ExprKind::Call { name, .. } if is_token_call(name) => {
                        self.rightmost_terminal(body, depth - 1)
                    }
                    _ => None,
                }
            }
            // Yacc takes the precedence from the *last terminal* in the rule,
            // not from the last symbol: in `exp '+' exp` that is `'+'`, because
            // a trailing non-terminal contributes nothing.
            ExprKind::Seq(items) => items
                .iter()
                .rev()
                .find_map(|item| self.rightmost_terminal(item, depth - 1)),
            ExprKind::Call { name, args } => match (name.as_str(), args.len()) {
                ("seq", n) if n > 0 => args
                    .iter()
                    .rev()
                    .take(n)
                    .find_map(|arg| self.rightmost_terminal(arg, depth - 1)),
                ("prec" | "prec.left" | "prec.right" | "prec.dynamic", 2) => {
                    self.rightmost_terminal(&args[1], depth - 1)
                }
                ("token" | "token.immediate" | "optional" | "repeat" | "repeat1", 1) => {
                    self.rightmost_terminal(&args[0], depth - 1)
                }
                ("field" | "reserved", 2) => self.rightmost_terminal(&args[1], depth - 1),
                ("alias", 2) => self.rightmost_terminal(&args[0], depth - 1),
                _ => None,
            },
            // A choice can end in either side, a `?`/`*` can end in nothing:
            // neither has a rightmost terminal.
            ExprKind::Choice(_)
            | ExprKind::Optional(_)
            | ExprKind::Repeat(_)
            | ExprKind::Repeat1(_)
            | ExprKind::Number(_) => None,
        }
    }

    /// Check the declarations that only make sense with a table: a level name
    /// must be quoted (a bare name is a rule reference), and a terminal must
    /// name something the grammar defines.
    pub fn check(&self, ast: &GrammarAst) -> Vec<Error> {
        let mut errors = Vec::new();
        let defined: HashMap<&str, Span> = ast
            .rule_names
            .iter()
            .map(|(name, span)| (name.as_str(), *span))
            .collect();
        for group in ast.precedences.iter().filter(|group| group.is_level()) {
            if let Some(entry) = group
                .entries
                .first()
                .filter(|entry| matches!(entry.kind, ExprKind::Symbol(_)))
            {
                let written = display_terminal(entry);
                errors.push(
                    Error::new(
                        entry.span,
                        format!("`{written}` names a rule, so it cannot be a precedence level"),
                    )
                    .with_help(format!(
                        "quote the level name: `%precedence {} \"{}\";` — a bare entry gives \
                         that rule the precedence, a quoted one declares a name",
                        assoc_word(group.assoc),
                        written.trim_start_matches('$'),
                    )),
                );
                continue;
            }
            for terminal in &group.terminals {
                let ExprKind::Symbol(name) = &terminal.kind else {
                    continue;
                };
                if !defined.contains_key(name.as_str()) {
                    errors.push(
                        Error::new(
                            terminal.span,
                            format!("`{name}` is not a rule, so it cannot name a terminal"),
                        )
                        .with_help(
                            "a level inherits down to the terminals it lists: write the literal \
                         (`'+'`), a pattern (`/[+]/`), or the name of a token rule",
                        ),
                    );
                }
            }
        }
        errors
    }
}

/// The orderings `grammar.json` should contain.
///
/// A directive without associativity and without terminals is one *ordering*
/// (its own list), which is what `--from-json` prints and what allows
/// deliberately incomparable groups. Consecutive levels share one ordering, the
/// way `%left`/`%right` lines stack up, so the table is one descending chain.
#[must_use]
pub fn orderings(ast: &GrammarAst) -> Vec<Vec<&Expr>> {
    let mut lists: Vec<Vec<&Expr>> = Vec::new();
    let mut table: Vec<&Expr> = Vec::new();
    for group in &ast.precedences {
        if group.is_level() {
            table.extend(group.entries.iter());
        } else {
            if !table.is_empty() {
                lists.push(std::mem::take(&mut table));
            }
            lists.push(group.entries.iter().collect());
        }
    }
    if !table.is_empty() {
        lists.push(table);
    }
    lists
}

const fn assoc_word(assoc: Option<Assoc>) -> &'static str {
    match assoc {
        Some(Assoc::Left) => "left",
        Some(Assoc::Right) => "right",
        None => "",
    }
}

pub const fn function_for(assoc: Option<Assoc>) -> &'static str {
    match assoc {
        Some(Assoc::Left) => "prec.left",
        Some(Assoc::Right) => "prec.right",
        None => "prec",
    }
}

/// Whether a rule body is itself a token definition (`/x/`, `'x'`, `token(…)`),
/// which a precedence level must leave alone.
fn is_lexical_body(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Str(_) | ExprKind::Pattern { .. } => true,
        ExprKind::Call { name, .. } => is_token_call(name),
        _ => false,
    }
}

fn is_token_call(name: &str) -> bool {
    matches!(name, "token" | "token.immediate")
}

/// A production that already carries a precedence mark, from `@` or from an
/// explicit `prec(…)` call, keeps it: an annotation always wins over the table.
fn is_annotated(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { name, .. } if name.starts_with("prec"))
}

fn key_str(value: &str) -> String {
    format!("s:{value}")
}

fn key_pattern(value: &str, flags: &str) -> String {
    format!("p:{value}/{flags}")
}

/// Every terminal key a declaration could mean. A rule name resolves to the
/// terminal it is defined as, and keeps a name-keyed alias so that `= LT` and a
/// production ending in `LT` meet even when the rule nests tokens.
fn resolve_keys(expr: &Expr, rules: &HashMap<String, Expr>, depth: u8) -> Vec<String> {
    let mut keys = Vec::new();
    collect_keys(expr, rules, depth, &mut keys);
    keys
}

fn collect_keys(expr: &Expr, rules: &HashMap<String, Expr>, depth: u8, keys: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Str(value) => keys.push(key_str(value)),
        ExprKind::Pattern { value, flags } => keys.push(key_pattern(value, flags)),
        ExprKind::Symbol(name) => {
            keys.push(format!("n:{name}"));
            if depth > 0 {
                if let Some(body) = rules.get(name) {
                    collect_keys(body, rules, depth - 1, keys);
                }
            }
        }
        ExprKind::Call { name, args } if is_token_call(name) && args.len() == 1 => {
            collect_keys(&args[0], rules, depth.saturating_sub(1), keys);
        }
        _ => {}
    }
}

/// Diagnostic text for `--precedences`.
pub fn describe(src: &str) -> Res<String> {
    let ast = crate::parser::Parser::new(src)?.parse()?;
    let levels = Levels::from_ast(&ast);
    if let Some(error) = levels.check(&ast).into_iter().next() {
        return Err(error);
    }
    let mut out = String::new();
    if levels.levels.is_empty() && ast.precedences.is_empty() {
        return Ok("no `%precedence` directives\n".to_owned());
    }

    out.push_str("precedence, highest first\n");
    let mut inherited = 0usize;
    let mut explicit = 0usize;
    for level in &levels.levels {
        let terminals = if level.terminals.is_empty() {
            "(none)".to_owned()
        } else {
            level.terminals.join(" ")
        };
        let mut users = Vec::new();
        for (name, body) in &ast.rules {
            for production in productions(body) {
                let annotated = is_annotated(production);
                let mark = if annotated {
                    annotate_name(production)
                } else {
                    levels.mark_for(production).map(|(name, _)| name)
                };
                if mark.as_deref() == Some(level.name.as_str()) {
                    if annotated {
                        explicit += 1;
                    } else {
                        inherited += 1;
                    }
                    let (line, col, ..) = crate::lexer::locate(src, production.span.start);
                    users.push(format!(
                        "    {line:>4}:{col}  {name}: {}",
                        one_line(src, production)
                    ));
                }
            }
        }
        out.push_str(&format!(
            "  {} {}\n    {}\n",
            assoc_word(level.assoc),
            level.name,
            terminals
        ));
        for user in users {
            out.push_str(&user);
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "\n{inherited} inherited, {explicit} explicit (`@`), {} level(s)\n",
        levels.levels.len()
    ));
    Ok(out)
}

fn productions(body: &Expr) -> Vec<&Expr> {
    match &body.kind {
        ExprKind::Choice(items) => items.iter().collect(),
        _ => vec![body],
    }
}

fn annotate_name(production: &Expr) -> Option<String> {
    match &production.kind {
        ExprKind::Call { name, args } if name.starts_with("prec") && args.len() == 2 => {
            match &args[0].kind {
                ExprKind::Str(s) | ExprKind::Symbol(s) => Some(s.clone()),
                ExprKind::Number(n) => Some(n.to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn display_terminal(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Str(value) => format!("'{value}'"),
        ExprKind::Pattern { value, flags } => format!("/{value}/{flags}"),
        ExprKind::Symbol(name) => name.clone(),
        _ => "?".to_owned(),
    }
}

fn one_line(src: &str, expr: &Expr) -> String {
    let text = src
        .get(expr.span.start..expr.span.end)
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > 60 {
        let cut: String = text.chars().take(60).collect();
        format!("{cut}…")
    } else {
        text
    }
}
