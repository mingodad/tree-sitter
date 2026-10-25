# `generate` on grammars that were written for yacc/bison

Eight commits, `master..perf/large-grammars`, none of them touching the parser
runtime or the generated-table format. They are about one thing: a grammar of
real size (a port of [postgres16.g], 705 rules, 489 tokens) could not be
generated at all — `generate` ran for minutes, allocated most of a gigabyte and
never finished — and when it does generate, the diagnostics it gives a grammar
author are one-conflict-per-run.

Measured on the PostgreSQL 16 grammar (1.0 GiB RAM disk, M-series laptop,
`--release`):

| | before | after |
|---|---|---|
| canonical LR(1) | never finishes (>90 s killed, 942 MB, 0 output) | 4.1 s / 87 MB, 1378 states (`--lalr`) |
| `build_lex_table` | >25 s | 0.45 s |
| lexer states | 14133 | 3400 |
| `token_conflicts` | 12.3 s | 3.2 s |
| triaging the grammar's conflicts | 12 `generate` runs | one 0.4 s run |

Nothing here is specific to that grammar: every change is on the path that all
grammars take, and every one of them is checked against the existing corpus
(`roundtrip.sh --parsers`: 92 grammars, `parser.c` byte-identical) plus the 16
test-fixture grammars (`e2e.sh`) and the 14 DSL-equivalence grammars
(`diff.sh`).

## The commits

**`b829c6b` perf: dedupe NFA state insertion with generation stamps.**
`NfaCursor::add_states` did a linear `contains` for every state it added, which
is quadratic in the state set — and the state sets here have thousands of
entries. Replaced with a `Vec<u32>` of generation stamps and one
`sort_unstable()` + `dedup()` per call.

**`a3e4498` perf: expand the extras pattern once instead of once per token.**
This is the big one. `LexicalGrammar::nfa` is the union of `sep* t_i` over all
tokens, i.e. the separator pattern is *copied into every token's start block*.
The lex table only ever intersects this NFA with the set of tokens that are
live in the current state, so a second NFA is built for the lex table with the
separator pattern expanded once, in a shared block that exits into a binary
split `hub` over the token bodies (`shared_nfa` / `shared_owner` /
`shared_start`). `sep* ⋃ t_i ≡ ⋃ sep* t_i`, so the result is identical; states
that belong to no single token are marked with `shared_owner == u32::MAX` and
are always kept when the live-set filter runs. `TokenConflictMap` walks two
automata in lockstep and keeps using the per-token NFA.

**`22a5636` perf: build the token conflict matrix in parallel.**
Every token pair is analysed independently, so rows are computed in
`std::thread::scope` chunks. `pair_workers` decides the split and stays at one
thread for small grammars, where spawning costs more than the work.

**`69142a0` fix: rank an undeclared precedence below a declared level.**
`compare_precedence` only knew how to compare `Precedence::None` against an
*integer* implicit precedence; against a named `%precedence` level it returned
`Equal`, so the conflict fell through to associativity, and a `%right` level
then prefers the shift — the opposite of Bison, where an undeclared token has
implicit precedence lower than any declared level. Ported grammars use named
levels exactly where Bison does, so this silently inverted their precedence
decisions (`UPDATE foo SET …` was the visible symptom).

**`b363428` feat: add `--lalr` and `--max-states`, and report progress.**
Canonical LR(1) states are what make large grammars infeasible: the state count
grows with the number of distinct lookahead sets per kernel item. For
postgres16 the canonical table needs 354k states, versus 1378 in LALR(1), which
is what yacc/bison build. `--lalr` identifies a state by its kernel items only
(`ParseItemSetCore`); when a new kernel core matches an existing state, the
lookaheads are unioned into it and the state is re-queued with its action tables
reset. Lookahead sets only grow, so this reaches a fixpoint. `--lalr` is
opt-in: it can report conflicts that canonical LR(1) resolves away, so it stays
a porting tool rather than a default. `ParseStateInfo::item_sets_by_ids`
becomes a `Vec` indexed by state id (LALR looks states up by id, not by full
item set). `--max-states N` turns a state explosion into an error instead of an
OOM, and a progress line every two seconds (terminal or debug log only) tells
"working" from "stuck".

**`7e75cdb` chore: report generation timings through the `log` crate.**
The stage and lexer timings were behind `TS_DEBUG_TIMING` /
`TS_DEBUG_CONFLICTS`, which duplicate `generate --log` — the same debug channel
the rest of the generator writes to. The conflict-matrix thread count moves into
`pair_workers`, and the `token_conflicts` stage reports it next to the token
count. The parse-state heartbeat was the last unconditional `eprintln!`; it now
prints only to a terminal (or with debug logging on).

**`01b1463` feat: report every unresolved conflict, not just the first.**
`handle_conflict` returned the first conflict as an error, so a grammar with n
independent conflicts took n `generate` runs to triage, one pair per run. Now a
conflict that the grammar does not declare is recorded and building continues —
the tables are discarded either way, and carrying on is what reaches the states
behind the one that conflicted. The state keeps both actions as if the conflict
had been declared, which is exactly what the declared-conflict path does.
Conflicts are deduplicated by symbol set, so a pair is reported once however
many states run into it. The single-conflict report is unchanged (item sets,
interpretations, and the precedence or `conflicts` entry that resolves it);
several are summarized as one line each, with the `conflicts` entry it needs,
and `--json-summary` still has the full item sets. For postgres16 that turned 12
sequential runs into one run that lists all 12 conflicts.

**`49aa685` feat: report rules that can never appear in a tree.**
Two silent failure modes, both of which leave a parser that accepts less than
the grammar says. A rule that nothing reaches from the start rule is dropped by
`InputGrammar::normalize` (dropping it is right — that is how a grammar with
leftover rules still generates), and it is now reported where it happens. A
rule whose every production needs a rule that can never match derives no
sentence, so no state can ever reduce to it — the classic missing base case
(`a: a b;` with no `a: b;`) or a rule that needs a token nothing lexes. That one
generates a parser happily and the rule then never appears in a tree; Bison
calls the same input "nonterminal useless in grammar". The check is a worklist
over the flattened productions, linear in the number of steps (a rule becomes
productive when one of its productions has no unproductive steps left; tokens
and external tokens always count as productive), and it costs nothing on a
healthy grammar (postgres16: 4.01 s / 80 MB, unchanged). Both are
`Diagnostic`s, so they print as warnings, reach `--json-summary`, and appear
exactly where a translator's most likely bug — a lost production — would
otherwise go unnoticed. On the 92-grammar corpus, 10 grammars gain a
`not reachable` warning and every one of them is a genuinely dead rule; the
only grammar that gains `cannot match anything` is the
`conflicting_precedence` fixture, which really has no base case.

**`1432574` feat: report the lexer states and the transitions they drop.**
The report that would have saved the last three dog-foods. A lexer state is
built per *set* of tokens valid in the parse states that start in it, so a
transition only some paths through the state can take is dropped — and one rule
decides those drops: where a token completes, another token's longer match is
not viable, and a separator transition (an extra) loses to a completed token at
equal precedence. That last clause is the whole of "an extra loses to a token
that is a prefix of it": in a grammar with `Slash: /\//` and a `/* … */` extra,
`1 /* c */` lexes the `/` as division, because `Slash` completes on `/` and the
transition into the comment is dropped. Nothing today shows that:
`--report-states-for-rule` shows the parser, and the generated C shows only the
transitions that survived.

The drops are collected while the lex table is built (they were already being
skipped, just not remembered), together with each lexer state's valid tokens and
the parse states that start in it, and `--report-lex-states` prints them: `-`
gives one line per dropped transition with the number of lexer states and parse
states it affects, extras first; `*` gives every lexer state a parse state
starts in; `N` gives one. For bc (22 rules, 34 tokens) the whole report is 15
lines and two of them are the grammar's two real lexer bugs:

```
extras: bare patterns (see `extras` in grammar.json)

a token shadows an extra:
  `-`  ->  the extras (on '-')  1 lex state, 1 parse state
  `/`  ->  the extras (on '*')  2 lex states, 2 parse states
```

The report is opt-in, costs one `Vec` while the table is built, and leaves the
generated tables untouched (bc's `parser.c` is byte-identical with and without
it). Unnamed extras — the common `extras: [/_\s+/]` — have no symbol to print,
so the section says `bare patterns` and the rows say `the extras`; naming them
would need the rule pool in `build_lex_table`, which is not passed there.

**`d29e0d2` feat: reject unknown grammar keys, warn when an extra is shadowed.**
Two ways the author was left to guess.  `grammar.json` was deserialized without
`deny_unknown_fields`, so a misspelled key was dropped on the floor: `"extra"`
for `"extras"`, an unknown key inside a rule node — both generated a parser from
the remaining fields and exited 0.  A misspelled *field* was caught only by
accident, because the field was then missing.  Both the top-level grammar and
the tagged rule enum now deny unknown fields, so the error names the key and
lists the ones that exist (`unknown field `extra`, expected one of `name`,
`rules`, … , `reserved``).  Two known keys stay accepted: `$schema`, which
`tree-sitter generate` itself writes (16 of the 92-grammar corpus), and
`inherits`, which the JS DSL resolves before generating but the generator cannot
— that one now warns instead of being ignored, because a file that still says it
is generating a partial grammar.

Second, the lexer report of the previous commit explains why `1 /* c */` lexes
`/` as division, but a report has to be asked for, and an author who does not
already suspect the cause will not ask.  A token that completes on the first
character of an extra now warns at generate time, deduplicated per
(token, characters, extra), capped at ten rows:

```
warning: a token that completes at the start of an extra hides that extra:
  `/` completes, so the transition on '*' into an extra is never taken
  at equal length a token beats an extra: no token may be a prefix of an extra
  that must win, so rename the token or change the extra
```

Token-vs-token shadows stay report-only: they are usually benign, and warning on
them would bury the separator case.

## API changes

- `build_parse_table(…, lalr: bool, max_states: Option<usize>, diagnostics)`,
  `build_tables(…, optimizations, max_states, diagnostics)`,
  `generate_parser_in_directory(…, max_states, diagnostics)`.
- `generate_parser_for_grammar` keeps its signature (passes `None`).
- `OptLevel::Lalr = 1 << 1`; `ParseTableBuilderError::TooManyStates { max }`
  and `::Conflicts(Vec<ConflictError>)`.
- `LexicalGrammar` gains `shared_nfa`, `shared_owner`, `shared_start`;
  `ParseStateInfo::item_sets_by_ids` is now a `Vec<ParseItemSet>` indexed by
  state id.
- `token_conflicts::pair_workers(usize) -> usize` is `pub(crate)`.
- `Diagnostic::UnreachableRule { name }` and `::UnproductiveRule { name }`.
- `Diagnostic::ShadowedExtras(Box<[ShadowedExtra]>)`, and
  `parse_grammar` rejects unknown keys (`$schema` and `inherits` excepted,
  the latter with a warning).
- `build_tables(…, report_lex_states: Option<&str>, …)`,
  `generate_parser_in_directory(…, report_lex_states, …)`;
  `LexTables` gains `report`, and `minimize_lex_table`/`sort_states` return the
  id mapping they were computing anyway.

## Tests

`cargo test -p tree-sitter-generate`: 104 passed (98 before, 6 new):

- `test_separators_are_expanded_once` and
  `test_shared_nfa_has_no_separators_without_extras` — the shared NFA accepts
  exactly the strings the per-token NFA accepts, under the same live-token-set
  filter, for every case in the existing separator suite;
- `test_undeclared_precedence_is_lower_than_a_named_level`;
- `test_every_unresolved_conflict_is_reported_in_one_run`,
  `test_a_declared_conflict_is_not_reported`,
  `test_a_single_unresolved_conflict_keeps_the_detailed_report`;
- `test_a_rule_that_nothing_reaches_is_reported`,
  `test_a_rule_that_derives_nothing_is_reported`,
  `test_a_healthy_grammar_has_no_unreachable_or_unproductive_diagnostics`.

Each commit compiles standalone (`cargo check --all-targets`) and is
`cargo +nightly fmt --check`-clean (the repo's `rustfmt.toml` uses unstable
options, so stable silently checks nothing). `git am` of the exported patches
onto `master` reproduces the branch tree exactly.

## Not in this series

- `token_conflicts` is still 80 % of the 4 s: 128k token pairs, each walked over
  the per-token-copy NFAs. A prefilter that skips pairs whose bodies share no
  prefix, or a body-only NFA for that walk, would cut most of it — but it
  changes which pairs are reported as conflicting, so it needs its own
  byte-identical round.
- Unresolved conflicts still do not report the *state* that has them, so the
  summary line names the symbol sequence instead. `--json-summary` has the item
  sets; a state id and kernel in the human-readable message would be better.
- `%nonassoc` has no equivalent in tree-sitter, and grammar-level lookahead
  (`_LA` tokens) still has to be modelled in the grammar.
- The new diagnostics report names, not positions: they are found after JSON is
  parsed and the grammar's spans are gone. A warning that pointed at the rule
  in `grammar.js` would need spans carried through `normalize`/flattening.
- `--lalr` could be picked automatically when canonical LR(1) exceeds
  `--max-states`, but silently changing the table shape is worse than asking.

[postgres16.g]: https://github.com/laurivosandi/grammars/blob/master/postgres/postgres16.g
