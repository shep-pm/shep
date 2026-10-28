#!/usr/bin/env python3
"""Refuse a comment block longer than IR-47's line limits.

IR-47 says a comment says only what the code cannot: no history, no rejected
alternatives, no paraphrase of the next line. Whether a line earns its place
stays with whoever reads the code; this counts lines only.

Limits, per run of consecutive comment lines of one kind:

    ///   at most 6 body lines after the summary, 12 when the block has an
          `# Errors` section
    //!   at most 10 lines
    //    at most 4 lines in a row

Lines inside a fenced code block and lines of an `# Example` or `# Examples`
section are never counted: both are the doc doing the work a doc is for. A
line that is only its marker (`///` on its own) is still a line, and counts.

Blocks already over their limit are listed in
`.github/doc-length-baseline.txt` by file and first line, the way
`.github/rust-file-size-baseline.txt` lists the files already over 1000 lines.
A listed block may shrink but never past its recorded count, a block not on the
list may not appear, and an entry whose block now fits has to come off the list.
A block main moved elsewhere reads as both a new block and a stale entry, so a
branch measures itself after merging main.

Exits 0 when every block is within its limit or baselined, 1 otherwise.

Usage:

    python3 scripts/check-doc-length.py
    python3 scripts/check-doc-length.py crates/shep-cli/src/output/mod.rs ...
    python3 scripts/check-doc-length.py --baseline .github/doc-length-baseline.txt
    python3 scripts/check-doc-length.py --write-baseline

Positional paths are read from the repository root and narrow the run to the
files named: an entry for any other file is left alone rather than reported as
stale.
"""

from __future__ import annotations

import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / ".github" / "doc-length-baseline.txt"

# IR-47's three shapes, one per comment marker.
DOC_BODY_LINES = 6
DOC_BODY_LINES_WITH_ERRORS = 12
MODULE_LINES = 10
PLAIN_LINES = 4

KINDS = ("//!", "///", "//")


@dataclass
class Block:
    """One run of consecutive comment lines that all carry the same marker."""

    path: str
    line: int
    kind: str
    first_line: str
    counted: int
    has_errors: bool = False

    @property
    def budget(self) -> int:
        if self.kind == "//!":
            return MODULE_LINES
        if self.kind == "//":
            return PLAIN_LINES
        return DOC_BODY_LINES_WITH_ERRORS if self.has_errors else DOC_BODY_LINES

    @property
    def measured(self) -> int:
        """The number the budget is compared against.

        A `///` block spends its budget on its body: the summary line is free,
        since every doc comment has one.
        """
        return self.counted - 1 if self.kind == "///" else self.counted

    @property
    def over(self) -> bool:
        return self.measured > self.budget

    @property
    def key(self) -> tuple[str, str]:
        """What identifies this block in the baseline: its file and first line.

        A line number would move when anything above it is edited, which would
        read as a new block over its limit. The first line's text is what a
        reader or an editor matches on, and a block whose summary is rewritten
        is a different block, deliberately re-baselined or not at all.
        """
        return (self.path, self.first_line)

    def describe(self) -> str:
        noun = {"///": "doc comment", "//!": "module doc", "//": "line comment"}[self.kind]
        part = "body lines" if self.kind == "///" else "lines"
        return f"{self.path}:{self.line}: {noun} has {self.measured} {part} (IR-47 allows {self.budget})"


@dataclass
class BaselineEntry:
    """One baselined block: its recorded line count and its file and first line."""

    recorded: int
    path: str
    first_line: str
    line: int
    source: str

    @property
    def key(self) -> tuple[str, str]:
        return (self.path, self.first_line)

    @property
    def location(self) -> str:
        """Where this entry sits in the baseline file, which is the line to edit."""
        return f"{self.source}:{self.line}"

    def describe(self) -> str:
        return f"{self.location}: entry `{self.recorded} {self.path} {self.first_line}`"


def marker(line: str) -> str | None:
    """The comment marker this line opens with, or None when it opens nothing."""
    stripped = line.strip()
    for kind in KINDS:
        if stripped.startswith(kind):
            return kind
    return None


def is_heading(text: str) -> bool:
    return text.startswith("# ")


def is_example_heading(text: str) -> bool:
    return text.lower() in ("# example", "# examples")


def is_errors_heading(text: str) -> bool:
    return text.lower() == "# errors"


def count_block(body: list[str]) -> tuple[int, bool]:
    """Count the lines a block spends against its limit, and whether it has an `# Errors` section.

    `body` holds each line's text after its marker. A fenced block runs to its
    closing fence or the end of the comment, and an `# Example` section to the
    next heading or the end of the comment. Neither counts, delimiters and
    heading line included.
    """
    counted = 0
    has_errors = False
    fence = False
    example = False

    for raw in body:
        text = raw.strip()
        if text.startswith("```"):
            fence = not fence
            continue
        if fence:
            continue
        if is_heading(text):
            if is_example_heading(text):
                example = True
                continue
            example = False
            if is_errors_heading(text):
                has_errors = True
        if example:
            continue
        counted += 1

    return counted, has_errors


def blocks_in(path: str, text: str) -> list[Block]:
    """Every comment block in one file, in file order."""
    blocks: list[Block] = []
    kind: str | None = None
    start = 0
    first_line = ""
    body: list[str] = []

    def flush() -> None:
        if kind is None:
            return
        counted, has_errors = count_block(body)
        blocks.append(
            Block(
                path=path,
                line=start,
                kind=kind,
                first_line=first_line,
                counted=counted,
                has_errors=has_errors,
            )
        )

    for number, line in enumerate(text.splitlines(), start=1):
        found = marker(line)
        if found is not None and found == kind:
            body.append(line.strip()[len(kind) :])
            continue
        flush()
        kind = found
        if found is not None:
            start = number
            first_line = line.strip()
            body = [line.strip()[len(found) :]]
        else:
            body = []
    flush()
    return blocks


def tracked_rust_files() -> list[str]:
    """Every `.rs` file git tracks, as `git ls-files '*.rs'` lists them."""
    listing = subprocess.run(
        ["git", "ls-files", "*.rs"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return [entry for entry in listing.stdout.splitlines() if entry]


def relative(path: Path) -> str:
    """The path as a reader types it: relative to the repository root when it sits inside it."""
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def parse_baseline(path: Path) -> list[BaselineEntry]:
    """Read the baseline. One block per line: `<lines> <path> <first line>`.

    Blank lines and lines starting with `#` are the file's own prose, the way
    `.github/rust-file-size-baseline.txt` uses them for its issue notes. Every
    entry starts with a number, so no entry reads as prose.
    """
    entries: list[BaselineEntry] = []
    if not path.is_file():
        return entries
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            recorded, rest = line.split(" ", 1)
            file_path, first_line = rest.split(" ", 1)
            recorded_int = int(recorded)
        except ValueError:
            raise SystemExit(f"{path}:{number}: not a baseline entry: {line}")
        entries.append(BaselineEntry(recorded_int, file_path, first_line, number, relative(path)))
    return entries


def write_baseline(path: Path, blocks: list[Block]) -> None:
    """Rewrite the baseline from the blocks over their limit right now."""
    sha = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    lines = [
        f"# Comment blocks over IR-47's line limits, measured at {sha}.",
        "# Limits: `///` 6 body lines after the summary (12 when the block has an",
        "# `# Errors` section), `//!` 10 lines, `//` 4 in a row. Fenced code and an",
        "# `# Example` section never count; a line that is only its marker does.",
        "# Format: <lines> <path> <first line of the block>, one group per file.",
        "# A listed block may shrink but never past its recorded count; a block not",
        "# listed may not appear, and an entry whose block now fits must go.",
        "# Regenerate with `python3 scripts/check-doc-length.py --write-baseline`.",
        "# Merge main before measuring: a block main moved elsewhere reads as a new",
        "# block here and a stale entry there.",
        "",
    ]
    ordered = sorted(blocks, key=lambda b: (b.path, b.line))
    previous = None
    for block in ordered:
        if previous is not None and block.path != previous:
            # One gap per file: the file name is what a reader scans for.
            lines.append("")
        lines.append(f"{block.measured} {block.path} {block.first_line}")
        previous = block.path
    path.write_text("\n".join(lines) + "\n")


def check(blocks: list[Block], baseline_path: Path, narrowed: bool = False) -> tuple[list[str], int, int]:
    """Every failure to report, plus the number of blocks and of baselined blocks.

    `narrowed` says the run read only the files `blocks` came from. A whole
    repository run reads every tracked file, so every entry left unmatched is
    stale, including one naming a file main deleted or renamed. A narrowed run
    leaves the entries of the files it never read alone: the positional form
    exists to check the files a change touched.
    """
    offenders = [block for block in blocks if block.over]
    checked = {block.path for block in blocks}

    entries = parse_baseline(baseline_path)
    remaining: dict[tuple[str, str], list[BaselineEntry]] = {}
    for entry in entries:
        remaining.setdefault(entry.key, []).append(entry)

    failures: list[str] = []
    baselined = 0
    for block in offenders:
        matches = remaining.get(block.key)
        entry = matches.pop(0) if matches else None
        if entry is None:
            failures.append(f"{block.describe()} is not in the baseline")
        elif block.measured > entry.recorded:
            failures.append(
                f"{block.describe()} grew past the {entry.recorded} lines its baseline entry "
                f"allows ({entry.location})"
            )
        else:
            baselined += 1

    for entry_list in remaining.values():
        for entry in entry_list:
            if not narrowed or entry.path in checked:
                failures.append(f"{entry.describe()} is stale: no block over its limit matches it")

    return failures, len(blocks), baselined


def main(argv: list[str]) -> int:
    baseline_path = BASELINE
    write = False
    files: list[str] = []
    args = list(argv)

    while args:
        arg = args.pop(0)
        if arg == "--baseline":
            if not args:
                raise SystemExit("--baseline wants a path")
            baseline_path = Path(args.pop(0))
        elif arg == "--write-baseline":
            write = True
        elif arg in ("-h", "--help"):
            print(__doc__)
            return 0
        elif arg.startswith("-"):
            raise SystemExit(f"unknown option: {arg}")
        else:
            files.append(arg)

    # A narrowed run measures too little of the repository to be allowed to
    # rewrite the baseline from it: every file it never read would lose its
    # entries, and the next full run would report them as new.
    if write and files:
        raise SystemExit("--write-baseline measures the whole repository: no file arguments with it")

    narrowed = bool(files)
    if not files:
        files = tracked_rust_files()

    blocks = [block for file in files for block in blocks_in(file, (ROOT / file).read_text())]
    if write:
        write_baseline(baseline_path, [block for block in blocks if block.over])
        print(f"Wrote {sum(1 for block in blocks if block.over)} baselined block(s) to {baseline_path}")
        return 0

    failures, total, baselined = check(blocks, baseline_path, narrowed)
    for failure in failures:
        print(failure, file=sys.stderr)

    if failures:
        print(
            f"\n{len(failures)} comment block(s) above break IR-47's line limits.\n"
            "\n"
            "Cut the block to its limit, or make the case for it in the pull "
            "request and add it to the baseline by hand: a baseline entry is a "
            "debt this repository agreed to carry, not a way to keep a long "
            "comment.\n"
            "\n"
            "When main moved a block under this run, so that an entry names a "
            "file the block no longer sits in, re-measure the whole tree with "
            "`python3 scripts/check-doc-length.py --write-baseline` after "
            "merging main, and review the baseline diff like any other change.",
            file=sys.stderr,
        )
        return 1

    print(
        f"{total} comment blocks within IR-47's limits "
        f"({baselined} baselined block(s) still at or under their recorded size)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
