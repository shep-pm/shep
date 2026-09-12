# Lookout frame 1g, the close dialog: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the config pane closes carrying changes the running process has not taken, ask whether to restart, reload, or leave them parked, and write on the answer rather than on the keypress.

**Architecture:** A new `CloseDialog` on `App`, raised by `esc` in place of the `PaneMenu` it deletes, interpreted by a nested keymap inside `on_pane_key` rather than by a third `InputMode`. It puts each filed edit and `SheepConfigView::pending` through one new shep-core predicate, holds the chosen verb until the batched writes are answered, and draws as the first overlay lookout has ever had.

**Tech Stack:** Rust 2024, MSRV 1.88, ratatui 0.30, insta snapshots.

**Spec:** [docs/brainstorming/specs/2026-09-12-lookout-1g-close-dialog-design.md](../../brainstorming/specs/2026-09-12-lookout-1g-close-dialog-design.md). Read it before task 1. The plan argues from it and does not repeat its reasoning.

## Global constraints

- **Conventional commit subjects, every commit**: `type(scope): summary`, with `!` on the one that breaks something, in the crate that breaks. `feat`, `fix`, `perf`, `refactor` produce changelog entries; `docs`, `test`, `ci`, `chore`, `style` are skipped unless marked breaking. `revert` and `build` are refused by the parser and vanish. `.github/workflows/commits.yml` gates this.
- **One cargo shape for this whole plan**: `-p shep`, never `--workspace`, except where a step names `-p shep-core` or `-p shep-daemon` explicitly for task 1. Alternating shapes invalidates the feature unification and rebuilds.
- **The inner loop** is `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`. Run the unfiltered form only at the task gate.
- **Invoke the `shep-idiomatic-rust` skill before writing any Rust here.** Every new public item needs docs, `# Errors` where it returns `Result`, and a deliberate `Debug` decision.
- **No em dashes in any prose that ships**, including doc comments and the dialog's own copy.
- **Never "lamb" for an instance.** `docs/terminology.md:20`.
- **Repo-relative paths only**, never an absolute path out of a local checkout.
- **Code shown in this plan about files that already exist is a reading, not a quotation, unless it is marked as quoted.** Grep the real file before matching against it. Where this plan and the code disagree, the code wins and the plan is wrong.
- **Invoke the `tui-screen-capture` skill for any task that changes what the dashboard draws**, which here is tasks 4 and 5. Snapshot tests pin characters and will happily pin a border one column off, a band whose background never painted, or a box that clips at its own floor. Capture before and after, at more than one width, and use `--attrs` when the question is whether something was painted rather than whether it was spelled. `--keys` land near the end of `--seconds`, so a short capture looks like the change did nothing; and `SHEP_HOME` must be short enough for a unix socket path if a live daemon is involved, so `mktemp -d` rather than a scratchpad path.

---

### Task 1: `reaches_running` in shep-core, and the daemon reads it

**Files:**
- Modify: `crates/shep-core/src/config/apply.rs`
- Modify: `crates/shep-daemon/src/supervisor.rs` (the `in_force` line, quoted below)
- Test: both files' own `mod tests`

**Interfaces:**
- Consumes: nothing.
- Produces: `shep_core::config::reaches_running(field: &str) -> bool`. Task 2 calls it.

The fact being moved is quoted verbatim from `crates/shep-daemon/src/supervisor.rs:5117` as of `081dbfce`:

```rust
        let in_force =
            !park_all && (group == ApplyGroup::Live || matches!(key, "autostart" | "depends_on"));
```

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/config/apply.rs`, inside `mod tests`:

```rust
    #[test]
    fn a_live_field_reaches_a_running_child() {
        assert!(reaches_running("max_restarts"));
    }

    /// The two `NextSpawn` fields nothing spawns to read: `restorable()`
    /// reads one and `plan_for_names` the other, off the stored spec the
    /// moment it lands.
    #[test]
    fn autostart_and_depends_on_reach_one_without_a_respawn() {
        assert!(reaches_running("autostart"));
        assert!(reaches_running("depends_on"));
    }

    /// The other three `NextSpawn` fields come off the per-sheep task's
    /// `ResolvedApp`, so a respawn is what applies them.
    #[test]
    fn the_other_next_spawn_fields_do_not() {
        assert!(!reaches_running("kill_signal"));
        assert!(!reaches_running("listen_timeout"));
        assert!(!reaches_running("readiness_probe"));
    }

    #[test]
    fn a_needs_respawn_field_does_not() {
        assert!(!reaches_running("cwd"));
    }

    /// An unknown name answers like `apply_group` does, conservatively.
    #[test]
    fn an_unknown_field_does_not_reach_a_running_child() {
        assert!(!reaches_running("a_field_from_a_later_shep"));
    }
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep-core --lib --all-features reaches_running
```

Expected: `cannot find function `reaches_running` in this scope`.

- [ ] **Step 3: Write it**

In `crates/shep-core/src/config/apply.rs`, immediately after `apply_group`:

```rust
/// Whether a write to `field` reaches a child that is already running.
///
/// `false` for every field a respawn is what applies. The shepherd parks
/// exactly these, and `shep lookout`'s close dialog asks about exactly
/// these, so the two read one answer rather than each deriving its own.
///
/// [`ApplyGroup::NextSpawn`] splits. `kill_signal`, `listen_timeout` and
/// `readiness_probe` come off the per-sheep task's `ResolvedApp`, moved in
/// once at spawn. `autostart` and `depends_on` are read at a muster, a boot
/// or an ordered walk instead, off the stored spec the moment it lands, so
/// telling an operator to restart for either would be telling them to do
/// nothing.
///
/// A group a later shep-core adds answers `false`, matching
/// [`apply_group`]'s own conservative fallback: the safe claim is that the
/// running process does not have the new value.
#[must_use]
pub fn reaches_running(field: &str) -> bool {
    match apply_group(field) {
        ApplyGroup::Live => true,
        ApplyGroup::NextSpawn => matches!(field, "autostart" | "depends_on"),
        ApplyGroup::NeedsRespawn | ApplyGroup::Structural | _ => false,
    }
}
```

Then export it wherever `apply_group` is exported. Grep first:

```bash
grep -rn "apply_group" crates/shep-core/src/config/mod.rs crates/shep-core/src/lib.rs
```

- [ ] **Step 4: Run them and watch them pass**

```bash
cargo test -p shep-core --lib --all-features reaches_running
```

- [ ] **Step 5: Point the daemon at it**

In `crates/shep-daemon/src/supervisor.rs`, replace the `in_force` binding quoted at the head of this task:

```rust
        let in_force = !park_all && reaches_running(key);
```

Add `reaches_running` to the `shep_core::config` import at `crates/shep-daemon/src/supervisor.rs:26`. Keep the comment above the binding: move its text into the new function's doc comment (step 3 already carries it) and leave a one-line pointer behind:

```rust
        // `reaches_running` owns the `autostart`/`depends_on` carve-out now,
        // so the pane's prediction and this answer cannot drift.
```

- [ ] **Step 6: Pin the agreement**

In `crates/shep-core/src/config/apply.rs`'s `mod tests`:

```rust
    /// The claim the hoist rests on: every field the table knows, answered
    /// by both routes, agreeing. A field added to `FIELDS` without a thought
    /// about which side of the line it falls on fails here.
    #[test]
    fn reaches_running_agrees_with_the_group_table_for_every_field() {
        for (field, group) in FIELDS {
            let expected = match group {
                ApplyGroup::Live => true,
                ApplyGroup::NextSpawn => matches!(*field, "autostart" | "depends_on"),
                _ => false,
            };
            assert_eq!(
                reaches_running(field),
                expected,
                "{field} is classified {group:?}"
            );
        }
    }
```

- [ ] **Step 7: Run both crates' suites**

```bash
cargo test -p shep-core --lib --all-features
```

```bash
cargo test -p shep-daemon --lib --all-features -- --skip ::slow::
```

- [ ] **Step 8: Commit**

```bash
git add crates/shep-core/src/config/apply.rs crates/shep-daemon/src/supervisor.rs
git commit -m "refactor(core): hoist the reaches-a-running-child rule out of the daemon"
```

---

### Task 2: the dialog, its trigger, and its borderless form

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`
- Modify: `crates/shep-cli/src/lookout/input.rs`
- Modify: `crates/shep-cli/src/lookout/view/pane.rs`
- Modify: `crates/shep-cli/src/lookout/view/status.rs`
- Modify: `crates/shep-cli/src/lookout/view/fixtures.rs`
- Delete from `app.rs`: `PaneMenu`, `apply_offer`, `on_pane_menu_key`, the `pane_menu` field and accessor
- Delete from `view/pane.rs`: `menu_text`, `top_line`'s menu branch

**Interfaces:**
- Consumes: `shep_core::config::reaches_running` (task 1).
- Produces: `App::close_dialog() -> Option<&CloseDialog>`, `CloseDialog::{unsent, parked, reload, target_name}`, `KeyPress::Continue`, and `view::pane::close_dialog_lines(dialog, palette, width) -> Vec<Line<'static>>`. Tasks 3, 4 and 5 all read these.

This task ships a working dialog in the borderless full-width form, which is the real form below 90 columns. Task 4 adds the box above it. Nothing here is throwaway.

- [ ] **Step 1: Write the failing trigger tests**

In `crates/shep-cli/src/lookout/app.rs`'s `mod tests`. Every one drives a real key, never a predicate:

```rust
    /// The case the old menu missed: an edit made in this pane, on a sheep
    /// with nothing parked before it opened.
    #[test]
    fn esc_with_a_respawn_edit_asks_before_it_writes() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_some(), "the dialog is up");
        assert!(
            matches!(effect, Effect::None),
            "nothing is written until the dialog is answered, got {effect:?}"
        );
        assert!(app.config_pane().is_some(), "the pane is still open");
    }

    #[test]
    fn esc_with_only_live_edits_writes_and_closes_with_no_dialog() {
        let mut app = fixtures::app_in_sheep_pane();
        // `max_restarts` is `ApplyGroup::Live`.
        fixtures::file_edit(&mut app, "max_restarts", "9");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(app.config_pane().is_none(), "the pane closed");
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    /// Nothing holds the old config, so there is nothing to respawn, and `R`
    /// on a stopped sheep would start it.
    #[test]
    fn no_dialog_for_a_stopped_sheep() {
        let mut app = fixtures::app_in_sheep_pane_on_a_stopped_sheep();
        fixtures::file_edit(&mut app, "cwd", "/srv/app");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none(), "a stopped sheep is not asked about");
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    #[test]
    fn no_dialog_for_a_dog() {
        let mut app = fixtures::app_in_dog_pane();
        fixtures::file_edit(&mut app, "url", "http://127.0.0.1:9/");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    /// The parked half: no edit of the operator's own, fields the shepherd
    /// is already holding.
    #[test]
    fn esc_over_parked_fields_alone_still_asks() {
        let mut app = fixtures::app_in_sheep_pane();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        let dialog = app.close_dialog().expect("two fields are parked");
        assert_eq!(dialog.unsent(), 0);
        assert_eq!(dialog.parked(), 2);
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }

    #[test]
    fn read_only_is_never_asked() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(app.config_pane().is_none(), "read-only still closes");
    }

    #[test]
    fn esc_from_the_dialog_writes_nothing_and_keeps_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none(), "the dialog closed");
        assert!(app.config_pane().is_some(), "the pane did not");
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }

    /// The edits survive it: `esc` is `keep editing`, not `discard`.
    #[test]
    fn esc_from_the_dialog_leaves_the_edits_filed() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.close_dialog().is_some(),
            "the second esc found the same two edits still filed"
        );
    }

    /// An expiry is an `esc`, never a `c`. A dialog nobody answered is not
    /// consent to write.
    #[test]
    fn the_dialog_expires_without_writing() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let later = app.now() + CONFIRM_EXPIRY;
        let effect = app.update(Msg::Tick { now: later });
        assert!(app.close_dialog().is_none(), "it expired");
        assert!(app.config_pane().is_some(), "the pane is still open");
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }
```

`fixtures::file_edit`, `app_in_sheep_pane_on_a_stopped_sheep`, `app_in_dog_pane` and `app_in_sheep_pane_read_only` may not all exist. Grep `crates/shep-cli/src/lookout/view/fixtures.rs` first and add only what is missing, following the shape of `app_in_sheep_pane_with_two_edits`, which does exist.

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: close_dialog
```

Expected: `no method named `close_dialog` found`.

- [ ] **Step 3: Write the state**

In `crates/shep-cli/src/lookout/app.rs`, where `PaneMenu` is today:

```rust
/// The question `esc` asks when a pane closes over changes the running
/// child has not taken.
///
/// Raised by [`App::close_offer`], answered by [`App::on_close_dialog_key`],
/// and expired by the same [`CONFIRM_EXPIRY`] every other prompt gets.
///
/// Both counts are carried rather than recomputed: the set is taken from
/// the pane when the dialog goes up, and `parked` is the shepherd's own
/// answer from the last fetch.
///
/// `Debug` is derived (IR-41): two counts, a reload mode, a name, a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseDialog {
    unsent: Vec<String>,
    parked: usize,
    reload: ReloadKind,
    instances: u32,
    kill_timeout: String,
    graceful_timeout: String,
    name: String,
    pid: Option<u32>,
    at: Instant,
}
```

with accessors for each, `unsent()` returning the count and `unsent_fields()` the names, since the heading needs one and the sentence below it the other.

On `App`, replace `pane_menu: Option<PaneMenu>` with `close_dialog: Option<CloseDialog>` and its `None` initialiser.

- [ ] **Step 4: Write the trigger**

Replace `apply_offer` (`app.rs:4844`) with:

```rust
    /// The question this pane's `Escape` asks, or [`None`] when it just
    /// writes and leaves.
    ///
    /// Silent behind a closed gate, where every key it offers would be
    /// refused; silent on a dog, which has no apply table to classify an
    /// edit with; and silent on a sheep that is not running, where nothing
    /// holds the old config and `R` would start it rather than replace it.
    fn close_offer(&self) -> Option<CloseDialog> {
        if self.control == Control::ReadOnly {
            return None;
        }
        let pane = self.config_pane()?;
        let PaneTarget::Sheep { name } = pane.target() else {
            return None;
        };
        if !self.sheep_is_running(name) {
            return None;
        }
        let unsent = pane.unsent_fields_needing_a_respawn();
        let parked = pane.parked_count();
        if unsent.is_empty() && parked == 0 {
            return None;
        }
        Some(CloseDialog::new(unsent, parked, pane, self.now))
    }
```

`ConfigPane::unsent_fields_needing_a_respawn` is new, in `crates/shep-cli/src/lookout/pane.rs`, and is the only place `reaches_running` is called:

```rust
    /// The filed edits a respawn is what applies, by field name, in the
    /// set's own key order.
    ///
    /// An env key counts: `env` is `ApplyGroup::NeedsRespawn` and every
    /// value is baked into the child at exec.
    #[must_use]
    pub(super) fn unsent_fields_needing_a_respawn(&self) -> Vec<String> {
        self.edits
            .iter()
            .filter_map(|(key, _)| match key {
                EditKey::Field(name) if !reaches_running(name) => Some(name.clone()),
                EditKey::Env(name) => Some(format!("env {name}")),
                EditKey::Field(_) => None,
            })
            .collect()
    }
```

`sheep_is_running` is new on `App`, reading the flock map by name. Grep for how `flock_target` walks `self.flock` and follow it; the status field is on `ProcessInfo`.

- [ ] **Step 5: Rewire `esc`**

At `crates/shep-cli/src/lookout/app.rs:4595`, the `KeyPress::Escape` arm of `on_pane_key` currently takes the writes before it decides. Invert it:

```rust
            KeyPress::Escape => {
                let help_open = self.config_pane().is_some_and(ConfigPane::help_open);
                if help_open {
                    // unchanged, quoted from the existing arm
                }
                if let Some(dialog) = self.close_offer() {
                    self.close_dialog = Some(dialog);
                    return Effect::None;
                }
                let writes = self.take_pane_writes();
                self.close_pane();
                if writes.is_empty() {
                    return Effect::None;
                }
                return Effect::SendAll(writes);
            }
```

- [ ] **Step 6: Write the dialog's keymap**

Replace `on_pane_menu_key` with `on_close_dialog_key`, and the guard at `app.rs:4574` with the `close_dialog` one. Task 3 fills in what `R` and `L` do; here they close and write, which task 3 then makes conditional:

```rust
    /// The dialog owns the keyboard until it is answered or it expires.
    ///
    /// `Escape` closes the dialog and not the pane, which is the difference
    /// from the menu this replaces: `esc` here means keep editing, so the
    /// filed set stays filed and nothing is written.
    fn on_close_dialog_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Action(verb @ (ActionVerb::Reload | ActionVerb::Restart)) => {
                self.answer_close(Some(verb))
            }
            KeyPress::Continue => self.answer_close(None),
            KeyPress::Escape => {
                self.close_dialog = None;
                Effect::None
            }
            // every other variant, listed exhaustively the way
            // `on_pane_menu_key` listed them
            _ => Effect::None,
        }
    }
```

The `_ => Effect::None` above is shorthand for this plan only. Write the arms out exhaustively, matching the style of the function being replaced: the repo does not use a wildcard there, and a wildcard would swallow a `KeyPress` variant added later.

- [ ] **Step 7: Bind `c`**

`crates/shep-cli/src/lookout/app.rs`, in `KeyPress`:

```rust
    /// `c` in the close dialog: write the set and leave the sheep running.
    /// Bound nowhere else, the way `Undo` is read only by the config pane.
    Continue,
```

`crates/shep-cli/src/lookout/input.rs`, beside the `'u'` line at 59:

```rust
        KeyCode::Char('c') => Some(KeyPress::Continue),
```

and its test beside the `Undo` one at 473:

```rust
        assert_eq!(press(KeyCode::Char('c')), Some(KeyPress::Continue));
```

The compiler will name every exhaustive `match` on `KeyPress` that now needs the variant. There are roughly six in `app.rs`; each gets `Effect::None` or `{}`, grouped with `NextGroup`/`Group`/`Undo` where those are already grouped, and the comment beside them extended to name the dialog.

- [ ] **Step 8: Write the copy**

In `crates/shep-cli/src/lookout/view/pane.rs`, replacing `menu_text`:

```rust
/// The dialog's rows, in its borderless form: what a terminal under 90
/// columns gets, and what the boxed form draws inside its border.
pub(super) fn close_dialog_lines(
    dialog: &CloseDialog,
    palette: Palette,
    width: u16,
) -> Vec<Line<'static>>
```

The heading, one of three:

```rust
match (dialog.unsent(), dialog.parked()) {
    (0, parked) => format!("{parked} FIELD{} ALREADY WAITING", plural(parked)),
    (unsent, 0) => format!("{unsent} EDIT{} NEED A RESPAWN", plural(unsent)),
    (unsent, parked) => format!(
        "{unsent} EDIT{} NEED A RESPAWN, {parked} FIELD{} ALREADY DID",
        plural(unsent),
        plural(parked)
    ),
}
```

The naming sentence, truncating past three:

```
cwd and err_file take hold when the process starts again.
cwd, err_file and 3 more take hold when the process starts again.
```

`Everything else you changed is already live.` draws only when the filed set holds something `reaches_running` answered `true` for.

The three option rows. `{kill}` is the sheep's own `kill_timeout` rendered the way the pane renders a duration elsewhere; grep `display_value` in `crates/shep-cli/src/lookout/pane.rs` for it:

```
R   restart now      stop, then start. The stop takes up to {kill}.
c   continue         write them and leave it running. They wait for a respawn.
esc  keep editing, write nothing   ·   this prompt expires in {n}s {gauge}
```

The reload row, one of four, off `dialog.reload()` and `dialog.instances()`:

| Mode | Instances | Line |
|---|---|---|
| `Overlap` | 1 | `the replacement starts alongside and takes over. No gap, if the app sets SO_REUSEPORT itself.` |
| `Overlap` | N | `one instance at a time, each replacement alongside the one it replaces. No gap, if the app sets SO_REUSEPORT itself.` |
| `Serial` | 1 | `drains it, then starts the replacement. Up to {graceful}, so slower than a restart for the same gap.` |
| `Serial` | N | `one instance at a time, each drained before its replacement starts. Up to {graceful} each, 1 of N down at a time.` |

The countdown gauge is ten cells of `█` and `░`, the same vocabulary the host strip's gauges use. Grep `gauge` in `crates/shep-cli/src/lookout/view/host.rs` and reuse rather than writing a second one.

- [ ] **Step 9: Draw it**

In `draw_pane` (`crates/shep-cli/src/lookout/view/pane.rs:1746`), after the existing line loop:

```rust
    if let Some(dialog) = app.close_dialog() {
        let lines = close_dialog_lines(dialog, app.palette(), area.width);
        // Bottom-anchored over the field list, the rows the frame draws it on.
        // Task 4 replaces this with the boxed form above 90 columns.
        let top = area.y + area.height.saturating_sub(u16::try_from(lines.len()).unwrap_or(0));
        for (offset, line) in lines.iter().enumerate() {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(area.x, top + offset, line, area.width);
        }
    }
```

`pane_lines`' `menu` parameter goes away with `PaneMenu`. Every caller listed by the compiler updates, including the three in `crates/shep-cli/src/lookout/view/fixtures.rs`.

- [ ] **Step 10: Revert the status hint, as its own comment predicted**

`crates/shep-cli/src/lookout/view/status.rs:427` carries a doc comment saying this line reverts when this frame lands. Read it, then change `esc write & close` to `esc close` and replace the comment's "until then" paragraph with what is now true. While the dialog is up the status bar reduces to one muted line:

```
the dialog owns the keyboard until it is answered or it expires
```

which needs a new slot in `status_line`'s ladder, ahead of the config pane's own hint.

- [ ] **Step 11: Write the render tests**

```rust
    /// Bounded on purpose. A frame-wide `contains("respawn")` passes off
    /// 1e's own legend row, which is drawn underneath this dialog and says
    /// the word. Assert on the dialog's rows, never on the frame.
    #[test]
    fn the_dialog_names_both_halves_in_its_heading() {
        let dialog = fixtures::close_dialog_with(2, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120);
        assert_eq!(
            text_of(&lines)[0].trim(),
            "2 EDITS NEED A RESPAWN, 1 FIELD ALREADY DID"
        );
    }

    #[test]
    fn a_serial_reload_does_not_promise_no_gap() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Serial, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120);
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(reload.contains("slower than a restart"), "{reload}");
        assert!(!reload.contains("No gap"), "{reload}");
    }

    #[test]
    fn an_overlapping_reload_carries_the_reuse_port_caveat() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Overlap, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120);
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(reload.contains("if the app sets SO_REUSEPORT itself"), "{reload}");
    }

    /// `docs/terminology.md:20`. The frame calls instances lambs; four
    /// places in the bundle do.
    #[test]
    fn no_line_calls_an_instance_a_lamb() {
        for kind in [ReloadKind::Overlap, ReloadKind::Serial] {
            for instances in [1, 3] {
                let dialog = fixtures::close_dialog_reloading(kind, instances);
                let lines = close_dialog_lines(&dialog, fixtures::plain(), 120);
                for row in text_of(&lines) {
                    assert!(!row.contains("lamb"), "{row}");
                }
            }
        }
    }

    #[test]
    fn the_restart_row_states_the_sheeps_own_kill_timeout() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120);
        let restart = fixtures::row_starting_with(&lines, "R");
        assert!(restart.contains("5s"), "{restart}");
    }
```

**Mutate every one of these, watch it fail, restore it.** Then ask what else could make it pass. `a_serial_reload_does_not_promise_no_gap` is the one to be suspicious of: check that `row_starting_with` is finding the reload row and not the first row whose text happens to start with an `L`.

- [ ] **Step 12: Run the suite**

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

Four snapshots move here and are expected to: `sheep_pane_apply_menu` is deleted with the menu, and the three `pane_lines` callers lose an argument. Review each `.snap` diff by eye before accepting.

- [ ] **Step 13: Commit**

```bash
git add crates/shep-cli/src/lookout crates/shep-core
git commit -m "feat(lookout)!: ask before the config pane writes, and drop the parked-field menu"
```

The `!` is deliberate: `esc` no longer writes on the keypress, which is a behaviour change an operator will notice, and `PaneMenu` leaves a public accessor behind it.

---

### Task 3: write, then act

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`

**Interfaces:**
- Consumes: `CloseDialog`, `on_close_dialog_key` (task 2).
- Produces: `App::answer_close(verb: Option<ActionVerb>) -> Effect`, and a `held: Option<HeldAction>` field. Task 5's docs describe it.

- [ ] **Step 1: Write the failing tests**

```rust
    /// Order is the whole point: a restart sent before the write lands
    /// respawns into the old config.
    #[test]
    fn r_sends_every_write_before_the_restart() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let Effect::SendAll(batch) = effect else {
            panic!("expected a batch, got {effect:?}");
        };
        assert_eq!(batch.len(), 2, "the two writes and no action yet");
        assert!(batch.iter().all(|sent| !matches!(sent, Sent::Action { .. })));
    }

    #[test]
    fn the_restart_goes_once_the_last_write_is_answered() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        let first = app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Ok(Response::Ok),
        });
        assert!(matches!(first, Effect::None), "not yet: {first:?}");
        let second = app.update(Msg::Replied {
            sent: batch[1].clone(),
            result: Ok(Response::Ok),
        });
        assert!(
            matches!(second, Effect::Send(Sent::Action { verb: ActionVerb::Restart, .. })),
            "got {second:?}"
        );
    }

    /// A refused field alongside an accepted one still needs the restart the
    /// accepted one was waiting for.
    #[test]
    fn a_partly_refused_batch_still_restarts() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Err(fixtures::invalid_config()),
        });
        let last = app.update(Msg::Replied {
            sent: batch[1].clone(),
            result: Ok(Response::Ok),
        });
        assert!(matches!(last, Effect::Send(Sent::Action { .. })), "got {last:?}");
    }

    /// Bouncing a healthy process to apply nothing is the one outcome with
    /// a cost and no benefit.
    #[test]
    fn a_wholly_refused_batch_does_not_restart() {
        let mut app = fixtures::app_in_sheep_pane_with_one_edit();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        let last = app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Err(fixtures::invalid_config()),
        });
        assert!(matches!(last, Effect::None), "got {last:?}");
        assert!(app.notice().is_some(), "and it says why");
    }

    #[test]
    fn c_writes_and_sends_no_action() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Continue));
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
        assert!(app.config_pane().is_none(), "the pane closed");
        let Effect::SendAll(batch) = effect else { unreachable!() };
        let done = app.update(Msg::Replied {
            sent: batch[batch.len() - 1].clone(),
            result: Ok(Response::Ok),
        });
        assert!(matches!(done, Effect::None), "no action follows a c: {done:?}");
    }

    /// The parked half has no writes to wait for, so the action goes at once.
    #[test]
    fn r_over_parked_fields_alone_sends_the_action_immediately() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(matches!(effect, Effect::Send(Sent::Action { .. })), "got {effect:?}");
    }

    /// A reply that never comes cannot strand the verb.
    #[test]
    fn a_held_verb_expires() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let later = app.now() + CONFIRM_EXPIRY;
        app.update(Msg::Tick { now: later });
        assert!(app.held_action().is_none(), "it expired");
    }
```

`Response::Ok` and `fixtures::invalid_config()` are placeholders for whatever the real reply and error types are. Grep `Msg::Replied` in `crates/shep-cli/src/lookout/app.rs` for a test that already builds one and copy its shapes.

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: answer_close held
```

- [ ] **Step 3: Write it**

```rust
/// A verb the close dialog chose, waiting on the writes it must follow.
///
/// `outstanding` counts replies not yet in, and `landed` is whether any of
/// them was accepted. The action goes on the last reply, and only if
/// something landed: a batch refused in full leaves nothing for a respawn
/// to apply.
///
/// `Debug` is derived (IR-41): a verb, two counts, a name, a time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldAction {
    verb: ActionVerb,
    name: String,
    outstanding: usize,
    landed: bool,
    at: Instant,
}
```

`answer_close(verb)` takes the pane's writes, closes the pane and the dialog, and either holds the verb or sends it at once when there are no writes to wait for. The reply handler decrements `outstanding`, ORs `landed`, and on zero either raises `Effect::Send(Sent::Action { .. })` through the existing `apply_parked` path or drops the verb with a notice.

Expire `held` on the same `Msg::Tick` arm that expires `close_dialog` and `action`, at `crates/shep-cli/src/lookout/app.rs:2152`.

- [ ] **Step 4: Run them and watch them pass**

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

- [ ] **Step 5: Commit**

```bash
git add crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): send the close dialog's verb after its writes land"
```

---

### Task 4: the box, and the first overlay lookout has drawn

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/pane.rs`
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (the doc comment at 262, which becomes false here)

**Interfaces:**
- Consumes: `close_dialog_lines` (task 2).
- Produces: nothing later tasks call. Task 5 snapshots what it draws.

- [ ] **Step 1: Write the failing tests**

```rust
/// 86 interior plus a border cell each side is 88, plus a margin cell each
/// side is 90. One column narrower and the border would have to clip, which
/// `docs/lookout/design-files/README.md:332` refuses, so 89 draws the
/// borderless form instead.
const BOX_WIDTH: u16 = 86;
const BOX_FLOOR: u16 = BOX_WIDTH + 4;
```

```rust
    #[test]
    fn the_box_draws_at_its_floor_and_not_one_column_below() {
        assert!(dialog_is_boxed(BOX_FLOOR));
        assert!(!dialog_is_boxed(BOX_FLOOR - 1));
    }

    #[test]
    fn the_borderless_form_spans_the_whole_width_and_never_clips() {
        let rendered = fixtures::render_dialog(89, 48);
        let heading = fixtures::row_containing(&rendered, "NEED A RESPAWN");
        assert!(!heading.contains('▐'), "no border below the floor: {heading}");
        assert!(
            visible_width(&heading) <= 89,
            "clipped or overran: {heading}"
        );
    }

    /// Dimming changes style and leaves every character alone, so a test
    /// that asserts text here is asserting nothing.
    #[test]
    fn the_pane_behind_the_dialog_is_muted() {
        let buffer = fixtures::draw_pane_with_dialog(160, 48);
        let behind = buffer[(2, 4)].style();
        assert_eq!(behind.fg, fixtures::plain_dimmed().fg);
    }

    /// The three the check found. `▐` is Neutral and the four corners are
    /// too; `▀`, `▄` and `▌` are East-Asian Ambiguous, and a terminal that
    /// doubles the right edge shifts every interior row. Recorded rather
    /// than fixed, since no Neutral right-half block exists to swap in.
    #[test]
    fn the_border_vocabulary_is_the_one_that_was_checked() {
        for glyph in ['▛', '▜', '▙', '▟', '▐', '▀', '▄', '▌'] {
            assert_eq!(char_columns(glyph), 1, "{glyph}");
        }
    }
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: boxed muted border_vocabulary
```

- [ ] **Step 3: Write the mute pass and the box**

In `draw_pane`, replacing the bottom-anchored block task 2 left:

```rust
    if let Some(dialog) = app.close_dialog() {
        // The pane draws first and is then muted whole, so 1e's own render
        // is untouched and its four pinned snapshots do not move.
        buffer.set_style(area, app.palette().muted());
        draw_close_dialog(dialog, app.palette(), area, buffer);
    }
```

`Buffer::set_style(Rect, Style)` is a reading of ratatui 0.30 from this plan's author and has not been compiled. Check it against the version in `Cargo.lock` before relying on it; if it is absent or patches rather than replaces, walk the cells with `Buffer::cell_mut` instead and say so in a comment.

`draw_close_dialog` picks the form:

```rust
fn draw_close_dialog(dialog: &CloseDialog, palette: Palette, area: Rect, buffer: &mut Buffer) {
    let lines = close_dialog_lines(dialog, palette, /* interior width */);
    if area.width >= BOX_FLOOR {
        // centred: (area.width - BOX_WIDTH - 2) / 2 cells of margin each side
    } else {
        // full width, no border, bottom anchored
    }
}
```

- [ ] **Step 4: Correct the two doc comments that now say something false**

`crates/shep-cli/src/lookout/view/mod.rs:262` says every screen is "a swap, not an overlay, so nothing below draws while one is up". That stays true of the four bodies and stops being true of this dialog. Amend it rather than deleting it: the sentence is load-bearing about why `Body` is a `match`. Same for `crates/shep-cli/src/lookout/view/status.rs:188`, which says there is no overlay anywhere in the module.

- [ ] **Step 5: Run the suite**

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

1e's four `edit_pane*` snapshots must not move. If they do, the mute pass or the draw order is wrong and the fix is there, not in the snapshot.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/view
git commit -m "feat(lookout): draw the close dialog as a box over the dimmed pane"
```

---

### Task 5: the gallery, and the docs

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs`
- Create: four `.snap` files under `crates/shep-cli/src/lookout/snapshots/`
- Modify: `docs/lookout/frames.txt`, `docs/lookout/frames.ansi` (generated)
- Modify: `docs/lookout/README.md`
- Modify: `web/src/pages/docs/lookout.astro` and whatever else the grep in step 4 finds

- [ ] **Step 1: Add the scenes**

In `frames.rs`'s `scenes!` block, with a doc comment each:

```rust
    /// The close dialog at the design size: two edits filed, one field
    /// already parked, the box centred over the dimmed editing pane.
    CloseDialog,
    /// The same dialog at the exact width its border needs.
    CloseDialogFloor,
    /// One column below that, drawn full width with no border.
    CloseDialogNarrow,
    /// The parked half alone: no edit of the operator's own, and the
    /// heading that says so.
    CloseDialogParked,
```

In `size()`, beside the `EditPane` entries at `frames.rs:613`, with the arithmetic written down:

```rust
            // 160: the design target. The box is 86 interior plus a border
            // cell each side, so (160 - 88) / 2 = 36 dimmed columns each
            // side of it.
            Self::CloseDialog | Self::CloseDialogParked => (160, 48),
            // 90: the floor exactly, 86 + 2 border + 2 margin. A scene
            // pinned one column short of its own box would draw the
            // borderless form and silently stop showing the border it
            // exists to show.
            Self::CloseDialogFloor => (90, 48),
            // 89: one below the floor, which is the borderless form.
            Self::CloseDialogNarrow => (89, 48),
```

- [ ] **Step 2: Build each scene from real key presses**

Follow the `Scene::EditPane` arm at `frames.rs:1689`, which opens the pane with `KeyPress::Edit`, replies with `edit_pane_config_view()`, and files edits by selecting a field and pressing `Confirm`. Each new scene does that and then presses `KeyPress::Escape` once, which raises the dialog. `CloseDialogParked` files no edits at all.

- [ ] **Step 3: Pin and regenerate**

```bash
cargo test -p shep --lib --all-features frames_are_pinned
```

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

Then read the four new snapshots by eye. `CloseDialog` must show the border; `CloseDialogNarrow` must not. If the floor scene and the narrow scene render identically, the arithmetic is wrong.

- [ ] **Step 4: Docs**

`docs/lookout/README.md` gains a `What 1g settled` section matching the six already there, and its 12a bullet at line 72 changes: the apply menu it describes is deleted here, so the press-to-act carve-out now belongs to this dialog.

Then grep the site before assuming it is fine:

```bash
grep -rn "esc write\|apply menu\|parked\|close the pane" web/src/pages/docs/
```

- [ ] **Step 5: The docs gate**

```bash
cargo build --release
```

```bash
./web/scripts/generate-cli-reference.sh
```

`git diff` afterwards. No verb or flag moves in this plan, so the generated reference should not change. If it does, something else drifted and that is worth a look before it is committed.

```bash
cd web && npx astro build
```

```bash
cd web && npx astro check
```

Both. `build` does not typecheck, so a wrong prop passes it and only `check` reports it.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/frames.rs crates/shep-cli/src/lookout/snapshots docs web
git commit -m "docs(lookout): gallery scenes and docs for the close dialog"
```

---

## The task gate, once, when everything above is done

One command per invocation, `$?` read directly, never through a pipe:

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

Then, because this plan touches `cfg(unix)` code paths the local build does not compile:

```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

The local gate does not cover Linux or Windows. Read the CI result before calling the branch green.

## Self-review notes

Checked against the spec on 2026-09-12.

- Every spec section maps to a task: the trigger and `reaches_running` to 1 and 2, the three carve-outs to 2, keys and routing to 2, write-then-act to 3, layout and the border to 4, copy to 2, testing and gallery and docs to 5.
- `Edits::worst_impact` is not called anywhere in this plan, and the spec now says why: it answers with the heaviest group in the set, and this dialog names fields. Task 2 deletes it and its `rank` helper, since nothing else reads either and `Edit::impact` keeps the COST column fed. If a reviewer would rather keep them, the `expect(dead_code)` reason on both has to stop naming this frame.
- Type consistency: `CloseDialog::unsent()` returns a count and `unsent_fields()` the names, used in that split by the heading and the sentence under it. `ReloadKind` is the shipped enum from `crates/shep-cli/src/lookout/pane.rs:1833`, not a new one.
