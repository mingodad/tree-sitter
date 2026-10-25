//! Reading the `.g` file with tree-sitter.
//!
//! This is the whole point of the crate: instead of splitting the file into
//! sections with regexes and re-tokenizing the Bison rules by hand, parse it
//! once with the grammar in `bison_flex/grammar.tsg` and walk the tree.
//!
//! The tree-sitter API used here is small and worth knowing:
//!
//! * [`Parser::parse`] turns bytes into a [`Tree`]; a [`TreeCursor`] walks it without allocating.
//! * [`Node::kind`] is the rule or token name from the grammar; anonymous tokens (`':'`) report
//!   their text as their kind.
//! * [`Node::named_children`] skips anonymous tokens, but **not** extras: a named extra such as
//!   `COMMENT` or `NL` is a named node too. Filtering those four kinds is the only housekeeping the
//!   walk needs.
//! * [`Node::utf8_text`] gives the source text of a leaf, so quoted literals keep their quotes and
//!   indentation is never guessed from columns.
//! * [`Tree::root_node`]`().has_error()` says whether recovery happened; if it did, the first
//!   `ERROR`/`MISSING` node points at the input that failed. (There are no fields in `grammar.tsg`
//!   for `child_by_field_name` to use, because the format is too positional to benefit from them.)

use std::fmt;

use tree_sitter::{Language, Node, Parser, Tree};

use crate::ir::{Alt, Assoc, Atom, Directive, Grammar, Lexer, Rule};

/// A parse or input error, with the position it was found at.
#[derive(Debug)]
pub struct InputError {
    pub message: String,
    pub row: usize,
    pub column: usize,
}

impl InputError {
    fn at(node: &Node, message: impl Into<String>) -> Self {
        let point = node.start_position();
        Self {
            message: message.into(),
            row: point.row,
            column: point.column,
        }
    }
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Positions are 1-based for humans; tree-sitter's are 0-based.
        write!(f, "{}:{}: {}", self.row + 1, self.column + 1, self.message)
    }
}

/// The kinds that `grammar.tsg` declares as extras. They appear as children of
/// every node that spans them, so every walk has to step over them.
const EXTRAS: [&str; 3] = ["WS", "NL", "COMMENT"];

fn is_extra(node: &Node) -> bool {
    EXTRAS.contains(&node.kind())
}

/// Named children with the extras removed, in source order.
fn meaningful<'t>(node: &Node<'t>) -> Vec<Node<'t>> {
    node.named_children(&mut node.walk())
        .filter(|child| !is_extra(child))
        .collect()
}

fn text<'s>(node: &Node, source: &'s [u8]) -> &'s str {
    node.utf8_text(source).expect("source is UTF-8")
}

pub fn parse(source: &[u8], language: &Language) -> Result<Tree, InputError> {
    let mut parser = Parser::new();
    parser
        .set_language(language)
        .expect("the bison_flex parser is ABI-compatible");
    parser.parse(source, None).ok_or(InputError {
        message: "the parser gave up (cancelled or out of memory)".to_string(),
        row: 0,
        column: 0,
    })
}

/// Read the whole file.
pub fn read(source: &[u8], language: &Language) -> Result<Grammar, InputError> {
    let tree = parse(source, language)?;
    let root = tree.root_node();

    if root.has_error() {
        return Err(syntax_error(&root, source));
    }

    let mut directives = Vec::new();
    let mut rules = Vec::new();
    let mut lexer = Lexer::default();
    let mut macros = Vec::new();
    let mut lexer_rules = Vec::new();

    // `source_file: directive* SectionSep grule* SectionSep LexerLine*
    // SectionSep LexerLine* SectionSep`, so the separators number the sections.
    let mut section = 0;
    for child in meaningful(&root) {
        match child.kind() {
            "SectionSep" => section += 1,
            "directive" => directives.extend(read_directive(&child, source)?),
            "grule" => rules.push(read_rule(&child, source)?),
            "LexerLine" => match section {
                2 => macros.push(text(&child, source).to_string()),
                3 => lexer_rules.push(text(&child, source).to_string()),
                _ => {
                    return Err(InputError::at(
                        &child,
                        "a lexer line outside the two Flex sections",
                    ));
                }
            },
            other => {
                return Err(InputError::at(
                    &child,
                    format!("unexpected node `{other}` at the top level"),
                ));
            }
        }
    }
    if section != 4 {
        return Err(InputError {
            message: format!("expected four `%%` section separators, found {section}"),
            row: root.end_position().row,
            column: 0,
        });
    }

    lexer.macros = split_macros(&macros)?;
    let mut notes = Vec::new();
    lexer.rules = split_lexer_rules(&lexer_rules, &mut notes)?;
    lexer.notes = notes;
    // `%option caseless` sits with the definitions, not with the rules.
    lexer.caseless = macros
        .iter()
        .chain(lexer_rules.iter())
        .any(|line| is_caseless_option(line));

    Ok(Grammar {
        directives,
        rules,
        lexer,
    })
}

/// Where the parse went wrong, as a diagnostic.
///
/// An `ERROR` node covers everything that did not fit, so the outermost one
/// spans half the file and says nothing. Two descents are needed: to the
/// innermost `ERROR` (the token that broke the pattern), and then to the last
/// node the parser could *finish*, because recovery resumes right after it.
fn syntax_error(root: &Node, source: &[u8]) -> InputError {
    let node = innermost_error(root).unwrap_or(*root);
    if node.is_missing() {
        return InputError::at(&node, format!("expected {}", spelled(node.kind())));
    }

    let resume = if let Some(done) = last_complete(&node) {
        skip_blank(source, done.end_byte())
    } else {
        // Nothing whole inside the `ERROR`: report where the junk starts.
        let mut cursor = node.walk();
        let first_junk = node
            .children(&mut cursor)
            .find(|child| !child.is_extra() && child.end_byte() > child.start_byte());
        match first_junk {
            Some(child) if child.start_byte() < source.len() => child.start_byte(),
            _ => node.start_byte(),
        }
    };
    let (row, column) = position_of(source, resume);
    InputError {
        message: match snippet_from(source, resume) {
            Some(text) => format!("unexpected `{text}`"),
            None => "unexpected end of file".to_string(),
        },
        row,
        column,
    }
}

/// How to name a token in a message. `MISSING` nodes have no text, only a
/// kind, so the punctuation has to be spelled out for the reader.
fn spelled(kind: &str) -> String {
    let text = match kind {
        "Semi" => ";",
        "Pipe" => "|",
        "Colon" => ":",
        "SectionSep" => "%%",
        "PercentEmpty" => "%empty",
        "Literal" => "a quoted literal",
        "Name" => "a rule or token name",
        "Newline" => "a newline",
        other => return format!("`{other}`"),
    };
    format!("`{text}`")
}

fn innermost_error<'t>(node: &Node<'t>) -> Option<Node<'t>> {
    if node.is_missing() {
        return Some(*node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let broken = child.is_error() || child.is_missing() || child.has_error();
        // A zero-width `ERROR` at the end of the file only says "something is
        // still open here"; the node around it is the informative one.
        let informative = child.is_missing() || child.end_byte() > child.start_byte();
        if broken && informative {
            return innermost_error(&child).or(Some(child));
        }
    }
    node.is_error().then_some(*node)
}

/// The last child of an `ERROR` node that is a whole construct: a declaration,
/// a rule, a separator or a lexer line. That is what the parser understood
/// before it started resynchronizing, so the error is right after it.
fn last_complete<'t>(node: &Node<'t>) -> Option<Node<'t>> {
    const WHOLE: [&str; 4] = ["directive", "grule", "SectionSep", "LexerLine"];
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    children
        .iter()
        .rev()
        .find(|child| WHOLE.contains(&child.kind()))
        .copied()
}

fn skip_blank(source: &[u8], mut at: usize) -> usize {
    while at < source.len() && source[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

fn position_of(source: &[u8], at: usize) -> (usize, usize) {
    let at = at.min(source.len());
    let row = source[..at].iter().filter(|byte| **byte == b'\n').count();
    let line_start = source[..at]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    (row, at - line_start)
}

/// The rest of the line, elided: what the parser could not fit.
fn snippet_from(source: &[u8], at: usize) -> Option<String> {
    let rest = std::str::from_utf8(&source[at.min(source.len())..]).ok()?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    let line = rest.lines().next().unwrap_or_default().trim_end();
    Some(if line.chars().count() > 40 {
        format!("{}…", line.chars().take(40).collect::<String>())
    } else {
        line.to_string()
    })
}

fn read_directive(node: &Node, source: &[u8]) -> Result<Option<Directive>, InputError> {
    let children = meaningful(node);
    let Some(keyword) = children.first() else {
        return Err(InputError::at(node, "empty directive"));
    };

    // The keywords are declared as named tokens in `grammar.tsg` (`%left` is
    // the token `PercentLeft`), so a node's kind is the token's name, not its
    // text.
    let assoc = match keyword.kind() {
        "PercentLeft" => Some(Assoc::Left),
        "PercentRight" => Some(Assoc::Right),
        "PercentNonassoc" => Some(Assoc::Nonassoc),
        "PercentPrecedence" => Some(Assoc::None),
        _ => None,
    };

    if let Some(assoc) = assoc {
        let terms = declared_symbols(&children[1..], source)?;
        return Ok(Some(Directive::Assoc { assoc, terms }));
    }

    match keyword.kind() {
        "PercentToken" => Ok(Some(Directive::Token(declared_symbols(
            &children[1..],
            source,
        )?))),
        "PercentStart" => {
            let name = children
                .get(1)
                .map(|child| text(child, source).to_string())
                .unwrap_or_default();
            Ok(Some(Directive::Start(name)))
        }
        // Recognized by the metagrammar, but nothing translates them yet, and
        // ignoring a declaration silently is how a grammar ends up subtly
        // different from its source.
        "PercentFallback"
        | "PercentTokenIndent"
        | "PercentTokenDedent"
        | "PercentTokenSymbolTable" => Ok(Some(Directive::Unsupported(keyword.kind().to_string()))),
        other => Err(InputError::at(
            keyword,
            format!("directive `{other}` is not translated"),
        )),
    }
}

/// The symbols of a `decl_tokens` node: `'literal'`, `Name`, and `<type>` tags.
/// Tags are Bison type annotations, not grammar symbols, so they are dropped.
fn declared_symbols(children: &[Node], source: &[u8]) -> Result<Vec<String>, InputError> {
    let mut symbols = Vec::new();
    for child in children {
        collect_symbols(child, source, &mut symbols)?;
    }
    Ok(symbols)
}

/// Walk to the leaves: `decl_tokens` holds `decl_token`s, and a `decl_token` is
/// a `Literal`, a `Name` or a `<type>` tag. Only the tags are dropped.
fn collect_symbols(
    node: &Node,
    source: &[u8],
    symbols: &mut Vec<String>,
) -> Result<(), InputError> {
    match node.kind() {
        "Literal" | "Name" => {
            symbols.push(text(node, source).to_string());
            Ok(())
        }
        "TypeTag" => Ok(()),
        _ => {
            for child in meaningful(node) {
                collect_symbols(&child, source, symbols)?;
            }
            Ok(())
        }
    }
}

// --- Bison rules -----------------------------------------------------------

fn read_rule(node: &Node, source: &[u8]) -> Result<Rule, InputError> {
    let children = meaningful(node);
    let Some(lhs) = children.first() else {
        return Err(InputError::at(node, "empty rule"));
    };
    if lhs.kind() != "Name" {
        return Err(InputError::at(lhs, "a rule must start with its name"));
    }
    Ok(Rule {
        lhs: text(lhs, source).to_string(),
        alts: read_alternatives(&children[2..], source, "Semi")?,
    })
}

/// `(alt_rhs? opt_prec? opt_prod_alias?) (Pipe …)*` up to `end`.
///
/// The same shape appears in three places — a rule, and a bracketed,
/// parenthesized or braced group inside a rule — so it is parsed once.
fn read_alternatives(children: &[Node], source: &[u8], end: &str) -> Result<Vec<Alt>, InputError> {
    let mut alts = Vec::new();
    let mut atoms = Vec::new();
    let mut prec = None;
    let mut saw_end = false;

    for child in children {
        match child.kind() {
            kind if kind == end => {
                saw_end = true;
                break;
            }
            "Pipe" => {
                alts.push(Alt { atoms, prec });
                atoms = Vec::new();
                prec = None;
            }
            "alt_rhs" => {
                atoms = read_alt_rhs(child, source)?;
            }
            "opt_prec" => {
                // `PercentPrec` is a named token, so the symbol is the *last*
                // named child of the node, not the only one.
                let symbol = meaningful(child)
                    .into_iter()
                    .find(|node| matches!(node.kind(), "Literal" | "Name"))
                    .ok_or_else(|| InputError::at(child, "`%prec` without a symbol"))?;
                prec = Some(text(&symbol, source).to_string());
            }
            // `#Name` names an alternative for a tool that reads the `.g`
            // file; tree-sitter has no per-production name, and dropping it
            // changes nothing about the language.
            "opt_prod_alias" => {}
            other => {
                return Err(InputError::at(
                    child,
                    format!("unexpected `{other}` in a rule body"),
                ));
            }
        }
    }
    alts.push(Alt { atoms, prec });
    if !saw_end {
        return Err(InputError::at(
            children.last().expect("a rule body has children"),
            format!("unterminated rule: expected `{end}`"),
        ));
    }
    Ok(alts)
}

/// `%empty` or a sequence of symbols.
fn read_alt_rhs(node: &Node, source: &[u8]) -> Result<Vec<Atom>, InputError> {
    let children = meaningful(node);
    let Some(first) = children.first() else {
        return Ok(Vec::new());
    };
    if first.kind() == "PercentEmpty" {
        return Ok(Vec::new());
    }
    if first.kind() != "rhs_sequence" {
        return Err(InputError::at(
            first,
            format!("unexpected `{}` in a production body", first.kind()),
        ));
    }

    let mut atoms = Vec::new();
    for rhs in meaningful(first) {
        atoms.extend(read_rhs(&rhs, source)?);
    }
    Ok(atoms)
}

/// One `rhs`: an atom plus its postfix operators.
fn read_rhs(node: &Node, source: &[u8]) -> Result<Vec<Atom>, InputError> {
    let children = meaningful(node);
    let Some(atom) = children.first() else {
        return Err(InputError::at(node, "empty rule body"));
    };
    let mut atom = read_atom(atom, source)?;

    for postfix in &children[1..] {
        match postfix.kind() {
            "Question" => atom = Atom::Opt(atom.name().to_string()),
            "Star" => {
                return Err(InputError::at(
                    postfix,
                    "`*` (zero or more) is not translated yet",
                ));
            }
            "Plus" => {
                return Err(InputError::at(
                    postfix,
                    "`+` (one or more) is not translated yet",
                ));
            }
            other => return Err(InputError::at(postfix, format!("unexpected `{other}`"))),
        }
    }
    Ok(vec![atom])
}

/// A `Literal` or a `Name`. The metagrammar also has `[...]`, `(...)` and
/// `{...}` groups, which would need a new rule per group to translate; they are
/// rejected with the position of the bracket instead of dropped silently.
fn read_atom(node: &Node, source: &[u8]) -> Result<Atom, InputError> {
    let children = meaningful(node);
    match children.first().map(Node::kind) {
        Some("Literal" | "Name") => Ok(Atom::Plain(text(&children[0], source).to_string())),
        Some(_) => Err(InputError::at(
            &children[0],
            "`[...]`, `(...)` and `{...}` groups are not translated yet",
        )),
        None => Err(InputError::at(node, "unexpected rule body node")),
    }
}

// --- Flex sections ---------------------------------------------------------

/// `name pattern`, one per line. `%`-lines are directives, not macros.
fn split_macros(lines: &[String]) -> Result<Vec<(String, String)>, InputError> {
    let mut macros = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let Some((name, pattern)) = line.split_once(char::is_whitespace) else {
            return Err(InputError {
                message: format!("expected `name pattern`, found `{line}`"),
                row: 0,
                column: 0,
            });
        };
        macros.push((name.to_string(), pattern.trim().to_string()));
    }
    Ok(macros)
}

/// `%option caseless`, which makes the keyword rules case-insensitive.
fn is_caseless_option(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("%option") && line.split_whitespace().any(|word| word == "caseless")
}

/// `pattern action`, one entry per rule.
///
/// Flex rules may name start conditions, and tree-sitter builds one stateless
/// lexer: a rule that can run in `INITIAL` is expressible, a rule that only runs
/// in another state is not. So this keeps the expressible half and *reports* the
/// rest instead of dropping it, because a silently dropped rule looks exactly
/// like a `.g` that never had that token — which is how `bison.g`'s whole lexer
/// (it lives in `<INITIAL,RULES_ST> { … }`) was lost, and why the translation
/// then blamed the source for not returning `STRING`.
///
/// `<states> { … }` groups rules under the same start conditions, which is what
/// Flex means by it, so a group that includes `INITIAL` expands to its rules.
fn split_lexer_rules(
    lines: &[String],
    notes: &mut Vec<String>,
) -> Result<Vec<(String, String)>, InputError> {
    let mut rules = Vec::new();
    let mut index = 0;
    // A `/* … */` block hides every line inside it: real lexers comment rules
    // out that way, and a commented-out rule must not win a token's body.
    let mut in_comment = false;
    let mut entries: Vec<String> = Vec::new();
    let mut state_only: Vec<String> = Vec::new();
    while index < lines.len() {
        let line = lines[index].trim();
        index += 1;
        if in_comment {
            in_comment = !line.contains("*/");
            continue;
        }
        if let Some(after) = line.strip_prefix("/*") {
            in_comment = !after.contains("*/");
            continue;
        }
        if line.is_empty() || line.starts_with("//") || line.starts_with('%') {
            continue;
        }
        // `<INITIAL,RULES_ST> {` … `}`: every line in the group carries those
        // start conditions. Expand it when `INITIAL` is among them, and report
        // it otherwise: a rule that only runs in `xformat_la` needs a lexer
        // state that tree-sitter does not have.
        let (states, _) = take_leading_states(line);
        {
            if line.ends_with('{') && !states.is_empty() {
                let start = index;
                while index < lines.len() && lines[index].trim() != "}" {
                    index += 1;
                }
                let inner: Vec<String> = lines[start..index.min(lines.len())]
                    .iter()
                    .map(|line| line.trim().to_string())
                    .collect();
                index += 1;
                if states.iter().any(|state| state == "INITIAL") {
                    entries.extend(inner.into_iter().filter(|line| !line.is_empty()));
                } else {
                    state_only.extend(inner.into_iter().filter(|line| !line.is_empty()));
                }
                continue;
            }
        }
        entries.push(line.to_string());
    }

    let mut state_change = 0;
    let mut unreadable: Vec<String> = Vec::new();
    for entry in entries {
        if entry.starts_with("/*") || entry.starts_with("//") {
            continue;
        }
        let (states, rest) = take_leading_states(&entry);
        if !states.is_empty() && !states.iter().any(|state| state == "INITIAL") {
            state_only.push(entry.clone());
            continue;
        }
        let Some((pattern, action)) = split_pattern(rest) else {
            unreadable.push(entry.clone());
            continue;
        };
        // `[a-z]+ IDENT //FIXME`: the action is the token name, and the rest
        // of the line is a comment.
        let action = match action.split_once(char::is_whitespace) {
            Some((word, rest)) if rest.trim_start().starts_with("//") => word,
            Some((word, rest)) if rest.trim_start().starts_with("/*") => word,
            Some(_) => action,
            None => action,
        };
        let (target, pattern) = take_trailing_states(pattern);
        if !target.is_empty() {
            state_change += 1;
        }
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        rules.push((pattern.to_string(), action.trim().to_string()));
    }

    if !state_only.is_empty() {
        notes.push(format!(
            "{} lexer rule(s) only run in a start condition, which tree-sitter cannot \
             express (its lexer has no states); they were left out: {}. If a token below \
             has no body, this is why",
            state_only.len(),
            summarize(&state_only),
        ));
    }
    if state_change > 0 {
        notes.push(format!(
            "{state_change} lexer rule(s) switch to a start condition after matching; the \
             switch was dropped, so the pattern applies in every state"
        ));
    }
    if !unreadable.is_empty() {
        notes.push(format!(
            "{} lexer rule(s) could not be read and were left out: {}",
            unreadable.len(),
            summarize(&unreadable),
        ));
    }
    Ok(rules)
}

/// Up to three rules, for a one-line note.
fn summarize(rules: &[String]) -> String {
    let shown: Vec<String> = rules.iter().take(3).map(|rule| format!("`{rule}`")).collect();
    if rules.len() > shown.len() {
        format!("{} and {} more", shown.join(", "), rules.len() - shown.len())
    } else {
        shown.join(", ")
    }
}

/// Split a Flex rule at the first whitespace that is not inside a character
/// class, a `"…"` string or an escape — a pattern may contain all three, so
/// splitting on the *last* whitespace run turns `… IDENT //FIXME` into a
/// pattern that ends in the token name.
fn split_pattern(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut in_class = false;
    let mut in_string = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            b'"' if !in_class => in_string = !in_string,
            b'[' if !in_string => in_class = true,
            b']' if !in_string => in_class = false,
            b' ' | b'\t' if !in_class && !in_string => {
                let rest = line[index..].trim();
                if rest.is_empty() {
                    return None;
                }
                return Some((line[..index].trim_end(), rest));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// The start conditions a rule names before its pattern: `<INITIAL>`, `<a,b>`,
/// `<>` (none), or `.` (stay where you are), which this dialect writes as `<.>`.
///
/// Only a list of state names counts: `<` and `>` are ordinary pattern
/// characters, and bc.g's `==|<=|>=|!=|<|>` ends with one.
fn take_leading_states(text: &str) -> (Vec<String>, &str) {
    if let Some(after) = text.strip_prefix('<') {
        if let Some(end) = after.find('>') {
            if let Some(states) = states_of(&after[..end]) {
                return (states, &after[end + 1..]);
            }
        }
    }
    (Vec::new(), text)
}

/// The start conditions a rule names after its pattern, i.e. where a match
/// moves the lexer. tree-sitter has no states, so the caller reports the count.
fn take_trailing_states(text: &str) -> (Vec<String>, &str) {
    let trimmed = text.trim_end();
    let Some(start) = trimmed.rfind('<') else {
        return (Vec::new(), text);
    };
    if trimmed[..start].ends_with('\\') {
        return (Vec::new(), text);
    }
    let inner = &trimmed[start + 1..];
    let inner = inner.strip_suffix('>').unwrap_or(inner);
    // `{LBrace}<>TargetLanguageAction>`: ANTLR's .g files write a stray `<>`
    // before the state list, so allow the inner name to be preceded by one `>`.
    let inner = inner.strip_prefix('>').unwrap_or(inner);
    match states_of(inner) {
        Some(states) => (states, trimmed[..start].trim_end()),
        None => (Vec::new(), text),
    }
}

/// `INITIAL`, `A,B`, empty, or `.` — a start-condition list, if the text is one.
fn states_of(text: &str) -> Option<Vec<String>> {
    if text == "." {
        return Some(vec![".".to_string()]);
    }
    if text.is_empty() {
        return Some(Vec::new());
    }
    if !is_state_list(text) {
        return None;
    }
    Some(text.split(',').map(str::to_string).collect())
}

/// `INITIAL` or `A,B`: what Flex allows between the angle brackets that name
/// start conditions.
fn is_state_list(text: &str) -> bool {
    !text.is_empty()
        && text.split(',').all(|state| {
            let mut chars = state.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}
