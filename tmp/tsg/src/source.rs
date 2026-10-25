//! Source positions for diagnostics raised *after* `grammar.json` exists.
//!
//! `crates/generate` reports unresolved conflicts, undeclared precedences and
//! the rest by **name**: by the time those errors are raised the document is
//! JSON, and the `.tsg` text is long gone. This module keeps the missing half —
//! a tiny index from name to [`Span`], built from the AST that the parser
//! already produced for free.
//!
//! Nothing here is threaded through lowering. The AST has a span on every node
//! (`parser.rs`), so the index is a projection of it:
//!
//! ```text
//! rule          `expr:`             name span (and body span)
//! directive     `%conflicts expr;`  one entry per name inside the directive
//! pattern       `/foo+/`           span of the text between the slashes
//! ```
//!
//! The renderer is deliberately rustc-shaped, like [`crate::lexer::render_error`],
//! but it accepts several labelled spans so an error can say both *where the
//! problem is* and *where the thing it clashes with is defined*.

use std::fmt::Write as _;

use crate::lexer::{locate, Span};
use crate::parser::{Expr, ExprKind, GrammarAst};

/// One name → span mapping. Kinds are spelled the way they appear in the
/// source, so a caller can pass them straight through: `"%extra"`,
/// `"%conflicts"`, `"%inline"`, `"%supertypes"`, `"%precedences"`,
/// `"%reserved"`, `"%external"`, `"%word"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: &'static str,
    /// The argument this entry is about. Empty for whole-directive spans.
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Index {
    rules: Vec<(String, Span, Span)>,
    directives: Vec<Entry>,
    patterns: Vec<(String, Span)>,
}

impl Index {
    /// Project the AST onto name → span lookups.
    pub fn from_ast(ast: &GrammarAst) -> Self {
        let bodies = |name: &str| {
            ast.rules
                .iter()
                .find(|(n, _)| n == name)
                .map_or(Span::default(), |(_, expr)| expr.span)
        };

        let rules = ast
            .rule_names
            .iter()
            .map(|(name, span)| (name.clone(), *span, bodies(name)))
            .collect();

        let mut directives = Vec::new();
        let mut push = |kind: &'static str, name: &str, span: Span| {
            directives.push(Entry {
                kind,
                name: name.to_owned(),
                span,
            });
        };

        for expr in &ast.extras {
            push("%extra", "", expr.span);
        }
        for expr in &ast.externals {
            push("%external", "", expr.span);
        }
        if let Some((word, span)) = &ast.word {
            push("%word", word, *span);
        }
        for (name, span) in &ast.inline {
            push("%inline", name, *span);
        }
        for (name, span) in &ast.supertypes {
            push("%supertypes", name, *span);
        }
        for (names, span) in &ast.conflicts {
            for name in names {
                push("%conflicts", name, *span);
            }
        }
        for group in &ast.precedences {
            for entry in &group.entries {
                if let ExprKind::Str(text) = &entry.kind {
                    push("%precedences", text, group.span);
                }
            }
        }
        for (name, _, span) in &ast.reserved {
            push("%reserved", name, *span);
        }

        let mut patterns = Vec::new();
        for expr in ast
            .rules
            .iter()
            .map(|(_, expr)| expr)
            .chain(&ast.extras)
            .chain(&ast.externals)
        {
            collect_patterns(expr, &mut patterns);
        }

        Self {
            rules,
            directives,
            patterns,
        }
    }

    /// The span of a rule's *name*.
    #[must_use]
    pub fn rule(&self, name: &str) -> Option<Span> {
        self.rules
            .iter()
            .find(|(n, ..)| n == name)
            .map(|(_, span, _)| *span)
    }

    /// The span of a rule's *body*, for errors that are about the production
    /// rather than the name.
    #[must_use]
    pub fn rule_body(&self, name: &str) -> Option<Span> {
        self.rules
            .iter()
            .find(|(n, ..)| n == name)
            .map(|(_, _, span)| *span)
    }

    /// The span of `name` inside a directive of the given kind, falling back to
    /// any entry of that kind (so `%extra` works without knowing its text).
    #[must_use]
    pub fn directive(&self, kind: &str, name: &str) -> Option<Span> {
        self.directives
            .iter()
            .find(|e| e.kind == kind && e.name == name)
            .or_else(|| self.directives.iter().find(|e| e.kind == kind))
            .map(|e| e.span)
    }

    /// The span of a pattern literal whose text is exactly `text`, or one that
    /// contains it (the generator sometimes reports a sub-pattern).
    #[must_use]
    pub fn pattern(&self, text: &str) -> Option<Span> {
        self.patterns
            .iter()
            .find(|(value, _)| value == text)
            .or_else(|| self.patterns.iter().find(|(value, _)| value.contains(text)))
            .map(|(_, span)| *span)
    }

    /// Where a rule or directive argument is defined, in the order a caller
    /// lists them; used to label "also defined here" notes.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<Span> {
        self.rule(name).or_else(|| {
            self.directives
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.span)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.directives.is_empty() && self.patterns.is_empty()
    }
}

fn collect_patterns(expr: &Expr, out: &mut Vec<(String, Span)>) {
    match &expr.kind {
        ExprKind::Pattern { value, .. } => {
            // The token span covers `/value/flags`; the text between the
            // slashes is copied verbatim, so the inner span is exact.
            let start = expr.span.start + 1;
            out.push((value.clone(), Span::new(start, start + value.len())));
        }
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                collect_patterns(item, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            collect_patterns(inner, out);
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                collect_patterns(arg, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Number(_) | ExprKind::Symbol(_) => {}
    }
}

/// A labelled span inside a rendered diagnostic.
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub text: String,
    /// The label the `-->` line points at; exactly one should be primary.
    pub primary: bool,
}

impl Label {
    #[must_use]
    pub fn primary(span: Span, text: impl Into<String>) -> Self {
        Self {
            span,
            text: text.into(),
            primary: true,
        }
    }

    #[must_use]
    pub fn note(span: Span, text: impl Into<String>) -> Self {
        Self {
            span,
            text: text.into(),
            primary: false,
        }
    }
}

/// Render a diagnostic with one or more labelled spans.
///
/// ```text
/// error: unresolved conflict in rule `expr`
///   --> grammar.tsg:4:1
///    |
///  4 | expr: expr "+" expr
///    | ^^^^ defined here
///  5 |     | expr "*" expr
///    |       ^^^^^^^^^^^^^ also involved
///    |
///    = help: …
/// ```
///
/// Lines further than [`GAP`] apart are split into separate blocks with a `...`
/// separator, so an error that mentions two distant rules does not dump the
/// whole file.
/// One underline, positioned in gutter columns.
struct Caret<'a> {
    start: usize,
    len: usize,
    text: &'a str,
}

pub fn render(
    src: &str,
    path: &str,
    kind: &str,
    headline: &str,
    labels: &[Label],
    help: Option<&str>,
) -> String {
    const GAP: usize = 2;

    let mut out = String::new();
    let _ = writeln!(out, "{kind}: {headline}");

    if labels.is_empty() {
        if let Some(help) = help {
            let _ = writeln!(out, "  = help: {help}");
        }
        return out;
    }

    // Every label resolved to its line, once.
    struct Placed<'a> {
        label: &'a Label,
        line: usize,
        col: usize,
        line_text: &'a str,
        line_start: usize,
    }
    let mut placed: Vec<Placed<'_>> = labels
        .iter()
        .map(|label| {
            let (line, col, line_text, line_start) = locate(src, label.span.start);
            Placed {
                label,
                line,
                col,
                line_text,
                line_start,
            }
        })
        .collect();
    placed.sort_by_key(|p| (p.line, p.col));

    let anchor = placed
        .iter()
        .find(|p| p.label.primary)
        .unwrap_or(&placed[0]);
    let width = placed
        .iter()
        .map(|p| p.line)
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let pad = " ".repeat(width);

    let _ = writeln!(out, "{pad}--> {path}:{}:{}", anchor.line, anchor.col);

    // Labels grouped by the line they land on, so that two labels on one line
    // share the source line and can share an underline row.
    let mut by_line: Vec<(usize, Vec<&Placed<'_>>)> = Vec::new();
    for p in &placed {
        match by_line.last_mut() {
            Some((line, group)) if *line == p.line => group.push(p),
            _ => by_line.push((p.line, vec![p])),
        }
    }

    let mut previous_line: Option<usize> = None;
    for (line, group) in &by_line {
        if let Some(prev) = previous_line {
            match line.saturating_sub(prev) {
                1 => {}
                gap if gap <= GAP => {
                    // Unlabelled lines in between are still worth showing.
                    for line in (prev + 1)..*line {
                        let (_, _, text, _) = locate(src, line_start_of(src, line));
                        let _ = writeln!(out, "{pad} |");
                        let _ = writeln!(out, "{line:>width$} | {text}");
                    }
                }
                _ => {
                    let _ = writeln!(out, "{pad}...");
                }
            }
        } else {
            let _ = writeln!(out, "{pad} |");
        }

        let line_text = group[0].line_text;
        let line_start = group[0].line_start;
        let _ = writeln!(out, "{line:>width$} | {line_text}");

        // Underlines are positioned relative to the end of the line's
        // indentation, which is copied through verbatim so that tabs still line
        // up with the source above.
        let indent_len = line_text.chars().take_while(|c| c.is_whitespace()).count();
        let indent_bytes: usize = line_text.chars().take(indent_len).map(char::len_utf8).sum();
        let prefix: String = line_text.chars().take(indent_len).collect();
        let base = line_start + indent_bytes;

        let carets: Vec<Caret<'_>> = group
            .iter()
            .map(|p| {
                let start = src[base..p.label.span.start.max(base)].chars().count();
                let end = p
                    .label
                    .span
                    .end
                    .min(line_start + line_text.len())
                    .max(p.label.span.start);
                let len = src[p.label.span.start.max(line_start)..end]
                    .chars()
                    .count()
                    .max(1)
                    .min(line_text.chars().count().saturating_sub(start).max(1));
                Caret {
                    start,
                    len,
                    text: &p.label.text,
                }
            })
            .collect();

        // Non-overlapping carets share a row; overlapping ones stack.
        let mut rows: Vec<Vec<Caret<'_>>> = Vec::new();
        for caret in carets {
            let row = rows
                .iter_mut()
                .find(|row| !row.iter().any(|other| overlaps(other, &caret)));
            match row {
                Some(row) => row.push(caret),
                None => rows.push(vec![caret]),
            }
        }

        for mut row in rows {
            row.sort_by_key(|caret| caret.start);
            let mut underline = String::new();
            let mut column = 0;
            for caret in &row {
                underline.push_str(&" ".repeat(caret.start.saturating_sub(column)));
                underline.push_str(&"^".repeat(caret.len));
                column = caret.start + caret.len;
            }
            let text = row
                .iter()
                .rev()
                .find(|caret| !caret.text.is_empty())
                .map(|caret| caret.text)
                .unwrap_or_default();
            if text.is_empty() {
                let _ = writeln!(out, "{pad} | {prefix}{underline}");
            } else {
                let _ = writeln!(out, "{pad} | {prefix}{underline} {text}");
            }
        }

        previous_line = Some(*line);
    }

    if let Some(help) = help {
        let _ = writeln!(out, "{pad} |");
        let _ = writeln!(out, "{pad} = help: {help}");
    }
    out
}

/// Whether two underlines would want the same gutter column.
fn overlaps(a: &Caret<'_>, b: &Caret<'_>) -> bool {
    a.start < b.start + b.len && b.start < a.start + a.len
}

fn line_start_of(src: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    src.match_indices('\n')
        .nth(line - 2)
        .map_or(src.len(), |(i, _)| i + 1)
}
