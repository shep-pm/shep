//! Bounded reads of a config pane's own rows, never a search over the frame.

use crate::lookout::app::App;

use super::palette::plain;
use super::render::rendered;

/// The active group's own field rows, as their key names: a bounded slice
/// of the config pane's state rather than a search over the rendered
/// frame, which is what keeps a test on this from passing off a match in
/// the legend or another section.
///
/// # Panics
///
/// Panics if the pane is closed, which is a fixture bug rather than a
/// failure the test is about.
#[track_caller]
pub fn config_pane_field_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.rows()
        .into_iter()
        .filter_map(|row| match row {
            crate::lookout::pane::PaneRow::Field(index) => {
                Some(pane.fields().fields()[index].key.clone())
            }
            crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv => None,
        })
        .collect()
}

/// The active group's own env rows: one entry per env key, then
/// `+ add a key`. Searched below the `env` section header and nowhere else,
/// so a test on this cannot pass off a match from the field list above it:
/// the keys and the field names share one namespace on screen, and a sheep
/// has fields called `user` and `env`.
///
/// Takes a `ConfigPane` directly rather than an `App`, since some of this
/// pane's own tests build one without a dashboard around it.
///
/// # Panics
///
/// Panics if it draws no row for a key or for `+ add a key`, which is a
/// fixture bug rather than a failure the test is about.
#[track_caller]
pub fn config_pane_env_rows_for_tests(pane: &crate::lookout::pane::ConfigPane) -> Vec<String> {
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    let rendered_lines: Vec<String> = lines.iter().map(rendered).collect();
    // The `env` section header is the bound. Everything above it is a field
    // row or chrome, and a prefix match over the whole frame would hand back
    // the `user` field's row for an env key named `user`.
    //
    // The header sits at column 2 and every row under it at column 3, which
    // is what tells the header apart from a field called `env`. It is not
    // matched whole because the explanation panel is merged to the right of
    // it at this width, so the line carries the panel's own row too.
    let header = rendered_lines
        .iter()
        .position(|line| {
            line.strip_prefix("  env")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
        .expect("the pane draws an env section header");
    let env_rows = &rendered_lines[header + 1..];
    let mut rows: Vec<String> = pane
        .env_key_names()
        .iter()
        .map(|name| {
            env_rows
                .iter()
                .find(|line| {
                    line.trim_start_matches(['>', ' '])
                        .starts_with(name.as_str())
                })
                .unwrap_or_else(|| panic!("no row for env key {name}"))
                .clone()
        })
        .collect();
    rows.push(
        env_rows
            .iter()
            .find(|line| line.contains("add a key"))
            .expect("the pane draws a + add a key row")
            .clone(),
    );
    rows
}

/// Every filed edit's own key, as [`config_pane_field_rows_for_tests`] does
/// for the active group's own fields: a bounded slice of the pane's own
/// edit set, never a search over the rendered frame.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_pending_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.edits()
        .iter()
        .map(|(key, _)| match key {
            crate::lookout::edits::EditKey::Field(name) => name.clone(),
            crate::lookout::edits::EditKey::Env(name) => format!("env.{name}"),
        })
        .collect()
}

/// The one rendered line naming `key`, wherever it draws: the active
/// group's own field row, or the pending-edits section when `key` belongs
/// to a group not on screen. Bounded to that single row by stripping the
/// mark, lock and flag columns and requiring what is left to start with
/// `key`, which is what keeps this from matching a longer key or the
/// legend.
///
/// # Panics
///
/// Panics if the pane is closed or draws no row for `key`.
#[track_caller]
pub fn config_pane_row_for_tests(app: &App, key: &str) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| {
            line.trim_start_matches(['>', ' ', '=', '~', '!', '*'])
                .starts_with(key)
        })
        .unwrap_or_else(|| panic!("no row for {key}"))
}

/// The pane's own title band, alone: line zero of the rendered frame, the
/// one line [`crate::lookout::view::pane::pane_lines`] ever puts the edit
/// count in.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_title_band_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    rendered(&lines[0])
}

/// The pane's own tab row, alone: the one line naming every group in
/// [`shep_core::config::GROUP_ORDER`], found by its own `tab next group`
/// phrase rather than by a fixed index, so a chrome line gained or lost
/// above it does not silently move which row this reads.
///
/// # Panics
///
/// Panics if the pane is closed or draws no tab row at `width`.
#[track_caller]
pub fn config_pane_tab_row_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| line.contains("tab next group"))
        .expect("the pane draws a tab row at this width")
}

/// Whether the pane draws a tab row at `width`, without panicking when it
/// does not: the non-panicking half of [`config_pane_tab_row_for_tests`],
/// for a caller (a dog pane, which has no groups) asserting the row's
/// absence rather than reading its content.
pub fn config_pane_draws_a_tab_row(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("tab next group"))
}

/// Whether the pane's own merged frame includes the explanation panel at
/// `width`: the wiring question `pane_lines` answers by width alone, which
/// [`config_pane_panel_for_tests`] cannot: that helper calls `panel_lines`
/// directly, and `panel_lines` draws unconditionally, carrying none of
/// `pane_lines`' own decision about whether the terminal is wide enough to
/// show it at all.
pub fn config_pane_draws_a_panel(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("FOCUSED"))
}

/// The explanation panel for whichever field the pane's own cursor is on,
/// as plain rows: what [`crate::lookout::view::pane::panel_lines`] draws,
/// styles dropped, at the app's own palette.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_panel_for_tests(app: &App, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    crate::lookout::view::pane::panel_lines(pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}

/// The explanation panel for the field named `key`, regardless of where the
/// pane's own cursor sits: a bounded look at one field's own panel content
/// rather than a walk that would first have to move the cursor there.
///
/// # Panics
///
/// Panics if the pane is closed or has no field named `key`.
#[track_caller]
pub fn config_pane_panel_focused_on(app: &App, key: &str, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    let field = pane
        .fields()
        .by_key(key)
        .unwrap_or_else(|| panic!("no field named {key}"));
    crate::lookout::view::pane::panel_for_field(field, pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}
