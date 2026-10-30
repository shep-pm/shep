//! The `Whistle`s `read_tests` builds: one on a local socket, one reading its
//! `hosts` list from a `shep.toml` written for the test.

#[cfg(unix)]
use std::path::Path;

use shep_core::paths::ShepPaths;

use crate::whistle::{Whistle, gate};

/// A `ShepPaths` naming only the three fields any test here reads: the
/// socket, the barks file and the `shep.toml` the hosts list is read from.
/// The rest carry an empty placeholder.
fn test_paths(
    socket: std::path::PathBuf,
    barks: std::path::PathBuf,
    daemon_config: std::path::PathBuf,
) -> ShepPaths {
    ShepPaths {
        home: std::path::PathBuf::new(),
        daemon_config,
        dogs_config: std::path::PathBuf::new(),
        snapshot: std::path::PathBuf::new(),
        logs: std::path::PathBuf::new(),
        pids: std::path::PathBuf::new(),
        run: std::path::PathBuf::new(),
        socket,
        barks,
        kv: std::path::PathBuf::new(),
        overrides: std::path::PathBuf::new(),
        secrets: std::path::PathBuf::new(),
        secrets_cache: std::path::PathBuf::new(),
        dog_defaults: std::path::PathBuf::new(),
    }
}

pub(super) fn whistle_at(socket: std::path::PathBuf, barks_path: std::path::PathBuf) -> Whistle {
    Whistle::new(
        test_paths(socket, barks_path, dir_placeholder("no-shep.toml")),
        gate::Control::ReadOnly,
    )
}

/// A path nothing is ever written to, for the `daemon_config` of a
/// `Whistle` whose test wants no hosts listed at all.
fn dir_placeholder(name: &str) -> std::path::PathBuf {
    std::path::Path::new("/nonexistent").join(name)
}

/// Writes a `shep.toml` into `dir` listing `hosts`, and a `Whistle` at it
/// whose host calls run `program` in place of `ssh`.
///
/// The list is written to the file rather than handed to the `Whistle`, so
/// these tests read the same `hosts` array `shep host add` writes.
#[cfg(unix)]
pub(super) fn whistle_with_hosts(dir: &Path, hosts: &[&str], program: &Path) -> Whistle {
    let quoted: Vec<String> = hosts.iter().map(|host| format!("\"{host}\"")).collect();
    let config = dir.join("shep.toml");
    std::fs::write(&config, format!("hosts = [{}]\n", quoted.join(", "))).unwrap();
    Whistle::for_test_with_ssh_program(
        test_paths(
            shep_client::testing::control_address(dir),
            dir.join("barks.jsonl"),
            config,
        ),
        gate::Control::ReadOnly,
        program,
    )
}
