"""Tests for scripts/fix-release-bump.py.

Run them with:

    python3 -m unittest discover --start-directory scripts

The end-to-end case builds a throwaway repository whose history is the diamond
release-plz under-walks, with a release branch cut the way release-plz cuts
one, and a bare remote standing in for github.com. Nothing here touches the
real repository or any remote.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent

_spec = importlib.util.spec_from_file_location("fix_release_bump", SCRIPTS / "fix-release-bump.py")
fix = importlib.util.module_from_spec(_spec)
sys.modules["fix_release_bump"] = fix
_spec.loader.exec_module(fix)

HEADER = (
    "# Changelog\n"
    "\n"
    "All notable changes to this crate are documented in this file.\n"
    "\n"
    "The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),\n"
    "and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).\n"
    "\n"
)

RELEASE_BRANCH = "release-plz-2026-09-06T00-00-00Z"

INDEX_PATH = "web/public/dogs.json"


def index_text(shep: str | None, **extra: object) -> str:
    """The dog index as `scripts/dog-index-update.py` writes it, naming `shep`.

    A row with a non-ASCII description is in it, so a rewrite that escapes it
    shows as a change to a line the guard has no business touching.
    """
    document: dict[str, object] = {"$schema": "./dogs.schema.json", "version": 1}
    if shep is not None:
        document["shep"] = shep
    document.update(extra)
    document["dogs"] = [
        {
            "name": "Bella",
            "package": "shep-log-rotate",
            "description": "Rotates grown log files \u2014 and asks the shepherd to reopen them.",
        }
    ]
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


class Fixture(unittest.TestCase):
    """A repository with a diamond in it, a release branch under it, and a remote."""

    # The far leg's headline commit. A subclass that takes the `!` out holds the
    # ordinary feature release a guard has no business touching.
    breaks = True

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.remote = self.tmp / "remote.git"
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        self.remote.mkdir()

        self.git_init(cwd=self.remote, bare=True)
        self.git_init(cwd=self.repo)
        self.git("remote", "add", "origin", str(self.remote), cwd=self.repo)

        for name in ("release-plz.toml", "release-plz-changelog.toml"):
            (self.repo / name).write_text((REPO_ROOT / name).read_text())

        self._write_manifests()
        self._write_changelogs()
        self.write(INDEX_PATH, index_text("0.13.0"))
        self.commit("feat(core): base work", date="2026-09-01T00:00:00")
        self.base_sha = self.head()
        self.git("tag", "shep-v0.13.0")
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")

        # Two legs cut from the same base, merged in turn: the shape that
        # sends release-plz's walk down one of them.
        self.git("checkout", "-q", "-b", "leg-a")
        self.write("crates/shep-daemon/src/dogs.rs", "pub fn at_rest() {}\n")
        at_rest = (
            "feat(daemon)!: list every known dog at rest"
            if self.breaks
            else "feat(daemon): list every known dog at rest"
        )
        self.commit(at_rest, date="2026-09-02T00:00:00")
        leg_a = self.head()
        self.git("checkout", "-q", "main")

        self.git("checkout", "-q", "-b", "leg-b", "leg-a~1")
        self.write("crates/shep-cli/src/lookout.rs", "pub fn remove_entry() {}\n")
        self.commit(
            "feat(cli): remove an entry from an array or map of tables",
            date="2026-09-02T01:00:00",
        )
        self.git("checkout", "-q", "main")

        # #807 squash-merged onto main, then #803 merged as a merge commit.
        self.git("cherry-pick", leg_a)
        self.amend_subject(f"{at_rest} (#807)", "2026-09-03T00:00:00")
        self.git(
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #803 from shep-pm/leg-b",
            "leg-b",
            date="2026-09-04T00:00:00",
        )
        self.amend_subject("Merge pull request #803 from shep-pm/leg-b", "2026-09-04T00:00:00")

        # The one commit release-plz's walk does reach: it sits on the leg the
        # walk entered, past the merge it followed.
        self.git("checkout", "-q", "-b", "leg-c")
        self.write("crates/shep-daemon/src/secrets.rs", "pub fn refire() {}\n")
        self.commit(
            "feat(daemon): re-fire config.dog.<name> when secrets.json changes a value a dog names",
            date="2026-09-05T00:00:00",
        )
        leg_c = self.head()
        self.git("checkout", "-q", "main")
        self.git("cherry-pick", leg_c)
        self.amend_subject(
            "feat(daemon): re-fire config.dog.<name> when secrets.json changes a value a dog names (#813)",
            "2026-09-06T00:00:00",
        )
        self.main_head = self.head()
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")

        self._write_release_branch()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # Building it.

    def git(self, *args: str, cwd: Path | None = None, date: str | None = None) -> str:
        env = dict(os.environ)
        if date:
            # Commit dates are what the new section's own date comes from, so
            # the fixture pins them rather than inheriting the clock.
            env["GIT_AUTHOR_DATE"] = date
            env["GIT_COMMITTER_DATE"] = date
        return subprocess.run(
            ["git", *args],
            cwd=cwd or self.repo,
            check=True,
            capture_output=True,
            text=True,
            env=env,
        ).stdout

    def git_init(self, cwd: Path, bare: bool = False) -> None:
        args = ["init", "-q", "--initial-branch=main"] + (["--bare"] if bare else [])
        self.git(*args, cwd=cwd)
        if not bare:
            self.git("config", "user.name", "fixture", cwd=cwd)
            self.git("config", "user.email", "fixture@example.invalid", cwd=cwd)

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, subject: str, date: str) -> None:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", subject, date=date)

    def amend_subject(self, subject: str, date: str) -> None:
        self.git("commit", "-q", "--amend", "-m", subject, date=date)

    def head(self, rev: str = "HEAD") -> str:
        return self.git("rev-parse", rev).strip()

    def remote_head(self, branch: str) -> str:
        return self.git("rev-parse", branch, cwd=self.remote).strip()

    def remote_file(self, branch: str, path: str) -> str:
        return self.git("show", f"{branch}:{path}", cwd=self.remote)

    def _write_manifests(self) -> None:
        self.write(
            "Cargo.toml",
            '[workspace]\n'
            'resolver = "3"\n'
            'members = [\n'
            '    "crates/shep-core",\n'
            '    "crates/shep-daemon",\n'
            '    "crates/shep-cli",\n'
            '    "crates/shep-cli-redirect",\n'
            ']\n'
            '\n'
            '[workspace.package]\n'
            'version = "0.13.0"\n'
            'edition = "2024"\n'
            '\n'
            '[workspace.dependencies]\n'
            'shep-core = { path = "crates/shep-core", version = "0.13.0" }\n'
            'shep-daemon = { path = "crates/shep-daemon", version = "0.13.0" }\n'
            '# A third-party pin sitting at the same number as the workspace,\n'
            '# which a release bump must leave alone.\n'
            'lookalike = { version = "0.13.0", default-features = false }\n',
        )
        self.write(
            "Cargo.lock",
            'version 4\n'
            '\n'
            '[[package]]\n'
            'name = "lookalike"\n'
            'version = "0.13.0"\n'
            '\n'
            '[[package]]\n'
            'name = "shep"\n'
            'version = "0.13.0"\n'
            '\n'
            '[[package]]\n'
            'name = "shep-cli"\n'
            'version = "0.13.0"\n'
            '\n'
            '[[package]]\n'
            'name = "shep-core"\n'
            'version = "0.13.0"\n'
            '\n'
            '[[package]]\n'
            'name = "shep-daemon"\n'
            'version = "0.13.0"\n',
        )
        for directory, name in (
            ("crates/shep-core", "shep-core"),
            ("crates/shep-daemon", "shep-daemon"),
            ("crates/shep-cli", "shep"),
            ("crates/shep-cli-redirect", "shep-cli"),
        ):
            self.write(
                f"{directory}/Cargo.toml",
                "[package]\n"
                f'name = "{name}"\n'
                'description = "fixture"\n'
                "version.workspace = true\n"
                "edition.workspace = true\n",
            )

    def _write_changelogs(self) -> None:
        for directory in ("crates/shep-core", "crates/shep-daemon", "crates/shep-cli"):
            self.write(
                f"{directory}/CHANGELOG.md",
                HEADER + "## [Unreleased]\n\n## [0.13.0] - 2026-09-01\n\n### Added\n\n- Base work\n\n\n",
            )

    def _write_release_branch(self) -> None:
        """The branch release-plz would have pushed: 0.13.1, and a changelog
        that only carries the commit its walk reached."""
        self.git("checkout", "-q", "-b", RELEASE_BRANCH)
        manifest = (self.repo / "Cargo.toml").read_text()
        manifest = manifest.replace('[workspace.package]\nversion = "0.13.0"', '[workspace.package]\nversion = "0.13.1"')
        for member in ("shep-core", "shep-daemon"):
            manifest = manifest.replace(
                f'{member} = {{ path = "crates/{member}", version = "0.13.0" }}',
                f'{member} = {{ path = "crates/{member}", version = "0.13.1" }}',
            )
        self.write("Cargo.toml", manifest)
        lock = (self.repo / "Cargo.lock").read_text()
        for member in ("shep", "shep-cli", "shep-core", "shep-daemon"):
            lock = lock.replace(
                f'name = "{member}"\nversion = "0.13.0"',
                f'name = "{member}"\nversion = "0.13.1"',
            )
        self.write("Cargo.lock", lock)
        for directory in ("crates/shep-core", "crates/shep-daemon", "crates/shep-cli"):
            text = (self.repo / f"{directory}/CHANGELOG.md").read_text()
            section = "## [0.13.1] - 2026-09-06\n\n"
            if directory == "crates/shep-daemon":
                section += (
                    "### Added\n\n"
                    "- Re-fire config.dog.<name> when secrets.json changes a value a dog names\n\n"
                )
            section += "\n"
            self.write(f"{directory}/CHANGELOG.md", text.replace("## [Unreleased]\n\n", "## [Unreleased]\n\n" + section))
        self.commit("chore: release v0.13.1", date="2026-09-06T01:00:00")
        self.git("push", "-q", "origin", f"HEAD:refs/heads/{RELEASE_BRANCH}")
        self.git("checkout", "-q", "main")
        self.release_head = self.remote_head(RELEASE_BRANCH)

    def amend_release_branch(self, path: str, text: str) -> None:
        """Put one file of the release branch in a different shape, as a commit
        on top of it. That is what a hand edit on the branch looks like."""
        self.git("checkout", "-q", RELEASE_BRANCH)
        self.write(path, text)
        self.commit("chore: hand edit on the release branch", date="2026-09-06T02:00:00")
        self.git("push", "-q", "origin", f"HEAD:refs/heads/{RELEASE_BRANCH}")
        self.git("checkout", "-q", "main")
        self.release_head = self.remote_head(RELEASE_BRANCH)

    # Running the guard.

    def run_guard(self, output: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "fix-release-bump.py"),
                "--repo",
                str(self.repo),
                "--base-sha",
                self.main_head,
                "--branch",
                RELEASE_BRANCH,
                "--remote",
                "origin",
                "--output",
                str(output),
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def results(self, output: Path) -> dict[str, str]:
        return dict(
            line.split("=", 1)
            for line in output.read_text().splitlines()
            if "=" in line
        )


class TestVersion(unittest.TestCase):
    """The bump a range implies, with no repository behind it."""

    BREAK = [fix.Commit(sha="z", date="2026-09-02", subject="feat(daemon)!: at rest", body="")]
    FEAT = [fix.Commit(sha="x", date="2026-09-02", subject="feat(cli): a verb", body="")]
    FIX = [fix.Commit(sha="y", date="2026-09-02", subject="fix(cli): a verb", body="")]

    def test_a_breaking_commit_pre_one_point_zero_moves_the_minor(self) -> None:
        self.assertEqual(fix.required_bump((0, 13, 0), self.BREAK, False), "minor")

    def test_a_breaking_commit_from_one_point_zero_moves_the_major(self) -> None:
        self.assertEqual(fix.required_bump((1, 2, 3), self.BREAK, False), "major")

    def test_a_breaking_footer_counts_as_a_break(self) -> None:
        commits = [
            fix.Commit(
                sha="z",
                date="2026-09-02",
                subject="fix(daemon): widen the reply",
                body="BREAKING CHANGE: the reply shape changed.\n",
            )
        ]
        self.assertEqual(fix.required_bump((0, 13, 0), commits, False), "minor")

    def test_a_pre_one_point_zero_feat_is_a_patch(self) -> None:
        """release-plz's own rule, not a stricter one.

        Measured across this repository's 74 tag transitions: every minor bump
        carries a breaking commit, and every range with a `feat` and no break
        came out a patch. A guard that read a feature as a minor would rewrite
        a correct feature release PR on every push to main.
        """
        self.assertEqual(fix.required_bump((0, 13, 0), self.FEAT, False), "patch")
        self.assertEqual(fix.required_bump((0, 13, 0), self.FEAT + self.FIX, False), "patch")

    def test_a_feat_from_one_point_zero_moves_the_minor(self) -> None:
        self.assertEqual(fix.required_bump((1, 2, 3), self.FEAT, False), "minor")

    def test_features_always_increment_minor_is_honoured(self) -> None:
        self.assertEqual(fix.required_bump((0, 13, 0), self.FEAT, True), "minor")

    def test_a_fix_is_a_patch(self) -> None:
        self.assertEqual(fix.required_bump((0, 13, 0), self.FIX, False), "patch")

    def test_nothing_to_release_implies_no_bump(self) -> None:
        self.assertIsNone(fix.required_bump((0, 13, 0), [], False))

    def test_bump_taken_reads_the_number_release_plz_moved(self) -> None:
        self.assertEqual(fix.bump_taken((0, 13, 0), (0, 13, 1)), "patch")
        self.assertEqual(fix.bump_taken((0, 13, 0), (0, 14, 0)), "minor")
        self.assertEqual(fix.bump_taken((0, 13, 0), (1, 0, 0)), "major")
        self.assertIsNone(fix.bump_taken((0, 13, 0), (0, 13, 0)))

    def test_apply_bump_leaves_a_zero_major_release_at_zero_major(self) -> None:
        self.assertEqual(fix.apply_bump((0, 13, 0), "patch"), (0, 13, 1))
        self.assertEqual(fix.apply_bump((0, 13, 0), "minor"), (0, 14, 0))
        self.assertEqual(fix.apply_bump((1, 2, 3), "minor"), (1, 3, 0))
        self.assertEqual(fix.apply_bump((0, 13, 0), "major"), (1, 0, 0))

    def test_a_larger_level_ranks_above_a_smaller_one(self) -> None:
        self.assertLess(fix.BUMP_LEVELS["patch"], fix.BUMP_LEVELS["minor"])
        self.assertLess(fix.BUMP_LEVELS["minor"], fix.BUMP_LEVELS["major"])


class TestChangelogRules(Fixture):
    def rules(self) -> fix.ChangelogRules:
        return fix.load_changelog_rules(fix.Repo(self.repo))

    def line_of(self, subject: str, body: str = "") -> str | None:
        entry = fix.entry_for(
            fix.Commit(sha="x", date="2026-09-02", subject=subject, body=body), self.rules()
        )
        return entry.line if entry else None

    def test_skipped_types_leave_no_line(self) -> None:
        for subject in (
            "docs(daemon): a note",
            "test(daemon): a case",
            "ci: a workflow",
            "chore: a tidy",
            "style: a format",
        ):
            self.assertIsNone(self.line_of(subject), subject)

    def test_a_breaking_commit_survives_its_own_skip(self) -> None:
        self.assertEqual(
            self.line_of("docs(daemon)!: the note is a contract"),
            "- The note is a contract **(BREAKING)**",
        )

    def test_a_pure_move_leaves_no_line_but_a_real_refactor_does(self) -> None:
        self.assertIsNone(self.line_of("refactor(lookout): split the scene tests out"))
        self.assertEqual(
            self.line_of("refactor(lookout): read the roll once per sweep"),
            "- Read the roll once per sweep",
        )

    def test_an_unconventional_subject_leaves_no_line(self) -> None:
        self.assertIsNone(self.line_of("Merge pull request #803 from shep-pm/leg-b"))
        self.assertIsNone(self.line_of("revert: drop the verb"))

    def test_a_pull_request_reference_is_not_part_of_the_line(self) -> None:
        self.assertEqual(
            self.line_of("feat(daemon)!: list every known dog at rest (#807)"),
            "- List every known dog at rest **(BREAKING)**",
        )

    def test_groups_come_from_the_parsers(self) -> None:
        rules = self.rules()
        groups = {
            subject: fix.entry_for(
                fix.Commit(sha="x", date="2026-09-02", subject=subject, body=""), rules
            ).group
            for subject in (
                "feat(cli): a",
                "fix(cli): a",
                "perf(cli): a",
                "refactor(cli): a",
                "chore: update Cargo.toml dependencies",
            )
        }
        self.assertEqual(
            groups,
            {
                "feat(cli): a": "Added",
                "fix(cli): a": "Fixed",
                "perf(cli): a": "Performance",
                "refactor(cli): a": "Changed",
                "chore: update Cargo.toml dependencies": "Internal",
            },
        )


class TestSectionRewrite(unittest.TestCase):
    def entry(self, group: str, line: str) -> fix.Entry:
        return fix.Entry(group=group, line=line, sha="x")

    def test_a_missing_entry_lands_under_its_group(self) -> None:
        text = HEADER + "## [Unreleased]\n\n## [0.14.0] - 2026-09-06\n\n### Added\n\n- Kept line\n\n\n"
        rewritten, added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- List every known dog at rest **(BREAKING)**")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual(
            rewritten,
            HEADER
            + "## [Unreleased]\n\n## [0.14.0] - 2026-09-06\n\n### Added\n\n"
            "- Kept line\n- List every known dog at rest **(BREAKING)**\n\n\n",
        )
        self.assertEqual(len(added), 1)

    def test_a_new_group_sorts_among_the_groups_already_there(self) -> None:
        text = HEADER + "## [0.14.0] - 2026-09-06\n\n### Added\n\n- Kept line\n\n### Performance\n\n- Fast\n\n\n"
        rewritten, _added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Fixed", "- Correct")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual(
            rewritten,
            HEADER
            + "## [0.14.0] - 2026-09-06\n\n### Added\n\n- Kept line\n\n"
            "### Fixed\n\n- Correct\n\n### Performance\n\n- Fast\n\n\n",
        )

    def test_the_heading_moves_to_the_version_that_carries_the_range(self) -> None:
        text = HEADER + "## [0.13.1] - 2026-09-06\n\n\n## [0.13.0] - 2026-09-01\n\n### Added\n\n- Old\n\n\n"
        rewritten, added, renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- New")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertIn("## [0.14.0] - 2026-09-06\n\n### Added\n\n- New\n\n\n", rewritten)
        self.assertNotIn("0.13.1", rewritten)
        self.assertTrue(renamed)
        self.assertEqual(len(added), 1)

    def test_an_empty_section_still_moves_to_the_new_version(self) -> None:
        text = HEADER + "## [0.13.1] - 2026-09-06\n\n\n## [0.13.0] - 2026-09-01\n"
        rewritten, added, renamed = fix.rewrite_changelog(
            text, [], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual(renamed, True)
        self.assertEqual(added, [])
        self.assertIn("## [0.14.0] - 2026-09-06\n\n\n## [0.13.0]", rewritten)

    def test_a_section_is_written_when_release_plz_wrote_none(self) -> None:
        text = HEADER + "## [Unreleased]\n\n## [0.13.0] - 2026-09-01\n\n### Added\n\n- Old\n\n\n"
        rewritten, added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Fixed", "- Correct")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual(
            rewritten,
            HEADER
            + "## [Unreleased]\n\n## [0.14.0] - 2026-09-06\n\n### Fixed\n\n- Correct\n\n\n"
            "## [0.13.0] - 2026-09-01\n\n### Added\n\n- Old\n\n\n",
        )
        self.assertEqual(len(added), 1)

    def test_a_line_matching_an_older_release_still_lands_in_the_new_section(self) -> None:
        """A change that was taken out and landed again generates the same line
        as the one that was removed. It is a real change in this release, so the
        guard writes it rather than reading the older heading as an answer."""
        text = HEADER + "## [0.13.1] - 2026-09-06\n\n\n## [0.13.0] - 2026-09-01\n\n### Added\n\n- Old\n\n\n"
        rewritten, added, renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- Old")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual([entry.line for entry in added], ["- Old"])
        self.assertTrue(renamed)
        self.assertIn("## [0.14.0] - 2026-09-06\n\n### Added\n\n- Old\n\n\n## [0.13.0]", rewritten)
        self.assertEqual(rewritten.count("- Old"), 2)

    def test_a_line_the_new_section_already_carries_is_not_written_twice(self) -> None:
        text = HEADER + "## [0.14.0] - 2026-09-06\n\n### Added\n\n- Kept line\n\n\n"
        rewritten, added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- Kept line")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertEqual(added, [])
        self.assertEqual(rewritten, text)

    def test_a_squash_commit_and_the_commit_it_squashed_add_one_line(self) -> None:
        text = HEADER + "## [0.14.0] - 2026-09-06\n\n\n"
        _rewritten, added, _renamed = fix.rewrite_changelog(
            text,
            [self.entry("Added", "- Remove an entry"), self.entry("Added", "- Remove an entry")],
            "0.14.0",
            (0, 13, 0),
            "2026-09-06",
        )
        self.assertEqual(len(added), 1)

    def test_a_hand_written_section_body_is_inserted_into_not_rewritten(self) -> None:
        text = HEADER + "## [0.14.0] - 2026-09-06\n\n### Added\n\n- Kept line\n  with its continuation\n\n\n"
        rewritten, _added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- New")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertIn("- Kept line\n  with its continuation\n- New\n", rewritten)

    def test_entries_with_no_group_heading_above_them_are_kept(self) -> None:
        """A body of bare entries parses as having no groups, so writing the
        section back from its parsed groups would drop them. Such a body is not
        the template's shape and is only ever inserted into."""
        text = HEADER + "## [0.14.0] - 2026-09-06\n\n- A line with no group above it\n\n\n"
        rewritten, added, _renamed = fix.rewrite_changelog(
            text, [self.entry("Added", "- New")], "0.14.0", (0, 13, 0), "2026-09-06"
        )
        self.assertIn("- A line with no group above it\n", rewritten)
        self.assertIn("### Added\n\n- New\n", rewritten)
        self.assertEqual(len(added), 1)


class TestManifestRewrite(unittest.TestCase):
    MEMBERS = {"shep", "shep-core", "shep-daemon", "shep-cli"}

    def test_both_manifest_places_move_and_a_third_party_pin_does_not(self) -> None:
        text = (
            "[workspace.package]\n"
            'version = "0.13.1"\n'
            "edition = \"2024\"\n"
            'rust-version = "1.88"\n'
            "\n"
            "[workspace.dependencies]\n"
            'shep-core = { path = "crates/shep-core", version = "0.13.1" }\n'
            'lookalike = { version = "0.13.1", default-features = false }\n'
        )
        rewritten = fix.rewrite_manifest(text, "0.13.1", "0.14.0", self.MEMBERS)
        self.assertEqual(
            rewritten,
            "[workspace.package]\n"
            'version = "0.14.0"\n'
            "edition = \"2024\"\n"
            'rust-version = "1.88"\n'
            "\n"
            "[workspace.dependencies]\n"
            'shep-core = { path = "crates/shep-core", version = "0.14.0" }\n'
            'lookalike = { version = "0.13.1", default-features = false }\n',
        )

    def test_the_lockfile_moves_for_members_only(self) -> None:
        text = (
            "version 4\n\n"
            "[[package]]\n"
            'name = "lookalike"\n'
            'version = "0.13.1"\n'
            "\n"
            "[[package]]\n"
            'name = "shep-cli"\n'
            'version = "0.13.1"\n'
        )
        rewritten = fix.rewrite_lock(text, "0.13.1", "0.14.0", self.MEMBERS)
        self.assertIn('name = "lookalike"\nversion = "0.13.1"', rewritten)
        self.assertIn('name = "shep-cli"\nversion = "0.14.0"', rewritten)

    def test_spacing_and_bracket_whitespace_are_tolerated(self) -> None:
        """tomllib reads these as the same tables and keys the guard is asked to
        move, so the line rewriter has to read them the same way rather than
        only the shape `cargo` writes."""
        text = (
            "[ workspace.package ]\n"
            'version="0.13.1"\n'
            "\n"
            "[workspace.dependencies]\n"
            'shep-core={path="crates/shep-core",version="0.13.1"}\n'
        )
        rewritten = fix.rewrite_manifest(text, "0.13.1", "0.14.0", self.MEMBERS)
        self.assertEqual(fix.manifest_version_text(rewritten), "0.14.0")
        self.assertIn('shep-core={path="crates/shep-core",version = "0.14.0"}', rewritten)

    def test_a_version_key_this_script_cannot_move_is_left_for_the_refusal(self) -> None:
        """A quoted key is valid TOML and tomllib reads it as `version`, while
        this script's line matcher does not. The rewrite leaves it, and the check
        that the branch carries the version it claims notices."""
        text = '[workspace.package]\n"version" = "0.13.1"\n'
        rewritten = fix.rewrite_manifest(text, "0.13.1", "0.14.0", self.MEMBERS)
        self.assertEqual(rewritten, text)
        self.assertEqual(fix.manifest_version_text(rewritten), "0.13.1")

    def test_lock_versions_reports_members_only(self) -> None:
        text = (
            "[[package]]\n"
            'name = "lookalike"\n'
            'version = "0.13.1"\n'
            "\n"
            "[[package]]\n"
            'name = "shep"\n'
            'version = "0.13.1"\n'
        )
        self.assertEqual(fix.lock_versions(text, self.MEMBERS), {"shep": "0.13.1"})

    def test_dependency_versions_reads_the_literals_cargo_publish_keeps(self) -> None:
        text = (
            "[workspace.dependencies]\n"
            'shep-core = { path = "crates/shep-core", version = "0.13.1" }\n'
            'lookalike = { version = "0.13.1", default-features = false }\n'
        )
        self.assertEqual(
            fix.dependency_versions(text), {"shep-core": "0.13.1", "lookalike": "0.13.1"}
        )


class TestReleaseBranchRewrite(Fixture):
    def test_the_branch_comes_out_at_the_version_that_carries_the_diamond(self) -> None:
        output = self.tmp / "output.txt"
        run = self.run_guard(output)
        self.assertEqual(run.returncode, 0, run.stderr)
        results = self.results(output)
        self.assertEqual(results["new_version"], "0.14.0")
        self.assertEqual(results["old_version"], "0.13.1")
        self.assertEqual(results["changed"], "true")

        manifest = self.remote_file(RELEASE_BRANCH, "Cargo.toml")
        self.assertIn('version = "0.14.0"', manifest)
        self.assertIn('shep-core = { path = "crates/shep-core", version = "0.14.0" }', manifest)
        self.assertIn('lookalike = { version = "0.13.0", default-features = false }', manifest)

        lock = self.remote_file(RELEASE_BRANCH, "Cargo.lock")
        self.assertIn('name = "shep"\nversion = "0.14.0"', lock)
        self.assertIn('name = "shep-cli"\nversion = "0.14.0"', lock)
        self.assertIn('name = "lookalike"\nversion = "0.13.0"', lock)

    def test_every_commit_the_walk_missed_reaches_the_right_changelog(self) -> None:
        output = self.tmp / "output.txt"
        self.assertEqual(self.run_guard(output).returncode, 0)

        daemon = self.remote_file(RELEASE_BRANCH, "crates/shep-daemon/CHANGELOG.md")
        cli = self.remote_file(RELEASE_BRANCH, "crates/shep-cli/CHANGELOG.md")
        core = self.remote_file(RELEASE_BRANCH, "crates/shep-core/CHANGELOG.md")

        self.assertIn("- List every known dog at rest **(BREAKING)**", daemon)
        self.assertIn("- Re-fire config.dog.<name> when secrets.json changes a value a dog names", daemon)
        self.assertIn("- Remove an entry from an array or map of tables", cli)
        self.assertNotIn("List every known dog", cli)
        self.assertNotIn("Remove an entry", daemon)

        # The bump is the workspace's, so a package with nothing to say still
        # gets the heading the release actually carries.
        self.assertIn("## [0.14.0] - 2026-09-06", core)
        self.assertNotIn("0.13.1", daemon + cli + core)

    def test_running_the_guard_again_changes_nothing(self) -> None:
        output = self.tmp / "first.txt"
        self.assertEqual(self.run_guard(output).returncode, 0)
        after_first = self.remote_head(RELEASE_BRANCH)

        second = self.tmp / "second.txt"
        run = self.run_guard(second)
        self.assertEqual(run.returncode, 0, run.stderr)
        results = self.results(second)
        self.assertEqual(results["changed"], "false")
        self.assertEqual(results["new_version"], "0.14.0")
        self.assertEqual(self.remote_head(RELEASE_BRANCH), after_first)
        self.assertIn("already accounts for every commit", run.stdout)

    def test_main_is_left_alone(self) -> None:
        before = self.remote_head("main")
        self.assertEqual(self.run_guard(self.tmp / "output.txt").returncode, 0)
        self.assertEqual(self.remote_head("main"), before)
        self.assertEqual(self.remote_file("main", "Cargo.toml").count('version = "0.13.0"'), 4)


class TestIndexVersion(Fixture):
    """The release guard also writes the new version into the dog index.

    release-plz leaves `web/public/dogs.json` alone, so on every release branch
    the index names the previous shep until the guard moves it, in the commit
    and the push it already makes.
    """

    def index_on_branch(self) -> dict[str, object]:
        return json.loads(self.remote_file(RELEASE_BRANCH, INDEX_PATH))

    def test_the_index_comes_out_naming_the_version_the_branch_carries(self) -> None:
        output = self.tmp / "output.txt"
        run = self.run_guard(output)
        self.assertEqual(run.returncode, 0, run.stderr)

        index = self.index_on_branch()
        self.assertEqual(index["shep"], self.results(output)["new_version"])
        self.assertEqual(index["shep"], "0.14.0")
        keys = list(index)
        self.assertEqual(keys[keys.index("version") + 1], "shep")

    def test_an_index_with_no_shep_gains_one_after_version(self) -> None:
        self.amend_release_branch(INDEX_PATH, index_text(None))
        self.assertEqual(self.run_guard(self.tmp / "output.txt").returncode, 0)

        keys = list(self.index_on_branch())
        self.assertEqual(keys, ["$schema", "version", "shep", "dogs"])

    def test_an_index_already_naming_it_is_left_alone(self) -> None:
        """Compared by what it says, not by how it is laid out: a file that
        already names the version is not reformatted."""
        compact = '{"$schema":"./dogs.schema.json","version":1,"shep":"0.14.0","dogs":[]}\n'
        self.amend_release_branch(INDEX_PATH, compact)
        run = self.run_guard(self.tmp / "output.txt")
        self.assertEqual(run.returncode, 0, run.stderr)

        self.assertEqual(self.remote_file(RELEASE_BRANCH, INDEX_PATH), compact)

    def test_only_the_shep_line_of_the_index_changes(self) -> None:
        before = self.remote_file(RELEASE_BRANCH, INDEX_PATH).splitlines()
        self.assertEqual(self.run_guard(self.tmp / "output.txt").returncode, 0)
        after = self.remote_file(RELEASE_BRANCH, INDEX_PATH).splitlines()

        self.assertEqual(len(before), len(after))
        changed = [(old, new) for old, new in zip(before, after) if old != new]
        self.assertEqual(changed, [('  "shep": "0.13.0",', '  "shep": "0.14.0",')])
        self.assertIn("files \u2014 and", self.remote_file(RELEASE_BRANCH, INDEX_PATH))
        self.assertTrue(self.remote_file(RELEASE_BRANCH, INDEX_PATH).endswith("}\n"))

    def test_a_branch_whose_only_gap_is_the_index_is_still_rewritten(self) -> None:
        self.assertEqual(self.run_guard(self.tmp / "first.txt").returncode, 0)
        # Version, manifests and changelogs are right now. Put the index back
        # the way release-plz leaves it, on top of what the guard pushed.
        self.git("fetch", "-q", "origin", f"+refs/heads/{RELEASE_BRANCH}")
        self.git("branch", "-f", RELEASE_BRANCH, "FETCH_HEAD")
        self.amend_release_branch(INDEX_PATH, index_text("0.13.1"))
        stale_head = self.release_head

        second = self.tmp / "second.txt"
        run = self.run_guard(second)
        self.assertEqual(run.returncode, 0, run.stderr)
        results = self.results(second)
        self.assertEqual(results["changed"], "true")
        self.assertEqual(results["new_version"], "0.14.0")
        self.assertNotEqual(self.remote_head(RELEASE_BRANCH), stale_head)
        self.assertEqual(results["release_head"], self.remote_head(RELEASE_BRANCH))
        self.assertEqual(self.index_on_branch()["shep"], "0.14.0")

    def test_a_branch_with_no_index_is_refused(self) -> None:
        self.git("checkout", "-q", RELEASE_BRANCH)
        self.git("rm", "-q", INDEX_PATH)
        self.commit("chore: lose the index", date="2026-09-06T02:00:00")
        self.git("push", "-q", "origin", f"HEAD:refs/heads/{RELEASE_BRANCH}")
        self.git("checkout", "-q", "main")
        before = self.remote_head(RELEASE_BRANCH)

        run = self.run_guard(self.tmp / "output.txt")
        self.assertEqual(run.returncode, 1, run.stdout)
        self.assertIn(INDEX_PATH, run.stderr)
        self.assertEqual(self.remote_head(RELEASE_BRANCH), before)

    def test_an_index_that_is_not_a_json_object_is_refused(self) -> None:
        for text in ("not json\n", "[]\n"):
            self.amend_release_branch(INDEX_PATH, text)
            before = self.remote_head(RELEASE_BRANCH)

            run = self.run_guard(self.tmp / "output.txt")
            self.assertEqual(run.returncode, 1, text)
            self.assertIn(INDEX_PATH, run.stderr)
            self.assertEqual(self.remote_head(RELEASE_BRANCH), before)

    def test_running_it_again_changes_nothing(self) -> None:
        self.assertEqual(self.run_guard(self.tmp / "first.txt").returncode, 0)
        after_first = self.remote_head(RELEASE_BRANCH)
        index = self.remote_file(RELEASE_BRANCH, INDEX_PATH)

        second = self.tmp / "second.txt"
        run = self.run_guard(second)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(self.results(second)["changed"], "false")
        self.assertEqual(self.remote_head(RELEASE_BRANCH), after_first)
        self.assertEqual(self.remote_file(RELEASE_BRANCH, INDEX_PATH), index)


class TestFeatureReleaseStands(Fixture):
    """The same diamond with the `!` taken out of the far leg.

    release-plz proposed 0.13.1 for it and 0.13.1 is right: a pre-1.0 feature
    is a patch. A guard that called a feature a minor would move this branch to
    0.14.0 on every push to main.
    """

    breaks = False

    def test_a_range_of_features_leaves_the_patch_release_plz_proposed(self) -> None:
        output = self.tmp / "output.txt"
        run = self.run_guard(output)
        self.assertEqual(run.returncode, 0, run.stderr)
        results = self.results(output)
        self.assertEqual(results["new_version"], "0.13.1")
        self.assertEqual(results["old_version"], "0.13.1")
        self.assertIn("asks for no larger bump", run.stdout)

        manifest = self.remote_file(RELEASE_BRANCH, "Cargo.toml")
        self.assertIn('[workspace.package]\nversion = "0.13.1"', manifest)
        self.assertNotIn("0.14.0", manifest)
        lock = self.remote_file(RELEASE_BRANCH, "Cargo.lock")
        self.assertIn('name = "shep"\nversion = "0.13.1"', lock)
        self.assertNotIn("0.14.0", lock)

    def test_a_range_of_features_still_completes_the_changelog(self) -> None:
        output = self.tmp / "output.txt"
        self.assertEqual(self.run_guard(output).returncode, 0)

        daemon = self.remote_file(RELEASE_BRANCH, "crates/shep-daemon/CHANGELOG.md")
        cli = self.remote_file(RELEASE_BRANCH, "crates/shep-cli/CHANGELOG.md")
        self.assertIn("- List every known dog at rest", daemon)
        self.assertNotIn("**(BREAKING)**", daemon)
        self.assertIn("- Remove an entry from an array or map of tables", cli)
        self.assertIn("## [0.13.1] - 2026-09-06", daemon)
        self.assertNotIn("0.14.0", daemon + cli)

    def test_a_corrected_feature_branch_is_left_alone(self) -> None:
        output = self.tmp / "first.txt"
        self.assertEqual(self.run_guard(output).returncode, 0)
        after_first = self.remote_head(RELEASE_BRANCH)

        second = self.tmp / "second.txt"
        run = self.run_guard(second)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(self.results(second)["changed"], "false")
        self.assertEqual(self.remote_head(RELEASE_BRANCH), after_first)

    def test_release_plz_toml_leaves_a_feature_at_a_patch(self) -> None:
        """The option the bump rule reads, read off the real config file."""
        repo = fix.Repo(self.repo)
        flags = fix.feature_flags(repo, fix.release_packages(repo))
        self.assertTrue(flags)
        self.assertFalse(any(flags.values()))


WORKSPACE_TRUE = "[workspace]\nfeatures_always_increment_minor = true\n\n"
SHEP_CLI_HELD = '[[package]]\nname = "shep-cli"\nrelease = false\n'
DAEMON_FALSE = '[[package]]\nname = "shep-daemon"\nfeatures_always_increment_minor = false\n'
SHEP_FALSE = '[[package]]\nname = "shep"\nfeatures_always_increment_minor = false\n'


class TestFeatureFlagOverrides(Fixture):
    """`features_always_increment_minor` read per package, the package over the workspace.

    The fixture's range carries features into crates/shep-daemon and
    crates/shep-cli, the crate named `shep`, and carries no break.
    """

    breaks = False

    def configure(self, text: str) -> None:
        """Put a release-plz.toml in front of the guard, as main carries it."""
        self.write("release-plz.toml", text)

    def flags(self) -> dict[str, bool]:
        repo = fix.Repo(self.repo)
        return fix.feature_flags(repo, fix.release_packages(repo))

    def run_with(self, text: str) -> dict[str, str]:
        self.configure(text)
        output = self.tmp / "output.txt"
        run = self.run_guard(output)
        self.assertEqual(run.returncode, 0, run.stderr)
        return self.results(output)

    def test_a_workspace_true_reaches_a_package_that_names_nothing(self) -> None:
        self.configure(WORKSPACE_TRUE + SHEP_CLI_HELD)
        self.assertEqual(
            self.flags(), {"shep": True, "shep-core": True, "shep-daemon": True}
        )

    def test_a_package_false_overrides_a_workspace_true(self) -> None:
        self.configure(WORKSPACE_TRUE + DAEMON_FALSE + SHEP_CLI_HELD)
        self.assertEqual(
            self.flags(), {"shep": True, "shep-core": True, "shep-daemon": False}
        )

    def test_a_workspace_that_names_nothing_leaves_every_package_at_false(self) -> None:
        self.configure(SHEP_CLI_HELD)
        self.assertEqual(
            self.flags(), {"shep": False, "shep-core": False, "shep-daemon": False}
        )

    def test_a_workspace_true_moves_a_feature_release_to_the_minor(self) -> None:
        self.assertEqual(self.run_with(WORKSPACE_TRUE + SHEP_CLI_HELD)["new_version"], "0.14.0")

    def test_every_feature_package_overriding_a_workspace_true_holds_the_patch(self) -> None:
        results = self.run_with(WORKSPACE_TRUE + DAEMON_FALSE + SHEP_FALSE + SHEP_CLI_HELD)
        self.assertEqual(results["new_version"], "0.13.1")
        self.assertNotIn("0.14.0", self.remote_file(RELEASE_BRANCH, "Cargo.toml"))

    def test_one_true_package_carries_the_release_a_feature_reached(self) -> None:
        results = self.run_with(WORKSPACE_TRUE + DAEMON_FALSE + SHEP_CLI_HELD)
        self.assertEqual(results["new_version"], "0.14.0")


class TestFeatureFlagChoice(unittest.TestCase):
    """Which package decides, once the flags and the feats are known."""

    PACKAGES = [
        fix.Package(name="shep-core", directory="crates/shep-core", changelog="c"),
        fix.Package(name="shep-daemon", directory="crates/shep-daemon", changelog="c"),
    ]

    def test_a_feature_that_reached_only_a_false_package_holds_the_patch(self) -> None:
        flags = {"shep-core": True, "shep-daemon": False}
        self.assertFalse(
            fix.features_move_the_minor(self.PACKAGES, flags, {"shep-daemon"})
        )

    def test_a_feature_that_reached_a_true_package_moves_the_minor(self) -> None:
        flags = {"shep-core": True, "shep-daemon": False}
        self.assertTrue(
            fix.features_move_the_minor(self.PACKAGES, flags, {"shep-daemon", "shep-core"})
        )

    def test_a_true_package_no_feature_reached_decides_nothing(self) -> None:
        flags = {"shep-core": True, "shep-daemon": False}
        self.assertFalse(fix.features_move_the_minor(self.PACKAGES, flags, set()))


class TestRefusals(Fixture):
    def run_against(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPTS / "fix-release-bump.py"),
                "--repo",
                str(self.repo),
                *extra,
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_a_range_with_no_tag_reachable_refuses(self) -> None:
        self.git("tag", "-d", "shep-v0.13.0")
        run = self.run_against("--base-sha", self.main_head, "--head-sha", self.main_head)
        self.assertEqual(run.returncode, 1)
        self.assertIn("No shep-v[0-9]* tag is reachable", run.stderr)

    def test_a_range_named_by_hand_needs_no_tag(self) -> None:
        self.git("tag", "-d", "shep-v0.13.0")
        run = self.run_against(
            "--base-sha",
            self.main_head,
            "--head-sha",
            self.main_head,
            "--since",
            self.base_sha,
            "--dry-run",
        )
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("0.14.0 carries the range", run.stdout)

    def test_a_branch_that_needs_nothing_reports_no_change(self) -> None:
        # main carries the same version the release branch does, so the only
        # thing to report is what the changelogs are missing.
        run = self.run_against(
            "--base-sha", self.main_head, "--head-sha", self.main_head, "--dry-run"
        )
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("0.14.0 carries the range", run.stdout)

    def test_a_dependency_literal_the_rewriter_cannot_move_refuses(self) -> None:
        """A quoted key is valid TOML, so cargo keeps the literal at the old
        version and publishes a crate that wants a member this release does not
        contain. The guard refuses rather than bumping around it."""
        manifest = self.remote_file(RELEASE_BRANCH, "Cargo.toml").replace(
            'shep-core = { path = "crates/shep-core", version = "0.13.1" }',
            '"shep-core" = { path = "crates/shep-core", version = "0.13.1" }',
        )
        self.amend_release_branch("Cargo.toml", manifest)
        run = self.run_against(
            "--base-sha", self.main_head, "--branch", RELEASE_BRANCH, "--remote", "origin"
        )
        self.assertEqual(run.returncode, 1, run.stdout)
        self.assertIn("[workspace.dependencies]", run.stderr)
        self.assertEqual(self.remote_head(RELEASE_BRANCH), self.release_head)

    def test_a_manifest_the_rewriter_cannot_move_refuses(self) -> None:
        """A branch titled one version and built at another is the mistake this
        guard exists to catch, so the guard refuses rather than reporting a
        version it did not write."""
        manifest = self.remote_file(RELEASE_BRANCH, "Cargo.toml").replace(
            '[workspace.package]\nversion = "0.13.1"',
            '[workspace.package]\n"version" = "0.13.1"',
        )
        self.amend_release_branch("Cargo.toml", manifest)
        run = self.run_against(
            "--base-sha",
            self.main_head,
            "--branch",
            RELEASE_BRANCH,
            "--remote",
            "origin",
        )
        self.assertEqual(run.returncode, 1, run.stdout)
        self.assertIn("cannot move", run.stderr)
        self.assertEqual(self.remote_head(RELEASE_BRANCH), self.release_head)


if __name__ == "__main__":
    unittest.main()
