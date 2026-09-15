//! Real [`crate::runner::ProcessRunner`] over actual OS processes.
//!
//! Split by concern: `runner` holds `TokioRunner` and `TokioProc` and their
//! trait impls, `log_file` is the buffered per-stream log writer, and `pump`
//! holds the tasks that drain a child's stdout, stderr and shepherd channel.

use core::time::Duration;

/// Capacity of every channel a spawn wires up: enough that a bursty child
/// does not back-pressure a sheep task that is merely slow to poll, without
/// buffering unboundedly.
const CHANNEL_CAPACITY: usize = 32;

/// Bytes a log file buffers before the pump writes them through.
///
/// `tokio::fs::File` hands every `write` to the blocking pool: 32.8 us of
/// daemon CPU per line against 0.99 us for the `write(2)` under it. Batching
/// amortises one dispatch over a whole buffer.
const LOG_BUFFER: usize = 8 * 1024;

/// How long a line may sit in that buffer before the pump writes it out
/// anyway.
///
/// Timed from the oldest unflushed line, so a steady trickle cannot push the
/// deadline out indefinitely. It exists for the sheep that logs one line and
/// then goes quiet.
const IDLE_FLUSH: Duration = Duration::from_millis(50);

/// Bytes each stream's reader may hold ahead of the lines it has emitted.
///
/// Two bounds rest on it: the most a handover can strand in userspace, since
/// bytes taken off the pipe die with the image at the `execve`, and the most
/// [`pump::drain_ready`] can write.
#[cfg(unix)]
const READ_BUFFER: usize = 8 * 1024;

/// How long the pump keeps reading after its sheep task has let go.
///
/// Without it [`pump::spawn_log_pump`]'s `select!` can take the
/// `logs_tx.closed()` branch with the child's last line still in the pipe:
/// `tokio::select!` picks between ready branches at random. A reaped child's
/// write ends are closed, so the common case answers EOF at once. The budget
/// is spent only when a lamb still holds a write end, and it does not bound
/// the lamb.
///
/// 100ms against a worst case of 7 to 12ms for two full pipes;
/// `both_pipes_filled_to_capacity_drain_inside_the_budget` pins it. Not
/// wider: a draining pump does not poll `ctl_rx`, and a handover gives each
/// pump one `REPORT_DEADLINE` (2s) to answer.
const FINAL_DRAIN: Duration = Duration::from_millis(100);

mod log_file;
mod pump;
mod runner;

#[cfg(test)]
mod tests;

pub use runner::{TokioProc, TokioRunner};

pub(crate) use log_file::{open_append, record_lock};
#[cfg(unix)]
pub(crate) use runner::signal_group;
