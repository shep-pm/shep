//! The close dialog, and the frames it is drawn into.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::protocol::{ProcessInfo, Response, SheepConfigView};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use crate::lookout::app::{App, CloseDialog, Control, KeyPress, Msg, Sent};
use crate::lookout::pane::{ConfigPane, ReloadKind};
use crate::lookout::theme::Palette;

use super::flock::with_selection_and_palette;
use super::palette::plain;
use super::render::render;
use super::sheep_pane::{
    app_in_sheep_pane_with_nothing_parked, file_edit, sheep_config_view_parking,
};

/// A [`ConfigPane`] over `web`, with `kill_timeout` and `graceful_timeout`
/// set to round numbers a close dialog's own copy names literally, `5s`
/// and `10s`: the sheep's own values a test can assert on verbatim, rather
/// than a millisecond count `resolved_display` would leave bare.
fn close_dialog_pane(
    wait_ready: bool,
    has_probe: bool,
    reuse_port: bool,
    instances: u32,
) -> ConfigPane {
    let config = AppConfig {
        name: "web".to_string(),
        kill_timeout: UpDuration::from_millis(5_000),
        graceful_timeout: UpDuration::from_millis(10_000),
        wait_ready,
        reuse_port,
        instances,
        readiness_probe: has_probe.then(|| ProbeConfig {
            kind: ProbeKind::Tcp,
            target: "127.0.0.1:8080".into(),
            interval: UpDuration::from_millis(10_000),
            timeout: UpDuration::from_millis(5_000),
            failure_threshold: 3,
        }),
        ..AppConfig::default()
    };
    ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()))
}

/// The pid every hand-built close dialog names, so a test reading the
/// heading's right clause has one number to match rather than whichever
/// the flock fixture handed out.
const DIALOG_PID: u32 = 71_578;

/// A close dialog naming `unsent` filed edits and `parked` shepherd
/// fields, over a plain overlapping-reload sheep: what
/// [`close_dialog_lines`](crate::lookout::view::pane::close::close_dialog_lines)
/// reads in its own heading and naming-sentence tests, without driving a
/// real key sequence to raise one.
pub fn close_dialog_with(unsent: usize, parked: usize) -> CloseDialog {
    let pane = close_dialog_pane(true, false, false, 1);
    let unsent_fields = (0..unsent).map(|i| format!("field{i}")).collect();
    CloseDialog::new(
        unsent_fields,
        parked,
        &pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// The same dialog [`close_dialog_with`] builds, over a sheep the
/// shepherd runs several of: no one pid to name, so the heading's right
/// clause carries none.
pub fn close_dialog_without_a_pid() -> CloseDialog {
    let pane = close_dialog_pane(true, false, false, 2);
    CloseDialog::new(
        vec!["cwd".to_string()],
        0,
        &pane,
        ProcStatus::Online,
        None,
        Instant::now(),
    )
}

/// A close dialog over a sheep whose reload takes `kind` and reaches
/// `instances` of it: what the reload row's own tests read. One unsent
/// field and nothing parked, since the reload row draws the same either
/// way and a test on it should not have to explain the heading too.
pub fn close_dialog_reloading(kind: ReloadKind, instances: u32) -> CloseDialog {
    let (wait_ready, has_probe, reuse_port) = match kind {
        // `reload_mode`'s own rule: `!wait_ready && has_probe && !reuse_port`
        // is `Serial`, anything else is `Overlap`.
        ReloadKind::Overlap => (true, false, false),
        ReloadKind::Serial => (false, true, false),
    };
    let pane = close_dialog_pane(wait_ready, has_probe, reuse_port, instances);
    CloseDialog::new(
        vec!["cwd".to_string()],
        0,
        &pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// A close dialog raised over a pane with `cwd` really filed (it needs a
/// respawn), and, when `with_live` is set, `max_restarts` filed alongside
/// it (`ApplyGroup::Live`, so the running sheep already takes it): what
/// the "everything else you changed is already live" sentence's own tests
/// read.
///
/// Driven through [`file_edit`] rather than handed synthetic names, unlike
/// [`close_dialog_with`]: `CloseDialog::live` is not a parameter, it is
/// read off the pane's own filed set, so the set has to be real for it to
/// answer anything.
pub fn close_dialog_with_live_edit(with_live: bool) -> CloseDialog {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    file_edit(&mut app, "cwd", "/srv/app");
    if with_live {
        file_edit(&mut app, "max_restarts", "9");
    }
    let pane = app.config_pane().expect("the pane is open");
    CloseDialog::new(
        pane.unsent_fields_needing_a_respawn(),
        pane.parked_count(),
        pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// The sheep pane, `cwd` filed and the close dialog raised the way `esc`
/// raises it for real (`App::close_offer`, rather than a synthetic
/// `CloseDialog::new`): what this task's own box, borderless and mute-pass
/// tests draw a frame from.
fn app_with_close_dialog_and_palette(palette: Palette) -> App {
    let mut app = with_selection_and_palette(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
        palette,
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    file_edit(&mut app, "cwd", "/srv/app");
    app.update(Msg::Key(KeyPress::Escape));
    assert!(
        app.close_dialog().is_some(),
        "close_offer refused to raise a dialog"
    );
    app
}

/// The same, at [`plain`].
pub fn app_with_close_dialog() -> App {
    app_with_close_dialog_and_palette(plain())
}

/// A frame with the close dialog open, drawn at `width` x `height`: what
/// the box and borderless width tests read the dialog's own margin
/// arithmetic against.
pub fn render_dialog(width: u16, height: u16) -> Buffer {
    render(&app_with_close_dialog(), width, height)
}

/// [`crate::lookout::view::pane::draw_pane`] alone, straight into a fresh buffer at
/// `width` x `height`, with the close dialog raised: what the mute-pass
/// test reads a cell from, since [`render`] draws the whole frame and the
/// config pane does not start at the buffer's own origin there.
pub fn draw_pane_with_dialog(width: u16, height: u16) -> Buffer {
    draw_pane_with_dialog_and_palette(width, height, plain())
}

/// The same, at `palette`: what the `NO_COLOR` mute-pass test reads.
pub fn draw_pane_with_dialog_and_palette(width: u16, height: u16, palette: Palette) -> Buffer {
    let app = app_with_close_dialog_and_palette(palette);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let pane = app.config_pane().expect("the pane is open");
    crate::lookout::view::pane::draw_pane(&app, pane, area, &mut buffer);
    buffer
}
