//! `Scene`: the enum of gallery frames the pinned snapshots and
//! `docs/lookout/frames.{txt,ansi}` render, and the `scenes!` macro that
//! keeps it and its `ALL` listing from diverging.

use super::super::app::Control;

/// Declares `Scene` and its `ALL` listing from one variant list, so the two
/// cannot diverge by construction: a variant left out of the macro call
/// simply does not exist, and one included is in both the enum and `ALL` by
/// the same repetition. `scene_after`, below, still has to be kept in step
/// by hand — its own doc explains what it catches that this macro does not.
macro_rules! scenes {
    (
        $(#[$enum_doc:meta])*
        pub enum Scene {
            $(
                $(#[$variant_doc:meta])*
                $variant:ident,
            )*
        }
    ) => {
        $(#[$enum_doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Scene {
            $(
                $(#[$variant_doc])*
                $variant,
            )*
        }

        impl Scene {
            /// Every scene, in the order they appear in the gallery.
            pub const ALL: &'static [Self] = &[
                $(Self::$variant,)*
            ];
        }
    };
}

scenes! {
/// The scenes the frame snapshots pin and the gallery renders.
pub enum Scene {
    /// A healthy flock at a comfortable width, all three panes up.
    HealthyWide,
    /// One sheep errored, one waiting to restart, one stopped.
    Errored,
    /// Three instances of one app under a group header, with the cursor on
    /// the header.
    Grouped,
    /// `F` pressed: the flock gathered by fold instead of by name. Two folds
    /// of differing size, one of them collapsed, an app grouped inside the
    /// larger fold, two sheep with no fold at all, and a dog, which is
    /// never in a fold.
    Folds,
    /// Both sections at once: several sheep under Flock, a healthy
    /// built-in dog and a silent adopted one under Dogs.
    WithDogs,
    /// The `MEM/CEIL` gauge's three states in one frame: comfortably under
    /// a configured ceiling, at 94% of one (the butter warning role, where
    /// there is colour to carry it), and no ceiling configured at all.
    MemCeiling,
    /// The `CFG` column's `!N` and `*N` markers, one sheep each, plus a CPU
    /// history long enough that the `CpuSpark` column draws a shape rather
    /// than a single bar.
    CfgDrift,
    /// Nothing registered.
    Empty,
    /// A narrow terminal: four columns dropped.
    Narrow,
    /// Below the floor.
    TooNarrow,
    /// Mid-reconnect.
    Retrying,
    /// The shepherd is gone and the values are frozen.
    Frozen,
    /// The read-only refusal.
    Refused,
    /// Mid-type: the table has already narrowed and the box is still open.
    FilterEditing,
    /// Applied and no longer editing.
    FilterActive,
    /// A query nothing matches.
    FilterNoMatch,
    /// 20 rows: the 18-tier. The detail pane is gone; the strip and the feed
    /// are not.
    NoDetail,
    /// 12 rows: below every optional-pane threshold. 12a's frame.
    TableOnly,
    /// The feed under a burst: lines dropped and bytes never read.
    FeedGap,
    /// The selected sheep has never written a log in this `$SHEP_HOME`.
    FeedMissing,
    /// 33x26: the narrowest terminal that still draws all three panes.
    Cramped,
    /// `sysinfo` reports this platform unsupported.
    HostUnknown,
    /// The detail pane with a lamb list.
    Lambs,
    /// The detail pane on a sheep with no pid, where the shepherd had no tree
    /// to walk.
    LambsUnknown,
    /// An action key pressed with the gate open. Nothing has been sent.
    Confirm,
    /// Enter pressed. The request is out.
    Acting,
    /// The shepherd refused, in its own words.
    ActionRefused,
    /// The shepherd did it, and the bar says so in the non-grave style.
    ActionAccepted,
    /// An action key pressed while the link is coming back.
    ActionRefusedOffline,
    /// The settings screen on a fresh `$SHEP_HOME`: only `[interpreters]` on
    /// disk, so every scalar reads its compiled default. The state most
    /// operators open the screen in.
    SettingsFresh,
    /// The settings screen with some scalars declared, so `shep.toml` and
    /// the default sit side by side.
    SettingsSet,
    /// The settings screen with a `[daemon]` confirm armed, naming the
    /// variable and the flag it cannot see.
    SettingsConfirm,
    /// The settings screen with the `socket` editor open mid-path.
    SettingsTyping,
    /// The settings screen's dogs table, showing the drift it exists to
    /// make visible.
    SettingsDogs,
    /// The settings screen on a narrow terminal, where both of its tables
    /// have dropped a column.
    SettingsNarrow,
    /// The settings screen on a terminal too short to hold every row, with
    /// the cursor on the last one, so the view has scrolled.
    SettingsShort,
    /// The full-screen bleats pane with all three filter axes stacked: a
    /// stream, a minimum level and a regex, wrapping turned on so the one
    /// surviving line's full text is on screen rather than truncated.
    Bleats,
    /// `S` pressed: the secrets pane, with `DB_PASSWORD` revealed, a
    /// `vercel` provider group and `ELSEWHERE_ONLY` sitting unresolved for
    /// this environment, tall enough to draw both the FOCUSED and WHO
    /// READS IT panels.
    Secrets,
    /// The sheep pane at its design size, 160x48: both charts, the config
    /// and env column, and the embedded feed, all up at once.
    SheepPane,
    /// 139x48: under 140 columns, the memory chart becomes a one-line
    /// `rss` summary and the CPU chart draws alone.
    SheepPaneCpuOnly,
    /// 99x48: under 100 columns, both charts collapse into 1a's own
    /// `CPU 20s` sparkline and `MEM/CEIL` gauge, one row.
    SheepPaneSparklines,
    /// 160x25: the charts hold their design width but not their design
    /// height. The memory chart is gone; the config and feed columns,
    /// which give ground last, are still up.
    SheepPaneShort,
    /// The redrawn editing pane, fresh: no edits filed, at the design
    /// target of 160x48, where the explanation panel and the `LANDS`
    /// column both draw.
    EditPane,
    /// The same pane with two edits filed: one that lands at once and one
    /// that needs a respawn, so the pending section and the title's own
    /// count both appear.
    EditPaneEdited,
    /// The fresh pane at 120 columns: the panel still draws, `LANDS`
    /// gives way to it.
    EditPaneSqueezed,
    /// The fresh pane at 88 columns: the panel is gone, so `LANDS` is
    /// back, since nothing else on screen carries cost.
    EditPaneNarrow,
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
    /// `h` pressed at the design target, 160x48: boxed, all four columns,
    /// the sheep, and the gate line reading control enabled.
    Keymap,
    /// The overlay at 130 columns, the narrowest the box's own border can
    /// still hold.
    KeymapFloor,
    /// One column below the box's floor of 130, at 129: the widest borderless
    /// form, still carrying all four columns.
    KeymapBorderlessWide,
    /// 100 columns: three columns share one bank and the fourth, DOING,
    /// drops to a bank of its own.
    KeymapNarrow,
    /// 70 columns: two columns per bank, two banks stacked.
    KeymapTwoColumn,
    /// The overlay raised after the link is lost: the gate line names the
    /// dead link rather than either control state.
    KeymapFrozen,
    /// The overlay raised under `--read-only`: the gate line reads
    /// read-only rather than control enabled.
    KeymapReadOnly,
    /// 160x16: under the box's own nineteen rows, so the boxed-only sheep
    /// is already gone and the `NO_COLOR` sentence sheds too, while every
    /// key row survives.
    KeymapShort,
}
}

impl Scene {
    /// The snapshot name and the gallery heading.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::HealthyWide => "healthy_wide",
            Self::Errored => "errored",
            Self::Grouped => "grouped",
            Self::Folds => "folds",
            Self::WithDogs => "with_dogs",
            Self::MemCeiling => "mem_ceiling",
            Self::CfgDrift => "cfg_drift",
            Self::Empty => "empty",
            Self::Narrow => "narrow",
            Self::TooNarrow => "too_narrow",
            Self::Retrying => "retrying",
            Self::Frozen => "frozen",
            Self::Refused => "refused",
            Self::FilterEditing => "filter_editing",
            Self::FilterActive => "filter_active",
            Self::FilterNoMatch => "filter_no_match",
            Self::NoDetail => "no_detail",
            Self::TableOnly => "table_only",
            Self::FeedGap => "feed_gap",
            Self::FeedMissing => "feed_missing",
            Self::Cramped => "cramped",
            Self::HostUnknown => "host_unknown",
            Self::Lambs => "lambs",
            Self::LambsUnknown => "lambs_unknown",
            Self::Confirm => "confirm",
            Self::Acting => "acting",
            Self::ActionRefused => "action_refused",
            Self::ActionAccepted => "action_accepted",
            Self::ActionRefusedOffline => "action_refused_offline",
            Self::SettingsFresh => "settings_fresh",
            Self::SettingsSet => "settings_set",
            Self::SettingsConfirm => "settings_confirm",
            Self::SettingsTyping => "settings_typing",
            Self::SettingsDogs => "settings_dogs",
            Self::SettingsNarrow => "settings_narrow",
            Self::SettingsShort => "settings_short",
            Self::Bleats => "bleats",
            Self::Secrets => "secrets",
            Self::SheepPane => "sheep_pane",
            Self::SheepPaneCpuOnly => "sheep_pane_cpu_only",
            Self::SheepPaneSparklines => "sheep_pane_sparklines",
            Self::SheepPaneShort => "sheep_pane_short",
            Self::EditPane => "edit_pane",
            Self::EditPaneEdited => "edit_pane_edited",
            Self::EditPaneSqueezed => "edit_pane_squeezed",
            Self::EditPaneNarrow => "edit_pane_narrow",
            Self::CloseDialog => "close_dialog",
            Self::CloseDialogFloor => "close_dialog_floor",
            Self::CloseDialogNarrow => "close_dialog_narrow",
            Self::CloseDialogParked => "close_dialog_parked",
            Self::Keymap => "keymap",
            Self::KeymapFloor => "keymap_floor",
            Self::KeymapBorderlessWide => "keymap_borderless_wide",
            Self::KeymapNarrow => "keymap_narrow",
            Self::KeymapTwoColumn => "keymap_two_column",
            Self::KeymapFrozen => "keymap_frozen",
            Self::KeymapReadOnly => "keymap_read_only",
            Self::KeymapShort => "keymap_short",
        }
    }

    /// One sentence saying what this frame is for, printed above it in the
    /// gallery.
    ///
    /// `every_scene_shows_the_thing_it_is_named_for` pins each clause: a
    /// caption may not claim what the frame is not asserted to show.
    #[must_use]
    pub const fn caption(self) -> &'static str {
        match self {
            Self::HealthyWide => {
                "All three panes at 120x30: the host strip under the title, the detail pane and the bleats feed under the table. The selected row is marked, painted where the palette has a ground to paint with and shown with a `>` where it does not, and every pane below the table describes that sheep, whose log row reads a real size off disk rather than the placeholder every other scene's fictional path leaves blank."
            }
            Self::Errored => {
                "One errored, one waiting to restart, one stopped, with the selection parked on the errored sheep. Each row's own STATUS cell is the only coloured cell in that row, and EXIT carries why each of the three stopped: a code for the two that crashed, a signal name for the one shep stopped itself."
            }
            Self::Grouped => {
                "Three instances of one app under a group header, with the cursor parked on the header. The header sums their restarts, CPU and memory and takes the SHORTEST of their uptimes, so a group reads as time since the app was last disturbed rather than as the age of its luckiest instance. The detail pane repeats that rollup and says lambs are per-instance; the feed will not guess which instance to tail."
            }
            Self::Folds => {
                "160 columns, wide enough for the full fold-view column set including SHARE and NOTES. `batch` and `core` are two folds of differing size, `batch` collapsed so only its header shows; `edge` is the largest, holding the grouped app `web` \u{d7}3 alongside standalone `api`, and the cursor is parked on its header. `cron` and `metrics` carry no fold and sit under a `no fold` band that is not selectable, and `bark`, a dog, sits under its own band because a dog is never in a fold. Each fold header sums its members' restarts, CPU and memory and takes the shortest of their uptimes, and its SHARE gauge and NOTES percentage both read that fold's share of the whole flock's memory."
            }
            Self::WithDogs => {
                "Three sheep under a FLOCK band and two dogs under a DOGS band: bark is built-in and healthy, log-rotate is adopted from /usr/local/bin/shep-log-rotate and has never handshaken, so its STATUS reads silent rather than online, and the cursor is parked on it."
            }
            Self::MemCeiling => {
                "Three sheep with a max_memory ceiling configured, or not: web-headroom sits at a quarter of its limit and its MEM/CEIL gauge fills a little, web-hot sits at 94 percent of its own limit and its gauge fills almost all the way, and where there is colour the fuller gauge also switches to the butter warning role. batch-worker has no ceiling at all, so its gauge reads the same muted bar a stopped sheep's does. The cursor is parked on web-hot, the row this frame exists to show."
            }
            Self::CfgDrift => {
                "140 columns: wide enough for the CFG and CPU 20s columns beside the ones every other scene shows. web has two fields parked for the next spawn and reads !2, api has one field an operator set that its Flockfile does not declare and reads *1, and cron has neither and reads a bare -. The status bar's own legend explains both glyphs; this is where they actually appear in a cell. web's counter is differenced across several two-second polls at varying deltas, so its CPU history draws a shape over time rather than one static bar, and the cursor is parked on web so the detail pane's own cfg !2 pending cell is on screen too."
            }
            Self::Empty => {
                "No sheep registered. Each of the three panes says why it is empty, and the three sentences are different because the three reasons are."
            }
            Self::Narrow => {
                "51 columns: FOLD, EXIT, RESTARTS, PID and MEM are gone, in that order. CPU and UPTIME survive because they explain WHY a RUNNING sheep is behaving badly, a question EXIT cannot even ask. The host strip truncates with an ellipsis rather than disappearing; the detail pane and the feed do not fit at all, at 14 rows."
            }
            Self::TooNarrow => {
                "28 columns: below the floor, the pane refuses rather than drawing overlapping garbage. Two short lines, so the refusal still fits the terminal it is refusing about."
            }
            Self::Retrying => {
                "The shepherd stopped answering. Five attempts over about eight seconds before this becomes the next frame. Every pane below the table keeps describing the selected sheep from the last listing."
            }
            Self::Frozen => {
                "The ladder ran out. The band says so in bark, the whole table and the host strip go to one muted ink so no cell reads as live, and UPTIME becomes FROZEN over a duration that has stopped advancing. The detail band and the feed give their rows to the link panel: they describe a shepherd that is gone, and it describes what happened."
            }
            Self::Refused => {
                "`x` with actions gated off. The refusal is literal, nothing about damage gets charming, and the panes below carry on."
            }
            Self::FilterEditing => {
                "Mid-type at 100x14. The table has already narrowed to the two sheep whose names contain the query, the title counts the narrowed set and the whole flock, and the status bar carries the query, a cursor, and the three keys that mean anything while the box is open."
            }
            Self::FilterActive => {
                "The same query applied. The box is closed, the table is still narrowed, and the bar has changed to name the two keys that now touch the filter."
            }
            Self::FilterNoMatch => {
                "A query nothing matches. The table names the query rather than claiming the flock is empty, and the title keeps the flock's real size on screen."
            }
            Self::NoDetail => {
                "20 rows: the detail pane is the first to go, because every number on it but the log paths is already in the row above it."
            }
            Self::TableOnly => {
                "12 rows: no optional panes at all. This is 12a's frame, and the only thing that changed is the two-column gutter that marks the selection, painted where there is a ground to paint with and shown with `>` where there is not."
            }
            Self::FeedGap => {
                "The feed under a burst: 3.8 megabytes were never read and some hundreds of lines were read and dropped. The pane counts both, and counts them separately, because it knows the second exactly and cannot know how many lines are in the first."
            }
            Self::FeedMissing => {
                "The selected sheep has never written a log in this $SHEP_HOME. The feed names that cause rather than sitting blank."
            }
            Self::Cramped => {
                "33 columns: the narrowest terminal that draws. 26 rows (a couple more than the 24-row floor for all three panes being up), so this frame has a little breathing room rather than sitting exactly on the edge. Everything truncates with an ellipsis; nothing overlaps."
            }
            Self::HostUnknown => {
                "`sysinfo` reports this platform unsupported. The strip says so and keeps the flock's own totals, which lookout can always compute."
            }
            Self::Lambs => {
                "The detail pane with a lamb list: how many descendants the shepherd's walk found, how old that reading is, and each lamb's pid and executable name. The stamp sits before the list so a narrow terminal truncates lambs rather than the caveat."
            }
            Self::LambsUnknown => {
                "The same pane on a stopped sheep. The shepherd had no pid to walk from and left the field unset rather than empty, and the line says which of the two it is looking at rather than reporting none found."
            }
            Self::Confirm => {
                "`R` pressed with the gate open. Nothing has been sent: the bar asks a question naming the verb and the exact sheep, and `api` is still online in the table behind it."
            }
            Self::Acting => {
                "Enter pressed. The request is out and nothing on the table has changed, because nothing the shepherd has said has changed: `api` is still online and the cursor has not moved."
            }
            Self::ActionAccepted => {
                "The shepherd answered. The bar says what it did, in the non-grave style a refusal does not get, and the table shows the row the reply carried rather than waiting for the next poll."
            }
            Self::ActionRefused => {
                "The shepherd refused while the request was out, and its own sentence is forwarded rather than rewritten. The sheep has left the flock in the listing behind it, so the table is one row shorter and the cursor has moved to the row below."
            }
            Self::ActionRefusedOffline => {
                "An action key pressed while the link is coming back. The refusal names the same reconnect attempt the banner above it does, rather than the exhausted-ladder sentence. Phase 16 review Minor #8 caught the two disagreeing on one frame."
            }
            Self::SettingsFresh => {
                "The settings screen on a fresh $SHEP_HOME: shep.toml holds only [interpreters], so every scalar's SOURCE column reads `the default` and its VALUE is the compiled fallback. The state most operators open this screen in."
            }
            Self::SettingsSet => {
                "The settings screen with some scalars declared. `shep.toml` and `the default` sit side by side in the SOURCE column, so what the operator wrote and what shep assumes are both visible at once."
            }
            Self::SettingsConfirm => {
                "A [daemon] confirm armed on log_level, naming the variable and the flag it cannot see: SHEP_LOG_LEVEL and --log-level, and the shep daemon reload the edit needs."
            }
            Self::SettingsTyping => {
                "The socket editor open mid-path. The status bar names the field being typed, not the dashboard's own filter box, which is what it showed here before the fix this frame now pins."
            }
            Self::SettingsDogs => {
                "The dogs table showing the drift it exists to reveal: otel running while disabled in the file, ledger enabled and absent, and bark enabled, running, and silent."
            }
            Self::SettingsNarrow => {
                "The same screen at 45 columns. Both of its tables have dropped a column rather than clipping: the scalar rows have lost the apply cost and kept SOURCE, and the dogs table has lost SOURCE and kept RUNNING. Each keeps whichever half is not said anywhere else."
            }
            Self::SettingsShort => {
                "The same screen at 14 rows, which is fewer than it has to draw. The cursor is on the last dog, so the view has scrolled to reach it and `... 5 above` says how much is off the top. The scroll is counted in LINES rather than in rows: a section header and the dogs caption cost the same height a row does."
            }
            Self::Bleats => {
                "The full-screen bleats pane, pinned to api, with a stream, a minimum level and a regex all stacked: only out, only warn and above, only a line mentioning retrying or jitter. The filter row states the composition and counts one surviving line out of sixteen, and that one line is also the longest in the fixture, so wrapping is on and its full text runs onto a second row instead of an ellipsis."
            }
            Self::Secrets => {
                "The secrets pane, opened with `S`. DB_PASSWORD is revealed and named by one reader; vercel/API_TOKEN sits in its own read-only provider group; ELSEWHERE_ONLY has a slot in ci but not here, so it reads unresolved for this tab. The frame is tall enough to carry both the FOCUSED panel and WHO READS IT below the table."
            }
            Self::SheepPane => {
                "The sheep pane on web, at its design size: 160 = 8 gutter + 140 body + 12 margin for each chart, and 160 = 76 config + 1 divider + 83 feed across the row below them. Both charts draw their full body, the config and env column lists web's own fields, and the embedded feed carries its own lines, all on one screen. web's cpu_ms counter is differenced across several two-second polls, rising by varying deltas, so both charts draw a shape rather than a single repeated bar."
            }
            Self::SheepPaneCpuOnly => {
                "139 columns: one cell under the 140 the full two-chart body needs. The CPU chart still draws at its own body width, but the memory chart is gone, replaced by a one-line `rss` summary and a ten-cell gauge: memory still has a gauge to fall back on, and the CPU chart is the more diagnostic of the two, so it is the one that stays."
            }
            Self::SheepPaneSparklines => {
                "99 columns: one cell under the 100 the CPU-only tier needs. Both charts are gone, and the pane falls back to 1a's own pair: the `CPU 20s` sparkline and the `MEM/CEIL` gauge, on one row."
            }
            Self::SheepPaneShort => {
                "160 columns, 25 rows: the charts hold their design width but not their design height. Under 26 rows the memory chart goes; the CPU chart and the config and env column, which give ground last, are still up."
            }
            Self::EditPane => {
                "The redrawn editing pane on api, fresh: no edits filed, at 160x48. The tab row names all eight groups with only the active one chipped, the header reads FIELD / VALUE / LANDS, the explanation panel sits beside the field list naming the field under the cursor, and every env value reads (set) rather than the value itself."
            }
            Self::EditPaneEdited => {
                "The same pane with two edits filed: cwd, which needs a respawn, and max_memory, which lands at once. The pending edits section lists both under the active group's own fields, and the title band reads 2 edits."
            }
            Self::EditPaneSqueezed => {
                "The same fresh pane at 120 columns: narrow enough that LANDS gives way to the explanation panel, which still names the focused field's own cost in words."
            }
            Self::EditPaneNarrow => {
                "The same fresh pane at 88 columns: narrow enough that the explanation panel is gone, so LANDS is back, since nothing else on screen carries cost."
            }
            Self::CloseDialog => {
                "esc pressed with cwd and err_file edited and listen_timeout already parked from before: the close dialog names both counts, boxed and centred at 160x48 over the dimmed editing pane."
            }
            Self::CloseDialogFloor => {
                "The same dialog at 90 columns, exactly the floor its border needs: 86 interior cells plus a border cell and a margin cell each side."
            }
            Self::CloseDialogNarrow => {
                "The same dialog one column under the floor, at 89: the border is dropped and the dialog draws full width instead of clipping."
            }
            Self::CloseDialogParked => {
                "esc pressed with no edit of the operator's own, only listen_timeout already parked: the heading names the parked half alone."
            }
            Self::Keymap => {
                "`h` pressed at 160x48, the design target: boxed at 128 columns (126 interior plus one border cell each side), all four columns drawn whole, the sheep once in the DOING column's own rows, and the gate line reading control enabled over the dimmed dashboard behind it."
            }
            Self::KeymapFloor => {
                "The same overlay at 130 columns, the narrowest the border can still hold: 130 = 128 (126 interior plus one border cell each side) plus one margin cell each side. One column narrower and the border is gone."
            }
            Self::KeymapBorderlessWide => {
                "129 columns, one below KeymapFloor's own 130: the border is gone, but columns_for(129) is still 4, so this is the widest form the overlay ever draws with all four columns and no frame around them. It sits next to KeymapFloor on purpose, so the two together pin the boundary rather than bracketing it loosely."
            }
            Self::KeymapNarrow => {
                "100 columns: columns_for(100) is 3, so MOVING, LOOKING and CHANGING share one bank and DOING drops to a bank of its own below a blank separator row."
            }
            Self::KeymapTwoColumn => {
                "70 columns: columns_for(70) is 2, so the four groups split into two banks of two, MOVING and LOOKING on top and CHANGING and DOING below."
            }
            Self::KeymapFrozen => {
                "The overlay raised after the link is already lost: the gate line names the dead link, not either control state, the same precedence the status bar's own right-hand label gives Link::Lost over Control."
            }
            Self::KeymapReadOnly => {
                "The overlay raised under --read-only: the gate line reads read-only where the design target's own reads control enabled."
            }
            Self::KeymapShort => {
                "160x16, under the nineteen rows the box needs: the sheep is boxed-only and already gone, and the borderless form has shed to its own Decoration tier, which drops the NO_COLOR sentence too, while every key row is still on screen."
            }
        }
    }

    /// Whether this scene's dashboard may act.
    ///
    /// Allowed is the fallthrough, matching the real dashboard's default.
    /// Two exceptions, and they show the closed gate in different places:
    /// `Refused` in the dashboard's own status bar, `KeymapReadOnly` in the
    /// overlay's gate line.
    #[must_use]
    pub const fn control(self) -> Control {
        match self {
            Self::Refused | Self::KeymapReadOnly => Control::ReadOnly,
            _ => Control::Allowed,
        }
    }

    /// The terminal size this scene is rendered at.
    #[must_use]
    pub const fn size(self) -> (u16, u16) {
        match self {
            Self::Empty => (100, 28),
            // 150: `columns_for` runs on `width - GUTTER`, so 150 - GUTTER =
            // 148, past `ALL`'s 146 threshold: the one scene that needs
            // both `CpuSpark` and `MemCeil` drawn, not dropped for width.
            Self::MemCeiling => (150, 30),
            // 140: `columns_for` runs on `width - GUTTER`, so 140 - GUTTER =
            // 138, past `NO_CEIL`'s 134 threshold and short of `ALL`'s 146:
            // `CpuSpark` and `Cfg` both draw, `MemCeil` does not (it has its
            // own scene above).
            Self::CfgDrift => (140, 30),
            // 51: `columns_for` runs on `width - GUTTER` (the two-column
            // selection marker), so 51 - GUTTER = 49, the `NO_MEM` tier:
            // four columns gone, CPU and UPTIME still there.
            Self::Narrow => (51, 14),
            Self::TooNarrow => (28, 8),
            Self::FilterEditing | Self::FilterActive | Self::FilterNoMatch => (100, 14),
            Self::NoDetail => (120, 20),
            Self::TableOnly => (120, 12),
            Self::Cramped => (33, 26),
            // `fold_columns_for` runs on `width - GUTTER`, so 160 - GUTTER
            // is exactly `FOLD_ALL`'s threshold: the one scene that needs the
            // full column set, SHARE and NOTES included.
            Self::Folds => (160, 30),
            // The design target, and comfortably past the floor this
            // scene actually needs. `columns_for` runs on `width - GUTTER`,
            // and `ALL`'s own threshold is the sum of what it draws:
            //
            //   ALL's fixed widths        = 112
            //   13 two-cell separators    =  26
            //   NAME at its floor         =   8  (NAME_MIN)
            //   112 + 26 + 8              = 146, `TIERS`'s widest entry
            //
            // So 148 columns is the floor and 160 leaves NAME 20 cells,
            // which is what the frame drew. `the_frozen_scene_draws_every_
            // column` pins the floor; the extra twelve are the design's.
            //
            // At the 120 this used to inherit from the default arm, the
            // table renders the NO_SPARK tier: no CPU sparkline and no
            // MEM/CEIL gauge, which is two of the cells the frame exists to
            // show frozen. 30 rows, not the design's 48: the link panel is
            // six rows where the two panes it replaces were twelve, so
            // nothing here needs the taller frame.
            Self::Frozen => (160, 30),
            Self::Confirm
            | Self::Acting
            | Self::ActionRefused
            | Self::ActionAccepted
            | Self::ActionRefusedOffline => (100, 14),
            // 180: the log_level confirm's sentence runs to 170 columns,
            // past every other scene's width. Narrower would truncate the
            // flag it shows.
            Self::SettingsConfirm => (180, 30),
            // 45: the middle tier of both `SCALAR_TIERS` and `DOG_TIERS`,
            // so both tables have dropped one column without losing a row.
            Self::SettingsNarrow => (45, 24),
            // 14 rows: twelve of body, against a screen that wants
            // eighteen lines. Short enough that the cursor cannot be
            // reached without scrolling, tall enough that what survives is
            // a legible section rather than a single row.
            Self::SettingsShort => (120, 14),
            // 100: `bleats_full`'s own text width is `width - TAG_PREFIX_WIDTH`
            // (5), so 100 - 5 = 95. The fixture's one surviving line is 153
            // characters, so it wraps onto exactly two rows at this width
            // (153 / 95, rounded up); a wider frame would still wrap it but
            // would no longer pin that arithmetic. 14 rows, the same tier
            // `Confirm` and its siblings use: a title, a filter row and two
            // wrapped rows fit inside `body_rows`'s 12 with room to spare.
            //
            // The title truncates here and that is the honest render: two
            // full log paths plus the window figures do not fit 100 columns,
            // and `fit` cuts what a real terminal would. Widening to show
            // them whole would unpin the wrap arithmetic above, which is what
            // this scene exists for. `the_title_names_the_window_and_what_
            // fell_below_it` covers the figures at a width that fits them.
            Self::Bleats => (100, 14),
            // Tall enough that `content_bottom` leaves room for both
            // `draw_panels`' FOCUSED and WHO READS IT rows below the table,
            // the frame this scene exists to show.
            Self::Secrets => (160, 48),
            // 160 = 8 gutter + 140 body + 12 margin for each chart, and
            // 160 = 76 config + 1 divider + 83 feed: the sheep pane's own
            // design size, wide enough for every column at once.
            Self::SheepPane => (160, 48),
            // 139: one cell under the 140 the full two-chart body needs, so
            // `chart_tier` downgrades to `CpuOnly` on width alone.
            Self::SheepPaneCpuOnly => (139, 48),
            // 99: one cell under the 100 `CpuOnly` needs, so `chart_tier`
            // downgrades again, to `Sparkline`.
            Self::SheepPaneSparklines => (99, 48),
            // 25 rows: one short of `FULL_TIER_MIN_HEIGHT` (26), so the
            // memory chart drops while the CPU chart, the config column and
            // the feed all still fit.
            Self::SheepPaneShort => (160, 25),
            // The design target: 45% of 160 is 72, so both the panel and
            // `LANDS` draw at their widest.
            Self::EditPane | Self::EditPaneEdited => (160, 48),
            // 120: past `panel_width`'s `LEFT_MIN` floor, so the panel
            // still draws, but short of `LANDS_WITH_PANEL_MIN` (160), so
            // `LANDS` gives way to it.
            Self::EditPaneSqueezed => (120, 48),
            // 88: short of `panel_width`'s `LEFT_MIN` floor (`88 - 50 <
            // 40`), so the panel does not draw at all and `LANDS` returns.
            Self::EditPaneNarrow => (88, 48),
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
            // 160: the design target, boxed at 128 (126 interior plus one
            // border cell each side), so (160 - 128) / 2 = 16 dimmed columns
            // each side of it.
            Self::Keymap | Self::KeymapFrozen | Self::KeymapReadOnly => (160, 48),
            // 130: `overlay::floor_for(126)`, the narrowest width the box's
            // own border can still draw at.
            Self::KeymapFloor => (130, 48),
            // 129: one below the floor of 130, the widest borderless form that
            // still carries all four columns (`columns_for(129) == 4`).
            Self::KeymapBorderlessWide => (129, 48),
            // 100: `columns_for(100) == 3`, so DOING drops to a bank of
            // its own.
            Self::KeymapNarrow => (100, 48),
            // 70: `columns_for(70) == 2`, two banks of two columns each.
            Self::KeymapTwoColumn => (70, 48),
            // 16 rows: under the box's own 19, so the sheep (boxed-only) is
            // already gone, and the borderless form has shed to its own
            // `Shed::Decoration`, which drops the NO_COLOR line too; every
            // key row stays intact. 16, not 22: at 22 the box holds whole
            // and nothing sheds.
            Self::KeymapShort => (160, 16),
            // HealthyWide, Errored, Grouped, WithDogs, Retrying, Refused,
            // FeedGap, FeedMissing, HostUnknown, Lambs, LambsUnknown: every
            // scene that carries all three optional panes at their ordinary
            // rows.
            _ => (120, 30),
        }
    }

    /// Whether this scene raises the keymap overlay before it renders.
    ///
    /// Every arm named on both sides rather than a wildcard on either, so
    /// a ninth `Keymap*` variant fails to compile here instead of
    /// rendering silently without the overlay raised.
    #[must_use]
    pub const fn is_keymap(self) -> bool {
        match self {
            Self::Keymap
            | Self::KeymapFloor
            | Self::KeymapBorderlessWide
            | Self::KeymapNarrow
            | Self::KeymapTwoColumn
            | Self::KeymapFrozen
            | Self::KeymapReadOnly
            | Self::KeymapShort => true,
            Self::HealthyWide
            | Self::Errored
            | Self::Grouped
            | Self::Folds
            | Self::WithDogs
            | Self::MemCeiling
            | Self::CfgDrift
            | Self::Empty
            | Self::Narrow
            | Self::TooNarrow
            | Self::Retrying
            | Self::Frozen
            | Self::Refused
            | Self::FilterEditing
            | Self::FilterActive
            | Self::FilterNoMatch
            | Self::NoDetail
            | Self::TableOnly
            | Self::FeedGap
            | Self::FeedMissing
            | Self::Cramped
            | Self::HostUnknown
            | Self::Lambs
            | Self::LambsUnknown
            | Self::Confirm
            | Self::Acting
            | Self::ActionRefused
            | Self::ActionAccepted
            | Self::ActionRefusedOffline
            | Self::SettingsFresh
            | Self::SettingsSet
            | Self::SettingsConfirm
            | Self::SettingsTyping
            | Self::SettingsDogs
            | Self::SettingsNarrow
            | Self::SettingsShort
            | Self::Bleats
            | Self::Secrets
            | Self::SheepPane
            | Self::SheepPaneCpuOnly
            | Self::SheepPaneSparklines
            | Self::SheepPaneShort
            | Self::EditPane
            | Self::EditPaneEdited
            | Self::EditPaneSqueezed
            | Self::EditPaneNarrow
            | Self::CloseDialog
            | Self::CloseDialogFloor
            | Self::CloseDialogNarrow
            | Self::CloseDialogParked => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::super::GALLERY_PREAMBLE;
    use super::super::scene;
    use super::*;

    /// fails if a scene stops rendering what it is named for.
    ///
    /// Each caption clause in [`Scene::caption`] is pinned by one
    /// assertion here.
    /// The preamble's own scene count matches `Scene::ALL`.
    ///
    /// `write_the_gallery` commits the preamble into
    /// `docs/lookout/frames.txt`, so a wrong count is a document that
    /// miscounts itself for an operator. Spelled out rather than a digit,
    /// which is the form a reader sees.
    #[test]
    fn the_gallery_preamble_counts_the_scenes_it_has() {
        const NUMBERS: [(usize, &str); 25] = [
            (34, "thirty-four"),
            (35, "thirty-five"),
            (36, "thirty-six"),
            (37, "thirty-seven"),
            (38, "thirty-eight"),
            (39, "thirty-nine"),
            (40, "forty"),
            (41, "forty-one"),
            (42, "forty-two"),
            (43, "forty-three"),
            (44, "forty-four"),
            (45, "forty-five"),
            (46, "forty-six"),
            (47, "forty-seven"),
            (48, "forty-eight"),
            (49, "forty-nine"),
            (50, "fifty"),
            (51, "fifty-one"),
            (52, "fifty-two"),
            (53, "fifty-three"),
            (54, "fifty-four"),
            (55, "fifty-five"),
            (56, "fifty-six"),
            (57, "fifty-seven"),
            (58, "fifty-eight"),
        ];
        let spelled = NUMBERS
            .iter()
            .find(|(n, _)| *n == Scene::ALL.len())
            .map(|(_, word)| *word)
            .expect("add the next number to NUMBERS when the gallery outgrows it");
        // `fifty` is a prefix of `fifty-one` through `fifty-eight`, so a bare
        // `contains` passes when the preamble overstates inside its own
        // decade: 50 scenes against a preamble saying "fifty-eight" satisfies
        // `contains("fifty")`. Refusing the hyphen is what makes the match a
        // whole number rather than a prefix of a larger one.
        assert!(
            !GALLERY_PREAMBLE.contains(&format!("{spelled}-")),
            "the preamble spells a larger number than {} scenes: {spelled}-…",
            Scene::ALL.len()
        );
        assert!(
            GALLERY_PREAMBLE.contains(spelled),
            "the preamble says something other than {spelled}, and \
             `write_the_gallery` commits it for an operator to read"
        );
    }

    /// Only checks that every scene has a caption, since whether the
    /// caption is pinned by an assertion is not machine-checkable.
    #[test]
    fn every_scene_has_a_caption_and_a_distinct_label() {
        let mut labels = std::collections::BTreeSet::new();
        for which in Scene::ALL {
            assert!(
                labels.insert(which.label()),
                "two scenes share {}",
                which.label()
            );
            let caption = which.caption();
            assert!(caption.len() > 30, "{} has a stub caption", which.label());
            assert!(
                caption.ends_with('.'),
                "{}'s caption is not a sentence",
                which.label()
            );
        }
    }

    /// The scene that follows `scene` in gallery order, or `None` after the
    /// last one. Mirrors `Scene::ALL`'s own order.
    ///
    /// Exhaustive over [`Scene`], with no wildcard arm: a variant added to
    /// the enum without an arm here fails to compile. `scene_all_lists_every_variant_the_compiler_can_see`
    /// below walks this chain and checks it against `ALL`, which a
    /// hand-counted length could not: a forgotten scene just left the count
    /// honest at the old number.
    ///
    /// The `scenes!` macro that declares `Scene` keeps a variant from being
    /// left out of `ALL` outright — the two are generated from one list, so
    /// they cannot disagree on membership. What that macro cannot catch is
    /// this chain: a variant could still get an arm here that loops back on
    /// itself or points somewhere unreachable from `HealthyWide`, compiling
    /// fine while never being walked. That is exactly what this test's own
    /// walk-and-compare catches, so the two mechanisms are covering
    /// different halves of the same failure, not one covering the other.
    fn scene_after(scene: Scene) -> Option<Scene> {
        match scene {
            Scene::HealthyWide => Some(Scene::Errored),
            Scene::Errored => Some(Scene::Grouped),
            Scene::Grouped => Some(Scene::Folds),
            Scene::Folds => Some(Scene::WithDogs),
            Scene::WithDogs => Some(Scene::MemCeiling),
            Scene::MemCeiling => Some(Scene::CfgDrift),
            Scene::CfgDrift => Some(Scene::Empty),
            Scene::Empty => Some(Scene::Narrow),
            Scene::Narrow => Some(Scene::TooNarrow),
            Scene::TooNarrow => Some(Scene::Retrying),
            Scene::Retrying => Some(Scene::Frozen),
            Scene::Frozen => Some(Scene::Refused),
            Scene::Refused => Some(Scene::FilterEditing),
            Scene::FilterEditing => Some(Scene::FilterActive),
            Scene::FilterActive => Some(Scene::FilterNoMatch),
            Scene::FilterNoMatch => Some(Scene::NoDetail),
            Scene::NoDetail => Some(Scene::TableOnly),
            Scene::TableOnly => Some(Scene::FeedGap),
            Scene::FeedGap => Some(Scene::FeedMissing),
            Scene::FeedMissing => Some(Scene::Cramped),
            Scene::Cramped => Some(Scene::HostUnknown),
            Scene::HostUnknown => Some(Scene::Lambs),
            Scene::Lambs => Some(Scene::LambsUnknown),
            Scene::LambsUnknown => Some(Scene::Confirm),
            Scene::Confirm => Some(Scene::Acting),
            Scene::Acting => Some(Scene::ActionRefused),
            Scene::ActionRefused => Some(Scene::ActionAccepted),
            Scene::ActionAccepted => Some(Scene::ActionRefusedOffline),
            Scene::ActionRefusedOffline => Some(Scene::SettingsFresh),
            Scene::SettingsFresh => Some(Scene::SettingsSet),
            Scene::SettingsSet => Some(Scene::SettingsConfirm),
            Scene::SettingsConfirm => Some(Scene::SettingsTyping),
            Scene::SettingsTyping => Some(Scene::SettingsDogs),
            Scene::SettingsDogs => Some(Scene::SettingsNarrow),
            Scene::SettingsNarrow => Some(Scene::SettingsShort),
            Scene::SettingsShort => Some(Scene::Bleats),
            Scene::Bleats => Some(Scene::Secrets),
            Scene::Secrets => Some(Scene::SheepPane),
            Scene::SheepPane => Some(Scene::SheepPaneCpuOnly),
            Scene::SheepPaneCpuOnly => Some(Scene::SheepPaneSparklines),
            Scene::SheepPaneSparklines => Some(Scene::SheepPaneShort),
            Scene::SheepPaneShort => Some(Scene::EditPane),
            Scene::EditPane => Some(Scene::EditPaneEdited),
            Scene::EditPaneEdited => Some(Scene::EditPaneSqueezed),
            Scene::EditPaneSqueezed => Some(Scene::EditPaneNarrow),
            Scene::EditPaneNarrow => Some(Scene::CloseDialog),
            Scene::CloseDialog => Some(Scene::CloseDialogFloor),
            Scene::CloseDialogFloor => Some(Scene::CloseDialogNarrow),
            Scene::CloseDialogNarrow => Some(Scene::CloseDialogParked),
            Scene::CloseDialogParked => Some(Scene::Keymap),
            Scene::Keymap => Some(Scene::KeymapFloor),
            Scene::KeymapFloor => Some(Scene::KeymapBorderlessWide),
            Scene::KeymapBorderlessWide => Some(Scene::KeymapNarrow),
            Scene::KeymapNarrow => Some(Scene::KeymapTwoColumn),
            Scene::KeymapTwoColumn => Some(Scene::KeymapFrozen),
            Scene::KeymapFrozen => Some(Scene::KeymapReadOnly),
            Scene::KeymapReadOnly => Some(Scene::KeymapShort),
            Scene::KeymapShort => None,
        }
    }

    /// Walks [`scene_after`] from the first scene and checks the walk names
    /// exactly `Scene::ALL`, in the same order.
    ///
    /// The walk can only see variants `scene_after` was taught about, and
    /// that match cannot compile with one missing, so a scene left out of
    /// `ALL` shows up here as a length or order mismatch rather than as
    /// nothing at all.
    #[test]
    fn scene_all_lists_every_variant_the_compiler_can_see() {
        let mut walked = vec![Scene::HealthyWide];
        while let Some(next) = scene_after(*walked.last().unwrap()) {
            // Bounded, because `scene_after` is a hand-kept chain and a
            // variant wired back at an earlier one would otherwise spin here
            // forever. Nextest has no per-test timeout, so an unbounded walk
            // fails as a twenty-minute job timeout with no named test rather
            // than as an assertion.
            assert!(
                walked.len() < Scene::ALL.len(),
                "scene_after cycles: walked {} scenes without reaching the end of {}",
                walked.len(),
                Scene::ALL.len()
            );
            walked.push(next);
        }
        assert_eq!(walked.as_slice(), Scene::ALL);
    }

    /// `sgr` now draws a foreground, a background and `REVERSED`, so the
    /// title and section bands' reverse video and the selected row's
    /// painted ground both reach `frames.ansi`. This test still forbids
    /// any modifier beyond `REVERSED`: nothing in the dashboard uses one
    /// today, and a scene that started would render unstyled without this
    /// guard noticing.
    #[test]
    fn no_scene_uses_a_modifier_the_ansi_renderer_cannot_render() {
        for which in Scene::ALL {
            let buffer = scene(*which).1;
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let cell = &buffer[(buffer.area.x + x, buffer.area.y + y)];
                    assert!(
                        cell.modifier.is_empty() || cell.modifier == Modifier::REVERSED,
                        "{} has an unrendered modifier at {x},{y}: {:?}",
                        which.label(),
                        cell.modifier
                    );
                }
            }
        }
    }
}
