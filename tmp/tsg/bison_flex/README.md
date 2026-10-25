# bison_flex — a tree-sitter front end for the joined Bison/Flex `.g` format

`grammar.tsg` is a tree-sitter grammar for the format used by `postgres16.g`
and the metagrammar `bison-flex.g`. It is the front half of a translator: parse
the source once, then read the Bison declarations and rules from a CST instead
of re-parsing them with regexes.

## What is parsed structurally

- the `%token`, `%fallback`, `%left`, `%right`, `%nonassoc`, `%precedence`,
  `%start`, `%token_indent`, `%token_dedent`, and `%token_symbol_table`
  declarations;
- token names, quoted literals, and `<type>` tags;
- Bison rule names, alternatives, `%empty`, `%prec`, `#ProductionAlias`;
- EBNF `[...]`, `(...)`, `{...}`, and the `?`, `*`, `+` postfix operators;
- the four `%%` section separators.

## What is line-oriented

The Flex section is captured as `LexerLine` nodes, one per non-empty line.
Flex actions are arbitrary C, so they cannot be represented faithfully by a
context-free grammar. The translator splits the lines it needs: `%option`,
`%x`, macro definitions, keyword mappings, and lexer rules. This also means
unknown Flex directives and multi-line C actions do not cause parse errors.

## Deliberate limits

This is a first cut, aimed at real grammars and at translation rather than
validating every C construct. Not yet modelled:

- `%{ ... %}` prologue/epilogue code and `%union { ... }`;
- Bison mid-rule actions and `%code` blocks as opaque code;
- unknown Bison directives (`%type`, `%define`, `%expect`, `%destructor`, …);
- `%dprec` / `%merge` annotations;
- a structured Flex regex AST.

Those can be added without changing the rule/declaration shape that the
translator consumes.

## Use

```sh
./bison_flex/check.sh

# or manually
./target/debug/tsg bison_flex/grammar.tsg -o bison_flex/src/grammar.json
(cd bison_flex && ../bin/tree-sitter generate src/grammar.json)
(cd bison_flex && ../bin/tree-sitter build --output lib/bison_flex.dylib .)
./bin/tree-sitter parse --lib-path bison_flex/lib/bison_flex.dylib \
    --lang-name bison_flex postgres16.g
```

The translator that consumes this CST lives in [`translate/`](translate/README.md)
(`g2tsg`: `.g` → `.tsg`, ~1 500 lines of Rust). `postgres16/check.sh` builds the
real grammar with it, so that is the worked example; `g2tsg --dump` prints the
tree this grammar produces for any `.g` file:

```sh
(cd translate && cargo build --release)
./translate/target/release/g2tsg ../postgres16.g --dump | head -1
```

`extract.py` reads the XML tree and reports the counts a translator depends on:

```sh
./bin/tree-sitter parse --lib-path bison_flex/lib/bison_flex.dylib \
    --lang-name bison_flex --xml postgres16.g > /tmp/pg.xml
python3 bison_flex/extract.py /tmp/pg.xml
```

Current shape of `postgres16.g`:

```text
rules:              705
alternatives:       3282
token directives:   513
precedence levels:  27
empty alternatives: 187
%prec annotations:  55
lexer lines:        617
```

`bison-flex.g` itself parses with the same grammar: 69 rules, 104 alternatives,
8 empty alternatives, 4 section separators.

## Next

1. Parse every `.g` in the corpus and classify directives/actions the grammar
   does not yet cover (`corpus.sh`, `corpus.py`).
2. Run the corpus through `translate/`: the translator accepts the constructs
   `postgres16.g` uses and rejects the rest with a position, so the rejections
   are the to-do list.
3. Keep the lexer approximation rules (keyword mappings, `_LA` states,
   dollar-quoted strings) in the translator, not in the metagrammar — they are in
   `translate/src/flex.rs` and `translate/src/emit.rs`.
