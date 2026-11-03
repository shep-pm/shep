//! Whether this host can give every sheep its own cgroup
//!
//! [`detect`] runs once at boot. On cgroup v2 with a subtree the shepherd may
//! write, it moves the shepherd into a `shepherd` leaf and enables the
//! controllers a sheep's limits need on the parent, which becomes the
//! [`Cgroups::Available`] base. Every other outcome names its [`Reason`].

use core::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The cgroup v2 mount point every unified hierarchy is read from.
const MOUNT: &str = "/sys/fs/cgroup";

/// The leaf the shepherd lives in, since v2 refuses controllers on a cgroup
/// that holds processes.
const LEAF: &str = "shepherd";

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

#[cfg(test)]
mod tests;
