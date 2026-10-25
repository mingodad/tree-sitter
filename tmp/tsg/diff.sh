#!/usr/bin/env bash
# Differential test: for every examples/<name>.tsg that has a matching
# test/fixtures/test_grammars/<name>/grammar.js, compare
#
#   tsg examples/<name>.tsg
#
# against the reference JSON produced by running the real JavaScript DSL
# (crates/generate/src/dsl.js) under node on the original grammar.js.
#
# Usage:  ./diff.sh            (uses ./target/debug/tsg; override with TSG=)
#         TSG=/path/to/tsg ./diff.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)  # repository root
work="$here/.diff"
tsg=${TSG:-$here/target/debug/tsg}

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
    if ! python3 "$here/compare.py" "$name" \
            "$work/$name.expected.json" "$work/$name.actual.json"; then
        failures=$((failures + 1))
    fi
done

echo
if [ "$failures" -eq 0 ]; then
    echo "all $checked grammars match the JavaScript DSL byte-for-byte (structurally)"
else
    echo "$failures of $checked grammars differ"
fi
exit $((failures > 0))
