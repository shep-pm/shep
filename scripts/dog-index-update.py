#!/usr/bin/env python3
"""Write a dog release's version and hashes into the community dog index.

`.github/workflows/dog-release.yml` runs this after a dog's binaries are on
its release. The change it makes is the one a human reviews before shep will
install that version, so it touches one row's `source.version` and
`source.sha256` and nothing else, and keeps the file byte-for-byte otherwise.

The hashes file is `sha256sum` output over the release's assets, named
`<package>-<version>-<target>`, plus `.exe` on Windows. The map written is
exactly the targets in that file: a target the new release no longer builds
drops out rather than keeping a stale hash.

Exits 0 on success, 1 when it refuses; a refusal leaves the index unwritten.

Usage:

    python3 scripts/dog-index-update.py web/public/dogs.json \\
        shep-log-rotate 0.2.5 sums.txt
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
# The same grammar as `propertyNames` in web/public/dogs.schema.json.
TARGET = re.compile(r"^[a-z0-9._-]+$")


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


def apply(index: str, package: str, version: str, hashes: str) -> str:
    """The index text with `package`'s cargo row approving `version` and `hashes`."""
    if not version.strip():
        raise IndexUpdateError("version is empty")
    digests = parse_hashes(package, version, hashes)
    document = json.loads(index)
    rows = [row for row in document["dogs"] if row["package"] == package]
    if not rows:
        raise IndexUpdateError(
            f"no row for package {package!r}: adding a dog to the index is a pull request of its own"
        )
    source = rows[0]["source"]
    if source["kind"] != "cargo":
        raise IndexUpdateError(f"{package!r} is a {source['kind']!r} row, and only a cargo row takes hashes")
    source["version"] = version
    source["sha256"] = digests
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print("usage: dog-index-update.py <dogs.json> <package> <version> <sha256sum file>", file=sys.stderr)
        return 1
    path, package, version, hashes = argv
    try:
        sums = Path(hashes).read_text()
        updated = apply(Path(path).read_text(), package, version, sums)
        count = len(parse_hashes(package, version, sums))
    except IndexUpdateError as err:
        print(f"dog-index-update: {err}", file=sys.stderr)
        return 1
    Path(path).write_text(updated)
    print(f"{package} {version}: wrote {count} hashes to {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
