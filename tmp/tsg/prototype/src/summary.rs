//! The generator's report, decoded from `tree-sitter generate --json-summary`.
//!
//! Every type here mirrors a Rust type in `crates/generate`. Serde's external
//! tagging preserves variant names, so the JSON
//! `{"BuildTables":{"Conflict":{…}}}` *is* `GenerateError::BuildTables(
//! ParseTableBuilderError::Conflict(…))`. That is why this decoding table
//! translates one-for-one into the `match` that belongs in `generate.rs`: the
//! prototype and the real integration differ only in whether the values arrive
//! as types or as JSON.

use tsg::json::{Json, to_string};
use tsg::json_read;

#[derive(Debug)]
/// `build_parse_table::Resolution` — the generator's own list of ways out.
pub enum Resolution {
    Precedence(Vec<String>),
    Associativity(Vec<String>),
    AddConflict(Vec<String>),
}

#[derive(Debug)]
pub struct Conflict {
    pub symbol_sequence: Vec<String>,
    pub conflicting_lookahead: String,
    /// `variable_name` of each interpretation, plus its preceding symbols.
    pub interpretations: Vec<(String, Vec<String>)>,
    pub resolutions: Vec<Resolution>,
}

#[derive(Debug)]
pub enum GenError {
    Conflict(Conflict),
    AmbiguousExtra {
        parent_symbols: Vec<String>,
    },
    ImproperNonTerminalExtra(String),
    UndeclaredPrecedence {
        precedence: String,
        rule: String,
    },
    ConflictingPrecedenceOrdering {
        precedence_1: String,
        precedence_2: String,
    },
    IndirectRecursion(Vec<String>),
    NonTerminalWordToken {
        symbol_name: String,
        conflicting_symbol_name: Option<String>,
    },
    SupertypeTerminal(String),
    /// `FlattenGrammarError::EmptyString` / `ExpandTokensError::EmptyString`.
    EmptyStringRule(String),
    EmptyStringExtra(String),
    /// `ProcessInlinesError::{FirstRule, Token, NoReachableProductions}`.
    Inline {
        kind: &'static str,
        rule: String,
    },
    Other(String),
}

#[derive(Debug)]
pub enum GenDiagnostic {
    UnaryChoice(Option<String>),
    UnarySeq(Option<String>),
    SupertypeInlined(String),
    UnnecessaryConflicts(Vec<Vec<String>>),
    UnsupportedRegexFlag {
        flag: String,
        pattern: String,
    },
    Other(String),
}

pub struct Summary {
    pub diagnostics: Vec<GenDiagnostic>,
    pub error: Option<GenError>,
}

/// Decode the envelope the CLI prints for `--json-summary`.
pub fn parse(text: &str) -> Result<Summary, String> {
    let text = text.trim();
    let value = json_read::parse(text).map_err(|e| format!("not JSON ({e}): {text:.200}"))?;
    let Json::Obj(fields) = &value else {
        return Err("summary is not an object".to_owned());
    };
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };

    let error = match get("error") {
        None | Some(Json::Null) => None,
        Some(value) => Some(decode_error(value)),
    };

    let diagnostics = get("diagnostics")
        .and_then(Json::as_arr)
        .map(|items| items.iter().map(decode_diagnostic).collect())
        .unwrap_or_default();

    Ok(Summary { diagnostics, error })
}

/// A serde enum is a one-key object: return that key and its payload.
fn single(value: &Json) -> Option<(&str, &Json)> {
    match value {
        Json::Obj(fields) if fields.len() == 1 => {
            fields.first().map(|(key, value)| (key.as_str(), value))
        }
        _ => None,
    }
}

fn as_strings(value: &Json) -> Vec<String> {
    value
        .as_arr()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn string(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_str).map(ToOwned::to_owned)
}

fn decode_error(value: &Json) -> GenError {
    let Some((outer, payload)) = single(value) else {
        return GenError::Other(to_string(value));
    };
    let Some((inner, body)) = single(payload) else {
        return GenError::Other(to_string(value));
    };
    match (outer, inner) {
        ("BuildTables", "Conflict") => GenError::Conflict(decode_conflict(body)),
        ("BuildTables", "AmbiguousExtra") => GenError::AmbiguousExtra {
            parent_symbols: vec_field(body, "parent_symbols"),
        },
        ("BuildTables", "ImproperNonTerminalExtra") => {
            GenError::ImproperNonTerminalExtra(field_str(body, "ImproperNonTerminalExtra"))
        }
        ("Prepare", "ValidatePrecedences") => decode_precedence(body),
        ("Prepare", "ValidateIndirectRecursion") => {
            GenError::IndirectRecursion(as_strings(body))
        }
        ("Prepare", "ExtractTokens") => decode_extract(body),
        ("Prepare", "FlattenGrammar") => match single(body) {
            Some(("EmptyString", rule)) => {
                GenError::EmptyStringRule(field_str(rule, "EmptyString"))
            }
            _ => GenError::Other(to_string(value)),
        },
        // `EmptyStringExtraError` is a newtype struct, so serde flattens it to a
        // bare string: `{"ValidateExtras": "ws"}`.
        ("Prepare", "ValidateExtras") => match body {
            Json::Str(name) => GenError::EmptyStringExtra(name.clone()),
            _ => GenError::Other(to_string(value)),
        },
        ("Prepare", "ProcessInlines") => {
            let kind = match single(body) {
                Some(("FirstRule", _)) => "FirstRule",
                Some(("Token", _)) => "Token",
                Some(("NoReachableProductions", _)) => "NoReachableProductions",
                _ => return GenError::Other(to_string(value)),
            };
            GenError::Inline {
                kind,
                rule: vec_field(body, kind).first().cloned().unwrap_or_default(),
            }
        }
        _ => GenError::Other(to_string(value)),
    }
}

fn decode_precedence(body: &Json) -> GenError {
    match single(body) {
        Some(("Undeclared", fields)) => GenError::UndeclaredPrecedence {
            precedence: obj_str(fields, "precedence"),
            rule: obj_str(fields, "rule"),
        },
        Some(("Ordering", fields)) => GenError::ConflictingPrecedenceOrdering {
            precedence_1: obj_str(fields, "precedence_1"),
            precedence_2: obj_str(fields, "precedence_2"),
        },
        _ => GenError::Other(to_string(body)),
    }
}

fn decode_extract(body: &Json) -> GenError {
    match single(body) {
        Some(("WordToken", fields)) => {
            let conflicting = fields
                .as_obj()
                .and_then(|fields| {
                    fields
                        .iter()
                        .find(|(name, _)| name == "conflicting_symbol_name")
                        .map(|(_, value)| value)
                })
                .and_then(|value| string(Some(value)));
            GenError::NonTerminalWordToken {
                symbol_name: obj_str(fields, "symbol_name"),
                conflicting_symbol_name: conflicting,
            }
        }
        Some(("SupertypeTerminal", name)) => {
            GenError::SupertypeTerminal(field_str(name, "SupertypeTerminal"))
        }
        Some(("EmptyString", name)) => GenError::EmptyStringRule(field_str(name, "EmptyString")),
        _ => GenError::Other(to_string(body)),
    }
}

fn decode_conflict(fields: &Json) -> Conflict {
    let interpretations = fields
        .as_obj()
        .and_then(|fields| {
            fields
                .iter()
                .find(|(name, _)| name == "possible_interpretations")
                .map(|(_, value)| value)
        })
        .and_then(Json::as_arr)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    (
                        obj_str(item, "variable_name"),
                        vec_field_inner(item, "preceding_symbols"),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let resolutions = single_field(fields, "possible_resolutions")
        .and_then(Json::as_arr)
        .map(|items| items.iter().filter_map(decode_resolution).collect())
        .unwrap_or_default();

    Conflict {
        symbol_sequence: vec_field(fields, "symbol_sequence"),
        conflicting_lookahead: obj_str(fields, "conflicting_lookahead"),
        interpretations,
        resolutions,
    }
}

fn decode_resolution(value: &Json) -> Option<Resolution> {
    let (kind, body) = single(value)?;
    let symbols = vec_field(body, "symbols");
    match kind {
        "Precedence" => Some(Resolution::Precedence(symbols)),
        "Associativity" => Some(Resolution::Associativity(symbols)),
        "AddConflict" => Some(Resolution::AddConflict(symbols)),
        _ => None,
    }
}

fn decode_diagnostic(value: &Json) -> GenDiagnostic {
    let Some((kind, body)) = single(value) else {
        return GenDiagnostic::Other(to_string(value));
    };
    match kind {
        "UnaryChoice" => GenDiagnostic::UnaryChoice(string(single_field(body, "name"))),
        "UnarySeq" => GenDiagnostic::UnarySeq(string(single_field(body, "name"))),
        "SupertypeInlined" => {
            GenDiagnostic::SupertypeInlined(obj_str(body, "name"))
        }
        "UnnecessaryConflicts" => GenDiagnostic::UnnecessaryConflicts(
            body.as_arr()
                .map(|sets| sets.iter().map(as_strings).collect())
                .unwrap_or_default(),
        ),
        "UnsupportedRegexFlag" => GenDiagnostic::UnsupportedRegexFlag {
            flag: obj_str(body, "flag"),
            pattern: obj_str(body, "pattern"),
        },
        _ => GenDiagnostic::Other(to_string(value)),
    }
}

fn single_field<'a>(value: &'a Json, name: &str) -> Option<&'a Json> {
    value
        .as_obj()
        .and_then(|fields| fields.iter().find(|(key, _)| key == name))
        .map(|(_, value)| value)
}

/// Read `name` as a string out of a plain (multi-key) object.
fn obj_str(value: &Json, name: &str) -> String {
    single_field(value, name)
        .and_then(Json::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

/// A `{"Variant": "text"}` payload, where `Variant` is already known.
fn field_str(value: &Json, name: &str) -> String {
    match value {
        Json::Obj(fields) if fields.len() == 1 => {
            let (key, value) = &fields[0];
            if key == name {
                value.as_str().map(ToOwned::to_owned).unwrap_or_default()
            } else {
                String::new()
            }
        }
        Json::Str(text) => text.clone(),
        _ => String::new(),
    }
}

fn vec_field(value: &Json, name: &str) -> Vec<String> {
    vec_field_inner(value, name)
}

fn vec_field_inner(value: &Json, name: &str) -> Vec<String> {
    single_field(value, name).map(as_strings).unwrap_or_default()
}
