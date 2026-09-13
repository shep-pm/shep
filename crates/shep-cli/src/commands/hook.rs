//! [`run_on_remove`]: asking a dog to clean up after itself.
//!
//! The dog is spawned with `on-remove` as its whole argv, under the
//! environment a supervised run would give it and a budget. Most dogs do
//! not know the argument and say so by exiting non-zero, which is
//! [`HookOutcome::Refused`] rather than anything having gone wrong.
//!
//! `tokio::process`, not the `std::process` poll loop `dogs::vet_binary`
//! runs: that probe nulls every stdio handle and never reads a byte, so it
//! cannot deadlock. This one reads, and a child filling one pipe while its
//! reader is parked on the other hangs until the budget kills it, losing
//! everything it wrote.

use core::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::dogs::dog_env;
use crate::terminal_safe::sanitise;

/// The whole argv a dog's hook is spawned with.
pub(crate) const ON_REMOVE: &str = "on-remove";

/// How long a dog gets to run its hook.
///
/// Five seconds, against the one second `dogs::VERSION_BUDGET` gives a
/// probe that only has to print a line. A hook does real work, flushing a
/// buffer or unlinking a file, and a cold binary on a loaded machine spends
/// 180 to 300ms reaching `main` before any of it starts.
pub(crate) const HOOK_BUDGET: Duration = Duration::from_secs(5);

/// The most of each stream an outcome keeps.
///
/// Four kibibytes per stream, against a hook that prints a line or two.
/// Per stream rather than over the pair, so a dog that explains itself on
/// stderr is still readable after a chatty stdout. The budget, not this,
/// is what bounds how much a spewing dog can write.
const HOOK_OUTPUT_LIMIT: usize = 4 * 1024;

/// What a dog did with its hook.
///
/// Nothing here is shep's own failure: the dog is a third-party binary, and
/// every outcome is a report of what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookOutcome {
    /// The dog exited 0.
    Ran {
        /// What it printed, stdout then stderr, each capped.
        output: String,
    },
    /// The dog exited non-zero, or a signal stopped it.
    ///
    /// A dog that does not know the argument and one whose hook tried and
    /// failed both land here: no exit code tells them apart, so the dog's
    /// own words travel with the outcome and the caller reports both the
    /// same way.
    Refused {
        /// The exit code, or `None` when a signal stopped it.
        code: Option<i32>,
        /// What it printed, on [`Self::Ran`]'s terms.
        output: String,
    },
    /// The dog was still running when the budget ran out, and was killed.
    /// Whatever it wrote is lost: a hook that does not finish has not said
    /// anything shep can act on.
    TimedOut {
        /// The budget that was exceeded.
        after: Duration,
    },
    /// The binary could not be spawned at all: gone since it was adopted,
    /// no longer executable, or not something this kernel can exec.
    NotSpawned {
        /// The OS error, rendered, since it is only ever shown.
        reason: String,
    },
}

impl fmt::Display for HookOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ran { .. } => f.write_str("ran its on-remove hook"),
            Self::Refused {
                code: Some(code), ..
            } => {
                write!(f, "refused the on-remove hook, exiting {code}")
            }
            Self::Refused { code: None, .. } => {
                f.write_str("was stopped by a signal during the on-remove hook")
            }
            Self::TimedOut { after } => {
                write!(f, "did not finish its on-remove hook within {after:?}")
            }
            Self::NotSpawned { reason } => write!(f, "could not be run: {reason}"),
        }
    }
}

/// Runs `binary`'s hook, bounded by `budget`.
///
/// `home` and `name` reach the dog as `$SHEP_HOME` and `$SHEP_DOG_NAME`,
/// through the same [`dog_env`] the adopt probe runs a candidate under:
/// this is a stranger's binary, and the operator's own environment is not
/// its business.
///
/// Both pipes are read concurrently, and whatever the dog printed is
/// stripped of terminal escapes before it is kept. A dog still running at
/// `budget` has its whole process group killed.
pub(crate) async fn run_on_remove(
    binary: &Path,
    home: &Path,
    name: &str,
    budget: Duration,
) -> HookOutcome {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg(ON_REMOVE)
        .env_clear()
        .envs(dog_env(home, name))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Backstop for the paths below that do not reach the kill ladder,
        // a panic between here and the timeout among them.
        .kill_on_drop(true);
    // A group of the hook's own, so the sweep below has one to name rather
    // than the leader alone. Mirrors `dogs::ask`.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return HookOutcome::NotSpawned {
                reason: err.to_string(),
            };
        }
    };
    // Read while the handle is alive: after the ladder below there is no
    // pid left to ask for, and this is the group the sweep names.
    let leader = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Bound the whole run, and let the borrow of `child` end at the
    // semicolon so the timeout arm can still reach it.
    let finished = tokio::time::timeout(budget, drain(&mut child, stdout, stderr)).await;

    match finished {
        Err(_elapsed) => {
            // The group first, and while it still has members: a sweep
            // after the leader is reaped names a pid the OS may have
            // handed to somebody else. `dogs::kill_probe_tree` orders it
            // the same way for the same reason.
            kill_group(leader);
            let _ = child.kill().await;
            HookOutcome::TimedOut { after: budget }
        }
        Ok(Err(err)) => HookOutcome::NotSpawned {
            reason: err.to_string(),
        },
        Ok(Ok((out, err, status))) => {
            let output = format!("{}{}", capped(&out), capped(&err));
            if status.success() {
                HookOutcome::Ran { output }
            } else {
                HookOutcome::Refused {
                    code: status.code(),
                    output,
                }
            }
        }
    }
}

/// Reads both pipes to end and then reaps `child`.
///
/// The two reads run together, which is the whole point: a dog that writes
/// more than one pipe buffer to stderr while a sequential reader is parked
/// on stdout blocks in its own `write` until the budget kills it, losing
/// everything it wrote.
///
/// # Errors
/// A read failed, or the wait did.
async fn drain(
    child: &mut tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> std::io::Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus)> {
    let (out, err) = tokio::try_join!(read_head(stdout), read_head(stderr))?;
    let status = child.wait().await?;
    Ok((out, err, status))
}

/// Drains `src` to end, keeping only its first [`HOOK_OUTPUT_LIMIT`] bytes.
///
/// To end, not to the cap: a reader that stopped at the cap would leave a
/// still-writing dog blocked on a full pipe, which is the deadlock this
/// module exists to avoid. Reading on and discarding costs nothing and
/// holds the kept bytes at a bound whatever the dog does.
///
/// `None` is a pipe that was never opened, which is no output.
///
/// # Errors
/// The underlying read failed.
async fn read_head<R: tokio::io::AsyncRead + Unpin>(src: Option<R>) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;

    let Some(mut src) = src else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let read = src.read(&mut chunk).await?;
        if read == 0 {
            return Ok(kept);
        }
        let room = HOOK_OUTPUT_LIMIT.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// SIGKILLs the process group `leader` leads, which `kill_on_drop` does
/// not: that reaches the leader alone, so a dog that forked leaves the
/// fork behind. `dogs::kill_probe_tree` sweeps the adopt probe's group for
/// the same reason and on the same terms.
///
/// A descendant that calls `setsid` leaves the group and survives either
/// way. Failure is never reported: an empty group answers `ESRCH` and the
/// outcome is the same.
#[cfg(unix)]
fn kill_group(leader: Option<u32>) {
    // POSIX holds a group id out of the pool while the group has members,
    // so this cannot name a stranger's. `-0` is this process's own group,
    // so zero must not pass.
    if let Some(pid) = leader.and_then(|id| i32::try_from(id).ok())
        && pid > 0
    {
        let group = nix::unistd::Pid::from_raw(-pid);
        let _ = nix::sys::signal::kill(group, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Windows has no process group to sweep; `kill_on_drop` is the whole of it.
#[cfg(windows)]
fn kill_group(_leader: Option<u32>) {}

/// `raw` as UTF-8, stripped of anything that could drive a terminal.
///
/// [`read_head`] has already bounded the length. Cutting a multi-byte
/// character in half yields a replacement character rather than dropping
/// the whole stream, which `read_to_string`'s all-or-nothing would.
///
/// Sanitised here rather than at whatever prints it, on the same terms as
/// `dog_index` and `fetch`: the bytes are a third party's, and a caller
/// that forgets is how an escape sequence reaches an operator's terminal.
/// [`sanitise`] collapses runs of whitespace, so the output arrives as one
/// line.
fn capped(raw: &[u8]) -> String {
    sanitise(&String::from_utf8_lossy(raw)).0
}

// `/bin/sh` fixtures, and a process group the kill can reach.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Bounds every test's own wait, so a hook that hangs fails here with
    /// its outcome named rather than at the harness's process timeout,
    /// which names nothing.
    const BOUND: Duration = Duration::from_secs(30);

    /// Short enough that the timing test costs a second, long enough that a
    /// `/bin/sh` fixture on a loaded runner is never killed mid-print.
    const TEST_BUDGET: Duration = Duration::from_secs(2);

    /// Writes an executable `/bin/sh` script at `dir/name`.
    fn dog(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Runs the hook under [`BOUND`], so a deadlock is a named failure.
    async fn run(binary: &Path, home: &Path) -> HookOutcome {
        tokio::time::timeout(BOUND, run_on_remove(binary, home, "watchdog", TEST_BUDGET))
            .await
            .expect("the hook runner must not outlive its own budget")
    }

    /// fails if the two pipes are drained one after the other. The fixture
    /// writes a quarter of a mebibyte to each, well past the 16KiB macOS
    /// and 64KiB Linux pipe buffers, so a reader parked on stdout leaves
    /// the child blocked in `write` on stderr until the budget kills it.
    /// That arrives as `TimedOut`, and the hook's output is lost with it.
    #[tokio::test]
    async fn a_dog_that_fills_both_pipes_is_drained_rather_than_deadlocked() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(
            dir.path(),
            "chatty",
            "printf 'out-marker\\n'\n\
             printf 'err-marker\\n' >&2\n\
             line=$(printf '%0256d' 0)\n\
             i=0\n\
             while [ \"$i\" -lt 1024 ]; do\n\
               printf '%s\\n' \"$line\"\n\
               printf '%s\\n' \"$line\" >&2\n\
               i=$((i + 1))\n\
             done",
        );

        let outcome = run(&binary, dir.path()).await;

        let HookOutcome::Ran { output } = &outcome else {
            panic!("a dog that exits 0 after writing both pipes ran its hook, got {outcome:?}");
        };
        assert!(
            output.contains("out-marker"),
            "stdout must reach the outcome"
        );
        assert!(
            output.contains("err-marker"),
            "and so must stderr, which a single cap over the pair would cut off"
        );
    }

    /// fails if a dog that does not know the argument is treated as a
    /// failure of shep's. Every dog written before this hook existed
    /// refuses, so the refusal is the ordinary answer, and the dog's own
    /// sentence is the only thing that says why.
    #[tokio::test]
    async fn a_dog_that_does_not_know_the_argument_refuses_and_is_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(
            dir.path(),
            "older",
            "printf 'unrecognized subcommand: %s\\n' \"$1\" >&2\nexit 2",
        );

        let outcome = run(&binary, dir.path()).await;

        let HookOutcome::Refused { code, output } = &outcome else {
            panic!("a non-zero exit is a refusal, got {outcome:?}");
        };
        assert_eq!(*code, Some(2));
        assert!(
            output.contains("unrecognized subcommand: on-remove"),
            "the dog's own words carry the reason: {output}"
        );
    }

    /// fails if the dog is handed a flag, a name, or anything else beside
    /// the verb. A dog dispatching on `$1` would take a second argument for
    /// one of its own.
    #[tokio::test]
    async fn the_hook_is_the_dogs_whole_argv() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(dir.path(), "echoing", "printf '%s|%s' \"$#\" \"$*\"");

        let outcome = run(&binary, dir.path()).await;

        assert_eq!(
            outcome,
            HookOutcome::Ran {
                output: "1|on-remove".to_string()
            }
        );
    }

    /// fails if a dog is run under the operator's environment rather than
    /// the shepherd's. `$CARGO` stands in for every variable a dog has no
    /// business reading; the precondition is asserted first, so a run
    /// without it fails here rather than passing on a variable that was
    /// never set.
    #[tokio::test]
    async fn a_hook_sees_the_shepherds_environment_and_not_the_operators() {
        assert!(
            std::env::var_os("CARGO").is_some(),
            "this test reads $CARGO to prove the clear; run it under cargo"
        );
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(
            dir.path(),
            "nosy",
            "printf '%s|%s|%s' \"$SHEP_HOME\" \"$SHEP_DOG_NAME\" \"${CARGO:-cleared}\"",
        );

        let outcome = run(&binary, dir.path()).await;

        let HookOutcome::Ran { output } = &outcome else {
            panic!("expected a clean run, got {outcome:?}");
        };
        assert_eq!(
            *output,
            format!("{}|watchdog|cleared", dir.path().display()),
            "the home and the name reach the dog; nothing else does"
        );
    }

    /// fails if a wedged dog holds the verb open past its budget. Real
    /// time, not a paused clock: the wait being bounded is the whole
    /// assertion, and a real child is what does the waiting.
    #[tokio::test]
    async fn a_dog_that_never_finishes_is_killed_at_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(dir.path(), "wedged", "sleep 600");

        let started = std::time::Instant::now();
        let outcome = run(&binary, dir.path()).await;

        assert_eq!(outcome, HookOutcome::TimedOut { after: TEST_BUDGET });
        assert!(
            started.elapsed() < BOUND,
            "the budget, not the test's own bound, is what ended it"
        );
    }

    /// fails if the kill at the budget reaches the dog and not what it
    /// forked. `kill_on_drop` signals the leader alone, so a dog that
    /// backgrounded any work leaves it running with nobody left to reap it.
    ///
    /// The grandchild writes its file after the budget has already expired,
    /// so its absence is the sweep and not a race with it.
    #[tokio::test]
    async fn a_fork_the_dog_left_behind_is_swept_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(
            dir.path(),
            "forking",
            "( sleep 4; : > \"$SHEP_HOME/grandchild\" ) &\n\
             sleep 600",
        );
        let survivor = dir.path().join("grandchild");

        let outcome = run(&binary, dir.path()).await;
        assert_eq!(outcome, HookOutcome::TimedOut { after: TEST_BUDGET });

        // Past the grandchild's own 4s, so a survivor has had its chance.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !survivor.exists(),
            "a fork outliving the dog kept running after the budget killed it"
        );
    }

    /// fails if a dog's output reaches an outcome carrying escape
    /// sequences. Everything else shep prints from a third party goes
    /// through the same sanitiser, and a hook's output is a third party's.
    #[tokio::test]
    async fn a_dogs_output_cannot_drive_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(dir.path(), "escaping", "printf 'clean\\033[2Jgone'");

        let outcome = run(&binary, dir.path()).await;

        let HookOutcome::Ran { output } = &outcome else {
            panic!("expected a clean run, got {outcome:?}");
        };
        assert!(
            !output.contains('\u{1b}'),
            "the escape must be stripped: {output:?}"
        );
        assert!(
            output.contains("clean") && output.contains("[2Jgone"),
            "and its printable tail must survive as inert text: {output:?}"
        );
    }

    /// fails if a dog whose binary went missing since it was adopted reads
    /// as a refusal. Nothing refused anything: there was no process.
    #[tokio::test]
    async fn a_binary_that_is_not_there_is_not_a_refusal() {
        let dir = tempfile::tempdir().unwrap();

        let outcome = run(&dir.path().join("never-installed"), dir.path()).await;

        assert!(
            matches!(outcome, HookOutcome::NotSpawned { .. }),
            "a missing binary is its own outcome, got {outcome:?}"
        );
    }

    /// fails if the cap is applied over the pair rather than per stream: a
    /// dog that fills stdout would then push its stderr out entirely.
    #[tokio::test]
    async fn each_stream_keeps_its_own_head() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dog(
            dir.path(),
            "lopsided",
            "line=$(printf '%08192d' 0)\n\
             printf '%s' \"$line\"\n\
             printf 'err-marker' >&2",
        );

        let outcome = run(&binary, dir.path()).await;

        let HookOutcome::Ran { output } = &outcome else {
            panic!("expected a clean run, got {outcome:?}");
        };
        assert_eq!(
            output.len(),
            HOOK_OUTPUT_LIMIT + "err-marker".len(),
            "stdout is cut to the cap and stderr arrives whole after it"
        );
        assert!(output.ends_with("err-marker"), "{output}");
    }
}
