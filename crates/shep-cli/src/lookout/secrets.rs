//! What the secrets pane draws, computed off the files it reads.
//!
//! No value reaches the model. A row carries a length, and the value
//! itself is read back one at a time by [`stored_value`], for a reveal that
//! has already passed the gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use shep_core::config::DaemonConfig;
use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use shep_core::secrets::{self, ALL_ENVIRONMENTS};

use crate::secret_readers::{self, Reader};

/// Which store a row came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    /// `secrets.json`, the operator's own. Writable.
    Operator,
    /// One provider dog's pushed values, out of `secrets-cache.json`.
    /// Read-only: they are a cache of what the dog said.
    Namespace(String),
}

/// One key, as this pane's current environment tab sees it.
///
/// `Debug` is derived and safe: `byte_len` is a length, and no field holds
/// a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretRow {
    /// The key, `namespace/KEY` for a provider row.
    pub key: String,
    /// Which store it came from.
    pub source: Source,
    /// The environment slot supplying this tab's value: the tab's own
    /// name, [`ALL_ENVIRONMENTS`], or `None` when nothing resolves here.
    ///
    /// Mirrors `SecretView::resolve`'s order, which is exact environment,
    /// then `all`, then nothing, and never another named environment.
    pub in_force: Option<String>,
    /// Every environment with a slot for this key, in name order.
    pub set_in: Vec<String>,
    /// The in-force value's length in bytes, or `None` when nothing
    /// resolves here.
    pub byte_len: Option<usize>,
    /// The flock that names this key, in name order.
    pub readers: Vec<Reader>,
}

/// Everything the pane needs for one environment tab.
#[derive(Debug, Clone, Default)]
pub(crate) struct SecretsModel {
    /// Every environment either store holds a slot for, the operator's own
    /// and the providers' cache alike, plus [`ALL_ENVIRONMENTS`], in name
    /// order. The tab row, and `SET IN`'s denominator.
    pub environments: Vec<String>,
    /// Operator rows first, then each namespace's, keys in order within
    /// each.
    pub rows: Vec<SecretRow>,
    /// Why the operator's store would not read, when it would not.
    ///
    /// The pane's band row shows this in place of the roll age below, when
    /// there is one.
    pub unreadable: Option<String>,
    /// How old the muster roll is, or `None` when it is missing.
    pub roll_age: Option<Duration>,
    /// `[secrets] allow_read` in `shep.toml`, the reveal gate. Missing or
    /// unreadable both read as `false`, mirroring
    /// `whistle::gate::resolve_control`'s fail-closed default.
    pub allow_read: bool,
    /// The operator store these rows came from, for [`stored_value`].
    pub store: PathBuf,
    /// The provider cache these rows came from, for [`stored_value`].
    pub provider_cache: PathBuf,
}

impl SecretsModel {
    /// This model's rows from one store, in key order.
    ///
    /// The pane's own group header reads this for a group's member count.
    pub fn rows_for<'a>(&'a self, source: &'a Source) -> impl Iterator<Item = &'a SecretRow> {
        self.rows.iter().filter(move |row| &row.source == source)
    }
}

/// Builds the model for `environment`.
///
/// Best-effort throughout. An unreadable operator store reports itself and
/// leaves the provider rows alone; a missing roll costs the readers and
/// nothing else.
pub(crate) fn model(paths: &ShepPaths, procs: &[ProcessInfo], environment: &str) -> SecretsModel {
    let (store, unreadable) = match secrets::all(&paths.secrets) {
        Ok(store) => (store, None),
        Err(error) => (BTreeMap::new(), Some(error.to_string())),
    };
    let providers = secrets::provider_cache_on_disk(&paths.secrets_cache);
    let readers = secret_readers::by_reference(paths, procs);

    let mut environments: BTreeSet<String> = BTreeSet::new();
    environments.insert(ALL_ENVIRONMENTS.to_string());
    for slots in store.values() {
        environments.extend(slots.keys().cloned());
    }
    // Both stores, not the operator's alone. A dog can push an environment
    // the operator never named, and this list is both `SET IN`'s denominator
    // and the tab row: leaving those out prints a count bigger than its own
    // denominator and hides the environment from every tab.
    for keys in providers.values.values() {
        for slots in keys.values() {
            environments.extend(slots.keys().cloned());
        }
    }

    let mut rows = Vec::new();
    for (key, slots) in &store {
        rows.push(row(
            key.clone(),
            Source::Operator,
            slots,
            environment,
            &readers,
        ));
    }
    for (namespace, keys) in &providers.values {
        for (key, slots) in keys {
            let qualified = format!("{namespace}/{key}");
            rows.push(row(
                qualified,
                Source::Namespace(namespace.clone()),
                slots,
                environment,
                &readers,
            ));
        }
    }

    let shep_toml = std::fs::read_to_string(&paths.daemon_config).ok();
    let allow_read = DaemonConfig::load(shep_toml.as_deref(), &|_| None)
        .is_ok_and(|config| config.secrets.allow_read);

    SecretsModel {
        environments: environments.into_iter().collect(),
        rows,
        unreadable,
        roll_age: secret_readers::roll_age(paths),
        allow_read,
        store: paths.secrets.clone(),
        provider_cache: paths.secrets_cache.clone(),
    }
}

/// The one value behind `row`, read back off disk for a reveal.
///
/// `None` when nothing resolves in this tab, when the store will not read,
/// and when the slot has gone since the model was built. Read on demand
/// rather than carried in the model: a value the pane holds for its whole
/// life is a value in every core dump of it, and the pane's life is as long
/// as the operator leaves it open. Its caller runs this off the UI task
/// ([`crate::lookout::app::Effect::RevealSecret`]), so the two file reads
/// here are never on the reducer.
///
/// The gate is the caller's ([`crate::lookout::app::App::reveal_gate_open`]):
/// this function does not check it.
pub(crate) fn stored_value(store: &Path, provider_cache: &Path, row: &SecretRow) -> Option<String> {
    let environment = row.in_force.as_deref()?;
    match &row.source {
        Source::Operator => secrets::get(store, &row.key, environment).ok().flatten(),
        // The row's key is `namespace/KEY`; the cache nests the two.
        Source::Namespace(namespace) => {
            let bare = row.key.strip_prefix(namespace)?.strip_prefix('/')?;
            secrets::provider_cache_on_disk(provider_cache)
                .values
                .get(namespace)?
                .get(bare)?
                .get(environment)
                .cloned()
        }
    }
}

/// One row from one key's environment slots.
fn row(
    key: String,
    source: Source,
    slots: &BTreeMap<String, String>,
    environment: &str,
    readers: &BTreeMap<String, Vec<Reader>>,
) -> SecretRow {
    let in_force = if slots.contains_key(environment) {
        Some(environment.to_string())
    } else if slots.contains_key(ALL_ENVIRONMENTS) {
        Some(ALL_ENVIRONMENTS.to_string())
    } else {
        None
    };
    let byte_len = in_force
        .as_ref()
        .and_then(|slot| slots.get(slot))
        .map(String::len);
    SecretRow {
        byte_len,
        in_force,
        set_in: slots.keys().cloned().collect(),
        readers: readers.get(&key).cloned().unwrap_or_default(),
        key,
        source,
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::AppConfig;
    use shep_core::secrets::{PROVIDER_CACHE_VERSION, Resolution, SecretRef, SecretView};

    use crate::secret_readers::test_support::{online, paths_under, write_roll};

    use super::*;

    #[test]
    fn in_force_agrees_with_secret_view_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "EXACT", "production", "a").unwrap();
        secrets::set(&paths.secrets, "FALLBACK", ALL_ENVIRONMENTS, "b").unwrap();
        secrets::set(&paths.secrets, "ELSEWHERE", "ci", "c").unwrap();
        secrets::set(&paths.secrets, "BOTH", "production", "exact").unwrap();
        secrets::set(&paths.secrets, "BOTH", ALL_ENVIRONMENTS, "fallback").unwrap();

        let built = model(&paths, &[], "production");
        let store = secrets::all(&paths.secrets).unwrap();
        let view = SecretView::new(
            "production".to_string(),
            store,
            secrets::provider_cache_on_disk(&paths.secrets_cache),
        );

        for row in &built.rows {
            let reference = SecretRef::parse(&row.key).unwrap();
            let resolved = matches!(view.resolve(&reference), Resolution::Found(_));
            assert_eq!(
                row.in_force.is_some(),
                resolved,
                "{} disagreed with resolve",
                row.key
            );
        }
        let by_key = |key: &str| {
            built
                .rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("{key} missing"))
        };
        assert_eq!(by_key("EXACT").in_force.as_deref(), Some("production"));
        assert_eq!(
            by_key("FALLBACK").in_force.as_deref(),
            Some(ALL_ENVIRONMENTS)
        );
        assert_eq!(by_key("ELSEWHERE").in_force, None);
        assert_eq!(
            by_key("BOTH").in_force.as_deref(),
            Some("production"),
            "a key with both slots takes the exact environment, never `all`"
        );
        assert_eq!(by_key("BOTH").byte_len, Some("exact".len()));
    }

    #[test]
    fn a_named_environment_never_falls_back_to_a_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "K", "production", "live").unwrap();

        let built = model(&paths, &[], "staging");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.in_force, None, "staging must not borrow production's");
        assert_eq!(row.set_in, ["production"]);
    }

    #[test]
    fn set_in_lists_every_environment_the_key_has_a_slot_for() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "K", ALL_ENVIRONMENTS, "x").unwrap();
        secrets::set(&paths.secrets, "K", "ci", "y").unwrap();

        let built = model(&paths, &[], "production");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.set_in, [ALL_ENVIRONMENTS, "ci"]);
    }

    #[test]
    fn environments_are_the_union_of_the_store_plus_all() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "A", "production", "x").unwrap();
        secrets::set(&paths.secrets, "B", "ci", "y").unwrap();

        let built = model(&paths, &[], "production");

        assert_eq!(built.environments, [ALL_ENVIRONMENTS, "ci", "production"]);
    }

    /// `Request::PutSecrets` checks a pushed environment's name against
    /// `is_name` and nothing else, so a dog can name one the operator's own
    /// store never mentions. `SET IN` divides a row's slot count by this
    /// list, and the tab row is drawn from it, so a pushed name left out
    /// prints a count larger than its own denominator and hides the
    /// environment from the tabs.
    #[test]
    fn a_pushed_environment_the_store_does_not_name_is_still_an_environment() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "K", "production", "x").unwrap();
        std::fs::write(
            &paths.secrets_cache,
            format!(
                r#"{{"version":{PROVIDER_CACHE_VERSION},"namespaces":{{"vercel":{{"API_TOKEN":{{"preview":"tok","production":"tok","staging":"tok"}}}}}},"pushed":{{"vercel":["preview","production","staging"]}}}}"#
            ),
        )
        .unwrap();

        let built = model(&paths, &[], "production");

        assert_eq!(
            built.environments,
            [ALL_ENVIRONMENTS, "preview", "production", "staging"]
        );
        let row = built
            .rows
            .iter()
            .find(|row| row.key == "vercel/API_TOKEN")
            .unwrap();
        assert!(
            row.set_in.len() <= built.environments.len(),
            "SET IN would print `{} of {}`",
            row.set_in.len(),
            built.environments.len()
        );
    }

    #[test]
    fn an_unreadable_store_reports_rather_than_reading_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        std::fs::write(&paths.secrets, "{\"version\":9999,\"entries\":{}}").unwrap();

        let built = model(&paths, &[], "production");

        assert!(built.rows.is_empty());
        assert!(
            built
                .unreadable
                .as_deref()
                .is_some_and(|m| m.contains("9999")),
            "got {:?}",
            built.unreadable
        );
    }

    #[test]
    fn byte_len_is_the_values_length_and_never_the_value() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "K", "production", "hunter2").unwrap();

        let built = model(&paths, &[], "production");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.byte_len, Some(7));
        assert!(
            !format!("{row:?}").contains("hunter2"),
            "the row must not carry the value: {row:?}"
        );
    }

    #[test]
    fn a_provider_row_is_qualified_and_sourced_by_its_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        std::fs::write(
            &paths.secrets_cache,
            format!(
                r#"{{"version":{PROVIDER_CACHE_VERSION},"namespaces":{{"vercel":{{"API_TOKEN":{{"production":"tok"}}}}}},"pushed":{{"vercel":["production"]}}}}"#
            ),
        )
        .unwrap();

        let built = model(&paths, &[], "production");

        let row = built
            .rows
            .iter()
            .find(|row| row.key == "vercel/API_TOKEN")
            .unwrap_or_else(|| panic!("no qualified row among {:?}", built.rows));
        assert_eq!(row.source, Source::Namespace("vercel".to_string()));
    }

    /// Both stores, since a row's value lives in whichever one its source
    /// names and the provider cache nests the namespace the row key spells
    /// with a slash.
    #[test]
    fn stored_value_reads_an_operator_row_and_a_provider_row_out_of_their_own_stores() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "PLAIN", "production", "hunter2").unwrap();
        std::fs::write(
            &paths.secrets_cache,
            format!(
                r#"{{"version":{PROVIDER_CACHE_VERSION},"namespaces":{{"vercel":{{"API_TOKEN":{{"production":"tok"}}}}}},"pushed":{{"vercel":["production"]}}}}"#
            ),
        )
        .unwrap();
        let built = model(&paths, &[], "production");
        let value_of = |key: &str| {
            let row = built
                .rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("no {key} among {:?}", built.rows));
            stored_value(&built.store, &built.provider_cache, row)
        };

        assert_eq!(value_of("PLAIN").as_deref(), Some("hunter2"));
        assert_eq!(value_of("vercel/API_TOKEN").as_deref(), Some("tok"));
    }

    /// A tab the key has no slot in resolves to nothing, so there is no
    /// value to ask the store for: `secrets::get` would answer for a
    /// sibling environment if it were asked with one.
    #[test]
    fn stored_value_is_none_when_nothing_resolves_in_this_tab() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "PLAIN", "ci", "hunter2").unwrap();

        let built = model(&paths, &[], "production");

        let row = built.rows.iter().find(|row| row.key == "PLAIN").unwrap();
        assert_eq!(row.in_force, None);
        assert_eq!(stored_value(&built.store, &built.provider_cache, row), None);
    }

    #[test]
    fn a_readers_key_must_match_the_row_key_operator_and_provider_alike() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        secrets::set(&paths.secrets, "PLAIN", ALL_ENVIRONMENTS, "a").unwrap();
        std::fs::write(
            &paths.secrets_cache,
            format!(
                r#"{{"version":{PROVIDER_CACHE_VERSION},"namespaces":{{"vercel":{{"API_TOKEN":{{"all":"tok"}}}}}},"pushed":{{"vercel":["all"]}}}}"#
            ),
        )
        .unwrap();
        let mut operator_app = AppConfig::minimal("operator-app", "./srv");
        operator_app
            .env
            .insert("A".into(), "{{secret:PLAIN}}".into());
        let mut provider_app = AppConfig::minimal("provider-app", "./srv");
        provider_app
            .env
            .insert("B".into(), "{{secret:vercel/API_TOKEN}}".into());
        write_roll(&paths, &[operator_app, provider_app]);

        let procs = [online("operator-app"), online("provider-app")];
        let built = model(&paths, &procs, "production");

        let row_named = |key: &str| built.rows.iter().find(|row| row.key == key).unwrap();
        assert_eq!(row_named("PLAIN").readers.len(), 1);
        assert_eq!(
            row_named("vercel/API_TOKEN").readers.len(),
            1,
            "a provider row's key must match the reference by_reference stored, or \
             every provider row silently shows no readers"
        );
    }
}
