"""The one hand edit applied to the g2tsg output for bison.g.

Everything else now comes from the file itself: `bison.g`'s lexer is a Flex
section (it just sits after the grammar, with `<INITIAL,RULES_ST> { … }` groups),
so `g2tsg` reads its tokens and its `skip()` extras.  What it cannot express it
reports — `ACTION_CODE` and `EPILOGUE` are only reachable inside a start
condition (`<ACTION_CODE_ST>`, `<EPILOGUE_ST>`), and tree-sitter's lexer has no
states, so those two tokens have no body, the productions that need them are
dropped, and the note says so.

The edit that is left is about grammar shape, not lexing.  `ID_COLON` is the
yacc lexer hack that binds `name:` into one token, so a rule whose body opens
with `|` — the ordinary spelling of an empty first alternative — ends up as
`id_colon '|' …` with nothing before the bar.  `rhs` cannot simply be made
nullable: it is used in the middle of other rules, and tsg rejects that (its
message says to move the emptiness to the use site, so it is moved here).
"""

import sys

path = sys.argv[1]
text = open(path).read()

old = """rules:
    id_colon rhses_1?
  | id_colon named_ref_opt? ':' rhses_1?;"""
new = """rules:
    id_colon rhses_1?
  | id_colon named_ref_opt? ':' rhses_1?
  | id_colon '|' rhses_1?;"""
assert old in text, "the `rules` rule moved; update patch.py"
open(path, "w").write(text.replace(old, new, 1))
print("patched", path)
