# SQLite SQL in `.tsg`

`grammar.tsg` is [`../sqlite3.lemonex`](../sqlite3.lemonex) — SQLite's Lemon
grammar, with the C actions removed and an integrated lexer — translated to the
declarative front end. 134 Lemon rules and 167 `%token` terminals became 132
nonterminal rules and 169 token rules in the `tokens { … }` block, plus 26
lines of translation notes. The source is still the reference; read the two files
side by side.

`translate.py` is the mechanical translation (also a record of every decision
below). `grammar.tsg` is its output. The generated `grammar.json` has 301 rules
(132 nonterminal + 169 lexical) and one `%conflicts`.

## Run it

```sh
# .tsg -> grammar.json -> parser.c -> parse all samples
./check.sh

# or manually
../target/debug/tsg grammar.tsg -o src/grammar.json
../bin/tree-sitter generate src/grammar.json
../bin/tree-sitter build --output lib/sqlite3.dylib .
../bin/tree-sitter parse --lib-path lib/sqlite3.dylib --lang-name sqlite3 sample.sql
```

`tree-sitter parse` needs the grammar to look like a grammar directory, hence
`tree-sitter.json` and `src/`. `check.sh` builds into `.check/` and checks that every `*.sql` sample parses
with no `ERROR`/`MISSING`; the checked-in samples are:

| file | what it exercises |
|---|---|
| `sample.sql` | DDL, DML, joins, aggregates, `ORDER BY` |
| `caseless.sql` | lowercase keywords from `%lexer_caseless`; both comment forms |
| `keywords.sql` | keywords used as identifiers via `%fallback ID` |
| `prec.sql` | every interesting precedence/associativity pair |
| `vtab.sql` | `%wildcard ANY` in `CREATE VIRTUAL TABLE` arguments |

## Translation rules

| Lemon / Lemonex | `.tsg` | Note |
|---|---|---|
| `%token` terminal `PLUS ::= "+".` | `PLUS: '+';` | terminals stay named tokens; rules read like the source |
| `%lexer_caseless` + `SELECT ::= "SELECT".` | `SELECT: /SELECT/i;` | keyword tokens are case-insensitive; `ID` is not |
| `%word ID` / `%fallback ID` | `%word ID;` | a keyword not valid in the current state lexes as `ID`; `CREATE TABLE select(…)` works |
| `%wildcard ANY` | `ANY: /[^\s,()]+/;` | used by `vtabargtoken` and `anylist` |
| `input ::= cmdlist.` | `input: cmdlist;` | first rule outside `tokens` is the start rule |
| empty production `foo ::= .` | `foo?` at each use | tree-sitter forbids empty non-start rules |
| left-recursive list with an empty base | add the base explicitly | e.g. `carglist`, `refargs` |
| rule precedence `[BITNOT]` | `@right(bitnot)` | explicit mark where Lemon's choice is not the default |
| `%nonassoc ON` | `%precedence "on" = ON;` | tree-sitter has no non-associative precedence |

### Precedence: first terminal vs rightmost terminal

This is the one semantic difference that needs care. Lemon gives a production
the precedence of its **leftmost terminal** that has one. yacc (and hence
`tsg`'s `%precedence` rewrite) uses the **rightmost terminal**.

The translator computes both. If they agree, the production inherits the level
from its rightmost terminal, so no mark is needed. If they differ, it writes an
explicit `@assoc(level)` — `@left(compare)`, `@right(bitnot)`, or `@on` for the
non-associative level — because a bare `@level` would lose the associativity.
`@0` suppresses a rightmost terminal that Lemon would ignore.

Examples:

```tsg
// Lemon takes LIKE_KW: level "compare" (left), tsg's rightmost terminal is
// the nonterminal likeop, so the level must be written out.
| expr likeop expr @left(compare)

// Lemon's rule mark is [BITNOT] (%right), though the rightmost terminal is
// PLUS/MINUS (level "add").
| PLUS expr @right(bitnot)
| MINUS expr @right(bitnot)

// The rightmost terminal is already the level owner, so these inherit.
| expr PLUS expr
| expr AND expr
```

The 13 levels are declared highest first, each owning its terminals:

```tsg
%precedence "on"      = ON;
%precedence right "bitnot"  = BITNOT;
%precedence left  "collate" = COLLATE;
%precedence left  "concat"  = CONCAT PTR;
%precedence left  "mul"     = STAR SLASH REM;
%precedence left  "add"     = PLUS MINUS;
%precedence left  "bit"     = BITAND BITOR LSHIFT RSHIFT;
%precedence right "escape"  = ESCAPE;
%precedence left  "rel"     = GT LE LT GE;
%precedence left  "compare" = IS MATCH LIKE_KW BETWEEN IN ISNULL NOTNULL NE EQ;
%precedence right "not"     = NOT;
%precedence left  "and"     = AND;
%precedence left  "or"      = OR;
```

`prec.sql` is the regression test: `1 + 2 * 3` groups `+` outside `*`,
`1 || 2 || 3` and `1 LIKE 2 = 3` are left-associative, `- 1 * 2` is
`(- 1) * 2`, `~ 1 + 2` is `(~ 1) + 2`, and `BETWEEN`'s `AND` stays inside the
`BETWEEN`.

### `%conflicts seltablist`

One GLR fork remains: after `FROM nm`, the parser cannot tell whether `ON`
starts the join constraint of that table or a new `seltablist` element. Lemon's
LALR automaton defers it; tree-sitter needs a conflict declaration. Removing
`%conflicts seltablist;` reproduces the `insert_cmd INTO xfullname SELECT …
FROM nm • ON` error, so it is necessary, not decorative. No other conflicts are
declared.

## Deliberate deviations from the source

1. **`QNUMBER` is omitted.** It is an internal token generated by SQLite's
   parser, not by the lexer; the source has no `QNUMBER ::= "…".` rule. The
   single `term ::= QNUMBER` production therefore has no surface spelling and is
   dropped.
2. **`ON` is not non-associative.** tree-sitter has no non-associative
   precedence, so the level is declared without `left`/`right` and `a ON b ON c`
   would parse. The source rejects it at parse time.
3. **`scanpt`/`scantok` vanish.** They are empty productions used only to
   capture token spans for C actions; with actions gone, empty rules become `?`
   at their use sites, and these two have no non-empty production left.
4. **Comments and whitespace are named extras.** `WS`, `LINE_COMMENT` and
   `BLOCK_COMMENT` are kept as `%extra` tokens (the lexer section spells all
   three), so the parse tree shows them, unlike the generated `parser.c` which
   would normally hide extras in a query.
5. **The lexer is scannerless.** All tokens are regexes or literals in priority
   order; the source's integrated lexer had the same shapes, and `%lexer_caseless`
   is expressed by `/…/i` on the keyword tokens.

## Translation notes for the next grammar

- In-rule `|` is expanded before anything else. `sqlite3.lemonex` has runs like
  `term ::= ID|INDEXED|JOIN_KW`, and Lemon's `|` binds to a single token here.
- Nullability is a fixpoint, not a one-pass property. A rule can become nullable
  through a chain of empty productions; make a new set of nullable-only rules
  and re-run until stable, or a use site keeps an optional marker that later
  disappears (`scanpt?` was this bug).
- The base of a pure left-recursive list is not always the first production.
  If all productions are left-recursive, add the tail of each production as a
  base (`carglist ::= carglist ccons.` becomes `carglist: carglist ccons | ccons;`).
- A nullable symbol used as the base of a list needs a hand-written EBNF shape,
  not the generic fold: `vtabarg` is `vtabargtoken+`, `vtabarglist` is
  `vtabarg (COMMA vtabarg)*`, and `anylist` is `(ANY | LP anylist? RP)+`.
- Precedence and associativity are separate: an explicit `@level` is `PREC`, not
  `PREC_LEFT`/`PREC_RIGHT`. Write `@left(level)`/`@right(level)` whenever the
  level is associated.
- Generate once without conflicts, then add only the conflicts the generator
  names. An unnecessary `%conflicts expr;` produces an "unnecessary conflicts"
  warning after the associativity fix.

## Verification

Current output (macOS arm64, debug build):

```
$ ./check.sh
ok   caseless   6 lines
ok   keywords   4 lines
ok   prec       7 lines
ok   sample     6 lines
ok   vtab       2 lines
ok   301 rules, 1688390 parser.c bytes, 5 samples
```

`../selfhost.sh` also parses `grammar.tsg` with the parser built from
`examples/tsg.tsg`, so this file is inside the dogfooding set; adding it changed
the self-host count to 41 files / 9702 lines.

Sizes: `grammar.tsg` 18 024 B, `grammar.json` 214 328 B, `parser.c` 1 688 390 B,
`node-types.json` 77 494 B. `tree-sitter generate` reports no diagnostics.
