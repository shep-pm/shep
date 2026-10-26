//! Signalling a process group.

#[cfg(unix)]
use core::fmt;

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{self, Signal as NixSignal};
#[cfg(unix)]
use nix::unistd::{Pid, getpgid};

/// A signal [`Tree::stop`](crate::Tree::stop) and `signal_group` (unix) can send.
///
/// Named rather than numbered: signal numbers differ by platform (`SIGUSR2`
/// is 12 on Linux and 31 on macOS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Signal {
    /// `SIGTERM`, the polite request to stop.
    Term,
    /// `SIGINT`, what Ctrl+C sends.
    Int,
    /// `SIGQUIT`.
    Quit,
    /// `SIGUSR2`.
    Usr2,
    /// `SIGKILL`, which the receiver cannot catch or ignore.
    Kill,
}

#[cfg(unix)]
impl Signal {
    fn to_nix(self) -> NixSignal {
        match self {
            Self::Term => NixSignal::SIGTERM,
            Self::Int => NixSignal::SIGINT,
            Self::Quit => NixSignal::SIGQUIT,
            Self::Usr2 => NixSignal::SIGUSR2,
            Self::Kill => NixSignal::SIGKILL,
        }
    }
}

/// Why a signal did not reach a process group.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GroupError {
    /// The pid is 0, 1 or above `i32::MAX`.
    ///
    /// `kill(0, ..)` signals the caller's own group and `kill(-1, ..)` every
    /// process the caller may signal, and a pid above `i32::MAX` has no form
    /// `kill(2)` takes.
    NotSignallable {
        /// The pid that was refused.
        pid: u32,
    },
    /// `kill(2)` itself failed.
    Delivery {
        /// The pid of the group leader.
        pid: u32,
        /// The `errno` `kill(2)` set.
        code: i32,
    },
}

#[cfg(unix)]
impl GroupError {
    /// Whether the failure was `ESRCH`: no process group is led by that pid,
    /// usually because every member has already exited.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        matches!(self, Self::Delivery { code, .. } if *code == Errno::ESRCH as i32)
    }
}

#[cfg(unix)]
impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSignallable { pid } => {
                write!(f, "pid {pid} is not a signallable process id")
            }
            Self::Delivery { pid, code } => write!(
                f,
                "signalling the group led by pid {pid} failed: {}",
                std::io::Error::from_raw_os_error(*code)
            ),
        }
    }
}

#[cfg(unix)]
impl core::error::Error for GroupError {}

/// Narrows a pid to the `i32` form `kill(2)` takes, refusing one it must not
/// take.
///
/// 0 is refused because `-0` is `0` and `kill(0, ..)` signals the caller's own
/// group; 1 because `-1` is `kill(-1, ..)`, every process the caller may
/// signal.
#[cfg(unix)]
fn signallable_pid(pid: u32) -> Result<i32, GroupError> {
    i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or(GroupError::NotSignallable { pid })
}

/// Sends `signal` to the whole process group led by `pid`.
///
/// `-pid` names the group a child spawned with `process_group(0)` leads, as
/// [`Tree::spawn`](crate::Tree::spawn) does. A descendant that calls `setsid`
/// lands in a session of its own, which this misses.
///
/// # Errors
///
/// [`GroupError::NotSignallable`] if `pid` is 0, 1 or above `i32::MAX`.
/// [`GroupError::Delivery`] if `kill(2)` failed, typically `ESRCH` because no
/// group is led by `pid` (see [`GroupError::is_gone`]).
#[cfg(unix)]
pub fn signal_group(pid: u32, signal: Signal) -> Result<(), GroupError> {
    send(pid, signal.to_nix())
}

/// [`signal_group`] over a raw nix signal, so a test can aim a harmless one.
#[cfg(unix)]
fn send(pid: u32, signal: NixSignal) -> Result<(), GroupError> {
    let leader = signallable_pid(pid)?;
    deliver(pid, Pid::from_raw(-leader), signal)
}

/// Sends `signal` to the one process `pid`, not its group.
///
/// For a child that has left the group it led, which [`signal_group`] no
/// longer reaches. The caller must know `pid` is not yet reaped, or it may
/// name a stranger.
#[cfg(unix)]
pub(crate) fn signal_process(pid: u32, signal: Signal) -> Result<(), GroupError> {
    let target = signallable_pid(pid)?;
    deliver(pid, Pid::from_raw(target), signal.to_nix())
}

/// Whether `pid` is in a process group other than the one it would lead.
///
/// `false` when that cannot be told, such as a process that is already gone.
#[cfg(unix)]
pub(crate) fn has_left_group(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    matches!(getpgid(Some(Pid::from_raw(raw))), Ok(group) if group.as_raw() != raw)
}

#[cfg(unix)]
fn deliver(pid: u32, target: Pid, signal: NixSignal) -> Result<(), GroupError> {
    signal::kill(target, signal).map_err(|errno| GroupError::Delivery {
        pid,
        code: errno as i32,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A harmless signal: if a guard regresses, the test fails instead of
    /// taking the harness's own group, or every process, down with it.
    const HARMLESS: NixSignal = NixSignal::SIGCONT;

    #[test]
    fn signal_group_refuses_pid_zero_and_one() {
        assert_eq!(
            send(0, HARMLESS),
            Err(GroupError::NotSignallable { pid: 0 })
        );
        assert_eq!(
            send(1, HARMLESS),
            Err(GroupError::NotSignallable { pid: 1 })
        );
        assert_eq!(
            send(0, HARMLESS).unwrap_err().to_string(),
            "pid 0 is not a signallable process id"
        );
    }

    #[test]
    fn signal_group_refuses_a_pid_too_large_for_the_os() {
        // The conversion refuses these before `kill(2)`, so a lethal signal
        // is safe here.
        for pid in [i32::MAX as u32 + 1, u32::MAX] {
            assert_eq!(
                signal_group(pid, Signal::Kill),
                Err(GroupError::NotSignallable { pid })
            );
        }
    }

    #[test]
    fn a_signallable_pid_converts_unchanged() {
        assert_eq!(signallable_pid(2), Ok(2));
        assert_eq!(signallable_pid(i32::MAX as u32), Ok(i32::MAX));
    }
}
