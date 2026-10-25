#!/bin/sh
# Generate the metagrammar parser that `translate/build.rs` compiles in.
#
# `g2tsg` depends on `bison_flex/src/parser.c` at build time, and `clean.sh`
# deletes generated parsers — so without this step, `cargo build` in
# `bison_flex/translate` fails with "parser.c is missing" and the only way out
# is to know that this exists. It is idempotent and takes about a second.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}

for tool in "$tsg" "$ts"; do
    [ -x "$tool" ] || { echo "missing $tool" >&2; exit 2; }
done

mkdir -p "$here/src"
XDG_CACHE_HOME="$here/.cache" "$tsg" "$here/grammar.tsg" -o "$here/src/grammar.json"
(cd "$here" && XDG_CACHE_HOME="$here/.cache" "$ts" generate src/grammar.json)
rm -rf "$here/.cache"
printf 'ok   %s\n' "$here/src/parser.c"
