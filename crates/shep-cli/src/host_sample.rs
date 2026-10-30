//! The CLI's in-process reading of the host, through `sysinfo`.
//!
//! Shared by the metrics dog and `whistle`. The shepherd's own reading for
//! `shep flock` is `HostUsage`, a different thing on the wire.

use sysinfo::{MemoryRefreshKind, ProcessRefreshKind, RefreshKind, System};

/// The machine the flock is running on, read through `sysinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReading {
    /// Total physical memory in bytes.
    pub memory_total_bytes: u64,
    /// Memory in use, as the platform reports it.
    pub memory_used_bytes: u64,
    /// How many processes the host is running, the flock included.
    pub processes: usize,
    /// Seconds since the host booted.
    pub uptime_seconds: u64,
}

/// Reads the host's memory, process count and uptime through one
/// short-lived `sysinfo::System` per call.
///
/// `None` on a target `sysinfo` does not support, which is expected
/// rather than an error a scrape should fail over.
pub(crate) fn sample_host() -> Option<HostReading> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            // `nothing()` alone still leaves `tasks` on: sysinfo's Linux
            // backend then counts every thread as a process.
            .with_processes(ProcessRefreshKind::nothing().without_tasks()),
    );
    Some(HostReading {
        memory_total_bytes: system.total_memory(),
        memory_used_bytes: system.used_memory(),
        processes: system.processes().len(),
        uptime_seconds: System::uptime(),
    })
}

/// [`sample_host`] on the blocking pool, awaited.
///
/// The walk is one long blocking sweep of the process table. Run inline
/// inside an `async fn` it stalls every other task on that tokio worker,
/// so both async callers come here instead. A panic inside the pool reads
/// as `None`.
pub(crate) async fn sample_host_off_worker() -> Option<HostReading> {
    tokio::task::spawn_blocking(sample_host)
        .await
        .unwrap_or_default()
}
