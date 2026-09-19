//! Sheep, flocks, and the dashboards built straight from one.

use std::time::Instant;

use shep_core::protocol::{BusEvent, DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::lookout::app::{ActionVerb, App, Control, KeyPress, Msg, RowKey};
use crate::lookout::tail::{Stream, Tail};
use crate::lookout::theme::Palette;

use super::bleats::line;
use super::palette::plain;

/// A dashboard with `flock` listed and nothing else applied.
pub fn app_with(flock: Vec<ProcessInfo>, palette: Palette) -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        palette,
        Control::ReadOnly,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: flock,
        at: t0,
    });
    app
}

/// One online sheep named `name`, carrying `fold`, for the fold-grouping
/// tests.
pub fn sheep_in_fold(id: u32, name: &str, fold: Option<&str>) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .fold(fold.map(str::to_string))
        .build()
}

/// One sheep in `fold` with a chosen status, for the mixed-status rows.
#[must_use]
pub fn sheep_in_fold_with_status(
    id: u32,
    name: &str,
    fold: Option<&str>,
    status: ProcStatus,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, status)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .fold(fold.map(str::to_string))
        .build()
}

/// One online sheep named `name`, carrying `fold`, `uptime_ms`, `memory` and
/// `restarts`: the shape [`crate::lookout::app::App::fold_totals`]'s rollup
/// test needs numbers to sum and to take the minimum of.
pub fn sheep_with(
    id: u32,
    name: &str,
    fold: Option<&str>,
    uptime_ms: u64,
    memory: Option<u64>,
    restarts: u32,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(uptime_ms)
        .memory_bytes(memory)
        .restarts(restarts)
        .fold(fold.map(str::to_string))
        .build()
}

/// One instance of a grouped app, at `slot`, carrying `fold`: the same
/// shape [`sheep_in_fold`] builds, with an instance slot set so
/// [`crate::lookout::app::App::grouped_names`] gathers it under a
/// [`RowKey::Group`] header.
pub fn instance_in_fold(id: u32, name: &str, slot: u32, fold: Option<&str>) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .instance(Some(slot))
        .fold(fold.map(str::to_string))
        .build()
}

/// `count` online sheep, the first `with_readings` of which report cpu and
/// memory. The rest report neither, which is the case the `-` assertions need.
pub fn flock_of(count: u32, with_readings: u32) -> Vec<ProcessInfo> {
    (0..count)
        .map(|id| {
            let reports = id < with_readings;
            ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
                .pid(Some(48_000 + id))
                .uptime_ms(4_512_000)
                .cpu_percent(reports.then_some(3.5))
                .memory_bytes(reports.then_some(182 << 20))
                .out_file(Some(format!("/home/ada/.shep/logs/sheep-{id}-out.log")))
                .err_file(Some(format!("/home/ada/.shep/logs/sheep-{id}-err.log")))
                .build()
        })
        .collect()
}

/// Two sheep and one dog, for the flock table's section-header tests: a
/// [`RowKey::Section`] splits the two kinds.
pub fn app_with_a_dog() -> App {
    let flock = vec![
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .pid(Some(48_001))
            .uptime_ms(4_512_000)
            .build(),
        ProcessInfo::builder(2, "api", ProcStatus::Online)
            .pid(Some(48_002))
            .uptime_ms(4_512_000)
            .build(),
        ProcessInfo::builder(90, "otel", ProcStatus::Online)
            .pid(Some(90_000))
            .dog(Some(DogSource::BuiltIn))
            .build(),
    ];
    app_with(flock, plain())
}

/// A dashboard with `otel` adopted from `/opt/otel`, selected, and the
/// control gate open.
pub fn app_with_a_dog_selected_and_control() -> App {
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let dog = ProcessInfo::builder(90, "otel", ProcStatus::Online)
        .pid(Some(90_000))
        .dog(Some(DogSource::Adopted {
            path: "/opt/otel".to_string(),
        }))
        .build();
    let mut app = app_with(vec![decoy, dog], plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::SelectDown));
    app
}

/// The same, with `otel` built in rather than adopted, so it carries no path.
pub fn app_with_a_built_in_dog_selected_and_control() -> App {
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let dog = ProcessInfo::builder(90, "otel", ProcStatus::Online)
        .pid(Some(90_000))
        .dog(Some(DogSource::BuiltIn))
        .build();
    let mut app = app_with(vec![decoy, dog], plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::SelectDown));
    app
}

/// A dashboard with an empty flock: nothing is selected, so the feed's own
/// "no sheep is selected" line is what renders.
pub fn with_no_selection() -> App {
    app_with(Vec::new(), plain())
}

/// A two-sheep dashboard with the selection walked onto `info`, on row 1
/// rather than row 0, so a `selected_row()` that fell back to the first row
/// would be caught. Both properties are asserted below rather than assumed.
pub fn with_selection(info: ProcessInfo) -> App {
    with_selection_and_palette(info, plain())
}

/// The same, at a given palette.
///
/// The decoy is named `!decoy` rather than `decoy`: the table reads by name,
/// and `!` sorts below every ASCII letter and digit, so the decoy is row 0
/// whatever the sheep under test is called.
pub fn with_selection_and_palette(info: ProcessInfo, palette: Palette) -> App {
    assert!(
        info.id > 0,
        "the decoy takes id 0, so the sheep under test cannot"
    );
    let wanted = info.id;
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let mut app = app_with(vec![decoy, info], palette);
    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(
        app.selected(),
        Some(RowKey::Sheep(wanted)),
        "the sheep under test must end up selected, and on row 1: the mutation \
         this fixture exists to catch reads row 0 instead"
    );
    app
}

/// A dashboard with twelve sheep and a full bleats feed, for the checks that
/// need every pane to have more than it can show.
pub fn full_app() -> App {
    let mut app = app_with(flock_of(12, 12), plain());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: (0..10)
                .map(|n| line(Stream::Out, &format!("line-{n}")))
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app
}

/// Four sheep at ids 1..=4 named `web`, `api`, `web-worker`, `cron`, with
/// `query` typed into the filter box and applied. An empty `query` leaves the
/// dashboard unfiltered, which is what the "nothing changed" assertions need.
///
/// Two of the four contain `web`, with `api` between them, so a fixture that
/// stepped over hidden rows would show up as a wrong count rather than as a
/// passing test.
pub fn filtered_app(query: &str) -> App {
    filtered_app_of(named_flock(), query)
}

/// [`filtered_app`] over an explicit flock, for the empty-flock mirror.
pub fn filtered_app_of(flock: Vec<ProcessInfo>, query: &str) -> App {
    let mut app = app_with(flock, plain());
    if !query.is_empty() {
        app.update(Msg::Key(KeyPress::FilterStart));
        for typed in query.chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
    }
    app
}

/// The same four sheep with `query` half-typed and the box still open: no
/// `TextApply`, which is the whole difference from [`filtered_app`].
pub fn editing_app(query: &str) -> App {
    let mut app = app_with(named_flock(), plain());
    app.update(Msg::Key(KeyPress::FilterStart));
    for typed in query.chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app
}

/// The four named sheep the filter fixtures share. `flock_of` names its sheep
/// `sheep-0`..`sheep-N`, which every query would match or miss together.
fn named_flock() -> Vec<ProcessInfo> {
    [(1, "web"), (2, "api"), (3, "web-worker"), (4, "cron")]
        .into_iter()
        .map(|(id, name)| {
            ProcessInfo::builder(id, name, ProcStatus::Online)
                .pid(Some(48_000 + id))
                .uptime_ms(4_512_000)
                .build()
        })
        .collect()
}

/// [`filtered_app`]'s four sheep with the gate open and the cursor on `api`
/// at id 2, which is the sheep every action assertion in this file names.
///
/// The cursor is walked to `api` rather than moved a fixed number of rows:
/// the table reads by name, so which row `api` occupies depends on the
/// other sheep's names, not on its id.
pub fn allowed_app() -> App {
    let mut app = app_with(named_flock(), plain());
    app.set_control_for_tests(Control::Allowed);
    for _ in 0..named_flock().len() {
        if app.selected() == Some(RowKey::Sheep(2)) {
            break;
        }
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    assert_eq!(
        app.selected(),
        Some(RowKey::Sheep(2)),
        "the cursor must end up on api"
    );
    app
}

/// [`allowed_app`] with `verb` armed and nothing sent.
pub fn armed_app(verb: ActionVerb) -> App {
    let mut app = allowed_app();
    app.update(Msg::Key(KeyPress::Action(verb)));
    app
}

/// [`armed_app`] confirmed: the request is out and the reply has not landed.
pub fn acting_app(verb: ActionVerb) -> App {
    let mut app = armed_app(verb);
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// An armed confirm with a filter applied and a notice standing, so the bar
/// has something in all three slots at once.
///
/// Order matters: the notice must be raised after arming, since `on_key`'s
/// normal branch clears it, and `Msg::Event` never passes through `on_key`.
pub fn armed_app_with_a_filter_and_a_notice() -> App {
    let mut app = filtered_app("api");
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
    app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
    app
}
