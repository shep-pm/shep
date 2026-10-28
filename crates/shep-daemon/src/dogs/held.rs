//! A saved sheep keeps its name when a dog wants it
//!
//! The built-in dogs are listed at rest, which takes their names before an
//! operator has enabled either. An upgrade must not cost anyone a sheep that
//! already holds one, so the dog is the one that gives way.

use super::DogSpec;
use crate::snapshot;
use std::collections::BTreeSet;
use std::path::Path;

/// The names of the sheep the roll at `path` restores.
///
/// A roll that is missing or unreadable holds no names: the restore reports
/// that itself, and this is only asking which dogs to leave out.
pub(crate) fn saved_sheep_names(path: &Path) -> BTreeSet<String> {
    let Ok(saved) = snapshot::read(path) else {
        return BTreeSet::new();
    };
    snapshot::restorable(saved)
        .members
        .iter()
        .map(|app| app.config().name.clone())
        .collect()
}

/// Drops from `enabled` and `known` every dog whose name is in `held`,
/// warning once per name.
///
/// Both lists, because the dog is neither spawned nor registered at rest
/// under a name a sheep holds. `shep enable <name>` then refuses with the
/// message it already has for a sheep in the way.
pub(crate) fn leave_held_names(
    enabled: &mut Vec<DogSpec>,
    known: &mut Vec<DogSpec>,
    held: &BTreeSet<String>,
) {
    let mut warned = BTreeSet::new();
    for specs in [enabled, known] {
        specs.retain(|spec| {
            if !held.contains(&spec.name) {
                return true;
            }
            if warned.insert(spec.name.clone()) {
                tracing::warn!(
                    name = %spec.name,
                    "a saved sheep holds this name, so the dog of that name is not registered; \
                     rename one of them"
                );
            }
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{capture_logs, roll_of};
    use shep_core::config::AppConfig;
    use shep_core::protocol::DogSource;
    use std::collections::BTreeMap;

    fn built_in(name: &str) -> DogSpec {
        DogSpec {
            name: name.to_string(),
            source: DogSource::BuiltIn { grants: None },
            env: BTreeMap::new(),
        }
    }

    fn names(specs: &[DogSpec]) -> Vec<&str> {
        specs.iter().map(|spec| spec.name.as_str()).collect()
    }

    #[test]
    fn a_dog_named_like_a_held_sheep_is_left_out_of_both_lists_and_warned_about_once() {
        let mut enabled = vec![built_in("metrics")];
        let mut known = vec![built_in("metrics"), built_in("bark")];
        let held = BTreeSet::from(["metrics".to_string()]);

        let logs = capture_logs(|| leave_held_names(&mut enabled, &mut known, &held));

        assert!(enabled.is_empty());
        assert_eq!(names(&known), ["bark"]);
        assert_eq!(logs.matches("rename one of them").count(), 1, "{logs:?}");
        assert!(
            logs.contains("WARN") && logs.contains("metrics"),
            "{logs:?}"
        );
    }

    #[test]
    fn no_held_name_leaves_every_dog_and_says_nothing() {
        let mut enabled = vec![built_in("metrics")];
        let mut known = vec![built_in("metrics"), built_in("bark")];

        let logs = capture_logs(|| leave_held_names(&mut enabled, &mut known, &BTreeSet::new()));

        assert_eq!(names(&enabled), ["metrics"]);
        assert_eq!(names(&known), ["metrics", "bark"]);
        assert_eq!(logs, "");
    }

    #[test]
    fn the_roll_names_the_sheep_it_restores() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roll.json");
        let roll = roll_of(vec![
            AppConfig::minimal("api", "./srv"),
            AppConfig::minimal("metrics", "./srv"),
        ]);
        snapshot::write_atomic(&path, &roll).unwrap();

        assert_eq!(
            saved_sheep_names(&path),
            BTreeSet::from(["api".to_string(), "metrics".to_string()])
        );
    }

    #[test]
    fn a_missing_or_unreadable_roll_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roll.json");
        assert!(saved_sheep_names(&path).is_empty());

        std::fs::write(&path, "not json").unwrap();
        assert!(saved_sheep_names(&path).is_empty());
    }
}
