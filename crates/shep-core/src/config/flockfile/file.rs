//! [`Flockfile`]: the parsed document, and the two entry points that build one.
//!
//! [`Flockfile::parse`] returns the flock; [`Flockfile::parse_declared`] also
//! reports which keys each app table wrote, which the config alone cannot say.

use crate::config::AppConfig;

use super::{
    declared_app::DeclaredApp, error::FlockfileError, format::FlockFormat, parse::parse_into,
    raw::RawFlockfile,
};
/// A parsed Flockfile: the declared flock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flockfile {
    /// App entries in declaration order
    pub apps: Vec<AppConfig>,
}

impl Flockfile {
    /// Parses Flockfile source text in the given format.
    ///
    /// # Errors
    /// - Format variants ([`FlockfileError::Toml`] etc.): backend parse
    ///   failure, carrying the backend's message. Json5 additionally rejects
    ///   sources nested past a depth of 64 before reaching the backend
    ///   parser, whose recursive-descent stack-overflows on deep input
    ///   rather than returning an error.
    /// - [`FlockfileError::NoApps`]: parsed fine but declared no apps.
    /// - [`FlockfileError::UnknownKeys`]: named a key no field claims.
    pub fn parse(source: &str, format: FlockFormat) -> Result<Self, FlockfileError> {
        Ok(Self {
            apps: parse_nonempty_apps(source, format)?,
        })
    }

    /// Parses `text` and reports, per app, which keys the document wrote.
    ///
    /// Runs the same per-format parse and validation [`Flockfile::parse`]
    /// does, then separately deserializes the same source into a
    /// [`serde_json::Value`] and reads each app table's keys off it:
    /// `AppConfig`'s `#[serde(default)]` erases which keys a document
    /// actually named, which is exactly what the value pass recovers.
    ///
    /// # Errors
    /// Every error [`Flockfile::parse`] returns, for the same inputs.
    pub fn parse_declared(
        text: &str,
        format: FlockFormat,
    ) -> Result<Vec<DeclaredApp>, FlockfileError> {
        // Same reasoning as `Flockfile::parse`: this reads a Flockfile off
        // disk (the reload/muster path in shep-cli), not a value off the
        // wire, so a typo here must still be loud.
        let apps = parse_nonempty_apps(text, format)?;

        // A document that reached this point already parsed successfully
        // into `RawFlockfile` above, so the same source deserializing into a
        // generic `Value` cannot fail for a reason the `RawFlockfile` pass
        // would not already have caught.
        let value = parse_into::<serde_json::Value>(text, format)?;
        let tables: Vec<Option<&serde_json::Map<String, serde_json::Value>>> = value
            .get("app")
            .and_then(serde_json::Value::as_array)
            .map(|apps| apps.iter().map(serde_json::Value::as_object).collect())
            .unwrap_or_default();

        Ok(apps
            .into_iter()
            .enumerate()
            .map(|(index, config)| {
                let table = tables.get(index).copied().flatten();
                let declared = table
                    .map(|t| t.keys().cloned().collect())
                    .unwrap_or_default();
                let declared_env = table
                    .and_then(|t| t.get("env"))
                    .and_then(serde_json::Value::as_object)
                    .map(|e| e.keys().cloned().collect())
                    .unwrap_or_default();
                DeclaredApp {
                    config,
                    declared,
                    declared_env,
                }
            })
            .collect())
    }
}

/// The apps a Flockfile declares, refusing a document that declares none.
///
/// Both public entry points start here. The top-level fields that are not
/// apps are discarded by name, so the next one to arrive — `$schema` and
/// `dog` both came in this way, and `deny_unknown_fields` means the next one
/// must too — is a change to `RawFlockfile` and to this destructure, rather
/// than to two callers that have to stay in step.
fn parse_nonempty_apps(
    source: &str,
    format: FlockFormat,
) -> Result<Vec<AppConfig>, FlockfileError> {
    let RawFlockfile {
        schema: _schema,
        // Discarded by name. Whatever a dog wrote under `[dog]` is that
        // dog's to read out of the file itself; shep only had to stop
        // refusing the document for containing it.
        dog: _dog,
        apps,
    } = RawFlockfile::new(source, format)?;
    if apps.is_empty() {
        return Err(FlockfileError::NoApps);
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flockfile::format::FlockFormat;

    #[test]
    fn toml_array_of_tables() {
        let src = r#"
[[app]]
name = "web"
script = "./srv"

[[app]]
name = "worker"
script = "python3"
args = ["job.py"]
"#;
        let flock = Flockfile::parse(src, FlockFormat::Toml).unwrap();
        assert_eq!(flock.apps.len(), 2);
        assert_eq!(flock.apps[1].name, "worker");
    }

    #[test]
    fn json_and_json5_and_yaml() {
        let json = r#"{ "app": [{ "name": "web", "script": "./srv" }] }"#;
        assert_eq!(
            Flockfile::parse(json, FlockFormat::Json)
                .unwrap()
                .apps
                .len(),
            1
        );

        let json5 = r#"{ app: [{ name: "web", script: "./srv" }], /* comment */ }"#;
        assert_eq!(
            Flockfile::parse(json5, FlockFormat::Json5)
                .unwrap()
                .apps
                .len(),
            1
        );

        let yaml = "app:\n  - name: web\n    script: ./srv\n";
        assert_eq!(
            Flockfile::parse(yaml, FlockFormat::Yaml)
                .unwrap()
                .apps
                .len(),
            1
        );
    }

    #[test]
    fn empty_app_list_is_an_error() {
        assert_eq!(
            Flockfile::parse("app: []\n", FlockFormat::Yaml).unwrap_err(),
            FlockfileError::NoApps
        );
    }

    #[test]
    fn parse_errors_carry_the_backend_message() {
        match Flockfile::parse("not toml [[", FlockFormat::Toml).unwrap_err() {
            FlockfileError::Toml(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Toml error, got {other:?}"),
        }
    }

    /// fails if the declared key set is inferred from values rather than read
    /// from the document. `autorestart = true` is also the default, so a
    /// parser that reports "fields that differ from Default" would miss it,
    /// and a later file load would then overwrite an operator who had
    /// turned it off.
    #[test]
    fn declared_reports_keys_the_document_wrote_even_at_their_default() {
        let text = r#"
[[app]]
name = "web"
script = "./srv"
autorestart = true
"#;
        let apps = Flockfile::parse_declared(text, FlockFormat::Toml).unwrap();
        assert_eq!(apps.len(), 1);
        let declared = &apps[0].declared;
        assert!(declared.contains("autorestart"), "declared: {declared:?}");
        assert!(declared.contains("name"));
        assert!(declared.contains("script"));
        assert!(
            !declared.contains("max_memory"),
            "a key nobody wrote is not declared"
        );
        assert_eq!(declared.len(), 3);
    }

    /// fails if env keys are not reported separately. `env` is the only map
    /// of user-supplied keys in `AppConfig`, so the merge treats it one
    /// level deeper than every other field.
    #[test]
    fn declared_env_reports_the_keys_inside_the_env_table() {
        let text = r#"
[[app]]
name = "web"
script = "./srv"
env = { DB_HOST = "", NODE_ENV = "production" }
"#;
        let apps = Flockfile::parse_declared(text, FlockFormat::Toml).unwrap();
        assert_eq!(
            apps[0].declared_env.iter().collect::<Vec<_>>(),
            vec!["DB_HOST", "NODE_ENV"]
        );
        assert!(apps[0].declared.contains("env"));
    }

    /// fails if a format other than TOML loses the key set. All four go
    /// through one generic intermediate, so a regression here means the
    /// intermediate was bypassed for a format.
    #[test]
    fn declared_survives_every_parse_format() {
        let cases: [(FlockFormat, &str); 4] = [
            (
                FlockFormat::Toml,
                "[[app]]\nname = \"web\"\nscript = \"./srv\"\nautorestart = true\n",
            ),
            (
                FlockFormat::Yaml,
                "app:\n  - name: web\n    script: ./srv\n    autorestart: true\n",
            ),
            (
                FlockFormat::Json,
                r#"{"app":[{"name":"web","script":"./srv","autorestart":true}]}"#,
            ),
            (
                FlockFormat::Json5,
                "{ app: [{ name: \"web\", script: \"./srv\", autorestart: true }] }",
            ),
        ];
        for (format, text) in cases {
            let apps = Flockfile::parse_declared(text, format)
                .unwrap_or_else(|e| panic!("{format:?} failed to parse: {e}"));
            assert!(
                apps[0].declared.contains("autorestart"),
                "{format:?}: declared {:?}",
                apps[0].declared
            );
        }
    }

    /// A typo in a Flockfile must still be loud. This is the whole reason
    /// `deny_unknown_fields` was there.
    #[test]
    fn a_misspelled_flockfile_key_is_refused_and_named() {
        let err = Flockfile::parse(
            "[[app]]\nname = \"web\"\nscript = \"./srv\"\nmax_restrts = 5\n",
            FlockFormat::Toml,
        )
        .expect_err("a typo must be refused");
        let FlockfileError::UnknownKeys { keys } = err else {
            panic!("expected UnknownKeys, got {err:?}");
        };
        assert!(
            keys.iter().any(|k| k.contains("max_restrts")),
            "got {keys:?}"
        );
    }

    /// Nesting is why this uses serde_ignored rather than a key list.
    ///
    /// `kind`/`target` are supplied alongside the typo: both are required by
    /// `ProbeConfig` with no default, and omitting them would surface a
    /// missing-field error instead of the unknown-key one this test means to
    /// exercise.
    #[test]
    fn a_misspelled_key_inside_a_probe_is_also_named() {
        let err = Flockfile::parse(
            "[[app]]\nname = \"web\"\nscript = \"./srv\"\n[app.readiness_probe]\nkind = \"http\"\ntarget = \"http://localhost/x\"\ntimeuot = \"5s\"\n",
            FlockFormat::Toml,
        )
        .expect_err("a nested typo must be refused");
        let FlockfileError::UnknownKeys { keys } = err else {
            panic!("expected UnknownKeys, got {err:?}");
        };
        // The variant alone would pass on any path at all, including one
        // from a different app. Nesting is the reason this parse goes
        // through `serde_ignored` rather than a flat key list, so the
        // path is the thing worth asserting.
        assert!(
            keys.iter()
                .any(|key| key.contains("readiness_probe") && key.contains("timeuot")),
            "the nested path should name both the probe and the key: {keys:?}"
        );
    }

    /// The design goal is one refusal naming every typo, not one refusal
    /// per run. Two misspelled keys in one document must both show up in
    /// the single [`FlockfileError::UnknownKeys`] this produces.
    #[test]
    fn two_misspelled_keys_are_both_named_in_one_error() {
        let err = Flockfile::parse(
            "[[app]]\nname = \"web\"\nscript = \"./srv\"\nmax_restrts = 5\ninstences = 2\n",
            FlockFormat::Toml,
        )
        .expect_err("both typos must be refused");
        let FlockfileError::UnknownKeys { keys } = err else {
            panic!("expected UnknownKeys, got {err:?}");
        };
        assert!(
            keys.iter().any(|k| k.contains("max_restrts")),
            "got {keys:?}"
        );
        assert!(keys.iter().any(|k| k.contains("instences")), "got {keys:?}");
    }
}
