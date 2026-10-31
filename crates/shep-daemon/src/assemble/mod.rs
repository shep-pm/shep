//! Spawn assembly: pure functions that build [`SpawnSpec`](crate::runner::SpawnSpec) from app config.
//!
//! The assembler takes a validated `ResolvedApp` and produces a spec ready
//! for [`ProcessRunner::spawn`](crate::runner::ProcessRunner::spawn). The
//! daemon reads the defaults, paths, credentials and secret store before the
//! call; only the process env is read here, through a closure a test swaps.
//!
//! Only [`assemble`] may be spawned: it resolves every `{{secret:...}}` or refuses the spec.
//!
//! Public for `tests/real_runner/` and [`instance_slots`]'s doc example alone.

mod assembly_errors;
mod environment_inheritance;
mod log_path_resolution;
#[cfg(test)]
mod sandbox_tests;
mod slots;
mod spawn_spec;
#[cfg(test)]
mod testing;
pub use assembly_errors::AssembleError;
pub use slots::instance_slots;
pub use spawn_spec::assemble;
pub(crate) use spawn_spec::describe;
