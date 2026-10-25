# Dog-food #2: `antlr-v4.5.g`

Dog-food #3 (`bc.g`) is appended at the bottom of this file.  It stopped
mid-run: the translator fixes it found are **not committed**, and two of
its three remaining parse errors are a tree-sitter lexer bug, not a
translator bug.  Read that section first if you are picking this up.

`postgres16.g` was the first port, and it was chosen because it was hard and
because a SQL sample could prove the result.  This run takes the opposite kind
of input: an 888-line Bison + Flex grammar for ANTLR 4.5 — small, no precedence
levels at all, and with a Flex section that cannot express what the source
really needs (lexer *modes* and target-language `{ … }` actions).

Nothing here is a regression test by itself; `antlr/check.sh` is (78 rules, 45
tokens, 242 states, 3 samples).  What follows is what the run cost, what broke,
and what is still rough in `tsg` and in `tree-sitter`, in that order.

## The run

```
$ g2tsg antlr-v4.5.g -o grammar.tsg --name grammar
note: the lexer never returns ACTION_CONTENT, END_ACTION, END_ARGUMENT and 1 more; dropped 22 production(s) that need them
grammar.tsg: 78 rules, 45 tokens, 0 levels
$ tsg grammar.tsg -o src/grammar.json
$ tree-sitter generate src/grammar.json --lalr        # 0 conflicts, 0.07 s
$ tree-sitter build . && tree-sitter parse samples/*.antlr
ok   grammar_decl     grammar TG;
ok   mode_spec        grammar TG;
ok   parser_grammar   parser grammar TG;
```

Two contrasts with postgres16 are worth stating:

| | postgres16 | antlr-v4.5 |
| --- | --- | --- |
| rules / tokens / levels | 705 / 489 / 27 | 78 / 45 / **0** |
| `%conflicts` needed | 12 | **0** |
| generate (`--lalr`) | 4.2 s, 1 378 states | 0.07 s, 242 states |
| parser.c | 25 MB | 451 KB |
| SQL/source corpus | parses clean | 3 of 3 skeleton samples clean |

The zero is the interesting part: this grammar is plain LALR and stays plain
LALR *as long as the empty productions come out right*.  Everything that went
wrong in this run went wrong there.

## Four translator bugs the run found (all fixed)

1. **A dropped production could become a silent empty production.**
   `ruleSpec : parserRuleSpec | lexerRuleSpec;` loses its first alternative
   because `parserRuleSpec` needs `RULE_REF`, which this Flex section cannot
   lex.  The rule was then left with no alternatives at all, and
   `remove_empty_rules` read "no alternatives" as "matches the empty string"
   (an `all()` over an empty list is `true`).  A reference to it was deleted
   instead, leaving `rules : rules ;` and a nullable `rules` — so the
   translation made `grammarSpec` accept *zero* rules and produced 6 conflicts
   out of one misplaced `?`.  Fixed by treating "no alternatives" as
   unproductive (drop the rule *and* the productions that mention it) rather
   than empty.

2. **A trailing comment ate the lexer action.**
   `[a-z][A-Za-z0-9_]+	RULE_REF //FIXME` was split at the *last* whitespace
   run, so the action became `//FIXME` and `RULE_REF` had no body — one of the
   four tokens that "the lexer never returns".  A Flex pattern may contain
   whitespace (inside `[…]` or `"…"`), so the split now happens at the first
   whitespace that is outside both.

3. **Commented-out lexer rules were read as rules.**
   The section is line-oriented, so `//{ID}	TOKEN_REF` and every line inside a
   `/* … */` block became a real Flex rule.  A commented-out rule could
   therefore *win* a token's body (the last rule for a token wins).  Both are
   skipped now.

4. **A start rule that loses every production was silently replaced.**
   Because the rule was removed, `emit` fell back to "the first rule left" and
   the language's entry point moved without a word.  That is now an error:
   *every production of `s` — the start rule — needs CODE the lexer never
   returns; a start condition (`%x`, `<state>`) or a C action is not
   translated.*

One more, found by the corpus before this run: `%prec NAME` where `NAME` is a
declared token that no `%left`/`%right`/`%precedence` line mentions.  Bison
gives it precedence `none`, which is exactly what an unannotated alternative
gets, so the annotation is a no-op and must be dropped — not refused.

## Rough edges that remain in `tree-sitter` and `tsg`

Ordered by what they cost a porting tool, with the evidence this run produced.

1. **No empty production.**  Unchanged from `HANDOFF.md` and still the single
   biggest cost.  Here it is visible in miniature: 78 rules, and the same rule
   shape (`X : %empty | X item`) has to be rewritten into a base case by the
   translator.  Get one `?` wrong and it is not an error but 6 conflicts.  In
   postgres16 the same rewrite turns 705 rules into 8 000 productions and
   1 378 states, and all 12 `%conflicts` are `?` artifacts.

2. ~~**No diagnostic for an unreachable or non-productive rule.**~~ Fixed in the
   generator series (`49aa685`, see `tree-sitter-patches/PR.md`), and this run is
   the reason it exists.  It was the gap that let bug 1 through: the buggy
   grammar *generated fine*.  Measured before the fix:

   ```
   $ cat dead.tsg
   %name dead;
   tokens { B: /b/; }
   s: a;
   a: a B;            // no base case: derives nothing
   unreachable: B;    // nothing can reach it
   $ tsg dead.tsg -o src/grammar.json && tree-sitter generate src/grammar.json
   $ echo $?
   0                  // 5 states, and `s` can never match anything
   ```

   Bison reports the equivalent as *"nonterminal useless in grammar"*.
   tree-sitter said nothing, so a translated grammar that quietly lost a rule
   looked healthy.  It now says:

   ```
   Warning: rule `unreachable` is not reachable from the start rule. it was ignored.
   Warning: rule `s` cannot match anything: every production of it needs a rule or token that can never match. the parser will never accept it.
   Warning: rule `a` cannot match anything: every production of it needs a rule or token that can never match. the parser will never accept it.
   ```

   Three warnings for one cause is deliberate: Bison also reports every useless
   nonterminal, and one of them is the start rule, which is the part that
   matters most.  They are warnings, so the exit status of a grammar that used
   to generate is unchanged; `--json-summary` carries them too.

3. **Bison's default shift preference has no spelling.**  When the source
   grammar is ambiguous-but-tolerated (Bison resolves shift/reduce by shifting),
   the port has exactly one tool — a `conflicts` entry — which is all-or-nothing
   and gives up on the whole production rather than naming a preference.  This
   run did not need it (0 conflicts); postgres16 needed 12.

4. **`tsg` does not compile a `/…/` body.**  A regex the runtime rejects is
   caught two steps later by `tree-sitter generate`, against `grammar.json`
   rather than against the grammar line.  (Hit earlier by the corpus:
   `[\x0000-\x001F]`.)  Compiling the body at parse time would also catch the
   patterns this file has that Flex would never accept (`(?s:…)`, `+?`).

5. **`tsg` cannot say "no production ever produces this token".**  tree-sitter
   requires a lexer body for every token in a rule body, and a Bison grammar is
   allowed to declare a token that its Flex section never returns.  The only
   way out is to delete the productions that need it — which *changes the
   language* (22 productions here, so the parser is a skeleton) and which the
   translator has to do with a `note:` and no way to ask the user.

6. **Flex is not ANTLR, and the gap is silent at the metagrammar level.**
   `%x`/`<state> { … }` mode blocks and `{ target code }` actions are outside
   Flex; `grammar.tsg` reads the Flex sections line by line, so those lines are
   *accepted* and then produce dead tokens downstream.  The note says what was
   dropped, but nothing at the translation step says "this token exists only in
   a start condition".  A `%mode`-aware front end is a different project; the
   rough edge is that the failure surfaces as *missing productions*, not as a
   diagnostic at the lexer rule.

7. **The corpus got *smaller*, on purpose.**  290 → 284 of 358 grammars produce
   a `.tsg` that `tsg` accepts.  The 16 that changed now fail with *"every
   production of `X` — the start rule — needs CODE the lexer never returns"*,
   because the previous behaviour was to promote the next rule to start symbol
   and hand back a parser for the wrong language.  Six of the 284 used to be in
   the "accepted" column while being wrong.  A smaller number that means
   "correct or refused" is the one to optimise, and this is the kind of thing
   the notes have to say out loud so the number is not read as a regression.

8. **Flex `skip()` is a comment rule, and the translator has nowhere to put it.**
   This is the one that keeps a real ANTLR grammar file from parsing, and it is
   entirely a translator gap:

   ```
   {DocComment}		skip()          # ANTLR's own grammar, lines 663-672
   {BlockComment}	skip()
   {LineComment}		skip()
   ```

   A Flex rule whose action is `skip()` returns no token, so `g2tsg` correctly
   drops it — and then there is nothing to skip a `//` comment, so the line
   `// The main entry point for parsing a v4 grammar.` is lexed as
   `RULE_REF RULE_REF RULE_REF …` and reported as one `ERROR` (68 of them over
   the file, all starting at a comment or at the `;` that follows a rule).
   The target has the right notion — `tsg` has `%extras`, `grammar.json` has
   `extras` — so this is a small emitter change: a `skip()` rule becomes a named
   token plus an `extras` entry, and Flex's `%option` order decides the name.
   Not done, so not measured.

## What "one shot" would need

In order of payoff, and none of them are in the translator:

1. **An empty production** in tree-sitter (or an explicit `repeat`/`optional`
   node in `tsg`) — removes the `?` rewrite, its conflicts, and most of the
   state explosion.
2. ~~**Reachability and productivity diagnostics** in `generate`~~ — done, commit
   `49aa685`; `tsg` itself still says nothing (see `HANDOFF.md` item 12).
3. **Compile `/…/` in `tsg`** — moves a regex error from `grammar.json` back to
   the grammar line.
4. `%fallback` / a wildcard token (17 corpus files) and `%nonassoc` (no
   equivalent at all).
5. Translator-side only, no tree-sitter gap: `(…)`, `[…]`, `{…}` groups
   (10 files) and the 20 nullable-mid-body lists.

## Reproducing this run

```
cd tmp/tsg
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
(cd bison_flex/translate && cargo build --release)

TSG=target/debug/tsg \
TS=$HOME/.ts-build/release/tree-sitter \
G2TSG=$HOME/.ts-build/release/g2tsg \
  ./antlr/check.sh
```

`antlr/antlr-v4.5.g` is a copy of
`parsertl-playground/playground/antlr-v4.5.g` (BSD-3-Clause, header intact) so
the check is reproducible without the corpus; `antlr/samples/*.antlr` are the
three shapes the translated grammar still accepts.

The full 358-grammar histogram this run started from is in
`HANDOFF.md` ("The corpus run") and `bison_flex/translate/README.md`
("What the corpus says").

---

# Dog-food #3: `bc.g` — stopped mid-run

`parsertl-playground/playground/bc.g` (292 lines, Bison + Flex) plus
`test.bc-calculator` (272 lines: the OpenBSD `bc.library`, with `define e(x)`,
`auto` declarations, `scale`, `ibase`, and comments) as the input sample.

This run was chosen because it is the first input where the translator is not
just incomplete but *wrong in ways that look plausible*: a truncated regex, a
token that lost two thirds of its patterns, a precedence chain that silently
split.  It also produced the most important finding of the three dog foods, and
that finding is **not** in `g2tsg`: an extra loses to a token that is a prefix
of it.

## The run

```
$ g2tsg bc.g -o grammar.tsg --name bc \
      --title "bc calculator, translated from bc.g (Bison + Flex)." \
      --conflicts conflicts.txt
grammar.tsg: 22 rules, 34 tokens, 12 levels
$ tsg grammar.tsg -o src/grammar.json
$ tree-sitter generate --lalr src/grammar.json     # 187 states, 0 conflicts
$ tree-sitter build . && tree-sitter parse samples/calculator.bc
ERROR [104,17]-[104,48]  ERROR [105,24]-[105,49]  ERROR [105,57]-[105,59]
MISSING Scale [74,13]                             # rows are 0-based
```

One declared conflict, and it is honest policy rather than a translation
artifact:

```
# conflicts.txt
statement        # dangling else: opt_else has an empty alternative, and
                 # tree-sitter cannot put precedence on an empty production.
                 # It prefers the shift, which is Bison's choice too; the tree
                 # is the same, so declare it instead of rewriting the rule.
```

`bc.g` is a good small gate candidate (22 rules, 12 precedence levels,
`%nonassoc IF_WITHOUT_ELSE`, a lexer with macros and 34 token names), but it is
**not** a gate yet: it does not parse its own sample cleanly, and the two
reasons are below.

## Why the sample still has errors (two separate causes)

1. ~~**The translator hardcodes `%extra /--[^\n]*/`**~~ — fixed.  Extras are now
   the lexer's own `skip()` rules (`emit.rs`): `bc` gets `/[ \t]+/` and
   `/\/\*(?s:.)*?\*\//`, so `while (y--)` is `y` and `--` again, and
   `MISSING Scale` is gone.  The same change gives `antlr-v4.5.g` its `//` and
   `doc` comments and `postgres16.g` the `--`/`/* */` pair *from its own macro*
   instead of from a guess.
   **Measured, and it is a regression for `bc`**: 3 `ERROR` + 1 `MISSING Scale`
   before, 24 `ERROR` + 1 `MISSING Break` after.  Faithfulness exposes what the
   guess was hiding: `bc.g`'s lexer skips `[ \t]+` and nothing else, it never
   returns `ENDOFLINE` at all (the translator hardcodes `ENDOFLINE: /\n/`), and
   the grammar is full of `ENDOFLINE`.  The old extra `[ \t\n\r\f]+` matched
   `\n` too, so every newline in a state that did not know `ENDOFLINE` was
   silently swallowed.  That is not the translator's call to make: it is a hole
   in `bc.g`, and it needs an `ENDOFLINE` rule (or an explicit `%extra /\n/`),
   decided in the `.g` or in `bodies.txt`.

2. **`1 /* comment */` — the `/` wins.**  Both remaining errors are a value
   followed by a block comment where `/` is a valid operator:

   ```
   a = (2.31*s)/1 /* estimated integer part of the answer */
   s = t + length(a) + 2 /* estimated length of the answer */
   ```

   The comment is *not* skipped.  `parse -d pretty` says exactly why:

   ```
   lex_internal state:1, row:104, column:16
     skip character:' '
     consume character:'/'
   lexed_lookahead sym:Slash, size:2
   ```

   The lexer consumed `/` and stopped; it never tried the `/* … */` extra, even
   though the extra matches 33 characters and `Slash` matches 1.  **This is
   tree-sitter, not the translator.**  Minimal reproduction (30 s, no corpus):

   ```
   %name mx;
   tokens { Slash: /\//; Star: /\*/; Num: /[0-9]+/; }
   %extra /[ \t\n]+/;
   %extra /\/\*([^*]|\*+[^*/])*\*+\//;
   expr: expr Slash expr | expr Star expr | Num;
   %conflicts expr;
   ```

   ```
   $ printf '1/1 /* hi */ 2\n1 /* hi */ /2\n' > s.mx
   $ tree-sitter parse s.mx
   (ERROR [0,4]-[0,9] …)     # /* lexed as Slash Star
   ```

   Verified to be **pre-existing**: the same output from a build of upstream
   `0fff6ab` (worktree `/Users/mingo/dev/ts-up`, `CARGO_TARGET_DIR=/Users/mingo/dev/ts-up-target`),
   i.e. it is *not* caused by the shared-separator NFA change in this series.
   Consequence for the series: nothing to fix here, but it is worth reporting —
   "an extra whose first character starts a valid token loses to that token" is
   one sentence, and every grammar with `/…/` comments and a division operator
   is exposed.

   (The `antlr-v4.5.g` errors in dog-food #2 are *not* this bug: there the
   comment rule was `skip()`, which the translator drops entirely.)

## Three translator bugs the run found (fixed, **uncommitted**)

All three are in `bison_flex/translate/`; nothing outside `tmp/` changed.

1. **`strip_start_condition` truncated a pattern that ends in `>`**
   (`src/cst.rs`).  `<A,B>pattern` is a Flex start condition, so the translator
   strips a leading `<…>`.  It also stripped a *trailing* `<…>`, which ate the
   last operator of `REL_OP: /==|<=|>=|!=|<|>/;` → `…!=|`.  Fix: only strip when
   the bracketed text is a well-formed state list (`is_state_list`).
2. **Multiple Flex rules for one token kept only the last pattern**
   (`src/emit.rs`).  `bc`'s `NUMBER` has three patterns
   (`{DIGIT}+(\.{DIGIT}+)?`, `{DIGIT}+\.`, `\.{DIGIT}+`) and only the third
   survived, so `2.31` was not a `NUMBER`.  Fix: keep them in source order and
   merge the bodies with `|` — which is Flex's own semantics (one token, longest
   match across all of its patterns, ties to the earlier rule).
3. **An empty precedence level was emitted as a bare ordering list**
   (`src/emit.rs`).  `%nonassoc IF_WITHOUT_ELSE` declares a level that no
   terminal inherits.  `tsg` treats `%precedence "p11";` as an ordering list
   (`is_level()` = has assoc, has `=`, or has terminals), so `@p10` and `@p11`
   were incomparable and `- expression • AND` became a real conflict.  Fix:
   emit `%precedence "p11" = ;` when the level has no assoc and no terminals —
   `tsg`'s documented spelling for "a level nobody inherits" — and leave
   `%precedence right "p22";` alone, since the assoc already makes it a level.

### These were latent bugs, not `bc` tuning — measured over the corpus

Rebuilt the whole playground corpus (358 `.g`, 287 translatable) with the old
and the new `g2tsg` and diffed the output:

| | files |
| --- | --- |
| output changed | **203** of 287 |
| output identical | 84 |
| failures before / after | 71 / 71 — **no new failure** |

Of the 203: 138 changed only token bodies, 26 only precedence lines, 39 both.
Examples of what the old output had silently lost:

```
CSSGrammar-vlc.g   LENGTH: /([0-9]+|[0-9]*\.[0-9]+)pc/          # 1 of 8 units
                   LENGTH: /…em|…ex|…px|…cm|…mm|…in|…pt|…pc/    # after fix
calc-alias.g       %precedence "p3";   →   %precedence "p3" = ;
```

Corpus counts for the constructs themselves: **220 of 358** grammars have a
token produced by two or more lexer rules, and **38 of 358** have a lexer
pattern ending in `>`.

`postgres16/grammar.tsg` is byte-identical after all three fixes (checked with
the standard `diff`), so the gates are unaffected.  `postgres16/translate.py`
(the documented reference implementation) was **not** updated: it never had
these three bugs in the same form and only ever sees `postgres16.g`;
`postgres16/check.sh` still asserts that both implementations agree.

## What reports exist, and what does not (the "would this have helped?" list)

Written down because it answers "why did this take as long as it did".  Every
item below was exercised on `bc`.

**1. Conflicts — `tree-sitter generate` output, `--json-summary`.**
Symbol sequence, competing interpretations, and a minimized example; `01b1463`
makes it print *all* of them in one run (the `bc` dangling else took 11 runs
before that, one conflict per run).  Limit: it names a symbol sequence and never
a state id, so it does not join to the state report below.

**2. Parse state machine — `generate --report-states-for-rule <RULE>|*|-`.**
Much better than its obscurity suggests.  `-` gives a state-count table per
rule, `*` gives every rule, a name gives kernel items **with lookaheads** plus the
symbol sequence that reaches the state:

```
$ tree-sitter generate --report-states-for-rule statement src/grammar.json
expression          	470
statement           	188
…
state index: 726
state id: 386
symbol sequence: If ( expression ) Break
items:
statement → Break •	[Else, ENDOFLINE, ;]
```

It respects `--lalr`, which makes it a *teaching* report: the same rule reports
188 states canonical versus 47 with `--lalr`, and `statement → Break •`
collapses from three states with different lookaheads into one with
`[Else, ENDOFLINE, ;, }]`.  That is what LALR merging does, visible in ten
lines.  Limit: keyed by rule name only (no state id, no lookahead filter), and
the two numbering schemes (`state index` and `state id`) are unrelated.

**3. Lexer — `parse -d quiet|normal|pretty`, and since `0009` `generate --report-lex-states <N>|*|-`.**
Before `0009` this was trace-only.  Now `-` prints one line per transition the
lexer drops (with the lex/parse state counts it affects), `*` prints every lex
state a parse state starts in, `N` prints one: its parse states, valid tokens and
drops.  The `/*` bug of this run is two lines of `-`:

```
a token shadows an extra:
  `/`  ->  the extras (on '*')  2 lex states, 2 parse states
```

The rule it makes visible, worth remembering: **at equal matched length a token
beats an extra, and this is not configurable.**  The author's only lever is that
no token may be a prefix of an extra.  What is still missing (see
`AUTHORING.md`): unnamed extras print as "the extras", and a separator shadow is
not a *warning* at generate time — it is still opt-in.

**3b. Lexer, the old way — `parse -d quiet|normal|pretty`.**
Traces `lex_internal state:N`, `skip character`, `consume character`,
`lexed_lookahead sym:…, size:…`, `detect_error`, `recover_with_missing`,
`skip_token`.  This is what explained the `/*` bug above.  There is **no** lexer
report otherwise: nothing lists the tokens and extras valid in a lex state,
nothing dumps the token DFA or the lex table, nothing says which token shadows
which.  Lexing is only observable by tracing a parse, along the paths the input
happens to take — which is why the `/*` bug needed a custom minimal grammar to
confirm.  (`tsg`'s `token_conflicts` computes a token-overlap NFA internally for
exactly this question; the CLI has no equivalent.)

**4. Parse tree and recovery — `parse -x/--xml`, `-c/--cst`, `--dot`, `-s/--stat`, `-D/--debug-graph`.**
`-D` writes `log.html` with graphviz graphs of the stack and the tree (needs a
wasm-enabled CLI); `-d` prints `recover_with_missing symbol:…`, which is the
`MISSING Scale` shape above.

**5. Grammar pipeline — `generate -l`.**
Debug log of the grammar after each stage (parse → normalize → prepare → flatten
→ lex table), plus the stage timings added in `7e75cdb`.  This is how you answer
"where did my rule go" — except for the drops that `49aa685` now reports
naming rules instead.

Missing, i.e. reached for and not found, in the order I would want them:

- ~~a **lex-state report** (valid tokens + extras per state) and a **shadowing /
  overlap report**~~ — done in `0009` (`--report-lex-states`), see item 3 above.
  Still missing from it: extras by name, and "token A is unreachable because B
  always wins", which is a stronger statement than "this transition was dropped";
- **example generation** for token ambiguity: "give me a string `A` matches that
  `B` also matches";
- **unused `conflicts` detection** — a dead `%conflicts`/`conflicts.txt` entry is
  accepted forever, silently;
- conflict output that carries **state ids**, so it joins to
  `--report-states-for-rule`;
- **extras are invisible in every report**: `--report-states-for-rule` prints
  token lookaheads and never mentions that a comment might be skipped;
- **size attribution**: no way to ask which rule or state accounts for
  postgres16's 25 MB `parser.c`.

## Where this stopped, and what I would do next

Stopped deliberately, at a clean point: the translator changes are measured
(203 files changed, 0 regressions), `postgres16` is byte-identical, and nothing
is committed.  Next, in order:

1. **Derive `%extra`s from `skip()` instead of hardcoding them** (this also
   fixes dog-food #2's 68 `ERROR`s in `antlr-v4.5.g`, whose comment rules are
   `skip()`).  Cost: `postgres16/grammar.tsg` is regenerated once, because its
   extras stop being hardcoded.  This is the last known overfit in `emit.rs`.
2. **Add a lex-state report to the generator**, next to
   `--report-states-for-rule` (same CLI shape, same respect for `--lalr`), and
   then a shadowing report.  Then the `/*`-loses-to-`/` finding is either
   explained by the report or is a bug report for the tree-sitter author.
3. **Promote `bc` to a third gate** (`bc/check.sh`): it is small, it has
   precedence levels, `%nonassoc`, a multi-pattern token, and a real sample.
   Blocked only on items 1 and 2 — after those, `samples/calculator.bc` should
   parse with zero `ERROR`/`MISSING`.
4. Still open from dog-food #2: `%fallback` (17 corpus files), `(…)`/`[…]`/`{…}`
   groups (10 files), the 20 nullable-mid-body lists, and Flex `^` anchors
   (24 REGEX + 13 OTHER files; `tree-sitter generate` says "Assertions are not
   supported").

The run itself is checked in at `tmp/tsg/bc/` (68 KB: `bc.g`,
`grammar.tsg`, `conflicts.txt`, `samples/calculator.bc`, `README.md`) so the next
session does not need the playground corpus to reproduce it.  It has no
`check.sh`: it does not parse cleanly, and `bc/README.md` says why.

Machine state at this stop (ramdisk has ~163 Mi free):

```
tmp/tsg/bc/                the run, checked in (grammar.tsg is uncommitted-translator output)
tmp/tsg/.bc/mx/            the 12-line /* */ reproduction
tmp/tsg/.bc/up/            the same grammar, generated by upstream 0fff6ab
tmp/tsg/.bc/corpf/         the pre-fix outputs (baseline for the overfitting diff)
tmp/tsg/.bc/corpf2/        the 203 changed outputs + changed.txt
tmp/tsg/.bc/fails-{old,new}.txt
```

This is scratch and therefore under `tmp/tsg/.bc/` (11 MB).  It used to live in
`/Users/mingo/dev/ts-exp/`, which the constraints forbid — everything that was
outside this folder is now deleted (`ts-build/` 2.5 GB, `ts-exp/` 769 MB,
`ts-up-target/` 439 MB, and `tmp/` holds the only copy of the project).  See
`HANDOFF.md`, "Constraints (hard)": a build target dir is a throwaway that gets
deleted when the session stops.

The upstream check was done in a `git worktree` at `/Users/mingo/dev/ts-up` with
its own 439 MB target dir; both are **deleted** (`git worktree remove --force`,
then `git worktree prune` — `git worktree list` shows only the checkout, and
`.git/worktrees/` is empty).  Re-create it the same way if the extras-versus-token
finding ever needs re-confirming; it is 30 s of build, and it must not be left
lying around:

```
cd /Volumes/RamDisk/tree-sitter && git worktree add -f /Users/mingo/dev/ts-up 0fff6ab
cd /Users/mingo/dev/ts-up && PATH="/opt/homebrew/opt/rustup/bin:$PATH" \
  CARGO_TARGET_DIR=/Users/mingo/dev/ts-up-target cargo build --release -p tree-sitter-cli
# then remove both again
```

`g2tsg` build (unchanged from dog-food #2):

```
cd tmp/tsg/bison_flex/translate
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
CARGO_TARGET_DIR=$HOME/.ts-build cargo build --release --offline
# binary: $HOME/.ts-build/release/g2tsg   (delete $HOME/.ts-build when you stop)
```

Re-running the `bc` project end to end:

```
cd /Volumes/RamDisk/tree-sitter/tmp/tsg/bc
G=$HOME/.ts-build/release/g2tsg
TSG=/Volumes/RamDisk/tree-sitter/tmp/tsg/target/debug/tsg
TS=$HOME/.ts-build/release/tree-sitter
export XDG_CACHE_HOME=/Volumes/RamDisk/tree-sitter/tmp/tsg/.bc/cache
$G bc.g -o grammar.tsg --name bc \
   --title "bc calculator, translated from bc.g (Bison + Flex)." --conflicts conflicts.txt
$TSG grammar.tsg -o src/grammar.json && $TS generate --lalr src/grammar.json
$TS build . && $TS parse samples/calculator.bc
```

Regenerating the corpus comparison (the overfitting test above):

```
cd /Volumes/RamDisk/tree-sitter/tmp/tsg/.bc && rm -rf corpf2 && mkdir corpf2
for f in /Users/mingo/dev/parsertl-playground/playground/*.g; do
  b=$(basename "$f")
  $G "$f" -o "corpf2/$b.tsg" 2>/dev/null || rm -f "corpf2/$b.tsg"
done
# then diff against corpf/ (the pre-fix outputs from the previous session)
```
