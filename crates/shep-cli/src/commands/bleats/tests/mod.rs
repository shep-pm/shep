//! Shared test fixtures: the `ProcessInfo` and `BleatsArgs` builders, the
//! log-file helper, and the run timeout every streamed test in [`follow`]
//! bounds itself by. [`follow`] covers the bus-subscribed path,
//! [`tail`] the `--no-follow` file-reading one.

use std::time::Duration;

use shep_client::testing::fake_client_with_push;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use super::*;
use crate::cli::{Cli, Commands};

fn info(id: u32, name: &str) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .out_file(Some(format!("/logs/{name}-0-out.log")))
        .err_file(Some(format!("/logs/{name}-0-err.log")))
        .build()
}

/// Like [`info`], but with a real instance slot: `info` never sets one.
fn info_with_instance(id: u32, name: &str, slot: u32) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .instance(Some(slot))
        .out_file(Some(format!("/logs/{name}-{slot}-out.log")))
        .err_file(Some(format!("/logs/{name}-{slot}-err.log")))
        .build()
}

fn bleats_args(selector: &str, no_follow: bool, err: bool, out: bool) -> BleatsArgs {
    BleatsArgs {
        selector: selector.to_string(),
        no_follow,
        // The follow tests here assert on what the bus delivers, so
        // this asks for no history.
        lines: crate::cli::DEFAULT_BLEAT_LINES,
        err,
        out,
    }
}

fn follow_args(selector: &str) -> BleatsArgs {
    BleatsArgs {
        lines: 0,
        ..bleats_args(selector, false, false, false)
    }
}

fn follow_args_err(selector: &str) -> BleatsArgs {
    bleats_args(selector, false, true, false)
}

fn follow_args_out(selector: &str) -> BleatsArgs {
    bleats_args(selector, false, false, true)
}

fn no_follow_args(selector: &str) -> BleatsArgs {
    bleats_args(selector, true, false, false)
}

fn no_follow_args_err(selector: &str) -> BleatsArgs {
    bleats_args(selector, true, true, false)
}

fn no_follow_args_out(selector: &str) -> BleatsArgs {
    bleats_args(selector, true, false, true)
}

/// Writes `content` to `dir/name` and returns the path as a `String`,
/// what a scripted [`ProcessInfo`]'s `out_file`/`err_file` needs.
fn write_log(dir: &Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn no_follow_parses_and_plain_bleats_still_follows() {
    use clap::Parser;

    let Commands::Bleats(args) = Cli::try_parse_from(["shep", "bleats"]).unwrap().command else {
        panic!()
    };
    assert!(!args.no_follow, "the default is to follow");

    let Commands::Bleats(args) = Cli::try_parse_from(["shep", "bleats", "--no-follow"])
        .unwrap()
        .command
    else {
        panic!()
    };
    assert!(args.no_follow);

    // `--no-follow` is `ArgAction::SetTrue` and stores no value, so a
    // following token is not consumed by it and lands on the positional.
    let Commands::Bleats(args) = Cli::try_parse_from(["shep", "bleats", "--no-follow", "true"])
        .unwrap()
        .command
    else {
        panic!()
    };
    assert!(args.no_follow);
    assert_eq!(
        args.selector, "true",
        "--no-follow takes no value; the token is the selector"
    );
}

/// Every `bleats(...)`/`bleats_with_signal(...)` call in this module is
/// bounded by this timeout, so a hang fails with a named assertion
/// instead of a killed CI job.
const RUN_TIMEOUT: Duration = Duration::from_secs(5);

mod follow;
mod tail;
