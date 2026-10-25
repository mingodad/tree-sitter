//! Source spans, diagnostics, and the hand-written lexer.

use std::fmt::Write as _;

/// A half-open byte range into the source text. Every token and AST node
/// carries one so diagnostics can point at the exact offending text — the
/// thing the `grammar.json` front-end fundamentally cannot do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn covering(self, other: Self) -> Self {
        Self::new(self.start.min(other.start), self.end.max(other.end))
    }
}

#[derive(Debug)]
pub struct Error {
    pub span: Span,
    pub msg: String,
    /// An optional `= help:` line, the way rustc suggests a fix.
    pub help: Option<String>,
}

impl Error {
    pub fn new(span: Span, msg: impl Into<String>) -> Self {
        Self {
            span,
            msg: msg.into(),
            help: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

pub type Res<T> = Result<T, Error>;

/// Turn a byte offset into `(line, column, line text, line start offset)`.
/// `line` and `column` are 1-based.
pub fn locate(src: &str, offset: usize) -> (usize, usize, &str, usize) {
    let offset = offset.min(src.len());
    let mut line_no = 1usize;
    let mut line_start = 0usize;
    for (i, c) in src.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line_no += 1;
            line_start = i + 1;
        }
    }
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |p| line_start + p);
    let line_text = &src[line_start..line_end];
    let col_no = src[line_start..offset].chars().count() + 1;
    (line_no, col_no, line_text, line_start)
}

/// Render an error as a `rustc`-style diagnostic:
///
/// ```text
/// error: undefined rule `identifer`
///   --> grammar.tsg:12:14
///    |
/// 12 | _value: object | identifer;
///    |                  ^^^^^^^^^
/// ```
pub fn render_error(src: &str, path: &str, err: &Error) -> String {
    render_diagnostic(src, path, "error", err)
}

/// Warnings are not fatal: the compile still succeeds and the JSON is written.
pub fn render_warnings(src: &str, path: &str, warnings: &[Error]) -> String {
    let mut out = String::new();
    for err in warnings {
        out.push_str(&render_diagnostic(src, path, "warning", err));
    }
    out
}

fn render_diagnostic(src: &str, path: &str, kind: &str, err: &Error) -> String {
    let label = crate::source::Label::primary(err.span, "");
    crate::source::render(src, path, kind, &err.msg, &[label], err.help.as_deref())
}

/// Render several diagnostics, in source order, with a summary. Compilation
/// stops at the first *parse* error, but every rule can be lowered, so a
/// 1000-line grammar should not need one run per mistake.
pub fn render_errors(src: &str, path: &str, errors: &[Error]) -> String {
    const LIMIT: usize = 10;

    let mut sorted: Vec<&Error> = errors.iter().collect();
    sorted.sort_by_key(|err| (err.span.start, err.span.end));

    let mut out = String::new();
    for err in sorted.iter().take(LIMIT) {
        out.push_str(&render_error(src, path, err));
    }
    if sorted.len() > 1 {
        let _ = write!(
            out,
            "\nerror: aborting due to {} previous errors",
            sorted.len()
        );
        if sorted.len() > LIMIT {
            let _ = write!(out, " ({} not shown)", sorted.len() - LIMIT);
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    Str(String),
    /// Pattern source and flags, kept verbatim (backslashes preserved).
    Regex(String, String),
    Number(i32),
    /// `%name` — payload is the directive name without the `%`.
    Directive(String),
    Colon,
    Semi,
    Pipe,
    Lparen,
    Rparen,
    /// `{` — opens a `tokens { … }` block.
    LCurly,
    /// `}` — closes it.
    RCurly,
    Comma,
    Question,
    Star,
    Plus,
    Dot,
    Dollar,
    /// `@` — a postfix annotation: `exp '+' exp @left(add)`.
    At,
    Eq,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(s) => format!("`{s}`"),
            Self::Str(_) => "a string literal".to_owned(),
            Self::Regex(..) => "a regular expression".to_owned(),
            Self::Number(n) => format!("`{n}`"),
            Self::Directive(d) => format!("`%{d}`"),
            Self::Colon => "`:`".to_owned(),
            Self::Semi => "`;`".to_owned(),
            Self::Pipe => "`|`".to_owned(),
            Self::Lparen => "`(`".to_owned(),
            Self::Rparen => "`)`".to_owned(),
            Self::LCurly => "`{`".to_owned(),
            Self::RCurly => "`}`".to_owned(),
            Self::Comma => "`,`".to_owned(),
            Self::Question => "`?`".to_owned(),
            Self::Star => "`*`".to_owned(),
            Self::Plus => "`+`".to_owned(),
            Self::Dot => "`.`".to_owned(),
            Self::Dollar => "`$`".to_owned(),
            Self::At => "`@`".to_owned(),
            Self::Eq => "`=`".to_owned(),
            Self::Eof => "end of file".to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

pub fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn cur(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn nth(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.cur()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_trivia(&mut self) -> Res<()> {
        loop {
            match self.cur() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.nth(1) == Some('/') => {
                    while let Some(c) = self.cur() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.nth(1) == Some('*') => {
                    let start = self.pos;
                    self.bump();
                    self.bump();
                    loop {
                        match self.cur() {
                            None => {
                                return Err(Error::new(
                                    Span::new(start, self.pos),
                                    "unterminated block comment",
                                ))
                            }
                            Some('*') if self.nth(1) == Some('/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn ident_body(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.cur() {
            if is_ident_continue(c) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        s
    }

    fn next_token(&mut self) -> Res<Token> {
        self.skip_trivia()?;
        let start = self.pos;
        let c = match self.cur() {
            None => {
                return Ok(Token {
                    tok: Tok::Eof,
                    span: Span::new(start, start),
                })
            }
            Some(c) => c,
        };

        let tok = match c {
            '%' => {
                self.bump();
                let name = self.ident_body();
                if name.is_empty() {
                    return Err(Error::new(
                        Span::new(start, self.pos),
                        "expected a directive name after `%`",
                    ));
                }
                Tok::Directive(name)
            }
            '\'' | '"' => return self.lex_string(c),
            '/' => return self.lex_regex(),
            '0'..='9' => self.lex_number(false)?,
            '-' => {
                self.bump();
                self.lex_number(true)?
            }
            ':' => {
                self.bump();
                Tok::Colon
            }
            ';' => {
                self.bump();
                Tok::Semi
            }
            '|' => {
                self.bump();
                Tok::Pipe
            }
            '(' => {
                self.bump();
                Tok::Lparen
            }
            ')' => {
                self.bump();
                Tok::Rparen
            }
            '{' => {
                self.bump();
                Tok::LCurly
            }
            '}' => {
                self.bump();
                Tok::RCurly
            }
            ',' => {
                self.bump();
                Tok::Comma
            }
            '?' => {
                self.bump();
                Tok::Question
            }
            '*' => {
                self.bump();
                Tok::Star
            }
            '+' => {
                self.bump();
                Tok::Plus
            }
            '.' => {
                self.bump();
                Tok::Dot
            }
            '$' => {
                self.bump();
                Tok::Dollar
            }
            '@' => {
                self.bump();
                Tok::At
            }
            '=' => {
                self.bump();
                Tok::Eq
            }
            c if is_ident_start(c) => Tok::Ident(self.ident_body()),
            other => {
                let end = self.pos + other.len_utf8();
                self.pos = end;
                return Err(Error::new(
                    Span::new(start, end),
                    format!("unexpected character `{other}`"),
                ));
            }
        };

        Ok(Token {
            tok,
            span: Span::new(start, self.pos),
        })
    }

    fn lex_number(&mut self, negative: bool) -> Res<Tok> {
        let start = self.pos - usize::from(negative);
        let mut digits = String::new();
        while let Some(c) = self.cur() {
            if c.is_ascii_digit() {
                digits.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            return Err(Error::new(
                Span::new(start, self.pos),
                "expected a number after `-`",
            ));
        }
        let magnitude: i64 = digits.parse().map_err(|_| {
            Error::new(
                Span::new(start, self.pos),
                "precedence value does not fit in a 32-bit integer",
            )
        })?;
        let value = if negative { -magnitude } else { magnitude };
        let value = i32::try_from(value).map_err(|_| {
            Error::new(
                Span::new(start, self.pos),
                "precedence value does not fit in a 32-bit integer",
            )
        })?;
        Ok(Tok::Number(value))
    }

    fn lex_string(&mut self, quote: char) -> Res<Token> {
        let start = self.pos;
        self.bump(); // opening quote
        let mut out = String::new();

        loop {
            let c = match self.bump() {
                None => {
                    return Err(Error::new(
                        Span::new(start, self.pos),
                        "unterminated string literal",
                    ))
                }
                Some(c) => c,
            };
            match c {
                '\\' => {
                    let escape_start = self.pos - 1;
                    let e = match self.bump() {
                        None => {
                            return Err(Error::new(
                                Span::new(start, self.pos),
                                "unterminated escape sequence",
                            ))
                        }
                        Some(e) => e,
                    };
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '0' => out.push('\0'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'v' => out.push('\u{b}'),
                        '\\' => out.push('\\'),
                        '\'' => out.push('\''),
                        '"' => out.push('"'),
                        '/' => out.push('/'),
                        'x' => {
                            let cp = self.lex_fixed_hex(2).ok_or_else(|| {
                                Error::new(
                                    Span::new(escape_start, self.pos),
                                    "`\\x` must be followed by exactly two hex digits",
                                )
                            })?;
                            out.push(cp);
                        }
                        'u' => {
                            if let Some(c) = self.lex_unicode_escape(escape_start)? {
                                out.push(c);
                            }
                        }
                        other => {
                            // Unknown escape: preserve it verbatim rather than
                            // silently dropping the backslash.
                            out.push('\\');
                            out.push(other);
                        }
                    }
                }
                c if c == quote => break,
                other => out.push(other),
            }
        }

        Ok(Token {
            tok: Tok::Str(out),
            span: Span::new(start, self.pos),
        })
    }

    /// Handle `\uXXXX`, `\u{...}`, and UTF-16 surrogate pairs.
    fn lex_unicode_escape(&mut self, escape_start: usize) -> Res<Option<char>> {
        if self.cur() == Some('{') {
            self.bump();
            let mut hex = String::new();
            while let Some(c) = self.cur() {
                if c == '}' {
                    break;
                }
                hex.push(c);
                self.bump();
            }
            if self.bump() != Some('}') {
                return Err(Error::new(
                    Span::new(escape_start, self.pos),
                    "unterminated `\\u{...}` escape sequence",
                ));
            }
            return u32::from_str_radix(&hex, 16)
                .ok()
                .and_then(char::from_u32)
                .map(Some)
                .ok_or_else(|| {
                    Error::new(
                        Span::new(escape_start, self.pos),
                        "invalid `\\u{...}` escape sequence",
                    )
                });
        }

        let Some(first) = self.lex_fixed_hex(4) else {
            return Err(Error::new(
                Span::new(escape_start, self.pos),
                "`\\u` must be followed by exactly four hex digits",
            ));
        };
        let hi = first as u32;
        if (0xD800..0xDC00).contains(&hi) {
            // High surrogate: look for the matching low surrogate.
            let save = self.pos;
            if self.cur() == Some('\\') && self.nth(1) == Some('u') {
                self.bump();
                self.bump();
                if let Some(lo) = self.lex_fixed_hex(4) {
                    let lo = lo as u32;
                    if (0xDC00..0xE000).contains(&lo) {
                        let combined = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                        return char::from_u32(combined).map(Some).ok_or_else(|| {
                            Error::new(Span::new(escape_start, self.pos), "invalid surrogate pair")
                        });
                    }
                }
            }
            self.pos = save;
            return Err(Error::new(
                Span::new(escape_start, self.pos),
                "lone high surrogate in `\\u` escape sequence",
            ));
        }
        Ok(char::from_u32(hi))
    }

    /// Consume exactly `n` hex digits, returning the resulting `char`.
    fn lex_fixed_hex(&mut self, n: usize) -> Option<char> {
        let mut value: u32 = 0;
        for _ in 0..n {
            let c = self.cur()?;
            let d = c.to_digit(16)?;
            value = value * 16 + d;
            self.bump();
        }
        char::from_u32(value)
    }

    fn lex_regex(&mut self) -> Res<Token> {
        let start = self.pos;
        self.bump(); // '/'
        let mut value = String::new();
        let mut in_class = false;

        loop {
            let c = match self.bump() {
                None => {
                    return Err(Error::new(
                        Span::new(start, self.pos),
                        "unterminated regular expression",
                    ))
                }
                Some(c) => c,
            };
            match c {
                '\\' => {
                    value.push('\\');
                    match self.bump() {
                        None => {
                            return Err(Error::new(
                                Span::new(start, self.pos),
                                "unterminated regular expression",
                            ))
                        }
                        Some(n) => value.push(n),
                    }
                }
                '[' => {
                    in_class = true;
                    value.push(c);
                }
                ']' => {
                    in_class = false;
                    value.push(c);
                }
                '/' if !in_class => break,
                '\n' => {
                    return Err(Error::new(
                        Span::new(start, self.pos),
                        "unterminated regular expression (newlines must be escaped)",
                    ))
                }
                other => value.push(other),
            }
        }

        let mut flags = String::new();
        while let Some(c) = self.cur() {
            if c.is_ascii_alphabetic() {
                flags.push(c);
                self.bump();
            } else {
                break;
            }
        }

        Ok(Token {
            tok: Tok::Regex(value, flags),
            span: Span::new(start, self.pos),
        })
    }
}

pub fn tokenize(src: &str) -> Res<Vec<Token>> {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        let done = token.tok == Tok::Eof;
        tokens.push(token);
        if done {
            break;
        }
    }
    Ok(tokens)
}
