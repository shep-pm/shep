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
use std::path::PathBuf;

use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};

mod common;

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
    /// The wire kinds that carry this field. The guard holds each sample
    /// against exactly the fields its kind claims.
    kinds: &'static [&'static str],
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
    Field { ident: "Kind",   ty: "string",   tag: "kind",             kinds: &["ready", "metric", "action-reply"] },
    Field { ident: "Name",   ty: "*string",  tag: "name,omitempty",   kinds: &["metric"] },
    Field { ident: "Value",  ty: "*float64", tag: "value,omitempty",  kinds: &["metric"] },
    Field { ident: "Action", ty: "*string",  tag: "action,omitempty", kinds: &["action-reply"] },
    Field { ident: "Body",   ty: "*string",  tag: "body,omitempty",   kinds: &["action-reply"] },
    Field { ident: "ID",     ty: "*uint64",  tag: "id,omitempty",     kinds: &["action-reply"] },
];

/// `Params` is a pointer because an absent one and an empty one are
/// different messages.
#[rustfmt::skip]
const SHEPHERD_FIELDS: &[Field] = &[
    Field { ident: "Kind",   ty: "string",  tag: "kind",              kinds: &["shutdown", "action"] },
    Field { ident: "Name",   ty: "*string", tag: "name,omitempty",    kinds: &["action"] },
    Field { ident: "Params", ty: "*string", tag: "params,omitempty",  kinds: &["action"] },
    Field { ident: "ID",     ty: "*uint64", tag: "id,omitempty",      kinds: &["action"] },
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
    // Two samples of one variant would declare the same constant twice,
    // and Go refuses a redeclaration.
    let mut declared: BTreeSet<&'static str> = BTreeSet::new();
    for kind in child
        .iter()
        .map(child_kind)
        .chain(shepherd.iter().map(shepherd_kind))
    {
        if !declared.insert(kind.ident) {
            continue;
        }
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

/// The JSON value kind a declared Go type has to decode as.
///
/// Signedness and width are past what a JSON number can say, so `*uint64`
/// and `*float64` answer the same.
fn declared_json_kind(ty: &str) -> &'static str {
    match ty {
        "string" | "*string" => "string",
        "*float64" | "*uint64" => "number",
        other => panic!("no JSON kind is declared for the Go type {other}"),
    }
}

/// The JSON value kind a sample actually encoded.
fn encoded_json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Checks one enum's encodings against the Go table it emits.
///
/// Each sample carries exactly the fields its kind claims, in declaration
/// order, and each value decodes as the kind its Go type declares. Go emits
/// struct fields in declaration order, and the corpus compares key order.
fn check_keys<'a>(samples: impl Iterator<Item = (String, &'a str)>, fields: &[Field]) {
    let samples: Vec<(String, &str)> = samples.collect();
    let sampled: BTreeSet<&str> = samples.iter().map(|(_, kind)| *kind).collect();
    for field in fields {
        for kind in field.kinds {
            assert!(
                sampled.contains(kind),
                "{} is declared on {kind}, which no sample carries",
                field.ident
            );
        }
    }

    for (encoded, kind) in &samples {
        assert!(
            encoded.starts_with(&format!(r#"{{"kind":"{kind}""#)),
            "the emitter and serde disagree about a kind: {encoded}"
        );
        let carried: Vec<&Field> = fields
            .iter()
            .filter(|field| field.kinds.contains(kind))
            .collect();
        let decoded = serde_json::from_str::<serde_json::Value>(encoded).expect("decode");
        let object = decoded.as_object().expect("a message encodes as an object");

        let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        let declared: BTreeSet<&str> = carried.iter().copied().map(json_name).collect();
        assert_eq!(
            keys, declared,
            "{kind} and its Go field table disagree: {encoded}"
        );

        let mut previous = 0;
        for field in carried {
            let key = json_name(field);
            let at = encoded
                .find(&format!(r#""{key}":"#))
                .expect("a checked key is on the wire");
            assert!(at >= previous, "{key} is out of order in {encoded}");
            previous = at;
            let value = &object[key];
            assert_eq!(
                encoded_json_kind(value),
                declared_json_kind(field.ty),
                "{} is {} in Go and {kind} encodes {key} as {value}",
                field.ident,
                field.ty
            );
        }
    }
}

#[test]
fn the_committed_go_file_is_what_the_emitter_writes() {
    common::bless_or_compare(
        &wire_path(),
        &emit(),
        "github.com/shep-pm/shep-go/channel vendors these bytes as channel/wire.go.",
    );
}

/// Go refuses a redeclared constant, and no Go compiler runs in this gate.
#[test]
fn the_const_block_declares_each_identifier_once() {
    let emitted = emit();
    let block = emitted
        .split("const (\n")
        .nth(1)
        .expect("a const block")
        .split("\n)\n")
        .next()
        .expect("the const block closes");
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for line in block.lines() {
        let Some((ident, _)) = line.trim().split_once(" = ") else {
            continue;
        };
        assert!(
            declared.insert(ident),
            "{ident} is declared twice in the const block"
        );
    }
}

#[test]
fn every_go_field_names_at_least_one_kind() {
    for (enum_name, fields) in [
        ("ChildMessage", CHILD_FIELDS),
        ("ShepherdMessage", SHEPHERD_FIELDS),
    ] {
        for field in fields {
            assert!(
                !field.kinds.is_empty(),
                "{enum_name}.{} names no kind, so no sample reaches it",
                field.ident
            );
        }
    }
}

#[test]
fn an_absent_params_and_an_empty_one_encode_differently() {
    let absent = ShepherdMessage::Action {
        name: "gc".into(),
        params: None,
        id: 7,
    };
    let empty = ShepherdMessage::Action {
        name: "gc".into(),
        params: Some(String::new()),
        id: 7,
    };
    let absent_encoded = serde_json::to_string(&absent).expect("encode");
    let empty_encoded = serde_json::to_string(&empty).expect("encode");
    assert!(
        !absent_encoded.contains(r#""params""#),
        "an absent params key was still on the wire: {absent_encoded}"
    );
    assert!(
        empty_encoded.contains(r#""params":"""#),
        "an empty params value did not round-trip: {empty_encoded}"
    );
    let params_field = SHEPHERD_FIELDS
        .iter()
        .find(|field| field.ident == "Params")
        .expect("Params is declared");
    assert_eq!(
        params_field.ty, "*string",
        "a plain string would collapse the two encodings above into one Go value"
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
