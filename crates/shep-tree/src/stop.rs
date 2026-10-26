//! Spawning a child in a tree of its own and stopping it in a ladder.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use crate::group::Signal;

/// How often [`Tree::stop`] looks at whether the child has exited.
const POLL: Duration = Duration::from_millis(10);

/// The exit code a Windows job member reports once [`Tree::stop`] kills it.
#[cfg(windows)]
const KILLED_EXIT_CODE: u32 = 1;

/// A child and everything it starts, held together so they stop together.
///
/// On unix the tree is the child's process group; on Windows it is a job
/// object. Neither is closed or killed when a `Tree` drops: a child outlives
/// the process that started it, as it would from a shell.
#[derive(Debug)]
pub struct Tree {
    #[cfg(windows)]
    job: crate::job::Job,
}

impl Tree {
    /// Spawns `command` as the leader of a new tree.
    ///
    /// Returns the tree and the child it holds. Pass that same child to
    /// [`Tree::stop`].
    ///
    /// On unix this sets `process_group(0)` on `command`, so the child leads
    /// a group of its own and the shell's Ctrl+C no longer reaches it. On
    /// Windows the child is assigned to a new `Job` right
    /// after the spawn; a grandchild it forks before that assignment stays
    /// outside.
    ///
    /// # Errors
    ///
    /// Whatever the OS says if the child cannot be spawned or, on Windows, the
    /// job cannot be created or the child assigned to it. A child spawned and
    /// then not assigned is killed before the error returns.
    pub fn spawn(command: &mut Command) -> io::Result<(Self, Child)> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let child = command.process_group(0).spawn()?;
            Ok((Self {}, child))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            let job = crate::job::Job::create()?;
            let mut child = command.spawn()?;
            if let Err(error) = job.assign(child.as_raw_handle()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok((Self { job }, child))
        }
    }

    /// Stops `child` and everything in its tree, and returns how `child` exited.
    ///
    /// The ladder:
    ///
    /// 1. Send `signal` to the whole tree.
    /// 2. Poll `child` for up to `grace`.
    /// 3. Kill the whole tree, even if `child` has already exited, so a
    ///    descendant that ignored `signal` does not outlive its leader.
    ///
    /// `child` must be the one [`Tree::spawn`] returned with this tree. The
    /// call blocks for up to `grace` and then until the kill lands.
    ///
    /// Windows has no signal to deliver to a foreign process, so step 1 there
    /// does nothing and `child` gets the grace to exit by itself;
    /// [`Signal::Kill`] skips the wait.
    ///
    /// A child that has left its group with `setpgid` or `setsid` is signalled
    /// and killed by its own pid as well as the group, which its descendants
    /// that stayed in it keep reachable.
    ///
    /// If `child` exits within the grace, it is reaped before the final kill
    /// names its group by pid. While any member lives the group keeps the pid
    /// from being reused, but once the group is empty a new process could take
    /// it. `std` has no wait that leaves the child unreaped to close that gap.
    ///
    /// # Errors
    ///
    /// The OS error from waiting on `child`, or from a kill that failed for any
    /// reason other than the tree being already gone.
    pub fn stop(
        &self,
        child: &mut Child,
        signal: Signal,
        grace: Duration,
    ) -> io::Result<ExitStatus> {
        let mut status = None;
        if self.send(child, signal) {
            let deadline = Instant::now() + grace;
            loop {
                if let Some(exited) = child.try_wait()? {
                    status = Some(exited);
                    break;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                std::thread::sleep(POLL.min(left));
            }
        }
        self.kill(child, status.is_some())?;
        match status {
            Some(status) => Ok(status),
            None => child.wait(),
        }
    }

    /// Delivers `signal` to the tree. `false` when there is nothing to wait
    /// for: the signal was the kill, or could not be delivered.
    #[cfg(unix)]
    fn send(&self, child: &Child, signal: Signal) -> bool {
        signal != Signal::Kill && Self::deliver(child, signal).is_ok()
    }

    #[cfg(windows)]
    fn send(&self, _child: &Child, signal: Signal) -> bool {
        signal != Signal::Kill
    }

    /// The group `child` led, and `child` itself once it has left that group
    /// with `setpgid` or `setsid`, even while descendants keep the group alive.
    ///
    /// `child` must not be reaped: a reaped pid may name a stranger.
    #[cfg(unix)]
    fn deliver(child: &Child, signal: Signal) -> Result<(), crate::group::GroupError> {
        use crate::group::{has_left_group, signal_group, signal_process};

        let group = signal_group(child.id(), signal);
        let gone = group.as_ref().is_err_and(|error| error.is_gone());
        if gone || has_left_group(child.id()) {
            signal_process(child.id(), signal)?;
            return if gone { Ok(()) } else { group };
        }
        group
    }

    /// Kills the tree. `reaped` is whether `child` has been waited on, so its
    /// pid may be gone or reused: only the group is named then, and only a
    /// descendant can still be in it.
    #[cfg(unix)]
    fn kill(&self, child: &Child, reaped: bool) -> io::Result<()> {
        let result = if reaped {
            crate::group::signal_group(child.id(), Signal::Kill)
        } else {
            Self::deliver(child, Signal::Kill)
        };
        match result {
            Err(error) if !error.is_gone() => Err(io::Error::other(error)),
            _ => Ok(()),
        }
    }

    #[cfg(windows)]
    fn kill(&self, _child: &Child, _reaped: bool) -> io::Result<()> {
        self.job.terminate(KILLED_EXIT_CODE)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;
    use std::sync::mpsc;

    use super::*;

    /// Spawns `script` under `sh` in a tree, with stdout piped, and returns
    /// the first line it prints, which scripts print once they are ready.
    fn spawn_ready(script: &str) -> (Tree, Child, String) {
        let mut command = Command::new("sh");
        command.args(["-c", script]).stdout(Stdio::piped());
        let (tree, mut child) = Tree::spawn(&mut command).expect("sh must spawn");
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        (tree, child, line.trim().to_owned())
    }

    /// Kills the tree when a test ends, panicking or not, so a failed
    /// assertion does not leave a loop running.
    struct Cleanup(u32);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = crate::group::signal_group(self.0, Signal::Kill);
            let _ = crate::group::signal_process(self.0, Signal::Kill);
        }
    }

    #[test]
    fn a_child_that_ignores_term_is_killed_after_the_grace() {
        let (tree, mut child, line) =
            spawn_ready(r#"trap '' TERM; echo ready; while :; do sleep 1; done"#);
        let _cleanup = Cleanup(child.id());
        assert_eq!(line, "ready");

        let grace = Duration::from_millis(300);
        let started = Instant::now();
        let status = tree.stop(&mut child, Signal::Term, grace).unwrap();

        assert!(started.elapsed() >= grace, "the child was given its grace");
        assert_eq!(status.signal(), Some(9), "and then killed with SIGKILL");
    }

    #[test]
    fn a_child_that_exits_on_the_signal_is_not_made_to_wait_out_the_grace() {
        let (tree, mut child, _) = spawn_ready("echo ready; while :; do sleep 1; done");
        let _cleanup = Cleanup(child.id());

        let started = Instant::now();
        let status = tree
            .stop(&mut child, Signal::Term, Duration::from_secs(30))
            .unwrap();

        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(status.signal(), Some(15), "it died of SIGTERM, not SIGKILL");
    }

    #[test]
    fn a_grandchild_in_the_group_dies_with_the_child() {
        // The leader dies of SIGTERM at once; the grandchild ignores it, so
        // only the final kill can reach it. The grandchild holds the stdout
        // pipe's write end, so the pipe reaches end of file only once it is
        // gone, whatever an init that does not reap does with the corpse.
        let (tree, mut child, line) =
            spawn_ready(r#"sh -c 'trap "" TERM; echo ready; while :; do sleep 1; done' & wait"#);
        let _cleanup = Cleanup(child.id());
        assert_eq!(line, "ready");
        let mut stdout = child.stdout.take().unwrap();

        tree.stop(&mut child, Signal::Term, Duration::from_secs(30))
            .unwrap();

        let (done, eof) = mpsc::channel();
        std::thread::spawn(move || done.send(stdout.read_to_end(&mut Vec::new())));
        eof.recv_timeout(Duration::from_secs(10))
            .expect("the grandchild outlived its group")
            .unwrap();
    }

    #[test]
    fn a_child_that_left_its_group_is_still_stopped() {
        // `perl`, since `setsid(1)` is absent on macOS. The child puts itself
        // in a group of its own, so the group `spawn` made is empty and every
        // group signal answers `ESRCH`.
        stop_a_child_that_left_its_group(
            r#"exec perl -e '$SIG{TERM} = "IGNORE"; setpgrp(0, 0); $| = 1; print "ready\n"; sleep 600'"#,
        );
    }

    #[test]
    fn a_child_that_left_a_group_a_descendant_keeps_alive_is_still_stopped() {
        // The forked descendant stays in the original group, so group signals
        // succeed against it and never reach the child.
        stop_a_child_that_left_its_group(
            r#"exec perl -e '$SIG{TERM} = "IGNORE"; $| = 1; fork or sleep 600; setpgrp(0, 0); print "ready\n"; sleep 600'"#,
        );
    }

    /// Stops a child that ignores SIGTERM and has left its group, on a thread:
    /// a regression blocks in `wait`, and the test should fail rather than hang.
    fn stop_a_child_that_left_its_group(script: &str) {
        let (tree, mut child, line) = spawn_ready(script);
        let _cleanup = Cleanup(child.id());
        assert_eq!(line, "ready");

        let grace = Duration::from_millis(300);
        let started = Instant::now();
        let (done, stopped) = mpsc::channel();
        std::thread::spawn(move || done.send(tree.stop(&mut child, Signal::Term, grace)));
        let status = stopped
            .recv_timeout(Duration::from_secs(10))
            .expect("stop blocked on a child that left its group")
            .unwrap();

        assert!(started.elapsed() >= grace, "the child was given its grace");
        assert_eq!(status.signal(), Some(9));
    }
}
