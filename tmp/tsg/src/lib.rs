//! `tsg` — a zero-dependency compiler from the declarative `.tsg` grammar
//! language to tree-sitter's `grammar.json`.
//!
//! The only contract with tree-sitter is the JSON document described by
//! `docs/src/assets/schemas/grammar.schema.json` and deserialized in
//! `crates/generate/src/parse_grammar.rs`. Everything downstream
//! (`prepare_grammar`, `build_tables`, `render`) is reused untouched, so this
//! crate is a drop-in replacement for the JavaScript DSL's preprocessing step:
//!
//! ```text
//! grammar.tsg -> tsg -> src/grammar.json -> tree-sitter generate -> parser.c
//! ```
//!
//! Two things are exported that the binary does not need, and that an embedder
//! (i.e. `crates/generate`) does:
//!
//! * [`lexer::Error`] carries a byte [`lexer::Span`], rendered rustc-style by
//!   [`lexer::render_errors`];
//! * [`source::Index`] maps *names* — rule names, directive arguments, pattern text — back to
//!   spans. `tree-sitter generate` reports conflicts and precedence mistakes by name only, because
//!   `grammar.json` has no positions; this is what turns those messages into carets.
//!
//! See `examples/` and `SYNTAX.md` for the language reference.

pub mod compile;
pub mod diff;
pub mod emit;
pub mod json;
pub mod json_read;
pub mod lexer;
pub mod lexing;
pub mod nullable;
pub mod parser;
pub mod precedence;
pub mod rules;
pub mod scanner;
pub mod source;
pub mod to_tsg;

pub use compile::{compile, compile_with, Compiled, Options};
pub use lexer::{Error, Span};
