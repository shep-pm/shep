use serde_json::Value;
use serde_json::json;

use super::*;

/// The design's own call, and the answer it is meant to produce.
#[test]
fn the_builder_call_answers_the_sections_the_design_draws() {
    let pane = Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB"), ("RAM", "41 / 63 GiB")])
        .refresh_on(["lease.changed"])
        .table(
            "models",
            ["MODEL", "STATE", "FOOTPRINT"],
            [
                Row::new("qwen3-32b", ["qwen3-32b", "loaded", "19.8 GiB"]),
                Row::new("strata-7b", ["strata-7b", "idle", "6.1 GiB"]),
            ],
        )
        .row_action("revoke", "Revoke lease", Destructive::Yes)
        .action("pause", "Pause the board", Destructive::No);

    assert_eq!(
        serde_json::from_str::<Value>(&pane.body().unwrap()).unwrap(),
        json!({
            "sections": [
                {
                    "kind": "fields",
                    "id": "host",
                    "title": "host",
                    "refresh_on": ["lease.changed"],
                    "fields": [
                        {"label": "VRAM", "value": "20 / 24 GiB"},
                        {"label": "RAM", "value": "41 / 63 GiB"}
                    ]
                },
                {
                    "kind": "table",
                    "id": "models",
                    "title": "models",
                    "refresh_on": [],
                    "columns": ["MODEL", "STATE", "FOOTPRINT"],
                    "rows": [
                        {"key": "qwen3-32b", "cells": ["qwen3-32b", "loaded", "19.8 GiB"]},
                        {"key": "strata-7b", "cells": ["strata-7b", "idle", "6.1 GiB"]}
                    ],
                    "row_actions": [
                        {"action": "revoke", "label": "Revoke lease", "destructive": true}
                    ]
                }
            ],
            "actions": [
                {"action": "pause", "label": "Pause the board", "destructive": false}
            ]
        }),
        "{}",
        pane.body().unwrap()
    );
}

/// The builder and the reader are the same module, so a dog's own answer reads
/// back as the pane it was built from.
#[test]
fn a_built_pane_reads_back_through_its_own_body() {
    let pane = Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB")])
        .refresh_on(["lease.changed", "model.loaded"])
        .table(
            "models",
            ["MODEL", "STATE"],
            [Row::new("qwen3-32b", ["qwen3-32b", "loaded"])],
        )
        .row_action("revoke", "Revoke lease", Destructive::Yes)
        .action("pause", "Pause the board", Destructive::No);

    let read = Pane::read(&pane.body().unwrap()).unwrap();

    assert_eq!(read, pane);
    assert_eq!(read.skipped, 0);
}

/// A builder call with nothing to apply it to says nothing rather than
/// inventing a section.
#[test]
fn a_refresh_or_row_action_with_no_section_before_it_adds_nothing() {
    let pane = Pane::new()
        .refresh_on(["lease.changed"])
        .row_action("revoke", "Revoke lease", Destructive::Yes)
        .fields("host", [("VRAM", "20 GiB")])
        .row_action("rule", "Rule", Destructive::No);

    assert_eq!(pane.actions, Vec::<PaneAction>::new());
    assert_eq!(pane.sections.len(), 1);
    match &pane.sections[0] {
        Section::Fields(section) => {
            assert!(section.refresh_on.is_empty(), "the call before any section");
        }
        Section::Table(section) => panic!("no table was added: {:?}", section.id),
    }
}

/// A row too short or too long for its columns, a repeated section id and a
/// repeated row key: each is refused, and each names what the dog's author has
/// to fix rather than only saying the answer is bad.
#[test]
fn a_pane_with_a_bad_row_id_or_key_is_refused_by_name() {
    let short = Pane::new()
        .table("models", ["MODEL", "STATE"], [Row::new("qwen", ["loaded"])])
        .body();
    let long = Pane::new()
        .table(
            "models",
            ["MODEL"],
            [Row::new("qwen", ["qwen3-32b", "loaded"])],
        )
        .body();
    let one_column = Pane::new()
        .table("models", ["MODEL"], [Row::new("qwen", ["qwen3-32b"])])
        .body();
    let repeated_id = Pane::new()
        .fields("host", [("VRAM", "20 GiB")])
        .fields("host", [("RAM", "41 GiB")])
        .body();
    let repeated_key = Pane::new()
        .table(
            "models",
            ["MODEL"],
            [Row::new("qwen", ["loaded"]), Row::new("qwen", ["idle"])],
        )
        .body();

    assert_eq!(
        short.unwrap_err(),
        "section `models` has 2 columns but its row `qwen` holds 1 cell"
    );
    assert_eq!(
        long.unwrap_err(),
        "section `models` has 1 column but its row `qwen` holds 2 cells"
    );
    assert!(one_column.is_ok(), "a row with one cell per column");
    assert_eq!(repeated_id.unwrap_err(), "two sections share the id `host`");
    assert_eq!(
        repeated_key.unwrap_err(),
        "section `models` uses the row key `qwen` twice"
    );
}

/// The writer refuses what the reader refuses, in the same words, so a dog
/// never answers with a pane its own reader would reject. Each answer here is
/// the JSON a dog written in another script would send.
#[test]
fn the_builder_refuses_the_same_answer_the_reader_refuses_in_the_same_words() {
    let cases = [
        (
            Pane::new().table("models", ["MODEL", "STATE"], [Row::new("qwen", ["loaded"])]),
            json!({"sections": [{"kind": "table", "id": "models", "title": "models",
                                 "columns": ["MODEL", "STATE"],
                                 "rows": [{"key": "qwen", "cells": ["loaded"]}]}]}),
            "section `models` has 2 columns but its row `qwen` holds 1 cell",
        ),
        (
            Pane::new()
                .fields("host", [("VRAM", "20 GiB")])
                .fields("host", [("RAM", "41 GiB")]),
            json!({"sections": [{"kind": "fields", "id": "host", "title": "host", "fields": []},
                                {"kind": "fields", "id": "host", "title": "host", "fields": []}]}),
            "two sections share the id `host`",
        ),
        (
            Pane::new().table(
                "models",
                ["MODEL"],
                [Row::new("qwen", ["loaded"]), Row::new("qwen", ["idle"])],
            ),
            json!({"sections": [{"kind": "table", "id": "models", "title": "models",
                                 "columns": ["MODEL"],
                                 "rows": [{"key": "qwen", "cells": ["loaded"]},
                                          {"key": "qwen", "cells": ["idle"]}]}]}),
            "section `models` uses the row key `qwen` twice",
        ),
    ];

    for (built, answer, fault) in cases {
        let answer = answer.to_string();
        assert_eq!(built.body().unwrap_err(), fault);
        assert!(Pane::read(&answer).unwrap_err().contains(fault), "{answer}");
    }
}

#[test]
fn an_answer_that_is_not_a_pane_is_unreadable() {
    for body in [
        "not json at all",
        "",
        r#"{"sections": {"host": []}}"#,
        r#"{"sections": [{"id": "host", "fields": []}]}"#,
        r#"{"sections": [{"kind": "table", "id": "models", "columns": ["MODEL", "STATE"],
             "rows": [{"key": "qwen", "cells": ["loaded"]}]}]}"#,
        r#"{"sections": [{"kind": "fields", "id": "host", "fields": []},
             {"kind": "fields", "id": "host", "fields": []}]}"#,
        r#"{"sections": [{"kind": "table", "id": "models", "columns": ["MODEL"],
             "rows": [{"key": "qwen", "cells": ["loaded"]},
                      {"key": "qwen", "cells": ["idle"]}]}]}"#,
    ] {
        assert!(Pane::read(body).is_err(), "{body}");
    }
}

/// The cap is the one `shep.validate` quotes an unreadable answer at, and the
/// cut lands on a character boundary rather than inside a character.
#[test]
fn an_unreadable_answer_is_quoted_at_the_validate_cap_on_a_character_boundary() {
    // Byte 200 sits inside `é`, so the cut walks back to 199.
    let body = format!("{}é", "a".repeat(199));

    let reason = Pane::read(&body).unwrap_err();

    assert_eq!(
        reason,
        format!(
            "the dog answered with something shep cannot read: {}...",
            "a".repeat(199)
        )
    );
}

/// A kind this shep does not know is skipped rather than unreadable: a later
/// shep adds kinds and an older lookout still draws the rest, in order.
#[test]
fn a_section_of_an_unknown_kind_is_skipped_and_counted() {
    let body = json!({
        "sections": [
            {"kind": "fields", "id": "host", "title": "host",
             "fields": [{"label": "VRAM", "value": "20 / 24 GiB"}]},
            {"kind": "log-tail", "id": "recent", "title": "recent", "lines": ["a later shep's"]},
            {"kind": "table", "id": "models", "title": "models",
             "columns": ["MODEL"], "rows": [{"key": "qwen3-32b", "cells": ["qwen3-32b"]}]}
        ]
    })
    .to_string();

    let pane = Pane::read(&body).unwrap();

    assert_eq!(pane.skipped, 1);
    assert_eq!(pane.sections.len(), 2);
    assert_eq!(id_of(&pane.sections[0]), "host");
    assert_eq!(id_of(&pane.sections[1]), "models");
}

/// IR-41: a lazy `derive(Debug)` refactor must fail here, because a dog may put
/// anything in a cell.
#[test]
fn debug_prints_section_ids_and_row_counts_and_no_cell_text() {
    let pane = Pane::new()
        .fields("host", [("token", "hunter2")])
        .table("models", ["MODEL"], [Row::new("qwen3-32b", ["hunter2"])])
        .action("pause", "Pause the board", Destructive::No);

    assert_eq!(
        format!("{pane:?}"),
        r#"Pane { sections: [Fields { id: "host", fields: 1 }, Table { id: "models", rows: 1, row_actions: 0 }], actions: 1, skipped: 0 }"#
    );
    assert_eq!(
        format!("{:?}", Field::new("token", "hunter2")),
        r#"Field { label: "token", value: <elided> }"#
    );
    assert_eq!(
        format!("{:?}", Row::new("qwen3-32b", ["hunter2"])),
        r#"Row { key: "qwen3-32b", cells: 1 }"#
    );
}
