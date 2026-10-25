//! Prototype of the Level-1.5 elaboration layer described in
//! `../INTEGRATION.md`, run against the **real** parser generator.
//!
//! Pipeline, per fixture:
//!
//! ```text
//! grammar.tsg --(tsg)--> src/grammar.json --(tree-sitter generate --json-summary)--> errors
//!                                                                                      │
//!                          tsg::source::Index (name -> span) ──────────────────────────┤
//!                                                                                      ▼
//!                                                                            rustc-style carets
//! ```
//!
//! The errors are the generator's own, decoded from its `--json-summary`
//! envelope (`src/summary.rs`) rather than imitated, which is also why this
//! doubles as a test of the structured-diagnostics story: an editor gets these
//! positions from the same JSON.
//!
//! In the real integration the `match` in `elaborate` moves into
//! `crates/generate/src/generate.rs` (which owns the types), the `Index` comes
//! back from the loader next to `grammar_json`, and the CLI prints the result
//! instead of the bare `Display`. The index and the renderer are already in the
//! `tsg` crate — that is the part worth reviewing.

mod summary;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use tsg::{
    lexer,
    source::{self, Index, Label},
};

use summary::{GenDiagnostic, GenError};

// --- fixtures --------------------------------------------------------------

/// Grammars that `tsg` accepts and the generator rejects (or warns about):
/// exactly the class where `.tsg` loses its positions today.
const FIXTURES: &[(&str, &str)] = &[
    (
        "conflict",
        r#"%extras none;

file: expr;
expr: expr "+" expr
    | expr "*" expr
    | number;
number: /[0-9]+/;
"#,
    ),
    (
        "indirect",
        r#"%extras none;

file: _a;

_a: _b;
_b: _c;
_c: _a;
"#,
    ),
    (
        "undeclared_prec",
        r#"%extras none;

file: expr;
expr: prec("unary", seq("-", expr))
    | number;
number: /[0-9]+/;
"#,
    ),
    (
        "word_token",
        r#"%extras none;
%word kw;

file: kw;
kw: seq("i", "f");
"#,
    ),
    (
        "empty_rule",
        r#"%extras none;

file: _a;
_a: optional("x");
"#,
    ),
    (
        "empty_extra",
        r#"%extra ws;
ws: /\s*/;

file: "x";
"#,
    ),
    (
        "unary",
        r#"file: expr;
expr: choice(seq("x"));
"#,
    ),
    (
        "supertype_inline",
        r#"%supertypes _expr;
%inline _expr;

file: _expr;
_expr: "x";
"#,
    ),
    (
        "needless_conflicts",
        r#"%conflicts expr;

file: expr;
expr: "x" | "y";
"#,
    ),
];

fn main() {
    // Absolute, because `Command` runs with the grammar directory as its cwd.
    let tree_sitter = std::env::var_os("TS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bin/tree-sitter"),
        PathBuf::from,
    );
    let tree_sitter = tree_sitter.canonicalize().expect("locate tree-sitter");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".run");
    fs::create_dir_all(&dir).expect("create run dir");

    // `prototype <file.tsg>` explains one real grammar; no argument runs the
    // fixture suite.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(path) = args.first() {
        let path = PathBuf::from(path);
        let name = path
            .file_stem()
            .map_or_else(|| "input".to_owned(), |stem| stem.to_string_lossy().into_owned());
        let text = fs::read_to_string(&path).expect("read grammar");
        let copy = dir.join(format!("{name}.tsg"));
        fs::write(&copy, &text).expect("write copy");
        println!("┌─ {name} ─────────────────────────────────────────────────");
        explain(&tree_sitter, &dir, &copy);
        return;
    }

    for (name, text) in FIXTURES {
        let path = dir.join(format!("{name}.tsg"));
        fs::write(&path, text).expect("write fixture");
        println!("\n┌─ {name} ─────────────────────────────────────────────────");
        for line in text.trim_end().lines() {
            println!("│ {line}");
        }
        println!("└──────────────────────────────────────────────────────────");
        explain(&tree_sitter, &dir, &path);
    }
}

/// Compile, generate, and print whatever the generator said — annotated where
/// the index can place it.
fn explain(tree_sitter: &Path, dir: &Path, path: &Path) {
    let display = path.display().to_string();
    let src = fs::read_to_string(path).expect("read fixture");

    let compiled = match tsg::compile(&src) {
        Ok(compiled) => compiled,
        Err(errors) => {
            // Pre-JSON: tsg's own spans, unchanged.
            print!("{}", lexer::render_errors(&src, &display, &errors));
            return;
        }
    };

    let grammar_dir = dir.join(path.file_stem().expect("stem"));
    fs::create_dir_all(grammar_dir.join("src")).expect("create grammar dir");
    fs::write(grammar_dir.join("src/grammar.json"), &compiled.json).expect("write json");

    let output = Command::new(tree_sitter)
        .current_dir(&grammar_dir)
        .args(["generate", "src/grammar.json", "--json-summary"])
        .output()
        .expect("run tree-sitter");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let envelope = if stdout.trim_start().starts_with('{') {
        stdout
    } else {
        stderr
    };

    let summary = match summary::parse(&envelope) {
        Ok(summary) => summary,
        Err(message) => {
            println!("[cannot decode summary] {message}");
            return;
        }
    };

    match &summary.error {
        None => println!("(the generator accepted it)"),
        Some(err) => match elaborate(err, &src, &display, &compiled.index) {
            Some(text) => print!("{text}"),
            None => {
                println!("[UNELABORATED — no position available]");
                println!("{err:?}");
            }
        },
    }

    for diagnostic in &summary.diagnostics {
        match elaborate_diagnostic(diagnostic, &src, &display, &compiled.index) {
            Some(text) => print!("{text}"),
            None => println!("[UNELABORATED warning] {diagnostic:?}"),
        }
    }
}

// --- elaboration -----------------------------------------------------------

fn elaborate(err: &GenError, src: &str, path: &str, index: &Index) -> Option<String> {
    match err {
        GenError::Conflict(conflict) => conflict_error(conflict, src, path, index),
        GenError::AmbiguousExtra { parent_symbols } => {
            let mut labels = Vec::new();
            if let Some(span) = index.directive("%extra", "") {
                labels.push(Label::primary(span, "these extras end ambiguously"));
            }
            for symbol in parent_symbols {
                push_rule(&mut labels, index, symbol, "can end here");
            }
            Some(source::render(
                src,
                path,
                "error",
                "extra rules must have unambiguous endings",
                &labels,
                Some("make the extras disjoint, or give one a higher precedence"),
            ))
        }
        GenError::ImproperNonTerminalExtra(symbol) => {
            let mut labels = Vec::new();
            if let Some(span) = index.directive("%extra", "") {
                labels.push(Label::primary(span, "non-terminal used as an extra"));
            }
            push_rule(&mut labels, index, symbol, "not allowed here");
            Some(source::render(
                src,
                path,
                "error",
                &format!("the non-terminal rule `{symbol}` cannot be an extra"),
                &labels,
                Some("extras must be tokens; use a pattern, or wrap the rule in `token(...)`"),
            ))
        }
        GenError::UndeclaredPrecedence { precedence, rule } => {
            let mut labels = Vec::new();
            if let Some(span) = index.rule_body(rule) {
                labels.push(Label::primary(span, format!("`{rule}` uses `{precedence}`")));
            }
            Some(source::render(
                src,
                path,
                "error",
                &format!("precedence `{precedence}` is used in rule `{rule}` but never declared"),
                &labels,
                Some(&format!(
                    "declare it before using it: `%precedence \"{precedence}\";`"
                )),
            ))
        }
        GenError::ConflictingPrecedenceOrdering {
            precedence_1,
            precedence_2,
        } => {
            let mut labels = Vec::new();
            for name in [precedence_1, precedence_2] {
                if let Some(span) = index.directive("%precedences", name) {
                    labels.push(Label::note(span, format!("`{name}` is declared here")));
                }
            }
            Some(source::render(
                src,
                path,
                "error",
                &format!("`{precedence_1}` and `{precedence_2}` are ordered inconsistently"),
                &labels,
                Some("one precedence must be consistently above the other"),
            ))
        }
        GenError::IndirectRecursion(chain) => {
            let mut labels = Vec::new();
            if let Some(span) = chain.first().and_then(|first| index.rule(first)) {
                labels.push(Label::primary(span, "cycle starts here"));
            }
            for name in chain.iter().skip(1) {
                // The chain closes on its own start; do not caret the same rule
                // twice.
                if index.rule(name) != labels.first().map(|label| label.span) {
                    push_rule(&mut labels, index, name, "leads back to the start");
                }
            }
            let arrowed = chain.join(" -> ");
            Some(source::render(
                src,
                path,
                "error",
                &format!("indirectly recursive rule: {arrowed}"),
                &labels,
                Some("the cycle consumes nothing, so it can never terminate; make one production consume a token"),
            ))
        }
        GenError::NonTerminalWordToken {
            symbol_name,
            conflicting_symbol_name,
        } => {
            let mut labels = Vec::new();
            if let Some(span) = index.directive("%word", symbol_name) {
                labels.push(Label::primary(span, "the word token must be a single token"));
            }
            push_rule(&mut labels, index, symbol_name, "is not a token");
            if let Some(other) = conflicting_symbol_name {
                push_rule(&mut labels, index, other, "has a duplicate rule here");
            }
            Some(source::render(
                src,
                path,
                "error",
                &format!("`{symbol_name}` cannot be the word token: it is a non-terminal"),
                &labels,
                Some(&format!(
                    "point `%word` at a token, e.g. `{symbol_name}: /…/;` instead of a `seq`"
                )),
            ))
        }
        GenError::SupertypeTerminal(rule) | GenError::EmptyStringRule(rule) => {
            let verb = if matches!(err, GenError::SupertypeTerminal(_)) {
                "is a token, so it cannot be a supertype"
            } else {
                "can match nothing, which tree-sitter cannot parse"
            };
            Some(rule_body_error(rule, verb, src, path, index))
        }
        GenError::EmptyStringExtra(pattern) => {
            // The name is a rule (named extra) or a pattern (anonymous extra).
            let mut labels = Vec::new();
            if let Some(span) = index.directive("%extra", pattern) {
                labels.push(Label::primary(span, "this extra can match nothing"));
            }
            if let Some(span) = index.rule_body(pattern) {
                labels.push(Label::note(span, "the empty match comes from here"));
            } else if let Some(span) = index.pattern(pattern) {
                labels.push(Label::note(span, "the empty match comes from here"));
            }
            Some(source::render(
                src,
                path,
                "error",
                "an extra token that matches the empty string would loop forever",
                &labels,
                Some("require at least one character: `+` instead of `*`, or drop the optional part"),
            ))
        }
        GenError::Inline { kind, rule } => {
            let verb = match *kind {
                "FirstRule" => "is the start rule, so it cannot be inlined",
                "Token" => "is a token, so it cannot be inlined",
                _ => "has no reachable productions after inlining",
            };
            Some(rule_body_error(rule, verb, src, path, index))
        }
        GenError::Other(raw) => {
            println!("[UNELABORATED] {raw}");
            None
        }
    }
}

fn conflict_error(
    conflict: &summary::Conflict,
    src: &str,
    path: &str,
    index: &Index,
) -> Option<String> {
    // The rule name lives on the interpretations, not on the error itself.
    let subject = conflict
        .interpretations
        .first()
        .map(|(name, _)| name.as_str())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            conflict
                .symbol_sequence
                .iter()
                .find(|symbol| index.rule(symbol).is_some())
                .map(String::as_str)
        })?;

    let primary = index
        .rule(subject)
        .or_else(|| conflict_symbols(conflict).find_map(|name| index.rule(name)))?;

    let mut labels = vec![Label::primary(
        primary,
        format!(
            "`{subject}` is reduced here, `{}` is the lookahead",
            conflict.conflicting_lookahead
        ),
    )];
    let mut seen = vec![primary];
    for name in conflict_symbols(conflict) {
        if let Some(span) = index.rule(name) {
            if !seen.contains(&span) {
                seen.push(span);
                labels.push(Label::note(span, format!("`{name}` is also involved")));
            }
        }
    }

    Some(source::render(
        src,
        path,
        "error",
        &format!("unresolved conflict in rule `{subject}`"),
        &labels,
        Some(&resolution_help(conflict, subject)),
    ))
}

/// Translate the generator's own `possible_resolutions` into advice. This is
/// the same information the human-readable `Display` prints as
/// "Possible resolutions", reused as a one-line `help:`.
fn resolution_help(conflict: &summary::Conflict, subject: &str) -> String {
    use summary::Resolution::{AddConflict, Associativity, Precedence};

    let mut parts = Vec::new();
    for resolution in &conflict.resolutions {
        match resolution {
            Associativity(symbols) => parts.push(format!(
                "make `{}` left- or right-associative: `prec.left(…)` / `prec.right(…)`",
                symbols.join("`, `")
            )),
            // These are rule names, so the advice is rule-level `prec(…)`, not a
            // named precedence level.
            Precedence(symbols) => parts.push(format!(
                "give `{}` a higher rule precedence: `prec(…)` in that rule",
                symbols.join("` or `")
            )),
            AddConflict(symbols) => parts.push(format!(
                "accept the ambiguity with `%conflicts {};`",
                symbols.join(" ")
            )),
        }
    }
    if parts.is_empty() {
        format!("accept the ambiguity with `%conflicts {subject};`, or add a precedence")
    } else {
        parts.join(", or ")
    }
}

/// Rule-ish names in the conflict: the symbol sequence plus the interpretations'
/// variables. Literals (`'+'`) never match a rule, so they produce no label.
fn conflict_symbols(conflict: &summary::Conflict) -> impl Iterator<Item = &str> {
    conflict
        .symbol_sequence
        .iter()
        .map(String::as_str)
        .chain(
            conflict
                .interpretations
                .iter()
                .map(|(name, _)| name.as_str()),
        )
        .filter(|symbol| {
            !symbol.is_empty() && !symbol.starts_with('\'') && !symbol.starts_with('"')
        })
}

fn rule_body_error(rule: &str, verb: &str, src: &str, path: &str, index: &Index) -> String {
    let mut labels = Vec::new();
    if let Some(span) = index.rule_body(rule) {
        labels.push(Label::primary(span, format!("rule `{rule}`")));
    }
    source::render(
        src,
        path,
        "error",
        &format!("rule `{rule}` {verb}"),
        &labels,
        None,
    )
}

fn push_rule(labels: &mut Vec<Label>, index: &Index, name: &str, text: &str) {
    if let Some(span) = index.rule(name) {
        labels.push(Label::note(span, format!("`{name}` {text}")));
    }
}

fn elaborate_diagnostic(
    diagnostic: &GenDiagnostic,
    src: &str,
    path: &str,
    index: &Index,
) -> Option<String> {
    match diagnostic {
        GenDiagnostic::UnaryChoice(name) | GenDiagnostic::UnarySeq(name) => {
            let name = name.as_deref()?;
            // The diagnostic carries only the rule name, so the caret is
            // rule-granular; there is no span to point at the wrapper itself.
            let span = index.rule(name)?;
            let what = if matches!(diagnostic, GenDiagnostic::UnaryChoice(_)) {
                "choice"
            } else {
                "seq"
            };
            Some(source::render(
                src,
                path,
                "warning",
                &format!("`{what}` with a single element in rule `{name}`"),
                &[
                    Label::primary(span, format!("rule `{name}`")),
                    Label::note(
                        index.rule_body(name).unwrap_or(span),
                        format!("a `{what}(…)` wrapper around one element does nothing"),
                    ),
                ],
                Some(&format!("drop the `{what}(…)` wrapper")),
            ))
        }
        GenDiagnostic::SupertypeInlined(name) => {
            let mut labels = Vec::new();
            if let Some(span) = index.directive("%supertypes", name) {
                labels.push(Label::primary(span, "declared as a supertype"));
            }
            if let Some(span) = index.directive("%inline", name) {
                labels.push(Label::note(span, "but also inlined, which wins"));
            }
            Some(source::render(
                src,
                path,
                "warning",
                &format!("`{name}` is both a supertype and inlined, so the supertype is ignored"),
                &labels,
                Some("remove it from one of the two lists"),
            ))
        }
        GenDiagnostic::UnnecessaryConflicts(sets) => {
            let mut labels = Vec::new();
            for set in sets {
                for name in set {
                    if let Some(span) = index.directive("%conflicts", name) {
                        labels.push(Label::note(span, format!("`{name}` does not need it")));
                    }
                }
            }
            Some(source::render(
                src,
                path,
                "warning",
                "these `%conflicts` are unnecessary",
                &labels,
                Some("remove them; a conflict that does not exist hides the ones that do"),
            ))
        }
        GenDiagnostic::UnsupportedRegexFlag { flag, pattern } => {
            let span = index.pattern(pattern)?;
            Some(source::render(
                src,
                path,
                "warning",
                &format!("the flag `{flag}` in `/{pattern}/` does nothing in tree-sitter"),
                &[Label::primary(span, format!("`{flag}` is ignored"))],
                Some("drop the flag, or express the intent in the pattern itself"),
            ))
        }
        GenDiagnostic::Other(raw) => {
            println!("[UNELABORATED warning] {raw}");
            None
        }
    }
}
