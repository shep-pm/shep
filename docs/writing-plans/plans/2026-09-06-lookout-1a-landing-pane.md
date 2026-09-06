# Lookout landing pane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redraw `shep lookout`'s landing pane with mode bands, a fourteen-column table carrying a CPU sparkline and a memory gauge, and the shared cell functions every other redesigned pane will borrow.

**Architecture:** Two new foundations first, `theme.rs` gaining background and reverse-video styles plus four roles, and a new `view/cell.rs` holding four pure string builders. Then one additive wire field, then a per-sheep sample buffer on the existing poll, then the four regions of the pane in turn. Nothing here adds a `Request`, an overlay, or a ratatui widget: the module builds `Line`/`Span` by hand on purpose and this plan keeps doing that.

**Tech Stack:** Rust 2024, MSRV 1.88, ratatui 0.30.2 with the crossterm backend, `unicode-width` 0.2 for column measurement, `insta` for snapshots.

**Spec:** [docs/brainstorming/specs/2026-09-06-lookout-1a-landing-pane-design.md](../../brainstorming/specs/2026-09-06-lookout-1a-landing-pane-design.md)

## Global Constraints

- **Read the spec before task 1.** Every decision below is argued there and the "Why" blocks are not repeated here.
- **The frames are not the spec.** `docs/lookout/design-files/` is a concept bundle. Where it and the spec disagree, build the spec. `docs/lookout/design-files/rulings.md` lists the places the frames are wrong about shipped shep.
- **Code snippets describing existing code in this plan are the plan author's reading, not a quotation.** Grep and read the real file before editing. If a snippet disagrees with the file, the file wins: say so in your report rather than making the file match the plan.
- **Inner loop:** `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`. One cargo shape for the whole task; do not alternate with `--workspace`. Tasks 3 and 4 touch other crates and name their own shape.
- **Task gate**, once per task, each command separately with `$?` captured directly, never through a pipe: `cargo fmt --all --check`, then `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- **Conventional commit subjects**, `type(scope): summary`, with `!` on anything breaking. release-plz drops what does not parse and `.github/workflows/commits.yml` gates it.
- **Every new public item needs docs and a deliberate `Debug` decision.** `#![forbid(unsafe_code)]` is live in shep-cli and shep-core.
- **Invoke the `shep-idiomatic-rust` skill before writing any Rust here.** Cite rules as `IR-<n>` in review.
- **Never write an absolute home directory path** into a file, a comment or a commit message.
- Column widths, thresholds and sample counts in this plan are exact. Do not round them.

---

### Task 1: the palette learns to paint

**Files:**
- Modify: `crates/shep-cli/src/lookout/theme.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Palette::sky() -> Style`, `Palette::line() -> Style`, `Palette::gauge_rest() -> Style`, `Palette::band(Role) -> Style`, `Palette::ground() -> Style`. `Role` is the existing `crate::vocabulary::Role`, extended with a `Sky` variant.

The struct today holds four `Option<Color>` fields and its only style constructor is `fn fg(Option<Color>) -> Style` at theme.rs:133. It gains `sky`, `line`, `paper2` and `gauge_rest`, and two constructors beside `fg`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `theme.rs`:

```rust
#[test]
fn the_deep_tier_carries_every_new_role() {
    let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
    assert_eq!(deep.sky(), Style::default().fg(Color::Indexed(74)));
    assert_eq!(deep.line(), Style::default().fg(Color::Indexed(238)));
    assert_eq!(deep.gauge_rest(), Style::default().fg(Color::Indexed(236)));
}

#[test]
fn the_shallow_tier_names_the_new_roles_too() {
    let shallow = Palette::detect(None, Some(OsStr::new("dumb")), None);
    assert_eq!(shallow.sky(), Style::default().fg(Color::Blue));
    assert_eq!(shallow.line(), Style::default().fg(Color::DarkGray));
    assert_eq!(shallow.gauge_rest(), Style::default().fg(Color::DarkGray));
}

#[test]
fn a_band_is_reverse_video_and_never_names_a_background() {
    let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
    let band = deep.band(crate::vocabulary::Role::Meadow);
    assert_eq!(band.fg, Some(Color::Indexed(29)));
    assert_eq!(band.bg, None, "a band names no background: the terminal supplies the text colour");
    assert!(band.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn no_color_drops_the_colour_and_keeps_the_reverse() {
    let off = Palette::detect(Some(OsStr::new("1")), None, None);
    let band = off.band(crate::vocabulary::Role::Meadow);
    assert_eq!(band.fg, None);
    assert_eq!(band.bg, None);
    assert!(
        band.add_modifier.contains(Modifier::REVERSED),
        "NO_COLOR is about colour; a band still has to name the mode"
    );
    assert_eq!(off.ground(), Style::default(), "no painted ground without colour");
    assert_eq!(off.sky(), Style::default());
}

#[test]
fn a_ground_is_the_one_painted_background() {
    let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
    let ground = deep.ground();
    assert_eq!(ground.bg, Some(Color::Indexed(235)));
    assert_eq!(ground.fg, None, "the row's own cells keep their own foreground");
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- theme:: --skip ::slow::
```

Expected: compile failure, no method `sky` on `Palette`.

- [ ] **Step 3: Add the roles and the two constructors**

`Role` in `crates/shep-cli/src/vocabulary.rs` gains a `Sky` variant. Nothing maps a `ProcStatus` to it, so `role_of` is untouched; it exists so `band` and the gauge can name a role rather than a colour.

In `theme.rs`, add four fields to the struct and set them in all three arms of `detect`. `NO_COLOR` sets every one to `None`. The deep arm uses `Indexed(74)` for sky, `Indexed(238)` for line, `Indexed(235)` for paper-2 and `Indexed(236)` for the gauge tail. The shallow arm uses `Color::Blue` for sky and `Color::DarkGray` for both line and the gauge tail, and leaves `paper2` as `None`, since the 16-colour set has no quiet dark ground and a `Black` background is wrong on a light terminal.

```rust
/// Reverse video over a role's own colour, naming no background.
///
/// The terminal supplies the text colour from its own background, so a
/// light terminal is right without a second palette. `REVERSED` survives
/// `NO_COLOR`: it is a modifier rather than a colour, and it is what keeps
/// a band naming the mode on a monochrome terminal.
#[must_use]
pub fn band(self, role: crate::vocabulary::Role) -> Style {
    Self::fg(self.of(role)).add_modifier(Modifier::REVERSED)
}

/// The one painted background: the selected row and the status bar.
///
/// Reverses the module doc's rule for exactly two rows. Ordinary ground
/// still stays [`Color::Reset`], so the operator's own background shows
/// through everywhere else. `None` under `NO_COLOR` and on the
/// 16-colour tier, where the callers fall back to the ASCII marker.
#[must_use]
pub fn ground(self) -> Style {
    self.paper2
        .map_or_else(Style::default, |colour| Style::default().bg(colour))
}
```

`of(Role) -> Option<Color>` is the existing `role_style`'s match, extracted so both it and `band` share one mapping.

Rewrite the module doc's `--paper` paragraph to say what is painted now and why ordinary ground still is not.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- theme:: --skip ::slow::
```

Expected: PASS, including the four pre-existing tests.

- [ ] **Step 5: Extend the anstyle cross-check**

`the_anstyle_binding_agrees_with_this_ones_colours` currently walks the four roles. Add sky to whatever list it iterates, or, if the CLI's `anstyle` binding has no sky, assert in one line that it does not and say why in a comment: sky is a lookout-only role because `shep flock` has no memory gauge to colour.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/theme.rs crates/shep-cli/src/vocabulary.rs
git commit -m "feat(lookout): give the palette a band, a ground and four roles"
```

---

### Task 2: the four shared cell functions

**Files:**
- Create: `crates/shep-cli/src/lookout/view/cell.rs`
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (add `pub mod cell;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn gauge(value: u64, ceiling: Option<u64>, cells: usize) -> String`
  - `pub fn sparkline(samples: &[f32], cells: usize) -> String`
  - `pub fn rule(cells: usize) -> String`
  - `pub fn band(label: &str, cells: usize) -> String`

Pure `String` builders, no ratatui types, so every one is testable against a literal. This follows the module's stated convention of hand-building text rather than reaching for `ratatui::widgets::Sparkline` or `Gauge`, which exist at 0.30.2 and which this module deliberately never uses.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gauge_fills_from_the_left_and_pads_with_the_light_shade() {
        assert_eq!(gauge(0, Some(100), 10), "░░░░░░░░░░");
        assert_eq!(gauge(50, Some(100), 10), "█████░░░░░");
        assert_eq!(gauge(100, Some(100), 10), "██████████");
    }

    #[test]
    fn a_gauge_over_its_ceiling_is_full_rather_than_wider() {
        assert_eq!(gauge(250, Some(100), 10), "██████████");
    }

    #[test]
    fn a_gauge_with_no_ceiling_is_all_tail() {
        assert_eq!(gauge(48, None, 10), "░░░░░░░░░░");
    }

    #[test]
    fn a_zero_ceiling_is_all_tail_rather_than_a_division_by_zero() {
        assert_eq!(gauge(48, Some(0), 10), "░░░░░░░░░░");
    }

    #[test]
    fn a_gauge_rounds_to_the_nearest_cell() {
        // 4 of 10 cells: 44% rounds down, 45% rounds up.
        assert_eq!(gauge(44, Some(100), 10), "████░░░░░░");
        assert_eq!(gauge(45, Some(100), 10), "█████░░░░░");
    }

    #[test]
    fn a_sparkline_is_one_cell_per_sample_scaled_to_its_own_peak() {
        assert_eq!(sparkline(&[0.0, 50.0, 100.0], 3), "▁▅█");
    }

    #[test]
    fn a_sparkline_shorter_than_its_cells_pads_on_the_left() {
        assert_eq!(sparkline(&[100.0], 4), "   █");
    }

    #[test]
    fn a_sparkline_longer_than_its_cells_keeps_the_newest() {
        assert_eq!(sparkline(&[100.0, 0.0, 0.0], 2), "▁▁");
    }

    #[test]
    fn an_empty_sparkline_is_blank_rather_than_a_flat_line() {
        // A flat line at the floor reads as measured-and-idle. Blank reads
        // as no data yet, which is what twenty seconds after start is.
        assert_eq!(sparkline(&[], 4), "    ");
    }

    #[test]
    fn a_flat_sparkline_sits_at_the_floor() {
        assert_eq!(sparkline(&[0.0, 0.0, 0.0], 3), "▁▁▁");
    }

    #[test]
    fn a_rule_is_exactly_its_cells() {
        assert_eq!(rule(4), "────");
        assert_eq!(rule(0), "");
    }

    #[test]
    fn a_band_marks_its_label_and_pads_to_width() {
        assert_eq!(band("FLOCK", 20), " ██ FLOCK           ");
    }

    #[test]
    fn a_band_narrower_than_its_label_truncates_rather_than_overflowing() {
        assert_eq!(band("FLOCK", 6).chars().count(), 6);
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::cell --skip ::slow::
```

Expected: compile failure, no module `cell`.

- [ ] **Step 3: Write the four functions**

```rust
//! The shared cells the redesigned panes draw magnitude with.
//!
//! Every one returns a `String` rather than a `Line` or a `Span`: the
//! caller owns the styling, and a pure string is testable against a
//! literal the way `flock::mark` already is.
//!
//! Deliberately not `ratatui::widgets::Sparkline` or `Gauge`. This module
//! builds its rows by hand so column widths stay fixed and a row stays a
//! string a test can assert on, which those widgets take away.

/// The eight sparkline steps, low to high.
const STEPS: [char; 8] = ['\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}',
                          '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];

/// A horizontal bar of `cells`, filled in proportion to `value` over
/// `ceiling` and padded with the light shade.
///
/// `None`, zero, and any ceiling a value cannot be measured against give
/// an all-tail bar: the bar says "no ceiling set" rather than guessing a
/// denominator. A value above its ceiling fills the bar rather than
/// overflowing the column.
#[must_use]
pub fn gauge(value: u64, ceiling: Option<u64>, cells: usize) -> String {
    let filled = match ceiling {
        Some(ceiling) if ceiling > 0 => {
            let scaled = (value as f64 / ceiling as f64 * cells as f64).round();
            (scaled as usize).min(cells)
        }
        _ => 0,
    };
    let mut out = String::with_capacity(cells * 3);
    out.extend(std::iter::repeat_n('\u{2588}', filled));
    out.extend(std::iter::repeat_n('\u{2591}', cells - filled));
    out
}

/// The newest `cells` samples, one cell each, scaled to the window's own
/// peak.
///
/// Padded on the left with spaces when there are fewer samples than cells,
/// so the line grows into the column from the right as history arrives.
/// No samples at all is blank rather than a flat line at the floor: a flat
/// line reads as measured and idle, and blank reads as not measured yet.
#[must_use]
pub fn sparkline(samples: &[f32], cells: usize) -> String {
    if samples.is_empty() || cells == 0 {
        return " ".repeat(cells);
    }
    let window = &samples[samples.len().saturating_sub(cells)..];
    let peak = window.iter().copied().fold(0.0_f32, f32::max);
    let mut out = String::with_capacity(cells * 3);
    for _ in window.len()..cells {
        out.push(' ');
    }
    for sample in window {
        let step = if peak <= 0.0 {
            0
        } else {
            let scaled = (sample / peak * (STEPS.len() - 1) as f32).round();
            (scaled as usize).min(STEPS.len() - 1)
        };
        out.push(STEPS[step]);
    }
    out
}

/// A rule of exactly `cells` box-drawing horizontals.
#[must_use]
pub fn rule(cells: usize) -> String {
    "\u{2500}".repeat(cells)
}

/// A section band: the two-block marker, the label, and padding to `cells`.
///
/// The caller styles it; this only lays it out. Truncates rather than
/// overflowing, since a band that runs past its `Rect` shifts the row.
#[must_use]
pub fn band(label: &str, cells: usize) -> String {
    let head = format!(" \u{2588}\u{2588} {label}");
    let drawn = crate::output::width::char_columns(&head);
    if drawn >= cells {
        return super::flock::fit(&head, cells as u16);
    }
    let mut out = head;
    out.extend(std::iter::repeat_n(' ', cells - drawn));
    out
}
```

Write `sparkline`, `rule` and `band` bodies to satisfy the tests. Measure every truncation with `crate::output::width::char_columns`, never `chars().count()`, since the block glyphs are East-Asian Ambiguous and `char_columns` is what the rest of the module uses.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::cell --skip ::slow::
```

Expected: PASS, 12 tests.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/view/cell.rs crates/shep-cli/src/lookout/view/mod.rs
git commit -m "feat(lookout): add the shared gauge, sparkline, rule and band cells"
```

---

### Task 3: the memory ceiling joins the wire

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (the `ProcessInfo` struct and its builder)
- Modify: whichever daemon site builds a `ProcessInfo`; find it with `rg 'ProcessInfo::builder' crates/shep-daemon/src`
- Modify: `crates/shep-core/src/protocol/snapshots/*_wire_v4.snap` (regenerated, not hand-edited)

**Interfaces:**
- Consumes: nothing.
- Produces: `ProcessInfo.max_memory: Option<u64>`, bytes, and `ProcessInfoBuilder::max_memory(Option<u64>)`.

**Cargo shape for this task:** `cargo test --workspace --all-features -- --skip ::slow::`. It crosses three crates, so use the workspace shape throughout and do not add a `-p` run.

- [ ] **Step 1: Write the failing test**

In `crates/shep-core/src/protocol/request.rs`'s test module:

```rust
#[test]
fn a_process_info_carries_its_memory_ceiling_and_defaults_to_none() {
    let plain = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    assert_eq!(plain.max_memory, None, "a sheep with no ceiling reports none");

    let capped = ProcessInfo::builder(2, "hungry", ProcStatus::Online)
        .max_memory(Some(52 * 1024 * 1024))
        .build();
    assert_eq!(capped.max_memory, Some(54_525_952));
}

#[test]
fn an_older_daemons_process_info_still_decodes() {
    // The field is additive, so a payload written before it existed has to
    // decode with the ceiling absent rather than fail the whole envelope.
    let older = r#"{"id":1,"name":"web","status":"online","restarts":0,"uptime_ms":0}"#;
    let info: ProcessInfo = serde_json::from_str(older).expect("an older payload decodes");
    assert_eq!(info.max_memory, None);
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test --workspace --all-features -- a_process_info_carries_its_memory_ceiling --skip ::slow::
```

Expected: compile failure, no field `max_memory`.

- [ ] **Step 3: Add the field**

```rust
/// The sheep's `max_memory` ceiling in bytes, when it has one.
///
/// Additive, like [`Self::instance`] and [`Self::handshook`] before it, so
/// neither `PROTOCOL_VERSION` nor `SCHEMA_VERSION` moves: an older payload
/// decodes with it absent and an older client ignores it. Lookout's
/// `MEM/CEIL` gauge is the only reader; `None` draws an all-tail bar
/// rather than guessing a denominator.
pub max_memory: Option<u64>,
```

No `#[serde(default)]` and no `skip_serializing_if`: not one of `ProcessInfo`'s fifteen other `Option` fields carries either, and serde already decodes a missing `Option` as `None`, which is measured rather than assumed (see the preflight ruling in the ledger). An attribute here would make this field the odd one out and change its wire shape against its siblings.

Add the matching builder method, and add the field to the builder's defaults beside the other `None`s. Then populate it at the daemon's construction site from the app's `AppConfig::max_memory`, converting `MemSize` to bytes with whatever accessor that newtype already offers; grep for how the memory-limit enforcer reads it rather than inventing a conversion.

- [ ] **Step 4: Run the tests and regenerate the snapshots**

```bash
cargo test --workspace --all-features -- --skip ::slow::
```

Two snapshots will fail. Review the diff, confirm the only change is the new key, and accept:

```bash
cargo insta accept
```

Then re-run the same command and confirm green. If `cargo insta` is not installed, the `.snap.new` files are written beside the originals and can be renamed by hand after reading them.

- [ ] **Step 5: Check the constants did not move**

```bash
rg 'PROTOCOL_VERSION: u32|SCHEMA_VERSION' crates/shep-core/src
```

Expected: `PROTOCOL_VERSION` still 4. Neither constant changes in this task. If a test tells you to bump one, stop and report it rather than bumping.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-core crates/shep-daemon
git commit -m "feat(core): carry a sheep's memory ceiling on ProcessInfo"
```

---

### Task 4: the sample buffer

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (the `App` struct, and the `Msg::Snapshot` arm near app.rs:1335)

**Interfaces:**
- Consumes: nothing.
- Produces: `App::cpu_history(&self, id: u32) -> &[f32]` and `App::flock_cpu_history(&self) -> &[f32]`, both newest-last.

- [ ] **Step 1: Write the failing tests**

`app.rs`'s test module already builds `App` values somewhere. Find its constructor first and reuse it rather than adding a second one:

```bash
rg -n 'fn .*\bApp\b.*->|App::new|fn fixture' crates/shep-cli/src/lookout/app.rs | head -20
```

Then add two helpers beside the tests, named for what they make:

```rust
/// One snapshot row for `id`, reporting `cpu` percent.
fn row_with_cpu(id: u32, cpu: f32) -> ProcessInfo {
    ProcessInfo::builder(id, &format!("sheep-{id}"), ProcStatus::Online)
        .cpu_percent(Some(cpu))
        .build()
}

/// The same row with no CPU reading, which is what a stopped sheep sends.
fn row_without_cpu(id: u32) -> ProcessInfo {
    ProcessInfo::builder(id, &format!("sheep-{id}"), ProcStatus::Stopped).build()
}
```

`on_snapshot` in the tests below stands for however the module already drives
`Msg::Snapshot` in its own tests. Find that too, in the same grep, and call it
rather than reaching into the field directly: the point of these tests is that
the poll populates the buffer.

```rust
#[test]
fn a_snapshot_appends_one_sample_per_sheep() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu(1, 10.0)]);
    app.on_snapshot(vec![row_with_cpu(1, 20.0)]);
    assert_eq!(app.cpu_history(1), &[10.0, 20.0]);
}

#[test]
fn a_sheep_with_no_cpu_reading_appends_a_zero_rather_than_a_gap() {
    // The sparkline is one cell per sample; a skipped sample would slide
    // the whole window and make an old spike look recent.
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu(1, 10.0)]);
    app.on_snapshot(vec![row_without_cpu(1)]);
    assert_eq!(app.cpu_history(1), &[10.0, 0.0]);
}

#[test]
fn the_buffer_holds_at_most_a_hundred_and_forty_samples() {
    let mut app = fixture();
    for _ in 0..200 {
        app.on_snapshot(vec![row_with_cpu(1, 1.0)]);
    }
    assert_eq!(app.cpu_history(1).len(), 140);
}

#[test]
fn a_sheep_that_leaves_the_flock_takes_its_history_with_it() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu(1, 10.0), row_with_cpu(2, 5.0)]);
    app.on_snapshot(vec![row_with_cpu(1, 10.0)]);
    assert!(app.cpu_history(2).is_empty(), "a deleted sheep leaves no history behind");
}

#[test]
fn the_flock_series_is_the_sum_of_the_snapshot() {
    let mut app = fixture();
    app.on_snapshot(vec![row_with_cpu(1, 10.0), row_with_cpu(2, 5.5)]);
    assert_eq!(app.flock_cpu_history(), &[15.5]);
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::app --skip ::slow::
```

Expected: compile failure, no method `cpu_history`.

- [ ] **Step 3: Add the state**

```rust
/// The widest window any pane draws, in samples.
///
/// The landing pane's sparkline needs ten. 1d's charts want six minutes,
/// which is 180 at the two-second poll, and 140 is what fits the frames'
/// 140-cell chart body. Sized for the charts now so the sheep pane
/// inherits a filled buffer rather than starting cold on a pane the
/// operator has just opened.
const HISTORY: usize = 140;
```

Two fields on `App`: `cpu_history: HashMap<u32, VecDeque<f32>>` and `flock_cpu: VecDeque<f32>`. In the `Msg::Snapshot` arm, after the flock map is replaced, push one sample per row, push the sum, truncate each to `HISTORY` from the front, and drop every history entry whose id is not in the new snapshot.

A row with no `cpu_percent` pushes `0.0` rather than skipping, so the window stays aligned in time.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::app --skip ::slow::
```

Expected: PASS, 5 new tests, nothing else moved.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): retain per-sheep cpu samples across the poll"
```

---

### Task 5: two new columns and two new ladder rungs

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/flock.rs`

**Interfaces:**
- Consumes: `cell::gauge`, `cell::sparkline` (task 2); `Palette::sky`, `gauge_rest`, `ground` (task 1); `ProcessInfo.max_memory` (task 3); `App::cpu_history` (task 4).
- Produces: `Column::CpuSpark`, `Column::MemCeil`, a widened header test, and two private cell builders `fn cpu_spark_cell(app: &App, info: &ProcessInfo) -> String` and `fn mem_ceil_cell(info: &ProcessInfo) -> String`, both called from `cell()` and both tested directly.

Widths: `CpuSpark` 10, `MemCeil` 10. Every existing width is unchanged.

New tier constants, and the two new thresholds, derived exactly as the existing ones are (`fixed + gaps + NAME_MIN`):

```rust
const TIERS: &[(u16, &[Column])] = &[
    (146, ALL),          // 112 fixed + 26 gaps + 8 NAME_MIN
    (134, NO_CEIL),      // 102 + 24 + 8
    (122, NO_SPARK),     // 92 + 22 + 8, today's full set and today's threshold
    (116, NO_CFG),
    (101, NO_SMIT),
    (89, NO_FOLD),
    (78, NO_EXIT),
    (68, NO_RESTARTS),
    (59, NO_PID),
    (49, NO_MEM),
    (41, NO_CPU),
    (MIN_WIDTH, FLOOR),
];
```

`NO_SPARK` is today's `ALL`, renamed rather than rewritten. Order in `ALL`: Id, Name, Status, CpuSpark, Cpu, MemCeil, Mem, Pid, Restarts, Exit, Cfg, Uptime, Fold, Smit.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_two_new_rungs_restore_todays_table_before_shedding_anything_old() {
    assert_eq!(columns_for(146), ALL);
    assert!(!columns_for(134).contains(&Column::MemCeil));
    assert!(columns_for(134).contains(&Column::CpuSpark));
    assert!(!columns_for(122).contains(&Column::CpuSpark));
    assert_eq!(columns_for(122), NO_SPARK, "122 is today's full set, unchanged");
}

#[test]
fn every_shared_header_still_matches_flock_rows_exactly() {
    use crate::output::Render;

    let shared: Vec<&str> = ALL
        .iter()
        .map(|column| column.header())
        .filter(|header| crate::output::FlockRows::headers().contains(header))
        .collect();
    assert_eq!(shared, crate::output::FlockRows::headers());
}

#[test]
fn the_only_headers_lookout_adds_are_the_two_shep_flock_cannot_draw() {
    use crate::output::Render;

    let extra: Vec<&str> = ALL
        .iter()
        .map(|column| column.header())
        .filter(|header| !crate::output::FlockRows::headers().contains(header))
        .collect();
    assert_eq!(extra, vec!["CPU 20s", "MEM/CEIL"]);
}

#[test]
fn a_running_sheep_with_no_ceiling_draws_an_empty_gauge() {
    // Not a "-": the column is a bar, and an all-tail bar reads as
    // "no ceiling set" without a second rendering to learn.
    let info = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .memory_bytes(Some(48 * 1024 * 1024))
        .build();
    assert_eq!(mem_ceil_cell(&info), "░░░░░░░░░░");
}

#[test]
fn a_sheep_at_its_ceiling_fills_the_gauge() {
    let info = ProcessInfo::builder(1, "hungry", ProcStatus::Online)
        .memory_bytes(Some(52 * 1024 * 1024))
        .max_memory(Some(52 * 1024 * 1024))
        .build();
    assert_eq!(mem_ceil_cell(&info), "██████████");
}
```

Keep the existing `the_full_column_set_matches_flock_rows_headers_exactly` deleted, replaced by the two tests above. Say so in the commit body.

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::flock --skip ::slow::
```

Expected: compile failure, no variant `CpuSpark`.

- [ ] **Step 3: Add the columns**

Two variants on `Column`, their `header()` arms (`"CPU 20s"` and `"MEM/CEIL"`), their `width()` arms (10 and 10), the reordered `ALL`, the two new tier constants, and the two new `TIERS` rows.

`cell()` gains two arms. `MemCeil` calls `cell::gauge(info.memory_bytes.unwrap_or(0), info.max_memory, 10)`. `CpuSpark` calls `cell::sparkline(app.cpu_history(info.id), 10)`.

`row_line` and `group_line` today style only the STATUS cell. Both need a per-column style: `CpuSpark` in meadow, `MemCeil` in sky, or butter when the gauge is at or above 90% of its ceiling. Extract the existing `if *column == Column::Status` branch into one `fn cell_style(palette, column, row) -> Style` used by both, rather than duplicating the match.

A group row's `CpuSpark` and `MemCeil` are blank, joining ID, PID, EXIT and CFG in `group_cell`'s existing blank set: a group has no single history and no single ceiling.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

Expected: PASS. The existing tier test at flock.rs:660, which asserts `fixed + gaps + NAME_MIN <= width` for every width from 31 to 200, must pass unedited. If it fails, a threshold is wrong; fix the threshold, never the test.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/view/flock.rs
git commit -m "feat(lookout): add the cpu sparkline and memory gauge columns"
```

---

### Task 6: the selection gutter and the painted row

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/flock.rs` (`mark`, and the row's style)
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (the draw at mod.rs:245 that offsets by `GUTTER`)

**Interfaces:**
- Consumes: `Palette::ground` (task 1).
- Produces: `pub fn gutter(selected: bool, palette: Palette) -> (&'static str, Style)`, beside the existing `mark`, which stays as the `NO_COLOR` fallback.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_painted_gutter_is_one_column_and_holds_no_glyph() {
    let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
    let (text, style) = gutter(true, deep);
    assert_eq!(text, " ", "a space, not a block: a block is Ambiguous width");
    assert_eq!(crate::output::width::char_columns(text), 1);
    assert!(style.bg.is_some());
}

#[test]
fn without_colour_the_gutter_falls_back_to_the_ascii_marker() {
    let off = Palette::detect(Some(OsStr::new("1")), None, None);
    let (text, style) = gutter(true, off);
    assert_eq!(text, ">");
    assert_eq!(style, Style::default());
    assert_eq!(gutter(false, off).0, " ");
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::flock --skip ::slow::
```

Expected: compile failure, no function `gutter`.

- [ ] **Step 3: Write it**

```rust
/// The selected row's edge: a painted space, or the ASCII marker when
/// there is no colour to paint with.
///
/// A space rather than `▌`, for the reason [`mark`] gives about `▸`:
/// every block glyph in this pane's vocabulary is East-Asian
/// *Ambiguous*, and a doubled cell in the gutter shifts the whole row.
/// A space is one column on every terminal, and the background carries
/// the whole signal.
#[must_use]
pub fn gutter(selected: bool, palette: Palette) -> (&'static str, Style) {
    match (selected, palette.ground()) {
        (false, _) => (" ", Style::default()),
        (true, ground) if ground.bg.is_some() => (" ", ground),
        (true, _) => (mark(true), Style::default()),
    }
}
```

`mark` stays exactly as it is, along with its test: it is now the `NO_COLOR` fallback rather than the only path.

The selected row's own cells take `palette.ground()` merged with each cell's foreground style, so the ground runs the width of the row rather than stopping where the text stops. Pad the row's last cell out to the table width before styling, or the buffer cells past the text keep their default background and the ground ends mid-row.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/view/flock.rs crates/shep-cli/src/lookout/view/mod.rs
git commit -m "feat(lookout): paint the selected row and its gutter edge"
```

---

### Task 7: the bands

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (the title row and the section headers)

**Interfaces:**
- Consumes: `cell::band`, `cell::rule` (task 2); `Palette::band` (task 1).
- Produces: two private functions in `view/mod.rs`, `fn title_band(app: &App, width: u16) -> Line<'static>` and `fn section_band(label: &str, role: Role, palette: &Palette, width: u16) -> Line<'static>`.

The `Flock` and `Dogs` section headers already exist, from decision 1 of the pane design pass. They become bands. The title row becomes a full-width reverse-video band in meadow.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_title_band_is_reverse_video_across_the_whole_width() {
    let line = title_band(&app_fixture(), 80);
    assert_eq!(
        line.spans.iter().map(|s| s.content.chars().count()).sum::<usize>(),
        80,
        "a band that stops where its text stops leaves unpainted cells"
    );
    assert!(line.spans[0].style.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn a_frozen_link_turns_the_title_band_bark() {
    let mut app = app_fixture();
    app.freeze();  // grep for the real name of the frozen transition
    let line = title_band(&app, 80);
    assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(166)));
}

#[test]
fn the_section_bands_name_their_section_in_words() {
    let flock = section_band("FLOCK", crate::vocabulary::Role::Meadow, &palette(), 40);
    assert!(flock.spans.iter().any(|s| s.content.contains("FLOCK")));
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

Expected: compile failure.

- [ ] **Step 3: Write them**

The title band's role is meadow ordinarily and bark when the link is frozen. Do not add a butter arm here; the editing and secrets bands belong to 1e and 1h.

The row must be padded to the full width before styling, because ratatui paints a `Span`'s background only under the cells the span's text occupies.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/view/mod.rs
git commit -m "feat(lookout): draw the title and section bands"
```

---

### Task 8: the host strip

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/host.rs`

**Interfaces:**
- Consumes: `cell::gauge`, `cell::sparkline` (task 2); `Palette::sky`, `attention` (task 1); `App::flock_cpu_history` (task 4).
- Produces: two private functions in `view/host.rs`, `fn load_gauge(load: f64, cores: Option<usize>) -> String` and `fn summary(app: &App) -> String`.

The strip keeps its single line and its truncate-from-the-right drop order, which is the only drop mechanism it has. It gains a ten-cell butter load gauge against cores, a ten-cell sky memory gauge against host total, an eight-cell flock sparkline, and a right-hand `N errored · N parked`.

`strip_line` today wraps everything in one muted `Span`. It becomes several spans so the two gauges can take their roles. Rewrite the comment at host.rs:22 rather than deleting it: it argues that colour on this line would be decoration, which was true when the line held only words, and a gauge is the case it did not cover.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_load_gauge_measures_against_the_core_count() {
    // 5.12 of 14 cores is 37%, four of ten cells.
    assert_eq!(load_gauge(5.12, Some(14)), "████░░░░░░");
}

#[test]
fn a_host_that_cannot_report_cores_draws_no_load_gauge() {
    assert_eq!(load_gauge(5.12, None), "░░░░░░░░░░");
}

#[test]
fn the_summary_counts_errored_and_parked_sheep() {
    let app = fixture_with(vec![
        ProcessInfo::builder(1, "flaky", ProcStatus::Errored).build(),
        ProcessInfo::builder(2, "catcher", ProcStatus::Online)
            .pending(Some(vec!["max_memory".to_string(), "err_file".to_string()]))
            .build(),
    ]);
    assert_eq!(summary(&app), "1 errored · 1 parked");
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::host --skip ::slow::
```

- [ ] **Step 3: Write them**

The errored count filters the flock on `ProcStatus::Errored`. The parked count is the number of sheep whose `pending` list is non-empty, the same field `cfg_cell` already reads. Both sum over `app.all_rows()`, the iterator host.rs already walks for CPU and memory.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view::host --skip ::slow::
```

Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/view/host.rs
git commit -m "feat(lookout): give the host strip gauges and a flock summary"
```

---

### Task 9: the detail band, the feed chip and the status bar

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/detail.rs`
- Modify: `crates/shep-cli/src/lookout/view/bleats.rs`
- Modify: `crates/shep-cli/src/lookout/view/status.rs`

**Interfaces:**
- Consumes: `cell::band` (task 2); `Palette::ground`, `line` (task 1).
- Produces: one private function in `view/detail.rs`, `fn log_row(app: &App, width: u16) -> Line<'static>`, replacing the two `path_line` calls at detail.rs:144. `path_line` itself goes, since nothing else calls it.

Three small changes that share one review, since none is independently interesting.

**Detail band.** `path_line` is called twice at detail.rs:144. The two calls merge into one row: the `out` label and path, a `│` divider in the `line` role, the `err` label and path, then the log's size on disk from a `fs::metadata` call per path. When the row does not fit, the size goes first, then the paths truncate from the left, since a log path's tail is the half that identifies it. The band gains the `SHEEP N` chip.

**Feed.** Gains the `BLEATS` chip on the header line it already prints. The `out` and `err` tags stay muted; do not colour them, and leave the comment at bleats.rs:93 in place, it is still the reason. No timestamp or level parsing.

**Status bar.** The keys take butter over `palette.ground()`, and the right-aligned control indicator reuses the read-only string status.rs already carries.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_log_row_carries_both_paths_and_the_size() {
    let line = log_row(&app_fixture(), 160);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("out"));
    assert!(text.contains("err"));
    assert!(text.contains('\u{2502}'), "a divider between the two");
}

#[test]
fn a_narrow_log_row_drops_the_size_before_it_truncates_a_path() {
    let wide: String = log_row(&app_fixture(), 160).spans.iter().map(|s| s.content.as_ref()).collect();
    let narrow: String = log_row(&app_fixture(), 70).spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(wide.contains("on disk"));
    assert!(!narrow.contains("on disk"));
}

#[test]
fn a_path_truncates_from_the_left_so_its_filename_survives() {
    let narrow: String = log_row(&app_fixture(), 60).spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(narrow.contains("-out.log"), "the tail identifies the file");
}

#[test]
fn the_stream_tag_is_still_muted() {
    // bleats.rs:93 argues a coloured `err` says a stderr line is damage.
    // The redesign colours gauges, not this.
    let line = feed_lines(&app_fixture(), 80, 4).remove(1);
    assert_eq!(line.spans[0].style, app_fixture().palette().muted());
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

- [ ] **Step 3: Write them**

A `fs::metadata` call that fails, for a log file rotated away between the poll and the draw, drops the size rather than the row.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::view --skip ::slow::
```

Expected: PASS.

- [ ] **Step 5: Gate and commit**

Three commits, one per file, since each is its own concern:

```bash
git add crates/shep-cli/src/lookout/view/detail.rs
git commit -m "feat(lookout): merge the log paths into one row with its size"
git add crates/shep-cli/src/lookout/view/bleats.rs
git commit -m "feat(lookout): give the bleats feed its chip"
git add crates/shep-cli/src/lookout/view/status.rs
git commit -m "feat(lookout): paint the status bar and its control indicator"
```

---

### Task 10: the gallery learns about backgrounds

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs` (`sgr` at frames.rs:83 and the renderer at 62)
- Modify: `docs/lookout/frames.txt`, `docs/lookout/frames.ansi` (regenerated, never hand-edited)

**Interfaces:**
- Consumes: everything above.
- Produces: new `Scene` variants for the redesigned pane.

`sgr` takes a foreground and has no background branch, and the renderer never reads a cell's background. Until both change, the generated `frames.ansi` draws every band as plain text, and the gallery the design wants to replace the mockups with would be wrong about the one rule the design calls its acceptance criterion.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_ansi_dump_emits_a_bands_reverse_video_and_a_grounds_background() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
    buffer[(0, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
    buffer[(1, 0)].set_style(Style::default().bg(Color::Indexed(235)));
    let dump = render_ansi(&buffer);
    assert!(dump.contains("\u{1b}[7m"), "reverse video");
    assert!(dump.contains("\u{1b}[48;5;235m"), "an indexed background");
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test -p shep --lib --bins --all-features -- lookout::frames --skip ::slow::
```

Expected: FAIL, neither escape present.

- [ ] **Step 3: Add the branches**

`sgr` gains a background parameter and a `REVERSED` branch. The renderer reads `cell.bg` and `cell.modifier` alongside `cell.fg`. Keep the reset behaviour it already has: a cell that sets nothing must still clear whatever the previous cell set, or a band bleeds along the row.

- [ ] **Step 4: Add the scenes and regenerate**

Add a `Scene` per redesigned region, following the shape every existing scene uses. The count test that pins `Scene::ALL`'s length and the gallery test that asserts the file's heading count both need their number updated to match; update the number, never the assertion.

```bash
cargo test -p shep --lib --bins --all-features -- --ignored write_the_gallery
```

Then read the diff. `git diff docs/lookout/frames.txt` should show the new scenes and the redesigned pane, and nothing surprising in the scenes that did not change.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/shep-cli/src/lookout/frames.rs docs/lookout/frames.txt docs/lookout/frames.ansi
git commit -m "feat(lookout): render backgrounds and reverse video in the gallery"
```

---

### Task 11: the docs

**Files:**
- Modify: `docs/lookout/README.md`
- Modify: `web/src/pages/docs/*.astro`, only where they name the flock table's columns
- Modify: `web/src/content/docs/reference/*` (regenerated, never hand-edited)

**Interfaces:**
- Consumes: everything above.
- Produces: nothing code reads.

`docs/lookout/README.md`'s drop-order sentence is already stale before this change: it lists SMIT, FOLD, EXIT, RESTARTS, PID, MEM, CPU, UPTIME and omits CFG entirely, even though CFG is the first column the shipped ladder sheds. Rewrite it against the twelve-rung ladder this plan leaves behind, and say that the sparkline needs a terminal at least 148 columns wide and the gauge at least 136.

- [ ] **Step 1: Find what names the columns**

```bash
rg -n 'SMIT|CFG column|MEM/CEIL|drop order' docs/lookout/README.md web/src/pages/docs/
```

- [ ] **Step 2: Rewrite the prose**

Run the `humanizer` skill and then `rin-voice` over anything you write here. No em dashes. Do not hard-wrap a GitHub comment; a markdown file in the repo follows whatever the file already does.

- [ ] **Step 3: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```

`git diff` afterwards is the check. If nothing changed, say so: this plan does not add a verb or a flag, so an empty diff here is the expected result rather than a failure.

- [ ] **Step 4: Build and typecheck the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is the one that catches a wrong prop; `build` does not typecheck.

- [ ] **Step 5: Full gate and commit**

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

```bash
git add docs/lookout/README.md web/
git commit -m "docs(lookout): describe the redesigned table and its drop order"
```

---

## What this plan does not do

- 1d's charts, and the aggregation that would turn a two-second sample into a fixed-width bucket.
- Any new `Request`. Task 3's field is additive on an existing response.
- The overlay machinery 1g and 1k need. No region of 1a is an overlay.
- An ambiguous-width override or a terminal probe.
- Timestamp and level parsing in the feed.
- Frames 1d, 1e, 1g, 1h, 1i, 1j, 1k and 1l.
