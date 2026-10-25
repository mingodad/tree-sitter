# Making `.tsg` declarative: the EBNF-first path

The goal, stated: `.tsg` should read as a grammar, not as a program calling a
grammar-construction API. EBNF is the way; the call form (`seq`, `choice`,
`prec`, …) exists for two jobs and only two:

* it is what `--from-json` emits, so `grammar.json → .tsg → grammar.json` is
  lossless, and
* it is the mechanical translation target for `grammar.js`, where
  `seq('if', $._expr)` really is what the source says.

Dogfooding the Lua port showed the cost of leaving it there. That grammar needed
43 `prec` calls and one `field` label, and the first time it was written in real
EBNF it hit two parser bugs. Both are fixed; two of the constructs now have
syntax; the rest of this document is what is left, in the order I would do it.

Ground rule for everything below: **syntax sugar desugars in the parser**, so
`grammar.json` and `parser.c` are unchanged. Every proposal here is checkable by
compiling the same grammar both ways and diffing the JSON — which is how the two
that landed were verified.

---

## 1. Where we are

| # | Construct | Call form | EBNF spelling | Status |
|---|---|---|---|---|
| 1 | sequence | `seq(a, b)` | `a b` | done |
| 2 | alternation | `choice(a, b)` | <code>a &#124; b</code> | done |
| 3 | zero or one | `optional(a)` | `a?` | done |
| 4 | zero or more | `repeat(a)` | `a*` | done |
| 5 | one or more | `repeat1(a)` | `a+` | done |
| 6 | blank | `blank()` | `()` | **done** |
| 7 | rule precedence | `prec(v, a)` | `a @v` | **done** |
| 8 | left / right | `prec.left(v, a)` | `a @left(v)` / `a @right(v)` | **done** |
| 9 | dynamic precedence | `prec.dynamic(n, a)` | `a @dynamic(n)` | **done** |
| 10 | field label | `field('k', a)` | `k= a` | done (§2) |
| 11 | alias | `alias(a, 'k')` / `alias(a, rule)` | `a ^'k'` / `a ^rule` | proposed (§3) |
| 12 | lexical rule | `token(a)` | `tokens { X: a; }` | **done** (§4) |
| 13 | immediate token | `token.immediate(a)` | `tokens { X: a @immediate; }` | **done** (§4) |
| 14 | reserved set | `reserved('set', a)` | `a %set` | proposed (§5) |
| 15 | end of input | `eof()` | `$` | proposed (§5) |
| 16 | explicit symbol | `sym(name)` / `$name` | — | keep: needed when a rule name is a DSL function (§2b) |
| 17 | precedence level | `prec.left(v, a)` per production | `%precedence left "v" = 't';` | **done** (§6) |

`@` was chosen over `^`, `->` and `as`: it is not a keyword, it is not a
delimiter that already means something, and postfix keeps the production
readable left to right — the annotation sits where Lemon puts `[LPAREN]`:

```
stat: … | funccall @right(call);
exp:  … | 'not' exp @unary | exp '+' exp @left(add);
```

That is also the answer to F1 in `lua53/DOGFOODING.md`: tree-sitter marks a
*definition*, so a per-use mark has to desugar to wrapping the reference. It does
— `funccall @right(call)` becomes `PREC_RIGHT(call, SYMBOL funccall)` — and the
18 sites in the Lua port read like the source again.

Measured, both ways: `.probe/amb/{a,b}.tsg` and `lua53/grammar.tsg` before/after
compile to identical `grammar.json` (`compare.py` `ok`, 3 and 30 rules). The Lua
grammar is 201 lines, uses `@` 43 times, `()` never (nothing needed it), and
puts its lexical rules in `tokens { … }`.

---

## 2. Field labels: `k= a` — done

`field()` was the most common call after `seq` — 19 occurrences in `tsg.tsg`
alone — and it has an obvious declarative spelling, because a colon already means
"this names the thing" in `.tsg` (rule headers, `%reserved property = …`):

```tsg
rule_definition: name= identifier ':' body= _expression ';';
pair: left= 'a' right= 'b';
```

Implemented in `Parser::parse_labelled` (~30 lines, no lexer change, no lowering
change): the parser desugars `k= a` to `field('k', a)`, the same way `@` desugars
to `prec`, so the emitted JSON is identical to the call spelling and every later
pass (`precedence.rs`, `nullable.rs`) sees the call it already understood.

**Why `=` and not `:`.** The first implementation used `:`, which made
`name: element` (label) and `name: element;` (rule definition) the same prefix —
so a missing `;` in front of a new rule was silently swallowed as a label, and the
`expected `;`` error that had caught it disappeared. A test noticed. `=` costs
nothing (it is unused inside expressions) and gives the error back, now with both
readings named:

```
error: expected `;` to end the statement, but found `:` — a field label is written
       `name= element`, and if `other:` starts a new rule then the rule above is
       missing its `;`
```

Two details settled by the implementation:

- **A label names one element**, not the branch: `k= a b` is `field('k', a) b`,
  and `k= a | b` labels `a` in the first alternative only. This falls out of
  where the sugar sits (before `parse_postfix`), and it is the reading a
  left-to-right sentence wants.
- **The multiplier stays outside the label**: `k= a?` is
  `field('k', optional('a'))`; to field the optional itself write `(k= a)?`.
  Labels do not nest — `k= j= a` is an error with the grouping hint.

Payoff: `tsg.tsg` is now written in the surface it describes — EBNF plus labels
and a `tokens { … }` block; the call form remains for `--from-json`.

## 2b. Names that collide with the DSL

A grammar may declare a rule called `alias` (Ruby) or `field` (MSSQL). Call
syntax has to keep winning there — a grammar.js round trip needs `field(name,
rule)` and `SYMBOL field` in the same file, which is exactly what those two
corpus grammars contain — so the rule is referenced as `$field` and `compile`
warns once, at the definition:

```
warning: rule `field` has the same name as the DSL function `field(…)`
  --> grammar.tsg:180:1
   |
180 | field: exp | NAME '=' exp;
   | ^^^^^
   |
   = help: `field (` reads as a call, so reference the rule as `$field` or rename it
```

EBNF cannot remove this one: `field ((',' | ';') field)*` and `field(name, expr)`
are the same token sequence until the closing paren. The declarative fix is
naming, not syntax — the DSL function names are a fixed, small, documented list,
so a grammar can simply avoid them (Lua's `field`/`fieldlist` are the only
near-miss in the corpus). What is *not* acceptable is the status quo ante: an
arity error about a call the author never wrote.

## 3. Aliases: `a ^'k'` / `a ^rule`

`alias` (7 uses) attaches tree-sitter's node-name override. Postfix `^` with the
same quoting rule as everywhere else — quoted is an anonymous name, bare is a
rule name:

```tsg
exp: identifier ^'variable' | '(' exp ')' ^'group';
```

Alternatives considered: `as` and `->` are keywords/arrow noise; `@` is taken;
`^` is the only free postfix. Cost: ~40 lines (parser + the existing alias
lowering, unchanged). Low risk, low priority.

## 4. Lexical rules: `tokens { … }` — done

`token()` matters for one thing: pinning that a rule is *lexical*, i.e. one token
with no rule references and no automatic extras. The section gives that a name:

```tsg
tokens {
    comment: /--[^\n]*/;        // one rule = one token
    keyword: 'if' | 'then';      // one token, two spellings
    NUMBER: '-' /[0-9]+/ @immediate;
}
```

Three properties of the spelling matter:

- **One rule is one token**, so a sequence or choice is wrapped for you:
  `keyword: 'if' | 'then';` is `token(choice('if', 'then'))`, not a nonterminal
  that happens to match strings. `token(…)` outside the section still means the
  same thing; the section desugars to it in the parser, so `grammar.json` and
  `parser.c` are unchanged (verified by compiling both spellings and diffing).
- **File order is priority order.** An equal-length match is resolved by the
  earlier rule, so the section can express priority instead of leaving it
  implicit. A block may sit before the start rule without becoming the start
  rule: the first rule *not* declared in `tokens` is the grammar's start.
- **`@immediate` is the spelling of `token.immediate(…)`** and applies to the
  whole rule body.

The check that the body contains only strings and patterns runs in `lexing.rs`,
where the source is still in hand: a reference is reported at the reference with
the rule named, and a zero-width body (`''`, `()`, `x?`) is reported at the part
that makes it empty. `%word` and `%extra` use the same analysis, so a non-token
word or an extra that starts with a nonterminal is a source error rather than a
position-free generator message.

## 5. `$` for end of input, `%set` for reserved words

`eof()` has an obvious symbol, and the lexer already knows `$` (`$ident` is the
explicit symbol form), so `$` alone should be `EOF`:

```tsg
source_file: _item* $;
```

`reserved('property', a)` reads badly because it names a set that `%reserved`
already named. Suffix `%`: `identifier %property`. Two uses in the examples; do
it only if `%reserved` survives contact with real grammars.

---

## 6. The bigger win: precedence from terminals — **done**

Marking 43 productions by hand is still a program, not a grammar. Bison/Lemon
solve it by declaring precedence on the *terminal* and letting each production
inherit the level of its rightmost terminal. That is the declarative spelling,
and it is what `%precedence` now is:

```tsg
%precedence left  "or";
%precedence left  "and";
%precedence left  "compare" = '<' '<=' '>' '>=' '==' '~=';
%precedence left  "add"     = '+' '-';
%precedence right "concat"  = '..';
%precedence right "pow"     = '^';
```

(The `%precedence left "or";` lines above declare levels with no terminals — a
name that only `@` uses. That spelling, `assoc` without `=`, is what separates a
level from a bare ordering list; see `SYNTAX.md`.)

Then Lua's `exp` block loses every annotation except where the default is wrong:

```tsg
exp:
    'nil' | 'true' | 'false' | INTEGER | FLOAT | STRING | '...'
  | function | table | var | funccall
  | 'not' exp @unary | '#' exp @unary | '-' exp @unary | '~' exp @unary
  | exp 'or'  exp | exp 'and' exp
  | exp '<' exp  | exp '<=' exp | exp '>' exp | exp '>=' exp
  | exp '==' exp | exp '~=' exp
  | exp '|' exp  | exp '~' exp | exp '&' exp
  | exp '<<' exp | exp '>>' exp
  | exp '..' exp | exp '+' exp | exp '-' exp
  | exp '*' exp | exp '/' exp | exp '//' exp | exp '%' exp
  | exp '^' exp
  | '(' exp ')' @right(call);
```

The table is stated once, as a table — and `lua53/grammar.tsg` is exactly this
now: 43 annotations became 21 inherited marks and 20 explicit ones, and the
emitted `grammar.json` is **byte-for-byte identical** to the annotated spelling
(`compare.py ok`, 30 rules), which is how the inference is tested.

How the design questions were answered:

1. **Does the terminal inherit or the production?** The *production*: the
   rightmost terminal of each alternative picks the level, and the rewrite wraps
   that alternative. `a '+' b | a '-' b` shares a level anyway, which is why a
   level carries one name rather than a set.
2. **How is it overridden?** An explicit `@` always wins. That is the whole
   answer to "`'-' exp` must be unary, not add" — no `@none` marker is needed,
   because the common case is *replacing* an inherited mark, not clearing one.
   Clearing is not expressible today, and `@0` does not do it: in
   `compare_precedence` (`build_parse_table.rs:1409`) an integer precedence is
   only compared with another integer or with `None`, so `prec(0, …)` against a
   *named* level is `Equal`, not lower. If "inherit nothing here" ever comes up,
   it needs a real marker.
3. **Equal levels.** One level, one name. Names stay explicit because they are
   the unit of comparison; the *terminals* are what the author now writes.
4. **Rightmost *nonterminal*.** No terminal, no inheritance: `stat: 'do' block
   'end'` and `funccall: var args` inherit nothing, so the latter keeps its
   `@right(call)`. This is the one place where yacc's model does not cover
   tree-sitter: Lemon's `. [LPAREN]` marks a *use* of a non-terminal, which is
   what `@` does, and only `@` can express it.

It is front-end-only (the JSON is unchanged: the same `PREC_LEFT` nodes,
inferred instead of typed), which is why it could land before the integration.

The one thing to be careful about: **direction.** `grammar.json` lists
precedences highest first, and a level directive joins that chain in declaration
order — so the first line wins. Yacc writes the opposite way (`%left '+'` before
`%left '*'`), so a translator's first attempt will invert the table and *parse
fine with the wrong shape*. Two mitigations: `tsg -p` prints the levels in
order with the productions that inherit each, and a level names itself, so a
comparison is by name and never positional.

---

## 7. Feedback: explain the grammar, not the automaton

`tsg` holds the source and the spans, the generator holds the item sets and the
conflict reasons, and today the two never meet — which is the whole L1.5 story in
`INTEGRATION.md`. Three additions, cheapest first.

### 7.1 Say which construct is responsible

The Lua port's real failures were *not* mysterious automaton states, they were
one-line mistakes with a source position:

* `%precedence call pow …;` — a rule reference where a name was meant. Now
  rejected by `tsg` itself, at the directive, with the fix inline. **Done.**
* `block: …` matching the empty string — 18 use sites had to say `?`. `tsg` can
  decide nullability exactly, so this should never reach the generator. The
  message should name the construct: *"`block` can match nothing, so `block` at
  line 51 must be `block?` — or, if you meant a production that produces no
  nodes, `%inline`"*.
* a rule named after a DSL function — warned about at the definition, with the
  `$name` spelling in the help. **Done** (§2b), and it exists because
  `roundtrip.sh` caught the opposite choice: 2 of 92 corpus grammars use both
  meanings, so "the rule shadows the function" is not available.
* `stat • '('` conflict — the elaborator names both rules, but the useful
  sentence is about the annotation: *"`funccall @right(call)` makes calls
  right-associative; `'(' exp ')' @right(call)` in `exp` competes at the same
  level. Raise `call` above `pow` in `%precedence`, or accept it with
  `%conflicts stat funccall;`"*.

Each of those is a `match` arm over the generator's structured output plus one
lookup in the `Index` — no new dependency, no change to `generate.rs` beyond
handing the index over (Level 1).

### 7.2 A precedence table you can read — **done**

`%precedence` was write-only: the order is a descending chain, levels not
mentioned together are incomparable, and nothing showed which productions landed
where. `tsg -p grammar.tsg` now prints it:

```
precedence, highest first
  right call
    (none)
      89:5  stat: funccall @right(call)
     122:5  exp: var @right(call)
  right pow
    '^'
     151:5  exp: exp '^' exp
  left mul
    '*' '/' '//' '%'
     147:5  exp: exp '*' exp
     148:5  exp: exp '/' exp
...
21 inherited, 20 explicit (`@`), 13 level(s)
```

Every level, the terminals it owns, every production that landed there (with the
line to look at), and the split between inherited and hand-marked. It also
validates the directives, so `tsg -p` and `tsg` report the same mistakes.

### 7.3 Lexer modes you can explain — first half done

`tsg --explain-modes grammar.tsg` (alias `--explain-lexing`) prints the token
priority list, which rules are `%word`/`%extra`/immediate/external, and the source
of each body:

```
lexer modes: token priority (equal-length matches: the earlier rule wins)
   1  regex           —             token(/\/([^/\\\n]|\\.|\[([^\]\\]|\\.)*\])+\/(i)?/)
   2  integer         —             token(/-?[0-9]+/)
   3  identifier      word          /[A-Za-z_][A-Za-z0-9_]*/
```

This is the lexical side of the answer: the rules, their order, and the markers
that change how they are lexed. The *per-parse-state* lexer states are a
different table — the generator derives one DFA start state per distinct set of
valid tokens and records it in `ts_lex_modes[state].lex_state`; those sets come
from `build_tables`, not from the front end. A later integration can append that
column via `tree-sitter generate --report-states-for-rule -` without changing the
syntax or the priority report.

### 7.4 LALR debugging that speaks `.tsg`

For genuine LR questions — *why* is this state a conflict — the useful view is
the item set annotated with source spans:

```
$ tsg --explain state 42 grammar.tsg
  exp → exp '+' exp •                     grammar.tsg:118:5  @left(add)
  exp → exp • '+' exp                     grammar.tsg:118:5
  lookahead '+'  →  shift, because '@left' prefers shifting
```

`tsg --explain conflict N` and `--explain rule NAME` are the ones worth having;
`state` is a debugging aid for grammar authors who know LR. The substrate is
`build_parse_table.rs`'s item sets, which are already in the summary envelope
under `possible_interpretations`/`precedence`.

### 7.5 `%conflicts` with a reason

One conflict in the Lua grammar is intentional, and the reason lives in a
comment that `UnnecessaryConflicts` will delete the moment a refactor makes it
moot:

```tsg
%conflicts fieldlist, "a trailing ','/'`;`' before '}' is not LR(1)-decidable";
```

---

## 8. Order of work

1. **`to_tsg` gains an EBNF style** (default or `--style ebnf`). This is the
   leverage move: 17 real grammars and `examples/tsg.tsg` start exercising `|`,
   `?`, `*`, `+`, grouping, `()` and `@` on every `roundtrip.sh` run instead of
   only the call path — which is how the two parser bugs in §1 were found, on the
   first file that used a real group. Add a `tsg → json → tsg` idempotence test at
   the same time.
2. ~~Field labels `k= a`~~ — **done** (§2). `tsg.tsg` is now written in the
   surface it describes; its lexical rules are a `tokens { … }` block.
3. ~~Nullability pre-emption (§7.1)~~ — **done**: a rule that can match nothing
   is reported at the alternative responsible, with the use sites in the help,
   before `grammar.json` exists.
4. ~~Precedence from terminals (§6)~~ — **done** (§6).
5. ~~`--precedences` (§7.2)~~ — **done**; construct-aware conflict help (§7.1)
   is next, and is the reason the *integration* exists.
6. ~~`tokens { … }` (§4)~~ — **done**. What remains is polish: `$` (§5),
   `^` aliases (§3), `%set` reserved words (§5), then the real lexer states in
   `--explain-modes` (§7.3).

## 9. Open questions

1. Does the call form stay a *first-class* surface (authorable, tested) or
   become an import-only dialect that `to_tsg` emits? I lean import-only, with
   `--from-json --style ebnf` removing the last reason to type `seq(`.
2. ~~`@v` where `v` is a bare name~~ — **answered by §6**: a *level* requires its
   name to be quoted, and says so (`\`add\` names a rule, so it cannot be a
   precedence level`), so bare = rule reference and quoted = precedence name hold
   in both places. `@v` stays permissive because it is a *use* site, and the
   common `@unary` should not need quotes; the checking happens where the name is
   declared, once.
3. `@` postfix applies to a whole alternative. Is a *mid-sequence* annotation
   needed? (`a (b c @left(x)) d` works today via grouping.) I have not needed
   one.
4. Should `()` also work as an alternative (`a | ()`), i.e. is the blank ever
   wanted outside `?`-desugaring? Currently yes, it parses.
