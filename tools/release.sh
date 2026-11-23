#!/usr/bin/env bash
# Publish a release of origin/main in one command.
#
#   tools/release.sh           # tag the next patch version (v0.1.17 gives v0.1.18)
#   tools/release.sh v0.2.0    # tag this version; a version with `-` is a prerelease
#
# The tag starts .github/workflows/release.yml: it builds Linux AMD64, Linux ARM64 and Windows, runs the offstage gate,
# and publishes. offstage installs the newest release, so this also deploys it to the agents.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
git fetch -q origin main --tags
if (($#)); then
    next=$1
else
    last=$(git tag -l 'v[0-9]*' --sort=-v:refname | grep -v -e - | head -1)
    [[ $last =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || { echo "release: cannot read the last tag '$last'" >&2; exit 1; }
    next="v${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((BASH_REMATCH[3] + 1))"
fi
[[ $next =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "release: bad version '$next'" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/$next" >/dev/null && { echo "release: $next exists" >&2; exit 1; }

gh auth switch --user mrwick1 >/dev/null 2>&1 || true
git tag -a "$next" -m "$next" origin/main
git push -q origin "refs/tags/$next"
echo "release: $next tagged at $(git rev-parse --short origin/main)"
echo "release: https://github.com/mrwick1/bare/actions/workflows/release.yml"
