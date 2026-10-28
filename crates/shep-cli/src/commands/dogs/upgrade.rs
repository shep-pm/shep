//! The upgrade path of a dog's grants: what a shep that first knows about
//! grants, or a dog rebuilt to ask for more, does to `shep.toml` so access
//! never widens unseen. [`upgrade_grants`] runs at every boot and reload.
//!
//! - An enabled dog with a manifest and no record is granted what the
//!   manifest declares, marked not reviewed, once.
//! - A dog whose manifest declares a permission it holds neither as granted
//!   nor as revoked has it put in `waiting`, and keeps running on what it had.
//! - A dog with no manifest is left alone: undeclared, with full access.

use std::path::Path;

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;

use crate::commands::shep_toml::{GrantRecord, ShepToml, ShepTomlError};

use super::consent::{Review, review};
use super::vet::{ManifestRead, VERSION_BUDGET, read_manifest};

/// What a dog's current build says it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Declaration {
    /// No manifest: the dog declares nothing and keeps full access.
    Undeclared,
    /// The permissions in its manifest that this shep knows.
    Declares(Grants),
    /// The manifest could not be read, or requires a permission this shep
    /// does not know. Nothing is changed on a guess.
    Unknown,
}

/// What `declaration` makes of a dog's record.
///
/// `None` is nothing to write: the record stays as it is, or a dog with
/// none stays undeclared.
pub(super) fn plan(
    current: Option<&GrantRecord>,
    enabled: bool,
    declaration: &Declaration,
) -> Option<GrantRecord> {
    let Declaration::Declares(declared) = declaration else {
        return None;
    };
    let Some(record) = current else {
        if !enabled {
            return None;
        }
        // A dog that asks for nothing has nothing to review.
        return Some(if declared.names().next().is_none() {
            GrantRecord::reviewed(declared.clone())
        } else {
            GrantRecord::at_upgrade(declared.clone())
        });
    };
    let held = |name: &&str| record.granted.names().any(|g| g == *name);
    let revoked = |name: &&str| record.revoked.names().any(|r| r == *name);
    let waiting = Grants::from_names(
        declared
            .names()
            .filter(|name| !held(name) && !revoked(name))
            .map(str::to_owned),
    );
    // A revocation of something the manifest no longer declares has done its
    // work; keeping it would hide the permission if a later build asked again.
    let revoked = Grants::from_names(
        record
            .revoked
            .names()
            .filter(|name| declared.names().any(|d| d == *name))
            .map(str::to_owned),
    );
    let next = GrantRecord {
        waiting,
        revoked,
        ..record.clone()
    };
    (next != *record).then_some(next)
}

/// What `name`'s current build declares: an adopted dog's binary is asked
/// (and cached by its digest), a built-in is this binary.
pub(super) fn declaration_of(cfg: &ShepToml, home: &Path, name: &str) -> Declaration {
    let manifest = match cfg.adopted_dog_path(name) {
        Some(binary) => match read_manifest(&binary, home, name, VERSION_BUDGET) {
            Ok(ManifestRead::Read(manifest)) => Some(manifest),
            Ok(ManifestRead::Absent) => None,
            Ok(ManifestRead::Unreadable) | Err(_) => return Declaration::Unknown,
        },
        None => crate::dog::builtin_manifest(name),
    };
    match review(manifest.as_ref()) {
        Ok(Review::Undeclared) => Declaration::Undeclared,
        Ok(Review::Declared { listed, .. }) => Declaration::Declares(Grants::from_names(
            listed.iter().map(|entry| entry.name.clone()),
        )),
        Err(_) => Declaration::Unknown,
    }
}

/// The records `examined` calls for, given what `cfg` holds now.
///
/// A dog whose record is unreadable is skipped: `shep enable` reports it
/// where the operator is looking.
fn changes(cfg: &ShepToml, examined: &[(String, Declaration)]) -> Vec<(String, GrantRecord)> {
    let enabled = cfg.enabled_dog_names();
    examined
        .iter()
        .filter_map(|(name, declaration)| {
            let current = cfg.grant_record(name).ok()?;
            plan(
                current.as_ref(),
                enabled.iter().any(|dog| dog == name),
                declaration,
            )
            .map(|record| (name.clone(), record))
        })
        .collect()
}

/// Brings every dog's record up to what its build declares, naming the dogs
/// whose record changed.
///
/// Writes `shep.toml` only when a record changes, so an untouched file keeps
/// its inode and mode on every boot after the first.
///
/// # Errors
/// [`ShepTomlError`] when `shep.toml` cannot be read or written.
pub(crate) fn upgrade_grants(paths: &ShepPaths) -> Result<Vec<String>, ShepTomlError> {
    let cfg = ShepToml::read_only(&paths.daemon_config)?;
    let enabled = cfg.enabled_dog_names();
    let mut candidates: Vec<String> = crate::dog::BUILT_IN_DOGS
        .iter()
        .map(|name| (*name).to_owned())
        .chain(cfg.adopted_dog_names())
        .collect();
    candidates.sort();
    candidates.dedup();
    // Only dogs that are on, or already hold a record, can change.
    let examined: Vec<(String, Declaration)> = candidates
        .into_iter()
        .filter(|name| enabled.contains(name) || matches!(cfg.grant_record(name), Ok(Some(_))))
        .map(|name| {
            let declaration = declaration_of(&cfg, &paths.home, &name);
            (name, declaration)
        })
        .collect();
    if changes(&cfg, &examined).is_empty() {
        return Ok(Vec::new());
    }
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        let changed = changes(cfg, &examined);
        for (name, record) in &changed {
            cfg.set_grant_record(name, record)?;
        }
        Ok(changed.into_iter().map(|(name, _)| name).collect())
    })
}

/// [`upgrade_grants`] for one dog, for `shep enable` and `shep dogs grant`,
/// which look at a dog's build the moment the operator acts on it.
///
/// # Errors
/// [`ShepTomlError`] when `shep.toml` cannot be read or written.
pub(super) fn upgrade_one(paths: &ShepPaths, name: &str) -> Result<(), ShepTomlError> {
    let cfg = ShepToml::read_only(&paths.daemon_config)?;
    let known = crate::dog::BUILT_IN_DOGS.contains(&name) || cfg.adopted_dog_path(name).is_some();
    if !known {
        return Ok(());
    }
    let examined = vec![(name.to_owned(), declaration_of(&cfg, &paths.home, name))];
    if changes(&cfg, &examined).is_empty() {
        return Ok(());
    }
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        for (name, record) in changes(cfg, &examined) {
            cfg.set_grant_record(&name, &record)?;
        }
        Ok(())
    })
}

/// The sentence a boot or reload reports for the dogs whose record changed.
#[must_use]
pub(crate) fn upgrade_notice(changed: &[String]) -> String {
    format!(
        "updated the grants recorded for {}: `shep dogs` shows any permission waiting for \
         `shep enable` or `shep dogs grant`, and any list not yet reviewed",
        changed.join(", ")
    )
}
