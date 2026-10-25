#!/usr/bin/env bash
# Rebuild the SQLite grammar from sqlite3.lemonex and parse the samples.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}
work="$here/.check"

for tool in "$tsg" "$ts" python3; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 2; }
done

rm -rf "$work"
mkdir -p "$work/src" "$work/lib" "$work/cache"
export XDG_CACHE_HOME="$work/cache"

python3 "$here/translate.py"
"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
# `generate <file>` writes parser.c next to the file, but `build` finds the parser
# through a tree-sitter.json: point one at $work so the two agree (they disagreed
# when $here/tree-sitter.json said "path": ".", so build looked in $here/src,
# where the generated parser never lands).
cat > "$work/tree-sitter.json" <<'JSON'
{"grammars": [{"name": "sqlite3", "camelcase": "Sqlite3", "scope": "source.sqlite3", "path": "."}],
 "metadata": {"version": "0.0.0", "license": "MIT", "description": "SQLite SQL", "links": {}}}
JSON
(cd "$here" && "$ts" generate "$work/src/grammar.json")
"$ts" build --output "$work/lib/sqlite3.dylib" "$work"

for sql in "$here"/*.sql; do
    name=$(basename "$sql" .sql)
    "$ts" parse --lib-path "$work/lib/sqlite3.dylib" --lang-name sqlite3 \
        "$sql" > "$work/$name.log" 2> "$work/$name.err"
    if grep -q 'ERROR\|MISSING' "$work/$name.log"; then
        echo "FAIL $name  (tree has ERROR or MISSING)" >&2
        grep -n 'ERROR\|MISSING' "$work/$name.log" | head >&2
        exit 1
    fi
    printf 'ok   %-10s %s lines\n' "$name" "$(wc -l < "$sql" | tr -d ' ')"
done

printf 'ok   %s rules, %s parser.c bytes, %s samples\n' \
    "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["rules"]))' "$work/src/grammar.json")" \
    "$(wc -c < "$work/src/parser.c" | tr -d ' ')" \
    "$(ls "$here"/*.sql | wc -l | tr -d ' ')"
