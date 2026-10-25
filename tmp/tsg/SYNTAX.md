# The `.tsg` grammar language

`.tsg` is a declarative, bison/flex-flavoured surface syntax for tree-sitter
grammars. It compiles to the same `grammar.json` that `grammar.js` produces, so
the entire existing backend (`parse_grammar` → `prepare_grammar` →
`build_tables` → `render`) is reused unchanged.

```
grammar.tsg  ──tsg──▶  src/grammar.json  ──tree-sitter generate──▶  src/parser.c
```

Nothing in the `.tsg` pipeline depends on JavaScript, node, or any crate outside
`std`.

---

## 1. File structure

A file is a sequence of **items**, each terminated by `;`:

| Item | Form | Emitted as |
| --- | --- | --- |
| grammar name | `%name json;` | `"name"` |
| start rule | `%start source_file;` | `"start"` |
| parent grammar | `%inherits javascript;` | `"inherits"` |
| keyword token | `%word identifier;` | `"word"` |
| extra token | `%extra /\s/;` | entry in `"extras"` |
| no extra tokens | `%extras none;` | `"extras": []` |
| external token | `%external heredoc;` | entry in `"externals"` |
| balanced external token | `%external body = delimited('{', '}');` | entry in `"externals"`, plus a generated `src/scanner.c` |
| inlined rules | `%inline _expr _type;` | `"inline"` |
| supertypes | `%supertypes _expr;` | `"supertypes"` |
| GLR conflict set | `%conflicts expression type;` | entry in `"conflicts"` |
| precedence ordering | `%precedence member_expression "and" "or";` | entry in `"precedences"` |
| precedence level | `%precedence left "add" = '+' '-';` | entry in `"precedences"`, marks its terminals |
| reserved word set | `%reserved property = 'var', 'let';` | entry in `"reserved"` |
| empty reserved set | `%reserved property = ;` | `"property": []` |
| lexical rules | `tokens { NAME: /[A-Z]+/; }` | one `TOKEN`/`IMMEDIATE_TOKEN` per rule |
| rule definition | `identifier: /[a-z]\w*/;` | entry in `"rules"` |

Comments are `// line` and `/* block */`. Strings may be written with `'` or
`"` and understand the usual escapes (`\n`, `\t`, `\\`, `\'`, `\xHH`,
`\uXXXX`, `\u{...}`, UTF-16 surrogate pairs).

**Rule order decides the start rule only when nothing else does**: `%start
source_file;` names the entry point, and the name travels to `grammar.json` as
`"start"` (the generator promotes it to `variables[0]`). Without `%start` the
first rule *not* declared in a `tokens` block is the start rule, which is what
Bison's `%start` and the DSL's rule order both come down to. Rule order is still
preserved in `grammar.json`, and inside a `tokens` block it is token **priority**
order: when two tokens match the same length, the earlier rule wins.

Declaring `%start` is what keeps a `tokens` block free to move: reordering token
rules for priority can no longer silently change which rule a tree starts at.
The named rule has to be a rule, not a token — a token cannot start a tree, and
`tsg` says so with the directive's position.

If no `%extra` is declared, `extras` defaults to `[{"type":"PATTERN","value":"\\s"}]`,
exactly like `dsl.js`. `%extras none;` is the way to ask for a genuinely empty
list (the DSL's `extras: _ => []`); it cannot be combined with `%extra`.

### External scanners

`%external heredoc;` is a promise: the token comes from `src/scanner.c`, which
the author writes. `%external body = delimited('{', '}');` is the same promise
with the scanner generated for you — `tsg -o src/grammar.json` writes
`src/scanner.c` beside the JSON:

```tsg
%name jsonc;

// `{ … }` is one token: the scanner keeps its own nesting depth.
%external action_code = delimited('{', '}');

document: action_code+;
```

The delimiters are one character each (in `'…'` or `"…"` form). A scanner sees
one code point at a time and cannot look ahead, so a two-character delimiter has
no meaning there; asking for one is an error, not a silently different scanner.

What the generated scanner does, exactly:

* It runs **before** tree-sitter skips extras, so it skips whitespace itself
  (`\t \n \r \f \v` and space) before looking for the opening delimiter.
  Extras it cannot reproduce — a comment in particular — are **reported** at
  compile time with the tokens that are affected: a `{` that comes right after
  a `/* … */` is not recognised, because the scanner never sees it. Put the
  delimiter where only whitespace can precede it, or write `src/scanner.c`.
* It counts nesting, so `{ a { b } c }` is one token, and the depth is a local
  variable: a delimited region is one token, so nothing has to be serialized
  and no state survives between tokens.
* It does **not** understand the region's contents. A `}` inside a string
  literal or a comment counts as a closing brace, so `{ "}" }` ends early. A
  region whose contents have their own lexical structure needs a hand-written
  scanner (`%external body;`).
* An unterminated region ends at end of input and is returned as a token; the
  parser then rejects the missing closing brace with a normal `MISSING`/`ERROR`
  node, which is where that diagnostic belongs.
* The generated file starts with `// Generated by tsg` and `tsg` refuses to
  overwrite any `scanner.c` that does not: a hand-written scanner is never
  clobbered. Without `-o`, no scanner is written (and `tsg` says so), because
  it would not land next to the `grammar.json` that `tree-sitter generate` reads.

An external token declared **without** a scanner and without a hand-written
`src/scanner.c` prints a note, because the token then never matches and every
production that needs it is dead: `tsg` says so instead of letting the parser
fail at parse time.

---

## 2. Expressions

| Syntax | Meaning | JSON |
| --- | --- | --- |
| `'if'` | string literal | `STRING` |
| `/\[0-9\]+/` | regexp; flags allowed, e.g. `/[a-z]/i` | `PATTERN` |
| `identifier` | reference to another rule | `SYMBOL` |
| `$identifier` | same, explicit form | `SYMBOL` |
| `a b c` | concatenation | `SEQ` |
| <code>a &#124; b</code> | alternation | `CHOICE` |
| `a?` | zero or one | `CHOICE` [`a`, `BLANK`] |
| `a*` | zero or more | `REPEAT` |
| `a+` | one or more | `REPEAT1` |
| `(a b)` | grouping; the nested sequence is preserved | — |
| `()` | the blank rule (an empty alternative) | `BLANK` |
| `a @name` | rule precedence | `PREC` |
| `a @3` | integer rule precedence | `PREC` |
| `a @left(name)` | left associativity (`@left(3)` for an integer) | `PREC_LEFT` |
| `a @right(name)` | right associativity | `PREC_RIGHT` |
| `a @dynamic(3)` | dynamic precedence | `PREC_DYNAMIC` |
| `a @immediate` | immediate token (inside `tokens { … }`) | `IMMEDIATE_TOKEN` |
| `k= a` | field label on one element | `FIELD` |

Postfix operators bind tighter than juxtaposition, which binds tighter than
`|`:

```tsg
// Parsed as: 'let' (identifier | _pattern)? '=' (_expr ';')*
statement: 'let' identifier? '=' _expr ';'*
```

Juxtaposition and grouping are *structural*: `(a b)` stays a nested sequence,
just as `seq('a', seq('b', 'c'))` does, because that is what `dsl.js` emits.
tree-sitter's own `normalize` pass flattens both later, so the two spellings are
equivalent to the parser; keeping the nesting makes `grammar.json → .tsg →
grammar.json` lossless.

**EBNF is the primary surface.** The call form exists so that
`grammar.json → .tsg → grammar.json` is lossless and so that a `grammar.js` can be
translated mechanically; it is the same constructs, so the two may be mixed
(`a | seq(b, c)`, `prec.left(p, a b)`).

A `@`-annotation is postfix and applies to a whole alternative, because that is
what a production's precedence is — `exp '+' exp @left(add)` marks the sum, not
the last `exp`. Annotating a part of a sequence takes grouping:
`a (b c @left(x))`.

```tsg
%precedence "unary" "add";
exp:
    NUMBER
  | '-' exp @unary
  | exp '+' exp @left(add);
```

`@` is sugar: it lowers to `prec`, `prec.left`, `prec.right` and `prec.dynamic`,
so the emitted `grammar.json` is identical to the call spelling.

### Field labels

`k= a` names the element to its right, and is the declarative spelling of
`field('k', a)`:

```tsg
rule_definition: name= identifier ':' body= _expression ';';
pair: left= 'a' right= 'b';                  // field('left','a') field('right','b')
access: base= _expr '.' member= _expr;      // one label per element
```

A label binds to **one element**, not to the rest of the branch, so
`k= a b` is `field('k', a) b` and `k= a | b` labels `a` in the first alternative
only. Multipliers written after the label stay outside it — `k= a?` is
`field('k', optional('a'))` — and grouping moves them: `(k= a)?` is
`optional(field('k', 'a'))`. Labels do not nest (`k= j= a` is an error); group the
inner one instead.

The `=` is deliberate. `name: body;` is a rule definition, so a field label
spelled `name: body` would make a *missing* `;` indistinguishable from a label.
With `=` the two spellings differ, and the mistake keeps its error — the message
names both readings:

```
error: expected `;` to end the statement, but found `:` — a field label is
       written `name= element`, and if `other:` starts a new rule then the rule
       above is missing its `;`
```

### Precedence levels

Writing the level on every production is the housekeeping a yacc grammar does
not do, because yacc derives a production's precedence from its **rightmost
terminal**. A level can say which terminals it owns, and then the annotations
are not needed:

```tsg
%precedence right "pow"     = '^';
%precedence left  "mul"     = '*' '/' '%';
%precedence left  "add"     = '+' '-';
%precedence left  "compare" = '<' '<=' '>' '>=';

exp:
    NUMBER
  | exp '^' exp        // → prec.right("pow", …)
  | exp '*' exp        // → prec.left("mul", …)
  | exp '+' exp        // → prec.left("add", …)
  | exp '<' exp        // → prec.left("compare", …)
  ;
```

Four rules:

* **Levels are a chain, highest first.** All level directives join one ordering,
  so every level is comparable with every other, which is what makes
  `'*'` outrank `'+'`. (A directive *without* `left`/`right` and without `=`
  is not a level: `%precedence "a" "b";` is a bare *ordering*, a separate list,
  kept because that is what `--from-json` prints and what a `grammar.json`
  with deliberately incomparable groups looks like.)
* **The rightmost *terminal* decides**, as in yacc — not the last symbol. In
  `exp '+' exp` that is `'+'`; in `'-' exp` it is `'-'`; in `funccall args` it
  is nothing, because `args` is a rule, so that production needs `@right(call)`
  if it needs a mark at all. A production with an explicit `@` keeps it: the
  annotation always wins.
* **A level can own no terminals**: `%precedence right "call";` (or
  `%precedence "call" = ;`) puts a name that is only ever used with `@` in its
  place in the chain. The associativity keyword is what distinguishes it from a
  bare ordering list, so a single quoted name is never ambiguous.
* **`nonassoc` is refused.** tree-sitter has no non-associative precedence —
  `prec` without associativity resolves conflicts by precedence alone, so
  `a < b < c` would still parse. Say so instead of pretending.

`-` and `~` are the interesting case, and it is yacc's own trap: `%left "add"
= '+' '-';` gives `exp '-' exp` the add precedence *and* gives `'-' exp` the
same one. When the unary production has to differ, name its level:
`| '-' exp @unary`. That is why Lua has 4 annotations left and 21 inherited.

`tsg -p grammar.tsg` prints the resolved table — every level, the terminals it
owns, and the productions that inherited it, with line numbers. Use it when a
parse is surprising; a precedence mistake is otherwise invisible in the JSON.

### Function calls

`dsl.js` functions are available with the same names and the same argument
order, so translating a `grammar.js` is mechanical. `name (` is always a call
when `name` is one of these functions, so a grammar that declares a rule with one
of these names (Ruby has `alias`, MSSQL has `field`) must reference it
explicitly:

```tsg
fieldlist: $field ((',' | ';') $field)*;   // the rule; `field (…)` would be a call
field: NAME '=' exp;
```

`compile` reports the collision once, as a warning at the rule definition.

| Call | JSON | Notes |
| --- | --- | --- |
| `seq(a, b, …)` | `SEQ` | nested `seq` calls are *not* flattened |
| `choice(a, b, …)` | `CHOICE` | one argument keeps a one-member `CHOICE` |
| `optional(a)` | `CHOICE` [`a`, `BLANK`] | |
| `repeat(a)` / `repeat1(a)` | `REPEAT` / `REPEAT1` | |
| `blank()` | `BLANK` | |
| `eof()` | `EOF` | |
| `prec(n, a)` | `PREC` | `n` is an integer or a precedence name |
| `prec.left([n,] a)` | `PREC_LEFT` | `n` defaults to `0` |
| `prec.right([n,] a)` | `PREC_RIGHT` | `n` defaults to `0` |
| `prec.dynamic(n, a)` | `PREC_DYNAMIC` | integer only |
| `token(a)` | `TOKEN` | import form; `a` may not contain rule references |
| `token.immediate(a)` | `IMMEDIATE_TOKEN` | import form; prefer `a @immediate` in `tokens` |
| `alias(a, 'name')` | `ALIAS` with `"named": false` | string ⇒ anonymous |
| `alias(a, name)` | `ALIAS` with `"named": true` | rule name ⇒ named |
| `field(name, a)` | `FIELD` | field name may be bare or quoted |
| `reserved('set', a)` | `RESERVED` | contextual keyword set |
| `sym(name)` | `SYMBOL` | escape hatch for awkward names |

`%inherits` names the parent grammar. tree-sitter's Rust pipeline ignores the
key — the JS DSL flattens a base grammar before serialising — but the schema
defines it, so it is preserved rather than silently dropped.

A **precedence name** is a string literal or a bare name in the value position:

```tsg
binary_expression:
    prec.left('or', seq(expression, '||', expression))
  | prec.left(1,  seq(expression, '&&', expression));
```

A bare number is only valid as a precedence value; anywhere else it is a
compile error with a span.

---

## 3. Worked example

`test/fixtures/test_grammars/readme_grammar/grammar.js`:

```js
export default grammar({
  name: 'readme_grammar',
  extras: $ => [/\s/, $.comment],
  rules: {
    expression: $ => choice($.sum, $.product, $.number, $.variable,
                            seq('(', $.expression, ')')),
    sum: $ => prec.left(1, seq($.expression, '+', $.expression)),
    product: $ => prec.left(2, seq($.expression, '*', $.expression)),
    number: _ => /\d+/,
    comment: _ => /#.*/,
    variable: _ => new RustRegex('(?i:[a-z])\\w*'),
  },
});
```

`examples/readme_grammar.tsg`:

```tsg
%name readme_grammar;
%extra /\s/;
%extra comment;

expression: sum | product | number | variable | '(' expression ')';
sum: prec.left(1, seq(expression, '+', expression));
product: prec.left(2, seq(expression, '*', expression));
number: /\d+/;
comment: /#.*/;
variable: /(?i:[a-z])\w*/;
```

Both produce an identical `src/parser.c`.

---

## 4. Diagnostics

Because every token and AST node carries a byte span, errors point at the exact
text:

```text
error: undefined rule `identifer` (did you mean `identifier`?)
  --> grammar.tsg:3:10
   |
 3 | file: identifer;
   |       ^^^^^^^^^
```

Reported statically, before any table is built:

* lexer: unterminated strings / regexes / comments, bad escapes, stray characters
* parser: missing `;` or `:`, unbalanced parentheses, unknown directives,
  duplicate rules (both locations are named), trailing garbage
* lowering: wrong arity per function, non-integer `prec.dynamic`, invalid field
  names, empty `%extra`, rule references inside `token(...)` or a `tokens` block,
  a token that can match the empty string, an `%extra` that does not start with
  a token, a `%word` that names a nonterminal, regex flags that
  cannot do anything (`/x/g`, `/x/u`, `/x/s`, …) or that repeat `i`
* resolution: undefined rules (with a "did you mean"), undefined names in
  `%word` / `%inline` / `%supertypes` / `%conflicts`, and a `%start` that names
  no rule or names a token

Conflicts that only appear once tables are built are still reported by
tree-sitter itself, unchanged.

Diagnostics are rustc-shaped (`error: …`, `--> file:line:col`, a gutter, a
caret span, occasionally a `= help:` line) because every token and every AST node
carries a `Span`. Flags that tree-sitter would accept and ignore get a hint about
what to write instead; `--allow-ignored-flags` turns those errors into warnings
and passes the flags through untouched, for reproducing `grammar.js` byte for
byte. A parse
error stops the run, since there is nothing to recover to; everything after the
parse — undefined rules, bad call arity, bad field names, `%inline`/`%conflicts`/
`%word` arguments that name nothing — is collected per rule and reported
together, top to bottom, at most 10 at a time.

---

## 5. The language describes itself

`examples/tsg.tsg` is a tree-sitter grammar for this syntax, written in this
syntax; `./selfhost.sh` compiles it, builds a parser from the result and parses
every `.tsg` file in the repository with it — including `lua53/grammar.tsg`,
which uses the precedence levels, `@` and `()`, and `surface/*.tsg`, which spell
every directive form and expression shape once. Keeping the self-description
current is a test, not a chore: `surface/` must compile *and* parse, and
`coverage.py` requires every named node in the description's own
`node-types.json` to appear in those parses. This is also how the last three
front-end bugs were found (a `name (group)` ambiguity, a stale `%precedence`
rule, and `%precedence "x" = ;` being rejected) — by pointing the description at
a spelling it had never been asked to read.

The description is written in the same surface it describes: EBNF plus field
labels and a `tokens { … }` block for its lexical rules (`DECLARATIVE.md` §4).
It has 41 rules and is checked in both directions: `surface/*.tsg` must compile
*and* parse, and every named node type in its `node-types.json` must occur in
those parses.

Its `regex` rule is worth a look if you write a grammar for a lexer with regex
literals:

```tsg
tokens {
    regex: /\/([^/\\\n]|\\.|\[([^\]\\]|\\.)*\])+\/(i)?/;
}
```

The flags are part of the literal, so the token cannot match across whitespace:
`/x/i` is one token and `/x/ i` is a regex followed by the symbol `i`, which is
exactly how `lexer.rs` reads it. `(i)?` rather than `i?` only because `i?` after
the closing `/` would read as a flag on this literal itself.

The obvious tidier spelling — `regex: seq(token(/…\//), optional(regex_flags))`
with `regex_flags: token.immediate(/i/)` — is worse, and measurably so. A token
never crosses whitespace, so with a *wide* class the split behaves
(`/x/ i` gives a symbol), but with the class narrowed to `i`, tree-sitter's
lexer reads `/x/ i` as a flag, which the compiler reads as a symbol. The grammar
would then disagree with `tsg` about a **valid** file, which is the one thing a
self-description must not do.

The class is `i`, not a letter run: this file is the documentation, and
`[a-zA-Z]*` would claim that any letter run is a valid flag. A wider class does
not buy a rejection either — `/x/g` parses as `/x/ g` under any class, because
juxtaposition allows a symbol after a regex and a token pattern cannot see
whitespace. So the grammar accepts a superset, and the narrowing happens where
it can point at a character:

* `tsg` accepts no flags or a single `i` and refuses the rest with a span and a
  per-flag hint (`write \`[\\s\\S]\` where you meant a dot that matches
  newlines`); a flags run that is also a rule name gets the space hint instead
  (`if you meant the rule \`g\`, put a space after the literal: \`/…/ g\``).
  `--allow-ignored-flags` downgrades this to a warning, which is how `grammar.js`
  output stays reproducible byte for byte.
* nothing else: `queries/highlights.scm` cannot flag a bad flag, because with
  the literal owning its flags there is no separate flags node to capture — and
  an `@error` rule that can never match is exactly the kind of silent no-op this
  language exists to remove.

`selfhost.sh` checks the escape forms, the refusals, the escape hatch, the
flags/symbol boundary (`/x/i` one token, `/x/ i` a symbol) and that the shipped
queries still compile against the grammar.

## 6. The other direction

`tsg --from-json grammar.json` (or `-r`) runs the compiler backwards and prints
a `.tsg` file. It exists so that the ~140 existing grammars in
`test/fixtures/test_grammars/` can be migrated mechanically instead of by hand,
and so that a hand-written `.tsg` can be checked for drift.

The output is *not* meant to be hand-maintained: it has no comments, it spells
`SEQ`/`CHOICE` explicitly when nesting requires it, and it prints every directive
whether or not it is interesting. An absent collection key (`precedences`,
`externals`, `reserved`, …) and an empty one mean the same thing to
`parse_grammar`, so `compare.py` normalises them; everything else must match
exactly. The round trip is what matters, and `./roundtrip.sh` asserts it over
the whole corpus: every `grammar.json` in the repository — the 75 test fixtures
and the 17 real-world grammars in this directory (bash, c, cpp, dart, go, html,
java, js, kotlin, php, python, ruby, rust, swift, ts) — is fed through
`grammar.json → .tsg → grammar.json` and compared structurally.
`./roundtrip.sh --parsers` additionally byte-compares the `parser.c` built from
each side; 80 of the 92 are identical, and the 12 that are not compared are
fixtures that `tree-sitter generate` rejects on both sides by design.

A few things exist only to make the round trip lossless:

* `%extras none;` — an empty `extras` list, which has no other spelling
  (`extras` is the one collection where absent means `/\s/`, not empty).
* `%reserved name = ;` — an empty reserved word set, likewise.
* the call forms `seq(x)` and `choice(x)` — a one-member `SEQ` or `CHOICE`, which
  no surface operator can express. Real grammars contain them (`swift` has a
  one-member `CHOICE` in `inheritance_modifier`).

One thing cannot come back, and the reverse direction says so instead of
pretending: an external token's **scanner** is C, `grammar.json` only records the
name, so `%external body = delimited('{', '}')` comes back as `%external body;`
with a note. Regenerate the `.tsg` (and its `src/scanner.c`) from the `.tsg`, not
from the JSON.

## 7. Deliberate omissions

* `grammar(baseGrammar, …)` / `inherits` as a *merging* mechanism. `%inherits`
  records the parent's name for fidelity, but a `.tsg` file is always
  self-contained: no base grammar is loaded or merged. `%include` is future work.
* Arbitrary JavaScript. Regexes are the only escape hatch that survives, because
  tree-sitter only supports Rust's regex syntax anyway (`new RustRegex(...)`), and
  in `.tsg` a regex literal with inline flags covers that case.
* Computed rule construction (`Object.keys`, `map`, generator functions). Those
  are not declarative, and the point of the format is to make grammars
  analysable and diffable.
* A general lexer-state machine (Flex's `%x`, `<state>`, `<<>` push, `BEGIN`).
  `%external name = delimited(open, close)` generates a scanner for the one case
  that is genuinely inexpressible without one — a balanced region — and
  everything else stays a regex, where tree-sitter can see it. A hand-written
  `src/scanner.c` is still the answer for a region whose *contents* have lexical
  structure of their own.

---

## 8. Ambiguities, and how each one is resolved

A declarative surface is only as good as its worst ambiguity, so here is the
complete list — every place where two readings are possible, what the compiler
does, and what to write instead. Everything not in this table is decided by the
grammar without a lookahead trick.

| Spelling | Two readings | Resolution |
| --- | --- | --- |
| `name (` | the call `name(…)` vs the rule `name` followed by a group | **The call wins.** A rule whose name is also a DSL function is referenced `$name`, and `compile` warns once at its definition. Unavoidable: round-tripping `grammar.js` needs `field(name, x)` and the rule `field` in one file (Ruby, MSSQL). |
| `name: element` | a field label vs the rule definition `name: element;` | **Removed** — labels are `name= element`. The `:` form is now an error that names both readings. |
| `%precedence left "add";` | associativity `left` on the level `add` vs an ordering list whose first name is `left` | `left`/`right` are **reserved** in that position: the line is a level. To order a name spelled `left`, quote it (`%precedence "left" "add";`). |
| `@left` | the precedence *name* `left` vs a forgotten `@left(…)` | `left`/`right`/`dynamic` are **reserved after `@`**: a bare `@left` is an error explaining that it needs an argument list, and that a name spelled `left` cannot be annotated at all — rename it. |
| `a b @left(x)` | the annotation applies to `b` vs to the whole production | **The whole production**, because that is what a production's precedence is. Annotate part of a sequence with grouping: `a (b @left(x)) c`. |
| `/x/ i` | one token with the flag `i` vs a regex followed by the rule `i` | **Two tokens**: flags live inside the literal (`/x/i`). The space is meaningful, and when the second reading is probably what was meant (`/x/g`) the diagnostic says `put a space after the literal`. |
| `//` | an empty pattern vs a line comment | **Comment.** An empty pattern is unwritable; the empty production is `()` and an empty string literal is `''`. |
| `-1` | a literal vs a binary minus | There is no binary `-`. `-1` is always a literal, and a bare number is only legal as a precedence value; anywhere else it says so. |
| `k= j= a` | nested labels | **Refused.** A label names one element; group the inner one: `k= (j= a)`. |
| `(%)` | — | `%` only ever starts a directive, and directives are only legal at the top level, so a directive name can never collide with a rule name. |
| `''` / `()` | empty string literal vs blank production | Different things, both legal: `''` is a `STRING` token with an empty value, `()` is a production with no steps (legal only where the emptiness is absorbed by a multiplier or the start rule). |

The two that cannot be removed are the first row (`name (`) — it is the price of
accepting `grammar.js`'s own spelling — and the reserved words (`left`, `right`,
`dynamic`). Everything else in the list above is either a spelling choice
already made or a mistake with a message that names its own fix.
