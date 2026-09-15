//! Fixtures more than one of `app`'s test modules leans on.

use super::*;
pub(super) use crate::lookout::view::fixtures;

pub(super) fn sheep(id: u32, name: &str, status: ProcStatus) -> ProcessInfo {
    ProcessInfo::builder(id, name, status)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .build()
}

pub(super) fn started() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::ReadOnly,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: vec![
            sheep(1, "web", ProcStatus::Online),
            sheep(2, "api", ProcStatus::Errored),
            sheep(3, "worker", ProcStatus::Online),
        ],
        at: t0,
    });
    (app, t0)
}

/// `started()`'s three sheep with the gate open and the cursor mid-list, on
/// `web` at id 1.
///
/// The table reads by name, so the ids disagree with the display order:
/// `api` 2, `web` 1, `worker` 3. Mid-list, because a cursor clamped at
/// either end would pass the tests that assert a stray `j` did not move it.
pub(super) fn allowed() -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::Allowed,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: vec![
            sheep(1, "web", ProcStatus::Online),
            sheep(2, "api", ProcStatus::Online),
            sheep(3, "worker", ProcStatus::Online),
        ],
        at: t0,
    });
    app.update(Msg::Tick { now: t0 });
    app.update(Msg::Key(KeyPress::SelectDown));
    app
}

/// `allowed()`'s shape with three instances of one app: `web` at slots 0, 1
/// and 2, ids 1 through 3. Nothing is selected; each test selects itself.
pub(super) fn allowed_with_instances() -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::Allowed,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: instanced_rows(),
        at: t0,
    });
    app
}

/// `web`'s three instances, at slots 0, 1 and 2 and ids 1 through 3.
pub(super) fn instanced_rows() -> Vec<ProcessInfo> {
    (0..3)
        .map(|slot| {
            ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                .instance(Some(slot))
                .build()
        })
        .collect()
}

/// The request an effect would put on the wire, or a panic naming what
/// came back instead. The seam this test module cares about: the
/// reducer's own `Sent` is an echo tag, and `Sent::request` is what the
/// link task actually sends.
pub(super) fn wire(effect: Effect) -> Request {
    match effect {
        Effect::Send(sent) => sent.request(),
        other => panic!("expected a request, got {other:?}"),
    }
}

/// A dashboard whose filter is set without any keymap involved.
///
/// Four sheep, two of which contain `web`: `api-web` at id 1 and
/// `web-worker` at id 4, with `cron` and `queue` between them. The table
/// sorts by name, so the gap is what makes `j` stepping over a hidden row
/// falsifiable.
pub(super) fn filtered(query: &str) -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::ReadOnly,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: vec![
            sheep(1, "api-web", ProcStatus::Online),
            sheep(2, "cron", ProcStatus::Online),
            sheep(3, "queue", ProcStatus::Online),
            sheep(4, "web-worker", ProcStatus::Online),
        ],
        at: t0,
    });
    app.set_filter(query.to_string());
    app
}

/// Walks the pane's cursor onto `key`. The pane is a public type with
/// no public "go to this field" key, so the cursor is driven the way
/// an operator drives it. A thin wrapper: `view::fixtures::select_field`
/// is this exact walk, and this module had its own copy before the tab
/// row gave a field's group somewhere to switch to first.
pub(super) fn pane_to(app: &mut App, key: &str) {
    fixtures::select_field(app, key);
}
