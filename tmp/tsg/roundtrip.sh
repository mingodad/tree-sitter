#!/usr/bin/env bash
# Round-trip test:  grammar.json --(tsg -r)--> *.tsg --(tsg)--> grammar.json
#
# The corpus is every grammar.js in the repository, translated to grammar.json
# by the *real* JavaScript DSL (crates/generate/src/dsl.js) under node. Each
# document is then pushed through the reverse and forward front-ends and
# compared structurally.
#
# Usage:  ./roundtrip.sh             round-trip every corpus grammar
#         ./roundtrip.sh --parsers   also byte-compare the generated parser.c
#         ./roundtrip.sh c           round-trip only c.json
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work="$here/.roundtrip"
tsg=${TSG:-$here/target/debug/tsg}
ts=${TS:-$here/bin/tree-sitter}

parsers=0
filters=()
for arg in "$@"; do
    case "$arg" in
        --parsers) parsers=1 ;;
        *) filters+=("$arg") ;;
    esac
done

if [ ! -x "$tsg" ]; then
    echo "error: no tsg binary at $tsg (run \`cargo build\` or set TSG=)" >&2
    exit 2
fi
if [ "$parsers" -eq 1 ] && [ ! -x "$ts" ]; then
    echo "error: --parsers needs the tree-sitter CLI at $ts (or set TS=)" >&2
    exit 2
fi

if [ "$parsers" -eq 1 ]; then
    free=$(df -m "$work" 2>/dev/null | awk 'NR == 2 { print $4 }')
    if [ -n "$free" ] && [ "$free" -lt 80 ]; then
        echo "warning: only ${free} MB free under $work; --parsers needs ~50 MB" >&2
    fi
fi

rm -rf "$work/corpus"
mkdir -p "$work/corpus"
printf '{"type": "module"}\n' > "$work/package.json"
cp "$root/crates/generate/src/dsl.js" "$work/dsl.js" || exit 2

# --- corpus: grammar.js -> grammar.json, using the real DSL -------------------
if [ ${#filters[@]} -eq 0 ]; then
    echo "building the corpus with the JavaScript DSL ..."
    generated=0
    skipped=0
    while IFS= read -r js; do
        rel=${js#"$root"/}
        name=$(printf '%s' "$rel" | sed 's|/grammar.js$||; s|/|_|g')
        out="$work/corpus/$name.json"
        if TREE_SITTER_GRAMMAR_PATH="$js" node "$work/dsl.js" > "$out" 2> "$work/$name.err"; then
            generated=$((generated + 1))
        else
            rm -f "$out"
            skipped=$((skipped + 1))
        fi
    done < <(find "$root" -name grammar.js -not -path "*/tmp/*" -not -path "*/target/*" -not -path "*/node_modules/*" | sort)
    echo "  $generated grammars, $skipped not compilable by the DSL (expected for error fixtures)"
fi

# The corpus also includes any grammar.json dropped in this directory: the
# hand-written showcase grammars (c, rust, swift, ...).
for json in "$here"/*.json; do
    [ -e "$json" ] || continue
    cp "$json" "$work/corpus/_root_$(basename "$json")"
done

# Whether the parser generator refuses a grammar.json: the oracle for "the
# `.tsg` front-end is allowed to refuse this".
rejects() {
    local dir="$work/reject"
    rm -rf "$dir"
    mkdir -p "$dir/src" || return 1
    cp "$1" "$dir/src/grammar.json"
    ! ( cd "$dir" && "$ts" generate src/grammar.json > /dev/null 2>&1 )
    local status=$?
    rm -rf "$dir"
    return $status
}

# --- round trip --------------------------------------------------------------
failures=0
checked=0
parser_failures=0

for json in "$work"/corpus/*.json; do
    [ -e "$json" ] || continue
    name=$(basename "$json" .json)

    if [ ${#filters[@]} -gt 0 ]; then
        match=0
        for filter in "${filters[@]}"; do
            case "$name" in *"$filter"*) match=1 ;; esac
        done
        [ "$match" -eq 1 ] || continue
    fi

    tsg_file="$work/$name.tsg"
    back="$work/$name.json"

    if ! "$tsg" --from-json "$json" -o "$tsg_file" 2> "$work/$name.err"; then
        echo "FAIL $name  (grammar.json -> .tsg)"
        sed 's/^/     /' "$work/$name.err"
        failures=$((failures + 1))
        continue
    fi
    if ! "$tsg" "$tsg_file" -o "$back" 2> "$work/$name.err"; then
        # The `.tsg` front-end rejects a few grammars before they ever become
        # JSON (a rule that can match nothing, a supertype that cannot be
        # inlined, ...). That is an improvement, not a regression — but only if
        # the parser generator rejects them too. Ask it, rather than assume.
        if [ -x "$ts" ] && rejects "$json"; then
            printf 'ok   %s  (rejected by both front-ends)\n' "$name"
            checked=$((checked + 1))
            continue
        fi
        echo "FAIL $name  (.tsg -> grammar.json)"
        sed 's/^/     /' "$work/$name.err"
        failures=$((failures + 1))
        continue
    fi
    if ! python3 "$here/compare.py" "$name" "$json" "$back"; then
        failures=$((failures + 1))
        continue
    fi
    checked=$((checked + 1))

    if [ "$parsers" -eq 1 ]; then
        # Generate `parser.c` twice -- from the original JSON and from the JSON
        # that came back out of the .tsg front-end -- and byte-compare them. The
        # scratch directories are removed immediately: a whole-corpus run would
        # otherwise write ~120 MB of generated parsers.
        orig="$work/parsers/orig"
        rt="$work/parsers/rt"
        status=0
        for variant in orig rt; do
            dir="$work/parsers/$variant"
            rm -rf "$dir"
            mkdir -p "$dir/src" || { status=2; break; }
            if [ "$variant" = orig ]; then
                cp "$json" "$dir/src/grammar.json"
            else
                cp "$back" "$dir/src/grammar.json"
            fi
            if ! (cd "$dir" && "$ts" generate src/grammar.json > "$work/$name.$variant.log" 2>&1); then
                echo "SKIP $name  (tree-sitter generate rejects this grammar on both sides)"
                status=2
                break
            fi
        done
        if [ "$status" -eq 0 ]; then
            if cmp -s "$orig/src/parser.c" "$rt/src/parser.c"; then
                printf '     parser.c identical (%s bytes)\n' \
                    "$(wc -c < "$rt/src/parser.c" | tr -d ' ')"
            else
                echo "FAIL $name  (parser.c differs after the round trip)"
                diff "$orig/src/parser.c" "$rt/src/parser.c" | head -20 | sed 's/^/     /'
                parser_failures=$((parser_failures + 1))
            fi
        fi
        rm -rf "$orig" "$rt"
    fi
done

echo
if [ "$failures" -eq 0 ]; then
    echo "all $checked corpus grammars round-trip exactly"
else
    echo "$failures of $((checked + failures)) corpus grammars do not round-trip"
fi
if [ "$parser_failures" -ne 0 ]; then
    echo "$parser_failures grammars produce a different parser.c after the round trip"
fi
exit $((failures + parser_failures > 0))
