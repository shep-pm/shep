//! The shape serde accepts, before any validation.
//!
//! `deny_unknown_fields` is the point: a typo must fail loudly. `$schema`
//! and `dog` are let in beside `app` explicitly, one field at a time, so
//! the next one has to be added the same way.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::AppConfig;
use crate::config::table::deserialize_table;

use super::{error::FlockfileError, format::FlockFormat, parse::parse_into_ignoring};

// Application entries are locked to the `app` key: a typo'd key must fail
// loudly. `$schema` and `dog` are the two keys explicitly let in beside
// it; a future schema key is added the same explicit way, so older
// binaries reject newer Flockfiles by design rather than ignore them.
#[derive(Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// `rename` sets `schema_name`, which schemars uses as the root schema's
// `title`. The type is called `RawFlockfile` because it is the
// pre-validation twin of `Flockfile`; the document an operator writes is a
// Flockfile, and that is what the title has to say.
#[cfg_attr(feature = "schema", schemars(rename = "Flockfile"))]
#[serde(deny_unknown_fields)]
pub(super) struct RawFlockfile {
    /// The editor's schema hint, read and discarded.
    ///
    /// This is the "future schema key" the comment above anticipated, added
    /// HERE explicitly rather than by relaxing `deny_unknown_fields`: a
    /// typo'd key must still fail loudly, and exactly one more key is now
    /// legal. shep does not validate against the named schema and makes no
    /// promise about it — it is a hint for the operator's editor, which is
    /// the only consumer that ever reads it.
    ///
    /// TOML Flockfiles do not need it: taplo's `#:schema <url>` directive is
    /// a comment, invisible to serde. JSON and JSON5 have no comment an
    /// editor agrees to look in, which is why this field exists at all.
    #[serde(default, rename = "$schema")]
    pub(super) schema: Option<String>,
    /// A dog's own per-app configuration, read and discarded.
    ///
    /// Added the same way `$schema` was, explicitly rather than by relaxing
    /// `deny_unknown_fields`, so a typo'd key still fails loudly and exactly
    /// one more key is legal.
    ///
    /// It exists because the alternative is a Flockfile no daemon will accept.
    /// A dog that needs per-app configuration has nowhere to put it: shep-deploy
    /// wants a build command for the app it deploys, which belongs beside that
    /// app's declaration and nowhere else, and a Flockfile carrying one was
    /// refused outright by `shep start`. Measured 2026-08-28 against shep
    /// 0.1.8: an operator following shep-deploy's own README could not register
    /// their app at all. `unknown field `build`, expected `$schema` or `app``.
    ///
    /// It must BE a table. shep does not read what is inside it, does not
    /// validate it, and makes no promise about it. Those are two different
    /// claims and only the second one is a promise not to care: the dog that
    /// owns a key under this table is the only thing that understands it, and
    /// shep refusing a document because it does not recognise another
    /// program's config is a coupling neither side wants.
    ///
    /// A bare TOML date, time or datetime is not a table either. `toml` hands
    /// one to a generic deserializer as a one-key table under a private name,
    /// and that shape is refused rather than read as a table with one odd key
    /// in it.
    ///
    /// Nested under one key rather than allowing loose top-level keys, so
    /// exactly one name is reserved and a typo anywhere else still fails.
    ///
    /// A map of ignored values rather than `IgnoredAny`, which would have
    /// accepted `dog = 5` and `dog = ["a"]` as happily as a table. Not reading
    /// what a dog wrote is the point; not caring whether it wrote a table at
    /// all is a different thing, and it would have made the one key this file
    /// adds the one key where a typo does not fail loudly. A defaulted map
    /// rather than `Option`, for the same reason: `None` and `null` would
    /// otherwise deserialize identically, and `null` is no more a table than
    /// `5` or `["a"]` is. `deserialize_with` is the custom function below,
    /// needed because the derived map deserializer lets `null` back through
    /// for one YAML backend; see that function's own comment.
    #[serde(default, deserialize_with = "deserialize_dog_table")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "BTreeMap<String, serde_json::Value>")
    )]
    pub(super) dog: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default, rename = "app")]
    pub(super) apps: Vec<AppConfig>,
}

// `deserialize_any`, not the derived `BTreeMap` deserializer: at least one
// YAML backend folds a null scalar into an empty map before
// `deserialize_map` ever reaches a visitor. `deserialize_any` reaches that
// null as `visit_unit`, which errors by default. The shared table reader in
// `config::table` is that visitor, given the key name so its refusal says
// `` `dog` must be a table `` instead of serde's `invalid type: unit value,
// expected a table`, and refuses a bare TOML datetime, which arrives as a
// one-key table and would otherwise parse.
fn deserialize_dog_table<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, serde::de::IgnoredAny>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_table(deserializer, "dog")
}

impl RawFlockfile {
    /// Parses `source`, refusing any key no field claims.
    ///
    /// Shared by `Flockfile::parse` and `parse_declared`: both read a
    /// Flockfile off disk (never a value off the wire), so both refuse a typo
    /// the same way. `deny_unknown_fields` used to live on `AppConfig`
    /// itself, which made every new Flockfile field a protocol event: the
    /// same type rides the wire, where an unknown field means a newer peer
    /// rather than a typo. The denial belongs here instead.
    pub(super) fn new(source: &str, format: FlockFormat) -> Result<Self, FlockfileError> {
        let mut unknown = Vec::new();
        let raw: RawFlockfile = parse_into_ignoring(source, format, |path| {
            // `dog` is a map of `IgnoredAny` by design (see its doc comment):
            // shep does not read or validate what a dog wrote there, so
            // serde_ignored's callback for a key inside it is not a typo, it
            // is the field doing exactly what it is for.
            if !path.starts_with("dog.") {
                unknown.push(path.to_string());
            }
        })?;
        if !unknown.is_empty() {
            return Err(FlockfileError::UnknownKeys { keys: unknown });
        }
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flockfile::file::Flockfile;
    use crate::config::flockfile::format::FlockFormat;

    /// The refusal a key that has to be a table gets: the key named, the
    /// shape named, and none of serde's type vocabulary.
    ///
    /// The exact phrase is pinned rather than a substring match on the key:
    /// a TOML or YAML error echoes the offending source line, which contains
    /// the key name anyway, so a loose match passes on a message that still
    /// says `invalid type: unit value, expected a table`.
    fn assert_must_be_a_table(err: &FlockfileError, key: &str) {
        let msg = err.to_string();
        let phrase = format!("`{key}` must be a table");
        assert!(
            msg.contains(&phrase),
            "the refusal must read {phrase:?}, got: {msg}"
        );
        assert!(
            !msg.contains("invalid type") && !msg.contains("unit value"),
            "the refusal still speaks serde to the operator: {msg}"
        );
    }

    /// A Flockfile body carrying `body` under one `[[app]]`, in TOML.
    fn toml_app(body: &str) -> String {
        format!("[[app]]\nname = \"web\"\nscript = \"./srv\"\n{body}\n")
    }

    /// fails if a Flockfile carrying a dog's own configuration is refused.
    ///
    /// A dog with per-app configuration has nowhere else to put it: it
    /// belongs beside the app's declaration, in the repository the dog
    /// deploys.
    ///
    /// The contents are not validated: shep does not know what a dog's
    /// keys mean, and refusing a document for not recognising another
    /// program's config is a coupling neither side wants.
    #[test]
    fn a_dog_table_is_accepted_and_ignored() {
        let src = r#"
[dog.deploy]
command = "npm run build"
artifacts = ["dist/app.js"]

[dog.some-other-dog]
anything = { nested = true, count = 3 }

[[app]]
name = "web"
script = "./srv"
"#;
        let flock =
            Flockfile::parse(src, FlockFormat::Toml).expect("a dog's table is not an error");
        assert_eq!(flock.apps.len(), 1);
        assert_eq!(flock.apps[0].name, "web");
    }

    /// fails if `dog` accepts something that is not a table.
    ///
    /// Not reading what a dog wrote does not mean not caring whether it
    /// wrote a table: that would make this the one key where a typo does
    /// not fail loudly.
    #[test]
    fn a_dog_that_is_not_a_table_is_refused() {
        for value in ["5", "\"nope\"", "[1, 2]", "true"] {
            let src = format!("dog = {value}\n\n[[app]]\nname = \"web\"\nscript = \"./srv\"\n");
            let err = Flockfile::parse(&src, FlockFormat::Toml)
                .expect_err("`dog = {value}` is not a table and must be refused");
            assert_must_be_a_table(&err, "dog");
        }
    }

    /// fails if a bare TOML date, time or datetime is accepted as `dog`.
    ///
    /// `toml` hands a generic `Deserialize` a bare datetime as a one-key
    /// table named `$__toml_private_datetime`, so it reaches `visit_map` and
    /// parses as a table unless that key is refused on the way in. Before
    /// this refusal it passed: `dog = 1979-05-27T07:32:00Z` was a Flockfile
    /// shep accepted.
    #[test]
    fn a_dog_that_is_a_datetime_is_refused_in_toml() {
        for value in ["1979-05-27T07:32:00Z", "1979-05-27", "07:32:00"] {
            let src = format!("dog = {value}\n\n[[app]]\nname = \"web\"\nscript = \"./srv\"\n");
            let err = Flockfile::parse(&src, FlockFormat::Toml)
                .expect_err("`dog = {value}` is a bare datetime, not a table");
            assert_must_be_a_table(&err, "dog");
        }
    }

    /// fails if a real `dog` table stops parsing in any format.
    ///
    /// The refusal above is about the key's own shape, not about the key.
    #[test]
    fn a_dog_table_is_accepted_in_every_format() {
        let cases: [(FlockFormat, &str); 4] = [
            (
                FlockFormat::Toml,
                "[dog.deploy]\ncommand = \"npm run build\"\n\n[[app]]\nname = \"web\"\nscript = \"./srv\"\n",
            ),
            (
                FlockFormat::Yaml,
                "dog:\n  deploy:\n    command: npm run build\napp:\n  - name: web\n    script: ./srv\n",
            ),
            (
                FlockFormat::Json,
                r#"{ "dog": { "deploy": { "command": "npm run build" } }, "app": [{ "name": "web", "script": "./srv" }] }"#,
            ),
            (
                FlockFormat::Json5,
                "{ dog: { deploy: { command: 'npm run build' } }, app: [{ name: 'web', script: './srv' }] }",
            ),
        ];
        for (format, text) in cases {
            let flock = Flockfile::parse(text, format)
                .unwrap_or_else(|e| panic!("{format:?} refused a real dog table: {e}"));
            assert_eq!(flock.apps.len(), 1, "{format:?}");
        }
    }

    /// fails if a datetime nested inside a real `dog` table is refused.
    ///
    /// Only a bare `dog` wearing the datetime marker is the bug. A date a
    /// dog wrote inside its own table is the dog's business, and shep reads
    /// none of it.
    #[test]
    fn a_datetime_inside_a_dog_table_is_still_the_dog_business() {
        let src = "[dog.deploy]\nat = 1979-05-27T07:32:00Z\n\n[[app]]\nname = \"web\"\nscript = \"./srv\"\n";
        assert_eq!(
            Flockfile::parse(src, FlockFormat::Toml)
                .expect("a date inside a dog's own table is not shep's to refuse")
                .apps
                .len(),
            1
        );
    }

    /// fails if `dog: null` is accepted in JSON. TOML has no null literal,
    /// so only JSON, YAML and JSON5 need this case, one test each.
    #[test]
    fn a_dog_that_is_null_is_refused_in_json() {
        let json = r#"{ "dog": null, "app": [{ "name": "web", "script": "./srv" }] }"#;
        let err = Flockfile::parse(json, FlockFormat::Json)
            .expect_err("`dog: null` is not a table and must be refused in JSON");
        assert_must_be_a_table(&err, "dog");
    }

    /// fails if `dog: null` is accepted in YAML. A YAML backend may treat a
    /// null scalar requested as a map as an empty map rather than a type
    /// error; `dog`'s own `deserialize_with` exists to close exactly that.
    #[test]
    fn a_dog_that_is_null_is_refused_in_yaml() {
        let yaml = "dog: null\napp:\n  - name: web\n    script: ./srv\n";
        let err = Flockfile::parse(yaml, FlockFormat::Yaml)
            .expect_err("`dog: null` is not a table and must be refused in YAML");
        assert_must_be_a_table(&err, "dog");
    }

    /// fails if `dog: null` is accepted in JSON5. JSON5 is parsed by its own
    /// crate, so this is not redundant with the JSON case above.
    #[test]
    fn a_dog_that_is_null_is_refused_in_json5() {
        let json5 = "{ dog: null, app: [{ name: 'web', script: './srv' }] }";
        let err = Flockfile::parse(json5, FlockFormat::Json5)
            .expect_err("`dog: null` is not a table and must be refused in JSON5");
        assert_must_be_a_table(&err, "dog");
    }

    /// fails if `app[].env: null` is accepted in YAML, where a null scalar
    /// asked for as a map becomes an empty map before any visitor sees it.
    ///
    /// An `env` that is null is not an `env` that is empty: the operator
    /// wrote a value shep cannot mean.
    #[test]
    fn an_env_that_is_null_is_refused_in_yaml() {
        let yaml = "app:\n  - name: web\n    script: ./srv\n    env: null\n";
        let err = Flockfile::parse(yaml, FlockFormat::Yaml)
            .expect_err("`env: null` is not a table and must be refused in YAML");
        assert_must_be_a_table(&err, "env");
    }

    /// fails if `env: null` is accepted in JSON. It was refused before, in
    /// serde's words: `invalid type: null, expected a map`.
    #[test]
    fn an_env_that_is_null_is_refused_in_json() {
        let json = r#"{ "app": [{ "name": "web", "script": "./srv", "env": null }] }"#;
        let err = Flockfile::parse(json, FlockFormat::Json)
            .expect_err("`env: null` is not a table and must be refused in JSON");
        assert_must_be_a_table(&err, "env");
    }

    /// fails if `env: null` is accepted in JSON5, which is parsed by its own
    /// crate.
    #[test]
    fn an_env_that_is_null_is_refused_in_json5() {
        let json5 = "{ app: [{ name: 'web', script: './srv', env: null }] }";
        let err = Flockfile::parse(json5, FlockFormat::Json5)
            .expect_err("`env: null` is not a table and must be refused in JSON5");
        assert_must_be_a_table(&err, "env");
    }

    /// fails if a YAML key with nothing after it is read as an absent key.
    ///
    /// `env:` alone is YAML for null, so it is refused by the same arm as
    /// `env: null`. An operator who means "no variables" leaves the key out.
    #[test]
    fn a_yaml_env_with_no_value_is_refused() {
        let yaml = "app:\n  - name: web\n    script: ./srv\n    env:\n";
        let err = Flockfile::parse(yaml, FlockFormat::Yaml)
            .expect_err("a bare `env:` is YAML for null and must be refused");
        assert_must_be_a_table(&err, "env");
    }

    /// fails if a bare TOML date, time or datetime lands in `env`.
    ///
    /// Before this refusal it passed, and the private marker key landed in
    /// the sheep's environment: `env = 1979-05-27T07:32:00Z` gave the process
    /// a variable named `$__toml_private_datetime`.
    #[test]
    fn an_env_that_is_a_datetime_is_refused_in_toml() {
        for value in ["1979-05-27T07:32:00Z", "1979-05-27", "07:32:00"] {
            let src = toml_app(&format!("env = {value}"));
            let err = Flockfile::parse(&src, FlockFormat::Toml)
                .expect_err("`env = {value}` is a bare datetime, not a table");
            assert_must_be_a_table(&err, "env");
        }
    }

    /// fails if `app[].dogs: null` is accepted in YAML, for the same reason
    /// as `env` above.
    #[test]
    fn a_dogs_that_is_null_is_refused_in_yaml() {
        let yaml = "app:\n  - name: web\n    script: ./srv\n    dogs: null\n";
        let err = Flockfile::parse(yaml, FlockFormat::Yaml)
            .expect_err("`dogs: null` is not a table and must be refused in YAML");
        assert_must_be_a_table(&err, "dogs");
    }

    /// fails if `dogs: null` is accepted in JSON. It was refused before, in
    /// serde's words: `invalid type: null, expected a map`.
    #[test]
    fn a_dogs_that_is_null_is_refused_in_json() {
        let json = r#"{ "app": [{ "name": "web", "script": "./srv", "dogs": null }] }"#;
        let err = Flockfile::parse(json, FlockFormat::Json)
            .expect_err("`dogs: null` is not a table and must be refused in JSON");
        assert_must_be_a_table(&err, "dogs");
    }

    /// fails if `dogs: null` is accepted in JSON5, which is parsed by its
    /// own crate.
    #[test]
    fn a_dogs_that_is_null_is_refused_in_json5() {
        let json5 = "{ app: [{ name: 'web', script: './srv', dogs: null }] }";
        let err = Flockfile::parse(json5, FlockFormat::Json5)
            .expect_err("`dogs: null` is not a table and must be refused in JSON5");
        assert_must_be_a_table(&err, "dogs");
    }

    /// fails if a bare `dogs:` in YAML is read as an absent key.
    #[test]
    fn a_yaml_dogs_with_no_value_is_refused() {
        let yaml = "app:\n  - name: web\n    script: ./srv\n    dogs:\n";
        let err = Flockfile::parse(yaml, FlockFormat::Yaml)
            .expect_err("a bare `dogs:` is YAML for null and must be refused");
        assert_must_be_a_table(&err, "dogs");
    }

    /// fails if a bare TOML date, time or datetime is accepted as `dogs`.
    ///
    /// This one was refused before, but as the inner table's complaint
    /// (`a dog's table must be a table, not a bare value`), which names no
    /// key the operator wrote.
    #[test]
    fn a_dogs_that_is_a_datetime_is_refused_in_toml() {
        for value in ["1979-05-27T07:32:00Z", "1979-05-27", "07:32:00"] {
            let src = toml_app(&format!("dogs = {value}"));
            let err = Flockfile::parse(&src, FlockFormat::Toml)
                .expect_err("`dogs = {value}` is a bare datetime, not a table");
            assert_must_be_a_table(&err, "dogs");
        }
    }

    /// fails if a refusal of `env` prints what the operator put there.
    ///
    /// An `env` value is a secret and a parse error reaches a log. JSON and
    /// JSON5 are the two backends whose whole message is hers to write: TOML
    /// and YAML echo the offending source line whatever serde says, which is
    /// why the value-free claim is made about these two.
    #[test]
    fn an_env_refusal_names_the_key_and_never_the_value() {
        let secret = "postgres://hunter2@db";
        let cases: [(FlockFormat, String); 2] = [
            (
                FlockFormat::Json,
                format!(
                    r#"{{ "app": [{{ "name": "web", "script": "./srv", "env": "{secret}" }}] }}"#
                ),
            ),
            (
                FlockFormat::Json5,
                format!("{{ app: [{{ name: 'web', script: './srv', env: '{secret}' }}] }}"),
            ),
        ];
        for (format, text) in cases {
            let err = Flockfile::parse(&text, format)
                .expect_err("an `env` that is a bare string must be refused");
            let msg = err.to_string();
            assert!(
                !msg.contains(secret),
                "{format:?}: the refusal leaked the value: {msg}"
            );
            assert_must_be_a_table(&err, "env");
        }
    }

    /// fails if a typo anywhere else stops failing loudly.
    ///
    /// Exactly one more key is legal, which is the whole reason the table is
    /// nested under one name rather than allowing loose top-level keys.
    #[test]
    fn a_key_that_is_not_dog_still_fails() {
        let src = r#"
[build]
command = "npm run build"

[[app]]
name = "web"
script = "./srv"
"#;
        let err = Flockfile::parse(src, FlockFormat::Toml)
            .expect_err("an unknown top-level key must still be refused");
        assert!(
            format!("{err}").contains("build"),
            "the refusal must name the key: {err}"
        );
    }

    #[test]
    fn a_schema_key_is_accepted_and_ignored() {
        let src = r#"{ "$schema": "./flockfile.schema.json",
                       "app": [{ "name": "web", "script": "./srv" }] }"#;
        let flock = Flockfile::parse(src, FlockFormat::Json).unwrap();
        assert_eq!(flock.apps.len(), 1);
    }

    /// fails if the new field is implemented by relaxing
    /// `deny_unknown_fields` instead of naming one more key, which would
    /// silently accept every typo the document lock exists to catch.
    #[test]
    fn one_more_key_is_legal_and_no_others_are() {
        let src = r#"{ "schema": "x", "app": [{ "name": "w", "script": "./s" }] }"#;
        assert!(
            matches!(
                Flockfile::parse(src, FlockFormat::Json),
                Err(FlockfileError::Parse {
                    format: FlockFormat::Json,
                    ..
                })
            ),
            "bare `schema` (no $) must still be an unknown field"
        );
    }

    #[test]
    fn a_toml_flockfile_takes_the_key_too() {
        let src = "\"$schema\" = \"./flockfile.schema.json\"\n\
                   [[app]]\nname = \"web\"\nscript = \"./srv\"\n";
        assert_eq!(
            Flockfile::parse(src, FlockFormat::Toml).unwrap().apps.len(),
            1
        );
    }
}
