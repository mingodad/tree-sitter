#!/usr/bin/env bash
# Rebuild the PostgreSQL grammar from postgres16.g and parse the SQL samples.
#
# postgres16.g is 705 rules; the canonical-LR(1) table for it is ~354k states,
# so the table is built with `--lalr` (tree-sitter's LALR mode).
#
# The .tsg is *generated*: ../bison_flex/translate reads postgres16.g with the
# tree-sitter grammar in ../bison_flex/grammar.tsg and writes it.  This script
# fails if the committed grammar.tsg is out of date.
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

for tool in "$tsg" "$ts" python3; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

mkdir -p "$work/src" "$work/lib" "$work/cache"

export XDG_CACHE_HOME="$work/cache"

start=$(python3 -c 'import time; print(time.time())')

# 1. Generate the .tsg from the source .g, and fail if the committed copy is
#    stale.  (The committed copy is what `tsg` compiles below.)
"$g2" "$root/postgres16.g" -o "$work/gen.tsg" --name postgres \
    --title "PostgreSQL 16 SQL grammar, translated from ../postgres16.g (Bison + Flex)." \
    --header "$here/header.txt" \
    --conflicts "$here/conflicts.txt" \
    --bodies "$here/bodies.txt"
if ! cmp -s "$here/grammar.tsg" "$work/gen.tsg"; then
    echo "FAIL grammar.tsg is stale; the new translation is $work/gen.tsg" >&2
    diff "$here/grammar.tsg" "$work/gen.tsg" | head >&2
    exit 1
fi

# 2. The regex-based translator is the reference for the translation itself
#    (the header comments are the only difference).
python3 "$here/translate.py" "$work/reference.tsg"
if ! diff <(sed -n '/^%name/,$p' "$work/reference.tsg") \
           <(sed -n '/^%name/,$p' "$here/grammar.tsg"); then
    echo "FAIL g2tsg and translate.py disagree" >&2
    exit 1
fi

"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
(cd "$here" && "$ts" generate "$work/src/grammar.json" --lalr)
gen=$(python3 -c "import time; print('%.2f' % (time.time() - $start))")

# `tree-sitter build` reads the parser from the grammar directory.
mkdir -p "$here/src"
cp "$work/src/parser.c" "$work/src/grammar.json" "$here/src/"
mkdir -p "$here/src/tree_sitter"
cp "$work/src/tree_sitter/"*.h "$here/src/tree_sitter/" 2>/dev/null || true
(cd "$here" && "$ts" build --output "$work/lib/postgres.dylib" .)

for sql in "$here"/*.sql; do
    name=$(basename "$sql" .sql)
    "$ts" parse --lib-path "$work/lib/postgres.dylib" --lang-name postgres \
        "$sql" > "$work/$name.log" 2> "$work/$name.err"
    if grep -q 'ERROR\|MISSING' "$work/$name.log"; then
        echo "FAIL $name  (tree has ERROR or MISSING)" >&2
        grep -n 'ERROR\|MISSING' "$work/$name.log" | head >&2
        exit 1
    fi
    printf 'ok   %-10s %s lines\n' "$name" "$(wc -l < "$sql" | tr -d ' ')"
done

printf 'ok   %s rules, %s states, %s parser.c bytes, %s samples, generate %ss\n' \
    "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["rules"]))' "$work/src/grammar.json")" \
    "$(grep -c '\[STATE(' "$work/src/parser.c")" \
    "$(wc -c < "$work/src/parser.c" | tr -d ' ')" \
    "$(ls "$here"/*.sql | wc -l | tr -d ' ')" \
    "$gen"
