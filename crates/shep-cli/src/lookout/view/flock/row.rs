//! Flat-view rows: one line per sheep, per group header, and per section
//! label, and the cells each of them draws.
//!
//! A row is built span by span rather than through a table widget, so a
//! cell needing two styles, like the memory gauge's filled run and its
//! unfilled tail, can have them without the row knowing.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use super::super::super::app::{App, GroupTotals, Row, RowKey};
use super::super::super::theme::Palette;
use super::super::cell;
use super::columns::{Column, name_width};
use super::facts::FrameFacts;
use super::layout::{fit_owned, pad_ground};
use crate::output::{cfg_cell, exit_cell, human_bytes, human_duration};

/// One line for a row the table draws: a real sheep, the header above an
/// app's grouped instances, or a [`RowKey::Section`] header.
///
/// `key`'s `Sheep` ids always name a row still in the flock in practice, so
/// the blank fallback below is never drawn; it exists rather than an
/// `expect`, on the same "no honest value" rule this table applies to a
/// missing pid.
///
/// A `Sheep` row under a group header draws as a slot rather than a
/// standalone sheep ([`FrameFacts::is_grouped`]), so a header reading `web ×3` is
/// not followed by three rows each repeating `web`.
///
/// `selected` paints the row's own ground ([`Palette::ground`]) rather than
/// relying on the gutter marker alone; a section header ignores it, since
/// the cursor never lands on one.
#[must_use]
pub fn key_line(
    app: &App,
    facts: &FrameFacts<'_>,
    key: &RowKey,
    columns: &[Column],
    width: u16,
    selected: bool,
) -> Line<'static> {
    match key {
        RowKey::Sheep(id) => app.row(*id).map_or_else(
            || Line::from(Span::raw(" ".repeat(usize::from(width)))),
            |row| row_line(app, facts, row, columns, width, selected),
        ),
        RowKey::Group(name) => group_line(app, name, columns, width, selected),
        RowKey::Section(label) => section_line(label, width, app.data_palette().muted()),
        // Flat view never emits a `RowKey::Fold`: `push_fold_rows` builds
        // them and only runs under `Grouping::ByFold`, whose rows go through
        // `fold_key_line` instead. `key_line` is the flat renderer, reached
        // from one place, `view::mod`'s `Grouping::Flat` arm.
        RowKey::Fold(_) => unreachable!("flat view emits no fold header"),
    }
}

/// A [`RowKey::Section`] header: the label, then a rule filling the rest of
/// the table's width.
pub(super) fn section_line(label: &str, width: u16, style: Style) -> Line<'static> {
    let used = label.chars().count() + 1;
    let rule = "─".repeat(usize::from(width).saturating_sub(used));
    Line::from(Span::styled(format!("{label} {rule}"), style))
}

/// An app's group header row: [`App::group_totals`]'s own rollup, in the
/// same columns [`row_line`] uses for a real sheep. Mirrors
/// `output::rows::FlockRows`'s own group row so the two surfaces never
/// disagree about what an app's instances add up to.
///
/// The selected row's own ground ([`Palette::ground`]) paints these cells
/// too, on top of STATUS/`CpuSpark`/`MemCeil`; the gutter marker
/// ([`super::layout::gutter`]) is no longer the only tell.
fn group_line(
    app: &App,
    name: &str,
    columns: &[Column],
    width: u16,
    selected: bool,
) -> Line<'static> {
    let palette = app.data_palette();
    // One `group_members` call for the whole row. Every rollup below reads
    // this slice: asking `App` by name each time meant a whole-flock filter,
    // collect and sort per cell, since `group_uniform_status` sits inside the
    // column loop.
    let members = app.group_members(name);
    let totals = app.totals_for(&members);
    // `palette.status`, not `palette.reported`: a group row is always an
    // app's own instances, never a dog, so it has nothing to be silent
    // about.
    let status = App::uniform_status_for(&members);
    let status_style = status.map_or(Style::default(), |status| palette.status(status));
    let status_text = App::status_text_for(&members);
    let name_width = self::name_width(width, columns);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len() * 2 + 1);
    let mut used: u16 = 0;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", ground));
            used += 2;
        }
        let cell_width = if *column == Column::Name {
            name_width
        } else {
            column.width()
        };
        let text = fit_owned(
            group_cell(name, *column, &totals, &status_text, &members),
            cell_width,
        );
        let style = cell_style(palette, *column, status_style, status, None);
        // A group has no single history or ceiling ([`group_cell`]), so its
        // `MemCeil` text is always empty and there is nothing to fill.
        let tail_style = mem_ceil_tail_style(palette, status, None, style);
        push_row_cell(
            &mut spans,
            *column,
            text,
            style.patch(ground),
            tail_style.patch(ground),
            0,
        );
        used += cell_width;
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// One cell of an app's group header row.
///
/// ID, PID, EXIT and CFG are blank, not `-`: there is no single value for a
/// group row to have "no honest value" about, since a load can park a
/// different set of fields on each slot. `CpuSpark` and `MemCeil` join them
/// for the same reason: a group has no single history and no single
/// ceiling. FOLD and SMIT read the first member's, since both are per-app
/// facts every instance shares.
fn group_cell(
    name: &str,
    column: Column,
    totals: &GroupTotals,
    status_text: &str,
    members: &[&Row],
) -> String {
    match column {
        Column::Id
        | Column::Pid
        | Column::Exit
        | Column::Cfg
        | Column::CpuSpark
        | Column::MemCeil => String::new(),
        Column::Name => format!("{name} \u{d7}{}", totals.count),
        Column::Status => status_text.to_string(),
        Column::Restarts => totals.restarts.to_string(),
        Column::Cpu => totals
            .cpu
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        Column::Mem => totals.memory.map_or_else(|| "-".to_string(), human_bytes),
        Column::Uptime => totals
            .uptime_ms
            .map_or_else(|| "-".to_string(), human_duration),
        Column::Fold => members
            .first()
            .and_then(|row| row.info.fold.clone())
            .unwrap_or_else(|| "-".to_string()),
        Column::Smit => members
            .first()
            .and_then(|row| row.info.smit.clone())
            .unwrap_or_else(|| "-".to_string()),
    }
}

/// One sheep's line. STATUS, `CpuSpark` and `MemCeil` are the cells that
/// carry colour.
///
/// `selected` paints the row's own ground ([`Palette::ground`]) over every
/// cell, padded to `width` by [`pad_ground`] so the paint reaches the last
/// column rather than stopping where the text does; the gutter marker
/// ([`super::layout::gutter`]) is no longer the only tell.
///
/// `facts` supplies the two things this row cannot answer for itself: whether
/// a group header sits above it, which is the only thing that changes NAME,
/// FOLD and SMIT (see [`cell()`]), and the CPU sparkline's flock-wide ceiling.
#[must_use]
pub fn row_line(
    app: &App,
    facts: &FrameFacts<'_>,
    row: &Row,
    columns: &[Column],
    width: u16,
    selected: bool,
) -> Line<'static> {
    let palette = app.data_palette();
    let grouped = facts.is_grouped(&row.info.name);
    let name = name_width(width, columns);
    let status_style = palette.reported(row.reported());
    let status = Some(row.info.status);
    let mem_ceil_ratio = mem_ceil_ratio(&row.info);
    let mem_ceil_fill = self::mem_ceil_fill(&row.info);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len() * 2 + 1);
    let mut used: u16 = 0;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", ground));
            used += 2;
        }
        let cell_width = if *column == Column::Name {
            name
        } else {
            column.width()
        };
        let text = fit_owned(
            cell(app, row, *column, grouped, facts.cpu_ceiling),
            cell_width,
        );
        let style = cell_style(palette, *column, status_style, status, mem_ceil_ratio);
        let tail_style = mem_ceil_tail_style(palette, status, mem_ceil_ratio, style);
        push_row_cell(
            &mut spans,
            *column,
            text,
            style.patch(ground),
            tail_style.patch(ground),
            mem_ceil_fill,
        );
        used += cell_width;
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// One cell's text.
///
/// `-` rather than an empty cell for every unknown: an empty cell in a
/// padded table is indistinguishable from a rendering bug, and `0.0%` would
/// claim a measurement the shepherd never made.
///
/// `grouped` changes three cells, matching `output::rows::slot_row`. NAME
/// becomes `↳ :2`, teaching the `web:2` selector by sitting under the name
/// the header already printed; FOLD and SMIT go blank rather than `-`,
/// since the group row above carries both.
fn cell(app: &App, row: &Row, column: Column, grouped: bool, cpu_ceiling: f32) -> String {
    let info = &row.info;
    match column {
        Column::Id => info.id.to_string(),
        Column::Name if grouped => info
            .instance
            .map_or_else(String::new, |slot| format!(" \u{21b3} :{slot}")),
        Column::Name => info.name.clone(),
        // `Row::reported`, not `info.status.to_string()`: a dog that has
        // never handshook must not read `online` here any more than it does
        // in `shep flock`'s own table.
        Column::Status => row.reported().word(),
        Column::Pid => info
            .pid
            .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
        Column::Restarts => info.restarts.to_string(),
        // `crate::output::exit_cell`, not a second implementation of the
        // code/signal split.
        Column::Exit => exit_cell(info.pid, info.last_exit),
        // `crate::output::cfg_cell`, not a second implementation of the
        // pending-over-overridden precedence.
        Column::Cfg => cfg_cell(info.pending.as_deref(), info.overridden.as_deref()),
        Column::CpuSpark => cpu_spark_cell(app, info, cpu_ceiling),
        // `App::cpu_now`, the sparkline's own newest cell, not
        // `info.cpu_percent`: the shepherd's running mean, differently
        // windowed, would disagree with the shape beside it.
        Column::Cpu => app
            .cpu_now(info.id)
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        Column::MemCeil => mem_ceil_cell(info),
        Column::Mem => info
            .memory_bytes
            .map_or_else(|| "-".to_string(), human_bytes),
        // The live value, not the snapshot's: `App::uptime_ms` advances a
        // running sheep between polls and stops once the link is lost.
        Column::Uptime => app
            .uptime_ms(info.id)
            .map_or_else(|| "-".to_string(), human_duration),
        Column::Fold | Column::Smit if grouped => String::new(),
        Column::Fold => info.fold.clone().unwrap_or_else(|| "-".to_string()),
        Column::Smit => info.smit.clone().unwrap_or_else(|| "-".to_string()),
    }
}

/// The `CPU 20s` cell: [`App::cpu_history`], rendered into ten cells by
/// [`cell::sparkline`].
///
/// `cpu_ceiling` arrives from [`FrameFacts`] rather than from
/// [`App::cpu_ceiling`]: it is one number for the whole frame, and reading it
/// here folded a max over every sheep's whole history once per row.
fn cpu_spark_cell(app: &App, info: &ProcessInfo, cpu_ceiling: f32) -> String {
    cell::sparkline(app.cpu_history(info.id), 10, cpu_ceiling)
}

/// The `MEM/CEIL` cell: [`ProcessInfo::memory_bytes`] against
/// [`ProcessInfo::max_memory`], rendered into ten cells by [`cell::gauge`].
/// A missing reading counts as `0`, matching [`cell::gauge`]'s own
/// no-ceiling case: an idle-looking bar rather than a guessed denominator.
fn mem_ceil_cell(info: &ProcessInfo) -> String {
    cell::gauge(info.memory_bytes.unwrap_or(0), info.max_memory, 10)
}

/// Where [`mem_ceil_cell`]'s ten characters split into filled and tail, so
/// [`push_row_cell`] can style the two runs separately without re-deriving
/// the fill from `info` a second time.
fn mem_ceil_fill(info: &ProcessInfo) -> usize {
    cell::gauge_fill(info.memory_bytes.unwrap_or(0), info.max_memory, 10)
}

/// `memory_bytes` over `max_memory`, or `None` when either is missing or the
/// ceiling is zero. Feeds [`cell_style`]'s butter threshold; the bar itself
/// is drawn by [`mem_ceil_cell`], which never divides.
fn mem_ceil_ratio(info: &ProcessInfo) -> Option<f64> {
    let value = info.memory_bytes?;
    let ceiling = info.max_memory?;
    if ceiling == 0 {
        return None;
    }
    Some(value as f64 / ceiling as f64)
}

/// Whether `MemCeil` has anything to measure: a live reading against a real
/// ceiling, on a sheep that is actually running. A stopped sheep's last
/// known reading and a sheep with no ceiling at all share the same "nothing
/// to show" rendering (decision 7), so both read `false` here.
fn mem_ceil_measuring(status: Option<ProcStatus>, ratio: Option<f64>) -> bool {
    status == Some(ProcStatus::Online) && ratio.is_some()
}

/// The per-column style [`row_line`] and [`group_line`] both apply, so the
/// STATUS rule they already shared does not get a second, drifting copy now
/// that `CpuSpark` and `MemCeil` need one too.
///
/// `status_style` is resolved by the caller, since a sheep and a group
/// header read status through different paths. `status` and `mem_ceil_ratio`
/// are `None` for a group row: it has no single status or ceiling to be
/// near. This is the style [`push_row_cell`] gives `MemCeil`'s *filled* run;
/// see [`mem_ceil_tail_style`] for its unfilled tail.
fn cell_style(
    palette: Palette,
    column: Column,
    status_style: Style,
    status: Option<ProcStatus>,
    mem_ceil_ratio: Option<f64>,
) -> Style {
    match column {
        Column::Status => status_style,
        // The role a healthy sheep's own STATUS cell wears.
        Column::CpuSpark => palette.status(ProcStatus::Online),
        Column::MemCeil if !mem_ceil_measuring(status, mem_ceil_ratio) => palette.muted(),
        Column::MemCeil => {
            if mem_ceil_ratio.is_some_and(|ratio| ratio >= 0.9) {
                palette.attention()
            } else {
                palette.sky()
            }
        }
        _ => Style::default(),
    }
}

/// `MemCeil`'s unfilled tail: `gauge_rest` against a real ceiling on a
/// running sheep, the same muted role as `fill_style` everywhere else so the
/// bar reads as one flat colour rather than two competing ones.
fn mem_ceil_tail_style(
    palette: Palette,
    status: Option<ProcStatus>,
    mem_ceil_ratio: Option<f64>,
    fill_style: Style,
) -> Style {
    if mem_ceil_measuring(status, mem_ceil_ratio) {
        palette.gauge_rest()
    } else {
        fill_style
    }
}

/// Pushes one column's text as one span, except `MemCeil`, which splits at
/// `fill` into a filled run styled `style` and an unfilled tail styled
/// `tail_style` (decision 7's rule that the tail must not compete with the
/// fill). `fill` is clamped to the text's own length, so a group row's
/// always-empty `MemCeil` cell and a stopped sheep's cell both split
/// harmlessly.
fn push_row_cell(
    spans: &mut Vec<Span<'static>>,
    column: Column,
    text: String,
    style: Style,
    tail_style: Style,
    fill: usize,
) {
    if column == Column::MemCeil {
        let fill = fill.min(text.chars().count());
        let mut chars = text.chars();
        let filled: String = chars.by_ref().take(fill).collect();
        let rest: String = chars.collect();
        spans.push(Span::styled(filled, style));
        spans.push(Span::styled(rest, tail_style));
    } else {
        spans.push(Span::styled(text, style));
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fixtures;
    use super::super::columns::ALL;
    use super::super::layout::fit;
    use super::*;

    /// Not a "-": the column is a bar, and an all-tail bar reads as
    /// "no ceiling set" without a second rendering to learn.
    #[test]
    fn a_running_sheep_with_no_ceiling_draws_an_empty_gauge() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let info = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .memory_bytes(Some(48 * 1024 * 1024))
            .build();
        assert_eq!(mem_ceil_cell(&info), "░░░░░░░░░░");
    }

    #[test]
    fn a_sheep_at_its_ceiling_fills_the_gauge() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let info = ProcessInfo::builder(1, "hungry", ProcStatus::Online)
            .memory_bytes(Some(52 * 1024 * 1024))
            .max_memory(Some(52 * 1024 * 1024))
            .build();
        assert_eq!(mem_ceil_cell(&info), "██████████");
    }

    /// Decision 7: the unfilled tail must not compete with the fill, so the
    /// two runs need distinct spans and distinct roles, not one style over
    /// the whole ten-cell text.
    #[test]
    fn the_gauges_unfilled_tail_carries_gauge_rest_not_the_fill_role() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let palette = fixtures::coloured();
        assert_ne!(
            palette.sky().fg,
            palette.gauge_rest().fg,
            "the two roles must actually differ for this test to mean anything"
        );
        let info = ProcessInfo::builder(1, "hungry", ProcStatus::Online)
            .memory_bytes(Some(26 * 1024 * 1024))
            .max_memory(Some(52 * 1024 * 1024))
            .build();
        let app = fixtures::app_with(vec![info], palette);
        let row = app.row(1).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let fill: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| span.style.fg == palette.sky().fg && !span.content.is_empty())
            .map(|span| span.content.as_ref())
            .collect();
        let tail: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| span.style.fg == palette.gauge_rest().fg && !span.content.is_empty())
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(fill, vec!["█████"], "got fill spans {fill:?}");
        assert_eq!(tail, vec!["░░░░░"], "got tail spans {tail:?}");
    }

    /// Decision 7: a sheep with nothing to measure against reads as muted,
    /// not as though `sky` had something to report.
    #[test]
    fn a_running_sheep_with_no_ceiling_draws_muted_not_sky() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let palette = fixtures::coloured();
        let info = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .memory_bytes(Some(48 * 1024 * 1024))
            .build();
        let app = fixtures::app_with(vec![info], palette);
        let row = app.row(1).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let mem_ceil_text: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| span.content.as_ref() == "░░░░░░░░░░")
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(mem_ceil_text, vec!["░░░░░░░░░░"]);
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.as_ref() == "░░░░░░░░░░"
                    && span.style.fg == palette.muted().fg),
            "expected the all-tail bar in muted, got {:?}",
            line.spans
                .iter()
                .map(|s| (s.content.as_ref(), s.style.fg))
                .collect::<Vec<_>>()
        );
        assert!(
            !line
                .spans
                .iter()
                .any(|span| span.content.as_ref() == "░░░░░░░░░░"
                    && span.style.fg == palette.sky().fg),
            "must not read sky, which claims a real measurement"
        );
    }

    /// Decision 7's other muted case: a sheep that is not running, even one
    /// with a ceiling, since a stopped process has nothing live to gauge.
    #[test]
    fn a_stopped_sheeps_gauge_draws_muted() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let palette = fixtures::coloured();
        let info = ProcessInfo::builder(1, "stopped", ProcStatus::Stopped)
            .memory_bytes(Some(26 * 1024 * 1024))
            .max_memory(Some(52 * 1024 * 1024))
            .build();
        let app = fixtures::app_with(vec![info], palette);
        let row = app.row(1).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        assert!(
            !line
                .spans
                .iter()
                .any(|span| span.style.fg == palette.sky().fg),
            "a stopped sheep has nothing live to gauge, so no span reads sky"
        );
    }

    /// `cfg(unix)`: the fixture carries a signalled exit, which
    /// `output::rows::signal_label` resolves against the running platform's
    /// own signal table. A Windows `ExitOutcome` never carries a signal at
    /// all, so this arm is only ever reached by a synthetic fixture.
    #[cfg(unix)]
    #[test]
    fn the_exit_cell_reuses_the_same_rendering_flock_rows_uses() {
        use shep_core::protocol::{ExitInfo, ProcessInfo};
        use shep_core::status::ProcStatus;

        let crashed = ProcessInfo::builder(1, "crashed", ProcStatus::Errored)
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .build();
        let killed = ProcessInfo::builder(2, "killed", ProcStatus::Stopped)
            .last_exit(Some(ExitInfo {
                code: None,
                signal: Some(9),
            }))
            .build();
        let running = ProcessInfo::builder(3, "running", ProcStatus::Online)
            .pid(Some(4_242))
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .build();

        let app = fixtures::app_with(vec![crashed, killed, running], fixtures::plain());
        let rows = app.rows();
        let cell_for = |id: u32| {
            let row = rows.iter().find(|row| row.info.id == id).unwrap();
            cell(&app, row, Column::Exit, false, app.cpu_ceiling())
        };

        assert_eq!(cell_for(1), "1");
        assert_eq!(cell_for(2), "SIGKILL");
        assert_eq!(
            cell_for(3),
            "-",
            "a running sheep has nothing for EXIT to say"
        );
    }

    /// `web x3` in NAME, memory summed across instances, ID/PID/EXIT blank,
    /// and UPTIME the minimum rather than any one instance's own reading.
    /// Asserted on the rendered [`Line`], not on `App::group_totals`
    /// directly, so a change in either the arithmetic or the rendering has
    /// to redden this.
    #[test]
    fn a_group_rows_cells_show_the_apps_rollup() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .instance(Some(0))
                    .memory_bytes(Some(100 << 20))
                    .uptime_ms(120_000)
                    .build(),
                ProcessInfo::builder(2, "web", ProcStatus::Online)
                    .instance(Some(1))
                    .memory_bytes(Some(150 << 20))
                    .uptime_ms(30_000)
                    .build(),
                ProcessInfo::builder(3, "web", ProcStatus::Online)
                    .instance(Some(2))
                    .memory_bytes(Some(50 << 20))
                    .uptime_ms(600_000)
                    .build(),
            ],
            fixtures::plain(),
        );

        let line = key_line(
            &app,
            &FrameFacts::new(&app),
            &RowKey::Group("web".to_string()),
            ALL,
            200,
            false,
        );
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        // The exact row, column by column: a substring check on a run of
        // blank cells can pass by accident on neighbouring padding.
        let name = name_width(200, ALL);
        let expected = [
            fit("", Column::Id.width()),           // ID: blank, no single id
            fit("web \u{d7}3", name),              // NAME: app x instance count
            fit("online", Column::Status.width()), // STATUS: every instance agrees
            fit("", Column::Pid.width()),          // PID: blank, no single pid
            fit("0", Column::Restarts.width()),    // RESTARTS: summed, all zero
            fit("", Column::Exit.width()),         // EXIT: blank, no single exit
            fit("", Column::Cfg.width()),          // CFG: blank, per-instance fact
            fit("", Column::CpuSpark.width()),     // CPU 20s: blank, no single history
            fit("-", Column::Cpu.width()),         // CPU: no reading on any instance
            fit("", Column::MemCeil.width()),      // MEM/CEIL: blank, no single ceiling
            // 100 + 150 + 50 = 300 MiB, summed rather than averaged.
            fit("300.0M", Column::Mem.width()),
            // The MINIMUM across the three instances (30s), not the first
            // one's (120s) or the last one's (600s).
            fit("30s", Column::Uptime.width()),
            fit("-", Column::Fold.width()),
            fit("-", Column::Smit.width()),
        ]
        .join("  ");

        // Trailing pad compared separately: since NAME gained a ceiling
        // ([`NAME_MAX`]), the columns no longer fill a wide terminal and the
        // row is padded out to it. Both facts are worth pinning, but the
        // cells are the ones this test is about.
        assert_eq!(rendered.trim_end(), expected.trim_end(), "got {rendered:?}");
        assert_eq!(
            crate::output::width::visible_width(&rendered),
            200,
            "the row is padded to the table's width, so its ground reaches the edge"
        );
    }

    /// A slot row drawn like a standalone sheep repeats FOLD and SMIT down
    /// every row and shows no slot number, unlike `shep flock`'s own `↳ :1`.
    ///
    /// Asserted on the rendered line rather than on `cell`, since the
    /// defect was in which caller `key_line` picked.
    #[test]
    fn a_slot_row_under_a_group_header_renders_as_a_slot() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let member = |id: u32, slot: u32| {
            ProcessInfo::builder(id, "web", ProcStatus::Online)
                .instance(Some(slot))
                .pid(Some(4_000 + id))
                .fold(Some("edge".to_string()))
                .smit(Some("web".to_string()))
                .uptime_ms(30_000)
                .build()
        };
        let app = fixtures::app_with(vec![member(1, 0), member(2, 1)], fixtures::plain());

        let line = key_line(
            &app,
            &FrameFacts::new(&app),
            &RowKey::Sheep(2),
            ALL,
            200,
            false,
        );
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        let name = name_width(200, ALL);
        let expected = [
            fit("2", Column::Id.width()),
            // NAME: the slot alone, indented under the name the header
            // above already printed.
            fit(" \u{21b3} :1", name),
            fit("online", Column::Status.width()),
            fit("4002", Column::Pid.width()),
            fit("0", Column::Restarts.width()),
            fit("-", Column::Exit.width()),
            fit("-", Column::Cfg.width()),
            // CPU 20s: one sample recorded by `app_with`'s own snapshot,
            // 0.0 since neither member reports a reading.
            fit("         \u{2581}", Column::CpuSpark.width()),
            fit("-", Column::Cpu.width()),
            // MEM/CEIL: an all-tail bar, no memory reading and no ceiling.
            fit(
                "\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}",
                Column::MemCeil.width(),
            ),
            fit("-", Column::Mem.width()),
            fit("30s", Column::Uptime.width()),
            // FOLD and SMIT blank, not `-`: the group row carries both.
            fit("", Column::Fold.width()),
            fit("", Column::Smit.width()),
        ]
        .join("  ");

        // Same as the group-row test: NAME has a ceiling now, so a wide

        // terminal leaves the row padded past its last cell.

        assert_eq!(rendered.trim_end(), expected.trim_end(), "got {rendered:?}");
    }

    /// The guard on the test above: an app with one instance never gets a
    /// header, so `is_grouped` keeps it drawing as it always did.
    #[test]
    fn an_ungrouped_sheep_still_shows_its_own_name_and_fold() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let app = fixtures::app_with(
            vec![
                ProcessInfo::builder(7, "solo", ProcStatus::Online)
                    .instance(Some(0))
                    .fold(Some("edge".to_string()))
                    .build(),
            ],
            fixtures::plain(),
        );

        let line = key_line(
            &app,
            &FrameFacts::new(&app),
            &RowKey::Sheep(7),
            ALL,
            200,
            false,
        );
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(rendered.contains("solo"), "got {rendered:?}");
        assert!(rendered.contains("edge"), "got {rendered:?}");
        assert!(!rendered.contains('\u{21b3}'), "got {rendered:?}");
    }

    /// The process is alive, so nothing but `handshook` can catch this.
    #[test]
    fn a_silent_dog_reads_silent_not_online() {
        use shep_core::protocol::{DogSource, ProcessInfo};
        use shep_core::status::ProcStatus;

        let dog = ProcessInfo::builder(9, "log-rotate", ProcStatus::Online)
            .pid(Some(4_242))
            .dog(Some(DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            }))
            .handshook(Some(false))
            .build();
        let app = fixtures::app_with(vec![dog], fixtures::plain());
        let row = app.row(9).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.contains("silent"),
            "expected silent, got {rendered:?}"
        );
        assert!(
            !rendered.contains("online"),
            "must not say online: {rendered:?}"
        );
    }

    /// The guard on [`a_silent_dog_reads_silent_not_online`] above.
    #[test]
    fn a_dog_that_has_handshook_still_reads_online() {
        use shep_core::protocol::{DogSource, ProcessInfo};
        use shep_core::status::ProcStatus;

        let dog = ProcessInfo::builder(9, "log-rotate", ProcStatus::Online)
            .pid(Some(4_242))
            .dog(Some(DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            }))
            .handshook(Some(true))
            .build();
        let app = fixtures::app_with(vec![dog], fixtures::plain());
        let row = app.row(9).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("online"), "got {rendered:?}");
        assert!(!rendered.contains("silent"), "got {rendered:?}");
    }

    /// A sheep's `handshook` is always `None`; it must never get caught by
    /// the same silent rule a dog does.
    #[test]
    fn a_sheep_still_reads_online_and_has_no_handshake() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let sheep = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .pid(Some(4_000))
            .build();
        assert_eq!(sheep.handshook, None, "a sheep is never sent one");
        let app = fixtures::app_with(vec![sheep], fixtures::plain());
        let row = app.row(1).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("online"), "got {rendered:?}");
        assert!(!rendered.contains("silent"), "got {rendered:?}");
    }

    /// The daemon never sends a sheep `handshook: Some(false)`; a sheep has
    /// no version relationship with the shepherd to fail. Exercises the
    /// `dog.is_none()` guard in `Row::reported` with an input no other test
    /// drives.
    #[test]
    fn a_sheep_never_reads_as_silent() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut impossible = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .pid(Some(4_000))
            .build();
        impossible.handshook = Some(false);
        let app = fixtures::app_with(vec![impossible], fixtures::plain());
        let row = app.row(1).unwrap();

        let line = row_line(&app, &FrameFacts::new(&app), row, ALL, 200, false);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.contains("online"),
            "the sheep table has no dogs in it, and no silence rule either: {rendered:?}"
        );
        assert!(!rendered.contains("silent"), "got {rendered:?}");
    }
}
