#!/usr/bin/env bash
# Parse every `.g` in the corpus with bison_flex/grammar.tsg and classify the
# constructs the metagrammar cannot read yet.
#
#   ./bison_flex/corpus.sh                 # default corpus (parsertl playground)
#   ./bison_flex/corpus.sh DIR             # another directory of `.g` files
#   ./bison_flex/corpus.sh DIR --only sql  # substring filter
#
# One XML file at a time: 358 grammars produce far more XML than the ramdisk
# holds.  Everything scratch stays in bison_flex/.corpus/.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tsg=${TSG:-$root/target/debug/tsg}
ts=${TS:-$root/bin/tree-sitter}
corpus=${1:-/Users/mingo/dev/parsertl-playground/playground}
shift || true
work="$here/.corpus"

for tool in "$tsg" "$ts" "$corpus"; do
    if [ ! -e "$tool" ]; then
        echo "error: missing $tool" >&2
        exit 2
    fi
done

rm -rf "$work"
mkdir -p "$work/src" "$work/lib"
export XDG_CACHE_HOME="$work/cache"
export TREE_SITTER_LIBDIR="$work/lib"

"$tsg" "$here/grammar.tsg" -o "$work/src/grammar.json"
(cd "$work" && "$ts" generate src/grammar.json)
(cd "$work" && "$ts" build --output lib/bison_flex.dylib . >/dev/null)

python3 "$here/corpus.py" "$corpus" \
    --ts "$ts" --lib "$work/lib/bison_flex.dylib" --work "$work/xml" \
    --json "$work/report.json" "$@"
