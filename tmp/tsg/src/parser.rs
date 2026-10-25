//! The `.tsg` surface syntax: AST and recursive-descent parser.
//!
//! ```text
//! file       := item*
//! item       := directive | rule
//! rule       := IDENT ':' expr ';'
//! directive  := '%name' IDENT ';'
//!             | '%inherits' IDENT ';'
//!             | '%word' IDENT ';'
//!             | '%extra' expr ';'
//!             | '%extras none;'
//!             | '%external' expr ';'
//!             | '%inline' IDENT+ ';'
//!             | '%supertypes' IDENT+ ';'
//!             | '%conflicts' IDENT+ ';'
//!             | '%precedence' (STRING | IDENT)+ ';'
//!             | '%reserved' IDENT '=' expr (',' expr)* ';'
//! expr       := seq ('|' seq)*
//! seq        := postfix*
//! postfix    := primary ('?' | '*' | '+')*
//! primary    := STRING | REGEX | NUMBER | IDENT | '$' IDENT
//!             | CALL '(' (expr (',' expr)*)? ')'
//!             | '(' expr ')'
//! CALL       := one of `rules::CALL_NAMES`
//! ```
//!
//! A call is only recognised for the fixed set of DSL function names. A grammar
//! may declare a rule with one of those names (Ruby has `alias`, MSSQL has
//! `field`); call syntax still wins, so referencing such a rule takes the
//! explicit form `$field`, and `compile` warns about the collision.

use crate::lexer::{is_ident_start, locate, Error, Res, Span, Tok, Token};

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Str(String),
    Pattern { value: String, flags: String },
    Number(i32),
    Symbol(String),
    Seq(Vec<Expr>),
    Choice(Vec<Expr>),
    Optional(Box<Expr>),
    Repeat(Box<Expr>),
    Repeat1(Box<Expr>),
    Call { name: String, args: Vec<Expr> },
}

/// Associativity of a precedence level, as declared on the directive:
/// `%precedence left "add" = '+' '-';`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    Left,
    Right,
}

/// One `%precedence` directive. Two shapes share it, because `grammar.json`
/// needs both and `--from-json` has to be able to print both:
///
/// * `%precedence "call" "pow";` — one *ordering*: a list of names and rule
///   references, comparable with each other and with nothing else. This is the
///   form `to_tsg` emits, since `precedences` is a list of such lists.
/// * `%precedence left "add" = '+' '-';` — one *level* in a single table: a
///   name, its associativity, and the terminals whose productions inherit it.
///   Consecutive levels are emitted as one ordering, highest first, the way
///   `%left`/`%right` stack up in yacc.
#[derive(Debug)]
pub struct PrecedenceGroup {
    pub assoc: Option<Assoc>,
    pub entries: Vec<Expr>,
    pub terminals: Vec<Expr>,
    /// An `=` was written. `%precedence "call" = ;` declares a level that no
    /// terminal inherits, which is how a name that is only ever used by `@` is
    /// placed in the chain. Without it, a bare list is an *ordering*.
    pub has_equals: bool,
    pub span: Span,
}

impl PrecedenceGroup {
    /// Whether this directive is a yacc-style *level* — it says something about
    /// associativity or inheritance — rather than a bare ordering list, which
    /// is what `--from-json` prints and what keeps deliberately incomparable
    /// groups incomparable.
    #[must_use]
    pub const fn is_level(&self) -> bool {
        self.assoc.is_some() || self.has_equals || !self.terminals.is_empty()
    }
}

/// One rule from a `tokens { … }` block.
#[derive(Debug, Clone)]
pub struct TokenRule {
    pub name: String,
    /// The rule's name.
    pub span: Span,
    /// The whole rule, for the "declared lexical here" label.
    pub section: Span,
    /// `NAME: /x/ @immediate;` — no leading extras before this token.
    pub immediate: bool,
}

/// Everything in a `.tsg` file: directives plus ordered rule definitions.
///
/// `rules` is a `Vec`, not a map: rule order is semantic — the first rule is
/// the grammar's start rule.
#[derive(Debug, Default)]
pub struct GrammarAst {
    pub name: Option<(String, Span)>,
    pub word: Option<(String, Span)>,
    /// `%inherits javascript;` — the parent grammar's name. tree-sitter's Rust
    /// pipeline ignores this key (the DSL flattens base grammars in JS), but the
    /// schema has it, so it is preserved rather than dropped.
    pub inherits: Option<(String, Span)>,
    pub extras: Vec<Expr>,
    /// `%extras none;` — an explicitly empty extras list, which the DSL spells
    /// `extras: _ => []`. Without it, an absent `%extra` means the default
    /// `/[\s]/`, exactly as in `dsl.js`.
    pub no_extras: bool,
    pub externals: Vec<Expr>,
    pub inline: Vec<(String, Span)>,
    pub supertypes: Vec<(String, Span)>,
    pub conflicts: Vec<(Vec<String>, Span)>,
    pub precedences: Vec<PrecedenceGroup>,
    pub reserved: Vec<(String, Vec<Expr>, Span)>,
    pub rules: Vec<(String, Expr)>,
    /// Rules declared inside a `tokens { … }` block, in file order. Their
    /// bodies were wrapped in `token(…)` by the parser, so nothing downstream
    /// has to know about sections; this list is what the lexical checks and
    /// `--explain-lexing` read. Membership is *declaration*, not analysis: it is
    /// the author saying "this is one token", which is what makes a mismatch
    /// between the claim and the body a reportable error.
    pub tokens: Vec<TokenRule>,
    /// Parallel to `rules`: the span of each rule's *name*, so downstream
    /// diagnostics (which only know names) can find the definition again. See
    /// [`crate::source::Index`].
    pub rule_names: Vec<(String, Span)>,
}

pub struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    index: usize,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Res<Self> {
        Ok(Self {
            src,
            tokens: crate::lexer::tokenize(src)?,
            index: 0,
        })
    }

    fn cur(&self) -> &Tok {
        &self.tokens[self.index].tok
    }

    fn cur_span(&self) -> Span {
        self.tokens[self.index].span
    }

    fn prev_end(&self) -> usize {
        self.tokens[self.index.saturating_sub(1)].span.end
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.index.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
        token
    }

    fn at(&self, tok: &Tok) -> bool {
        self.cur() == tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.at(tok) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Res<Token> {
        if self.at(&tok) {
            Ok(self.bump())
        } else {
            Err(Error::new(
                self.cur_span(),
                format!("expected {what}, but found {}", self.cur().describe()),
            ))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Res<(String, Span)> {
        match self.cur().clone() {
            Tok::Ident(name) => {
                let span = self.cur_span();
                self.bump();
                Ok((name, span))
            }
            other => Err(Error::new(
                self.cur_span(),
                format!("expected {what}, but found {}", other.describe()),
            )),
        }
    }

    fn expect_semi(&mut self) -> Res<()> {
        if self.eat(&Tok::Semi) {
            return Ok(());
        }
        if self.at(&Tok::Colon) {
            let name = match self.tokens.get(self.index.saturating_sub(1)) {
                Some(Token {
                    tok: Tok::Ident(name),
                    ..
                }) => format!("`{name}:`"),
                _ => "`name:`".to_string(),
            };
            return Err(Error::new(
                self.cur_span(),
                format!(
                    "expected `;` to end the statement, but found `:` — a field \
                     label is written `name= element`, and if {name} starts a new \
                     rule then the rule above is missing its `;`"
                ),
            ));
        }
        if self.at(&Tok::Eq) {
            return Err(Error::new(
                self.cur_span(),
                "expected `;` to end the statement, but found `=` — a label names \
                 one element and does not nest; group the inner one: \
                 `k= (j= 'a')`",
            ));
        }
        let hint = if self.at(&Tok::Eof) {
            " (the statement is not terminated)"
        } else {
            ""
        };
        Err(Error::new(
            self.cur_span(),
            format!(
                "expected `;` to end the statement{hint}, but found {}",
                self.cur().describe()
            ),
        ))
    }

    /// Parse a whole source file.
    pub fn parse(mut self) -> Res<GrammarAst> {
        let mut grammar = GrammarAst::default();

        loop {
            match self.cur().clone() {
                Tok::Eof => break,
                Tok::Directive(directive) => {
                    self.bump();
                    self.parse_directive(&directive, &mut grammar)?;
                }
                Tok::Ident(name) => {
                    self.bump();
                    let keyword = self.prev_span();
                    if name == "tokens" && self.at(&Tok::LCurly) {
                        self.parse_tokens_block(keyword, &mut grammar)?;
                    } else {
                        self.parse_rule(name, None, &mut grammar)?;
                    }
                }
                other => {
                    return Err(Error::new(
                        self.cur_span(),
                        format!(
                            "expected a directive (`%name`, `%extra`, ...), a rule \
                             definition (`name: ...;`) or a `tokens` block, but \
                             found {}",
                            other.describe()
                        ),
                    ))
                }
            }
        }

        Ok(grammar)
    }

    fn parse_directive(&mut self, directive: &str, grammar: &mut GrammarAst) -> Res<()> {
        match directive {
            "name" => {
                let (value, span) = self.expect_ident("a grammar name")?;
                self.expect_semi()?;
                if grammar.name.is_some() {
                    return Err(Error::new(span, "`%name` was already specified"));
                }
                grammar.name = Some((value, span));
            }
            "inherits" => {
                let (value, span) = self.expect_ident("the name of the parent grammar")?;
                self.expect_semi()?;
                if grammar.inherits.is_some() {
                    return Err(Error::new(span, "`%inherits` was already specified"));
                }
                if !value
                    .chars()
                    .next()
                    .is_some_and(crate::lexer::is_ident_start)
                    || !value.chars().all(crate::lexer::is_ident_continue)
                {
                    return Err(Error::new(
                        span,
                        format!("`{value}` is not a valid grammar name"),
                    ));
                }
                grammar.inherits = Some((value, span));
            }
            "word" => {
                let (value, span) = self.expect_ident("the name of the keyword token")?;
                self.expect_semi()?;
                if grammar.word.is_some() {
                    return Err(Error::new(span, "`%word` was already specified"));
                }
                grammar.word = Some((value, span));
            }
            "extra" => {
                let expr = self.parse_expr()?;
                self.expect_semi()?;
                if grammar.no_extras {
                    return Err(Error::new(
                        expr.span,
                        "`%extras none;` was already specified, so `%extra` cannot                          be used as well",
                    ));
                }
                grammar.extras.push(expr);
            }
            "extras" => {
                let span = self.cur_span();
                let (word, word_span) = self.expect_ident("`none`")?;
                if word != "none" {
                    return Err(Error::new(
                        word_span,
                        format!(
                            "expected `none` after `%extras` (use `%extra <rule>;` to                              declare an extra), but found `{word}`"
                        ),
                    ));
                }
                self.expect_semi()?;
                if grammar.no_extras {
                    return Err(Error::new(span, "`%extras none;` was already specified"));
                }
                if !grammar.extras.is_empty() {
                    return Err(Error::new(
                        span,
                        "`%extras none;` cannot be combined with `%extra`",
                    ));
                }
                grammar.no_extras = true;
            }
            "external" => {
                let expr = self.parse_expr()?;
                self.expect_semi()?;
                grammar.externals.push(expr);
            }
            "inline" => {
                let items = self.parse_ident_list("at least one rule name")?;
                grammar.inline.extend(items);
            }
            "supertypes" | "supertype" => {
                let items = self.parse_ident_list("at least one rule name")?;
                grammar.supertypes.extend(items);
            }
            "conflicts" => {
                let span = self.cur_span();
                let mut names = Vec::new();
                while let Tok::Ident(name) = self.cur().clone() {
                    self.bump();
                    names.push(name);
                }
                if names.is_empty() {
                    return Err(Error::new(
                        self.cur_span(),
                        "`%conflicts` expects at least one rule name",
                    ));
                }
                let end = self.prev_end();
                self.expect_semi()?;
                grammar
                    .conflicts
                    .push((names, span.covering(Span::new(end, end))));
            }
            "precedence" | "precedences" => {
                let span = self.cur_span();
                // `left`/`right` are keywords here, so a *rule* of that name has
                // to be written `$left` in a precedence list.
                let assoc =
                    match self.cur().clone() {
                        Tok::Ident(word) => match word.as_str() {
                            "left" => Some(Assoc::Left),
                            "right" => Some(Assoc::Right),
                            "nonassoc" => return Err(Error::new(
                                self.cur_span(),
                                "tree-sitter has no non-associative precedence",
                            )
                            .with_help(
                                "leave the associativity off: `%precedence \"cmp\" = '<' '<=';` \
                                 with none, `a < b < c` is a grammar conflict to resolve, \
                                 rather than a parse error at runtime",
                            )),
                            _ => None,
                        },
                        _ => None,
                    };
                if assoc.is_some() {
                    self.bump();
                }

                let mut entries = Vec::new();
                loop {
                    let entry = match self.cur().clone() {
                        Tok::Str(value) => {
                            let s = self.cur_span();
                            self.bump();
                            Expr {
                                span: s,
                                kind: ExprKind::Str(value),
                            }
                        }
                        Tok::Ident(name) => {
                            let s = self.cur_span();
                            self.bump();
                            Expr {
                                span: s,
                                kind: ExprKind::Symbol(name),
                            }
                        }
                        Tok::Dollar => self.parse_dollar_symbol()?,
                        _ => break,
                    };
                    entries.push(entry);
                }

                // `= '+' '-'`: the terminals whose productions inherit this level.
                let mut terminals = Vec::new();
                let has_equals = self.eat(&Tok::Eq);
                if has_equals {
                    loop {
                        let terminal = match self.cur().clone() {
                            Tok::Str(value) => {
                                let s = self.cur_span();
                                self.bump();
                                Expr {
                                    span: s,
                                    kind: ExprKind::Str(value),
                                }
                            }
                            Tok::Regex(value, flags) => {
                                let s = self.cur_span();
                                self.bump();
                                Expr {
                                    span: s,
                                    kind: ExprKind::Pattern { value, flags },
                                }
                            }
                            Tok::Ident(name) => {
                                let s = self.cur_span();
                                self.bump();
                                Expr {
                                    span: s,
                                    kind: ExprKind::Symbol(name),
                                }
                            }
                            Tok::Dollar => self.parse_dollar_symbol()?,
                            Tok::Semi => break,
                            other => {
                                return Err(Error::new(
                                    self.cur_span(),
                                    format!(
                                        "expected a terminal after `=`, but found {}",
                                        other.describe()
                                    ),
                                )
                                .with_help(
                                    "`=` lists the terminals that inherit this level: \
                                     `%precedence left \"add\" = '+' '-';`",
                                ))
                            }
                        };
                        terminals.push(terminal);
                    }
                }

                if entries.is_empty() {
                    return Err(Error::new(
                        self.cur_span(),
                        "`%precedence` expects at least one precedence name \
                         (a string literal or a rule name)",
                    ));
                }
                if !terminals.is_empty() && entries.len() > 1 {
                    return Err(Error::new(
                        span.covering(self.prev_span()),
                        "a precedence level has exactly one name",
                    )
                    .with_help(
                        "`left`/`right` and `= terminals` describe one level; give each \
                         level its own `%precedence` line, highest first",
                    ));
                }
                let end = self.prev_end();
                self.expect_semi()?;
                grammar.precedences.push(PrecedenceGroup {
                    assoc,
                    entries,
                    terminals,
                    has_equals,
                    span: span.covering(Span::new(end, end)),
                });
            }
            "reserved" => {
                let (wordset, span) = self.expect_ident("the reserved word set name")?;
                self.expect(Tok::Eq, "`=` after the word set name")?;
                // `%reserved name = ;` is an explicitly empty set, which the
                // reverse direction needs (`js` has one).
                let mut rules = Vec::new();
                if !self.at(&Tok::Semi) {
                    rules.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        rules.push(self.parse_expr()?);
                    }
                }
                let end = self.prev_end();
                self.expect_semi()?;
                if grammar.reserved.iter().any(|(n, ..)| *n == wordset) {
                    return Err(Error::new(
                        span,
                        format!("reserved word set `{wordset}` was already defined"),
                    ));
                }
                grammar
                    .reserved
                    .push((wordset, rules, span.covering(Span::new(end, end))));
            }
            other => {
                return Err(Error::new(
                    self.cur_span(),
                    format!(
                        "unknown directive `%{other}` (known: %name, %word, %extra, \
                         %external, %inline, %supertypes, %conflicts, %precedence, \
                         %reserved)"
                    ),
                ))
            }
        }
        Ok(())
    }

    fn parse_ident_list(&mut self, what: &str) -> Res<Vec<(String, Span)>> {
        let mut items = Vec::new();
        while let Tok::Ident(name) = self.cur().clone() {
            let span = self.cur_span();
            self.bump();
            items.push((name, span));
        }
        if items.is_empty() {
            return Err(Error::new(
                self.cur_span(),
                format!("expected {what}, but found {}", self.cur().describe()),
            ));
        }
        self.expect_semi()?;
        Ok(items)
    }

    fn parse_rule(
        &mut self,
        name: String,
        section: Option<Span>,
        grammar: &mut GrammarAst,
    ) -> Res<()> {
        let name_span = self.tokens[self.index - 1].span;
        if let Some((_, first)) = grammar.rules.iter().find(|(n, _)| *n == name) {
            let (line, col, ..) = locate(self.src, first.span.start);
            return Err(Error::new(
                name_span,
                format!("rule `{name}` is already defined at line {line}, column {col}"),
            ));
        }
        self.expect(Tok::Colon, "`:` after the rule name")?;
        let body = self.parse_expr()?;
        self.expect_semi()?;
        let body = match section {
            // Every rule in a `tokens` block is one token, so the block *is* the
            // spelling of `token(…)`: the parser wraps the body, and `to_tsg`
            // and every later pass keep seeing the call they already understood.
            Some(section) => {
                let immediate = is_immediate_call(&body);
                let body = if immediate || is_token_call(&body) {
                    body
                } else {
                    let span = body.span;
                    Expr {
                        span,
                        kind: ExprKind::Call {
                            name: "token".to_string(),
                            args: vec![body],
                        },
                    }
                };
                grammar.tokens.push(TokenRule {
                    name: name.clone(),
                    span: name_span,
                    section,
                    immediate,
                });
                body
            }
            None => body,
        };
        grammar.rules.push((name.clone(), body));
        grammar.rule_names.push((name, name_span));
        Ok(())
    }

    /// `tokens { NAME: …; NUMBER: …; }` — the lexical section.
    ///
    /// Rules inside are pushed as they are parsed, so the file's rule order is
    /// preserved: that order is tree-sitter's token priority (equal-length
    /// matches are resolved by declaration order), and a section must be able to
    /// say so rather than hide it.
    fn parse_tokens_block(&mut self, keyword: Span, grammar: &mut GrammarAst) -> Res<()> {
        let open = self.cur_span();
        self.bump(); // `{`
        while !self.at(&Tok::RCurly) {
            match self.cur().clone() {
                Tok::Eof => {
                    return Err(Error::new(
                        open,
                        "this `tokens` block is never closed; add a `}` after the last \
                         token rule",
                    ))
                }
                Tok::Ident(name) => {
                    self.bump();
                    self.parse_rule(name, Some(keyword), grammar)?;
                }
                other => {
                    return Err(Error::new(
                        self.cur_span(),
                        format!(
                            "expected a token rule (`name: ...;`) or `}}`, but found {}",
                            other.describe()
                        ),
                    ))
                }
            }
        }
        self.bump(); // `}`
        Ok(())
    }

    fn parse_expr(&mut self) -> Res<Expr> {
        let first = self.parse_branch()?;
        if !self.at(&Tok::Pipe) {
            return Ok(first);
        }
        let start = first.span.start;
        let mut members = vec![first];
        while self.eat(&Tok::Pipe) {
            members.push(self.parse_branch()?);
        }
        let end = members.last().map_or(start, |e| e.span.end);
        Ok(Expr {
            span: Span::new(start, end),
            kind: ExprKind::Choice(members),
        })
    }

    fn starts_primary(&self) -> bool {
        matches!(
            self.cur(),
            Tok::Str(_)
                | Tok::Regex(..)
                | Tok::Number(_)
                | Tok::Ident(_)
                | Tok::Dollar
                | Tok::Lparen
        )
    }

    fn parse_seq(&mut self) -> Res<Expr> {
        let mut members = Vec::new();
        while self.starts_primary() {
            let before = self.index;
            members.push(self.parse_labelled()?);
            debug_assert!(self.index > before, "parser must make progress");
        }
        match members.len() {
            0 => Err(Error::new(
                self.cur_span(),
                format!(
                    "expected an expression (a string, a regex, a rule name or `(`), \
                     but found {}",
                    self.cur().describe()
                ),
            )),
            1 => Ok(members.pop().expect("length 1")),
            _ => {
                let start = members[0].span.start;
                let end = members.last().map_or(start, |e| e.span.end);
                Ok(Expr {
                    span: Span::new(start, end),
                    kind: ExprKind::Seq(members),
                })
            }
        }
    }

    /// An alternative, optionally annotated: `seq ('@' precedence)?`.
    ///
    /// The annotation applies to the whole alternative, because that is what a
    /// production's precedence is — `exp '+' exp @left(add)` marks the sum, not
    /// the last `exp`. It desugars to `prec`/`prec.left`/`prec.right`/
    /// `prec.dynamic`, so lowering and the emitted JSON are unaffected.
    fn parse_branch(&mut self) -> Res<Expr> {
        let expr = self.parse_seq()?;
        if !self.at(&Tok::At) {
            return Ok(expr);
        }
        self.bump();
        let (name, value) = match self.cur().clone() {
            Tok::Number(n) => {
                self.bump();
                (
                    "prec",
                    Expr {
                        span: self.prev_span(),
                        kind: ExprKind::Number(n),
                    },
                )
            }
            // `@immediate` marks a token that may not be preceded by extras —
            // `token.immediate` in `grammar.js`. It is a *whole-production*
            // annotation like `@left(…)`, and it desugars in the parser, so the
            // `tokens` block can simply ask whether a body already carries it.
            Tok::Ident(found) if found == "immediate" => {
                self.bump();
                if self.at(&Tok::Lparen) {
                    return Err(Error::new(
                        self.cur_span(),
                        "`@immediate` takes no argument; it marks the token it is \
                         written after",
                    ));
                }
                return Ok(Expr {
                    span: Span::new(expr.span.start, self.prev_span().end),
                    kind: ExprKind::Call {
                        name: "token.immediate".to_string(),
                        args: vec![expr],
                    },
                });
            }
            Tok::Ident(found) if matches!(found.as_str(), "left" | "right" | "dynamic") => {
                self.bump();
                if !self.at(&Tok::Lparen) {
                    // `left`/`right`/`dynamic` are reserved after `@`, so a bare
                    // `@left` is a forgotten argument list rather than a name.
                    return Err(Error::new(
                        self.cur_span(),
                        format!(
                            "`@{found}` needs an argument list: `@{found}(1)` or \
                             `@{found}(name)`; a precedence *name* spelled `{found}` \
                             would have to be quoted where it is declared, and is \
                             then still unspellable here — rename it"
                        ),
                    ));
                }
                let (args, end) = self.parse_args()?;
                let value = match args.len() {
                    1 => args.into_iter().next().expect("length 1"),
                    n => {
                        return Err(Error::new(
                            Span::new(self.prev_span().start, end),
                            format!("`@{found}` takes one precedence value, but got {n}"),
                        ))
                    }
                };
                (
                    match found.as_str() {
                        "left" => "prec.left",
                        "right" => "prec.right",
                        _ => "prec.dynamic",
                    },
                    value,
                )
            }
            Tok::Ident(found) => {
                self.bump();
                (
                    "prec",
                    Expr {
                        span: self.prev_span(),
                        kind: ExprKind::Symbol(found),
                    },
                )
            }
            other => {
                return Err(Error::new(
                    self.cur_span(),
                    format!(
                        "expected a precedence after `@` (a name, an integer, or \
                         `@left(…)` / `@right(…)` / `@dynamic(…)`), but found {}",
                        other.describe()
                    ),
                ))
            }
        };
        let span = Span::new(expr.span.start, self.prev_span().end);
        Ok(Expr {
            span,
            kind: ExprKind::Call {
                name: name.to_string(),
                args: vec![value, expr],
            },
        })
    }

    /// `name= element` — a field label on one element of a sequence.
    ///
    /// It desugars to `field('name', element)`, the same way `@` desugars to
    /// `prec`: the parser is the only place that knows the sugar. It binds to the
    /// element to its right, not to the rest of the branch, so `k= a b` is
    /// `field('k', a) b` and `k= a | b` labels `a` in the first alternative only
    /// — writing `field('k', a)` was the only way to say this before, and it is
    /// the way `--from-json` still prints it.
    ///
    /// The spelling is `=` and not `:` on purpose. `name: body;` is a rule
    /// definition, so a *missing* `;` in front of a line that looks like `x: y`
    /// would be silently read as a label; with `=` the two forms are spelled
    /// differently and that mistake stays the error it was.
    fn parse_labelled(&mut self) -> Res<Expr> {
        let label = match (self.cur(), self.tokens.get(self.index + 1)) {
            (Tok::Ident(name), Some(Token { tok: Tok::Eq, .. })) => name.clone(),
            _ => return self.parse_postfix(),
        };
        let label_span = self.cur_span();
        self.bump();
        self.bump();
        let inner = self.parse_postfix()?;
        let span = Span::new(label_span.start, inner.span.end);
        Ok(Expr {
            span,
            kind: ExprKind::Call {
                name: "field".to_string(),
                args: vec![
                    Expr {
                        span: label_span,
                        kind: ExprKind::Str(label),
                    },
                    inner,
                ],
            },
        })
    }

    fn parse_postfix(&mut self) -> Res<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            let (which, end) = match self.cur() {
                Tok::Question => (0u8, self.cur_span().end),
                Tok::Star => (1, self.cur_span().end),
                Tok::Plus => (2, self.cur_span().end),
                _ => break,
            };
            self.bump();
            let span = Span::new(expr.span.start, end);
            let inner = Box::new(expr);
            expr = Expr {
                span,
                kind: match which {
                    0 => ExprKind::Optional(inner),
                    1 => ExprKind::Repeat(inner),
                    _ => ExprKind::Repeat1(inner),
                },
            };
        }
        Ok(expr)
    }

    /// `$name` — the explicit symbol form, which a rule whose name is also a DSL
    /// function (`field`, `alias`, `token`, …) needs in order to be referenced.
    fn parse_dollar_symbol(&mut self) -> Res<Expr> {
        let start = self.cur_span();
        self.bump();
        let (name, span) = self.expect_ident("a rule name after `$`")?;
        Ok(Expr {
            span: start.covering(span),
            kind: ExprKind::Symbol(name),
        })
    }

    fn parse_primary(&mut self) -> Res<Expr> {
        let token = self.bump();
        match token.tok {
            Tok::Str(value) => Ok(Expr {
                span: token.span,
                kind: ExprKind::Str(value),
            }),
            Tok::Regex(value, flags) => Ok(Expr {
                span: token.span,
                kind: ExprKind::Pattern { value, flags },
            }),
            Tok::Number(value) => Ok(Expr {
                span: token.span,
                kind: ExprKind::Number(value),
            }),
            Tok::Dollar => {
                let (name, span) = self.expect_ident("a rule name after `$`")?;
                Ok(Expr {
                    span: token.span.covering(span),
                    kind: ExprKind::Symbol(name),
                })
            }
            Tok::Ident(mut name) => {
                // Allow dotted call names such as `prec.left`.
                while self.at(&Tok::Dot) {
                    self.bump();
                    let (suffix, _) = self.expect_ident("a name after `.`")?;
                    name.push('.');
                    name.push_str(&suffix);
                }
                if self.at(&Tok::Lparen) && crate::rules::CALL_NAMES.contains(&name.as_str()) {
                    let (args, end) = self.parse_args()?;
                    Ok(Expr {
                        span: Span::new(token.span.start, end),
                        kind: ExprKind::Call { name, args },
                    })
                } else if name.contains('.') {
                    Err(Error::new(
                        token.span,
                        format!("`{name}` is not a known function"),
                    ))
                } else {
                    Ok(Expr {
                        span: token.span,
                        kind: ExprKind::Symbol(name),
                    })
                }
            }
            Tok::Lparen if self.at(&Tok::Rparen) => {
                // `()` is the blank rule: an empty alternative, which is
                // `blank()` and not something the EBNF multipliers can say.
                let end = self.cur_span().end;
                self.bump();
                Ok(Expr {
                    span: Span::new(token.span.start, end),
                    kind: ExprKind::Call {
                        name: "blank".to_owned(),
                        args: Vec::new(),
                    },
                })
            }
            Tok::Lparen => {
                let inner = self.parse_expr()?;
                self.expect(Tok::Rparen, "`)` to close the group")?;
                let end = self.prev_end();
                // Grouping is purely syntactic: keep the inner expression but
                // widen its span so diagnostics cover the parentheses.
                Ok(Expr {
                    span: Span::new(token.span.start, end),
                    kind: inner.kind,
                })
            }
            other => Err(Error::new(
                token.span,
                format!(
                    "expected an expression (a string, a regex, a rule name or `(`), \
                     but found {}",
                    other.describe()
                ),
            )),
        }
    }

    fn parse_args(&mut self) -> Res<(Vec<Expr>, usize)> {
        self.bump(); // '('
        let mut args = Vec::new();
        if !self.at(&Tok::Rparen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
                // Allow a trailing comma in multi-line argument lists.
                if self.at(&Tok::Rparen) {
                    break;
                }
            }
        }
        let close = match self.cur() {
            Tok::Rparen => self.cur_span(),
            other => {
                return Err(Error::new(
                    self.cur_span(),
                    format!("expected `,` or `)`, but found {}", other.describe()),
                ))
            }
        };
        self.bump();
        Ok((args, close.end))
    }
}

/// Whether `expr` is a `token(…)` call.
pub fn is_token_call(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { name, args } if name == "token" && args.len() == 1)
}

/// Whether `expr` is a `token.immediate(…)` call.
pub fn is_immediate_call(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { name, args } if name == "token.immediate" && args.len() == 1)
}

/// Whether a name is a legal `field` name.
pub fn is_valid_field_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if is_ident_start(c) => {}
        _ => return false,
    }
    chars.all(crate::lexer::is_ident_continue)
}
