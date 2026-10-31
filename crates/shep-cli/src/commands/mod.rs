//! Per-verb command implementations, `daemon` first.
//!
//! `mod commands;` at `lib.rs` carries no platform gate: every module here
//! compiles on every platform, and a Unix call site gates itself.

pub mod admin;
pub mod bleats;
pub(crate) mod bounded;
pub(crate) mod copy;
pub mod daemon;
pub mod dev;
pub(crate) mod dial_stdio;
pub(crate) mod dog_migration;
pub mod dogs;
pub(crate) mod empty;
pub(crate) mod foreground;
// The `on-remove` hook runner, approved in docs/decisions.md and built
// ahead of a ruling on which verb fires it, so nothing calls it yet.
#[allow(dead_code)]
pub(crate) mod hook;
pub mod hosts;
pub mod import;
pub(crate) mod init;
pub mod kv;
pub mod lifecycle;
pub(crate) mod logon_task;
pub mod logs;
pub mod muster;
pub mod query;
// `shep runtime`'s PID-1 zombie reaper. Windows has no zombie state and no
// reparent-to-init rule, so there is nothing for a reaper to do.
#[cfg(unix)]
pub(crate) mod reap;
pub(crate) mod rpc;
pub mod runtime;
pub mod schema;
pub mod secret;
pub(crate) mod selector;
pub mod serve;
pub(crate) mod settings;
pub(crate) mod settings_read;
pub(crate) mod settings_write;
pub(crate) mod shep_toml;
pub mod signal;
// `shep startup` installs a boot-time unit on unix: systemd, launchd, openrc,
// or a BSD `rc.d` script. On Windows it registers a logon task instead, in
// `logon_task`, whose rendering compiles everywhere so its tests run there.
#[cfg(unix)]
pub(crate) mod startup;
pub(crate) mod startup_shared;
pub mod trigger;
pub mod whisper;
