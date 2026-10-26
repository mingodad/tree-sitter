#!/usr/bin/env bash
# Free the ramdisk before a build, without deleting anything that is not
# regenerable.  Everything this removes is either a check-script scratch
# directory, a build tree, or output that `tsg`/`tree-sitter` writes next to a
# grammar (all of it is in .gitignore).
#
#   ./clean.sh                # scratch + generated parsers + every cargo target/
#                             #   tree (tsg ~50 M, translator ~135 M, and the
#                             #   tree-sitter workspace's target/ ~300 M when this
#                             #   checkout sits inside it); rebuilds are 2-30 s,
#                             #   see --binaries for the recipes
#   ./clean.sh --keep-target  # keep the target/ trees; rebuilding `tsg` costs
#                             #   ~15 s and the gates need target/debug/tsg
#   ./clean.sh --binaries     # also bin/ (patched tree-sitter, tsg, g2tsg)
#   ./clean.sh --all          # target/ trees and bin/
#   ./clean.sh --dry-run ...  # print what would go, remove nothing
#
# Freeing the ramdisk is the point, so removing build trees is the *default*:
# it used to need --target, and the failure mode was a "freed 0 M" summary with
# 350 M of target/ left behind.  Nothing outside this directory is ever
# removed, so a CARGO_TARGET_DIR elsewhere (~/.ts-build in the recipes) is only
# reported, never deleted.
#
# Never touches anything outside this directory.  The two things worth
# protecting by hand are bin/tree-sitter (a release build of this checkout with
# patches 0001-0010) and bin/g2tsg; --binaries prints the recipes to rebuild
# them, see HANDOFF.md §"Toolchain provenance".
set -euo pipefail

# `pwd -P`: this tree is reachable through a symlink (/Users/mingo/tmp ->
# /Volumes/RAMDisk), and the guard in `prune` compares *real* paths.  A logical
# $root made every path look "outside $root", so the script refused everything
# and removed nothing — with the actual reason on stderr, easy to miss.
root=$(cd "$(dirname "$0")" && pwd -P)
dry=0
want_target=1
want_binaries=0
for arg in "$@"; do
    case $arg in
        --dry-run|-n) dry=1 ;;
        --target) want_target=1 ;;
        --no-target|--keep-target) want_target=0 ;;
        --binaries) want_binaries=1 ;;
        --all) want_target=1; want_binaries=1 ;;
        -h|--help) awk 'NR > 1 && /^set -euo/ {exit} NR > 1 {sub(/^# ?/, ""); print}' "$0"; exit 0 ;;
        *) echo "clean.sh: unknown argument '$arg' (try --help)" >&2; exit 2 ;;
    esac
done

before=$(df -k /Volumes/RamDisk 2>/dev/null | awk 'NR==2 {print $4}')
freed=0

# `prune PATH`: remove PATH if it is inside $root.  Prints one line per item,
# with its size, so the log says what the space was.
prune() {
    local path=$1 size
    # Nothing to do is checked *before* the guard: a path whose parent scratch
    # directory was already removed has no directory to resolve, and reporting
    # that as "outside" hid the real refusals among noise.
    [ -e "$path" ] || return 0
    case $(cd "$(dirname "$path")" 2>/dev/null && pwd -P) in
        "$root"|"$root"/*) ;;
        *) echo "clean.sh: refusing '$path' (outside $root)" >&2; return 0 ;;
    esac
    size=$(du -sk "$path" 2>/dev/null | awk '{print $1}')
    size=${size:-0}
    if [ "$dry" = 1 ]; then
        printf '  would remove %6s M  %s\n' "$((size / 1024))" "${path#"$root"/}"
    else
        rm -rf "$path"
        printf '  removed       %6s M  %s\n' "$((size / 1024))" "${path#"$root"/}"
    fi
    freed=$((freed + size))
}

# The tree-sitter checkout that *contains* this one is a cargo workspace, and
# building there without CARGO_TARGET_DIR writes ~475 M into its target/.  That
# path is outside $root, so `prune` refuses it — which is why `--target` could
# report a freed size while the ramdisk stayed full.  This checkout sits two
# levels down (tmp/tsg), so search upward instead of assuming `..`, and only
# remove a target/ whose parent is verifiably the tree-sitter workspace.
workspace_root=
workspace_up=
for up in . .. ../..; do
    cand=$(cd "$root/$up" 2>/dev/null && pwd -P) || continue
    if grep -qx '\[workspace\]' "$cand/Cargo.toml" 2>/dev/null &&
        grep -qx 'name = "tree-sitter-generate"' \
            "$cand/crates/generate/Cargo.toml" 2>/dev/null; then
        workspace_root=$cand
        workspace_up=$up
        break
    fi
done
workspace_target=${workspace_root:+$workspace_root/target}

prune_workspace_target() {
    local size rel
    if [ -z "$workspace_root" ]; then
        # A stray target/ next to us is not ours to guess about.
        echo "clean.sh: note: no tree-sitter workspace above $root; only local target/ trees are removed" >&2
        return 0
    fi
    [ -e "$workspace_target" ] || return 0
    if [ "$workspace_up" = . ]; then rel=target; else rel=$workspace_up/target; fi
    size=$(du -sk "$workspace_target" 2>/dev/null | awk '{print $1}')
    size=${size:-0}
    if [ "$dry" = 1 ]; then
        printf '  would remove %6s M  %s\n' "$((size / 1024))" "$rel"
    else
        rm -rf "$workspace_target"
        printf '  removed       %6s M  %s\n' "$((size / 1024))" "$rel"
    fi
    freed=$((freed + size))
}

# Size in KiB, 0 when the path is missing: `du` writes nothing in that case and
# an empty operand would make the arithmetic expansion an error.
size_of() {
    local k
    [ -e "$1" ] || { echo 0; return; }
    k=$(du -sk "$1" 2>/dev/null | awk 'NR==1 {print $1}')
    echo "${k:-0}"
}

echo "clean.sh: $root"

# 1. Scratch from the check scripts.  A parse log or a copied corpus is worth
#    nothing after the run that wrote it.
for d in .self .roundtrip .diff .e2e .harness .xmode .probe .bc .ref .fmt \
         .g2tsg .tsbuild .tsgrel .scan .scan2 .scan3 .ch bison_flex/.corpus; do
    prune "$root/$d"
done
# Any `.check` (gates build their parsers there) and any leftover `.old*`.
while IFS= read -r d; do prune "$d"; done < <(find "$root" -maxdepth 2 -name '.check*' -o -maxdepth 2 -name '.old*' 2>/dev/null | sort)
# `__pycache__` from the python helpers.
while IFS= read -r d; do prune "$d"; done < <(find "$root" -maxdepth 3 -name __pycache__ 2>/dev/null | sort)

# 2. Parsers and linkable libraries written by `tsg` / `tree-sitter build`.
#    The `.tsg` and `.g` sources stay; these are output (see .gitignore).
while IFS= read -r f; do prune "$f"; done < <(find "$root" \( \
        -name parser.c -o -name grammar.json -o -name node-types.json \
     \) -path '*/src/*' 2>/dev/null | sort)
while IFS= read -r d; do prune "$d"; done < <(find "$root" -path '*/src/tree_sitter' -type d 2>/dev/null | sort)
# `tsg` writes a scanner.c for `%external name = delimited(…)`.  Only the
# generated one goes; a hand-written scanner does not carry the marker.  (It is
# deliberately not in .gitignore, so a stray one shows up in `git status`.)
while IFS= read -r f; do
    if head -1 "$f" 2>/dev/null | grep -q '^// Generated by tsg'; then prune "$f"; fi
done < <(find "$root" -path '*/src/scanner.c' 2>/dev/null | sort)
while IFS= read -r d; do prune "$d"; done < <(find "$root" -maxdepth 3 -name lib -type d 2>/dev/null | sort)
while IFS= read -r f; do prune "$f"; done < <(find "$root" -maxdepth 3 -name '*.dylib' -o -maxdepth 3 -name '*.so' 2>/dev/null | sort)

# 3. Build trees, on request.  `target/` holds the `tsg` binary the gates call,
#    so it is worth keeping between runs — but there is more than one: the
#    translator has its own (`bison_flex/translate/target`, 135 M), and pruning
#    only `$root/target` used to leave that behind while reporting the space it
#    did not free.  `-prune` stops `find` descending into a tree it will delete.
if [ "$want_target" = 1 ]; then
    while IFS= read -r d; do prune "$d"; done < <(find "$root" -maxdepth 6 \
        -name target -type d -prune 2>/dev/null | sort)
    # A redirector target dir (`.tsbuild`, `.xt`) is ours too, when it is inside;
    # when it is elsewhere (~/.ts-build in the recipes) it is only reported.
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        ctd_real=$(cd "$CARGO_TARGET_DIR" 2>/dev/null && pwd -P) || ctd_real=
        case ${ctd_real:-none} in
            "$root"|"$root"/*) prune "$CARGO_TARGET_DIR" ;;
            none) ;;
            *)
                printf '  outside       %6s M  CARGO_TARGET_DIR=%s (not this script'"'"'s to remove)\n' \
                    "$(($(size_of "$CARGO_TARGET_DIR") / 1024))" "$CARGO_TARGET_DIR"
                ;;
        esac
    fi
    prune_workspace_target
fi
if [ "$want_binaries" = 1 ]; then
    prune "$root/bin"
    cat <<'EOF'
  rebuild before the next gate run:
    CARGO_TARGET_DIR=tmp/tsg/.tsbuild CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=true cargo build --release --offline -p tree-sitter-cli
    cp tmp/tsg/.tsbuild/release/tree-sitter tmp/tsg/bin/tree-sitter && rm -rf tmp/tsg/.tsbuild
    (cd tmp/tsg && cargo build && ./bison_flex/build-parser.sh)   # parser.c for build.rs
    (cd tmp/tsg/bison_flex/translate && CARGO_TARGET_DIR=../../.g2tsg \
       CARGO_INCREMENTAL=0 cargo build --release --offline) &&
       cp tmp/tsg/.g2tsg/release/g2tsg tmp/tsg/bin/g2tsg && rm -rf tmp/tsg/.g2tsg
    (cd tmp/tsg && cargo build -p tsg)     # target/debug/tsg
EOF
fi

if [ "$want_target" = 0 ]; then
    kept=0
    for d in "$root/target" "$root"/*/target "$workspace_target"; do
        [ -n "${d:-}" ] || continue
        kept=$((kept + $(size_of "$d")))
    done
    if [ "$kept" -gt 0 ]; then
        printf '  kept          %6s M  target/ trees (--keep-target)\n' "$((kept / 1024))"
    fi
fi

after=$(df -k /Volumes/RamDisk 2>/dev/null | awk 'NR==2 {print $4}')
printf '\n%s %s M, %s M free%s\n' \
    "$([ "$dry" = 1 ] && echo would free || echo freed)" "$((freed / 1024))" \
    "$((after / 1024))" \
    "$([ -n "${before:-}" ] && [ -n "${after:-}" ] && echo " (was $((before / 1024)) M)")"
if [ "$dry" = 0 ] && [ "$want_target" = 1 ]; then
    echo "rebuild first: (cd tmp/tsg && cargo build -p tsg)   # gates need target/debug/tsg"
    echo "gates: ./selfhost.sh ./diff.sh ./e2e.sh ./roundtrip.sh --parsers"
fi
