"""Tests for scripts/dog-index-update.py.

Run them with:

    python3 -m unittest discover --start-directory scripts

The script is what `dog-release.yml` runs to turn a release's hashes and the
manifest its Linux binary printed into the `web/public/dogs.json` change a
human reviews, so these pin that change: which row moves, which keys it gains,
and everything the script refuses to write. The last two classes read the
committed index and `web/public/dogs.schema.json`, and the workflow that calls
this script.
"""

from __future__ import annotations

import contextlib
import fnmatch
import importlib.util
import io
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "dog-release.yml"

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


def manifest_text(permissions: list | None = None, **extra: object) -> str:
    """What a dog answers `--shep-manifest` with, with `permissions` as given.

    The rest of the manifest is the fields a real one carries, so a test that
    reads one is reading the shape the workflow hands over, not a shape built
    for it. `permissions=None` leaves the key out, which is what a manifest
    with nothing declared prints.
    """
    body: dict = {"manifest": 2, "name": "example", "version": "0.2.0", "protocol": 7, **extra}
    if permissions is not None:
        body["permissions"] = permissions
    return json.dumps(body)


class ApplyTests(unittest.TestCase):
    def apply(
        self,
        text: str,
        version: str,
        hashes: str,
        package: str = "shep-example",
        repository: str = REPOSITORY,
        permissions: list | None = None,
    ) -> dict:
        return json.loads(
            dog_index_update.apply(text, package, version, hashes, repository, permissions)
        )

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

    # A list written for 0.1.0 would read as 0.2.0's. With no manifest the
    # script has nothing to restate it from, so the list goes out with the
    # version it described rather than standing as a claim about this one.
    def test_a_permissions_list_from_the_version_being_replaced_is_dropped(self) -> None:
        old = {"kind": "cargo", "version": "0.1.0", "permissions": [{"name": "flock.read", "required": True}]}
        doc = self.apply(index_text(row(source=old)), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}))
        self.assertEqual(
            self.source_of(doc),
            {"kind": "cargo", "version": "0.2.0", "sha256": {LINUX: HASH_A}},
        )

    def test_a_manifests_permissions_are_written_with_only_name_and_required(self) -> None:
        # `reason` is what the manifest carries for the install prompt. The row
        # is a listing, and the prompt reads the binary's own manifest.
        permissions = dog_index_update.manifest_permissions(
            manifest_text(
                [
                    {"name": "flock.read", "required": True, "reason": "Lists the flock."},
                    {"name": "logs.write", "required": False, "reason": "Appends to a log file."},
                ]
            )
        )
        doc = self.apply(
            index_text(row()), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), permissions=permissions
        )
        self.assertEqual(
            self.source_of(doc),
            {
                "kind": "cargo",
                "version": "0.2.0",
                "sha256": {LINUX: HASH_A},
                "permissions": [
                    {"name": "flock.read", "required": True},
                    {"name": "logs.write", "required": False},
                ],
            },
        )

    def test_permissions_are_written_in_the_manifests_order(self) -> None:
        # The manifest's order is the order the install prompt asks them in.
        permissions = dog_index_update.manifest_permissions(
            manifest_text(
                [
                    {"name": "logs.write", "required": True, "reason": "Appends."},
                    {"name": "flock.read", "required": True, "reason": "Lists the flock."},
                ]
            )
        )
        out = dog_index_update.apply(
            index_text(row()), "shep-example", "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), REPOSITORY, permissions
        )
        self.assertLess(out.index("logs.write"), out.index("flock.read"))
        self.assertLess(out.index('"sha256"'), out.index('"permissions"'))

    def test_a_manifest_declaring_nothing_writes_an_empty_list(self) -> None:
        # An empty list says this version declares none. No key at all says the
        # index lists none, which is a different thing to read.
        permissions = dog_index_update.manifest_permissions(manifest_text([]))
        doc = self.apply(
            index_text(row()), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), permissions=permissions
        )
        self.assertEqual(self.source_of(doc)["permissions"], [])

    def test_a_manifest_replaces_the_permissions_an_older_version_left(self) -> None:
        old = {
            "kind": "cargo",
            "version": "0.1.0",
            "permissions": [{"name": "flock.read", "required": True}],
        }
        permissions = dog_index_update.manifest_permissions(
            manifest_text([{"name": "events.publish", "required": False, "reason": "Publishes."}])
        )
        doc = self.apply(
            index_text(row(source=old)), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), permissions=permissions
        )
        self.assertEqual(self.source_of(doc)["permissions"], [{"name": "events.publish", "required": False}])

    def test_a_permission_with_a_name_that_is_not_a_non_empty_string_is_refused(self) -> None:
        for name in ("", 7, None, True, ["flock.read"], {"name": "flock.read"}):
            with self.subTest(name=name), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "permission"):
                self.apply(
                    index_text(row()),
                    "0.2.0",
                    sums("shep-example", "0.2.0", {LINUX: HASH_A}),
                    permissions=[{"name": name, "required": True}],
                )

    def test_a_permission_with_a_required_that_is_not_a_boolean_is_refused(self) -> None:
        for required in ("true", 1, None, "yes"):
            with self.subTest(required=required), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "permission"):
                self.apply(
                    index_text(row()),
                    "0.2.0",
                    sums("shep-example", "0.2.0", {LINUX: HASH_A}),
                    permissions=[{"name": "flock.read", "required": required}],
                )

    def test_a_permission_that_is_not_an_object_is_refused(self) -> None:
        for item in ("flock.read", None, 7):
            with self.subTest(item=item), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "permission"):
                self.apply(
                    index_text(row()),
                    "0.2.0",
                    sums("shep-example", "0.2.0", {LINUX: HASH_A}),
                    permissions=[item],
                )

    def test_a_permission_with_no_required_is_refused(self) -> None:
        # `required` decides whether the dog can run without it, so a row that
        # leaves it out would be read as whichever way the reader guessed.
        with self.assertRaisesRegex(dog_index_update.IndexUpdateError, "permission"):
            self.apply(
                index_text(row()),
                "0.2.0",
                sums("shep-example", "0.2.0", {LINUX: HASH_A}),
                permissions=[{"name": "flock.read"}],
            )

    def test_a_manifest_that_is_not_a_json_object_is_refused(self) -> None:
        for text in ("not a manifest\n", "7", "[]", ""):
            with self.subTest(text=text), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "manifest"):
                dog_index_update.manifest_permissions(text)

    def test_a_manifest_declaring_nothing_omits_the_key_and_reads_as_none(self) -> None:
        # Manifest v2 serializes `permissions` with skip_serializing_if =
        # "Vec::is_empty" (crates/shep-core/src/dogs/manifest.rs), so a dog
        # with nothing declared prints no `permissions` key at all. That is the
        # same answer as an empty list, and the row has to carry the empty list:
        # the entry reads a missing key as "not listed" and an empty one as
        # "none" (crates/shep-cli/src/lookout/view/catalogue.rs).
        self.assertEqual(dog_index_update.manifest_permissions(manifest_text()), [])
        doc = self.apply(
            index_text(row()),
            "0.2.0",
            sums("shep-example", "0.2.0", {LINUX: HASH_A}),
            permissions=dog_index_update.manifest_permissions(manifest_text()),
        )
        self.assertEqual(self.source_of(doc), {"kind": "cargo", "version": "0.2.0", "sha256": {LINUX: HASH_A}, "permissions": []})

    def test_a_manifest_whose_permissions_is_not_a_list_is_refused(self) -> None:
        # Absent reads as nothing declared. Present and not a list is the
        # manifest saying something a row cannot carry.
        for text in (
            '{"manifest":2,"name":"example","version":"0.2.0","protocol":7,"permissions":"flock.read"}',
            '{"manifest":2,"name":"example","version":"0.2.0","protocol":7,"permissions":null}',
            '{"manifest":2,"name":"example","version":"0.2.0","protocol":7,"permissions":{"name":"flock.read"}}',
        ):
            with self.subTest(text=text), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "permissions"):
                dog_index_update.manifest_permissions(text)

    def test_a_document_without_the_fields_a_manifest_needs_is_refused(self) -> None:
        # manifest.rs names these four as what makes an answer a manifest at
        # all, so a dog printing something else is refused rather than read as
        # a dog declaring none.
        for text in ('{"hello":"world"}', '{"name":"example"}', '{"manifest":2,"name":"example"}'):
            with self.subTest(text=text), self.assertRaisesRegex(dog_index_update.IndexUpdateError, "manifest"):
                dog_index_update.manifest_permissions(text)

    def test_a_manifest_of_any_shape_writes_its_permissions_and_nothing_else(self) -> None:
        # A dog's manifest carries its settings schema, its verbs, its events
        # and a reason beside every permission. None of those belongs in the
        # index row, and a manifest from a later shep reads the same way.
        permissions = dog_index_update.manifest_permissions(
            manifest_text(
                [{"name": "flock.read", "required": True, "reason": "Lists the flock."}],
                settings={"type": "object"},
                actions=[{"name": "flush"}],
                something_a_later_shep_added=1,
            )
        )
        doc = self.apply(
            index_text(row()), "0.2.0", sums("shep-example", "0.2.0", {LINUX: HASH_A}), permissions=permissions
        )
        self.assertEqual(self.source_of(doc)["permissions"], [{"name": "flock.read", "required": True}])

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

    def test_a_sixth_argument_names_the_manifest_and_its_permissions_are_written(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            index.write_text(index_text(row()))
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            manifest = Path(tmp) / "manifest.json"
            manifest.write_text(
                manifest_text([{"name": "flock.read", "required": True, "reason": "Lists the flock."}])
            )
            code, out, _ = self.run_main(
                str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY, str(manifest)
            )
            self.assertEqual(code, 0)
            self.assertIn("1 hashes and 1 permissions", out)
            self.assertEqual(
                json.loads(index.read_text())["dogs"][0]["source"]["permissions"],
                [{"name": "flock.read", "required": True}],
            )

    def test_a_manifest_declaring_nothing_writes_an_empty_list_and_says_zero(self) -> None:
        # The common case: a dog that asks for nothing prints no `permissions`
        # key, and its row says "none" rather than "not listed".
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            index.write_text(index_text(row()))
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            manifest = Path(tmp) / "manifest.json"
            manifest.write_text(manifest_text())
            code, out, err = self.run_main(
                str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY, str(manifest)
            )
            self.assertEqual((code, err), (0, ""))
            self.assertIn("1 hashes and 0 permissions", out)
            self.assertEqual(
                json.loads(index.read_text())["dogs"][0]["source"]["permissions"], []
            )

    def test_five_arguments_still_work_and_write_no_permissions(self) -> None:
        # A leg whose binary printed nothing hands the workflow five arguments,
        # and the row is written as it was before this field existed.
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            index.write_text(index_text(row()))
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            code, _, err = self.run_main(str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY)
            self.assertEqual((code, err), (0, ""))
            self.assertNotIn("permissions", json.loads(index.read_text())["dogs"][0]["source"])

    def test_a_manifest_named_and_absent_is_refused_and_leaves_the_index_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            before = index_text(row())
            index.write_text(before)
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            code, _, err = self.run_main(
                str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY, str(Path(tmp) / "manifest.json")
            )
            self.assertEqual(code, 1)
            self.assertIn("manifest.json", err)
            self.assertEqual(index.read_text(), before)

    def test_a_manifest_that_is_not_json_is_refused_and_leaves_the_index_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            before = index_text(row())
            index.write_text(before)
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            manifest = Path(tmp) / "manifest.json"
            manifest.write_text("shep-example: not a manifest\n")
            code, _, err = self.run_main(
                str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY, str(manifest)
            )
            self.assertEqual(code, 1)
            self.assertIn("manifest", err)
            self.assertEqual(index.read_text(), before)

    def test_a_permission_that_is_not_valid_is_refused_and_leaves_the_index_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            index = Path(tmp) / "dogs.json"
            before = index_text(row())
            index.write_text(before)
            hashes = Path(tmp) / "sums.txt"
            hashes.write_text(sums("shep-example", "0.2.0", {LINUX: HASH_A}))
            manifest = Path(tmp) / "manifest.json"
            manifest.write_text(manifest_text([{"name": "flock.read", "required": "yes"}]))
            code, _, err = self.run_main(
                str(index), "shep-example", "0.2.0", str(hashes), REPOSITORY, str(manifest)
            )
            self.assertEqual(code, 1)
            self.assertIn("permission", err)
            self.assertEqual(index.read_text(), before)

    def test_a_seventh_argument_is_refused_as_a_usage_error(self) -> None:
        code, _, err = self.run_main("a", "b", "c", "d", "e", "f", "g")
        self.assertEqual(code, 1)
        self.assertIn("usage: dog-index-update.py", err)


class CommittedIndexTests(unittest.TestCase):
    """What the workflow edits is the file in this repository."""

    def test_the_committed_index_round_trips_byte_for_byte(self) -> None:
        # A re-serialise that reformatted the file would bury the one-row
        # change a reviewer is meant to read under a whole-file diff.
        text = (REPO_ROOT / "web/public/dogs.json").read_text()
        self.assertEqual(json.dumps(json.loads(text), indent=2, ensure_ascii=False) + "\n", text)

    def test_every_permissions_list_in_the_committed_index_validates_against_the_schema(self) -> None:
        # The checks are read off the schema rather than restated here, so this
        # refuses a row whose permissions drift from what an editor is told. No
        # row lists any yet, so the first release that writes a manifest is
        # what this test starts reading.
        schema = json.loads((REPO_ROOT / "web/public/dogs.schema.json").read_text())
        cargo = schema["properties"]["dogs"]["items"]["properties"]["source"]["oneOf"][0]
        items = cargo["properties"]["permissions"]["items"]
        self.assertEqual(items["properties"]["name"]["type"], "string")
        self.assertEqual(items["properties"]["name"]["minLength"], 1)
        self.assertEqual(items["properties"]["required"]["type"], "boolean")
        self.assertEqual(sorted(items["required"]), ["name", "required"])
        self.assertFalse(items["additionalProperties"])
        index = json.loads((REPO_ROOT / "web/public/dogs.json").read_text())
        for dog in index["dogs"]:
            permissions = dog["source"].get("permissions")
            if permissions is None:
                continue
            self.assertEqual(dog_index_update.validate_permissions(permissions, dog["package"]), permissions)

    def test_the_schema_declares_the_cargo_fields_the_script_writes(self) -> None:
        schema = json.loads((REPO_ROOT / "web/public/dogs.schema.json").read_text())
        cargo = schema["properties"]["dogs"]["items"]["properties"]["source"]["oneOf"][0]
        self.assertEqual(sorted(cargo["properties"]), ["kind", "permissions", "sha256", "version"])
        self.assertEqual(cargo["properties"]["sha256"]["additionalProperties"]["pattern"], "^[0-9a-f]{64}$")
        permission = cargo["properties"]["permissions"]["items"]
        self.assertEqual(sorted(permission["properties"]), ["name", "required"])
        self.assertEqual(permission["required"], ["name", "required"])
        self.assertFalse(permission["additionalProperties"])


class WorkflowTests(unittest.TestCase):
    """What `.github/workflows/dog-release.yml` does with the manifest.

    `scripts/test_dog_release_targets.py` pins the build matrix. These pin what
    it cannot: the manifest is read by the one job that holds a read-only token,
    and its artifact stays out of the release, because every file a release
    serves is a file `shep install` can be handed.
    """

    def job(self, name: str) -> str:
        """One job's text, from its key to the next key at the same indent.

        Comments are gone: a step that says it reads the manifest is not a step
        that reads it.
        """
        lines = WORKFLOW.read_text().splitlines()
        start = next(i for i, line in enumerate(lines) if line == f"  {name}:")
        following = [i for i in range(start + 1, len(lines)) if re.fullmatch(r"  \w[\w-]*:", lines[i])]
        body = lines[start : following[0] if following else len(lines)]
        return "\n".join(re.sub(r"(?<!\S)#.*", "", line) for line in body)

    def step(self, job_name: str, needle: str) -> str:
        """The one step of a job that mentions `needle`."""
        found = [s for s in re.split(r"^      - ", self.job(job_name), flags=re.M)[1:] if needle in s]
        self.assertEqual(len(found), 1, f"{job_name} has {len(found)} steps mentioning {needle!r}")
        return found[0]

    def test_the_upload_job_downloads_no_artifact_holding_a_manifest(self) -> None:
        # `dist-*` is the pattern that decides what reaches the release. The
        # manifest artifact is named beside it, not inside it.
        upload = self.job("upload")
        self.assertEqual(re.findall(r"--pattern\s+['\"]([^'\"]+)['\"]", upload), ["dist-*"])
        artifacts = re.findall(r"^\s+name: ((?:dist|manifest)-.+?)\s*$", self.job("build"), re.M)
        self.assertEqual(sorted(artifacts), ["dist-${{ matrix.target }}", "manifest-${{ matrix.target }}"])
        for name in artifacts:
            if name.startswith("manifest-"):
                for pattern in re.findall(r"--pattern\s+['\"]([^'\"]+)['\"]", upload):
                    self.assertFalse(fnmatch.fnmatch(name, pattern), f"{name} matches {pattern}")

    def test_only_the_read_only_build_job_runs_the_dogs_binary(self) -> None:
        # The manifest is read by exec'ing the built binary, so it is read in
        # the job whose token cannot write anything, and nowhere else.
        self.assertEqual(
            [name for name in ("resolve", "upload", "index-pr") if "--shep-manifest" in self.job(name)],
            [],
        )
        reading = self.step("build", "--shep-manifest")
        self.assertIn("if: matrix.target == 'x86_64-unknown-linux-gnu'", reading)

    def test_the_probe_bounds_a_dog_that_never_exits(self) -> None:
        # A dog that ignores --shep-manifest and starts its loop never returns.
        # Run bare, the probe holds the build job to its 60-minute timeout, and
        # `upload` and `index-pr` need that job: no assets and no index pull
        # request. A manifest that never arrives has to stay a missing manifest.
        reading = self.step("build", "--shep-manifest")
        bound = re.search(r"\btimeout\b(?:\s+-\S+)*\s+(\d+)", reading)
        self.assertIsNotNone(bound, "the probe runs the dog's binary with no time limit")
        seconds = int(bound.group(1))
        self.assertLess(seconds, 600, "the probe is a probe, not a build")
        for line in reading.splitlines():
            if "--shep-manifest" in line and "$asset" in line:
                self.assertIn("timeout", line, f"an unbounded probe: {line.strip()}")

    def test_the_probe_refuses_more_than_one_json_document(self) -> None:
        # `jq -e` reports the last document it read, so a binary printing two
        # objects, the last one holding a permissions array, would pass the
        # build and fail the index job's json.loads after the release is
        # already published. The check has to see every document at once.
        reading = self.step("build", "--shep-manifest")
        guards = [line for line in reading.splitlines() if "jq" in line and "type" in line]
        self.assertEqual(len(guards), 1, "the probe has one manifest check")
        self.assertRegex(guards[0], r"jq -\w*s\b|--slurp", "the check reads only the last document")
        self.assertIn("length == 1", guards[0])

    def test_the_probe_reads_a_manifest_that_declares_no_permissions(self) -> None:
        # Manifest v2 leaves the `permissions` key out when the list is empty
        # (crates/shep-core/src/dogs/manifest.rs), so a check that demands the
        # key calls an ordinary dog a broken one, says so in the job summary,
        # and leaves its row reading "not listed" instead of "none". The check
        # is what manifest.rs says makes an answer a manifest: the four fields
        # it requires, and a permissions that is absent or a list.
        guards = [line for line in self.step("build", "--shep-manifest").splitlines() if "jq" in line and "type" in line]
        self.assertEqual(len(guards), 1, "the probe has one manifest check")
        self.assertIn('has("permissions")', guards[0])
        for field in ("manifest", "name", "version", "protocol"):
            self.assertIn(f'"{field}"', guards[0], f"the check does not require {field}")

    def test_the_index_job_can_fetch_the_manifest_artifact(self) -> None:
        # `gh run download` needs `actions: read`, and the manifest is optional:
        # a leg that printed nothing leaves the job with five arguments.
        index_pr = self.job("index-pr")
        self.assertIn("actions: read", index_pr)
        self.assertIn("manifest-x86_64-unknown-linux-gnu", index_pr)
        self.assertIn("dog-index-update.py", index_pr)

    def test_a_manifest_that_will_not_download_fails_the_index_job(self) -> None:
        # `gh run download` fails the same way whether the artifact was never
        # uploaded or the download broke. Only the run's own artifact list tells
        # them apart, and reading a broken download as "no manifest" would drop
        # the row's permissions and say the dog answered nothing, after the
        # release is out.
        fetch = self.step("index-pr", "gh run download")
        self.assertNotIn("|| true", fetch)
        self.assertIn("/artifacts", fetch)
        self.assertLess(fetch.index("/artifacts"), fetch.index("gh run download"))
        self.assertIn("exit 1", fetch)

    def test_a_run_that_handed_over_no_manifest_still_writes_the_row(self) -> None:
        # The artifact is absent whenever the leg read nothing, and that is the
        # dog's answer, not a failure: the row is still written, minus the
        # permissions.
        fetch = self.step("index-pr", "gh run download")
        self.assertLess(fetch.index("exit 0"), fetch.index("gh run download"))
        write = self.step("index-pr", "dog-index-update.py")
        self.assertIn("MANIFEST", write)


if __name__ == "__main__":
    unittest.main()
