//! `shep upgrade <dog> --rollback`: puts back the install the last upgrade
//! replaced, with its receipt.
//!
//! It swaps rather than discards: the install rolled away from becomes the one
//! a second `--rollback` returns to, so no build is lost.

use std::path::Path;

use shep_core::paths::ShepPaths;

use crate::cli::AdoptArgs;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::adopt::{Flow, adopt_noting};
use super::consent::Input;
use super::dog_upgrade::{describe, fail_receipt, report_waiting, waiting_of};
use super::install::{Reach, lock_dog};
use super::install_keep::Kept;
use super::receipt::{self, Held};

/// `--rollback`: puts back the install the last upgrade replaced, and its receipt.
pub(super) async fn rollback(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    reach: &Reach,
    input: &mut Input,
    name: &str,
    seen: &Held,
    enabled: bool,
) -> ExitCode {
    let _held = match lock_dog(streams, paths, name, reach.lock_wait).await {
        Ok(held) => held,
        Err(code) => return code,
    };
    // Read again under the lock.
    let current = match receipt::read(paths, name) {
        Ok(Some(current)) => current,
        Ok(None) => seen.clone(),
        Err(err) => return fail_receipt(streams, paths, name, &err),
    };
    let nothing = |streams: &mut Streams<'_>, why: &str| {
        streams.fail(
            ExitCode::Failure,
            &format!("nothing to roll `{name}` back to: {why}"),
        )
    };
    let Some(previous) = current.previous.clone() else {
        return nothing(streams, "the last install replaced nothing");
    };
    if !receipt::is_install_dir(paths, name, &previous) {
        return nothing(
            streams,
            &format!(
                "its receipt names {}, which is not one of its install directories",
                previous.display()
            ),
        );
    }
    let prior = match receipt::read_file(&previous.join("receipt.json")) {
        Ok(Some(prior)) => prior,
        Ok(None) => return nothing(streams, &format!("{} holds no receipt", previous.display())),
        Err(err) => {
            return nothing(
                streams,
                &format!("{}'s receipt does not read: {err}", previous.display()),
            );
        }
    };
    if !prior.binary.is_file() {
        return nothing(streams, &format!("{} is gone", prior.binary.display()));
    }
    let Some(current_root) = current
        .root()
        .filter(|root| receipt::is_install_dir(paths, name, root))
        .map(Path::to_path_buf)
    else {
        return nothing(
            streams,
            "the install it runs is not one of its install directories",
        );
    };
    if let Err(err) = receipt::write_document(
        &current_root.join("receipt.json"),
        &current.with_previous(None),
    ) {
        return streams.fail(
            ExitCode::Failure,
            &format!(
                "keeping the receipt of the install it leaves in {}: {err}",
                current_root.display()
            ),
        );
    }
    let receipt_file = receipt::receipt_path(paths, name);
    let kept = match Kept::park(&[&receipt_file]) {
        Ok(kept) => kept,
        Err(err) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("setting aside the receipt of {name}: {err}"),
            );
        }
    };
    if let Err(err) =
        receipt::write_document(&receipt_file, &prior.with_previous(Some(&current_root)))
    {
        return streams.fail(
            ExitCode::Failure,
            &format!("writing {}: {err}", receipt_file.display()),
        );
    }
    let before = waiting_of(paths, name);
    let adopting = AdoptArgs {
        path: prior.binary.clone(),
        name: Some(name.to_owned()),
        yes: false,
    };
    let flow = Flow {
        verb: "upgrade",
        enable: enabled,
        restart: true,
    };
    let config_before = std::fs::read(&paths.daemon_config).ok();
    let code = adopt_noting(streams, paths, &adopting, input, flow, None, &mut false).await;
    if code == ExitCode::Success || std::fs::read(&paths.daemon_config).ok() != config_before {
        kept.commit();
    }
    if code == ExitCode::Success {
        streams.aside(
            "upgrade",
            &format!(
                "rolled {name} back from {} to {}",
                describe(&current.pinned),
                describe(&prior.pinned)
            ),
        );
        report_waiting(streams, paths, name, &before);
    }
    code
}
