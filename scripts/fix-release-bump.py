#!/usr/bin/env python3
"""Rewrite a release pull request that release-plz under-walked.

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
cause on the topology rather than on the changelog config. Measured again
2026-10-13 against the same release-plz: release pull request #809 proposed
0.13.1 for a main that carried `feat(daemon)!: list every known dog at rest`
(#807), and its changelogs listed none of #807, #803 or #812.

`git log --full-history` prunes neither leg. This reads that range, works out
the bump the whole range implies and the changelog line each commit owes each
package, and writes them onto the release branch: the version
in the workspace manifest, the intra-workspace dependency versions, the
lockfile, each new changelog heading and the entries that were missing under
it. It writes to the release branch and nowhere else, so nothing publishes:
a release still waits for a human to merge the pull request.

Unlike the check this replaces, it is not limited to breaking commits. A
missing `feat` or `fix` line is a wrong changelog as well as a missing bump,
and the same walk finds both.

The bump it works out is release-plz's own rule and not a stricter one. It
raises a bump only when the whole range implies more than the bump release-plz
already took, and it measures "more" the way release-plz does. Measured
2026-10-13 across this repository's own 74 tag transitions: every minor bump
carries a breaking commit, and every range with a `feat` and no break came out
a patch. release-plz bumps a pre-1.0 feature to the patch unless
`features_always_increment_minor` asks otherwise for a package the feature
reached, read per package with the package's own value over the workspace one,
and this repository's `release-plz.toml` does not ask. A guard that read a
feature as a minor would rewrite a correct feature release PR on every push to
main, and a maintainer who merged it would publish a needless semver break.

Usage:

    python3 scripts/fix-release-bump.py \\
        --base-sha <sha> --branch release-plz-2026-10-13T11-39-45Z

Exits 0 when the release branch already accounts for every commit the full
history finds, and after it has rewritten and pushed the branch when it does
not. Exits 1 only when the branch cannot be read or judged, which is the case
a human has to look at.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

# The tag release-plz writes for the `shep` package. Every crate shares one
# version through version_group, so one crate's tags date the whole workspace.
TAG_GLOB = "shep-v[0-9]*"

# type, optional scope, then the bang. The repository gates this shape in
# .github/workflows/commits.yml, so a subject that does not match it is not a
# conventional commit and release-plz drops it for its own reasons. The scope
# allows any character but parentheses, per git-conventional.
CONVENTIONAL = re.compile(
    r"^(?P<type>[a-z]+)(?:\((?P<scope>[^()]*)\))?(?P<breaking>!)?: (?P<description>.*)$"
)

# git-cliff reads a trailing `(#123)` as the commit's pull request number and
# keeps it out of the message, which is why no generated entry in this
# repository carries one even though most subjects on main do.
PR_REFERENCE = re.compile(r"\s*\((#\d+|!\d+)\)\s*$")

# The other marker conventional commits accept. Rarer here, and the reason to
# read bodies at all.
BREAKING_FOOTER = re.compile(r"^BREAKING[ -]CHANGE:", re.MULTILINE)

# git-cliff groups a commit that matched no parser under this name. Reachable
# only through `protect_breaking_commits`, since `filter_commits` drops an
# unmatched commit otherwise.
DEFAULT_GROUP = "Other"

# `## [0.13.1] - 2026-10-13`, as release-plz's own template writes it.
SECTION_HEADING = re.compile(r"^## \[(?P<version>[^\]]+)\](?: - (?P<date>.*?))?\s*$")

# The dog index `shep dogs --available` reads. Its top-level `shep` names the
# latest released shep, and a release branch is where that number is earned.
INDEX_PATH = "web/public/dogs.json"

# The identity a rewritten release branch is committed under. The push carries
# the release token, so this only decides what GitHub renders as the author.
COMMIT_AUTHOR = ("github-actions[bot]", "41898282+github-actions[bot]@users.noreply.github.com")


class Repo:
    """git over one repository, with the two reads this script needs."""

    def __init__(self, path: Path) -> None:
        self.path = path

    def git(self, *args: str) -> str:
        """Run git and return its stdout, refusing with the command on screen."""
        return subprocess.run(
            ["git", *args],
            cwd=self.path,
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def show(self, rev: str, path: str) -> str | None:
        """The text of `path` at `rev`, or None when rev has no such path."""
        return self.try_git("show", f"{rev}:{path}")

    def try_git(self, *args: str) -> str | None:
        try:
            return self.git(*args)
        except subprocess.CalledProcessError:
            return None


@dataclass(frozen=True)
class Commit:
    sha: str
    date: str
    subject: str
    body: str

    @property
    def breaking(self) -> bool:
        head = CONVENTIONAL.match(self.subject)
        return bool(head and head.group("breaking")) or bool(BREAKING_FOOTER.search(self.body))

    @property
    def conventional(self) -> bool:
        return CONVENTIONAL.match(self.subject) is not None

    @property
    def commit_type(self) -> str | None:
        head = CONVENTIONAL.match(self.subject)
        return head.group("type") if head else None

    def description(self) -> str:
        """What git-cliff puts in `commit.message`: the text after the colon,
        with the pull request marker it reads as its own field taken out."""
        head = CONVENTIONAL.match(self.subject)
        if head is None:
            return ""
        return PR_REFERENCE.sub("", head.group("description")).strip()


@dataclass(frozen=True)
class Parser:
    message: re.Pattern[str]
    group: str | None
    skip: bool


@dataclass(frozen=True)
class ChangelogRules:
    """The half of release-plz-changelog.toml this script has to agree with."""

    parsers: tuple[Parser, ...]
    filter_unconventional: bool
    filter_commits: bool
    protect_breaking: bool
    header: str


@dataclass(frozen=True)
class Package:
    name: str
    directory: str
    changelog: str


@dataclass(frozen=True)
class Entry:
    group: str
    line: str
    sha: str


def parse_version(raw: str) -> tuple[int, int, int] | None:
    """(major, minor, patch), or None for anything carrying a pre-release."""
    if "-" in raw or "+" in raw:
        return None
    parts = raw.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        return None
    major, minor, patch = (int(p) for p in parts)
    return major, minor, patch


def show_version(version: tuple[int, int, int]) -> str:
    return ".".join(str(part) for part in version)


BUMP_LEVELS = {"patch": 0, "minor": 1, "major": 2}


def bump_taken(base: tuple[int, int, int], target: tuple[int, int, int]) -> str | None:
    """The bump level release-plz already took from the last tag to the release head.

    None when it moved no number at all.
    """
    if target[0] != base[0]:
        return "major"
    if target[1] != base[1]:
        return "minor"
    if target[2] != base[2]:
        return "patch"
    return None


def apply_bump(base: tuple[int, int, int], level: str) -> tuple[int, int, int]:
    """The version a bump level produces from `base`.

    Which level a range implies is `required_bump`'s call: pre-1.0 it sends a
    break to `minor`, so a zero major never leaves zero here.
    """
    major, minor, patch = base
    if level == "major":
        return (major + 1, 0, 0)
    if level == "minor":
        return (major, minor + 1, 0)
    return (major, minor, patch + 1)


def feature_flags(repo: Repo, packages: list[Package]) -> dict[str, bool]:
    """Whether a feature moves the minor, worked out per release-enabled package.

    release-plz reads `features_always_increment_minor` under `[workspace]` and
    under each `[[package]]`, and a package's own value overrides the workspace
    one. A package named without the key, or not named at all, takes the
    workspace value, and release-plz's default for both is false. This
    repository sets it nowhere, so every package lands on false and a pre-1.0
    feature is a patch.
    """
    try:
        config = tomllib.loads((repo.path / "release-plz.toml").read_text())
    except (OSError, tomllib.TOMLDecodeError):
        return dict.fromkeys((package.name for package in packages), False)
    workspace = config.get("workspace", {})
    default = (
        workspace.get("features_always_increment_minor") is True
        if isinstance(workspace, dict)
        else False
    )
    named: dict[str, bool] = {}
    for entry in config.get("package", []):
        if (
            isinstance(entry, dict)
            and isinstance(entry.get("name"), str)
            and "features_always_increment_minor" in entry
        ):
            named[entry["name"]] = entry["features_always_increment_minor"] is True
    return {package.name: named.get(package.name, default) for package in packages}


def features_move_the_minor(
    packages: list[Package], flags: dict[str, bool], feat_packages: set[str]
) -> bool:
    """Whether a feature reached a package that asks a feature to move the minor.

    One shared version takes the largest bump any package inside it asks for, so
    a package that overrides a workspace `true` back to false holds the release
    at a patch only while every feature in the range stayed off it.
    """
    return any(
        flags.get(package.name, False)
        for package in packages
        if package.name in feat_packages
    )


def required_bump(
    base: tuple[int, int, int], commits: list[Commit], features_move_minor: bool
) -> str | None:
    """The bump level the whole range implies, measured as release-plz measures it.

    A break moves the minor pre-1.0 and the major from 1.0. A `feat` moves the
    minor only where release-plz would: from 1.0, or pre-1.0 with
    `features_always_increment_minor` true for a package the feature reached.
    Everything else is a patch, which is what a pre-1.0 feature is here. None
    when no commit left in the range implies a bump at all.
    """
    if not commits:
        return None
    major, _minor, _patch = base
    if any(commit.breaking for commit in commits):
        return "major" if major else "minor"
    if (
        major >= 1 or features_move_minor
    ) and any(commit.commit_type == "feat" for commit in commits):
        return "minor"
    return "patch"


def load_changelog_rules(repo: Repo) -> ChangelogRules:
    """Read release-plz-changelog.toml rather than restating it.

    A copy of the parsers rots the first time one is added, and the guard would
    then write a changelog release-plz disagrees with.
    """
    config = tomllib.loads((repo.path / "release-plz-changelog.toml").read_text())
    git_config = config.get("git", {})
    parsers: list[Parser] = []
    for entry in git_config.get("commit_parsers", []):
        if not isinstance(entry, dict) or "message" not in entry:
            # A parser keyed on a body, a label or an author is left out rather
            # than guessed at. Nothing in this repository's file is one.
            continue
        try:
            pattern = re.compile(entry["message"])
        except re.error as problem:
            raise ValueError(
                f"release-plz-changelog.toml carries a parser this script cannot "
                f"read: {entry['message']!r} ({problem})"
            ) from problem
        parsers.append(Parser(pattern, entry.get("group"), entry.get("skip") is True))
    return ChangelogRules(
        parsers=tuple(parsers),
        filter_unconventional=git_config.get("filter_unconventional") is True,
        filter_commits=git_config.get("filter_commits") is True,
        protect_breaking=git_config.get("protect_breaking_commits") is True,
        header=config.get("changelog", {}).get("header", ""),
    )


def entry_for(commit: Commit, rules: ChangelogRules) -> Entry | None:
    """The changelog line this commit owes a package, or None for no line.

    First matching parser wins, exactly as git-cliff reads `commit_parsers`.
    """
    if not commit.conventional and rules.filter_unconventional:
        return None

    group: str | None = None
    skipped = False
    matched = False
    for parser in rules.parsers:
        if parser.message.match(commit.subject):
            matched = True
            group = parser.group
            skipped = parser.skip
            break

    if not matched and rules.filter_commits:
        return None
    if skipped and not (commit.breaking and rules.protect_breaking):
        return None

    group = group or DEFAULT_GROUP
    description = commit.description()
    if not description:
        return None
    line = "- " + description[:1].upper() + description[1:]
    if commit.breaking:
        line += " **(BREAKING)**"
    return Entry(group=group, line=line, sha=commit.sha)


def release_packages(repo: Repo) -> list[Package]:
    """Every crate release-plz is told to release, with its changelog path.

    Read rather than listed, because a hardcoded name rots the first time a
    crate joins or leaves the release flow and nothing would say so. Only
    crates/ is scanned: shep-examples sits in examples/ and is already outside
    the tree the commit filter looks at.
    """
    config = tomllib.loads((repo.path / "release-plz.toml").read_text())
    held = {
        entry["name"] for entry in config.get("package", []) if entry.get("release") is False
    }
    packages: list[Package] = []
    for manifest in sorted((repo.path / "crates").glob("*/Cargo.toml")):
        name = tomllib.loads(manifest.read_text())["package"]["name"]
        if name in held:
            continue
        directory = f"crates/{manifest.parent.name}"
        packages.append(
            Package(name=name, directory=directory, changelog=f"{directory}/CHANGELOG.md")
        )
    return packages


def workspace_member_names(repo: Repo) -> set[str]:
    """Package names cargo lists as workspace members, release flow or not.

    The lockfile carries every one of them, and release-plz bumps every one of
    them, so the lockfile rewrite has to know about the placeholder crate and
    the examples too.
    """
    root = tomllib.loads((repo.path / "Cargo.toml").read_text())
    names: set[str] = set()
    for member in root.get("workspace", {}).get("members", []):
        manifest = repo.path / member / "Cargo.toml"
        if manifest.is_file():
            names.add(tomllib.loads(manifest.read_text())["package"]["name"])
    return names


def unreleased_crate_dirs(repo: Repo) -> list[str]:
    """Directories under crates/ that release-plz is told not to release."""
    config = tomllib.loads((repo.path / "release-plz.toml").read_text())
    held = {
        entry["name"] for entry in config.get("package", []) if entry.get("release") is False
    }
    dirs = []
    for manifest in sorted((repo.path / "crates").glob("*/Cargo.toml")):
        name = tomllib.loads(manifest.read_text())["package"]["name"]
        if name in held:
            dirs.append(f"crates/{manifest.parent.name}")
    return dirs


def collect_commits(repo: Repo, since: str, until: str) -> list[Commit]:
    """Every commit in the range, both diamond legs included.

    Oldest first, which is the order git-cliff writes a section in.
    """
    paths = ["crates/"] + [f":(exclude){d}" for d in unreleased_crate_dirs(repo)]
    raw = repo.git(
        "log",
        "--full-history",
        "--reverse",
        "--date=short",
        "--format=%H%x1f%ad%x1f%s%x1f%b%x1e",
        f"{since}..{until}",
        "--",
        *paths,
    )
    commits: list[Commit] = []
    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        sha, date, subject, body = record.split("\x1f", 3)
        commits.append(Commit(sha=sha, date=date, subject=subject, body=body))
    return commits


def touched_paths(repo: Repo, sha: str) -> list[str]:
    """Files the commit brought into the tree it was merged into.

    Diffed against the first parent rather than read from the commit itself, so
    a merge commit counts for what it carried in. This repository merges some
    pull requests as merge commits whose subject comes from the pull request
    title, and `git show` reports no paths for those at all.
    """
    raw = repo.try_git("diff", "--name-only", f"{sha}^1", sha)
    if raw is None:
        return []
    return [line for line in raw.splitlines() if line]


def split_sections(text: str) -> tuple[list[str], list[tuple[str, list[str]]]]:
    """(lines before the first `## ` heading, [(heading line, body lines)])."""
    lines = text.splitlines(keepends=True)
    head: list[str] = []
    index = 0
    while index < len(lines) and not lines[index].startswith("## "):
        head.append(lines[index])
        index += 1
    sections: list[tuple[str, list[str]]] = []
    while index < len(lines):
        heading = lines[index]
        index += 1
        body: list[str] = []
        while index < len(lines) and not lines[index].startswith("## "):
            body.append(lines[index])
            index += 1
        sections.append((heading, body))
    return head, sections


def section_version(heading: str) -> tuple[int, int, int] | None:
    match = SECTION_HEADING.match(heading)
    return parse_version(match.group("version")) if match else None


def group_blocks(body: list[str]) -> list[tuple[str, int, int]]:
    """(group name, first line, one past last line) for each `### ` block in a section."""
    blocks: list[tuple[str, int, int]] = []
    for index, line in enumerate(body):
        if line.startswith("### "):
            blocks.append((line[4:].strip(), index, len(body)))
    for position in range(len(blocks) - 1):
        name, start, _ = blocks[position]
        blocks[position] = (name, start, blocks[position + 1][1])
    return blocks


# What a section release-plz's own template writes: blank lines, `### ` group
# headings and `- ` entries under a heading, and nothing else.
TEMPLATE_LINE = re.compile(r"^\s*$|^### |^- ")


def template_shaped(body: list[str]) -> bool:
    """Whether a section body is only what the template writes.

    An entry with no `### ` heading above it is not that shape. Parsing such a
    body finds no groups, and writing the section back from those groups drops
    the entry, so a body like that is only ever inserted into.
    """
    under_a_group = False
    for line in body:
        if line.startswith("### "):
            under_a_group = True
        elif line.startswith("- "):
            if not under_a_group:
                return False
        elif not TEMPLATE_LINE.match(line):
            return False
    return True


def parse_groups(body: list[str]) -> list[tuple[str, list[str]]]:
    """(group name, its entry lines) for a section body, in the order written."""
    groups: list[tuple[str, list[str]]] = []
    current: tuple[str, list[str]] | None = None
    for line in body:
        if line.startswith("### "):
            current = (line[4:].strip(), [])
            groups.append(current)
        elif line.startswith("- ") and current is not None:
            current[1].append(line)
    return groups


def render_body(groups: list[tuple[str, list[str]]]) -> list[str]:
    """Write a section body back the way the template writes it: a blank line,
    each group with a blank line under its heading, and a blank line over the
    heading of the next version."""
    out: list[str] = ["\n"]
    for name, lines in groups:
        out += [f"### {name}\n", "\n", *lines, "\n"]
    out.append("\n")
    return out


def group_missing(missing: list[Entry]) -> dict[str, list[str]]:
    by_group: dict[str, list[str]] = {}
    for entry in missing:
        by_group.setdefault(entry.group, []).append(f"{entry.line}\n")
    return by_group


def insert_into_body(body: list[str], group: str, lines: list[str]) -> None:
    """Put entries under a group that already has them, in place.

    Used for a body that is not the shape the template writes, where rewriting
    the whole section would flatten something a person wrote by hand.
    """
    blocks = group_blocks(body)
    block = next((b for b in blocks if b[0] == group), None)
    if block is None:
        earlier = [b for b in blocks if b[0] < group]
        block = earlier[-1] if earlier else None
        lines = [f"### {group}\n", "\n", *lines]
    if block is None:
        start, end = 0, len(body)
    else:
        start, end = block[1], block[2]
    insert_at = start
    for index in range(start, end):
        if body[index].strip():
            insert_at = index + 1
    body[insert_at:insert_at] = lines


def complete_section(body: list[str], missing: list[Entry]) -> None:
    """Write the missing entries into a section body, in place.

    A body in the shape the template writes is parsed and written back, which
    puts a new group where its name sorts among the groups already there. Any
    other body is only inserted into, never rewritten.
    """
    by_group = group_missing(missing)
    if not template_shaped(body):
        for group in sorted(by_group):
            insert_into_body(body, group, by_group[group])
        return

    groups = parse_groups(body)
    for name, lines in groups:
        lines.extend(by_group.pop(name, []))
    for group in sorted(by_group):
        position = next(
            (index for index, (name, _lines) in enumerate(groups) if name > group),
            len(groups),
        )
        groups.insert(position, (group, by_group[group]))
    body[:] = render_body(groups)


def existing_entry_lines(lines: list[str]) -> set[str]:
    return {line.strip() for line in lines if line.startswith("- ")}


def missing_entries(entries: list[Entry], present: set[str]) -> list[Entry]:
    """The entries this release still owes, in the order they were committed.

    Two filters, both needed. A line already under this release's own heading is
    written, and adding it twice would be the guard duplicating release-plz.
    And a merge commit plus the branch commits it merged share a subject, so the
    same line arrives twice through `--full-history` and is written once.

    Nothing under an older heading is filtered: a change that was reverted and
    landed again generates the same line as the one that was taken out, and it
    is a real change in this release. Only this release's own section decides
    what is already there.
    """
    missing: list[Entry] = []
    seen: set[str] = set()
    for entry in entries:
        key = entry.line.strip()
        if key in present or key in seen:
            continue
        seen.add(key)
        missing.append(entry)
    return missing


def rewrite_changelog(
    text: str,
    entries: list[Entry],
    new_version: str,
    base_version: tuple[int, int, int],
    date: str,
) -> tuple[str, list[Entry], bool]:
    """(the changelog with every entry accounted for, what was added, whether the
    heading moved).

    The section release-plz wrote for this release is kept as it wrote it and
    completed under it, rather than regenerated, so a line release-plz phrased
    from a commit this script also sees stays exactly as it is.
    """
    head, sections = split_sections(text)
    target = None
    for position, (heading, _body) in enumerate(sections):
        version = section_version(heading)
        if version is not None and version > base_version:
            target = position
            break

    if target is None:
        missing = missing_entries(entries, set())
        if not missing:
            return text, [], False
        body: list[str] = ["\n"]
        complete_section(body, missing)
        # Below a hand-kept `## [Unreleased]` block and above the last release,
        # which is where release-plz puts its own section.
        position = next(
            (index for index, (heading, _body) in enumerate(sections) if section_version(heading) is not None),
            len(sections),
        )
        sections.insert(position, (f"## [{new_version}] - {date}\n", body))
        return render_sections(head, sections), missing, False

    heading, body = sections[target]
    missing = missing_entries(entries, existing_entry_lines(body))
    if missing:
        complete_section(body, missing)
    match = SECTION_HEADING.match(heading)
    renamed = match is not None and match.group("version") != new_version
    if renamed:
        # The bump is the whole workspace's, so every package's new heading
        # carries it, including one whose section release-plz left empty.
        heading = f"## [{new_version}] - {match.group('date') or date}\n"
    if not missing and not renamed:
        return text, [], False
    sections[target] = (heading, body)
    return render_sections(head, sections), missing, renamed


def render_sections(head: list[str], sections: list[tuple[str, list[str]]]) -> str:
    return "".join(head) + "".join(heading + "".join(body) for heading, body in sections)


def version_literal(old: str) -> re.Pattern[str]:
    """`version = "<old>"`, tolerant of the spacing a hand-edited manifest has."""
    return re.compile(r'version\s*=\s*"' + re.escape(old) + r'"')


def rewrite_manifest(text: str, old: str, new: str, member_names: set[str]) -> str:
    """Bump `[workspace.package]` and the intra-workspace dependency literals.

    Both places move together: cargo strips a `path` dependency at publish time
    and substitutes the literal beside it, so a version in one and not the
    other publishes a crate that asks for a workspace member that does not
    exist. Line-based, so every comment in the manifest survives.
    """
    literal = version_literal(old)
    moved = f'version = "{new}"'
    section = ""
    out: list[str] = []
    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            # Whitespace inside the brackets is legal TOML, and tomllib reads
            # `[ workspace.package ]` as the same table this script is asked to
            # move, so it has to read it the same way.
            section = stripped[1:-1].strip()
        if section == "workspace.package" and stripped.startswith("version"):
            line = literal.sub(moved, line, count=1)
        elif section == "workspace.dependencies":
            key = stripped.split("=", 1)[0].strip()
            if key in member_names:
                line = literal.sub(moved, line, count=1)
        out.append(line)
    return "".join(out)


def rewrite_lock(text: str, old: str, new: str, member_names: set[str]) -> str:
    """Bump every workspace member's entry in Cargo.lock.

    Only members: a third-party package that happens to sit at the old version
    is not this release's to move.
    """
    literal = version_literal(old)
    moved = f'version = "{new}"'
    name = ""
    out: list[str] = []
    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if stripped == "[[package]]":
            name = ""
        elif stripped.startswith('name = "'):
            name = stripped[len('name = "') : -1]
        elif name in member_names and literal.match(stripped):
            line = literal.sub(moved, line, count=1)
        out.append(line)
    return "".join(out)


def manifest_version_text(text: str) -> str | None:
    """The version a manifest's `[workspace.package]` names, as TOML reads it."""
    try:
        value = tomllib.loads(text)["workspace"]["package"]["version"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError):
        return None
    return value if isinstance(value, str) else None


def lock_versions(text: str, member_names: set[str]) -> dict[str, str]:
    """The version each workspace member carries in a lockfile."""
    found: dict[str, str] = {}
    name = ""
    for line in text.splitlines():
        stripped = line.strip()
        if stripped == "[[package]]":
            name = ""
        elif stripped.startswith('name = "'):
            name = stripped[len('name = "') : -1]
        elif name in member_names and stripped.startswith('version = "'):
            found[name] = stripped[len('version = "') : -1]
    return found


def dependency_versions(text: str) -> dict[str, str]:
    """The version each intra-workspace dependency literal names, as TOML reads it.

    `cargo publish` drops a `path` and keeps the literal beside it, so a member
    dependency left at the old version publishes a crate that asks for a workspace
    member that release does not contain.
    """
    try:
        dependencies = tomllib.loads(text)["workspace"].get("dependencies", {})
    except (KeyError, TypeError, tomllib.TOMLDecodeError):
        return {}
    found: dict[str, str] = {}
    for key, value in dependencies.items():
        if isinstance(value, dict) and isinstance(value.get("version"), str):
            found[key] = value["version"]
    return found


def manifest_version(repo: Repo, rev: str) -> tuple[int, int, int] | None:
    """`[workspace.package] version` at a revision."""
    raw = repo.show(rev, "Cargo.toml")
    if raw is None:
        return None
    return parse_version(manifest_version_text(raw) or "")


def rewrite_index(text: str, version: str) -> str | None:
    """The dog index naming `version` as its latest shep, or None when `text`
    is not a JSON object.

    An index that already names it comes back untouched, however it is laid
    out. Otherwise `shep` is set right after `version` and the file is dumped
    the way `scripts/dog-index-update.py` dumps it, so the only line that moves
    is the one this writes.
    """
    try:
        document = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(document, dict):
        return None
    if document.get("shep") == version:
        return text
    written: dict[str, object] = {}
    placed = False
    for key, value in document.items():
        if key == "shep":
            continue
        written[key] = value
        if key == "version":
            written["shep"] = version
            placed = True
    if not placed:
        written = {"shep": version, **written}
    return json.dumps(written, indent=2, ensure_ascii=False) + "\n"


def newest_date(commits: list[Commit], fallback: str) -> str:
    dates = [commit.date for commit in commits if commit.date]
    return max(dates) if dates else fallback


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent))
    parser.add_argument("--base-sha", required=True, help="main's tip the release PR is based on")
    parser.add_argument("--branch", help="the release branch to rewrite and push")
    parser.add_argument("--remote", default="origin")
    parser.add_argument(
        "--head-sha",
        help="the release head expected; the fetched branch tip wins if they differ",
    )
    parser.add_argument(
        "--since",
        help="the release tag to count from; the newest one reachable from --base-sha by default",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what the branch is missing and change nothing",
    )
    parser.add_argument("--output", help="write key=value results here, for $GITHUB_OUTPUT")
    args = parser.parse_args(argv)

    repo = Repo(Path(args.repo))

    def refuse(message: str) -> int:
        print(message, file=sys.stderr)
        return 1

    # A shallow clone answers `git log` without error and simply stops at the
    # graft, so a truncated range is indistinguishable from a complete one.
    # `release-plz-pr.yml` fetches at depth 0, and on this history every depth
    # shallow enough to cut the range also leaves `git describe` with no tag to
    # find, which refuses below. Neither of those is a property of the script,
    # so it checks rather than relying on them: a merge's second parent is
    # exactly what a graft drops, and the commits this exists to catch live
    # there.
    if repo.git("rev-parse", "--is-shallow-repository").strip() == "true":
        return refuse(
            "This is a shallow clone, where git log stops at the graft and says\n"
            "nothing, so a commit below it would read as absent. Refusing rather\n"
            "than rewriting a branch from a range that may be cut. Fetch at depth 0."
        )

    try:
        rules = load_changelog_rules(repo)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as problem:
        return refuse(f"Cannot read release-plz-changelog.toml: {problem}")

    base_version = manifest_version(repo, args.base_sha)
    if base_version is None:
        return refuse(
            f"No [workspace.package] version is readable at {args.base_sha}, so the\n"
            "bump this branch needs cannot be worked out. Refusing rather than guessing."
        )

    head_sha = args.head_sha
    if args.branch and not args.dry_run:
        repo.git("fetch", args.remote, f"+refs/heads/{args.branch}")
        fetched = repo.git("rev-parse", "FETCH_HEAD").strip()
        if head_sha and fetched != head_sha:
            print(
                f"The release branch moved: expected {head_sha}, fetched {fetched}. "
                "Rewriting what was fetched."
            )
        head_sha = fetched
    if not head_sha:
        return refuse("No release head to read: pass --branch or --head-sha.")

    head_version = manifest_version(repo, head_sha)
    if head_version is None:
        return refuse(
            f"No [workspace.package] version is readable at {head_sha}, so this\n"
            "cannot tell a real bump from a branch that never got one."
        )

    since = args.since
    if since is None:
        try:
            since = repo.git(
                "describe", "--tags", "--abbrev=0", f"--match={TAG_GLOB}", args.base_sha
            ).strip()
        except subprocess.CalledProcessError:
            return refuse(
                f"No {TAG_GLOB} tag is reachable from {args.base_sha}, so the range of\n"
                "commits since the last release cannot be built. Refusing rather than\n"
                "guessing: a shallow checkout does this, and so does a missing tag fetch."
            )

    commits = collect_commits(repo, since, args.base_sha)
    packages = release_packages(repo)

    # The changelog line each commit owes, worked out once, and the files each
    # one brought into the tree it landed on. The path lookup is a git call per
    # commit, so it runs on the commits that passed the changelog filters
    # rather than on the whole range.
    entries = {commit.sha: entry_for(commit, rules) for commit in commits}
    surviving = [commit for commit in commits if entries[commit.sha] is not None]
    paths = {commit.sha: touched_paths(repo, commit.sha) for commit in surviving}

    def touches(commit: Commit, directory: str) -> bool:
        prefix = f"{directory}/"
        return any(path.startswith(prefix) for path in paths[commit.sha])

    # A commit that reached no release-enabled crate implies no bump either:
    # a docs page and a web page are not somebody's release.
    releasable = [
        commit
        for commit in surviving
        if any(touches(commit, package.directory) for package in packages)
    ]

    want = required_bump(
        base_version,
        releasable,
        features_move_the_minor(
            packages,
            feature_flags(repo, packages),
            {
                package.name
                for package in packages
                if any(
                    commit.commit_type == "feat"
                    for commit in releasable
                    if touches(commit, package.directory)
                )
            },
        ),
    )
    taken = bump_taken(base_version, head_version)
    # Only ever a raise: the bump release-plz picked from the history it could
    # see stands unless the leg it never crossed asks for a larger one.
    raise_to = (
        apply_bump(base_version, want)
        if want is not None and (taken is None or BUMP_LEVELS[want] > BUMP_LEVELS[taken])
        else None
    )
    new_version = max(head_version, raise_to) if raise_to else head_version
    new_version_text = show_version(new_version)
    date = newest_date(releasable, "1970-01-01")

    def attributed(package: Package) -> list[Entry]:
        return [
            entries[commit.sha]
            for commit in surviving
            if entries[commit.sha] is not None and touches(commit, package.directory)
        ]

    plan: list[tuple[Package, str, list[Entry], bool]] = []
    for package in packages:
        text = repo.show(head_sha, package.changelog)
        if text is None:
            text = rules.header
        rewritten, added, renamed = rewrite_changelog(
            text, attributed(package), new_version_text, base_version, date
        )
        if rewritten != text:
            plan.append((package, rewritten, added, renamed))

    members = workspace_member_names(repo)
    manifest_text = repo.show(head_sha, "Cargo.toml") or ""
    lock_text = repo.show(head_sha, "Cargo.lock") or ""
    manifest_new = rewrite_manifest(manifest_text, show_version(head_version), new_version_text, members)
    lock_new = rewrite_lock(lock_text, show_version(head_version), new_version_text, members)

    if new_version != head_version:
        # Report only a version the branch actually carries. A manifest that
        # names its version in a shape the line rewriter cannot move would
        # otherwise leave the pull request titled one version and built at
        # another, which is the mistake this guard exists to catch.
        carried = manifest_version_text(manifest_new)
        if carried != new_version_text:
            return refuse(
                f"Cargo.toml at {head_sha} names its version in a shape this script\n"
                f"cannot move: it reads {manifest_version_text(manifest_text) or 'nothing'}\n"
                f"and the rewrite left {carried or 'nothing'}, not {new_version_text}.\n"
                f"Refusing rather than pushing a branch titled {new_version_text} and\n"
                f"built at {show_version(head_version)}."
            )
        stale = sorted(
            name
            for name, version in lock_versions(lock_new, members).items()
            if version == show_version(head_version)
        )
        if stale:
            return refuse(
                f"Cargo.lock at {head_sha} still carries {show_version(head_version)} for\n"
                f"{', '.join(stale)}, so the bump is incomplete. Refusing rather than\n"
                "pushing a branch whose lockfile disagrees with its manifests."
            )
        pinned = sorted(
            key
            for key, version in dependency_versions(manifest_new).items()
            if key in members and version == show_version(head_version)
        )
        if pinned:
            return refuse(
                f"Cargo.toml at {head_sha} still asks for {show_version(head_version)} of\n"
                f"{', '.join(pinned)} in [workspace.dependencies], written in a shape this\n"
                f"script cannot move. Refusing rather than publishing a crate that wants\n"
                f"a workspace member {new_version_text} does not contain."
            )

    index_text = repo.show(head_sha, INDEX_PATH)
    index_new = rewrite_index(index_text, new_version_text) if index_text is not None else None
    if index_new is None:
        return refuse(
            f"{INDEX_PATH} at {head_sha} is missing or is not a JSON object, so the\n"
            f"latest shep it names cannot be set to {new_version_text}. Refusing rather\n"
            "than pushing a release the dog index does not name."
        )

    changed = (
        new_version != head_version
        or bool(plan)
        or manifest_new != manifest_text
        or lock_new != lock_text
        or index_new != index_text
    )

    print(f"Range {since}..{args.base_sha}: {len(commits)} commit(s), both legs.")
    proposed = show_version(head_version)
    if new_version == head_version:
        print(f"Release branch proposes {proposed}; the range asks for no larger bump.")
    else:
        print(f"Release branch proposes {proposed}; {new_version_text} carries the range.")
    for package, _text, added, renamed in plan:
        moved = f", heading {proposed} -> {new_version_text}" if renamed else ""
        print(f"{package.changelog}: +{len(added)} entr{'y' if len(added) == 1 else 'ies'}{moved}")
        for entry in added:
            print(f"  + [{entry.group}] {entry.line}")
    if index_new != index_text:
        print(f"{INDEX_PATH}: shep -> {new_version_text}")
    if not changed:
        print("The release branch already accounts for every commit in the range.")

    results = {
        "changed": "true" if changed else "false",
        "old_version": show_version(head_version),
        "new_version": new_version_text,
        "release_head": head_sha,
    }

    if args.dry_run or not changed:
        write_results(args.output, results)
        return 0

    if not args.branch:
        return refuse(
            "The branch needs rewriting and --branch was not given, so nothing was written."
        )

    commit_text = (
        f"chore(release): v{new_version_text} with the commits release-plz did not walk\n\n"
        f"release-plz walks each package's history one commit at a time and asks the\n"
        f"commit it is standing on for its own next ancestor, so on a diamond it goes\n"
        f"down one leg and never crosses to the other. {len(commits)} commit(s) sit in\n"
        f"{since}..{args.base_sha} under git log --full-history. The version here is\n"
        f"the bump that whole range implies, and the changelog lines added here are the\n"
        f"ones that walk left out. {INDEX_PATH} names that version as the latest shep.\n"
    )

    with tempfile.TemporaryDirectory() as worktree:
        repo.git("worktree", "add", "--detach", worktree, head_sha)
        try:
            if manifest_new != manifest_text:
                (Path(worktree) / "Cargo.toml").write_text(manifest_new)
            if lock_new != lock_text:
                (Path(worktree) / "Cargo.lock").write_text(lock_new)
            if index_new != index_text:
                (Path(worktree) / INDEX_PATH).write_text(index_new)
            for package, text, _added, _renamed in plan:
                (Path(worktree) / package.changelog).write_text(text)
            repo.git("-C", worktree, "add", "-A")
            if not repo.git("-C", worktree, "status", "--porcelain").strip():
                print("Nothing to write: the branch already matches.")
                write_results(args.output, results)
                return 0
            repo.git(
                "-C",
                worktree,
                "-c",
                f"user.name={COMMIT_AUTHOR[0]}",
                "-c",
                f"user.email={COMMIT_AUTHOR[1]}",
                "commit",
                "-m",
                commit_text,
            )
            new_head = repo.git("-C", worktree, "rev-parse", "HEAD").strip()
        finally:
            repo.git("worktree", "remove", "--force", worktree)
            repo.git("worktree", "prune")

    repo.git("push", args.remote, f"{new_head}:refs/heads/{args.branch}")
    print(f"Pushed {new_head} to {args.branch}.")
    results["release_head"] = new_head
    write_results(args.output, results)
    return 0


def write_results(path: str | None, results: dict[str, str]) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        for key, value in results.items():
            handle.write(f"{key}={value}\n")


if __name__ == "__main__":
    sys.exit(main())
