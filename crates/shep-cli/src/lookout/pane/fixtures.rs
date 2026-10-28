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
    ConfigPane::dog("bark".into(), None, schema, bark_section(), Vec::new())
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
        Vec::new(),
    )
}

/// A dog whose schema publishes a decimal money cap: `shep-kelpie`'s
/// `budget.per_item_usd`, `type: number`, `format: double`,
/// `exclusiveMinimum: 0`. The one scalar shape the pane had no editor for
/// until `kind_of` gained a `Number` arm.
pub(super) fn money_schema() -> Value {
    serde_json::json!({
        "properties": {
            "per_item_usd": { "type": "number", "format": "double", "exclusiveMinimum": 0 },
        },
    })
}

/// [`money_schema`] over `section`.
pub(super) fn money_pane(section: &str) -> ConfigPane {
    ConfigPane::dog(
        "kelpie".into(),
        None,
        money_schema(),
        section.to_owned(),
        Vec::new(),
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

/// A dog whose schema nests: two top-level tables that each hold a `model`,
/// one holding a table of its own, and a leaf after them.
pub(super) fn tree_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "agents": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "retries": { "type": "integer" },
                },
            },
            "budget": {
                "type": "object",
                "properties": {
                    "cap": { "type": "integer" },
                    "model": { "type": "string" },
                },
            },
            "deep": {
                "type": "object",
                "properties": {
                    "inner": {
                        "type": "object",
                        "properties": { "leaf": { "type": "boolean" } },
                    },
                },
            },
            "name": { "type": "string" },
        },
    })
}

pub(super) fn tree_section() -> String {
    "name = \"x\"\n[agents]\nmodel = \"small\"\n[budget]\nmodel = \"large\"\ncap = 3\n".to_owned()
}

/// [`tree_schema`] over [`tree_section`].
pub(super) fn tree_pane() -> ConfigPane {
    ConfigPane::dog(
        "tree".into(),
        None,
        tree_schema(),
        tree_section(),
        Vec::new(),
    )
}

/// A paddock-shaped dog: a `clients` array whose entries carry a secret
/// `key` beside a `name` or an `id`, a nested `limits` table and a list of
/// their own `peers`, a `models` map, and a scalar after them.
pub(super) fn entries_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "clients": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
            "models": {
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/Model" },
            },
            "retries": { "type": "integer" },
        },
        "$defs": {
            "Client": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "key": { "type": "string", "x-shep-secret": true },
                    "limits": {
                        "type": "object",
                        "properties": { "rate": { "type": "integer" } },
                    },
                    "name": { "type": "string" },
                    "peers": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
                },
            },
            "Model": {
                "type": "object",
                "properties": {
                    "context": { "type": "integer" },
                    "provider": { "type": "string" },
                },
            },
        },
    })
}

/// [`entries_schema`]'s section: three clients, the first named, the second
/// with only an id and the third with neither, and two models.
pub(super) fn entries_section() -> String {
    concat!(
        "retries = 3\n",
        "[[clients]]\nname = \"alpha\"\nkey = \"hunter2\"\n[clients.limits]\nrate = 5\n",
        "[[clients]]\nid = \"b-2\"\nkey = \"hunter3\"\n",
        "[[clients]]\nkey = \"hunter4\"\n",
        "[models.small]\nprovider = \"local\"\ncontext = 4096\n",
        "[models.large]\nprovider = \"remote\"\n",
    )
    .to_owned()
}

/// [`entries_schema`] over [`entries_section`].
pub(super) fn entries_pane() -> ConfigPane {
    ConfigPane::dog(
        "paddock".into(),
        None,
        entries_schema(),
        entries_section(),
        Vec::new(),
    )
}

/// [`entries_schema`] with defaults on the items: a client defaults to
/// enabled, with a secret `key` and a nested `limits` table of its own
/// default `rate`; a model defaults its `context`. Neither names a `name`,
/// `provider` or `burst`, which a new entry therefore lacks.
pub(super) fn defaulted_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "clients": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
            "models": {
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/Model" },
            },
            "retries": { "type": "integer" },
        },
        "$defs": {
            "Client": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "default": true },
                    "key": { "type": "string", "x-shep-secret": true, "default": "change-me" },
                    "limits": {
                        "type": "object",
                        "properties": {
                            "burst": { "type": "integer" },
                            "rate": { "type": "integer", "default": 5 },
                        },
                    },
                    "name": { "type": "string" },
                },
            },
            "Model": {
                "type": "object",
                "properties": {
                    "context": { "type": "integer", "default": 4096 },
                    "provider": { "type": "string" },
                },
            },
        },
    })
}

/// [`defaulted_schema`] over [`entries_section`].
pub(super) fn defaulted_pane() -> ConfigPane {
    ConfigPane::dog(
        "paddock".into(),
        None,
        defaulted_schema(),
        entries_section(),
        Vec::new(),
    )
}
