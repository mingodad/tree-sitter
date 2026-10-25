# `bc` — a third dog-food port (not a gate yet)

`bc.g` is 292 lines of real Bison + Flex for a `bc` calculator, and
`samples/calculator.bc` is OpenBSD's `bc.library` (272 lines, licence header
intact) — `define e(x)`, `auto` declarations, `scale`/`ibase`, comments.

It was chosen as dog-food #3 because it is the first input where the translator
was not merely incomplete but *silently wrong*: a truncated regex, a token that
kept only one of its three patterns, and a precedence chain that split.  Those
three are fixed; the findings are written up in `../DOGFOOD.md` under
"Dog-food #3: bc.g".

## Status

```
$ g2tsg bc.g -o grammar.tsg --name bc --conflicts conflicts.txt
grammar.tsg: 22 rules, 34 tokens, 12 levels
$ tsg grammar.tsg -o src/grammar.json
$ tree-sitter generate --lalr src/grammar.json     # 187 states, 0 conflicts
$ tree-sitter parse samples/calculator.bc
3 ERROR, 1 MISSING                                 # see below
```

Not clean, so this is **not** a gate: there is no `check.sh` here, and
`grammar.tsg` / `src/grammar.json` are checked in only so the state of the run
is visible.  Both remaining causes are known and neither is a grammar bug:

1. `emit.rs` hardcodes `%extra /--[^\n]*/` (true for postgres16, false for `bc`,
   where `--` is decrement) → `MISSING Scale` at file line 75.
2. An extra loses to a token that is a prefix of it: `1 /* comment */` lexes the
   `/` as division.  **This is tree-sitter, not the translator** — reproduced on
   upstream `0fff6ab` with a 12-line grammar (see `DOGFOOD.md`).

Fix (1) by deriving extras from `skip()` rules; (2) wants a lex-state report
before deciding whether it is a bug report or a documented limitation.

**Warning:** this `grammar.tsg` was produced by the *uncommitted* translator
fixes from dog-food #3 (start-condition strip, multi-pattern tokens, empty
precedence levels).  If the translator changes are reverted or rewritten, this
file stops being `g2tsg` output and must be regenerated:

```
cd tmp/tsg
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
CARGO_TARGET_DIR=$HOME/.ts-build cargo build --release --offline \
    --manifest-path bison_flex/translate/Cargo.toml
$HOME/.ts-build/release/g2tsg bc/bc.g -o bc/grammar.tsg \
    --name bc --title "bc calculator, translated from bc.g (Bison + Flex)." \
    --conflicts bc/conflicts.txt
```

## Why it is worth promoting to a gate

- 22 rules but **12 precedence levels**, including `%nonassoc IF_WITHOUT_ELSE`,
  `%right Else`, `%prec UNARY_MINUS` — the surface `postgres16` covers only
  partly and `antlr-v4.5.g` does not cover at all.
- One token (`NUMBER`) produced by three lexer rules, and one token (`REL_OP`)
  whose pattern ends in `>` — both were translator bugs until dog-food #3.
- A real 272-line sample that is dense in `define`, `if/else`, `while`,
  `auto` lists, `scale`, `print`, division, and comments — every one of which
  appeared in the failure list above.
- It generates in well under a second with `--lalr`, so it is cheap to run on
  every change, unlike `postgres16` (4.1 s / 25 MB).

`samples/calculator.bc` is `test.bc-calculator` from
`parsertl-playground/playground`, copied so this check is reproducible without
the corpus.  `bc.g` is copied from the same place; both keep their original
licence headers.
