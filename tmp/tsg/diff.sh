#!/usr/bin/env bash
# Differential test: for every examples/<name>.tsg that has a matching
# test/fixtures/test_grammars/<name>/grammar.js, compare
#
#   tsg examples/<name>.tsg
#
# against the reference JSON produced by running the real JavaScript DSL
# (crates/generate/src/dsl.js) under node on the original grammar.js.
#
# The comparison is `tree-sitter generate --stamp`, not a text compare: the
# stamp is a hash of the parser the generator actually builds, after tokens are
# extracted, inlines expanded and conflicts resolved. Two grammars with the
# same stamp are the same parser, so a notational difference (rule order,
# parentheses around a choice that does not change it) passes and a real one
# fails. compare.py is run afterwards only to explain a failure.
#
# Usage:  ./diff.sh            (uses ./target/debug/tsg; override with TSG=)
#         TSG=/path/to/tsg ./diff.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)  # repository root
work="$here/.diff"
tsg=${TSG:-$here/target/debug/tsg}
ts=${TS:-$here/bin/tree-sitter}

if [ ! -x "$ts" ]; then
    echo "error: no tree-sitter CLI at $ts (see HANDOFF.md; override with TS=)" >&2
    exit 2
fi

if [ ! -x "$tsg" ]; then
    echo "error: no tsg binary at $tsg (run \`cargo build\` or set TSG=)" >&2
    exit 2
fi

mkdir -p "$work"
printf '{"type": "module"}\n' > "$work/package.json"
cp "$root/crates/generate/src/dsl.js" "$work/dsl.js" || exit 2

failures=0
checked=0

for tsg_file in "$here"/examples/*.tsg; do
    name=$(basename "$tsg_file" .tsg)
    fixture="$root/test/fixtures/test_grammars/$name"
    if [ ! -f "$fixture/grammar.js" ]; then
        echo "skip $name  (no test/fixtures/test_grammars/$name/grammar.js)"
        continue
    fi

    cp "$fixture/grammar.js" "$work/$name.js"

    if ! TREE_SITTER_GRAMMAR_PATH="$work/$name.js" \
            node "$work/dsl.js" > "$work/$name.expected.json" 2> "$work/$name.js.err"; then
        echo "FAIL $name  (reference dsl.js failed)"
        sed 's/^/     /' "$work/$name.js.err"
        failures=$((failures + 1))
        continue
    fi

    if ! "$tsg" "$tsg_file" -o "$work/$name.actual.json" 2> "$work/$name.tsg.err"; then
        echo "FAIL $name  (tsg failed)"
        sed 's/^/     /' "$work/$name.tsg.err"
        failures=$((failures + 1))
        continue
    fi

    checked=$((checked + 1))
    expected_stamp=$("$ts" generate "$work/$name.expected.json" --stamp 2>&1)
    actual_stamp=$("$ts" generate "$work/$name.actual.json" --stamp 2>&1)
    if [ "$expected_stamp" != "$actual_stamp" ]; then
        echo "FAIL $name  (the two front ends build different parsers)"
        printf '%s\n' "$expected_stamp" | sed 's/^/     dsl.js: /'
        printf '%s\n' "$actual_stamp" | sed 's/^/     tsg:    /'
        python3 "$here/compare.py" "$name" \
            "$work/$name.expected.json" "$work/$name.actual.json" | sed 's/^/     /'
        failures=$((failures + 1))
    fi
done

echo
if [ "$failures" -eq 0 ]; then
    echo "all $checked grammars build the same parser as the JavaScript DSL"
else
    echo "$failures of $checked grammars differ"
fi
exit $((failures > 0))
