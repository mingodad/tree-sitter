# Lua 5.3 in `.tsg`

`grammar.tsg` is [`../lua53.lemonex`](../lua53.lemonex) — a Lua 5.3 grammar in
Lemon (LALR(1)) plus an integrated lexer — translated to the declarative front
end. 185 productions and 56 terminals became ~150 lines of grammar and 55 lines of
commentary, written in the EBNF surface (`'if' exp 'then'`, `a | b`, `x?`, `x*`),
with its lexical rules in a `tokens { … }` block.

`seq(...)`/`choice(...)`/`optional(...)`/`repeat(...)` are the same constructs
spelled as calls, and the two surfaces mix freely; this file used the call form
first (it is the mechanical translation of `grammar.js`) and was converted to
EBNF — `compare.py` confirms the two produce identical `grammar.json`, hence an
identical `parser.c`.

It is the dogfooding grammar for the `.tsg` integration; the findings are in
[`DOGFOODING.md`](./DOGFOODING.md).

## Run it

```sh
# .tsg -> grammar.json -> parser.c -> parse
../target/debug/tsg grammar.tsg -o src/grammar.json
../bin/tree-sitter generate src/grammar.json
../bin/tree-sitter parse sample.lua     # exit 0, no ERROR/MISSING
```

`tree-sitter parse` needs the grammar to look like a grammar directory, hence
`tree-sitter.json` and `src/` — both hand-written here; `tree-sitter init
--grammar-format tsg` is Level 2 of the integration plan.

Also worth parsing: `tricky.lua` (method calls, call chains, varargs, long
strings, mixed separators) and `prec.lua` (one expression per precedence and
associativity question).

`tree-sitter generate` reports no diagnostics. The one `%conflicts` is required:
removing it reproduces the `'if' '{' field • ';'` conflict.

## Translation rules

| Lemon / Lemonex | `.tsg` | Note |
|---|---|---|
| `exp ::= exp PLUS exp.` | `exp '+' exp` | the level comes from the rightmost terminal, as in Lemon |
| `%left PLUS MINUS.` | `%precedence left "add" = '+' '-';` | a level owns its terminals; `+`/`-` share `add` |
| `[LPAREN]`, `[NOT]` rule marks | `@right(call)`, `@unary` | tree-sitter marks productions, not uses — see F1 |
| `semi ::= ";".` / `semi ::= .` | `optional(';')` at the use site | empty rules are illegal in tree-sitter — see F3 |
| `statlist`, `scope`, `block` | `repeat1` + `optional(block)` at 10 sites | `scope` only existed to collect locals for actions |
| `PLUS ::= "+".` | inline `'+'` | 56 terminals inlined 114 times; see F5 |
| `%lexer_mode LONGSTRING` | one regex token | scannerless: no modes needed |
| `--"".*` vs `{longbracket}` | `token(/--(\[=*\[(.|\n)*\]=*\]\|[^\n]*)/)` | works; `--[ not a long comment` needs no special case |
| semantic actions | — | validation is tree-sitter's job; `var`-assignment vs call becomes GLR |

## Deliberate deviations from the source

Everything here is either impossible in tree-sitter or a documented superset.
None of it changes the shape of the parse tree for valid Lua 5.3.

1. **Numeric literals are the source's, not Lua 5.3's.** `FLOAT` is
   `[0-9]+\.[0-9]*([eE][+-]?[0-9]+)?`, copied verbatim, so `1e3` and `.5` are not
   numbers — real Lua 5.3 accepts both. Kept for fidelity to the file being
   ported.
2. **`--` alone is a comment.** The source requires one non-`[` character after
   `--` for a line comment; the single-regex form also accepts a bare `--`.
3. **`(...)` accepts `...` anywhere in the parameter list.** The source's
   `parlist` allows `...` only last (`namelist DOTS`), which is lookahead-2. The
   port treats `...` as just another list item, so `(..., a)` parses. Rejecting
   it needs an external scanner or a parse-time check, not a grammar.
4. **`{f;}`/`{f,g;}` need `%conflicts`.** Whether a trailing separator belongs to
   the table or starts another field is deferred in Lemon's automaton; in
   tree-sitter it is a GLR fork. One `%conflicts fieldlist;`, verified necessary.
5. **`;` is only a separator.** As in the source: `;;` is not a statement.
6. **Long-bracket matching is not depth-aware.** `[=[ … ]=]` is one token and
   `[=[ … ]] … ]=]` works, but nothing counts brackets; the source's Flex rules
   don't either.
7. **`STRING` is a single pattern**, not `choice(token(…), token(…))`, because
   the two-token spelling triggers a silent zero-width node on error recovery
   upstream. See `DOGFOODING.md` §2 and `variants/v5-token-choice-misparse.tsg`.

## Precedence

The 13 levels are declared once, highest first, each owning the terminals that
inherit it:

```tsg
%precedence right "call";
%precedence right "pow"     = '^';
%precedence right "unary";
%precedence left  "mul"     = '*' '/' '//' '%';
%precedence left  "add"     = '+' '-';
…
```

21 productions inherit their level; 20 keep an explicit `@`
because the rightmost-terminal rule cannot see them: `call` marks *uses* of
non-terminals (`funccall args`, `var '.' NAME`), and `-`/`~` are owned by the
binary levels, so their unary productions say `@unary` — Lemon has the same trap
(`%left MINUS` applies to unary minus too). `tsg -p grammar.tsg` prints the
resolved table.

## Shape

Rules keep the source's names, in preorder, so the two files can be read side by
side. The tree is the obvious one — `chunk > block > stat`, `exp` for every
expression, `var`/`funccall` for the two assignment/call shapes that make Lua a
GLR grammar. Terms that only ever hold one child (`exp`) are visible rather than
hidden so error messages can point at them; hiding them is a one-word change
(`%inline` won't do it for a recursive rule — rename to `_exp`).
