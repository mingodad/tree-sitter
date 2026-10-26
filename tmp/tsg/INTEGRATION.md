# How to integrate `tsg` into tree-sitter

Findings from reading the actual pipeline at `/Volumes/RamDisk/tree-sitter`
(0.28.0, `crates/generate` 51 files, `crates/cli` 2.2k lines). Nothing below is
speculative about the existing code; the two measurements marked **[measured]**
were run this session.

## The one fact that shapes everything

The JS DSL is *not* used by the parser generator. It is used by exactly one
function, `load_grammar_file()` (`crates/generate/src/generate.rs:607`), whose
entire contract is `Path -> String` where the string is the JSON document
deserialized by `parse_grammar()`. Everything after that point
(`prepare_grammar`, `build_tables`, `render`, `node_types`) has never seen
JavaScript.

```
generate_parser_in_directory()            generate.rs:335
  └─ load_grammar_file(&grammar_path)     generate.rs:607   ← the only seam
       ├─ ".js"   → load_js_grammar_file  (quickjs or spawn node/bun/deno)
       ├─ ".json" → fs::read_to_string
       └─ other   → Err(FileExtension)
  → fs::write(src/grammar.json)           generate.rs:397   (skipped if input *is* grammar.json)
  → parse_grammar(&grammar_json)          parse_grammar.rs
  → generate_parser_for_grammar_with_opts → render → src/parser.c
```

`tree-sitter build`, `build --wasm`, the bindings, CMake, npm, and `tree-sitter
test` all consume `src/parser.c` / `src/grammar.json`, none of them mention
`grammar.js`. So **`.tsg` is a front-end-only change**: it adds a third arm to a
three-arm `match`, and `grammar.json` remains the interchange format of record.
That is the strongest argument for this integration and the reason it can land
incrementally.

## Level 0 — do nothing (status quo)

`tsg grammar.tsg > src/grammar.json && tree-sitter generate src/grammar.json`.
Works today, uses the existing `.json` arm therefore no tree-sitter change at
all. Costs: two steps in every Makefile/CMake, and error spans from `tsg` are
printed without tree-sitter being able to treat them as diagnostics. Fine for
experimentation, not for adoption — `tree-sitter generate grammar.tsg` not
working is the whole difference between "a tool you can also use" and "a
supported grammar format".

## Level 1 — make `tree-sitter generate grammar.tsg` work (the real integration)

This is the minimum shippable unit. Four files, ~40 lines.

### 1a. Put the compiler in the workspace as a crate

`crates/declarative/` — package `tree-sitter-declarative` (lib
`tree_sitter_declarative`), `[dependencies]` empty, `edition.workspace`,
`rust-version.workspace`, `[[bin]] name = "tsg"` kept so the standalone binary
and its fast edit/compile loop survive and `cargo install tree-sitter-declarative`
gives old-tree-sitter users the migration tool.

Move `tmp/tsg/src/*.rs` → `crates/declarative/src/`, add `lib.rs`:

```rust
pub mod compile; pub mod json; pub mod json_read;
pub mod lexer; pub mod parser; pub mod rules; pub mod to_tsg;

pub use compile::{compile, compile_with, Compiled, Options};
pub use lexer::{Error, Span, render_error, render_errors, render_warnings};
pub fn to_tsg(json: &str) -> Result<String, Error>;   // thin wrapper over json_read + to_tsg
```

Alternatives considered:

| placement | pro | con |
|---|---|---|
| `crates/declarative` (recommended) | usable by generate *and* cli (migration command), own tests, publishable, feature-gated in generate | one more crate in the release train |
| `crates/generate/src/declarative/` | no Cargo plumbing | reverse mode trapped behind `load`; `[lib] path = src/generate.rs` means no public surface; generate grows a 4k-line sublanguage |
| keep out-of-tree, shell out to `tsg` | zero tree-sitter change | second binary to install; no spans; defeats "zero dependency Rust" |

**[measured]** the crate is cheap: `edition = "2024"` compiles unmodified, release
build 0.66 s, 3936 lines, and the workspace's pedantic+nursery clippy config
produces **62 warnings** in it (24 `unnecessary structure name repetition`, 12
`mod X`+`use X`, 7 `this could be a &str`, 4 identical match arms, 4 missing
Cargo metadata, rest small) — an afternoon, not a rewrite. Compile-to-JSON is
**2–4 ms** end-to-end including process start (cpp 3.6, rust 2.4, python 2.1),
i.e. below the noise floor of `tree-sitter generate` itself and ~10× faster than
`node` cold start.

### 1b. `crates/generate/Cargo.toml`

```toml
[features]
load = [ "dep:semver", "dep:tree-sitter-declarative" ]

[dependencies]
tree-sitter-declarative = { path = "../declarative", optional = true, version = "0.1" }
```

Optional and under `load`, exactly like `semver`, so
`cargo build -p tree-sitter-generate --no-default-features` is unchanged and the
parser-generator-only build stays dependency-free of it.

### 1c. `crates/generate/src/generate.rs` — the seam

Three edits.

```rust
// 1. load_grammar_file (line 614)
match grammar_path.extension().and_then(|e| e.to_str()) {
    Some("js")  => Ok(load_js_grammar_file(grammar_path, js_runtime)?),
    Some("tsg") => Ok(load_tsg_grammar_file(grammar_path)?),
    Some("json")=> Ok(fs::read_to_string(grammar_path).map_err(...)?),
    _ => Err(LoadGrammarError::FileExtension(grammar_path.to_owned()))?,
}

// 2. new LoadGrammarError variant
#[error(transparent)]
Declarative(#[from] DeclarativeError),

// 3. repo-root derivation (line 371) — .tsg sits next to grammar.js
Some(e) if e == "tsg" => path_buf.parent(),
```

and the loader itself:

```rust
#[cfg(feature = "load")]
fn load_tsg_grammar_file(path: &Path) -> Result<String, DeclarativeError> {
    let src = fs::read_to_string(path)
        .map_err(|e| DeclarativeError::io(path, e))?;
    match tree_sitter_declarative::compile(&src) {
        Ok(compiled) => Ok(compiled.json),          // warnings → diagnostics, see 1d
        Err(errors) => Err(DeclarativeError::new(path, &src, &errors)),
    }
}
```

Note what needs **no** change: the `if grammar_path.file_name() != "grammar.json"`
write-back at line 397 already does the right thing for `.tsg` — it (re)writes
`src/grammar.json`, which is exactly the byproduct we want committed.

### Status (after `--dump-expanded`)

Levels 1c/1e are implemented **by subprocess**, not in-process: `load_grammar_file`
dispatches `*.tsg` to the `tsg` binary (`TREE_SITTER_TSG`, else `PATH`), and
`default_grammar_path` finds `grammar.tsg` when there is no `grammar.js`. So
`tree-sitter generate` and `tree-sitter generate --dump-expanded -` both work on
a directory with nothing but a declarative grammar, and `src/grammar.json` is
still written as a by-product.

What that buys: tsg's stderr reaches the author verbatim, caret diagnostics and
all; three tests in `generate.rs` pin the seam (stdout is the grammar, stderr is
the error, a missing binary names `TREE_SITTER_TSG`).

What it does not buy: the structured spans below. The subprocess road keeps
`rendered` and loses `diagnostics`, so `--json-summary` cannot report positions
and an editor has to re-parse the text. If Level 1.5 is ever built, this is the
part that gets replaced by 1a–1c. When both `grammar.js` and `grammar.tsg` exist,
`grammar.js` wins and a warning is logged, per the 1e policy below.

### 1d. Error type (the reason to do this in-process rather than shelling out)

`LoadGrammarError` and `GenerateError` both derive `Serialize, Deserialize,
PartialEq, Eq` (the CLI's `--json-summary` serializes them), and `Diagnostic`
carries no positions at all. So the spans cannot ride on `Diagnostic`; they need
their own type in `generate.rs`:

```rust
#[cfg(feature = "load")]
#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
#[error("{rendered}")]
pub struct DeclarativeError {
    pub path: PathBuf,
    pub rendered: Box<str>,                  // the caret diagnostics, ready to print
    pub diagnostics: Box<[DeclarativeDiagnostic]>,   // the same data, structured
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeclarativeDiagnostic {
    pub message: Box<str>,
    pub help: Option<Box<str>>,
    pub start_byte: usize, pub end_byte: usize,
    pub start_row: usize,  pub start_column: usize,
    pub end_row: usize,    pub end_column: usize,
}
```

`rendered` keeps `anyhow`/`warn!` paths readable without touching the CLI;
`diagnostics` is what an editor or `--json-summary` consumer actually wants.
Non-fatal `tsg` warnings (e.g. `--allow-ignored-flags` leftovers) can become a
`Diagnostic::Declarative(DeclarativeDiagnostic)` variant so they flow through the
existing `Vec<Diagnostic>` channel.

Cheap shortcut if you want Level 1 in one commit: `Declarative(Box<str>)` holding
`rendered` only, and add the structured fields when something consumes them.

### 1e. Directory argument / no argument

`generate_parser_in_directory` currently defaults to `repo_path.join("grammar.js")`
in two places (lines 367 and 420). Recommendation: keep `grammar.js` as the
default when only it exists; when **both** `grammar.js` and `grammar.tsg` exist
and no explicit path was given, **error** with "pass the grammar path explicitly:
found `grammar.js` and `grammar.tsg`". Silent preference is exactly the class of
bug the `.tsg` language exists to prevent, and it is a one-line policy either
way. (Open question — see below.)

## Level 1.5 — error fidelity (this is the actual payoff, and it is not free)

Level 1 changes *which command you type*, not *what you are told*: `.tsg`
syntax/semantic errors are already printed with carets today because `tsg`
reports them itself. The errors that are bad today are the ones raised **after**
`grammar.json` exists, and those never see the source. Measured, on
`%extras none; file: expr; expr: expr "+" expr | expr "*" expr | number; number: /[0-9]+/;`:

```
$ tsg grammar.tsg            # exit 0, no complaint — the grammar is well formed
$ tree-sitter generate src/grammar.json
Error: Error when generating parser
Caused by:
    Unresolved conflict for symbol sequence:

      expr  '+'  expr  •  '+'  …

    Possible interpretations:
      1:  (expr  expr  '+'  expr)  •  '+'  …
      2:  expr  '+'  (expr  expr  •  '+'  expr)

    Possible resolutions:
      1:  Specify a left or right associativity in `expr`
      2:  Add a conflict for these rules: `expr`
```

No file, no line, no caret — and this is the most frequently hit error in
grammar development. So the integration must add an annotation layer on top of
the seam, or the answer to "does this improve error messages?" is no.

### Why the layer is needed

`GenerateError`/`Diagnostic` are position-free by construction:

| error (post-JSON) | carries | can it be placed? |
|---|---|---|
| `ConflictError` | `variable_name`, `preceding_symbols`, `conflicting_lookahead`, interpretations | **yes** — all names |
| `AmbiguousExtraError` | `parent_symbols` | yes |
| `ImproperNonTerminalExtra` | one symbol name | yes |
| `IndirectRecursionError` | a chain of names | yes — one caret per link |
| `UndeclaredPrecedenceError` | `precedence`, `rule` | yes |
| `ConflictingPrecedenceOrderingError` | two precedence names | yes |
| `NonTerminalWordTokenError` | `symbol_name` | yes (`%word`) |
| `SuperTypeCycleError` | list of names | yes |
| `EmptyStringExtraError` | the pattern text | yes — match pattern text to a `%extra` literal |
| `NonAsciiByteClassError` | byte range **inside** the pattern | yes, if pattern→source spans are kept |
| `Diagnostic::UnaryChoice`/`UnarySeq` | rule name | yes |
| `Diagnostic::SupertypeInlined` | rule name | yes |
| `Diagnostic::UnnecessaryConflicts` | rule-name sets | yes (`%conflicts`) |
| `ParseGrammarError` (whole class) | — | **n/a** — unreachable for `.tsg`, tsg emits the shape by construction |

So the only thing missing is **name → span**, and the AST already has it:
`GrammarAst.rules: Vec<(String, Expr)>` with `Expr.span`, plus spans on
`inline`/`supertypes`/`conflicts`/`precedences`/`reserved`/`externals`
(`parser.rs:57–76`). Nothing has to be threaded through lowering.

### Design: keep a `DeclarativeSource` and elaborate on failure

**Built and run — see "Prototype results" below.** What exists in the crate now:

```rust
// crates/declarative (today: tmp/tsg/src/source.rs)
pub struct Entry { pub kind: Kind, pub name: String, pub span: Span }
pub struct Index {
    rules: Vec<(String, Span, Span)>,     // name -> name span, body span
    directives: Vec<Entry>,               // %extra %word %inline %supertypes %conflicts %precedences
    patterns: Vec<(String, Span)>,        // regex literal text -> source span
}
impl Index {
    pub fn from_ast(ast: &GrammarAst) -> Self;
    pub fn rule(&self, name: &str) -> Option<Span>;
    pub fn rule_body(&self, name: &str) -> Option<Span>;
    pub fn directive(&self, kind: &str, name: &str) -> Option<Span>; // falls back to any of that kind
    pub fn pattern(&self, text: &str) -> Option<Span>;               // exact, then substring
}
pub struct Label { pub span: Span, pub text: String, pub primary: bool }
pub fn render(src: &str, path: &str, kind: &str, headline: &str, labels: &[Label], help: Option<&str>) -> String;
```

`Index` is built inside `compile()` and returned as `Compiled.index`, so the
loader in `generate.rs` gets it for free next to `grammar_json`; `src` and
`path` are whatever the caller already has. `render` is rustc-shaped
(`-->` anchor, `...` gap collapsing, one underline row per non-overlapping
group, tab-aware, `= help:` footer), and `Lexer::render_error(s)` now uses it,
which is why the existing pre-JSON messages got slightly better too.


Target output for the capture above:

```
error: unresolved conflict in rule `expr`
  --> grammar.tsg:4:1
   |
 4 | expr: expr "+" expr
   | ^^^^ defined here
 5 |     | expr "*" expr
 6 |     | number;
   |
   = help: give the operands a precedence and associativity, or add `%conflicts expr;`
```

Honest limit: the index knows the **rule**, not which alternative or which
sub-expression — the conflict is a table state, not a syntactic position. So the
caret lands on the definition and the alternatives are listed beneath it. That is
the same resolution users get from the JS DSL's error today, plus a position.

No new formatting was needed: the renderer is `source::render`, and
`lexer::render_diagnostic` now delegates to it, so pre-JSON and post-JSON
messages look the same.

### Prototype results

`tmp/tsg/prototype/` runs the whole thing end to end against the real generator
(transcript: `prototype/OUTPUT.md`). Nine grammars that `tsg` accepts and the
generator rejects or warns about; every one comes out with a file, a line, a
caret and, where the generator knows one, a `help:` line:

```
error: unresolved conflict in rule `expr`
 --> .run/conflict.tsg:4:1
  |
4 | expr: expr "+" expr
  | ^^^^ `expr` is reduced here, `'+'` is the lookahead
  |
  = help: give `expr` an associativity: `prec.left(…)` or `prec.right(…)`, or accept the ambiguity with `%conflicts expr;`
```

```
error: indirectly recursive rule: _a -> _b -> _c -> _a
 --> .run/indirect.tsg:5:1
  |
5 | _a: _b;
  | ^^ cycle starts here
6 | _b: _c;
  | ^^ `_b` leads back to the start
7 | _c: _a;
  | ^^ `_c` leads back to the start
  |
  = help: the cycle consumes nothing, so it can never terminate; make one production consume a token
```

```
error: an extra token that matches the empty string would loop forever
 --> .run/empty_extra.tsg:1:8
  |
1 | %extra ws;
  |        ^^ this extra can match nothing
2 | ws: /\s*/;
  |     ^^^^^ the empty match comes from here
  |
  = help: require at least one character: `+` instead of `*`, or drop the optional part
```

Covered: `Conflict`, `IndirectRecursion`, `UndeclaredPrecedence`,
`NonTerminalWordToken`, `FlattenGrammar::EmptyString`,
`ValidateExtras::EmptyStringExtra`, `SupertypeTerminal`,
`Diagnostic::{UnarySeq, SupertypeInlined, UnnecessaryConflicts}`. Not reached by
a fixture, though the arms exist: `AmbiguousExtra` and `ImproperNonTerminalExtra`
(both need composed grammars to trigger) and the `ProcessInlines` family.

#### Dogfooding: a real grammar, not fixtures

`tmp/tsg/lua53/` ports a 336-line Lemon+Flex Lua 5.3 grammar (185 productions,
56 terminals) to `.tsg` and runs it through `tsg` → `tree-sitter generate` →
`parser.c`. It parses the source file's own test program, plus two stress files,
with no diagnostics — so the notional pipeline works on something the size of a
real grammar, at 142 lines of `.tsg`.

Four things came out of it, and they reorder the roadmap (`lua53/DOGFOODING.md`
has the annotated version):

1. **A pre-emption beaten by a bug.** `%precedence call …;` declares a *rule
   reference*, not a precedence *name*, so every `prec(call, …)` is undeclared.
   The generator's message has no position; `tsg` had both ends and said nothing.
   Now it rejects the directive, once, with a paste-ready fix (~50 lines in
   `compile.rs`). This is the case for pre-emption over elaboration: the
   elaborator could only point at `stat:` and suggest a fix that was already done.
   Same run also surfaced that one existing `tsg` test had never executed.
2. **Two features are worth more than any error message.** Per-use precedence
   (Lemon's `[LPAREN]`) cost 18 `prec.right(call, …)` wrappers around
   *references*, because tree-sitter marks definitions instead of uses; and
   empty-rule elimination (`semi`, `statlist`, `parlist` are all legal-but-empty
   in the source) cost 18 hand-placed `optional(…)` and two wrong guesses. Both
   are front-end-only, both are mechanically derivable from the AST.
3. **A conflict is where the elaborator pays.** The `stat • '('` conflict is the
   one class `tsg` cannot pre-empt, and the elaborated form named both rules and
   derived `%conflicts stat funccall;` from `possible_resolutions`.
4. **A silent misparse upstream.** `STRING: choice(token(…), token(…))` makes
   tree-sitter insert a zero-width token and exit 0 on garbage; one pattern
   reports `MISSING` correctly, and `grammar.js` misbehaves identically. Nothing
   in the pipeline warns, which is the argument for L1.5 as a *gate* and for a
   `tsg` lint on token-choices.

The full list of what the translation needed and did not have — per-use
precedence (`funccall@call`), `%left`/`%right` with a rightmost-terminal default,
automatic empty-rule elimination, a `--precedences` dump, terminal aliases, and
`%conflicts` with a reason — is `lua53/DOGFOODING.md` §3, each with the grammar it
would have replaced and a size estimate.

#### How it was run, and why that is also a finding

This machine has no crates.io access **and no cargo registry cache**, so
`tree-sitter-generate` cannot be linked here at all (`bitflags` unresolvable).
The prototype therefore calls the prebuilt `bin/tree-sitter generate
src/grammar.json --json-summary` and decodes the envelope with
`tsg::json_read::parse` (zero deps). That turned out to be a better test anyway:
serde's external tagging means `{"Prepare":{"ValidateIndirectRecursion":[…]}}`
*is* `GenerateError::Prepare(PrepareGrammarError::ValidateIndirectRecursion(…))`,
so the decoding table in `prototype/src/summary.rs` is a one-for-one stand-in
for the `match` that lands in `generate.rs`. It also proves the
`--json-summary`/editor story, since those positions arrive as JSON.

Five things the prototype changed my mind about:

1. **`ConflictError` does not name the rule.** There is no `variable_index`; the
   only rule name is `possible_interpretations[0].variable_name` (with
   `symbol_sequence` as fallback). Anyone writing the elaboration has to know
   that; it is worth a comment in `generate.rs` rather than rediscovering it.
2. **`possible_resolutions` is serialized** (I had assumed it was skipped), so the
   `help:` line can be derived from the generator's own suggestions instead of
   hard-coded prose — `Associativity` → `prec.left/right`, `AddConflict` →
   `%conflicts a b;`, `Precedence` → `%precedences`. That is what the transcript
   shows. It also means `Resolution` and `Interpretation` should be **re-exported**
   from `tree-sitter-generate`: they are reachable through `ConflictError`'s
   public fields but cannot be *named* by an outside crate, which forces any
   out-of-process elaborator to re-declare them.
3. **Newtype errors collapse to bare scalars in JSON.**
   `EmptyStringExtraError(Box<str>)` serializes as `{"ValidateExtras":"ws"}`,
   not `{"ValidateExtras":{"EmptyStringExtra":"ws"}}`. Fine for a human, poor
   as a versioned editor schema; if `--json-summary` is to be a contract, these
   want explicit struct fields. Same class: `PrepareGrammarError` is
   `#[error(transparent)]` but not `#[serde(transparent)]`-shaped per variant.
4. **`Diagnostic` really is position-free.** `UnarySeq{name}` and
   `UnnecessaryConflicts(sets)` only carry names, so warnings are rule-granular
   (the caret lands on the rule name and a note on its body) unless `Diagnostic`
   gains a span. The prototype shows the awkward version; the pre-emption in
   `compile.rs` below is the fix, and it also gives exact spans.
5. **Bad fixtures are half the work.** Finding grammars that actually reach each
   arm took longer than the elaboration: e.g. an *anonymous* nullable extra
   (`%extra /[a-z]*/;`) is accepted, while a *named* one (`%extra ws; ws: /\s*/;`)
   hits `ValidateExtras`; a single-element `seq` only warns once it is wrapped in
   a `choice` (`expr: choice(seq("x"))`). These belong in the integration's test
   suite as `.tsg` files with expected renders.


### Cheaper still: catch some of these *before* JSON

Several post-JSON complaints are decidable from the AST alone, so they can move
into `compile.rs` and get **exact** spans instead of rule-level ones:

* `UndeclaredPrecedence` — **done for the common cause**: a bare `%precedence`
  entry is a rule reference, so `tsg` now reports the quoting mistake at the
  directive with the corrected text. A precedence *level* (`%precedence left
  "add" = '+' '-';`) removes the rest of this class, because the level name is
  declared once and inherited;
* nullability (`EmptyString` / `FlattenGrammar::EmptyString`) — **done**:
  `src/nullable.rs` runs the same fixpoint `flatten_grammar.rs` does (`used` +
  a zero-step production + not `requires_eof_lookahead`), so the caret lands on
  the alternative that can match nothing and the help names the use sites. The
  corpus is the proof: 2 of 92 grammars are refused, and both are
  `expected_error.txt` fixtures that the generator refuses too;
* `SupertypeInlined` — tsg knows both `%supertypes` and `%inline` lists;
* `UnarySeq` / `UnaryChoice` — visible in the AST;
* `NonTerminalWordToken` — visible in the AST (`%word` target vs rule shape).

That both improves the message and deletes the class from the "needs the
annotation layer" bucket. ~150 lines, no new dependency, no `regex-syntax`.

### Work estimate

| piece | lines | status |
|---|---|---|
| `source.rs`: index + `Label` + rustc renderer | **430** | **done** (in `tmp/tsg`, tests pass) |
| `parser.rs`: `rule_names` spans; `compile.rs`: `Compiled.index` | ~30 | **done** |
| `lexer.rs`: delegate to `source::render` | −35 | **done**, pre-JSON messages unchanged in shape |
| elaboration `match` (in `generate.rs`; `prototype/src/main.rs` today) | **567** | **prototype, runs** |
| `--json-summary` decoding (`prototype/src/summary.rs`) | 328 | throwaway — exists only because the generator crate will not link offline; in-tree it is a `match` on the real types |
| pre-empt checks in `compile.rs` | ~150 | **2 of 5 done** (precedence names + levels, nullability); `SupertypeInlined`, `UnarySeq`/`UnaryChoice`, `NonTerminalWordToken` left |
| tests: nine `.tsg` fixtures + expected renders | ~120 | fixtures proven in the prototype |

So the unknown is smaller than estimated: the index and renderer are written and
the elaboration is written once, against the generator's real output. The
remaining risk is not "can this work" but "how much of `generate.rs` has to
change to hand the index over" — which is Level 1.

## Level 2 — make it feel native (small, mostly mechanical)

- `crates/cli/src/main.rs`: nothing required for the happy path; `--js-runtime`
  with a `.tsg` input should be ignored (or rejected) rather than passed along.
- `docs/src/cli/generate.md:11` — add `.tsg` to the accepted forms; document that
  `src/grammar.json` is still produced and still sufficient to regenerate.
- New docs page under `docs/src/creating-parsers/` from `SYNTAX.md`, plus a
  cross-reference from `2-the-grammar-dsl.md` ("an equivalent declarative form").
  `docs/src/assets/schemas/grammar.schema.json` does **not** change — `.tsg`
  compiles to the same schema, which is worth stating explicitly in the docs.
- `tree-sitter init`: a `--grammar-format tsg` flag emitting `grammar.tsg` (new
  `templates/grammar.tsg`) and adjusting the places that hardcode `grammar.js`:
  `init.rs:401` (which file to create), `:1038–1049` (Makefile rule),
  `:1108–1111` (CMake custom command), and the `files`/`include` lists in
  `templates/package.json:26` and `templates/_cargo.toml:15`. The `update_*`
  migration arms at `:1383`, `:1398`, `:1411`, `:1473` stay JS-only, they are
  about rewriting existing repos.
- `crates/xtask/src/generate.rs:248` (`find_grammar_files`) should also match
  `grammar.tsg` for fixture regeneration; it passes the *directory* to
  `tree-sitter generate`, so once Level 1e exists it keeps working either way.

## Level 3 — the escape route (`grammar.js` → `grammar.tsg` at scale)

This is the item with the most user value and it is already built:
`to_tsg`. New CLI command `tree-sitter migrate [PATH]` (or
`generate --emit-tsg`):

1. evaluate `grammar.js` with the existing loader (quickjs, default feature),
2. `tree_sitter_declarative::to_tsg(&json)` → `grammar.tsg`,
3. recompile `grammar.tsg` → JSON and diff against step 1, failing loudly on any
   mismatch,
4. `--allow-ignored-flags` surfaces here, because real-world `grammar.js` files
   carry `/x/g` and `/x/u` that the strict compiler rejects.

**`dsl.js` cannot be deleted yet.** A `.tsg` file is self-contained —
`grammar(baseGrammar, …)` merging and `require()` of shared rule modules are
deliberate omissions (`SYNTAX.md` §7), so grammars like `tree-sitter-typescript`
(whose `grammar.js` requires `./common/define-grammar` and composes rules
imperatively) cannot *author* in `.tsg` until `%include` exists. The *migration*
path is still fine for them, since step 1 flattens everything into the JSON. So:
keep `qjs-rt` as `default`, ship `.tsg` alongside JS, and let the JS path retire
only if `%include` lands and the node dependency is separately retired.

## Acceptance gates (what "done" means, per level)

Each is cheap because `tmp/tsg`'s harnesses already take `TSG=` to point at a
binary, and can be pointed at `cargo run -p tree-sitter-declarative --`:

- **L1**: `cargo test -p tree-sitter-declarative` (the 33 tests move with the
  code). New test in `crates/cli/src/tests/`: in a temp dir, write a `.tsg`,
  `tree-sitter generate grammar.tsg`, and byte-compare `src/parser.c` against
  the same grammar compiled from `grammar.json`. Then: `tree-sitter generate
  --json-summary` on a `.tsg` with a bad flag must emit spans in the envelope.
- **L1**: `./e2e.sh` and `./diff.sh` from `tmp/tsg` re-run with `TSG=` pointing
  at the integrated crate — 16 grammars still byte-identical.
- **L1.5**: the nine fixtures in `prototype/src/main.rs` become checked-in
  `.tsg` files under `crates/declarative/tests/fixtures/`, each with an expected
  render (the transcript in `prototype/OUTPUT.md` is the golden file to start
  from). `--json-summary` output for them is compared too — that is where the
  serialization findings above turn into a stable schema.
- **L2**: `tree-sitter init --grammar-format tsg` produces a repo where
  `make`, `tree-sitter build`, and `tree-sitter test` work with no node.
- **L3**: migration of all 17 real grammars, each verified by recompile-and-diff;
  `./roundtrip.sh` (92 grammars) is the existing proof for the reverse direction.

Re-verified after the `source.rs` / `Compiled.index` / renderer work (the reverse
path and `grammar.json` output are untouched by it): `cargo test` 33 passed,
`./selfhost.sh` 11 ok, `./diff.sh` 14 grammars byte-identical, `./roundtrip.sh`
92/92 exact, `./roundtrip.sh --parsers` completes with every compared `parser.c`
reported identical.

## Risks, ranked

1. **Ambiguity policy** when `grammar.js` and `grammar.tsg` coexist. Pick error;
   anything else is a silent-wrong-grammar bug.
2. **Clippy pedantic/nursery in the workspace** — 62 warnings **[measured]**,
   plus `#![deny(disallowed_methods)]` and `todo`. Budget a cleanup pass; the
   `unnecessary structure name repetition` and `mod X`/`use X` clusters are
   auto-fixable (`cargo clippy --fix`).
3. **Span units.** `tsg` spans are byte offsets into UTF-8; tree-sitter's
   `Point { row, column }` is bytes too, so this maps directly — but decide
   once whether `declarative::Error` exposes bytes only (recommended) and let
   `generate.rs` compute rows/columns, so the crate stays position-free.
4. **Two error vocabularies.** `LoadGrammarError`'s `#[error("Failed to load
   grammar.js -- {0}")]` prefix is wrong for `.tsg`; the new variant must not
   inherit it (hence `#[error(transparent)]`).
5. **Authorship/licensing** for a 4k-line inbound contribution — the code is
   MIT and already written to tree-sitter's own schema, but attribution and the
   CLA conversation are outside the code.

## Recommended first commit

**Level 1 + Level 1.5** as a single PR — the seam without the annotation layer
changes nothing a user sees except the missing `node` step, and the whole point
of the exercise is the diagnostics. Contents: `crates/declarative` (code moved
verbatim, clippy clean, 33 tests, **plus `src/source.rs` and `Compiled.index`,
which are already written and tested**), the `load` feature + optional dep, the
`.tsg` arm in `load_grammar_file`, the `repo_root` fix, `DeclarativeError`, the
both-files policy, and the elaboration `match` for `ConflictError` taken from
`prototype/src/main.rs` (including the `possible_resolutions` → `help:` line),
plus the golden-diagnostic tests. Two follow-ups worth separate commits inside
the same PR: the pre-empted checks in `compile.rs`, and re-exporting
`Interpretation`/`Resolution` from `tree-sitter-generate`. Everything after that
is additive.

## Open questions for the user

1. New crate `crates/declarative` (recommended) or a private submodule of
   `crates/generate/src/`?
2. File name `grammar.tsg` next to `grammar.js`, or a directory-level
   `tree-sitter.json` key pointing at the grammar source?
3. Coexistence policy: error (recommended) vs prefer `.tsg` vs prefer `.js`?
4. Keep `[[bin]] tsg` in the crate so the standalone tool survives?
5. Does the migration command ship as `tree-sitter migrate` in the same PR, or
   Level 1 first?
6. `--json-summary` as a contract: the prototype found that newtype errors
   serialize to bare scalars (`{"ValidateExtras":"ws"}`). Normalise those to
   objects now, while `.tsg` is the only consumer that cares, or leave the
   format alone and treat it as debug output?
7. Should the elaboration live behind a flag (`--no-elaborate`) for people who
   script against today's `Display` text? The prototype prints the same
   information, reordered and with positions, so a strict text-only consumer
   would notice.
