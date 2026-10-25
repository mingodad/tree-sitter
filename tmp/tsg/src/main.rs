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

use std::io::{Read, Write};
use std::process::ExitCode;

use tsg::{compile, json_read, lexer, lexing, precedence, to_tsg};

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
}

fn usage() -> String {
    [
        "tsg — compile a declarative grammar to tree-sitter's grammar.json",
        "",
        "USAGE:",
        "    tsg [INPUT.tsg] [-o OUTPUT.json]",
        "    tsg --from-json [INPUT.json] [-o OUTPUT.tsg]",
        "",
        "    INPUT   read from this file, or stdin if omitted or `-`",
        "    -o      write to this file instead of stdout",
        "",
        "OPTIONS:",
        "    -r, --from-json            reverse mode: translate grammar.json into .tsg",
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
    let rendered = if reversed {
        let grammar = match json_read::parse(&source) {
            Ok(grammar) => grammar,
            Err(err) => {
                eprint!("{}", lexer::render_error(&source, &display_name, &err));
                return ExitCode::FAILURE;
            }
        };
        match to_tsg::grammar_to_tsg(&grammar) {
            Ok(text) => text,
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

    ExitCode::SUCCESS
}
