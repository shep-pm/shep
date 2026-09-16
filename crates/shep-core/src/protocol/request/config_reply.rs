//! What a config request answers with: drift, what applied, what was refused, and the view.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Request, Response};

/// One registered sheep whose stored config differs from a caller's copy:
/// the answer [`Request::ConfigDrift`] is asking for
///
/// Field names only, never their values. This is printed at an operator,
/// and [`AppConfig::env`](crate::config::AppConfig::env) carries secrets,
/// so a differing `env` reports `"env"` and nothing more. `Debug` is
/// derived: there is nothing here to redact.
// wire format: changing field names is a breaking change
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheepDrift {
    /// The sheep's name. Both configs share it by construction: it is what
    /// matched them to each other.
    pub name: String,
    /// The [`AppConfig`] fields that differ, in field-name order. Never
    /// empty: a sheep with nothing to report is left out of the answer.
    pub fields: Vec<String>,
}

impl SheepDrift {
    /// Builds one sheep's report.
    #[must_use]
    pub fn new(name: impl Into<String>, fields: Vec<String>) -> Self {
        Self {
            name: name.into(),
            fields,
        }
    }
}

/// What one app's [`Request::ApplyConfig`] did: the answer a load owes the
/// operator who ran it
///
/// One of these per app the request named, found or not and changed or not.
///
/// [`Self::applied`] and [`Self::pending`] carry field names only, never
/// their values, as [`SheepDrift`] does; the merged config never reaches a
/// client. [`Self::refused`] is prose and is scoped out of that rule: it
/// quotes values out of the file the caller just sent, never out of the
/// flock's stored config. `Debug` is derived on that basis: nothing here
/// needs redacting.
// wire format: changing field names is a breaking change
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheepApplied {
    /// The sheep's name, exactly as the request spelled it.
    pub name: String,
    /// Fields now in force, in field-name order. Empty when the load changed
    /// nothing the daemon could act on immediately.
    pub applied: Vec<String>,
    /// Fields the app picks up at its next spawn, in field-name order. Empty
    /// when nothing is waiting.
    ///
    /// `shep reload <name>` promotes them; a client rendering this list says
    /// so, since a pending list with no remedy beside it cannot be acted on.
    pub pending: Vec<String>,
    /// Why some or all of this app's change did not land, in the daemon's own
    /// words, or `None` when the whole of it did.
    ///
    /// Not the same question as the two lists being empty: a refusal raised
    /// before anything was touched leaves both empty, and so does a load with
    /// nothing to do. It is a sentence rather than a code because the message
    /// is what tells them apart.
    pub refused: Option<String>,
}

impl SheepApplied {
    /// Builds one app's report.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        applied: Vec<String>,
        pending: Vec<String>,
        refused: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            applied,
            pending,
            refused,
        }
    }
}

/// One app a multi-sheep reload or restart could not accept, and why
///
/// A staged walk asks the supervisor per app, so an app already reloading is
/// refused on its own while the rest of the fold goes ahead, and so is one
/// that left the flock after the walk was planned. One of these per refused
/// app rides back in [`Response::Reloading`] or [`Response::Restarted`],
/// which is what lets the client name the app and exit non-zero instead of
/// printing a table with a row quietly missing from it.
///
/// [`Self::reason`] is the daemon's own sentence rather than a code, the
/// rule [`SheepApplied::refused`] takes and for its reason: the class of
/// refusal is not on the wire, and the message is what tells two of them
/// apart. `Debug` is derived; a name and a refusal sentence carry no env,
/// no path and no argument vector.
// wire format: changing field names is a breaking change
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheepRefusal {
    /// The app's name, as the walk that planned the reload or the restart
    /// spelled it.
    pub name: String,
    /// Why that app was refused, in the daemon's own words.
    pub reason: String,
}

impl SheepRefusal {
    /// Builds one app's refusal.
    #[must_use]
    pub fn new(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reason: reason.into(),
        }
    }
}

/// One sheep's effective config as a pane sees it: every field but env's
/// values, plus which fields an operator has overridden and which are
/// waiting on a respawn.
///
/// The answer to [`Request::SheepConfig`], and the one reply in this module
/// that carries a whole [`AppConfig`]. [`SheepApplied`] deliberately carries
/// field names alone, and the difference is what each is for: that one is
/// printed at an operator who already has the file, this one feeds a pane
/// that is about to edit fields it has to be able to show first.
// wire format: changing field names is a breaking change
//
// `#[non_exhaustive]`: shep-core is a published library and a sixth field
// would otherwise break an out-of-tree consumer's construction of this with
// no version bump to say so (IR-20). [`SheepConfigView::new`] is how the
// daemon builds one, and it is what enforces the emptied `env`.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheepConfigView {
    /// The sheep's name.
    pub name: String,
    /// The effective config with `env` cleared. Every remaining field is
    /// operator-supplied policy the pane is about to let them edit, so
    /// withholding a value would make the pane unusable while protecting
    /// nothing.
    pub config: AppConfig,
    /// The env keys, so the pane can list them. Never the values.
    pub env_keys: Vec<String>,
    /// Which of [`Self::env_keys`] resolve from the secret store, so a pane
    /// can mark the row without showing anything. Recorded before `env` is
    /// cleared, which is the only moment the values exist to be read.
    pub env_secrets: Vec<String>,
    /// Field names an operator has set that the Flockfile does not declare.
    pub overridden: Vec<String>,
    /// Field names parked until the next respawn.
    pub pending: Vec<String>,
}

impl SheepConfigView {
    /// Builds one, clearing `env` and recording its keys.
    ///
    /// The clearing happens here rather than at the one call site, so a
    /// second caller cannot forget it: this constructor is the only way to
    /// build the type outside this crate, since `#[non_exhaustive]` blocks
    /// a literal.
    #[must_use]
    pub fn new(mut config: AppConfig, overridden: Vec<String>, pending: Vec<String>) -> Self {
        let env_secrets = crate::secrets::sealed_keys(&config);
        let env_keys = config.env.keys().cloned().collect();
        config.env.clear();
        Self {
            name: config.name.clone(),
            config,
            env_keys,
            env_secrets,
            overridden,
            pending,
        }
    }
}

/// Redacted (IR-41): `config` carries `args` and `cwd`, which routinely hold
/// a token or a home directory, and this type is what a `{:?}` on a
/// [`Response`] would print. The four lists are counted rather than named
/// for the same reason: `env_keys` and `env_secrets` are key sets, which are
/// themselves worth keeping out of a log.
impl fmt::Debug for SheepConfigView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SheepConfigView {{ name: {:?}, env_keys: {}, env_secrets: {}, overridden: {}, pending: {} }}",
            self.name,
            self.env_keys.len(),
            self.env_secrets.len(),
            self.overridden.len(),
            self.pending.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane edits everything else about a sheep, so the config itself
    /// has to travel; `env` is the one map in it that holds secrets, and
    /// the keys travel while the values never do (IR-41).
    #[test]
    fn a_sheep_config_view_never_carries_an_env_value() {
        let mut config = AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("DB_PASS".to_string(), "hunter2".to_string());
        let view = SheepConfigView::new(config, Vec::new(), Vec::new());
        assert!(view.config.env.is_empty());
        assert_eq!(view.env_keys, ["DB_PASS"]);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
    }

    /// A `{:?}` on a `Response` reaches it, and `config` holds `args` and
    /// `cwd` as well as the env keys (IR-41).
    #[test]
    fn a_sheep_config_views_debug_is_the_exact_redacted_string() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".to_string(), "1".to_string());
        let view = SheepConfigView::new(config, vec!["max_restarts".to_string()], Vec::new());
        assert_eq!(
            format!("{view:?}"),
            r#"SheepConfigView { name: "web", env_keys: 1, env_secrets: 0, overridden: 1, pending: 0 }"#
        );
    }

    /// Recorded before the clear, since the values are what name a reference
    /// and they are gone by the time anything else can look.
    #[test]
    fn a_config_view_records_which_env_keys_are_sealed() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PLAIN".into(), "value".into());
        config.env.insert("SEALED".into(), "{{secret:PW}}".into());
        let view = SheepConfigView::new(config, Vec::new(), Vec::new());
        assert!(view.config.env.is_empty());
        assert_eq!(view.env_keys, ["PLAIN", "SEALED"]);
        assert_eq!(view.env_secrets, ["SEALED"]);
    }

    /// Asserts on the JSON, not the struct: a `Vec<String>` cannot say which
    /// of the two a string is, so a build carrying a value would typecheck.
    #[test]
    fn a_sheep_applied_carries_names_and_never_values() {
        let applied = SheepApplied::new(
            "web",
            vec!["cwd".to_string()],
            vec!["env".to_string()],
            None,
        );
        let json = serde_json::to_string(&applied).unwrap();
        assert!(json.contains("\"env\""), "the NAME travels: {json}");
        assert!(
            !json.contains("DATABASE_URL"),
            "and no value ever does: {json}"
        );
    }

    #[test]
    fn a_sheep_applied_debug_prints_the_names_it_was_given() {
        let applied = SheepApplied::new("web", vec!["cwd".to_string()], Vec::new(), None);
        assert_eq!(
            format!("{applied:?}"),
            "SheepApplied { name: \"web\", applied: [\"cwd\"], pending: [], refused: None }"
        );
    }
}
