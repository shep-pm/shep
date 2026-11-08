#!/usr/bin/env python3
"""Write a dog release's version, hashes and permissions into the community dog index.

`.github/workflows/dog-release.yml` runs this after a dog's binaries are on
its release. The change it makes is the one a human reviews before shep will
install that version, so it changes one row's `source.version`,
`source.sha256` and `source.permissions`, and keeps the file byte-for-byte
otherwise.

The hashes file is `sha256sum` output over the release's assets, named
`<package>-<version>-<target>`, plus `.exe` on Windows. The map written is
exactly the targets in that file: a target the new release no longer builds
drops out rather than keeping a stale hash.

The manifest, when one is given, is what the release's Linux binary answered
`--shep-manifest` with. Only its `permissions` are read, and of each only its
`name` and `required`: the row is a listing, and `shep install` prompts from
the binary's own manifest. A manifest that declares nothing prints no
`permissions` key at all, and writes the empty list that says "none". A
manifest is optional. Without one the row carries no `permissions`, which says
"not listed", and a list an earlier version left in the row goes out with the
version it described, because a stale list reads as a claim about this one.

The row's `repo` must name the repository the release was built from, so a
release elsewhere cannot approve another dog's row.

A row is a claim about which version shep will install, so a run for an older
version than the row already names is refused rather than written: it would set
the row back to hashes of assets that release no longer serves. The same holds
for a version a pull request for this dog is still holding, which is what
`--pending` says, and for a version the dog has released and this run is not,
which is what `--released` says. A cancelled run in the workflow's concurrency
queue is how a newer release goes unwritten, and the release list is what tells
the next run for that dog that it is not the newest one. All three exit 3, which
says "a newer version is already released, approved or pending" and is not this
run's failure. When the refusal leaves main naming hashes the release no longer
serves, the message says so, because that is the part no one can see from the
release page.

Exits 0 on success, 1 when it refuses, 3 when a newer version is already
released, approved or pending; either refusal leaves the index unwritten.

Usage:

    python3 scripts/dog-index-update.py web/public/dogs.json \\
        shep-log-rotate 0.2.5 sums.txt shep-pm/shep-log-rotate [manifest.json] \\
        [--pending 0.3.0] [--released "0.2.5 0.3.0"]
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


class IndexUpdateError(Exception):
    """The change cannot be made safely; the message says why."""


class VersionOutOfDateError(IndexUpdateError):
    """A newer version is already approved, or pending in an open pull request.

    Exit 3 rather than 1: nothing here is broken. The index and the open pull
    request are right as they stand, and this run wrote nothing.
    """


SUM_LINE = re.compile(r"^(?P<digest>\S+) [ *](?P<name>\S+)$")
DIGEST = re.compile(r"^[0-9a-f]{64}$")
# A triple is at least arch-vendor-os, with no dot: stricter than `propertyNames`
# in web/public/dogs.schema.json on purpose, so a `.sha256` or `.sig` sitting
# beside the binaries on the release is refused instead of becoming a target.
TARGET = re.compile(r"^[a-z0-9_]+(-[a-z0-9_]+){2,}$")
# What crates/shep-core/src/dogs/manifest.rs says makes an answer a manifest:
# "output without them is not a manifest, and the asker falls back to the
# version and schema flags". A document missing one is not a dog that declared
# nothing, so it is refused rather than read as an empty list.
MANIFEST_FIELDS = ("manifest", "name", "version", "protocol")
# Semver 2.0, which is what `resolve` accepts as a tag and what a row carries.
# Build metadata is parsed and then ignored, because section 11 leaves it out of
# precedence: `1.0.0+a` and `1.0.0+b` are the same version to compare.
VERSION = re.compile(
    r"^(?P<major>0|[1-9][0-9]*)\.(?P<minor>0|[1-9][0-9]*)\.(?P<patch>0|[1-9][0-9]*)"
    r"(?:-(?P<prerelease>[0-9A-Za-z.-]+))?(?:\+(?P<build>[0-9A-Za-z.-]+))?$"
)
PRERELEASE_IDENTIFIER = re.compile(r"^(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)$")
BUILD_IDENTIFIER = re.compile(r"^[0-9A-Za-z-]+$")
NUMERIC = re.compile(r"^(?:0|[1-9][0-9]*)$")


def parse_version(text: str) -> tuple[int, int, int, list[str]]:
    """A version as `(major, minor, patch, prerelease)`, or a refusal.

    Build metadata is checked and dropped: it says which build made the
    version, not which version outranks which.
    """
    match = VERSION.match(text)
    if match is None:
        raise IndexUpdateError(f"{text!r} is not a semver version")
    prerelease: list[str] = []
    if match["prerelease"] is not None:
        prerelease = match["prerelease"].split(".")
        for identifier in prerelease:
            if not PRERELEASE_IDENTIFIER.match(identifier):
                raise IndexUpdateError(f"{text!r} is not a semver version: {identifier!r} is not an identifier")
    if match["build"] is not None:
        for identifier in match["build"].split("."):
            if not BUILD_IDENTIFIER.match(identifier):
                raise IndexUpdateError(f"{text!r} is not a semver version: {identifier!r} is not build metadata")
    return int(match["major"]), int(match["minor"]), int(match["patch"]), prerelease


def compare_prerelease(left: list[str], right: list[str]) -> int:
    """-1, 0 or 1 as semver 2.0 section 11 ranks two prerelease lists.

    A version with no prerelease outranks one with. Inside a prerelease,
    identifiers compare pairwise: numbers as numbers, alphanumeric as strings,
    and a number below an alphanumeric. A longer list outranks its own prefix,
    and two lists that match identifier for identifier are equal.
    """
    if not left and not right:
        return 0
    if not left:
        return 1
    if not right:
        return -1
    for a, b in zip(left, right):
        if a == b:
            continue
        numeric_a, numeric_b = NUMERIC.match(a) is not None, NUMERIC.match(b) is not None
        if numeric_a and numeric_b:
            return -1 if int(a) < int(b) else 1
        if numeric_a:
            return -1
        if numeric_b:
            return 1
        return -1 if a < b else 1
    if len(left) == len(right):
        return 0
    return -1 if len(left) < len(right) else 1


def outranks(candidate: str, version: str) -> bool:
    """Whether `candidate` is a version newer than `version`.

    A `candidate` that is not a version answers no: a dog's tag list can hold
    anything, and only a version can outrank one.
    """
    try:
        return newer(candidate, version)
    except IndexUpdateError:
        return False


def newest_outranking(candidates: tuple[str, ...], version: str) -> str | None:
    """The newest of `candidates` that outranks `version`, or `None`."""
    newest: str | None = None
    for candidate in candidates:
        if outranks(candidate, version) and (newest is None or newer(candidate, newest)):
            newest = candidate
    return newest


def newer(left: str, right: str) -> bool:
    """Whether version `left` outranks `right`, build metadata ignored.

    Equal versions are not newer: a re-run of the version a row already
    approves is the run that replaces its hashes.
    """
    a, b = parse_version(left), parse_version(right)
    for x, y in zip(a[:3], b[:3]):
        if x != y:
            return x > y
    return compare_prerelease(a[3], b[3]) > 0


def same_version(left: str, right: str) -> bool:
    """Whether two versions rank equal, build metadata ignored."""
    return not newer(left, right) and not newer(right, left)


def parse_hashes(package: str, version: str, hashes: str) -> dict[str, str]:
    """Map target triple to digest from `sha256sum` lines over a release's assets."""
    prefix = f"{package}-{version}-"
    found: dict[str, str] = {}
    for line in hashes.splitlines():
        if not line.strip():
            continue
        match = SUM_LINE.match(line)
        if match is None:
            raise IndexUpdateError(f"not a sha256sum line: {line!r}")
        name = match["name"]
        if not name.startswith(prefix):
            raise IndexUpdateError(f"{name!r} does not start with {prefix!r}")
        target = name.removeprefix(prefix).removesuffix(".exe")
        if not TARGET.match(target):
            raise IndexUpdateError(f"{name!r} does not end in a target triple")
        if not DIGEST.match(match["digest"]):
            raise IndexUpdateError(f"the sha256 for {name!r} is not 64 lowercase hex digits")
        if target in found:
            raise IndexUpdateError(f"two assets for target {target}")
        found[target] = match["digest"]
    if not found:
        raise IndexUpdateError("no hashes to write")
    return dict(sorted(found.items()))


def read_manifest(path: str) -> str:
    """The text a release leg kept when its built binary answered `--shep-manifest`."""
    try:
        return Path(path).read_text()
    except OSError as err:
        raise IndexUpdateError(f"there is no manifest to read at {path}: {err}") from err


def manifest_permissions(text: str) -> list[object]:
    """The `permissions` entries of a manifest, as the manifest printed them.

    A manifest with nothing declared prints no `permissions` key: manifest.rs
    serializes the field with `skip_serializing_if = "Vec::is_empty"`. That
    reads as an empty list, which is the list the row has to carry, because a
    row with no key at all reads as "not listed" and an empty one as "none".

    `MANIFEST_FIELDS` is what manifest.rs says makes an answer a manifest, so a
    document without them is refused rather than read as a dog declaring none.
    Everything else in the manifest is left where it is: the row lists
    permissions, and a dog's settings schema, verbs and events belong to the
    dog. `validate_permissions` decides which of the entries read here can be
    written.
    """
    try:
        document = json.loads(text)
    except json.JSONDecodeError as err:
        raise IndexUpdateError(f"the manifest is not JSON: {err}") from err
    if not isinstance(document, dict):
        raise IndexUpdateError("the manifest is not a JSON object")
    missing = [field for field in MANIFEST_FIELDS if field not in document]
    if missing:
        raise IndexUpdateError(f"the manifest is missing {', '.join(missing)}")
    if "permissions" not in document:
        return []
    permissions = document["permissions"]
    if not isinstance(permissions, list):
        raise IndexUpdateError(f"the manifest's permissions is {type(permissions).__name__}, not a list")
    return permissions


def validate_permissions(permissions: list[object], label: str) -> list[dict[str, object]]:
    """Each permission as `{"name", "required"}`, in the order the manifest gave.

    A name is a non-empty string and `required` is a boolean, which is what
    `web/public/dogs.schema.json` requires of a row's permissions: a row the
    site's own validator would refuse is refused here instead of opening a
    pull request that fails. `label` names the dog in the message.
    """
    written: list[dict[str, object]] = []
    for permission in permissions:
        if not isinstance(permission, dict):
            raise IndexUpdateError(f"{label}: a permission is {permission!r}, not an object")
        name = permission.get("name")
        required = permission.get("required")
        if not isinstance(name, str) or not name:
            raise IndexUpdateError(f"{label}: a permission's name is {name!r}, not a non-empty string")
        if not isinstance(required, bool):
            raise IndexUpdateError(f"{label}: a permission's required is {required!r}, not true or false")
        written.append({"name": name, "required": required})
    return written


def same_repository(url: str, repository: str) -> bool:
    """Whether a row's `repo` URL names `owner/name` on GitHub, ignoring case, `.git` and a slash."""
    path = url.removeprefix("https://github.com/").rstrip("/").removesuffix(".git")
    return url.startswith("https://github.com/") and path.lower() == repository.lower()


def apply(
    index: str,
    package: str,
    version: str,
    hashes: str,
    repository: str,
    permissions: list[object] | None = None,
    pending: str | None = None,
    released: tuple[str, ...] = (),
) -> str:
    """The index text with `package`'s cargo row approving `version` and `hashes`.

    `repository` is the `owner/name` the release was built from; the row's
    `repo` has to name it, so a release elsewhere cannot approve this row.
    `permissions` is what the released version's manifest declared, or `None`
    when no manifest was handed over, which writes no `permissions` key. A
    list the row carried goes out either way: it described the version being
    replaced, and this row is read as a statement about the one it now names.
    `pending` is the version an open index pull request for this dog would
    merge, or `None` when none is open. `released` is the dog's own tag list.

    Raises `VersionOutOfDateError` when the row, `pending`, or a tag on
    `released` outranks `version`: this run is not the one the index should
    move to, and writing it would set the index back to the hashes of a release
    the dog no longer serves. A `released` entry that is not a version is left
    out, because a dog's tag list can hold anything.
    """
    if not version.strip():
        raise IndexUpdateError("version is empty")
    digests = parse_hashes(package, version, hashes)
    declared = None if permissions is None else validate_permissions(permissions, package)
    document = json.loads(index)
    rows = [row for row in document["dogs"] if row["package"] == package]
    if not rows:
        raise IndexUpdateError(
            f"no row for package {package!r}: adding a dog to the index is a pull request of its own"
        )
    if not same_repository(rows[0]["repo"], repository):
        raise IndexUpdateError(
            f"{package!r} lists {rows[0]['repo']} as its repo, but the release was built from {repository}"
        )
    source = rows[0]["source"]
    if source["kind"] != "cargo":
        raise IndexUpdateError(f"{package!r} is a {source['kind']!r} row, and only a cargo row takes hashes")
    approved = source.get("version")
    if approved is not None and not isinstance(approved, str):
        raise IndexUpdateError(f"{package!r}'s row carries {approved!r} as its version, not a string")
    ahead: dict[str, list[str]] = {}
    if approved is not None and newer(approved, version):
        ahead.setdefault(approved, []).append("the row already approves it")
    if pending is not None and newer(pending, version):
        ahead.setdefault(pending, []).append("pending in an open index pull request")
    newest_released = newest_outranking(released, version)
    if newest_released is not None:
        ahead.setdefault(newest_released, []).append(f"released by {repository}")
    if ahead:
        named = ", and ".join(f"{v}, {' and '.join(whys)}" for v, whys in ahead.items())
        message = f"{package!r}: {named}, each newer than {version}. Writing this row would set the index back."
        if approved is not None and same_version(approved, version) and source.get("sha256") != digests:
            message += (
                f" main's {approved} row names hashes this release no longer serves, so installs of "
                f"{approved} fail verification until a newer row is merged. Re-run {approved} after that."
            )
        raise VersionOutOfDateError(message)
    source["version"] = version
    source["sha256"] = digests
    source.pop("permissions", None)
    if declared is not None:
        source["permissions"] = declared
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


USAGE = (
    "usage: dog-index-update.py <dogs.json> <package> <version> <sha256sum file>"
    " <owner/name> [manifest.json] [--pending <version>] [--released \"<tag> ...\"]"
)


def take_flag(args: list[str], name: str) -> str | None:
    """The value after `name`, both removed from `args`, or `None` when absent."""
    if name not in args:
        return None
    at = args.index(name)
    if at + 1 == len(args):
        raise IndexUpdateError(f"{name} needs a value")
    value = args.pop(at + 1)
    args.pop(at)
    return value


def main(argv: list[str]) -> int:
    args = list(argv)
    try:
        pending = take_flag(args, "--pending")
        released = take_flag(args, "--released")
    except IndexUpdateError as err:
        print(f"{USAGE} ({err})", file=sys.stderr)
        return 1
    if len(args) not in (5, 6):
        print(USAGE, file=sys.stderr)
        return 1
    path, package, version, hashes, repository = args[:5]
    manifest = args[5] if len(args) == 6 else None
    try:
        index = Path(path).read_text()
        sums = Path(hashes).read_text()
        declared = None if manifest is None else manifest_permissions(read_manifest(manifest))
        tags = tuple(released.split()) if released is not None else ()
        updated = apply(index, package, version, sums, repository, declared, pending, tags)
        count = len(parse_hashes(package, version, sums))
    except VersionOutOfDateError as err:
        print(f"dog-index-update: {err}", file=sys.stderr)
        return 3
    except IndexUpdateError as err:
        print(f"dog-index-update: {err}", file=sys.stderr)
        return 1
    Path(path).write_text(updated)
    wrote = f"{count} hashes"
    if declared is not None:
        wrote += f" and {len(declared)} permissions from {manifest}"
    print(f"{package} {version}: wrote {wrote} to {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
