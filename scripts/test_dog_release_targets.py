"""The targets a dog is released for are the targets shep is.

`dog-release.yml` builds a dog for every target `release-artifacts.yml` builds
shep for, so that whatever machine runs `shep install` finds a prebuilt binary
where shep itself has one. GitHub gives a matrix no way to cross from one
workflow file into another, so the list is written twice, and this is what
keeps the copies equal.

Run them with:

    python3 -m unittest discover --start-directory scripts
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parent.parent / ".github" / "workflows"

# What the two matrices must agree on. `build_tool` is an input to the action
# release-artifacts.yml builds with; dog-release.yml calls cargo itself.
SHARED_KEYS = ("target", "os", "packages")


def matrix_entries(text: str) -> list[dict[str, str]]:
    """The `include:` entries of the first matrix in a workflow, as written."""
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines) if re.fullmatch(r"\s*include:\s*", line))
    indent = len(lines[start]) - len(lines[start].lstrip())
    entries: list[dict[str, str]] = []
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        item = re.match(r"-\s+(\w+):\s*(\S.*)$", stripped)
        if item:
            entries.append({item[1]: item[2]})
            continue
        pair = re.match(r"(\w+):\s*(\S.*)$", stripped)
        if pair:
            entries[-1][pair[1]] = pair[2]
    return entries


def shared(entries: list[dict[str, str]]) -> list[dict[str, str]]:
    return [{k: e[k] for k in SHARED_KEYS if k in e} for e in entries]


class MatrixParserTests(unittest.TestCase):
    def test_reads_entries_keys_and_skips_comments(self) -> None:
        text = (
            "    strategy:\n"
            "      matrix:\n"
            "        include:\n"
            "          # a comment\n"
            "          - target: a\n"
            "            os: x\n"
            "\n"
            "          - target: b\n"
            "            os: y\n"
            "            packages: musl-tools\n"
            "    runs-on: z\n"
        )
        self.assertEqual(
            matrix_entries(text),
            [{"target": "a", "os": "x"}, {"target": "b", "os": "y", "packages": "musl-tools"}],
        )


class TargetsTests(unittest.TestCase):
    def test_dog_release_builds_every_target_shep_does_on_the_same_hosts(self) -> None:
        shep = shared(matrix_entries((WORKFLOWS / "release-artifacts.yml").read_text()))
        dog = shared(matrix_entries((WORKFLOWS / "dog-release.yml").read_text()))
        self.assertGreaterEqual(len(shep), 7)
        self.assertEqual(
            dog,
            shep,
            "dog-release.yml's matrix differs from release-artifacts.yml's `upload` job. "
            "Edit both together.",
        )

    def test_every_build_step_waits_on_the_skip_targets_leg(self) -> None:
        text = (WORKFLOWS / "dog-release.yml").read_text()
        build = text[text.index("\n  build:\n") : text.index("\n  upload:\n")]
        steps = re.split(r"\n      - ", build)[1:]
        self.assertTrue(steps[0].startswith("id: leg"), steps[0])
        self.assertGreaterEqual(len(steps), 9)
        for step in steps[1:]:
            self.assertIn("steps.leg.outputs.build == 'true'", step)


if __name__ == "__main__":
    unittest.main()
