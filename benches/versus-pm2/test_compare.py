"""Tests for compare.py, the half of the versus-pm2 harness CI can run.

The harness itself never runs in CI (see README.md), so these build
`metrics.jsonl` records in the exact shape `versus-pm2.sh` emits them and
check what the judge makes of them. One test reads the harness's source to
hold that shape to what the script actually writes.

    python3 -m unittest discover --start-directory benches/versus-pm2 --verbose
"""

from __future__ import annotations

import contextlib
import io
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import compare  # noqa: E402

# The 2026-09-14 figures, which is also what baseline.json was seeded from.
SHEP = {
    "rss_kb": 17828,
    "cputime_derived_pct": 0.138,
    "us_per_line": 2.27,
    "cold_s": 0.195,
    "warm_s": 0.095,
}
PM2 = {
    "rss_kb": 67461,
    "cputime_derived_pct": 0.065,
    "us_per_line": 4.10,
    "cold_s": 0.366,
    "warm_s": 0.201,
}
# One binary, as the harness sized it before #616, and the three-binary
# install it sizes from #616 on, which is what baseline.json holds.
SHEP_BINARY_BYTES = 18811453
SHEP_INSTALL_BYTES = 56465818
PM2_INSTALL_KB = 23665


def make_run(
    shep: dict | None = None,
    pm2: dict | None = None,
    second_round: dict | None = None,
    shep_bytes: int = SHEP_INSTALL_BYTES,
    footprint_field: str = "shep_install_bytes",
    os_: str = "Darwin",
    arch: str = "arm64",
    drop: tuple[str, ...] = (),
) -> list[dict]:
    """A complete A/B/A run in the harness's own record shapes.

    `shep` and `pm2` override figures for every round of that tool;
    `second_round` overrides the A2 round only, which is how a machine that
    shifted mid-run looks. `drop` removes records by `metric`, or by
    `metric/tag` for one round. The footprint record takes #616's shape
    unless `footprint_field` names the one-binary field it replaced.
    """
    rounds = {
        "A1": {**SHEP, **(shep or {})},
        "B": {**PM2, **(pm2 or {})},
        "A2": {**SHEP, **(shep or {}), **(second_round or {})},
    }
    records: list[dict] = [
        {"metric": "run", "date": "2026-09-24", "os": os_, "arch": arch, "cpu": "Apple M4 Pro", "ncpu": 14},
        {
            "metric": "versions",
            "shep_sha": "0123456789abcdef0123456789abcdef01234567",
            "shep_version": "0.9.1",
            "pm2": "7.0.4",
            "node": "v26.8.1",
        },
    ]
    for tag in ("A1", "B", "A2"):
        tool = "pm2" if tag == "B" else "shep"
        f = rounds[tag]
        records += [
            {
                "metric": "idle", "tool": tool, "tag": tag, "online": 10,
                "pcpu_mean": 0.1, "pcpu_max": 0.4, "samples": 60,
                "cputime_derived_pct": f["cputime_derived_pct"],
                "rss_kb": f["rss_kb"], "helper_rss_kb": 0,
            },
            {
                "metric": "log", "tool": tool, "tag": tag, "growing": 1,
                "line_bytes": 62, "pcpu_mean": 40.0, "pcpu_max": 45.0,
                "cputime_derived_pct": 41.0, "lines": 5_000_000,
                "lines_per_s": 170_000.0, "us_per_line": f["us_per_line"],
                "elapsed_s": 30.0, "daemon_cpu_s": 12.3, "log": "/x/loud-out.log",
            },
            {
                "metric": "start", "tool": tool, "tag": tag,
                "cold_s": f["cold_s"], "cold_online": 10,
                "warm_s": f["warm_s"], "warm_online": 10,
                "list_roundtrip_s": 0.01,
            },
        ]
    if footprint_field == "shep_install_bytes":
        third = shep_bytes // 3
        footprint = {
            "metric": "footprint",
            "shep_binaries_bytes": {"shep": third, "shep-runtime": third, "shep-dev": shep_bytes - 2 * third},
            "shep_install_bytes": shep_bytes,
            "shep_archive_gz_bytes": shep_bytes // 3,
        }
    else:
        footprint = {"metric": "footprint", footprint_field: shep_bytes}
    records.append({**footprint, "pm2_install_kb": PM2_INSTALL_KB})
    return [
        r
        for r in records
        if r["metric"] not in drop and f"{r['metric']}/{r.get('tag')}" not in drop
    ]


class Harness(unittest.TestCase):
    """Temp files, a baseline recorded from the default run, and a way to call main."""

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.dir = Path(scratch.name)
        self.baseline = self.dir / "baseline.json"
        self.assertEqual(self.record(make_run()), 0, self.out)

    def write_run(self, records: list[dict]) -> Path:
        path = self.dir / "metrics.jsonl"
        path.write_text("".join(json.dumps(r) + "\n" for r in records))
        return path

    def invoke(self, *argv: str) -> int:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = compare.main(list(argv))
        self.out = out.getvalue() + err.getvalue()
        return code

    def record(self, records: list[dict]) -> int:
        return self.invoke("record", str(self.write_run(records)), "--out", str(self.baseline))

    def check(self, records: list[dict], *extra: str) -> int:
        return self.invoke("check", str(self.write_run(records)), "--baseline", str(self.baseline), *extra)

    def verdict(self, key: str) -> str:
        """The verdict column of one metric's row in the last report."""
        match = re.search(rf"^{key}\s.*\s(\w+)$", self.out, re.MULTILINE)
        self.assertIsNotNone(match, f"no row for {key} in:\n{self.out}")
        return match.group(1)


class Judge(Harness):
    def test_the_run_a_baseline_was_recorded_from_holds_against_it(self) -> None:
        self.assertEqual(self.check(make_run()), 0, self.out)
        self.assertIn("held: every gated metric", self.out)

    def test_a_warm_start_regression_with_pm2_steady_is_named(self) -> None:
        # #291's shape: 0.056 s to 0.095 s while pm2 moved under 7%.
        code = self.check(make_run(shep={"warm_s": 0.162}, pm2={"warm_s": 0.205}))
        self.assertEqual(code, 1, self.out)
        self.assertEqual(self.verdict("start_warm"), "regressed")
        self.assertIn("regressed: start_warm", self.out)
        self.assertIn("+70.5% against a 10% threshold, while pm2 moved +2.0%", self.out)

    def test_an_eight_percent_log_regression_fails_the_tighter_threshold(self) -> None:
        # #292's log figure: +8%, which a 10% threshold would have passed.
        self.assertEqual(self.check(make_run(shep={"us_per_line": 2.27 * 1.08})), 1, self.out)
        self.assertEqual(self.verdict("log_cpu_per_line"), "regressed")

    def test_an_install_that_grew_regresses_with_no_control_consulted(self) -> None:
        self.assertEqual(self.check(make_run(shep_bytes=int(SHEP_INSTALL_BYTES * 1.26))), 1, self.out)
        self.assertEqual(self.verdict("footprint"), "regressed")
        self.assertIn("with no control to consult", self.out)

    def test_one_binary_is_never_compared_with_a_whole_install(self) -> None:
        # Read as a size change, a third of the install is a 67% improvement.
        run = make_run(shep_bytes=SHEP_BINARY_BYTES, footprint_field="shep_binary_bytes")
        self.assertEqual(self.check(run), 2, self.out)
        self.assertEqual(self.verdict("footprint"), "unjudged")
        self.assertIn(
            "the baseline holds `shep_install_bytes` and this run measured `shep_binary_bytes`",
            self.out,
        )
        self.assertNotIn("improved", self.out)
        self.assertNotIn("-66.7%", self.out)

    def test_a_run_from_before_616_is_judged_against_a_baseline_from_before_616(self) -> None:
        run = make_run(shep_bytes=SHEP_BINARY_BYTES, footprint_field="shep_binary_bytes")
        self.assertEqual(self.record(run), 0, self.out)
        footprint = json.loads(self.baseline.read_text())["metrics"]["footprint"]
        self.assertEqual((footprint["field"], footprint["shep"]), ("shep_binary_bytes", SHEP_BINARY_BYTES))
        self.assertEqual(self.check(run), 0, self.out)
        self.assertEqual(self.verdict("footprint"), "held")

    def test_pm2_moving_past_the_threshold_makes_the_metric_unjudgeable(self) -> None:
        # Both slower by the same amount is a slower machine, not a regression.
        code = self.check(make_run(shep={"cold_s": 0.195 * 1.3}, pm2={"cold_s": 0.366 * 1.3}))
        self.assertEqual(code, 2, self.out)
        self.assertEqual(self.verdict("start_cold"), "unjudged")
        self.assertIn("pm2 moved +30.0%", self.out)

    def test_pm2_moving_faster_is_also_the_machine_moving(self) -> None:
        self.assertEqual(self.check(make_run(pm2={"rss_kb": int(67461 * 0.8)})), 2, self.out)
        self.assertEqual(self.verdict("idle_rss"), "unjudged")

    def test_shep_rounds_that_disagree_cannot_resolve_the_threshold(self) -> None:
        code = self.check(make_run(second_round={"rss_kb": int(17828 * 1.15)}))
        self.assertEqual(code, 2, self.out)
        self.assertEqual(self.verdict("idle_rss"), "unjudged")
        self.assertIn("the shep rounds disagree by 15.0%", self.out)

    def test_a_regression_outranks_an_unjudgeable_metric_elsewhere(self) -> None:
        code = self.check(
            make_run(shep={"warm_s": 0.2}, second_round={"rss_kb": int(17828 * 1.15)})
        )
        self.assertEqual(code, 1, self.out)
        self.assertEqual(self.verdict("start_warm"), "regressed")
        self.assertEqual(self.verdict("idle_rss"), "unjudged")

    def test_a_round_the_harness_refused_to_report_is_a_missing_round(self) -> None:
        # m_idle returns before emitting when fewer than ten apps came up.
        self.assertEqual(self.check(make_run(drop=("idle/A2",))), 2, self.out)
        self.assertIn("the run has 1 of 2 shep rounds", self.out)

    def test_a_missing_pm2_round_leaves_no_control(self) -> None:
        self.assertEqual(self.check(make_run(drop=("log/B",))), 2, self.out)
        self.assertIn("the run has no pm2 figure", self.out)

    def test_ungated_idle_cpu_is_reported_and_never_decides(self) -> None:
        self.assertEqual(self.check(make_run(shep={"cputime_derived_pct": 0.9})), 0, self.out)
        self.assertEqual(self.verdict("idle_cpu"), "reported")

    def test_a_threshold_override_can_gate_an_ungated_metric(self) -> None:
        code = self.check(make_run(shep={"cputime_derived_pct": 0.9}), "--threshold", "idle_cpu=50")
        self.assertEqual(code, 1, self.out)
        self.assertEqual(self.verdict("idle_cpu"), "regressed")

    def test_a_threshold_override_loosens_one_metric(self) -> None:
        run = make_run(shep={"warm_s": 0.110})  # +15.8%
        self.assertEqual(self.check(run), 1, self.out)
        self.assertEqual(self.check(run, "--threshold", "start_warm=20%"), 0, self.out)

    def test_a_threshold_naming_no_metric_is_refused(self) -> None:
        self.assertEqual(self.check(make_run(), "--threshold", "start_wram=20"), 2)
        self.assertIn("KEY one of", self.out)
        self.assertEqual(self.check(make_run(), "--threshold", "start_warm=0"), 2)
        self.assertEqual(self.check(make_run(), "--threshold", "start_warm=nan"), 2)

    def test_an_improvement_passes_and_says_to_record(self) -> None:
        self.assertEqual(self.check(make_run(shep={"warm_s": 0.070})), 0, self.out)
        self.assertEqual(self.verdict("start_warm"), "improved")
        self.assertIn("record a new baseline", self.out)

    def test_another_platform_is_refused_outright(self) -> None:
        self.assertEqual(self.check(make_run(os_="Linux", arch="x86_64")), 2, self.out)
        self.assertIn("recorded on Darwin arm64 and this run is Linux x86_64", self.out)
        self.assertNotIn("verdict", self.out)

    def test_a_run_from_an_older_harness_has_no_platform(self) -> None:
        self.assertEqual(self.check(make_run(drop=("run",))), 2, self.out)
        self.assertIn("older than this checker", self.out)

    def test_a_different_pm2_is_noted_and_left_to_the_control(self) -> None:
        run = make_run()
        next(r for r in run if r["metric"] == "versions")["pm2"] = "7.1.0"
        self.assertEqual(self.check(run), 0, self.out)
        self.assertIn("note: pm2 was 7.0.4 for the baseline and is 7.1.0 here", self.out)

    def test_a_unit_change_is_refused_rather_than_compared(self) -> None:
        baseline = json.loads(self.baseline.read_text())
        baseline["metrics"]["idle_rss"]["unit"] = "MiB"
        self.baseline.write_text(json.dumps(baseline))
        self.assertEqual(self.check(make_run()), 2, self.out)
        self.assertIn("in 'MiB' and this checker in 'KiB'", self.out)

    def test_an_unknown_schema_is_refused(self) -> None:
        baseline = json.loads(self.baseline.read_text())
        baseline["schema"] = 2
        self.baseline.write_text(json.dumps(baseline))
        self.assertEqual(self.check(make_run()), 2)
        self.assertIn("schema 2, and this checker reads 1", self.out)

    def test_an_int_past_a_floats_range_is_a_missing_figure_not_a_crash(self) -> None:
        run = make_run()
        next(r for r in run if r["metric"] == "idle" and r["tag"] == "A1")["rss_kb"] = 10**400
        self.assertEqual(self.check(run), 2, self.out)
        self.assertIn("idle_rss: the run has 1 of 2 shep rounds", self.out)
        self.assertEqual(self.record(run), 2, self.out)
        self.assertNotIn("Traceback", self.out)

    def test_a_crash_exits_cannot_judge_never_regressed(self) -> None:
        with mock.patch.object(compare, "judge", side_effect=RuntimeError("boom")):
            self.assertEqual(self.check(make_run()), 2)
        self.assertIn("crashed, so nothing was judged", self.out)

    def test_a_line_that_is_not_json_names_its_line(self) -> None:
        path = self.dir / "metrics.jsonl"
        path.write_text(json.dumps(make_run()[0]) + "\n{not json\n")
        self.assertEqual(self.invoke("check", str(path), "--baseline", str(self.baseline)), 2)
        self.assertIn("metrics.jsonl:2: not JSON", self.out)


class Record(Harness):
    def test_the_baseline_carries_the_mean_and_both_rounds(self) -> None:
        self.assertEqual(self.record(make_run(second_round={"warm_s": 0.099})), 0, self.out)
        warm = json.loads(self.baseline.read_text())["metrics"]["start_warm"]
        self.assertEqual(
            warm,
            {"unit": "s", "field": "warm_s", "shep": 0.097, "shep_rounds": [0.095, 0.099], "pm2": 0.201},
        )

    def test_the_baseline_names_what_it_measured(self) -> None:
        baseline = json.loads(self.baseline.read_text())
        self.assertEqual(baseline["schema"], compare.SCHEMA)
        self.assertEqual(baseline["recorded"], "2026-09-24")
        self.assertEqual(baseline["shep_version"], "0.9.1")
        self.assertEqual(baseline["machine"], {"os": "Darwin", "arch": "arm64", "cpu": "Apple M4 Pro", "ncpu": 14})
        self.assertEqual(baseline["metrics"]["footprint"]["pm2"], PM2_INSTALL_KB * 1024)
        self.assertEqual(baseline["metrics"]["footprint"]["field"], "shep_install_bytes")
        self.assertEqual(baseline["metrics"]["idle_rss"]["field"], "rss_kb")

    def test_an_unsteady_run_is_not_recorded(self) -> None:
        before = self.baseline.read_text()
        self.assertEqual(self.record(make_run(second_round={"cold_s": 0.195 * 1.2})), 2)
        self.assertIn("start_cold: the shep rounds disagree by 20.0%", self.out)
        self.assertEqual(self.baseline.read_text(), before)

    def test_an_incomplete_run_is_not_recorded(self) -> None:
        self.assertEqual(self.record(make_run(drop=("footprint", "versions"))), 2)
        self.assertIn("footprint: the run has 0 of 1 shep rounds", self.out)
        self.assertIn("`versions` record is missing", self.out)


class Committed(unittest.TestCase):
    """The files this directory ships, read as the checker reads them."""

    def test_the_committed_baseline_covers_every_gated_metric(self) -> None:
        baseline = compare.load_baseline(compare.DEFAULT_BASELINE)
        for metric in compare.METRICS:
            if metric.threshold is None:
                continue
            with self.subTest(metric=metric.key):
                entry = baseline["metrics"].get(metric.key)
                self.assertIsNotNone(entry, "a gated metric the baseline cannot judge")
                self.assertEqual(entry["unit"], metric.unit)
                self.assertEqual(entry.get("field"), metric.field)
                self.assertGreater(compare.number(entry["shep"]), 0)
                self.assertGreater(compare.number(entry["pm2"]), 0)
        self.assertTrue(baseline["machine"]["os"] and baseline["machine"]["arch"])

    def test_every_field_the_checker_reads_is_one_the_harness_writes(self) -> None:
        # The harness builds its JSON by hand as `\"name\":` inside double
        # quotes, so a renamed field there is otherwise a metric this reads
        # as missing forever, and nothing that runs in CI would say so.
        script = (HERE / "versus-pm2.sh").read_text()
        emitted = set(re.findall(r'\\"(\w+)\\":', script)) | set(
            re.findall(r'"(\w+)": ', script)
        )
        kinds = set(re.findall(r'\\"metric\\":\\"(\w+)\\"', script)) | set(
            re.findall(r'"metric": "(\w+)"', script)
        )
        read = {"tool", "pm2_install_kb", "shep_version", "shep_sha", "pm2", "node"}
        read |= {"date", "os", "arch", "cpu", "ncpu"}
        read |= {metric.field for metric in compare.METRICS if metric.record != "footprint"}
        self.assertEqual(read - emitted, set())
        # Footprint takes whichever of its fields the harness writes, so one
        # is enough, and it must be one: none is a footprint never read.
        self.assertTrue(emitted & set(compare.FOOTPRINT_FIELDS), compare.FOOTPRINT_FIELDS)
        wanted = {metric.record for metric in compare.METRICS} | {"run", "versions"}
        self.assertEqual(wanted - kinds, set())


if __name__ == "__main__":
    unittest.main()
