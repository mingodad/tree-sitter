//! Semantic comparison of two `grammar.json` values.
//!
//! Two `.tsg` files can be the same grammar and differ in every byte — comments,
//! indentation, `tokens { }` grouping, sugar that lowers to the same rules.  And
//! `grammar.json` carries order that *is* meaning (rule order decides the start
//! rule, key order is preserved on purpose), so a text comparison is wrong in
//! both directions: it fails on formatting, and when it does fail it cannot say
//! what changed.
//!
//! So: compare the lowered grammars, path by path, and report the differences
//! with their location.  Only arrays whose order is provably not part of the
//! meaning are sorted before comparing (`conflicts`: a list of sets, which the
//! generator rewrites).  Everything else is compared in document order.

use std::fmt::Write as _;

use crate::json::Json;

pub struct Difference {
    /// `rules.expr.members[2].name`, or `conflicts[0][1]`.
    pub path: String,
    /// The two values, rendered short enough for one line each.
    pub left: String,
    pub right: String,
}

/// Compare two grammars and collect the differences, in document order.
pub fn compare(left: &Json, right: &Json) -> Vec<Difference> {
    let mut differences = Vec::new();
    walk(left, right, "", false, &mut differences);
    differences
}

/// `sorted`: this position is an array whose order is not part of the meaning.
fn walk(left: &Json, right: &Json, path: &str, sorted: bool, out: &mut Vec<Difference>) {
    match (left, right) {
        (Json::Obj(left), Json::Obj(right)) => {
            if left.len() != right.len() {
                return differ(
                    &Json::Obj(left.clone()),
                    &Json::Obj(right.clone()),
                    path,
                    out,
                );
            }
            for ((left_key, left_value), (right_key, right_value)) in left.iter().zip(right) {
                if left_key != right_key {
                    return differ(
                        &Json::str(left_key.clone()),
                        &Json::str(right_key.clone()),
                        path,
                        out,
                    );
                }
                let child = if path.is_empty() {
                    left_key.clone()
                } else {
                    format!("{path}.{left_key}")
                };
                // `conflicts` is a list of sets: neither the order of the sets
                // nor of their members is part of the meaning.
                let child_sorted = left_key == "conflicts";
                walk(left_value, right_value, &child, child_sorted, out);
            }
        }
        (Json::Arr(left), Json::Arr(right)) => {
            let (left, right) = if sorted {
                (sorted_set(left), sorted_set(right))
            } else {
                (left.clone(), right.clone())
            };
            if left.len() != right.len() {
                return differ(&Json::Arr(left), &Json::Arr(right), path, out);
            }
            for (index, (left_value, right_value)) in left.iter().zip(&right).enumerate() {
                walk(
                    left_value,
                    right_value,
                    &format!("{path}[{index}]"),
                    false,
                    out,
                );
            }
        }
        (left, right) => {
            if render(left) != render(right) {
                differ(left, right, path, out);
            }
        }
    }
}

fn differ(left: &Json, right: &Json, path: &str, out: &mut Vec<Difference>) {
    out.push(Difference {
        path: if path.is_empty() { "<grammar>" } else { path }.to_owned(),
        left: render(left),
        right: render(right),
    });
}

/// Sort a set-like array by its rendered form, which is canonical for the
/// string arrays that `conflicts` holds.
fn sorted_set(items: &[Json]) -> Vec<Json> {
    let mut items: Vec<(String, Json)> = items
        .iter()
        .map(|item| (render(item), item.clone()))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items.into_iter().map(|(_, item)| item).collect()
}

/// One-line rendering, truncated: a difference is meant to be recognised, not
/// reproduced.
fn render(value: &Json) -> String {
    let mut out = String::new();
    write_value(value, &mut out, 0);
    if out.chars().count() > 160 {
        out = out.chars().take(157).collect::<String>() + "…";
    }
    out
}

fn write_value(value: &Json, out: &mut String, depth: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => {
            let _ = write!(out, "{flag}");
        }
        Json::Num(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Str(text) => {
            let _ = write!(out, "{text:?}");
        }
        Json::Arr(items) => {
            if depth > 2 {
                out.push_str("[…]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(item, out, depth + 1);
            }
            out.push(']');
        }
        Json::Obj(entries) => {
            if depth > 2 {
                out.push_str("{…}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{key}: ");
                write_value(item, out, depth + 1);
            }
            out.push('}');
        }
    }
}
