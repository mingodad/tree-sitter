#!/bin/sh
# Build the tools that are meant to be *installed*, and put them in bin/.
#
#   ./install.sh           tsg (release) -> bin/tsg, plus bin/VERSIONS
#   ./install.sh g2tsg     also refresh bin/g2tsg from bison_flex/translate
#
# `bin/` is gitignored on purpose: every file in it is a build product, and this
# script plus HANDOFF.md §"Toolchain provenance" is the whole recipe.  The point
# of installing `tsg` at a fixed path is that the patched generator shells out to
# it: `tree-sitter generate grammar.tsg` looks for `tsg` on PATH (or in
# TREE_SITTER_TSG), and `bin/tsg` makes that work without naming a path in
# target/, which changes profile and disappears on `cargo clean`.
#
# The gates (e2e.sh, diff.sh, roundtrip.sh, selfhost.sh) deliberately do *not*
# use bin/tsg: they want the binary cargo just built from the working tree, so
# they keep defaulting to target/debug/tsg.
set -eu

here=$(cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

if [ -f "$here/.ch/registry" ] || [ -d "$here/.ch" ]; then
    CARGO_HOME=${CARGO_HOME:-$here/.ch}
    export CARGO_HOME
fi

echo "== building tsg (release)"
cargo build --release --offline -q -p tsg

mkdir -p bin
cp target/release/tsg bin/tsg
echo "== installed bin/tsg ($(wc -c < bin/tsg | tr -d ' ') bytes, $(bin/tsg --version 2>&1 | head -1))"

if [ "${1:-}" = g2tsg ]; then
    echo "== building g2tsg (release)"
    (cd bison_flex/translate && cargo build --release --offline -q)
    cp bison_flex/translate/target/release/g2tsg bin/g2tsg
    echo "== installed bin/g2tsg ($(wc -c < bin/g2tsg | tr -d ' ') bytes)"
fi

# Provenance: which commit a binary in bin/ came from.  Written last, because a
# stale copy in bin/ that cannot be dated is how the previous one rotted.
commit=$(git -C "$here" rev-parse --short HEAD 2>/dev/null || echo unknown)
{
    echo "tsg version=0.1.0 commit=$commit built=$(date -u +%Y-%m-%dT%H:%M:%SZ) bytes=$(wc -c < bin/tsg | tr -d ' ')"
    for other in tree-sitter g2tsg; do
        [ -f "bin/$other" ] && echo "$other bytes=$(wc -c < "bin/$other" | tr -d ' ') commit=unknown built=$(date -r "bin/$other" -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown) (rebuild recipe: HANDOFF.md §Toolchain provenance)"
    done
} > bin/VERSIONS

echo
echo "Use it as the generator's front end:"
echo "    export PATH=$here/bin:\$PATH     # then: tree-sitter generate grammar.tsg"
echo "    # or: export TREE_SITTER_TSG=$here/bin/tsg"
