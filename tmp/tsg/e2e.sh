#!/usr/bin/env bash
# End-to-end differential test.
#
# For each examples/<name>.tsg with a matching test fixture:
#
#   A. reference:  grammar.js  --(tree-sitter generate)-->  src/parser.c
#   B. ours:       <name>.tsg  --(tsg)--> src/grammar.json
#                              --(tree-sitter generate)-->  src/parser.c
#
# Then byte-compare the two parser.c files (and node-types.json).
#
# This exercises the entire real backend: parse_grammar, prepare_grammar,
# build_tables and render — nothing is stubbed.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work="$here/.e2e"
tsg=${TSG:-$here/target/debug/tsg}
ts=${TS:-$here/bin/tree-sitter}

for tool in "$tsg" "$ts"; do
    if [ ! -x "$tool" ]; then
        echo "error: missing $tool" >&2
        exit 2
    fi
done

rm -rf "$work"
mkdir -p "$work"

failures=0
checked=0

for tsg_file in "$here"/examples/*.tsg; do
    name=$(basename "$tsg_file" .tsg)
    fixture="$root/test/fixtures/test_grammars/$name"
    if [ ! -f "$fixture/grammar.js" ]; then
        # No reference grammar: still check that the whole backend accepts the
        # emitted JSON (this is how the hand-written showcase grammars run).
        smoke_dir="$work/$name-smoke"
        mkdir -p "$smoke_dir/src"
        if ! "$tsg" "$tsg_file" -o "$smoke_dir/src/grammar.json"; then
            echo "FAIL $name  (tsg failed)"
            failures=$((failures + 1))
            continue
        fi
        if ! (cd "$smoke_dir" && "$ts" generate src/grammar.json > "$work/$name.smoke.log" 2>&1); then
            echo "FAIL $name  (tree-sitter generate failed)"
            sed 's/^/     /' "$work/$name.smoke.log"
            failures=$((failures + 1))
            continue
        fi
        checked=$((checked + 1))
        printf 'ok   %s  (no reference; generated %s bytes of parser.c)\n' \
            "$name" "$(wc -c < "$smoke_dir/src/parser.c" | tr -d ' ')"
        continue
    fi

    # A. reference parser, generated from the original grammar.js
    js_dir="$work/$name-js"
    mkdir -p "$js_dir/src"
    cp "$fixture/grammar.js" "$js_dir/grammar.js"
    if ! (cd "$js_dir" && "$ts" generate > "$work/$name.js.log" 2>&1); then
        echo "FAIL $name  (tree-sitter generate <grammar.js> failed)"
        sed 's/^/     /' "$work/$name.js.log"
        failures=$((failures + 1))
        continue
    fi

    # B. our parser, generated from the .tsg front-end
    our_dir="$work/$name-tsg"
    mkdir -p "$our_dir/src"
    if ! "$tsg" "$tsg_file" -o "$our_dir/src/grammar.json"; then
        echo "FAIL $name  (tsg failed)"
        failures=$((failures + 1))
        continue
    fi
    if ! (cd "$our_dir" && "$ts" generate src/grammar.json > "$work/$name.tsg.log" 2>&1); then
        echo "FAIL $name  (tree-sitter generate <grammar.json> failed)"
        sed 's/^/     /' "$work/$name.tsg.log"
        failures=$((failures + 1))
        continue
    fi

    checked=$((checked + 1))
    ok=1
    for artefact in src/parser.c src/node-types.json; do
        if ! cmp -s "$js_dir/$artefact" "$our_dir/$artefact"; then
            echo "FAIL $name  ($artefact differs)"
            diff "$js_dir/$artefact" "$our_dir/$artefact" | head -20 | sed 's/^/     /'
            ok=0
        fi
    done
    if [ "$ok" -eq 1 ]; then
        printf 'ok   %s  (parser.c %s bytes, node-types.json %s bytes)\n' \
            "$name" \
            "$(wc -c < "$our_dir/src/parser.c" | tr -d ' ')" \
            "$(wc -c < "$our_dir/src/node-types.json" | tr -d ' ')"
    else
        failures=$((failures + 1))
    fi
done

echo
if [ "$failures" -eq 0 ]; then
    echo "all $checked grammars build; every one with a reference is byte-identical"
else
    echo "$failures of $checked grammars differ"
fi
exit $((failures > 0))
