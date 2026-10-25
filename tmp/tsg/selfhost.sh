#!/usr/bin/env bash
# Self-hosting test.
#
# examples/tsg.tsg is a tree-sitter grammar for the tsg language, written in
# tsg. This script:
#
#   1. compiles it with tsg               .tsg -> grammar.json
#   2. builds a parser from it            grammar.json -> parser.c -> .dylib
#   3. round-trips the description        grammar.json -> .tsg -> grammar.json
#   4. parses every .tsg file it can find with that parser
#   5. runs the shipped queries, which is where invalid flags become errors
#
# Step 4 is the real test: the parser produced by the tool must accept the
# language the tool accepts, including the 1000-line files the reverse
# direction emits for mssql and plsql.
#
# `surface/` closes the loop in the other direction. Those files spell every
# directive form and every expression shape the compiler accepts, once, so the
# description in examples/tsg.tsg can be checked against the implementation:
# each one must compile (step 4b) and parse without an ERROR node (step 4), and
# between them they must account for every named node type in the description's
# own node-types.json (step 5b, via coverage.py).
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tsg=${TSG:-$here/target/debug/tsg}
ts=${TS:-$here/bin/tree-sitter}
work="$here/.self"

for tool in "$tsg" "$ts"; do
    if [ ! -x "$tool" ]; then
        echo "error: missing $tool" >&2
        exit 2
    fi
done

# Keep the parser cache and the compiled dylib inside the project.
export XDG_CACHE_HOME="$work/cache"
export TREE_SITTER_LIBDIR="$work/lib"

rm -rf "$work"
mkdir -p "$work/src" "$TREE_SITTER_LIBDIR"

failures=0

# 1. Compile the language description with itself.
if ! "$tsg" "$here/examples/tsg.tsg" -o "$work/src/grammar.json"; then
    echo "FAIL  tsg.tsg does not compile"
    exit 1
fi
printf 'ok   examples/tsg.tsg -> grammar.json (%s rules, %s bytes)\n' \
    "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["rules"]))' "$work/src/grammar.json")" \
    "$(wc -c < "$work/src/grammar.json" | tr -d ' ')"

# 2. Build a parser with the real backend.
# The queries ship with the grammar, the way a parser package does.
mkdir -p "$work/queries" && cp "$here"/queries/*.scm "$work/queries/" || true
if ! (cd "$work" && "$ts" generate src/grammar.json > "$work/generate.log" 2>&1); then
    echo "FAIL  tree-sitter generate rejected the grammar"
    sed 's/^/      /' "$work/generate.log"
    exit 1
fi
if ! (cd "$work" && "$ts" build --output lib/tsg.dylib . > "$work/build.log" 2>&1); then
    echo "FAIL  the generated parser does not compile"
    sed 's/^/      /' "$work/build.log"
    exit 1
fi
printf 'ok   parser.c %s bytes, node-types.json %s bytes\n' \
    "$(wc -c < "$work/src/parser.c" | tr -d ' ')" \
    "$(wc -c < "$work/src/node-types.json" | tr -d ' ')"

# 3. The language description has to survive its own reverse direction.
"$tsg" --from-json "$work/src/grammar.json" -o "$work/roundtrip.tsg" &&
    "$tsg" "$work/roundtrip.tsg" -o "$work/roundtrip.json" &&
    python3 "$here/compare.py" selfhost "$work/src/grammar.json" "$work/roundtrip.json" ||
    failures=$((failures + 1))

# 4. Lexical coverage: the shapes the grammar's own patterns must accept, the
#    token boundary they must get right, and the flags `tsg` must refuse.
cat > "$work/coverage.tsg" <<'COVERAGE'
%name coverage;
%extras none;
a: /[a-z]+/i;
b: /x/i;
c: /\//;
d: /[/]/;
e: /[^\]a]/;
f: 'single \' quote';
g: "double \" quote";
h: '\x41\u0041\u{1F600}\n\t\\';
COVERAGE

# 4b. `surface/` is the description's own test suite: every directive form and
#     every expression shape the compiler accepts, once. Each file must compile
#     with the compiler *and* parse with the parser built from examples/tsg.tsg;
#     step 5b then requires those parses to contain every named node in the
#     grammar's node-types.json, so a construct cannot be added to the compiler
#     without the description learning it.
for file in "$here"/surface/*.tsg; do
    if ! "$tsg" "$file" -o "$work/$(basename "$file" .tsg).json" 2> "$work/surface.err"; then
        echo "FAIL  surface/$(basename "$file") does not compile"
        sed 's/^/      /' "$work/surface.err"
        failures=$((failures + 1))
    fi
done

"$tsg" "$work/coverage.tsg" -o "$work/coverage.json" &&
    [ "$(grep -c '"flags"' "$work/coverage.json")" -eq 2 ] ||
    failures=$((failures + 1))
printf 'ok   lexical coverage (%s escape forms, %s flag strings kept)\n' \
    8 "$(grep -c '"flags"' "$work/coverage.json")"

# Flags tree-sitter would accept and then silently drop are errors here, each
# pointing at its own character: "accepted and ignored" is the gotcha. A second
# `i` counts too — `i` is the whole accepted set, so `/x/ii` means nothing.
printf '%%name b;\n%%extras none;\na: /x/g;\nb: /y/u;\nc: /z/s;\nd: /w/ii;\n' > "$work/badflag.tsg"
if "$tsg" "$work/badflag.tsg" > "$work/badflag.json" 2> "$work/badflag.err"; then
    echo "FAIL  ignored flags compiled"
    failures=$((failures + 1))
elif grep -q 'regex flag `g` has no effect' "$work/badflag.err" &&
    grep -q 'regex flag `u` has no effect' "$work/badflag.err" &&
    grep -q 'regex flag `s` has no effect' "$work/badflag.err" &&
    grep -q 'regex flag `i` is repeated' "$work/badflag.err"; then
    printf 'ok   rejects ignored flags (%s errors: g, u, s, and a repeated i)\n' \
        "$(grep -c 'has no effect\|is repeated' "$work/badflag.err")"
else
    echo "FAIL  ignored flags were not reported"
    sed 's/^/      /' "$work/badflag.err"
    failures=$((failures + 1))
fi

# A rule named like a flag stays reachable: the space is the separator, so the
# author who really has a `g` rule is served by `/x/ g` and by the hint.
printf '%%name b;\n%%extras none;\ng: token(/g/);\nlong: seq(/x/, g);\nshort: /x/ g;\n' \
    > "$work/rulesym.tsg"
if "$tsg" "$work/rulesym.tsg" > /dev/null 2>&1; then
    printf 'ok   a rule named like a flag is reachable (`/x/ g`, `seq(/x/, g)`)\n'
else
    echo "FAIL  a rule named like a flag was rejected"
    failures=$((failures + 1))
fi
printf '%%name b;\n%%extras none;\ng: token(/g/);\na: /x/g;\n' > "$work/ruleflag.tsg"
if "$tsg" "$work/ruleflag.tsg" 2> "$work/ruleflag.err"; then
    echo "FAIL  /x/g with a `g` rule compiled"
    failures=$((failures + 1))
elif grep -q 'put a space after the literal: `/…/ g`' "$work/ruleflag.err"; then
    printf 'ok   /x/g points at the space (`/x/ g` is the sequence)\n'
else
    echo "FAIL  no space hint for /x/g with a `g` rule"
    sed 's/^/      /' "$work/ruleflag.err"
    failures=$((failures + 1))
fi

# The escape hatch: byte-for-byte reproduction of grammar.js output.
if "$tsg" --allow-ignored-flags "$work/badflag.tsg" 2>/dev/null |
    grep -q '"flags": "g"'; then
    printf 'ok   --allow-ignored-flags keeps them verbatim\n'
else
    echo "FAIL  --allow-ignored-flags did not pass the flag through"
    failures=$((failures + 1))
fi

# 5. Parse every .tsg file in reach.
files=()
for file in "$here"/examples/*.tsg; do files+=("$file"); done
for file in "$here"/.roundtrip/_root_*.tsg; do
    if [ -f "$file" ]; then files+=("$file"); fi
done
# The dogfooding grammar uses the whole current surface — level directives, `@`
# annotations, `()`, `$name` — so it is the file most likely to outrun the
# self-description.
if [ -f "$here/lua53/grammar.tsg" ]; then files+=("$here/lua53/grammar.tsg"); fi
if [ -f "$here/sqlite3/grammar.tsg" ]; then files+=("$here/sqlite3/grammar.tsg"); fi
files+=("$work/roundtrip.tsg" "$work/coverage.tsg")
surface=()
for file in "$here"/surface/*.tsg; do surface+=("$file"); files+=("$file"); done

# Correctness first: every file must parse without an ERROR or MISSING node.
# `--lib-path` keeps the parse independent of where the sources live.
"$ts" parse --lib-path "$work/lib/tsg.dylib" --lang-name tsg \
    "${files[@]}" > "$work/parse.log" 2> "$work/parse.err"
status=$?
if [ "$status" -ne 0 ]; then
    echo "FAIL  $status file(s) did not parse"
    grep -E "ERROR|MISSING" "$work/parse.err" | head -20 | sed 's/^/      /'
    failures=$((failures + 1))
else
    printf 'ok   parsed %s files, %s lines of tsg, no ERROR or MISSING node\n' \
        "${#files[@]}" "$(cat "${files[@]}" | wc -l | tr -d ' ')"
fi

# 5b. The other direction of the same question: which of the constructs the
#     description knows about does nothing actually spell? `coverage.py` compares
#     node-types.json against the trees of surface/, so a rule added to
#     examples/tsg.tsg with no fixture — or a fixture silently losing a construct
#     — fails here instead of becoming a stale comment.
"$ts" parse --lib-path "$work/lib/tsg.dylib" --lang-name tsg \
    "${surface[@]}" > "$work/surface.log" 2> "$work/surface.err"
coverage=$(python3 "$here/coverage.py" "$work/src/node-types.json" "$work/surface.log")
if [ $? -eq 0 ]; then
    printf 'ok   node coverage (%s)\n' "$coverage"
else
    sed 's/^/      /' <<< "$coverage"
    failures=$((failures + 1))
fi

# The flags are part of the literal, so the space decides and the token extent
# is the compiler's: `/x/i` is one 4-character `regex`, `/x/ i` is a regex plus
# the symbol `i`, and `/x/g` is a regex plus the symbol `g` — there is no letter
# class wide enough to turn that into an error, so the compiler rejects it.
printf '%%name b;\n%%extras none;\na: /x/i;\nc: /x/g;\nd: /x/ i;\n' > "$work/flagbound.tsg"
tree=$("$ts" parse --lib-path "$work/lib/tsg.dylib" --lang-name tsg \
    "$work/flagbound.tsg" 2>/dev/null)
if echo "$tree" | grep -q '(regex \[2, 3\] - \[2, 7\])' &&
    echo "$tree" | grep -q '(symbol \[3, 6\] - \[3, 7\]' &&
    echo "$tree" | grep -q '(symbol \[4, 7\] - \[4, 8\]'; then
    printf 'ok   token boundary (/x/i is one token; a space ends the flags)\n'
else
    echo "FAIL  the flags/symbol boundary is wrong"
    echo "$tree" | sed 's/^/      /'
    failures=$((failures + 1))
fi

# 6. The queries ship with the grammar, so they have to keep compiling against
#    it: a query that names a node type the grammar no longer has is a silent
#    no-op in an editor.
printf '%%name probe;\n%%extras none;\nempty: /x/;\nok: /x/i;\nother: /x/g;\nspaced: /x/ i;\n' \
    > "$work/flags-probe.tsg"
#    Both files, because compiling a query proves nothing about the patterns
#    that were just added: `surface/directives.tsg` is what makes `@operator` and
#    `@keyword` fire, so a capture that quietly stops matching is caught here.
if "$ts" query "$work/queries/highlights.scm" --lib-path "$work/lib/tsg.dylib" \
    --lang-name tsg "$work/flags-probe.tsg" "$here/surface/directives.tsg" \
    > "$work/query.log" 2>&1 &&
    grep -q 'capture: .* - operator,' "$work/query.log" &&
    grep -q 'capture: .* - keyword,' "$work/query.log"
then
    printf 'ok   queries compile against the grammar (%s captures, incl @operator/@keyword)\n' \
        "$(grep -c 'capture:' "$work/query.log")"
else
    echo "FAIL  the queries no longer compile against the grammar"
    sed 's/^/      /' "$work/query.log"
    failures=$((failures + 1))
fi

echo
if [ "$failures" -eq 0 ]; then
    echo "tsg parses tsg"
else
    echo "$failures failure(s)"
fi
exit $((failures > 0))
