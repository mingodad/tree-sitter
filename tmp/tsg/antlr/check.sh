#!/usr/bin/env bash
# A second dog-food grammar: ANTLR v4.5 written as a Bison + Flex grammar.
#
# Smaller and less lucky than postgres16:
#
#   * 0 precedence levels — the source declares none,
#   * 0 conflicts — it is a plain LALR grammar once the empty productions are
#     gone (postgres16 needs 12 `%conflicts`, all of them `?` artifacts),
#   * but 22 productions are dropped, because the Flex section cannot express
#     what the source needs: ANTLR lexer *modes* (`%x`, `<state> { … }`) and
#     target-language actions `{ … }` are not Flex.  So the parser accepts a
#     skeleton (`grammar TG;`, `mode MM;`, …) and refuses a real grammar file.
#
# The .tsg is generated; this script fails if the committed copy is stale.
set -euo pipefail

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
    cat >&2 <<EOF
missing g2tsg (the .g -> .tsg translator).  Build it first:

    cd $root/bison_flex/translate && cargo build --release

then set G2TSG=$root/bison_flex/translate/target/release/g2tsg, or put it on PATH.
EOF
    exit 2
fi

for tool in "$tsg" "$ts"; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

mkdir -p "$work/src" "$work/lib" "$work/cache"
export XDG_CACHE_HOME="$work/cache"

# 1. Generate the .tsg from the source .g, and fail if the committed copy is
#    stale.
"$g2" "$here/antlr-v4.5.g" -o "$work/gen.tsg" --name grammar \
    --title "ANTLR 4.5 grammars, translated from antlr-v4.5.g (Bison + Flex)." \
    > "$work/gen.log" 2>&1 || { cat "$work/gen.log" >&2; exit 1; }
sed 's/^note: /note: /' "$work/gen.log" >&2
#    Compare grammars, not text: `tsg --diff` ignores comments and formatting and
#    names the rule and field that drifted.  The `cmp` below still runs, to say
#    when the committed copy is not a byte-for-byte copy of the translation.
if ! "$tsg" "$here/grammar.tsg" --diff "$work/gen.tsg" >&2; then
    echo "FAIL grammar.tsg is not the same grammar as the new translation $work/gen.tsg" >&2
    exit 1
fi
if ! cmp -s "$here/grammar.tsg" "$work/gen.tsg"; then
    echo "note: grammar.tsg and $work/gen.tsg differ only in comments or formatting" >&2
fi

# 2. `tsg` accepts the translated grammar, and `tree-sitter generate` finds no
#    conflict — no `%conflicts` needed here at all.
"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
(cd "$here" && "$ts" generate "$work/src/grammar.json" --lalr)

# `tree-sitter build` reads the parser from the grammar directory.
mkdir -p "$here/src"
cp "$work/src/grammar.json" "$here/src/"
mkdir -p "$here/src/tree_sitter"
cp "$work/src/tree_sitter/"*.h "$here/src/tree_sitter/" 2>/dev/null || true
cp "$work/src/parser.c" "$here/src/parser.c"
(cd "$here" && "$ts" build --output "$work/lib/grammar.dylib" .)

# 3. Every sample has to parse without ERROR or MISSING.
for sample in "$here"/samples/*.antlr; do
    name=$(basename "$sample" .antlr)
    "$ts" parse --lib-path "$work/lib/grammar.dylib" --lang-name grammar \
        "$sample" > "$work/$name.log" 2> "$work/$name.err"
    if grep -q 'ERROR\|MISSING' "$work/$name.log"; then
        echo "FAIL $name (tree has ERROR or MISSING)" >&2
        cat "$work/$name.log" >&2
        exit 1
    fi
    printf 'ok   %-32s %s\n' "$name" "$(head -1 "$sample")"
done

printf 'ok   %s rules, %s tokens, %s states, %s parser.c bytes, %s samples\n' \
    "$(grep -c '^[a-zA-Z_][a-zA-Z_0-9]*:$' "$here/grammar.tsg")" \
    "$(sed -n '/^tokens {/,/^}/p' "$here/grammar.tsg" | grep -c ':')" \
    "$(grep -o 'STATE_COUNT [0-9]*' "$work/src/parser.c" | head -1 | grep -o '[0-9]*')" \
    "$(wc -c < "$work/src/parser.c" | tr -d ' ')" \
    "$(ls "$here"/samples/*.antlr | wc -l | tr -d ' ')"
