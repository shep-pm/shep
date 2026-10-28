"""Tests for scripts/check-doc-length.py.

Run them with:

    python3 -m unittest discover --start-directory scripts

The limits are the whole contract, so most of these build a comment block in
memory and ask only what the checker says about it: no repository file is
read or written. The baseline rules get the same treatment, against a baseline
file in a temporary directory. The last class is the one that reads this
repository: it runs the check over every tracked `.rs` file against the
committed baseline, which is what the `doc length` CI job runs.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent

# The script has a hyphen in its name, so it loads by file location the way
# scripts/test_audit_changelog_gaps.py loads the changelog audit. `dataclass`
# looks its class's module up in sys.modules, so the module has to be registered
# before its body runs.
_spec = importlib.util.spec_from_file_location("check_doc_length", SCRIPTS / "check-doc-length.py")
check_doc_length = importlib.util.module_from_spec(_spec)
sys.modules["check_doc_length"] = check_doc_length
_spec.loader.exec_module(check_doc_length)

FAKE = "crates/fake/src/lib.rs"


def doc_fixture(body: int, *, errors: bool = False) -> list[str]:
    """A `///` block whose body is `body` lines long, `# Errors` heading included."""
    lines = ["/// Summary."]
    left = body
    if errors:
        lines.append("/// # Errors")
        left -= 1
    lines += [f"/// Body line {n}." for n in range(1, left + 1)]
    return lines


def plain_fixture(lines: int) -> list[str]:
    return [f"// Line {n}." for n in range(1, lines + 1)]


def module_fixture(lines: int) -> list[str]:
    return [f"//! Line {n}." for n in range(1, lines + 1)]


def one_block(*lines: str) -> list[check_doc_length.Block]:
    """One block, parsed from `lines`, as the checker would see it in FAKE."""
    return check_doc_length.blocks_in(FAKE, "\n".join(lines) + "\n")


def only(*lines: str) -> check_doc_length.Block:
    blocks = one_block(*lines)
    assert len(blocks) == 1, blocks
    return blocks[0]


class DocCommentLimit(unittest.TestCase):
    """`///`: at most 6 body lines after the summary, 12 counting `# Errors`."""

    def test_summary_alone_is_free(self):
        block = only("/// Summary.")
        self.assertEqual(block.measured, 0)
        self.assertFalse(block.over)

    def test_six_body_lines_fit(self):
        blocks = one_block(*doc_fixture(6))
        self.assertEqual(len(blocks), 1)
        self.assertEqual(blocks[0].measured, 6)
        self.assertFalse(blocks[0].over)

    def test_a_block_opening_on_a_heading_has_no_summary_to_forgive(self):
        block = only("/// # Safety", *doc_fixture(6)[1:])
        self.assertEqual(block.measured, 7)
        self.assertTrue(block.over)

    def test_a_block_opening_on_a_fence_has_no_summary_to_forgive(self):
        block = only("/// ```", "/// let x = 1;", "/// ```", *doc_fixture(7)[1:])
        self.assertEqual(block.measured, 7)
        self.assertTrue(block.over)

    def test_a_block_opening_on_a_bare_marker_has_no_summary_to_forgive(self):
        block = only("///", *doc_fixture(6)[1:])
        self.assertEqual(block.measured, 7)
        self.assertTrue(block.over)

    def test_seven_body_lines_break(self):
        blocks = one_block(*doc_fixture(7))
        self.assertEqual(blocks[0].measured, 7)
        self.assertTrue(blocks[0].over)

    def test_blank_separator_line_counts(self):
        # IR-47 counts lines, and `///` on its own is one. A block that spends
        # its budget on paragraph breaks is still out of budget.
        blocks = one_block("/// Summary.", "///", *doc_fixture(7)[1:])
        self.assertEqual(blocks[0].measured, 8)
        self.assertTrue(blocks[0].over)

    def test_errors_section_raises_the_limit_to_twelve(self):
        blocks = one_block(*doc_fixture(12, errors=True))
        self.assertTrue(blocks[0].has_errors)
        self.assertEqual(blocks[0].budget, 12)
        self.assertEqual(blocks[0].measured, 12)
        self.assertFalse(blocks[0].over)

    def test_thirteen_body_lines_break_even_with_errors(self):
        blocks = one_block(*doc_fixture(13, errors=True))
        self.assertEqual(blocks[0].measured, 13)
        self.assertTrue(blocks[0].over)

    def test_panics_section_keeps_the_limit_at_six(self):
        blocks = one_block("/// Summary.", "/// # Panics", "/// Never.")
        self.assertFalse(blocks[0].has_errors)
        self.assertEqual(blocks[0].budget, 6)

    def test_a_second_heading_ends_the_errors_section_without_raising_the_limit_again(self):
        blocks = one_block("/// Summary.", "/// # Errors", "/// Nothing.", "/// # Panics", "/// Never.")
        self.assertTrue(blocks[0].has_errors)
        self.assertEqual(blocks[0].measured, 4)


class ModuleDocLimit(unittest.TestCase):
    """`//!`: at most 10 lines."""

    def test_ten_lines_fit(self):
        blocks = one_block(*module_fixture(10))
        self.assertEqual(blocks[0].measured, 10)
        self.assertFalse(blocks[0].over)

    def test_eleven_lines_break(self):
        blocks = one_block(*module_fixture(11))
        self.assertEqual(blocks[0].measured, 11)
        self.assertTrue(blocks[0].over)

    def test_a_module_doc_is_not_read_as_a_line_comment(self):
        blocks = one_block("//! Line 1.")
        self.assertEqual(blocks[0].kind, "//!")


class LineCommentLimit(unittest.TestCase):
    """`//`: at most 4 lines in a row."""

    def test_four_lines_fit(self):
        blocks = one_block(*plain_fixture(4))
        self.assertEqual(blocks[0].measured, 4)
        self.assertFalse(blocks[0].over)

    def test_five_lines_break(self):
        blocks = one_block(*plain_fixture(5))
        self.assertEqual(blocks[0].measured, 5)
        self.assertTrue(blocks[0].over)

    def test_code_between_two_blocks_keeps_them_apart(self):
        blocks = one_block(*plain_fixture(4), "let x = 1;", *plain_fixture(4))
        self.assertEqual(len(blocks), 2)
        self.assertFalse(any(block.over for block in blocks))

    def test_a_blank_line_keeps_blocks_apart(self):
        blocks = one_block(*plain_fixture(4), "", *plain_fixture(4))
        self.assertEqual(len(blocks), 2)
        self.assertFalse(any(block.over for block in blocks))

    def test_a_doc_block_and_the_line_comment_below_it_are_separate(self):
        blocks = one_block("/// Summary.", "// Line 1.", "// Line 2.", "// Line 3.", "// Line 4.", "// Line 5.")
        self.assertEqual([block.kind for block in blocks], ["///", "//"])
        self.assertTrue(blocks[1].over)


class LinesThatNeverCount(unittest.TestCase):
    """Fenced code and `# Example` sections are the doc doing its job."""

    def test_fenced_code_does_not_count(self):
        block = only(
            "/// Summary.",
            "/// ```text",
            "/// shep start app",
            "/// shep list",
            "/// ```",
            "/// Body line 1.",
        )
        self.assertEqual(block.measured, 1)
        self.assertFalse(block.over)

    def test_an_unclosed_fence_still_hides_the_rest_of_the_block(self):
        block = only("/// Summary.", "/// ```text", *doc_fixture(9)[1:])
        self.assertEqual(block.measured, 0)

    def test_example_section_does_not_count(self):
        block = only(
            "/// Summary.",
            "/// Body line 1.",
            "/// # Example",
            "/// ```",
            "/// let dog = Dog::new();",
            "/// ```",
            "/// prose inside the example section",
            "/// # Panics",
            "/// Never.",
        )
        self.assertEqual(block.measured, 3)
        self.assertFalse(block.over)

    def test_examples_spelling_counts_as_an_example_section(self):
        block = only("/// Summary.", "/// # Examples", "/// ```", "/// code", "/// ```")
        self.assertEqual(block.measured, 0)

    def test_an_example_section_ends_at_the_next_heading(self):
        blocks = one_block(
            "/// Summary.",
            "/// # Example",
            "/// prose inside the example",
            "/// # Errors",
            "/// Never fails.",
        )
        self.assertEqual(blocks[0].measured, 2)
        self.assertTrue(blocks[0].has_errors)

    def test_a_plain_comment_carries_a_fence_too(self):
        blocks = one_block(
            "// Line 1.",
            "// ```text",
            "// shep start app",
            "// shep list",
            "// ```",
            "// Line 2.",
        )
        self.assertEqual(blocks[0].measured, 2)


class Reporting(unittest.TestCase):
    """A failure names the file and the block's first line."""

    def test_a_seven_line_doc_body_is_reported_with_its_file_and_line(self):
        text = "\n".join(["use std::fmt;", *doc_fixture(7)]) + "\n"
        blocks = check_doc_length.blocks_in(FAKE, text)
        self.assertTrue(blocks[0].over)
        self.assertEqual(blocks[0].line, 2)
        self.assertIn(f"{FAKE}:2", blocks[0].describe())
        self.assertIn("7 body lines (IR-47 allows 6)", blocks[0].describe())

    def test_a_five_line_plain_comment_is_reported_with_its_file_and_line(self):
        text = "\n".join(["let x = 1;", *plain_fixture(5)]) + "\n"
        blocks = check_doc_length.blocks_in(FAKE, text)
        self.assertTrue(blocks[0].over)
        self.assertEqual(blocks[0].line, 2)
        self.assertIn(f"{FAKE}:2", blocks[0].describe())
        self.assertIn("5 lines (IR-47 allows 4)", blocks[0].describe())


class BaselineRules(unittest.TestCase):
    """A listed block may shrink, a new one may not appear, a stale entry must go."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.baseline = Path(self.tmp.name) / "doc-length-baseline.txt"

    def write_baseline(self, *entries: str) -> None:
        self.baseline.write_text("\n".join(entries) + "\n")

    def check(self, *lines: str, narrowed: bool = True) -> list[str]:
        blocks = check_doc_length.blocks_in(FAKE, "\n".join(lines) + "\n")
        failures, _, _ = check_doc_length.check(blocks, self.baseline, narrowed)
        return failures

    def test_a_block_within_its_limit_needs_no_entry(self):
        self.assertEqual(self.check(*doc_fixture(6)), [])

    def test_a_new_block_over_its_limit_fails_against_an_empty_baseline(self):
        self.write_baseline("# nothing here")
        failures = self.check(*doc_fixture(7))
        self.assertEqual(len(failures), 1)
        self.assertIn(f"{FAKE}:1", failures[0])
        self.assertIn("7 body lines", failures[0])
        self.assertIn("is not in the baseline", failures[0])

    def test_a_baselined_block_may_shrink(self):
        self.write_baseline("9 crates/fake/src/lib.rs /// Summary.")
        self.assertEqual(self.check(*doc_fixture(7)), [])

    def test_a_baselined_block_may_not_grow_past_its_recorded_size(self):
        self.write_baseline("7 crates/fake/src/lib.rs /// Summary.")
        failures = self.check(*doc_fixture(9))
        self.assertEqual(len(failures), 1)
        self.assertIn("grew past the 7 lines its baseline entry allows", failures[0])

    def test_a_baselined_block_cut_to_fit_fails_until_its_entry_is_removed(self):
        self.write_baseline("9 crates/fake/src/lib.rs /// Summary.")
        self.assertEqual(self.check(*doc_fixture(7)), [])
        failures = self.check(*doc_fixture(5))
        self.assertEqual(len(failures), 1)
        self.assertIn("is stale: no block over its limit matches it", failures[0])

    def test_a_stale_entry_is_reported_at_its_own_line_in_the_baseline(self):
        # The line a maintainer has to delete lives in the baseline file, not in
        # the `.rs` file the entry names, and the message has to say which.
        self.write_baseline(
            "# a note",
            "",
            "9 crates/fake/src/lib.rs /// Summary.",
        )
        failures = self.check(*doc_fixture(5))
        self.assertEqual(len(failures), 1)
        self.assertIn(f"{self.baseline}:3: entry `9 {FAKE} /// Summary.`", failures[0])

    def test_a_grown_block_names_the_baseline_line_that_records_its_size(self):
        self.write_baseline("7 crates/fake/src/lib.rs /// Summary.")
        failures = self.check(*doc_fixture(9))
        self.assertEqual(len(failures), 1)
        self.assertIn(f"grew past the 7 lines its baseline entry allows ({self.baseline}:1)", failures[0])

    def test_an_entry_only_covers_the_file_it_names(self):
        self.write_baseline("9 crates/other/src/lib.rs /// Summary.")
        failures = self.check(*doc_fixture(7))
        self.assertEqual(len(failures), 1)
        self.assertIn("is not in the baseline", failures[0])

    def test_an_entry_for_a_file_this_run_never_reads_is_left_alone(self):
        # Checking one file locally is the point of the positional form, so the
        # other files' entries are not that run's business.
        self.write_baseline("9 crates/other/src/lib.rs /// Summary.")
        self.assertEqual(self.check(*doc_fixture(1)), [])

    def test_a_narrowed_run_reports_an_entry_for_a_file_it_read_that_has_no_comment_blocks(self):
        # A baselined block deleted from a file leaves the file with no blocks
        # at all, and the entry naming it still has to come off the list.
        self.write_baseline("9 crates/fake/src/lib.rs /// Summary.")
        failures, _, _ = check_doc_length.check([], self.baseline, narrowed=True, read={FAKE})
        self.assertEqual(len(failures), 1)
        self.assertIn("is stale: no block over its limit matches it", failures[0])

    def test_a_full_run_reports_an_entry_for_a_file_that_no_longer_exists(self):
        # A whole-repository run reads every tracked file, so an entry naming a
        # file that is gone, renamed or moved out of the tree is stale, and a
        # baseline that quietly keeps such entries stops recording anything.
        self.write_baseline("9 crates/other/src/lib.rs /// Summary.")
        failures = self.check(*doc_fixture(1), narrowed=False)
        self.assertEqual(len(failures), 1)
        self.assertIn("is stale: no block over its limit matches it", failures[0])

    def test_an_entry_only_covers_the_block_its_first_line_names(self):
        self.write_baseline("9 crates/fake/src/lib.rs /// A different block.")
        failures = self.check(*doc_fixture(7))
        self.assertEqual(len(failures), 2)
        self.assertTrue(any("is not in the baseline" in failure for failure in failures))
        self.assertTrue(any("is stale" in failure for failure in failures))

    def test_two_blocks_with_the_same_first_line_are_covered_one_entry_each(self):
        self.write_baseline("7 crates/fake/src/lib.rs /// Summary.")
        failures = self.check(*doc_fixture(7), "let x = 1;", *doc_fixture(7))
        self.assertEqual(len(failures), 1)
        self.assertIn("is not in the baseline", failures[0])

    def test_a_malformed_entry_is_refused(self):
        self.write_baseline("not an entry")
        with self.assertRaises(SystemExit) as raised:
            self.check(*doc_fixture(1))
        self.assertIn("not a baseline entry", str(raised.exception))

    def test_a_missing_baseline_is_read_as_an_empty_one(self):
        failures, _, _ = check_doc_length.check(check_doc_length.blocks_in(FAKE, "\n".join(doc_fixture(7))), self.baseline)
        self.assertEqual(len(failures), 1)


class RegeneratingTheBaseline(unittest.TestCase):
    """`--write-baseline` writes what `check` then accepts."""

    def run_main(self, *args: str) -> int:
        """`main` with its report swallowed: these tests want the exit code."""
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return check_doc_length.main(list(args))

    def test_a_narrowed_run_may_not_rewrite_the_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "long.rs"
            source.write_text("\n".join(doc_fixture(8)) + "\n")
            with self.assertRaises(SystemExit) as raised:
                self.run_main(str(source), "--write-baseline")
            self.assertIn("whole repository", str(raised.exception))

    def test_a_narrowed_main_run_over_a_file_with_no_comments_reports_its_stale_entry(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "bare.rs"
            source.write_text("fn main() {}\n")
            baseline = Path(tmp) / "baseline.txt"
            baseline.write_text(f"9 {source} /// Summary.\n")
            self.assertEqual(self.run_main(str(source), "--baseline", str(baseline)), 1)

    def test_a_written_baseline_round_trips(self):
        blocks = check_doc_length.blocks_in(FAKE, "\n".join(doc_fixture(8)) + "\n")
        with tempfile.TemporaryDirectory() as tmp:
            baseline = Path(tmp) / "baseline.txt"
            check_doc_length.write_baseline(baseline, [block for block in blocks if block.over])
            entry = baseline.read_text().splitlines()[-1]
            self.assertEqual(entry, "8 crates/fake/src/lib.rs /// Summary.")

            failures, _, baselined = check_doc_length.check(blocks, baseline)
            self.assertEqual(failures, [])
            self.assertEqual(baselined, 1)

            grown = check_doc_length.blocks_in(FAKE, "\n".join(doc_fixture(9)) + "\n")
            failures, _, _ = check_doc_length.check(grown, baseline)
            self.assertEqual(len(failures), 1)
            self.assertIn("grew past the 8 lines", failures[0])

            cut = check_doc_length.blocks_in(FAKE, "\n".join(doc_fixture(5)) + "\n")
            failures, _, _ = check_doc_length.check(cut, baseline)
            self.assertEqual(len(failures), 1)
            self.assertIn("is stale", failures[0])

    def test_entries_are_grouped_one_file_per_block(self):
        blocks = [
            *check_doc_length.blocks_in("crates/b.rs", "\n".join(doc_fixture(8)) + "\n"),
            *check_doc_length.blocks_in("crates/a.rs", "\n".join(plain_fixture(6)) + "\n"),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            baseline = Path(tmp) / "baseline.txt"
            check_doc_length.write_baseline(baseline, blocks)
            self.assertEqual(
                baseline.read_text().splitlines()[-3:],
                [
                    "6 crates/a.rs // Line 1.",
                    "",
                    "8 crates/b.rs /// Summary.",
                ],
            )

    def test_the_baseline_option_points_the_check_at_another_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "long.rs"
            text = "\n".join(doc_fixture(8)) + "\n"
            source.write_text(text)
            baseline = Path(tmp) / "baseline.txt"
            blocks = check_doc_length.blocks_in(str(source), text)
            check_doc_length.write_baseline(baseline, [block for block in blocks if block.over])
            self.assertEqual(self.run_main(str(source), "--baseline", str(baseline)), 0)
            self.assertEqual(self.run_main(str(source)), 1)


class ThisRepository(unittest.TestCase):
    """The committed baseline is exactly what main's `.rs` files owe today."""

    def test_main_passes_over_every_tracked_rs_file(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(check_doc_length.main([]), 0)

    def test_the_baseline_lists_the_blocks_over_their_limit(self):
        files = check_doc_length.tracked_rust_files()
        blocks = [b for f in files for b in check_doc_length.blocks_in(f, (REPO_ROOT / f).read_text())]
        entries = check_doc_length.parse_baseline(check_doc_length.BASELINE)
        self.assertEqual(len(entries), sum(1 for b in blocks if b.over))
        self.assertTrue(entries)

    def test_the_baseline_file_is_what_the_check_reads(self):
        self.assertTrue((REPO_ROOT / ".github" / "doc-length-baseline.txt").is_file())


if __name__ == "__main__":
    unittest.main()
