# Roadmap: the shortest path to "easy to write usable grammars"

This file supersedes the *ordering* in `HANDOFF.md` §"Open items" for everything
about authoring.  It exists because the earlier plan was organized by port pain
and then by compatibility, and the criterion that actually matters is narrower:

> An author writes what the language is, and never works around the tool.

Two constraints were explicitly **dropped** when this file was written, so do not
re-litigate them:

- **JS-DSL parity.**  New `grammar.json` keys may be emitted only by `.tsg`.
  `dsl.js` may follow later, or never.  Precedent: `externals`, `supertypes`,
  `reserved`, `word`, `conflicts` all landed in the format first.
- **Compatibility with stock `tree-sitter`.**  The toolchain is `tsg` + the
  patched generator in `/Volumes/RamDisk/tree-sitter`.  `parser.c` must still be
  ABI-15-clean as long as nothing in `lib/` changes, but that is a *by-product*
  of the work below, not a requirement on it.

One rule survives, because it is not about compatibility but about honesty:
**the tool must never emit a parser that differs from what the source says.**  A
loss is reported or an error, never silent.

## What "gymnastics" are being removed

Measured on this tree; each row is a thing a grammar author has to do today for
no reason related to their language.

| gymnastics | why it is necessary today | evidence |
|---|---|---|
| write `x?` at every use site instead of `%empty` | an empty rule is rejected: `FlattenGrammarError::EmptyString` | `crates/generate/src/prepare_grammar/flatten_grammar.rs:735`; postgres16 became 328 alternatives / 8 000 productions / 354k states |
| hand-write `src/scanner.c` for start conditions, `reject()`, trailing context | the declarative grammar has no lexer section, only `%external NAME = delimited(…)` | 52/358 corpus grammars use `<<>` pushdown, 42/358 use `reject()` |
| reorder tokens or add `token(prec(…))` so keywords beat identifiers | `%fallback` is unimplemented | 17/358 files, 1 970 occurrences |
| patch the grammar shape by hand so a body may open with `|` | there is no nullable `rhs` | `bison/patch.py` (one hand edit left in the bison gate) |
| map `state 470` back to a rule by hand | conflict messages carry state ids, not rule names | `AUTHORING.md` §1, §5; `DOGFOOD.md` dog-food #3 |
| read one conflict per `generate` run, and get no warning for an unused token | reports are opt-in text, not machine-readable | `AUTHORING.md` §1/§3 |

## Work, in order

Each item names what to change, the evidence that it matters, and how it is
accepted.  Sizes are honest: 1 is a day, 5 is a week or more.

### 1. `%fallback` in `g2tsg`  (translator only, ~1 day)

`%fallback IDENT "if" …` means "the keyword string wins when it matches".  That
is lexical priority, and `.tsg` already encodes it: earlier rules in a `tokens`
block win at equal matched length (`SYNTAX.md` §1).  So the translator has to
order the fallback targets before the fallback token and emit nothing new.

- Where: `bison_flex/translate/src/{cst,ir,emit}.rs`.
- Accept: the 17 corpus files that fail on `%fallback` translate; re-run the
  358-file sweep (`coverage.py`) and record the new count — baseline **288**.
  Also in the same pass: turn the 23 silently swallowed constructs and the
  `%expect` / mid-rule-action / `%type` cases into reported notes.
- Gate: `bison_flex/check.sh`, plus the sweep number in `HANDOFF.md`.
- **Done**: 288 → **301** translate, 66 → 52 rejected, 4 → 5 invalid (17 files
  no longer stop at `%fallback`; 13 go on to hit the nullable-mid-body gap in
  item 3, 3 the dead-token gap, and `sablecc3x.g` translates to a `.tsg` that
  `tsg` rejects because of a `.` in a rule name).  The notes half measured
  differently than this file assumed: `%union` / `%type` / `%expect` /
  `%token_*` are *refused* with a position by the metagrammar, not swallowed, so
  nothing silent is left to report there.

### 2. A real lexer section, compiled to `scanner.c`  (~1 week)

The Flex half of the corpus, and the one feature that changes what an author can
write.  `%external body = delimited('{', '}')` is the first proof of the
mechanism; the general form is a `%lexer { … }` block with named start
conditions, patterns, and the action shapes `{ }`, `<<>`, `<>`, `reject()`,
`return TOKEN`:

```tsg
%lexer {
  INITIAL       { [a-z]+ -> IDENT;  "{" -> open(BRACE); }
  <BRACE>       { "{"<<>;  "}""<>BRACE; }
}
```

Because Flex's state is per-branch and must survive backtracking, it cannot be a
generated table (`lex_modes[state].lex_state` is one value, no stack): it is
compiled to an external scanner, whose `serialize`/`deserialize` payload *is*
stored in the subtree.  That gives a genuine stack, `valid_symbols` (what the
parse expects, which the internal lexer never sees), and `advance` + `mark_end`
for arbitrary lookahead.  One lexer, many states, every token external — the
Flex model, and the only one that compiles to ABI-15 code.

- Where: new `src/lexer_dsl.rs` in `tsg` (parse + compile to C), extend
  `src/scanner.rs`, keep `delimited(…)` as sugar for the balanced case.
- Accept: `bison.g`'s three start conditions (`RULES_ST`, `EPILOGUE_ST`,
  `ACTION_CODE_ST`) become one `%lexer` block with no hand-written C, and
  `EPILOGUE` — today dropped, "not a balanced region" — becomes expressible.
  `carbon-lang.y` and `actions.y` still parse with 0 `ERROR`.
- Gate: `bison/check.sh` grows an `EPILOGUE` count; `bin/g2tsg` learns to emit
  `%lexer` instead of `%external … = delimited(…)`.

### 3. Nullable rules  (~3 days, riskiest)

Stop rejecting `BLANK`.  The tables already support it (`Reduce { child_count:
0 }`), so this is `flatten_grammar` plus a zero-width reduce that must make
progress.  Removes the `x?` inlining explosion and the `bison/patch.py` hack.

- Where: `crates/generate/src/prepare_grammar/flatten_grammar.rs`, the reduce
  path in `lib/src/parser.c` (read-only until the tests say otherwise), and
  `tsg`'s `nullable.rs` reporting.
- Accept: a grammar with `rhs: | rhs "|" ids;` round-trips through
  `tsg`/`generate`, and postgres16 loses the flattening explosion (state count
  drops by an order of magnitude; record it).
- Risk: an ε-cycle (`a: a?`) can loop at runtime.  Detect and reject it in
  `tsg` *and* in the generator; a hang is the worst possible failure mode.

### 4. Reports as JSON, then the annotated dump  (~3 days)

The generator already computes everything; it prints it as `info!`-level text.
Add `generate --report json` (per-rule state counts, conflicts with **rule
names**, lex states with named extras and unreachable tokens) and let `tsg`
render it as comments attached to the item that owns them — the annotated-`.tsg`
idea in `HANDOFF.md` §"`%external NAME = delimited(open, close)`" and the later
discussion.  `tsg --annotate` alone (tier A: what `tsg` already knows — token
priority/overlap, unused tokens, resolved precedences) needs no generator change
and is the first thing to ship.

- Accept: `tsg annotate | tsg --diff original` says *same* for all 92 corpus
  grammars (comments cannot change a grammar), and `--fmt` of the annotated file
  equals `--fmt` of the original.
- Note: annotation is a **sidecar**; spans are front-end metadata and do not go
  into `grammar.json`.

### 5. Small sugar, after the above  (each < 1 day)

- `%nonassoc`: no shift at a conflicting entry.  The tables have `Recover`, not
  an error action (`crates/generate/src/tables.rs:16`), so document that the
  result is an `ERROR` node via recovery, not a rejected parse.
- Keyword extraction (`%keyword`), if `%fallback` (item 1) is not enough.
- `%start` front ends — **done** (`tsg` emits `"start"`, `--from-json` prints
  `%start`, `g2tsg` passes Bison's `%start` through; the generator half is patch
  `0011`).  Measured before it was written: 241 of 358 corpus `.g` files declare
  `%start`, and in every one it was also the first rule — so it unblocks nothing
  by itself.  It is still worth its ~4 hours because it stops rule *order* from
  being load-bearing and invisible: a `tokens` block can now be moved for token
  priority without silently changing the entry point, and Bison's declaration
  survives the translation instead of being re-derived from position.

## Parked, with the measurement that would revive it

- **ABI 16 / a lexer-state stack in the tables.**  Only worth it if item 2's
  scanner is too slow: benchmark a generated `%lexer` against the internal DFA on
  postgres16.  `--abi N` already stamps `LANGUAGE_VERSION`
  (`render.rs:398`); `MIN_COMPATIBLE_LANGUAGE_VERSION` must stay 13 so old
  grammars keep working.  Costs: `ts_parser_set_language` rejects the grammar on
  every deployed host runtime (`lib/src/parser.c:2043`).
- **In-process integration** (`tree-sitter generate grammar.tsg`): designed in
  `INTEGRATION.md` Level 1/1.5, ~40 lines + a crate move.  Blocked on the front
  end settling, not on feasibility.  Revisit after item 2.
- **Annotated dump from `grammar.json` losing source comments**: comes free with
  item 4 if `emit.rs` learns to re-insert interior comments.
- **`^`/`$` anchors, `sym(...)`, `%include`**: no measured demand in the corpus.

## Where things stand when this file was written

- HEAD `1037cc5` + the annotated-dump discussion; single linear branch.
- `cargo test` (in this directory): **88 passed**.  Translator tests: **23**.
- All gates green: `bison` (349 rules, 1 506 917 B, `carbon-lang.y` 4 365 nodes,
  `actions.y` 110 nodes, 6 `ACTION_CODE`, 0 ERROR), `bison_flex`, `postgres16`
  (1195 rules), `antlr` (78), `sqlite3` (5 samples), `selfhost`, `diff` 14/14,
  `e2e` 16/16, `roundtrip --parsers` 92/92.
- Corpus: 358 `.g`, **288 translate**.  Failure classes: 17 `%fallback`, 17
  start-condition/C-action, 10 nullable-mid-body, 7 one-off.
- Build recipes, disk rules and provenance: `HANDOFF.md` §"Toolchain
  provenance" and §"Constraints (hard)".  Build `tsg` with
  `cargo build --offline` (0.7 s, 34 M); the patched CLI is `bin/tree-sitter`
  and does not need rebuilding for items 1, 4-tier-A or 5.
- Read `SYNTAX.md` §1 (the construct table) before adding syntax, and
  `AUTHORING.md` before adding a diagnostic.
