#!/usr/bin/env python3

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


class ReleaseError(Exception):
    pass


def usage(program_name):
    print(
        f"""Usage: {program_name} NEW_VERSION

Update Cargo.toml and Cargo.lock, commit the version changes, and create
a Git tag. NEW_VERSION must be greater than the current version and
must not include the leading "v".

Two-component versions are accepted and normalized:

  0.2 -> 0.2.0

Example:
  {program_name} 7.7.0
"""
    )


def fail(message):
    raise ReleaseError(message)


def run(command, capture_output=False, check=True):
    result = subprocess.run(
        command,
        text=True,
        stdout=subprocess.PIPE if capture_output else None,
        check=False,
    )

    if check and result.returncode != 0:
        fail(
            "command failed with exit status "
            f"{result.returncode}: {' '.join(command)}"
        )

    if capture_output:
       # Preserve leading spaces because Git porcelain status uses them
       # as meaningful index/worktree status columns.
       return result.stdout.rstrip("\r\n")

    return ""


def git(arguments, capture_output=False, check=True):
    return run(
        ["git", *arguments],
        capture_output=capture_output,
        check=check,
    )


def is_ascii_decimal(component):
    return (
        len(component) > 0
        and all("0" <= character <= "9" for character in component)
    )


def parse_numeric_version(supplied, accept_two_components):
    components = supplied.split(".")

    if len(components) == 2 and accept_two_components:
        components.append("0")

    if len(components) != 3:
        fail(
            "version must have the form MAJOR.MINOR or "
            f"MAJOR.MINOR.PATCH; got {supplied!r}"
        )

    if not all(is_ascii_decimal(component) for component in components):
        fail(
            "every version component must contain only ASCII decimal "
            f"digits; got {supplied!r}"
        )

    numeric = tuple(int(component, 10) for component in components)

    return numeric


def format_numeric_version(version):
    major, minor, patch = version
    return f"{major}.{minor}.{patch}"


def normalize_requested_version(supplied):
    if supplied.startswith("v"):
        fail(
            "NEW_VERSION must not include the leading 'v'; "
            f"got {supplied!r}"
        )

    if "+" in supplied:
        fail(
            "NEW_VERSION must not include build metadata; "
            "the script adds it automatically"
        )

    if "-" in supplied:
        fail("prerelease versions are not supported by this script")

    return parse_numeric_version(
        supplied,
        accept_two_components=True,
    )


def parse_release_tag(tag):
    if not tag.startswith("v"):
        return None

    version_text = tag[1:]

    if "+" in version_text:
        version_text = version_text.split("+", 1)[0]

    if "-" in version_text:
        return None

    try:
        return parse_numeric_version(
            version_text,
            accept_two_components=True,
        )
    except ReleaseError:
        return None


def find_current_release_tag():
    tags_text = git(
        ["tag", "--merged", "HEAD", "--list"],
        capture_output=True,
    )

    candidates = []

    for tag in tags_text.splitlines():
        tag = tag.strip()

        if not tag:
            continue

        version = parse_release_tag(tag)

        if version is not None:
            candidates.append((version, tag))

    if not candidates:
        fail(
            "no reachable release tag beginning with 'v' was found"
        )

    version, tag = max(
        candidates,
        key=lambda candidate: (
            candidate[0],
            candidate[1],
        ),
    )

    return tag, version


def assert_proposed_version_is_unused(proposed_version):
    tags_text = git(
        ["tag", "--list"],
        capture_output=True,
    )

    for tag in tags_text.splitlines():
        tag_version = parse_release_tag(tag.strip())

        if tag_version == proposed_version:
            fail(
                "a tag already exists for version "
                f"{format_numeric_version(proposed_version)}: {tag}"
            )


def assert_git_repository(repo_root):
    actual_root_text = git(
        ["rev-parse", "--show-toplevel"],
        capture_output=True,
    )

    actual_root = Path(actual_root_text).resolve()

    if actual_root != repo_root:
        fail(
            f"expected repository root {repo_root}, "
            f"but Git reported {actual_root}"
        )


def assert_on_branch():
    branch = git(
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
        capture_output=True,
        check=False,
    )

    if not branch:
        fail("HEAD must be attached to a branch")


def git_status():
    return git(
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
        capture_output=True,
    )


def assert_clean_working_tree(context):
    status = git_status()

    if status:
        fail(
            f"the working tree must be clean {context}.\n"
            "Git status:\n"
            f"{status}"
        )


def split_line_ending(line):
    if line.endswith("\r\n"):
        return line[:-2], "\r\n"

    if line.endswith("\n"):
        return line[:-1], "\n"

    return line, ""


def set_manifest_version(manifest_path, new_version):
    with manifest_path.open(
        "r",
        encoding="utf-8",
        newline="",
    ) as source:
        lines = source.readlines()

    in_package_section = False
    package_section_count = 0
    version_assignment_count = 0
    updated_lines = []

    for line in lines:
        body, line_ending = split_line_ending(line)

        structural_text = body

        if "#" in structural_text:
            structural_text = structural_text.split("#", 1)[0]

        structural_text = structural_text.strip()

        if (
            structural_text.startswith("[")
            and structural_text.endswith("]")
        ):
            if structural_text == "[package]":
                package_section_count += 1
                in_package_section = True
            else:
                in_package_section = False

            updated_lines.append(line)
            continue

        if not in_package_section:
            updated_lines.append(line)
            continue

        if "=" not in structural_text:
            updated_lines.append(line)
            continue

        key, value = structural_text.split("=", 1)

        if key.strip() != "version":
            updated_lines.append(line)
            continue

        old_value = value.strip()

        if (
            len(old_value) < 2
            or not old_value.startswith('"')
            or not old_value.endswith('"')
        ):
            fail(
                "the [package] version in Cargo.toml must be "
                "a quoted string"
            )

        indentation_length = len(body) - len(body.lstrip())
        indentation = body[:indentation_length]

        comment = ""

        if "#" in body:
            comment_text = body.split("#", 1)[1]
            comment = "  #" + comment_text

        updated_lines.append(
            f'{indentation}version = "{new_version}"'
            f"{comment}{line_ending}"
        )

        version_assignment_count += 1

    if package_section_count != 1:
        fail(
            "Cargo.toml must contain exactly one [package] section; "
            f"found {package_section_count}"
        )

    if version_assignment_count != 1:
        fail(
            "the Cargo.toml [package] section must contain exactly "
            "one version assignment; "
            f"found {version_assignment_count}"
        )

    original_mode = manifest_path.stat().st_mode

    temporary_name = None

    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="",
            dir=manifest_path.parent,
            prefix=manifest_path.name + ".",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_name = temporary.name
            temporary.writelines(updated_lines)
            temporary.flush()
            os.fsync(temporary.fileno())

        os.chmod(temporary_name, original_mode)
        os.replace(temporary_name, manifest_path)
        temporary_name = None
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def refresh_lock_file_and_verify(
    manifest_path,
    expected_version,
):
    metadata_text = run(
        [
            "cargo",
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ],
        capture_output=True,
    )

    try:
        metadata = json.loads(metadata_text)
    except json.JSONDecodeError as error:
        fail(f"Cargo returned invalid metadata JSON: {error}")

    expected_manifest = manifest_path.resolve()
    matching_packages = []

    for package in metadata["packages"]:
        package_manifest = Path(
            package["manifest_path"]
        ).resolve()

        if package_manifest == expected_manifest:
            matching_packages.append(package)

    if len(matching_packages) != 1:
        fail(
            "Cargo metadata must contain exactly one package for "
            f"{expected_manifest}; found {len(matching_packages)}"
        )

    actual_version = matching_packages[0]["version"]

    if actual_version != expected_version:
        fail(
            f"expected Cargo to report version {expected_version!r}, "
            f"but it reported {actual_version!r}"
        )

    lock_path = manifest_path.parent / "Cargo.lock"

    if not lock_path.is_file():
        fail("Cargo.lock must exist after refreshing Cargo metadata")


def assert_only_release_files_changed():
    status = git_status()

    if not status:
        fail(
            "updating the package version did not change "
            "Cargo.toml or Cargo.lock"
        )

    allowed_paths = {
        "Cargo.toml",
        "Cargo.lock",
    }

    unexpected_lines = []

    for line in status.splitlines():
        if len(line) < 4:
            unexpected_lines.append(line)
            continue

        path_text = line[3:]

        if " -> " in path_text:
            unexpected_lines.append(line)
            continue

        if path_text not in allowed_paths:
            unexpected_lines.append(line)

    if unexpected_lines:
        fail(
            "version generation changed files other than "
            "Cargo.toml and Cargo.lock:\n"
            + "\n".join(unexpected_lines)
        )


def commit_release_files(message):
    assert_only_release_files_changed()

    git(["add", "Cargo.toml", "Cargo.lock"])

    staged_diff_result = subprocess.run(
        [
            "git",
            "diff",
            "--cached",
            "--quiet",
            "--exit-code",
        ],
        check=False,
    )

    if staged_diff_result.returncode == 0:
        fail("there are no staged release changes to commit")

    if staged_diff_result.returncode != 1:
        fail(
            "git diff --cached failed with exit status "
            f"{staged_diff_result.returncode}"
        )

    git(["commit", "-m", message])

    assert_clean_working_tree("after the release commit")


def main():
    program_name = Path(sys.argv[0]).name

    if len(sys.argv) == 2 and sys.argv[1] == "--help":
        usage(program_name)
        return 0

    if len(sys.argv) != 2 or not sys.argv[1]:
        print(
            "Error: exactly one NEW_VERSION argument is required.",
            file=sys.stderr,
        )
        print(file=sys.stderr)
        usage(program_name)
        return 2

    script_path = Path(__file__).resolve()
    repo_root = script_path.parent.parent
    manifest_path = repo_root / "Cargo.toml"

    os.chdir(repo_root)

    assert_git_repository(repo_root)
    assert_on_branch()

    if not manifest_path.is_file():
        fail(f"manifest does not exist: {manifest_path}")

    if not (repo_root / "Cargo.lock").is_file():
        fail("Cargo.lock must exist before generating a release")

    proposed_version = normalize_requested_version(
        sys.argv[1]
    )

    current_tag, current_version = (
        find_current_release_tag()
    )

    if proposed_version <= current_version:
        fail(
            f"version {format_numeric_version(proposed_version)!r} "
            "is not greater than "
            f"{format_numeric_version(current_version)!r} "
            f"from tag {current_tag!r}"
        )

    assert_proposed_version_is_unused(proposed_version)

    print(
        "Success: "
        f"{format_numeric_version(proposed_version)} "
        "is greater than "
        f"{format_numeric_version(current_version)}"
    )

    
    # The release process must start from a clean tree. Check this before making any changes or commits.
    assert_clean_working_tree(
        "before generating a version"
    )

    base_version = format_numeric_version(
        proposed_version
    )

    set_manifest_version(
        manifest_path,
        base_version,
    )

    refresh_lock_file_and_verify(
        manifest_path,
        base_version,
    )

    commit_release_files(
        "Update Cargo.toml and Cargo.lock "
        f"for version {base_version}"
    )

    version_commit = git(
        ["rev-parse", "HEAD"],
        capture_output=True,
    )

    full_version = (
        f"{base_version}+{version_commit}"
    )

    set_manifest_version(
        manifest_path,
        full_version,
    )

    refresh_lock_file_and_verify(
        manifest_path,
        full_version,
    )

    commit_release_files(
        "Update Cargo.toml and Cargo.lock "
        f"for version {full_version}"
    )

    tag = f"v{full_version}"

    existing_tags = {
        line.strip()
        for line in git(
            ["tag", "--list"],
            capture_output=True,
        ).splitlines()
        if line.strip()
    }

    if tag in existing_tags:
        fail(f"Git tag already exists: {tag}")

    git(["tag", tag])

    final_commit = git(
        ["rev-parse", "HEAD"],
        capture_output=True,
    )

    tagged_commit = git(
        ["rev-list", "-n", "1", tag],
        capture_output=True,
    )

    if tagged_commit != final_commit:
        fail(
            f"tag {tag} does not point to the final release commit"
        )

    assert_clean_working_tree(
        "after generating the release"
    )

    print()
    print("Created version:")
    print(f"  package version: {full_version}")
    print(f"  Git tag:         {tag}")
    print(f"  Git commit:      {final_commit}")

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ReleaseError as error:
        print(f"Error: {error}", file=sys.stderr)
        print("exiting", file=sys.stderr)
        raise SystemExit(1)
