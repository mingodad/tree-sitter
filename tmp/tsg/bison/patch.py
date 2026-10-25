"""Hand adjustments applied to the g2tsg output for bison.g.

bison.g is grammar-only: it has no Flex section, so `g2tsg` reports that the
lexer never returns STRING, TSTRING, BRACKETED_ID … and drops every production
that needs them.  The token bodies therefore live in `bodies.txt`, and the four
edits below are the parts of the translation that need judgement:

1. `EPILOGUE` is dropped: the only pattern that can express "the rest of the
   file" is `(?s:.)+`, which in a tree-sitter lexer swallows everything.  The
   `epilogue_opt` rule and its use go with it (carbon-lang.y has no epilogue).
2. Comments become extras, which bison.g cannot say (they are in the Flex
   section the file does not have).
3. `ID_COLON` is a lexer hack — `name:` as one token — so a rule that opens with
   `|` (the yacc spelling of an empty first alternative) needs an alternative at
   the use site.  `rhs` cannot be made nullable: tsg rejects a nullable rule that
   is used in the middle of other rules, and it is right to.
"""

import re
import sys

path = sys.argv[1]
text = open(path).read()

text = re.sub(r"^  EPILOGUE: .*\n", "", text, flags=re.M)
old_input = "    prologue_declarations? SECTION_MARK grammar epilogue_opt?;"
assert old_input in text, "input rule moved"
text = text.replace(old_input, "    prologue_declarations? SECTION_MARK grammar;", 1)
text = re.sub(r"\nepilogue_opt:\n(    .*\n|  \| .*\n)+", "\n", text)

lines, out, done = text.splitlines(), [], False
for line in lines:
    if not done and line.startswith("%extra "):
        out += [
            "%extra /[ \\t\\n\\r\\f]+/;",
            "%extra /\\/\\/[^\\n\\r]*/;",
            "%extra /\\/\\*(?s:.)*?\\*\\//;",
        ]
        done = True
        continue
    out.append(line)
assert done, "no %extra line to replace"
text = "\n".join(out) + "\n"

old_rules = """rules:
    id_colon rhses_1?
  | id_colon named_ref_opt? ':' rhses_1?;"""
new_rules = """rules:
    id_colon rhses_1?
  | id_colon named_ref_opt? ':' rhses_1?
  | id_colon '|' rhses_1?;"""
assert old_rules in text, "rules rule moved"
text = text.replace(old_rules, new_rules, 1)
open(path, "w").write(text)
print("patched", path)
