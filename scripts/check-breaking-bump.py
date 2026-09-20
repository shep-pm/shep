#!/usr/bin/env python3
"""Refuse a release pull request whose bump is too small for a breaking commit.

release-plz does not read one `git log`. It walks each package's history a
commit at a time, checking each one out and asking the commit it is standing
on for its own next ancestor. On a diamond, two branches cut from one base and
merged in turn, that walk goes down whichever leg it entered and never crosses
to the other, so every commit on the far leg is invisible to both the version
bump and the changelog.

Measured 2026-09-20 against release-plz 0.3.160. shep-daemon's
`refactor(daemon)!: carry the real error in BootError::Adopt` was dropped that
way and 0.9.0 came out as 0.8.5. Nothing else objected: cargo-semver-checks
has no lint for a changed variant payload under `#[non_exhaustive]`. The same
commits rebased into a line produced 0.9.0 correctly, which is what pins the
cause on the topology rather than on the changelog config.

`git log --full-history` prunes neither leg, so it sees what the walk missed.
This compares the breaking commits it finds since the last release against the
version the release pull request proposes, and refuses a bump too small to
carry them.

What it does not catch: two breaking commits where release-plz saw one and
missed the other. The version is then right and the changelog is one line
short, which misleads a reader rather than breaking anyone's build.

Usage:

    python3 scripts/check-breaking-bump.py \\
        --base-sha <sha> --base-version 0.8.4 --head-version 0.8.5

Exits 0 when the bump covers every breaking commit since the last release,
1 otherwise.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The tag release-plz writes for the `shep` package. Every crate shares one
# version through version_group, so one crate's tags date the whole workspace.
TAG_GLOB = "shep-v[0-9]*"

# type, optional scope, then the bang. The repository gates this shape in
# .github/workflows/commits.yml, so a subject that does not match it is not a
# conventional commit and release-plz drops it for its own reasons.
BREAKING_SUBJECT = re.compile(r"^[a-z]+(\([a-z0-9_.-]+\))?!: ")

# The other marker conventional commits accept. Rarer here, and the reason to
# read bodies at all.
BREAKING_FOOTER = re.compile(r"^BREAKING[ -]CHANGE:", re.MULTILINE)


def git(*args: str) -> str:
    """Run git in the repository root and return its stdout."""
    return subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def unreleased_crate_dirs() -> list[str]:
    """Directories under crates/ that release-plz is told not to release.

    Read rather than listed, because a hardcoded name rots the first time a
    crate joins or leaves the release flow and nothing would say so. Only
    crates/ is scanned, since that is the tree the commit filter looks at:
    shep-examples sits in examples/ and is already outside it.
    """
    config = tomllib.loads((ROOT / "release-plz.toml").read_text())
    held = {
        entry["name"]
        for entry in config.get("package", [])
        if entry.get("release") is False
    }
    dirs = []
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        name = tomllib.loads(manifest.read_text())["package"]["name"]
        if name in held:
            dirs.append(f"crates/{manifest.parent.name}")
    return dirs


def breaking_commits(since: str, until: str) -> list[tuple[str, str]]:
    """(sha, subject) for every breaking commit touching a released crate."""
    paths = ["crates/"] + [f":(exclude){d}" for d in unreleased_crate_dirs()]
    raw = git(
        "log",
        "--full-history",
        "--format=%H%x1f%s%x1f%b%x1e",
        f"{since}..{until}",
        "--",
        *paths,
    )
    found = []
    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        sha, subject, body = record.split("\x1f", 2)
        if BREAKING_SUBJECT.match(subject) or BREAKING_FOOTER.search(body):
            found.append((sha, subject))
    return found


def parse_version(raw: str) -> tuple[int, int, int] | None:
    """(major, minor, patch), or None for anything carrying a pre-release."""
    if "-" in raw or "+" in raw:
        return None
    parts = raw.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        return None
    major, minor, patch = (int(p) for p in parts)
    return major, minor, patch


def required_version(base: tuple[int, int, int]) -> tuple[int, int, int]:
    """The smallest version a breaking change may be published under.

    Cargo treats a 0.x minor as its compatibility boundary, so pre-1.0 a break
    moves the minor and 1.0 onward it moves the major.
    """
    major, minor, _ = base
    return (0, minor + 1, 0) if major == 0 else (major + 1, 0, 0)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--base-version", required=True)
    parser.add_argument("--head-version", required=True)
    args = parser.parse_args()

    try:
        tag = git(
            "describe", "--tags", "--abbrev=0", f"--match={TAG_GLOB}", args.base_sha
        ).strip()
    except subprocess.CalledProcessError:
        print(
            f"No {TAG_GLOB} tag is reachable from {args.base_sha}, so the range of\n"
            "commits since the last release cannot be built. Refusing rather than\n"
            "guessing: a shallow checkout does this, and so does a missing tag fetch.",
            file=sys.stderr,
        )
        return 1

    commits = breaking_commits(tag, args.base_sha)
    if not commits:
        print(f"No breaking commit since {tag}. Any bump is allowed.")
        return 0

    base = parse_version(args.base_version)
    head = parse_version(args.head_version)
    if base is None or head is None:
        print(
            f"{args.base_version} or {args.head_version} carries a pre-release, which\n"
            "release-plz.toml says is set by hand. Leaving the bump to whoever set it."
        )
        return 0

    want = required_version(base)
    if head >= want:
        shown = ".".join(str(n) for n in head)
        print(
            f"{len(commits)} breaking commit(s) since {tag}, and {shown} covers them."
        )
        return 0

    listing = "\n".join(f"  {sha[:8]}  {subject}" for sha, subject in commits)
    smallest = ".".join(str(n) for n in want)
    print(
        f"{len(commits)} breaking commit(s) landed since {tag}:\n"
        f"{listing}\n"
        "\n"
        f"The release pull request proposes {args.head_version}, which publishes those\n"
        f"as a compatible release. Coming from {args.base_version}, {smallest} is the\n"
        "smallest version that carries a break.\n"
        "\n"
        "release-plz walks each package's history one commit at a time and asks the\n"
        "commit it is standing on for its own next ancestor, so on a diamond it goes\n"
        "down one leg and never sees the other. The commits above are what\n"
        "git log --full-history finds and that walk did not.\n"
        "\n"
        "Set the version by hand on the release branch, or rebase the pull request\n"
        "that carried the break so it sits in one line of history.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
