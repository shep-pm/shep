//! End-to-end tier: drives the real `shep` binary via `assert_cmd` against a
//! real daemon, a real socket, and real spawned sheep, each on a fresh
//! `$SHEP_HOME` in its own [`tempfile::TempDir`].
//!
//! Two rules every case follows: `.timeout(CMD_TIMEOUT)` before `.output()`,
//! so a hang fails as a named assertion; and a [`DaemonGuard`] adopting the
//! `$SHEP_HOME` immediately after the `Output` that might have spawned a
//! daemon, before any assertion that could panic.
//!
//! Windows scripts are `.cmd` (see `script_header`); cases that cannot port
//! carry their own `#[cfg(unix)]`.

// The `#[cfg(unix)]` cases take their helpers and constants with them, so on
// Windows those items compile unused.
#![cfg_attr(windows, allow(dead_code))]
// A module holding only `#[cfg(unix)]` cases is empty on Windows, so its
// `use super::*` and this file's re-export of it are unused there.
#![cfg_attr(windows, allow(unused_imports))]

#[cfg(unix)]
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt as _;
use tempfile::TempDir;

mod adopt;
mod adopt_consent;
mod adopt_defaults;
mod adopted_help;
mod assertions;
mod available_dogs;
// Shared with `init.rs` and `term_panic_order.rs`, which are separate targets.
// The file gates itself to unix.
#[path = "../common/mod.rs"]
mod common;
mod constants;
mod daemon_log;
mod dev;
mod dial_stdio;
mod dispatch;
mod dog_channel;
mod dog_channel_samples;
mod dog_delete;
mod dog_grants;
mod dogs_barks;
mod fixtures;
mod flockfile_disabled;
mod flockfile_load;
mod guard;
mod handover_basic;
mod handover_clustered;
mod handover_disabled;
mod handover_dog_link;
mod handover_dogs;
mod handover_extras;
mod home_and_watch;
mod host_flag;
mod hosts;
mod import_env;
mod init_verb;
mod kv_store;
mod lifecycle;
mod logs;
mod lookout_whistle;
mod polling;
mod real_clock;
mod rendering;
mod runtime;
mod serve;
mod settings_get;
mod settings_set;
mod settings_unset;
mod spawn_failures;
mod ssh_stand_in;
mod startup;

pub(crate) use assertions::*;
pub(crate) use constants::*;
pub(crate) use fixtures::*;
pub(crate) use guard::*;
pub(crate) use handover_basic::*;
pub(crate) use handover_clustered::*;
pub(crate) use polling::*;
