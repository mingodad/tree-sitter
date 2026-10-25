#!/usr/bin/env python3
"""Translate ../postgres16.g (Bison grammar + Flex scanner) to grammar.tsg.

This is the original translator: it splits the file with regexes.  It is kept as
the reference implementation — ../bison_flex/translate does the same translation
with a real parser, and check.sh compares the two.

The source is the reference; read the two side by side.  Every decision that is
not mechanical is marked in the generated file and explained in README.md.

The parts of the source this script understands:

  * `%token` (one terminal per line), `%left`/`%right`/`%nonassoc` (lowest
    first), `%start`.
  * Bison rules (`lhs : alt | alt ;`), `%empty`, `%prec terminal`.
  * Flex macros (`name pattern`) and rules (`pattern action`), `%option
    caseless`, `%x` start conditions, and the keyword map (`WORD TOKEN`).

It does not understand C actions, `%{...%}`, `%union`, `%type`, `%dprec`,
`%expect`: postgres16.g contains none of them.
"""

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = HERE.parent / "postgres16.g"
# Overridable so check.sh can diff the output against the committed grammar
# without rewriting it.
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "grammar.tsg"

# ---------------------------------------------------------------------------
# 0. Split the four sections and drop comments
# ---------------------------------------------------------------------------


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


raw = SRC.read_text()
sections = re.split(r"(?m)^%%[^\n]*$", raw)
if len(sections) != 5 or sections[-1].strip():
    raise SystemExit("expected 4 sections and a trailing %%, found %d" % len(sections))
decl_src, rules_src, lmac_src, lrules_src = (
    strip_comments(s) for s in sections[:4])

# ---------------------------------------------------------------------------
# 1. Declarations
# ---------------------------------------------------------------------------

tokens = []           # terminal names declared with %token
literals = []         # terminals declared as quoted literals ('+', ...)
levels = []           # (assoc, [terminals]) lowest first
start = None

for line in decl_src.splitlines():
    parts = line.split()
    if not parts:
        continue
    if parts[0] == "%token":
        for t in parts[1:]:
            (literals if t.startswith("'") else tokens).append(t)
    elif parts[0] in ("%left", "%right", "%nonassoc", "%precedence"):
        levels.append((parts[0][1:], parts[1:]))
    elif parts[0] == "%start":
        start = parts[-1]

if start is None:
    raise SystemExit("no %start")
token_set = set(tokens)

# Bison numbers levels from 1 (lowest) upwards; the /*N*/ comments in the
# source are that number.  `%prec X` names a level through a terminal in it.
level_of = {}
for number, (assoc, terms) in enumerate(levels, 1):
    for t in terms:
        level_of[t] = number

# ---------------------------------------------------------------------------
# 2. Flex macros and rules: terminal -> regex
# ---------------------------------------------------------------------------

macros = {}
for line in lmac_src.splitlines():
    line = line.strip()
    if not line or line.startswith("%"):
        continue
    m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s+(.+)$", line)
    if not m:
        raise SystemExit("unparsed macro: " + line)
    macros[m.group(1)] = m.group(2).strip()


def expand(pattern, depth=0):
    """Substitute {macro} references (Flex macro expansion)."""
    if depth > 20:
        raise SystemExit("macro recursion: " + pattern)

    def repl(m):
        name = m.group(1)
        if name not in macros:
            raise SystemExit("unknown macro {%s} in %s" % (name, pattern))
        return "(" + expand(macros[name], depth + 1) + ")"

    return re.sub(r"\{([A-Za-z_][A-Za-z0-9_]*)\}", repl, pattern)


def flex_to_regex(p):
    """Flex pattern -> the regex dialect tree-sitter accepts.

    Handles the escapes this source uses: double-quoted literal strings, `\\c`
    for regex metacharacters, octal bytes (\\200-\\377), and the Flex `-`
    outside classes.  Character classes are copied through unchanged, which is
    correct because Flex and tree-sitter agree on their syntax.
    """
    out = []
    i = 0
    in_class = False
    while i < len(p):
        c = p[i]
        if c == "\\" and i + 1 < len(p):
            n = p[i + 1]
            if n in "01234567":
                m = re.match(r"[0-7]{1,3}", p[i + 1:])
                out.append("\\x%02x" % int(m.group(0), 8))
                i += 1 + len(m.group(0))
                continue
            if n == '"':
                out.append('"')
            elif n in "nrt":
                out.append("\\" + n)
            elif n in "abefv":
                # Flex spells the control characters C's way; the regex dialect
                # wants a byte (g2tsg's flex.rs does the same).
                out.append("\\x%02x" % {"a": 7, "b": 8, "e": 27, "f": 12, "v": 11}[n])
            elif n in r".[]{}()*+?|^$/\\":
                out.append("\\" + n)
            else:
                out.append(n)
            i += 2
            continue
        if c == '"' and not in_class:
            j = p.index('"', i + 1)
            out.append(re.escape(p[i + 1:j]).replace("/", "\\/"))
            i = j + 1
            continue
        if c == "[" and not in_class:
            in_class = True
        elif c == "]" and in_class:
            in_class = False
        if c == "/":
            # `/` closes the `/…/` literal in the emitted .tsg, so it always
            # needs the escape, inside a character class included.
            out.append("\\/")
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


# Parse the rules section.  Lines look like `pattern action`, sometimes with a
# `<state>` prefix on the pattern and a `<state>` suffix before the action.
# `<state> {` ... `}` blocks are the lookahead hacks; they are recorded and
# skipped (see the `_LA` handling below).
lex_rules = []        # (pattern, action)
i = 0
lines = lrules_src.splitlines()
while i < len(lines):
    line = lines[i].strip()
    i += 1
    if not line or line.startswith("%"):
        continue
    if re.match(r"^<[A-Za-z_][A-Za-z0-9_]*>\s*\{\s*$", line):
        # <xstate> { ... } block: the action is a state switch.
        while i < len(lines) and lines[i].strip() != "}":
            i += 1
        i += 1
        continue
    line = re.sub(r"^<[^>]+>", "", line)          # <state> prefix
    m = re.match(r"^(.*?)\s+(\S+)$", line)
    if not m:
        # A rule with no action is only a state switch (`<x>pattern<y>`).
        continue
    pattern, action = m.group(1).strip(), m.group(2).strip()
    pattern = re.sub(r"(?<!\\)<[^>]+>\s*$", "", pattern).strip()   # <state> suffix
    lex_rules.append((pattern, action))

# The keyword map: a bare word pattern and a terminal action.  `%option
# caseless` makes it case-insensitive.  `MODE_TYPE_NAME MODE_TYPE_NAME` and
# `BCONST BCONST` are the same shape.
spell = {}
for pattern, action in lex_rules:
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", pattern) and action in token_set:
        spell[action] = pattern

# Tokens produced by the lookahead rules: the word before `\s+` is the base
# keyword; the state block's action names the _LA token.
la_base = {}
la_base = {}
for name in token_set:
    if name.endswith("_LA"):
        # `NULLS_LA` is produced from the word NULLS, whose token is NULLS_P.
        word = name[:-3]
        for token, spelling in spell.items():
            if spelling == word:
                la_base[name] = token
                break

# Every other rule whose action is a terminal gives that terminal its pattern.
overrides = {}
for pattern, action in lex_rules:
    if action not in token_set or action in spell:
        continue
    overrides[action] = expand(pattern)

# Hand-written bodies where Flex syntax has no direct tree-sitter equivalent.
# The patterns come from the Flex rules above; see README.md.
hand = {
    "Op": r"(\|\||->>|->|#>>|#>|@>|<@|\?\||\?&|\?)",
    "SCONST": r"'((''|[^']))*'",
    "IDENT": r"([A-Za-z\x80-\xff_][A-Za-z\x80-\xff_0-9$]*|\"((\"\"|[^\"]))*\")",
    "FCONST": r"([0-9]*\.[0-9]+|[0-9]+\.[0-9]*)([Ee][-+]?[0-9]+)?",
}
# Prefer the hand-written body when the parsed pattern translates cleanly, and
# fall back to the source pattern for everything else.
bodies = {}
for name in token_set:
    if name in spell:
        bodies[name] = "/" + re.escape(spell[name]) + "/i"
    elif name in hand:
        bodies[name] = "/" + hand[name] + "/"
    elif name in overrides:
        bodies[name] = "/" + flex_to_regex(overrides[name]) + "/"
    elif name.endswith("_LA") or name == "UMINUS":
        bodies[name] = None      # dropped below
    else:
        raise SystemExit("no lexer body for token " + name)

la_of = {name: la_base.get(name) for name in token_set if name.endswith("_LA")}
missing = [n for n, b in la_of.items() if b is None]
if missing:
    raise SystemExit("lookahead token without a base token: %r" % missing)

# ---------------------------------------------------------------------------
# 3. Rules
# ---------------------------------------------------------------------------

TOK = re.compile(
    r"'(?:[^'\\]|\\.)*'|[A-Za-z_][A-Za-z0-9_]*|%empty|%prec|[:|;]"
)
sym = TOK.findall(rules_src)
rules = {}            # lhs -> [(atoms, prec)]
order = []
i = 0
while i < len(sym):
    lhs = sym[i]
    if i + 1 >= len(sym) or sym[i + 1] != ":":
        raise SystemExit("expected ':' after %r, got %r" % (lhs, sym[i + 1:i + 3]))
    i += 2
    alts = []
    atoms = []
    prec = None
    while True:
        t = sym[i]
        if t == "|":
            alts.append((atoms, prec))
            atoms, prec = [], None
        elif t == ";":
            alts.append((atoms, prec))
            i += 1
            break
        elif t == "%empty":
            pass
        elif t == "%prec":
            prec = sym[i + 1]
            i += 1
        else:
            atoms.append(t)
        i += 1
    if lhs in rules:
        raise SystemExit("duplicate rule " + lhs)
    rules[lhs] = alts
    order.append(lhs)

for lhs, alts in rules.items():
    for atoms, prec in alts:
        for a in atoms:
            if a.startswith("'"):
                continue
            if a in token_set or a in rules:
                continue
            raise SystemExit("%s: unknown symbol %r" % (lhs, a))
        if prec is not None and prec not in level_of:
            raise SystemExit("%s: %%prec %r is not in a level" % (lhs, prec))

# ---------------------------------------------------------------------------
# 4. Nullability
# ---------------------------------------------------------------------------

nullable = set()
changed = True
while changed:
    changed = False
    for lhs, alts in rules.items():
        if lhs in nullable:
            continue
        for atoms, _ in alts:
            if all(not a.startswith("'") and a in nullable for a in atoms):
                nullable.add(lhs)
                changed = True
                break

# A nullable rule that refers to itself is a list.  Turn it into the explicit
# recursive form, adding a base alternative when the source only has `%empty`:
#
#   N : N ',' item | item  | %empty      =>  N : N ',' item | item
#   N : N ',' item | %empty              =>  N : N ',' item | ',' item
#
# The leading/trailing self-reference is left alone by the substitution pass.
for lhs in [n for n in order if n in nullable and any(
        any(a == n for a in atoms) for atoms, _ in rules[n])]:
    alts = [(atoms, prec) for atoms, prec in rules[lhs] if atoms]
    where = set()
    for atoms, _ in alts:
        for k, a in enumerate(atoms):
            if a == lhs:
                where.add(0 if k == 0 else (-1 if k == len(atoms) - 1 else "mid"))
    if "mid" in where or len(where) > 1:
        raise SystemExit("self-recursive nullable rule is not a list: " + lhs)
    if 0 in where:
        rec = [(a[1:], prec) for a, prec in alts if a and a[0] == lhs]
        base = [(a, prec) for a, prec in alts if not a or a[0] != lhs]
        if not base:
            base = [(rest, prec) for rest, prec in rec]
        rules[lhs] = [([lhs] + rest, prec) for rest, prec in rec] + base
    else:
        rec = [(a[:-1], prec) for a, prec in alts if a and a[-1] == lhs]
        base = [(a, prec) for a, prec in alts if not a or a[-1] != lhs]
        if not base:
            base = [(rest, prec) for rest, prec in rec]
        rules[lhs] = [(rest + [lhs], prec) for rest, prec in rec] + base

# ---------------------------------------------------------------------------
# 5. Rewrite empty productions
#
# Tree-sitter forbids a non-start rule that matches the empty string.  The
# language is kept by moving the optionality to the use sites: a nullable rule
# N stays a rule, but every reference to it becomes `N?`.  An alternative that
# is *entirely* optional then needs the "at least one present" form:
#
#   a? b? c?   =>   a b? c? | b c? | c
#
# (`a`, `b`, `c` here are the non-empty part of each optional item).  The
# start rule keeps its empty production, which tree-sitter allows.
# ---------------------------------------------------------------------------

EMPTY = []


def subst(atoms, self_name):
    """Wrap references to nullable rules in `?`, except a list's own edge."""
    out = []
    for a in atoms:
        if not a.startswith("'") and a in nullable and a != self_name:
            out.append(("opt", a))
        else:
            out.append(a)
    return out


def first_present(atoms):
    """Split an all-optional sequence into the non-empty subsequences."""
    out = []
    for k, item in enumerate(atoms):
        head = item[1] if isinstance(item, tuple) else item
        out.append([head] + atoms[k + 1:])
    return out


rewritten = {}
for lhs in order:
    alts = rules[lhs]
    if lhs != start:
        alts = [(atoms, prec) for atoms, prec in alts if atoms]
    alts = [(subst(atoms, lhs), prec) for atoms, prec in alts]
    if lhs == start:
        rewritten[lhs] = alts
        continue
    out = []
    for atoms, prec in alts:
        if atoms and all(isinstance(a, tuple) for a in atoms):
            out.extend((seq, prec) for seq in first_present(atoms))
        else:
            out.append((atoms, prec))
    if not out:
        raise SystemExit("rule became empty: " + lhs)
    rewritten[lhs] = out
rules = rewritten

# The rewrite is only sound if every non-start rule is now non-empty.
still = set()
changed = True
while changed:
    changed = False
    for lhs, alts in rules.items():
        if lhs in still:
            continue
        for atoms, _ in alts:
            if all(isinstance(a, tuple) or (not a.startswith("'") and a in still)
                   for a in atoms):
                still.add(lhs)
                changed = True
                break
for lhs in still:
    if lhs != start:
        raise SystemExit("still nullable after the rewrite: " + lhs)

# ---------------------------------------------------------------------------
# 6. Precedence
# ---------------------------------------------------------------------------

# tsg wants one descending chain, highest first.  Bison's level numbers are
# lowest first, so reverse.  A terminal that the rewrite removed (the _LA
# lookaheads) or that never lexes (UMINUS) is dropped from its level; the level
# itself stays, so the chain keeps its shape and `%prec` can name it.

removed_terminals = set(la_of) | {"UMINUS"}
level_lines = []
for number in range(len(levels), 0, -1):
    assoc, terms = levels[number - 1]
    terms = [t for t in terms if t not in removed_terminals]
    assoc_part = (assoc + " ") if assoc != "nonassoc" else ""
    if terms:
        level_lines.append('%%precedence %s"p%d" = %s;'
                           % (assoc_part, number, " ".join(terms)))
    else:
        level_lines.append('%%precedence %s"p%d";' % (assoc_part, number))


def mark(lhs, atoms, prec):
    """The `@` annotation for an alternative, if any.

    `%prec` wins; otherwise the rightmost terminal's level is implicit (tsg's
    `%precedence` table does that part), so nothing is emitted.
    """
    if prec is None:
        return ""
    n = level_of[prec]
    assoc = levels[n - 1][0]
    if assoc == "nonassoc":
        return " @p%d" % n
    return " @%s(p%d)" % (assoc, n)


# ---------------------------------------------------------------------------
# 7. Emit
# ---------------------------------------------------------------------------

lines = []
A = lines.append
A("// PostgreSQL 16 SQL grammar, translated from ../postgres16.g (Bison + Flex).")
A("//")
A("// Read this next to the source.  The translations that need judgement:")
A("//")
A("//   * Flex start conditions are lookahead.  `<xwith_la> reject()` followed by")
A("//     `WITH<INITIAL> WITH_LA` means `WITH` when the next word is `TIME`.")
A("//     Tree-sitter's lexer cannot see ahead, and does not need to: each `_LA`")
A("//     terminal's lexeme is exactly that of its base keyword, and the parser")
A("//     already sees the following word as its own terminal.  So every `_LA`")
A("//     terminal is replaced by its base token (`WITH_LA` -> `WITH`,")
A("//     `NULLS_LA` -> `NULLS_P`), and the lookahead rules are dropped.  The")
A("//     `%prec` levels that named them (level 9 for `NOT_LA`) are unchanged.")
A("//   * `%nonassoc` levels have no tree-sitter equivalent.  They keep their")
A("//     place in the chain as levels with no associativity (plain `@pN`), so")
A("//     the ordering is identical; only `1 < 2 < 3 < 4` is no longer an error.")
A("//   * The 196 nullable rules cannot match empty as non-start rules in")
A("//     tree-sitter.  Their references became `?` and their own bodies lost")
A("//     `%empty`; an all-optional body became its non-empty subsequences.")
A("//     `%prec` on an empty alternative (e.g. `%empty %prec Op`) goes with it.")
A("//   * Terminals stay named tokens, so `unreserved_keyword : ABORT | ...`")
A("//     reads like the source.  Keywords are case-insensitive (`%option")
A("//     caseless`) and are listed before `IDENT` so the tie goes to the word.")
A("//   * The Flex rules are the lexer, not PostgreSQL's scan.l: dollar quoting")
A("//     with a tag, `E'...'`, `B'...'`, `X'...'` and doubled `''` are not")
A("//     accepted by this source (its `dolq` start conditions only approximate")
A("//     them).  `BCONST`/`XCONST` match the literal words, as in the source.")
A("")
A("%name postgres;")
A("%%start %s;" % start)
A("")
A("tokens {")
keyword_order = [name for name in tokens if name in spell and name not in la_of]
other_order = [name for name in tokens
               if name not in spell and name not in la_of and name != "UMINUS"]
for name in keyword_order + other_order:
    if name == "IDENT":
        continue
    A("  %s: %s;" % (name, bodies[name]))
A("  IDENT: %s;" % bodies["IDENT"])
A("}")
A("")
# Separators: the Flex rules whose action is `skip()` (or empty, or a bare `;`)
# discard what they match.  Deriving them beats hardcoding whitespace and SQL's
# two comment styles, which silently failed to skip any other comment syntax.
separators = []
for pattern, action in lex_rules:
    action = action.strip()
    if not (action in ("", ";", "skip()", "skip();")
            or (action.startswith("/*") and action.endswith("*/"))
            or action.startswith("//")):
        continue
    body = flex_to_regex(expand(pattern))
    if body and body not in separators:
        separators.append(body)
if not separators:
    separators.append(r"[ \t\n\r\f]+")
for body in separators:
    A("%%extra /%s/;" % body)
A("")
A("// Bison lists precedence lowest first; tsg wants the descending chain.")
A("// `nonassoc` becomes a level with no associativity (see the notes above).")
lines.extend(level_lines)
A("")
# tree-sitter resolves LR conflicts only by precedence when the rules are
# named; otherwise it needs an explicit `%conflicts`.  Bison needs none of
# these: every one is an artifact of the translation, mostly the `x?`
# distribution that replaces Bison's empty alternatives (tree-sitter forbids
# empty productions) plus dropping the `_LA` lookahead tokens.
CONFLICTS = [
    ("utility_option_name", "unreserved_keyword"),
    ("simple_select",),
    ("a_expr",),
    ("target_el",),
    ("opt_frame_clause", "unreserved_keyword"),
    ("ConstDatetime", "col_name_keyword"),
    ("table_ref",),
    ("OptTempTableName", "unreserved_keyword"),
    ("a_expr", "bare_label_keyword"),
    ("ConstDatetime",),
    ("b_expr",),
    ("func_table",),
]
A("// Conflicts, all translation artifacts (Bison's grammar has none).")
for pair in CONFLICTS:
    A("%%conflicts %s;" % " ".join(pair))
A("")
A("// Bison named the start symbol with `%start`.")
A("// The start rule is written first.")
A("")
emit_order = [start] + [lhs for lhs in order if lhs != start]
for lhs in emit_order:
    alts = rules[lhs]
    # `_LA` terminals become their base token.
    def rewrite_la(atoms):
        out = []
        for a in atoms:
            if isinstance(a, tuple):
                out.append(("opt", la_of.get(a[1], a[1])))
            else:
                out.append(la_of.get(a, a))
        return out
    alts = [(rewrite_la(atoms), prec) for atoms, prec in alts]
    rendered = []
    for atoms, prec in alts:
        text = " ".join(a[1] + "?" if isinstance(a, tuple) else a for a in atoms)
        text += mark(lhs, atoms, prec)
        rendered.append(text.strip())
    seen = set()
    rendered = [r for r in rendered if not (r in seen or seen.add(r))]
    A(lhs + ":")
    for k, r in enumerate(rendered):
        A(("    " if k == 0 else "  | ") + r + (";" if k == len(rendered) - 1 else ""))
    A("")

OUT.write_text("\n".join(lines).rstrip() + "\n")
print("wrote %s: %d rules, %d tokens, %d levels"
      % (OUT, len(rules), len(keyword_order) + len(other_order) - 1, len(levels)))
