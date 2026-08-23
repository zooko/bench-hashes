#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<EOF
Usage: ${0##*/} NEW_VERSION

Update Cargo.toml and Cargo.lock, commit the version changes, and create
a Git tag. NEW_VERSION must be greater than the current version and
must not include the leading "v".

Two-component versions are accepted and normalized:

  0.2 -> 0.2.0

Example:
  ${0##*/} 7.7.0
EOF
}

die() {
    echo "Error: $*" >&2
    echo "exiting" >&2
    exit 1
}

if [[ $# == 1 && "$1" == "--help" ]]; then
    usage
    exit 0
fi

if [[ $# != 1 || -z "$1" ]]; then
    echo "Error: exactly one NEW_VERSION argument is required." >&2
    echo >&2
    usage >&2
    exit 2
fi

SCRIPT_DIR=$(
    cd "$(dirname "${BASH_SOURCE[0]}")"
    pwd
)

REPO_ROOT=$(
    cd "$SCRIPT_DIR/.."
    pwd
)

cd "$REPO_ROOT"

git rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
    die "this command must run inside a Git working tree"

normalize_version() {
    local supplied=$1
    local major
    local minor
    local patch

    if [[ "$supplied" =~ ^([0-9]+)\.([0-9]+)$ ]]; then
        major=${BASH_REMATCH[1]}
        minor=${BASH_REMATCH[2]}
        patch=0
    elif [[ "$supplied" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
        major=${BASH_REMATCH[1]}
        minor=${BASH_REMATCH[2]}
        patch=${BASH_REMATCH[3]}
    else
        die \
            "version must have the form MAJOR.MINOR or MAJOR.MINOR.PATCH; got '$supplied'"
    fi

    # Convert to decimal integers, removing any leading zeroes.
    major=$((10#$major))
    minor=$((10#$minor))
    patch=$((10#$patch))

    printf '%d.%d.%d\n' "$major" "$minor" "$patch"
}

assert_version_greater() {
    local current
    local proposed

    current=$(normalize_version "$1")
    proposed=$(normalize_version "$2")

    local current_major
    local current_minor
    local current_patch
    local proposed_major
    local proposed_minor
    local proposed_patch

    IFS=. read -r \
        current_major \
        current_minor \
        current_patch \
        <<< "$current"

    IFS=. read -r \
        proposed_major \
        proposed_minor \
        proposed_patch \
        <<< "$proposed"

    if ((proposed_major > current_major)); then
        :
    elif ((proposed_major < current_major)); then
        die "version '$proposed' is not greater than '$current'"
    elif ((proposed_minor > current_minor)); then
        :
    elif ((proposed_minor < current_minor)); then
        die "version '$proposed' is not greater than '$current'"
    elif ((proposed_patch > current_patch)); then
        :
    else
        die "version '$proposed' is not greater than '$current'"
    fi

    echo "Success: $proposed is greater than $current"
}

set_manifest_version() {
    local new_version=$1
    local temporary
    local line
    local in_package=0
    local package_sections=0
    local changed=0

    temporary=$(
        mktemp "${TMPDIR:-/tmp}/bench-hashes-Cargo.toml.XXXXXX"
    )

    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" =~ ^[[:space:]]*$$package$$[[:space:]]*$ ]]; then
            in_package=1
            package_sections=$((package_sections + 1))
            printf '%s\n' "$line" >> "$temporary"
            continue
        fi

        if [[ "$line" =~ ^[[:space:]]*$$.*$$[[:space:]]*$ ]]; then
            in_package=0
        fi

        if ((in_package)) &&
            [[ "$line" =~ ^[[:space:]]*version[[:space:]]*= ]]; then
            printf 'version = "%s"\n' "$new_version" >> "$temporary"
            changed=$((changed + 1))
        else
            printf '%s\n' "$line" >> "$temporary"
        fi
    done < Cargo.toml

    if ((package_sections != 1)); then
        rm -f "$temporary"
        die \
            "Cargo.toml must contain exactly one [package] section; found $package_sections"
    fi

    if ((changed != 1)); then
        rm -f "$temporary"
        die \
            "Cargo.toml [package] section must contain exactly one version assignment; found $changed"
    fi

    # Writing through the original path preserves Cargo.toml's permissions.
    cat "$temporary" > Cargo.toml
    rm -f "$temporary"

    local actual_version
    actual_version=$(
        cargo metadata \
            --no-deps \
            --format-version 1 |
        python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
packages = metadata["packages"]

if len(packages) != 1:
    raise SystemExit(
        f"expected one workspace package, found {len(packages)}"
    )

print(packages[0]["version"])
'
    )

    [[ "$actual_version" == "$new_version" ]] ||
        die \
            "expected Cargo package version '$new_version', but Cargo read '$actual_version'"
}

update_lock_file() {
    cargo update --workspace --offline
}

NEW_VERSION=$(normalize_version "$1")

# Use the most recent reachable version tag. It does not need to point
# directly at HEAD, because ordinary development commits normally occur
# between releases.
CURRENT_TAG=$(
    git describe \
        --tags \
        --abbrev=0 \
        --match 'v[0-9]*' 2>/dev/null
) || die "no reachable version tag beginning with 'v' was found"

[[ "$CURRENT_TAG" == v* ]] ||
    die "current tag must start with 'v'; got '$CURRENT_TAG'"

CURRENT_VERSION=${CURRENT_TAG#v}

# Tags generated by this script have the form:
#
#   v0.2.0+COMMIT
#
# Compare only the numeric version.
CURRENT_VERSION=${CURRENT_VERSION%%+*}

assert_version_greater "$CURRENT_VERSION" "$NEW_VERSION"

# Generating a release from a dirty tree is a contract violation. Unlike the
# old script, this checks before changing Cargo.toml.
GIT_STATUS=$(
    git status \
        --porcelain=v1 \
        --untracked-files=all
)

[[ -z "$GIT_STATUS" ]] ||
    die "the working tree must be clean before generating a version"

set_manifest_version "$NEW_VERSION"
update_lock_file

git add Cargo.toml Cargo.lock
git commit -m \
    "Update Cargo.toml and Cargo.lock for version $NEW_VERSION"

VERSION_COMMIT=$(git rev-parse HEAD)
FULL_VERSION="${NEW_VERSION}+${VERSION_COMMIT}"

set_manifest_version "$FULL_VERSION"
update_lock_file

git add Cargo.toml Cargo.lock
git commit -m \
    "Update Cargo.toml and Cargo.lock for version $FULL_VERSION"

git tag "v${FULL_VERSION}"

FINAL_COMMIT=$(git rev-parse HEAD)

echo
echo "Created version:"
echo "  package version: $FULL_VERSION"
echo "  Git tag:         v$FULL_VERSION"
echo "  Git commit:      $FINAL_COMMIT"
