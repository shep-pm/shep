//! Deterministic scripted [`ProcessRunner`](crate::runner::ProcessRunner) for engine tests
//!
// WHY: deterministic + instant under the paused tokio clock; real OS process
// behavior is covered by `tests/real_runner.rs` and, on Windows, by
// `tests/real_runner_windows.rs`.

mod fake_process;
mod proc_script;
mod scripted_runner;
#[cfg(test)]
mod testing;
pub use fake_process::{FakeIo, FakeProc};
pub use proc_script::ProcScript;
pub use scripted_runner::{FIRST_SCRIPTED_PID, ScriptedRunner};
