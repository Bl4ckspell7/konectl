#!/usr/bin/env bash
# Release version helper for konectl.
#
#   bump.sh compute <auto|patch|minor|major>
#       Print the next version (no leading "v"). Read-only.
#
#   bump.sh apply <version>
#       Set the version of all workspace crates (Cargo.toml, Cargo.lock),
#       (re)generate CHANGELOG.md and write this release's notes to
#       $RELEASE_NOTES (default /tmp/release-notes.md). Mutates the tree.
#
# "auto" derives the bump from Conventional Commits via git-cliff. The very
# first release (no tags yet) uses the current Cargo.toml version as-is.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

CARGO_TOML="Cargo.toml"
CARGO_LOCK="Cargo.lock"
RELEASE_NOTES="${RELEASE_NOTES:-/tmp/release-notes.md}"
# Workspace packages; all share the version from [workspace.package].
CRATES=("konectl" "konectl-cli" "konectl-gui")

die() {
    echo "error: $*" >&2
    exit 1
}

# Current version: `version = "x"` inside the [workspace.package] table.
current_version() {
    # sed quits after the first match, so no `| head` (which can make the
    # writer fail with SIGPIPE under pipefail).
    sed -nE '/^\[workspace\.package\]/,/^\[/{s/^version = "([^"]+)".*/\1/p;T;q}' "$CARGO_TOML"
}

# Highest existing release tag, without the leading "v".
latest_tag_version() {
    local tags
    tags="$(git tag --list 'v[0-9]*' --sort=-v:refname)"
    tags="${tags%%$'\n'*}"
    echo "${tags#v}"
}

# bump_semver <x.y.z> <major|minor|patch>
bump_semver() {
    local IFS=. ma mi pa
    read -r ma mi pa <<<"$1"
    case "$2" in
        major) echo "$((ma + 1)).0.0" ;;
        minor) echo "${ma}.$((mi + 1)).0" ;;
        patch) echo "${ma}.${mi}.$((pa + 1))" ;;
        *) die "unknown bump level: $2" ;;
    esac
}

compute() {
    local level="${1:-auto}" base bumped next
    # First release: no tags yet -> keep the current Cargo.toml version.
    if [ -z "$(git tag --list 'v[0-9]*')" ]; then
        current_version
        return
    fi
    base="$(latest_tag_version)"
    [ -n "$base" ] || base="$(current_version)"
    if [ "$(git rev-list --count "v${base}..HEAD")" -eq 0 ]; then
        die "no commits since v${base}, nothing to release"
    fi
    case "$level" in
        auto)
            # git-cliff prints the current version when no commit calls for a
            # bump (e.g. only chores); that becomes a patch release.
            bumped="$(git cliff --bumped-version)" || die "git cliff --bumped-version failed"
            next="${bumped#v}"
            if [ "$next" = "$base" ]; then
                next="$(bump_semver "$base" patch)"
            fi
            echo "$next"
            ;;
        *)
            bump_semver "$base" "$level"
            ;;
    esac
}

apply() {
    local v="${1:?version required}" crate
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version: $v"

    # Cargo.toml: the shared package version, and the version the CLI requires
    # of the library (crates.io resolves it instead of the path).
    sed -i -E '/^\[workspace\.package\]/,/^\[/{s/^version = "[^"]+"/version = "'"$v"'"/}' "$CARGO_TOML"
    sed -i -E 's/^(konectl = \{ path = "crates\/konectl", version = )"[^"]+"/\1"'"$v"'"/' "$CARGO_TOML"

    # Cargo.lock: the workspace packages (the version is the line after the name).
    for crate in "${CRATES[@]}"; do
        sed -i '/^name = "'"$crate"'"$/{n;s/^version = "[^"]*"/version = "'"$v"'"/}' "$CARGO_LOCK"
    done

    check_applied "$v"

    # Changelog + this release's notes (Conventional Commits via git-cliff).
    # Each release ends with a blank line; the command substitutions drop the
    # trailing blank lines so both files end with a single newline.
    local changelog notes
    changelog="$(git cliff --tag "v$v")"
    notes="$(git cliff --tag "v$v" --unreleased --strip all)"
    printf '%s\n' "$changelog" >CHANGELOG.md
    printf '%s\n' "$notes" >"$RELEASE_NOTES"
}

# Fails if one of the edits in apply() did not match.
check_applied() {
    local v="$1" crate
    [ "$(current_version)" = "$v" ] || die "$CARGO_TOML: [workspace.package] version not updated"
    grep -q '^konectl = { path = "crates/konectl", version = "'"$v"'" }' "$CARGO_TOML" ||
        die "$CARGO_TOML: konectl dependency version not updated"
    for crate in "${CRATES[@]}"; do
        [[ "$(grep -A1 -x 'name = "'"$crate"'"' "$CARGO_LOCK")" == *$'\n'"version = \"$v\"" ]] ||
            die "$CARGO_LOCK: $crate version not updated"
    done
}

case "${1:-}" in
    compute) compute "${2:-auto}" ;;
    apply) apply "${2:-}" ;;
    *)
        echo "usage: $0 {compute <auto|patch|minor|major>|apply <version>}" >&2
        exit 1
        ;;
esac
