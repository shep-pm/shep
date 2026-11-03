"""Tests for scripts/dog-index-update.py.

Run them with:

    python3 -m unittest discover --start-directory scripts

The script is what `dog-release.yml` runs to turn a release's hashes into the
`web/public/dogs.json` change a human reviews, so these pin that change: which
row moves, which keys it gains, and everything the script refuses to write.
The last class reads the committed index and `web/public/dogs.schema.json`.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent

# Hyphenated file name: loaded by location, as test_check_doc_length.py does.
_spec = importlib.util.spec_from_file_location("dog_index_update", SCRIPTS / "dog-index-update.py")
dog_index_update = importlib.util.module_from_spec(_spec)
sys.modules["dog_index_update"] = dog_index_update
_spec.loader.exec_module(dog_index_update)

HASH_A = "a" * 64
HASH_B = "b" * 64
LINUX = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-msvc"
REPOSITORY = "shep-pm/shep-example"


def row(package: str = "shep-example", source: dict | None = None) -> dict:
    return {
        "name": "Rex",
        "package": package,
        "adopt_as": "example",
        "description": "Does an example thing.",
        "repo": "https://github.com/shep-pm/shep-example",
        "license": "MIT",
        "category": "other",
        "source": source if source is not None else {"kind": "cargo", "version": "0.1.0"},
    }


def index_text(*rows: dict) -> str:
    return json.dumps({"$schema": "./dogs.schema.json", "version": 1, "dogs": list(rows)}, indent=2) + "\n"


def sums(package: str, version: str, files: dict[str, str]) -> str:
    """`sha256sum` output for asset names built from the package, version and target."""
    return "".join(f"{digest}  {package}-{version}-{target}\n" for target, digest in files.items())


class ApplyTests(unittest.TestCase):
    def apply(
        self,
        text: str,
        version: str,
        hashes: str,
        package: str = "shep-example",
        repository: str = REPOSITORY,
    ) -> dict:
        return json.loads(dog_index_update.apply(text, package, version, hashes, repository))

    def source_of(self, doc: dict, package: str = "shep-example") -> dict:
        return next(r for r in doc["dogs"] if r["package"] == package)["source"]

    def test_sets_the_version_and_every_hash_on_the_row(self) -> None:
        doc = self.apply(index_text(row()), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A, WINDOWS: HASH_B}))
        self.assertEqual(
            self.source_of(doc),
            {"kind": "cargo", "version": "0.2.0", "sha256": {LINUX: HASH_A, WINDOWS: HASH_B}},
        )

    def test_a_windows_asset_keeps_its_target_without_the_exe(self) -> None:
        hashes = f"{HASH_B}  shep-example-0.2.0-{WINDOWS}.exe\n"
        doc = self.apply(index_text(row()), "0.2.0", hashes)
        self.assertEqual(self.source_of(doc)["sha256"], {WINDOWS: HASH_B})

    def test_the_map_is_replaced_not_merged(self) -> None:
        old = {"kind": "cargo", "version": "0.1.0", "sha256": {"aarch64-apple-darwin": HASH_A}}
        doc = self.apply(index_text(row(source=old)), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_B}))
        self.assertEqual(self.source_of(doc)["sha256"], {LINUX: HASH_B})

    def test_targets_are_written_sorted(self) -> None:
        hashes = sums("shep-example", "0.2.0", {WINDOWS: HASH_B, LINUX: HASH_A})
        out = dog_index_update.apply(index_text(row()), "shep-example", "0.2.0", hashes, REPOSITORY)
        self.assertLess(out.index(WINDOWS), out.index(LINUX))  # pc < unknown

    def test_other_rows_and_the_wrapper_are_untouched(self) -> None:
        other = row(package="shep-other")
        other["name"] = "Moon"
        before = index_text(row(), other)
        doc = self.apply(before, "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}))
        self.assertEqual(doc["dogs"][1], other)
        self.assertEqual(doc["$schema"], "./dogs.schema.json")
        self.assertEqual(doc["version"], 1)

    def test_a_second_run_with_the_same_hashes_changes_nothing(self) -> None:
        hashes = sums("shep-example", "0.2.0", {LINUX: HASH_A})
        once = dog_index_update.apply(index_text(row()), "shep-example", "0.2.0", hashes, REPOSITORY)
        twice = dog_index_update.apply(once, "shep-example", "0.2.0", hashes, REPOSITORY)
        self.assertEqual(once, twice)

    def test_output_ends_in_one_newline_and_indents_by_two(self) -> None:
        out = dog_index_update.apply(index_text(row()), "shep-example", "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), REPOSITORY)
        self.assertTrue(out.endswith("}\n") and not out.endswith("\n\n"))
        self.assertIn('\n  "dogs": [', out)

    def test_a_package_with_no_row_is_refused_by_name(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "shep-missing"):
            self.apply(index_text(row()), "0.2.0", sums("shep-missing", "0.2.0", {LINUX: HASH_A}), package="shep-missing")

    def test_a_row_that_is_not_a_cargo_row_is_refused(self) -> None:
        manual = row(source={"kind": "manual", "instructions": "Run make."})
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "manual"):
            self.apply(index_text(manual), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}))

    def test_no_hashes_is_refused(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "no hashes"):
            self.apply(index_text(row()), "0.2.0", "\n")

    def test_a_hash_that_is_not_64_lowercase_hex_is_refused(self) -> None:
        for bad in ("a" * 63, "A" * 64, "g" * 64):
            with self.subTest(bad=bad), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "sha256"):
                self.apply(index_text(row()), "0.2.0", sums("shep-example", "0.2.0", {LINUX: bad}))

    def test_an_asset_for_another_version_is_refused(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "shep-example-0.1.0-"):
            self.apply(index_text(row()), "0.2.0", sums("shep-example", "0.1.0", {LINUX: HASH_A}))

    def test_an_asset_with_an_empty_or_malformed_target_is_refused(self) -> None:
        for target in ("", "Linux", "x86 64"):
            with self.subTest(target=target), self.assertRaises(dog_index_update.IndexUpdateError):
                self.apply(index_text(row()), "0.2.0", f"{HASH_A}  shep-example-0.2.0-{target}\n")

    def test_a_sidecar_or_short_suffix_is_refused(self) -> None:
        # A checksum sidecar or a signature shares the release's name prefix.
        for target in (f"{LINUX}.sha256", f"{LINUX}.sig", "linux", "x86_64-linux"):
            with self.subTest(target=target), self.assertRaises(dog_index_update.IndexUpdateError):
                self.apply(index_text(row()), "0.2.0", f"{HASH_A}  shep-example-0.2.0-{target}\n")

    def test_a_release_from_another_repository_is_refused(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "someone-else/shep-example"):
            self.apply(
                index_text(row()),
                "0.2.0",
                sums("shep-example", "0.2.0", {LINUX: HASH_A}),
                repository="someone-else/shep-example",
            )

    def test_the_repository_compares_without_case_slash_or_dot_git(self) -> None:
        shouty = row()
        shouty["repo"] = "https://github.com/Shep-PM/Shep-Example.git/"
        doc = self.apply(index_text(shouty), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}))
        self.assertEqual(self.source_of(doc)["version"], "0.2.0")

    def test_the_same_target_twice_is_refused(self) -> None:
        hashes = (
            f"{HASH_A}  shep-example-0.2.0-{WINDOWS}\n"
            f"{HASH_B}  shep-example-0.2.0-{WINDOWS}.exe\n"
        )
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, WINDOWS):
            self.apply(index_text(row()), "0.2.0", hashes)

    def test_a_line_that_is_not_a_sum_is_refused(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "not a sha256sum line"):
            self.apply(index_text(row()), "0.2.0", "hello\n")

    def test_a_binary_mode_marker_is_accepted(self) -> None:
        hashes = f"{HASH_A} *shep-example-0.2.0-{LINUX}\n"
        self.assertEqual(self.source_of(self.apply(index_text(row()), "0.2.0", hashes))["sha256"], {LINUX: HASH_A})

    def test_an_empty_version_is_refused(self) -> None:
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "version"):
            self.apply(index_text(row()), "", sums("shep-example", "", {LINUX: HASH_A}))


class CommandLineTests(unittest.TestCase):
    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = dog_index_update.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_writes_the_index_in_place_and_names_the_package_and_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            index.write_text(index_text(row()))
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            code, out, _ = self.run_main(str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY)
            self.assertEqual(code, 0)
            self.assertIn("shep-example 0.2.0", out)
            self.assertEqual(json.loads(index.read_text())["dogs"][0]["source"]["sha256"], {LINUX: HASH_A})

    def test_a_refusal_exits_one_and_leaves_the_file_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            before = index_text(row())
            index.write_text(before)
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text("hello\n")
            code, _, err = self.run_main(str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY)
            self.assertEqual(code, 1)
            self.assertIn("not a sha256sum line", err)
            self.assertEqual(index.read_text(), before)


class CommittedIndexTests(unittest.TestCase):
    """What the workflow edits is the file in this repository."""

    def test_the_committed_index_round_trips_byte_for_byte(self) -> None:
        # A re-serialise that reformatted the file would bury the one-row
        # change a reviewer is meant to read under a whole-file diff.
        text = (REPO_ROOT / "web/public/dogs.json").read_text()
        self.assertEqual(json.dumps(json.loads(text), indent=2, ensure_ascii=False) + "\n", text)

    def test_the_schema_declares_the_cargo_fields_the_script_writes(self) -> None:
        schema = json.loads((REPO_ROOT / "web/public/dogs.schema.json").read_text())
        cargo = schema["properties"]["dogs"]["items"]["properties"]["source"]["oneOf"][0]
        self.assertEqual(sorted(cargo["properties"]), ["kind", "sha256", "version"])
        self.assertEqual(cargo["properties"]["sha256"]["additionalProperties"]["pattern"], "^[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
