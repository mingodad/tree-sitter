#!/bin/sh
# Build the patched tree-sitter CLI and install it as bin/tree-sitter.
#
#   ./build-cli.sh                    # release, stripped (8.5 MB binary)
#   ./build-cli.sh --no-default-features   # ~200 MB smaller build, no grammar.js
#
# Every artefact stays inside this directory, and the ~420 MB build tree is
# deleted afterwards, so the peak is ~420 MB of a 1 GiB ramdisk (measured: 834 M
# used at the peak, 27 s).  The CLI belongs to the *enclosing* workspace, so this
# cd's up to it, but CARGO_TARGET_DIR / CARGO_HOME point back inside here:
#
#   .tsbuild/   build tree, removed at the end
#   .cargo/     CARGO_HOME: cargo's 64 KB bookkeeping lands here, and
#               registry is a symlink to ~/.cargo/registry (read-mostly; only a
#               genuinely new crate download writes there, which is ordinary
#               cargo behaviour), so nothing is copied onto the ramdisk
#
# This exists because the previous recipe put the build tree in ~/.ts-build: a
# 408 MB directory on the SSD, outside the project, which is not ours to write.
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/../.." && pwd -P)

if [ ! -f "$root/Cargo.toml" ] || ! grep -qx '\[workspace\]' "$root/Cargo.toml"; then
    echo "build-cli.sh: $root is not the tree-sitter workspace" >&2
    exit 2
fi

mkdir -p "$here/.cargo"
if [ ! -e "$here/.cargo/registry" ]; then
    # The registry is *read* on every resolve and *written* only when a crate is
    # actually fetched, which is the one case where a write into ~/.cargo is the
    # ordinary cargo behaviour every other project on this machine already has.
    # So link it instead of copying 100 MB onto the ramdisk; the bookkeeping
    # files (.global-cache, .package-cache) stay here, because CARGO_HOME is
    # this directory.
    ln -s "$HOME/.cargo/registry" "$here/.cargo/registry"
    echo "== linked .cargo/registry -> ~/.cargo/registry (0 bytes here)"
fi

echo "== building tree-sitter-cli"
cd "$root"
CARGO_HOME="$here/.cargo" CARGO_TARGET_DIR="$here/.tsbuild" \
    CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_STRIP=true \
    cargo build --release --offline -q -p tree-sitter-cli "$@"

mkdir -p "$here/bin"
cp "$here/.tsbuild/release/tree-sitter" "$here/bin/tree-sitter"
commit=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
echo "== installed bin/tree-sitter ($(wc -c < "$here/bin/tree-sitter" | tr -d ' ') bytes, $("$here/bin/tree-sitter" --version))"

rm -rf "$here/.tsbuild"
{
    echo "tsg version=$("$here/bin/tsg" --version 2>/dev/null | awk '{print $2}' || echo '?') commit=$commit built=$(date -u +%Y-%m-%dT%H:%M:%SZ) bytes=$(wc -c < "$here/bin/tsg" 2>/dev/null | tr -d ' ' || echo 0)"
    echo "tree-sitter version=$("$here/bin/tree-sitter" --version | awk '{print $2}') commit=$commit built=$(date -u +%Y-%m-%dT%H:%M:%SZ) bytes=$(wc -c < "$here/bin/tree-sitter" | tr -d ' ')"
    for other in g2tsg; do
        [ -f "$here/bin/$other" ] && echo "$other bytes=$(wc -c < "$here/bin/$other" | tr -d ' ') commit=unknown built=$(date -r "$here/bin/$other" -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown) (rebuild recipe: HANDOFF.md §Toolchain provenance)"
    done
} > "$here/bin/VERSIONS"
echo "== freed the build tree; $(df -k /Volumes/RamDisk | awk 'NR==2 {print int($4/1024)" M free"}')"
