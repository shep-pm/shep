//! The Go spelling of this crate's two wire enums.
//!
//! `github.com/shep-pm/shep-go/channel` vendors these bytes as
//! `channel/wire.go`. Neither match below has a wildcard arm. A new
//! variant stops this file compiling until Go's spelling is decided.
//!
//! Regenerate with
//! `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test wire_export`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};

/// One Go constant for one `kind` string.
struct Kind {
    ident: &'static str,
    doc: &'static str,
    wire: &'static str,
}

/// One field of a Go struct, in declaration order.
struct Field {
    ident: &'static str,
    ty: &'static str,
    tag: &'static str,
}

/// The Go constant for a child message's kind.
///
/// No wildcard arm. A new variant fails to compile here first.
fn child_kind(message: &ChildMessage) -> Kind {
    match message {
        ChildMessage::Ready => Kind {
            ident: "KindReady",
            doc: "KindReady is the child's readiness signal.",
            wire: "ready",
        },
        ChildMessage::Metric { .. } => Kind {
            ident: "KindMetric",
            doc: "KindMetric is one metric sample from the child.",
            wire: "metric",
        },
        ChildMessage::ActionReply { .. } => Kind {
            ident: "KindActionReply",
            doc: "KindActionReply is the child's answer to one action.",
            wire: "action-reply",
        },
    }
}

/// The Go constant for a shepherd message's kind.
///
/// No wildcard arm, for the same reason as [`child_kind`].
fn shepherd_kind(message: &ShepherdMessage) -> Kind {
    match message {
        ShepherdMessage::Shutdown => Kind {
            ident: "KindShutdown",
            doc: "KindShutdown is the shepherd asking the app to stop.",
            wire: "shutdown",
        },
        ShepherdMessage::Action { .. } => Kind {
            ident: "KindAction",
            doc: "KindAction is the shepherd dispatching one custom action.",
            wire: "action",
        },
    }
}

/// One value per variant, carrying every optional field.
///
/// The guard test reads the keys these serialize to.
fn child_samples() -> Vec<ChildMessage> {
    vec![
        ChildMessage::Ready,
        ChildMessage::Metric {
            name: "rps".into(),
            value: 42.0,
        },
        ChildMessage::ActionReply {
            action: "gc".into(),
            body: "ok".into(),
            id: Some(7),
        },
    ]
}

/// One value per variant, carrying every optional field.
fn shepherd_samples() -> Vec<ShepherdMessage> {
    vec![
        ShepherdMessage::Shutdown,
        ShepherdMessage::Action {
            name: "gc".into(),
            params: Some("now".into()),
            id: 7,
        },
    ]
}

/// Every optional field is a pointer. `omitempty` on a value would drop a
/// metric of zero and an id of zero.
#[rustfmt::skip]
const CHILD_FIELDS: &[Field] = &[
    Field { ident: "Kind",   ty: "string",   tag: "kind" },
    Field { ident: "Name",   ty: "*string",  tag: "name,omitempty" },
    Field { ident: "Value",  ty: "*float64", tag: "value,omitempty" },
    Field { ident: "Action", ty: "*string",  tag: "action,omitempty" },
    Field { ident: "Body",   ty: "*string",  tag: "body,omitempty" },
    Field { ident: "ID",     ty: "*uint64",  tag: "id,omitempty" },
];

/// `Params` is a pointer because an absent one and an empty one are
/// different messages.
#[rustfmt::skip]
const SHEPHERD_FIELDS: &[Field] = &[
    Field { ident: "Kind",   ty: "string",  tag: "kind" },
    Field { ident: "Name",   ty: "*string", tag: "name,omitempty" },
    Field { ident: "Params", ty: "*string", tag: "params,omitempty" },
    Field { ident: "ID",     ty: "*uint64", tag: "id,omitempty" },
];

const CHILD_DOC: &str = "\
// ChildMessage is one line the app writes to the shepherd.
//
// Kind selects which other fields carry meaning. Each of those is a
// pointer. Dropping a zero would lose a metric of 0.
";

const SHEPHERD_DOC: &str = "\
// ShepherdMessage is one line the shepherd writes to the app.
//
// Kind selects which other fields carry meaning. An absent Params
// differs from an empty one, so it is a pointer too.
";

/// Pads both columns the way gofmt aligns a struct.
fn emit_struct(out: &mut String, doc: &str, name: &str, fields: &[Field]) {
    let ident_width = fields
        .iter()
        .map(|field| field.ident.len())
        .max()
        .unwrap_or(0);
    let type_width = fields.iter().map(|field| field.ty.len()).max().unwrap_or(0);
    out.push_str(doc);
    writeln!(out, "type {name} struct {{").expect("write to a String");
    for field in fields {
        writeln!(
            out,
            "\t{:ident_width$} {:type_width$} `json:{:?}`",
            field.ident, field.ty, field.tag
        )
        .expect("write to a String");
    }
    out.push_str("}\n");
}

/// The whole Go file, ending in one newline.
fn emit() -> String {
    let mut out = String::new();
    out.push_str("// Code generated by shep's wire exporter. DO NOT EDIT.\n");
    out.push_str("// Source: crates/shep-channel/src/wire.rs in github.com/shep-pm/shep.\n");
    out.push_str("\npackage channel\n\n");

    out.push_str("// Version is the value the shepherd exports as SHEP_CHANNEL_VERSION.\n");
    out.push_str("//\n");
    out.push_str("// A stamp, not a negotiation. An app can notice a wire it has never\n");
    out.push_str("// seen. It cannot ask for a different one.\n");
    writeln!(out, "const Version = {CHANNEL_VERSION:?}").expect("write to a String");
    out.push('\n');

    out.push_str("// The kinds carried in every message's \"kind\" field.\n");
    out.push_str("const (\n");
    let child = child_samples();
    let shepherd = shepherd_samples();
    for kind in child
        .iter()
        .map(child_kind)
        .chain(shepherd.iter().map(shepherd_kind))
    {
        writeln!(out, "\t// {}", kind.doc).expect("write to a String");
        writeln!(out, "\t{} = {:?}", kind.ident, kind.wire).expect("write to a String");
    }
    out.push_str(")\n\n");

    emit_struct(&mut out, CHILD_DOC, "ChildMessage", CHILD_FIELDS);
    out.push('\n');
    emit_struct(&mut out, SHEPHERD_DOC, "ShepherdMessage", SHEPHERD_FIELDS);
    out
}

fn wire_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("wire")
        .join("channel.go")
}

/// The wire key a Go field's `json` tag names.
fn json_name(field: &Field) -> &'static str {
    field.tag.split(',').next().expect("a tag names a key")
}

/// Checks one enum's encodings against the Go table it emits.
///
/// Every key reaches a field, every field is reached, and both agree on
/// order. Go emits struct fields in declaration order, and the corpus
/// compares key order.
fn check_keys<'a>(samples: impl Iterator<Item = (String, &'a str)>, fields: &[Field]) {
    let known: BTreeSet<String> = fields
        .iter()
        .map(|field| json_name(field).to_owned())
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (encoded, kind) in samples {
        assert!(
            encoded.starts_with(&format!(r#"{{"kind":"{kind}""#)),
            "the emitter and serde disagree about a kind: {encoded}"
        );
        let mut previous = 0;
        for field in fields {
            let key = json_name(field);
            let Some(at) = encoded.find(&format!(r#""{key}":"#)) else {
                continue;
            };
            assert!(at >= previous, "{key} is out of order in {encoded}");
            previous = at;
            seen.insert(key.to_owned());
        }
        let keys: BTreeSet<String> = serde_json::from_str::<serde_json::Value>(&encoded)
            .expect("decode")
            .as_object()
            .expect("a message encodes as an object")
            .keys()
            .cloned()
            .collect();
        assert!(
            keys.is_subset(&known),
            "a wire key has no Go field: {encoded}"
        );
    }
    assert_eq!(seen, known, "a Go field is not on the wire any more");
}

#[test]
fn the_committed_go_file_is_what_the_emitter_writes() {
    let emitted = emit();
    let path = wire_path();
    if std::env::var_os("SHEP_CHANNEL_BLESS").is_some() {
        fs::create_dir_all(path.parent().expect("wire/ has a parent")).expect("create wire dir");
        fs::write(&path, &emitted).expect("write the wire file");
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}. Run with SHEP_CHANNEL_BLESS=1 to create it.",
            path.display()
        )
    });
    assert_eq!(
        committed,
        emitted,
        "{} is stale. github.com/shep-pm/shep-go/channel vendors these bytes as channel/wire.go.",
        path.display()
    );
}

#[test]
fn every_wire_key_reaches_a_go_field_in_the_same_order() {
    check_keys(
        child_samples().iter().map(|sample| {
            (
                serde_json::to_string(sample).expect("encode"),
                child_kind(sample).wire,
            )
        }),
        CHILD_FIELDS,
    );
    check_keys(
        shepherd_samples().iter().map(|sample| {
            (
                serde_json::to_string(sample).expect("encode"),
                shepherd_kind(sample).wire,
            )
        }),
        SHEPHERD_FIELDS,
    );
}
