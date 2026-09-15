//! Deterministic scripted [`ProcessRunner`](crate::runner::ProcessRunner) for engine tests
//!

mod fake_process;
mod proc_script;
mod scripted_runner;
#[cfg(test)]
mod testing;
pub use fake_process::{FakeIo, FakeProc};
pub use proc_script::ProcScript;
pub use scripted_runner::{FIRST_SCRIPTED_PID, ScriptedRunner};
