#!/bin/sh
# Push this tree-sitter fork (with tmp/tsg inside it) to mingodad/tree-sitter.
#
#   ./push-fork.sh                 # push branch wip as 'tsg'
#   ./push-fork.sh my-branch       # push wip as another branch name
#   ./push-fork.sh --unshallow tsg # fetch the full history first (see below)
#
# The clone this lives in is SHALLOW (`git rev-parse --is-shallow-repository`),
# so a push may be refused by GitHub with "shallow update not allowed".  If that
# happens, rerun with --unshallow: it fetches the missing history into .git
# (expect a few hundred MB on a ramdisk that has ~700 MB free, and do not let
# that happen on a full disk) and then pushes a normal branch.
#
# Authentication is not scripted, because it must not be: the token or key has to
# come from you.  Two ways that keep credentials inside this directory:
#
#   1. project-local SSH key (recommended):
#        ssh-keygen -t ed25519 -f .ssh/id_ed25519 -N ''      # creates .ssh/ here
#        # add .ssh/id_ed25519.pub at https://github.com/settings/keys
#        export GIT_SSH_COMMAND="ssh -i $PWD/.ssh/id_ed25519 -o IdentitiesOnly=yes"
#        git remote set-url mine git@github.com:mingodad/tree-sitter.git
#
#   2. a personal access token, asked for once and stored in the login keychain:
#        printf 'protocol=https\nhost=github.com\nusername=mingodad\n' \
#          | git credential-osxkeychain store
#        # then git prompts for the password (paste the token) once
#
# Usage with either: ./push-fork.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
root=$(cd "$here/../.." && pwd -P)
cd "$root"

unshallow=0
if [ "${1:-}" = "--unshallow" ]; then
    unshallow=1
    shift
fi
branch=${1:-tsg}

if ! git remote | grep -qx mine; then
    git remote add mine https://github.com/mingodad/tree-sitter.git
    echo "== added remote 'mine' -> https://github.com/mingodad/tree-sitter.git"
fi

if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
    if [ "$unshallow" = 1 ]; then
        echo "== fetching the history this clone is missing (this is the slow part)"
        git fetch --unshallow origin
    else
        echo "== note: shallow clone; if the push is refused with"
        echo "   'shallow update not allowed', rerun as: $0 --unshallow $branch"
    fi
fi

echo "== pushing wip -> mine/$branch ($(git rev-list --count wip) commits, $(git ls-files | wc -l | tr -d ' ') files)"
git push -u mine "wip:$branch"
echo "== https://github.com/mingodad/tree-sitter/tree/$branch"
