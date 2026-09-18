# Lookout 1d: the sheep pane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give one sheep a whole screen: two histories on a shared time axis, its config, its env keys, and a tall feed.

**Architecture:** A fifth `Body` variant holding the pane's state, opened with `↵` from the flock table. The charts read `App`'s own ring buffers, which keep filling while the pane is shut. The daemon starts publishing the raw CPU counter it already computes, so lookout can difference consecutive polls instead of buffering a running mean over a window that resets every fifteen seconds.

**Tech Stack:** Rust 1.88, edition 2024, ratatui 0.30.2, insta snapshots.

**Spec:** [docs/brainstorming/specs/2026-09-08-lookout-1d-sheep-pane-design.md](../../brainstorming/specs/2026-09-08-lookout-1d-sheep-pane-design.md)

## Global Constraints

- **Invoke the `tui-screen-capture` skill before changing anything this pane draws, not after.** A ratatui pane cannot be checked by reading a diff. Run the binary in a pty at the width the task is about, read the screen back, and compare it against the frame. This is how a wrong column width, an unpainted band or a dropped glyph gets caught; a snapshot test that passes proves the string, not the screen. If the Skill tool is unavailable, read `~/.claude/skills/tui-screen-capture/SKILL.md` directly.
- **Invoke the `shep-idiomatic-rust` skill before writing Rust.** Cite `IR-<n>` where a rule applies. Top drift risks here: `core::error::Error` rather than `std::error::Error`, a `# Errors` section on every fallible public function, and a deliberate `Debug` decision on every new public type.
- Two additive wire fields only: `ProcessInfo::cpu_ms` and `SheepConfigView::env_secrets`. **No `PROTOCOL_VERSION`, `MIN_SUPPORTED` or `SCHEMA_VERSION` movement.** Neither field is a rename, a removal or a retype.
- Every new public item needs a doc comment and a deliberate `Debug` decision. Anything carrying env or secrets is redacted, with an exact-string test (IR-41).
- **Every test gets the mutation check before the task is called done.** Break the thing the test claims to pin, run it, watch it fail, put it back, then ask out loud what else could have made it pass. Six tests shipped on the last two panes pinning nothing, two of them passing on text that was present for an unrelated reason. A test whose failure message would not name the defect is not finished.
- **Every gallery scene states its width arithmetic in a comment** and asserts it is wide enough for the tier it exercises. A scene one cell short of its own column set silently drops the thing it exists to show.
- Conventional commit subjects, `type(scope): summary`, in the crate that changes. Nothing here breaks, so no `!`. Nine accepted types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`.
- ONE cargo shape: `--workspace`. Do not alternate with `-p <crate>`; this workspace shares one target-dir build lock and the cache churns badly when a brief names two shapes.
- Iterate with `cargo test --workspace --all-features --lib --bins`. Run the full `cargo test --workspace --all-features` once per task before committing.
- `map_key` dispatches on `InputMode`, not on which `Body` is showing, and there are only two modes. A new pane does not get a third; its keys are handled by the reducer matching on `Body`.
- Repo-relative paths only, in code, comments and commit messages. Never an absolute path out of a local checkout.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/shep-core/src/values.rs` | the CPU percent formula, shared by the daemon and lookout |
| `crates/shep-core/src/secrets.rs` | `sealed_keys`, the env-key direction of `references` |
| `crates/shep-core/src/protocol/request.rs` | `ProcessInfo::cpu_ms`, `SheepConfigView::env_secrets` |
| `crates/shep-daemon/src/limits/stats.rs` | `SheepStats::cpu_ms`, filled from the reading `sample_now` already takes |
| `crates/shep-daemon/src/rpc.rs` | carrying it into the listing |
| `crates/shep-cli/src/lookout/view/cell.rs` | `chart`, beside `gauge` and `sparkline` |
| `crates/shep-cli/src/lookout/pane.rs` | `sheep_fields`, extracted from `ConfigPane::sheep` |
| `crates/shep-cli/src/lookout/pane_sheep.rs` (new) | the pane's own state |
| `crates/shep-cli/src/lookout/view/sheep.rs` (new) | rendering the pane |
| `crates/shep-cli/src/lookout/app.rs` | the ring buffers, `Body::Sheep`, the renamed key arms |
| `crates/shep-cli/src/lookout/input.rs` | the rename |
| `crates/shep-cli/src/lookout/frames.rs` | a scene per responsive tier |

New files rather than growing `app.rs`, which is already past 10,000 lines.

---

## Task 1: The daemon publishes its CPU counter

**Files:**
- Modify: `crates/shep-core/src/values.rs`
- Modify: `crates/shep-core/src/protocol/request.rs` (`ProcessInfo` at :739-743, its builder at :946-953, the literal at :871)
- Modify: `crates/shep-core/src/protocol/frame.rs:62`, `crates/shep-core/src/protocol/events.rs:254` and `:408` (struct literals that will stop compiling)
- Modify: `crates/shep-daemon/src/limits/stats.rs` (`SheepStats` at :25-36, `sample_now` at :159, `cpu_percent` at :284)
- Modify: `crates/shep-daemon/src/rpc.rs` (`with_live_stats` at :1200)
- Modify: `crates/shep-cli/src/output/snapshots/shep__output__tests__the_json_envelope_shape_is_pinned.snap`

**Interfaces:**
- Produces: `shep_core::values::cpu_percent(cpu_ms: u64, window: core::time::Duration) -> Option<f32>`; `ProcessInfo::cpu_ms: Option<u64>` and `ProcessInfo::builder(..).cpu_ms(Option<u64>)`; `SheepStats::cpu_ms: u64`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/values.rs`'s `mod tests`:

```rust
/// Per-mille of one core: 1000 CPU-milliseconds over one wall second is
/// 100% of one core, and the daemon and lookout must agree on that or the
/// two numbers on screen disagree.
#[test]
fn a_full_core_for_the_whole_window_is_a_hundred_percent() {
    assert_eq!(cpu_percent(1000, Duration::from_secs(1)), Some(100.0));
    assert_eq!(cpu_percent(1000, Duration::from_secs(2)), Some(50.0));
}

/// Over one core is a tree spanning several, not a bug.
#[test]
fn a_tree_over_one_core_reports_over_a_hundred() {
    assert_eq!(cpu_percent(4000, Duration::from_secs(1)), Some(400.0));
}

/// A zero window would divide a near-zero delta by a near-zero number and
/// report anything from 0% to thousands.
#[test]
fn a_zero_window_has_no_honest_answer() {
    assert_eq!(cpu_percent(1000, Duration::ZERO), None);
}
```

In `crates/shep-core/src/protocol/request.rs`'s `mod tests`, beside the existing `ProcessInfo` builder tests near `:1910`:

```rust
/// The raw counter rides the wire beside the percent. A client polling
/// faster than the shepherd's own sampling interval differences two of
/// these; `cpu_percent` cannot serve it, because consecutive readings
/// share a baseline.
#[test]
fn a_process_info_carries_the_cpu_counter() {
    let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
        .cpu_ms(Some(1_234))
        .build();
    assert_eq!(info.cpu_ms, Some(1_234));
}

/// Absent by default, like every other sampled field: a lifecycle verb's
/// answer carries no reading.
#[test]
fn a_process_info_without_a_reading_has_no_counter() {
    let info = ProcessInfo::builder(3, "web", ProcStatus::Online).build();
    assert_eq!(info.cpu_ms, None);
}
```

Read two neighbouring tests first and use whatever builder-and-`build()` shape they use; the two lines above are this plan's guess at it, not a reading of the file.

In `crates/shep-daemon/src/limits/stats.rs`'s `mod tests`, beside `:342`:

```rust
/// The counter is reported whether or not a baseline exists: it is the
/// reading itself, not a rate measured against anything.
#[test]
fn a_sample_carries_the_counter_without_a_baseline() {
    let stats = StatsState::new(Arc::new(ScriptedSampler::new(vec![vec![rss_cpu(
        100, None, 1024, 5_000,
    )]])));
    stats.watch(1, 100);
    assert_eq!(stats.sample_now()[&100].cpu_ms, 5_000);
    assert_eq!(stats.sample_now()[&100].cpu_percent, None);
}
```

Read the neighbouring tests for how `StatsState` and `ScriptedSampler` are actually constructed and mirror that; the constructor call above is a guess.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins cpu_ms
```

Expected: FAIL, `no method named cpu_ms` and `no function or associated item named cpu_percent`.

- [ ] **Step 3: Add the shared formula**

In `crates/shep-core/src/values.rs`:

```rust
/// Tree CPU over a window, as a percentage of one core.
///
/// `cpu_ms` is the CPU-milliseconds the tree spent during `window`. A value
/// over 100 is a tree using more than one core, not a bug.
///
/// `None` when `window` is zero: dividing by it produces a nonsense figure
/// rather than a large one.
///
/// Shared by the shepherd, which measures against its own periodic
/// baseline, and by `shep lookout`, which differences two readings of
/// [`ProcessInfo::cpu_ms`](crate::protocol::ProcessInfo::cpu_ms) over its
/// own poll. Both put a percentage on the same screen, so the conversion
/// lives in one place.
#[must_use]
pub fn cpu_percent(cpu_ms: u64, window: Duration) -> Option<f32> {
    if window.is_zero() {
        return None;
    }
    // CPU-milliseconds over wall-seconds is per-mille of one core. Computed
    // in f64 and narrowed once at the end, since f32 would lose milliseconds
    // off a counter that has run for a month.
    Some((cpu_ms as f64 / window.as_secs_f64() / 10.0) as f32)
}
```

- [ ] **Step 4: Add the wire field**

In `crates/shep-core/src/protocol/request.rs`, beside `memory_bytes` at `:743`:

```rust
    /// The tree's cumulative CPU-milliseconds, absent under the same
    /// conditions as [`Self::cpu_percent`].
    ///
    /// The counter rather than a rate, so a client polling faster than the
    /// shepherd's own sampling interval can difference two readings and get
    /// the mean over its own interval. [`Self::cpu_percent`] cannot serve
    /// that: it is measured against a baseline the shepherd rewrites on its
    /// own schedule, so consecutive readings share one and each is a running
    /// mean over a window that grows and then resets.
    pub cpu_ms: Option<u64>,
```

And beside the `memory_bytes` setter at `:952`:

```rust
    /// Sets the tree's cumulative CPU-milliseconds; `None` when unsampled.
    #[must_use]
    pub fn cpu_ms(mut self, cpu_ms: Option<u64>) -> Self {
        self.info.cpu_ms = cpu_ms;
        self
    }
```

Add `cpu_ms: None` to the struct literal at `request.rs:871` and to the three in `frame.rs:62`, `events.rs:254` and `events.rs:408`. The compiler names every one of them; do not go looking for others by hand.

- [ ] **Step 5: Fill it in the daemon**

In `crates/shep-daemon/src/limits/stats.rs`, on `SheepStats` beside `memory_bytes`:

```rust
    /// The tree's cumulative CPU-milliseconds as of this reading.
    pub cpu_ms: u64,
```

In `sample_now`, the value is already in hand as `observed_cpu_ms` (`:174`). Add `cpu_ms: observed_cpu_ms` to the `SheepStats` it builds.

Rewrite the private `cpu_percent` at `:284` to call the shared one, keeping its own comment about the saturating subtraction:

```rust
fn cpu_percent(baseline: Baseline, cpu_ms: u64, now: Instant) -> Option<f32> {
    // Saturating: a counter that went backwards means the tree under this
    // pid is not the one the baseline was taken from (a lamb exited, or the
    // pid was recycled), and zero is the honest reading for that window.
    let elapsed_cpu_ms = cpu_ms.saturating_sub(baseline.cpu_ms);
    values::cpu_percent(elapsed_cpu_ms, now.saturating_duration_since(baseline.at))
}
```

In `crates/shep-daemon/src/rpc.rs`'s `with_live_stats`, beside the two lines already there:

```rust
            info.cpu_ms = Some(reading.cpu_ms);
```

- [ ] **Step 6: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS, except the JSON envelope snapshot, which now fails on an added field.

- [ ] **Step 7: Review and accept the snapshot**

```bash
cargo insta accept --workspace
```

Read the diff first. The only change should be one `"cpu_ms"` line per sheep in the envelope. Anything else means something moved that should not have.

- [ ] **Step 8: Run the mutation check**

For each test written in step 1: change the thing it pins (make `cpu_percent` divide by 100 instead of 10, drop the `is_zero` guard, stop setting `cpu_ms` in `sample_now`), confirm the right test fails with a message that names the defect, then restore.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(core): publish the tree CPU counter beside its percent"
```

---

## Task 2: The view says which env keys are sealed

**Files:**
- Modify: `crates/shep-core/src/secrets.rs` (`references` at :377)
- Modify: `crates/shep-core/src/protocol/request.rs` (`SheepConfigView` at :1380, `new` at :1404, its `Debug` at :1422)

**Interfaces:**
- Produces: `shep_core::secrets::sealed_keys(config: &AppConfig) -> Vec<String>`; `SheepConfigView::env_secrets: Vec<String>`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/secrets.rs`'s `mod tests`:

```rust
/// The key, not the reference. `references` answers the other direction,
/// and a pane marking a row needs this one.
#[test]
fn a_sealed_key_is_reported_by_its_own_name() {
    let mut config = app_config();
    config.env.insert("A".into(), "{{secret:ONE}}".into());
    config.env.insert("B".into(), "literal".into());
    assert_eq!(sealed_keys(&config), vec!["A".to_string()]);
}

/// A value that only embeds a reference still comes from the store.
#[test]
fn an_embedded_reference_seals_its_key() {
    let mut config = app_config();
    config.env.insert(
        "DB_URL".into(),
        "postgres://u:{{secret:pg/PASSWORD}}@h".into(),
    );
    assert_eq!(sealed_keys(&config), vec!["DB_URL".to_string()]);
}

/// A positional token is not a secret: `{{name}}` and `{{instance}}` are
/// filled from the sheep, not from the store.
#[test]
fn a_positional_token_does_not_seal_a_key() {
    let mut config = app_config();
    config.env.insert("LOG".into(), "{{name}}.log".into());
    assert!(sealed_keys(&config).is_empty());
}

/// Only `env`. `args` and `out_file` can name a reference too, and
/// `references` reports those; this function is about env rows.
#[test]
fn a_reference_outside_env_seals_no_key() {
    let mut config = app_config();
    config.args = vec!["--token={{secret:ONE}}".into()];
    assert!(sealed_keys(&config).is_empty());
}
```

Use whatever fixture the neighbouring tests use to build an `AppConfig`; `app_config()` above is a placeholder name, and `secrets.rs:808` shows the real shape.

In `crates/shep-core/src/protocol/request.rs`'s `mod tests`, beside the existing `env_keys` test at `:2359`:

```rust
/// Recorded before the clear, since the values are what name a reference
/// and they are gone by the time anything else can look.
#[test]
fn a_config_view_records_which_env_keys_are_sealed() {
    let mut config = app_config();
    config.env.insert("PLAIN".into(), "value".into());
    config.env.insert("SEALED".into(), "{{secret:PW}}".into());
    let view = SheepConfigView::new(config, Vec::new(), Vec::new());
    assert!(view.config.env.is_empty());
    assert_eq!(view.env_keys, ["PLAIN", "SEALED"]);
    assert_eq!(view.env_secrets, ["SEALED"]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins sealed
```

Expected: FAIL, `cannot find function sealed_keys`.

- [ ] **Step 3: Write `sealed_keys`**

Read `references` at `secrets.rs:377` first. Its closure already tokenises a value and recognises a secret reference through `template::walk` and `template::secret_reference`. Factor that per-value test out so both functions use one tokenizer pass shape rather than two spellings of the same check, then:

```rust
/// The env keys whose value names at least one `{{secret:...}}` reference,
/// in `env`'s own order.
///
/// The keys, never the references: [`references`] answers the other
/// direction, and a pane that wants to mark a row as sealed needs this one.
/// A value that merely embeds a reference counts, since the store is still
/// what fills it in.
///
/// `env` alone. A reference in `args` or `out_file` is real and
/// [`references`] reports it, but it is not an env row and nothing renders
/// it as one.
#[must_use]
pub fn sealed_keys(config: &AppConfig) -> Vec<String> {
    config
        .env
        .iter()
        .filter(|(_, value)| names_a_secret(value))
        .map(|(key, _)| key.clone())
        .collect()
}
```

- [ ] **Step 4: Add the field to the view**

In `crates/shep-core/src/protocol/request.rs`, beside `env_keys`:

```rust
    /// Which of [`Self::env_keys`] resolve from the secret store, so a pane
    /// can mark the row without showing anything. Recorded before `env` is
    /// cleared, which is the only moment the values exist to be read.
    pub env_secrets: Vec<String>,
```

In `new`, above the clear:

```rust
        let env_secrets = crate::secrets::sealed_keys(&config);
        let env_keys = config.env.keys().cloned().collect();
        config.env.clear();
```

`env_secrets` is a key set like `env_keys`, so it is counted rather than named in the existing `Debug` at `:1422`. Extend that impl and its exact-string test at `:2374` together.

- [ ] **Step 5: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 6: Run the mutation check**

Move the `sealed_keys` call below the `config.env.clear()` and confirm the view test fails rather than silently reporting an empty list. Drop the `env` filter so `args` is scanned too, and confirm `a_reference_outside_env_seals_no_key` fails. Restore both.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(core): report which env keys resolve from the store"
```

---

## Task 3: The chart cell

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/cell.rs`

**Interfaces:**
- Produces: `cell::chart(samples: &[f32], ceiling: f32, cols: usize, rows: usize) -> Vec<String>`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/lookout/view/cell.rs`'s `mod tests`, in the style of the `sparkline` tests at `:146`:

```rust
#[test]
fn a_chart_at_the_ceiling_fills_every_row() {
    assert_eq!(chart(&[100.0], 100.0, 1, 4), ["█", "█", "█", "█"]);
}

#[test]
fn a_chart_at_half_the_ceiling_fills_the_bottom_half() {
    assert_eq!(chart(&[50.0], 100.0, 1, 4), [" ", " ", "█", "█"]);
}

/// The half-block is the whole point of the cell: four rows carry eight
/// steps, not four.
#[test]
fn an_odd_half_step_draws_the_lower_half_block() {
    assert_eq!(chart(&[12.5], 100.0, 1, 4), [" ", " ", " ", "▄"]);
}

#[test]
fn a_chart_over_its_ceiling_saturates_rather_than_overflowing() {
    assert_eq!(chart(&[250.0], 100.0, 1, 2), ["█", "█"]);
}

/// Left-padded like `sparkline`, so the chart grows into its column from
/// the right as history arrives rather than stretching to fit.
#[test]
fn a_chart_shorter_than_its_columns_pads_on_the_left() {
    assert_eq!(chart(&[100.0], 100.0, 3, 1), ["  █"]);
}

#[test]
fn a_chart_longer_than_its_columns_keeps_the_newest() {
    assert_eq!(chart(&[100.0, 0.0, 0.0], 100.0, 2, 1), ["  "]);
}

/// No samples is blank rather than a floor line, for `sparkline`'s reason:
/// a flat line reads as measured and idle, blank reads as not measured yet.
#[test]
fn an_empty_chart_is_blank_rather_than_a_floor_line() {
    assert_eq!(chart(&[], 100.0, 3, 2), ["   ", "   "]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins chart
```

Expected: FAIL, `cannot find function chart`.

- [ ] **Step 3: Write `chart`**

```rust
/// A half-block area chart: `rows` rows of `cols` cells, oldest sample
/// leftmost, top row first.
///
/// Sixteen steps in eight rows. For a sample of value `v` the column's
/// height in half-steps is `h = round(v / ceiling * rows * 2)`; for row `r`
/// counted from the top, `s = h - (rows - 1 - r) * 2`, and the cell is `█`
/// when `s >= 2`, `▄` when `s == 1`, and blank otherwise.
///
/// Left-padded and ceiling-saturating like [`sparkline`], and blank rather
/// than a floor line with no samples, for the reasons that function's own
/// doc gives.
#[must_use]
pub fn chart(samples: &[f32], ceiling: f32, cols: usize, rows: usize) -> Vec<String> {
    if rows == 0 {
        return Vec::new();
    }
    if cols == 0 {
        return vec![String::new(); rows];
    }
    let window = &samples[samples.len().saturating_sub(cols)..];
    let pad = cols - window.len();
    let ceiling = if ceiling > 0.0 { ceiling } else { 1.0 };
    let steps = rows * 2;
    let heights: Vec<usize> = window
        .iter()
        .map(|sample| (sample.clamp(0.0, ceiling) / ceiling * steps as f32).round() as usize)
        .collect();
    (0..rows)
        .map(|row| {
            let floor = (rows - 1 - row) * 2;
            let mut line = " ".repeat(pad);
            for height in &heights {
                line.push(match height.saturating_sub(floor) {
                    0 => ' ',
                    1 => '\u{2584}',
                    _ => '\u{2588}',
                });
            }
            line
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 5: Run the mutation check**

Change `steps` from `rows * 2` to `rows` and confirm `an_odd_half_step_draws_the_lower_half_block` fails. Drop the `clamp` and confirm the saturation test fails. Reverse the row order and confirm the half-ceiling test fails. Restore each.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(lookout): add the half-block area chart cell"
```

---

## Task 4: Lookout differences the counter, and keeps RSS

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (`cpu_history` at :1415, `record_cpu_samples` at :4069, the `Msg::Snapshot` arm at :1508, the accessors at :4093, the tests at :5122-5175)

**Interfaces:**
- Consumes: `ProcessInfo::cpu_ms` and `shep_core::values::cpu_percent` from Task 1.
- Produces: `App::rss_history(&self, id: u32) -> &[u64]`; `App::cpu_history` unchanged in signature, changed in what fills it.

- [ ] **Step 1: Write the failing tests**

The existing tests at `app.rs:5122` build rows with `cpu_percent` and must move to `cpu_ms`. They also share one instant, and differencing over a zero window is `None`, so the harness has to advance the clock. Read how `self.now` is set before writing the helper.

Replace `on_snapshot` with a version that advances, and keep a two-second gap so the numbers match the real poll:

```rust
    impl App {
        /// Drives `Msg::Snapshot` the way the poll does, two seconds after
        /// the last one. The gap is load-bearing: a differenced sample over
        /// a zero window has no honest value.
        fn on_snapshot(&mut self, rows: Vec<ProcessInfo>) {
            self.now += Duration::from_secs(2);
            let at = self.now;
            self.update(Msg::Snapshot { rows, at });
        }
    }
```

Then:

```rust
/// The first reading has nothing behind it to difference, so it records a
/// baseline and appends nothing. A zero would claim an idle sample that
/// was never measured.
#[test]
fn the_first_reading_records_a_baseline_and_no_sample() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
    assert!(app.cpu_history(1).is_empty());
}

/// 2000 CPU-milliseconds across a two-second poll is one core.
#[test]
fn two_readings_difference_into_one_sample() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
    assert_eq!(app.cpu_history(1), &[100.0]);
}

/// The 15s baseline is what this whole change exists to stop mattering. A
/// one-second burst reads once and then reads zero, rather than decaying
/// across the next seven polls.
#[test]
fn a_burst_does_not_smear_across_later_polls() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
    assert_eq!(app.cpu_history(1), &[50.0, 0.0, 0.0]);
}

/// A sheep with no reading appends a zero rather than a gap: the chart is
/// one cell per sample, and a skipped sample would slide the whole window
/// and make an old spike look recent. The stored reading goes with it, so
/// the next live reading is not differenced across the stop.
#[test]
fn an_unsampled_sheep_appends_a_zero_and_forgets_its_baseline() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
    app.on_snapshot(vec![row_without_cpu(1)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
    assert_eq!(app.cpu_history(1), &[100.0, 0.0]);
}

/// A respawn gives a new tree whose counter starts below the old one's.
/// Clamped to zero, the same rule the daemon applies, and it costs one
/// dropped sample rather than a negative spike.
#[test]
fn a_counter_that_went_backwards_reads_zero() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
    app.on_snapshot(vec![row_with_cpu_ms(1, 12)]);
    assert_eq!(app.cpu_history(1), &[0.0]);
}

/// RSS is sampled at an instant, so it is buffered as it arrives with no
/// differencing at all.
#[test]
fn rss_is_buffered_as_read() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_rss(1, 1_024)]);
    app.on_snapshot(vec![row_with_rss(1, 2_048)]);
    assert_eq!(app.rss_history(1), &[1_024, 2_048]);
}

/// Same depth and same drop-on-leave rule as the CPU buffer.
#[test]
fn a_sheep_that_leaves_takes_its_rss_history_too() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_rss(1, 1_024), row_with_rss(2, 512)]);
    app.on_snapshot(vec![row_with_rss(1, 1_024)]);
    assert!(app.rss_history(2).is_empty());
}
```

Update `the_buffer_holds_at_most_a_hundred_and_forty_samples` and `the_flock_series_is_the_sum_of_the_snapshot` to build rows with a rising `cpu_ms` rather than a fixed `cpu_percent`. Add `row_with_cpu_ms`, `row_without_cpu` and `row_with_rss` beside the existing row helpers.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins rss_history
```

Expected: FAIL, `no method named rss_history`.

- [ ] **Step 3: Add the state**

Beside `cpu_history` at `app.rs:1415`:

```rust
    /// Each sheep's last [`HISTORY`] RSS samples, oldest first, keyed by
    /// [`ProcessInfo::id`], on the same terms as [`Self::cpu_history`].
    ///
    /// Buffered as read rather than differenced: RSS is a reading at an
    /// instant, and only CPU arrives as a counter.
    rss_history: HashMap<u32, VecDeque<u64>>,
    /// The previous CPU counter and the instant it was read, per sheep.
    ///
    /// What makes a sample a mean over one poll rather than over the
    /// shepherd's own baseline window. Dropped when a sheep reports no
    /// reading, so a stop is never differenced across.
    cpu_last: HashMap<u32, (u64, Instant)>,
```

Initialise both to empty at `:1498`.

- [ ] **Step 4: Rewrite the recorder**

Rename `record_cpu_samples` to `record_samples`, take the snapshot's instant, and call it as `self.record_samples(at)` in the `Msg::Snapshot` arm:

```rust
    fn record_samples(&mut self, at: Instant) {
        // Collected first: the differencing below needs `&mut self.cpu_last`
        // while a walk of `self.flock` would still be borrowing it.
        let readings: Vec<(u32, Option<u64>, u64)> = self
            .flock
            .values()
            .map(|row| {
                (
                    row.info.id,
                    row.info.cpu_ms,
                    row.info.memory_bytes.unwrap_or(0),
                )
            })
            .collect();
        let mut sum = 0.0;
        for (id, cpu_ms, rss) in readings {
            let rss_history = self.rss_history.entry(id).or_default();
            rss_history.push_back(rss);
            if rss_history.len() > HISTORY {
                rss_history.pop_front();
            }
            rss_history.make_contiguous();

            let cpu = match cpu_ms {
                None => {
                    self.cpu_last.remove(&id);
                    0.0
                }
                Some(now_ms) => match self.cpu_last.insert(id, (now_ms, at)) {
                    // Nothing behind this reading to difference. The buffer
                    // stays one short of the poll count rather than claiming
                    // an idle sample it never measured.
                    None => continue,
                    Some((then_ms, then)) => shep_core::values::cpu_percent(
                        now_ms.saturating_sub(then_ms),
                        at.saturating_duration_since(then),
                    )
                    .unwrap_or(0.0),
                },
            };
            sum += cpu;
            let history = self.cpu_history.entry(id).or_default();
            history.push_back(cpu);
            if history.len() > HISTORY {
                history.pop_front();
            }
            history.make_contiguous();
        }
        self.cpu_history.retain(|id, _| self.flock.contains_key(id));
        self.rss_history.retain(|id, _| self.flock.contains_key(id));
        self.flock_cpu.push_back(sum);
        if self.flock_cpu.len() > HISTORY {
            self.flock_cpu.pop_front();
        }
        self.flock_cpu.make_contiguous();
    }
```

Add the accessor beside `cpu_history` at `:4093`:

```rust
    /// One sheep's RSS samples in bytes, oldest first, newest last.
    ///
    /// Empty for a sheep with no history yet and for one that has left the
    /// flock, on [`Self::cpu_history`]'s terms.
    #[must_use]
    pub fn rss_history(&self, id: u32) -> &[u64] {
        self.rss_history
            .get(&id)
            .map_or(&[], |series| series.as_slices().0)
    }
```

Read `cpu_history`'s own body at `:4093` and mirror it; the `map_or` above is this plan's guess at its shape.

- [ ] **Step 5: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS. Frame snapshots that show a `CPU 20s` sparkline will move, because the scenes now feed a counter rather than a percent. Read the diff and accept only if the shape is what the scene's own caption claims.

- [ ] **Step 6: Run the mutation check**

Drop the `saturating_sub` and confirm `a_counter_that_went_backwards_reads_zero` fails rather than reporting a huge number. Remove the `cpu_last.remove` on the `None` arm and confirm `an_unsampled_sheep_appends_a_zero_and_forgets_its_baseline` fails. Replace `continue` with `0.0` and confirm `the_first_reading_records_a_baseline_and_no_sample` fails. Restore each.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): difference the CPU counter and buffer RSS"
```

---

## Task 5: Share the sheep field set

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`ConfigPane::sheep` at :803-853)

**Interfaces:**
- Produces: `pane::sheep_fields(config: &AppConfig) -> (FieldSet, Map<String, Value>)`.

- [ ] **Step 1: Write the failing test**

In `crates/shep-cli/src/lookout/pane.rs`'s `mod tests`:

```rust
/// One walk, two screens. The editing pane and the sheep pane's read-only
/// listing must not disagree about group order or about which fields are
/// read-only, and the only way to guarantee that is to build both from
/// this.
#[test]
fn the_shared_field_set_matches_what_the_config_pane_builds() {
    let view = web();
    let (fields, values) = sheep_fields(&view.config);
    let pane = ConfigPane::sheep(view);
    assert_eq!(
        fields.fields().iter().map(|f| f.key.clone()).collect::<Vec<_>>(),
        pane.fields().iter().map(|f| f.key.clone()).collect::<Vec<_>>()
    );
    assert_eq!(values, *pane.values());
}
```

`web()` is the existing fixture at `pane.rs:1704`. `ConfigPane` may not expose `fields()` and `values()` yet; add whatever accessor the test needs rather than making the fields public, and follow the accessor style the pane already uses.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --workspace --all-features --lib --bins sheep_fields
```

Expected: FAIL, `cannot find function sheep_fields`.

- [ ] **Step 3: Extract the function**

Move the body of `ConfigPane::sheep` from the `flockfile_schema_json()` line through the `values` binding into:

```rust
/// The field set and the value map a sheep's config renders as.
///
/// Shared by [`ConfigPane::sheep`] and the sheep pane's read-only listing,
/// so group order and the read-only marking on a `Structural` field cannot
/// differ between the two screens. Built from the Flockfile schema rather
/// than from a second list of names, for the reason
/// [`ConfigPane::sheep`]'s own doc gives.
pub(crate) fn sheep_fields(config: &AppConfig) -> (FieldSet, Map<String, Value>) {
```

Keep the `Structural` comment with the code it explains. `ConfigPane::sheep` then reads:

```rust
    pub fn sheep(view: SheepConfigView) -> Self {
        let (fields, values) = sheep_fields(&view.config);
        Self {
            target: PaneTarget::Sheep { name: view.name },
            fields,
            values,
            env_keys: view.env_keys,
            // ... the rest unchanged
        }
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS, with no snapshot movement. This is a pure extraction; a moved frame means the extraction changed behaviour.

- [ ] **Step 5: Run the mutation check**

Pass `&[]` instead of `GROUP_ORDER` inside `sheep_fields` and confirm the new test fails on order rather than passing because both sides changed together. Restore.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(lookout): share the sheep field set between panes"
```

---

## Task 6: `J` and `K` say what the key is, not what one pane does

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs` (:70-71 and its tests)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`KeyPress` at :117-119, the arms at :2510, :2747, :2877, :3070, :3175, :3547, :3654)

**Interfaces:**
- Produces: `KeyPress::StepUp` and `KeyPress::StepDown`, replacing `ListMoveUp` and `ListMoveDown`.

- [ ] **Step 1: Write the failing test**

In `crates/shep-cli/src/lookout/input.rs`'s `mod tests`:

```rust
/// Named for the key, not for one pane's use of it. `map_key` dispatches
/// on mode rather than on which body is showing, so the body is what
/// decides whether a step reorders a list or walks to the next sheep.
#[test]
fn shift_j_and_shift_k_are_steps() {
    assert_eq!(map_key(&key('J'), InputMode::Normal), Some(KeyPress::StepDown));
    assert_eq!(map_key(&key('K'), InputMode::Normal), Some(KeyPress::StepUp));
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --workspace --all-features --lib --bins shift_j
```

Expected: FAIL, `no variant named StepDown`.

- [ ] **Step 3: Rename**

Rename the two `KeyPress` variants and update every arm the compiler names. Give the pair a doc comment on the enum:

```rust
    /// `K`. What a step means is the body's to decide: a config pane
    /// reorders the list element under the cursor, the sheep pane walks to
    /// the previous sheep.
    StepUp,
    /// `J`, the twin of [`Self::StepUp`].
    StepDown,
```

Nothing else changes. `map_key` still maps one char to one variant and still dispatches on mode.

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS, no snapshot movement.

- [ ] **Step 5: Run the mutation check**

Swap the two mappings in `map_key` and confirm the new test fails naming the wrong direction. Restore.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(lookout): name J and K for the key rather than the pane"
```

---

## Task 7: The pane opens, names its sheep, and closes

**Files:**
- Create: `crates/shep-cli/src/lookout/pane_sheep.rs`
- Create: `crates/shep-cli/src/lookout/view/sheep.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (`Body` at :1315, `Sent::SheepConfig` at :475, `on_sheep_config` at :2054, `close_pane` at :3120, the `Confirm` arm, the `StepUp`/`StepDown` arms from Task 6)
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (the `draw` arms at :228-261)
- Modify: `crates/shep-cli/src/lookout/mod.rs` (module declarations)

**Interfaces:**
- Consumes: `App::rss_history` and `App::cpu_history` from Task 4; `KeyPress::StepUp`/`StepDown` from Task 6.
- Produces: `Body::Sheep(SheepPane)`; `SheepPane::new(sheep: RowKey) -> Self`; `SheepPane::sheep(&self) -> &RowKey`; `SheepPane::config(&self) -> Option<&SheepConfigView>`; `SheepPane::adopt_config(&mut self, view: SheepConfigView)`; `view::sheep::draw(app, frame, area)`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/lookout/app.rs`'s `mod tests`:

```rust
/// `↵` opens the pane on the selected sheep and asks for its config in
/// the same step, since the pane's left column has nothing to draw
/// without it.
#[test]
fn enter_opens_the_sheep_pane_and_asks_for_its_config() {
    let mut app = fixture();
    let effect = app.update(Msg::Key(KeyPress::Confirm));
    assert!(matches!(app.body(), Body::Sheep(_)));
    assert!(matches!(
        effect,
        Effect::Send(Sent::SheepConfig { .. })
    ));
}

/// An armed prompt owns `↵`. Opening a pane out from under a question the
/// operator has not answered would answer it for them.
#[test]
fn enter_confirms_an_armed_action_rather_than_opening_the_pane() {
    let mut app = fixture();
    let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    assert!(matches!(app.body(), Body::FlockTable));
}

/// A dog row has no charts to draw and its config is a TOML section
/// rather than a `SheepConfigView`, so `↵` does nothing there. `e` still
/// opens the dog config pane it opens today.
#[test]
fn enter_on_a_dog_row_opens_nothing() {
    let mut app = fixture_with_a_dog_selected();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    assert!(matches!(app.body(), Body::FlockTable));
}

/// `e` inside the sheep pane opens the editor, not a refill of the pane
/// that asked. Both send `Request::SheepConfig`, so the reply has to say
/// which one it is for.
#[test]
fn e_inside_the_sheep_pane_opens_the_editor() {
    let mut app = fixture();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    let _ = app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Sent { /* the SheepConfig reply, built as the neighbouring tests build one */ });
    assert!(matches!(app.body(), Body::ConfigPane(_)));
}

#[test]
fn escape_closes_the_sheep_pane() {
    let mut app = fixture();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    let _ = app.update(Msg::Key(KeyPress::Escape));
    assert!(matches!(app.body(), Body::FlockTable));
}

/// `J` walks the flock without leaving the pane, and asks for the new
/// sheep's config.
#[test]
fn step_down_moves_to_the_next_sheep() {
    let mut app = fixture();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    let effect = app.update(Msg::Key(KeyPress::StepDown));
    let Body::Sheep(pane) = app.body() else {
        panic!("still in the sheep pane")
    };
    assert_eq!(pane.sheep(), &RowKey::Sheep(2));
    assert!(matches!(effect, Effect::Send(Sent::SheepConfig { .. })));
}
```

Read how the neighbouring tests deliver a `Request` reply before writing the `e` test; the `Msg::Sent` line above is a placeholder for whatever shape they use.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins sheep_pane
```

Expected: FAIL, `no variant named Sheep`.

- [ ] **Step 3: Write the pane**

`crates/shep-cli/src/lookout/pane_sheep.rs`:

```rust
//! The sheep pane: one sheep's histories, config, env keys and feed.
//!
//! The charts read [`App`]'s own ring buffers rather than anything here,
//! so history keeps filling while the pane is shut and a pane opened on a
//! sheep that has been running all along starts full.

/// One sheep, given the whole screen.
#[derive(Debug)]
pub struct SheepPane {
    sheep: RowKey,
    /// `None` until `Request::SheepConfig` answers. The left column draws
    /// its own waiting line rather than an empty group list, which would
    /// read as a sheep with no config at all.
    config: Option<SheepConfigView>,
    feed: BleatsPane,
    /// The config column's scroll, independent of the feed's.
    view: Viewport,
}
```

`SheepConfigView`'s own `Debug` is redacted, so deriving here is safe; add the exact-string test anyway (IR-41), matching `ConfigPane`'s at `pane.rs:786`.

Add the `Body` variant, the `draw` arm, and a `Sent::SheepConfig` discriminator. `on_sheep_config` at `:2054` sets `Body::ConfigPane` unconditionally today; it needs to branch on the discriminator, not on the current body, because `e` pressed inside the pane leaves the body as `Sheep` while the reply is in flight:

```rust
    /// Which screen asked for a sheep's config. Both send the same request
    /// and the reply cannot tell them apart on its own: `e` pressed inside
    /// the sheep pane leaves that pane on screen while the reply is in
    /// flight, so a body check would refill the pane instead of opening
    /// the editor.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ConfigFor {
        /// The editing pane, opened by `e`.
        Editor,
        /// The sheep pane's read-only left column.
        SheepPane,
    }
```

Row 0 is the title band and row 1 the identity band. Reuse what `view/detail.rs` already renders for the selected sheep's facts rather than writing a second version, and append `!N pending` from `config.pending.len()` in butter when it is non-zero.

Row 47 is the status bar: `esc flock`, `e edit`, `b full log`, `J/K next sheep`, `/ filter`, `x stop`, `R restart`, `L reload`, with 1a's right-aligned control marker. Follow `view/status.rs`'s existing construction.

For now `view::sheep::draw` may leave rows 2 to 46 blank. Tasks 8 to 10 fill them.

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 5: Look at the screen**

Use the `tui-screen-capture` skill. Run the binary at 160x48 against a live shepherd, press `↵` on a sheep, and read the screen back. Check the title band is painted across all 160 cells, the identity band's chip is 12 cells, and the status bar's keys are butter. A snapshot test passing here proves the string, not the paint.

- [ ] **Step 6: Run the mutation check**

Make `Confirm` open the pane unconditionally and confirm `enter_confirms_an_armed_action_rather_than_opening_the_pane` fails. Make `on_sheep_config` branch on the body rather than the discriminator and confirm `e_inside_the_sheep_pane_opens_the_editor` fails. Restore both.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): open a pane on one sheep"
```

---

## Task 8: The two charts

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/sheep.rs`
- Modify: `crates/shep-cli/src/lookout/pane_sheep.rs`

**Interfaces:**
- Consumes: `cell::chart` from Task 3; `App::cpu_history` and `App::rss_history` from Task 4.
- Produces: `pane_sheep::window(body_cells: usize) -> Duration`; `pane_sheep::scale_top(peak: f64, floor: f64) -> f64`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/lookout/pane_sheep.rs`'s `mod tests`:

```rust
/// One cell per poll. The frame says six minutes at 5s samples and both
/// halves are wrong: the poll is 2s, so 140 cells is 4m40s.
#[test]
fn the_window_is_one_poll_per_cell() {
    assert_eq!(window(140), Duration::from_secs(280));
    assert_eq!(window(120), Duration::from_secs(240));
}

/// A 1-2-5 ladder so the gutter labels land on round numbers.
#[test]
fn a_scale_top_rounds_up_the_ladder() {
    assert_eq!(scale_top(34.0, 2.0), 50.0);
    assert_eq!(scale_top(6.0, 2.0), 10.0);
    assert_eq!(scale_top(1.2, 2.0), 2.0);
}

/// Floored, so a flock genuinely doing nothing stays flat instead of
/// having its rounding noise stretched into a shape.
#[test]
fn a_scale_top_never_falls_below_its_floor() {
    assert_eq!(scale_top(0.01, 2.0), 2.0);
}
```

In `view/sheep.rs`'s `mod tests`:

```rust
/// The header states the window it actually drew, not a literal. At two
/// widths, because a literal passes at one of them.
#[test]
fn the_cpu_header_states_the_drawn_window() {
    assert!(header_at(160).contains("4m40s, one 2s sample per column"));
    assert!(header_at(140).contains("4m00s, one 2s sample per column"));
}

/// The buffer starts empty on every launch and dies with the process, so
/// a chart that is not yet full says how full it is.
#[test]
fn a_partial_buffer_says_how_much_it_has() {
    assert!(header_with_samples(35, 140).contains("collecting · 1m10s of 4m40s"));
}

/// With a limit set the ceiling is the limit, drawn as its own row and
/// labelled.
#[test]
fn the_memory_chart_labels_a_real_ceiling() {
    let rows = mem_rows_with_limit(Some(52 << 20));
    assert!(rows.iter().any(|row| row.contains("ceiling")));
}

/// With no limit there is no ceiling row and the header says what it
/// scaled to instead, per the design rule that every measurement states
/// its denominator.
#[test]
fn the_memory_chart_states_its_substitute_denominator() {
    let header = mem_header_with_limit(None, 48 << 20);
    assert!(header.contains("no limit set"));
    assert!(header.contains("scaled to peak"));
    assert!(!mem_rows_with_limit(None).iter().any(|row| row.contains("ceiling")));
}
```

Write `header_at`, `header_with_samples`, `mem_rows_with_limit` and `mem_header_with_limit` as thin local helpers over the real rendering functions. Do not assert against a string the test itself built.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins window
```

Expected: FAIL, `cannot find function window`.

- [ ] **Step 3: Implement**

```rust
/// The span a chart of `body_cells` covers, one poll per cell.
///
/// Computed rather than written into a label: the body is
/// `min(width - 20, HISTORY)`, so the window is 4m40s at 160 columns and
/// 4m00s at 140, and any literal would be wrong at one of them.
#[must_use]
pub fn window(body_cells: usize) -> Duration {
    FLOCK_POLL * body_cells as u32
}

/// `peak` rounded up a 1-2-5 ladder, never below `floor`.
///
/// The ladder is what puts the gutter labels on round numbers. The floor
/// is [`CPU_CEILING_FLOOR`]'s reason in a second place: below it there is
/// nothing to see, and saying so is the honest answer.
#[must_use]
pub fn scale_top(peak: f64, floor: f64) -> f64 {
    let peak = peak.max(floor);
    let decade = 10f64.powf(peak.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        let candidate = step * decade;
        if candidate >= peak {
            return candidate;
        }
    }
    10.0 * decade
}
```

Rows 2 to 17, per the spec's row table. The CPU chart is 8 rows at `scale_top(window peak, CPU_CEILING_FLOOR)`; the memory chart is 5 rows at `scale_top` over `max(max_memory, window peak)` in bytes, with the row nearest `max_memory` drawn `╌` in butter and labelled `ceiling` in the right margin. Both bodies are the same width over the same window, so a memory step and a CPU spike line up vertically. **That alignment is why this frame was picked over side-by-side charts. Do not let the two charts take different widths.**

Row 17 is the shared axis, `now` ending on the last column.

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 5: Look at the screen**

Use `tui-screen-capture` at 160x48 and again at 140x48. Check that the two chart bodies start and end on the same column, that the memory ceiling row is dashed rather than solid, and that the axis `now` lands on the last cell rather than one short of it. Then run with `NO_COLOR=1` and check the charts still read: no cell may be colour-only.

- [ ] **Step 6: Run the mutation check**

Hard-code the header's window as `4m40s` and confirm the 140-column assertion fails. Give the memory chart a body one cell narrower than the CPU chart and confirm something fails; if nothing does, the alignment is untested and needs its own assertion. Restore.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): draw the sheep pane's CPU and memory charts"
```

---

## Task 9: The config and env column

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/sheep.rs`
- Modify: `crates/shep-cli/src/lookout/pane_sheep.rs`

**Interfaces:**
- Consumes: `pane::sheep_fields` from Task 5; `SheepConfigView::env_secrets` from Task 2.

- [ ] **Step 1: Write the failing tests**

```rust
/// Eight groups, in the schema's own order. The frame lists seven and
/// puts `restart` second; `cron` is missing from it entirely.
#[test]
fn the_groups_are_the_schemas_eight_in_its_own_order() {
    let labels = group_labels_of(&web_view());
    assert_eq!(
        labels,
        [
            "process", "logging", "inputs", "restart", "readiness", "shutdown", "watch", "cron"
        ]
    );
}

/// A field parked until the next respawn is marked and says so.
#[test]
fn a_pending_field_is_marked_and_annotated() {
    let row = field_row_of(&web_view(), "max_memory");
    assert!(row.starts_with('!'));
    assert!(row.contains("awaits respawn"));
}

/// The wire clears env before the struct is built, so no pane can show a
/// value. This test exists so a later change that starts carrying them
/// fails here rather than shipping.
#[test]
fn no_env_value_reaches_the_column() {
    let rendered = env_rows_of(&web_view()).join("");
    assert!(!rendered.contains("hunter2"));
}

/// A key the store fills renders sealed; a Flockfile key does not.
#[test]
fn a_sealed_key_is_marked_and_a_plain_one_is_not() {
    let rows = env_rows_of(&view_with_env(&[("PLAIN", "v"), ("SEALED", "{{secret:PW}}")]));
    assert!(row_for("SEALED", &rows).contains("sealed"));
    assert!(!row_for("PLAIN", &rows).contains("sealed"));
}

/// Until the reply lands there is no config, and an empty group list
/// would read as a sheep that has none.
#[test]
fn a_pane_without_its_config_yet_says_so() {
    assert!(column_of(None).contains("reading config"));
}
```

`web_view()` builds a `SheepConfigView` with a pending field and an env map; `secrets.rs:808` and `pane.rs:1704` both show the shape.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins env_rows
```

Expected: FAIL, `cannot find function env_rows_of`.

- [ ] **Step 3: Implement**

Rows 19 to 45, left 76 cells. Row 19 is the header, `██ CONFIG & ENV   e edit  tab next group  N pending`. Below it, `sheep_fields` grouped output with a blank row and a `─` rule in the line colour above each group label. A field row is the name in ink-3, the value in ink-2, and a right-aligned ink-3 note where one helps. A field in `view.pending` is prefixed `!`, rendered butter, and carries `awaits respawn` on the right. `(unset)` and `(default)` are ink-3.

The env section closes the column: each key from `view.env_keys`, and a key also in `view.env_secrets` draws a butter block run, the word `sealed`, and `edit in S`. No values, ever.

`j` and `k` scroll this column through the pane's own `Viewport`; `g` and `G` jump to its ends.

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 5: Look at the screen**

Use `tui-screen-capture` at 160x48 on a sheep with a pending field and a sealed env key. Check the column stops at 76 cells and the divider sits at column 77, the group rules span the column rather than the screen, and the sealed run is blocks rather than characters.

- [ ] **Step 6: Run the mutation check**

Put a real value in the env rows and confirm `no_env_value_reaches_the_column` fails. Drop `cron` from the expected order and confirm the group test fails rather than passing on a prefix match. Restore.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): draw the sheep pane's config and env column"
```

---

## Task 10: The feed column

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/sheep.rs`
- Modify: `crates/shep-cli/src/lookout/pane_sheep.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (the feed's key arms)

**Interfaces:**
- Consumes: `BleatsPane` from `crates/shep-cli/src/lookout/pane_bleats.rs:238`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The pane's own filters, not a second set. The header advertises them,
/// so they have to work here and not only in the full-screen pane.
#[test]
fn a_filter_applied_in_the_sheep_pane_narrows_its_feed() {
    let mut app = fixture_with_feed();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    apply_match(&mut app, "boom");
    assert!(feed_rows(&app).iter().all(|row| row.contains("boom")));
}

/// `b` hands the same pane the whole screen, carrying its filters.
#[test]
fn b_promotes_the_feed_to_full_screen_with_its_filters() {
    let mut app = fixture_with_feed();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    apply_match(&mut app, "boom");
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let Body::Bleats(pane) = app.body() else {
        panic!("full screen")
    };
    assert_eq!(pane.match_filter(), Some("boom"));
}

/// Stepping to another sheep re-scopes the feed. A feed left on the
/// previous sheep under a new title is worse than an empty one.
#[test]
fn stepping_re_scopes_the_feed() {
    let mut app = fixture_with_feed();
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    let _ = app.update(Msg::Key(KeyPress::StepDown));
    let Body::Sheep(pane) = app.body() else {
        panic!("still in the sheep pane")
    };
    assert_eq!(pane.feed_sheep(), &RowKey::Sheep(2));
}
```

`match_filter` and `feed_sheep` may not exist; add whatever accessor each test needs, in the style `BleatsPane` already uses.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins feed
```

Expected: FAIL.

- [ ] **Step 3: Implement**

Rows 19 to 45, right 83 cells, with the divider at column 77. Row 19 is the `BLEATS` chip plus the header the pane already knows how to state: `out then err · N earlier · [level≥warn] · / narrow`. Every count is scoped to the window the tail reader holds, never to the file.

The embedded pane owns `/`, `o`, `m`, `f`, `w`, `n` and `N` while the sheep pane is up. `b` moves the same pane to `Body::Bleats`, carrying its filters rather than rebuilding it.

- [ ] **Step 4: Run the tests**

```bash
cargo test --workspace --all-features --lib --bins
```

Expected: PASS.

- [ ] **Step 5: Look at the screen**

Use `tui-screen-capture` at 160x48 with a sheep writing to both streams. Check the feed wraps inside 83 cells rather than pushing the divider, and that a long line is truncated rather than reflowing the column.

- [ ] **Step 6: Run the mutation check**

Leave the feed scoped to the old sheep on a step and confirm `stepping_re_scopes_the_feed` fails. Rebuild the pane on `b` instead of carrying it and confirm the filter test fails. Restore.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): embed the bleats feed in the sheep pane"
```

---

## Task 11: The responsive ladder and the gallery scenes

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/sheep.rs`
- Modify: `crates/shep-cli/src/lookout/frames.rs`

**Interfaces:**
- Produces: a `Scene` per tier in `Scene::ALL`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The arithmetic, asserted rather than trusted:
///     160 = 8 gutter + 140 body + 12 margin
///     body = min(width - 20, HISTORY)
/// A scene one cell short of its own column set silently drops the thing
/// it exists to show, which is why this is a test and not a comment.
#[test]
fn the_chart_body_is_the_width_less_its_gutter_and_margin() {
    assert_eq!(chart_body(160), 140);
    assert_eq!(chart_body(140), 120);
}

/// Past the design target the buffer runs out before the columns do, so
/// the margin grows rather than leaving cells that can never fill.
#[test]
fn a_wider_terminal_grows_the_margin_rather_than_the_body() {
    assert_eq!(chart_body(200), 140);
}

/// Memory goes first and CPU stays: a CPU chart is the more diagnostic of
/// the two, and memory still has a gauge to fall back on.
#[test]
fn below_a_hundred_and_forty_columns_only_the_cpu_chart_draws() {
    let rendered = render_at(139, 48);
    assert!(rendered.contains("██ CPU"));
    assert!(rendered.contains("rss "));
    assert!(!rendered.contains("██ MEM"));
}

/// Below 100 both go and the pane falls back to 1a's pair.
#[test]
fn below_a_hundred_columns_both_charts_become_the_sparkline_pair() {
    let rendered = render_at(99, 48);
    assert!(!rendered.contains("██ CPU"));
    assert!(!rendered.contains("██ MEM"));
    assert!(rendered.contains("CPU 20s"));
}

/// Rows too: the charts hold 2 to 17, and the config and feed columns are
/// what the pane is for, so they give ground last.
#[test]
fn a_short_terminal_drops_the_charts_before_the_columns() {
    assert!(!render_at(160, 25).contains("██ MEM"));
    assert!(render_at(160, 25).contains("██ CONFIG & ENV"));
    assert!(!render_at(160, 19).contains("██ CPU"));
    assert!(render_at(160, 19).contains("██ CONFIG & ENV"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins chart_body
```

Expected: FAIL, `cannot find function chart_body`.

- [ ] **Step 3: Implement and add the scenes**

One scene per tier, each with its arithmetic in the caption and its width asserted:

| Scene | Size | Shows |
|---|---|---|
| `SheepPane` | 160x48 | both charts, both columns, the full status bar |
| `SheepPaneCpuOnly` | 139x48 | the CPU chart and the memory gauge line |
| `SheepPaneSparklines` | 99x48 | 1a's pair |
| `SheepPaneShort` | 160x25 | the CPU chart, no memory chart, both columns |

Each scene's fixture carries enough history to draw a shape rather than one repeated bar, the way `frames.rs:886` sets `web` up for the sparkline. A scene whose sheep has one sample proves nothing about a chart.

- [ ] **Step 4: Run the tests and regenerate the gallery**

```bash
cargo test --workspace --all-features --lib --bins
```

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

Expected: PASS, and `docs/lookout/frames.txt` gains the four scenes.

- [ ] **Step 5: Look at every tier**

Use `tui-screen-capture` at 160x48, 139x48, 99x48 and 160x25. The gallery renders through one dump; the pty renders through a real terminal, and the two disagree exactly where a bug lives.

- [ ] **Step 6: Run the mutation check**

Change one scene's width to 139 and confirm its own width assertion fails rather than the scene quietly dropping a chart. Restore.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(lookout): degrade the sheep pane by width and height"
```

---

## Task 12: Docs

**Files:**
- Modify: `web/src/pages/docs/*.astro` (whichever pages name the JSON envelope's fields or the lookout keys)
- Modify: `web/src/content/**` CLI reference, regenerated
- Modify: `docs/lookout/README.md`

**Interfaces:** none.

- [ ] **Step 1: Regenerate the CLI reference**

```bash
cargo build --release
```

```bash
./web/scripts/generate-cli-reference.sh
```

Read `git diff` afterwards. A stale copy fails no build, which is exactly why it drifts.

- [ ] **Step 2: Find the prose that is now wrong**

```bash
grep -rn "cpu_percent\|memory_bytes" web/src/pages/docs/
```

```bash
grep -rn "lookout" web/src/pages/docs/ | grep -i "key\|press"
```

Two things changed that an operator can see: `shep list --json` gained `cpu_ms`, and lookout gained a pane with `↵`, `J` and `K`. Update the pages that state either. `docs/lookout/README.md`'s key list needs the same pass.

- [ ] **Step 3: Build and check the site**

```bash
cd web && npx astro build
```

```bash
cd web && npx astro check
```

Both. `astro build` does not typecheck, so a page passing a component a prop it does not have builds clean and renders wrong; `check` is what catches it.

- [ ] **Step 4: Run the task gate**

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

```bash
cargo test --workspace --all-features
```

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

One at a time, each from its own command, `$?` captured directly and never through a pipe.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "docs(lookout): document the sheep pane and the CPU counter field"
```

---

## Self-review

**Spec coverage.** Decision 1 is Tasks 1 and 4. Decision 2 is Task 8's `window` and its two-width header test. Decision 3 is Task 8's `scale_top` and the two memory-ceiling tests. Decision 4 is Task 5. Decision 5 is Tasks 2 and 9. Decision 6 is Task 10. Decision 7 is Tasks 6 and 7. Decision 8 is Task 11. The row table and status bar are Task 7. The wire section is Tasks 1 and 2. The docs section is Task 12. Out of scope stays out: no `w` key, no secrets jump, no dog variant, and Task 7's `enter_on_a_dog_row_opens_nothing` pins that last one rather than leaving it to be noticed.

**Type consistency.** `cpu_ms` is `Option<u64>` on `ProcessInfo` and plain `u64` on `SheepStats`, deliberately: the daemon always has a reading for a watched pid, the wire does not. `cpu_percent` is `Option<f32>` in both `shep_core::values` and the daemon's private wrapper. `sealed_keys` and `env_secrets` are both `Vec<String>` of env keys, never of references. `window` takes cells and returns a `Duration`; `chart_body` takes a width and returns cells.

**Where this plan is guessing.** Task 1's `StatsState` construction, Task 2's `AppConfig` fixture, Task 4's `rss_history` body, Task 5's `ConfigPane` accessors, and Task 7's reply-delivery shape are written from greps rather than from reading the surrounding code. Each says so at the point it guesses. Read the neighbours and follow them; do not make the file match this plan.
