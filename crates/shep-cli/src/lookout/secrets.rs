//! What the secrets pane draws, computed off the files it reads.
//!
//! No value reaches this module. A row carries a length, and the value
//! itself is fetched only by an explicit reveal, which is Task 6's job.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

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
///
/// `environments`, `unreadable` and `roll_age` have no non-test reader yet:
/// the pane that draws them lands in a later task. `#[allow(dead_code)]`
/// says so rather than inventing one.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct SecretsModel {
    /// Every environment the store holds a slot for, plus
    /// [`ALL_ENVIRONMENTS`], in name order. The tab row.
    pub environments: Vec<String>,
    /// Operator rows first, then each namespace's, keys in order within
    /// each.
    pub rows: Vec<SecretRow>,
    /// Why the operator's store would not read, when it would not.
    pub unreadable: Option<String>,
    /// How old the muster roll is, or `None` when it is missing.
    pub roll_age: Option<Duration>,
}

impl SecretsModel {
    /// This model's rows from one store, in key order.
    ///
    /// No non-test caller yet: the pane that draws `rows_for` lands in a
    /// later task. `#[allow(dead_code)]` says so rather than inventing one.
    #[allow(dead_code)]
    pub fn rows_for<'a>(&'a self, source: &'a Source) -> impl Iterator<Item = &'a SecretRow> {
        self.rows.iter().filter(move |row| &row.source == source)
    }
}

/// Builds the model for `environment`.
///
/// Best-effort throughout. An unreadable operator store reports itself and
/// leaves the provider rows alone; a missing roll costs the readers and
/// nothing else.
///
/// No non-test caller yet: the pane that renders this model lands in a
/// later task. `#[allow(dead_code)]` says so rather than inventing one.
#[allow(dead_code)]
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

    SecretsModel {
        environments: environments.into_iter().collect(),
        rows,
        unreadable,
        roll_age: secret_readers::roll_age(paths),
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
    use std::path::Path;

    use shep_core::secrets::{Resolution, SecretRef, SecretView};

    use super::*;

    /// `$SHEP_HOME` pinned to `dir` itself, same pattern as
    /// `secret_readers`'s own tests: the default `.shep` subdirectory is
    /// never created outside a real boot.
    fn paths_under(dir: &Path) -> ShepPaths {
        let home = dir.display().to_string();
        ShepPaths::resolve(&move |key| (key == "SHEP_HOME").then(|| home.clone()), dir)
    }

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
}
