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

Exits 0 on success, 1 when it refuses; a refusal leaves the index unwritten.

Usage:

    python3 scripts/dog-index-update.py web/public/dogs.json \\
        shep-log-rotate 0.2.5 sums.txt shep-pm/shep-log-rotate [manifest.json]
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


class IndexUpdateError(Exception):
    """The change cannot be made safely; the message says why."""


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
) -> str:
    """The index text with `package`'s cargo row approving `version` and `hashes`.

    `repository` is the `owner/name` the release was built from; the row's
    `repo` has to name it, so a release elsewhere cannot approve this row.
    `permissions` is what the released version's manifest declared, or `None`
    when no manifest was handed over, which writes no `permissions` key. A
    list the row carried goes out either way: it described the version being
    replaced, and this row is read as a statement about the one it now names.
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
    source["version"] = version
    source["sha256"] = digests
    source.pop("permissions", None)
    if declared is not None:
        source["permissions"] = declared
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) not in (5, 6):
        print(
            "usage: dog-index-update.py <dogs.json> <package> <version> <sha256sum file>"
            " <owner/name> [manifest.json]",
            file=sys.stderr,
        )
        return 1
    path, package, version, hashes, repository = argv[:5]
    manifest = argv[5] if len(argv) == 6 else None
    try:
        index = Path(path).read_text()
        sums = Path(hashes).read_text()
        declared = None if manifest is None else manifest_permissions(read_manifest(manifest))
        updated = apply(index, package, version, sums, repository, declared)
        count = len(parse_hashes(package, version, sums))
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
