//! `shep upgrade <dog>`: moves an installed dog to what its receipt says it
//! follows.
//!
//! An index install moves to the index's version and a ref install rebuilds
//! its ref at the newest commit, both through [`fetch_dog`] and
//! [`adopt_fetched`], so an upgrade is checked and recorded as an install is.
//! The directory it replaced is kept with a copy of its receipt, and the new
//! receipt names it. The one before that is removed, so at most one is kept.
//! `--rollback` is in [`super::dog_rollback`].

use std::path::Path;

use shep_core::paths::ShepPaths;

use crate::cli::{InstallArgs, UpgradeArgs};
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::DogSourceKind;
use crate::exit::ExitCode;
use crate::output::{DogActionStatus, Streams};

use super::consent::Input;
use super::dog_rollback::rollback;
use super::install::{Reach, Upgrading, adopt_fetched, fetch_dog, load_index};
use super::install_depends::Catalog;
use super::receipt::{self, HeldPin};
use super::upgrade::{upgrade_notice, upgrade_one};
use super::{adopted_source, emit_dog_action, fail_config};

/// [`DogActionRow`](crate::output::DogActionRow)'s status for a dog already at
/// what it follows.
const CURRENT_STATUS: DogActionStatus =
    DogActionStatus::NoShepherd("already current; nothing changed");

/// Where an upgrade is headed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Goal {
    /// What the receipt follows: the index's version, or the ref's newest commit.
    Follow,
    /// This ref of the row's repository.
    Ref(String),
    /// The index's version, for a dog installed from a ref.
    Release,
    /// The install the last upgrade replaced.
    Rollback,
}

/// `shep upgrade [<dog>]`.
pub async fn upgrade(streams: &mut Streams<'_>, paths: &ShepPaths, args: &UpgradeArgs) -> ExitCode {
    let goal = match (&args.git_ref, args.release, args.rollback) {
        (Some(git_ref), _, _) => Goal::Ref(git_ref.clone()),
        (None, true, _) => Goal::Release,
        (None, false, true) => Goal::Rollback,
        (None, false, false) => Goal::Follow,
    };
    if args.dog.is_none() && !args.all {
        return streams.fail(
            ExitCode::Failure,
            "bare `shep upgrade`, which would upgrade shep itself, is not implemented yet. \
             `shep upgrade <dog>` upgrades one installed dog and `shep upgrade --dogs` every one",
        );
    }
    // A rollback reads no index: nothing it needs is on the network.
    let (url, rows) = if goal == Goal::Rollback {
        (String::new(), Vec::new())
    } else {
        match load_index(streams, paths, args.refresh).await {
            Ok((url, loaded)) => (url, loaded.index.dogs),
            Err(code) => return code,
        }
    };
    let catalog = Catalog::new(&url, &rows);
    let reach = Reach::real();
    let mut input = Input::stdin();
    if let Some(name) = &args.dog {
        return upgrade_dog(streams, paths, &reach, &mut input, &catalog, name, &goal).await;
    }
    upgrade_all(streams, paths, &reach, &mut input, &catalog).await
}

/// `--dogs`: every adopted dog with a receipt, in turn. One that fails does
/// not stop the rest, and the first failure is the exit code.
pub(super) async fn upgrade_all(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    reach: &Reach,
    input: &mut Input,
    catalog: &Catalog<'_>,
) -> ExitCode {
    let cfg = match ShepToml::read_only(&paths.daemon_config) {
        Ok(cfg) => cfg,
        Err(err) => return fail_config(streams, &err),
    };
    let mut first_failure = ExitCode::Success;
    let mut upgraded = 0;
    for name in cfg.adopted_dog_names() {
        // An unreadable receipt is not skipped: the upgrade reports it.
        if matches!(receipt::read(paths, &name), Ok(None)) {
            streams.aside(
                "upgrade",
                &format!("skipping {name}: it has no receipt, so `shep adopt` put it there"),
            );
            continue;
        }
        upgraded += 1;
        let code = upgrade_dog(streams, paths, reach, input, catalog, &name, &Goal::Follow).await;
        if first_failure == ExitCode::Success {
            first_failure = code;
        }
    }
    if upgraded == 0 {
        streams.aside("upgrade", "no installed dog to upgrade");
    }
    first_failure
}

/// Upgrades the one dog adopted as `name` towards `goal`.
pub(super) async fn upgrade_dog(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    reach: &Reach,
    input: &mut Input,
    catalog: &Catalog<'_>,
    name: &str,
    goal: &Goal,
) -> ExitCode {
    let cfg = match ShepToml::read_only(&paths.daemon_config) {
        Ok(cfg) => cfg,
        Err(err) => return fail_config(streams, &err),
    };
    if cfg.adopted_dog_path(name).is_none() {
        return if crate::dog::BUILT_IN_DOGS.contains(&name) {
            streams.fail(
                ExitCode::Failure,
                &format!("`{name}` is built in to shep and upgrades with it"),
            )
        } else {
            streams.fail(
                ExitCode::NotFound,
                &format!("`{name}` is not an adopted dog; `shep dogs` lists them"),
            )
        };
    }
    let held = match receipt::read(paths, name) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return streams.fail(
                ExitCode::Failure,
                &format!(
                    "`{name}` has no receipt, so shep did not install it: it came from `shep \
                     adopt <path>`. `shep upgrade` follows what `shep install` put on disk. \
                     Rebuild it and `shep adopt` it again, or `shep install {name}` to take it \
                     from the index"
                ),
            );
        }
        Err(err) => return fail_receipt(streams, paths, name, &err),
    };
    let enabled = cfg.enabled_dog_names().iter().any(|dog| dog == name);
    if *goal == Goal::Rollback {
        return rollback(streams, paths, reach, input, name, &held, enabled).await;
    }

    let Some(row) = catalog.row(name) else {
        return streams.fail(
            ExitCode::NotFound,
            &format!(
                "the dog index has no dog called `{name}` now, so there is nothing to upgrade it \
                 to; `--rollback` still works"
            ),
        );
    };
    // What to build: a ref, or the index's version. A ref install follows its ref.
    let git_ref = match (goal, &held.pinned) {
        (Goal::Ref(git_ref), _) => Some(git_ref.clone()),
        (Goal::Follow, HeldPin::Ref { git_ref, .. }) => Some(git_ref.clone()),
        _ => None,
    };
    if git_ref.is_none()
        && let (
            DogSourceKind::Cargo {
                version: Some(approved),
                ..
            },
            HeldPin::Version(held_version),
        ) = (&row.source, &held.pinned)
        && approved == held_version
    {
        return current(streams, &cfg, name, &held.pinned);
    }

    let install_args = InstallArgs {
        dog: Some(name.to_owned()),
        yes: false,
        no_enable: false,
        refresh: false,
        cargo: false,
        git_ref: None,
    };
    let mut fetched = match fetch_dog(
        streams,
        paths,
        &install_args,
        reach,
        row,
        git_ref.as_deref(),
    )
    .await
    {
        Ok(fetched) => fetched,
        Err(code) => return code,
    };
    // The lock is held now: what is on disk is what this upgrade replaces.
    let held = match receipt::read(paths, name) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("`{name}` lost its receipt while it was being upgraded"),
            );
        }
        Err(err) => return fail_receipt(streams, paths, name, &err),
    };
    let lands = fetched.lands_as();
    if lands == held.pinned {
        return current(streams, &cfg, name, &held.pinned);
    }
    let new_root = receipt::root_of(fetched.binary()).map(Path::to_path_buf);
    let old_root = held.root().map(Path::to_path_buf);
    // A ref switched to a name for the commit it already runs lands in the
    // directory it is in: nothing is replaced, and what it kept stays kept.
    let same_root = new_root == old_root;
    let previous = if same_root {
        held.previous.clone()
    } else {
        old_root
            .clone()
            .filter(|root| receipt::is_install_dir(paths, name, root))
    };
    if !same_root
        && let Some(root) = &previous
        && let Err(err) =
            receipt::write_document(&root.join("receipt.json"), &held.with_previous(None))
    {
        return streams.fail(
            ExitCode::Failure,
            &format!(
                "keeping the receipt of the install it replaces in {}: {err}",
                root.display()
            ),
        );
    }
    if fetched.built_by_cargo()
        && let (HeldPin::Version(old), HeldPin::Version(new)) = (&held.pinned, &lands)
    {
        streams.aside(
            "upgrade_diff",
            &format!("what changed: https://diff.rs/{}/{old}/{new}", row.package),
        );
    }
    fetched.upgrading = Some(Upgrading {
        previous,
        enable: enabled,
    });
    let before = waiting_of(paths, name);
    let mut said_no = false;
    let code = adopt_fetched(
        streams,
        paths,
        &install_args,
        input,
        fetched,
        catalog.url(),
        &mut said_no,
    )
    .await;
    if code != ExitCode::Success {
        return code;
    }
    // The install before the replaced one is no longer kept.
    if let (Some(older), Some(new_root)) = (&held.previous, &new_root)
        && !same_root
        && Some(older) != old_root.as_ref()
        && older != new_root
        && receipt::is_install_dir(paths, name, older)
    {
        let _ = std::fs::remove_dir_all(older);
    }
    streams.aside(
        "upgrade",
        &format!(
            "upgraded {name} from {} to {}",
            describe(&held.pinned),
            describe(&lands)
        ),
    );
    report_waiting(streams, paths, name, &before);
    ExitCode::Success
}

/// The reply for a dog that is already at what it follows.
fn current(streams: &mut Streams<'_>, cfg: &ShepToml, name: &str, at: &HeldPin) -> ExitCode {
    streams.aside(
        "upgrade",
        &format!("{name} is already at {}; nothing to do", describe(at)),
    );
    emit_dog_action(
        streams,
        "upgrade",
        name,
        adopted_source(cfg, name, None),
        CURRENT_STATUS,
    )
}

/// A version, or a ref and the commit it was built at.
pub(super) fn describe(pinned: &HeldPin) -> String {
    match pinned {
        HeldPin::Version(version) => version.clone(),
        HeldPin::Ref { git_ref, commit } => {
            format!("{git_ref} at {}", commit.get(..12).unwrap_or(commit))
        }
    }
}

/// The permissions `name`'s record holds waiting.
pub(super) fn waiting_of(paths: &ShepPaths, name: &str) -> Vec<String> {
    ShepToml::read_only(&paths.daemon_config)
        .ok()
        .and_then(|cfg| cfg.grant_record(name).ok().flatten())
        .map(|record| record.waiting.names().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Puts a permission the new build declares in `waiting` and says so. The dog
/// has been restarted on the grants it already held.
pub(super) fn report_waiting(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    before: &[String],
) {
    match upgrade_one(paths, name) {
        Ok(true) => {
            let now = waiting_of(paths, name);
            let new: Vec<_> = now
                .iter()
                .filter(|p| !before.contains(p))
                .cloned()
                .collect();
            let detail = if new.is_empty() {
                String::new()
            } else {
                format!(" ({} waiting)", new.join(", "))
            };
            streams.aside(
                "upgrade_waiting",
                &format!("{}{detail}", upgrade_notice(&[name.to_owned()])),
            );
        }
        Ok(false) => {}
        Err(err) => {
            let _ = fail_config(streams, &err);
        }
    }
}

/// The refusal for a receipt that is there and does not read.
pub(super) fn fail_receipt(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    err: &receipt::HeldError,
) -> ExitCode {
    streams.fail(
        ExitCode::Failure,
        &format!(
            "reading {}: {err}",
            receipt::receipt_path(paths, name).display()
        ),
    )
}
