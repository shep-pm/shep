//! The tests for [`super::edit`]: what a key landing in a config pane files,
//! what it refuses, and what the editor does with the buffer.
//!
//! Split from `edit.rs` by the 1000-line file limit (IR-48), the same shape
//! [`super::config_tests`] and [`super::union_tests`] already have: the tests stay
//! with the code they exercise, in a file named for it.

use serde_json::Value;

use shep_core::config::{AppConfig, ApplyGroup, apply_group};
use shep_core::protocol::SheepConfigView;

use super::fixtures::{bark_pane, filed, filed_impact, money_pane, secret_dog_pane, web};
use super::*;
use super::{Lock, SectionWrite};

#[test]
fn cycling_a_bool_files_a_set_with_the_flipped_value() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("autorestart");
    pane.cycle();
    assert_eq!(filed(&pane, "autorestart"), Some(serde_json::json!(false)));
    assert_eq!(pane.edits().len(), 1, "one key, one entry");
}

/// A round trip is not a change, so the entry goes rather than being
/// filed as the value the sheep already holds: an entry that changes
/// nothing would still be written and still be counted.
#[test]
fn cycling_a_bool_back_to_the_stored_value_unfiles_it() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("autorestart");
    pane.cycle();
    pane.cycle();
    assert_eq!(filed(&pane, "autorestart"), None);
    assert!(pane.edits().is_empty());
}

#[test]
fn space_cycles_a_suggested_field_and_e_still_opens_the_editor() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("kill_signal");
    pane.cycle();
    assert!(
        filed(&pane, "kill_signal").is_some(),
        "space files a suggestion"
    );
    pane.begin_typing();
    assert!(pane.typing().is_some(), "e still opens a free-text editor");
}

/// The set records what each edit costs, per entry, so the close
/// dialog can read the heaviest one without re-deriving anything.
/// `apply_group` is a fact about the field; a write's fate is a fact
/// about the flock, which only the shepherd knows.
#[test]
fn a_filed_edit_carries_the_fields_own_apply_group() {
    for (key, want) in [
        ("autorestart", ApplyGroup::Live),
        ("watch", ApplyGroup::Live),
        ("autostart", ApplyGroup::NextSpawn),
        ("merge_logs", ApplyGroup::NeedsRespawn),
        ("shutdown_with_message", ApplyGroup::NeedsRespawn),
    ] {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key(key);
        pane.cycle();
        assert_eq!(
            pane.cost(key),
            Some(want),
            "the fixture must keep {key} {want:?} or the test means nothing"
        );
        assert_eq!(filed_impact(&pane, key), Some(want), "{key}");
    }
}

/// A dog's section belongs to the dog, so shep records no cost for it.
#[test]
fn a_dogs_filed_edit_carries_no_apply_group() {
    let mut pane = bark_pane();
    pane.move_to_key("history_bytes");
    pane.begin_typing();
    for _ in 0..8 {
        pane.type_backspace();
    }
    for typed in "8192".chars() {
        pane.type_char(typed);
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(filed(&pane, "history_bytes"), Some(serde_json::json!(8192)));
    assert_eq!(filed_impact(&pane, "history_bytes"), None);
}

/// The invariant [`super::super::app::App::close_offer`]'s own walk over
/// [`Edits::iter`] rests on: nothing a keystroke can do files a
/// `Structural` edit, because [`ConfigPane::sheep`] marks those fields
/// not editable and every filing door checks [`ConfigPane::lock`]
/// first.
#[test]
fn no_key_files_an_edit_for_a_structural_field() {
    let structural: Vec<String> = ConfigPane::sheep(web())
        .fields()
        .fields()
        .iter()
        .filter(|field| apply_group(&field.key) == ApplyGroup::Structural)
        .map(|field| field.key.clone())
        .collect();
    assert_eq!(
        structural,
        vec!["instances".to_owned(), "name".to_owned()],
        "the schema must still carry the two Structural fields"
    );
    for key in structural {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key(&key);
        assert_eq!(pane.lock(&key), Some(Lock::Refused), "{key}");
        pane.cycle();
        pane.begin_typing();
        pane.type_char('x');
        assert_eq!(
            pane.apply_typing(),
            None,
            "{key} passes validation; the lock is what stops it filing"
        );
        assert!(pane.edits().is_empty(), "{key} reached the set");
    }
}

/// A string here would be refused by `AppConfig`'s own deserializer.
#[test]
fn typing_into_an_integer_and_applying_files_a_number_not_a_string() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("max_restarts");
    pane.begin_typing();
    let typing = pane.typing().expect("the editor is open");
    assert_eq!(
        typing.buffer.as_str(),
        "32",
        "the editor opens on what is on screen"
    );
    pane.type_backspace();
    pane.type_backspace();
    for c in "40".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(filed(&pane, "max_restarts"), Some(serde_json::json!(40)));
}

/// The sibling of `d`'s own defect: emptying an integer field's buffer
/// used to file `Value::Null` regardless of the field's type, and
/// `max_restarts` is `u32`, not `Option<u32>`, so the daemon would have
/// refused it the same way it refused a bare `d` before that fix. An
/// emptied buffer now files the schema default through the same
/// [`ConfigPane::default_for`] `d` uses, not `null`.
#[test]
fn emptying_an_integer_field_and_applying_files_its_default_not_null() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("max_restarts");
    pane.begin_typing();
    for _ in 0..10 {
        pane.type_backspace();
    }
    assert_eq!(pane.typing().expect("still open").buffer.as_str(), "");
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(filed(&pane, "max_restarts"), Some(serde_json::json!(16)));
}

/// An `Option<T>` field with no schema default still unsets to `null`
/// through the emptied-buffer door: [`ConfigPane::default_for`] falls
/// back to [`Value::Null`] when [`Field::default_value`] is `None`,
/// which is exactly `cwd`'s case, so this path is unchanged for it.
#[test]
fn emptying_a_field_with_no_schema_default_still_unsets_to_null() {
    let config = AppConfig {
        name: "web".into(),
        cwd: Some("/srv/web".into()),
        ..AppConfig::default()
    };
    let mut pane = ConfigPane::sheep(SheepConfigView::new(config, vec!["cwd".into()], vec![]));
    pane.move_to_key("cwd");
    pane.begin_typing();
    assert_eq!(
        pane.typing().expect("the editor is open").buffer.as_str(),
        "/srv/web",
        "the editor opens on what is on screen"
    );
    for _ in 0..8 {
        pane.type_backspace();
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(filed(&pane, "cwd"), Some(serde_json::Value::Null));
}

/// The request names one key and one JSON value; the daemon
/// deserializes that value into the field it names, so an integer
/// field handed `"40"` is refused rather than set.
#[test]
fn an_edit_carries_the_key_and_the_typed_value_and_nothing_else() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("max_restarts");
    pane.begin_typing();
    pane.type_backspace();
    pane.type_backspace();
    for c in "40".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    let writes = pane.close().into_writes();
    let [PaneEdit::Set { key, value }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(key, "max_restarts");
    assert_eq!(value.as_value(), &serde_json::json!(40));
    assert!(
        !matches!(value.as_value(), serde_json::Value::String(_)),
        "an integer field must not travel as a string"
    );
}

/// `shep-kelpie`'s `budget.per_item_usd`: a money cap the pane drew
/// read-only, because `kind_of` had no arm for `type: number` and the field
/// fell through to [`FieldKind::Opaque`]. The editor seeds with what the row
/// shows, as it does for an integer.
#[test]
fn typing_a_decimal_into_a_number_field_files_a_float() {
    let mut pane = money_pane("per_item_usd = 12.5\n");
    pane.move_to_key("per_item_usd");
    pane.begin_typing();
    assert_eq!(
        pane.typing().expect("the editor is open").buffer.as_str(),
        "12.5",
        "the editor opens on what is on screen"
    );
    for _ in 0..4 {
        pane.type_backspace();
    }
    for c in "2.5".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(filed(&pane, "per_item_usd"), Some(serde_json::json!(2.5)));
}

/// A whole number typed into a decimal field is filed as a float, so the
/// dog reads the type its own schema names rather than the spelling an
/// operator happened to type.
#[test]
fn a_whole_number_typed_into_a_number_field_is_filed_as_a_float() {
    let mut pane = money_pane("per_item_usd = 12.5\n");
    pane.move_to_key("per_item_usd");
    pane.begin_typing();
    for _ in 0..4 {
        pane.type_backspace();
    }
    for c in "10".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None, "10 is a number");
    assert_eq!(filed(&pane, "per_item_usd"), Some(serde_json::json!(10.0)));
}

/// The seed is the stored value as the row shows it, which for a whole
/// number stored in `dogs.toml` is bare: an operator who only wants to add
/// cents starts from `10`, not from `10.0`.
#[test]
fn a_number_field_seeds_its_editor_with_the_stored_value_as_shown() {
    let mut pane = money_pane("per_item_usd = 10\n");
    pane.move_to_key("per_item_usd");
    pane.begin_typing();
    assert_eq!(
        pane.typing().expect("the editor is open").buffer.as_str(),
        "10"
    );
}

/// The other half of the gate: `f64::from_str` takes `NaN` and `inf`, and
/// `Value::from(f64)` answers a non-finite float with `null`, which a dog's
/// section writer reads as "remove this key". Refusing it is what keeps an
/// operator typing `NaN` from losing the key.
#[test]
fn a_number_field_refuses_a_word_and_a_non_finite_number_and_files_nothing() {
    for typed in ["abc", "NaN", "inf"] {
        let mut pane = money_pane("per_item_usd = 12.5\n");
        pane.move_to_key("per_item_usd");
        pane.begin_typing();
        for _ in 0..4 {
            pane.type_backspace();
        }
        for c in typed.chars() {
            pane.type_char(c);
        }
        let refused = pane.apply_typing().expect("refused");
        assert_eq!(
            refused.text,
            format!("per_item_usd is \"{typed}\", which is not a number shep accepts"),
            "{typed}"
        );
        assert_eq!(filed(&pane, "per_item_usd"), None, "{typed} filed nothing");
        assert!(
            pane.typing().is_some(),
            "{typed}: the editor stays open on what was typed"
        );
    }
}

/// A numeric `enum` is no closed set this pane can draw as a cycle widget:
/// `closed_set` reads strings, so `{"type": "number", "enum": [1.5, 2.5]}` is a
/// typed field. This pane is the only gate a dog's section has, so a `3.5`
/// refused nowhere reaches `dogs.toml` unread.
#[test]
fn a_number_field_refuses_a_value_outside_the_numeric_enum_it_published() {
    let schema = serde_json::json!({
        "properties": { "ratio": { "type": "number", "enum": [1.5, 2.5] } },
    });
    for typed in ["1.5", "2.5"] {
        let mut pane = ConfigPane::dog(
            "kelpie".into(),
            None,
            schema.clone(),
            "ratio = 1.5\n".into(),
            Vec::new(),
        );
        pane.move_to_key("ratio");
        pane.begin_typing();
        for _ in 0..3 {
            pane.type_backspace();
        }
        for c in typed.chars() {
            pane.type_char(c);
        }
        assert_eq!(
            pane.apply_typing(),
            None,
            "{typed} is the value the schema wrote"
        );
        // The pane records a change, not a re-statement of what the section
        // already holds, so filing is only asserted for the value that moves.
        assert_eq!(
            filed(&pane, "ratio"),
            (typed != "1.5").then(|| serde_json::json!(typed.parse::<f64>().unwrap())),
            "{typed}"
        );
    }
    let mut pane = ConfigPane::dog(
        "kelpie".into(),
        None,
        schema,
        "ratio = 1.5\n".into(),
        Vec::new(),
    );
    pane.move_to_key("ratio");
    pane.begin_typing();
    for _ in 0..3 {
        pane.type_backspace();
    }
    for c in "3.5".chars() {
        pane.type_char(c);
    }
    let refused = pane.apply_typing().expect("3.5 is not in the set");
    assert_eq!(
        refused.text,
        "ratio is \"3.5\"; ratio takes one of 1.5, 2.5"
    );
    assert_eq!(filed(&pane, "ratio"), None, "3.5 filed nothing");
}

/// `exclusiveMinimum: 0` is how `shep-kelpie` says "a cap, and not a cap of
/// nothing", and this pane is the only gate a dog's section has.
#[test]
fn a_number_field_refuses_zero_under_its_exclusive_floor_and_takes_the_next() {
    let mut pane = money_pane("per_item_usd = 12.5\n");
    pane.move_to_key("per_item_usd");
    pane.begin_typing();
    for _ in 0..4 {
        pane.type_backspace();
    }
    pane.type_char('0');
    let refused = pane.apply_typing().expect("0 is not above a floor of 0");
    assert_eq!(
        refused.text,
        "per_item_usd is \"0\"; per_item_usd must be above 0"
    );
    assert_eq!(filed(&pane, "per_item_usd"), None);
    assert!(pane.typing().is_some(), "the editor stays open on the 0");
    for c in ".01".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None, "0.01 is above the floor");
    assert_eq!(
        filed(&pane, "per_item_usd"),
        Some(serde_json::json!(0.01)),
        "and it is filed"
    );
}

/// An edit carries the key and the JSON value, and the daemon
/// deserializes that value into the field it names: a decimal field handed
/// `"2.5"` is refused rather than set.
#[test]
fn a_number_edit_carries_a_float_and_not_the_text_typed() {
    let mut pane = money_pane("per_item_usd = 12.5\n");
    pane.move_to_key("per_item_usd");
    pane.begin_typing();
    for _ in 0..4 {
        pane.type_backspace();
    }
    for c in "2.5".chars() {
        pane.type_char(c);
    }
    assert_eq!(pane.apply_typing(), None);
    let writes = pane.close().into_writes();
    let [PaneEdit::Set { key, value }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(key, "per_item_usd");
    assert_eq!(value.as_value(), &serde_json::json!(2.5));
    assert!(
        !matches!(value.as_value(), serde_json::Value::String(_)),
        "a decimal field must not travel as a string"
    );
}

/// The set is what closing hands out, and closing empties it: a set
/// that has been written is not still pending.
#[test]
fn closing_hands_out_every_filed_edit_and_leaves_the_pane_empty() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("autorestart");
    pane.cycle();
    pane.move_to_key("autostart");
    pane.cycle();
    assert_eq!(pane.edits().len(), 2);
    let writes = pane.close().into_writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(pane.edits().is_empty(), "the pane keeps nothing back");
    assert!(
        pane.close().into_writes().is_empty(),
        "a second close writes nothing twice"
    );
}

#[test]
fn a_unit_field_files_the_buffer_and_never_a_resolved_form() {
    for typed in ["64", "512M"] {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("max_memory");
        pane.begin_typing();
        for c in typed.chars() {
            pane.type_char(c);
        }
        assert_eq!(pane.apply_typing(), None, "{typed} is a size shep takes");
        assert_eq!(
            filed(&pane, "max_memory"),
            Some(serde_json::json!(typed)),
            "{typed}"
        );
    }
}

/// The other half of the test above. The daemon is the backstop for a
/// sheep's own write and does not exist for a dog, so this gate is the
/// only thing between the buffer and `dogs.toml`.
#[test]
fn a_unit_field_refuses_a_buffer_its_own_grammar_does_not_take() {
    let mut pane = ConfigPane::sheep(web());
    pane.move_to_key("max_memory");
    pane.begin_typing();
    for c in "banana".chars() {
        pane.type_char(c);
    }
    let refused = pane.apply_typing().expect("a size shep cannot read");
    assert_eq!(
        refused.text,
        "max_memory is \"banana\", which is not a size shep accepts; \
         try 512M, 2G, a bare number is bytes"
    );
    assert_eq!(filed(&pane, "max_memory"), None, "nothing was filed");
    assert!(
        pane.typing().is_some(),
        "the editor stays open on what was typed"
    );
}

/// The one editor a secret can be typed into, and `secret_dog_pane` is
/// the only fixture that reaches it. Seeded empty rather than with what
/// the section holds: the screen draws `<set>` for a secret and the
/// pane must not hand the old credential back for a backspace to edit.
#[test]
fn a_dogs_secret_field_seeds_its_editor_empty_and_files_what_was_typed() {
    let mut pane = secret_dog_pane();
    pane.move_to_key("webhook");
    pane.begin_typing();
    assert_eq!(
        pane.typing().expect("the editor is open").buffer.as_str(),
        "",
        "a secret seeds empty, however much the section holds"
    );
    for typed in "https://hook/NEW".chars() {
        pane.type_char(typed);
    }
    assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
    assert_eq!(
        filed(&pane, "webhook"),
        Some(serde_json::json!("https://hook/NEW"))
    );
    let debug = format!("{pane:?}");
    assert!(!debug.contains("OLD"), "{debug}");
    assert!(!debug.contains("NEW"), "{debug}");
}

/// A dog's write patches its section's own TOML text rather than going
/// through `Request::SetSheepField`'s deserializer, so `null` already
/// means "remove this key" there, not a value refused for the wrong
/// type. `d` must keep filing `Value::Null` for a dog even when its
/// schema names a non-null default, or it would hard-code the default
/// into the section instead of restoring it.
#[test]
fn d_on_a_dog_field_files_null_even_with_a_schema_default() {
    let schema = serde_json::json!({
        "properties": {
            "merge_logs": { "type": "boolean", "default": true },
        },
    });
    let mut pane = ConfigPane::dog(
        "bark".into(),
        None,
        schema,
        "merge_logs = false\n".into(),
        Vec::new(),
    );
    pane.move_to_key("merge_logs");
    pane.file_default();
    assert_eq!(
        filed(&pane, "merge_logs"),
        Some(Value::Null),
        "a dog restores by removing the key, not by filing its default"
    );
    let SectionWrite::Written(section) = pane.edited_section_with(pane.edits()) else {
        panic!("the fixture section parses, and the removal lands in it");
    };
    let out = section.as_str();
    assert!(!out.contains("merge_logs"), "{out}");
}

/// The buffer is what the operator is halfway through typing, and on
/// the env screen that is the secret itself (IR-41).
#[test]
fn debug_names_no_value_on_a_pane_typing() {
    let typing = PaneTyping {
        key: "cwd".into(),
        buffer: "/home/ada/secret-project".into(),
    };
    assert_eq!(
        format!("{typing:?}"),
        r#"PaneTyping { key: "cwd", buffer: <24 chars> }"#
    );
}
