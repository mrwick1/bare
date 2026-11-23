#!/usr/bin/env bash
# Ship the branch of this worktree in one command. There is no CI: this script is the merge gate.
#
#   tools/ship.sh             # merge origin/main, check, push, open the PR if there is none, merge it
#   tools/ship.sh --release   # the same, then tag the next version (tools/release.sh)
#
# Checks: `t` (the tests of the changed files), `cargo check --all-targets` of the changed crates, and the source scan of
# tools/portable_check.sh. The release workflow builds Linux and Windows again before it publishes.
set -euo pipefail

release=false
case "${1-}:$#" in
    :0) ;;
    --release:1) release=true ;;
    *) echo 'Usage: tools/ship.sh [--release]' >&2; exit 2 ;;
esac

cd "$(git rev-parse --show-toplevel)"
branch=$(git branch --show-current)
[[ -n $branch && $branch != main ]] || { echo 'ship: run it on a branch, not on main' >&2; exit 1; }
[[ -z $(git status --porcelain) ]] || { echo 'ship: commit or stash the changes first' >&2; exit 1; }

git fetch -q origin
git merge -q --no-edit origin/main
changed=$(git diff --name-only origin/main...HEAD)
[[ -n $changed ]] || { echo 'ship: no change against origin/main' >&2; exit 1; }

packages=()
for dir in $(grep -o '^crates/[^/]*' <<<"$changed" | sort -u); do
    [[ -f $dir/Cargo.toml ]] && packages+=(-p "$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)")
done
if ((${#packages[@]})); then
    cargo check -q --locked --all-targets "${packages[@]}"
fi
t
tools/portable_check.sh --scan

gh auth switch --user mrwick1 >/dev/null 2>&1 || true
git push -q -u origin "$branch"
gh pr view "$branch" --json number >/dev/null 2>&1 || gh pr create --fill --base main --head "$branch"
gh pr merge "$branch" --merge
echo "ship: $branch merged"

if $release; then
    tools/release.sh
fi
