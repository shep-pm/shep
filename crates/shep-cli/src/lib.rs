//! `shep-cli`: clap command surface, output rendering, and the daemon
//! launch/re-exec path behind the `shep` binary.
//!
//! The public API is three entry points, [`main`], [`main_runtime`] and
//! [`main_dev`], one per `[[bin]]` target, each returning
//! [`std::process::ExitCode`] for the binary that calls it. Everything else
//! is private: embedding shep in another program is `shep-client`'s job.

#![forbid(unsafe_code)]

mod adopted;
mod cli;
mod client;
mod commands;
mod completions;
mod config;
mod dispatch;
mod dog;
mod dog_index;
mod entry;
mod exit;
mod fetch;
mod flourish;
mod home;
mod host;
mod http;
mod launch;
mod lookout;
mod output;
mod secret_readers;
mod serve;
mod shutdown;
mod status;
mod style;
mod terminal_safe;
mod version_guard;
mod vocabulary;
mod welcome;
mod whistle;

use entry::{alias_argv, run_argv};

/// The `shep` entry point. Parses this process's arguments and runs one verb.
///
/// Returns rather than exiting: the caller's `main` owns the process exit, so
/// the integration tier can call this without taking the harness down.
#[must_use]
pub fn main() -> std::process::ExitCode {
    run_argv(std::env::args_os().collect())
}

/// The `shep-runtime` entry point: `shep runtime`, with the verb supplied.
#[must_use]
pub fn main_runtime() -> std::process::ExitCode {
    run_argv(alias_argv("runtime", std::env::args_os().collect()))
}

/// The `shep-dev` entry point: `shep dev`, with the verb supplied.
#[must_use]
pub fn main_dev() -> std::process::ExitCode {
    run_argv(alias_argv("dev", std::env::args_os().collect()))
}
