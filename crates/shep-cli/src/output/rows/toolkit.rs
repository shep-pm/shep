//! The paint and cell rules every `ProcessInfo`-shaped row renders through:
//! group rollups, the STATUS/EXIT/MEM/CPU/CFG cell text, and the column-name
//! keyed [`Paint`] decision each table's `rows_for` asks for.
//!
//! Named `toolkit` rather than `paint`, which is already the crate-level
//! ANSI/colour module at `output::paint`.

use std::collections::BTreeMap;

use shep_core::protocol::{DogSource, ExitInfo, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::style::Presentation;
use crate::vocabulary::{Reported, Role};

/// Splits a listing into runs of one app's adjacent rows, keyed on NAME.
///
/// Sound only because `sort_flock` orders by (name, instance, id). The
/// `slotted` rule stays with each caller.
pub(crate) fn name_groups(items: &[ProcessInfo]) -> impl Iterator<Item = &[ProcessInfo]> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= items.len() {
            return None;
        }
        let name = items[at].name.as_str();
        let end = items[at..]
            .iter()
            .position(|p| p.name != name)
            .map_or(items.len(), |offset| at + offset);
        let group = &items[at..end];
        at = end;
        Some(group)
    })
}

/// An app's summed CPU/MEM/RESTARTS and its earliest UPTIME, shared by
/// [`group_row`] and [`group_paint`].
pub(crate) struct GroupTotals {
    /// Every slot's restarts, added up.
    pub(crate) restarts: u32,
    /// Every slot's CPU summed, `None` only when no slot reported one.
    pub(crate) cpu: Option<f32>,
    /// Every slot's memory summed, `None` under `cpu`'s rule.
    pub(crate) memory: Option<u64>,
    /// The shortest uptime across slots: time since the app was last
    /// disturbed.
    pub(crate) uptime_ms: u64,
}

pub(crate) fn group_totals(group: &[ProcessInfo]) -> GroupTotals {
    GroupTotals {
        restarts: group.iter().map(|p| p.restarts).sum(),
        cpu: group
            .iter()
            .filter_map(|p| p.cpu_percent)
            .fold(None, |acc, c| Some(acc.unwrap_or(0.0) + c)),
        memory: group
            .iter()
            .filter_map(|p| p.memory_bytes)
            .fold(None, |acc, m| Some(acc.unwrap_or(0) + m)),
        uptime_ms: group.iter().map(|p| p.uptime_ms).min().unwrap_or(0),
    }
}

/// The header above an app's instances: what the app costs, how many there
/// are, and the per-app facts FOLD and SMIT.
///
/// STATUS stays plain text here so [`group_paint`] can dress it through the
/// same [`Paint::Status`] path a sheep's cell takes.
pub(crate) fn group_row(group: &[ProcessInfo], totals: &GroupTotals) -> Vec<String> {
    let first = &group[0];
    vec![
        String::new(),
        format!("{} \u{d7}{}", first.name, group.len()),
        group_status(group),
        String::new(),
        totals.restarts.to_string(),
        String::new(),
        // Blank, not `-`, as ID/PID/EXIT are above: pending and overridden
        // are per-instance facts with no group-level answer.
        String::new(),
        totals
            .cpu
            .map_or_else(|| "-".to_string(), |c| format!("{c:.1}%")),
        totals
            .memory
            .map_or_else(|| "-".to_string(), crate::output::human_bytes),
        crate::output::human_duration(totals.uptime_ms),
        first.fold.clone().unwrap_or_else(|| "-".to_string()),
        first.smit.clone().unwrap_or_else(|| "-".to_owned()),
    ]
}

/// One instance under its group header. NAME carries only the `\u{21b3} :2`
/// marker, which teaches the `web:2` selector.
///
/// FOLD and SMIT are blank, not `-`: the group row above carries both.
pub(crate) fn slot_row(p: &ProcessInfo) -> Vec<String> {
    let slot = p
        .instance
        .map_or_else(String::new, |s| format!(" \u{21b3} :{s}"));
    vec![
        p.id.to_string(),
        slot,
        // Never a dog, but through `Reported` anyway so this cell has one
        // spelling.
        reported(p).word(),
        p.pid.map_or_else(|| "-".to_string(), |pid| pid.to_string()),
        p.restarts.to_string(),
        exit_cell(p.pid, p.last_exit),
        // A real per-instance fact, unlike FOLD/SMIT below: a load can park
        // a different set of fields on each slot.
        cfg_cell(p.pending.as_deref(), p.overridden.as_deref()),
        p.cpu_percent
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        p.memory_bytes
            .map_or_else(|| "-".to_string(), crate::output::human_bytes),
        crate::output::human_duration(p.uptime_ms),
        String::new(),
        String::new(),
    ]
}

/// One line per process: an app with one instance, a mixed group missing a
/// slot, or a flat style.
///
/// `slotted` earns the `web:0` suffix: more than one instance, every one
/// reporting its slot. Anything else leaves NAME alone.
pub(crate) fn plain_row(p: &ProcessInfo, slotted: bool) -> Vec<String> {
    let name = match (slotted, p.instance) {
        (true, Some(slot)) => format!("{}:{slot}", p.name),
        _ => p.name.clone(),
    };
    vec![
        p.id.to_string(),
        name,
        // `Reported`, not `p.status`: the plain path must say what the boxed
        // one does.
        reported(p).word(),
        p.pid.map_or_else(|| "-".to_string(), |pid| pid.to_string()),
        p.restarts.to_string(),
        exit_cell(p.pid, p.last_exit),
        cfg_cell(p.pending.as_deref(), p.overridden.as_deref()),
        p.cpu_percent
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        p.memory_bytes
            .map_or_else(|| "-".to_string(), crate::output::human_bytes),
        crate::output::human_duration(p.uptime_ms),
        p.fold.clone().unwrap_or_else(|| "-".to_string()),
        p.smit.clone().unwrap_or_else(|| "-".to_owned()),
    ]
}

/// What one row's STATUS column reports: the lifecycle status, unless this
/// row is a dog whose process is up and which has never answered this
/// shepherd.
///
/// Keyed on `dog` as well as `handshook`, so the silence rule holds here
/// rather than resting on the daemon always sending a sheep `None`.
pub(crate) fn reported(p: &ProcessInfo) -> Reported {
    if p.dog.is_none() {
        return Reported::Live(p.status);
    }
    Reported::of(p.status, p.handshook)
}

/// The note one row owes a reader beyond its STATUS cell, or `None` when the
/// cell says everything.
///
/// Goes through [`reported`], so one guard decides both the word and the
/// note explaining it.
pub(crate) fn silence_note(p: &ProcessInfo) -> Option<String> {
    crate::vocabulary::silence_note(&p.name, reported(p), p.dog_stale)
}

/// The group's status: the shared word when every instance agrees, else a
/// count per state. Plain text either way; see [`group_row`].
pub(crate) fn group_status(group: &[ProcessInfo]) -> String {
    let first = group[0].status;
    if group.iter().all(|p| p.status == first) {
        return first.to_string();
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in group {
        *counts.entry(p.status.to_string()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(status, n)| format!("{n} {status}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The treatment a group's header row wears: RESTARTS, CPU and MEM read
/// [`GroupTotals`], and STATUS colours only when every slot agrees.
///
/// ID is left plain, unlike in [`process_info_paint`]: a group row's ID cell
/// is empty.
pub(crate) fn group_paint(header: &str, group: &[ProcessInfo], totals: &GroupTotals) -> Paint {
    match header {
        "FOLD" => Paint::Role(Role::Ink3),
        // No `Reported::of`: a dog is never stocked to several instances,
        // so no group this branch can see has a handshake to report.
        "STATUS" => {
            let first = group[0].status;
            if group.iter().all(|p| p.status == first) {
                Paint::Status(Reported::Live(first))
            } else {
                Paint::Default
            }
        }
        "RESTARTS" => Paint::Role(restarts_role(totals.restarts)),
        "CPU" => Paint::Role(cpu_role(totals.cpu)),
        "MEM" => Paint::Role(mem_role(totals.memory)),
        _ => Paint::Default,
    }
}

/// One STATUS cell: the face, the word and the role, for every table with a
/// STATUS column. `vocabulary.rs` owns the faces and the roles.
///
/// `presentation.level.sheep()` decides whether a face appears,
/// `status_word` whether the word rides beside it. The whole cell takes one
/// [`crate::output::paint::style_for`] span, so
/// [`crate::output::width::visible_width`] has one boundary to discount.
fn status_cell(reported: Reported, presentation: Presentation, status_word: bool) -> String {
    let word = reported.word();
    let mut text = if presentation.level.sheep() {
        let face = reported.face();
        if status_word {
            format!("{face} {word}")
        } else {
            face.to_string()
        }
    } else {
        word
    };
    colour_cell(&mut text, reported.role(), presentation);
    text
}

/// What one cell should become, decided from its column's name.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Paint {
    /// Nothing of this cell's own. The `-` placeholder rule still applies.
    Default,
    /// Wrap the cell in this role's span.
    Role(Role),
    /// Replace the cell with [`status_cell`]. The only variant that changes
    /// content rather than only colour, so STATUS cannot be a role.
    Status(Reported),
}

/// Paints one table's cells, asking `paint_of` for each by column name.
///
/// The closure gets `(header, cell, index)` and no row: `index` addresses the
/// payload, never a sibling cell. `zip` stops at the shorter of row and
/// `headers`.
pub(crate) fn paint<F>(
    mut rows: Vec<Vec<String>>,
    headers: &[&'static str],
    presentation: Presentation,
    status_word: bool,
    paint_of: F,
) -> Vec<Vec<String>>
where
    F: Fn(&str, &str, usize) -> Paint,
{
    for (index, row) in rows.iter_mut().enumerate() {
        for (cell, header) in row.iter_mut().zip(headers) {
            match paint_of(header, cell, index) {
                Paint::Status(reported) => {
                    *cell = status_cell(reported, presentation, status_word);
                }
                Paint::Role(role) => colour_cell(cell, role, presentation),
                Paint::Default => mute_a_dash(cell, presentation),
            }
        }
    }
    rows
}

/// The treatment every column read off a [`ProcessInfo`] wears, shared by
/// `FlockRows`, `DogRows` and `FlushedRows`.
///
/// A column absent from this match wears [`Paint::Default`]: NAME, UPTIME and
/// the two path columns have no state and no threshold.
pub(crate) fn process_info_paint(header: &str, p: &ProcessInfo) -> Paint {
    match header {
        // Chrome: stable labels, so they must not draw the eye.
        "ID" | "FOLD" => Paint::Role(Role::Ink3),
        // The one column reading two fields: `handshook` overrides `status`
        // for a dog that has never answered this shepherd.
        "STATUS" => Paint::Status(reported(p)),
        "RESTARTS" => Paint::Role(restarts_role(p.restarts)),
        "EXIT" => Paint::Role(exit_role(p.pid, p.last_exit)),
        "CPU" => Paint::Role(cpu_role(p.cpu_percent)),
        "MEM" => Paint::Role(mem_role(p.memory_bytes)),
        "SOURCE" => p
            .dog
            .as_ref()
            .map_or(Paint::Default, |source| Paint::Role(source_role(source))),
        // PID and SMIT reach the dash rule: a real value is plain, an
        // absent one is muted.
        _ => Paint::Default,
    }
}

/// [`Role`] for a SOURCE cell: the column answers "shep's own code, or
/// something else".
///
/// `built-in` is muted. Anything else, an adopted third-party binary or a
/// `DogSource` this client predates, takes `Role::Butter`: worth a glance,
/// never a fault.
pub(crate) fn source_role(source: &DogSource) -> Role {
    match source {
        DogSource::BuiltIn => Role::Ink3,
        _ => Role::Butter,
    }
}

/// The treatment the four dog-action rows wear over their shared columns
/// `NAME SOURCE SHEPHERD STATUS`.
///
/// Keyed off the rendered cell rather than the struct, unlike
/// [`process_info_paint`]: the four carry `source` and `status` as different
/// types. STATUS is coloured only when it names a status, since the field can
/// also hold a sentence saying why no shepherd answered.
pub(crate) fn dog_action_paint(header: &str, cell: &str) -> Paint {
    match header {
        "SOURCE" => match cell {
            "built-in" => Paint::Role(Role::Ink3),
            "-" => Paint::Default,
            _ => Paint::Role(Role::Butter),
        },
        // `Reported::Live`: these rows carry no `handshook` field.
        "STATUS" => status_named_by(cell).map_or(Paint::Default, |status| {
            Paint::Status(Reported::Live(status))
        }),
        _ => Paint::Default,
    }
}

/// [`Role`] for one OUTCOME cell, over the eleven kinds the three per-sheep
/// reply tables between them produce.
///
/// `Meadow` worked; `Ink3` has nothing to report, `skipped` being a reload
/// drainee and `not_running` a sheep with no live process; `Butter` is a gap
/// the operator can close; `Bark` failed. An unrecognised kind takes
/// `Butter`: this client is older than the daemon.
pub(crate) fn outcome_role(kind: &str) -> Role {
    match kind {
        "replied" | "delivered" | "sent" => Role::Meadow,
        "skipped" | "not_running" => Role::Ink3,
        "timed_out" | "failed" | "not_written" => Role::Bark,
        _ => Role::Butter,
    }
}

/// The treatment the three per-sheep reply tables wear over their shared
/// columns `ID NAME OUTCOME DETAIL`.
///
/// DETAIL is left plain: unbounded free text, present only when OUTCOME has
/// already said what happened.
pub(crate) fn reply_paint(header: &str, cell: &str) -> Paint {
    match header {
        "ID" => Paint::Role(Role::Ink3),
        "OUTCOME" => Paint::Role(outcome_role(cell)),
        _ => Paint::Default,
    }
}

/// The [`ProcStatus`] a free-text STATUS cell is naming, if it is naming one:
/// the dog-action rows carry `status` as a `String` that can also hold a
/// sentence.
///
/// Matched against each variant's own
/// [`fmt::Display`](std::fmt::Display), so it cannot drift from the rendering
/// it inverts.
pub(crate) fn status_named_by(text: &str) -> Option<ProcStatus> {
    const EVERY: [ProcStatus; 6] = [
        ProcStatus::Starting,
        ProcStatus::Online,
        ProcStatus::Stopping,
        ProcStatus::Stopped,
        ProcStatus::Errored,
        ProcStatus::WaitingRestart,
    ];
    EVERY.into_iter().find(|status| status.to_string() == text)
}

/// Colours a cell [`Role::Ink3`] when it holds the `-` placeholder: an absent
/// value must not compete with a real one. [`Paint::Default`] is what an impl
/// returns to ask for this rule.
pub(crate) fn mute_a_dash(cell: &mut String, presentation: Presentation) {
    if cell == "-" {
        colour_cell(cell, Role::Ink3, presentation);
    }
}

/// Wraps `cell` in [`crate::output::paint::style_for`]'s span for `role`, or
/// leaves it untouched when `presentation.colour` is off. The one place
/// colour is applied, STATUS included through [`status_cell`].
pub(crate) fn colour_cell(cell: &mut String, role: Role, presentation: Presentation) {
    if !presentation.colour {
        return;
    }
    let style = crate::output::paint::style_for(role, presentation.deep_colour);
    *cell = format!("{style}{cell}{style:#}");
}

/// MEM's colour boundary, in bytes. 128 MiB separates the two footprints a
/// real flock shows side by side: a worker at a few megabytes, a service at
/// hundreds.
pub(crate) const MEM_ELEVATED_BYTES: u64 = 128 * 1024 * 1024;

/// [`Role`] for a MEM cell. `None` is [`Role::Ink3`], the colour every dash
/// gets; otherwise [`MEM_ELEVATED_BYTES`]'s two-tier ramp.
///
/// Two tiers, never [`Role::Bark`], which is reserved for faults. The ramp
/// answers "is this unusual for this flock"; `--format json` carries the
/// exact number.
pub(crate) fn mem_role(memory_bytes: Option<u64>) -> Role {
    match memory_bytes {
        None => Role::Ink3,
        Some(bytes) if bytes >= MEM_ELEVATED_BYTES => Role::Butter,
        Some(_) => Role::Meadow,
    }
}

/// CPU's colour boundary, in percent of one core. Sustained use at or above
/// this is unusual for a steady-state service. `Role::Bark` stays reserved
/// for a fault, never a busy sheep.
pub(crate) const CPU_ELEVATED_PERCENT: f32 = 50.0;

/// [`Role`] for a CPU cell. `None` and `0.0%` are both [`Role::Ink3`]:
/// neither is news. A busy sheep takes [`CPU_ELEVATED_PERCENT`]'s ramp.
pub(crate) fn cpu_role(cpu_percent: Option<f32>) -> Role {
    match cpu_percent {
        None => Role::Ink3,
        Some(cpu) if cpu <= 0.0 => Role::Ink3,
        Some(cpu) if cpu >= CPU_ELEVATED_PERCENT => Role::Butter,
        Some(_) => Role::Meadow,
    }
}

/// [`Role`] for a RESTARTS cell: `Role::Ink3` at zero, `Role::Butter` above
/// it. Never `Role::Bark`: a restart is a signal, not a fault.
pub(crate) const fn restarts_role(restarts: u32) -> Role {
    if restarts == 0 {
        Role::Ink3
    } else {
        Role::Butter
    }
}

/// [`Role`] for an EXIT cell, mirroring [`exit_cell`]'s branches rather than
/// parsing the rendered text back: a live process and a clean `0` exit both
/// take `Role::Ink3`. Only a nonzero code or a signal earns `Role::Bark`.
pub(crate) fn exit_role(pid: Option<u32>, last_exit: Option<ExitInfo>) -> Role {
    if pid.is_some() {
        return Role::Ink3;
    }
    match last_exit {
        Some(ExitInfo {
            code: Some(code), ..
        }) if code != 0 => Role::Bark,
        Some(ExitInfo {
            signal: Some(_), ..
        }) => Role::Bark,
        // A clean `0` exit, an exit the daemon could not characterize (both
        // fields `None`), or no exit recorded: none of the three is news.
        _ => Role::Ink3,
    }
}

/// The EXIT column's cell: the last exit's code or signal name for a sheep
/// that is not running, `-` otherwise.
///
/// Gated on `pid` rather than on `status`: `pid` is `None` for exactly the
/// statuses with no live process. `pub(crate)` so `lookout::view::flock`
/// shares the rule.
pub(crate) fn exit_cell(pid: Option<u32>, last_exit: Option<ExitInfo>) -> String {
    if pid.is_some() {
        return "-".to_string();
    }
    match last_exit {
        None => "-".to_string(),
        Some(ExitInfo {
            code: Some(code), ..
        }) => code.to_string(),
        Some(ExitInfo {
            signal: Some(signal),
            ..
        }) => signal_label(signal),
        // Both `None`: an exit the daemon could not characterize.
        Some(ExitInfo {
            code: None,
            signal: None,
        }) => "-".to_string(),
    }
}

/// The CFG column's cell: `!N` for N fields parked for the next spawn, `*N`
/// for an override with nothing parked, `-` for neither.
///
/// `pending` wins when both are non-empty: a parked field is one `shep
/// reload` away from taking effect. `shep describe` lists the names a cell
/// has no room for. `pub(crate)` so `lookout::view::flock` shares the rule.
pub(crate) fn cfg_cell(pending: Option<&[String]>, overridden: Option<&[String]>) -> String {
    match pending {
        Some(fields) if !fields.is_empty() => return format!("!{}", fields.len()),
        _ => {}
    }
    match overridden {
        Some(fields) if !fields.is_empty() => format!("*{}", fields.len()),
        _ => "-".to_string(),
    }
}

/// Renders a raw unix signal number as its canonical name (`SIGKILL`), or the
/// bare number when this platform's own signal table has none for it.
///
/// Resolving it here is sound because a client reaches a daemon only over a
/// local socket, so the `ProcessInfo` came from this same OS. Gated at the
/// item rather than in the body: `nix` is a unix-only dependency, so a
/// Windows build never links it.
#[cfg(unix)]
fn signal_label(raw: i32) -> String {
    nix::sys::signal::Signal::try_from(raw)
        .map_or_else(|_| raw.to_string(), |signal| signal.as_str().to_string())
}

/// Windows counterpart: no signal table to consult, so the bare number.
#[cfg(not(unix))]
fn signal_label(raw: i32) -> String {
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::DogSource;
    use shep_core::status::ProcStatus;

    use crate::output::{DogRows, Render};

    use super::super::tests::{coloured, painted, sample_dog, sample_info};
    use super::*;

    /// A pending field an operator cannot see is a silent divergence.
    #[test]
    fn the_cfg_cell_marks_a_sheep_with_pending_config() {
        let mut info = sample_info(1, "web", 60_000);
        info.pending = Some(vec!["env".to_string()]);
        assert_eq!(
            cfg_cell(info.pending.as_deref(), info.overridden.as_deref()),
            "!1"
        );

        let clean = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        assert_eq!(
            cfg_cell(clean.pending.as_deref(), clean.overridden.as_deref()),
            "-"
        );
    }

    /// The boundary is inclusive on the `Butter` side.
    #[test]
    fn mem_role_ramps_at_its_documented_boundary() {
        assert_eq!(mem_role(None), Role::Ink3);
        assert_eq!(mem_role(Some(MEM_ELEVATED_BYTES - 1)), Role::Meadow);
        assert_eq!(mem_role(Some(MEM_ELEVATED_BYTES)), Role::Butter);
        // A light app and a heavy one must land on opposite sides.
        assert_eq!(mem_role(Some(3_800_000)), Role::Meadow, "3.8M is light");
        assert_eq!(mem_role(Some(800_000_000)), Role::Butter, "800M is heavy");
    }

    /// Idle (`0.0%`) stays `Ink3`; the boundary is inclusive on the `Butter`
    /// side.
    #[test]
    fn cpu_role_ramps_at_its_documented_boundary() {
        assert_eq!(cpu_role(None), Role::Ink3);
        assert_eq!(cpu_role(Some(0.0)), Role::Ink3);
        assert_eq!(cpu_role(Some(0.1)), Role::Meadow);
        assert_eq!(cpu_role(Some(CPU_ELEVATED_PERCENT - 0.1)), Role::Meadow);
        assert_eq!(cpu_role(Some(CPU_ELEVATED_PERCENT)), Role::Butter);
        assert_eq!(cpu_role(Some(99.0)), Role::Butter);
    }

    #[test]
    fn restarts_role_is_ink3_only_at_exactly_zero() {
        assert_eq!(restarts_role(0), Role::Ink3);
        assert_eq!(restarts_role(1), Role::Butter);
        assert_eq!(restarts_role(u32::MAX), Role::Butter);
    }

    /// A still-running sheep, a clean `0` and an uncharacterised exit are all
    /// `Ink3`.
    #[test]
    fn exit_role_is_bark_only_for_a_genuine_failure() {
        // Still running, over a `last_exit` that is still recorded.
        assert_eq!(
            exit_role(
                Some(1234),
                Some(ExitInfo {
                    code: Some(1),
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, no exit ever recorded.
        assert_eq!(exit_role(None, None), Role::Ink3);
        // Not running, a clean exit.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: Some(0),
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, the daemon could not characterize the exit.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: None,
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, a genuine nonzero exit code.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: Some(1),
                    signal: None
                })
            ),
            Role::Bark
        );
        // Not running, killed by a signal.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: None,
                    signal: Some(9)
                })
            ),
            Role::Bark
        );
    }

    // --- Colour: the seven tables that are not the flock listing ---------
    //
    // Each assertion compares the exact painted string: a check for the mere
    // presence of an escape byte passes on a cell painted the wrong role.

    /// Driven through [`paint`] over a reversed header list, so every column
    /// sits somewhere it never sits in life.
    #[test]
    fn a_columns_colour_follows_its_name_and_not_its_position() {
        let dog = sample_dog(ProcStatus::Online, Some(14_110));
        let forwards = DogRows::headers();
        let backwards: Vec<&'static str> = forwards.iter().copied().rev().collect();

        let mut cells: Vec<String> = DogRows(vec![dog.clone()]).rows().remove(0);
        cells.reverse();
        let painted_rows = paint(vec![cells], &backwards, coloured(), true, |header, _, _| {
            process_info_paint(header, &dog)
        });

        let at = |name: &str| backwards.iter().position(|h| *h == name).unwrap();
        // Every one of these indices differs from the column's real one.
        assert_eq!(painted_rows[0][at("ID")], painted("9", Role::Ink3));
        assert_eq!(painted_rows[0][at("RESTARTS")], painted("4", Role::Butter));
        assert_eq!(painted_rows[0][at("MEM")], painted("3.0M", Role::Meadow));
        assert_eq!(painted_rows[0][at("CPU")], painted("0.0%", Role::Ink3));
        assert_eq!(
            painted_rows[0][at("SOURCE")],
            painted("adopted", Role::Butter)
        );
        assert_eq!(
            painted_rows[0][at("STATUS")],
            painted("(o.o) online", Role::Meadow)
        );
        assert_eq!(painted_rows[0][at("NAME")], "log-rotate", "still plain");
        assert_eq!(painted_rows[0][at("UPTIME")], "41s", "still plain");
    }

    /// `unknown` is `Butter` and never `Bark`: a client older than its daemon
    /// usually has a perfectly healthy dog.
    #[test]
    fn source_draws_the_trust_line_and_never_paints_a_working_dog_red() {
        assert_eq!(source_role(&DogSource::BuiltIn), Role::Ink3);
        assert_eq!(
            source_role(&DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string()
            }),
            Role::Butter
        );
        assert_ne!(
            source_role(&DogSource::BuiltIn),
            source_role(&DogSource::Adopted {
                path: "/x".to_string()
            }),
            "shep's own code and a third-party binary must not look the same"
        );
    }

    /// All eleven kinds the three verbs produce.
    #[test]
    fn an_outcome_lands_in_the_tier_its_kind_calls_for() {
        for worked in ["replied", "delivered", "sent"] {
            assert_eq!(outcome_role(worked), Role::Meadow, "{worked}");
        }
        for quiet in ["skipped", "not_running"] {
            assert_eq!(outcome_role(quiet), Role::Ink3, "{quiet}");
        }
        for failed in ["timed_out", "failed", "not_written"] {
            assert_eq!(outcome_role(failed), Role::Bark, "{failed}");
        }
        for gap in ["no_channel", "no_stdin"] {
            assert_eq!(outcome_role(gap), Role::Butter, "{gap}");
        }
        assert_eq!(
            outcome_role("unknown"),
            Role::Butter,
            "a kind this client predates is a version gap, not a fault"
        );
    }

    /// A variant missing from `status_named_by`'s list renders plain instead
    /// of failing to compile. Driven off `Display`, so it also fails if the
    /// two disagree.
    #[test]
    fn every_status_is_recognised_by_its_own_rendering() {
        for status in [
            ProcStatus::Starting,
            ProcStatus::Online,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ] {
            assert_eq!(
                status_named_by(&status.to_string()),
                Some(status),
                "{status} is not recognised by its own rendering"
            );
        }
        assert_eq!(
            status_named_by("no shepherd running"),
            None,
            "and a sentence is not mistaken for one"
        );
    }

    /// `Row::reported` is the lookout's own copy, not shared code, so every
    /// axis that decides the answer is driven together: `dog`, `handshook`
    /// and every `ProcStatus`.
    #[test]
    fn the_flock_table_and_the_lookout_read_a_dogs_silence_the_same_way() {
        use crate::lookout::app::Row;

        let statuses = [
            ProcStatus::Starting,
            ProcStatus::Online,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ];
        let handshooks = [None, Some(false), Some(true)];
        let dogs = [None, Some(DogSource::BuiltIn)];

        for dog in &dogs {
            for &handshook in &handshooks {
                for &status in &statuses {
                    let info = ProcessInfo::builder(9, "log-rotate", status)
                        .dog(dog.clone())
                        .handshook(handshook)
                        .build();

                    let table = reported(&info);
                    let dashboard = Row {
                        info: info.clone(),
                        anchor: std::time::Instant::now(),
                    }
                    .reported();

                    assert_eq!(
                        table, dashboard,
                        "dog={dog:?} handshook={handshook:?} status={status:?}"
                    );
                }
            }
        }
    }
}
