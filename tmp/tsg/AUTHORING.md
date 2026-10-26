# Authoring friendliness — what `tree-sitter` still gets wrong

The three dog-foods (`DOGFOOD.md`) are organized by *port pain*: what a
translator could not express.  This file is organized by the criterion that
matters more, and that the notes never stated: **a grammar author (human or
translator) must never be left to guess.**  Every item below was exercised
against `bin/tree-sitter` (patched 0.28.0, commits `0001`–`0009`) on
2025-10-25, with the exact probe shown.

Status of the three dog-foods' findings: the lexer is no longer a black box
(`0009`, `--report-lex-states`), and rules that can never match are reported
(`0008`).  The classes below are what is left.

## 1. Silent acceptance (worst: a typo produces a working, wrong parser)

**Fixed in `93250d2`** (`deny_unknown_fields` on `GrammarJSON` and the tagged
rule enum): the error names the key and lists the valid ones —

```
unknown field `extra`, expected one of `name`, `rules`, `precedences`, … , `reserved`
unknown field `foo`, expected `content`
```

Before that, a hand-written or machine-emitted `grammar.json` was deserialized
without `deny_unknown_fields`, so a misspelled *key* was dropped on the floor:

| input | `generate` |
|---|---|
| `"extra": [...]` (typo for `extras`) | **rc 0, no output**, extras ignored |
| `"extrass": [...]`, plus a bogus rule-level key | **rc 0, silent** |
| `{"type":"REPEAT","content":…,"foo":1}` (unknown key in a node) | **rc 0, silent** |
| `{"type":"PATTERN","val":"\\s+"}` (typo in a *field*) | rc 1 — see §2 |
| `{"type":"REPEATX"}` | rc 1 — see §2 |

`"extra"`, `"precedences"`, `"inline"` vs `"inlined"`, `"word"` vs
`"word_token"` all pass.  A field typo is caught only because the field is then
missing.

Same class, still silent, and never mentioned in any report:

- ~~a `conflicts` entry that matches no unresolved conflict~~ — already reported
  by the series as `Diagnostic::UnnecessaryConflicts` (`0007`), so the
  `DOGFOOD.md` claim that it is "accepted forever" was stale;
- an `inline` entry, a `supertypes` entry, a `reserved` set, an external token
  that nothing uses;
- a token that no rule references at all (Bison: "useless token");
- a token whose pattern can never be produced because another token always
  wins ("unreachable token", distinct from the shadowed transitions of `0009`).

## 2. Real errors hide their cause — retracted

I wrote that the cause chain was hidden behind `--log`. It is not: I had piped
the output through `head -2` and lost it. With the full output, `anyhow`'s chain
prints at default verbosity:

```
$ tree-sitter generate grammar.json
Error: Error when generating parser

Caused by:
    unknown variant `REPEATX`, expected one of `ALIAS`, `BLANK`, `STRING`, …
```

No change needed.  Left here because the mistake is instructive: "no hidden
errors" claims must be measured with the whole output, not a truncated pipe.

## 3. The lexer report is opt-in, and extras have no names

`0009` explains the `/* c */` bug, but only if you already suspect it.  Two
concrete gaps:

- a *separator* shadow (an extra losing to a token) is almost always a bug and
  should be a **warning at generate time**, deduplicated per (token, extra), not
  an opt-in report.  Token-vs-token shadows are usually benign and should stay
  report-only;
- unnamed extras (`extras: [/\s+/]`, the common case) have no symbol, so the
  report can only say "the extras".  Naming them needs the rule pool in
  `build_lex_table`.

Ground truth to keep: **at equal matched length a token beats an extra, and this
is not configurable.**  The author's only lever is to ensure no token is a
prefix of an extra.

## 4. Constructs with no clear way to say it (the gymnastics)

From the port gap list (`HANDOFF.md`), re-ranked by how much grammar distortion
each one forces:

1. **No empty productions.**  Every `%empty` alternative is inlined at its use
   sites as `x?`: `simple_select` becomes 328 alternatives, the grammar 8000
   productions, canonical LR(1) 354k states, and the `?` distribution creates
   *derived* shift/reduce conflicts Bison never sees.  An explicit nullable node
   would remove all of it.  This is the one worth designing for.
2. **No `%nonassoc`.**  A chained `a < b < c` cannot be rejected.
3. **No lexer lookahead.**  `WITH_LA`/`NULLS_LA`/`NOT_LA` collapse onto their
   base token; that collapse *is* postgres16's one genuine ambiguity
   (`utility_option_name` vs `unreserved_keyword` on `FORMAT`).
4. **No `%fallback`** (1970 occurrences in the 358-file corpus).
5. **No regex assertions** (`^`, `$`): "Assertions are not supported".  Flex `^`
   anchors appear in 37 corpus files; the only workaround is an external
   scanner.
6. **No extras/token priority control** — the `/*` rule seen from the author's
   side (§3): you cannot ask for the longest match across tokens and extras.
7. **`--lalr` / `--max-states` are CLI-only** — a grammar cannot declare its own
   table shape, so every consumer needs a wrapper, and the flag silently changes
   every report (`188` vs `470` states for one rule).

## 5. Reports that cannot be joined to anything

- conflicts print symbol sequences and **no state id**, so conflict →
  `--report-states-for-rule` does not join.  The other half of the join now
  exists: `generate --dump-expanded` writes the grammar the table is built
  from (`{rule}_repeatN` aux rules, inlined hidden variables, precedence on
  individual steps), so the rule name in a conflict leads to the productions
  that actually conflict.  Measured on the Postgres port: 1118 written rules
  become 727 variables and 6965 productions, 99 variables invented by
  repetition;
- `0009` indexes lex states by minimised ids; whether they equal the
  `ts_lex_modes` numbers in `parser.c` (and the `lex_internal state:N` of
  `parse -d`) is **not yet verified** — if they differ, the report cannot be
  cross-read with a parse trace, which is most of its value;
- parse states have two unrelated numbering schemes (`state index`, `state id`);
- extras are invisible in `--report-states-for-rule` (token lookaheads only);
- no size attribution: nothing says which rule or state accounts for
  postgres16's 25 MB `parser.c`.

## 6. The front end should say it first

`tsg` and `g2tsg` emit `grammar.json` and then the author runs `generate` to
find out what is wrong.  Everything in §1, §2 and the rule-reachability check of
`0008` is computable in `tsg` itself, at the line of the `.tsg`, and is not
computed there — and `tsg` does not validate a `/…/` body (`[\x0000-\x001F]`
passes `tsg`, fails `regex-syntax` two steps later).

## Order I would do this in

Done in `93250d2`, on top of `0009` (no change to generated tables — `parser.c`
md5 identical with and without every report flag, on `mx` and `bc`):

1. ~~warn on unknown keys in `grammar.json` / `grammar.js` nodes~~ — now an
   error that names the key;
2. ~~promote separator shadows to a warning (deduplicated, capped)~~ — done;
3. ~~warn on dead `conflicts`~~ — already existed (`UnnecessaryConflicts`);
4. print the error cause by default — retracted, it already does (§2).

Still open, in order of value:

5. verify (and fix or document) the lex-state id join; add state ids to
   conflicts;
6. compute reachability/productivity in `tsg` and validate regex bodies there;
7. detect "unreachable token" from the lex table;
8. size attribution per rule;
9. an explicit nullable construction — the only item that changes what grammars
   can express, so it needs its own design round.

## Definition of green (after any of this)

```
cargo test -p tree-sitter-generate            # 104, from /Volumes/RamDisk/tree-sitter
cargo build && cargo test                     # 80, from tmp/tsg
./selfhost.sh ./diff.sh ./e2e.sh ./roundtrip.sh --parsers
./sqlite3/check.sh ./bison_flex/check.sh ./postgres16/check.sh ./antlr/check.sh
md5 of parser.c identical with and without the new flags
```

Toolchain: `bin/tree-sitter` is the **patched** build (release, stripped, 8.4 MB)
of `/Volumes/RamDisk/tree-sitter` at `0001`–`0009`; the gates default to it
(`TS=` overrides).  `postgres16` and `bc` need `--lalr` and therefore cannot run
on stock 0.28.0.

## 9. Explicit start rule — done

The start rule used to be positional: `InputGrammar.variables`
(`grammars.rs:41`) is in JSON order and everything downstream took
`variables.first()` as the start (`parse_grammar.rs:151`, the "used" DFS in
`normalize`).  `"start": "name"` needed no new field threaded through the
pipeline: the named rule is **promoted to `variables[0]`** when the JSON is read,
so nothing downstream changed.

What shipped:

1. `parse_grammar.rs`: `#[serde(default)] start: Option<String>` on
   `GrammarJSON`; after the rules map becomes `variables`, the named rule is
   rotated to the front.  Two errors: `start` names no rule, and `start` names a
   token.
2. `tsg`: `%start name;` → `"start"`.  The AST keeps the author's rule order
   (the directive is the reason order no longer decides the entry point), and
   `compile` validates the same two cases at the directive's span.  A grammar
   without `%start` emits no `"start"` key, so every existing `grammar.json` is
   byte-identical.  `--from-json` prints `%start` whenever the key is present,
   which is what makes the round trip order-preserving; `--fmt` prints it too.
3. `g2tsg`: Bison's `%start` is passed through as the directive, and the
   comment says which of the two cases produced the file (`%start` declared, or
   Bison's default of first-rule-wins).  `postgres16/translate.py` emits the same
   two lines, because that gate compares its output against `g2tsg`'s.

Regenerated artifacts: `postgres16/grammar.tsg`, `bison/grammar.tsg`,
`bc/grammar.tsg` (each differs only by the `%start` line and the section
comment).  Their `grammar.json`/`parser.c` are unchanged, because in every one
the declared start symbol is also the first rule.

Corpus reality (measured before the work): 241 of 358 `.g` files declare
`%start` and in every one it is also the first rule, so this unblocks nothing
today.  Its value is that rule order stops being load-bearing and invisible.
