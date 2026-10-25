# g2tsg — translating a Bison/Flex grammar with tree-sitter

`g2tsg` reads `postgres16.g` (705 Bison rules, 617 Flex rules, 6741 lines) and
writes the `.tsg` grammar that `tsg` compiles to `grammar.json`. The source is
read with the tree-sitter grammar in [`../grammar.tsg`](../grammar.tsg), so this
is a grammar-to-grammar translation driven by a parse tree — not by regexes over
lines.

```sh
cargo build --release            # ~4 s clean; one dependency, tree-sitter itself
./target/release/g2tsg ../../../postgres16.g --dump | head -2   # look at the tree
../../postgres16/check.sh        # end to end: translate, generate, parse SQL
```

It is 1 500 lines of Rust and it reproduces, byte for byte, the `.tsg` that was
first written by hand with [`../../postgres16/translate.py`](../../postgres16/translate.py).
That is the point: the same translation, but the source is read once by a real
parser, so every error has a line and a column.

## Why parse the `.g` file at all

A `.g` file has a context-free half (declarations, rules) and a C half (Flex
actions). The obvious approach — split on `%%`, then tokenize the Bison section
with one regex — works on `postgres16.g` and silently mis-reads the rest of the
corpus:

| source text | regex translator | this tool |
| --- | --- | --- |
| `%token <ival> ICONST` | `<ival>` becomes a token | type tag dropped, as Bison does |
| `stmt : [a b] c ;` | `[`, `]` dropped, meaning changed | rejected: `groups are not translated yet` at `2:5` |
| `expr : expr '+' expr #add` | `#add` dropped | rejected with its position |
| a missing `;` | the next rule is swallowed | `unexpected \`c d ;\` at 3:1` |
| `/* … */` inside a rule | comments stripped by a regex that also mangles `'/*'` | extras: comments are skipped by the lexer |

The last row matters most for a *corpus*: 358 grammars use constructs
`postgres16.g` does not, and a translator that guesses is worse than one that
stops with a position.

## The integration is small

Everything tree-sitter needs is three pieces, and the same three work for any
grammar crate (`tree-sitter-json`, …) — this one just happens to be in the repo.

```rust
// build.rs: compile the parser that `tree-sitter generate` produced.
fn main() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("src");
    cc::Build::new()
        .include(&src)
        .file(src.join("parser.c"))
        .compile("tree-sitter-bison-flex");
}
```

```rust
// main.rs: the generated parser exports one function returning a TSLanguage.
unsafe extern "C" {
    fn tree_sitter_bison_flex() -> *const c_void;
}

fn language() -> Language {
    unsafe { Language::from_raw(tree_sitter_bison_flex().cast()) }
}
```

```rust
// cst.rs: parse, then look at the root.
let tree = parser.parse(source, None).expect("parsed");
let root = tree.root_node();
if root.has_error() {
    return Err(syntax_error(&root, source));
}
for child in root.named_children(&mut node.walk()) { /* … */ }
```

There is no schema, no generated binding crate, no `Node` enum. The grammar's
rule and token names are strings, and `Node::kind` returns them.

## Reading the tree

The metagrammar's shape, and what the walk does with each node:

| `.g` construct | node kind | read as |
| --- | --- | --- |
| `%token a 'b' <c>` | `directive` → `PercentToken decl_tokens` | `Directive::Token(["a", "'b'"])` |
| `%left '+' '-'` | `directive` → `PercentLeft decl_tokens` | `Directive::Assoc { Left, [...] }` |
| `%start stmt` | `directive` → `PercentStart Name` | `Directive::Start("stmt")` |
| `a : b | c ;` | `grule` (`Name`, `Colon`, `alt_rhs`, `Pipe`, `Semi`) | `Rule { lhs, alts }` |
| `%empty`, `%prec X` | `PercentEmpty` inside `alt_rhs`, `opt_prec` | `Alt { atoms: [], prec }` |
| `[a b]` / `(a|b)` / `{a}` | `rhs_atom` → `LBracket … RBracket` | rejected, position of the bracket |
| `x?` / `x*` / `x+` | `rhs_postfix` → `Question` / `Star` / `Plus` | `Atom::Opt` / rejected |
| `a : b #add` | `opt_prod_alias` → `ProdAlias` | dropped (no tree-sitter equivalent) |
| Flex lines | `LexerLine` | a line of text, split in `split_lexer_rules` |

Four things about the API are worth knowing before writing a walker like this:

1. **Anonymous tokens are children too.** `named_children` skips the ones a
   translator does not care about (`Colon`, `WS`, …) and keeps the ones it must
   act on, because `grammar.tsg` declares those as named tokens: `Pipe`, `Semi`,
   `PercentEmpty`, `PercentPrec`, `Question`. That split — named for
   "punctuation with meaning", anonymous for the rest — is most of what makes
   the walk below read like the grammar.

2. **Extras are named nodes.** `COMMENT`, `WS` and `NL` are declared as extras
   and [they still show up as named children](https://tree-sitter.github.io/tree-sitter/using-parsers#extras).
   Every walk here starts by filtering [`EXTRAS`](src/cst.rs), which is why
   comments can appear anywhere in a rule body without disturbing it.

3. **`kind` is not `text`.** `%left` is the token `PercentLeft`, so the node kind
   is `"PercentLeft"`; `Node::utf8_text` gives the spelling. The translator
   matches on kinds and keeps text only for symbol names — which is also why
   `'+'` keeps its quotes and stays distinct from `PLUS`.

4. **No fields were needed.** `grammar.tsg` could have used field labels
   (`lhs: Name`), and then `child_by_field_name("lhs")` would replace "the first
   child of a `grule` is the LHS". It was left out because the format is
   positional everywhere, and a field name that always means "child 0" is
   documentation, not a lookup. Discriminating a `directive` by its first child's
   kind is the same amount of code.

`--dump` prints `root_node().to_sexp()`. It is the first thing to run when the
translator rejects a file:

```console
$ g2tsg ../postgres16.g --dump | head -1 | cut -c1-100
(source_file (COMMENT) (NL) (NL) (NL) (directive (PercentToken) (WS) (decl_tokens (decl_token (Name)))) …
```

## Errors, with positions

`has_error()` is the only check needed to know that a file parsed. If it is
true, an `ERROR` node spans everything that did not fit, so the *outermost* one
is useless — for a missing `;` it covers half the file. Two descents make the
message actionable:

* to the **innermost** `ERROR` — the token that broke the pattern;
* then to the **last whole construct** the parser finished, because recovery
  resumes immediately after it.

```console
$ printf '%%\na : b ;\nc d ;\n%%\n' > bad.g && g2tsg bad.g
g2tsg: error: bad.g:3:1: unexpected `c d ;`
```

`MISSING` nodes are reported as what was expected instead:

```console
$ printf '%%\na : b ;\n%%\n\n%%\n' > short.g && g2tsg short.g
g2tsg: error: short.g:6:1: expected `%%`
```

Positions are 1-based (`tree_sitter::Point` is 0-based), and everything else in
this crate — `InputError` in `src/cst.rs` — exists to carry a message and a
position to that one `eprintln!`.

## Where the translation decisions live

| file | what it does | tree-sitter? |
| --- | --- | --- |
| `src/cst.rs` | `.g` → `ir::Grammar`, line splitting for Flex | yes |
| `src/ir.rs` | the shape of the source, before translation | no |
| `src/flex.rs` | Flex pattern text → tree-sitter regex (`{macro}`, `\200`, `"…"`, `/`) | no |
| `src/emit.rs` | the decisions: `%empty`, `?`, `%prec`, `_LA`, tokens | no |

Everything that is specific to one grammar is a file next to the grammar, not
code:

* `../../postgres16/header.txt` — the notes that end up in the generated file;
* `../../postgres16/conflicts.txt` — the `%conflicts` entries, with a comment
  saying why each is a translation artifact;
* `../../postgres16/bodies.txt` — the four token bodies (`Op`, `SCONST`,
  `IDENT`, `FCONST`) that Flex syntax cannot express.

The one decision that dominates the output is nullability: tree-sitter forbids a
non-start rule that matches the empty string, so each of the 196 nullable rules
loses its `%empty` alternative and every reference to it becomes `rule?`; an
all-optional body becomes its non-empty subsequences (`a? b? c?` → `a b? c? | b c? | c`).
That rewrite is most of the `.tsg`'s 8000 flattened productions, and the reason
all 12 `%conflicts` exist.

Three smaller decisions are worth naming, because each one is a place where a
literal transcription produces a `.tsg` that `tsg` rejects:

* **An empty alternative is deleted.** After the `?` distribution an alternative
  can still be empty (`a : b | %empty`), and `/a: b | ;/` is not a rule — its
  callers already use `a?`, so the alternative goes.
* **A production the lexer cannot start is dropped, with a `note:`.** tree-sitter
  needs a lexer body for every token in the grammar, so a token that no Flex rule
  returns (`%token E_CREATE` in a grammar whose lexer never produces it) takes
  every production that needs it with it. That is a fixpoint: dropping a
  production can leave another rule with none. It is the difference between "the
  grammar cannot be written" and "so the tool writes a smaller grammar".
* **A name in a rule body is a terminal, even without a `%token` line.** Bison
  declares tokens implicitly — `%nonassoc lazy` and a `lazy  lazy` lexer rule are
  enough — so `tokens` is `%token` ∪ (rule symbols − rule names) − literals.

## How it is tested

`cargo test` translates nineteen small grammars in process — one per behaviour below
— and asserts on the emitted text. `../../postgres16/check.sh` runs the whole
thing for real:

1. **regenerate**: `g2tsg` writes `.check/gen.tsg`, and the script fails if the
   committed `grammar.tsg` differs — the grammar is generated, so it cannot
   drift;
2. **agreement**: the regex translator's output must match from `%name` down, so
   the CST rewrite is pinned to the translation that was verified by hand;
3. **end to end**: `tsg` → `grammar.json` (identical to the previous one) →
   `tree-sitter generate --lalr` (1378 states) → `tree-sitter build` → parse the
   SQL samples with zero `ERROR`/`MISSING`.

## What the corpus says

`postgres16.g` is one file. The playground corpus that motivated the tool is 358
`.g` files (ANSI C, CHILL, PHP, JavaScript, Scala, Eiffel, Verilog, shell…), and
running `g2tsg` over all of them and then `tsg` over each output is the only
honest measure of how general the translation is:

```
301  translate to a `.tsg` that `tsg` accepts
 52  rejected by `g2tsg`, with a file:line:col and a reason
  5  translated, but the `.tsg` is not valid
```

`%fallback` accounts for 17 of those: `%fallback ID WITH WHEN` says that where
the parser cannot use `WITH`, the scanner gives `ID` instead. tree-sitter does
that for free — its lexer only produces tokens the state accepts — so the whole
translation is the order of the `tokens` block: the specific token has to come
before the fallback one, or it loses the lexer tie.

That 301 is 6 fewer than before the `antlr-v4.5` run, and on
purpose: 16 files used to lose their start rule to the dead-production cascade
and then have the *next* rule promoted to start symbol — a parser for the wrong
language, accepted in silence. They now fail with "every production of `X` — the
start rule — needs `CODE` the lexer never returns". See `../../DOGFOOD.md`.

The 5 are one-offs (a `%prec` for a level that no directive declares, a `_LA`
token whose base token nothing lexes, a rule left with no alternative, a `.` in a
rule name). The 52
fall into these buckets, and none of them is a parsing problem — the metagrammar
read every one of those files:

| count | reason | whose gap |
| --- | --- | --- |
| 16 | every production of the start rule needs a token the lexer never returns | Flex cannot express modes/actions |
| 13 | `A` is nullable and refers to itself in the middle of a body | tree-sitter: no empty production |
| 10 | `[...]`, `(...)`, `{...}` groups are not translated yet | the translator (needs fresh rule names) |
| 9 | `unexpected …` / `expected …` | not a Bison file (the corpus mixes in ANTLR, Lark, PEG, BNFC) |
| 3 | the source has no rules | not a Bison file |
| 4 | one-offs | the translator |

The 13 nullable ones are the interesting number: `q : q a q` with a nullable `a`
is exactly the shape tree-sitter cannot express, and it is the same gap that
makes `postgres16.g` explode into 8000 flattened productions.

`%fallback` was the last construct that looked like it had no tree-sitter
equivalent. The ones that really do not are `%union` / `%type` / `%expect` and
the `%token_*` family: the metagrammar does not model them, so they are refused
with a position (measured on hand-written probes, not assumed), never dropped —
the same rule the rest of the translator follows.

## Limits

* Flex actions are C, so the Flex half stays line-oriented: `<state> { … }`
  blocks, the keyword map and `_LA` tokens are inferred from the shape of a
  line. That is a property of Flex, not a shortcut — an action can be any C.
* `[...]`, `(...)`, `{...}` groups and `*`/`+` postfix operators are rejected,
  not translated: each one needs a fresh rule name, which is a naming decision
  the tool cannot make. `postgres16.g` uses none of them; the corpus does.
* `%fallback` is translated (see above); the `%token_*` directives are
  recognized by the metagrammar and
  rejected by the translator rather than ignored. `%prec` of a token that no
  precedence directive mentions is dropped: Bison gives that token the level
  `none`, which is what an unannotated alternative already has.
* `%{…%}`, `%union`, `%type` are not in the metagrammar yet.
* Nothing here tries to be a *good tree-sitter grammar*: `A : A A | %empty` is
  translated to what the source means, and the resulting ambiguity is the
  grammar author's problem (see `../../HANDOFF.md`).
* The separator tokens are **hardcoded** (`emit.rs` emits whitespace, `--…` and
  `/*…*/` as `%extra` for every grammar).  That is right for `postgres16.g` and
  wrong in general — it eats `y--` in `bc.g` and drops `//` comments from
  `antlr-v4.5.g`.  Deriving them from the lexer's `skip()` rules is the next
  change; `../../DOGFOOD.md` ("Dog-food #3") has the measurements.
