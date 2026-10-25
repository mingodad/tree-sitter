#!/bin/sh
# Rebuild the Bison grammar from bison.g and parse carbon-lang.y with it.
#
# bison.g is a grammar for the yacc dialect; carbon-lang.y is written in that
# dialect, so parsing it is the end-to-end test of the translation.
#
# The lexer comes from bison.g itself.  Its `<ACTION_CODE_ST>` start condition is
# a balanced region, so `g2tsg` writes `%external ACTION_CODE = delimited('{',
# '}')` and `tsg` generates `src/scanner.c` for it: `actions.y` is the sample
# that needs those regions (nested braces included).  `<EPILOGUE_ST>` is not
# translated, and `g2tsg` reports it and the one production it drops.
# `patch.py` applies the one edit that is left (see its header).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}
g2=${G2TSG:-}
work="$here/.check"

if [ -z "$g2" ]; then
    g2=$(command -v g2tsg || true)
fi
if [ -z "$g2" ]; then
    echo "missing g2tsg (the .g -> .tsg translator); set G2TSG=$root/bin/g2tsg" >&2
    exit 2
fi

for tool in "$tsg" "$ts" python3; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

mkdir -p "$work/src" "$work/lib" "$work/cache"
export XDG_CACHE_HOME="$work/cache"

# 1. Translate, then apply the hand edits.  The committed grammar.tsg must stay
#    the same grammar as that output: comments and layout are allowed to differ.
"$g2" "$here/bison.g" -o "$work/gen.tsg" --name bison
python3 "$here/patch.py" "$work/gen.tsg"
if ! "$tsg" "$here/grammar.tsg" --diff "$work/gen.tsg" >&2; then
    echo "FAIL grammar.tsg is not the same grammar as the new translation $work/gen.tsg" >&2
    exit 1
fi
if ! cmp -s "$here/grammar.tsg" "$work/gen.tsg"; then
    echo "note: grammar.tsg and $work/gen.tsg differ only in comments or formatting" >&2
fi

# 2. Compile and generate.
"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
cat > "$work/tree-sitter.json" <<'JSON'
{"grammars": [{"name": "bison", "camelcase": "Bison", "scope": "source.bison", "path": "."}],
 "metadata": {"version": "0.0.0", "license": "MIT", "description": "GNU Bison grammar language", "links": {}}}
JSON
(cd "$here" && "$ts" generate "$work/src/grammar.json")
"$ts" build --output "$work/lib/bison.dylib" "$work"

# 3. Parse the samples: whole .y files, which is the language bison.g describes.
#    `actions.y` is the one that exercises the generated scanner — it has
#    semantic actions with nested braces, so ACTION_CODE only appears if the
#    delimited-region token works.
summary=
for sample in carbon-lang.y actions.y; do
    name=$(basename "$sample" .y)
    "$ts" parse --lib-path "$work/lib/bison.dylib" --lang-name bison \
        "$here/$sample" > "$work/$name.log" 2> "$work/$name.err"
    errors=$(grep -c '(ERROR' "$work/$name.log" || true)
    missing=$(grep -c '(MISSING' "$work/$name.log" || true)
    if [ "$errors" != 0 ] || [ "$missing" != 0 ]; then
        echo "FAIL $sample: $errors ERROR, $missing MISSING" >&2
        grep -n '(ERROR\|(MISSING' "$work/$name.log" | head >&2
        exit 1
    fi
    summary="$summary $sample: $(wc -l < "$work/$name.log" | tr -d ' ') nodes,"
done

# The scanner has to have been used, not just compiled: an action_code node per
# `{ … }` is what proves the region token was returned.
regions=$(grep -c 'ACTION_CODE' "$work/actions.log" || true)
if [ "$regions" = 0 ]; then
    echo "FAIL no ACTION_CODE node: the delimited-region scanner did not run" >&2
    exit 1
fi

printf 'ok   %s rules, %s parser.c bytes,%s %s ACTION_CODE nodes, 0 ERROR\n' \
    "$(grep -c 'type' "$work/src/grammar.json" | tr -d ' ')" \
    "$(wc -c < "$work/src/parser.c" | tr -d ' ')" \
    "$summary" "$regions"
