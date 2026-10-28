//! The flock's two ends of the exec, on top of `shep-handover`.
//!
//! [`hand_over`] names the descriptors [`Handover::named_fds`] lists, each
//! under its own number, and passes the blob as the envelope's state.
//! `shep-handover` does the rest: the gate on the target, the probe, the
//! envelope, close-on-exec and the `execve`. [`arrive`] and [`claim`] are the
//! other end, which turns what it hands back into the numbers
//! [`adopt`](super::adopt::adopt) works from.
//!
//! [`exec_target`] and [`record_launch_path`] are the crate's, re-exported so
//! the shepherd and a built-in dog's program resolve the binary one way.

use core::convert::Infallible;
use std::io;
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::path::Path;
use std::time::Duration;

use shep_core::paths::ShepPaths;
use shep_handover::{Arrival, BootError, HandOverError, Verdict};

pub use shep_handover::{exec_target, record_launch_path};

use super::blob::{Handover, VERSION};
use crate::sys::OpenFd;

/// The environment variable a release before the envelope left for its
/// successor, holding the path of the blob it is to adopt.
///
/// This build no longer sets it, and still reads it: a reload from that release
/// onto this one hands over. The marker this build leaves is
/// [`shep_handover::HANDOVER_ENV`].
pub const HANDOVER_ENV: &str = "SHEP_HANDOVER";

/// How long the successor's binary gets to say it reads the blob's format.
///
/// The probe starts the target as the shepherd and stops at its `boot`, which
/// is after the arguments, the configuration and the logging are set up. Long
/// enough for that on a loaded machine, short enough that a target which never
/// answers costs a reload five seconds before the stop and start it falls back
/// to.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Replace this process with a fresh copy of the shep binary, handing it
/// `blob`'s flock.
///
/// The target is asked first whether it reads the blob's format, so a build
/// that cannot is refused here, with this image still running. Each
/// descriptor `blob` names is carried under its own number as a decimal
/// string, so the blob's own are the ones the successor adopts by. The
/// gates on the flock are the caller's and have already run.
/// # Errors
///
/// Returns only if the handover did not happen: a named descriptor is not
/// open, no binary is safe to exec, the target refuses the blob's format,
/// the blob could not be written, or the exec failed.
pub fn hand_over(blob: &Handover, paths: &ShepPaths) -> Result<Infallible, HandOverError> {
    hand_over_tuned(blob, paths, |handover| {
        handover.probe_timeout(PROBE_TIMEOUT)
    })
}

/// [`hand_over`], with `tune` free to change the crate's description of the
/// handover before it runs, so a test can aim the exec at a target that fails.
fn hand_over_tuned(
    blob: &Handover,
    paths: &ShepPaths,
    tune: impl for<'a> FnOnce(
        shep_handover::Handover<'a, Handover>,
    ) -> shep_handover::Handover<'a, Handover>,
) -> Result<Infallible, HandOverError> {
    let mut open = Vec::new();
    for number in blob.named_fds() {
        let fd = OpenFd::new(number).map_err(|source| HandOverError::Descriptor {
            key: number.to_string(),
            source: io::Error::new(io::ErrorKind::InvalidInput, source),
        })?;
        open.push((number.to_string(), fd));
    }

    let mut handover = shep_handover::Handover::new(blob).blob_dir(&paths.run);
    for (key, fd) in &open {
        handover = handover.carry(key.clone(), fd);
    }
    tune(handover).exec(&|_: &Path| Verdict::Carryable)
}

/// Whether this image is a successor of a shepherd that handed over through
/// `shep-handover`, and the flock it left.
///
/// Also answers a predecessor's probe and exits, so it has to be reached
/// before this process does anything it would not want done twice.
///
/// # Errors
///
/// As [`shep_handover::boot`]: the blob is unreadable, in a format this build
/// does not read, or names a descriptor that was not inherited. The
/// predecessor is gone, and every descriptor it carried is closed.
pub fn arrive() -> Result<Option<Arrival<Handover>>, BootError> {
    shep_handover::boot::<Handover>()
}

/// Turn an [`Arrival`] into the blob and the open descriptors it names, left
/// unowned for [`adopt`](super::adopt::adopt) to take by number.
///
/// Every number the blob names must have arrived under its own number as a key.
/// Without that check the number-based adoption could take a descriptor this
/// image opened. The descriptors are left open on a refusal too: a refused
/// rehydrate holds the pidfile lock, which stops a fresh boot from running
/// beside the sheep this one failed to adopt.
///
/// # Errors
///
/// The blob is not a version this image implements, or a number it names was
/// not carried under that number.
pub fn claim(arrival: Arrival<Handover>) -> io::Result<Handover> {
    let Arrival {
        state: blob,
        mut fds,
    } = arrival;
    let mut problem = (blob.version != VERSION).then(|| {
        format!(
            "the handover blob is format version {}, and this shep implements version {VERSION}",
            blob.version
        )
    });
    for number in blob.named_fds() {
        match fds.take(&number.to_string()) {
            Some(carried) => {
                let owned = carried.into_owned_fd();
                if owned.as_raw_fd() != number {
                    problem.get_or_insert_with(|| {
                        format!(
                            "the descriptor carried as '{number}' arrived as {}",
                            owned.as_raw_fd()
                        )
                    });
                }
                // Open and unowned, as an inherited descriptor is.
                let _ = owned.into_raw_fd();
            }
            None => {
                problem.get_or_insert_with(|| {
                    format!("the blob names descriptor {number}, which was not carried under it")
                });
            }
        }
    }
    problem.map_or(Ok(blob), |problem| {
        Err(io::Error::new(io::ErrorKind::InvalidData, problem))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::ProcessEntry;
    use crate::handover::fixtures::entry_fixture;
    use crate::handover::{CarriedFds, CarriedSheep};
    use shep_handover::Carried;
    use std::path::PathBuf;

    #[test]
    fn the_exec_target_exists_and_is_a_file() {
        let p = exec_target().unwrap();
        assert!(p.is_file(), "{}", p.display());
    }

    /// Names the directory the exec self-test's middle stage works in, and
    /// tells that stage it is not the ordinary run of the test.
    const SELFTEST_HOME: &str = "SHEP_HANDOVER_SELFTEST";

    /// The full path of the self-test, as libtest's `--exact` wants it.
    const SELFTEST_NAME: &str = "handover::exec::tests::a_flock_crosses_the_exec_and_is_claimed";

    /// A blob naming real descriptors: `entry`'s sheep carries `fds`, and
    /// the listener and pidfile numbers are the caller's own open files.
    fn handover_with_fds(
        entry: &ProcessEntry,
        listener_fd: std::os::fd::RawFd,
        pidfile_fd: std::os::fd::RawFd,
        fds: CarriedFds,
    ) -> Handover {
        Handover {
            version: VERSION,
            sheep: vec![CarriedSheep::from_entry(
                entry, 7, fds, false, None, false, None,
            )],
            listener_fd,
            pidfile_fd,
            next_id: 9,
            next_deadline: 5,
            next_action_stamp: 2,
            reloads: Some(Vec::new()),
        }
    }

    fn selftest_paths(home: &Path) -> ShepPaths {
        let home = home.display().to_string();
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.clone()),
            Path::new("/nonexistent"),
        );
        std::fs::create_dir_all(&paths.run).unwrap();
        paths
    }

    #[test]
    fn a_flock_crosses_the_exec_and_is_claimed() {
        // Three stages of the same test binary, the first thing each does
        // being what a shepherd's `main` does: ask `arrive`. The ordinary run
        // re-runs this one test in a child, which writes into a pipe and
        // hands over. The probe that `hand_over` starts first is the same
        // binary, and `arrive` answers it and exits. The image `hand_over`
        // execs into is the successor: it claims the flock and reads that
        // pipe back by the number the blob names.
        if let Some(arrival) = arrive().expect("a successor can arrive") {
            successor_stage(arrival);
        }
        if let Some(home) = std::env::var_os(SELFTEST_HOME) {
            exec_stage(Path::new(&home));
        }

        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(SELFTEST_NAME)
            .arg("--exact")
            .arg("--nocapture")
            .env(SELFTEST_HOME, dir.path())
            .env_remove(HANDOVER_ENV)
            .env_remove(shep_handover::HANDOVER_ENV)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // `--exact` against a stale name matches nothing: the child would run
        // zero tests, exit successfully and print no marker.
        assert!(
            stdout.contains("running 1 test"),
            "the child ran no test, so `{SELFTEST_NAME}` is not where this test lives any more; \
             `--exact` needs the full path and nothing updates it automatically: {stdout}"
        );
        // A pipe written before the exec is readable after it, on the same fd
        // number: the image changed and the descriptor crossed.
        assert!(stdout.contains("adopted: hello"), "{stdout}");
    }

    /// The middle stage: fill a pipe, name its read end in a blob, and hand
    /// over. Returns only if the exec failed, which is a test failure.
    fn exec_stage(home: &Path) -> ! {
        use std::io::Write as _;

        let paths = selftest_paths(home);
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"hello").unwrap();
        drop(writer);

        let listener = tempfile::tempfile().unwrap();
        let pidfile = tempfile::tempfile().unwrap();
        let blob = handover_with_fds(
            &entry_fixture(|_| {}),
            listener.as_raw_fd(),
            pidfile.as_raw_fd(),
            CarriedFds {
                out_pipe: Some(reader.as_raw_fd()),
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
                dog_link: None,
            },
        );

        let err = hand_over(&blob, &paths).unwrap_err();
        panic!("the exec should not have returned: {err}");
    }

    /// The stage after the exec: claim the flock, and read the descriptor the
    /// blob names.
    fn successor_stage(arrival: Arrival<Handover>) -> ! {
        let blob = claim(arrival).expect("the flock is claimed");
        let fd = blob.sheep[0].fds.out_pipe.expect("a carried stdout pipe");
        let mut buf = [0_u8; 16];
        let read = nix::unistd::read(fd, &mut buf).expect("the carried descriptor is open");
        println!("adopted: {}", String::from_utf8_lossy(&buf[..read]));
        std::process::exit(0);
    }

    #[test]
    fn a_descriptor_the_blob_names_and_this_image_does_not_have_is_refused_before_the_exec() {
        let dir = tempfile::tempdir().unwrap();
        let paths = selftest_paths(dir.path());
        let pidfile = tempfile::tempfile().unwrap();
        let blob = handover_with_fds(
            &entry_fixture(|_| {}),
            // Not open in any process: far above anything a test run holds.
            9_999,
            pidfile.as_raw_fd(),
            CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
                dog_link: None,
            },
        );

        let err = hand_over(&blob, &paths).unwrap_err();

        assert!(
            matches!(&err, HandOverError::Descriptor { key, .. } if key == "9999"),
            "{err}"
        );
        assert!(std::fs::read_dir(&paths.run).unwrap().next().is_none());
    }

    /// The five kinds of descriptor a shepherd carries, each open and, as the
    /// daemon opens everything, close-on-exec.
    struct FiveKinds {
        listener: std::fs::File,
        pidfile: std::fs::File,
        out_log: std::fs::File,
        stdin: std::io::PipeWriter,
        channel: std::os::unix::net::UnixStream,
        // Held so the other ends stay open.
        _peers: (std::io::PipeReader, std::os::unix::net::UnixStream),
    }

    impl FiveKinds {
        fn new() -> Self {
            let (child_end, stdin) = std::io::pipe().unwrap();
            let (channel, child_channel) = std::os::unix::net::UnixStream::pair().unwrap();
            Self {
                listener: tempfile::tempfile().unwrap(),
                pidfile: tempfile::tempfile().unwrap(),
                out_log: tempfile::tempfile().unwrap(),
                stdin,
                channel,
                _peers: (child_end, child_channel),
            }
        }

        fn all(&self) -> [std::os::fd::BorrowedFd<'_>; 5] {
            use std::os::fd::AsFd as _;
            [
                self.listener.as_fd(),
                self.pidfile.as_fd(),
                self.out_log.as_fd(),
                self.stdin.as_fd(),
                self.channel.as_fd(),
            ]
        }

        /// A blob naming all five: the daemon's own two come from the blob
        /// itself and the other three from the one sheep.
        fn blob(&self) -> Handover {
            handover_with_fds(
                &entry_fixture(|_| {}),
                self.listener.as_raw_fd(),
                self.pidfile.as_raw_fd(),
                CarriedFds {
                    out_pipe: None,
                    err_pipe: None,
                    out_log: Some(self.out_log.as_raw_fd()),
                    err_log: None,
                    stdin: Some(self.stdin.as_raw_fd()),
                    channel: Some(self.channel.as_raw_fd()),
                    dog_link: None,
                },
            )
        }
    }

    fn is_close_on_exec(fd: std::os::fd::BorrowedFd<'_>) -> bool {
        use nix::fcntl::{FcntlArg, FdFlag, fcntl};
        let flags = fcntl(std::os::fd::AsRawFd::as_raw_fd(&fd), FcntlArg::F_GETFD).unwrap();
        FdFlag::from_bits_retain(flags).contains(FdFlag::FD_CLOEXEC)
    }

    /// `named_fds` yields them from two different places, the blob's own two
    /// and each sheep's, and a handover carries exactly what it yields.
    #[test]
    fn the_blob_names_the_listener_the_pidfile_and_each_sheeps_descriptors() {
        let kinds = FiveKinds::new();

        let mut named: Vec<_> = kinds.blob().named_fds().collect();
        named.sort_unstable();

        let mut expected = kinds.all().map(|fd| std::os::fd::AsRawFd::as_raw_fd(&fd));
        expected.sort_unstable();
        assert_eq!(named, expected);
    }

    /// The exec is aimed at a file that cannot run, so it fails after the
    /// descriptors were made inheritable.
    fn failed_handover(kinds: &FiveKinds, paths: &ShepPaths, target: &Path) -> HandOverError {
        hand_over_tuned(&kinds.blob(), paths, |handover| {
            handover.target(target).unprobed()
        })
        .unwrap_err()
    }

    /// The setup both failed-exec tests share: a temp dir held alive for the
    /// test, its paths, a target file that cannot execute, and five open
    /// descriptors.
    fn failed_exec_sandbox() -> (tempfile::TempDir, ShepPaths, PathBuf, FiveKinds) {
        let dir = tempfile::tempdir().unwrap();
        let paths = selftest_paths(dir.path());
        let target = dir.path().join("not-a-binary");
        std::fs::write(&target, "this will never execute").unwrap();
        let kinds = FiveKinds::new();
        (dir, paths, target, kinds)
    }

    #[test]
    fn a_failed_exec_leaves_no_blob_behind() {
        let (_dir, paths, target, kinds) = failed_exec_sandbox();

        let err = failed_handover(&kinds, &paths, &target);

        assert!(
            std::fs::read_dir(&paths.run).unwrap().next().is_none(),
            "a failed exec left a blob behind: {err}"
        );
    }

    #[test]
    fn a_failed_exec_makes_every_descriptor_close_on_exec_again() {
        let (_dir, paths, target, kinds) = failed_exec_sandbox();
        // The precondition, so the assertion below cannot pass on a
        // descriptor that was never inheritable in the first place.
        for fd in kinds.all() {
            assert!(is_close_on_exec(fd), "the daemon opens everything so");
        }

        let err = failed_handover(&kinds, &paths, &target);

        assert!(matches!(err, HandOverError::Exec(_)), "{err}");
        for fd in kinds.all() {
            assert!(
                is_close_on_exec(fd),
                "a handover that failed left a descriptor open to every child: {err}"
            );
        }
    }

    #[test]
    fn an_arrival_that_carried_nothing_is_refused_by_the_numbers_the_blob_names() {
        let pidfile = tempfile::tempfile().unwrap();
        let listener = tempfile::tempfile().unwrap();
        let blob = handover_with_fds(
            &entry_fixture(|_| {}),
            listener.as_raw_fd(),
            pidfile.as_raw_fd(),
            CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
                dog_link: None,
            },
        );

        let err = claim(Arrival {
            state: blob,
            fds: Carried::default(),
        })
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("which was not carried"), "{err}");
    }

    #[test]
    fn a_blob_in_a_version_this_image_does_not_implement_is_refused() {
        let mut blob = handover_with_fds(
            &entry_fixture(|_| {}),
            -1,
            -1,
            CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
                dog_link: None,
            },
        );
        blob.version = VERSION + 1;
        blob.sheep.clear();

        let err = claim(Arrival {
            state: blob,
            fds: Carried::default(),
        })
        .unwrap_err();

        assert!(err.to_string().contains("version 2"), "{err}");
    }

    #[test]
    fn the_blobs_format_is_its_version() {
        use shep_handover::State as _;
        assert_eq!(Handover::FORMAT, VERSION);
        assert!(Handover::reads(VERSION));
        assert!(!Handover::reads(VERSION + 1));
    }
}
