# What of `tsg` is worth sending upstream

Written after porting PostgreSQL 16 (700 rules, 7509 productions) to a
declarative grammar, against tree-sitter **0.28.0** plus the local patches in
`HANDOFF.md` §Patches (0001–0011). Everything below is evidence we produced
ourselves; none of it is a complaint about the language, all of it is about the
generator or the runtime.

Sorted by value to the project, not by our investment.

---

## 1. Extras after the last recognised token silently truncate the tree, exit 0

The worst kind of bug: no error, no `ERROR` node, no non-zero exit — the tree
just stops.

```
grammar: document = { region }        region = "a" | "b"
input:   { a } /*x*/ { b }
tree:    (document [0,0] - [0,5] (region [0,0] - [0,1]))
exit:    0
```

The trailing `/*x*/ { b }` is dropped. The same grammar on `/* c */ { a }`
(leading extras) gives an `ERROR` at `[0,0]` instead. Both with `extras` as bare
`PATTERN` entries.

**Ask:** confirmation, and whether "extras only between tokens" is intended.

**Caveat before filing:** we observed this through our own front end, so it
needs reduction to a plain `grammar.json` first. That reduction is maybe 20
minutes and we have not done it; do not file item 1 without it.

## 2. LALR emits duplicate designated initializers, and only `debug_assert!` notices

In LALR mode a state whose lookaheads grow is queued again and its actions
recomputed; the recomputation **appended** to `terminal_entries` /
`nonterminal_entries` instead of replacing. `ParseStateEntries::push` guarded
this with `debug_assert`, so release builds emitted both

```c
[sym] = ACTIONS(a), [sym] = ACTIONS(b)
```

for the same state. C keeps the last, so the output was right *by accident*.
PostgreSQL 16 hits it hard: **494 KB of duplicate initializers**, and
`cc -Winitializer-overrides` warns on every one of them.

Patch is small and already written and measured (replace instead of append at
both sites; make the duplicate a real `assert!`): parse tree byte-identical
before and after, `sqlite3`/`bison` fixtures unchanged, 108 generator tests
pass. This one is PR-shaped and should probably just be sent.

## 3. A flag that prints the grammar the tables are actually built from

The EBNF sugar is invisible: invented auxiliary rules, inherited precedence,
`%conflicts`, the difference between what you wrote and what got generated. For
a non-trivial grammar there is no supported way to see it, and the conflict
messages refer to rules that do not exist in your source.

We added `--dump-expanded` (≈200 lines in a new `dump_grammar.rs` plus the CLI
plumbing) which prints a bison-ish `.bnf` with an explicit marker for each
invented rule. It is what made the 16 PostgreSQL conflicts tractable — e.g. it
shows that one rule conflict comes from a choice inside a group, not from your
alternatives.

**Ask:** is such a flag wanted upstream? We will prepare it as a PR if so.

## 4. Conflicts cannot be joined to the grammar

`generate` says `Unresolved conflict for symbol sequence: …`. `--report-states-
for-rule` exists but speaks in states, and there is no way to get from one to
the other, nor to see the item sets that actually conflict. Closing this turns
"your grammar is ambiguous somewhere" into "these two productions, in this
state, on this token".

## 5. Possible runtime bug: `ts_language_table_entry` asserts on incremental reparse

`lib/src/language.c:173`:

```c
ts_assert(symbol < self->token_count);
```

We can trigger it only on **incremental** reparse of a grammar generated in
**LALR mode with a `%conflicts` whitelist** (a 16-entry one, PostgreSQL 16).
The same grammar is fine in LR(1), and a smaller curated PostgreSQL port is
fine in both. Unexplained — it may be a bad table on our side rather than a
runtime bug, but the shape (only LALR + whitelist + reuse) is suspicious enough
to mention. We have not reduced it.

## 6. `%conflicts` conflates two unrelated causes

Of the 16 conflicts in PostgreSQL 16, 4–5 are *state/factorisation* differences
and 3–5 are *precedence placement* — both fixable in the grammar, both
diagnosable from a dump. But **4 are token identity**: the productions are
identical to the version that does not conflict, and the only difference is
which lexer rule matched (`WITH_LA` vs `WITH`, `FORMAT` as
`unreserved_keyword`). Those are unfixable without lexer lookahead or an
external token, and tree-sitter cannot say that from the message. If the message
named the cause, half of the `%conflicts` entries in a big port would not be
written.

## 7. Data: once node construction dominates, the algorithm gap collapses

Measured on the same PostgreSQL input (details and caveats in
`.bench/README.md`; canonical LR(1) is never compared to LALR):

| | time | throughput |
|---|---|---|
| bison LALR, no tree | 5.8 ms | 175 MB/s |
| bison LALR + tree | 47.3 ms | 21.6 MB/s |
| tree-sitter `--lalr` | 81.5 ms | 12.5 MB/s |
| tree-sitter `--lalr`, 1-char edit | 1.58 ms | — |

Decomposition: ~7% parse loop, **~51% node construction**, ~42% residual
(lexer + subtree shuffling). The GLR-vs-LALR gap becomes ~1.06× once building
the tree dominates, i.e. the table algorithm is not where the time is.

Two things in that 51%: `ts_subtree_new_node` (`lib/src/subtree.c:504`)
`realloc`s the children array for every node — one allocation per interior node
— and leaves are stored inline only when `symbol <= UINT8_MAX`. ~800k nodes +
276k leaves per MB of input. An arena or pooled children buffer is the obvious
experiment; we have not run it.

Also: our front end compiles the whole 7509-production grammar in **9 ms**,
against **3.8 s** for `tree-sitter generate --lalr`. Grammar evaluation is not
the cost; table construction is.

## 8. Small: let the front end be plugged in

`load_grammar_file` dispatches on extension and knows only `.js` and `.json`.
One more arm, or a documented env var, and any declarative front end can be used
without forking the CLI (ours is ~150 lines: run the tool, pass its stderr
through, use stdout as the grammar). Only worth doing if item 3 lands.

---

## Not worth sending

That we prefer a declarative syntax to the JS DSL is an opinion about API taste
and the maintainer has heard it; it belongs in a blog post, not an issue. Same
for anything about cargo writing into `$HOME`.
