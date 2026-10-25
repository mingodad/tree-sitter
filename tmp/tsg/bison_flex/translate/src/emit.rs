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
    let mut tokens = Vec::new();
    let mut literals = Vec::new();
    let mut levels = Vec::new();
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
            Directive::Unsupported(keyword) => {
                return Err(format!("`{keyword}` is not translated yet"));
            }
        }
    }
    // Bison's default start symbol is the first rule.
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
        let (kept, kept_order) = remove_empty_rules(rules, order, &start);
        rules = kept;
        order = kept_order;

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

    // --- nullability ---------------------------------------------------------

    let nullable = nullable(&rules, &order);
    let always_empty = lists(&rules, &order, &nullable)?;
    let rules = lists_rewritten(&rules, &always_empty);
    let mut rules = distribute_optional(&rules, &order, &nullable, &start)?;
    // tree-sitter has no empty production, so an alternative that is still
    // empty after the `?` distribution cannot be written down. Its uses were
    // made optional by `distribute_optional`, so dropping it here loses
    // nothing that the rewrite did not already give to the callers.
    for lhs in rules.keys().cloned().collect::<Vec<_>>() {
        let kept: Vec<Alt> = rules[&lhs]
            .iter()
            .filter(|alt| !alt.atoms.is_empty())
            .cloned()
            .collect();
        if kept.is_empty() {
            rules.remove(&lhs);
            order.retain(|name| *name != lhs);
        } else {
            rules.insert(lhs, kept);
        }
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
    lines.push(String::new());
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
    lines.push("// The first rule outside `tokens` is the start rule.".to_string());
    lines.push(String::new());

    let emit_order = std::iter::once(&start).chain(order.iter().filter(|lhs| **lhs != start));
    for lhs in emit_order {
        let Some(alts) = rules.get(lhs) else {
            continue;
        };
        let mut rendered: Vec<String> = Vec::new();
        for alt in alts {
            let atoms: Vec<String> = alt
                .atoms
                .iter()
                .map(|atom| {
                    let name = atom.name();
                    // A lookahead terminal is its base token.
                    let name = la_of.get(name).map(String::as_str).unwrap_or(name);
                    match atom {
                        Atom::Opt(_) => format!("{name}?"),
                        Atom::Plain(_) => name.to_string(),
                    }
                })
                .collect();
            let text = format!(
                "{}{}",
                atoms.join(" "),
                mark(alt, &levels, &level_of, &token_set)?
            );
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

/// Up to three names, then "and N more" — an error message that lists forty
/// tokens tells the reader less than one that says there are forty.
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

/// Remove the rules that can only produce the empty string, together with
/// every reference to them, until none is left.
fn remove_empty_rules(
    rules: HashMap<String, Vec<Alt>>,
    order: Vec<String>,
    start: &str,
) -> (HashMap<String, Vec<Alt>>, Vec<String>) {
    let mut rules = rules;
    let mut order = order;
    loop {
        let empty_only: Vec<String> = order
            .iter()
            .filter(|lhs| *lhs != start)
            // A rule with *no* alternatives is unproductive, not empty; the
            // `all` of an empty list would say it matches nothing at all.
            .filter(|lhs| !rules[*lhs].is_empty() && rules[*lhs].iter().all(|alt| alt.is_empty()))
            .cloned()
            .collect();
        if empty_only.is_empty() {
            return (rules, order);
        }
        for lhs in &empty_only {
            rules.remove(lhs);
        }
        order.retain(|lhs| !empty_only.contains(lhs));
        let gone: HashSet<&str> = empty_only.iter().map(String::as_str).collect();
        for alts in rules.values_mut() {
            for alt in alts.iter_mut() {
                alt.atoms.retain(|atom| {
                    let name = atom.name();
                    name.starts_with('\'') || !gone.contains(name)
                });
            }
        }
    }
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
            let is_nullable = rules[lhs].iter().any(|alt| {
                alt.atoms.iter().all(|atom| match atom {
                    Atom::Opt(_) => true,
                    Atom::Plain(name) => !name.starts_with('\'') && nullable.contains(name),
                })
            });
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

/// A nullable rule that refers to itself is a list. Bison can write the base
/// case as `%empty`; tree-sitter cannot, so it has to become explicit.
fn lists(
    rules: &HashMap<String, Vec<Alt>>,
    order: &[String],
    nullable: &HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for lhs in order {
        if !nullable.contains(lhs) {
            continue;
        }
        let recursive = rules[lhs]
            .iter()
            .any(|alt| alt.atoms.iter().any(|atom| atom.name() == lhs));
        if !recursive {
            continue;
        }
        let mut where_recurses = HashSet::new();
        for alt in &rules[lhs] {
            for (index, atom) in alt.atoms.iter().enumerate() {
                if atom.name() != lhs {
                    continue;
                }
                where_recurses.insert(if index == 0 {
                    0
                } else if index == alt.atoms.len() - 1 {
                    2
                } else {
                    1
                });
            }
        }
        if where_recurses.contains(&1) || where_recurses.len() > 1 {
            return Err(format!(
                "`{lhs}` is nullable and refers to itself in the middle of a body, which is not a list"
            ));
        }
        found.push(lhs.clone());
    }
    Ok(found)
}

/// `N : N ',' item | %empty` → `N : N ',' item | ',' item`, and the mirror
/// image for a rule that recurses on its last symbol: the recursive
/// alternatives keep a self-reference, and the base case is derived from them
/// when the source only wrote `%empty`.
fn lists_rewritten(
    rules: &HashMap<String, Vec<Alt>>,
    lists: &[String],
) -> HashMap<String, Vec<Alt>> {
    let mut out = rules.clone();
    for lhs in lists {
        let alts: Vec<&Alt> = rules[lhs].iter().filter(|alt| !alt.is_empty()).collect();
        // Which edge the rule recurses on: the first symbol or the last.
        let leading = alts
            .iter()
            .any(|alt| alt.atoms.first().is_some_and(|atom| atom.name() == lhs));
        let on_edge = |alt: &Alt| {
            if leading {
                alt.atoms.first().is_some_and(|atom| atom.name() == lhs)
            } else {
                alt.atoms.last().is_some_and(|atom| atom.name() == lhs)
            }
        };
        let rest = |alt: &Alt| {
            if leading {
                alt.atoms[1..].to_vec()
            } else {
                alt.atoms[..alt.atoms.len() - 1].to_vec()
            }
        };

        let recursive: Vec<&Alt> = alts.iter().copied().filter(|alt| on_edge(alt)).collect();
        let mut base: Vec<Alt> = alts
            .iter()
            .copied()
            .filter(|alt| !on_edge(alt))
            .cloned()
            .collect();
        if base.is_empty() {
            // No base case at all: the item list is the rest of the recursive
            // alternative, without the self-reference.
            base = recursive
                .iter()
                .map(|alt| Alt {
                    atoms: rest(alt),
                    prec: alt.prec.clone(),
                })
                .collect();
        }

        let mut rewritten: Vec<Alt> = recursive
            .iter()
            .map(|alt| {
                let mut atoms = if leading {
                    vec![Atom::Plain(lhs.clone())]
                } else {
                    Vec::new()
                };
                atoms.extend(rest(alt));
                if !leading {
                    atoms.push(Atom::Plain(lhs.clone()));
                }
                Alt {
                    atoms,
                    prec: alt.prec.clone(),
                }
            })
            .collect();
        rewritten.extend(base);
        out.insert(lhs.clone(), rewritten);
    }
    out
}

/// Replace empty productions by optionality at the use sites.
///
/// Tree-sitter forbids a non-start rule that matches the empty string, so a
/// nullable rule `N` keeps its body but every reference to it becomes `N?`.
/// An alternative that is *entirely* optional then has to be spelled out as
/// its non-empty subsequences: `a? b? c?` → `a b? c? | b c? | c`.
fn distribute_optional(
    rules: &HashMap<String, Vec<Alt>>,
    order: &[String],
    maybe_empty: &HashSet<String>,
    start: &str,
) -> Result<HashMap<String, Vec<Alt>>, String> {
    let mut rewritten: HashMap<String, Vec<Alt>> = HashMap::new();
    for lhs in order {
        let multi = lhs != start;
        let mut out: Vec<Alt> = Vec::new();
        for alt in &rules[lhs] {
            if multi && alt.is_empty() {
                continue;
            }
            let atoms: Vec<Atom> = alt
                .atoms
                .iter()
                .map(|atom| match atom {
                    Atom::Opt(name) => Atom::Opt(name.clone()),
                    Atom::Plain(name) => {
                        if !name.starts_with('\'') && maybe_empty.contains(name) && name != lhs {
                            Atom::Opt(name.clone())
                        } else {
                            Atom::Plain(name.clone())
                        }
                    }
                })
                .collect();
            if multi && !atoms.is_empty() && atoms.iter().all(|atom| matches!(atom, Atom::Opt(_))) {
                for index in 0..atoms.len() {
                    // The first present symbol is mandatory; the rest stay
                    // optional, so the subsequences together cover the body.
                    let mut subsequence = vec![Atom::Plain(atoms[index].name().to_string())];
                    subsequence.extend(atoms[index + 1..].iter().cloned());
                    out.push(Alt {
                        atoms: subsequence,
                        prec: alt.prec.clone(),
                    });
                }
                continue;
            }
            out.push(Alt {
                atoms,
                prec: alt.prec.clone(),
            });
        }
        if out.is_empty() {
            return Err(format!(
                "`{lhs}` has no alternative left after removing `%empty`"
            ));
        }
        rewritten.insert(lhs.clone(), out);
    }

    // The rewrite is only sound if no non-start rule is still nullable.
    let still = nullable(&rewritten, order);
    for lhs in &still {
        if lhs != start {
            return Err(format!("`{lhs}` is still nullable after the rewrite"));
        }
    }
    Ok(rewritten)
}
