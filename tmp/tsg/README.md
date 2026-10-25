# tsg — a declarative grammar front-end for tree-sitter

A prototype that compiles a bison/flex-flavoured grammar syntax (`.tsg`) straight
to tree-sitter's `grammar.json`, with **zero dependencies** and **zero
JavaScript**. It also runs backwards, so existing grammars can be migrated.

```
            tsg                      tree-sitter generate
grammar.tsg ──────▶ src/grammar.json ─────────────────────▶ src/parser.c
          ◀──────
        tsg --from-json
```

The contract with tree-sitter is only the JSON document described by
`docs/src/assets/schemas/grammar.schema.json`. Everything downstream —
`parse_grammar`, `prepare_grammar`, `build_tables`, `render` — is reused
untouched. There is no second backend and no fork.

* Language reference: [SYNTAX.md](SYNTAX.md)
* ~5.8k lines of Rust, `std` only, no registry download
* Hand-written lexer, recursive-descent parsers, JSON writer **and** JSON reader
* Every `grammar.json` in the repository round-trips through `.tsg` unchanged

## Build

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # rustup is keg-only here
cargo build --release
cargo test
```

Everything — sources, build output, scratch directories, tool binaries — stays
inside this directory. `cargo` defaults to `./target` here, and `Cargo.toml`
declares its own `[workspace]` so the enclosing tree-sitter workspace is not
disturbed. No registry download is needed (zero dependencies), so no build
artefacts land outside this directory.

The one exception is `cargo` itself, which unconditionally creates a `$HOME/.cargo`
holding ~56 KB of lock files even for a dependency-free crate. To keep even that
inside the project:

```sh
CARGO_HOME="$PWD/.cargo" cargo build --release
```

## Use

```sh
tsg examples/json.tsg -o src/grammar.json
tree-sitter generate src/grammar.json
tree-sitter parse sample.json
```

`tsg` also reads stdin and writes stdout when given no argument / `-o`.

Going the other way, `--from-json` (or `-r`, also implied by a `.json` input)
prints a `.tsg` file:

```sh
tsg --from-json test/fixtures/test_grammars/c/grammar.json -o c.tsg
tsg c.tsg -o build/grammar.json
```

`--precedences` (`-p`) prints the resolved precedence table instead of JSON —
every level, the terminals it owns, and the productions that inherit each, with
line numbers. It runs the same checks as a build, so a mistake shows up here
first:

```sh
tsg -p lua53/grammar.tsg | head -12

precedence, highest first
  right call
    (none)
      89:5  stat: funccall @right(call)
     122:5  exp: var @right(call)
  right pow
    '^'
     151:5  exp: exp '^' exp
```

`--explain-modes` (alias `--explain-lexing`) prints the lexer side instead: the
token priority list, each rule's `word`/`extra`/`immediate`/`external` marker,
and the body the rule is built from. It is a view of the grammar, not of the
generated DFA: per-parse-state lexer states are derived by `build_tables` from
valid-token sets, and the report says so rather than inventing them.

`--annotate` (`-a`) is `--fmt` plus those facts, attached to the item that owns
them:

```sh
$ tsg --annotate examples/named_precedences.tsg | head -14
// test/fixtures/test_grammars/named_precedences/grammar.js
%name named_precedences;

%conflicts expression type;
%conflicts expression nested_type;

// ordering only: entries go from higher to lower precedence, and a production takes the level of its rightmost terminal
// member_expression > 'and' > 'or'
// nested_type > 'type_intersection' > 'type_union'
%precedence member_expression 'and' 'or';
%precedence nested_type 'type_intersection' 'type_union';

// can match nothing: it reduces to a zero-width node wherever it is used
program:
    repeat(choice(expression_statement, declaration_statement));
```

It is still a `.tsg` file, so it compiles to the same `grammar.json` — comments
never reach the JSON, `--diff` ignores them and `--fmt` drops them (saying how
many). What it adds is what the file only *implies*: which alternatives a
precedence level marks, tokens nothing produces, and which rules can match
nothing. `--annotate` of an annotated file is the annotated file, so it is safe
to keep as a reading copy.

## Files

| Path | What |
| --- | --- |
| `src/` | the compiler (~5.8k lines, `std` only) |
| `examples/*.tsg` | one grammar per test fixture, a JSON showcase, and `tsg.tsg` — the language itself |
| `HANDOFF.md` | state of play, the decisions behind it, and what to do next |
| `selfhost.sh` | compiles `examples/tsg.tsg`, parses every `.tsg` it can find with the result, runs the shipped queries |
| `surface/*.tsg` | every directive form and expression shape, once — the fixture set the description is checked against |
| `coverage.py` | asserts every named node in `node-types.json` is spelled in `surface/` |
| `queries/highlights.scm` | queries for `tsg.tsg`, kept compiling against it by `selfhost.sh` |
| `diff.sh` | JSON-level differential test against the real `dsl.js` under node |
| `e2e.sh` | `parser.c`-level differential test against the real CLI |
| `roundtrip.sh` | `grammar.json → .tsg → grammar.json` over the whole repo |
| `compare.py` | structural JSON comparison used by all four harnesses |
| `*.json` | 17 real-world grammars for the round trip (see below) |
| `bin/tree-sitter` | prebuilt upstream CLI, used only by `e2e.sh` (35 MB, safe to delete) |
| `target/`, `.diff/`, `.e2e/`, `.roundtrip/`, `.self/` | build output and scratch, all gitignored |

## Proof that it works

Four harnesses in this directory compare the prototype against the real thing.

### 1. `./diff.sh` — the same parser as the JavaScript DSL

For each `examples/<name>.tsg` with a matching
`test/fixtures/test_grammars/<name>/grammar.js`, it runs the **actual
`crates/generate/src/dsl.js` under node** on the original `grammar.js`, then
asks the generator about both `grammar.json`s:

```sh
tree-sitter generate <grammar.json> --stamp
```

The stamp is a digest of the parser the generator builds (states, actions,
token metadata, `node-types.json`) *after* unused rules are dropped, tokens are
extracted, inlines are expanded, conflicts are resolved and the lexer tables
are built. Two documents with the same stamp build the same parser, so this
compares meaning instead of text: rule order, whitespace, and parentheses that
do not change the rule tree pass; a dropped alternative, a moved precedence or a
changed alias fails. `compare.py` still runs, but only to *explain* a failure.

```
ok   aliased_rules          (7 rules)
ok   aliased_token_rules    (1 rules)
ok   associativity_right    (3 rules)
ok   basic_cst              (4 rules)
ok   dynamic_precedence     (6 rules)
ok   eof_basic              (2 rules)
ok   external_word_token    (5 rules)
ok   field_lookup_through_hidden_nodes (31 rules)
ok   inline_rules           (7 rules)
ok   named_precedences      (9 rules)
ok   precedence_on_single_child_negative (4 rules)
ok   readme_grammar         (6 rules)
ok   reserved_words         (17 rules)
ok   start_rule_is_blank    (1 rules)

all 14 grammars match the JavaScript DSL structurally
```

### 2. `./e2e.sh` — `parser.c` equivalence

The stronger test: generate the parser twice with the unmodified tree-sitter
CLI, once from `grammar.js` and once from `tsg`'s `grammar.json`, and
byte-compare the artefacts.

```
ok   aliased_rules          (parser.c 9006 bytes, node-types.json 1712 bytes)
ok   aliased_token_rules    (parser.c 5995 bytes, node-types.json 457 bytes)
ok   associativity_right    (parser.c 5677 bytes, node-types.json 694 bytes)
ok   basic_cst              (parser.c 6502 bytes, node-types.json 865 bytes)
ok   dynamic_precedence     (parser.c 8191 bytes, node-types.json 1564 bytes)
ok   eof_basic              (parser.c 6756 bytes, node-types.json 420 bytes)
ok   external_word_token    (parser.c 11745 bytes, node-types.json 1115 bytes)
ok   field_lookup_through_hidden_nodes (parser.c 62308 bytes, node-types.json 9128 bytes)
ok   inline_rules           (parser.c 10722 bytes, node-types.json 2444 bytes)
ok   named_precedences      (parser.c 17817 bytes, node-types.json 2096 bytes)
ok   precedence_on_single_child_negative (parser.c 7834 bytes, node-types.json 1127 bytes)
ok   readme_grammar         (parser.c 9655 bytes, node-types.json 1387 bytes)
ok   reserved_words         (parser.c 34928 bytes, node-types.json 7212 bytes)
ok   start_rule_is_blank    (parser.c 4017 bytes, node-types.json 91 bytes)
ok   json                   (no reference; generated 18094 bytes of parser.c)

all 16 grammars build; every one with a reference is byte-identical
```

### 3. `./roundtrip.sh` — the reverse direction, over the whole repo

Every `grammar.js` in the repository is translated to `grammar.json` by the real
`dsl.js`, and every `*.json` in this directory is used as-is. Each document is
pushed through `tsg --from-json` to `.tsg`, compiled back with `tsg`, and both
documents are stamped by the generator. Same stamp means the round trip was
lossless. Add `--parsers` to byte-compare the `parser.c` that each side produces
instead.

```
building the corpus with the JavaScript DSL ...
  75 grammars, 0 not compilable by the DSL (expected for error fixtures)
ok   _root_bash  (101 rules)
ok   _root_cpp  (326 rules)
ok   _root_kotlin  (203 rules)
...
all 92 corpus grammars round-trip exactly
  (10 of them are refused by the parser generator, so the
   JSON was compared instead of the built parser)
```

That is 17 real-world grammars — bash, c, cpp, dart, go, html, java, javascript,
kotlin, mssql, php, plsql, python, ruby, rust, swift, typescript, from 19 to 756
rules — plus the 75 test fixtures. Of the 92, 82 are compared by the parser the
generator builds from each side (26.6 MB of `parser.c` for mssql, 33.8 MB for
kotlin, 25.9 MB for cpp); the other 10 are fixtures that `tree-sitter generate`
refuses by design, so their `grammar.json` is compared instead, and the round
trip must leave them refused. No grammar diverges.

The stamp decides, not this repository: `roundtrip.sh` no longer carries an
opinion about what counts as a change.

These are not written to please this tool, and they exercise things the fixtures
do not:

* `%reserved` sets with 35 keywords, and one **empty** set (`js`)
* a **one-member `CHOICE`** (`swift`'s `inheritance_modifier`) and one-member
  `SEQ`s, which no surface operator can express — hence the `choice(x)`/`seq(x)`
  call forms
* a postfix operator applied to a postfix operator (`python`'s `float`:
  `(/[0-9]+_?/+)?`)
* `inherits` (`ts`, `cpp`) — preserved as `%inherits` even though tree-sitter's
  Rust pipeline ignores the key
* grammars with no whitespace extra at all (`%extras none;`), contextual
  keywords, supertypes, `%inline`, and external scanners

Between them these grammars exercise every `RuleJSON` variant, every
directive, hidden rules, supertypes, external tokens, contextual reserved
word sets, named precedences, negative and named precedence values,
`token`/`token.immediate` and `tokens { … }` declarations, aliases (named and
anonymous), fields, and the `eof()`/`blank()` corner cases.

`examples/json.tsg` is a from-scratch showcase with no JS counterpart; the
generated parser parses real JSON with correct fields:

```
$ tree-sitter parse sample.json
(document [0, 0] - [1, 0]
  (object [0, 0] - [0, 47]
    key: (string [0, 1] - [0, 4])
    value: (array [0, 6] - [0, 37]
      (number [0, 7] - [0, 8])
      ...
```

### 4. `./selfhost.sh` — tsg parses tsg

`examples/tsg.tsg` is a tree-sitter grammar for the tsg language, written in
tsg. The script compiles it with `tsg`, builds a parser from the result, and
then parses every `.tsg` file it can find with that parser:

```
$ ./selfhost.sh
ok   examples/tsg.tsg -> grammar.json (41 rules, 19799 bytes)
ok   parser.c 135041 bytes, node-types.json 19672 bytes
ok   selfhost  (41 rules)
ok   lexical coverage (8 escape forms, 2 flag strings kept)
ok   rejects ignored flags (4 errors: g, u, s, and a repeated i)
ok   a rule named like a flag is reachable (`/x/ g`, `seq(/x/, g)`)
ok   /x/g points at the space (`/x/ g` is the sequence)
ok   --allow-ignored-flags keeps them verbatim
ok   parsed 41 files, 9702 lines of tsg, no ERROR or MISSING node
ok   node coverage (every 32 node type in node-types.json occurs in surface/)
ok   token boundary (/x/i is one token; a space ends the flags)
ok   queries compile against the grammar (147 captures, incl @operator/@keyword)

tsg parses tsg
```

The parsed set is not just the fixtures: it includes `lua53/grammar.tsg` (a real
grammar using levels, `@` and `()`), `sqlite3/grammar.tsg` (a 132-rule Lemon
port) and `surface/*.tsg`, which spell every
directive form and every expression shape once. Those two directions together are
what keeps the description honest: **each surface file must compile and parse**
(so a construct the description cannot read is an `ERROR` node and fails), and
**every named node type the description declares must be spelled somewhere in
`surface/`** (`coverage.py`; so a construct added to the grammar with no fixture
fails too). Add a construct to `examples/tsg.tsg` and the second check tells you
to add a line to `surface/`. The query check runs over `surface/directives.tsg`
as well, so a `@operator` or `@keyword` capture that stops matching fails the
script instead of silently doing nothing in an editor.

The lexical coverage step is a drift test: a small file of regexes, flags
(`/abc/i`, `/x/gimsuy`, `/x/G`), escaped slashes, slashes inside character
classes and every string escape is compiled *and* parsed, and the recorded
flags are checked in the emitted `grammar.json`.

The 9 702 lines include the 1 048-line `mssql` and 979-line `plsql` files that
`tsg --from-json` emitted from their `grammar.json`, so this is not a toy: the
parser the tool produces accepts the tool's own input, at the size real grammars
reach.

It also means the language now has a tree-sitter grammar of its own, so
`tree-sitter parse` gives a syntax tree for free:

```
$ tree-sitter parse examples/json.tsg
(source_file
  (directive
    (name_directive name: (identifier)))
  (rule_definition
    name: (identifier)
    body: (choice_expression
      (symbol (identifier))
      (call_expression
        function: (function_name (identifier))
        (string)
        (symbol (identifier)))
      ...)))
```

Two details that fell out of writing it:

* **Calls are syntactic, not lexical.** A call is just *a name followed by
  `(`, exactly as in `src/parser.rs`, so `field(alias, identifier)` works even
  though `alias` is also a function name. Spelling the function names as
  keyword tokens instead fails on the real corpus: the mssql `.tsg` has
  `from: ... alias(_single_quote_string, literal)`, and `alias` would lex as a
  keyword where a rule reference is expected.
* **`symbol: prec(-1, identifier)`** resolves the only genuine shift/reduce
  conflict in the language (`alias` vs `alias(`) without a GLR split: the shift
  wins, so a bare name reduces to a symbol and a name followed by `(` becomes a
  call.
* **`//` cannot be a regex.** The regex token requires a non-empty body, so `//`
  is always the line comment — which is exactly what `lexer.rs` does, since
  trivia is skipped before a token is scanned.
* **The flags are part of the literal, and the class is `i`.** `/abc/i` is one
  `regex` token whose pattern ends `\/(i)?` — a token never matches across
  whitespace, so `/x/i` is one token and `/x/ i` is a regex followed by a symbol
  `i`, exactly how `lexer.rs` reads it. `(i)?` rather than `i?` only because
  `i?` after the closing `/` would read as a flag on this literal itself.
  Splitting the flags into `regex_flags: token.immediate(/[a-zA-Z]+/)` looks
  tidier and makes the space significant for free, but the moment the class is
  narrowed to `i` tree-sitter's lexer reads `/x/ i` as a flag — which the
  compiler does not — so the literal keeps its flags.
* **A closed flag class cannot reject; it only changes what the `g` is.** Only
  `i` changes matching; `u`/`v` are what tree-sitter calls "unicode flags" and
  drops like the rest (`g`, `m`, `s`, `x`, `y`, `U`…). So `/x/g` cannot be a
  parse error: juxtaposition allows a symbol after a regex, whitespace is trivia
  and a token pattern has no lookaround, which is why `/x/g` parses as `/x/ g`.
  That is the same layering as `foo(1)` — syntax accepts a superset, the
  semantic layer narrows with a span — and it is enforced in `src/compile.rs`:
  `error: regex flag \`g\` has no effect …` with the caret on the flag, or, when
  the flags spell a rule name, the space hint. The flags still reach
  `grammar.json` verbatim, so the output stays byte-identical to the JS DSL.

### Silent no-ops are errors

tree-sitter *accepts* `u` and `v` as "unicode flags" and then throws them away,
and drops every other flag after a `Warning:` line. That is how a grammar ends up
looking like it means something it does not: `/x/s` does not make `.` match a
newline, and `/x/g` is not global — both are simply gone. `tsg` refuses them:

```
$ tsg flags.tsg
error: regex flag `g` has no effect in tree-sitter: it is JavaScript's `global` flag, which means nothing here
  --> flags.tsg:4:7
  |
4 | b: /x/g;
  |       ^
  = help: drop the flag; a grammar is not a scanning loop
error: regex flag `u` has no effect in tree-sitter: it is JavaScript's `unicode` mode, which is always on
  --> flags.tsg:5:7
  |
5 | c: /y/u;
  |       ^
  = help: drop the flag; there is no non-Unicode mode to switch to
error: regex flag `s` has no effect in tree-sitter: `.` never matches a newline
  --> flags.tsg:6:7
  |
6 | d: /z/s;
  |       ^
  = help: write `[\s\S]` where you meant a dot that matches newlines

error: aborting due to 3 previous errors
```

`i` is the only flag with an effect, so the accepted set is *no flags, or one
`i`* — `[i]*` would be wrong for the same reason `/x/g` is: `/x/ii` says nothing
extra, and it is rejected too, with the caret on the second `i`. That set is
checked here and nowhere else, because it is a *semantic* rule: the grammar owns
the token (`/x/i` is one `regex`, `/x/ i` is a regex and a symbol) but cannot own
the decision, since no rule makes `/x/g` an error. An `@error` query rule would
have to match a flags node that `[i]` never produces, so there is none — a query
that can never match is the same silent no-op in another costume.

A flags run that is also a rule name gets a different hint, because there is a
second thing it could be — the sequence, written without the space that
separates it from the literal:

```
$ tsg g.tsg
error: regex flag `g` has no effect in tree-sitter: …
  --> g.tsg:3:7
  |
3 | a: /x/g;
  |       ^
  = help: if you meant the rule `g`, put a space after the literal: `/…/ g`; otherwise drop the flag
```

`/x/ g` and `seq(/x/, g)` both compile, so a rule named `g` — or `s`, or `i` —
stays fully reachable; only the character that touches the closing slash is a
flag.

The other hints are per flag (`g`/`y` "means nothing in a grammar", `m` "`^` and `$` are already
per-token", `x`, `U`, …). `--allow-ignored-flags` keeps such flags so that
`grammar.js` output still round-trips byte for byte; the compiler warns instead
of failing, which is exactly the behaviour being replaced.

## What the errors look like

This is the reason the front-end exists at all. `grammar.json` has no source
positions, so a mistake in a 756-rule grammar yields this:

```
$ tree-sitter generate src/grammar.json
Error: Error when generating parser

Caused by:
    Undefined symbol `keyword_begn`
```

No file, no line, no column, no suggestion. `tsg` keeps a `Span` on every token
and every AST node, so the same mistake — and four others in the same file —
come out like this, all in one run, top to bottom:

```
$ tsg .errors/many.tsg -o /dev/null
error: undefined rule `_qualified_filed` (did you mean `_qualified_field`?)
  --> .errors/many.tsg:9:12
  |
9 | %conflicts object_reference _qualified_filed;
  |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
error: rule `keyword_begin` cannot appear inside `token(...)`; tokens must be built only from strings and regexes
  --> .errors/many.tsg:560:25
  |
560 | _sqlcmd_variable: token(keyword_begin 'x');
  |                         ^^^^^^^^^^^^^^^^^
error: undefined rule `keyword_begn` (did you mean `keyword_begin`?)
  --> .errors/many.tsg:562:44
  |
562 | begin_transaction_statement: prec.right(0, keyword_begn keyword_distributed? _transaction_keyword field(name, identifier)? (keyword_with keyword_mark literal?)?);
  |                                            ^^^^^^^^^^^^
error: `prec.right` expects 1 or 2 arguments (an optional precedence value and a rule), but got 3
  --> .errors/many.tsg:564:21
  |
564 | rollback_statement: prec.right(0, 1, keyword_rollback (_transaction_keyword field(name, identifier)? | keyword_work)?);
  |                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
error: the first argument of `field` must be a field name
  --> .errors/many.tsg:565:69
  |
565 | save_transaction_statement: keyword_save _transaction_keyword field(1name, identifier);
  |                                                                     ^^^^^

error: aborting due to 5 previous errors
```

Notes on the design:

* **Parse errors are fatal, everything else accumulates.** A syntax error leaves
  nothing to recover to, but each rule is lowered independently, so every
  undefined symbol, bad arity, bad field name and dangling directive argument is
  reported in one pass — capped at 10, with a count of the rest.
* **"did you mean"** comes from a Levenshtein distance ≤ 2 over the defined rule
  names, which is what makes `keyword_begn` / `_qualified_filed` obvious.
* **Spans cover what you meant, not just what broke.** `field(1name, …)`
  underlines `1name`; a missing `;` underlines the token that was found instead;
  a missing `)` underlines the `;` that closed the group.
* **`--from-json` errors are positioned too**, since the JSON reader is
  hand-written and carries spans: `expected a JSON value, but found `}` at line
  1, column 58` with a caret.
* **No cascade.** A broken rule is dropped, not poisoned, so one typo does not
  produce twelve errors about its consumers.

## Layout

```
tmp/tsg/
├── src/
│   ├── main.rs      CLI
│   ├── lexer.rs     spans, diagnostics, tokens, hand-written lexer
│   ├── parser.rs    AST + recursive-descent parser
│   ├── rules.rs     Rule IR (mirror of RuleJSON) + lowering
│   ├── json.rs      ordered JSON value + pretty printer
│   ├── compile.rs   driver: AST → grammar.json, plus unit tests
│   ├── json_read.rs hand-written JSON reader (for the reverse direction)
│   └── to_tsg.rs    GrammarJSON → .tsg printer
├── examples/        one .tsg per test fixture, plus a JSON showcase
├── c.json           the C grammar, round-trip centrepiece
├── bin/tree-sitter  prebuilt upstream CLI for e2e.sh (delete freely)
├── diff.sh          JSON-level differential test vs. dsl.js under node
├── e2e.sh           parser.c-level differential test vs. tree-sitter CLI
├── roundtrip.sh     json → tsg → json over every grammar in the repo
└── compare.py       structural JSON comparison used by all three
```

## Why this shape

* **Target `GrammarJSON`, not a new IR.** The value is a better front-end, not a
  different compiler. Emitting the existing contract means zero risk to ~11k
  lines of table construction, and `parser.c` output is provably identical.
* **Zero dependencies.** A hand-written lexer/parser/JSON writer is a few hundred
  lines; `serde` + `serde_json` would have been more code in dependency
  plumbing than the whole prototype, and would drag in a registry download.
* **Spans from day one.** Retrofitting source locations into a JSON front-end is
  impossible: the JSON has no positions. Keeping them in the declarative
  front-end is nearly free, and it is the whole point.
* **A preprocessor, not a fork.** `grammar.tsg → src/grammar.json` slots in front
  of the existing pipeline; absorbing it later as
  `crates/generate/src/declarative/` is a mechanical move.
* **The reverse direction is the migration story.** ~140 grammars already exist
  in JS. A converter that is *provably* lossless on all of them is what makes a
  new syntax adoptable at all — otherwise every grammar would be an unverifiable
  hand port. That is why `roundtrip.sh` exists, and why two small bits of syntax
  (`%extras none;`, `seq(x)`) exist solely to keep the round trip exact.

## Not done (deliberately)

* `%include` / modularity — deferred; a `.tsg` file is self-contained.
* Wiring into the `tree-sitter` CLI (`tree-sitter generate grammar.tsg`) and
  into `crates/generate/src/declarative/`.
* Pretty-printing the reverse output for humans — it is written for the round
  trip, not for editing.
