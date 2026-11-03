#!/usr/bin/env bash
# Decide the next release from Conventional Commits since the last v* tag,
# and write its release notes.
#
#   scripts/release-version.sh [notes-file]
#
# Prints `version=X.Y.Z` (empty when nothing warrants a release) and
# `release=true|false`, in the format GitHub Actions reads from
# $GITHUB_OUTPUT.
#
# Bumps: a breaking change (`type!:` or a BREAKING CHANGE footer) is major,
# `feat` is minor, `fix`/`perf` is patch; before 1.0 a breaking change bumps
# the minor version instead. Other types (docs, chore, ci, refactor, test,
# build) don't release on their own. The first release uses the version in
# Cargo.toml, and a version bumped by hand in Cargo.toml always wins.
set -euo pipefail

notes_file=${1:-}
# tr: Cargo.toml has CRLF line endings when checked out on Windows.
base=$(tr -d '\r' <Cargo.toml | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n1)
last=$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)

if [ -z "$last" ]; then
    range=HEAD
    version=$base
else
    range="$last..HEAD"
    subjects=$(git log --format=%s "$range")
    bodies=$(git log --format=%b "$range")
    IFS=. read -r major minor patch <<<"${last#v}"

    if grep -qE '^[a-z]+(\([^)]*\))?!:' <<<"$subjects" || grep -q '^BREAKING CHANGE' <<<"$bodies"; then
        bump=major
    elif grep -qE '^feat(\([^)]*\))?:' <<<"$subjects"; then
        bump=minor
    elif grep -qE '^(fix|perf)(\([^)]*\))?:' <<<"$subjects"; then
        bump=patch
    else
        bump=none
    fi
    if [ "$bump" = major ] && [ "$major" = 0 ]; then
        bump=minor
    fi

    case $bump in
        major) version="$((major + 1)).0.0" ;;
        minor) version="$major.$((minor + 1)).0" ;;
        patch) version="$major.$minor.$((patch + 1))" ;;
        none) version="" ;;
    esac

    # A manual bump in Cargo.toml beyond the last release wins.
    if [ "$(printf '%s\n%s\n' "${last#v}" "$base" | sort -V | tail -n1)" != "${last#v}" ]; then
        if [ -z "$version" ] || [ "$(printf '%s\n%s\n' "$version" "$base" | sort -V | tail -n1)" = "$base" ]; then
            version=$base
        fi
    fi
fi

if [ -n "$version" ] && git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
    echo "tag v$version already exists" >&2
    exit 1
fi

if [ -n "$notes_file" ] && [ -n "$version" ]; then
    {
        section() {
            # $1 = title, $2 = subject regex
            local lines
            lines=$(git log --format=%s "$range" | grep -E "$2" | sed -E 's/^[a-z]+(\(([^)]*)\))?!?: /- **\2** /; s/\*\*\*\* //' || true)
            if [ -n "$lines" ]; then
                printf '### %s\n\n%s\n\n' "$1" "$lines"
            fi
        }
        section "Features" '^feat(\([^)]*\))?!?:'
        section "Fixes" '^(fix|perf)(\([^)]*\))?!?:'
        section "Other changes" '^(refactor|docs|build|ci|test|chore|style)(\([^)]*\))?!?:'
    } >"$notes_file"
fi

echo "version=$version"
if [ -n "$version" ]; then echo "release=true"; else echo "release=false"; fi
