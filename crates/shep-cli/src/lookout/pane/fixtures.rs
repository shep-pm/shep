//! Panes the tests in this module's siblings build on.
//!
//! One place rather than one copy per file: the same `web` sheep and `bark`
//! dog are the subject of tests in nearly every one of them, and a fixture
//! that drifts between two copies is a test that passes for the wrong
//! reason.

use serde_json::Value;
use shep_core::config::{AppConfig, ApplyGroup};
use shep_core::protocol::SheepConfigView;

use super::super::edits::EditKey;
use super::{ConfigPane, PaneEdit};

pub(super) fn web() -> SheepConfigView {
    let mut config = AppConfig {
        name: "web".into(),
        max_restarts: 32,
        ..AppConfig::default()
    };
    config
        .env
        .insert("DB_HOST".into(), "{{shared:DB_HOST}}".into());
    SheepConfigView::new(config, vec!["max_restarts".into()], vec!["env".into()])
}

/// The value the pane has filed for the config field `key`, or
/// [`None`] when nothing is filed for it.
pub(super) fn filed(pane: &ConfigPane, key: &str) -> Option<Value> {
    match pane.edits().get(&EditKey::Field(key.to_owned()))?.edit() {
        PaneEdit::Set { value, .. } => Some(value.as_value().clone()),
        PaneEdit::SetEnv { .. } => None,
    }
}

/// What the pane recorded that filed edit as costing.
pub(super) fn filed_impact(pane: &ConfigPane, key: &str) -> Option<ApplyGroup> {
    pane.edits().get(&EditKey::Field(key.to_owned()))?.impact()
}

pub(super) fn web_with_args(args: &[&str]) -> SheepConfigView {
    let config = AppConfig {
        name: "web".into(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        stop_exit_codes: vec![0, 143],
        ..AppConfig::default()
    };
    SheepConfigView::new(config, Vec::new(), Vec::new())
}

/// The bark section every dog test below reads: a comment, a scalar,
/// and a sink carrying a credential.
pub(super) fn bark_section() -> String {
    "# how often\npoll = \"60s\"\nhistory_bytes = 4096\n\n[sinks.ops]\nkind = \"slack\"\nurl = \"https://hooks.example/x\"\n".to_owned()
}

pub(super) fn bark_pane() -> ConfigPane {
    let schema = crate::dog::builtin_schema("bark").expect("bark is a built-in");
    ConfigPane::dog("bark".into(), None, schema, bark_section())
}

/// A dog whose schema declares one secret string. No built-in has one:
/// bark's only secret is a map, and a map has no editor to type a
/// secret into, so a typed secret is not reachable from any fixture
/// the pane already had.
pub(super) fn secret_dog_pane() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": {
            "webhook": { "type": "string", "x-shep-secret": true },
        }
    });
    ConfigPane::dog(
        "pydog".into(),
        None,
        schema,
        "webhook = \"https://hook/OLD\"\n".to_owned(),
    )
}

/// A field edit, filed the way [`Edits::set`] wants it, for the tests
/// below that build a batch by hand rather than by keystroke.
pub(super) fn field(key: &str, value: serde_json::Value) -> PaneEdit {
    PaneEdit::Set {
        key: key.to_owned(),
        value: value.into(),
    }
}
