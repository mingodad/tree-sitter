//! The translation decisions, and the `.tsg` writer.
//!
//! Nothing in this file touches tree-sitter: it takes the [`Grammar`] that
//! `cst::read` produced and applies the rules that a `.g` → `.tsg` port needs.
//! They are separated from the reading because they are *policy* — the parts a
//! human has to agree with — while reading a file is mechanism.

use std::collections::{HashMap, HashSet};

use crate::{
    flex,
    ir::{Alt, Assoc, Atom, Directive, Grammar, Rule},
};

/// Everything that is specific to one source grammar; the tool itself is
/// generic, so these are options rather than constants.
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// `%name`, i.e. the grammar's name inside tree-sitter.
    pub name: &'a str,
    /// First line of the generated file.
    pub title: &'a str,
    /// Source path, for the provenance line.
    pub source: &'a str,
    /// Extra comment lines copied into the header verbatim.
    pub header: &'a [String],
    /// `%conflicts` lines, in the order written.
    pub conflicts: &'a [String],
    /// `NAME pattern`: token bodies hand-translated from Flex, because the
    /// pattern needs semantics Flex syntax cannot express.
    pub bodies: &'a [(String, String)],
}

pub struct Translation {
    pub text: String,
    pub rules: usize,
    pub tokens: usize,
    pub levels: usize,
}

pub fn translate(grammar: &Grammar, options: &Options) -> Result<Translation, String> {
    for note in &grammar.lexer.notes {
        eprintln!("note: {note}");
    }
    let mut tokens = Vec::new();
    let mut literals = Vec::new();
    let mut levels = Vec::new();
    let mut fallback: Vec<(String, Vec<String>)> = Vec::new();
    let mut start = None;

    for directive in &grammar.directives {
        match directive {
            Directive::Token(symbols) => {
                for symbol in symbols {
                    if is_literal(symbol) {
                        literals.push(symbol.clone());
                    } else {
                        tokens.push(symbol.clone());
                    }
                }
            }
            Directive::Assoc { assoc, terms } => levels.push((*assoc, terms.clone())),
            Directive::Start(name) => start = Some(name.clone()),
            Directive::Fallback { target, tokens: specific } => {
                fallback.push((target.clone(), specific.clone()))
            }
            Directive::Unsupported(keyword) => {
                return Err(format!("`{keyword}` is not translated yet"));
            }
        }
    }
    // Bison's default start symbol is the first rule.
    let declared_start = start.is_some();
    let start = start
        .or_else(|| grammar.rules.first().map(|rule| rule.lhs.clone()))
        .ok_or("the source has no rules")?;

    // --- the rules -----------------------------------------------------------

    let mut order: Vec<String> = Vec::new();
    let mut rules: HashMap<String, Vec<Alt>> = HashMap::new();
    for rule in &grammar.rules {
        // A second definition of the same LHS appends alternatives, the way
        // yacc's descendants read it; bison itself would reject the file.
        match rules.get_mut(&rule.lhs) {
            Some(alts) => alts.extend(rule.alts.iter().cloned()),
            None => {
                rules.insert(rule.lhs.clone(), rule.alts.clone());
                order.push(rule.lhs.clone());
            }
        }
    }

    // --- the lexer: one regex body per token ---------------------------------

    let mut level_of = HashMap::new();
    for (number, (_, terms)) in levels.iter().enumerate() {
        for term in terms {
            level_of.insert(term.clone(), number + 1);
        }
    }

    // A `%prec` symbol is often declared as a token that nothing ever lexes
    // (`%token UMINUS` with no unary minus in the language). Only the tokens a
    // rule body can contain get a lexer body.
    let used = used_symbols(&grammar.rules);
    tokens.retain(|name| used.contains(name.as_str()));
    // Bison also reads a name in a rule body as an implicitly declared token,
    // which is how a keyword declared only by a `%nonassoc` line or by a lexer
    // rule becomes a terminal.
    for name in rule_symbols(&rules, &order) {
        // A literal is not a named token: tree-sitter reads it from the rule
        // body.
        if is_literal(&name) {
            continue;
        }
        if !rules.contains_key(name.as_str()) && !tokens.contains(&name) {
            tokens.push(name.to_string());
        }
    }
    let declared: HashSet<String> = tokens.iter().cloned().collect();

    // A bare-word pattern with a single token as its action is a keyword rule.
    let mut spell: Vec<(&str, &str)> = Vec::new();
    for (pattern, action) in &grammar.lexer.rules {
        if flex::is_bare_word(pattern) && declared.contains(action.as_str()) {
            // Overwriting keeps the position, like a Python dict, so the
            // lookahead search below is order-deterministic.
            match spell.iter_mut().find(|(token, _)| *token == action) {
                Some(entry) => entry.1 = pattern,
                None => spell.push((action, pattern)),
            }
        }
    }
    let spell_of = |name: &str| -> Option<&str> {
        spell
            .iter()
            .find(|(token, _)| *token == name)
            .map(|(_, spelling)| *spelling)
    };

    // `%x` start conditions are lookahead. `WITH_LA` is lexed from the same
    // word as `WITH`, so the lookahead terminal is its base token; the rule
    // that produced it only exists to pick the parser state.
    let mut la_of: HashMap<String, String> = HashMap::new();
    for name in &tokens {
        let Some(word) = name.strip_suffix("_LA") else {
            continue;
        };
        let Some((base, _)) = spell.iter().find(|(_, spelling)| *spelling == word) else {
            return Err(format!(
                "lookahead token `{name}` has no base token (nothing lexes `{word}`)"
            ));
        };
        la_of.insert(name.clone(), (*base).to_string());
    }

    // Any other rule whose action is a token gives that token its pattern.
    // Several Flex rules can name the same token — `{DIGIT}+(\.{DIGIT}+)?`,
    // `{DIGIT}+\.` and `\.{DIGIT}+` are all `NUMBER` — because Flex picks the
    // longest match among *all* the rules, and ties go to the earlier one. The
    // lexer returns the same token whichever one wins, so the patterns are one
    // body, in source order.
    let mut overrides: Vec<(&str, String)> = Vec::new();
    for (pattern, action) in &grammar.lexer.rules {
        if !declared.contains(action.as_str()) || spell_of(action).is_some() {
            continue;
        }
        let body = flex::expand(&grammar.lexer, pattern, 0)?;
        match overrides.iter_mut().find(|(token, _)| *token == action) {
            Some((_, merged)) => {
                merged.push('|');
                merged.push_str(&body);
            }
            None => overrides.push((action, body)),
        }
    }

    // A balanced region is a token, not a body: the scanner returns it. Its
    // name has to be a token even when no `%token` line mentions it, because the
    // Flex rule that returns it is the only declaration there is.
    let pushdown = grammar.lexer.pushdown.clone();
    for region in &pushdown {
        if !declared.contains(&region.token) {
            tokens.push(region.token.clone());
        }
    }

    // `%fallback FALLBACK tok…`: in a state that cannot use `tok`, the scanner
    // returns `FALLBACK`. tree-sitter gets that for free — its lexer only
    // produces tokens the state accepts — but only if `tok` comes first: an
    // earlier token wins a lexer tie, and the specific token has to win where
    // it is the valid one.
    for (target, specific) in &fallback {
        for name in specific {
            let (Some(from), Some(to)) = (
                tokens.iter().position(|token| token == name),
                tokens.iter().position(|token| token == target),
            ) else {
                continue;
            };
            if from > to {
                let token = tokens.remove(from);
                tokens.insert(to, token);
            }
        }
    }
    let bodies_for = |tokens: &[String]| -> Result<Vec<(String, String)>, String> {
        let mut bodies = Vec::new();
        for name in tokens {
            // A hand-translated body wins: translating the Flex pattern is the
            // common case, not the rule.
            let body = if let Some(spelling) = spell_of(name) {
                // `%option caseless` is what makes `/ABORT/i` right.
                let flag = if grammar.lexer.caseless { "i" } else { "" };
                format!("/{}/{flag}", flex::escape_literal(spelling))
            } else if let Some((_, body)) = options.bodies.iter().find(|(token, _)| token == name) {
                format!("/{body}/")
            } else if let Some((_, pattern)) = overrides.iter().find(|(token, _)| token == name) {
                format!("/{}/", flex::to_regex(pattern)?)
            } else {
                // No body: either a lookahead token, which is its base token
                // and never appears in the output, or a token the lexer never
                // returns, which makes every production it starts dead.
                continue;
            };
            bodies.push((name.clone(), body));
        }
        Ok(bodies)
    };

    // --- dead productions ----------------------------------------------------

    // A token the lexer never returns (`%token MOD_OP` next to a Flex rule that
    // returns nothing) cannot start a sentence, so every production that needs
    // it is dead. tree-sitter wants a lexer body for each token in the
    // grammar, so the dead code has to go — which can kill another rule, which
    // is why this is a fixpoint.
    let mut unlexable: Vec<String> = Vec::new();
    let mut dead_alternatives = 0;
    let bodies = loop {
        // An alternative that needed only unlexable tokens is now empty, which
        // would read as an empty production; and a rule left with no
        // alternative at all is unproductive, so mentioning it in another rule
        // is dead code too. Drop both, then ask again.
        let unproductive: Vec<String> = order
            .iter()
            .filter(|lhs| rules[*lhs].is_empty())
            .cloned()
            .collect();
        if !unproductive.is_empty() {
            for name in &unproductive {
                for alts in rules.values_mut() {
                    let before = alts.len();
                    alts.retain(|alt| !alt.atoms.iter().any(|atom| atom.name() == name));
                    dead_alternatives += before - alts.len();
                }
                rules.remove(name);
            }
            order.retain(|lhs| !unproductive.contains(lhs));
            continue;
        }

        let bodies = bodies_for(&tokens)?;
        let dead: Vec<String> = tokens
            .iter()
            .filter(|name| !la_of.contains_key(*name))
            // An external token has no body: the scanner returns it.
            .filter(|name| !pushdown.iter().any(|region| region.token == **name))
            .filter(|name| !bodies.iter().any(|(token, _)| token == *name))
            .cloned()
            .collect();
        if dead.is_empty() {
            break bodies;
        }
        for name in &dead {
            for alts in rules.values_mut() {
                let before = alts.len();
                alts.retain(|alt| !alt.atoms.iter().any(|atom| atom.name() == name));
                dead_alternatives += before - alts.len();
            }
            unlexable.push(name.clone());
        }
        tokens.retain(|name| !dead.contains(name));
    };

    if !rules.contains_key(&start) {
        let names: Vec<&str> = unlexable.iter().map(String::as_str).collect();
        return Err(format!(
            "every production of `{start}` — the start rule — needs {} the lexer never \
             returns; a start condition (`%x`, `<state>`) or a C action is not translated",
            few(&names),
        ));
    }

    if !unlexable.is_empty() {
        let names: Vec<&str> = unlexable.iter().map(String::as_str).collect();
        eprintln!(
            "note: the lexer never returns {}; dropped {} production(s) that need {}",
            few(&names),
            dead_alternatives,
            if names.len() == 1 { "it" } else { "them" },
        );
    }

    let token_set: HashSet<&str> = tokens.iter().map(String::as_str).collect();
    let body_of = |name: &str| -> Option<&str> {
        bodies
            .iter()
            .find(|(token, _)| token == name)
            .map(|(_, body)| body.as_str())
    };

    // An empty production is part of the language, never an encoding problem:
    // the generator rejects only zero-consumption cycles. Written as EBNF the
    // empty case needs no `()` and no companion rule, so every nullable rule
    // takes its natural shape — `(item)*` for a list, `x?` for an optional.
    let (rules, order) = simplify(rules, order, &start);
    // Reported after the rewrite: what is left is what the policy did *not*
    // express, and that is the list worth reading.
    if std::env::var_os("G2TSG_REPORT").is_some() {
        report_nullable(&rules, &order, &start);
    }

    // --- emit ----------------------------------------------------------------

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("// {}", options.title));
    lines.push(format!(
        "// Translated by g2tsg from {} (read with bison_flex/grammar.tsg).",
        options.source
    ));
    for line in options.header {
        lines.push(if line.is_empty() {
            "//".to_string()
        } else {
            format!("// {line}")
        });
    }
    lines.push(String::new());
    lines.push(format!("%name {};", options.name));
    if declared_start {
        // Keep the declaration: it is the difference between "this rule starts
        // the tree" and "this rule happens to come first".
        lines.push(format!("%start {start};"));
    }
    lines.push(String::new());
    for region in &pushdown {
        lines.push(format!(
            "// Flex start condition `{}`: one token for the whole region.",
            region.token
        ));
        lines.push(format!(
            "%external {} = delimited('{}', '{}');",
            region.token,
            escape_character(region.open),
            escape_character(region.close)
        ));
    }
    if !pushdown.is_empty() {
        lines.push(String::new());
    }
    lines.push("tokens {".to_string());

    // Keywords first: tree-sitter breaks a lexer tie with the earlier token, so
    // a word must be offered before `IDENT` that also matches it.
    let keyword_order: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|name| spell_of(name).is_some() && !la_of.contains_key(*name))
        .collect();
    let other_order: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|name| spell_of(name).is_none() && !la_of.contains_key(*name))
        .collect();
    let mut emitted = 0;
    for name in keyword_order.iter().chain(other_order.iter()) {
        // `IDENT` goes last: on a tie with a keyword the keyword has to win.
        if *name == "IDENT" {
            continue;
        }
        if let Some(body) = body_of(name) {
            lines.push(format!("  {name}: {body};"));
            emitted += 1;
        }
    }
    if let Some(body) = body_of("IDENT") {
        lines.push(format!("  IDENT: {body};"));
        emitted += 1;
    }
    lines.push("}".to_string());
    lines.push(String::new());
    // Separators: Flex discards what a rule with `skip()` (or an empty action, or
    // a bare `;`) matches.  Deriving them from the lexer replaces the three
    // hardcoded lines this used to emit — whitespace, `--` and `/* */` — which
    // silently failed to skip every other comment syntax, and which lose to a
    // token that is a prefix of them (`/` beats `/* */` at equal length).
    let mut extras: Vec<String> = Vec::new();
    for (pattern, action) in &grammar.lexer.rules {
        let action = action.trim();
        let skips = action.is_empty()
            || action == ";"
            || action == "skip()"
            || action == "skip();"
            || (action.starts_with("/*") && action.ends_with("*/"))
            || action.starts_with("//");
        if !skips {
            continue;
        }
        let body = flex::to_regex(&flex::expand(&grammar.lexer, pattern, 0)?)?;
        if !body.is_empty() && !extras.contains(&body) {
            extras.push(body);
        }
    }
    if extras.is_empty() {
        // No lexer rules to ask (a `.g` with only the grammar half): whitespace is
        // what every grammar wants, and it is a better guess than a comment syntax.
        extras.push("[ \\t\\n\\r\\f]+".to_string());
    }
    for body in &extras {
        lines.push(format!("%extra /{body}/;"));
    }
    lines.push(String::new());

    // Bison numbers levels from 1 (lowest) upwards; tsg wants one descending
    // chain. A terminal the translation dropped (a lookahead, `UMINUS`) leaves
    // its level behind, so the chain keeps its shape and `%prec` can name it.
    lines.push(
        "// Bison lists precedence lowest first; tsg wants the descending chain.".to_string(),
    );
    lines.push(
        "// `nonassoc` becomes a level with no associativity (see the notes above).".to_string(),
    );
    for number in (1..=levels.len()).rev() {
        let (assoc, terms) = &levels[number - 1];
        let terms: Vec<&str> = terms
            .iter()
            .map(String::as_str)
            .filter(|term| term.starts_with('\'') || token_set.contains(term))
            .filter(|term| !la_of.contains_key(*term))
            .collect();
        let assoc_part = match assoc {
            Assoc::Left => "left ",
            Assoc::Right => "right ",
            Assoc::None | Assoc::Nonassoc => "",
        };
        if terms.is_empty() {
            // A level that inherits no terminal still holds a place in the
            // chain. With an associativity the directive already says it is a
            // level; without one, `= ;` does — `%precedence "p11";` would be a
            // bare *ordering list*, which splits the chain in two and leaves
            // `@p11` incomparable with the levels around it.
            let equals = if assoc_part.is_empty() { " = " } else { "" };
            lines.push(format!("%precedence {assoc_part}\"p{number}\"{equals};"));
        } else {
            lines.push(format!(
                "%precedence {assoc_part}\"p{number}\" = {};",
                terms.join(" ")
            ));
        }
    }
    lines.push(String::new());

    // tree-sitter resolves a conflict by precedence only when the rules have
    // names; otherwise it wants an explicit `%conflicts`. The source grammar
    // needs none of these, so every entry is an artifact of the translation —
    // mostly the `x?` distribution that replacing Bison's empty alternatives
    // forces, plus dropping the lookahead tokens.
    for line in options.conflicts {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            lines.push(format!("//{}", comment));
        } else {
            lines.push(format!(
                "%conflicts {};",
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
    }
    lines.push(String::new());
    if declared_start {
        lines.push("// Bison named the start symbol with `%start`.".to_string());
    } else {
        lines.push("// Bison named no start symbol, so its default holds: the first rule.".to_string());
    }
    lines.push("// The start rule is written first.".to_string());
    lines.push(String::new());

    let emit_order = std::iter::once(&start).chain(order.iter().filter(|lhs| **lhs != start));
    for lhs in emit_order {
        let Some(alts) = rules.get(lhs) else {
            continue;
        };
        let mut rendered: Vec<String> = Vec::new();
        for alt in alts {
            let body = render_body(&alt.atoms, &la_of);
            let text = format!("{body}{}", mark(alt, &levels, &level_of, &token_set)?);
            let text = text.trim().to_string();
            // An alternative can become a duplicate of another after the
            // rewrites; tree-sitter would reject the ambiguity.
            if !rendered.contains(&text) {
                rendered.push(text);
            }
        }
        lines.push(format!("{lhs}:"));
        for (index, text) in rendered.iter().enumerate() {
            let prefix = if index == 0 { "    " } else { "  | " };
            let suffix = if index == rendered.len() - 1 { ";" } else { "" };
            lines.push(format!("{prefix}{text}{suffix}"));
        }
        lines.push(String::new());
    }

    let text = lines.join("\n").trim_end().to_string() + "\n";
    Ok(Translation {
        text,
        rules: order.len(),
        tokens: emitted,
        levels: levels.len(),
    })
}

/// The `@` annotation for an alternative: `%prec` if the source wrote one.
///
/// The rightmost terminal's level is *implicit* — tsg's `%precedence` table
/// supplies it — so only an explicit `%prec` produces an annotation.
fn mark(
    alt: &Alt,
    levels: &[(Assoc, Vec<String>)],
    level_of: &HashMap<String, usize>,
    tokens: &HashSet<&str>,
) -> Result<String, String> {
    let Some(prec) = &alt.prec else {
        return Ok(String::new());
    };
    let Some(number) = level_of.get(prec).copied() else {
        // Bison gives a token that no `%left`/`%right`/`%precedence` line
        // mentions the precedence `none`, which is also what an alternative
        // with no annotation gets — so the annotation is a no-op, not a loss.
        if tokens.contains(prec.as_str()) {
            return Ok(String::new());
        }
        return Err(format!("`%prec {prec}` names no token"));
    };
    Ok(match levels[number - 1].0 {
        Assoc::Nonassoc | Assoc::None => format!(" @p{number}"),
        Assoc::Left => format!(" @left(p{number})"),
        Assoc::Right => format!(" @right(p{number})"),
    })
}

/// A production body as `.tsg` text: its atoms, space separated, with a
/// repeated group written `(a b | c)*`. An empty body is the blank production,
/// `()`.
fn render_choice(alts: &[Alt], la_of: &HashMap<String, String>) -> String {
    alts.iter()
        .map(|alt| render_body(&alt.atoms, la_of))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn render_body(atoms: &[Atom], la_of: &HashMap<String, String>) -> String {
    if atoms.is_empty() {
        return "()".to_owned();
    }
    atoms
        .iter()
        .map(|atom| match atom {
            // A lookahead terminal is its base token.
            Atom::Opt(name) => format!("{}?", lookahead_name(name, la_of)),
            Atom::Maybe(items) => match items.as_slice() {
                [Alt { atoms, prec: None }] if atoms.len() == 1 => {
                    format!("{}?", render_body(atoms, la_of))
                }
                _ => {
                    let alternatives: Vec<String> = items
                        .iter()
                        .map(|alt| render_body(&alt.atoms, la_of))
                        .collect();
                    format!("({})?", alternatives.join(" | "))
                }
            },
            // `(x)*` is `x*`: parentheses only earn their place when the group
            // is more than one atom.
            Atom::Repeat(items) => match items.as_slice() {
                [Alt { atoms, prec: None }] if atoms.len() == 1 => {
                    format!("{}*", render_body(atoms, la_of))
                }
                _ => format!("({})*", render_choice(items, la_of)),
            },
            Atom::Plus(items) => match items.as_slice() {
                [Alt { atoms, prec: None }] if atoms.len() == 1 => {
                    format!("{}+", render_body(atoms, la_of))
                }
                _ => format!("({})+", render_choice(items, la_of)),
            },
            Atom::Group(items) => format!("({})", render_choice(items, la_of)),
            Atom::Field(name, atoms) => match atoms.as_slice() {
                [Atom::Opt(inner)] => format!("{name}= {}?", lookahead_name(inner, la_of)),
                [one] => format!("{name}= {}", render_body(std::slice::from_ref(one), la_of)),
                several => format!("{name}= ({})", render_body(several, la_of)),
            },
            Atom::Plain(name) => lookahead_name(name, la_of).to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn lookahead_name<'a>(name: &'a str, la_of: &'a HashMap<String, String>) -> &'a str {
    la_of.get(name).map(String::as_str).unwrap_or(name)
}

/// Up to three names, then "and N more" — an error message that lists forty
/// tokens tells the reader less than one that says there are forty.
/// A delimiter, as a `'…'` literal in the emitted `.tsg`. Keep it to the escapes
/// tsg's own lexer understands; anything else would be preserved verbatim and
/// then fail to lex.
fn escape_character(character: char) -> String {
    match character {
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        '\0' => "\\0".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\u{b}' => "\\v".to_string(),
        '\\' | '\'' => format!("\\{character}"),
        other => other.to_string(),
    }
}

fn few(names: &[&str]) -> String {
    let shown = names
        .iter()
        .take(3)
        .copied()
        .collect::<Vec<&str>>()
        .join(", ");
    match names.len() {
        0 => "nothing".to_string(),
        1..=3 => shown,
        n => format!("{shown} and {} more", n - 3),
    }
}

/// A Bison literal terminal, either `'x'` or `"xyz"`.
fn is_literal(name: &str) -> bool {
    name.starts_with('\'') || name.starts_with('"')
}

/// The symbols the rule bodies mention, in source order, so an implicitly
/// declared token is appended where a reader expects it.
fn rule_symbols(rules: &HashMap<String, Vec<Alt>>, order: &[String]) -> Vec<String> {
    let mut symbols: Vec<String> = Vec::new();
    for lhs in order {
        for alt in &rules[lhs] {
            for atom in &alt.atoms {
                let name = atom.name();
                if !symbols.iter().any(|seen| seen == name) {
                    symbols.push(name.to_string());
                }
            }
        }
    }
    symbols
}

/// Every symbol a rule body mentions, which is what a token has to be lexable
/// from.
fn used_symbols(rules: &[Rule]) -> HashSet<&str> {
    let mut used = HashSet::new();
    for rule in rules {
        for alt in &rule.alts {
            for atom in &alt.atoms {
                used.insert(atom.name());
            }
        }
    }
    used
}

/// Whether an alternative can match nothing: every atom is absent or nullable.
/// The blank alternative is `()` — no atoms at all — so it is nullable too.
fn is_nullable_alt(alt: &Alt, nullable: &HashSet<String>) -> bool {
    alt.atoms.iter().all(|atom| match atom {
        Atom::Opt(_) => true,
        Atom::Plain(name) => !name.starts_with('\'') && nullable.contains(name),
        Atom::Repeat(_) | Atom::Plus(_) => false,
        Atom::Group(alts) => alts.iter().any(|alt| is_nullable_alt(alt, nullable)),
        Atom::Maybe(_) => true,
        Atom::Field(_, atoms) => atoms.iter().all(|atom| match atom {
            Atom::Opt(_) | Atom::Maybe(_) => true,
            Atom::Plain(name) => !name.starts_with('\'') && nullable.contains(name),
            _ => false,
        }),
    })
}

/// Which rules can match the empty string: a least fixed point over the rule
/// bodies, starting from the alternatives that are already empty.
fn nullable(rules: &HashMap<String, Vec<Alt>>, order: &[String]) -> HashSet<String> {
    let mut nullable = HashSet::new();
    loop {
        let mut changed = false;
        for lhs in order {
            if nullable.contains(lhs) {
                continue;
            }
            let is_nullable = rules[lhs]
                .iter()
                .any(|alt| is_nullable_alt(alt, &nullable));
            if is_nullable {
                nullable.insert(lhs.clone());
                changed = true;
            }
        }
        if !changed {
            return nullable;
        }
    }
}

/// How often each name is referenced from *other* rules' bodies.
fn reference_counts(
    rules: &HashMap<String, Vec<Alt>>,
    order: &[String],
) -> HashMap<String, usize> {
    fn walk(atoms: &[Atom], counts: &mut HashMap<String, usize>) {
        for atom in atoms {
            match atom {
                Atom::Plain(name) | Atom::Opt(name) => *counts.entry(name.clone()).or_default() += 1,
                Atom::Repeat(alts) | Atom::Maybe(alts) | Atom::Plus(alts) | Atom::Group(alts) => {
                    for alt in alts {
                        walk(&alt.atoms, counts);
                    }
                }
                Atom::Field(_, atoms) => walk(atoms, counts),
            }
        }
    }
    let mut counts = HashMap::new();
    for lhs in order {
        let mut mine = HashMap::new();
        for alt in &rules[lhs] {
            walk(&alt.atoms, &mut mine);
        }
        for (name, count) in mine {
            if name != *lhs {
                *counts.entry(name).or_default() += count;
            }
        }
    }
    counts
}

/// What a rule's body says, once EBNF says it.
#[derive(Clone, Debug)]
enum Shape {
    /// `N : () | N α | α` — zero or more items.
    Star(Vec<Alt>),
    /// `N : α | N α` — one or more items.
    Plus(Vec<Alt>),
    /// `N : () | P | N I` — an optional base, then any number of items. P can
    /// be a group (`N : () | a b | c d | N ';'`); 144 of the 160 such rules in
    /// the playground corpus have exactly one base alternative.
    BaseList {
        bases: Vec<Alt>,
        items: Vec<Alt>,
        /// The source wrote `()`: `P? (I)*`, not `(P I*)?`, because the source
        /// allows the items without a base (`semicolon_list ';'` with an empty
        /// `semicolon_list`).
        nullable_base: bool,
        left: bool,
    },
    /// `N : () | a b | c` — something or nothing.
    Optional(Vec<Alt>),
}

impl Shape {
    /// The body the rule keeps. An optional rule loses its empty alternative
    /// here: its references, not its body, say `?`.
    fn kept(&self) -> Vec<Alt> {
        let atoms = match self {
            Shape::Star(items) => vec![Atom::Repeat(items.clone())],
            Shape::Plus(items) => vec![Atom::Plus(items.clone())],
            Shape::Optional(alts) => return alts.clone(),
            Shape::BaseList {
                bases,
                items,
                nullable_base,
                left,
            } => {
                let base_atoms = if *nullable_base {
                    vec![Atom::Maybe(bases.clone())]
                } else {
                    match bases.as_slice() {
                        [Alt { atoms, prec: None }] => atoms.clone(),
                        _ => vec![Atom::Group(bases.clone())],
                    }
                };
                let item = Atom::Repeat(items.clone());
                if *left {
                    base_atoms.into_iter().chain([item]).collect()
                } else {
                    [item].into_iter().chain(base_atoms).collect()
                }
            }
        };
        vec![Alt { atoms, prec: None }]
    }

    /// The one atom a reference to the rule becomes when the rule is inlined —
    /// `None` when inlining would need more than one atom, which is the sign
    /// that the name is doing work and should stay.
    fn one_atom(&self) -> Option<Atom> {
        match self {
            Shape::Star(items) => Some(Atom::Repeat(items.clone())),
            Shape::Plus(items) => Some(Atom::Plus(items.clone())),
            Shape::Optional(alts) => Some(Atom::Maybe(alts.clone())),
            Shape::BaseList { .. } => None,
        }
    }
}

/// Whether a body refers to `name` anywhere, groups included.
///
/// A list whose item mentions the rule being defined is not a list. Bison wrote
/// `if_expression : statement_expression | IF expression THEN if_expression ELSE if_expression`,
/// and `(IF expression THEN if_expression ELSE)* statement_expression` is the
/// same language, but it hides the nesting behind an operator that reads as a
/// flat sequence. Those keep the shape the source gave them.
fn mentions(atoms: &[Atom], name: &str) -> bool {
    atoms.iter().any(|atom| match atom {
        Atom::Plain(found) | Atom::Opt(found) => found == name,
        Atom::Repeat(alts) | Atom::Maybe(alts) | Atom::Plus(alts) | Atom::Group(alts) => {
            alts.iter().any(|alt| mentions(&alt.atoms, name))
        }
        Atom::Field(_, inner) => mentions(inner, name),
    })
}

/// `N`'s body in EBNF, or the reason no repetition says it.
fn shape_of(lhs: &str, alts: &[Alt], nullable: &HashSet<String>) -> Result<Shape, &'static str> {
    let empty = alts.iter().any(|alt| alt.atoms.is_empty());
    let filled: Vec<&Alt> = alts.iter().filter(|alt| !alt.atoms.is_empty()).collect();
    if filled.is_empty() {
        return Err("only-empty");
    }
    // `%prec` decides how a conflict is resolved, and an empty alternative can
    // carry one too (`opt_else : %empty %prec IF_WITHOUT_ELSE` is the
    // dangling-else fix). Folding either kind would move the decision, so a
    // rule that has any is left exactly as written.
    if alts.iter().any(|alt| alt.prec.is_some()) {
        return Err("prec");
    }
    // Every shape below that can match nothing spells that with the source's
    // empty alternative. A rule that matches nothing without one was already
    // respelled by an earlier round — `(a b)?`, `x*` — and re-reading it would
    // put the `?` on again.
    if !empty && nullable.contains(lhs) {
        return Err("already");
    }

    let leads = |alt: &Alt| alt.atoms.len() > 1 && alt.atoms[0].name() == lhs;
    let trails = |alt: &Alt| alt.atoms.len() > 1 && alt.atoms[alt.atoms.len() - 1].name() == lhs;
    let front: Vec<&Alt> = filled.iter().copied().filter(|a| leads(a) && !trails(a)).collect();
    let back: Vec<&Alt> = filled.iter().copied().filter(|a| trails(a) && !leads(a)).collect();
    if !front.is_empty() && !back.is_empty() {
        return Err("both-edges"); // recurses on both edges: not a list
    }
    let plain: Vec<&Alt> = filled.iter().copied().filter(|a| !leads(a) && !trails(a)).collect();

    let mut items: Vec<Alt> = Vec::new();
    for alt in front.iter().chain(back.iter()) {
        let rest = if leads(alt) {
            alt.atoms[1..].to_vec()
        } else {
            alt.atoms[..alt.atoms.len() - 1].to_vec()
        };
        let item = Alt { atoms: rest, prec: None };
        if item.atoms.is_empty() || is_nullable_alt(&item, nullable) {
            return Err("nullable-item"); // a repetition of something that can match nothing
        }
        if mentions(&item.atoms, lhs) {
            return Err("self-mention");
        }
        if !items.contains(&item) {
            items.push(item);
        }
    }

    // Every alternative that does not refer to the rule is a base. None of
    // them is redundant: `P | I | N I` allows `I` alone, which `P I*` does not.
    let mut bases: Vec<Alt> = Vec::new();
    for alt in plain {
        if !bases.contains(alt) {
            bases.push((*alt).clone());
        }
    }
    // A rule that is exactly its own item is a bare repetition: `I | N I` is
    // `I+`, `() | I | N I` is `I*`.
    if bases == items {
        return Ok(if empty {
            Shape::Star(items)
        } else {
            Shape::Plus(items)
        });
    }

    let left = !front.is_empty() || back.is_empty();
    // Right recursion puts the base last, which reads as a terminator. With
    // several bases it stops reading as a list and starts reading as a prefix
    // over a machine: bison's `statement` is `For (…) statement | While (…)
    // statement | If (…) statement opt_else | …` and is left alone.
    if !left && bases.len() > 1 && !items.is_empty() {
        return Err("multi-base-tail");
    }
    if items.is_empty() {
        // One alternative, or several, and no self-reference: a plain rule.
        if !empty {
            return Err("plain");
        }
        return Ok(Shape::Optional(bases));
    }
    match (bases.len(), empty) {
        (0, true) => Ok(Shape::Star(items)),
        (0, false) => Ok(Shape::Plus(items)),
        _ => Ok(Shape::BaseList {
            bases,
            items,
            nullable_base: empty,
            left,
        }),
    }
}

fn substitute(
    rules: &mut HashMap<String, Vec<Alt>>,
    order: &[String],
    target: &str,
    replacement: &[Atom],
) {
    fn walk(atoms: &mut Vec<Atom>, target: &str, replacement: &[Atom]) {
        let mut at = 0;
        while at < atoms.len() {
            if matches!(&atoms[at], Atom::Plain(name) | Atom::Opt(name) if name == target) {
                atoms.splice(at..at + 1, replacement.iter().cloned());
                at += replacement.len();
                continue;
            }
            match &mut atoms[at] {
                Atom::Repeat(alts) | Atom::Maybe(alts) | Atom::Plus(alts) | Atom::Group(alts) => {
                    for alt in alts.iter_mut() {
                        walk(&mut alt.atoms, target, replacement);
                    }
                }
                Atom::Field(_, inner) => walk(inner, target, replacement),
                _ => {}
            }
            at += 1;
        }
    }
    for lhs in order {
        if lhs == target {
            continue;
        }
        if let Some(alts) = rules.get_mut(lhs) {
            for alt in alts.iter_mut() {
                walk(&mut alt.atoms, target, replacement);
            }
        }
    }
}

/// Make every reference to a kept optional rule say `?`.
fn optionalise(rules: &mut HashMap<String, Vec<Alt>>, order: &[String], names: &[String]) {
    for name in names {
        substitute(rules, order, name, &[Atom::Opt(name.clone())]);
    }
}

/// Every rule, written the way EBNF would write it: a list is a repetition, an
/// optional thing is `?`, and a rule that only ever names one decoration is
/// inlined at the single place it is used.
///
/// The reference count decides the last one. With one reference the name buys
/// nothing — the token is already a node — so the rule becomes a field on the
/// optional it labels (`union_name= ID?`). With several references the name is
/// reused, the rule stops matching nothing, and each reference carries the `?`.
/// A list keeps its rule either way: `*`/`+` is the truthful operator, and a
/// reference cannot spell it better.
///
/// Shapes that a repetition would misstate are left exactly as the source wrote
/// them, `()` included, and the report names them by reason.
fn simplify(
    mut rules: HashMap<String, Vec<Alt>>,
    mut order: Vec<String>,
    start: &str,
) -> (HashMap<String, Vec<Alt>>, Vec<String>) {
    for _ in 0..12 {
        let mut changed = false;

        // A rule whose every alternative is empty is not part of any sentence:
        // the reference to it is nothing at all, so it goes, and a field that
        // would be left holding nothing goes with it.
        let only_empty: Vec<String> = order
            .iter()
            .filter(|lhs| *lhs != start)
            .filter(|lhs| {
                rules[*lhs]
                    .iter()
                    .all(|alt| alt.atoms.is_empty() && alt.prec.is_none())
            })
            .cloned()
            .collect();
        for name in only_empty {
            substitute(&mut rules, &order, &name, &[]);
            rules.remove(&name);
            order.retain(|other| *other != name);
            changed = true;
        }
        if drop_empty(&mut rules, &order) {
            changed = true;
        }

        let nullable = nullable(&rules, &order);
        let counts = reference_counts(&rules, &order);

        // A rule whose body is one name, used once, is that name.
        let passthrough: Vec<String> = order
            .iter()
            .filter(|lhs| *lhs != start && counts.get(*lhs).copied().unwrap_or(0) == 1)
            .filter(|lhs| {
                matches!(rules[*lhs].as_slice(),
                    [Alt { atoms, prec: None }]
                        if matches!(atoms.as_slice(), [Atom::Plain(n) | Atom::Opt(n)] if n != *lhs))
            })
            .cloned()
            .collect();
        for name in passthrough {
            let body = rules[&name][0].atoms.clone();
            substitute(&mut rules, &order, &name, &body);
            rules.remove(&name);
            order.retain(|other| *other != name);
            changed = true;
        }

        let mut shapes: Vec<(String, Shape, usize)> = Vec::new();
        for lhs in &order {
            if let Ok(shape) = shape_of(lhs, &rules[lhs], &nullable) {
                let refs = counts.get(lhs).copied().unwrap_or(0);
                shapes.push((lhs.clone(), shape, refs));
            }
        }

        // Write every body first, then inline: a shape taken before the
        // substitutions would otherwise overwrite them.
        for (lhs, shape, _) in &shapes {
            let rewritten = shape.kept();
            let at = rules.get_mut(lhs).expect("shape came from this rule");
            if *at != rewritten {
                *at = rewritten;
                changed = true;
            }
        }

        let mut kept_optionals: Vec<String> = Vec::new();
        for (lhs, shape, refs) in &shapes {
            let helper = is_factored_helper(lhs) && !matches!(shape, Shape::Optional(_));
            let inline = lhs != start && (*refs == 1 || helper);
            if inline {
                if let Some(atom) = shape.one_atom() {
                    // A rule that only names a decoration becomes a field.
                    let replacement = match atom {
                        Atom::Maybe(alts) if !alts.is_empty() => {
                            vec![Atom::Field(field_name(lhs), vec![Atom::Maybe(alts)])]
                        }
                        atom => vec![atom],
                    };
                    substitute(&mut rules, &order, lhs, &replacement);
                    rules.remove(lhs);
                    order.retain(|name| name != lhs);
                    changed = true;
                    continue;
                }
                // A base and its items need more than one atom; the reference
                // takes them all, which is why this is only done for a helper.
                if helper {
                    substitute(&mut rules, &order, lhs, &shape.kept()[0].atoms);
                    rules.remove(lhs);
                    order.retain(|name| name != lhs);
                    changed = true;
                    continue;
                }
            }
            if matches!(shape, Shape::Optional(_)) {
                kept_optionals.push(lhs.clone());
            }
        }

        // A kept optional no longer matches nothing, so its references say `?`.
        let kept_optionals: Vec<String> = kept_optionals
            .into_iter()
            .filter(|name| counts.get(name).copied().unwrap_or(0) > 0)
            .collect();
        if !kept_optionals.is_empty() {
            optionalise(&mut rules, &order, &kept_optionals);
            changed = true;
        }

        if !changed {
            break;
        }
    }

    // Last, the shapes the passes above cannot see: an alternative set that
    // says the same thing twice because it never factored what the branches
    // share.
    for lhs in &order {
        let alts = std::mem::take(rules.get_mut(lhs).expect("order came from rules"));
        rules.insert(lhs.clone(), factor(alts));
    }
    flatten_groups(&mut rules, &order);
    (rules, order)
}

/// Whether a rule is bison's factoring of a list rather than a name in the
/// language. Bison spells "one or more items" as a rule of its own, `symbols.1`
/// — `symbols_1` here — because Yacc has no `+`; the rule exists for the parser
/// generator, and `+` says the same thing wherever it is used.
fn is_factored_helper(rule: &str) -> bool {
    rule.ends_with("_1")
}

/// Drop what says nothing: a field left holding nothing, a group whose branches
/// were all erased. Erasing an always-empty rule can empty its surroundings.
fn drop_empty(rules: &mut HashMap<String, Vec<Alt>>, order: &[String]) -> bool {
    fn scrub(atoms: &mut Vec<Atom>) {
        atoms.retain_mut(|atom| match atom {
            Atom::Field(_, inner) => {
                scrub(inner);
                !inner.is_empty()
            }
            Atom::Maybe(alts) => {
                for alt in alts.iter_mut() {
                    scrub(&mut alt.atoms);
                }
                alts.retain(|alt| !alt.atoms.is_empty());
                !alts.is_empty()
            }
            Atom::Group(alts) => {
                for alt in alts.iter_mut() {
                    scrub(&mut alt.atoms);
                }
                if alts.iter().any(|alt| alt.atoms.is_empty()) {
                    let rest: Vec<Alt> = alts.iter().filter(|alt| !alt.atoms.is_empty()).cloned().collect();
                    if rest.is_empty() {
                        return false;
                    }
                    *atom = Atom::Maybe(rest);
                }
                true
            }
            Atom::Repeat(alts) | Atom::Plus(alts) => {
                for alt in alts.iter_mut() {
                    scrub(&mut alt.atoms);
                }
                alts.retain(|alt| !alt.atoms.is_empty());
                !alts.is_empty()
            }
            _ => true,
        });
    }
    let mut changed = false;
    for lhs in order {
        let before = rules[lhs].clone();
        for alt in rules.get_mut(lhs).expect("order came from rules").iter_mut() {
            scrub(&mut alt.atoms);
        }
        if *rules.get(lhs).expect("order came from rules") != before {
            changed = true;
        }
    }
    changed
}

/// Write an alternative set the way it reads: factor what the branches share,
/// and put `?` where one branch is another branch plus something.
///
/// `'(' ')' | '(' C ')' | '(' C ',' ')'` says `'('` and `')'` three times;
/// `'(' (C ','?)? ')'` says each once. Language-preserving, and no name is
/// lost. Alternatives carrying `%prec` are ordered and are left alone.
fn factor(mut alts: Vec<Alt>) -> Vec<Alt> {
    for alt in alts.iter_mut() {
        factor_atoms(&mut alt.atoms);
    }
    alts = dedupe(alts);
    for _ in 0..6 {
        if alts.len() < 2 || alts.iter().any(|alt| alt.prec.is_some()) {
            break;
        }
        let prefix = common_prefix(&alts);
        let suffix = common_suffix(&alts, prefix);
        if prefix > 0 || suffix > 0 {
            return vec![shared(&alts, prefix, suffix)];
        }
        if let Some(factored) = subsume(&alts) {
            return factored;
        }
        if let Some(merged) = merge_optional_tail(&alts) {
            alts = merged;
            continue;
        }
        match partition(&alts) {
            Some(next) => alts = next,
            None => break,
        }
    }
    alts
}

fn factor_atoms(atoms: &mut [Atom]) {
    for atom in atoms.iter_mut() {
        match atom {
            Atom::Repeat(alts) | Atom::Maybe(alts) | Atom::Plus(alts) | Atom::Group(alts) => {
                *alts = factor(std::mem::take(alts));
            }
            Atom::Field(_, inner) => factor_atoms(inner),
            _ => {}
        }
    }
}

fn dedupe(alts: Vec<Alt>) -> Vec<Alt> {
    let mut kept: Vec<Alt> = Vec::new();
    for alt in alts {
        if !kept.contains(&alt) {
            kept.push(alt);
        }
    }
    kept
}

fn common_prefix(alts: &[Alt]) -> usize {
    let first = &alts[0].atoms;
    let mut length = 0;
    'grow: while length < first.len() {
        for alt in &alts[1..] {
            if alt.atoms.len() <= length || alt.atoms[length] != first[length] {
                break 'grow;
            }
        }
        length += 1;
    }
    length
}

fn common_suffix(alts: &[Alt], prefix: usize) -> usize {
    let first = &alts[0].atoms;
    let mut length = 0;
    'grow: while length + prefix < first.len() {
        let atom = &first[first.len() - 1 - length];
        for alt in &alts[1..] {
            if alt.atoms.len() <= prefix + length || alt.atoms[alt.atoms.len() - 1 - length] != *atom {
                break 'grow;
            }
        }
        length += 1;
    }
    length
}

/// The single alternative that says what all of them said between the shared
/// prefix and the shared suffix.
fn shared(alts: &[Alt], prefix: usize, suffix: usize) -> Alt {
    let rests: Vec<Alt> = alts
        .iter()
        .map(|alt| Alt {
            atoms: alt.atoms[prefix..alt.atoms.len() - suffix].to_vec(),
            prec: None,
        })
        .collect();
    let mut atoms = alts[0].atoms[..prefix].to_vec();
    atoms.extend(group_of(rests));
    atoms.extend_from_slice(&alts[0].atoms[alts[0].atoms.len() - suffix..]);
    Alt { atoms, prec: None }
}

/// What an alternative set becomes in the middle of a sequence: nothing when it
/// can only be nothing, a plain group when it is a choice, `(...)?` when one of
/// the branches is empty.
fn group_of(rests: Vec<Alt>) -> Vec<Atom> {
    let rests = dedupe(rests);
    let optional = rests.iter().any(|alt| alt.atoms.is_empty());
    let filled: Vec<Alt> = rests.into_iter().filter(|alt| !alt.atoms.is_empty()).collect();
    if filled.is_empty() {
        return Vec::new();
    }
    let inner = factor(filled);
    if optional {
        return vec![Atom::Maybe(inner)];
    }
    match inner.as_slice() {
        [Alt { atoms, prec: None }] => atoms.clone(),
        _ => vec![Atom::Group(inner)],
    }
}

/// `p | p B` is `p B?`: one branch is another branch plus something. Picking
/// the shortest branch first gives the widest grouping, so `() | a | a b`
/// becomes `(a b?)?` in one step.
fn subsume(alts: &[Alt]) -> Option<Vec<Alt>> {
    let mut by_length: Vec<usize> = (0..alts.len()).collect();
    by_length.sort_by_key(|&index| alts[index].atoms.len());
    for &index in &by_length {
        let prefix = &alts[index].atoms;
        let covered: Vec<usize> = (0..alts.len())
            .filter(|&other| {
                other != index
                    && alts[other].atoms.len() > prefix.len()
                    && alts[other].atoms.starts_with(prefix)
            })
            .collect();
        if covered.is_empty() {
            continue;
        }
        let rests: Vec<Alt> = covered
            .iter()
            .map(|&other| Alt {
                atoms: alts[other].atoms[prefix.len()..].to_vec(),
                prec: None,
            })
            .collect();
        let mut atoms = prefix.clone();
        atoms.push(Atom::Maybe(factor(rests)));
        let mut rest: Vec<Alt> = vec![Alt { atoms, prec: None }];
        for (other, alt) in alts.iter().enumerate() {
            if other != index && !covered.contains(&other) {
                rest.push(alt.clone());
            }
        }
        return Some(factor(rest));
    }
    None
}

/// `X A? | A` is `X? A`: the shorter branch is the longer one's own tail with
/// its head optional. One atom of head only — `X Y A? | A` says nothing about
/// `X Y`, so it is not `(X Y)? A`.
fn merge_optional_tail(alts: &[Alt]) -> Option<Vec<Alt>> {
    for (index, longer) in alts.iter().enumerate() {
        let Some(Atom::Maybe(inner)) = longer.atoms.last() else {
            continue;
        };
        let [Alt { atoms: tail, prec: None }] = inner.as_slice() else {
            continue;
        };
        let Some(short) = (0..alts.len())
            .find(|&other| other != index && alts[other].atoms == *tail) else {
            continue;
        };
        let head = &longer.atoms[..longer.atoms.len() - 1];
        if head.len() > 1 {
            continue;
        }
        let merged = if head.is_empty() {
            longer.clone()
        } else {
            Alt {
                atoms: [Atom::Maybe(vec![Alt { atoms: head.to_vec(), prec: None }])]
                    .into_iter()
                    .chain(tail.iter().cloned())
                    .collect(),
                prec: None,
            }
        };
        let mut out: Vec<Alt> = Vec::new();
        for (other, alt) in alts.iter().enumerate() {
            if other == index {
                out.push(merged.clone());
            } else if other != short {
                out.push(alt.clone());
            }
        }
        return Some(factor(out));
    }
    None
}

/// Consecutive branches that begin with the same atom become one branch that
/// begins with it and then chooses. Consecutive only: alternatives are ordered,
/// and moving one past another is not the translator's call.
fn partition(alts: &[Alt]) -> Option<Vec<Alt>> {
    let mut out: Vec<Alt> = Vec::new();
    let mut changed = false;
    let mut at = 0;
    while at < alts.len() {
        if alts[at].atoms.is_empty() {
            out.push(alts[at].clone());
            at += 1;
            continue;
        }
        let head = alts[at].atoms[0].clone();
        let mut end = at + 1;
        while end < alts.len() && !alts[end].atoms.is_empty() && alts[end].atoms[0] == head {
            end += 1;
        }
        if end - at == 1 {
            out.push(alts[at].clone());
            at = end;
            continue;
        }
        changed = true;
        let rests: Vec<Alt> = alts[at..end]
            .iter()
            .map(|alt| Alt { atoms: alt.atoms[1..].to_vec(), prec: None })
            .collect();
        let mut atoms = vec![head];
        atoms.extend(group_of(rests));
        out.push(Alt { atoms, prec: None });
        at = end;
    }
    changed.then_some(out)
}

/// `(a b) c` says what `a b c` says: a group that kept exactly one branch
/// needs no parentheses. Factoring produces those, and only those — a real
/// choice keeps its group.
fn flatten_groups(rules: &mut HashMap<String, Vec<Alt>>, order: &[String]) {
    fn walk(atoms: &mut Vec<Atom>) {
        let mut at = 0;
        while at < atoms.len() {
            let single = match &mut atoms[at] {
                Atom::Group(alts) => {
                    for alt in alts.iter_mut() {
                        walk(&mut alt.atoms);
                    }
                    match alts.as_slice() {
                        [Alt { atoms: only, prec: None }] => Some(only.clone()),
                        _ => None,
                    }
                }
                Atom::Repeat(alts) | Atom::Maybe(alts) | Atom::Plus(alts) => {
                    for alt in alts.iter_mut() {
                        walk(&mut alt.atoms);
                    }
                    None
                }
                Atom::Field(_, inner) => {
                    walk(inner);
                    None
                }
                _ => None,
            };
            match single {
                Some(only) => {
                    atoms.splice(at..at + 1, only);
                }
                None => at += 1,
            }
        }
    }
    for lhs in order {
        for alt in rules.get_mut(lhs).expect("order came from rules").iter_mut() {
            walk(&mut alt.atoms);
        }
    }
}

/// The field a rule's name becomes when the rule is inlined: `tag_opt` → `tag`.
fn field_name(rule: &str) -> String {
    rule.strip_suffix("_opt").unwrap_or(rule).to_owned()
}

/// What the rewrite did not express, and why. Run with `G2TSG_REPORT=1`.
fn report_nullable(rules: &HashMap<String, Vec<Alt>>, order: &[String], start: &str) {
    let nullable = nullable(rules, order);
    let counts = reference_counts(rules, order);
    for lhs in order {
        let blank = rules[lhs].iter().any(|alt| alt.atoms.is_empty());
        if !blank {
            continue;
        }
        let refs = counts.get(lhs).copied().unwrap_or(0);
        let reason = match shape_of(lhs, &rules[lhs], &nullable) {
            Err(reason) => reason,
            // The shape is one a repetition says, so the `()` is a reshaping
            // that did not happen: a bug, not a limitation.
            Ok(_) => "unreached",
        };
        let start = if lhs == start { " (the start rule)" } else { "" };
        eprintln!("REPORT {reason:<16} {lhs} refs={refs}{start}");
    }
}
