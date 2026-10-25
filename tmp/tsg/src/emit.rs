//! `tsg fmt`: print a `.tsg` file back from its AST.
//!
//! The printer walks the AST rather than `grammar.json`, because the AST is the
//! only place the authoring sugar still exists: `?` stays `?`, `@left(name)`
//! stays a `prec.left` call, negative `@-1` stays negative, and `tokens { … }`
//! stays a block.  Printing from `grammar.json` would decompile the grammar and
//! reflow it into something the author never wrote.
//!
//! What it normalizes: one directive per line, one alternative per line, no
//! alignment padding, a fixed blank line between sections, minimal parentheses
//! (recomputed from the AST), and one quote style per literal.  What it does not
//! touch: the order of rules (the first non-token rule is the start rule unless
//! `%start` says otherwise), the order of alternatives, the order of precedence
//! levels, and every token body byte for byte.
//!
//! Comments live only in the source text.  The leading block is kept verbatim;
//! interior comments are counted and reported, never silently dropped.

use crate::{
    lexer::Error,
    literal::{quote, regex_literal},
    parser::{Assoc, Expr, ExprKind, GrammarAst, Parser, PrecedenceGroup},
};

pub struct Output {
    pub text: String,
    /// Comment lines that were *not* in the leading block.  The caller decides
    /// how loud to be about them; `fmt` never claims to carry them.
    pub interior_comments: usize,
}

/// Comment lines `--annotate` attaches, keyed by the thing they sit above: a
/// rule name, or a section name (`"extras"`, `"externals"`, `"tokens"`,
/// `"precedences"`).
///
/// They are written as ordinary `//` comments, indented to match whatever they
/// sit inside, so the annotated file is still a `.tsg` file that compiles to the
/// same `grammar.json` — which is the whole point, and what `--diff` checks.
#[derive(Debug, Default)]
pub struct Notes {
    rules: std::collections::BTreeMap<String, Vec<String>>,
    sections: std::collections::BTreeMap<String, Vec<String>>,
}

impl Notes {
    pub fn rule(&mut self, name: &str, lines: &[String]) {
        self.rules.entry(name.to_owned()).or_default().extend_from_slice(lines);
    }

    pub fn section(&mut self, name: &str, lines: &[String]) {
        self.sections.entry(name.to_owned()).or_default().extend_from_slice(lines);
    }

    fn for_rule(&self, name: &str) -> &[String] {
        self.rules.get(name).map_or(&[], Vec::as_slice)
    }

    fn for_section(&self, name: &str) -> &[String] {
        self.sections.get(name).map_or(&[], Vec::as_slice)
    }
}

/// Re-print `source` in canonical form.
pub fn fmt(source: &str) -> Result<Output, Error> {
    let ast = Parser::new(source)?.parse()?;
    Ok(printed(source, &ast, None))
}

/// Re-print `source` with `notes` above the rules and sections they belong to.
pub fn annotate(source: &str, ast: &GrammarAst, notes: &Notes) -> Output {
    printed(source, ast, Some(notes))
}

fn printed(source: &str, ast: &GrammarAst, notes: Option<&Notes>) -> Output {
    let (header, interior_comments) = comments(source);
    let mut out = String::new();
    if !header.is_empty() {
        out.push_str(&header);
        out.push('\n');
    }
    let body = body(ast, notes);
    out.push_str(&body);
    Output {
        text: out,
        interior_comments,
    }
}

/// The leading comment block, verbatim, and the number of comment lines after it.
fn comments(source: &str) -> (String, usize) {
    let mut header = String::new();
    let mut in_header = true;
    let mut interior = 0;
    for line in source.lines() {
        let trimmed = line.trim();
        if in_header {
            if trimmed.is_empty() || trimmed.starts_with("//") {
                header.push_str(line);
                header.push('\n');
                continue;
            }
            in_header = false;
        }
        if trimmed.starts_with("//") {
            interior += 1;
        }
    }
    let header = header.trim_end().to_owned();
    (header, interior)
}

fn body(ast: &GrammarAst, notes: Option<&Notes>) -> String {
    let mut sections: Vec<String> = Vec::new();
    let mut directives: Vec<String> = Vec::new();

    if let Some((name, _)) = &ast.name {
        directives.push(format!("%name {name};"));
    }
    if let Some((name, _)) = &ast.start {
        directives.push(format!("%start {name};"));
    }
    if let Some((name, _)) = &ast.inherits {
        directives.push(format!("%inherits {name};"));
    }
    if let Some((name, _)) = &ast.word {
        directives.push(format!("%word {name};"));
    }
    if !directives.is_empty() {
        sections.push(directives.join("\n"));
    }

    // Token rules, by name: they print as one block, at the position of their
    // first member.
    let token_names: Vec<&str> = ast.tokens.iter().map(|rule| rule.name.as_str()).collect();

    let mut extras: Vec<String> = Vec::new();
    if ast.no_extras {
        extras.push("%extras none;".to_owned());
    }
    for extra in &ast.extras {
        extras.push(format!("%extra {};", expr(extra, 0)));
    }
    if !extras.is_empty() {
        push_section(&mut sections, notes, "extras", extras.join("\n"));
    }

    let mut optionals: Vec<String> = Vec::new();
    for external in &ast.externals {
        let name = match &external.kind {
            ExprKind::Symbol(name) => Some(name.as_str()),
            _ => None,
        };
        let scanner = name.and_then(|name| {
            ast.scanners
                .iter()
                .find(|scanner| scanner.name == name)
        });
        match scanner {
            Some(scanner) => optionals.push(format!(
                "%external {} = delimited({}, {});",
                expr(external, 0),
                quote(&scanner.open.to_string()),
                quote(&scanner.close.to_string())
            )),
            None => optionals.push(format!("%external {};", expr(external, 0))),
        }
    }
    if !optionals.is_empty() {
        push_section(&mut sections, notes, "externals", optionals.join("\n"));
    }
    if !ast.inline.is_empty() {
        let names: Vec<&str> = ast.inline.iter().map(|(name, _)| name.as_str()).collect();
        sections.push(format!("%inline {};", names.join(" ")));
    }
    if !ast.supertypes.is_empty() {
        let names: Vec<&str> = ast
            .supertypes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        sections.push(format!("%supertypes {};", names.join(" ")));
    }
    if !ast.conflicts.is_empty() {
        let lines: Vec<String> = ast
            .conflicts
            .iter()
            .map(|(names, _)| format!("%conflicts {};", names.join(" ")))
            .collect();
        sections.push(lines.join("\n"));
    }
    if !ast.precedences.is_empty() {
        let lines: Vec<String> = ast.precedences.iter().map(precedence).collect();
        push_section(&mut sections, notes, "precedences", lines.join("\n"));
    }
    if !ast.reserved.is_empty() {
        let lines: Vec<String> = ast
            .reserved
            .iter()
            .map(|(name, words, _)| {
                if words.is_empty() {
                    format!("%reserved {name} = ;")
                } else {
                    let words: Vec<String> = words.iter().map(|word| expr(word, 0)).collect();
                    format!("%reserved {name} = {};", words.join(", "))
                }
            })
            .collect();
        sections.push(lines.join("\n"));
    }

    // The `tokens { … }` block keeps its block *and its place*: token rules are
    // lexer priority, and the position of the block among the rules is what
    // decides the start rule until the first non-token is promoted.  Emit the
    // block where its first member sat.
    let mut emitted_tokens = false;
    for (name, rule) in &ast.rules {
        if token_names.contains(&name.as_str()) {
            if emitted_tokens {
                continue;
            }
            emitted_tokens = true;
            let block = tokens_block(ast, &token_names, notes);
            push_section(&mut sections, notes, "tokens", block);
            continue;
        }
        push_rule(&mut sections, notes, name, rule_text(name, rule));
    }

    let mut out = String::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(section);
        out.push('\n');
    }
    out
}

/// The `tokens { … }` block, in file order.
fn tokens_block(ast: &GrammarAst, token_names: &[&str], notes: Option<&Notes>) -> String {
    let mut block = String::from("tokens {\n");
    for (name, rule) in &ast.rules {
        if !token_names.contains(&name.as_str()) {
            continue;
        }
        comment(&mut block, notes.map(|notes| notes.for_rule(name)), "    ");
        let immediate = ast
            .tokens
            .iter()
            .any(|token| &token.name == name && token.immediate);
        let suffix = if immediate { " @immediate" } else { "" };
        // The parser wraps a token body in `token(…)`; printing the wrapper back
        // would nest a second one, which changes `TOKEN` into `TOKEN(TOKEN(…))`.
        let body = unwrap_token(rule);
        block.push_str(&format!("  {name}: {}{suffix};\n", expr(body, 0)));
    }
    block.push('}');
    block
}

/// A section, with the notes that belong above it.
fn push_section(sections: &mut Vec<String>, notes: Option<&Notes>, key: &str, text: String) {
    let mut block = String::new();
    comment(&mut block, notes.map(|notes| notes.for_section(key)), "");
    block.push_str(&text);
    sections.push(block);
}

/// A rule, with the notes that belong above it.
fn push_rule(sections: &mut Vec<String>, notes: Option<&Notes>, name: &str, text: String) {
    let mut block = String::new();
    comment(&mut block, notes.map(|notes| notes.for_rule(name)), "");
    block.push_str(&text);
    sections.push(block);
}

/// Write `lines` as `//` comments, `indent` first.  Nothing happens for an empty
/// slice, so every caller can pass `None`/`&[]` without a special case.
fn comment(out: &mut String, lines: Option<&[String]>, indent: &str) {
    for line in lines.unwrap_or(&[]) {
        out.push_str(indent);
        out.push_str("// ");
        out.push_str(line);
        out.push('\n');
    }
}

fn unwrap_token(rule: &Expr) -> &Expr {
    match &rule.kind {
        ExprKind::Call { name, args }
            if (name == "token" || name == "token.immediate") && args.len() == 1 =>
        {
            &args[0]
        }
        _ => rule,
    }
}

fn precedence(group: &PrecedenceGroup) -> String {
    let entries: Vec<String> = group.entries.iter().map(|entry| expr(entry, 0)).collect();
    let entries = entries.join(" ");
    if !group.is_level() {
        return format!("%precedence {entries};");
    }
    let assoc = match group.assoc {
        Some(Assoc::Left) => "left ",
        Some(Assoc::Right) => "right ",
        None => "",
    };
    let terminals: Vec<String> = group
        .terminals
        .iter()
        .map(|terminal| expr(terminal, 0))
        .collect();
    format!("%precedence {assoc}{entries} = {};", terminals.join(" "))
}

fn rule_text(name: &str, rule: &Expr) -> String {
    // One alternative per line, `  |` on the rest, the last one carrying `;`:
    // the layout the existing grammars use, so a diff of a reformatted file is
    // about content and not about layout.
    let mut alternatives: Vec<String> = Vec::new();
    match &rule.kind {
        ExprKind::Choice(members) => {
            for member in members {
                // `2`, not `0`: an alternative that is *itself* a choice
                // (`a: (x | y) | z;`) has to keep its parentheses, or printing
                // it would flatten `choice(choice(x, y), z)` into
                // `choice(x, y, z)` — a different `grammar.json`, and the one
                // thing `--fmt` promises it will not do.
                alternatives.push(expr(member, 2));
            }
        }
        _ => alternatives.push(expr(rule, 2)),
    }
    let mut out = format!("{name}:\n");
    for (index, alternative) in alternatives.iter().enumerate() {
        if index == 0 {
            out.push_str(&format!("    {alternative}"));
        } else {
            out.push_str(&format!("\n  | {alternative}"));
        }
    }
    out.push(';');
    out
}

/// Precedence of a node in the surface grammar: `|` binds loosest, then
/// concatenation, then postfix `?`/`*`/`+`, then atoms.
fn level(kind: &ExprKind) -> u8 {
    match kind {
        ExprKind::Choice(_) => 1,
        ExprKind::Seq(_) => 2,
        _ => 3,
    }
}

/// Print `node`, parenthesised if it binds looser than `minimum`.
fn expr(node: &Expr, minimum: u8) -> String {
    let text = match &node.kind {
        ExprKind::Str(value) => quote(value),
        ExprKind::Pattern { value, flags } => regex_literal(value, flags),
        ExprKind::Number(number) => number.to_string(),
        ExprKind::Symbol(name) => {
            // A rule may be called like a DSL function: `field` is both a call
            // and a plausible rule name, and a bare `field (…)` reads as the
            // call.  `$name` is the explicit reference form, and the parser
            // accepts it everywhere.
            if call_names().contains(&name.as_str()) {
                format!("${name}")
            } else {
                name.clone()
            }
        }
        ExprKind::Seq(members) => members
            .iter()
            .map(|member| expr(member, 3))
            .collect::<Vec<String>>()
            .join(" "),
        ExprKind::Choice(members) => members
            .iter()
            .map(|member| expr(member, 2))
            .collect::<Vec<String>>()
            .join(" | "),
        ExprKind::Optional(inner) => format!("{}?", expr(inner, 3)),
        ExprKind::Repeat(inner) => format!("{}*", expr(inner, 3)),
        ExprKind::Repeat1(inner) => format!("{}+", expr(inner, 3)),
        ExprKind::Call { name, args } => {
            let args: Vec<String> = args.iter().map(|arg| expr(arg, 0)).collect();
            format!("{name}({})", args.join(", "))
        }
    };
    if level(&node.kind) < minimum {
        format!("({text})")
    } else {
        text
    }
}

/// Names the parser reads as a function call.  A rule reference with one of
/// these names must be printed as `$name`.
fn call_names() -> [&'static str; 15] {
    [
        "seq",
        "choice",
        "optional",
        "repeat",
        "repeat1",
        "blank",
        "eof",
        "prec",
        "prec.left",
        "prec.right",
        "prec.dynamic",
        "token",
        "token.immediate",
        "alias",
        "field",
    ]
}

