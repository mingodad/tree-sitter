//! The rule IR — a one-to-one mirror of `RuleJSON` in
//! `crates/generate/src/parse_grammar.rs` — and the lowering pass from the
//! declarative AST to it.

use std::collections::HashSet;

use crate::lexer::{Error, Res, Span};
use crate::parser::{is_valid_field_name, Expr, ExprKind};

#[derive(Clone, Debug)]
pub enum PrecValue {
    Int(i32),
    Name(String),
}

/// Every function name the DSL understands. The `.tsg` printer consults this so
/// that a rule *named* `seq` is written `$seq` instead of `seq`, which would
/// otherwise be parsed as a call as soon as a group follows it.
pub const CALL_NAMES: &[&str] = &[
    "alias",
    "blank",
    "choice",
    "eof",
    "field",
    "optional",
    "prec",
    "prec.dynamic",
    "prec.left",
    "prec.right",
    "repeat",
    "repeat1",
    "reserved",
    "seq",
    "sym",
    "token",
    "token.immediate",
];

#[derive(Clone, Debug)]
pub enum Rule {
    Blank,
    Str(String),
    Pattern {
        value: String,
        flags: Option<String>,
    },
    Symbol(String),
    Choice(Vec<Rule>),
    Seq(Vec<Rule>),
    Repeat(Box<Rule>),
    Repeat1(Box<Rule>),
    Prec {
        value: PrecValue,
        content: Box<Rule>,
    },
    PrecLeft {
        value: PrecValue,
        content: Box<Rule>,
    },
    PrecRight {
        value: PrecValue,
        content: Box<Rule>,
    },
    PrecDynamic {
        value: i32,
        content: Box<Rule>,
    },
    Token(Box<Rule>),
    ImmediateToken(Box<Rule>),
    Alias {
        content: Box<Rule>,
        named: bool,
        value: String,
    },
    Field {
        name: String,
        content: Box<Rule>,
    },
    Reserved {
        context_name: String,
        content: Box<Rule>,
    },
    Eof,
}

pub struct Lowerer<'a> {
    defined: &'a HashSet<String>,
}

impl<'a> Lowerer<'a> {
    pub fn new(defined: &'a HashSet<String>) -> Self {
        Self { defined }
    }

    pub fn lower(&self, expr: &Expr) -> Res<Rule> {
        match &expr.kind {
            ExprKind::Str(value) => Ok(Rule::Str(value.clone())),
            ExprKind::Pattern { value, flags } => Ok(Rule::Pattern {
                value: value.clone(),
                flags: if flags.is_empty() {
                    None
                } else {
                    Some(flags.clone())
                },
            }),
            ExprKind::Number(_) => Err(Error::new(
                expr.span,
                "a bare number is only meaningful as a precedence value, \
                 e.g. `prec(1, ...)`",
            )),
            ExprKind::Symbol(name) => Ok(Rule::Symbol(self.check_symbol(name, expr.span)?)),
            ExprKind::Seq(items) => Ok(Rule::Seq(
                items
                    .iter()
                    .map(|i| self.lower(i))
                    .collect::<Res<Vec<_>>>()?,
            )),
            ExprKind::Choice(items) => Ok(Rule::Choice(
                items
                    .iter()
                    .map(|i| self.lower(i))
                    .collect::<Res<Vec<_>>>()?,
            )),
            ExprKind::Optional(inner) => Ok(Rule::Choice(vec![self.lower(inner)?, Rule::Blank])),
            ExprKind::Repeat(inner) => Ok(Rule::Repeat(Box::new(self.lower(inner)?))),
            ExprKind::Repeat1(inner) => Ok(Rule::Repeat1(Box::new(self.lower(inner)?))),
            ExprKind::Call { name, args } => self.lower_call(expr.span, name, args),
        }
    }

    pub fn check_symbol(&self, name: &str, span: Span) -> Res<String> {
        if self.defined.contains(name) {
            return Ok(name.to_owned());
        }
        let mut candidates: Vec<&str> = self
            .defined
            .iter()
            .map(String::as_str)
            .filter(|d| close_enough(d, name))
            .collect();
        candidates.sort_unstable();
        let hint = match candidates.first() {
            Some(best) => format!(" (did you mean `{best}`?)"),
            None => String::new(),
        };
        Err(Error::new(span, format!("undefined rule `{name}`{hint}")))
    }

    fn lower_call(&self, span: Span, name: &str, args: &[Expr]) -> Res<Rule> {
        match name {
            "seq" => {
                arity_at_least(span, name, args, 1)?;
                // Deliberately *not* flattened: `seq(a, seq(b, c))` keeps its
                // nesting, exactly as `dsl.js` does. tree-sitter's own
                // `normalize` pass is what flattens it, later on.
                Ok(Rule::Seq(
                    args.iter()
                        .map(|a| self.lower(a))
                        .collect::<Res<Vec<_>>>()?,
                ))
            }
            "choice" => {
                // One member is allowed: `grammar.json` may contain a one-member
                // `CHOICE` (`swift` has one), and only the call form can express
                // it, so refusing it here would break the round trip.
                arity_at_least(span, name, args, 1)?;
                Ok(Rule::Choice(
                    args.iter()
                        .map(|a| self.lower(a))
                        .collect::<Res<Vec<_>>>()?,
                ))
            }
            "optional" => {
                arity(span, name, args, 1)?;
                Ok(Rule::Choice(vec![self.lower(&args[0])?, Rule::Blank]))
            }
            "repeat" => {
                arity(span, name, args, 1)?;
                Ok(Rule::Repeat(Box::new(self.lower(&args[0])?)))
            }
            "repeat1" => {
                arity(span, name, args, 1)?;
                Ok(Rule::Repeat1(Box::new(self.lower(&args[0])?)))
            }
            "blank" => {
                arity(span, name, args, 0)?;
                Ok(Rule::Blank)
            }
            "eof" => {
                arity(span, name, args, 0)?;
                Ok(Rule::Eof)
            }
            "prec" => {
                arity(span, name, args, 2)?;
                let value = prec_value(&args[0])?;
                let content = self.lower(&args[1])?;
                Ok(Rule::Prec {
                    value,
                    content: Box::new(content),
                })
            }
            "prec.left" => {
                let (value, content) = self.prec_assoc(span, name, args)?;
                Ok(Rule::PrecLeft {
                    value,
                    content: Box::new(content),
                })
            }
            "prec.right" => {
                let (value, content) = self.prec_assoc(span, name, args)?;
                Ok(Rule::PrecRight {
                    value,
                    content: Box::new(content),
                })
            }
            "prec.dynamic" => {
                arity(span, name, args, 2)?;
                let value = match &args[0].kind {
                    ExprKind::Number(n) => *n,
                    _ => {
                        return Err(Error::new(
                            args[0].span,
                            "`prec.dynamic` requires an integer precedence value",
                        ))
                    }
                };
                Ok(Rule::PrecDynamic {
                    value,
                    content: Box::new(self.lower(&args[1])?),
                })
            }
            "token" => {
                arity(span, name, args, 1)?;
                let content = self.lower(&args[0])?;
                check_token_context(&content, args[0].span)?;
                Ok(Rule::Token(Box::new(content)))
            }
            "token.immediate" => {
                arity(span, name, args, 1)?;
                let content = self.lower(&args[0])?;
                check_token_context(&content, args[0].span)?;
                Ok(Rule::ImmediateToken(Box::new(content)))
            }
            "alias" => {
                arity(span, name, args, 2)?;
                let content = self.lower(&args[0])?;
                let (named, value) = match &args[1].kind {
                    ExprKind::Symbol(sym) => (true, sym.clone()),
                    ExprKind::Str(s) => (false, s.clone()),
                    _ => {
                        return Err(Error::new(
                            args[1].span,
                            "the second argument of `alias` must be a string literal \
                             (anonymous alias) or a rule name (named alias)",
                        ))
                    }
                };
                Ok(Rule::Alias {
                    content: Box::new(content),
                    named,
                    value,
                })
            }
            "field" => {
                arity(span, name, args, 2)?;
                let field_name = match &args[0].kind {
                    ExprKind::Str(s) => s.clone(),
                    ExprKind::Symbol(s) => s.clone(),
                    _ => {
                        return Err(Error::new(
                            args[0].span,
                            "the first argument of `field` must be a field name",
                        ))
                    }
                };
                if !is_valid_field_name(&field_name) {
                    return Err(Error::new(
                        args[0].span,
                        format!(
                            "invalid field name `{field_name}`: field names must start \
                             with a letter or underscore, followed by letters, digits \
                             or underscores"
                        ),
                    ));
                }
                Ok(Rule::Field {
                    name: field_name,
                    content: Box::new(self.lower(&args[1])?),
                })
            }
            "reserved" => {
                arity(span, name, args, 2)?;
                let context_name = match &args[0].kind {
                    ExprKind::Str(s) => s.clone(),
                    ExprKind::Symbol(s) => s.clone(),
                    _ => {
                        return Err(Error::new(
                            args[0].span,
                            "the first argument of `reserved` must be a reserved \
                             word set name",
                        ))
                    }
                };
                Ok(Rule::Reserved {
                    context_name,
                    content: Box::new(self.lower(&args[1])?),
                })
            }
            "sym" => {
                arity(span, name, args, 1)?;
                match &args[0].kind {
                    ExprKind::Str(s) => Ok(Rule::Symbol(self.check_symbol(s, args[0].span)?)),
                    ExprKind::Symbol(s) => Ok(Rule::Symbol(self.check_symbol(s, args[0].span)?)),
                    _ => Err(Error::new(args[0].span, "`sym` expects a rule name")),
                }
            }
            other => Err(Error::new(span, format!("unknown function `{other}`"))),
        }
    }

    fn prec_assoc(&self, span: Span, name: &str, args: &[Expr]) -> Res<(PrecValue, Rule)> {
        match args.len() {
            1 => Ok((PrecValue::Int(0), self.lower(&args[0])?)),
            2 => {
                let value = prec_value(&args[0])?;
                let content = self.lower(&args[1])?;
                Ok((value, content))
            }
            n => Err(Error::new(
                span,
                format!(
                    "`{name}` expects 1 or 2 arguments (an optional precedence value \
                     and a rule), but got {n}"
                ),
            )),
        }
    }
}

fn prec_value(expr: &Expr) -> Res<PrecValue> {
    match &expr.kind {
        ExprKind::Number(n) => Ok(PrecValue::Int(*n)),
        ExprKind::Str(s) => Ok(PrecValue::Name(s.clone())),
        ExprKind::Symbol(s) => Ok(PrecValue::Name(s.clone())),
        _ => Err(Error::new(
            expr.span,
            "a precedence value must be an integer or a precedence name",
        )),
    }
}

fn arity(span: Span, name: &str, args: &[Expr], expected: usize) -> Res<()> {
    if args.len() == expected {
        return Ok(());
    }
    Err(Error::new(
        span,
        format!(
            "`{name}` expects {expected} argument{}, but got {}",
            if expected == 1 { "" } else { "s" },
            args.len()
        ),
    ))
}

fn arity_at_least(span: Span, name: &str, args: &[Expr], minimum: usize) -> Res<()> {
    if args.len() >= minimum {
        return Ok(());
    }
    Err(Error::new(
        span,
        format!(
            "`{name}` expects at least {minimum} argument{}, but got {}",
            if minimum == 1 { "" } else { "s" },
            args.len()
        ),
    ))
}

/// Reject rule references inside `token(...)`, matching the `UnexpectedRule`
/// check in `parse_grammar.rs` but with a source span attached.
pub fn check_token_context(rule: &Rule, span: Span) -> Res<()> {
    match rule {
        Rule::Symbol(name) => Err(Error::new(
            span,
            format!(
                "rule `{name}` cannot be part of a token: tokens are built only from \
                 strings and regexes"
            ),
        )
        .with_help(
            "`token(…)` and the rules in a `tokens { … }` block are lexical, so neither \
             can reference another rule; write the pattern out, or keep the rule a \
             nonterminal",
        )),
        Rule::Choice(members) | Rule::Seq(members) => {
            for member in members {
                check_token_context(member, span)?;
            }
            Ok(())
        }
        Rule::Repeat(inner)
        | Rule::Repeat1(inner)
        | Rule::Token(inner)
        | Rule::ImmediateToken(inner)
        | Rule::Field { content: inner, .. }
        | Rule::Reserved { content: inner, .. }
        | Rule::Alias { content: inner, .. } => check_token_context(inner, span),
        Rule::Prec { content, .. }
        | Rule::PrecLeft { content, .. }
        | Rule::PrecRight { content, .. }
        | Rule::PrecDynamic { content, .. } => check_token_context(content, span),
        Rule::Blank | Rule::Str(_) | Rule::Pattern { .. } | Rule::Eof => Ok(()),
    }
}

/// Cheap "did you mean" helper: same first character and a small edit distance.
fn close_enough(candidate: &str, wanted: &str) -> bool {
    if candidate == wanted {
        return true;
    }
    let (a, b) = (candidate.as_bytes(), wanted.as_bytes());
    if a.first() != b.first() {
        return false;
    }
    fn distance(x: &[u8], y: &[u8]) -> usize {
        let mut prev: Vec<usize> = (0..=y.len()).collect();
        let mut cur = vec![0usize; y.len() + 1];
        for (i, &xc) in x.iter().enumerate() {
            cur[0] = i + 1;
            for (j, &yc) in y.iter().enumerate() {
                let sub = prev[j] + usize::from(xc != yc);
                cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        prev[y.len()]
    }
    distance(a, b) <= 2
}
