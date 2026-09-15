//! The hidden `daemon` subcommand: runs the supervisor in this process.
//!
//! [`run_daemon`] loads `shep.toml`, boots `shep_daemon::boot`'s supervisor,
//! and blocks in `RunningDaemon::run` until a signal or `KillDaemon` tears it
//! down. Autostart and `--foreground` share one code path; the flag adds
//! readiness reporting and nothing else, so an inherited `$NOTIFY_SOCKET`
//! cannot make an autostarted daemon answer another service's unit. Detaching
//! and the stderr redirect into `shepd.err.log` live in `launch.rs`.

mod lifecycle;
mod reload;
mod reload_report;

pub use lifecycle::{boot_supervisor, daemon_exit_code, run_daemon};
pub use reload::reload;
