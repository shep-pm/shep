//! A dog pane's arrays and maps of tables, driven by key presses: how they
//! open, what the keys that edit do on them, and what a refresh keeps.

use super::*;
use crate::lookout::app::testing::*;
use crate::lookout::pane::EntryBody;
use crate::lookout::view::fixtures::{entries_dog_section, render, rows_of};

pub(super) fn pane(app: &App) -> &ConfigPane {
    app.config_pane().expect("the pane is open")
}

/// Presses `→` on the row whose key is `key`, walking down to it first.
pub(super) fn open(app: &mut App, key: &str) {
    app.update(Msg::Key(KeyPress::SelectFirst));
    while !matches!(
        pane(app).cursor(),
        Some(PaneRow::Field(index)) if pane(app).fields().fields()[index].key == key
    ) {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app.update(Msg::Key(KeyPress::TabNext));
}

/// Opens `clients` and every entry under it.
pub(super) fn open_every_client(app: &mut App) {
    open(app, "clients");
    loop {
        let pane = pane(app);
        let rows = pane.rows();
        let closed = rows.iter().position(|row| match row {
            PaneRow::Entry(index) => pane.entry_row(*index).is_some_and(|entry| {
                matches!(&entry.body, EntryBody::Node { path, .. } if !pane.open_tables().contains(path))
            }),
            _ => false,
        });
        let Some(at) = closed else {
            return;
        };
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..at {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        app.update(Msg::Key(KeyPress::TabNext));
    }
}

#[test]
fn entries_and_entry_rows_start_closed() {
    let app = fixtures::app_in_dog_pane_with_entries();
    let pane = pane(&app);
    assert_eq!(pane.rows().len(), 3, "clients, models and retries");
    assert!(pane.open_tables().is_empty(), "{:?}", pane.open_tables());
}

#[test]
fn a_frame_with_every_client_open_counts_them_and_never_draws_a_credential() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    let text = rows_of(&render(&app, 160, 48)).join("\n");
    assert!(text.contains("3 entries"), "{text}");
    assert!(text.contains("<set>"), "{text}");
    for secret in ["hunter2", "hunter3", "hunter4"] {
        assert!(
            !text.contains(secret),
            "{secret} reached the frame:\n{text}"
        );
    }
}

/// Walks the cursor onto the row keyed `key`, which the open entries have
/// drawn.
#[track_caller]
pub(super) fn select(app: &mut App, key: &str) {
    crate::lookout::view::fixtures::select_node(app, key);
}

/// Replaces what the editor `Enter` opens on `key` was seeded with by
/// `text`, and applies it.
#[track_caller]
pub(super) fn type_into(app: &mut App, key: &str, text: &str) {
    select(app, key);
    app.update(Msg::Key(KeyPress::Confirm));
    for _ in 0..16 {
        app.update(Msg::Key(KeyPress::TextBackspace));
    }
    for typed in text.chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// `Esc` on the pane, with the re-read it sends answered by `section`, and
/// the one write that follows.
#[track_caller]
pub(super) fn close_with(app: &mut App, section: &str) -> String {
    let sent = Sent::DogSection {
        name: "paddock".into(),
    };
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::Send(sent.clone()),
        "esc reads the section again before writing"
    );
    let effect = app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection {
            toml: section.to_owned().into(),
        }),
    });
    let batch = wire_batch(effect);
    let [Sent::SetDogSection { name, toml, .. }] = batch.as_slice() else {
        panic!("closing the pane sends the section once: {batch:?}");
    };
    assert_eq!(name, "paddock");
    toml.as_str().to_owned()
}

/// A list, a map and a count under an entry stay read-only: `e`, space and
/// `d` on one file nothing and open no editor.
#[test]
fn e_space_and_d_on_a_count_under_an_entry_file_nothing() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    select(&mut app, "clients[0].peers");
    assert_eq!(
        pane(&app).cursor_lock().map(|(_, lock)| lock),
        Some(Lock::NoWidget)
    );
    for key in [
        KeyPress::Edit,
        KeyPress::Cycle,
        KeyPress::Remove,
        KeyPress::Confirm,
    ] {
        assert_eq!(app.update(Msg::Key(key)), Effect::None, "{key:?}");
        let pane = pane(&app);
        assert!(pane.edits().is_empty(), "{key:?} filed something");
        assert!(pane.typing().is_none(), "{key:?} opened an editor");
    }
}

#[test]
fn right_on_an_entries_field_inside_an_entry_opens_nothing() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    let open = pane(&app).open_tables().clone();
    let rows = pane(&app).rows().len();
    select(&mut app, "clients[0].peers");
    app.update(Msg::Key(KeyPress::TabNext));
    assert_eq!(pane(&app).open_tables(), &open);
    assert_eq!(pane(&app).rows().len(), rows);
}

#[test]
fn naming_the_second_client_changes_that_one_line_of_the_section() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[1].name", "beta");
    let written = close_with(&mut app, &entries_dog_section());
    let expected = entries_dog_section().replace(
        "[[clients]]\nid = \"b-2\"\nkey = \"hunter3\"\n",
        "[[clients]]\nid = \"b-2\"\nkey = \"hunter3\"\nname = \"beta\"\n",
    );
    assert_eq!(written, expected);
}

#[test]
fn edits_to_two_entries_land_in_one_write() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[0].timeout", "45");
    type_into(&mut app, "clients[2].name", "gamma");
    let written = close_with(&mut app, &entries_dog_section());
    let expected = entries_dog_section()
        .replace("timeout = 30\n", "timeout = 45\n")
        .replace(
            "[[clients]]\nkey = \"hunter4\"\n",
            "[[clients]]\nkey = \"hunter4\"\nname = \"gamma\"\n",
        );
    assert_eq!(written, expected);
}

#[test]
fn u_drops_the_newest_edit_inside_an_entry_and_the_edit_count_follows() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[0].timeout", "45");
    type_into(&mut app, "clients[1].name", "beta");
    // The count the title band prints is this set's length.
    assert_eq!(pane(&app).edits().len(), 2);
    app.update(Msg::Key(KeyPress::Undo));
    assert_eq!(pane(&app).edits().len(), 1);
    assert!(
        pane(&app)
            .edits()
            .get(&EditKey::Field("clients[1].name".into()))
            .is_none()
    );
    assert!(
        pane(&app)
            .edits()
            .get(&EditKey::Field("clients[0].timeout".into()))
            .is_some()
    );
}

#[test]
fn a_secret_key_in_an_entry_reads_set_opens_empty_and_never_labels_its_entry() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    select(&mut app, "clients[0].key");
    let drawn = rows_of(&render(&app, 160, 48)).join("\n");
    assert!(drawn.contains("<set>"), "{drawn}");
    app.update(Msg::Key(KeyPress::Confirm));
    let typing = pane(&app).typing().expect("an editor opened");
    assert_eq!(typing.buffer.as_str(), "", "a secret opens empty");
    for typed in "hunter9".chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
    let labels = rows_of(&render(&app, 160, 48)).join("\n");
    for label_row in labels.lines().filter(|row| row.contains("entry")) {
        assert!(!label_row.contains("hunter"), "{label_row}");
    }
}

#[test]
fn pending_entry_edits_mask_the_old_and_new_credentials() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[0].key", "hunter9");
    let text = rows_of(&render(&app, 160, 80)).join("\n");
    assert!(text.contains("pending edits"), "{text}");
    assert!(text.contains("<set> -> <set>"), "{text}");
    for secret in ["hunter2", "hunter3", "hunter4", "hunter9"] {
        assert!(!text.contains(secret), "a credential reached the frame");
    }
}

#[test]
fn a_refresh_with_an_entry_open_keeps_the_filed_edits_the_open_set_and_the_cursor() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[1].name", "beta");
    let open = pane(&app).open_tables().clone();
    let cursor = pane(&app).cursor();
    let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
        panic!("r re-reads the section");
    };
    app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection {
            toml: entries_dog_section().into(),
        }),
    });
    let pane = pane(&app);
    assert_eq!(pane.open_tables(), &open);
    assert_eq!(pane.cursor(), cursor);
    assert!(
        pane.edits()
            .get(&EditKey::Field("clients[1].name".into()))
            .is_some()
    );
}

#[test]
fn a_refresh_keeps_an_open_map_entry_open_by_key_after_a_key_above_it_is_removed() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open(&mut app, "models");
    // `large` is first, `small` second.
    app.update(Msg::Key(KeyPress::SelectDown));
    app.update(Msg::Key(KeyPress::SelectDown));
    app.update(Msg::Key(KeyPress::TabNext));
    assert!(pane(&app).open_tables().contains("models[\"small\"]"));
    let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
        panic!("r re-reads the section");
    };
    let without_large =
        entries_dog_section().replace("[models.large]\nprovider = \"remote\"\n\n", "");
    app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection {
            toml: without_large.into(),
        }),
    });
    let open: Vec<&String> = pane(&app).open_tables().iter().collect();
    assert_eq!(open, ["models", "models[\"small\"]"]);
}

#[test]
fn a_refresh_drops_an_open_array_entry_past_the_new_end() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    assert!(pane(&app).open_tables().contains("clients[2]"));
    let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
        panic!("r re-reads the section");
    };
    let two = entries_dog_section().replace("[[clients]]\nkey = \"hunter4\"\n\n", "");
    app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection { toml: two.into() }),
    });
    let open = pane(&app).open_tables();
    assert!(open.contains("clients[1]"), "{open:?}");
    assert!(!open.contains("clients[2]"), "{open:?}");
}

pub(super) const ALPHA: &str = "[[clients]]\nname = \"alpha\"\nkey = \"hunter2\"\ntimeout = 30\n\n";

/// `r` answered with `section`.
#[track_caller]
pub(super) fn refresh_with(app: &mut App, section: String) {
    let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
        panic!("r re-reads the section");
    };
    app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection {
            toml: section.into(),
        }),
    });
}

/// `clients[1]` was `b-2`. Once the first client is gone it is the entry that
/// was `clients[2]`, and an edit filed for `b-2` must not be written there.
#[test]
fn a_refresh_that_moves_an_array_entry_drops_the_edit_filed_under_its_old_place() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[1].name", "beta");
    refresh_with(&mut app, entries_dog_section().replace(ALPHA, ""));
    assert!(pane(&app).edits().is_empty());
    assert_eq!(
        app.notice().map(ToString::to_string).as_deref(),
        Some("paddock: 1 edit under an entry that moved in the file was dropped")
    );
}

#[test]
fn esc_that_finds_an_array_entry_moved_holds_the_pane_without_the_edit() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[1].name", "beta");
    type_into(&mut app, "retries", "5");
    let moved = entries_dog_section().replace(ALPHA, "");
    let sent = Sent::DogSection {
        name: "paddock".into(),
    };
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::Send(sent.clone())
    );
    let effect = app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogSection {
            toml: moved.clone().into(),
        }),
    });
    assert_eq!(effect, Effect::None);
    assert_eq!(pane(&app).edits().len(), 1, "only the entry's edit is lost");
    assert_eq!(
        app.notice().map(ToString::to_string).as_deref(),
        Some(
            "paddock: its section in dogs.toml changed while this pane was open, so nothing \
             was written; esc writes your edits over the new values; \
             1 edit under an entry that moved in the file was dropped"
        )
    );
    let written = close_with(&mut app, &moved);
    assert_eq!(written, moved.replace("retries = 3", "retries = 5"));
}

#[test]
fn a_refresh_that_leaves_the_entry_where_it_was_keeps_its_edit() {
    let mut app = fixtures::app_in_dog_pane_with_entries();
    open_every_client(&mut app);
    type_into(&mut app, "clients[1].name", "beta");
    let without_last = entries_dog_section().replace("[[clients]]\nkey = \"hunter4\"\n\n", "");
    refresh_with(&mut app, without_last);
    assert!(
        pane(&app)
            .edits()
            .get(&EditKey::Field("clients[1].name".into()))
            .is_some()
    );
}
