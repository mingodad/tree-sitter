//! `tsg --annotate`: the grammar, plus what the front end already knows.
//!
//! Tier A of ROADMAP item 4.  No generator change, no new analysis: every note
//! is something `tsg` already computes to compile or to diagnose — token
//! priority (`lexing`), which rules can match nothing (`nullable`), where a
//! precedence level lands (`precedence`), and which tokens no rule ever names.
//! The point is that these facts decide how the parser behaves and are today
//! either implicit in the file's order or only visible in `--explain-lexing`.
//!
//! The output is still a `.tsg` file: comments only, attached above the rule or
//! section they describe.  Comments cannot reach `grammar.json`, so annotating
//! can never change the grammar — `--diff` ignores them and `--fmt` drops them,
//! which is exactly what makes the round trip a check rather than a promise.

use std::collections::BTreeMap;

use crate::{
    emit::{self, Notes, Output},
    lexer::{Error, Span},
    lexing, nullable,
    parser::{Assoc, ExprKind, GrammarAst, Parser},
    precedence::{self, Levels},
};

/// Re-print `source` with notes above every rule and section that has any.
pub fn annotate(source: &str) -> Result<Output, Error> {
    let ast = Parser::new(source)?.parse()?;
    let notes = notes(&ast);
    Ok(emit::annotate(source, &ast, &notes))
}

fn notes(ast: &GrammarAst) -> Notes {
    let lexing = lexing::analyse(ast);
    let empty = nullable::analyse(ast);
    let levels = Levels::from_ast(ast);
    let produced = produced_names(ast, &empty.references);
    let start = ast.start_rule();
    let token_count = lexing
        .order
        .iter()
        .filter(|entry| lexing.is_token(&entry.name))
        .count();

    let mut notes = Notes::default();

    // A token nothing names is the one *per-token* fact worth a line: the
    // generator emits it and no state ever accepts it, so it looks declared and
    // behaves as if deleted.  Priority needs no per-token note — the order in
    // the `tokens { … }` block *is* the priority order, and the block gets one
    // line saying so (below).  A rank on all 169 SQLite tokens would bury the
    // two that matter.
    for entry in &lexing.order {
        if !lexing.is_token(&entry.name) || produced.contains(&entry.name) {
            continue;
        }
        notes.rule(
            &entry.name,
            &[format!(
                "nothing produces this token: no rule body, `%extra`, `%reserved`, `%word` or \
                 `%external` names it, so the lexer can never return it (of {token_count} tokens)"
            )],
        );
    }

    // Nonterminals: emptiness (legal, unless it is circular) and which
    // alternatives a precedence level marks.
    let mut circles: BTreeMap<String, String> = BTreeMap::new();
    for cycle in nullable::epsilon_cycles(ast, &empty) {
        let names = cycle
            .rules
            .iter()
            .map(|rule| format!("`{rule}`"))
            .collect::<Vec<_>>();
        let text = if let Some(only) = names.first().filter(|_| names.len() == 1) {
            format!(
                "matches nothing and reaches itself, so the parser would reduce {only} forever: \
                 the compiler refuses this"
            )
        } else {
            format!(
                "{} can each match nothing only through the others, so the parser would reduce \
                 them forever: the compiler refuses this",
                names.join(", ")
            )
        };
        for rule in &cycle.rules {
            circles.insert(rule.clone(), text.clone());
        }
    }
    for (name, body) in &ast.rules {
        if lexing.is_token(name) {
            continue;
        }
        let mut lines: Vec<String> = Vec::new();
        if let Some(text) = circles.get(name) {
            lines.push(text.clone());
        } else if empty.rules.contains_key(name) {
            lines.push(if start == Some(name.as_str()) {
                "matches nothing, which a rule may: it reduces to a zero-width node".to_owned()
            } else {
                "can match nothing: it reduces to a zero-width node wherever it is used".to_owned()
            });
        }
        let productions = precedence::productions(body);
        // Level -> (associativity, how many alternatives it marks here).
        let mut inherited: BTreeMap<String, (Option<Assoc>, usize)> = BTreeMap::new();
        let mut explicit = 0;
        for production in &productions {
            if precedence::annotate_name(production).is_some() {
                explicit += 1;
                continue;
            }
            if let Some((level, assoc)) = levels.mark_for(production) {
                let slot = inherited.entry(level).or_insert((assoc, 0));
                slot.1 += 1;
            }
        }
        for (level, (assoc, marked)) in inherited {
            let assoc = associativity(assoc);
            let mut line = format!(
                "level `{level}` ({assoc}) marks {marked} of {} alternative{} through its \
                 rightmost terminal",
                productions.len(),
                if productions.len() == 1 { "" } else { "s" },
            );
            if explicit > 0 {
                line.push_str(&format!(" (the other {explicit} carry an explicit `@`)"));
            }
            lines.push(line);
        }
        if !lines.is_empty() {
            notes.rule(name, &lines);
        }
    }

    // Sections.  Each note says what the generator *does* with the block, which
    // is not the same as what it looks like it does.
    let mut extras: Vec<String> = Vec::new();
    if ast.no_extras {
        extras.push("`%extras none;`: nothing is skipped, not even whitespace".to_owned());
    } else if ast.extras.is_empty() {
        extras.push(
            "no `%extra` declared, so the compiler emits the DSL default `//\\s/` as the only one"
                .to_owned(),
        );
    } else {
        extras.push(
            "skipped between tokens; when no `%extra` is declared at all the compiler emits `//\\s/`"
                .to_owned(),
        );
    }
    notes.section("extras", &extras);

    if !ast.externals.is_empty() {
        notes.section(
            "externals",
            &[
                "these tokens come from `src/scanner.c`: `grammar.json` stores only their names, \
              not their patterns"
                    .to_owned(),
            ],
        );
    }

    if !ast.tokens.is_empty() {
        notes.section(
            "tokens",
            &[format!(
                "declared lexical rules: {token_count} of the grammar's tokens; the order below is \
                 the priority order — of two equal-length matches, the earlier rule wins"
            )],
        );
    }

    if !ast.precedences.is_empty() {
        let mut lines = Vec::new();
        let named = !levels.levels.is_empty();
        lines.push(if named {
            "highest first; a production with no explicit `@` takes the level of its rightmost \
             terminal"
                .to_owned()
        } else {
            "ordering only: entries go from higher to lower precedence, and a production takes the \
             level of its rightmost terminal"
                .to_owned()
        });
        for level in &levels.levels {
            let assoc = associativity(level.assoc);
            let terminals = if level.terminals.is_empty() {
                "(none)".to_owned()
            } else {
                level.terminals.join(" ")
            };
            lines.push(format!("`{}` ({assoc}) = {terminals}", level.name));
        }
        // `%precedence a b;` without `=` orders entries instead of naming a
        // level; those groups never reach `Levels`, so print them here.
        for group in ast.precedences.iter().filter(|group| !group.is_level()) {
            lines.push(
                group
                    .entries
                    .iter()
                    .map(entry_name)
                    .collect::<Vec<_>>()
                    .join(" > "),
            );
        }
        notes.section("precedences", &lines);
    }

    notes
}

/// Every name something in the grammar can actually hand to the lexer: rule-body
/// references, `%extra`s, `%reserved` sets, `%word`, and externals.
///
/// A token outside this set is dead — the generator emits its `Symbol` entry and
/// then never accepts it in any state, so the author gets a token that silently
/// never matches.  That is worth a line in the file rather than a paragraph in a
/// report nobody opens.
fn produced_names(
    ast: &GrammarAst,
    referenced: &std::collections::HashMap<String, Vec<Span>>,
) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = referenced.keys().cloned().collect();
    for (name, _, _) in &ast.reserved {
        out.insert(name.clone());
    }
    if let Some((word, _)) = &ast.word {
        out.insert(word.clone());
    }
    for external in &ast.externals {
        if let ExprKind::Symbol(name) = &external.kind {
            out.insert(name.clone());
        }
    }
    // `%extra` is an expression, not a name: `%extra /[ \t]+/;` produces nothing
    // but an anonymous pattern, while `%extra comment;` names a rule.  Only the
    // reference produces a token, so only the reference counts here.
    for extra in &ast.extras {
        names(extra, &mut out);
    }
    out
}

fn names(expr: &crate::parser::Expr, out: &mut std::collections::HashSet<String>) {
    match &expr.kind {
        ExprKind::Symbol(name) => {
            out.insert(name.clone());
        }
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                names(item, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            names(inner, out);
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                names(arg, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => {}
    }
}

/// How a `%precedence` entry reads in a comment.  Entries are names, literals
/// or numbers — anything else is the parser's business, not the annotation's.
fn entry_name(expr: &crate::parser::Expr) -> String {
    match &expr.kind {
        ExprKind::Str(text) => format!("'{text}'"),
        ExprKind::Symbol(name) => name.clone(),
        ExprKind::Number(number) => number.to_string(),
        _ => "(expression)".to_owned(),
    }
}

fn associativity(assoc: Option<Assoc>) -> &'static str {
    match assoc {
        Some(Assoc::Left) => "left",
        Some(Assoc::Right) => "right",
        None => "no associativity",
    }
}

#[cfg(test)]
mod tests {
    use super::annotate;
    use crate::{compile, emit};

    const GRAMMAR: &str = r#"
        // Annotated on purpose.
        %name calc;
        %extra /[ \t\n]+/;
        %word identifier;
        %precedence left "add" = '+' '-';
        %precedence left "mul" = '*' '/';

        tokens {
          integer: /[0-9]+/;
          identifier: /[a-z]+/;
          unused: /zzz/;
        }

        expr: expr '+' expr
            | expr '-' expr
            | expr '*' expr
            | integer;
    "#;

    fn json(source: &str) -> String {
        compile::compile(source)
            .unwrap_or_else(|errors| panic!("{}", errors.len()))
            .json
    }

    #[test]
    fn annotating_cannot_change_the_grammar() {
        let annotated = annotate(GRAMMAR).expect("annotates").text;
        assert_eq!(json(GRAMMAR), json(&annotated));
    }

    #[test]
    fn formatted_output_is_the_same_before_and_after() {
        let annotated = annotate(GRAMMAR).expect("annotates").text;
        assert_eq!(
            emit::fmt(GRAMMAR).expect("fmt").text,
            emit::fmt(&annotated).expect("fmt").text
        );
    }

    #[test]
    fn annotating_an_annotated_file_changes_nothing() {
        let once = annotate(GRAMMAR).expect("annotates").text;
        let twice = annotate(&once).expect("annotates").text;
        assert_eq!(once, twice);
    }

    #[test]
    fn notes_name_the_priority_order_and_the_dead_token() {
        let text = annotate(GRAMMAR).expect("annotates").text;
        assert!(text.contains("// nothing produces this token"), "{text}");
        assert!(
            text.contains("// declared lexical rules: 3 of the grammar's tokens"),
            "{text}"
        );
    }

    #[test]
    fn notes_say_where_a_precedence_level_lands() {
        let text = annotate(GRAMMAR).expect("annotates").text;
        assert!(
            text.contains("level `mul` (left) marks 1 of 4 alternatives"),
            "{text}"
        );
        assert!(
            text.contains("level `add` (left) marks 2 of 4 alternatives"),
            "{text}"
        );
        assert!(text.contains("// `mul` (left) = '*' '/'"), "{text}");
    }

    #[test]
    fn notes_say_which_rule_may_match_nothing_and_which_circle_is_refused() {
        // Annotating does not compile, so a note has to say which of the two
        // cases an empty production is — the legal one, or the circle the
        // compiler refuses — without waiting for the diagnostic.
        let source = "%name n;\nroot: middle?;\nmiddle: 'b' | ();\n";
        let text = annotate(source).expect("annotates").text;
        assert!(
            text.contains(
                "// can match nothing: it reduces to a zero-width node wherever it is used"
            ),
            "{text}"
        );
        let text = annotate("%name n;\nroot: a;\na: a?;\n")
            .expect("annotates")
            .text;
        assert!(
            text.contains("// matches nothing and reaches itself"),
            "{text}"
        );
        assert!(text.contains("the compiler refuses this"), "{text}");
    }

    #[test]
    fn notes_read_an_ordering_only_precedence_list() {
        let text = annotate("%name o;\n%precedence a 'and' 'or';\nroot: a;\na: 'x';\n")
            .expect("annotates")
            .text;
        assert!(text.contains("// a > 'and' > 'or'"), "{text}");
        assert!(text.contains("// ordering only"), "{text}");
    }

    #[test]
    fn a_grammar_with_nothing_to_say_gets_no_comments() {
        let text = annotate("%name tiny;\nroot: 'a';\n")
            .expect("annotates")
            .text;
        assert!(!text.contains("//"), "{text}");
    }
}
