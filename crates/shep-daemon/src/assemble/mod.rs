//! Spawn assembly: pure functions that build [`SpawnSpec`](crate::runner::SpawnSpec) from app config.
//!
//! The assembler takes a validated `ResolvedApp` and produces a fully-resolved
//! [`SpawnSpec`](crate::runner::SpawnSpec) ready for [`ProcessRunner::spawn`](crate::runner::ProcessRunner::spawn).
//! No I/O here: the defaults, the process env, the paths, the credentials and
//! the secret store are all read by the daemon before the assembler is called.
//!
//! Two builders, and only one of them may be spawned. [`assemble`] resolves
//! every `{{secret:...}}` and refuses the spec when one will not; the private
//! `describe` is for the callers that read a spec's log paths or build its
//! prober without ever starting a process.
//!
//! Public for its two out-of-crate readers and nothing else: `tests/real_runner.rs`
//! calls [`assemble`] to build a spec it then spawns for real, and
//! [`instance_slots`]'s doc example is compiled as its own crate.

mod assembly_errors;
mod environment_inheritance;
mod log_path_resolution;
mod slots;
mod spawn_spec;
#[cfg(test)]
mod testing;
pub use assembly_errors::AssembleError;
pub use slots::instance_slots;
pub use spawn_spec::assemble;
pub(crate) use spawn_spec::describe;
