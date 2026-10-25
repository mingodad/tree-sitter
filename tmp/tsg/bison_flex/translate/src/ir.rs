//! The intermediate form: what the `.g` file says, before any translation
//! decision. Reading produces this; `emit` turns it into `.tsg`.
//!
//! Everything that is a *name* in the source (a token, a literal, a rule) is
//! kept as the text it was written as, so `'+'` and `PLUS` stay distinct and a
//! rule body can be compared against the declaration without normalizing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    Left,
    Right,
    Nonassoc,
    /// `%precedence`: a level name, no associativity.
    None,
}

#[derive(Clone, Debug)]
pub enum Directive {
    /// `%token a 'b' <c>`: one entry per symbol, `<type>` tags dropped (they
    /// are Bison type annotations, not grammar symbols).
    Token(Vec<String>),
    /// `%left` / `%right` / `%nonassoc` / `%precedence`, with the terminals of
    /// that level.
    Assoc { assoc: Assoc, terms: Vec<String> },
    /// `%start name`.
    Start(String),
    /// `%fallback FALLBACK tok…`: in a state where the parser cannot use one of
    /// `tok…`, the scanner returns `FALLBACK` instead.
    Fallback { target: String, tokens: Vec<String> },
    /// `%token_indent`, `%token_symbol_table`, … The metagrammar recognizes
    /// these but nothing translates them yet.
    Unsupported(String),
}

/// One atom of a production body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Atom {
    /// A terminal or a non-terminal, as written.
    Plain(String),
    /// `x?` written in the source (a postfix operator, not a nullable rule).
    Opt(String),
}

impl Atom {
    pub fn name(&self) -> &str {
        match self {
            Self::Plain(name) | Self::Opt(name) => name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Alt {
    pub atoms: Vec<Atom>,
    pub prec: Option<String>,
}

impl Alt {
    /// An alternative with no symbols, i.e. `%empty`.
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub lhs: String,
    pub alts: Vec<Alt>,
}

/// A balanced region recognised by a start condition, from the idiom
///
/// ```text
/// <X> { "{"<>X>          re-enter X (push; no token)
///       "}"<<> TOKEN     pop; return TOKEN
///       (?s:.)<.>   }    anything else keeps the state
/// ```
///
/// This is the one shape a stateful Flex lexer has that tree-sitter *can*
/// express: the depth is local to the scanner, so the state Flex pushes cannot
/// leak into the parser. `tsg` writes it as `%external TOKEN = delimited(…)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pushdown {
    /// The token the closing delimiter returns.
    pub token: String,
    pub open: char,
    pub close: char,
}

/// The Flex half of the file, still line-oriented.
///
/// C actions cannot be parsed by a context-free grammar without an opaque
/// scanner, so `grammar.tsg` stops at `LexerLine`: one node per non-empty
/// line. The lines are split here, where a wrong split can be reported
/// against the line it came from.
#[derive(Clone, Debug, Default)]
pub struct Lexer {
    /// `name pattern`, from the definitions section.
    pub macros: Vec<(String, String)>,
    /// `pattern action`, from the rules section.
    pub rules: Vec<(String, String)>,
    /// Start-condition groups that are balanced regions, in source order.
    pub pushdown: Vec<Pushdown>,
    /// `%option caseless`, i.e. keywords are case-insensitive.
    pub caseless: bool,
    /// What the reader could not express, in the source's own terms: rules that
    /// only run in a start condition, patterns that could not be read. Reported
    /// instead of dropped — a rule that disappears silently makes the
    /// translator's later "the lexer never returns X" note blame the source for
    /// a token it does return.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Grammar {
    pub directives: Vec<Directive>,
    pub rules: Vec<Rule>,
    pub lexer: Lexer,
}
