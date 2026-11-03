//! Whether this host can give every sheep its own cgroup
//!
//! [`detect`] runs once at boot. On cgroup v2 with a subtree the shepherd may
//! write, it moves itself into a `shepherd` leaf and enables the controllers a
//! sheep's limits need: the [`Cgroups::Available`] base. Else it names a [`Reason`].
//!
//! Under an available base each sheep gets `<base>/sheep-<id>-<instance>`,
//! which [`join_on_exec`] creates and the child joins before its exec, with
//! `memory.max` ([`write_memory_max`]) and `cpu.max` ([`write_cpu_max`]) set
//! first and [`oom_kills`] read at the exit. [`remove_stale`] drops it.

use core::fmt;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use shep_core::config::CpuCores;

use crate::runner::RunnerError;

/// The cgroup v2 mount point every unified hierarchy is read from.
const MOUNT: &str = "/sys/fs/cgroup";

/// The leaf the shepherd lives in, since v2 refuses controllers on a cgroup
/// that holds processes.
const LEAF: &str = "shepherd";

const CPU_PERIOD_US: u64 = CpuCores::PERIOD_US;

/// Written to `cgroup.subtree_control`: the controllers a sheep's limits use.
const CONTROLLERS: &str = "+memory +cpu +pids";

/// What [`detect`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cgroups {
    /// The shepherd sits in `<base>/shepherd` and `base` can hold one child
    /// cgroup per sheep, with `memory`, `cpu` and `pids` enabled.
    Available { base: PathBuf },
    /// No sheep gets a cgroup, for this reason.
    Unavailable(Reason),
}

/// Why a host cannot give a sheep its own cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reason {
    /// `/proc/self/cgroup` holds a v1 line, no `0::` line, or the mount at
    /// `/sys/fs/cgroup` is not cgroup2.
    NotV2,
    /// The shepherd's own cgroup refused a write: it is not delegated to this
    /// user.
    NotDelegated,
    /// The shepherd's cgroup holds a process other than the shepherd, such as
    /// the CLI that started it, so v2 refuses controllers on it.
    Busy,
    /// A read or write failed for a reason other than permission, such as a
    /// controller the parent does not offer.
    Setup(io::ErrorKind),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotV2 => f.write_str("this host is not on cgroup v2 alone"),
            Self::NotDelegated => f.write_str("the shepherd's cgroup is not delegated to it"),
            Self::Busy => f.write_str("the shepherd's cgroup holds another process"),
            Self::Setup(kind) => write!(f, "setting up the shepherd's cgroup failed: {kind}"),
        }
    }
}

impl fmt::Display for Cgroups {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available { base } => {
                write!(f, "cgroup v2 available under {}", base.display())
            }
            Self::Unavailable(reason) => write!(f, "cgroups unavailable: {reason}"),
        }
    }
}

/// Finds the cgroup v2 subtree this shepherd may write, and moves the
/// shepherd into its leaf.
///
/// Idempotent: a shepherd already in `<base>/shepherd`, as after
/// `shep daemon reload`'s `execve`, finds the same base.
pub(crate) fn detect() -> Cgroups {
    detect_at(
        Path::new("/proc/self/cgroup"),
        Path::new(MOUNT),
        std::process::id(),
    )
}

/// [`detect`] against a `/proc/self/cgroup` file and a cgroup2 `mount`, which
/// a test points at a temp directory.
fn detect_at(proc_cgroup: &Path, mount: &Path, pid: u32) -> Cgroups {
    match try_detect(proc_cgroup, mount, pid) {
        Ok(found) => found,
        Err(reason) => Cgroups::Unavailable(reason),
    }
}

fn try_detect(proc_cgroup: &Path, mount: &Path, pid: u32) -> Result<Cgroups, Reason> {
    let text = fs::read_to_string(proc_cgroup).map_err(|err| Reason::Setup(err.kind()))?;
    let own = unified_path(&text).ok_or(Reason::NotV2)?;
    if !mount.join("cgroup.controllers").is_file() {
        return Err(Reason::NotV2);
    }

    let own_dir = mount.join(own.trim_start_matches('/'));
    let base = if own_dir.file_name().is_some_and(|name| name == LEAF) {
        own_dir.parent().unwrap_or(mount).to_path_buf()
    } else {
        if holds_another_process(&own_dir, pid) {
            return Err(Reason::Busy);
        }
        move_into_leaf(&own_dir.join(LEAF), pid)?;
        own_dir
    };
    fs::write(base.join("cgroup.subtree_control"), CONTROLLERS).map_err(|err| {
        match err.kind() {
            io::ErrorKind::ResourceBusy => Reason::Busy,
            _ => classify(&err),
        }
    })?;
    Ok(Cgroups::Available { base })
}

/// Whether `dir`'s `cgroup.procs` lists a pid other than `pid`. An unreadable
/// file is none: the write that follows reports the real error.
fn holds_another_process(dir: &Path, pid: u32) -> bool {
    fs::read_to_string(dir.join("cgroup.procs")).is_ok_and(|procs| {
        procs
            .split_whitespace()
            .any(|listed| listed.parse::<u32>().is_ok_and(|listed| listed != pid))
    })
}

/// Creates `leaf` and writes `pid` into it, removing a leaf this call made
/// if the write fails.
fn move_into_leaf(leaf: &Path, pid: u32) -> Result<(), Reason> {
    let created = match fs::create_dir(leaf) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
        Err(err) => return Err(classify(&err)),
    };
    fs::write(leaf.join("cgroup.procs"), pid.to_string()).map_err(|err| {
        if created {
            let _ = fs::remove_dir(leaf);
        }
        classify(&err)
    })
}

/// The path on the only line of a v2-only `/proc/self/cgroup`: a v1 line
/// anywhere, as on a hybrid host, is none.
fn unified_path(text: &str) -> Option<&str> {
    let mut lines = text.lines().filter(|line| !line.is_empty());
    let path = lines.next()?.strip_prefix("0::")?;
    lines.next().is_none().then_some(path)
}

fn classify(err: &io::Error) -> Reason {
    match err.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => Reason::NotDelegated,
        kind => Reason::Setup(kind),
    }
}

/// Where the sheep with `id` and `instance` lives under `base`. The name is a
/// function of both, so a successor after `shep daemon reload` finds it again.
pub(crate) fn sheep_dir(base: &Path, id: u32, instance: u32) -> PathBuf {
    base.join(format!("sheep-{id}-{instance}"))
}

/// Whether `name` is one [`sheep_dir`] makes.
fn is_sheep_dir(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix("sheep-"))
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(id, instance)| id.parse::<u32>().is_ok() && instance.parse::<u32>().is_ok())
}

/// Creates the cgroup at `dir` and makes `command`'s child join it before it
/// execs. A kill of any process in it takes the whole group, through
/// `memory.oom.group`. `memory_max` and `cpu_quota` are written first, by
/// [`write_memory_max`] and [`write_cpu_max`].
///
/// `dir` may already exist, as after a restart that reuses its name, but not
/// with a process in it: a sheep that outlived a crashed shepherd keeps its
/// cgroup, and a new sheep with the same id must not share it.
///
/// # Errors
/// What creating the directory, writing `memory.oom.group` or `memory.max`
/// or opening `cgroup.procs` reported, or [`io::ErrorKind::ResourceBusy`] when `dir`
/// already holds a process. A directory this call made is removed again.
pub(crate) fn join_on_exec(
    command: &mut std::process::Command,
    dir: &Path,
    memory_max: Option<u64>,
    cpu_quota: Option<u64>,
) -> io::Result<()> {
    let created = match fs::create_dir(dir) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
        Err(err) => return Err(err),
    };
    let procs = if is_occupied(dir) {
        Err(io::Error::from(io::ErrorKind::ResourceBusy))
    } else {
        fs::write(dir.join("memory.oom.group"), "1")
            .and_then(|()| memory_max.map_or(Ok(()), |limit| write_memory_max(dir, Some(limit))))
            .and_then(|()| cpu_quota.map_or(Ok(()), |quota| write_cpu_max(dir, Some(quota))))
            .and_then(|()| {
                fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join("cgroup.procs"))
            })
    }
    .inspect_err(|_| {
        if created {
            let _ = fs::remove_dir(dir);
        }
    })?;
    crate::sys::join_cgroup_on_exec(command, procs);
    Ok(())
}

/// Writes `limit` bytes to `dir`'s `memory.max`, or `max` (no cap) for `None`.
///
/// `memory.swap.max` follows, best effort: `0` under a cap, so a sheep over it
/// is killed rather than pushed to swap, and `max` again without one. A host
/// with no swap accounting has no such file, and the cap holds without it.
///
/// # Errors
/// What the `memory.max` write reported, such as a cgroup the `memory`
/// controller is not enabled for.
pub(crate) fn write_memory_max(dir: &Path, limit: Option<u64>) -> io::Result<()> {
    let (max, swap) = match limit {
        Some(bytes) => (bytes.to_string(), "0"),
        None => ("max".to_string(), "max"),
    };
    fs::write(dir.join("memory.max"), max)?;
    let _ = fs::write(dir.join("memory.swap.max"), swap);
    Ok(())
}

/// The microseconds of CPU time per [`CPU_PERIOD_US`] that `cores` allows,
/// rounded to the nearest microsecond.
pub(crate) fn cpu_quota(cores: CpuCores) -> u64 {
    cores.quota_us()
}

/// Writes `<quota> <period>` to `dir`'s `cpu.max`, or `max <period>` (no cap)
/// for `None`. The tree is throttled, never killed.
///
/// # Errors
/// What the `cpu.max` write reported, such as a cgroup the `cpu` controller
/// is not enabled for, or a quota under the kernel's one-millisecond minimum.
pub(crate) fn write_cpu_max(dir: &Path, quota: Option<u64>) -> io::Result<()> {
    let quota = quota.map_or_else(|| "max".to_string(), |micros| micros.to_string());
    fs::write(dir.join("cpu.max"), format!("{quota} {CPU_PERIOD_US}"))
}

/// How many of the cgroup's own processes the kernel's OOM killer has killed,
/// off `memory.events.local`. `0` when the file is absent or unreadable, as for
/// a cgroup not made yet.
///
/// Local, not `memory.events`: the latter also counts kills anywhere below
/// `dir`, so an ancestor's OOM could pass for this sheep's own. The counter is
/// cumulative, so a caller compares two readings.
pub(crate) fn oom_kills(dir: &Path) -> u64 {
    fs::read_to_string(dir.join("memory.events.local")).map_or(0, |text| parse_oom_kill(&text))
}

/// The `oom_kill` line's value in a `memory.events` listing, `0` without one.
fn parse_oom_kill(text: &str) -> u64 {
    text.lines()
        .filter_map(|line| line.strip_prefix("oom_kill "))
        .find_map(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Whether `dir`'s `cgroup.procs` lists a process. A listing is `<pid>\n` per
/// process, so its first byte is a digit: one byte tells, and an endless file
/// such as `/dev/full` is neither read to its end nor taken for a listing.
fn is_occupied(dir: &Path) -> bool {
    let mut first = [0u8; 1];
    fs::File::open(dir.join("cgroup.procs"))
        .and_then(|mut procs| io::Read::read(&mut procs, &mut first))
        .is_ok_and(|read| read == 1 && first[0].is_ascii_digit())
}

/// The error for a spawn that failed after [`join_on_exec`], which may be the
/// join itself: the child reports both the same way. Drops the cgroup.
pub(crate) fn spawn_failed(name: &str, dir: &Path, error: &io::Error) -> RunnerError {
    let _ = remove_if_empty(dir);
    RunnerError::SpawnFailed(format!(
        "{name} could not start in cgroup {}: {error}",
        dir.display()
    ))
}

/// Removes `dir` unless a process is still in it, which the kernel would
/// refuse anyway. A directory already gone is removed.
///
/// # Errors
/// What `cgroup.procs` or the removal reported, other than the directory
/// being absent.
pub(crate) fn remove_if_empty(dir: &Path) -> io::Result<()> {
    if is_occupied(dir) {
        return Ok(());
    }
    match fs::remove_dir(dir) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Drops the cgroup of a spawn that failed before the child existed, which no
/// exit will ever clean up. `None` is a spawn with no cgroup.
pub(crate) fn release(dir: Option<&Path>) {
    if let Some(dir) = dir {
        let _ = remove_if_empty(dir);
    }
}

/// Removes every empty `sheep-<id>-<instance>` cgroup under `base`, which a
/// shepherd that crashed left behind. One with a process in it stays, so a
/// sheep that outlived the crash or crossed a reload keeps its cgroup.
pub(crate) fn remove_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if !is_sheep_dir(&entry.file_name()) {
            continue;
        }
        if let Err(error) = remove_if_empty(&entry.path()) {
            tracing::warn!(%error, cgroup = %entry.path().display(), "stale sheep cgroup not removed");
        }
    }
}

#[cfg(test)]
mod tests;
