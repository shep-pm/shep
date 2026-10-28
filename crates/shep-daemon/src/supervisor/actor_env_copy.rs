//! Copying one sheep's env onto another's, inside the shepherd.

use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::OnceLock;

use shep_core::protocol::{EnvCopyChange, EnvCopyKind};
use shep_core::secrets::shown_env_value;

use super::actor_pane::record_env_edit;
use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Copies `from`'s intended env onto `to` as operator overrides, in one
    /// write, parking the result for `to`'s next spawn.
    ///
    /// `to` keeps its own keys and `from`'s overwrite them; `replace` makes
    /// `to`'s env exactly `from`'s. A key in `except` is left as `to` had it.
    /// A `{{secret:...}}` is copied as the reference, unresolved.
    ///
    /// # Errors
    /// - [`SupervisorError::NoSuchSheep`] - `from` or `to` is no sheep.
    /// - [`SupervisorError::IsADog`] - `from` or `to` is a dog's.
    /// - [`SupervisorError::EnvChanged`] - `expect` differs from the list.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    pub(super) fn handle_copy_sheep_env(
        &mut self,
        from: &str,
        to: &str,
        replace: bool,
        except: &[String],
        dry_run: bool,
        expect: Option<&[EnvCopyChange]>,
    ) -> Result<EnvCopy, SupervisorError> {
        let source_id = self
            .representative_id(from)
            .ok_or_else(|| SupervisorError::NoSuchSheep(from.to_string()))?;
        let target_id = self
            .representative_id(to)
            .ok_or_else(|| SupervisorError::NoSuchSheep(to.to_string()))?;
        self.ensure_not_dog(from, source_id)?;
        self.ensure_not_dog(to, target_id)?;
        let source = self
            .intended_spec(source_id)
            .map(|spec| spec.config().env.clone())
            .ok_or_else(|| SupervisorError::NoSuchSheep(from.to_string()))?;
        let mut intended = self
            .intended_spec(target_id)
            .map(|spec| spec.config().clone())
            .ok_or_else(|| SupervisorError::NoSuchSheep(to.to_string()))?;

        let except: BTreeSet<&str> = except.iter().map(String::as_str).collect();
        let before = intended.env.clone();
        let mut after = before.clone();
        for (key, value) in &source {
            if !except.contains(key.as_str()) {
                after.insert(key.clone(), value.clone());
            }
        }
        if replace {
            after.retain(|key, _| except.contains(key.as_str()) || source.contains_key(key));
        }

        let changes = diff(&before, &after);
        if let Some(reviewed) = expect
            && reviewed != changes
        {
            return Err(SupervisorError::EnvChanged(differing_keys(
                reviewed, &changes,
            )));
        }
        if changes.is_empty() {
            return Ok(EnvCopy { app: None, changes });
        }

        intended.env = after.clone();
        let parked =
            normalize(intended).map_err(|err| SupervisorError::InvalidEnv(err.to_string()))?;
        if dry_run {
            return Ok(EnvCopy { app: None, changes });
        }

        let overridden = self.mutate_override(to, |record| {
            for change in &changes {
                let value = after.get(&change.key).map(String::as_str);
                record_env_edit(
                    record,
                    to,
                    &change.key,
                    value,
                    before.contains_key(&change.key),
                )?;
            }
            Ok(())
        })?;
        self.park_slots(to, &parked, &overridden);
        Ok(EnvCopy {
            app: Some(parked),
            changes,
        })
    }
}

/// A fingerprint of the real values a change moves between, under a key that
/// lives as long as this process.
///
/// A redacted value cannot say whether it is still the one that was reviewed;
/// this can, and tells a client nothing about the value.
fn seal(old: Option<&String>, new: Option<&String>) -> u64 {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    let mut hasher = KEY.get_or_init(RandomState::new).build_hasher();
    old.hash(&mut hasher);
    new.hash(&mut hasher);
    hasher.finish()
}

/// The keys whose change is in one list and not in the other, in key order.
fn differing_keys(reviewed: &[EnvCopyChange], now: &[EnvCopyChange]) -> String {
    let keys: BTreeSet<&str> = reviewed
        .iter()
        .filter(|change| !now.contains(change))
        .chain(now.iter().filter(|change| !reviewed.contains(change)))
        .map(|change| change.key.as_str())
        .collect();
    keys.into_iter().collect::<Vec<_>>().join(", ")
}

/// Every key that differs between `before` and `after`, in key order, with
/// each value as [`shown_env_value`] lets a client read it.
fn diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<EnvCopyChange> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let (old, new) = (before.get(key), after.get(key));
            let kind = match (old, new) {
                (Some(old), Some(new)) if old == new => return None,
                (Some(_), Some(_)) => EnvCopyKind::Changed,
                (None, Some(_)) => EnvCopyKind::Added,
                (Some(_), None) => EnvCopyKind::Removed,
                (None, None) => return None,
            };
            Some(
                EnvCopyChange::new(
                    key.as_str(),
                    kind,
                    old.map(|value| shown_env_value(value)),
                    new.map(|value| shown_env_value(value)),
                )
                .sealed(seal(old, new)),
            )
        })
        .collect()
}
