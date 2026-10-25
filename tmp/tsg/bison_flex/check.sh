#!/usr/bin/env bash
# Parse the metagrammar and the Postgres stress grammar, then check the counts
# the translator relies on.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}
work="$here/.check"

for tool in "$tsg" "$ts"; do
    if [ ! -x "$tool" ]; then
        echo "error: missing $tool" >&2
        exit 2
    fi
done

rm -rf "$work"
mkdir -p "$work/src" "$work/lib"
export XDG_CACHE_HOME="$work/cache"
export TREE_SITTER_LIBDIR="$work/lib"

# The translator's build.rs compiles the metagrammar parser from `src/parser.c`,
# which `clean.sh` deletes as generated output; regenerate it first so that
# "run the gate" is always a complete answer to "make this work again".
TSG="$tsg" TS="$ts" "$here/build-parser.sh" >/dev/null

"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
(cd "$work" && "$ts" generate src/grammar.json)
(cd "$work" && "$ts" build --output lib/bison_flex.dylib . >/dev/null)

"$ts" parse --lib-path "$work/lib/bison_flex.dylib" --lang-name bison_flex \
    --xml "$root/postgres16.g" > "$work/postgres16.xml"
"$ts" parse --lib-path "$work/lib/bison_flex.dylib" --lang-name bison_flex \
    --xml "$root/bison-flex.g" > "$work/bison-flex.xml"

if grep -qE '<(ERROR|MISSING)' "$work"/*.xml; then
    echo "FAIL  a grammar parsed with ERROR or MISSING" >&2
    grep -nE '<(ERROR|MISSING)' "$work"/*.xml | head >&2
    exit 1
fi

python3 "$here/extract.py" "$work/postgres16.xml" "$work/bison-flex.xml"

BISON_FLEX_HOME="$here" python3 - "$work/postgres16.xml" "$work/bison-flex.xml" <<'PY'
import os
import sys
from pathlib import Path
sys.path.insert(0, os.environ["BISON_FLEX_HOME"])
import extract

pg = extract.summary(Path(sys.argv[1]))
self = extract.summary(Path(sys.argv[2]))
expected = {
    "rules": 705,
    "alternatives": 3282,
    "token_directives": 513,
    "token_names": 513,
    "precedence_levels": 27,
    "empty_alternatives": 187,
    "prec_annotations": 55,
    "section_separators": 4,
}
for key, value in expected.items():
    if pg[key] != value:
        raise SystemExit(f"postgres16.g: {key}={pg[key]}, expected {value}")
if self["rules"] != 69 or self["section_separators"] != 4:
    raise SystemExit(f"bison-flex.g: rules={self['rules']}, expected 69")
print("ok   postgres16.g and bison-flex.g parse with the expected shape")
PY
