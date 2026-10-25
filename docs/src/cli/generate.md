# `tree-sitter generate`

The most important command for grammar development is `tree-sitter generate`, which reads the grammar in structured form
and outputs C files that can be compiled into a shared or static library (e.g., using the [`build`](./build.md) command).

```bash
tree-sitter generate [OPTIONS] [GRAMMAR_PATH] # Aliases: gen, g
```

The optional `GRAMMAR_PATH` argument should point to the structured grammar, in one of two forms:
- `grammar.js` a (ESM or CJS) JavaScript file; if the argument is omitted, it defaults to `./grammar.js`.
- `grammar.json` a structured representation of the grammar that is created as a byproduct of `generate`; this can be used
to regenerate a missing `parser.c` without requiring a JavaScript runtime (useful when distributing parsers to consumers).

If there is an ambiguity or *local ambiguity* in your grammar, Tree-sitter will detect it during parser generation, and
it will exit with a `Unresolved conflict` error message. To learn more about conflicts and how to handle them, see
the section on [`Structuring Rules Well`](../creating-parsers/3-writing-the-grammar.md#structuring-rules-well)
in the user guide.

A grammar that is ported from another parser generator usually has several independent conflicts. Rather than stopping
at the first one, `generate` reports all of them in a single run, with the `conflicts` entry that each one needs. Pass
[`--json-summary`](#--json-summary) to get the item sets behind each conflict.

## Generated files

- `src/parser.c` implements the parser logic specified in the grammar.
- `src/tree_sitter/parser.h` provides basic C definitions that are used in the generated `parser.c` file.
- `src/tree_sitter/alloc.h` provides memory allocation macros that can be used in an external scanner.
- `src/tree_sitter/array.h` provides array macros that can be used in an external scanner.
- `src/grammar.json` contains a structured representation of the grammar; can be used to regenerate the parser without having
to re-evaluate the `grammar.js`.
- `src/node-types.json` provides type information about individual syntax nodes; see the section on [`Static Node Types`](../using-parsers/6-static-node-types.md).


## Options

### `-l/--log`

Print the log of the parser generation process. This includes information such as what tokens are included in the error
recovery state, what keywords were extracted, what states were split and why, and the entry point state.

### `--abi <VERSION>`

The ABI to use for parser generation. The default is ABI 15, with ABI 14 being a supported target.

### `--no-parser`

Only generate `grammar.json` and `node-types.json`

### `--stamp`

Build the grammar and print a digest of what the build produced, then exit without writing any file:

```json
{
  "digest": "7ad4882426282053",
  "node_types": "971ef8e9055b57bc",
  "bytes": 4656,
  "states": 4,
  "lex_states": 4
}
```

The digest covers the rendered parser and `node-types.json` from the same build, after unused rules are dropped, tokens
are extracted, inline rules are expanded, conflicts are resolved against precedence, and the lexer tables are built. Two
grammars with the same stamp build the same parser, so this is how to check that a rewrite of a grammar (a formatter, a
translator, a generator) changed nothing that matters: compare stamps instead of `grammar.json` text. Rule order,
whitespace, and parentheses that do not change the rule tree do not move the stamp; a dropped alternative, a changed
precedence, or a different alias does. The digest depends on the ABI version and optimization options, so both sides of a
comparison must pass the same ones.

### `-o/--output`

The directory to place the generated parser in. The default is `src/` in the current directory.

### `--report-states-for-rule <RULE>`

Print the overview of states from the given rule. This is useful for debugging and understanding the generated parser's
item sets for all given states in a given rule. To solely view state count numbers for rules, pass in `-` for the rule argument.
To view the overview of states for every rule, pass in `*` for the rule argument.

### `--json-summary`

Report conflicts in a JSON format.

### `--js-runtime <EXECUTABLE>`

The path to the JavaScript runtime executable to use when generating the parser. The default is `node`.
Note that you can also set this with `TREE_SITTER_JS_RUNTIME`. Starting from version 0.26, you can
also pass in `native` to use the experimental native QuickJS runtime that comes bundled with the CLI.
This avoids the dependency on a JavaScript runtime entirely. The native QuickJS runtime is compatible
with ESM as well as with CommonJS in strict mode. If your grammar depends on `npm` to install dependencies such as base
grammars, the native runtime can be used *after* running `npm install`.

### `--lalr`

Build LALR(1) parse states instead of canonical LR(1) states. States whose kernel items are identical, but whose
lookahead sets differ, are merged and rebuilt, the way yacc and bison do it. For large grammars this can reduce the
number of parse states (and the size of the generated table) by orders of magnitude, at the cost of reporting
conflicts that canonical LR(1) states would have resolved away.

This is mainly useful when porting a grammar that already works in yacc or bison. Bison builds LALR(1) tables, so a
grammar that is conflict-free there normally generates with `--lalr` as well; if canonical LR(1) state generation is
infeasible for it (generation takes minutes, or `--max-states` aborts), `--lalr` is what makes it generate at all. Any
conflict that `--lalr` reports but bison does not is ambiguity the port introduced, so resolve it rather than adding a
`conflicts` entry. This is not a general speed knob: it changes which states are built, and therefore the generated
parser, so keep the default for grammars that were written for Tree-sitter.

### `--max-states <N>`

Abort parser generation with an error if the parser needs more than `N` parse states. This bounds the time and memory
that `generate` can spend on a grammar whose state count is exploding, instead of leaving you to kill the process.

### `--disable-optimization`

Disable optimizations when generating the parser. Currently, this only affects the merging of compatible parse states.
