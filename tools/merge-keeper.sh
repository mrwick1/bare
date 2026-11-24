#!/usr/bin/env bash
# Run from a bare checkout. --dry-run tries the merges and reports conflicts.
set -uo pipefail

case "${1-}:$#" in
    :0) dry_run=false ;;
    --dry-run:1) dry_run=true ;;
    *) echo 'Usage: tools/merge-keeper.sh [--dry-run]' >&2; exit 2 ;;
esac

repo=mrwick1/bare
prelude=crates/vex/src/prelude_facts.rs
baseline=tools/fixtures/js/test262-js2-baseline.txt
debt=docs/progress/cost-debt.md
fields=number,autoMergeRequest,mergeStateStatus,headRefOid,headRefName,isCrossRepository,baseRefName
eligible='select(.autoMergeRequest != null and .mergeStateStatus == "DIRTY" and .baseRefName == "main")'
scratch_root=${TMPDIR:-/tmp}
if [[ -d /data/bare-tmp/r21 && -w /data/bare-tmp/r21 ]]; then
    scratch_root=/data/bare-tmp/r21
fi

cd "$(git rev-parse --show-toplevel)" || exit 1
for url in "$(git remote get-url origin)" "$(git remote get-url --push origin)"; do
    case "$url" in
        "https://github.com/$repo"| "git@github.com:$repo"| "ssh://git@github.com/$repo") ;;
        "https://github.com/$repo.git"| "git@github.com:$repo.git"| "ssh://git@github.com/$repo.git") ;;
        *) printf 'Unexpected origin URL for %s\n' "$repo" >&2; exit 1 ;;
    esac
done

if ! "$dry_run"; then
    gh auth switch --user mrwick1 || exit 1
fi
prs=$(gh pr list --repo "$repo" --state open --base main --limit 1000 --json "$fields" \
    --jq ".[] | $eligible | [.number,.headRefOid,.headRefName,.isCrossRepository] | @tsv") || exit 1
if [[ -z "$prs" ]]; then
    echo 'No dirty PRs with auto-merge enabled.'
    exit 0
fi
git fetch origin '+refs/heads/main:refs/remotes/origin/main' || exit 1
main=$(git rev-parse origin/main) || exit 1

repair() (
    number=$1 head=$2 branch=$3
    ref=refs/heads/$branch
    [[ "$branch" != main ]] && git check-ref-format "$ref" || return 1
    tracking=refs/remotes/origin/$branch
    git fetch origin "+$ref:$tracking" || return 1
    if [[ "$(git rev-parse "$tracking")" != "$head" ]]; then
        echo "PR #$number: head changed. Run again to use the new head." >&2
        return 1
    fi

    scratch=$(mktemp -d "$scratch_root/merge-keeper-$number.XXXXXXXX") || return 1
    # Use the caller's checkout to remove the temporary worktree.
    root=$PWD
    trap 'result=$?
          if [[ -e "$scratch/worktree/.git" ]]; then
              git -C "$root" worktree remove --force "$scratch/worktree" || exit 1
          fi
          rm -rf -- "$scratch" || exit 1
          exit "$result"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    git worktree add --detach "$scratch/worktree" "$head" || return 1
    cd "$scratch/worktree" || return 1
    export CARGO_TARGET_DIR="$scratch/target" CARGO_BUILD_JOBS=4

    merge_status=0
    git -c rerere.enabled=false merge --no-edit --no-commit --no-ff "$main" || merge_status=$?
    if ! git rev-parse -q --verify MERGE_HEAD >/dev/null; then
        [[ $merge_status == 0 ]] || return 1
        echo "PR #$number: main is already in the head."
        return 0
    fi
    conflicts=$(git diff --name-only --diff-filter=U) || return 1
    while IFS= read -r path; do
        case "$path" in
            ''|"$prelude"|"$baseline"|"$debt") ;;
            *)
                printf 'PR #%s: manual merge required. Conflicts:\n%s\n' "$number" "$conflicts" >&2
                return 1
                ;;
        esac
    done <<< "$conflicts"

    while IFS= read -r path; do
        case "$path" in
            "$prelude"|"$baseline")
                if git cat-file -e "$main:$path"; then
                    git restore --source="$main" --staged --worktree -- "$path" || return 1
                else
                    git rm -f -- "$path" || return 1
                fi
                ;;
            "$debt")
                git show ":2:$path" > "$scratch/head" &&
                    git show ":3:$path" > "$scratch/main" || return 1
                git show ":1:$path" > "$scratch/base" 2>/dev/null || : > "$scratch/base"
                git merge-file --union -p "$scratch/head" "$scratch/base" "$scratch/main" > "$path" &&
                    git add -- "$path" || return 1
                ;;
        esac
    done <<< "$conflicts"

    if "$dry_run"; then
        printf 'PR #%s: would validate and push a merge to %s. Resolved files:\n%s\n' \
            "$number" "$branch" "${conflicts:-none}"
        return 0
    fi
    # New builds generate this file in OUT_DIR. Older builds still track it.
    if git ls-files --error-unmatch -- "$prelude" >/dev/null 2>&1; then
        BLESS_PRELUDE_FACTS=1 cargo test --locked -p bare-vex --lib prelude_facts &&
            git add -- "$prelude" || return 1
    fi
    cargo check --locked -p bare-vex -p bare --tests || return 1
    git commit --no-edit || return 1
    current=$(gh pr view "$number" --repo "$repo" --json "$fields,state" \
        --jq "$eligible | select(.state == \"OPEN\" and .isCrossRepository == false) | .headRefOid") || return 1
    if [[ "$current" != "$head" ]]; then
        echo "PR #$number: eligibility or head changed. Run again to check the new state." >&2
        return 1
    fi
    gh auth switch --user mrwick1 || return 1
    git merge-base --is-ancestor "$head" HEAD &&
        git push --force-with-lease="$ref:$head" origin "HEAD:$ref" || return 1
    printf 'PR #%s: pushed %s.\n' "$number" "$(git rev-parse HEAD)"
)

status=0
while IFS=$'\t' read -r number head branch fork; do
    if [[ "$fork" == true ]]; then
        echo "PR #$number: skipped fork branch $branch."
    elif ! repair "$number" "$head" "$branch"; then
        echo "PR #$number: left for manual attention." >&2
        status=1
    fi
done <<< "$prs"
exit "$status"
