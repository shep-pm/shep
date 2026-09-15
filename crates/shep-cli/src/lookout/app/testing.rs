//! Fixtures more than one of `app`'s test modules leans on.

use super::*;
pub(super) use crate::lookout::edits::EditKey;
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

/// Lands a `Request::SheepConfig` reply for `web` carrying `env_keys`,
/// the way the event loop lands one after a write or an `r`.
pub(super) fn refresh_config(app: &mut App, env_keys: &[&str]) {
    let mut config = shep_core::config::AppConfig {
        name: "web".to_string(),
        ..Default::default()
    };
    for key in env_keys {
        config.env.insert((*key).to_string(), "x".to_string());
    }
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(
            shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new()),
        ))),
    });
}

/// `esc`, and `c` right behind it if that raised the close dialog
/// instead of writing outright.
///
/// What every write-on-close test in this module wants now: every
/// sheep fixture here parks `kill_signal` unconditionally
/// (`sheep_config_view`'s own default), so a bare `esc` only asks. `c`
/// is what actually gets the write onto the wire, the same as an
/// operator continuing past the dialog would; harmless when nothing
/// asked, since [`App::close_dialog`] is `None` and this returns
/// `esc`'s own effect unchanged.
pub(super) fn close_writing(app: &mut App) -> Effect {
    let effect = app.update(Msg::Key(KeyPress::Escape));
    if app.close_dialog().is_some() {
        app.update(Msg::Key(KeyPress::Continue))
    } else {
        effect
    }
}

/// Every request a closed pane's batch would put on the wire, in the
/// order it sends them.
pub(super) fn wire_all(effect: Effect) -> Vec<Request> {
    match effect {
        Effect::SendAll(batch) => batch.iter().map(Sent::request).collect(),
        other => panic!("expected a batch, got {other:?}"),
    }
}

/// The `Sent` values a closed pane's batch carries.
pub(super) fn wire_batch(effect: Effect) -> Vec<Sent> {
    match effect {
        Effect::SendAll(batch) => batch,
        other => panic!("expected a batch, got {other:?}"),
    }
}

/// The one request a closed pane's batch carries, or a panic naming
/// how many it carried instead.
pub(super) fn one_wire(effect: Effect) -> Request {
    let mut requests = wire_all(effect);
    assert_eq!(requests.len(), 1, "{requests:?}");
    requests.remove(0)
}

/// The value the open pane has filed for `key`.
pub(super) fn filed_value(app: &App, key: &str) -> serde_json::Value {
    let entry = app
        .config_pane()
        .expect("the pane is open")
        .edits()
        .get(&EditKey::Field(key.to_owned()))
        .unwrap_or_else(|| panic!("nothing filed for {key}"));
    match entry.edit() {
        PaneEdit::Set { value, .. } => value.as_value().clone(),
        other => panic!("expected a field set, got {other:?}"),
    }
}
