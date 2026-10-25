//! `g2tsg` — read a joined Bison/Flex `.g` file and write a `.tsg` grammar.
//!
//! Usage:
//!
//! ```text
//! g2tsg postgres16.g -o grammar.tsg --name postgres \
//!       --header header.txt --conflicts conflicts.txt --bodies bodies.txt
//! ```
//!
//! The input is parsed by the tree-sitter grammar in `bison_flex/grammar.tsg`
//! (compiled by `build.rs`), so this is a grammar-to-grammar translation, not a
//! text substitution: every decision below is made on a node of a real parse
//! tree. `--dump` prints that tree.

mod cst;
mod emit;
mod flex;
mod ir;

#[cfg(test)]
mod tests;

use std::{ffi::c_void, path::PathBuf, process::ExitCode};

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_bison_flex() -> *const c_void;
}

/// The parser that `tree-sitter generate` produced from `bison_flex/grammar.tsg`.
fn language() -> Language {
    unsafe { Language::from_raw(tree_sitter_bison_flex().cast()) }
}

struct Args {
    input: PathBuf,
    out: Option<PathBuf>,
    name: String,
    title: String,
    header: Option<PathBuf>,
    conflicts: Option<PathBuf>,
    bodies: Option<PathBuf>,
    dump: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("g2tsg: error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args()? else {
        return Ok(()); // --help
    };
    let source =
        std::fs::read(&args.input).map_err(|error| format!("{}: {error}", args.input.display()))?;

    if args.dump {
        // The tree as it is, errors and all: this is the tool you reach for
        // when the translator rejects a file.
        let tree = cst::parse(&source, &language()).map_err(|error| error.to_string())?;
        println!("{}", tree.root_node().to_sexp());
        if tree.root_node().has_error() {
            eprintln!("note: the tree contains ERROR or MISSING nodes");
        }
        return Ok(());
    }

    let grammar = cst::read(&source, &language())
        .map_err(|error| format!("{}:{error}", args.input.display()))?;

    let header = lines_of(&args.header)?;
    let conflicts = lines_of(&args.conflicts)?;
    let bodies = pairs_of(&args.bodies)?;
    let source_name = args
        .input
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = if args.title.is_empty() {
        format!("A tree-sitter grammar translated from {source_name}.")
    } else {
        args.title.clone()
    };

    let options = emit::Options {
        name: &args.name,
        title: &title,
        source: &source_name,
        header: &header,
        conflicts: &conflicts,
        bodies: &bodies,
    };
    let translation = emit::translate(&grammar, &options)?;

    match &args.out {
        Some(path) => std::fs::write(path, &translation.text)
            .map_err(|error| format!("{}: {error}", path.display()))?,
        None => print!("{}", translation.text),
    }
    eprintln!(
        "{}: {} rules, {} tokens, {} levels",
        args.out
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "stdout".to_string()),
        translation.rules,
        translation.tokens,
        translation.levels
    );
    Ok(())
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        input: PathBuf::new(),
        out: None,
        name: "grammar".to_string(),
        title: String::new(),
        header: None,
        conflicts: None,
        bodies: None,
        dump: false,
    };
    let mut input = None;
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| rest.next().ok_or_else(|| format!("`{flag}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "-o" | "--out" => args.out = Some(PathBuf::from(value(&arg)?)),
            "--name" => args.name = value(&arg)?,
            "--title" => args.title = value(&arg)?,
            "--header" => args.header = Some(PathBuf::from(value(&arg)?)),
            "--conflicts" => args.conflicts = Some(PathBuf::from(value(&arg)?)),
            "--bodies" => args.bodies = Some(PathBuf::from(value(&arg)?)),
            "--dump" => args.dump = true,
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => {
                if input.replace(PathBuf::from(other)).is_some() {
                    return Err("more than one input file".to_string());
                }
            }
        }
    }
    args.input = input.ok_or("no input file; try `g2tsg --help`")?;
    Ok(Some(args))
}

/// Comment lines and conflicts are plain text, one entry per line, so a
/// grammar's notes stay readable next to the grammar.
fn lines_of(path: &Option<PathBuf>) -> Result<Vec<String>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(text.lines().map(str::to_string).collect())
}

/// `NAME pattern` pairs, for token bodies that Flex syntax cannot express.
fn pairs_of(path: &Option<PathBuf>) -> Result<Vec<(String, String)>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut pairs = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, pattern)) = line.split_once(char::is_whitespace) else {
            return Err(format!(
                "{}:{}: expected `NAME pattern`",
                path.display(),
                number + 1
            ));
        };
        pairs.push((name.to_string(), pattern.trim().to_string()));
    }
    Ok(pairs)
}

fn print_help() {
    println!(
        "\
g2tsg — translate a Bison/Flex .g file to a .tsg grammar

    g2tsg FILE.g [-o OUT] [--name NAME] [--title TEXT]
          [--header FILE] [--conflicts FILE] [--bodies FILE] [--dump]

  -o, --out PATH       write the grammar here (default: stdout)
      --name NAME      %name of the grammar (default: grammar)
      --title TEXT     first comment line of the output
      --header FILE    comment lines copied into the output
      --conflicts FILE %conflicts lines; `#` starts a comment
      --bodies FILE    NAME pattern, token bodies hand-translated from Flex
      --dump           print the parse tree (s-expression) and exit

The input is read with tree-sitter, using bison_flex/grammar.tsg."
    );
}
