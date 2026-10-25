<!-- generated: cargo run --offline > OUTPUT.md -->

```console
$ cargo run --offline


┌─ conflict ─────────────────────────────────────────────────
│ %extras none;
│ 
│ file: expr;
│ expr: expr "+" expr
│     | expr "*" expr
│     | number;
│ number: /[0-9]+/;
└──────────────────────────────────────────────────────────
error: unresolved conflict in rule `expr`
 --> .run/conflict.tsg:4:1
  |
4 | expr: expr "+" expr
  | ^^^^ `expr` is reduced here, `'+'` is the lookahead
  |
  = help: give `expr` an associativity: `prec.left(…)` or `prec.right(…)`, or accept the ambiguity with `%conflicts expr;`

┌─ indirect ─────────────────────────────────────────────────
│ %extras none;
│ 
│ file: _a;
│ 
│ _a: _b;
│ _b: _c;
│ _c: _a;
└──────────────────────────────────────────────────────────
error: indirectly recursive rule: _a -> _b -> _c -> _a
 --> .run/indirect.tsg:5:1
  |
5 | _a: _b;
  | ^^ cycle starts here
6 | _b: _c;
  | ^^ `_b` leads back to the start
7 | _c: _a;
  | ^^ `_c` leads back to the start
  |
  = help: the cycle consumes nothing, so it can never terminate; make one production consume a token

┌─ undeclared_prec ─────────────────────────────────────────────────
│ %extras none;
│ 
│ file: expr;
│ expr: prec("unary", seq("-", expr))
│     | number;
│ number: /[0-9]+/;
└──────────────────────────────────────────────────────────
error: precedence `unary` is used in rule `expr` but never declared
 --> .run/undeclared_prec.tsg:4:7
  |
4 | expr: prec("unary", seq("-", expr))
  |       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ `expr` uses `unary`
  |
  = help: declare it before using it: `%precedences unary "…" "…";`

┌─ word_token ─────────────────────────────────────────────────
│ %extras none;
│ %word kw;
│ 
│ file: kw;
│ kw: seq("i", "f");
└──────────────────────────────────────────────────────────
error: `kw` cannot be the word token: it is a non-terminal
 --> .run/word_token.tsg:2:7
  |
2 | %word kw;
  |       ^^ the word token must be a single token
 ...
5 | kw: seq("i", "f");
  | ^^ `kw` is not a token
  |
  = help: point `%word` at a token, e.g. `kw: /…/;` instead of a `seq`

┌─ empty_rule ─────────────────────────────────────────────────
│ %extras none;
│ 
│ file: _a;
│ _a: optional("x");
└──────────────────────────────────────────────────────────
error: rule `_a` can match nothing, which tree-sitter cannot parse
 --> .run/empty_rule.tsg:4:5
  |
4 | _a: optional("x");
  |     ^^^^^^^^^^^^^ rule `_a`

┌─ empty_extra ─────────────────────────────────────────────────
│ %extra ws;
│ ws: /\s*/;
│ 
│ file: "x";
└──────────────────────────────────────────────────────────
error: an extra token that matches the empty string would loop forever
 --> .run/empty_extra.tsg:1:8
  |
1 | %extra ws;
  |        ^^ this extra can match nothing
2 | ws: /\s*/;
  |     ^^^^^ the empty match comes from here
  |
  = help: require at least one character: `+` instead of `*`, or drop the optional part

┌─ unary ─────────────────────────────────────────────────
│ file: expr;
│ expr: choice(seq("x"));
└──────────────────────────────────────────────────────────
(the generator accepted it)
warning: `seq` with a single element in rule `expr`
 --> .run/unary.tsg:2:1
  |
2 | expr: choice(seq("x"));
  | ^^^^  ^^^^^^^^^^^^^^^^ a `seq(…)` wrapper around one element does nothing
  |
  = help: drop the `seq(…)` wrapper

┌─ supertype_inline ─────────────────────────────────────────────────
│ %supertypes _expr;
│ %inline _expr;
│ 
│ file: _expr;
│ _expr: "x";
└──────────────────────────────────────────────────────────
(the generator accepted it)
warning: `_expr` is both a supertype and inlined, so the supertype is ignored
 --> .run/supertype_inline.tsg:1:13
  |
1 | %supertypes _expr;
  |             ^^^^^ declared as a supertype
2 | %inline _expr;
  |         ^^^^^ but also inlined, which wins
  |
  = help: remove it from one of the two lists

┌─ needless_conflicts ─────────────────────────────────────────────────
│ %conflicts expr;
│ 
│ file: expr;
│ expr: "x" | "y";
└──────────────────────────────────────────────────────────
(the generator accepted it)
warning: these `%conflicts` are unnecessary
 --> .run/needless_conflicts.tsg:1:12
  |
1 | %conflicts expr;
  |            ^^^^ `expr` does not need it
  |
  = help: remove them; a conflict that does not exist hides the ones that do
```
