//! Driver: `.tsg` source -> `grammar.json` text.

use std::collections::{HashMap, HashSet};

use crate::json::{json_rule, to_string, Json};
use crate::lexer::{Error, Span};
use crate::lexing;
use crate::parser::{Expr, ExprKind, GrammarAst, Parser};
use crate::precedence;
use crate::rules::{Lowerer, Rule};
use crate::source::Index;

/// Knobs for compilation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    /// Accept flags tree-sitter would silently ignore instead of rejecting them.
    /// Only useful when reproducing `grammar.js` output that carries JavaScript
    /// flags (`/x/g`, `/x/u`) byte for byte.
    pub allow_ignored_flags: bool,
}

/// A compiled document, plus anything worth saying about it that is not fatal.
#[derive(Debug)]
pub struct Compiled {
    pub json: String,
    pub warnings: Vec<Error>,
    /// Name → span for every rule, directive argument and pattern literal.
    /// `grammar.json` has no positions, so this is the only way to point at the
    /// `.tsg` text once the JSON exists.
    pub index: Index,
}

/// Compile `.tsg` source into a `grammar.json` document.
///
/// A *parse* error is fatal, because there is nothing to recover to. Everything
/// after that is per-rule, so all of those errors are collected and reported
/// together rather than one run at a time.
/// Compile with the default [`Options`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn compile(src: &str) -> Result<Compiled, Vec<Error>> {
    compile_with(src, Options::default())
}

pub fn compile_with(src: &str, options: Options) -> Result<Compiled, Vec<Error>> {
    let mut ast = match Parser::new(src).and_then(|parser| parser.parse()) {
        Ok(ast) => ast,
        Err(err) => return Err(vec![err]),
    };

    if ast.rules.is_empty() {
        return Err(vec![Error::new(
            Span::new(src.len(), src.len()),
            "a grammar needs at least one rule; the first rule is the start rule",
        )]);
    }

    // A `tokens { … }` block may sit before the start rule. Its rules are still
    // rules in file order — that is token priority — but the first rule of the
    // *grammar* has to be a non-token. Move the first rule that is not declared
    // lexical to the front, keeping every token's relative order unchanged.
    let declared: HashSet<&str> = ast.tokens.iter().map(|rule| rule.name.as_str()).collect();
    match ast
        .rules
        .iter()
        .position(|(name, _)| !declared.contains(name.as_str()))
    {
        Some(0) => {}
        Some(index) => {
            let start = ast.rules.remove(index);
            ast.rules.insert(0, start);
        }
        None => {
            return Err(vec![Error::new(
                Span::new(src.len(), src.len()),
                "a grammar needs a start rule outside `tokens`; every rule here is a token",
            )]);
        }
    }

    // External tokens introduce their own names, which ordinary rules may
    // then reference.
    let mut defined: HashSet<String> = ast.rules.iter().map(|(n, _)| n.clone()).collect();
    for external in &ast.externals {
        collect_symbols(external, &mut defined);
    }

    let lowerer = Lowerer::new(&defined);

    let mut errors: Vec<Error> = Vec::new();

    // yacc-style levels: `%precedence left "add" = '+' '-';` gives every
    // production ending in one of those terminals the level and its
    // associativity, exactly as `prec.left(add, …)` would. The rewrite happens
    // before lowering, so nothing downstream knows the difference.
    let levels = precedence::Levels::from_ast(&ast);
    errors.extend(levels.check(&ast));
    levels.apply(&mut ast);

    // A rule that can match nothing is only allowed as the start rule, and the
    // generator can only name it. Report it here, where the source is.
    errors.extend(nullability_errors(src, &ast));

    // Lexicality: what a `tokens { … }` block claims, and what `%word` and
    // `%extra` need the rule to be. Both are generator errors with no position
    // (`UnexpectedRule`, and an empty message for a non-token `%word`), so they
    // are checked here while the source is still in hand.
    let lexing = lexing::analyse(&ast);
    let (lexical_errors, reported) = lexical_errors(&ast, &lexing);
    errors.extend(lexical_errors);
    let mut lowered_rules: Vec<(String, Rule)> = Vec::with_capacity(ast.rules.len());
    for (name, expr) in &ast.rules {
        // One mistake, reported once: `lexical_errors` has already said what is
        // wrong with this rule, and lowering would repeat it without the
        // knowledge that a `tokens` block is what made the rule lexical.
        if reported.contains(name.as_str()) {
            continue;
        }
        match lowerer.lower(expr) {
            Ok(rule) => lowered_rules.push((name.clone(), rule)),
            Err(err) => errors.push(err),
        }
    }

    // Apply the DSL's default extra when none is declared. `dsl.js` does the
    // same, so grammars that omit `extras` keep working unchanged.
    let extras = if ast.no_extras {
        Vec::new()
    } else if ast.extras.is_empty() {
        vec![Rule::Pattern {
            value: "\\s".to_owned(),
            flags: None,
        }]
    } else {
        let mut out = Vec::with_capacity(ast.extras.len());
        for expr in &ast.extras {
            match lowerer.lower(expr) {
                Ok(Rule::Str(text)) if text.is_empty() => errors.push(Error::new(
                    expr.span,
                    "an empty string cannot be used as an `%extra`",
                )),
                Ok(rule) => out.push(rule),
                Err(err) => errors.push(err),
            }
        }
        out
    };

    // Lower externals. Their names *define* tokens, so they were added to
    // `defined` above and resolve like any other symbol.
    let mut externals: Vec<Rule> = Vec::with_capacity(ast.externals.len());
    for expr in &ast.externals {
        match lowerer.lower(expr) {
            Ok(rule) => externals.push(rule),
            Err(err) => errors.push(err),
        }
    }

    let mut reserved: Vec<(String, Vec<Rule>)> = Vec::with_capacity(ast.reserved.len());
    for (name, exprs, _span) in &ast.reserved {
        let mut rules = Vec::with_capacity(exprs.len());
        for expr in exprs {
            match lowerer.lower(expr) {
                Ok(rule) => rules.push(rule),
                Err(err) => errors.push(err),
            }
        }
        reserved.push((name.clone(), rules));
    }

    // Directive arguments that name rules must actually exist.
    let mut check = |name: &str, span: Span| {
        if let Err(err) = lowerer.check_symbol(name, span) {
            errors.push(err);
        }
    };
    if let Some((word, span)) = &ast.word {
        check(word, *span);
    }
    for (name, span) in &ast.inline {
        check(name, *span);
    }
    for (name, span) in &ast.supertypes {
        check(name, *span);
    }
    for (names, span) in &ast.conflicts {
        for name in names {
            check(name, *span);
        }
    }

    // A bare name in a `%precedence` list is a *rule* reference; only a quoted
    // one declares a precedence *name*. The JSON keeps both, so nothing
    // downstream can tell them apart, and `prec(call, …)` against the bare form
    // reaches the parser generator as "Undeclared precedence" with no position.
    let mut declared_names: HashSet<&str> = HashSet::new();
    let mut declared_rules: HashMap<&str, Span> = HashMap::new();
    for group in &ast.precedences {
        for entry in &group.entries {
            match &entry.kind {
                ExprKind::Str(name) => {
                    declared_names.insert(name.as_str());
                }
                ExprKind::Symbol(name) => {
                    declared_rules.insert(name.as_str(), entry.span);
                }
                _ => {}
            }
        }
    }

    // The first use of each precedence name, and only the first: one name used
    // in twenty productions is still one mistake.
    let mut used: Vec<(&str, Span)> = Vec::new();
    for (_, expr) in &ast.rules {
        let mut found = Vec::new();
        collect_precedence_names(expr, &mut found);
        for (name, span) in found {
            if !used.iter().any(|(seen, _)| *seen == name) {
                used.push((name, span));
            }
        }
    }
    let is_used = |name: &str| used.iter().any(|(used, _)| *used == name);

    // However many `prec` calls the names reach, the mistake is one directive:
    // report it where it was written, so the fix is a pair of quotes.
    for group in &ast.precedences {
        // A level already reports a bare name better: it can say `left`/`right`
        // and `=`, and there is one mistake, not two.
        if group.is_level() {
            continue;
        }
        let span = &group.span;
        let bare: Vec<&str> = group
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                ExprKind::Symbol(name) if is_used(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        if bare.is_empty() {
            continue;
        }
        let quoted: Vec<String> = bare.iter().map(|name| format!("\"{name}\"")).collect();
        let (what, rule) = if bare.len() == 1 {
            (
                format!("`{}` is used as a precedence name", bare[0]),
                "an unquoted entry here names a rule",
            )
        } else {
            (
                format!("`{}` are used as precedence names", bare.join("`, `")),
                "unquoted entries here name rules",
            )
        };
        errors.push(
            Error::new(*span, format!("{what}, but {rule}"))
            .with_help(format!(
                "quote {}: `%precedence {};` — a bare entry gives the rule that precedence, a quoted one declares a name",
                if bare.len() == 1 { "it" } else { "them" },
                quoted.join(" "),
            )),
        );
    }

    for (name, span) in &used {
        if declared_names.contains(name) || declared_rules.contains_key(name) {
            continue;
        }
        errors.push(
            Error::new(
                *span,
                format!("precedence `{name}` is used but never declared"),
            )
            .with_help(format!(
                "add it to a `%precedence` directive: `%precedence \"{name}\";`"
            )),
        );
    }

    // Level declarations check their own entries (`precedence::Levels::check`),
    // where the message can mention associativity and inheritance.
    let level_entries: HashSet<usize> = ast
        .precedences
        .iter()
        .filter(|group| group.is_level())
        .flat_map(|group| group.entries.iter().map(|entry| entry.span.start))
        .collect();
    for (name, span) in &declared_rules {
        if level_entries.contains(&span.start) {
            continue;
        }
        if !defined.contains(*name) && !is_used(name) {
            errors.push(Error::new(
                *span,
                format!("`%precedence` names the rule `{name}`, which is not defined"),
            ));
        }
    }

    if !errors.is_empty() {
        // Report top to bottom, the way the file reads.
        errors.sort_by_key(|err| (err.span.start, err.span.end));
        return Err(errors);
    }

    // Keep the name → span projection for diagnostics raised later, by the
    // parser generator, when the source text is no longer in scope.
    let index = Index::from_ast(&ast);

    // Flags that cannot do anything are an error: "accepted and ignored" is the
    // exact failure mode a declarative grammar is supposed to remove. With
    // `allow_ignored_flags` they are reported but tolerated, so that
    // `grammar.js` output can still be reproduced byte for byte.
    let mut flagged = Vec::new();
    let rule_names: HashSet<&str> = ast.rules.iter().map(|(name, _)| name.as_str()).collect();
    for (_, expr) in &ast.rules {
        check_pattern_flags(expr, &rule_names, &mut flagged);
    }
    for expr in ast.extras.iter().chain(&ast.externals) {
        check_pattern_flags(expr, &rule_names, &mut flagged);
    }
    for (_, exprs, _) in &ast.reserved {
        for expr in exprs {
            check_pattern_flags(expr, &rule_names, &mut flagged);
        }
    }
    flagged.sort_by_key(|err| (err.span.start, err.span.end));

    if !options.allow_ignored_flags && !flagged.is_empty() {
        return Err(flagged);
    }
    let mut warnings = flagged;

    // A rule named after a DSL function is legal, but `field (` is a call, so the
    // rule cannot be referenced without `$field`. Say it once, where the rule is
    // defined, instead of leaving an arity error about a call nobody wrote.
    for (name, span) in &ast.rule_names {
        if crate::rules::CALL_NAMES.contains(&name.as_str()) {
            warnings.push(
                Error::new(
                    *span,
                    format!("rule `{name}` has the same name as the DSL function `{name}(…)`"),
                )
                .with_help(format!(
                    "`{name} (` reads as a call, so reference the rule as `${name}` or rename it"
                )),
            );
        }
    }
    warnings.sort_by_key(|err| (err.span.start, err.span.end));

    Ok(Compiled {
        json: to_string(&document(
            &ast,
            &lowered_rules,
            &externals,
            &extras,
            &reserved,
        )),
        warnings,
        index,
    })
}

/// Every precedence name passed to `prec` / `prec.left` / `prec.right`, with the
/// span of the name itself.
///
/// Mirrors the arity rules in [`crate::rules`]: `prec.left(x)` is a *content*
/// with a default precedence, not a value, so only the two-argument form has one.
/// One diagnostic per rule that can derive nothing but is used somewhere.
///
/// The primary span is the alternative that produces no tokens, because that is
/// what has to change; the help names the use sites, because *that* is where
/// tree-sitter's own message leaves the reader.
/// A token cannot be built out of another rule, and a `tokens` block says so
/// before the generator does. `%word` and `%extra` likewise need a token.
fn lexical_errors<'a>(
    ast: &'a GrammarAst,
    lexing: &lexing::Lexing,
) -> (Vec<Error>, std::collections::HashSet<&'a str>) {
    let mut errors = Vec::new();
    let mut reported = std::collections::HashSet::new();

    for rule in &ast.tokens {
        let Some((_, body)) = ast.rules.iter().find(|(name, _)| *name == rule.name) else {
            continue;
        };
        // The parser wrapped the body in `token(…)`, so the interesting part is
        // one level in; describing the wrapper would say nothing.
        let content = match &body.kind {
            ExprKind::Call { name, args } if name == "token" || name == "token.immediate" => {
                &args[0]
            }
            _ => body,
        };
        if let Some((span, what)) = lexing::non_lexical(content) {
            reported.insert(rule.name.as_str());
            errors.push(
                Error::new(
                    span,
                    format!(
                        "`{}` is declared in `tokens`, but this is not lexical: {what}",
                        rule.name
                    ),
                )
                .with_help(
                    "a token is built only from strings and regexes, so it cannot use another \
                     rule; write the pattern out here, or move the rule out of the block and \
                     make it a nonterminal",
                ),
            );
        } else if let Some(span) = lexing::empty_span(content) {
            errors.push(
                Error::new(
                    span,
                    format!(
                        "`{}` is declared in `tokens`, but it can match the empty string",
                        rule.name
                    ),
                )
                .with_help(
                    "a token has to consume at least one character; remove the empty \
                     alternative, or move the rule out of the block and make it a \
                     nonterminal",
                ),
            );
        }
    }

    if let Some((word, span)) = &ast.word {
        if !lexing.is_token(word) {
            errors.push(
                Error::new(
                    *span,
                    format!("`%word {word}` needs a token, but `{word}` is a nonterminal"),
                )
                .with_help(
                    "`%word` names the token that keywords are made of, so its rule must be a \
                     single pattern; move it into a `tokens { … }` block",
                ),
            );
        }
    }

    for extra in &ast.extras {
        let body = lexing::extra_body(extra, ast);
        // An extra has to consume something. The direct `%extra '';` case is
        // reported by lowering (with its own wording); this catches the shapes
        // that only become empty through a rule body (`x: 'a'?;`).
        if let Some(span) = lexing::empty_span(body) {
            if !matches!(&body.kind, ExprKind::Str(value) if value.is_empty()) {
                errors.push(
                    Error::new(span, "an `%extra` rule must not match the empty string").with_help(
                        "extras are skipped between tokens; if they can match nothing, \
                             the lexer cannot make progress",
                    ),
                );
            }
        } else if let Some((span, what)) = lexing::extra_problem(extra, ast, lexing) {
            errors.push(
                Error::new(
                    span,
                    format!(
                        "an `%extra` rule must start with a token, but this starts with {what}"
                    ),
                )
                .with_help(
                    "extras are skipped between tokens, so each alternative has to start \
                     with a string, a regex, or a token rule",
                ),
            );
        }
    }

    (errors, reported)
}

fn nullability_errors(src: &str, ast: &GrammarAst) -> Vec<Error> {
    let nullable = crate::nullable::analyse(ast);
    let start = ast.rules.first().map(|(name, _)| name.as_str());
    let mut errors = Vec::new();
    for (name, _body) in &ast.rules {
        if Some(name.as_str()) == start {
            continue;
        }
        let Some(cause) = nullable.rules.get(name) else {
            continue;
        };
        let uses = nullable.references.get(name).map_or(&[][..], Vec::as_slice);
        if uses.is_empty() {
            continue;
        }
        let (line, col, ..) = crate::lexer::locate(src, uses[0].start);
        let count = uses.len();
        let user = if count == 1 {
            format!("`{name}` is used once, at {line}:{col}")
        } else {
            format!("`{name}` is used {count} times, the first at {line}:{col}")
        };
        errors.push(
            Error::new(
                *cause,
                format!("`{name}` can match nothing: this alternative produces no tokens"),
            )
            .with_help(format!(
                "tree-sitter allows that only for the start rule, and {user}; \
                 write `{name}?` there, or move the emptiness to the use site \
                 (e.g. `stat*` instead of a `block` rule that wraps it)",
            )),
        );
    }
    errors
}

fn collect_precedence_names<'a>(expr: &'a Expr, out: &mut Vec<(&'a str, Span)>) {
    match &expr.kind {
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                collect_precedence_names(item, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            collect_precedence_names(inner, out);
        }
        ExprKind::Call { name, args } => {
            let has_value = match name.as_str() {
                "prec" => true,
                "prec.left" | "prec.right" => args.len() == 2,
                _ => false,
            };
            if has_value {
                if let Some(first) = args.first() {
                    if let ExprKind::Str(n) | ExprKind::Symbol(n) = &first.kind {
                        out.push((n, first.span));
                    }
                }
            }
            for arg in args {
                collect_precedence_names(arg, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) | ExprKind::Symbol(_) => {
        }
    }
}

/// tree-sitter keeps `i`, treats `u` and `v` as "unicode flags" to ignore, and
/// drops everything else with a diagnostic of its own — but it never fails, so
/// `/x/s` silently stops meaning what it says (the author expected `.` to match
/// a newline) and the source position is long gone by then. This checks the
/// same set, with the span of the flag, and refuses to compile.
fn check_pattern_flags(expr: &Expr, rules: &HashSet<&str>, out: &mut Vec<Error>) {
    match &expr.kind {
        ExprKind::Pattern { value, flags } => {
            // A flags run that is also a rule name is a likely typo for the
            // sequence: `/x/ g` reaches the rule, `/x/g` reads it as a flag.
            // Neither the flag nor the sequence is the problem here, the
            // missing space is, so say so.
            let space_hint = rules.contains(flags.as_str()).then(|| {
                format!("if you meant the rule `{flags}`, put a space after the literal: `/…/ {flags}`; otherwise drop the flag")
            });
            let mut seen_i = false;
            for (index, flag) in flags.char_indices() {
                // `i` is the only flag tree-sitter keeps. `u` and `v` are
                // accepted and then dropped, which is the same silent no-op.
                if flag == 'i' {
                    if seen_i {
                        let start = expr.span.start + 1 + value.len() + 1 + index;
                        out.push(
                            Error::new(
                                Span::new(start, start + 1),
                                "regex flag `i` is repeated, and it cannot mean anything twice",
                            )
                            .with_help("drop the repeat: `i` is the only flag, so it is either there or not"),
                        );
                    }
                    seen_i = true;
                    continue;
                }
                // The flag characters sit right after `/value/`, so the offset
                // is exact and the caret lands on the offending letter.
                let start = expr.span.start + 1 + value.len() + 1 + index;
                let span = Span::new(start, start + flag.len_utf8());
                let (why, help) = flag_advice(flag);
                out.push(
                    Error::new(
                        span,
                        format!("regex flag `{flag}` has no effect in tree-sitter: {why}"),
                    )
                    .with_help(space_hint.clone().unwrap_or_else(|| help.to_string())),
                );
            }
        }
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                check_pattern_flags(item, rules, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            check_pattern_flags(inner, rules, out);
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                check_pattern_flags(arg, rules, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Number(_) | ExprKind::Symbol(_) => {}
    }
}

/// Why a flag cannot work, and what to write instead.
fn flag_advice(flag: char) -> (&'static str, &'static str) {
    match flag {
        'u' => (
            "it is JavaScript's `unicode` mode, which is always on",
            "drop the flag; there is no non-Unicode mode to switch to",
        ),
        'v' => (
            "it is JavaScript's `unicodeSets` mode, which is unsupported",
            "drop the flag; there is no `unicodeSets` mode to switch to",
        ),
        'g' => (
            "it is JavaScript's `global` flag, which means nothing here",
            "drop the flag; a grammar is not a scanning loop",
        ),
        'y' => (
            "it is JavaScript's `sticky` flag, which means nothing here",
            "drop the flag; every token is matched at one position anyway",
        ),
        'm' => (
            "`^` and `$` already mean the start and end of the token",
            "drop the flag",
        ),
        's' => (
            "`.` never matches a newline",
            "write `[\\s\\S]` where you meant a dot that matches newlines",
        ),
        'x' => (
            "whitespace inside a pattern is never ignored",
            "drop the flag; escape or spell out the whitespace you match",
        ),
        'U' => (
            "greediness cannot be swapped",
            "drop the flag; write the complementary class instead",
        ),
        _ => ("`i` is the only flag it honours", "drop the flag"),
    }
}

fn document(
    ast: &GrammarAst,
    rules: &[(String, Rule)],
    externals: &[Rule],
    extras: &[Rule],
    reserved: &[(String, Vec<Rule>)],
) -> Json {
    let mut entries: Vec<(String, Json)> = Vec::new();

    entries.push((
        "name".to_owned(),
        Json::str(ast.name.as_ref().map_or("", |(n, _)| n.as_str())),
    ));

    if let Some((inherits, _)) = &ast.inherits {
        entries.push(("inherits".to_owned(), Json::str(inherits)));
    }

    entries.push((
        "rules".to_owned(),
        Json::Obj(
            rules
                .iter()
                .map(|(name, rule)| (name.clone(), json_rule(rule)))
                .collect(),
        ),
    ));

    entries.push((
        "precedences".to_owned(),
        Json::Arr(
            precedence::orderings(ast)
                .into_iter()
                .map(|members| {
                    Json::Arr(
                        members
                            .iter()
                            .map(|e| match &e.kind {
                                ExprKind::Str(s) => Json::obj(vec![
                                    ("type", Json::str("STRING")),
                                    ("value", Json::str(s)),
                                ]),
                                ExprKind::Symbol(s) => Json::obj(vec![
                                    ("type", Json::str("SYMBOL")),
                                    ("name", Json::str(s)),
                                ]),
                                // The parser only ever puts these two in a
                                // precedence list.
                                _ => unreachable!("precedence entries are strings or names"),
                            })
                            .collect(),
                    )
                })
                .collect(),
        ),
    ));

    entries.push((
        "conflicts".to_owned(),
        Json::Arr(
            ast.conflicts
                .iter()
                .map(|(names, _)| Json::Arr(names.iter().map(Json::str).collect()))
                .collect(),
        ),
    ));

    entries.push((
        "externals".to_owned(),
        Json::Arr(externals.iter().map(json_rule).collect()),
    ));

    entries.push((
        "extras".to_owned(),
        Json::Arr(extras.iter().map(json_rule).collect()),
    ));

    entries.push((
        "inline".to_owned(),
        Json::Arr(ast.inline.iter().map(|(n, _)| Json::str(n)).collect()),
    ));

    entries.push((
        "supertypes".to_owned(),
        Json::Arr(ast.supertypes.iter().map(|(n, _)| Json::str(n)).collect()),
    ));

    if let Some((word, _)) = &ast.word {
        entries.push(("word".to_owned(), Json::str(word)));
    }

    entries.push((
        "reserved".to_owned(),
        Json::Obj(
            reserved
                .iter()
                .map(|(name, rules)| {
                    (
                        name.clone(),
                        Json::Arr(rules.iter().map(json_rule).collect()),
                    )
                })
                .collect(),
        ),
    ));

    Json::Obj(entries)
}

fn collect_symbols(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Symbol(name) => {
            out.insert(name.clone());
        }
        ExprKind::Seq(items) | ExprKind::Choice(items) => {
            for item in items {
                collect_symbols(item, out);
            }
        }
        ExprKind::Optional(inner) | ExprKind::Repeat(inner) | ExprKind::Repeat1(inner) => {
            collect_symbols(inner, out);
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                collect_symbols(arg, out);
            }
        }
        ExprKind::Str(_) | ExprKind::Pattern { .. } | ExprKind::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> String {
        compile(src).expect("compilation should succeed").json
    }

    fn warnings(src: &str) -> Vec<Error> {
        compile_with(
            src,
            Options {
                allow_ignored_flags: true,
            },
        )
        .expect("compilation should succeed")
        .warnings
    }

    #[test]
    fn flags_that_do_nothing_are_errors() {
        // `u` and `v` look harmless because tree-sitter calls them "unicode
        // flags", but it drops them too, so they are just as silent.
        for (src, flag) in [
            ("%name x;\na: /abc/g;", 'g'),
            ("%name x;\na: /abc/u;", 'u'),
            ("%name x;\na: /abc/v;", 'v'),
            ("%name x;\na: /abc/s;", 's'),
            ("%name x;\na: /abc/Q;", 'Q'),
            ("%name x;\n%extra /abc/m;\na: 'a';", 'm'),
            ("%name x;\na: /abc/ii;", 'i'),
        ] {
            let errors = errors(src);
            assert_eq!(errors.len(), 1, "{src}: {errors:?}");
            assert!(
                errors[0].msg.contains(&format!("`{flag}`"))
                    && (errors[0].msg.contains("has no effect")
                        || errors[0].msg.contains("is repeated")),
                "{}",
                errors[0].msg
            );
            assert!(errors[0].help.is_some(), "{src}: no help line");
        }
        // `i`, once, is the whole accepted set — and no flag at all is fine.
        assert!(compile("%name x;\na: /abc/i;\nb: /[a-z]+/;").is_ok());
    }

    #[test]
    fn a_flag_that_is_also_a_rule_name_gets_a_space_hint() {
        // The rule `g` is reachable — but only with a space, because the flag
        // characters touch the closing slash. Say that instead of leaving the
        // author to guess that `/x/g` was a sequence missing a space.
        let errs = errors("%name x;\ng: token(/g/);\na: /x/g;");
        assert_eq!(errs.len(), 1);
        let help = errs[0].help.as_deref().unwrap_or_default();
        assert!(help.contains("put a space after the literal"), "{help}");
        assert!(help.contains("`/…/ g`"), "{help}");

        // And the sequence itself compiles, so the rule is not shadowed.
        assert!(compile("%name x;\ng: token(/g/);\na: /x/ g;").is_ok());
        assert!(compile("%name x;\ng: token(/g/);\na: seq(/x/, g);").is_ok());

        // Without a rule of that name the generic advice stands.
        let generic = errors("%name x;\na: /x/g;");
        let help = generic[0].help.as_deref().unwrap_or_default();
        assert!(help.contains("grammar is not a scanning loop"), "{help}");
    }

    #[test]
    fn ignored_flags_can_be_allowed_for_byte_compatibility() {
        let rendered = compile_with(
            "%name x;\na: /abc/g;",
            Options {
                allow_ignored_flags: true,
            },
        )
        .expect("allowed flags should not fail")
        .json;
        assert!(rendered.contains("\"flags\": \"g\""), "{rendered}");
    }

    #[test]
    fn a_precedence_name_must_be_declared_as_a_name() {
        // The generator says "Undeclared precedence 'sum' in rule 'expr'" and
        // has no idea where the grammar is; here both ends are in scope.
        let err = err(r#"
            %name foo;
            %precedence sum "+";
            expr: 'a' | prec.left(sum, seq(expr, '+', expr));
            "#);
        // One mistake, one error, at the directive rather than at each of the
        // `prec` calls that trip over it.
        assert_eq!(
            err.msg,
            "`sum` is used as a precedence name, but an unquoted entry here names a rule"
        );
        let help = err.help.expect("help");
        assert!(help.contains(r#"`%precedence "sum";`"#), "{help}");
        assert!(help.contains("a bare entry gives the rule"), "{help}");
    }

    #[test]
    fn an_undeclared_precedence_name_is_reported_once_per_use() {
        let err = err(r#"
            %name foo;
            expr: 'a' | prec.left(sum, seq(expr, '+', expr));
            "#);
        assert!(err.msg.contains("never declared"), "{}", err.msg);
        assert!(err.help.expect("help").contains(r#"%precedence "sum";"#));

        // `prec.left(x)` has no value, so `x` is content rather than a name.
        let json = ok(r#"
            %name foo;
            x: 'a';
            expr: x | prec.left(x);
            "#);
        assert!(json.contains(r#""value": 0"#), "{json}");
    }

    #[test]
    fn a_bare_precedence_entry_must_name_a_rule() {
        let err = err(r#"
            %name foo;
            %precedence missing;
            expr: 'a';
            "#);
        assert!(
            err.msg.contains("`missing`, which is not defined"),
            "{}",
            err.msg
        );
    }

    /// Whitespace-free JSON, so assertions can name a structure exactly without
    /// depending on the pretty-printer.
    fn compact(src: &str) -> String {
        ok(src).chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn a_precedence_annotation_applies_to_the_whole_alternative() {
        // `@left(add)` marks the sum, not the last operand — this is the
        // difference between `prec.left(add, seq(…))` and a postfix that binds
        // to `exp`.
        let json = compact(
            r#"
            %name p;
            %precedence "add";
            exp: NUMBER | exp '+' exp @left(add);
            NUMBER: /[0-9]+/;
            "#,
        );
        assert!(
            json.contains(
                r#""type":"PREC_LEFT","value":"add","content":{"type":"SEQ","members":[{"type":"SYMBOL","name":"exp"},"#
            ),
            "{json}"
        );
    }

    #[test]
    fn a_bare_annotation_is_a_plain_rule_precedence() {
        let json = compact(
            r#"
            %name p;
            %precedence "unary";
            exp: '-' exp @unary | '(' exp ')';
            "#,
        );
        assert!(json.contains(r#""type":"PREC","value":"unary""#), "{json}");
    }

    #[test]
    fn an_empty_group_is_the_blank_rule() {
        let json = compact(
            r#"
            %name p;
            r: () | 'a';
            "#,
        );
        assert!(json.contains(r#"{"type":"BLANK"}"#), "{json}");
    }

    #[test]
    fn a_field_label_names_one_element() {
        // `k= a b` is `field('k', a) b`: the label is a prefix on the element to
        // its right, not on the branch — which is why it desugars in the parser
        // the way `@` does, and why both spellings have to emit the same JSON.
        let labelled = compact(
            r"
            %name p;
            pair: left= 'a' right= 'b';
            ",
        );
        let calls = compact(
            r"
            %name p;
            pair: field('left', 'a') field('right', 'b');
            ",
        );
        assert_eq!(labelled, calls);
        assert!(labelled.contains(r#""name":"left""#), "{labelled}");
    }

    #[test]
    fn a_field_label_in_an_alternative_names_only_that_alternative() {
        let labelled = compact(
            "%name p;
r: k= 'a' | 'b';
",
        );
        let calls = compact(
            "%name p;
r: field('k', 'a') | 'b';
",
        );
        assert_eq!(labelled, calls);
    }

    #[test]
    fn a_field_label_takes_a_postfix_element() {
        // `k= a?` labels the element, so the multiplier stays outside the field —
        // `field('k', optional(a))` written the other way round.
        let labelled = compact(
            "%name p;
r: k= 'a'?;
",
        );
        let calls = compact(
            "%name p;
r: field('k', optional('a'));
",
        );
        assert_eq!(labelled, calls);
    }

    #[test]
    fn a_field_label_can_name_a_rule_that_looks_like_a_dsl_function() {
        // The label is not a call: `field= 'x'` labels an element, and does not
        // try to invoke `field(...)`. The *rule* `field` still needs `$field`.
        let labelled = compact(
            "%name p;
r: field= 'x';
",
        );
        assert!(
            labelled.contains(r#""type":"FIELD","name":"field""#),
            "{labelled}"
        );
    }

    #[test]
    fn a_field_label_does_not_nest() {
        let error = err("%name p;\nr: k= j= 'a';\n");
        assert!(error.msg.contains("does not nest"), "{}", error.msg);
        // Grouping is the fix, and it has to be one.
        let grouped = compact("%name p;\nr: k= (j= 'a');\n");
        assert!(grouped.contains(r#""name":"j""#), "{grouped}");
    }

    #[test]
    fn an_associativity_annotation_needs_an_argument() {
        let error = err("%name p;\nr: 'a' @left;\n");
        assert!(
            error.msg.contains("needs an argument list"),
            "{}",
            error.msg
        );
        // The message must not leave the impression that a quoted name works here.
        assert!(error.msg.contains("unspellable"), "{}", error.msg);
    }

    #[test]
    fn an_annotation_needs_a_precedence() {
        let err = err("%name p;\nr: 'a' @;\n");
        assert!(
            err.msg.contains("expected a precedence after `@`"),
            "{}",
            err.msg
        );
    }

    #[test]
    fn a_rule_named_like_a_dsl_function_needs_an_explicit_reference() {
        // Ruby has a rule called `alias`, MSSQL one called `field`: the names
        // collide with DSL functions. Call syntax wins (round-tripping
        // `grammar.js` depends on it), so the rule takes `$name` — and the
        // collision is reported once, at the definition.
        let src = r#"
            %name t;
            fieldlist: $field ((',' | ';') $field)*;
            field: NAME;
            NAME: /[a-z]+/;
        "#;
        let out = compile(src).expect("compiles");
        assert!(out.json.contains("\"name\": \"field\""), "{}", out.json);
        assert!(!out.json.contains("FIELD"), "{}", out.json);
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0]
            .help
            .as_deref()
            .unwrap_or("")
            .contains("`$field`"));
    }

    #[test]
    fn a_dsl_function_still_wins_when_no_rule_shadows_it() {
        let json = ok(r#"
            %name t;
            r: field('key', NAME);
            NAME: /[a-z]+/;
            "#);
        assert!(json.contains("\"type\": \"FIELD\""), "{json}");
    }

    #[test]
    fn reports_where_the_flag_is() {
        let src = "%name flags;\na: /abc/i;\nb: /abc/iu;\nc: /abc/G;\nd: token(/[a-z]+/s);\n";
        let found = warnings(src);
        // `u` is reported as well: tree-sitter calls it a unicode flag and
        // drops it, so it is a no-op like the rest.
        let flags: Vec<_> = found
            .iter()
            .map(|err| &src[err.span.start..err.span.end])
            .collect();
        assert_eq!(flags, ["u", "G", "s"]);
        // the caret points at the flag itself, not at the literal
        assert!(found[1].msg.contains("`G`"), "{}", found[1].msg);
    }

    fn err(src: &str) -> Error {
        let mut errors = errors(src);
        assert_eq!(errors.len(), 1, "expected exactly one error");
        errors.remove(0)
    }

    // ---------------------------- precedence levels ---------------------------

    /// The point of the table: a production is marked by its rightmost
    /// terminal, so the author writes the grammar's precedence once.
    #[test]
    fn a_precedence_level_marks_every_production_ending_in_its_terminals() {
        let src = r#"
            %name t;
            %precedence right "pow" = '^';
            %precedence left "add" = '+' '-';
            %precedence left "mul" = '*' '/';
            e:
                NUM
              | e '+' e
              | e '-' e
              | e '*' e
              | e '^' e
              | 'not' e
              ;
            NUM: /[0-9]+/;
        "#;
        let json = ok(src);
        assert!(json.contains("\"type\": \"PREC_LEFT\""), "{json}");
        assert!(json.contains("\"value\": \"add\""), "{json}");
        assert!(json.contains("\"type\": \"PREC_RIGHT\""), "{json}");
        assert!(json.contains("\"value\": \"pow\""), "{json}");
        // `'not' e` ends in a non-terminal, whose rule is not a token: no mark.
        assert!(!json.contains("\"unary\""), "{json}");
        // A level is a *position in the chain*, not a separate list: the three
        // levels share one ordering, highest first.
        let precedence = precedence_lists(&json);
        let order: Vec<usize> = ["pow", "add", "mul"]
            .iter()
            .map(|level| precedence.find(&format!("\"{level}\"")).expect(level))
            .collect();
        assert!(order[0] < order[1] && order[1] < order[2], "{precedence}");
        assert_eq!(precedence.matches('[').count(), 1, "{precedence}");
    }

    /// The table and the hand-written annotation must compile to the same JSON;
    /// that equivalence is what `lua53/` is checked against.
    #[test]
    fn a_level_and_the_annotation_it_replaces_produce_the_same_json() {
        let table = ok(r#"
            %name t;
            %precedence left "add" = '+' '-';
            e: NUM | e '+' e | e '-' e;
            NUM: /[0-9]+/;
        "#);
        let annotations = ok(r#"
            %name t;
            %precedence "add";
            e: NUM | e '+' e @left(add) | e '-' e @left(add);
            NUM: /[0-9]+/;
        "#);
        assert_eq!(table, annotations);
    }

    /// An explicit annotation wins: it is how a production ending in a terminal
    /// that belongs to another level (`-x` vs `a - b`) says so.
    #[test]
    fn an_explicit_annotation_overrides_the_table() {
        let src = r#"
            %name t;
            %precedence right "unary";
            %precedence left "add" = '+';
            e: NUM | '-' e @unary | e '+' e;
            NUM: /[0-9]+/;
        "#;
        let json = ok(src);
        assert!(json.contains("\"type\": \"PREC\""), "{json}");
        assert!(json.contains("\"value\": \"unary\""), "{json}");
        assert!(json.contains("\"type\": \"PREC_LEFT\""), "{json}");
        assert!(json.contains("\"value\": \"add\""), "{json}");
    }

    /// `%precedence "a" "b";` is an ordering list (`--from-json` prints those),
    /// while `= …` or `left`/`right` makes a level. Both must stay available.
    #[test]
    fn an_ordering_list_is_not_a_level() {
        let json = ok("%name t;\n%precedence \"low\" \"high\";\na: 'x';");
        // Two names, one list, and not folded together with anything else.
        let precedence = precedence_lists(&json);
        assert!(precedence.contains("\"low\"") && precedence.contains("\"high\""));
        assert_eq!(precedence.matches('[').count(), 1, "{precedence}");
        assert!(
            !precedence.contains("],[") && !precedence.contains("], ["),
            "{precedence}"
        );
    }

    /// A level with no terminals: `= ;` is the spelling, because a bare quoted
    /// name is an ordering list (and a bare name is a rule reference).
    #[test]
    fn a_level_can_have_no_terminals() {
        let json = ok(r#"
            %name t;
            %precedence "call" = ;
            %precedence left "add" = '+';
            e: NUM | e '+' e | f;
            f: e @right(call);
            NUM: /[0-9]+/;
        "#);
        let precedence = precedence_lists(&json);
        let call = precedence.find("\"call\"").expect("call");
        let add = precedence.find("\"add\"").expect("add");
        assert!(call < add, "{precedence}");
        assert_eq!(precedence.matches('[').count(), 1, "{precedence}");
    }

    #[test]
    fn non_associativity_is_refused_with_an_explanation() {
        let errs = errors("%name t;\n%precedence nonassoc \"cmp\" = '<';\na: 'x';");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("non-associative"), "{}", errs[0].msg);
        assert!(
            errs[0].help.as_deref().unwrap_or("").contains("associativ"),
            "{:?}",
            errs[0].help
        );
    }

    #[test]
    fn a_level_name_must_be_quoted_and_its_terminals_must_exist() {
        // A bare name is a *rule* reference, so as a level name it is a mistake
        // worth naming explicitly.
        let errs = errors("%name t;\n%precedence left add = '+';\na: 'x';");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("names a rule"), "{}", errs[0].msg);
        assert!(errs[0].help.as_deref().unwrap_or("").contains("\"add\""));

        let errs = errors("%name t;\n%precedence left \"add\" = MISSING;\na: 'x';");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("not a rule"), "{}", errs[0].msg);
    }

    #[test]
    fn a_level_takes_exactly_one_name() {
        let errs = errors("%name t;\n%precedence left \"add\" \"sub\" = '+';\na: 'x';");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("exactly one name"), "{}", errs[0].msg);
    }

    // ------------------------------ nullability ------------------------------

    #[test]
    fn a_rule_that_can_match_nothing_is_reported_at_the_empty_alternative() {
        let src = "%name t;\nchunk: block?;\nblock: (stat ';'?)*;\nstat: 'x';\n";
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].msg.contains("`block` can match nothing"),
            "{}",
            errs[0].msg
        );
        // The caret is on the alternative, not on the rule's name: that is the
        // part that has to change (or move to the use site).
        assert_eq!(errs[0].span.start, src.find("(stat").unwrap());
        let help = errs[0].help.as_deref().unwrap_or_default();
        assert!(help.contains("`block?`"), "{help}");
        assert!(help.contains("used once, at 2:"), "{help}");
    }

    #[test]
    fn the_start_rule_may_be_nullable() {
        assert!(compile("%name t;\nchunk: ('x' | word)*;\nword: /[a-z]+/;").is_ok());
        // A nullable rule *used by* the start rule is still refused: the
        // exemption is for the start rule itself, not for its callees.
        let errs = errors("%name t;\nchunk: block?;\nblock: 'x'*;");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("`block`"), "{}", errs[0].msg);
    }

    #[test]
    fn a_rule_that_requires_eof_is_not_empty() {
        // `eof()` is zero-width, but it pins the production to the end of the
        // input, which is what makes such a rule legal.
        assert!(compile("%name t;\na: _eof 'x';\n_eof: eof();").is_ok());
    }

    #[test]
    fn an_unused_nullable_rule_is_not_reported() {
        // Nothing references it, so the generator never looks at it.
        assert!(compile("%name t;\na: 'x';\nspare: 'y'*;").is_ok());
    }

    /// The body of the `precedences` array: `[[…], […]]` with the outer
    /// brackets removed, so a test can count orderings without parsing JSON.
    fn precedence_lists(json: &str) -> String {
        let open =
            json.find("\"precedences\": [").expect("precedences key") + "\"precedences\": [".len();
        let mut depth = 1usize;
        for (i, c) in json[open..].char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return json[open..open + i].to_owned();
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated precedences array");
    }

    fn errors(src: &str) -> Vec<Error> {
        match compile(src) {
            Ok(compiled) => panic!("expected failure, got:\n{}", compiled.json),
            Err(errors) => errors,
        }
    }

    #[test]
    fn minimal_grammar() {
        let json = ok(r"
            %name foo;
            file: 'a';
            ");
        assert!(json.contains(r#""name": "foo""#), "{json}");
        assert!(json.contains(r#""type": "STRING""#), "{json}");
        // The default extra is applied when none is declared.
        assert!(json.contains(r#""value": "\\s""#), "{json}");
    }

    #[test]
    fn rule_order_is_preserved() {
        let json = ok(r"
            %name foo;
            first_rule: 'a';
            second_rule: 'b';
            third_rule: 'c';
            ");
        let first = json.find("first_rule").unwrap();
        let second = json.find("second_rule").unwrap();
        let third = json.find("third_rule").unwrap();
        assert!(first < second && second < third, "{json}");
    }

    #[test]
    fn postfix_lowering() {
        let json = ok(r"
            %name foo;
            file: 'a'? 'b'* 'c'+;
            ");
        assert!(json.contains(r#""type": "CHOICE""#), "{json}");
        assert!(json.contains(r#""type": "BLANK""#), "{json}");
        assert!(json.contains(r#""type": "REPEAT""#), "{json}");
        assert!(json.contains(r#""type": "REPEAT1""#), "{json}");
    }

    #[test]
    fn precedence_variants() {
        let json = ok(r#"
            %name foo;
            %precedence "named";
            _expr: 'a'
                 | prec.left(1, seq(_expr, '+', _expr))
                 | prec.right('!')
                 | prec.dynamic(2, _expr)
                 | prec("named", _expr);
            "#);
        // A bare name in `%precedence` is a rule reference, and a bare name in
        // `prec(...)` is a precedence name, so the two never meet.
        let bare = ok(r#"
            %name foo;
            %precedence _expr "+";
            _expr: 'a' | prec.left(1, seq(_expr, '+', _expr));
            "#);
        assert!(bare.contains(r#""type": "SYMBOL""#), "{bare}");
        assert!(json.contains(r#""type": "PREC_LEFT""#), "{json}");
        assert!(json.contains(r#""type": "PREC_RIGHT""#), "{json}");
        assert!(json.contains(r#""type": "PREC_DYNAMIC""#), "{json}");
        // `prec.right(_expr, '!')` with one argument defaults to precedence 0.
        assert!(json.contains("\"value\": 0"), "{json}");
        assert!(json.contains(r#""value": "named""#), "{json}");
    }

    #[test]
    fn alias_named_and_anonymous() {
        let json = ok(r"
            %name foo;
            file: alias('x', bar) alias('y', 'z');
            bar: 'b';
            ");
        assert!(json.contains(r#""named": true"#), "{json}");
        assert!(json.contains(r#""named": false"#), "{json}");
    }

    #[test]
    fn grouping_keeps_the_nested_sequence() {
        let json = ok(r"
            %name foo;
            file: ('a' 'b') 'c';
            ");
        // `dsl.js` does not flatten `seq('a', seq('b', 'c'))`, so neither do we:
        // two nested SEQs. tree-sitter's own `normalize` pass flattens later.
        assert_eq!(json.matches(r#""type": "SEQ""#).count(), 2, "{json}");
    }

    #[test]
    fn seq_call_does_not_flatten_either() {
        let json = ok(r"
            %name foo;
            file: seq('a', seq('b', 'c'));
            ");
        assert_eq!(json.matches(r#""type": "SEQ""#).count(), 2, "{json}");
    }

    #[test]
    fn reports_every_bad_rule_in_one_run() {
        let found = errors(
            r"
            %name foo;
            %conflicts a _nosuch;
            file: identifer1;
            a: 'a' unknown_a;
            b: 'b' unknown_b;
            ",
        );
        let messages: Vec<&str> = found.iter().map(|err| err.msg.as_str()).collect();
        assert_eq!(messages.len(), 4, "{messages:?}");
        // in source order: the directive first, then the rules top to bottom
        assert!(messages[0].contains("`_nosuch`"), "{messages:?}");
        assert!(messages[1].contains("`identifer1`"), "{messages:?}");
        assert!(messages[2].contains("`unknown_a`"), "{messages:?}");
        assert!(messages[3].contains("`unknown_b`"), "{messages:?}");
        assert!(found.windows(2).all(|w| w[0].span.start < w[1].span.start));
    }

    #[test]
    fn inherits_names_the_parent_grammar() {
        let json = ok(r"
            %name tsx;
            %inherits javascript;
            file: 'a';
            ");
        assert!(json.contains(r#""inherits": "javascript""#), "{json}");
        // key order: name, inherits, rules
        let name = json.find(r#""name""#).unwrap();
        let inherits = json.find(r#""inherits""#).unwrap();
        let rules = json.find(r#""rules""#).unwrap();
        assert!(name < inherits && inherits < rules, "{json}");
    }

    #[test]
    fn a_one_member_choice_is_allowed() {
        let json = ok(r"
            %name foo;
            file: choice('a');
            ");
        assert!(json.contains(r#""type": "CHOICE""#), "{json}");
    }

    #[test]
    fn a_reserved_word_set_may_be_empty() {
        let json = ok(r"
            %name foo;
            %reserved properties = ;
            file: 'a';
            ");
        assert!(json.contains(r#""properties": []"#), "{json}");
    }

    #[test]
    fn extras_none_means_no_extras_at_all() {
        let json = ok(r"
            %name foo;
            %extras none;
            file: 'a';
            ");
        assert!(json.contains(r#""extras": [],"#), "{json}");
    }

    #[test]
    fn extras_none_rejects_a_competing_extra() {
        let error = err(r"
            %name foo;
            %extras none;
            %extra 'x';
            file: 'a';
            ");
        assert!(error.msg.contains("cannot"), "{}", error.msg);
    }

    #[test]
    fn externals_are_referenceable() {
        let json = ok(r"
            %name foo;
            %external heredoc;
            file: heredoc;
            ");
        assert!(json.contains(r#""name": "heredoc""#), "{json}");
    }

    #[test]
    fn undefined_rule_is_reported_with_a_span() {
        let error = err(r"
            %name foo;
            file: identifer;
            identifier: 'a';
            ");
        assert!(
            error.msg.contains("undefined rule `identifer`"),
            "{}",
            error.msg
        );
        assert!(
            error.msg.contains("did you mean `identifier`"),
            "{}",
            error.msg
        );
        // The span must point at the misspelled name, not the whole line.
        let src = r"
            %name foo;
            file: identifer;
            identifier: 'a';
            ";
        assert_eq!(&src[error.span.start..error.span.end], "identifer");
    }

    #[test]
    fn duplicate_rule_is_reported_at_both_sites() {
        let error = err(r"
            %name foo;
            file: 'a';
            file: 'b';
            ");
        assert!(
            error.msg.contains("already defined at line 3"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn missing_semicolon_points_at_next_token() {
        // A label is `name= element`, so a line that looks like `x: y` can only be
        // a rule definition — which means a missing `;` is still an error, and the
        // message can name both readings.
        let error = err(r"
            %name foo;
            file: 'a'
            other: 'b';
            ");
        assert!(error.msg.contains("expected `;`"), "{}", error.msg);
        assert!(error.msg.contains("`name= element`"), "{}", error.msg);
        assert!(error.msg.contains("`other:`"), "{}", error.msg);
    }

    #[test]
    fn symbols_are_rejected_inside_token() {
        let error = err(r"
            %name foo;
            file: token(ident);
            ident: /x/;
            ");
        assert!(
            error.msg.contains("cannot be part of a token"),
            "{}",
            error.msg
        );
        assert!(error.msg.contains("`ident`"), "{}", error.msg);
    }

    #[test]
    fn a_tokens_block_is_token_call_sugar() {
        let call = ok(r"
            %name foo;
            file: A;
            A: token(seq('a', /b/));
            ");
        let block = ok(r"
            %name foo;
            file: A;
            tokens { A: 'a' /b/; }
            ");
        assert_eq!(call, block);
    }

    #[test]
    fn a_tokens_block_before_the_start_rule_does_not_become_the_start_rule() {
        let json = ok(r"
            %name foo;
            tokens { A: 'a'; }
            file: A;
            ");
        let file = json.find("\"file\"").expect("file rule");
        let token = json.find("\"A\"").expect("A rule");
        assert!(
            file < token,
            "the first non-token rule is the start rule:\n{json}"
        );
    }

    #[test]
    fn a_grammar_of_only_tokens_has_no_start_rule() {
        let error = err(r"
            %name foo;
            tokens { A: 'a'; }
            ");
        assert!(
            error.msg.contains("start rule outside `tokens`"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn a_tokens_block_rule_can_be_immediate() {
        let json = ok(r"
            %name foo;
            file: N;
            tokens { N: '-' /[0-9]+/ @immediate; }
            ");
        assert!(json.contains("IMMEDIATE_TOKEN"), "{json}");
    }

    #[test]
    fn a_symbol_inside_a_tokens_block_is_reported_at_the_reference() {
        let error = err(r"
            %name foo;
            file: F;
            tokens { F: part 'x'; }
            part: /p/;
            ");
        assert!(error.msg.contains("declared in `tokens`"), "{}", error.msg);
        assert!(error.msg.contains("not lexical"), "{}", error.msg);
        assert!(error.msg.contains("`part`"), "{}", error.msg);
    }

    #[test]
    fn a_zero_width_token_is_reported_at_the_empty_part() {
        let error = err(r"
            %name foo;
            file: X;
            tokens { X: ''; }
            ");
        assert!(
            error.msg.contains("can match the empty string"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn the_start_rule_is_not_a_word_token() {
        let error = err(r"
            %name foo;
            file: /x/;
            %word file;
            ");
        assert!(error.msg.contains("needs a token"), "{}", error.msg);
        assert!(error.msg.contains("nonterminal"), "{}", error.msg);
    }

    #[test]
    fn an_extra_may_be_a_nonterminal_whose_productions_start_with_tokens() {
        assert!(compile(
            r"
            %name foo;
            r: comment;
            comment: line | block;
            line: /\/\/[^\n]*/;
            block: /\/\*[^*]*\*\//;
            %extra comment;
            "
        )
        .is_ok());
    }

    #[test]
    fn an_extra_that_starts_with_a_nonterminal_names_it() {
        let error = err(r"
            %name foo;
            r: x;
            %extra x;
            x: y 'b';
            y: z;
            z: 'a';
            ");
        assert!(
            error.msg.contains("must start with a token"),
            "{}",
            error.msg
        );
        assert!(error.msg.contains("`y`"), "{}", error.msg);
    }

    #[test]
    fn an_extra_that_becomes_empty_through_its_rule_is_reported() {
        let error = err(r"
            %name foo;
            r: 'r';
            %extra x;
            x: 'a'?;
            ");
        assert!(
            error.msg.contains("must not match the empty string"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn empty_extra_is_rejected() {
        let error = err(r"
            %name foo;
            %extra '';
            file: 'a';
            ");
        assert!(
            error.msg.contains("empty string cannot be used"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn unknown_directive_lists_the_known_ones() {
        let error = err(r"
            %nam foo;
            file: 'a';
            ");
        assert!(
            error.msg.contains("unknown directive `%nam`"),
            "{}",
            error.msg
        );
    }

    #[test]
    fn diagnostics_render_a_caret() {
        let src = "%name foo;\nfile: bar;\n";
        let error = err(src);
        let rendered = crate::lexer::render_error(src, "g.tsg", &error);
        assert!(rendered.contains("--> g.tsg:2:7"), "{rendered}");
        assert!(rendered.contains("^^^"), "{rendered}");
    }
}
