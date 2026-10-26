use std::path::PathBuf;
#[cfg(feature = "load")]
use std::{
    env, fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use bitflags::bitflags;
use node_types::VariableInfo;
#[cfg(feature = "load")]
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod bitvec;
mod build_tables;
mod dedup;
mod grammars;
mod nfa;
mod node_types;
pub mod parse_grammar;
mod prepare_grammar;
#[cfg(feature = "qjs-rt")]
mod quickjs;
mod render;
mod rules;
mod strpool;
mod tables;

use build_tables::build_tables;
pub use build_tables::{AmbiguousExtraError, ConflictError, ParseTableBuilderError};
use grammars::{InlinedProductionMap, LexicalGrammar, SyntaxGrammar};
pub use node_types::{InvalidSupertypeError, SuperTypeCycleError, VariableInfoError};
pub use parse_grammar::ParseGrammarError;
use parse_grammar::parse_grammar;
use prepare_grammar::prepare_grammar;
pub use prepare_grammar::{
    ConflictingPrecedenceOrderingError, EmptyStringExtraError, ExpandRegexError,
    ExpandRepeatsError, ExpandRuleError, ExpandTokensError, ExpandTokensProcessingError,
    ExtractTokensError, FlattenGrammarError, IndirectRecursionError, InternSymbolsError,
    NonAsciiByteClassError, NonTerminalWordTokenError, PatternSpan, PrepareGrammarError,
    ProcessInlinesError, RegexError, RegexErrorKind, UndeclaredPrecedenceError,
    ValidatePrecedenceError,
};
use render::render_c_code;
pub use render::{ABI_VERSION_MAX, ABI_VERSION_MIN, RenderError};

use crate::{
    grammars::InputGrammar, prepare_grammar::PreparedGrammar, rules::AliasMap, strpool::StrPool,
};

struct JSONOutput {
    #[cfg(feature = "load")]
    node_types_json: String,
    syntax_grammar: SyntaxGrammar,
    lexical_grammar: LexicalGrammar,
    inlines: InlinedProductionMap,
    simple_aliases: AliasMap,
    variable_info: Vec<VariableInfo>,
    str_pool: StrPool,
}

struct GeneratedParser {
    c_code: String,
    #[cfg(feature = "load")]
    node_types_json: String,
}

// NOTE: This constant must be kept in sync with the definition of
// `TREE_SITTER_LANGUAGE_VERSION` in `lib/include/tree_sitter/api.h`.
const LANGUAGE_VERSION: usize = 15;

pub const ALLOC_HEADER: &str = include_str!("templates/alloc.h");
pub const ARRAY_HEADER: &str = include_str!("templates/array.h");
pub const PARSER_HEADER: &str = include_str!("parser.h.inc");

pub type GenerateResult<T> = Result<T, GenerateError>;

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum GenerateError {
    #[error("Error with specified path -- {0}")]
    GrammarPath(IoError),
    #[error(transparent)]
    IO(IoError),
    #[cfg(feature = "load")]
    #[error(transparent)]
    LoadGrammarFile(#[from] LoadGrammarError),
    #[error("Grammar file `{0}` not found")]
    GrammarFileNotFound(PathBuf),
    #[error(transparent)]
    ParseGrammar(#[from] ParseGrammarError),
    #[error(transparent)]
    Prepare(#[from] PrepareGrammarError),
    #[error(transparent)]
    VariableInfo(#[from] VariableInfoError),
    #[error(transparent)]
    BuildTables(#[from] ParseTableBuilderError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[cfg(feature = "load")]
    #[error(transparent)]
    ParseVersion(#[from] ParseVersionError),
    #[error(transparent)]
    SuperTypeCycle(#[from] SuperTypeCycleError),
}

#[derive(Debug, Error)]
pub struct IoError {
    pub error: std::io::Error,
    pub path: Option<PathBuf>,
}

impl PartialEq for IoError {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.error.kind() == other.error.kind()
            && self.error.raw_os_error() == other.error.raw_os_error()
    }
}

impl Eq for IoError {}

#[cfg(feature = "load")]
impl IoError {
    fn new(error: std::io::Error, path: Option<&Path>) -> Self {
        Self {
            error,
            path: path.map(Path::to_path_buf),
        }
    }
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)?;
        if let Some(ref path) = self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct IoErrorRepr {
    message: String,
    raw_os_error: Option<i32>,
    path: Option<PathBuf>,
}

impl Serialize for IoError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        IoErrorRepr {
            message: self.error.to_string(),
            raw_os_error: self.error.raw_os_error(),
            path: self.path.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IoError {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = IoErrorRepr::deserialize(deserializer)?;
        let error = repr.raw_os_error.map_or_else(
            || std::io::Error::other(repr.message),
            std::io::Error::from_raw_os_error,
        );
        Ok(Self {
            error,
            path: repr.path,
        })
    }
}

#[cfg(feature = "load")]
pub type LoadGrammarFileResult<T> = Result<T, LoadGrammarError>;

#[cfg(feature = "load")]
#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum LoadGrammarError {
    #[error("Path to a grammar file with `.js` or `.json` extension is required")]
    InvalidPath,
    #[error("Failed to load grammar.js -- {0}")]
    LoadJSGrammarFile(#[from] JSError),
    #[error("Failed to load grammar.json -- {0}")]
    IO(IoError),
    #[error("Unknown grammar file extension: {0:?}")]
    FileExtension(PathBuf),
}

#[cfg(feature = "load")]
#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParseVersionError {
    #[error("{0}")]
    Version(Box<str>),
    #[error("{0}")]
    JSON(Box<str>),
    #[error(transparent)]
    IO(IoError),
}

#[cfg(feature = "load")]
pub type JSResult<T> = Result<T, JSError>;

#[cfg(feature = "load")]
#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum JSError {
    #[error("Failed to run `{runtime}` -- {error}")]
    JSRuntimeSpawn { runtime: Box<str>, error: Box<str> },
    #[error("Got invalid UTF8 from `{runtime}` -- {error}")]
    JSRuntimeUtf8 { runtime: Box<str>, error: Box<str> },
    #[error("`{runtime}` process exited with status {code}")]
    JSRuntimeExit { runtime: Box<str>, code: i32 },
    #[error("Failed to open stdin for `{runtime}`")]
    JSRuntimeStdin { runtime: Box<str> },
    #[error("Failed to write {item} to `{runtime}`'s stdin -- {error}")]
    JSRuntimeWrite {
        runtime: Box<str>,
        item: Box<str>,
        error: Box<str>,
    },
    #[error("Failed to read output from `{runtime}` -- {error}")]
    JSRuntimeRead { runtime: Box<str>, error: Box<str> },
    #[error(transparent)]
    IO(IoError),
    #[cfg(feature = "qjs-rt")]
    #[error("Failed to get relative path")]
    RelativePath,
    #[error("Could not parse this package's version as semver -- {0}")]
    Semver(Box<str>),
    #[error("Failed to serialize grammar JSON -- {0}")]
    Serialization(Box<str>),
    #[cfg(feature = "qjs-rt")]
    #[error("QuickJS error: {0}")]
    QuickJS(Box<str>),
}

#[cfg(feature = "load")]
impl From<serde_json::Error> for JSError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string().into())
    }
}

#[cfg(feature = "load")]
impl From<semver::Error> for JSError {
    fn from(value: semver::Error) -> Self {
        Self::Semver(value.to_string().into())
    }
}

#[cfg(feature = "qjs-rt")]
impl From<rquickjs::Error> for JSError {
    fn from(value: rquickjs::Error) -> Self {
        Self::QuickJS(value.to_string().into())
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct OptLevel: u32 {
        const MergeStates = 1 << 0;
        /// Build LALR(1) states instead of canonical LR(1) states: when a new
        /// state has the same kernel core as an existing one, merge their
        /// lookahead sets and rebuild the state. This is what yacc/bison do,
        /// and it keeps large grammars from exploding in state count, at the
        /// cost of reporting conflicts that canonical LR(1) would not have.
        const Lalr = 1 << 1;
    }
}

impl Default for OptLevel {
    fn default() -> Self {
        Self::MergeStates
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Diagnostic {
    UnnecessaryConflicts(Box<[Box<[Box<str>]>]>),
    UnaryChoice { name: Option<Box<str>> },
    UnarySeq { name: Option<Box<str>> },
    UnsupportedRegexFlag { flag: char, pattern: Box<str> },
    SupertypeInlined { name: Box<str> },
    UnreachableRule { name: Box<str> },
    UnproductiveRule { name: Box<str> },
    ShadowedExtras(Box<[ShadowedExtra]>),
}

/// A token that completes where an extra begins, so the extra is never lexed there.
///
/// Where a token completes, the lexer table cannot keep a separator transition: at
/// equal matched length a token beats an extra, and that is not configurable. For a
/// comment extra this shows up as `1 /* c */` lexing the `/` as division.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShadowedExtra {
    pub token: Box<str>,
    pub characters: Box<str>,
    /// The extras the dropped transition belonged to, when they have names.
    pub extras: Box<[Box<str>]>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShadowedExtras(rows) => {
                writeln!(
                    f,
                    "a token that completes at the start of an extra hides that extra:",
                )?;
                for ShadowedExtra {
                    token,
                    characters,
                    extras,
                } in rows.iter()
                {
                    write!(
                        f,
                        "  `{token}` completes, so the transition on {characters} into "
                    )?;
                    if extras.is_empty() {
                        writeln!(f, "an extra is never taken")?;
                    } else {
                        writeln!(f, "extra `{}` is never taken", extras.join("`, `"))?;
                    }
                }
                if rows.len() >= 10 {
                    writeln!(f, "  (only the first 10 are shown)")?;
                }
                write!(
                    f,
                    "  at equal length a token beats an extra: no token may be a prefix of an \
                     extra that must win, so rename the token or change the extra",
                )?;
            }
            Self::UnnecessaryConflicts(conflicts) => {
                writeln!(f, "unnecessary conflicts:")?;
                for (i, conflict) in conflicts.iter().enumerate() {
                    write!(f, "  ")?;
                    for (j, symbol) in conflict.iter().enumerate() {
                        write!(f, "`{symbol}`")?;
                        if j < conflict.len() - 1 {
                            write!(f, ", ")?;
                        }
                    }
                    if i < conflicts.len() - 1 {
                        writeln!(f)?;
                    }
                }
            }
            Self::UnaryChoice { name } => {
                write!(
                    f,
                    "rule {} contains a `choice` rule with a single element. this is unnecessary.",
                    name.as_deref().unwrap_or("<ANONYMOUS>")
                )?;
            }
            Self::UnarySeq { name } => {
                write!(
                    f,
                    "rule {} contains a `seq` rule with a single element. this is unnecessary.",
                    name.as_deref().unwrap_or("<ANONYMOUS>")
                )?;
            }
            Self::UnsupportedRegexFlag { flag, pattern } => {
                write!(f, "unsupported regex flag `{flag}` in pattern `{pattern}`")?;
            }
            Self::SupertypeInlined { name } => {
                write!(
                    f,
                    "rule `{name}` is both a supertype and inlined. the supertype is ignored."
                )?;
            }
            Self::UnreachableRule { name } => {
                write!(
                    f,
                    "rule `{name}` is not reachable from the start rule. it was ignored."
                )?;
            }
            Self::UnproductiveRule { name } => {
                write!(
                    f,
                    "rule `{name}` cannot match anything: every production of it needs a rule \
                     or token that can never match. the parser will never accept it."
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "load")]
#[expect(
    clippy::too_many_arguments,
    reason = "all parameters are required for parser generation"
)]
pub fn generate_parser_in_directory<T, U, V>(
    repo_path: T,
    out_path: Option<U>,
    grammar_path: Option<V>,
    abi_version: usize,
    report_symbol_name: Option<&str>,
    report_lex_states: Option<&str>,
    js_runtime: Option<&str>,
    generate_parser: bool,
    optimizations: OptLevel,
    max_states: Option<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> GenerateResult<()>
where
    T: Into<PathBuf>,
    U: Into<PathBuf>,
    V: Into<PathBuf>,
{
    let mut repo_path: PathBuf = repo_path.into();

    // Populate a new empty grammar directory.
    let grammar_path = if let Some(path) = grammar_path {
        let path_buf: PathBuf = path.into();
        if !path_buf
            .try_exists()
            .map_err(|e| GenerateError::GrammarPath(IoError::new(e, Some(&path_buf))))?
        {
            // A nonexistent path with an extension (i.e. tree-sitter-foo/grammar.json)
            // is a missing input file, not a directory to create.
            if path_buf.extension().is_some() {
                return Err(GenerateError::GrammarFileNotFound(path_buf));
            }
            fs::create_dir_all(&path_buf)
                .map_err(|e| GenerateError::IO(IoError::new(e, Some(path_buf.as_path()))))?;
            repo_path = path_buf;
            repo_path.join("grammar.js")
        } else {
            // Given an _explicit_ path to an input file, derive the repo root from the
            // conventional locations:
            //     - grammar.js sits inside <root>/
            //     - grammar.json sits inside <root>/src/
            let repo_root = match path_buf.extension() {
                Some(e) if e == "js" => path_buf.parent(),
                Some(e) if e == "json" => path_buf.parent().and_then(Path::parent),
                _ => None,
            };
            if let Some(root) = repo_root {
                repo_path = root.to_path_buf();
            }
            path_buf
        }
    } else {
        repo_path.join("grammar.js")
    };

    // Read the grammar file.
    let grammar_json = load_grammar_file(&grammar_path, js_runtime)?;

    let src_path = out_path.map_or_else(|| repo_path.join("src"), std::convert::Into::into);
    let header_path = src_path.join("tree_sitter");

    // Ensure that the output directory exists
    fs::create_dir_all(&src_path)
        .map_err(|e| GenerateError::IO(IoError::new(e, Some(src_path.as_path()))))?;

    if grammar_path.file_name().unwrap() != "grammar.json" {
        fs::write(src_path.join("grammar.json"), &grammar_json)
            .map_err(|e| GenerateError::IO(IoError::new(e, Some(src_path.as_path()))))?;
    }

    // If our job is only to generate `grammar.json` and not `parser.c`, stop here.
    let input_grammar = parse_grammar(&grammar_json, diagnostics)?;

    if !generate_parser {
        let node_types_json =
            generate_node_types_from_grammar(input_grammar, diagnostics)?.node_types_json;
        write_file(&src_path.join("node-types.json"), node_types_json)?;
        return Ok(());
    }

    let semantic_version = read_grammar_version(&repo_path)?;

    // Generate the parser and related files.
    let GeneratedParser {
        c_code,
        node_types_json,
    } = generate_parser_for_grammar_with_opts(
        input_grammar,
        abi_version,
        semantic_version.map(|v| (v.major as u8, v.minor as u8, v.patch as u8)),
        report_symbol_name,
        report_lex_states,
        optimizations,
        max_states,
        diagnostics,
    )?;

    write_file(&src_path.join("parser.c"), c_code)?;
    write_file(&src_path.join("node-types.json"), node_types_json)?;
    fs::create_dir_all(&header_path)
        .map_err(|e| GenerateError::IO(IoError::new(e, Some(header_path.as_path()))))?;
    write_file(&header_path.join("alloc.h"), ALLOC_HEADER)?;
    write_file(&header_path.join("array.h"), ARRAY_HEADER)?;
    write_file(&header_path.join("parser.h"), PARSER_HEADER)?;

    Ok(())
}

pub fn generate_parser_for_grammar(
    grammar_json: &str,
    semantic_version: Option<(u8, u8, u8)>,
    optimizations: OptLevel,
    diagnostics: &mut Vec<Diagnostic>,
) -> GenerateResult<(String, String)> {
    let input_grammar = parse_grammar(grammar_json, diagnostics)?;
    let name = input_grammar.pool.resolve(input_grammar.name).to_string();
    let parser = generate_parser_for_grammar_with_opts(
        input_grammar,
        LANGUAGE_VERSION,
        semantic_version,
        None,
        None,
        optimizations,
        None,
        diagnostics,
    )?;
    Ok((name, parser.c_code))
}

fn generate_node_types_from_grammar(
    input_grammar: InputGrammar,
    diagnostics: &mut Vec<Diagnostic>,
) -> GenerateResult<JSONOutput> {
    let PreparedGrammar {
        syntax_grammar,
        lexical_grammar,
        inlines,
        default_aliases,
        str_pool,
    } = prepare_grammar(input_grammar, diagnostics)?;
    let variable_info = node_types::get_variable_info(
        &syntax_grammar,
        &lexical_grammar,
        &default_aliases,
        &inlines,
        &str_pool,
    )?;

    #[cfg(feature = "load")]
    let node_types_json = node_types::generate_node_types_json(
        &syntax_grammar,
        &lexical_grammar,
        &default_aliases,
        &variable_info,
        &inlines,
        &str_pool,
    )?;
    Ok(JSONOutput {
        #[cfg(feature = "load")]
        node_types_json,
        syntax_grammar,
        lexical_grammar,
        inlines,
        simple_aliases: default_aliases,
        variable_info,
        str_pool,
    })
}

fn generate_parser_for_grammar_with_opts(
    input_grammar: InputGrammar,
    abi_version: usize,
    semantic_version: Option<(u8, u8, u8)>,
    report_symbol_name: Option<&str>,
    report_lex_states: Option<&str>,
    optimizations: OptLevel,
    max_states: Option<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> GenerateResult<GeneratedParser> {
    let grammar_name = input_grammar.name;
    let JSONOutput {
        syntax_grammar,
        lexical_grammar,
        mut inlines,
        simple_aliases,
        variable_info,
        #[cfg(feature = "load")]
        node_types_json,
        str_pool,
    } = generate_node_types_from_grammar(input_grammar, diagnostics)?;
    let supertype_symbol_map = node_types::get_supertype_symbol_map(
        &syntax_grammar,
        &simple_aliases,
        &variable_info,
        &inlines,
    );
    inlines.discard_fully_inlined();
    let tables = build_tables(
        &syntax_grammar,
        &lexical_grammar,
        &simple_aliases,
        &variable_info,
        &inlines,
        &str_pool,
        report_symbol_name,
        report_lex_states,
        optimizations,
        max_states,
        diagnostics,
    )?;
    let c_code = render_c_code(
        grammar_name,
        tables,
        syntax_grammar,
        lexical_grammar,
        simple_aliases,
        str_pool,
        abi_version,
        semantic_version,
        supertype_symbol_map,
    )?;
    Ok(GeneratedParser {
        c_code,
        #[cfg(feature = "load")]
        node_types_json,
    })
}

/// This will read the `tree-sitter.json` config file and attempt to extract the version.
///
/// If the file is not found in the current directory or any of its parent directories, this will
/// return `None` to maintain backwards compatibility. If the file is found but the version cannot
/// be parsed as semver, this will return an error.
#[cfg(feature = "load")]
fn read_grammar_version(repo_path: &Path) -> Result<Option<Version>, ParseVersionError> {
    #[derive(Deserialize)]
    struct TreeSitterJson {
        metadata: Metadata,
    }

    #[derive(Deserialize)]
    struct Metadata {
        version: String,
    }

    let filename = "tree-sitter.json";
    let mut path = repo_path.join(filename);

    loop {
        let json = path
            .exists()
            .then(|| {
                let contents = fs::read_to_string(path.as_path())
                    .map_err(|e| ParseVersionError::IO(IoError::new(e, Some(path.as_path()))))?;
                serde_json::from_str::<TreeSitterJson>(&contents).map_err(|e| {
                    ParseVersionError::JSON(
                        format!("Failed to parse `{}` -- {e}", path.display()).into(),
                    )
                })
            })
            .transpose()?;
        if let Some(json) = json {
            return Version::parse(&json.metadata.version)
                .map_err(|e| {
                    ParseVersionError::Version(
                        format!(
                            "Failed to parse `{}` version as semver -- {e}",
                            path.display()
                        )
                        .into(),
                    )
                })
                .map(Some);
        }
        path.pop(); // filename
        if !path.pop() {
            return Ok(None);
        }
        path.push(filename);
    }
}

#[cfg(feature = "load")]
pub fn load_grammar_file(
    grammar_path: &Path,
    js_runtime: Option<&str>,
) -> LoadGrammarFileResult<String> {
    if grammar_path.is_dir() {
        Err(LoadGrammarError::InvalidPath)?;
    }
    match grammar_path.extension().and_then(|e| e.to_str()) {
        Some("js") => Ok(load_js_grammar_file(grammar_path, js_runtime)?),
        Some("json") => Ok(fs::read_to_string(grammar_path)
            .map_err(|e| LoadGrammarError::IO(IoError::new(e, Some(grammar_path))))?),
        _ => Err(LoadGrammarError::FileExtension(grammar_path.to_owned()))?,
    }
}

#[cfg(feature = "load")]
fn load_js_grammar_file(grammar_path: &Path, js_runtime: Option<&str>) -> JSResult<String> {
    let grammar_path = dunce::canonicalize(grammar_path)
        .map_err(|e| JSError::IO(IoError::new(e, Some(grammar_path))))?;

    #[cfg(feature = "qjs-rt")]
    if js_runtime == Some("native") {
        return quickjs::execute_native_runtime(&grammar_path);
    }

    // The "file:///" prefix is incompatible with the quickjs runtime, but is required
    // for node and bun
    #[cfg(windows)]
    let grammar_path = PathBuf::from(format!("file:///{}", grammar_path.display()));

    let js_runtime = js_runtime.unwrap_or("node");

    let mut js_command = Command::new(js_runtime);
    match js_runtime {
        "node" => {
            js_command.args(["--input-type=module", "-"]);
        }
        "bun" => {
            js_command.arg("-");
        }
        "deno" => {
            js_command.args(["run", "--allow-all", "-"]);
        }
        _ => {}
    }

    let mut js_process = js_command
        .env("TREE_SITTER_GRAMMAR_PATH", grammar_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| JSError::JSRuntimeSpawn {
            runtime: js_runtime.to_string().into(),
            error: e.to_string().into(),
        })?;

    let mut js_stdin = js_process
        .stdin
        .take()
        .ok_or_else(|| JSError::JSRuntimeStdin {
            runtime: js_runtime.to_string().into(),
        })?;

    let cli_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    write!(
        js_stdin,
        "globalThis.TREE_SITTER_CLI_VERSION_MAJOR = {};
         globalThis.TREE_SITTER_CLI_VERSION_MINOR = {};
         globalThis.TREE_SITTER_CLI_VERSION_PATCH = {};",
        cli_version.major, cli_version.minor, cli_version.patch,
    )
    .map_err(|e| JSError::JSRuntimeWrite {
        runtime: js_runtime.to_string().into(),
        item: "tree-sitter version".to_string().into(),
        error: e.to_string().into(),
    })?;
    js_stdin
        .write(include_bytes!("./dsl.js"))
        .map_err(|e| JSError::JSRuntimeWrite {
            runtime: js_runtime.to_string().into(),
            item: "grammar dsl".to_string().into(),
            error: e.to_string().into(),
        })?;
    drop(js_stdin);

    let output = js_process
        .wait_with_output()
        .map_err(|e| JSError::JSRuntimeRead {
            runtime: js_runtime.to_string().into(),
            error: e.to_string().into(),
        })?;
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8(output.stdout).map_err(|e| JSError::JSRuntimeUtf8 {
                runtime: js_runtime.to_string().into(),
                error: e.to_string().into(),
            })?;

            let mut grammar_json = &stdout[..];

            if let Some(pos) = stdout.rfind('\n') {
                // If there's a newline, split the last line from the rest of the output
                let node_output = &stdout[..pos];
                grammar_json = &stdout[pos + 1..];

                let mut stdout = std::io::stdout().lock();
                stdout
                    .write_all(node_output.as_bytes())
                    .map_err(|e| JSError::IO(IoError::new(e, None)))?;
                stdout
                    .write_all(b"\n")
                    .map_err(|e| JSError::IO(IoError::new(e, None)))?;
                stdout
                    .flush()
                    .map_err(|e| JSError::IO(IoError::new(e, None)))?;
            }

            Ok(serde_json::to_string_pretty(&serde_json::from_str::<
                serde_json::Value,
            >(grammar_json)?)?)
        }
        Some(code) => Err(JSError::JSRuntimeExit {
            runtime: js_runtime.to_string().into(),
            code,
        }),
        None => Err(JSError::JSRuntimeExit {
            runtime: js_runtime.to_string().into(),
            code: -1,
        }),
    }
}

#[cfg(feature = "load")]
pub fn write_file(path: &Path, body: impl AsRef<[u8]>) -> GenerateResult<()> {
    fs::write(path, body).map_err(|e| GenerateError::IO(IoError::new(e, Some(path))))
}

#[cfg(test)]
mod tests {
    use super::{LANGUAGE_VERSION, PARSER_HEADER};
    #[test]
    fn test_language_versions_are_in_sync() {
        let api_h = include_str!("../../../lib/include/tree_sitter/api.h");
        let api_language_version = api_h
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("#define TREE_SITTER_LANGUAGE_VERSION ")
                    .and_then(|v| v.parse::<usize>().ok())
            })
            .expect("Failed to find TREE_SITTER_LANGUAGE_VERSION definition in api.h");
        assert_eq!(LANGUAGE_VERSION, api_language_version);
    }

    #[test]
    fn test_parser_header_in_sync() {
        let parser_h = include_str!("../../../lib/src/parser.h");
        assert!(
            parser_h == PARSER_HEADER,
            "parser.h.inc is out of sync with lib/src/parser.h. Run: cp lib/src/parser.h crates/generate/src/parser.h.inc"
        );
    }

    #[test]
    fn test_external_word_token_outside_terminal_range() {
        let grammar_json = r#"{
            "name": "external_word_token_out_of_bounds",
            "word": "ext_word",
            "rules": {
                "program": {"type": "REPEAT", "content": {"type": "SYMBOL", "name": "item"}},
                "item": {"type": "CHOICE", "members": [
                    {"type": "SEQ", "members": [
                        {"type": "STRING", "value": "{"},
                        {"type": "SYMBOL", "name": "ext_word"},
                        {"type": "SYMBOL", "name": "ext_a"}
                    ]},
                    {"type": "SEQ", "members": [
                        {"type": "STRING", "value": "let"},
                        {"type": "SYMBOL", "name": "ext_word"},
                        {"type": "SYMBOL", "name": "ext_b"}
                    ]}
                ]}
            },
            "externals": [
                {"type": "SYMBOL", "name": "ext_a"},
                {"type": "SYMBOL", "name": "ext_b"},
                {"type": "SYMBOL", "name": "ext_word"}
            ]
        }"#;

        // An external word token must not index the internal terminal table.
        super::generate_parser_for_grammar(
            grammar_json,
            None,
            super::OptLevel::default(),
            &mut Vec::new(),
        )
        .unwrap();
    }

    /// Two independent ambiguities, and no `conflicts` entry for either.
    const AMBIGUOUS_GRAMMAR: &str = r#"{
        "name": "unresolved_conflicts",
        "rules": {
            "source_file": {"type": "CHOICE", "members": [
                {"type": "SYMBOL", "name": "statement"},
                {"type": "SYMBOL", "name": "expr"}
            ]},
            "statement": {"type": "CHOICE", "members": [
                {"type": "SEQ", "members": [
                    {"type": "STRING", "value": "if"},
                    {"type": "SYMBOL", "name": "expr"},
                    {"type": "STRING", "value": "then"},
                    {"type": "SYMBOL", "name": "statement"}
                ]},
                {"type": "SEQ", "members": [
                    {"type": "STRING", "value": "if"},
                    {"type": "SYMBOL", "name": "expr"},
                    {"type": "STRING", "value": "then"},
                    {"type": "SYMBOL", "name": "statement"},
                    {"type": "STRING", "value": "else"},
                    {"type": "SYMBOL", "name": "statement"}
                ]}
            ]},
            "expr": {"type": "CHOICE", "members": [
                {"type": "SYMBOL", "name": "identifier"},
                {"type": "SEQ", "members": [
                    {"type": "SYMBOL", "name": "expr"},
                    {"type": "STRING", "value": "+"},
                    {"type": "SYMBOL", "name": "expr"}
                ]}
            ]},
            "identifier": {"type": "PATTERN", "value": "[a-z]+"}
        }
    }"#;

    fn generate_error_message(grammar_json: &str) -> String {
        let error = super::generate_parser_for_grammar(
            grammar_json,
            None,
            super::OptLevel::default(),
            &mut Vec::new(),
        )
        .expect_err("grammar should not generate");
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(error) = source {
            message.push_str("\n");
            message.push_str(&error.to_string());
            source = error.source();
        }
        message
    }

    #[test]
    fn test_every_unresolved_conflict_is_reported_in_one_run() {
        // Reporting only the first conflict makes a ported grammar take one `generate`
        // run per conflict pair; bison reports them all at once.
        let message = generate_error_message(AMBIGUOUS_GRAMMAR);
        assert!(
            message.contains("Unresolved conflicts for 2 symbol sequences"),
            "expected both conflicts in one message, got: {message}"
        );
        assert!(message.contains("expr '+' expr"), "{message}");
        assert!(message.contains("statement •  'else'"), "{message}");
    }

    #[test]
    fn test_a_declared_conflict_is_not_reported() {
        let grammar_json = AMBIGUOUS_GRAMMAR.replace(
            r#""name": "unresolved_conflicts","#,
            r#""name": "unresolved_conflicts", "conflicts": [["statement"], ["expr"]],"#,
        );
        assert_ne!(grammar_json, AMBIGUOUS_GRAMMAR);
        super::generate_parser_for_grammar(
            &grammar_json,
            None,
            super::OptLevel::default(),
            &mut Vec::new(),
        )
        .unwrap();
    }

    /// The diagnostics a grammar produces, as `(kind, rule name)` pairs.
    fn diagnostics_of(grammar_json: &str) -> Vec<(&'static str, String)> {
        let mut diagnostics = Vec::new();
        super::generate_parser_for_grammar(
            grammar_json,
            None,
            super::OptLevel::default(),
            &mut diagnostics,
        )
        .unwrap();
        diagnostics
            .iter()
            .map(|d| match d {
                super::Diagnostic::UnreachableRule { name } => ("unreachable", name.to_string()),
                super::Diagnostic::UnproductiveRule { name } => ("unproductive", name.to_string()),
                other => ("other", other.to_string()),
            })
            .collect()
    }

    #[test]
    fn test_a_rule_that_nothing_reaches_is_reported() {
        // `orphan` is dropped before table construction, so without this warning
        // the grammar generates a parser that silently ignores it.
        let diagnostics = diagnostics_of(
            r#"{
                "name": "unreachable_rule",
                "rules": {
                    "source_file": {"type": "SYMBOL", "name": "a"},
                    "a": {"type": "STRING", "value": "a"},
                    "orphan": {"type": "STRING", "value": "o"}
                }
            }"#,
        );
        assert_eq!(diagnostics, [("unreachable", "orphan".to_string())]);
    }

    #[test]
    fn test_a_rule_that_derives_nothing_is_reported() {
        // `a: a ok` has no base case, so no state can ever reduce to `a`. Bison
        // calls this "nonterminal useless in grammar"; tree-sitter used to build
        // a parser in which the rule simply never matched.
        let diagnostics = diagnostics_of(
            r#"{
                "name": "unproductive_rule",
                "rules": {
                    "source_file": {"type": "CHOICE", "members": [
                        {"type": "SYMBOL", "name": "a"},
                        {"type": "STRING", "value": "ok"}
                    ]},
                    "a": {"type": "SEQ", "members": [
                        {"type": "SYMBOL", "name": "a"},
                        {"type": "STRING", "value": "ok"}
                    ]}
                }
            }"#,
        );
        assert_eq!(diagnostics, [("unproductive", "a".to_string())]);
    }

    #[test]
    fn test_a_healthy_grammar_has_no_unreachable_or_unproductive_diagnostics() {
        let diagnostics = diagnostics_of(
            r#"{
                "name": "healthy",
                "rules": {
                    "source_file": {"type": "REPEAT", "content": {"type": "SYMBOL", "name": "item"}},
                    "item": {"type": "CHOICE", "members": [
                        {"type": "STRING", "value": "a"},
                        {"type": "SEQ", "members": [
                            {"type": "SYMBOL", "name": "item"},
                            {"type": "STRING", "value": "b"}
                        ]}
                    ]}
                }
            }"#,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn test_a_single_unresolved_conflict_keeps_the_detailed_report() {
        // With one conflict there is room for the item sets and the precedence that
        // would resolve it, so keep that report instead of the summary.
        let grammar_json = AMBIGUOUS_GRAMMAR.replace(
            r#",
                {"type": "SEQ", "members": [
                    {"type": "SYMBOL", "name": "expr"},
                    {"type": "STRING", "value": "+"},
                    {"type": "SYMBOL", "name": "expr"}
                ]}"#,
            "",
        );
        assert_ne!(grammar_json, AMBIGUOUS_GRAMMAR);
        let message = generate_error_message(&grammar_json);
        assert!(
            message.contains("Unresolved conflict for symbol sequence"),
            "{message}"
        );
        assert!(message.contains("Possible interpretations"), "{message}");
    }
}
