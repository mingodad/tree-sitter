# Handoff — `tsg`, a declarative front end for tree-sitter

Written at a stopping point. Everything below was verified with the commands it
names; nothing here is aspirational.

## What this is

`tsg` compiles a bison/flex-flavoured declarative grammar (`.tsg`) to the
`grammar.json` that tree-sitter's compiler already understands, replacing the
JavaScript DSL (`crates/generate/src/dsl.js`) with a zero-dependency Rust binary.
It also runs backwards (`tsg --from-json` / `-r`): `grammar.json` → `.tsg`, which
is how the ~140 existing JS grammars get migrated mechanically.

Location: `/Volumes/RamDisk/tree-sitter/tmp/tsg` (also reachable as
`/Users/mingo/tmp/tree-sitter/tmp/tsg`). Nothing may be written outside this
directory — see *Constraints*.

Pipeline: `.tsg → src/grammar.json → tree-sitter generate → parser.c`.

## State at this stop: green

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # rustup is keg-only
cd /Volumes/RamDisk/tree-sitter/tmp/tsg
cargo build && cargo test        # 88 passed
./selfhost.sh                    # all steps ok, "tsg parses tsg"
./sqlite3/check.sh               # SQLite Lemon port: 5 samples, no ERROR/MISSING
./bison_flex/check.sh            # Bison/Flex `.g` front end: postgres16.g + bison-flex.g
./diff.sh                        # all 14 grammars match the JS DSL byte-for-byte
./roundtrip.sh --parsers         # all 92 corpus grammars round-trip exactly
./e2e.sh                         # all 16 grammars build; references byte-identical
./postgres16/check.sh            # postgres16.g → .tsg → parser.c, then parse SQL
./antlr/check.sh                 # antlr-v4.5.g → .tsg → parser.c, then parse 3 samples
./bison/check.sh                 # bison.g → .tsg + src/scanner.c → carbon-lang.y, actions.y
```

**Stopped mid dog-food #3 (`bc.g`)** — read `DOGFOOD.md` → "Dog-food #3" first.
Three translator bugs were found and fixed **uncommitted** in
`bison_flex/translate/` (a Flex start-condition strip that truncated a regex, a
token that kept only one of its lexer patterns, an empty precedence level emitted
as a bare ordering list).  They are measured over the whole corpus: 203 of 287
outputs change, 0 regressions, `postgres16/grammar.tsg` byte-identical.  `bc` is
**not** a gate yet: its sample still has 3 errors, one from a hardcoded `%extra`
in the translator (overfitting) and two from a tree-sitter lexer bug found by the
run (`1 /* … */` lexes the `/` as division; reproduced on upstream `0fff6ab`).

`postgres16/check.sh` and `antlr/check.sh` both need `TSG` (this tree's `tsg`),
`TS` (the *patched* CLI — `--lalr` does not exist upstream) and `G2TSG` (the
translator, below) in the environment; see `DOGFOOD.md` for the exact command.

The generator itself is patched (see the next section); build it off the ramdisk:

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH" CARGO_TARGET_DIR=$HOME/.ts-build
cd /Volumes/RamDisk/tree-sitter && cargo build --release -p tree-sitter-cli   # ~40 s warm, 3 min cold
# binary: $HOME/.ts-build/release/tree-sitter
```

`ts-build/` and `ts-exp/clean-target/` are **deleted** (2.7 GB): nothing of this
project is allowed to sit outside `tmp/tsg`, so a build target dir is created when
needed and removed when the session stops.  The gates below therefore need `TS`
and `G2TSG` built first; `target/debug/tsg` (137 MB) is the one build that lives
on the ramdisk, because `tsg` has zero dependencies and fits.

`cargo test -p tree-sitter-generate` — 104 passed.

Any `.tsg` now rejected *before* JSON is checked against the generator itself,
so a refusal is only accepted when `tree-sitter generate` refuses the same
grammar.

Sizes, current: `examples/tsg.tsg` → 41 rules, 19799 B `grammar.json`,
135041 B `parser.c`, 19672 B `node-types.json`. `src/` is ~6100 lines, 80 tests,
`std` only.

## Making `tree-sitter generate` usable (this session)

The dog-food port of `postgres16.g` was blocked not by `tsg` but by the
generator: `tree-sitter generate` on the 1195-rule grammar ran for 90 s, reached
942 MB, and never finished. Bison does the same grammar in under a second, so
the feedback loop was the thing to fix — in the generator, not by asking the
grammar author to contort the grammar.

Measured end state, `cd tmp/tsg/postgres16`, 1195 rules / 1378 parse states:

| | before | after |
|---|---|---|
| wall clock | > 90 s (killed) | **3.8 s** |
| max RSS | 942 MB | **87 MB** |
| `build_lex_table` | > 25 s, 14133 lex states | **0.47 s**, 3400 lex states |
| `token_conflicts` | — (never reached) | 3.3 s |
| `parse_table` | — | 0.09 s |

SQLite3, same binary: 2199 parse states, 1.2 s, and the five sample parse trees
are **byte-identical** to the ones produced by canonical LR(1).

### Cause 1: every token carried its own copy of the separators

`expand_tokens` builds the lexer NFA by expanding, per token, `sep* token`, so
a grammar with 510 non-immediate tokens contained 510 copies of the extras
regex (`[ \t\n\r\f]+`, `--…`, `/*…*/`). The subset construction then kept ~30
separator NFA states live *per token* — live sets near 2000 states, one per
character class, and `_platform_memmove` in the profile. Two tokens that share
their entire separator prefix can never be told apart by the subset
construction.

`LexicalGrammar` now carries a second NFA (`shared_nfa`, plus `shared_owner` and
`shared_start`): each token body is copied once and stays private, the separator
rules are expanded **once**, and that copy's exits go to a hub — a binary split
tree over the non-immediate bodies. The lex table is built from that NFA, with
every state's live set filtered to the tokens valid in the current parse state
(`shared_owner == u32::MAX` for the shared block, which is always allowed).

Because `sep* a | sep* b` is the same language as `sep* (a | b)`, this is not a
heuristic: filtered sets are exactly the reachable states of the per-token
copies. `nfa` (the per-token copies) is left untouched for `TokenConflictMap`,
which needs two independent token automata to walk in lockstep.

### Cause 2: quadratic state insertion and a serial conflict matrix

- `NfaCursor::add_states` re-sorted and de-duplicated on every character class
  (89 % of the lex phase). It now stamps states with a generation counter into
a `seen` vec and sorts once at the end (`crates/generate/src/nfa.rs`).
- `TokenConflictMap::new` walks 128271 token pairs; it is now split across
  `std::thread::scope` threads (one cursor per thread, rows merged by index).
  12.3 s → 3.2 s.

### Cause 3: canonical LR(1) is not an option for large grammars

Even with the lexer fixed, canonical LR(1) on the flattened postgres16 grammar
needs **354466 states** (tree-sitter forbids empty productions, so every
optional clause becomes a distribution of productions: 8000 productions, 1024 of
them for `CreateStmt` alone). Bison is fast because it is LALR.

`OptLevel::Lalr` (`--lalr`) builds the table by *kernel core*: state identity is
`ParseItemSetCore` (items with lookaheads stripped), and when a state's
lookaheads grow it is re-queued with its action tables reset — monotone growth,
so it terminates, and it is bison's algorithm. Trade-off, measured with a
11-state probe grammar: `--lalr` accepts a few grammars that canonical LR(1)
rejects, so it stays opt-in.

`--max-states N` and a 2 s progress line to stderr turn the remaining failure
mode (an explosion) into a fast, actionable error instead of a killed run.

### Cause 4: `%prec` against an undeclared token (a semantic gap)

With the grammar generating again, `UPDATE foo SET name = 'c'` parsed `SET` as
an *alias column* of `foo` and then died. `postgres16.g` says
`relation_expr_opt_alias : relation_expr %prec UMINUS | …`; `UMINUS` is a
pseudo-token, so its level contains no terminals, and `SET` is declared in no
precedence level at all. Bison therefore compares **22 vs 0** and reduces.

`compare_precedence` compared `Precedence::None` with a named level as
**Equal** (it only knew how to compare `None` against an *integer*), so the
conflict fell through to associativity, and a `%right` level prefers the shift —
the opposite of Bison. Fixed in `build_parse_table.rs`: `None` now ranks below
any named level, mirroring the rule already there for integers. With that,
`SELECT`/`INSERT`/`UPDATE`/`DELETE`/`CREATE TABLE`/`CREATE INDEX`/transaction
control all parse with zero `ERROR` nodes (`postgres16/sql_basic.sql`).

Regression evidence for that change, all of it byte-for-byte:
`roundtrip.sh --parsers` 92/92 `parser.c` identical, `e2e.sh` 16/16 identical,
`diff.sh` 14/14, `cargo test -p tree-sitter-generate` 104/104.

### The generator changes are committed, on a branch

The checkout is on `master`, which was fast-forwarded to the branch: the eight
commits sit on top of `0fff6ab` (the upstream commit `master` used to point at),
and `perf/large-grammars` is an alias for the same tip. One concern each:

| commit | what |
|---|---|
| `b829c6b` perf | `NfaCursor::add_states`: generation stamps, sort/dedup once |
| `a3e4498` perf | extras expanded once (`shared_nfa` / `shared_owner` / `shared_start`) |
| `22a5636` perf | `TokenConflictMap` rows across `std::thread::scope` |
| `69142a0` fix | `Precedence::None` ranks below a named level |
| `b363428` feat | `--lalr`, `--max-states`, 2 s progress line, docs (incl. when to use `--lalr`) |
| `7e75cdb` chore | timings and progress via `--log` / TTY instead of `TS_DEBUG_*` env vars |
| `01b1463` feat | all unresolved conflicts reported in one run, like bison |
| `49aa685` feat | unreachable and unproductive rules are reported instead of silently dropped |

Each commit compiles standalone and is nightly-`rustfmt` clean (the
`rustfmt.toml` unstable options require nightly). `git am` of the series onto
`master` reproduces the tree exactly. Nine new tests (104 total): the shared NFA
lexes exactly like the per-token NFA for every case in the existing suite, plus
dedicated separator cases, plus the precedence ordering, plus the three
conflict-reporting tests, plus the three for the useless-rule diagnostics.

The series is exported for the upstream author, with a written-up PR
description, in `tree-sitter-patches/` (`git am tree-sitter-patches/000*.patch`;
`PR.md` has the problem statement, the numbers, the API changes and the test
evidence).

### Gap list: what this port could not express (tsg/tree-sitter)

1. **No empty productions.** Every `%empty` alternative is inlined at its use
   sites as `x?`, so `simple_select` becomes 328 alternatives and the grammar
   flattens to 8000 productions; canonical LR(1) then explodes (354k states) and the `?`
   distribution produces *derived* shift/reduce conflicts that Bison never sees.
   This is the largest structural cost of the port. A tree-sitter production
   that may match nothing (an explicit nullable node) would remove all of it.
2. **`%prec` on a level with no terminals** could not beat an undeclared token
   (fixed above in the generator, but it was invisible to the grammar author).
3. **`%nonassoc` has no equivalent**; levels keep their ordering but lose the
   “error on a chained operator” property.
4. **No lexer lookahead.** `WITH_LA`/`NULLS_LA`/`NOT_LA` collapse onto their
   base token, which is what creates the one genuine ambiguity,
   `utility_option_name` vs `unreserved_keyword` on `FORMAT`.
5. **`%fallback` is not implemented** (1970 occurrences in the 358-file corpus),
   and the 12 `%conflicts` this grammar needs are all translation artifacts.
6. The translator had to write three token bodies by hand (`Op`, `SCONST`,
   `IDENT`) because the Flex rules were approximated in `postgres16.g` itself.

### Diagnostics gaps (not memory or speed, still worth reporting)

7. ~~**Conflict diagnostics are a cascade, one pair per run.**~~ Fixed in the
   series (`01b1463`): all unresolved conflicts are collected and reported in
   one run, deduplicated by symbol set, each with the `conflicts` entry it
   needs; a lone conflict keeps the detailed item-set report. Porting postgres16
   went from 12 sequential runs to one 0.4 s run listing all 12. Still missing:
   the **state id and kernel items** in the human-readable message (the summary
   names the symbol sequence instead; `--json-summary` has the item sets).
8. ~~**No supported way to see where `generate` spends its time.**~~ Addressed in
   the series (`7e75cdb`): stage/lexer timings and the progress line go through
   `--log` (debug logging) and a TTY check, not `TS_DEBUG_*` env vars. Still
   missing: an API home — a library caller of `generate_parser_for_grammar` has
   to install a `log` logger, and the per-stage numbers are not in
   `--json-summary` either. A `GenerateOptions`/summary field would be better.
9. **No bound on runaway generation**, so the only exit from a state explosion
   was killing the process (`--max-states` now exists in the series, but the
   author should decide whether that is the right shape — a `--timeout` or a
   state-count *estimate* before building might be better UX).
10. Minor: a bare `src/grammar.json` argument is resolved against the CWD, not
    against the grammar directory, which makes scripting the pipeline fiddly.
11. **`tsg` does not validate a `/…/` body**, so a regex the runtime rejects is
    only caught two steps later by `tree-sitter generate`. Found by the corpus:
    `[\x0000-\x001F]` is valid Flex (Flex reads any number of hex digits) and
    stays `[\x0000-\x001F]` in the `.tsg`, which `regex-syntax` refuses ("the
    start must be <= the end") because it wants `\x00`. A `tsg` that compiled the
    body would have pointed at the grammar line instead of at `grammar.json`.
12. ~~**No diagnostic for an unreachable or non-productive rule.**~~ Fixed in
    the series (`49aa685`): a dropped rule reports
    "rule `x` is not reachable from the start rule. it was ignored.", and a rule
    that derives nothing reports "rule `x` cannot match anything: …". Both are
    warnings, so they do not change `generate`'s exit status; `--json-summary`
    carries them. 10 of the 92-grammar corpus gain the first (all genuinely dead
    rules, e.g. the JSX subtree of the TypeScript grammar), and only
    `conflicting_precedence` gains the second, which really has no base case.
    Found by the `antlr-v4.5.g` port (`DOGFOOD.md`), where it let a translator
    bug through:

        s: a;  a: a B;  unreachable: B;      # `a` derives nothing, `unreachable` is unreachable

    `tsg` and `tree-sitter generate` both exit 0 and build a 5-state parser in
    which `s` can never match; that is what hid the translator bug. Still open:
    `tsg` itself says nothing, so the porting loop (`g2tsg` → `tsg`) only sees
    the warning once `tree-sitter generate` runs — computing the same two
    properties in `tsg`'s front end would put it at the line that lost the rule.

The port itself lives in `postgres16/`: `translate.py` (`.g` → `grammar.tsg`,
705 rules, 489 tokens, 27 levels, keywords before `IDENT`), `tree-sitter.json`,
`sql_basic.sql`, and `check.sh`, which runs the whole pipeline in ~9 s and fails
on any `ERROR`/`MISSING`. `%precedence` levels are emitted as a descending chain
with `@right(pN)`-style annotations, and the 12 `%conflicts` are listed at the
top of `translate.py` with the reason each one exists.

Remaining known cost: `token_conflicts` is ~80 % of the 4.1 s (128k pairs walked
over the per-token-copy NFAs). A sound prefilter would skip pairs whose bodies
share no prefix, and a body-only NFA would make most pairs terminate after one
step — but that changes conflict bits, so it needs another byte-identical
round of `roundtrip.sh`/`e2e.sh` before it can land.

## The regex-flag question (the regex-flag session)

Final design, in `examples/tsg.tsg`:

```tsg
regex: token(/\/([^/\\\n]|\\.|\[([^\]\\]|\\.)*\])+\/(i)?/);
```

Facts that were established by experiment, and that should not be re-litigated
without new evidence — each was measured with `tree-sitter parse` or `tsg`:

1. **No flag class can reject `/x/g`.** Juxtaposition permits a symbol after a
   regex, whitespace is trivia, and a tree-sitter token pattern has no
   lookaround. So `/x/g` parses as `(regex /x/) (symbol g)` under any class.
2. **Therefore the class only decides what the leftover letters *are*.** With
   `[i]` (or any closed class) `/x/g` and `/x/ g` produce byte-identical trees.
   With the wide class they differ (`(regex_flags "g")` vs `(symbol g)`).
3. **A flags-as-separate-token design is unreliable.** `regex: seq(token(/…\//),
   optional(regex_flags))` with `regex_flags: token.immediate(/[a-zA-Z]+/)`
   behaves correctly, but narrowing that class to `i` makes tree-sitter's lexer
   read `/x/ i` (space ⇒ a *symbol* for the compiler) as a **flag** — i.e. the
   grammar disagrees with `tsg` on a *valid* file. That is the disqualifying
   property, and it is why the flags now live inside the literal: a token never
   matches across whitespace.
4. **Accepted flag set = no flags, or one `i`.** Enforced only in
   `src/compile.rs` (`error: regex flag … has no effect`, caret on the flag,
   per-flag `= help:`). `--allow-ignored-flags` downgrades to warnings so
   `grammar.js` output stays reproducible byte for byte. All 556 `"flags"`
   occurrences in the 17 real-world grammars are `"i"`, so strictness costs
   nothing in practice.
5. **No `@error` query rule.** With flags inside the literal there is no flags
   node to capture, and an unmatchable rule is the same silent no-op the language
   exists to remove. `queries/highlights.scm` ships for editor use and
   `selfhost.sh` checks it still *compiles* against the grammar (10 captures).

An earlier suggestion — spelling the class `[a-zA-Z]*` so a query could flag bad
flags — was implemented, then dropped on the user's instruction: the grammar is
the documentation, and a wide class tells readers that `g` is a valid flag. Do
not reintroduce it without asking.

Per-flag advice lives in `flag_advice()` in `src/compile.rs`; a repeated `i` is
also an error.

**One newer compiler feature** (from the same discussion): a flags run that
spells a rule name gets the actionable hint instead of the generic one, because
the fix is a space:

```
error: regex flag `g` has no effect in tree-sitter: it is JavaScript's `global` flag, …
  --> g.tsg:3:7
  |
3 | a: /x/g;
  |       ^
  = help: if you meant the rule `g`, put a space after the literal: `/…/ g`; otherwise drop the flag
```

`/x/ g` and `seq(/x/, g)` both compile, so a rule named `g` (or `s`, or `i`) is
fully reachable. Test: `a_flag_that_is_also_a_rule_name_gets_a_space_hint`.

## Precedence levels and nullability (the session before)

Two changes, both front-end-only: `grammar.json` is unchanged, which is what
`lua53/` proves.

### `%precedence` grew the yacc surface — `src/precedence.rs`

```tsg
%precedence right "call";                    // a level only `@` uses
%precedence right "pow"     = '^';
%precedence left  "add"     = '+' '-';
```

A directive with `left`/`right` and/or `= terminals` is a **level**; every level
joins one ordering in declaration order, so all levels are comparable with each
other (that is what makes `*` outrank `+`). A directive without either is a bare
**ordering list** — `%precedence "a" "b";`, what `--from-json` prints and what a
`grammar.json` with deliberately incomparable groups looks like. That distinction
is load-bearing: `to_tsg` emits single-name ordering lists, so making a lone
quoted name a level would break 92/92 round trips.

The rewrite (`Levels::apply`, before lowering) walks each alternative and takes
its **rightmost terminal**, exactly as yacc does: in `exp '+' exp` that is `'+'`,
not the trailing `exp`; a rule reference that is a token rule resolves through to
its pattern; a nullable or choice-ending production gets nothing. The mark is a
`prec`/`prec.left`/`prec.right` wrapper around the alternative, so an explicit
`@` — which desugars to the same wrapper — simply wins.

Measured and now asserted in tests:

- `lua53/grammar.tsg`: 43 hand-written annotations → 13 level lines, 21 inherited,
  20 explicit; emitted `grammar.json` **byte-identical** to the annotated
  spelling (so `parser.c` too).
- 80 tests, including "a level and the annotation it replaces produce the same
  JSON", "an annotation overrides the table", "an ordering list is not a level",
  and the four that pin the field-label sugar (`k= a b` ≡ `field('k', a) b`).
- `tsg -p grammar.tsg` (`--precedences`) prints the resolved table: level, assoc,
  owned terminals, and every production that landed there with a line number,
  plus `N inherited, M explicit, K level(s)`. It also runs the directive checks,
  so `-p` and a build agree.

Things not to re-derive:

1. **Direction.** `grammar.json` lists precedences highest first and a level joins
   in declaration order, so the *first* line wins — the opposite of yacc, which
   writes `%left '+'` before `%left '*'`. Verified by parsing `1-2*3` (must be
   `1-(2*3)`) and `2^3^2` (right-assoc) through a generated parser. Do not
   "fix" this to match yacc: the chain has to stay consistent with `to_tsg`'s
   output and with the existing JSON.
2. **`-`/`~` are the yacc trap**, not a bug: `%left "add" = '+' '-';` gives unary
   minus the add level too, so `'-' exp @unary` keeps its annotation. Same as
   Bison; documented in `SYNTAX.md`.
3. **`nonassoc` is refused** with an explanation: tree-sitter has no
   non-associative precedence, so `a < b < c` would still parse.
4. A level name must be quoted — a bare name is a *rule reference* in
   `precedences` — and the error says so, once, at the directive.

### Nullability — `src/nullable.rs`

`tree-sitter` rejects a rule that matches the empty string *unless it is the
start rule*, and the message has no position. `compile` now runs a fixpoint over
the AST and reports it at the **alternative that produces no tokens**, with the
use sites in the help:

```
error: `block` can match nothing: this alternative produces no tokens
  --> g.tsg:3:8
  |
3 | block: (stat ';'?)*;
  |        ^^^^^^^^^^^^
  |
  = help: tree-sitter allows that only for the start rule, and `block` is used once, at 2:8; write `block?` there, …
```

The rule of thumb that matters: it mirrors `flatten_grammar.rs`'s `check()` —
*used* + a zero-step production + not `requires_eof_lookahead` — so `eof()`-only
rules are **not** reported (that false positive was in the first version and the
corpus caught it, twice: `eof_dropped_branch` broke, `epsilon_rules` and
`eof_repeat_via_nullable_rule` turned out to be `expected_error.txt` fixtures
that the real generator refuses too). `roundtrip.sh` now asks the generator
whether a refused grammar is really refuseable, instead of assuming.

## The description in the language it describes (this session)

`examples/tsg.tsg` was still written in the call dialect — `seq(...)`,
`choice(...)`, `field(...)` — while `lua53/grammar.tsg` had already been
converted. Two changes closed the gap, and the order matters:

1. **`examples/tsg.tsg` rewritten in EBNF** (67 call sites → operators):
   `repeat(x)`→`x*`, `repeat1(x)`→`x+`, `optional(x)`→`x?`, `seq`→juxtaposition,
   `choice`→`|`, `blank()`→`()`, `prec(-1, x)`→`x @-1`, `prec.left(1, …)`→`… @left(1)`.
   Verified by compiling both spellings and diffing `grammar.json`: identical
   before any new construct was added, so the rewrite is provably cosmetic.
2. **Field labels `k= a`** (`Parser::parse_labelled`), see `DECLARATIVE.md` §2.
   The first version spelled them `k: a`, which is the same prefix as a rule
   definition and swallowed the `expected `;`` error for a missing terminator; a
   test caught it, so labels are `k= a` and that error now names both readings.
   The parser desugars to `field('k', a)` the way `@` desugars to `prec`, so
   lowering, `precedence.rs` and `nullable.rs` needed no change — they still see
   `field(...)`. That removed the last 19 `field(` sites.

The last 4 `token(…)` call sites in the description are now a `tokens { … }`
block; see the latest-session section below. The call form still exists for
`--from-json`, but it is no longer needed to write a grammar.

Two things the self-description had to learn about itself, both found by the
parse-it-with-itself test rather than by reasoning:

- `labelled_expression: name= identifier '=' _postfix_expression;` needs
  `_sequence: _element _element*;` — a label is an element, not a postfix
  operator — and the `identifier ':'` lookahead ties with `symbol:
  identifier @-1`. It works as written; if a future change makes it ambiguous,
  `--json-summary` will name the state.
- `associativity= ('left' | 'right')?` **does** set the field on the keyword, so
  `(precedence_directive associativity: _ @keyword)` captures — verified with a
  one-pattern query, because `tree-sitter parse` does not print anonymous nodes
  in this version, so the tree looked as if the field had been dropped.

## Tokens and lexer modes (the previous session)

`token(…)` was the last call in the description. Two front-end changes landed
together; both are syntax sugar that desugars in the parser, so `grammar.json`
(and `parser.c`) are unchanged.

### `tokens { … }` — `src/parser.rs`, `src/lexing.rs`, `src/compile.rs`

```tsg
tokens {
    regex: /\/([^/\\\n]|\\.|\[([^\]\\]|\\.)*\])+\/(i)?/;
    integer: /-?[0-9]+/;
    identifier: /[A-Za-z_][A-Za-z0-9_]*/;
    NUMBER: '-' /[0-9]+/ @immediate;
}
```

- **One rule is one token**: the parser wraps the body in `token(…)`, so
  `keyword: 'if' | 'then';` is one `TOKEN`, not a nonterminal.
- **File order is priority order**: equal-length matches go to the earlier rule.
  A block may appear before the start rule without becoming the start rule; the
  first rule *not* declared in `tokens` is the start rule, and only that rule is
  moved to the front of `grammar.json` (token relative order is untouched).
- **`@immediate`** on a rule body is the spelling of `token.immediate(…)`.
- `src/lexing.rs` decides what is one token from the source: only `Str`,
  `Pattern`, `token(…)` and `token.immediate(…)` bodies qualify; a bare
  `choice`/`seq` is a nonterminal. It reports a rule reference inside a token at
  the reference, a zero-width token at the empty part, an `%extra` whose
  productions do not start with a token, and a `%word` that names a nonterminal.
- Verified by compiling the block and `token(…)` spellings of `examples/tsg.tsg`
  and `lua53/grammar.tsg` and diffing the JSON: byte-identical.

### `--explain-modes` / `--explain-lexing` — `src/main.rs`, `src/lexing.rs`

`lexing::describe` prints the token priority list with each rule's marker
(`word`, `extra`, `immediate`, `external`) and body, then the nonterminal count.
It is the lexical half of the lexer-mode answer. The other half — the generator's
`ts_lex_modes[state].lex_state` DFA start states — is derived from valid-token
sets in `build_tables`, not from the front end; the report says so instead of
inventing it. (`--report-states-for-rule -` on the real generator gives the
per-rule state counts when that integration is added.)

## SQLite 3 translation (this session)

`sqlite3/grammar.tsg` is [`sqlite3.lemonex`](./sqlite3.lemonex) translated to
`.tsg`: 134 Lemon rules and 167 `%token` terminals became 132 nonterminal rules
and 169 token rules, in 885 lines. `sqlite3/translate.py` is the mechanical
translation; `sqlite3/README.md` is the annotated result; `sqlite3/check.sh`
regenerates and parses five samples.

The central lesson is the precedence difference, and it is now resolved:

- **Lemon uses the leftmost terminal with a precedence; yacc/tsg use the
  rightmost.** `src/precedence.rs` is unchanged. The translator computes both
  and emits an explicit mark only when they differ.
- An explicit mark must carry associativity: `@left(compare)`,
  `@right(bitnot)`, `@on` for the non-associative level, and `@0` to suppress a
  rightmost terminal Lemon ignores. A bare `@level` is `PREC`, not `PREC_LEFT`,
  which initially made `1 LIKE 2 = 3` group as `1 LIKE (2 = 3)`; after the fix it
  is `(1 LIKE 2) = 3`.
- With associativity fixed, the earlier `%conflicts expr;` became unnecessary
  (the generator warned); it was removed. `%conflicts seltablist;` remains
  necessary for `FROM nm • ON`; removing it reproduces the generator error.

The source features that needed a translation decision:

| source | translation |
|---|---|
| `%lexer_caseless` | `/…/i` on keyword tokens; `ID`/literals/strings stay as spelled |
| `%fallback ID` + `%word ID` | `%word ID;` — a keyword not valid in the state lexes as `ID`; `CREATE TABLE select(…)` parses |
| `%wildcard ANY` | `ANY: /[^\s,()]+/;`, used by `vtabargtoken` and `anylist` |
| `%nonassoc ON` | plain level (`%precedence "on" = ON;`); tree-sitter cannot express nonassociativity |
| `QNUMBER` | omitted: internal to SQLite's parser, no lexer spelling; its one `term` production goes |
| empty productions | folded into use sites as `x?`; nullable-only `scanpt`/`scantok` disappear |

Nullability was the other trap: a rule can become nullable through a chain of
empty productions, so the translator recomputes nullable/nullable-only sets to a
fixpoint. A one-pass version left `scanpt?` markers behind after `scanpt` was
dropped. Pure left-recursive lists need their base added explicitly, and a
nullable base needs a hand-written EBNF shape (`vtabarg: vtabargtoken+`,
`vtabarglist: vtabarg (COMMA vtabarg)*`, `anylist: (ANY | LP anylist? RP)+`).

Verified:

```
$ ./sqlite3/check.sh
ok   caseless   6 lines
ok   keywords   4 lines
ok   prec       7 lines
ok   sample     6 lines
ok   vtab       2 lines
ok   301 rules, 1688390 parser.c bytes, 5 samples

$ ./selfhost.sh
ok   parsed 41 files, 9702 lines of tsg, no ERROR or MISSING node
```

`tree-sitter generate` reports no diagnostics. `sqlite3/grammar.tsg` is in the
self-host parse set, and `prec.sql` asserts the precedence/associativity answers
(including `- 1 * 2`, `~ 1 + 2`, `1 || 2 || 3`, `1 LIKE 2 = 3` and `BETWEEN`).
No Rust source changed in this session; the existing 80 tests and the other
harnesses are unaffected.

## Bison/Flex metagrammar front end (this session)

The Postgres translation was paused when the metagrammar `bison-flex.g` arrived.
Rather than keep growing the ad-hoc `.g` reader, the project now has a
tree-sitter grammar for the joined Bison/Flex format itself.

`bison_flex/grammar.tsg` is 140 lines and compiles with `tsg`;
`tree-sitter generate` reports no diagnostics. The generated parser accepts
both inputs with **0 ERROR / MISSING**:

```text
postgres16.g   705 rules, 3282 alternatives, 513 %token directives,
               27 precedence levels, 187 %empty, 55 %prec,
               617 Flex lines, 4 section separators
bison-flex.g   69 rules, 104 alternatives, 3 %token directives,
               8 %empty, 4 section separators
```

`./bison_flex/check.sh` does the whole loop: `tsg` → `tree-sitter generate` →
build parser → parse both files as XML → assert the counts above. It keeps
cache, dylib and XML in `bison_flex/.check/`, which is gitignored.
`bison_flex/extract.py` is the first CST consumer; it reads the XML and prints
the same shape. `bison_flex/README.md` is the design note.

### What is structural

- `%token`, `%fallback`, `%left`, `%right`, `%nonassoc`, `%precedence`,
  `%start`, `%token_indent`, `%token_dedent`, `%token_symbol_table`;
- token names, quoted literals, `<type>` tags;
- rule names, alternatives, `%empty`, `%prec`, `#ProductionAlias`;
- EBNF `[...]`, `(...)`, `{...}`, and `?`, `*`, `+`;
- all four `%%` separators.

### What is line-oriented, and why

The Flex section is captured as `LexerLine` nodes, one per non-empty line.
Flex actions are arbitrary C; a context-free grammar cannot model them honestly
without an opaque scanner. The translator splits the lines it needs: `%option`,
`%x`, macro definitions, keyword mappings, and lexer rules. Multi-line C actions
therefore do not produce parse errors.

Two details that matter if this is edited:

1. `NL`, `WS`, and `COMMENT` are extras, but `NL` is also an explicit token.
   Tree-sitter returns it when the parser expects it and skips it otherwise, so
   the same token terminates `%x`/macro/rule lines and is trivia in the rule
   section. This was tested; do not remove the token.
2. `SectionSep` is `%%[^\n]*` (the whole line, including a trailing comment),
   not just `%%`. Otherwise the broader `LexerLine` wins the longest match and
   the section boundary disappears.

Nullable nonterminals are still forbidden below the start rule. `%empty` is
`alt_rhs: PercentEmpty | rhs_sequence`, and every optional piece is spelled at
the use site rather than hidden in a nullable named rule.

### Known gaps for the 300-grammar corpus

This first cut is deliberately aimed at `postgres16.g` and `bison-flex.g`. It
does **not** yet model:

- `%{ ... %}` prologue/epilogue code and `%union { ... }`;
- Bison mid-rule actions and `%code` blocks as opaque code;
- unknown Bison directives (`%type`, `%define`, `%expect`, `%destructor`, …);
- `%dprec` / `%merge`;
- a structured Flex regex AST (only raw `LexerLine`s).

Those should be driven by what the corpus actually rejects, not guessed. The
next useful move is to point `check.sh` at the corpus and classify the failures.

### The CST → `.tsg` translator (`bison_flex/translate/`, this session)

The metagrammar now has a real consumer: `g2tsg`, a 2 000-line Rust crate in
`bison_flex/translate/` that reads a `.g` with `bison_flex/grammar.tsg` and
writes the `.tsg`. `postgres16/grammar.tsg` is now **generated** by it, and
`postgres16/check.sh` fails if the committed copy is stale.

    src/cst.rs   .g -> ir::Grammar (the tree-sitter walk; all API use is here)
    src/ir.rs    the source's shape before translation
    src/flex.rs  Flex pattern text -> tree-sitter regex
    src/emit.rs  the decisions (%empty, ?, %prec, _LA, token bodies)

Per-grammar data is files, not code: `postgres16/{header,conflicts,bodies}.txt`.

Verified: the emitted `.tsg` produces a **byte-identical** `grammar.json` to the
hand-verified one (so `parser.c` is unchanged: 1 195 rules, 1 378 states,
25 294 070 bytes, 4.11 s), and from `%name` down it is identical to
`postgres16/translate.py`, which stays as the reference implementation and which
`check.sh` diffs against. Whole translation: **0.17 s** for 6 741 lines /
250 KB (release).

What the CST buys, measured on inputs the regex translator mis-reads:
`%token <ival> ICONST` (the tag became a token), `stmt : [a b] c`, `#add`, and a
missing `;` (the next rule was swallowed). All four are now either handled or
rejected with position `line:col`; `g2tsg --dump` prints the s-expression when
one is rejected. `bison_flex/translate/README.md` is the write-up: integration
(3 pieces), the node table, the four API facts that matter (extras are named
nodes, `kind` ≠ `text`, named vs anonymous punctuation, no fields needed), the
error-reporting heuristic (innermost `ERROR`, then the last whole construct),
and the limits (groups/`*`/`+`/`%fallback` rejected, Flex stays line-oriented).
`src/tests.rs` holds 19 in-process translations, one per behaviour, and the crate
is `cargo +nightly fmt` and clippy clean.

The second dog-food grammar (`antlr-v4.5.g`) is written up in `DOGFOOD.md`: it
found four translator bugs (a dropped production turning into a silent empty
production, a trailing `//` comment eating a lexer action, commented-out lexer
rules being read as rules, and a start rule silently replaced when it lost every
production), and it is the evidence for the tree-sitter gap below — a rule that
derives nothing, or that nothing can reach, generates a parser with exit 0 and no
warning.

### The corpus run (this session)

`g2tsg` over the 358 `.g` files in
`/Users/mingo/dev/parsertl-playground/playground`, then `tsg` over each output:

    284  translate to a `.tsg` that `tsg` accepts          (was 74 before this)
     70  rejected by `g2tsg`, with a file:line:col
      4  translated, but the `.tsg` is not valid

(The first count was 290 before `antlr-v4.5.g`; 16 files lost their start rule to
the dead-production cascade and used to have the next rule promoted to start
symbol without a word. They are now errors — a smaller number that means
"correct or refused" is the one to trust. `DOGFOOD.md` explains.)

That block is the measurement before `%fallback` was translated.  After it
(`bison_flex/translate/README.md` §"What the corpus says", same harness): **301
translate / 52 rejected / 5 invalid**.  The 17 `%fallback` files moved: 13 now
stop at the nullable-mid-body gap, 3 at the dead-token gap, 1 translates but
`tsg` rejects it (a `.` in a rule name).

The 64 (the metagrammar read every one of them — these are translation gaps, not
parse failures):

    16  every production of the start rule needs a token the lexer never returns
    10  `A` is nullable and refers to itself in the middle of a body
    17  `%fallback`, `%token_indent`, … is not translated yet
    10  `[...]`, `(...)`, `{...}` groups are not translated yet
     9  `unexpected …` — not a Bison file (ANTLR, Lark, PEG, BNFC are in the corpus)
     3  the source has no rules — likewise
    10  one-offs (`%prec` of an undeclared token, `_LA` with no base, still-nullable)

The 10 nullable ones are the same gap as `postgres16.g`'s 8 000 productions, and
the 17 `%fallback` ones have no tree-sitter equivalent at all. Everything that was
fixed to get from 74 to 290 is in `bison_flex/translate/README.md` ("What the
corpus says"): `/` escaping in Flex patterns (a `/` closes the literal in the
emitted `.tsg`), implicitly declared tokens (`%nonassoc lazy` plus a lexer rule),
`"…"` literals not becoming token rules, empty alternatives deleted, dead
productions dropped to a fixpoint, `#alias` dropped, duplicate LHS appending,
and `\x` / `\0` byte escapes normalised (`\x0000` → `\x00`, which
`regex-syntax` accepts and Flex's "any number of hex digits" allows).

### Next restart

1. ~~Parse all 300+ `.g` files, record every rejected directive/line, and decide
   which need grammar support.~~ Done: 290/358 translate and are valid; the 64
   rejections are bucketed above.
2. Add opaque C handling. Preferred: an external scanner for balanced `{…}` and
   `%{…%}`. Acceptable first pass: a placeholder pass that preserves offsets.
3. ~~Write the CST → `.tsg` translator, starting with `postgres16.g`.~~
   Done: `bison_flex/translate/` (see above).
4. ~~Only then run `tree-sitter generate` on the translated Postgres grammar and
   measure the phases.~~ Done, and the measurement drove the generator fixes in
   `tree-sitter-patches/` (this was the whole reason for the port).

No Rust source changed for the metagrammar (it is still 140 lines of `.tsg`); the
new code is the `bison_flex/translate/` crate, which is a separate workspace and
does not affect the 80 `tsg` tests. It does depend on the tree-sitter runtime
at `../../../../lib`.

## Layout

```
Cargo.toml  Cargo.lock  README.md  SYNTAX.md  HANDOFF.md  .gitignore
src/{main,lexer,parser,rules,json,json_read,compile,to_tsg}.rs   4.6k lines, std only
src/{source,precedence,nullable,lib}.rs   1.1k lines, shared with the generator integration
examples/*.tsg        16 files: fixtures/showcases + tsg.tsg (the language itself)
surface/*.tsg         the description's test suite: every directive and expression
                      shape once, checked by selfhost.sh + coverage.py
queries/highlights.scm  queries for tsg.tsg
*.json                17 real-world grammars: bash c cpp dart go html java js
                      kotlin mssql php plsql python ruby rust swift ts
diff.sh  e2e.sh  roundtrip.sh  selfhost.sh  compare.py  coverage.py  json2ebnf.qjs
bison-flex.g          metagrammar for the joined Bison/Flex `.g` format
postgres16.g          Postgres stress grammar (705 rules)
bison_flex/           tree-sitter grammar for `.g`, check.sh, extract.py, README
bin/tree-sitter       prebuilt CLI, 8.4 MB: release+stripped build of
                      /Volumes/RamDisk/tree-sitter at patches 0001-0009
                      (NOT stock: it has --lalr, --max-states,
                      --report-states-for-rule, --report-lex-states)
target/ .diff/ .e2e/ .roundtrip/ .self/    build output and scratch, gitignored
```

Harnesses (all default to `$here/target/debug/tsg` and `$here/bin/tree-sitter`,
overridable with `TSG=` / `TS=`):

- `./diff.sh` — compares emitted JSON with the real `dsl.js` under node.
- `./e2e.sh` — `tree-sitter generate` both sides, byte-compares `parser.c`.
- `./roundtrip.sh [--parsers] [filter…]` — `grammar.json → .tsg → grammar.json`
  over all 92 grammars (17 real + 75 fixtures); `--parsers` is the byte-level
  check (takes a long time; filter args are substring filters, e.g. `_root_`).
- `./bison_flex/check.sh` — builds the Bison/Flex `.g` parser and asserts the
  shape of `postgres16.g` and `bison-flex.g` (see the section above).
- `./selfhost.sh` — compiles `examples/tsg.tsg`, builds a parser, parses every
  `.tsg` file it can find with it (22 files, 850 lines without the round-trip
  corpus; that corpus adds 75 more) and asserts no `ERROR`/`MISSING` node, plus
  the flag checks and the query compile check. The parsed set includes
  `lua53/grammar.tsg` and `surface/*.tsg`, so the self-description cannot
  silently fall behind the compiler.

Docs: `README.md` (why, proof, error tour, design notes, the flag analysis) and
`SYNTAX.md` (§1–§3 syntax, §4 diagnostics policy, §5 the self-description and the
flag experiments, §6 reverse direction, §7 deliberate omissions).

## Toolchain provenance, and the definition of green

Every gate script defaults to `ts=${TS:-$here/bin/tree-sitter}` and
`tsg=$here/target/debug/tsg`.  `bin/tree-sitter` is **the patched CLI** (release,
stripped, 8.4 MB) built from `/Volumes/RamDisk/tree-sitter` at patches
`0001`-`0009`; it is not stock 0.28.0.  Consequence: `postgres16/check.sh` and
`bc/` need `--lalr`/`--max-states` and **cannot** run on a stock CLI, so do not
"just download a release binary" and expect the gates to pass.  Rebuild recipe
(fits in the sandbox, ~2 min, 408 MB while it lasts):

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd /Volumes/RamDisk/tree-sitter
CARGO_TARGET_DIR=tmp/tsg/.tsbuild CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_RELEASE_STRIP=true cargo build --release --offline -p tree-sitter-cli
cp tmp/tsg/.tsbuild/release/tree-sitter tmp/tsg/bin/tree-sitter
rm -rf tmp/tsg/.tsbuild
```

**What is deliberately *not* in `bin/`: `tsg` itself.**  The gates call
`$root/target/debug/tsg` on purpose — it is always the current source, so a gate
can never pass against a stale compiler.  The cost is nil: on the largest grammar
here (`postgres16/grammar.tsg`, 116 KB) the debug binary takes 50 ms and a release
build 9 ms, next to `tree-sitter generate`'s 3.8 s.  A release copy in `bin/`
would buy 40 ms and add a way to test the wrong binary.  `cargo build` after
`clean.sh --target` takes 0.7 s.

`cargo test --release -p tree-sitter-generate` (104 tests) is the only way to run
the generator's tests here: a debug build of that crate is 277 MB and, with
`CARGO_PROFILE_DEV_DEBUG=0`, the test binary fails to strip ("permission
denied") — the release profile reuses the artifacts above.

`bin/g2tsg` (0.6 MB, release, stripped) is the `.g -> .tsg` translator, built the
same way from `bison_flex/translate` (`CARGO_TARGET_DIR=.../.g2tsg
cargo build --release --offline`; 43 MB while it lasts, the crate's tests are 23
and pass).  Its `build.rs` compiles `bison_flex/src/parser.c`, so run
`./bison_flex/build-parser.sh` first when `target/` or that file has been cleaned:
without it, `cargo build` there fails with "parser.c is missing" and nothing says
how to get it back.  `bison_flex/check.sh` now calls the script, so a gate run is
a complete repair.  `postgres16/check.sh` and `antlr/check.sh` need it as
`G2TSG=$root/bin/g2tsg`.  Both were red today for reasons that had nothing to do
with the generator:

- `antlr/check.sh` reported `grammar.tsg is stale`: the Flex start-condition
  strip did not handle ANTLR's `{LBrace}<>TargetLanguageAction>`, so tokens came
  out as `/(\{)<>TargetLanguageAction>/`.  `cst.rs::strip_start_condition` now
  tolerates one `>` before the state list; the translation matches the committed
  `grammar.tsg` again and the 3 samples parse with no `ERROR`/`MISSING`.
- `sqlite3/check.sh` failed at `build`, not at `generate`: `generate <file>`
  writes `parser.c` next to the file (`.check/src`), while `build` finds the
  parser through a `tree-sitter.json`, and the checked-in one says `"path": "."`,
  i.e. `./src`.  The script now writes a `tree-sitter.json` into `.check` and
  builds from there; `metadata` is required, `links` may be `{}`.

The translator no longer guesses the separators: `emit.rs` emits one `%extra`
per Flex rule whose action is `skip()`, `;` or empty (a `.g` with no such rules
falls back to whitespace).  That is what `bc` needed — the hardcoded
`%extra /--[^\n]*/` ate `while (y--)` — and `postgres16/translate.py` was made
to agree: it now derives the same extras, escapes `/` (its regexes are wrapped
in `/…/`) and spells the C control characters `\x0c`-style like `flex.rs`.
`postgres16/grammar.tsg`, `antlr/grammar.tsg` and `bc/grammar.tsg` were
regenerated from that; the first two gates re-verified byte-consistency.

Green means, at minimum:

```
cargo test --release -p tree-sitter-generate        # 104
cd tmp/tsg && cargo build && cargo test             # 80
./selfhost.sh ./diff.sh ./e2e.sh ./roundtrip.sh --parsers
./sqlite3/check.sh ./bison_flex/check.sh
G2TSG=$PWD/bin/g2tsg ./postgres16/check.sh ./antlr/check.sh
```

plus: `parser.c` md5 unchanged by any report-only flag, and new warnings absent
on the 92-grammar corpus.

## The untracked tree is the only copy

`tmp/` is not git-tracked (by design: 65 MB of scratch + generated grammars), so
deleting or losing `/Volumes/RamDisk/tree-sitter/tmp` loses the whole project
front end.  It happened once (an empty folder came back after a space
cleanup), and the only backup was `~/dev/tree-sitter.tgz`, **pre**-lex-report.
Do one of these before stopping for the day:

- `git add -f tmp/tsg && git commit -m "wip: tmp/tsg"` on a `wip` branch, or
- `tar czf tmp/tsg-wip.tgz -C tmp tsg` and keep it outside the ramdisk, or
- copy the same tarball to wherever the user's real backups go.

Never leave the day's only copy on the RAM disk.

## Constraints (hard)

- **Write only inside `tmp/tsg`.** `tree-sitter build`/`parse` otherwise create
  `~/.cache/tree-sitter/lock`; `selfhost.sh` redirects `XDG_CACHE_HOME` to
  `.self/cache` and the dylib to `.self/lib` (set `TREE_SITTER_LIBDIR` too).
- **Nothing outside `tmp/tsg`, including build output.** Scratch, evidence,
  grammar projects, `XDG_CACHE_HOME` and `CARGO_TARGET_DIR` all belong in
  `tmp/tsg/.<name>/`.  Recently violated and cleaned up (2.7 GB deleted):
  `~/.ts-build` (patched CLI), `~/.ts-exp` (a 545 MB snapshot copy of this whole
  project, plus the `g2tsg` target), and a `git worktree` at `0fff6ab` whose
  `add` also wrote `.git/worktrees/` into the checkout.  Budget: the ramdisk has
  ~150 Mi free, so any build that needs more than that (the CLI target dir is
  2.5 GB, `g2tsg` 224 MB) has to go **off** the ramdisk.  Make it a throwaway:
  build into a directory you delete when you stop, and say where it is in the
  session notes.  Do not leave it behind — that is what `ts-build/`, `ts-exp/`
  and `ts-up-target/` were.
- `tmp/tsg/target/` is the exception that fits: `tsg` has zero dependencies, so
  its own `cargo build`/`cargo test` run on the ramdisk.  With
  `.cargo/config.toml` setting `build.incremental = false` it is **34 MB**, not
  152 MB (59 MB of that was `incremental/`), and a from-scratch build is 0.7 s —
  incremental compilation was costing 4× the disk to save 0.3 s.
- **Zero dependencies** (`[dependencies]` is empty, `[workspace]` stanza present).
  crates.io returns 403 from this machine.
- **Never `/tmp`.** Scratch goes in `tmp/tsg/.<harness>/`.
- The ramdisk is 1.0 GiB total; keep an eye on `df -h /Volumes/RamDisk`.
  There are **two** cargo target trees — `target/` and
  `bison_flex/translate/target/` (135 MB, the translator has `regex`) — plus
  `bin/` (9 MB).  `./clean.sh --dry-run --target` is the inventory;
  `./clean.sh --target` removes every `target/` under this directory.  It used
  to prune only `$root/target` and report the space it had not freed, which is
  how 135 MB sat there while the script said 274 MB had gone.
- After `clean.sh --target`, the translator needs its metagrammar parser back:
  `./bison_flex/build-parser.sh` (0.3 s) before `cargo build` in
  `bison_flex/translate`.  `bison_flex/check.sh` calls it, so running the gate is
  enough.
- `tree-sitter generate` resolves a bare `src/grammar.json` relative to CWD, so
  run it from the grammar's parent directory.
- Don't remove the 920 MB `rust-docs` component (declined earlier).

## Authoring friendliness

`AUTHORING.md` is the criterion-driven gap list ("no hidden errors or warnings,
no gymnastics"): silent acceptance of unknown keys, errors that hide their cause
behind `--log`, opt-in lexer warnings, the missing constructs (empty
productions, `%nonassoc`, lexer lookahead, `%fallback`, `^`/`$`), reports that
cannot be joined, and the short list I would actually implement next.

## Open items, in the order I would do them

**For anything about authoring, `ROADMAP.md` overrides the order below.**  It is
organized by one criterion — "an author writes what the language is, and never
works around the tool" — and it explicitly drops JS-DSL parity and stock
tree-sitter compatibility as goals.  Read it first; the list below is the older,
broader backlog (CLI integration, editor story, EBNF surface, modularity).

1. **Wire it into the CLI** (`tree-sitter generate grammar.tsg`) or move the
   front end to `crates/generate/src/declarative/` — the prototype's only contract
   is emitting `grammar.json`, so this is a mechanical move.
2. **Migrate for real**: replace hand-written `examples/*.tsg` with
   `tsg --from-json` output and diff; that is the actual JS-DSL escape route.
3. **Editor story**: an LSP wrapper over `tsg`'s spanned diagnostics, or teach the
   `.tsg` grammar the semantic layer (rule names, call arities) so the tree can
   carry more of it.
4. `to_tsg --style ebnf` — the reverse direction still prints the call dialect,
   so the EBNF surface is only exercised by hand-written files. Making EBNF the
   default is the cheapest way to keep it honest. (It must keep the bare ordering
   form and handle `= ;` levels.)
5. The five remaining EBNF spellings in `DECLARATIVE.md`: `alias` (`a ^'k'`),
   `reserved` (`a %set`), `$` for `eof()`, `sym(...)`, and demoting the call form
   to import-only (`--allow-calls`, `--from-json` output). The old
   "variadic `reserved(...)`" worry is resolved: the call is fixed at two
   arguments in both `dsl.js` and `rules.rs`; the variadic form is the
   `%reserved set = a, b;` directive, which lowers to one `RESERVED` per rule.
6. Pretty-print the reverse output for humans (comments, minimal parentheses) —
   it is currently written for round-trip fidelity, not readability.
7. `%include` / modularity (`grammar(baseGrammar, …)` is deliberately omitted).
8. **Bison/Flex corpus and translator.** The front end at `bison_flex/` parses
   `postgres16.g` and `bison-flex.g`, and `bison_flex/translate/` (g2tsg) turns
   `postgres16.g` into the grammar that is checked in. Next is the 300+ corpus:
   running `g2tsg` over it produces the list of unsupported constructs with
   positions. The known gaps and the restart checklist are in the section above.
   Neither touched the Rust compiler.
9. ~~**Derive `%extra`s from the lexer's `skip()` rules**~~ — done (see the
   translator note above).  `bc/grammar.tsg`, `postgres16/grammar.tsg` and
   `antlr/grammar.tsg` were regenerated; `postgres16` and `antlr` re-verified.
   It is a *regression* on `bc` (3 `ERROR` + `MISSING Scale` → 24 `ERROR` +
   `MISSING Break`), and the reason is worth keeping: `bc.g`'s lexer has no
   newline rule, so the guessed `[ \t\n\r\f]+` extra was covering for a hole
   in the source grammar (`ENDOFLINE` is hardcoded to `/\n/` by the translator).
   Faithful extras report the hole instead of hiding it — the fix belongs in
   `bc.g` (or a `bodies.txt` extra), not in the emitter.
10. **Lexer reports in the generator** — a lex-state report next to
    `--report-states-for-rule` (valid tokens + extras per lex state, respecting
    `--lalr`), then a token-shadowing/overlap report.  The dog-food runs wanted
    this three times; `tsg` already computes a token-overlap NFA for itself.
    Inventory of what the CLI does report: `DOGFOOD.md`, dog-food #3.
11. **`bc` as a third gate** (`bc/check.sh`): 22 rules, 12 precedence levels,
    `%nonassoc`, a three-pattern `NUMBER`, and a real 272-line sample.  Blocked
    on items 9 and 10 only.

## Integration work in progress (see `INTEGRATION.md`)

The crate now also builds as a library (`src/lib.rs`, `[lib] name = "tsg"`), so
`main.rs` is a thin CLI over the same modules. Two additions exist purely for the
tree-sitter integration:

- `src/source.rs` — `Index` (rule/directive/pattern name -> span, built from
  `GrammarAst`) plus `Label`/`render`, a rustc-style multi-label renderer.
  `lexer::render_diagnostic` delegates to it, so pre-JSON messages keep their
  shape (and gained `...`/multi-label support).
- `Compiled.index` — `Index::from_ast` is built during `compile()` and returned
  next to `json`/`warnings`; `GrammarAst.rule_names` carries the name spans.
- `prototype/` — a dependency-free binary that runs the **real** generator
  (`bin/tree-sitter generate --json-summary`, decoded with `json_read`) over nine
  grammars that `tsg` accepts and the generator rejects, and renders each error
  against the `.tsg` source. Transcript: `prototype/OUTPUT.md`. It cannot link
  `tree-sitter-generate` here: there is no crates.io access *and* no cargo
  registry cache, so `bitflags` is unresolvable.

### Dogfooding grammar: `lua53/`

`lua53/grammar.tsg` is a Lua 5.3 grammar ported from `lua53.lemonex` (Lemon +
integrated lexer, 185 productions): 142 lines of `.tsg`, accepted by `tsg`,
generated by `tree-sitter generate` **with no diagnostics**, and parsing
`sample.lua`/`tricky.lua`/`prec.lua` cleanly. It exists to exercise the pipeline
on a real grammar instead of fixtures.

`lua53/DOGFOODING.md` is the annotated result: the four failures hit, the two
features that would have saved the most work (per-use precedence, automatic
empty-rule elimination), the missing `.tsg` features the port wanted, and one
**upstream tree-sitter bug** (`choice(token(…), token(…))` silently inserts a
zero-width node, exit 0; reproducible from `grammar.js`, workaround and repro in
`lua53/variants/v5-token-choice-misparse.tsg`). `lua53/variants/v1`–`v3` are the
three generator errors this grammar actually produced, kept as L1.5 fixtures.

Three `tsg` changes came out of it: `compile.rs` now rejects a `%precedence`
directive whose bare entries are used as precedence names (a `prec(call, …)`
against a rule reference, which previously reached the generator with no
position); `compile.rs` warns when a rule is named like a DSL function (Ruby's `alias`,
MSSQL's `field`), because call syntax has to keep winning — a grammar can need
`field(name, rule)` and `SYMBOL field` in the same file, which is why the
"declared rule shadows the function" variant broke 2 of 92 round trips; the
spelling for the rule is `$field`; and
the never-running `reports_where_the_flag_is` test was fixed. Test count
33 → 38; `selfhost.sh`, `diff.sh` re-verified.

`examples/tsg.tsg` — the language's own grammar — was brought up to date at the
same time: `%precedence` levels (`left`/`right`, `= terminals`, `= ;` for a level
no terminal inherits), the `@` annotation in all five shapes, and `()` as the
blank rule are part of the self-description. Two checks keep it honest:

* `surface/*.tsg` (`directives.tsg`, `expressions.tsg`, `no-extras.tsg`,
  `tokens.tsg`) spell every directive form, expression shape and lexical section
  once. Each must compile *and* parse with the parser built from
  `examples/tsg.tsg`, so a construct the description cannot read is an `ERROR`
  node and fails the build. This caught
  two real gaps: `%precedence "x" = ;` was rejected (the `repeat1` after `=`
  should have been `repeat`) and `%extras none;` had no fixture at all.
* `coverage.py` reads the description's own `node-types.json` and requires every
  *named* node type to occur in the parse of `surface/`. Verified to bite: adding
  `foo_directive` to `examples/tsg.tsg` without a fixture fails with
  `surface/ never spells 1 of 32 node types`.

Pointing the self-description at files it had never been asked to parse is what
found the `name (group)` bug and the stale `%precedence` rule; this is that idea
made automatic. The anonymous tokens (`%name`, `(`, `|`, …) are not tracked —
the parse output does not print them, and a fixture spelling one wrongly produces
an `ERROR` node, which the first check catches.

`grammar.tsg` itself is written in the EBNF surface, and its precedence is now a
table (13 `%precedence` levels owning their terminals), so 21 of the 41 marked
productions inherit their level and only 20 keep an `@`. It was checked against
both earlier spellings — `prec(...)` calls and 43 hand-written `@`s — and all
three emit identical `grammar.json` (and therefore `parser.c`).

`DECLARATIVE.md` is the design note that came out of that conversation: EBNF is
the primary surface, the call form is the `grammar.js`/`--from-json` interop
dialect, and it lists what is left to make the rest of the surface keyword-free
(`a ^'k'` aliases, `a %set`, `$`) plus the
feedback work that makes LR errors speak `.tsg` instead of automaton states.

`SYNTAX.md` §8 is now the audit that closes the other half of that question:
every spelling with two readings, how it is resolved, and what to write instead.
Two are irreducible (the call-vs-rule `name (` case, which `grammar.js` compat
requires, and the reserved words `left`/`right`/`dynamic`); the rest are either
decided by the grammar or reported with a message that names the fix. That
section is the checklist to consult before adding a construct: a new keyword that
can also be a rule name belongs in it.

## Things that will waste time if forgotten

- `compare.py <name> <expected> <actual>` prints the `ok` line itself; harnesses
  must not print a second one.
- Absent ≡ empty collections: `precedences`, `conflicts`, `externals`, `inline`,
  `supertypes`, `reserved` (but *not* `extras`).
- `tree-sitter parse --lib-path <dylib> --lang-name tsg <files…>` avoids needing a
  configured parser directory.
- Bash on this machine has no `timeout` and `cat -A`; `grep` patterns with
  `[2, 6]` need escaping, and a `(symbol …` grep must not include the closing
  paren — the tree line can wrap.
- Rust test gotcha: `let errors = errors(…)` shadows the helper; name locals
  differently.
- When editing large generated `.tsg`/docs, use `python3` with `assert old in s`
  before `str.replace` — a silent no-op replace already bit twice.

## Comparing two grammars: `tsg --diff`

`tsg A --diff B` compares grammars, not text.  Both sides may be `.tsg` or
`grammar.json` (dialect is picked by the file name, `-r` forces JSON for stdin);
each is lowered to `grammar.json` and the two documents are walked in document
order.  Comments, indentation and sugar never matter; order that *is* meaning
(rule order — the start rule — key order, alternative order) is compared, because
the generator observes it.  Only `conflicts` is sorted first: it is a list of
sets the generator rewrites.

```
$ tsg bc/grammar.tsg --diff bison_flex/grammar.tsg
name:
  - "bc"
  + "bison_flex"
...
$ tsg postgres16/grammar.tsg --diff src/grammar.json
no difference: postgres16/grammar.tsg and src/grammar.json are the same grammar
```

Exit 0 means "same grammar", 1 means "different", 2 means a usage error.  Up to
20 differences are printed with their path (`rules.BCONST.content.value`), so a
failure names the rule and the field instead of the line number.

`postgres16/check.sh` and `antlr/check.sh` now compare this way: `--diff` decides
whether the committed `.tsg` is still the translation, and `cmp` only distinguishes
"byte-for-byte copy" from "same grammar, different comments" (a note, not a
failure).  Verified by appending a stray `//` comment to `antlr/grammar.tsg`: the
gate stays green and prints the note instead of a 4 000-line diff.

### Why there is no `tsg fmt` yet

`tsg -r grammar.json` already prints normalized `.tsg` (round-trip is exact for
all 92 corpus grammars), so the printer exists.  It cannot be `fmt` for a `.tsg`
source: comments live only in the source text — the JSON path drops them — and
`postgres16/grammar.tsg` starts with 27 lines of program notes that a formatter
must not eat.  A formatter therefore has to print from the AST (`parser.rs`
`Grammar`), not from the lowered JSON, and it must keep the leading comment
block.  Measured cost of the shortcut: `tsg postgres16/grammar.tsg -o g.json &&
tsg -r g.json` loses exactly those 27 header lines and 4 390 diff lines overall.
Until that printer exists, use `--diff` to compare and leave whitespace alone.

## `tsg --fmt` and `--check`

`fmt` re-prints a `.tsg` from its AST — not from `grammar.json`, because the AST
is the only place the authoring sugar survives (`?`, `@left(name)`, `$field`,
`tokens { … }`).  It normalizes layout only: one directive per line, one
alternative per line with `  |` continuations, no alignment, minimal parentheses
recomputed from the AST, one quote style per literal.  It never reorders rules
(rule order is the start rule), alternatives, precedence levels or token bodies.

Two properties, checked by running the 31 hand-written grammars through it:

* **same grammar** — `tsg X --diff <(tsg --fmt X)`, which is now a one-liner;
* **idempotent** — `fmt(fmt(X)) == fmt(X)` byte for byte.

Three bugs the sweep found, all fixed: the `token(…)` wrapper the parser adds to
`tokens { }` bodies was printed back (nesting `TOKEN(TOKEN(…))`); the block was
hoisted to the top of the file, which changes the start rule when the block sits
late (as in `examples/tsg.tsg`); and a rule named `field` was printed bare,
where it re-parses as the DSL's two-argument `field(…)` call — it now prints
`$field`, exactly as the source comment in `lua53/grammar.tsg` warns.

`--check` is for CI: it writes nothing and exits 1 when the file is not
canonical.  **No committed grammar is canonical yet** (all 29 differ), because
of the known limitation below; running `tsg --fmt -o file` once would fix each,
but that is a deliberate, reviewable reformat, not something a hook should do
behind the author's back.

### Known limitation: comments

Comments exist only in the source text, and the AST has no comment nodes.  `fmt`
keeps the **leading comment block verbatim** — that is where these grammars put
their program notes — and reports the number of interior comment lines it did
not carry (never silently).  Making it lossless means re-inserting comments by
span between the items that own them (rules and directives both carry spans);
that is the next step, and until it exists `--diff`, which ignores comments
entirely, is the way to compare a reformatted file with its original.

## Gate 9: `bison/check.sh` — a `.g` with a lexer in the back

`bison.g` is a grammar for the yacc dialect and `carbon-lang.y` is written in that
dialect, so parsing it is the whole test: translate, generate, build, parse, and
require zero `ERROR`/`MISSING` nodes.

**Correction to an earlier reading of this file.** `bison.g` *does* have a Flex
lexer — it sits after the grammar (sections: declarations, rules, definitions,
rules), and it was the translator that lost it, in two opposite ways:

* `<states> { … }` groups were skipped outright (a comment in `cst.rs` called
  them "lookahead", which is true of `postgres16.g`'s `reject()` trick and false
  of a plain Flex group). `bison.g` keeps its *entire* lexer in
  `<INITIAL,RULES_ST> { … }`, so all of it went.
* a rule with a state prefix but no group (`<EPILOGUE_ST>(?s:.)+ EPILOGUE`) was
  *kept* with the prefix stripped, i.e. promoted to a rule that runs everywhere.
  `(?s:.)+` then matches the whole file.

Both are fixed in `cst.rs::split_lexer_rules`, and the rule is now stated once:

* a `<states> { … }` group is expanded when it includes `INITIAL` (a stateless
  lexer can express it), and kept out otherwise;
* a rule whose only states are non-`INITIAL` is kept out;
* a rule that names a target state (`pattern<X> action`) is kept, and the dropped
  switch is counted;
* everything kept out is *reported*, per class, with examples — a silently
  dropped rule is indistinguishable from a token the source never had, which is
  exactly how the earlier note ("the lexer never returns STRING, TSTRING,
  BRACKETED_ID and 7 more") ended up blaming the source for its own lexer.

Measured over the 358-file corpus: 288 translate (was 287), **119 files now say
which rules were left out for start conditions and 84 which could not be read**
(previously silent); 85 files have `<state> {` groups and 12 of them include
`INITIAL`, so those now expand. `bison.g` itself: 10 tokens and 3 extras from its
own lexer, no hand-written bodies, 37 `.tsg` rules → 301 generated rules, 1.3 MB
`parser.c`, and `carbon-lang.y` parses to 4 365 nodes with 0 ERROR / 0 MISSING.

**Gate 9, second pass: `ACTION_CODE` is now a generated scanner.** It is the
balanced-brace pushdown (`<ACTION_CODE_ST>` recurses on `{`, pops on `}`), and no
regex can do that — but a scanner can, because the depth is *local to the
region*: one token in, one token out, no state between tokens. `cst.rs` now
recognises the Flex idiom (`{` re-enters the state, `}` pops and returns a token,
`(?s:.)` keeps it) and `emit.rs` writes the one-line `%external ACTION_CODE =
delimited('{', '}')`; `tsg` generates `src/scanner.c`. Result: 38 `.tsg` rules →
**349 generated rules**, 1 506 917-byte `parser.c`, and two samples parse with 0
ERROR / 0 MISSING — `carbon-lang.y` (4 365 nodes) and `actions.y` (110 nodes,
**6 `ACTION_CODE` nodes**), which is the sample that proves the scanner ran:
without it, actions have no token at all.

**What is still not translatable** is `EPILOGUE`: `<EPILOGUE_ST>(?s:.)+ EPILOGUE`
swallows everything after the second `%%`, which is *not* a balanced region (no
closing delimiter, no return to `INITIAL`). It stays reported, and the one
production that needs it is dropped. `carbon-lang.y` has no epilogue, so the gate
is green — but the loss is printed, not hidden.

One hand edit is left in `bison/patch.py`, and it is about grammar shape, not
lexing: `ID_COLON` is the yacc hack that binds `name:` into one token, so a rule
whose body opens with `|` needs an alternative at the use site, because `rhs`
cannot be nullable (tsg rejects it, correctly, and says to move the emptiness to
the caller).

---

## `%external NAME = delimited(open, close)`: a generated scanner

The question this answers: tree-sitter has lexer states (`lex_modes[parse_state]
.lex_state` → `lex_fn`), but the state is a *pure function of the parse state* —
there is no lex-state stack, and there cannot be one, because incremental
re-parsing has to be able to re-lex from a parse state alone. So a lexer that
pushes and pops (Flex `%x` + `<<>`, or a balanced-brace body) cannot be written in
`grammar.json`. External scanners are the supported mechanism, and `tsg` now
generates one for the single case where the missing state is *local* to a token.

Declaring it:

```tsg
%name jsonc;
%external action_code = delimited('{', '}');
document: action_code+;
```

`tsg -o src/grammar.json` then writes `src/scanner.c` next to the JSON, and the
loader picks it up automatically (`crates/loader/src/loader.rs:2071`,
`get_scanner_path`). Nothing outside `tmp/tsg` changes and no patched generator
is needed: `grammar.json`'s `"externals"`, `render.rs`'s external-scanner glue and
the loader were already there.

The generated C is small and deliberately dumb: an `enum` in externals order
(the generator indexes `valid_symbols` by position), `create`/`destroy`,
`serialize`/`deserialize` that store nothing (no state survives a token), and
`tsg_scan_delimited`, which skips **whitespace**, requires the opening delimiter,
counts depth, and returns the token when depth reaches zero.

Three limits, all reported rather than hidden:

1. **Whitespace only.** The scanner runs *before* tree-sitter skips extras
   (`lib/src/parser.c` `ts_parser__lex`), so it must skip them itself; it cannot
   call the internal lexer, so it can only skip what it can express in
   characters. Any extra that is not whitespace — a comment, in particular — is
   named in a compile-time warning: a `{` that follows `/* … */` is not
   recognised. This is exact, not a guess: `is_whitespace_pattern` accepts `[ \t\n]+`,
   `\s+` and the parenthesised form `to_regex` emits, and rejects everything else.
2. **The region is opaque.** A `}` inside a string or comment closes it early.
   That is the *definition* of the token (Flex's `(?s:.)<.>` says the same), and
   a region with real lexical structure inside needs a hand-written scanner.
3. **No state between tokens**, hence `serialize` returns 0. If a future DSL ever
   supports a stack that outlives a token, that is where it goes.

`tsg` will not overwrite a `scanner.c` that does not begin with
`// Generated by tsg`, and it says so when an external token has no scanner at
all (that token can then never match, so every production needing it is dead).
