//! `tsg` — a zero-dependency compiler from the declarative `.tsg` grammar
//! language to tree-sitter's `grammar.json`.
//!
//! The only contract with tree-sitter is the JSON document described by
//! `docs/src/assets/schemas/grammar.schema.json` and deserialized in
//! `crates/generate/src/parse_grammar.rs`. Everything downstream
//! (`prepare_grammar`, `build_tables`, `render`) is reused untouched, so this
//! tool is a drop-in replacement for the JavaScript DSL's preprocessing step:
//!
//! ```text
//! grammar.tsg -> tsg -> src/grammar.json -> tree-sitter generate -> parser.c
//! ```
//!
//! See `examples/` and `SYNTAX.md` for the language reference.

use std::{
    io::{Read, Write},
    process::ExitCode,
};

use tsg::{
    annotate, compile, diff, emit, json, json_read, lexer, lexing, precedence, scanner, to_tsg,
};

struct Options {
    input: Option<String>,
    output: Option<String>,
    /// `--from-json`: translate `grammar.json` back into `.tsg`.
    reverse: bool,
    /// `--allow-ignored-flags`: keep flags tree-sitter would ignore, instead of
    /// rejecting them. For reproducing `grammar.js` output byte for byte.
    allow_ignored_flags: bool,
    /// `--precedences`: print the resolved precedence table instead of JSON.
    show_precedences: bool,
    /// `--explain-lexing` / `--explain-modes`: print the token priority list
    /// and the lexical inputs the generator derives lexer states from.
    show_lexing: bool,
    /// `--diff OTHER`: say whether two grammars differ, and where. Compares the
    /// lowered grammars, so comments and formatting never matter.
    diff: Option<String>,
    /// `--fmt`: re-print the grammar from its AST instead of compiling it.
    format: bool,
    /// `--annotate`: like `--fmt`, plus comments about what the front end knows
    /// (token priority, dead tokens, precedence levels, emptiness).
    annotate: bool,
    /// `--check`: like `--fmt`, but write nothing and fail if the file is not
    /// already canonical.
    check: bool,
}

fn usage() -> String {
    [
        "tsg — compile a declarative grammar to tree-sitter's grammar.json",
        "",
        "USAGE:",
        "    tsg [INPUT.tsg] [-o OUTPUT.json]",
        "    tsg --from-json [INPUT.json] [-o OUTPUT.tsg]",
        "    tsg --diff OTHER [INPUT.tsg]",
        "    tsg --fmt [INPUT.tsg] [-o OUTPUT.tsg]",
        "    tsg --annotate [INPUT.tsg] [-o OUTPUT.tsg]",
        "",
        "    INPUT   read from this file, or stdin if omitted or `-`",
        "    -o      write to this file instead of stdout",
        "",
        "OPTIONS:",
        "    -r, --from-json            reverse mode: translate grammar.json into .tsg",
        "        --diff OTHER           compare INPUT and OTHER as grammars, not as text",
        "                               (exit 1 and report the first differences)",
        "    -f, --fmt                  re-print the grammar in canonical form",
        "    -a, --annotate             re-print it with what tsg knows as comments:",
        "                               token priority, tokens nothing produces, which",
        "                               alternatives a precedence level marks, emptiness",
        "        --check                fail if the grammar is not already canonical",
        "    -p, --precedences          print the resolved precedence table",
        "        --explain-lexing       print token priority and derived lexer inputs",
        "        --explain-modes        alias for --explain-lexing",
        "        --allow-ignored-flags  accept `/x/g`-style flags tree-sitter ignores",
        "                               (they are reported, but do not fail)",
        "    -h, --help                 print this message",
        "    -V, --version              print the version",
        "",
        "Reverse mode is also selected automatically for an INPUT ending in `.json`.",
        "",
    ]
    .join("\n")
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        input: None,
        output: None,
        reverse: false,
        allow_ignored_flags: false,
        show_precedences: false,
        show_lexing: false,
        diff: None,
        format: false,
        annotate: false,
        check: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("tsg {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-r" | "--from-json" => options.reverse = true,
            "-p" | "--precedences" => options.show_precedences = true,
            "--explain-lexing" | "--explain-modes" => options.show_lexing = true,
            "--allow-ignored-flags" => options.allow_ignored_flags = true,
            "-f" | "--fmt" => options.format = true,
            "-a" | "--annotate" => options.annotate = true,
            "--check" => options.check = true,
            "--diff" => match args.next() {
                Some(path) => options.diff = Some(path),
                None => return Err("`--diff` requires a second grammar".to_owned()),
            },
            "-o" | "--output" => match args.next() {
                Some(path) => options.output = Some(path),
                None => return Err("`-o` requires a file path".to_owned()),
            },
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option `{other}`"))
            }
            other => {
                if options.input.is_some() {
                    return Err("only one input file may be given".to_owned());
                }
                options.input = Some(other.to_owned());
            }
        }
    }
    Ok(options)
}

fn main() -> ExitCode {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!();
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
    };

    let (source, display_name) = match &options.input {
        Some(path) if path != "-" => match std::fs::read_to_string(path) {
            Ok(source) => (source, path.clone()),
            Err(err) => {
                eprintln!("error: cannot read `{path}`: {err}");
                return ExitCode::FAILURE;
            }
        },
        _ => {
            let mut source = String::new();
            if let Err(err) = std::io::stdin().read_to_string(&mut source) {
                eprintln!("error: cannot read stdin: {err}");
                return ExitCode::FAILURE;
            }
            (source, "<stdin>".to_owned())
        }
    };

    if let Some(other) = &options.diff {
        return compare_grammars(&options, &source, &display_name, other);
    }

    if options.annotate {
        return annotate_grammar(&options, &source, &display_name);
    }

    if options.format || options.check {
        return format_grammar(&options, &source, &display_name);
    }

    if options.show_precedences {
        match precedence::describe(&source) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprint!("{}", lexer::render_error(&source, &display_name, &err));
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    if options.show_lexing {
        match lexing::describe(&source) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprint!("{}", lexer::render_error(&source, &display_name, &err));
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    let reversed = options.reverse || display_name.ends_with(".json");
    let mut scanner = None;
    let mut externals_without_scanner: Vec<String> = Vec::new();
    let rendered = if reversed {
        let grammar = match json_read::parse(&source) {
            Ok(grammar) => grammar,
            Err(err) => {
                eprint!("{}", lexer::render_error(&source, &display_name, &err));
                return ExitCode::FAILURE;
            }
        };
        match to_tsg::grammar_to_tsg(&grammar) {
            Ok(text) => {
                // `%external name = delimited(…)` is a *scanner*, and a scanner
                // is C in `src/scanner.c`: `grammar.json` only records the token
                // name. Say so, or the declaration disappears without a trace.
                let external = text
                    .lines()
                    .filter(|line| line.starts_with("%external "))
                    .count();
                if external > 0 {
                    eprintln!(
                        "note: {display_name} declares {external} external token{}, but the \
                         scanner's code is not in a grammar.json; if the source used \
                         `%external NAME = delimited(open, close)`, the generated \
                         `src/scanner.c` has to be regenerated from the `.tsg`",
                        if external == 1 { "" } else { "s" }
                    );
                }
                text
            }
            Err(message) => {
                eprintln!("error: {display_name}: {message}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match compile::compile_with(
            &source,
            compile::Options {
                allow_ignored_flags: options.allow_ignored_flags,
            },
        ) {
            Ok(compiled) => {
                if !compiled.warnings.is_empty() {
                    eprint!(
                        "{}",
                        lexer::render_warnings(&source, &display_name, &compiled.warnings)
                    );
                }
                scanner = compiled.scanner;
                externals_without_scanner = compiled.externals_without_scanner;
                compiled.json
            }
            Err(errors) => {
                eprint!("{}", lexer::render_errors(&source, &display_name, &errors));
                return ExitCode::FAILURE;
            }
        }
    };
    let json = rendered;

    match &options.output {
        Some(path) => {
            if let Err(err) = std::fs::write(path, &json) {
                eprintln!("error: cannot write `{path}`: {err}");
                return ExitCode::FAILURE;
            }
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            if let Err(err) = stdout.write_all(json.as_bytes()) {
                eprintln!("error: cannot write to stdout: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    for name in &externals_without_scanner {
        eprintln!(
            "note: {display_name}: external token `{name}` has no generated scanner, so it will \
             never match; declare it as `%external {name} = delimited(open, close);` or supply \
             `src/scanner.c` yourself"
        );
    }

    if let Some(source_text) = &scanner {
        if let Err(message) = write_scanner(&options, source_text) {
            eprintln!("error: {display_name}: {message}");
            return ExitCode::FAILURE;
        }
    }

    ExitCode::SUCCESS
}

/// Write `scanner.c` beside the `-o` output.
///
/// The file is a build artifact of the grammar, so it goes where `parser.c`
/// goes: `tsg x.tsg -o src/grammar.json` writes `src/scanner.c`. A file that
/// `tsg` did not write is never overwritten — a hand-written scanner is somebody
/// else's grammar, and silently replacing it would break it in a way that only
/// shows up at parse time.
fn write_scanner(options: &Options, source: &str) -> Result<(), String> {
    let Some(output) = &options.output else {
        eprintln!(
            "note: no `-o`, so `scanner.c` was not written; pass `-o src/grammar.json` to put it \
             beside `parser.c`"
        );
        return Ok(());
    };
    let path = std::path::Path::new(output)
        .parent()
        .map_or_else(|| std::path::PathBuf::from("scanner.c"), |dir| dir.join("scanner.c"));
    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.starts_with(scanner::MARKER) {
            return Err(format!(
                "`{}` exists and was not written by tsg; move it aside first if it is yours",
                path.display()
            ));
        }
    }
    std::fs::write(&path, source)
        .map_err(|err| format!("cannot write `{}`: {err}", path.display()))
}

fn annotate_grammar(options: &Options, source: &str, display_name: &str) -> ExitCode {
    let output = match annotate::annotate(source) {
        Ok(output) => output,
        Err(err) => {
            eprint!("{}", lexer::render_error(source, display_name, &err));
            return ExitCode::FAILURE;
        }
    };
    match &options.output {
        Some(path) => {
            if let Err(err) = std::fs::write(path, &output.text) {
                eprintln!("error: cannot write `{path}`: {err}");
                return ExitCode::FAILURE;
            }
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            if let Err(err) = stdout.write_all(output.text.as_bytes()) {
                eprintln!("error: cannot write to stdout: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn format_grammar(options: &Options, source: &str, display_name: &str) -> ExitCode {
    let output = match emit::fmt(source) {
        Ok(output) => output,
        Err(err) => {
            eprint!("{}", lexer::render_error(source, display_name, &err));
            return ExitCode::FAILURE;
        }
    };
    if output.interior_comments > 0 {
        eprintln!(
            "note: {display_name}: {} comment line{} after the leading block {} not carried; \
             keep comments in the file header, or compare with --diff",
            output.interior_comments,
            if output.interior_comments == 1 {
                ""
            } else {
                "s"
            },
            if output.interior_comments == 1 {
                "is"
            } else {
                "are"
            },
        );
    }
    if options.check {
        if output.text == source {
            return ExitCode::SUCCESS;
        }
        eprintln!("{display_name}: not in canonical form (run `tsg --fmt -o {display_name}`)");
        return ExitCode::FAILURE;
    }
    match &options.output {
        Some(path) => {
            if let Err(err) = std::fs::write(path, &output.text) {
                eprintln!("error: cannot write `{path}`: {err}");
                return ExitCode::FAILURE;
            }
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            if let Err(err) = stdout.write_all(output.text.as_bytes()) {
                eprintln!("error: cannot write to stdout: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Parse a grammar from either dialect: a `.tsg` source or a `grammar.json`
/// document, chosen by the file name (and by `-r` for stdin).
fn grammar_of(source: &str, display_name: &str, options: &Options) -> Result<json::Json, String> {
    if options.reverse || display_name.ends_with(".json") {
        return json_read::parse(source)
            .map_err(|err| lexer::render_error(source, display_name, &err));
    }
    let compiled = compile::compile_with(
        source,
        compile::Options {
            allow_ignored_flags: options.allow_ignored_flags,
        },
    )
    .map_err(|errors| lexer::render_errors(source, display_name, &errors))?;
    json_read::parse(&compiled.json).map_err(|err| format!("error: {display_name}: {err}"))
}

fn read_source(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut source = String::new();
        return std::io::stdin()
            .read_to_string(&mut source)
            .map(|_| source)
            .map_err(|err| format!("error: cannot read stdin: {err}"));
    }
    std::fs::read_to_string(path).map_err(|err| format!("error: cannot read `{path}`: {err}"))
}

fn compare_grammars(
    options: &Options,
    left_source: &str,
    left_name: &str,
    right_path: &str,
) -> ExitCode {
    let left = match grammar_of(left_source, left_name, options) {
        Ok(grammar) => grammar,
        Err(message) => {
            eprint!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let right_source = match read_source(right_path) {
        Ok(source) => source,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let right = match grammar_of(&right_source, right_path, options) {
        Ok(grammar) => grammar,
        Err(message) => {
            eprint!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let differences = diff::compare(&left, &right);
    if differences.is_empty() {
        println!("no difference: {left_name} and {right_path} are the same grammar");
        return ExitCode::SUCCESS;
    }
    let shown = 20;
    for difference in differences.iter().take(shown) {
        println!("{}:", difference.path);
        println!("  - {}", difference.left);
        println!("  + {}", difference.right);
    }
    if differences.len() > shown {
        println!("… and {} more", differences.len() - shown);
    }
    eprintln!(
        "{} difference{} between {left_name} and {right_path}",
        differences.len(),
        if differences.len() == 1 { "" } else { "s" }
    );
    ExitCode::FAILURE
}
