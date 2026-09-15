//! Rows for `shep get`/`shep set`'s KV store, `shep secret`'s slots, and
//! `shep dog available`'s listing.

use serde::Serialize;

use crate::dog_index::AvailableDog;
use crate::output::Render;

/// One row of `shep get`'s whole-store listing.
///
/// A named-field struct rather than a `(String, String)`: a tuple serializes
/// to a JSON array, and a consumer should read `key`/`value` by name.
#[derive(Debug, Serialize)]
pub struct KvEntry {
    /// The key, exactly as stored; [`shep_core::kv`]'s grammar has already
    /// validated it.
    pub key: String,
    /// Its value.
    pub value: String,
}

/// `shep get`'s whole-store listing (bare `shep get`), or one key's own entry
/// (`shep get <key>`).
///
/// `transparent`, so the JSON is a plain array of [`KvEntry`] objects rather
/// than the map a consumer would have to special-case. Never from a
/// `Response`: the store never touches the wire.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct KvRows(pub Vec<KvEntry>);

/// No colour: a key and a value are operator data, and shep has no opinion
/// about either.
impl Render for KvRows {
    fn headers() -> &'static [&'static str] {
        &["KEY", "VALUE"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|entry| vec![entry.key.clone(), entry.value.clone()])
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "KEY" => "key",
            "VALUE" => "value",
            other => panic!("KvRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns: a key with no value is not a row at all.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// One row of `shep secret list`: a key, and the environments holding a
/// value for it.
///
/// No value field, and no row type in this module has one: `shep secret get`
/// writes the value it resolved straight to stdout, so a stored value never
/// reaches a struct a `{:?}` or a table render could leak it through
/// (IR-41). A key name and an environment name are neither of them the
/// secret.
#[derive(Debug, Serialize)]
pub struct SecretKeyRow {
    /// The key, exactly as stored; [`shep_core::secrets`]'s grammar has
    /// already validated it.
    pub key: String,
    /// The environments it has a value for, in the store's own order.
    pub environments: Vec<String>,
}

/// `shep secret list`'s whole-store listing.
///
/// `transparent`, for [`KvRows`]' reason: a plain array of [`SecretKeyRow`]
/// objects rather than the map a consumer would have to special-case. Never
/// from a `Response`: the store never touches the wire.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SecretKeyRows(pub Vec<SecretKeyRow>);

/// No colour: a key and the environments naming it are operator data, and
/// shep has no opinion about either.
impl Render for SecretKeyRows {
    fn headers() -> &'static [&'static str] {
        &["KEY", "ENVIRONMENTS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| vec![row.key.clone(), row.environments.join(", ")])
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "KEY" => "key",
            "ENVIRONMENTS" => "environments",
            other => panic!("SecretKeyRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns: a key with no environment holds nothing and is not
    // stored at all.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// The slot `shep secret set` wrote, or `shep secret unset` emptied.
///
/// The key and the environment, never the value: echoing a credential back
/// would put it in the scrollback of every run that stored one, and in the
/// output of every script that pipes shep somewhere.
#[derive(Debug, Serialize)]
pub struct SecretSlotRow {
    /// The key that was written or removed.
    pub key: String,
    /// The environment whose slot it was.
    pub environment: String,
}

/// No colour, for [`KvRows`]' reason: both cells are operator data.
impl Render for SecretSlotRow {
    fn headers() -> &'static [&'static str] {
        &["KEY", "ENVIRONMENT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![self.key.clone(), self.environment.clone()]]
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "KEY" => "key",
            "ENVIRONMENT" => "environment",
            other => panic!("SecretSlotRow::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns, and the pair is the slot's whole identity.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// `shep secret get`'s `--format json` payload: the key and the value it
/// resolved.
///
/// The one row type in this module that carries a value. `get`'s table form
/// never builds one: it writes the bare value straight to stdout, for the
/// `DB_PASSWORD=$(shep secret get DB_PASSWORD)` case. Its JSON form has to
/// answer the same output-envelope contract every other command does
/// (`web/src/pages/docs/json-output.astro`), which means a payload type,
/// which means `Debug` needs its own redaction (IR-41): `derive(Debug)`
/// would print the value in a panic message, a test failure, or a `dbg!`.
///
/// `Serialize` is this type's only path to a plaintext value, and both it
/// and the table form above are reached only through a `secret get` that
/// `[secrets] allow_read` has already let through. `Debug` prints
/// `<redacted>` instead of the value.
#[derive(Serialize)]
pub struct SecretValueRow {
    /// The key, exactly as stored.
    pub key: String,
    /// The value `get` resolved.
    pub value: String,
}

/// Redacted (IR-41): `value` is a credential.
impl std::fmt::Debug for SecretValueRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretValueRow")
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// No colour, for [`KvRows`]' reason: both cells are operator data.
impl Render for SecretValueRow {
    fn headers() -> &'static [&'static str] {
        &["KEY", "VALUE"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![self.key.clone(), self.value.clone()]]
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "KEY" => "key",
            "VALUE" => "value",
            other => panic!("SecretValueRow::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns, and the pair is the whole answer.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// The verdict [`DescribedSecret::status`] carries: whether a reference
/// currently resolves, and if not, whether that is a known absence or
/// simply invisible to this command from here.
///
/// Serializes `snake_case`, matching this crate's other JSON enums (a dog's
/// `kind`, for one). [`Self::as_table_word`] is the separate, human-prose
/// spelling `describe`'s table form prints; the two are kept apart on
/// purpose; a JSON reader should never have to translate table wording, and
/// a table reader should never see an underscore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    /// The operator's store or a provider's namespace holds a value for
    /// this reference in this environment.
    Resolved,
    /// The operator's store holds nothing for this key, or a provider has
    /// pushed this namespace for this environment and that push lacks the
    /// key.
    Missing,
    /// No push for this namespace and this environment is in the local
    /// cache file this command reads. A provider dog that pushed with
    /// `persist = false` never reaches that cache, and one that has pushed
    /// another environment first has not pushed this pair yet, so this is
    /// not proof the running shepherd lacks the value too, only that this
    /// command cannot see it from here.
    Uncached,
}

impl SecretStatus {
    /// Classifies a live [`shep_core::secrets::Resolution`] the same way
    /// everywhere this crate reports one, so the table and JSON forms of
    /// `describe`'s secrets section can never disagree about a verdict.
    #[must_use]
    pub fn from_resolution(resolution: &shep_core::secrets::Resolution<'_>) -> Self {
        match resolution {
            shep_core::secrets::Resolution::Found(_) => Self::Resolved,
            shep_core::secrets::Resolution::MissingKey => Self::Missing,
            shep_core::secrets::Resolution::MissingNamespace => Self::Uncached,
        }
    }

    /// The word `describe`'s table prints for this verdict.
    #[must_use]
    pub fn as_table_word(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Missing => "missing",
            Self::Uncached => "not cached; a provider may still have it",
        }
    }
}

/// One `{{secret:...}}` reference `shep describe` reports on: never a
/// value, only whether it currently resolves.
///
/// Not a [`Render`] payload: `describe`'s table form prints these as prose
/// under the flock table, the same way `Pending`/`Overridden` do, and its
/// JSON form rides beside `data` on the envelope rather than inside it, so
/// existing `data[].name` scripts see no shape change. `emit_described`
/// builds both from a `&[DescribedSecret]` directly.
#[derive(Debug, Clone, Serialize)]
pub struct DescribedSecret {
    /// Which sheep this reference belongs to.
    pub name: String,
    /// The reference as the operator wrote it: `KEY` or `namespace/KEY`.
    pub reference: String,
    /// The environment it resolved in.
    pub environment: String,
    /// Whether this reference currently resolves, and if not, why.
    pub status: SecretStatus,
}

/// `shep dogs --available`'s community-index listing.
///
/// Never from a `Response`: the community index never touches the daemon
/// wire. `dog_index` is the security boundary that sanitises every string
/// [`AvailableDog`] carries, so this impl clones fields through unescaped.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct AvailableDogRows(pub Vec<AvailableDog>);

/// No colour: a catalogue of dogs an operator could adopt, with no signal
/// column for chrome such as CATEGORY to recede behind.
impl Render for AvailableDogRows {
    fn headers() -> &'static [&'static str] {
        &["NAME", "PACKAGE", "CATEGORY", "DESCRIPTION"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|dog| {
                vec![
                    dog.name.clone(),
                    dog.package.clone(),
                    dog.category.clone(),
                    dog.description.clone(),
                ]
            })
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "NAME" => "name",
            "PACKAGE" => "package",
            "CATEGORY" => "category",
            "DESCRIPTION" => "description",
            other => panic!("AvailableDogRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // The name an adopt line is built from; a table has no room to say
        // how it differs from NAME.
        "adopt_as",
        // Long and rarely glanced at in a row; the detail view is where an
        // operator reads them.
        "repo", "license",
        // The tagged `DogSourceKind` the detail view's install line is built
        // from.
        "source",
    ];

    // NAME and PACKAGE identify a row, so both sit at the floor. DESCRIPTION,
    // the unbounded free-text field, drops before CATEGORY.
    const PRIORITIES: &'static [u8] = &[0, 0, 6, 7];
}

/// `shep unset`'s own report: how many keys the store lost.
///
/// A count rather than the keys: `shep_core::kv::clear` never materializes
/// the set it empties, so a single key and `--all` share one shape.
#[derive(Debug, Serialize)]
pub struct KvUnsetRow {
    /// How many keys were removed: always `1` for a single-key `unset`, which
    /// exits [`crate::exit::ExitCode::NotFound`] when the key was absent, and
    /// `shep_core::kv::clear`'s own count for `--all`.
    pub removed: u32,
}

/// No colour, for [`DeletedIds`]' reason: one column, which is also the whole
/// content. `removed` is a count rather than an outcome, and `0` reads as
/// `0`.
impl Render for KvUnsetRow {
    fn headers() -> &'static [&'static str] {
        &["REMOVED"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![self.removed.to_string()]]
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "REMOVED" => "removed",
            other => panic!("KvUnsetRow::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // One column, and it is the row's whole identity.
    const PRIORITIES: &'static [u8] = &[0];
}

#[cfg(test)]
mod tests {
    use super::super::tests::assert_no_drift;
    use super::*;

    /// Neither column is a rendering of anything else, so `formatted` is
    /// empty.
    #[test]
    fn kv_rows_do_not_drift() {
        let rows = KvRows(vec![KvEntry {
            key: "bark.cooldown".to_string(),
            value: "30s".to_string(),
        }]);
        assert_no_drift(&rows, |j| &j[0], &[]);
    }

    #[test]
    fn kv_unset_row_does_not_drift() {
        assert_no_drift(&KvUnsetRow { removed: 2 }, |j| j, &[]);
    }

    /// ENVIRONMENTS is a joined rendering of a JSON array, so it is
    /// `formatted` rather than compared cell against field.
    #[test]
    fn secret_key_rows_do_not_drift() {
        let rows = SecretKeyRows(vec![SecretKeyRow {
            key: "DB_PASSWORD".to_string(),
            environments: vec!["all".to_string(), "staging".to_string()],
        }]);
        assert_no_drift(&rows, |j| &j[0], &["ENVIRONMENTS"]);
    }

    #[test]
    fn secret_slot_row_does_not_drift() {
        let row = SecretSlotRow {
            key: "DB_PASSWORD".to_string(),
            environment: "staging".to_string(),
        };
        assert_no_drift(&row, |j| j, &[]);
    }

    #[test]
    fn secret_value_row_does_not_drift() {
        let row = SecretValueRow {
            key: "DB_PASSWORD".to_string(),
            value: "hunter2".to_string(),
        };
        assert_no_drift(&row, |j| j, &[]);
    }

    /// fails if `SecretKeyRows`/`SecretSlotRow` grow a field that carries
    /// the value itself, or if `SecretValueRow` stops redacting the one
    /// value it does carry. All three are rendered to a terminal and to
    /// `--format json`, so a value landing in the wrong place is a
    /// credential in a log or a pipeline.
    #[test]
    fn only_secret_value_row_carries_a_value_and_its_debug_is_redacted() {
        assert!(!SecretKeyRows::headers().contains(&"VALUE"));
        assert!(!SecretSlotRow::headers().contains(&"VALUE"));
        let json = serde_json::to_string(&SecretSlotRow {
            key: "K".to_string(),
            environment: "all".to_string(),
        })
        .unwrap();
        assert!(!json.contains("value"), "{json}");

        let row = SecretValueRow {
            key: "K".to_string(),
            value: "hunter2".to_string(),
        };
        let rendered = format!("{row:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        // Exact string pinned so a lazy derive(Debug) refactor fails here,
        // matching `secrets::SecretFile`'s own redacted `Debug`.
        assert_eq!(
            rendered,
            r#"SecretValueRow { key: "K", value: "<redacted>" }"#
        );
    }

    /// The live index's single entry (`web/public/dogs.json`).
    fn sample_available_dog() -> AvailableDog {
        AvailableDog {
            name: "Spot".to_string(),
            package: "shep-log-rotate".to_string(),
            adopt_as: "log-rotate".to_string(),
            description: "Rotates grown log files and asks the shepherd to reopen them."
                .to_string(),
            repo: "https://github.com/shep-pm/shep-log-rotate".to_string(),
            license: "MIT OR Apache-2.0".to_string(),
            category: "logs".to_string(),
            source: crate::dog_index::DogSourceKind::CargoGit {
                url: "https://github.com/shep-pm/shep-log-rotate".to_string(),
            },
        }
    }

    /// `adopt_as`/`repo`/`license`/`source` serialize but are covered by
    /// `JSON_ONLY` rather than a column. The four real columns are plain
    /// strings, so `formatted` is empty.
    #[test]
    fn available_dog_rows_do_not_drift() {
        assert_no_drift(
            &AvailableDogRows(vec![sample_available_dog()]),
            |j| &j[0],
            &[],
        );
    }

    // --- PRIORITIES ------------------------------------------------------
}
