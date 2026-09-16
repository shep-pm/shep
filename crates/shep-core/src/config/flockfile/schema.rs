//! The JSON Schema shep publishes for editors, emitted from [`RawFlockfile`].

use schemars::Schema;

use super::raw::RawFlockfile;

/// The committed Flockfile JSON Schema.
///
/// `include_str!` makes the file a compile-time input: deleting it fails
/// the build, and changing `AppConfig` fails
/// `the_committed_schema_is_current` with the command that fixes it.
///
/// Lives inside this package, not the repository root: `cargo package`
/// packs only files under the package directory, and a root-relative
/// `include_str!` would fail every `cargo install shep`.
///
/// `#[allow(dead_code)]`: its only reader is `#[cfg(test)]`, so a plain
/// `cargo build`/`clippy` sees no reader and would otherwise flag it dead.
pub const COMMITTED: &str = include_str!("../../../assets/flockfile.schema.json");

/// How to regenerate the committed copy. Named in the drift test's own
/// failure message, so a red test is self-service.
///
/// Same `#[allow(dead_code)]` reasoning as [`COMMITTED`] just above: its one
/// reader is that same test.
#[allow(dead_code)]
const REGENERATE: &str =
    "cargo run --bin shep -- schema > crates/shep-core/assets/flockfile.schema.json";

/// Renders the Flockfile JSON Schema: the document grammar, pretty-printed
/// with a trailing newline so the committed file is a well-formed text file.
///
/// Generated from `RawFlockfile`, the type serde actually deserializes a
/// Flockfile into, so the schema and the parser cannot drift.
/// `AppConfig` supplies the per-app half and lands in `$defs`.
///
/// Describes the deserializer, not the normalizer:
/// `AppConfig::kill_signal` stays a plain string here even though
/// `config::normalize` accepts only four spellings, since the schema's
/// job is what serde parses, not what a later validation step accepts.
#[track_caller]
#[must_use]
pub fn flockfile_schema_string() -> String {
    let schema = flockfile_schema_json();
    let mut rendered =
        serde_json::to_string_pretty(&schema).expect("a schemars Schema always serializes");
    rendered.push('\n');
    rendered
}

/// Returns the Flockfile JSON Schema.
///
/// Generated from `RawFlockfile`, the type serde actually deserializes a
/// Flockfile into, so the schema and the parser cannot drift. Describes
/// the deserializer, not the normalizer: `AppConfig::kill_signal` stays a
/// plain string here even though `config::normalize` accepts only four
/// spellings.
///
/// # Panics
/// Never in practice: schemars produces a `serde_json::Value` tree, which
/// `to_string_pretty` cannot fail on. `#[track_caller]` so a future
/// fallible change reports the caller.
#[track_caller]
#[must_use]
pub fn flockfile_schema_json() -> Schema {
    schemars::schema_for!(RawFlockfile)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves a `$ref` into `$defs`, one hop, and returns the subschema.
    /// Everything with a `schema_name` is referenced rather than inlined, so
    /// an assertion that does not follow the ref is asserting about a
    /// `{"$ref": …}` object and passes or fails for the wrong reason.
    #[cfg(feature = "schema")]
    fn resolved<'a>(
        root: &'a serde_json::Value,
        node: &'a serde_json::Value,
    ) -> &'a serde_json::Value {
        match node.get("$ref").and_then(serde_json::Value::as_str) {
            Some(r) => {
                let name = r
                    .strip_prefix("#/$defs/")
                    .expect("every $ref in this schema points into $defs");
                &root["$defs"][name]
            }
            None => node,
        }
    }

    /// fails whenever the Flockfile grammar changes and the committed schema
    /// does not. That includes a doc-comment edit: schemars reads `///` into
    /// `description`, which becomes hover text in the operator's editor, so
    /// a docs-only change is a real schema change, and regenerating is the
    /// correct response, not a sign anything broke.
    #[cfg(feature = "schema")]
    #[test]
    fn the_committed_schema_is_current() {
        assert_eq!(
            flockfile_schema_string(),
            COMMITTED,
            "crates/shep-core/assets/flockfile.schema.json is stale. Regenerate it:\n    {REGENERATE}\n\
             A doc-comment edit on AppConfig counts; schemars puts doc comments \
             into `description`."
        );
    }

    /// fails if the artefact goes back to describing one app. The document is
    /// `{"app": [ … ]}`; a schema whose own `required` names `name` and
    /// `script` is an AppConfig schema under a Flockfile filename, and every
    /// real Flockfile would fail against it.
    #[cfg(feature = "schema")]
    #[test]
    fn the_schema_describes_a_document_not_one_app() {
        let schema: serde_json::Value = serde_json::from_str(&flockfile_schema_string()).unwrap();
        assert!(schema["properties"]["app"].is_object(), "{schema}");
        assert_eq!(schema["properties"]["app"]["type"], "array", "{schema}");
        assert!(
            schema["properties"]["name"].is_null(),
            "root must not be an app: {schema}"
        );
        assert!(schema["$defs"]["AppConfig"].is_object(), "{schema}");
    }

    /// fails if the schema starts describing `normalize`'s grammar instead of
    /// serde's. The four signal names belong to a validation step elsewhere;
    /// a schema that listed them would be describing something it cannot see.
    #[cfg(feature = "schema")]
    #[test]
    fn kill_signal_stays_an_unconstrained_string() {
        let schema: serde_json::Value = serde_json::from_str(&flockfile_schema_string()).unwrap();
        let field = resolved(
            &schema,
            &schema["$defs"]["AppConfig"]["properties"]["kill_signal"],
        );
        let types = field["type"]
            .as_array()
            .unwrap_or_else(|| panic!("kill_signal must carry a type array: {field}"));
        assert!(
            types.iter().any(|t| t == "string"),
            "kill_signal must accept a string: {field}"
        );
        assert!(
            field.get("enum").is_none(),
            "kill_signal must not become an enum of the four signal names: {field}"
        );
        assert!(
            field.get("pattern").is_none(),
            "kill_signal must not become pattern-constrained: {field}"
        );
    }

    /// fails if MemSize or UpDuration reverts to a derive and starts
    /// describing its inner integer. Follows the `$ref`: the fields are
    /// references into `$defs`, not inline schemas.
    #[cfg(feature = "schema")]
    #[test]
    fn duration_and_memory_fields_are_string_shaped() {
        let schema: serde_json::Value = serde_json::from_str(&flockfile_schema_string()).unwrap();
        let app = &schema["$defs"]["AppConfig"]["properties"];

        // `min_uptime: UpDuration` (not `Option`) is a bare `$ref`.
        let min_uptime = resolved(&schema, &app["min_uptime"]);
        assert_eq!(min_uptime["type"], "string", "{min_uptime}");
        assert_eq!(min_uptime["pattern"], r"^\d+(ms|h|m|s)?$", "{min_uptime}");

        // `max_memory: Option<MemSize>` is a `$ref` under `anyOf` beside `"null"`.
        let any_of = app["max_memory"]["anyOf"]
            .as_array()
            .unwrap_or_else(|| panic!("max_memory must be anyOf: {}", app["max_memory"]));
        let ref_node = any_of
            .iter()
            .find(|v| v.get("$ref").is_some())
            .unwrap_or_else(|| panic!("max_memory's anyOf must carry a $ref: {any_of:?}"));
        let max_memory = resolved(&schema, ref_node);
        assert_eq!(max_memory["type"], "string", "{max_memory}");
        assert_eq!(max_memory["pattern"], r"^\d+(G|M|K)?$", "{max_memory}");
    }
}
