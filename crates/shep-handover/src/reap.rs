//! Waiting for a child the old image started.
//!
//! An `execve` replaces the image and not the process, so a child the old
//! image spawned is still this process's child. The new image has no
//! `tokio::process::Child` for it and none can be built from a bare pid, so
//! this waits on the pid itself.

use std::io;

/// How a reaped child ended. A code and a signal are different fields and stay
/// that way: exactly one is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    /// The exit code, if the child exited.
    pub code: Option<i32>,
    /// The signal number, if a signal ended it.
    pub signal: Option<i32>,
}

/// Wait for one child of this process to end, and report how.
///
/// Targeted by pid, never `waitpid(-1)`, which would race a runtime's own
/// reaper. It arms its `SIGCHLD` stream before its first look, so an exit in
/// between is not missed. A status can be taken once, so call this once per
/// pid. Needs a Tokio runtime with the signal driver.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidInput`] if `pid` is zero or does not fit an `i32`.
/// - [`io::ErrorKind::NotFound`] if the kernel has no such child.
/// - [`io::ErrorKind::Unsupported`] on a platform with no `execve`.
/// - Any other `waitpid` errno, or a failed `SIGCHLD` arm, as-is.
pub async fn wait_for_child(pid: u32) -> io::Result<ChildExit> {
    #[cfg(unix)]
    {
        unix::wait(pid).await
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "waiting for a handed-over child is not supported on this platform",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::io;

    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use tokio::signal::unix::{SignalKind, signal};

    use super::ChildExit;

    pub(super) async fn wait(pid: u32) -> io::Result<ChildExit> {
        let pid = targeted(pid)?;
        let mut sigchld = signal(SignalKind::child())?;
        loop {
            match wait_once(pid) {
                Ok(Some(exit)) => return Ok(exit),
                Ok(None) => {}
                Err(Errno::ECHILD) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "pid {pid} is not a child of this process, so its exit status is lost"
                        ),
                    ));
                }
                Err(errno) => return Err(io::Error::from(errno)),
            }
            if sigchld.recv().await.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("the SIGCHLD stream closed while waiting for pid {pid}"),
                ));
            }
        }
    }

    /// `pid` as the one process `waitpid` should name.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for zero, and for anything too large
    /// to be an `i32`.
    fn targeted(pid: u32) -> io::Result<i32> {
        let pid = i32::try_from(pid).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {pid} is too large to wait on"),
            )
        })?;
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pid 0 is a group-wide wait, not a child",
            ));
        }
        Ok(pid)
    }

    /// One `waitpid(pid, WNOHANG)`, retried past `EINTR`.
    ///
    /// `Ok(None)` means the child is still running. Reporting `EINTR` as that
    /// would send the caller back to sleep on a `SIGCHLD` that may already
    /// have been the last one.
    fn wait_once(pid: i32) -> Result<Option<ChildExit>, Errno> {
        loop {
            match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
                Ok(status) => return Ok(exit_of(status)),
                Err(Errno::EINTR) => {}
                Err(errno) => return Err(errno),
            }
        }
    }

    /// `Stopped` and `Continued` need `WUNTRACED` and `WCONTINUED`, which
    /// nothing here passes; they map to "not an exit".
    fn exit_of(status: WaitStatus) -> Option<ChildExit> {
        match status {
            WaitStatus::Exited(_, code) => Some(ChildExit {
                code: Some(code),
                signal: None,
            }),
            WaitStatus::Signaled(_, signal, _) => Some(ChildExit {
                code: None,
                signal: Some(signal as i32),
            }),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn pid_zero_is_refused_as_a_group_wait() {
            let err = wait(0).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }

        #[tokio::test]
        async fn a_pid_too_large_for_an_i32_is_refused() {
            let err = wait(u32::MAX).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }

        #[tokio::test]
        async fn a_pid_that_is_not_a_child_is_reported_lost() {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            let err = wait(pid).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }

        #[tokio::test]
        #[allow(clippy::zombie_processes, reason = "the test under test is the reaper")]
        async fn a_childs_exit_code_is_reported() {
            let child = std::process::Command::new("sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap();
            let exit = wait(child.id()).await.unwrap();
            assert_eq!(
                exit,
                ChildExit {
                    code: Some(7),
                    signal: None
                }
            );
        }

        #[tokio::test]
        #[allow(clippy::zombie_processes, reason = "the test under test is the reaper")]
        async fn a_signalled_child_reports_the_signal_and_no_code() {
            let child = std::process::Command::new("sh")
                .args(["-c", "kill -KILL $$"])
                .spawn()
                .unwrap();
            let exit = wait(child.id()).await.unwrap();
            assert_eq!(
                exit,
                ChildExit {
                    code: None,
                    signal: Some(9)
                }
            );
        }
    }
}
