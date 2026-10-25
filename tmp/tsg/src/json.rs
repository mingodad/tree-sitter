//! A minimal JSON value, pretty-printer, and the `Rule` -> `RuleJSON`
//! serialization. Objects are ordered vectors because `grammar.json` key
//! order is meaningful (a `serde_json` feature the DSL relies on too: the
//! `preserve_order` flag is enabled in the workspace manifest).

use std::fmt::Write as _;

use crate::rules::{PrecValue, Rule};

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Str(String),
    Num(i64),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    pub fn obj(entries: Vec<(&str, Json)>) -> Self {
        Self::Obj(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }
}

fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_string(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn write_json(value: &Json, out: &mut String, level: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => {
            let _ = write!(out, "{n}");
        }
        Json::Str(s) => write_string(s, out),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                push_indent(out, level + 1);
                write_json(item, out, level + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, level);
            out.push(']');
        }
        Json::Obj(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (key, item)) in entries.iter().enumerate() {
                push_indent(out, level + 1);
                write_string(key, out);
                out.push_str(": ");
                write_json(item, out, level + 1);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, level);
            out.push('}');
        }
    }
}

pub fn to_string(value: &Json) -> String {
    let mut out = String::new();
    write_json(value, &mut out, 0);
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Rule -> JSON
// ---------------------------------------------------------------------------

pub fn json_rule(rule: &Rule) -> Json {
    match rule {
        Rule::Blank => Json::obj(vec![("type", Json::str("BLANK"))]),
        Rule::Eof => Json::obj(vec![("type", Json::str("EOF"))]),
        Rule::Str(value) => Json::obj(vec![
            ("type", Json::str("STRING")),
            ("value", Json::str(value)),
        ]),
        Rule::Pattern { value, flags } => {
            let mut entries = vec![
                ("type".to_owned(), Json::str("PATTERN")),
                ("value".to_owned(), Json::str(value)),
            ];
            if let Some(flags) = flags {
                entries.push(("flags".to_owned(), Json::str(flags)));
            }
            Json::Obj(entries)
        }
        Rule::Symbol(name) => Json::obj(vec![
            ("type", Json::str("SYMBOL")),
            ("name", Json::str(name)),
        ]),
        Rule::Choice(members) => Json::obj(vec![
            ("type", Json::str("CHOICE")),
            (
                "members",
                Json::Arr(members.iter().map(json_rule).collect()),
            ),
        ]),
        Rule::Seq(members) => Json::obj(vec![
            ("type", Json::str("SEQ")),
            (
                "members",
                Json::Arr(members.iter().map(json_rule).collect()),
            ),
        ]),
        Rule::Repeat(content) => Json::obj(vec![
            ("type", Json::str("REPEAT")),
            ("content", json_rule(content)),
        ]),
        Rule::Repeat1(content) => Json::obj(vec![
            ("type", Json::str("REPEAT1")),
            ("content", json_rule(content)),
        ]),
        Rule::Prec { value, content } => Json::obj(vec![
            ("type", Json::str("PREC")),
            ("value", json_prec(value)),
            ("content", json_rule(content)),
        ]),
        Rule::PrecLeft { value, content } => Json::obj(vec![
            ("type", Json::str("PREC_LEFT")),
            ("value", json_prec(value)),
            ("content", json_rule(content)),
        ]),
        Rule::PrecRight { value, content } => Json::obj(vec![
            ("type", Json::str("PREC_RIGHT")),
            ("value", json_prec(value)),
            ("content", json_rule(content)),
        ]),
        Rule::PrecDynamic { value, content } => Json::obj(vec![
            ("type", Json::str("PREC_DYNAMIC")),
            ("value", Json::Num(i64::from(*value))),
            ("content", json_rule(content)),
        ]),
        Rule::Token(content) => Json::obj(vec![
            ("type", Json::str("TOKEN")),
            ("content", json_rule(content)),
        ]),
        Rule::ImmediateToken(content) => Json::obj(vec![
            ("type", Json::str("IMMEDIATE_TOKEN")),
            ("content", json_rule(content)),
        ]),
        Rule::Alias {
            content,
            named,
            value,
        } => Json::obj(vec![
            ("type", Json::str("ALIAS")),
            ("content", json_rule(content)),
            ("named", Json::Bool(*named)),
            ("value", Json::str(value)),
        ]),
        Rule::Field { name, content } => Json::obj(vec![
            ("type", Json::str("FIELD")),
            ("name", Json::str(name)),
            ("content", json_rule(content)),
        ]),
        Rule::Reserved {
            context_name,
            content,
        } => Json::obj(vec![
            ("type", Json::str("RESERVED")),
            ("content", json_rule(content)),
            ("context_name", Json::str(context_name)),
        ]),
    }
}

pub fn json_prec(value: &PrecValue) -> Json {
    match value {
        PrecValue::Int(n) => Json::Num(i64::from(*n)),
        PrecValue::Name(name) => Json::str(name),
    }
}
