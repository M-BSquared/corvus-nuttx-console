#!/usr/bin/env bash
# Publishes this plugin. GitHub builds it for Linux, macOS and Windows against the newest
# published Corvus GCS, lets that program load it and attaches it to a release of this
# repository (.github/workflows/build.yml). Nothing built on this computer is published.
#
#   ./release.sh                        test, push main, build and publish
#   ./release.sh --corvus v2026.11.93   build for that version of Corvus GCS as well
#   ./release.sh --watch                wait for the build and say how it went
#   ./release.sh --no-verify            skip the local tests
#   ./release.sh --dry-run              say what would happen, change nothing
#
# With nothing new to push it starts the build again, which a new version of Corvus GCS with a
# new plugin interface needs. Commit first. The same file lies in every plugin.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

die() { echo "release.sh: $*" >&2; exit 1; }
usage() { echo "usage: ./release.sh [--corvus TAG] [--watch] [--no-verify] [--dry-run]" >&2; exit 1; }

CORVUS=""
WATCH=0
VERIFY=1
DRY_RUN=0
while (( $# )); do
    case "$1" in
        --help|-h)   sed -n '2,/^$/p' "$(basename "$0")" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --corvus)    [[ $# -ge 2 ]] || usage; CORVUS="$2"; shift ;;
        --watch)     WATCH=1 ;;
        --no-verify) VERIFY=0 ;;
        --dry-run)   DRY_RUN=1 ;;
        *)           usage ;;
    esac
    shift
done

command -v gh >/dev/null || die "the GitHub CLI (gh) is needed"
CRATE="$(sed -n 's/^name = "\(corvus_plugin_[a-z0-9_]*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n "$CRATE" ]] || die "Cargo.toml names no plugin crate"
REPO="$(git remote get-url origin | sed -E 's#^.*github\.com[:/]##; s#\.git$##')"
[[ -z "$(git status --porcelain)" ]] || die "there are uncommitted changes, commit them first"
git fetch --quiet origin main
git merge-base --is-ancestor origin/main HEAD || die "origin/main has commits this checkout lacks, pull first"
AHEAD="$(git rev-list --count origin/main..HEAD)"

echo "=== $CRATE -> $REPO ($AHEAD commit(s) to push) ==="

if (( VERIFY )); then
    if [[ -f ../../src/api/Cargo.toml ]]; then
        ( cd ../.. && cargo clippy --locked -p "$CRATE" --all-targets -- -D warnings && cargo test --locked -p "$CRATE" )
    else
        echo "--- not inside a Corvus GCS checkout: the tests run on GitHub only"
    fi
fi

if (( DRY_RUN )); then
    if (( AHEAD )); then echo "--- dry run: would push main, which builds and publishes"
    else echo "--- dry run: would start the build and publish"; fi
    [[ -z "$CORVUS" ]] || echo "--- dry run: for Corvus GCS $CORVUS"
    exit 0
fi

newest_run() {
    gh run list --repo "$REPO" --workflow build.yml --event "$1" --limit 1 --json databaseId --jq '.[0].databaseId // 0'
}

# A push builds by itself, for the newest Corvus GCS. A build for a named version, and a build
# with nothing new to push, are started by hand.
if (( AHEAD )) && [[ -z "$CORVUS" ]]; then EVENT=push; else EVENT=workflow_dispatch; fi
BEFORE="$(newest_run "$EVENT")"
if (( AHEAD )); then git push origin HEAD:main; fi
if [[ "$EVENT" == workflow_dispatch ]]; then
    if [[ -n "$CORVUS" ]]; then
        gh workflow run build.yml --repo "$REPO" --ref main -f "corvus=$CORVUS"
    else
        gh workflow run build.yml --repo "$REPO" --ref main
    fi
fi

if (( ! WATCH )); then
    echo "=== started: https://github.com/$REPO/actions ==="
    exit 0
fi

for _ in $(seq 1 30); do
    RUN="$(newest_run "$EVENT")"
    if [[ "$RUN" != "$BEFORE" ]]; then
        gh run watch "$RUN" --repo "$REPO" --exit-status --interval 30
        echo "=== done: https://github.com/$REPO/releases ==="
        exit 0
    fi
    sleep 4
done
die "the build did not show up, look at https://github.com/$REPO/actions"
