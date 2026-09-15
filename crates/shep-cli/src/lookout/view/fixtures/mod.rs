//! Fixtures the pane test modules share.

mod bleats;
mod close_dialog;
mod config_rows;
mod dog_pane;
mod flock;
mod host;
mod lambs;
mod palette;
mod refusals;
mod render;
mod secrets;
mod settings;
mod sheep_pane;

pub use self::bleats::{
    app_fixture, bleats_pane_with_a_wide_line, bleats_pane_with_filters, bleats_pane_with_lines,
    bleats_pane_with_long_line, bleats_pane_with_mixed_line_lengths, draw_lines, line, with_feed,
    with_feed_and_palette, with_feed_and_selection,
};
pub use self::close_dialog::{
    app_with_close_dialog, close_dialog_reloading, close_dialog_with, close_dialog_with_live_edit,
    close_dialog_without_a_pid, draw_pane_with_dialog, draw_pane_with_dialog_and_palette,
    render_dialog,
};
pub use self::config_rows::{
    config_pane_draws_a_panel, config_pane_draws_a_tab_row, config_pane_env_rows_for_tests,
    config_pane_field_rows_for_tests, config_pane_panel_focused_on, config_pane_panel_for_tests,
    config_pane_pending_rows_for_tests, config_pane_row_for_tests, config_pane_tab_row_for_tests,
    config_pane_title_band_for_tests,
};
pub use self::dog_pane::{app_in_dog_pane, app_in_dog_pane_with_two_edits, dog_section};
pub use self::flock::{
    acting_app, allowed_app, app_with, app_with_a_built_in_dog_selected_and_control,
    app_with_a_dog, app_with_a_dog_selected_and_control, armed_app,
    armed_app_with_a_filter_and_a_notice, editing_app, filtered_app, filtered_app_of, flock_of,
    full_app, instance_in_fold, sheep_in_fold, sheep_in_fold_with_status, sheep_with,
    with_no_selection, with_selection, with_selection_and_palette,
};
pub use self::host::{sample, with_host, with_host_none};
pub use self::lambs::{
    app_with_lamb_reading_at, lamb_line_of, sheep_with_lambs, with_lamb_reading,
    with_lamb_reading_for,
};
pub use self::palette::{coloured, no_color, plain, plain_dimmed};
pub use self::refusals::{a_refusal, invalid_config};
pub use self::render::{render, render_all, rendered, row_containing, row_starting_with, rows_of};
pub use self::secrets::{
    REVEALED_VALUE, app_armed_to_delete_a_secret, app_revealing, app_revealing_with_control,
    app_typing_a_new_key, app_typing_a_value, app_with_a_maximum_length_secret,
    app_with_a_pushed_secret, app_with_a_pushed_secret_selected_and_control,
    app_with_interleaved_secret_sources, app_with_secrets, app_with_secrets_and_a_provider_row,
    app_with_secrets_and_control, app_with_secrets_and_reads,
    app_with_secrets_on_a_named_tab_and_control, app_with_secrets_read_only, ask_to_reveal,
    render_secrets_gate_shut, render_secrets_with_more_readers_than_fit,
    render_secrets_with_no_roll, render_secrets_with_readers, render_secrets_with_roll_age,
    secrets_model,
};
pub use self::settings::{
    app_in_settings, app_in_settings_at, app_in_settings_on, app_in_settings_on_dog,
    app_in_settings_on_enabled_dog, app_in_settings_with_control, app_in_settings_with_dog_drift,
    app_in_settings_with_shadowed_style, app_in_settings_with_silent_dog, settings_snapshot,
};
pub use self::sheep_pane::{
    app_in_sheep_pane, app_in_sheep_pane_on_a_draining_sheep, app_in_sheep_pane_on_a_stopped_sheep,
    app_in_sheep_pane_read_only, app_in_sheep_pane_with_a_parked_field,
    app_in_sheep_pane_with_control, app_in_sheep_pane_with_env,
    app_in_sheep_pane_with_nothing_parked, app_in_sheep_pane_with_one_edit,
    app_in_sheep_pane_with_two_edits, app_in_sheep_pane_with_two_parked_fields,
    app_with_plain_palette_in_sheep_pane, file_edit, select_env_key, select_field,
    sheep_config_view, type_into_the_open_editor,
};

/// What the last dial said when the ladder ran out, in
/// `crate::lookout::source::LinkError::Unreachable`'s own shape.
///
/// The link panel renders it verbatim, so every test that freezes a
/// dashboard feeds it verbatim rather than inventing a shorter sentence the
/// panel would never see.
pub const FROZEN_WHY: &str = "the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)";
