#!/usr/bin/env python3
import re, itertools
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SRC = ROOT / 'sqlite3.lemonex'
OUT = HERE / 'grammar.tsg'

text = SRC.read_text()
body = text.split('%start_symbol input', 1)[1].split('/* ============================ lexer', 1)[0]
lexbody = text.split('/* ============================ lexer', 1)[1].split('/* ============================ driver', 1)[0]

spell = {}
for line in lexbody.splitlines():
    m = re.match(r'([A-Z][A-Z0-9_]*)\s*::=\s*"(.*)"\.\s*$', line.strip())
    if m:
        spell[m.group(1)] = m.group(2)

# Lemon levels, lowest first in the source.
levels_src = [
    ('left', 'or', ['OR']),
    ('left', 'and', ['AND']),
    ('right', 'not', ['NOT']),
    ('left', 'compare', ['IS', 'MATCH', 'LIKE_KW', 'BETWEEN', 'IN', 'ISNULL', 'NOTNULL', 'NE', 'EQ']),
    ('left', 'rel', ['GT', 'LE', 'LT', 'GE']),
    ('right', 'escape', ['ESCAPE']),
    ('left', 'bit', ['BITAND', 'BITOR', 'LSHIFT', 'RSHIFT']),
    ('left', 'add', ['PLUS', 'MINUS']),
    ('left', 'mul', ['STAR', 'SLASH', 'REM']),
    ('left', 'concat', ['CONCAT', 'PTR']),
    ('left', 'collate', ['COLLATE']),
    ('right', 'bitnot', ['BITNOT']),
    (None, 'on', ['ON']),
]
term_level = {}
level_assoc = {}
for assoc, name, terms in levels_src:
    level_assoc[name] = assoc
    for t in terms:
        term_level[t] = name

# Parse and expand Lemon's in-rule `|` (all runs are single-token alternatives).
orig = []  # (lhs, [symbols], bracket)
for line in body.splitlines():
    line = line.strip()
    if not line or line.startswith('%'):
        continue
    m = re.match(r'(\w+)\s*::=\s*(.*?)\.\s*(?:\[(\w+)\])?\s*$', line)
    if not m:
        raise SystemExit('unparsed: ' + line)
    lhs, rhs, bracket = m.groups()
    toks = rhs.replace('|', ' | ').split()
    items = []
    i = 0
    while i < len(toks):
        if i + 1 < len(toks) and toks[i + 1] == '|':
            group = [toks[i]]
            i += 1
            while i < len(toks) and toks[i] == '|':
                i += 1
                group.append(toks[i])
                i += 1
            items.append(group)
        elif toks[i] == '|':
            raise SystemExit('leading |: ' + line)
        else:
            items.append([toks[i]])
            i += 1
    for combo in itertools.product(*items):
        orig.append((lhs, list(combo), bracket))

# ANY is Lemon's %wildcard; QNUMBER has no lexer spelling in this source.
def resolve(t):
    if t == 'ANY':
        return 'ANY'
    if t == 'QNUMBER':
        return None
    return t

rules = {}   # lhs -> list of (alt, bracket, expanded original alt)
order = []
for lhs, alt, bracket in orig:
    if lhs not in rules:
        rules[lhs] = []
        order.append(lhs)
    expanded = [resolve(t) for t in alt]
    expanded = [t for t in expanded if t is not None]
    if alt and not expanded:
        # `term ::= QNUMBER` has no lexical spelling in this source.
        continue
    rules[lhs].append((expanded[:], bracket, expanded[:]))

all_rules = set(rules)

# --- original nullability -------------------------------------------------
nullable = set()
def is_nullable_alt(alt, nullset):
    return not alt or all(t in nullset for t in alt)
for lhs in rules:
    if any(is_nullable_alt(a, set()) for a, _, _ in rules[lhs]):
        nullable.add(lhs)
changed = True
while changed:
    changed = False
    for lhs, alts in rules.items():
        if lhs not in nullable and any(is_nullable_alt(a, nullable) for a, _, _ in alts):
            nullable.add(lhs); changed = True

removed = set()

def normalize(alts, lhs, nullset, rem):
    out = []
    for alt, bracket, orig_alt in alts:
        if not alt:
            continue
        items = []
        for idx, t in enumerate(alt):
            base_t = t.rstrip('?')
            if base_t in rem:
                continue
            if t == lhs:
                # A self-reference at the front is the recursion; elsewhere an
                # originally-nullable self-reference (e.g. `LP anylist RP`) can
                # also be absent.
                if idx > 0 and lhs in nullset:
                    items.append(t + '?')
                else:
                    items.append(t)
            elif t in nullset:
                if not t.endswith('?'):
                    items.append(t + '?')
                else:
                    items.append(t)
            else:
                items.append(t)
        if not items or all(x.endswith('?') for x in items):
            continue
        out.append((items, bracket, orig_alt))
    return out

# Iterate until stable.
for _ in range(30):
    before = (repr(rules), sorted(nullable), sorted(removed))
    # -- drop empty alternatives and make nullable uses optional
    for lhs, alts in list(rules.items()):
        rules[lhs] = normalize(alts, lhs, nullable, removed)
    # -- add bases to pure left-recursive lists
    for lhs, alts in list(rules.items()):
        if alts and all(a and a[0] == lhs for a, _, _ in alts):
            extra = []
            for a, b, o in alts:
                rest = a[1:]
                if rest and not all(x.endswith('?') for x in rest):
                    extra.append((rest, b, o))
            alts.extend(extra)
    # -- recompute nullability after the rewrite
    new_nullable = set()
    new_only = set()
    changed2 = True
    while changed2:
        changed2 = False
        for lhs, alts in rules.items():
            if lhs in new_nullable:
                continue
            if not alts:
                new_nullable.add(lhs); new_only.add(lhs); changed2 = True
                continue
            if any(is_nullable_alt(a, new_nullable) for a, _, _ in alts):
                new_nullable.add(lhs); changed2 = True
    # rules with no productions are gone
    for lhs in list(rules):
        if not rules[lhs]:
            del rules[lhs]
            removed.add(lhs)
    nullable, nullable_only = new_nullable, new_only
    if before == (repr(rules), sorted(nullable), sorted(removed)):
        break

# And a final pass in case the loop settled on the old sets.
for lhs, alts in list(rules.items()):
    rules[lhs] = normalize(alts, lhs, nullable, removed)
for lhs in list(rules):
    if not rules[lhs]:
        del rules[lhs]
        removed.add(lhs)

# --- token bodies ---------------------------------------------------------
def token_body(name):
    if name == 'ANY':
        return r'/[^\s,()]+/'
    d = spell[name]
    if name == 'WS':
        return '/[ \\t\\n\\r]+/'
    if name == 'LINE_COMMENT':
        return '/--.*/'
    if name == 'BLOCK_COMMENT':
        return r'/\/\*([^*]|\*+[^*\/])*\*+\//'
    if name in ('ID', 'FLOAT', 'INTEGER', 'STRING', 'BLOB', 'VARIABLE'):
        return '/' + d.replace('/', r'\/') + '/'
    if name in ('CTIME_KW', 'JOIN_KW', 'LIKE_KW'):
        return '/' + d + '/i'
    if name == 'NE':
        return '/<>|!=/'
    if re.fullmatch(r'[A-Za-z_][A-Za-z0-9_ ]*', d):
        return '/' + d + '/i'
    lit = d.replace('\\', '')
    if lit in {'&','~','|','.',',','=','>=','>','<=','<<','>>','<','-','+','%','*','/','(',')','->','||',';'}:
        return "'" + lit.replace("'", "\\'") + "'"
    raise SystemExit('no token body for %s = %s' % (name, d))

# Which terminals survive in the normalised grammar?
used_terms = set()
for lhs, alts in rules.items():
    for alt, _, _ in alts:
        for t in alt:
            base = t.rstrip('?')
            if base in spell and base not in all_rules:
                used_terms.add(base)
            elif base == 'ANY':
                used_terms.add(base)
used_terms.update(['WS', 'LINE_COMMENT', 'BLOCK_COMMENT'])
if 'ANY' in used_terms:
    # Lemon's %wildcard; it is not in the lexer section.
    pass

term_order = [t for t in spell if t in used_terms and t != 'ID']
if 'ID' in used_terms:
    term_order.append('ID')
if 'ANY' in used_terms:
    term_order.append('ANY')

def first_level(alt):
    for t in alt:
        if t in spell and t not in all_rules and t in term_level:
            return term_level[t]
    return None

def rightmost_level(alt):
    for t in reversed(alt):
        base = t.rstrip('?')
        if base in spell and base not in all_rules:
            return term_level.get(base)
    return None

def render(alt, bracket, orig_alt):
    lemon = term_level.get(bracket) if bracket else first_level(orig_alt)
    right = rightmost_level(alt)
    text = ' '.join(alt)
    if lemon != right:
        if lemon is None:
            text += ' @0'
        elif level_assoc.get(lemon):
            text += ' @%s(%s)' % (level_assoc[lemon], lemon)
        else:
            text += ' @' + lemon
    return text

# --- emit -----------------------------------------------------------------
lines = []
lines.append('// SQLite SQL grammar, translated from sqlite3.lemonex (Lemon + integrated lexer).')
lines.append('//')
lines.append('// Translation notes — read these with the source side by side:')
lines.append('//')
lines.append('//   * Lemon terminals stay named tokens (the `tokens { … }` block), so each')
lines.append('//     rule below reads like the Lemon production.  Keyword tokens are')
lines.append('//     case-insensitive regexes because the source begins with')
lines.append('//     `%lexer_caseless`; `ID` is the `%word` token.')
lines.append('//   * Lemon takes a rule\'s precedence from its *first* terminal.  tsg\'s')
lines.append('//     `%precedence` table uses the rightmost terminal (yacc\'s rule), so a')
lines.append('//     production whose two answers differ carries an explicit')
lines.append('//     `@left(level)`/`@right(level)` (or plain `@level` when the level')
lines.append('//     has no associativity); `@0` suppresses a terminal Lemon ignores.')
lines.append('//   * Empty Lemon productions cannot exist as non-start rules in')
lines.append('//     tree-sitter.  They are folded into the use sites as `x?`; rules that')
lines.append('//     were only `?` markers in the source (`scanpt`, `scantok`) disappear.')
lines.append('//   * `%wildcard ANY` becomes the `ANY` token; `QNUMBER` has no lexer rule')
lines.append('//     in the source and its single `term` production is omitted.')
lines.append('//   * The source\'s `%fallback ID` is the contextual keyword behaviour of')
lines.append('//     `%word ID`: a keyword token is only in the lexer mode of a state that')
lines.append('//     expects it, and `ID` is accepted where an identifier is expected.')
lines.append('')
lines.append('%name sqlite3;')
lines.append('%word ID;')
lines.append('')
lines.append('tokens {')
for t in term_order:
    lines.append('  %s: %s;' % (t, token_body(t)))
lines.append('}')
lines.append('')
lines.append('%extra WS;')
lines.append('%extra LINE_COMMENT;')
lines.append('%extra BLOCK_COMMENT;')
lines.append('')
lines.append('// One level per line, highest first (the source lists them lowest first).')
lines.append('// `ON` is `%nonassoc` in Lemon; tree-sitter has no non-associative')
lines.append('// precedence, so it is a level with no associativity.')
for assoc, name, terms in reversed(levels_src):
    assoc_part = (assoc + ' ') if assoc else ''
    lines.append('%%precedence %s"%s" = %s;' % (assoc_part, name, ' '.join(terms)))
lines.append('')
lines.append('// One GLR split: the optional ON/USING after a table reference.')
lines.append('%conflicts seltablist;')
lines.append('')
lines.append('// The first rule outside `tokens` is the start rule.')
lines.append('input: cmdlist;')
lines.append('')
SPECIAL = {
    'selcollist': ['(expr as? | STAR | nm DOT STAR) (COMMA (expr as? | STAR | nm DOT STAR))*'],
    'vtabarg': ['vtabargtoken+'],
    'vtabarglist': ["vtabarg (COMMA vtabarg)*"],
    'anylist': ['(ANY | LP anylist? RP)+'],
}
for lhs in order:
    if lhs == 'input' or lhs not in rules:
        continue
    lines.append(lhs + ':')
    if lhs in SPECIAL:
        rendered = SPECIAL[lhs]
    else:
        rendered = [render(a, b, o) for a, b, o in rules[lhs]]
    # de-duplicate while keeping order
    seen = set()
    rendered = [r for r in rendered if not (r in seen or seen.add(r))]
    for idx, r in enumerate(rendered):
        prefix = '    ' if idx == 0 else '  | '
        lines.append(prefix + r + (';' if idx == len(rendered) - 1 else ''))
    lines.append('')

OUT.parent.mkdir(exist_ok=True)
OUT.write_text('\n'.join(lines).rstrip() + '\n')
print('wrote', OUT, 'rules', len(rules), 'tokens', len(term_order))
