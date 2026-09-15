//! Sheep-shaped rows: the [`Render`] impls for anything whose payload is a
//! `Vec<ProcessInfo>` or a `Vec<Lamb>`, and the paint/cell helpers they share.

use std::collections::BTreeMap;

use serde::Serialize;
use shep_core::protocol::{DogSource, ExitInfo, Lamb, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::output::Render;
use crate::style::Presentation;
use crate::vocabulary::{Reported, Role};

/// `Vec<ProcessInfo>` for every verb whose reply carries one: `flock`,
/// `describe`, `fold`, `start`, `stop`, `restart`, `reopen`, `flush`.
///
/// A newtype for the orphan rule; `transparent`, so the JSON is a plain
/// array of `ProcessInfo`.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct FlockRows(pub Vec<ProcessInfo>);

impl Render for FlockRows {
    fn headers() -> &'static [&'static str] {
        &[
            "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CFG", "CPU", "MEM", "UPTIME",
            "FOLD", "SMIT",
        ]
    }

    /// One row per process, and the path `Bare` takes: `table_of` calls this
    /// directly when [`crate::style::StyleLevel::boxes`] is false, so the
    /// `web:0` suffix lives in [`plain_row`] as well as in
    /// [`Self::rows_for`].
    fn rows(&self) -> Vec<Vec<String>> {
        name_groups(&self.0)
            .flat_map(|group| {
                let slotted = group.len() > 1 && group.iter().all(|p| p.instance.is_some());
                group.iter().map(move |p| plain_row(p, slotted))
            })
            .collect()
    }

    /// [`Self::rows`], each cell painted by [`process_info_paint`]'s rule for
    /// its column, or [`group_paint`]'s for a header row.
    ///
    /// An app with several instances groups under one header row when
    /// [`crate::style::StyleLevel::boxes`] is true. `sort_flock` orders the
    /// listing by (name, instance, id), so instances are adjacent and one
    /// pass groups them. `Bare` never reaches this method; see [`Self::rows`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        /// Which payload a rendered row came from, for [`paint`]'s rule.
        enum RowSource<'a> {
            /// A slot row or a plain (ungrouped) row.
            Sheep(&'a ProcessInfo),
            /// A group's header row, plus its summed totals.
            Group(&'a [ProcessInfo], GroupTotals),
        }

        let mut out = Vec::with_capacity(self.0.len());
        let mut sources: Vec<RowSource<'_>> = Vec::with_capacity(self.0.len());
        for group in name_groups(&self.0) {
            // A slot nobody reported cannot be grouped or suffixed.
            let slotted = group.len() > 1 && group.iter().all(|p| p.instance.is_some());
            if slotted && presentation.level.boxes() {
                let totals = group_totals(group);
                out.push(group_row(group, &totals));
                sources.push(RowSource::Group(group, totals));
                for p in group {
                    out.push(slot_row(p));
                    sources.push(RowSource::Sheep(p));
                }
            } else {
                for p in group {
                    out.push(plain_row(p, slotted));
                    sources.push(RowSource::Sheep(p));
                }
            }
        }

        paint(
            out,
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, index| match &sources[index] {
                RowSource::Sheep(p) => process_info_paint(header, p),
                RowSource::Group(g, totals) => group_paint(header, g, totals),
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            "NAME" => "name",
            "STATUS" => "status",
            "PID" => "pid",
            "RESTARTS" => "restarts",
            "EXIT" => "last_exit",
            // `cfg_cell` folds two fields into one cell; `overridden` rides
            // in `JSON_ONLY`.
            "CFG" => "pending",
            "CPU" => "cpu_percent",
            "MEM" => "memory_bytes",
            "UPTIME" => "uptime_ms",
            "FOLD" => "fold",
            "SMIT" => "smit",
            other => panic!("FlockRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // Absolute paths, wider than the rest of the table together.
        "out_file",
        "err_file",
        // Always `null`: every row here is a sheep.
        "dog",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // A handshake is a fact about a dog, and every row here is a sheep.
        "handshook",
        // A dog fact too: a shepherd gives up only on dogs.
        "dog_stale",
        // The table labels each slot instead; the JSON stays flat.
        "instance",
        // CFG's header maps to `pending`, so `overridden` rides here.
        "overridden",
        // No new `shep flock` column for this: it names other sheep, not
        // this row's own status, and the table already drops columns under
        // pressure.
        "depends_on",
        // MEM already reports the raw reading; a gauge against this ceiling
        // is lookout's, not this table's.
        "max_memory",
        // CPU already reports the percentage; the raw counter behind it is
        // for a client differencing its own polls, not this table.
        "cpu_ms",
        // A reload's own answer, absent from every other listing this type
        // renders. Milliseconds are for the client waiting the swap out,
        // not for a column an operator reads.
        "reload_deadline_ms",
    ];

    // Parallel to `headers()`. The rest survive in ascending order. CFG ties
    // with EXIT at `6` and yields first: `render_boxed_ex`'s `max_by_key`
    // takes the last of an equal pair, and CFG sits later in `headers()`.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 2, 4, 6, 6, 5, 3, 1, 7, 8];
}

/// Splits a listing into runs of one app's adjacent rows, keyed on NAME.
///
/// Sound only because `sort_flock` orders by (name, instance, id). The
/// `slotted` rule stays with each caller.
fn name_groups(items: &[ProcessInfo]) -> impl Iterator<Item = &[ProcessInfo]> {
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
fn group_row(group: &[ProcessInfo], totals: &GroupTotals) -> Vec<String> {
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
fn slot_row(p: &ProcessInfo) -> Vec<String> {
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
fn plain_row(p: &ProcessInfo, slotted: bool) -> Vec<String> {
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
fn group_paint(header: &str, group: &[ProcessInfo], totals: &GroupTotals) -> Paint {
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

/// The dogs half of a flock listing: the `ProcessInfo`s whose `dog` marker
/// is set.
///
/// Every column the two tables share sits in the same order; each table's own
/// columns come last:
///
/// ```text
/// common:  ID  NAME  STATUS  PID  RESTARTS  EXIT  CPU  MEM  UPTIME
/// sheep:   ... + FOLD  SMIT
/// dogs:    ... + SOURCE
/// ```
///
/// `FOLD` and `SMIT` are impossible for a dog rather than empty: a dog
/// belongs to no fold, and a smit is a mark a dog paints on a sheep.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DogRows(pub Vec<ProcessInfo>);

/// `DogSource`'s table rendering, shared by every payload with a SOURCE
/// column. `DogSource` is `#[non_exhaustive]`, so a kind this client predates
/// renders `unknown`.
fn dog_source_label(source: &DogSource) -> &'static str {
    match source {
        DogSource::BuiltIn => "built-in",
        DogSource::Adopted { .. } => "adopted",
        _ => "unknown",
    }
}

impl Render for DogRows {
    fn headers() -> &'static [&'static str] {
        &[
            "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CPU", "MEM", "UPTIME", "SOURCE",
        ]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|p| {
                vec![
                    p.id.to_string(),
                    p.name.clone(),
                    // A dog whose process is up and which has never
                    // answered this shepherd reads `silent`, not `online`.
                    reported(p).word(),
                    p.pid.map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                    p.restarts.to_string(),
                    exit_cell(p.pid, p.last_exit),
                    p.cpu_percent
                        .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
                    p.memory_bytes
                        .map_or_else(|| "-".to_string(), crate::output::human_bytes),
                    crate::output::human_duration(p.uptime_ms),
                    // Never the adopted path: too wide for a column. `None`
                    // is unreachable, since callers filter on
                    // `dog.is_some()`.
                    p.dog.as_ref().map_or("-".to_string(), |source| {
                        dog_source_label(source).to_string()
                    }),
                ]
            })
            .collect()
    }

    /// [`process_info_paint`], the same function `FlockRows` uses; SOURCE is
    /// the one column not shared with that table.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        paint(
            self.rows(),
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, index| process_info_paint(header, &self.0[index]),
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            "NAME" => "name",
            "STATUS" => "status",
            "PID" => "pid",
            "RESTARTS" => "restarts",
            "EXIT" => "last_exit",
            "CPU" => "cpu_percent",
            "MEM" => "memory_bytes",
            "UPTIME" => "uptime_ms",
            "SOURCE" => "dog",
            other => panic!("DogRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // A sheep concept: a dog is supervised, never grouped by fold.
        "fold",
        // Absolute paths, wider than the rest of the table together.
        "out_file",
        "err_file",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // A dog paints smits; nothing paints one on a dog.
        "smit",
        // It decides what STATUS says, so a column would say it twice.
        // `status` alone still reads `online` for a silent dog.
        "handshook",
        // Not derivable from `handshook`: a dog spawned a moment ago and one
        // this shepherd has given up on are both `handshook: false`.
        "dog_stale",
        // Always `Some(0)`: a dog is never stocked to N instances.
        "instance",
        // `Actor::apply_one` refuses a config entry naming a dog, so a load
        // can neither park nor override one.
        "pending",
        "overridden",
        // A sheep concept: a dog is never staged behind a start order.
        "depends_on",
        // A sheep concept: a dog has no `AppConfig` and so no ceiling to
        // report; always `null` here.
        "max_memory",
        // CPU already reports the percentage; the raw counter behind it is
        // for a client differencing its own polls, not this table.
        "cpu_ms",
    ];

    // Parallel to `headers()`. The nine shared columns carry the numbers
    // `FlockRows` gives them, so both tables narrow in the same order.
    // SOURCE takes FOLD's `7`.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 2, 4, 6, 5, 3, 1, 7];
}

/// One sheep's lamb tree, as `describe`'s second table.
///
/// Not `#[serde(transparent)]`: this type's JSON is never read, since
/// `describe --format json` serializes the listing as [`FlockRows`] with its
/// own `lambs`. It exists to reach [`render_table`](super::render_table).
#[derive(Debug, Serialize)]
pub struct LambRows(pub Vec<Lamb>);

/// No colour: both columns are identity, and a lamb has no status, reading or
/// placeholder for one to carry.
impl Render for LambRows {
    fn headers() -> &'static [&'static str] {
        &["PID", "NAME"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|lamb| vec![lamb.pid.to_string(), lamb.name.clone()])
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "PID" => "pid",
            "NAME" => "name",
            other => panic!("LambRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Two columns, both identity, so this never
    // narrows; spelled out so a later header does not inherit it by omission.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// `shep enable <name>`: what the config edit and, if a shepherd is running,
/// the resulting `EnableDog` RPC did.
///
/// [`Self::shepherd_acted`] and [`Self::status`] are how a `--format json`
/// consumer tells the two outcomes apart.
#[derive(Debug, Serialize)]
pub struct DogEnabledRow {
    /// The dog's name.
    pub name: String,
    /// Where its binary comes from, read out of `shep.toml`:
    /// [`DogSource::Adopted`] for a name in `[daemon] adopted_dogs`,
    /// [`DogSource::BuiltIn`] otherwise.
    pub source: DogSource,
    /// Whether a shepherd was reached and asked to start the dog. `false`
    /// means only the config changed; `enable` never autostarts one.
    pub shepherd_acted: bool,
    /// The dog's resulting status: a real `ProcStatus` rendering
    /// (`"online"`, `"starting"`, ...) when a shepherd started it, or a
    /// sentence explaining why not when none answered.
    pub status: String,
}

// Shared scaffolding for the four dog-action tables. All four render one row
// of `["NAME", "SOURCE", "SHEPHERD", "STATUS"]` and share the JSON keys, the
// priorities and the paint dispatch; each resolves its own `source` to a
// label first.
struct DogActionRow<'a> {
    name: &'a str,
    source: &'a str,
    shepherd_acted: bool,
    status: &'a str,
}

impl DogActionRow<'_> {
    fn headers() -> &'static [&'static str] {
        &["NAME", "SOURCE", "SHEPHERD", "STATUS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![
            self.name.to_string(),
            self.source.to_string(),
            self.shepherd_acted.to_string(),
            self.status.to_string(),
        ]]
    }

    // The four dog-action rows' shared treatment; see `dog_action_paint`.
    fn rows_for(
        rows: Vec<Vec<String>>,
        presentation: Presentation,
        status_word: bool,
    ) -> Vec<Vec<String>> {
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| dog_action_paint(header, cell),
        )
    }

    // Parallel to `headers()`. SOURCE drops before SHEPHERD, and a 4-column
    // table loses only one of the two.
    const PRIORITIES: &'static [u8] = &[0, 7, 6, 0];
}

// One JSON key rule for the four dog-action tables; the panic names the
// concrete type. A macro, not a shared fn: rustc's dead-code pass cannot see
// a use that occurs only inside another trait impl's body.
macro_rules! dog_action_json_key {
    ($caller:expr, $header:expr) => {{
        let caller: &'static str = $caller;
        let header: &str = $header;
        match header {
            "NAME" => "name",
            "SOURCE" => "source",
            "SHEPHERD" => "shepherd_acted",
            "STATUS" => "status",
            other => panic!("{caller}::headers() does not include {other:?}"),
        }
    }};
}

impl Render for DogEnabledRow {
    fn headers() -> &'static [&'static str] {
        DogActionRow::headers()
    }

    fn rows(&self) -> Vec<Vec<String>> {
        DogActionRow {
            name: &self.name,
            source: dog_source_label(&self.source),
            shepherd_acted: self.shepherd_acted,
            status: &self.status,
        }
        .rows()
    }

    /// Shared with the other three dog-action rows; see
    /// [`DogActionRow::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        DogActionRow::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        dog_action_json_key!("DogEnabledRow", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = DogActionRow::PRIORITIES;
}

/// `shep disable <name>`: what the config edit and, if a shepherd is running,
/// the resulting `DisableDog` RPC did.
///
/// [`Self::source`] comes from the same `shep.toml` lookup
/// [`DogEnabledRow::source`] uses, never from the reply, which carries only
/// ids.
#[derive(Debug, Serialize)]
pub struct DogDisabledRow {
    /// The dog's name.
    pub name: String,
    /// Where its binary comes from; see this type's own doc.
    pub source: DogSource,
    /// Whether a shepherd was reached and asked to stop the dog.
    pub shepherd_acted: bool,
    /// The dog's resulting status: `"stopped"` when a shepherd acted, or a
    /// sentence explaining why not when none answered.
    pub status: String,
}

impl Render for DogDisabledRow {
    fn headers() -> &'static [&'static str] {
        DogActionRow::headers()
    }

    fn rows(&self) -> Vec<Vec<String>> {
        DogActionRow {
            name: &self.name,
            source: dog_source_label(&self.source),
            shepherd_acted: self.shepherd_acted,
            status: &self.status,
        }
        .rows()
    }

    /// Same treatment as [`DogEnabledRow::rows_for`]; see
    /// [`DogActionRow::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        DogActionRow::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        dog_action_json_key!("DogDisabledRow", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = DogActionRow::PRIORITIES;
}

/// `shep adopt <path> [--name <name>]`: what the config edit and, if a
/// shepherd is running, the resulting `EnableDog` RPC did.
///
/// [`Self::source`] is always [`DogSource::Adopted`]: this verb vetted the
/// path itself, so it looks nothing up.
#[derive(Debug, Serialize)]
pub struct DogAdoptedRow {
    /// The dog's name.
    pub name: String,
    /// Always [`DogSource::Adopted`], carrying the vetted, canonicalized
    /// path `adopt` just recorded.
    pub source: DogSource,
    /// Whether a shepherd was reached and asked to start the dog. `false`
    /// means only the config changed; no verb here autostarts one.
    pub shepherd_acted: bool,
    /// The dog's resulting status: a real `ProcStatus` rendering
    /// (`"online"`, `"starting"`, ...) when a shepherd started it, or a
    /// sentence explaining why not when none answered.
    pub status: String,
}

impl Render for DogAdoptedRow {
    fn headers() -> &'static [&'static str] {
        DogActionRow::headers()
    }

    fn rows(&self) -> Vec<Vec<String>> {
        DogActionRow {
            name: &self.name,
            source: dog_source_label(&self.source),
            shepherd_acted: self.shepherd_acted,
            status: &self.status,
        }
        .rows()
    }

    /// Same treatment as [`DogEnabledRow::rows_for`]; see
    /// [`DogActionRow::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        DogActionRow::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        dog_action_json_key!("DogAdoptedRow", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = DogActionRow::PRIORITIES;
}

/// `shep rehome <name>`: what the config edit and, if a shepherd is running,
/// the resulting `DisableDog` RPC did.
///
/// [`Self::source`] is an [`Option`] because `rehome` reports what it forgot,
/// and it still runs for a name `shep.toml` never had an entry for.
#[derive(Debug, Serialize)]
pub struct DogRehomedRow {
    /// The dog's name.
    pub name: String,
    /// Where its binary came from, read before this verb forgot it. See
    /// this type's own doc for what `None` means.
    pub source: Option<DogSource>,
    /// Whether a shepherd was reached and asked to stop the dog.
    pub shepherd_acted: bool,
    /// The dog's resulting status: `"stopped"` when a shepherd acted, or a
    /// sentence explaining why not when none answered.
    pub status: String,
}

impl Render for DogRehomedRow {
    fn headers() -> &'static [&'static str] {
        DogActionRow::headers()
    }

    fn rows(&self) -> Vec<Vec<String>> {
        // `-` for `None`, as `DogRows::rows` renders it.
        let source_label = self.source.as_ref().map_or_else(
            || "-".to_string(),
            |source| dog_source_label(source).to_string(),
        );
        DogActionRow {
            name: &self.name,
            source: &source_label,
            shepherd_acted: self.shepherd_acted,
            status: &self.status,
        }
        .rows()
    }

    /// Same treatment as [`DogEnabledRow::rows_for`]; see
    /// [`DogActionRow::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        DogActionRow::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        dog_action_json_key!("DogRehomedRow", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = DogActionRow::PRIORITIES;
}

/// `Response::Flushed(Vec<ProcessInfo>)`: the sheep a `shep flush` matched,
/// rendered by the files it emptied rather than by their lifecycle.
///
/// Serializes exactly as [`FlockRows`] does, over the same
/// `Vec<ProcessInfo>`, so only the table differs. `out_file`/`err_file` are
/// free-form config taken verbatim, so a mistyped one empties something that
/// is not a log.
///
/// One row per sheep: several can share a log path, and the daemon truncates
/// each distinct path once.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct FlushedRows(pub Vec<ProcessInfo>);

impl Render for FlushedRows {
    fn headers() -> &'static [&'static str] {
        &["ID", "NAME", "OUT_FILE", "ERR_FILE"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|p| {
                vec![
                    p.id.to_string(),
                    p.name.clone(),
                    // `-`: a peer daemon predating the field, never a sheep
                    // with no log file.
                    p.out_file.clone().unwrap_or_else(|| "-".to_string()),
                    p.err_file.clone().unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect()
    }

    /// [`process_info_paint`] again: ID muted, NAME plain, and both path
    /// columns left to the dash rule. A real path is the subject of this
    /// table rather than a reading about it.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        paint(
            self.rows(),
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, index| process_info_paint(header, &self.0[index]),
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            "NAME" => "name",
            "OUT_FILE" => "out_file",
            "ERR_FILE" => "err_file",
            other => panic!("FlushedRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // A sheep's lifecycle and resource use, which a flush neither reads
        // nor changes. They stay in the JSON so a consumer switching on the
        // envelope's `command` does not find the record shape switching too.
        "status",
        "pid",
        "restarts",
        "uptime_ms",
        "fold",
        "cpu_percent",
        "memory_bytes",
        // The raw counter behind `cpu_percent`, same reason.
        "cpu_ms",
        // Every row is a sheep: no `dog`, no handshake, and nothing for a
        // shepherd to give up on.
        "dog",
        "handshook",
        "dog_stale",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // Nothing a flush reads or changes, and a column each would push
        // OUT_FILE/ERR_FILE off the side of a terminal.
        "last_exit",
        "smit",
        "instance",
        "pending",
        "overridden",
        // Nothing a flush reads or changes.
        "depends_on",
        // A ceiling is a resource reading, the same reason `memory_bytes`
        // rides here.
        "max_memory",
    ];

    // Parallel to `headers()`. ERR_FILE survives one round longer than
    // OUT_FILE: a crash is read from stderr first.
    const PRIORITIES: &'static [u8] = &[0, 0, 7, 6];
}
