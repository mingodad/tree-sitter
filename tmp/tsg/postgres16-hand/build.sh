#!/bin/sh
# Build and exercise the hand-fixed copy of the postgres16 grammar.
#
#   ./build.sh                 parse ../../postgres16/sql_basic.sql
#   ./build.sh some.sql        parse something else
#
# `postgres16/check.sh` cannot run at all right now: it regenerates from
# postgres16.g and compares the result textually against the checked-in
# grammar.tsg, and the two disagree because the translator was fixed after the
# checked-in copy was taken.  This directory is that copy, brought forward by
# hand so the grammar can still be tested.
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/.." && pwd -P)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}
sample=${1:-$root/postgres16/sql_basic.sql}
work=$here/.build

for tool in "$tsg" "$ts"; do
    [ -x "$tool" ] || { echo "missing $tool" >&2; exit 2; }
done

rm -rf "$work"
mkdir -p "$work/src"
XDG_CACHE_HOME="$work/cache" "$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
(cd "$work" && XDG_CACHE_HOME="$work/cache" "$ts" generate src/grammar.json --lalr 2>"$work/generate.log")
cc -O0 -fPIC -shared -I "$work/src" "$work/src/parser.c" -o "$work/pg.dylib"
XDG_CACHE_HOME="$work/cache" "$ts" parse --lib-path "$work/pg.dylib" --lang-name postgres "$sample" > "$work/tree.log" 2>&1 || true

states=$(sed -n 's/^#define STATE_COUNT \([0-9]*\)$/\1/p' "$work/src/parser.c")
bytes=$(wc -c < "$work/src/parser.c" | tr -d ' ')
bad=$(grep -c 'ERROR\|MISSING' "$work/tree.log" || true)
if [ "$bad" = 0 ]; then
    echo "ok   $states states, $bytes parser.c bytes, $(wc -l < "$work/tree.log" | tr -d ' ') tree lines, 0 ERROR/MISSING"
else
    echo "FAIL $bad ERROR/MISSING nodes in $sample" >&2
    grep -n 'ERROR\|MISSING' "$work/tree.log" | head -10 >&2
    exit 1
fi
