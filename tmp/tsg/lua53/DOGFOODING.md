# Dogfooding report: Lua 5.3, Lemon → `.tsg`

The job: take [`lua53.lemonex`](../lua53.lemonex) — a 336-line Lemon grammar with an
integrated lexer (185 productions, 56 terminals, two Flex start conditions) — and
express it as `grammar.tsg`, then run it through the pipeline we are proposing to
integrate (`tsg` → `tree-sitter generate` → `parser.c`).

Result: [`grammar.tsg`](./grammar.tsg), 142 lines of grammar plus 52 lines of
commentary, compiles to a `parser.c` that parses the source file's own test
program and two extra stress files (`sample.lua`, `tricky.lua`, `prec.lua`) with
exit 0, and is accepted by `tree-sitter generate` with **no diagnostics at all**
(the one `%conflicts` it needs is genuinely necessary — removing it reproduces
the conflict).

The point of this document is not the grammar. It is what the translation
*needed* and did not have. Everything below is backed by a file in this
directory.

---

## 0. The four failures, in the order they happened

| # | Where it was caught | Failure | Now |
|---|---|---|---|
| 1 | `tree-sitter generate` (no position) | `Undeclared precedence 'call' in rule 'stat'` — `%precedence call …` declares a *rule reference*, not a precedence *name* | **`tsg` now rejects it**, at the directive, with a paste-ready fix |
| 2 | `tree-sitter generate` (no position) | `The rule 'block' matches the empty string` — legal Lua (`do end`), illegal tree-sitter | `tsg` could pre-empt it (nullability); today only the elaborator explains it |
| 3 | `tree-sitter generate` (no position) | `Unresolved conflict … stat • '('` — the `[LPAREN]` rule-precedence idiom from the source | elaborator shows both rules; fix is `prec.right('call', …)` |
| 4 | `tree-sitter parse`, **silently** | `x = 1 y` parsed with exit 0 and a zero-width `STRING` node | upstream bug; workaround documented in §2 |

Failures 1–3 are the integration thesis, measured. Failure 4 is new: dogfooding
found a silent misparse in tree-sitter itself.

---

## 1. Bugs in `tsg` found by this exercise (fixed here)

### 1.1 `%precedence` quoting is semantic, and nothing said so

`%precedence call pow unary …;` is the obvious way to port a Lemon precedence
block. It compiles, produces valid JSON — and means something entirely different
from `%precedence "call" "pow" "unary" …;`:

```json
"precedences": [[{"type": "SYMBOL", "name": "call"}, …]]   // bare  = rule reference
"precedences": [[{"type": "STRING", "value": "call"}, …]]  // quoted = precedence name
```

`prec(call, …)` can only refer to the second form, so every use is undeclared.
`tsg` accepted this and handed the generator a grammar it would reject. That is
exactly the failure mode a declarative front end exists to remove, and the front
end *had both ends in scope*.

Fixed in `src/compile.rs` (`collect_precedence_names` + the directive-level
report). One error, at the mistake, however many `prec` calls trip over it:

```console
$ tsg variants/v1-unquoted-precedence.tsg
error: `call`, `pow`, `unary`, …, `or_` are used as precedence names, but unquoted entries here name rules
  --> variants/v1-unquoted-precedence.tsg:35:5
   |
35 |     call pow unary mul add concat shift band bxor bor compare and_ or_;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = help: quote them: `%precedence "call" "pow" … "or_";` — a bare entry gives the rule that precedence, a quoted one declares a name
```

Two smaller things fell out of it: `warnings`/`reports_where_the_flag_is` were
dead code — **one existing test had never run** (missing `#[test]`), which the
new check's dead-code warning surfaced. Both tests run now (36 total).

### 1.2 The lesson worth writing down

Pre-emption beat elaboration, and it beat it *cheaply*: this was ~50 lines in the
front end, whereas the elaborator could only point at `stat:` and guess
(`= help: declare it before using it: %precedence "call";` helpfully wrong,
because the name *was* declared). **If `tsg` can know, `tsg` should say it.**

---

## 2. Upstream: silent zero-width token on error recovery

Minimal reproduction (`.probe/z`, 8 lines, also takes `grammar.js`):

```tsg
%name z;
%extra /\s/;
chunk: repeat(stat);
stat: seq(NAME, '=', exp) | seq(NAME, args);
exp: NAME | STRING;
args: choice(seq('(', ')'), STRING);
STRING: choice(token(/"[^"]*"/), token(/\[\[(.|\n)*\]\]/));   // two tokens
NAME: /[a-z]+/;
```

```console
$ printf 'x = y z\n' | tree-sitter parse -
  (stat
    (NAME)
    (args [0, 7] - [0, 7]
      (STRING [0, 7] - [0, 7])))      <- zero width
exit 0                                 <- no ERROR, no MISSING, no diagnostic
```

Merge the two alternatives into **one** pattern and the same input reports
`(MISSING STRING [0, 7] - [0, 7])` with exit 1. `grammar.js` produces
byte-identical JSON for the `choice` spelling (only `$schema` differs) and
misparses identically, so this is tree-sitter, not `tsg`. Consequences for us:

* `variants/v5-token-choice-misparse.tsg` is kept as the repro. `grammar.tsg`
  deliberately uses one pattern for `STRING`.
* A `tsg` lint is cheap and worth it: *"a rule that is a choice of two or more
  `token()`s can hit tree-sitter's silent zero-width recovery; prefer a single
  alternation"*. Nothing in the pipeline warns today.
* It is the strongest argument yet for **L1.5 as an acceptance gate**: the
  grammar checked out, generated, and parsed — silently wrong. Positions in
  diagnostics do not fix this class; only testing the *parse result* does.

---

## 3. Missing `.tsg` features, ranked by what they cost on this grammar

| # | Missing | Cost here | Proposal | Size |
|---|---|---|---|---|
| **F1** | per-use precedence (`[LPAREN]`) | 18 `prec.right(call, …)` wrappers around *references* | **`funccall @right(call)` — done** (`DECLARATIVE.md` §1) | S |
| **F2** | `%left`/`%right` with rightmost-terminal default | 43 `prec.*` calls → 43 `@` annotations; 4 of 6 comparison ops repeated by hand | **`%precedence left "add" = '+' '-';` — done**, 21 of 41 marks now inherited (`SYNTAX.md`, `DECLARATIVE.md` §6) | M |
| **F3** | empty-matching rules eliminated automatically | 10 `optional(block)` + 8 more `optional(…)`; two wrong guesses | **mostly done**: `compile` now reports the alternative that can match nothing, with the use sites, before JSON | M |
| **F4** | precedence ordering is write-only | direction is descending (source reads ascending); no way to see the result | **`tsg -p` — done** | S |
| **F5** | named terminals (`PLUS ::= "+"`) | 56 terminals, inlined 114 times | `%token`-style alias | S |
| **F6** | `%conflicts` with a reason | 1 needed; the *why* lives in a comment that will drift | `%conflicts fieldlist, "…";` | S |
| **F3b** | EBNF `name (group)` vs. DSL call | call syntax must win (2 of 92 corpus grammars need both) | `$name` + a warning at the definition (done, §F3b) | S |
| **F7** | nothing for `%lexer_mode` | by design — scannerless; see §6 | — | — |

### F1. Per-use precedence marking — the single biggest gap

Lemon/Bison attach precedence to a *use*:

```
stat ::= funccall. [LPAREN]
exp  ::= var.       [LPAREN]
```

tree-sitter attaches it to a *definition*, so the port has to wrap every
reference, and it has to pick one associativity for all uses of the rule:

```tsg
stat:     … | prec.right(call, funccall);
exp:      … | prec.right(call, var) | prec.right(call, funccall) | prec.right(call, seq('(', exp, ')'));
var:      … | prec.right(call, seq(var, '[', exp, ']')) | …      // 6 of these
funccall: … | prec.right(call, seq(var, args)) | …               // 6 of these
```

18 of the 43 `prec` calls in the file exist only for this. In the JSON, `prec`
around a *symbol* is already legal, so this is pure sugar for the front end:

```tsg
stat: … | funccall@call;
```

**Landed as `@`**, a postfix annotation: `funccall @right(call)`,
`exp '+' exp @left(add)`, `'-' exp @unary`, `x @dynamic(2)`. It desugars in the
parser to `prec*`, so `grammar.json` is untouched — verified by compiling
`lua53/grammar.tsg` before and after the 43-call rewrite: `compare.py ok`, 30
rules, and the same `parser.c`. The port now reads like the source, and the
`@`-marker on a *reference* is exactly Lemon's `[LPAREN]`.

Alternatives considered: `^` (kept for aliases), `->` and `as` (keyword-shaped,
which is what the language is trying to avoid), and `funccall [call]` (too close
to array indexing and to the EBNF convention where `[…]` means optional).

### F2. `%left` / `%right` sugar with the Bison default

The source's whole precedence block is 13 lines; the `.tsg` equivalent is 13
names in one descending chain plus 43 `prec` calls, and every operator has to be
wrapped by hand:

```tsg
| prec.left(compare, seq(exp, '<', exp))
| prec.left(compare, seq(exp, '<=', exp))
…                                   // ×6 for comparison, ×4 for mul, ×2 for shift
```

Two independent ergonomics problems:

1. **Repetition.** `prec.left(<level>, seq(exp, <op>, exp))` is the same shape 25
   times. Forgetting one is not a typo you find by reading; it is a conflict
   error from the generator, far from the omission.
2. **Direction.** Declared levels are a *descending* chain (earlier binds
   tighter) while every grammar in the world lists precedences ascending. I got
   this backwards first and had nothing to check it against.

Proposal: let the terminal carry the level and derive the rest, the way the
source already does.

```tsg
%precedence left  "or" "and";          // level + associativity, ascending
%precedence left  "<" "<=" "==";
%precedence right "..";
```

A production whose rightmost terminal belongs to a declared group gets that
level and associativity, no `prec` wrapper needed; `%precedence name "or";` keeps
today's explicit form for the cases that want it. That removes ~25 lines here and
makes the precedence table the single place a level is stated — which is what
`SYNTAX.md` §"rule order is semantic" is already aiming at.

Cost: the front end must compute each production's rightmost terminal (it has the
AST) and wrap matching productions. Opt-out is needed for the handful of
productions where the leftmost terminal is the interesting one — here, unary
minus, which the source marks `[NOT]` precisely to override the default.

### F3. Empty-matching rules are legal input, illegal output

Four source rules match nothing: `semi`, `statlist`, `scope` (via `statlist`) and
`parlist`. tree-sitter rejects *any* non-start rule that matches empty, so every
use site has to say `optional(…)` instead:

```tsg
chunk: optional(block);
stat:  seq('do', optional(block), 'end') | …      // 10 sites for `block` alone
funcbody: seq(params, optional(block), 'end');
params:   seq('(', optional(parlist), ')');
```

I got this wrong twice: first by dropping `binding`/`scope` so aggressively that
`local` statements disappeared entirely (found by parsing `sample.lua`), then by
leaving `parlist` empty-capable. Both were caught by the generator, one error at
a time, with no position.

This is the ideal front-end job: the grammar says what it says, and `tsg` knows
exactly which rules are nullable. Let `blank()` alternatives exist in the source
and **lower them away** — drop the empty production, wrap every reference in
`optional`, recursively — instead of making the author do it by hand at every
site. Semantics-preserving, no schema change, and it turns the generator's most
opaque error ("matches the empty string", followed by two paragraphs of prose)
into something that never happens.

### F4. The precedence order is write-only

`%precedence A B C;` means `A > B > C`. Nothing in `SYNTAX.md`, `tsg --help`, or
the JSON tells you that, and the JSON is a nested array of `SYMBOL`s whose
meaning is only recoverable from `build_parse_table.rs:1418`. A `--precedences`
(and, generally, an `--explain`) mode that prints the resolved relation plus
which productions landed at each level would have caught both my direction error
and the F1 quoting error in one look:

```console
$ tsg --precedences grammar.tsg
call > pow > unary > mul > add > concat > shift > band > bxor > bor > compare > and_ > or_
  call     stat, exp, var, funccall (4 right-associative uses; 18 references)
  or_      exp (left)
```

Cheap (the index already exists), and it doubles as the contract for F2's
implicit levels.

### F5. Named terminals

The source names all 56 terminals (`PLUS ::= "+"`) and refers to them by name.
`.tsg` has no equivalent that does not add a node or force `%inline`, so the port
inlines 114 literal occurrences of 56 distinct terminals. That is the right
tree-sitter idiom and it is not a bug — but it is the one place where a
*mechanical* port stops being mechanical, and it makes "keyword misspelled"
invisible until a parse test fails. A `%token '+' as plus;`-style alias that is
erased at emit time (no node, no AST change) would let a Lemon/yacc grammar port
symbol-for-symbol while keeping today's output. Low priority, high porting value;
`--from-json` should round-trip it if it ever exists.

### F3b. The EBNF surface cannot spell `name (group)` when `name` is a DSL function

I first wrote this grammar in the call form (`seq(...)`, `choice(...)`) because
that is the mechanical translation of `grammar.js`, then converted it to EBNF —
and hit an ambiguity that is specific to the *nice* spelling:

```tsg
fieldlist: field ((',' | ';') field)*;       // juxtaposition …
fieldlist: field (...);                      // … parsed as field(name, rule)
```

`name (` was a call whenever `name` was one of the DSL function names, and `field`
is one of them — Lua's `field`/`fieldlist` rules collide with the DSL's
`field(name, rule)`. The error message ("`field` expects 2 arguments, but got 1")
describes a call the author did not write. Worse, the collision is silent when
the argument count happens to match.

First attempt: let a declared rule shadow the DSL function. That worked for this
file — and broke `roundtrip.sh`, 2 of 92 grammars, because Ruby declares a rule
`alias` *and* calls `alias(…)`, and MSSQL declares `field` *and* calls
`field(…)`. Both meanings are needed in one file, so shadowing is not available:

```
FAIL _root_ruby  (.tsg -> grammar.json)   error: expected `)` …, found `,`
  alias: 'alias' field(name, _method_name) field(alias, _method_name);
FAIL _root_mssql (.tsg -> grammar.json)   sqlcmd_setvar: … field(name, identifier) …
```

Resolution: call syntax keeps winning, this file writes `$field` (the explicit
symbol form), and `compile` now warns once at the definition — "rule `field` has
the same name as the DSL function `field(…)` … reference the rule as `$field`".
That is the general shape of the fix: keep the grammar language deterministic,
and pay for it with one good message instead of an arity error about a call
nobody wrote.

Two things this says about the integration:

* **The EBNF surface is under-tested.** `to_tsg` (`--from-json`) emits only the
  call form, so every file in `examples/` and `examples/tsg.tsg` itself is
  call-form output. Nothing exercises `|`, `?`, `*`, `+`, juxtaposition or
  grouping — this grammar is the first real user of them, and the first thing it
  did was find a parser bug. A `tsg → grammar.json → tsg` round trip, and/or an
  EBNF emitter style for `to_tsg`, would close the hole.
* **Style mixing hides this class of bug.** The first draft of this file was
  EBNF only at the top: `stat:` alternated with `|` but every branch was
  `seq(...)`, so the file that *looked* converted still exercised only the call
  path. The bug appeared on the first line that used a real group.
* **The corpus is part of the test.** The shadowing fix looked obviously right
  and passed every test in the crate; `roundtrip.sh` found the two real grammars
  that use both meanings within a minute. Any change to how names resolve has to
  go through the 92-grammar corpus, not the fixture set.

### F6. `%conflicts` carries no reason

Exactly one conflict is needed, and it is not a bug in the grammar:

```tsg
// `{f,}` vs `{f,g}`: whether a `,`/`;` after the last field is the table's
// trailing separator is not LR(1)-decidable — the source gets away with it
// because Lemon defers the decision in the automaton, tree-sitter needs GLR.
%conflicts fieldlist;
```

The comment is doing the work. Two consequences: a future reader cannot tell
intentional ambiguity from tolerated ambiguity, and the generator's
`UnnecessaryConflicts` diagnostic will happily delete the annotation if a
refactor makes it moot — correct behaviour, no way to record why it was there.
`%conflicts fieldlist, "trailing `,`/`;` before `}`";` costs almost nothing and
keeps the rationale next to the annotation.

---

## 4. Missing integration features (Level 1 of `INTEGRATION.md`)

This exercise ran the *manual* version of the pipeline and hit every seam:

* **`generate` does not accept `.tsg`.** The port runs `tsg grammar.tsg -o
  src/grammar.json && tree-sitter generate src/grammar.json`. Needs the `.tsg`
  arm in `load_grammar_file` (`generate.rs:607`) — and `src/grammar.json` then
  becomes a build artifact that must not be mistaken for the source of truth
  (`generate.rs:397` write-back).
* **Grammar discovery is `grammar.js`-shaped.** `tree-sitter parse` in a grammar
  directory wanted `src/grammar.json` + `tree-sitter.json`; I hand-wrote both.
  `xtask/.../find_grammar_files` (`crates/xtask/src/generate.rs:248`) and the two
  `grammar.js` defaults in `generate.rs` (`:367`, `:420`) all need the second
  filename. This is what L1 buys and what L2 (`init --grammar-format tsg`)
  finishes.
* **Nothing consumes `Compiled.index`.** The elaborator had to re-read the
  `.tsg` through the CLI's `--json-summary` (see `prototype/OUTPUT.md`). In-tree,
  the index should travel with the JSON so conflicts get positions without a
  second process.
* **Uncaught errors were only found by *parsing*.** With `.tsg` wired in, the
  natural acceptance test for the integration is this grammar: generate must
  produce the `parser.c` that parses `sample.lua`/`tricky.lua`/`prec.lua`.

---

## 5. Missing diagnostics: the three failures as reusable fixtures

`variants/` is not a synthetic test set; it is the shape real mistakes take.

### v1 — precedence name declared as a rule reference

Now caught by `tsg` (§1.1). Fixture kept because the elaborator must *not* be
the one explaining it.

### v2 — empty rule

```console
$ tree-sitter generate
The rule `block` matches the empty string.

Tree-sitter does not support syntactic rules that match the empty string
unless they are used only as the grammar's start rule.

$ prototype/target/debug/tsg-elaborate-prototype variants/v2-empty-block.tsg
error: rule `block` can match nothing, which tree-sitter cannot parse
  --> v2-empty-block.tsg:51:8
   |
51 | block: seq(repeat(seq(stat, optional(';'))), optional(seq(laststat, optional(';'))));
   |        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ rule `block`
```

Better than nothing, and still the wrong owner: `tsg` can decide nullability
exactly, so the error should not survive to the generator at all (§F3).

### v3 — conflict

```console
$ tree-sitter generate
Unresolved conflict for symbol sequence:
  funccall  •  '('  …
Possible interpretations:
  1:  (funccall  funccall  •  args)  (precedence: 'call')
  2:  (stat  funccall)  •  '('  …    (precedence: 'call')
Possible resolutions:
  1:  Specify a higher precedence in `funccall` than in the other rules.
  …

$ prototype/target/debug/tsg-elaborate-prototype variants/v3-call-conflict.tsg
error: unresolved conflict in rule `stat`
  --> v3-call-conflict.tsg:56:1
   |
56 | stat:
   | ^^^^ `stat` is reduced here, `'('` is the lookahead
   ...
147 | funccall:
    | ^^^^^^^^ `funccall` is also involved
    |
    = help: give `funccall` a higher rule precedence: `prec(…)` in that rule, or …, or accept the ambiguity with `%conflicts stat funccall;`
```

The elaborator earns its keep here, because this is the one class `tsg` cannot
pre-empt. Two things it needs that the generator does not provide:

* `ConflictError` has no rule ids — the second label came from searching
  `symbol_sequence` for a name that is not the first interpretation's
  `variable_name`. Exposing both sides (or a `variable_index`) makes that exact
  instead of heuristic.
* The `help:` advice is *derived* from `possible_resolutions`, so it stays
  correct as the generator's advice changes. Keep it that way.

### What each failure wanted

| Failure | Wanted | Owner |
|---|---|---|
| precedence quoting | error at the directive, fix included | `tsg` (done) |
| empty rule | never reach the generator | `tsg` (done: `nullable.rs`) |
| conflict | position, both rules, actionable suggestion | elaborator |
| silent zero-width token | any diagnostic at all | tree-sitter upstream |

---

## 6. What already worked (do not redesign these)

* `%extra` / `%word` / `%conflicts` / `%precedence` cover the source's directives
  one-for-one; the only thing the lexer sections cost was two regexes.
* Long comments, long strings, escapes, `//`, `%` and `[[…]]` all expressible as
  single tokens; the Flex start conditions are genuinely unnecessary in a
  scannerless grammar. `--[ not a long comment` needs no special case.
* `prec.dynamic` went unused; `prec.left`/`prec.right` were sufficient once the
  per-use wrappers were in place.
* Emitted JSON for the minimal grammar is byte-identical to `grammar.js` (only
  `$schema` differs), and the whole 185-production grammar compiles in
  milliseconds with no Node.
* `tree-sitter generate` reported **zero diagnostics** for the finished grammar,
  which means the `%conflicts` is necessary and the flags/supertypes/precedence
  checks all agree with the port.

---

## 7. Recommended order, updated by this exercise

1. **L1** — `.tsg` arm in `generate`, discovery, `Compiled.index` handed over.
   Acceptance test: `tree-sitter generate lua53/grammar.tsg` reproduces the
   `parser.c` that parses these three files.
2. **`tsg` pre-emptions** — nullability (F3) and the precedence-name check
   (§1.1, done). Both are front-end-only, both remove a position-free error.
3. **F1 + F4** — per-use precedence and `--precedences`. Together they turn
   "porting a Lemon grammar" from judgement into mechanics, and they are the two
   things I would have used most.
4. **L1.5** — the elaborator, with `variants/v1`–`v5` as the fixture set.
5. **L2** — `init --grammar-format tsg`, including the `tree-sitter.json`/
   `src/` layout I had to write by hand.
6. **F2, F5, F6** — sugar, aliases, reasons. Nice to have; none of them blocked
   this port.

## Files

```
grammar.tsg                          the port
sample.lua                           the source's own test program
tricky.lua                           34 constructs: methods, chains, varargs, …
prec.lua                             one expression per precedence/assoc question
variants/v1-unquoted-precedence.tsg  precedence names vs rule references
variants/v2-empty-block.tsg          empty-matching rule
variants/v3-call-conflict.tsg        the stat/funccall conflict, before the fix
variants/v5-token-choice-misparse.tsg  silent zero-width STRING (upstream)
tree-sitter.json, src/               what `tree-sitter parse` needs, hand-written
```
