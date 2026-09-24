#!/usr/bin/env python3
"""Judge a versus-pm2 run against the committed baseline, or record a new one.

`versus-pm2.sh` appends one JSON record per measurement to `metrics.jsonl`.
This reduces a run to one figure per metric for each tool, then either
compares shep's figures with `baseline.json` or writes them there:

    python3 benches/versus-pm2/compare.py check  <metrics.jsonl>
    python3 benches/versus-pm2/compare.py record <metrics.jsonl>

`versus-pm2.sh --check` and `--record` run the harness and then call this, so
calling it by hand is for judging a run that has already finished. Every run
keeps its `metrics.jsonl`, so a second opinion never needs a second run.

`check` exits 0 when every gated metric held, 1 when at least one regressed
past its threshold, and 2 when the run cannot be judged against this baseline.
2 is not a pass. It means the run could not tell a change of that size from
the machine moving under it, and the report names the metric and the reason
rather than guessing.

A regression is a change in shep's own figure. pm2 is the control, not the
denominator: when shep moved and pm2 held, on the same box in the same run,
the change is shep's. That is the argument #291 and #292 were both made on.
Dividing by pm2 instead would read a node upgrade that shrank pm2's daemon as
shep's memory growing. So a gated metric is judged only when

- the run and the baseline come from the same OS and architecture,
- shep's rounds agree with each other within the metric's threshold, which is
  what the harness's A/B/A order exists to show, and
- pm2's figure is within the same threshold of its own baseline figure, unless
  the metric has no control: a file size does not move with machine load.

Unreadable input also exits 2, which is argparse's own code for a usage error.
Standard library only, so it runs wherever the harness does.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_BASELINE = HERE / "baseline.json"

# Moves when a baseline.json field is renamed, removed or retyped. A checker
# refuses a schema it does not know rather than compare figures it may be
# misreading.
SCHEMA = 1

HELD, REGRESSED, UNJUDGED = 0, 1, 2


class InputError(Exception):
    """A file this was handed cannot be read as what it claims to be."""


@dataclass(frozen=True)
class Metric:
    """One row of the report, and where in `metrics.jsonl` its figure lives."""

    # The name in baseline.json, and the one `--threshold` takes.
    key: str
    label: str
    # The `metric` field of the records it is read from, and the field on
    # them. `footprint` is the exception, see `read`.
    record: str
    field: str
    unit: str
    # The largest worsening tolerated, as a fraction. None reports the metric
    # and never gates on it.
    threshold: float | None
    # How many shep rounds a complete run has. A/B/A gives two.
    rounds: int = 2
    # Whether pm2's figure has to hold for shep's to be judged.
    controlled: bool = True


METRICS: tuple[Metric, ...] = (
    Metric("idle_rss", "idle daemon RSS", "idle", "rss_kb", "KiB", 0.10),
    # Reported, never gated. Both tools sit at hundredths of one percent of a
    # core, where the instrument's resolution is most of the reading. The
    # cputime-derived column rather than `pcpu_mean`, because macOS `ps %cpu`
    # is a decaying average and a cputime delta over the window is exact.
    Metric("idle_cpu", "idle daemon CPU", "idle", "cputime_derived_pct", "%", None),
    # Tighter than the rest. A real 8% regression is on record (#292) that
    # 10% would have waved through, and this is the steadiest figure the
    # harness takes: the two shep rounds agreed within 1.0% on 2026-09-14.
    Metric("log_cpu_per_line", "log-plane CPU per line", "log", "us_per_line", "us", 0.05),
    Metric("start_cold", "start ten apps, cold", "start", "cold_s", "s", 0.10),
    Metric("start_warm", "start ten apps, warm", "start", "warm_s", "s", 0.10),
    # Measured once per run, and a byte count is not noise, so 5% is a
    # deliberate size change rather than a wobble. No control: pm2's install
    # tree says nothing about whether shep's binary grew.
    Metric(
        "footprint",
        "shep binary vs pm2 install",
        "footprint",
        "shep_binary_bytes",
        "bytes",
        0.05,
        rounds=1,
        controlled=False,
    ),
)

BY_KEY = {metric.key: metric for metric in METRICS}


@dataclass(frozen=True)
class Reading:
    """What one run measured for one metric."""

    # One figure per shep round, in the order the run took them.
    shep: tuple[float, ...]
    pm2: float | None

    @property
    def shep_mean(self) -> float | None:
        return sum(self.shep) / len(self.shep) if self.shep else None

    @property
    def spread(self) -> float | None:
        """How far apart shep's rounds are, as a fraction of the lowest.

        None when there is nothing to compare, or when a round read zero and
        no fraction of it means anything.
        """
        if len(self.shep) < 2 or min(self.shep) <= 0:
            return None
        return (max(self.shep) - min(self.shep)) / min(self.shep)


@dataclass(frozen=True)
class Row:
    """One metric's verdict, with the figures that decided it."""

    metric: Metric
    threshold: float | None
    reading: Reading
    base_shep: float | None
    shep_change: float | None
    pm2_change: float | None
    # "held", "improved", "regressed", "unjudged" or "reported".
    status: str
    why: str


def number(value: object) -> float | None:
    """A JSON number as a float, or None for anything else, bools included."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value) if math.isfinite(value) else None


def change(now: float | None, then: float | None) -> float | None:
    """The fractional change from `then` to `now`, where one exists."""
    if now is None or then is None or then <= 0:
        return None
    return (now - then) / then


def load_run(path: Path) -> list[dict]:
    """Every record in a `metrics.jsonl`, in the order the harness wrote them."""
    try:
        text = path.read_text()
    except OSError as err:
        raise InputError(f"cannot read {path}: {err.strerror}") from err
    records = []
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as err:
            raise InputError(f"{path}:{n}: not JSON: {err.msg}") from err
        if not isinstance(record, dict) or not isinstance(record.get("metric"), str):
            raise InputError(f"{path}:{n}: not a metric record")
        records.append(record)
    return records


def load_baseline(path: Path) -> dict:
    try:
        baseline = json.loads(path.read_text())
    except OSError as err:
        raise InputError(f"cannot read {path}: {err.strerror}") from err
    except json.JSONDecodeError as err:
        raise InputError(f"{path}: not JSON: {err.msg}") from err
    if not isinstance(baseline, dict):
        raise InputError(f"{path}: not a baseline object")
    if baseline.get("schema") != SCHEMA:
        raise InputError(
            f"{path}: schema {baseline.get('schema')!r}, and this checker reads {SCHEMA}"
        )
    if not isinstance(baseline.get("metrics"), dict):
        raise InputError(f"{path}: no metrics table")
    if not isinstance(baseline.get("machine", {}), dict):
        raise InputError(f"{path}: machine is not an object")
    return baseline


def first(records: list[dict], kind: str) -> dict | None:
    return next((record for record in records if record["metric"] == kind), None)


def read(metric: Metric, records: list[dict]) -> Reading:
    """One metric's figures from a run.

    Every metric but one is a record per tool per round, with `tool` naming
    which. Footprint is one record carrying both tools' sizes, and pm2's is
    in KiB because `du -sk` is what measured it.
    """
    if metric.record == "footprint":
        record = first(records, "footprint") or {}
        shep = number(record.get("shep_binary_bytes"))
        pm2 = number(record.get("pm2_install_kb"))
        return Reading(
            shep=() if shep is None else (shep,),
            pm2=None if pm2 is None else pm2 * 1024,
        )
    rounds = [r for r in records if r["metric"] == metric.record]
    shep = tuple(
        value
        for r in rounds
        if r.get("tool") == "shep" and (value := number(r.get(metric.field))) is not None
    )
    pm2 = [
        value
        for r in rounds
        if r.get("tool") == "pm2" and (value := number(r.get(metric.field))) is not None
    ]
    return Reading(shep=shep, pm2=sum(pm2) / len(pm2) if pm2 else None)


def judge(metric: Metric, threshold: float | None, reading: Reading, base: object) -> Row:
    """Decide one metric, naming the first reason it cannot be decided."""
    entry = base if isinstance(base, dict) else None
    base_shep = number(entry.get("shep")) if entry else None
    base_pm2 = number(entry.get("pm2")) if entry else None
    shep_change = change(reading.shep_mean, base_shep)
    pm2_change = change(reading.pm2, base_pm2)
    spread = reading.spread

    def row(status: str, why: str) -> Row:
        return Row(metric, threshold, reading, base_shep, shep_change, pm2_change, status, why)

    # In the order a reader would have to fix them.
    if entry is None:
        reason = "the baseline has no figure for it"
    elif entry.get("unit") != metric.unit:
        reason = f"the baseline records it in {entry.get('unit')!r} and this checker in {metric.unit!r}"
    elif base_shep is None or base_shep <= 0:
        reason = "the baseline's shep figure is not a positive number"
    elif len(reading.shep) < metric.rounds:
        reason = f"the run has {len(reading.shep)} of {metric.rounds} shep rounds for it"
    elif threshold is not None and metric.rounds > 1 and spread is None:
        reason = "a shep round read zero, so the rounds cannot show the machine held still"
    elif threshold is not None and spread is not None and spread > threshold:
        reason = (
            f"the shep rounds disagree by {spread:.1%}, more than its {limit(threshold)} "
            "threshold, so this run cannot resolve a change that small"
        )
    elif threshold is not None and metric.controlled and pm2_change is None:
        missing = "the run" if reading.pm2 is None else "the baseline"
        reason = f"{missing} has no pm2 figure, so nothing shows the machine held still"
    elif threshold is not None and metric.controlled and abs(pm2_change) > threshold:
        reason = (
            f"pm2 moved {pm2_change:+.1%} against its own baseline, past the "
            f"{limit(threshold)} threshold, so the machine moved and shep's change is not shep's alone"
        )
    else:
        reason = None

    if threshold is None:
        return row("reported", reason or "")
    if reason is not None:
        return row("unjudged", reason)

    control = (
        f"while pm2 moved {pm2_change:+.1%}" if metric.controlled else "with no control to consult"
    )
    if shep_change > threshold:
        return row(
            "regressed",
            f"{show(base_shep, metric.unit)} -> {show(reading.shep_mean, metric.unit)}, "
            f"{shep_change:+.1%} against a {limit(threshold)} threshold, {control}",
        )
    if shep_change < -threshold:
        return row(
            "improved",
            f"better by {-shep_change:.1%}, past the threshold: record a new baseline once "
            "this ships, or a later slide back to the old figure passes unnoticed",
        )
    return row("held", "")


def show(value: float | None, unit: str) -> str:
    if value is None:
        return "-"
    if unit == "KiB":
        return f"{value / 1024:.2f} MiB"
    if unit == "bytes":
        return f"{value / 1048576:.2f} MiB"
    if unit == "s":
        return f"{value:.3f} s"
    if unit == "us":
        return f"{value:.2f} us"
    return f"{value:.3f}%"


def signed(fraction: float | None) -> str:
    return "-" if fraction is None else f"{fraction:+.1%}"


def limit(threshold: float) -> str:
    """A threshold as the percent it was given in: 12.5 stays 12.5, not 12."""
    return f"{threshold * 100:g}%"


def describe(meta: dict) -> str:
    """One line naming what a run or baseline measured, and on what."""
    sha = meta.get("shep_sha")
    shep = f"shep {meta.get('shep_version') or '?'}" + (
        f" ({sha[:8]})" if isinstance(sha, str) and sha else ""
    )
    machine = meta.get("machine") or {}
    where = f"{machine.get('os') or '?'} {machine.get('arch') or '?'}"
    if machine.get("cpu"):
        where += f", {machine['cpu']}"
    return (
        f"{meta.get('recorded') or '?'}  {shep}, pm2 {meta.get('pm2') or '?'}, "
        f"node {meta.get('node') or '?'}, {where}"
    )


def run_meta(records: list[dict]) -> dict:
    """The baseline-shaped description of a run, from its `run` and `versions` records."""
    run = first(records, "run") or {}
    versions = first(records, "versions") or {}
    ncpu = run.get("ncpu")
    return {
        "recorded": run.get("date") or None,
        "shep_version": versions.get("shep_version") or None,
        "shep_sha": versions.get("shep_sha") or None,
        "pm2": versions.get("pm2") or None,
        "node": versions.get("node") or None,
        "machine": {
            "os": run.get("os") or None,
            "arch": run.get("arch") or None,
            "cpu": run.get("cpu") or None,
            "ncpu": ncpu if isinstance(ncpu, int) and not isinstance(ncpu, bool) else None,
        },
    }


def platform_refusal(baseline: dict, meta: dict) -> str | None:
    """Why this run and this baseline cannot be compared at all, if they cannot."""
    theirs = baseline.get("machine") or {}
    ours = meta["machine"]
    if not (ours["os"] and ours["arch"]):
        return (
            "the run has no `run` record naming its OS and architecture, so it came "
            "from a harness older than this checker and its platform is unknown"
        )
    if not (theirs.get("os") and theirs.get("arch")):
        return "the baseline names no OS and architecture"
    if (theirs["os"], theirs["arch"]) != (ours["os"], ours["arch"]):
        return (
            f"the baseline was recorded on {theirs['os']} {theirs['arch']} and this run is "
            f"{ours['os']} {ours['arch']}. A binary's size and a daemon's RSS are both "
            "per-platform, so record a baseline here first: `versus-pm2.sh --record` against "
            "a build of the baseline's shep version, then check the new build against that"
        )
    return None


def thresholds(overrides: list[str]) -> dict[str, float | None]:
    """The table's thresholds, with any `--threshold key=pct` applied."""
    chosen = {metric.key: metric.threshold for metric in METRICS}
    for override in overrides:
        key, sep, pct = override.partition("=")
        if not sep or key not in chosen:
            raise InputError(
                f"--threshold {override!r}: expected KEY=PCT with KEY one of {', '.join(chosen)}"
            )
        try:
            value = float(pct.removesuffix("%"))
        except ValueError:
            value = math.nan
        if not math.isfinite(value) or value <= 0:
            raise InputError(f"--threshold {override!r}: PCT must be a positive number of percent")
        chosen[key] = value / 100
    return chosen


def check(args: argparse.Namespace) -> int:
    limits = thresholds(args.threshold)
    baseline = load_baseline(args.baseline)
    records = load_run(args.metrics)
    meta = run_meta(records)

    print(f"baseline  {describe(baseline)}")
    print(f"this run  {describe(meta)}")
    if refusal := platform_refusal(baseline, meta):
        print(f"\ncannot judge: {refusal}.")
        return UNJUDGED
    for tool in ("pm2", "node"):
        if baseline.get(tool) and meta[tool] and baseline[tool] != meta[tool]:
            print(
                f"note: {tool} was {baseline[tool]} for the baseline and is {meta[tool]} here. "
                "pm2's own rows below say whether that moved the control."
            )

    rows = [
        judge(metric, limits[metric.key], read(metric, records), baseline["metrics"].get(metric.key))
        for metric in METRICS
    ]

    table = [("metric", "limit", "baseline", "this run", "change", "pm2", "spread", "verdict")]
    for row in rows:
        spread = row.reading.spread
        table.append(
            (
                row.metric.key,
                "-" if row.threshold is None else limit(row.threshold),
                show(row.base_shep, row.metric.unit),
                show(row.reading.shep_mean, row.metric.unit),
                signed(row.shep_change),
                signed(row.pm2_change) if row.metric.controlled else "n/a",
                "-" if spread is None else f"{spread:.1%}",
                row.status,
            )
        )
    widths = [max(len(line[i]) for line in table) for i in range(len(table[0]))]
    print()
    for line in table:
        print("  ".join(cell.ljust(width) for cell, width in zip(line, widths)).rstrip())

    notes = [row for row in rows if row.why]
    if notes:
        print()
    for row in notes:
        print(f"{row.status.upper():<9}  {row.metric.key}: {row.why}")

    regressed = [row.metric.key for row in rows if row.status == "regressed"]
    unjudged = [row.metric.key for row in rows if row.status == "unjudged"]
    print()
    if regressed:
        print(f"regressed: {', '.join(regressed)}")
        return REGRESSED
    if unjudged:
        print(f"cannot judge: {', '.join(unjudged)}. Not a pass; re-run on a quieter machine.")
        return UNJUDGED
    print("held: every gated metric is within its threshold")
    return HELD


def record(args: argparse.Namespace) -> int:
    records = load_run(args.metrics)
    meta = run_meta(records)
    problems = []
    if not (meta["machine"]["os"] and meta["machine"]["arch"]):
        problems.append("the run has no `run` record naming its OS and architecture")
    if not (meta["shep_version"] and meta["pm2"] and meta["node"]):
        problems.append("the run's `versions` record is missing or incomplete")

    metrics = {}
    for metric in METRICS:
        reading = read(metric, records)
        spread = reading.spread
        if len(reading.shep) < metric.rounds:
            problems.append(f"{metric.key}: the run has {len(reading.shep)} of {metric.rounds} shep rounds")
        elif reading.pm2 is None:
            problems.append(f"{metric.key}: the run has no pm2 figure")
        elif metric.threshold is not None and metric.rounds > 1 and (
            spread is None or spread > metric.threshold
        ):
            problems.append(
                f"{metric.key}: the shep rounds disagree by "
                f"{'an unmeasurable amount' if spread is None else f'{spread:.1%}'}, "
                f"past its {limit(metric.threshold)} threshold"
            )
        else:
            metrics[metric.key] = {
                "unit": metric.unit,
                "shep": round(reading.shep_mean, 6),
                "shep_rounds": list(reading.shep),
                "pm2": round(reading.pm2, 6),
            }

    if problems:
        # A baseline with a hole in it, or one its own thresholds cannot
        # judge against, is worse than the one it would replace.
        print(f"not recording {args.metrics}:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return UNJUDGED

    baseline = {
        "schema": SCHEMA,
        "source": "recorded by versus-pm2.sh --record",
        **meta,
        "metrics": metrics,
    }
    args.out.write_text(json.dumps(baseline, indent=2) + "\n")
    print(f"wrote {args.out}")
    print(f"  {describe(baseline)}")
    print("  `git diff` it before committing: the diff is the before and after.")
    return HELD


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(
        prog="compare.py",
        description="Judge a versus-pm2 run against baseline.json, or record a new baseline.",
    )
    verbs = top.add_subparsers(dest="verb", required=True)

    judge_it = verbs.add_parser(
        "check",
        help="compare a run with the baseline: exit 0 held, 1 regressed, 2 cannot judge",
    )
    judge_it.add_argument("metrics", type=Path, help="a run's metrics.jsonl")
    judge_it.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    judge_it.add_argument(
        "--threshold",
        action="append",
        default=[],
        metavar="KEY=PCT",
        help=f"override one metric's threshold, in percent; KEY is one of {', '.join(BY_KEY)}",
    )
    judge_it.set_defaults(run=check)

    keep_it = verbs.add_parser(
        "record",
        help="write a run's figures as the baseline, refusing an incomplete or unsteady run",
    )
    keep_it.add_argument("metrics", type=Path, help="a run's metrics.jsonl")
    keep_it.add_argument("--out", type=Path, default=DEFAULT_BASELINE)
    keep_it.set_defaults(run=record)
    return top


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        return args.run(args)
    except InputError as err:
        print(f"compare.py: {err}", file=sys.stderr)
        return UNJUDGED


if __name__ == "__main__":
    sys.exit(main())
