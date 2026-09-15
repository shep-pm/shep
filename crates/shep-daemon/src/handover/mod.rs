//! Whole-flock handover: whether this daemon's flock can be replaced in
//! place, the [`Handover`] blob that describes it, and the exec that carries
//! it.
//!
//! [`fitness`](fn@fitness) is the gate, and it refuses whole. One refusal: a
//! live sheep whose log pump did not report its descriptors in time. A
//! sheep's stdout, stderr, log files, stdin pipe and shepherd channel all
//! cross the exec, per sheep rather than per app.

pub(crate) mod adopt;
mod blob;
mod carried;
mod fds;
mod fitness;
pub(crate) mod reap;
pub(crate) mod uptime;

#[cfg(test)]
mod fixtures;

pub use blob::{Counters, DaemonFds, Handover};
pub(crate) use carried::SheepFd;
pub use carried::{CarriedFds, CarriedSheep};
pub use fitness::{Candidate, Fitness, OwnedCandidate, fitness};

#[allow(
    unused_imports,
    reason = "`Handover::read`'s error type, and the format number its tests name"
)]
pub use blob::{LoadError, VERSION};
#[allow(
    unused_imports,
    reason = "the payload of `Fitness::Refused`, named by this crate's own tests"
)]
pub use fitness::RefusedReason;

use core::convert::Infallible;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use shep_core::paths::ShepPaths;

/// Where this process's binary was when it started, as `argv[0]` resolved
/// against the startup directory. Set once by [`record_launch_path`], read
/// only by [`exec_target`].
///
/// The inner `Option` is "recorded, and there was nothing usable to record",
/// distinct from "never recorded", which is a missing call.
static LAUNCH_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Record how this process was invoked, for a later [`exec_target`].
///
/// Call once, before anything can move the current directory: it resolves
/// `argv[0]` against it. The first call wins.
pub fn record_launch_path() {
    let _ = LAUNCH_PATH.set(launch_path_from_argv());
}

/// The binary to `execv` for a handover, and never the running image.
///
/// Prefers the path [`record_launch_path`] recorded, falling back to
/// [`std::env::current_exe`], both through [`check_target`]. On Linux
/// `current_exe` reads `/proc/self/exe`, which after an upgrade renames a new
/// binary over the old one comes back as `"<path> (deleted)"` and cannot be
/// exec'd.
///
/// # Errors
/// - [`io::ErrorKind::NotFound`] if neither candidate is a file safe to exec.
pub fn exec_target() -> io::Result<PathBuf> {
    let recorded = LAUNCH_PATH.get().cloned().flatten();
    let current = std::env::current_exe();
    resolve_target(
        [recorded, current.as_deref().ok().map(Path::to_path_buf)],
        current.as_ref().err(),
    )
}

/// Returns the first entry of `candidates` that [`check_target`] accepts,
/// skipping a `None`. `current_exe_error` is folded into the diagnostic only.
///
/// [`crate::dogs::dog_app`] resolves a built-in dog's program through it too.
///
/// # Errors
/// [`io::ErrorKind::NotFound`] if every candidate is `None` or refused.
/// The message names each candidate tried and what was wrong with it.
pub(crate) fn resolve_target(
    candidates: [Option<PathBuf>; 2],
    current_exe_error: Option<&io::Error>,
) -> io::Result<PathBuf> {
    let mut refusals = Vec::new();
    for candidate in candidates {
        let Some(candidate) = candidate else { continue };
        match check_target(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(problem) => refusals.push(format!("{} ({problem})", candidate.display())),
        }
    }

    if let Some(e) = current_exe_error {
        refusals.push(format!("this process's own image ({e})"));
    }
    if refusals.is_empty() {
        refusals.push("no candidate at all".to_owned());
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no binary to exec: {}", refusals.join("; ")),
    ))
}

/// Why a candidate path is not safe to `execv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetProblem {
    /// The path carries Linux's `" (deleted)"` suffix, so it names an
    /// unlinked inode rather than a file.
    DeletedInode,
    /// Nothing is at the path, or it could not be read.
    Missing,
    /// Something is at the path, but it is a directory or a device.
    NotAFile,
}

impl core::fmt::Display for TargetProblem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::DeletedInode => "names a deleted inode, not a file",
            Self::Missing => "is not on disk",
            Self::NotAFile => "is not a file",
        };
        f.write_str(text)
    }
}

/// Whether `candidate` is a file this daemon may replace itself with.
///
/// The `" (deleted)"` check runs first and refuses even a path that really
/// does exist.
fn check_target(candidate: &Path) -> Result<(), TargetProblem> {
    if candidate.to_string_lossy().contains(" (deleted)") {
        return Err(TargetProblem::DeletedInode);
    }
    match std::fs::metadata(candidate) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(TargetProblem::NotAFile),
        Err(_) => Err(TargetProblem::Missing),
    }
}

/// This process's `argv[0]`, resolved against the current directory.
///
/// `None` when there is no `argv[0]`, when it is empty, or when it holds no
/// separator and so came from a `PATH` lookup this cannot undo. An absolute
/// `argv[0]` passes through the join unchanged.
fn launch_path_from_argv() -> Option<PathBuf> {
    let argv0 = PathBuf::from(std::env::args_os().next()?);
    if argv0.as_os_str().is_empty() {
        return None;
    }
    if argv0.is_absolute() {
        return Some(argv0);
    }
    let has_separator = argv0
        .parent()
        .is_some_and(|dir| !dir.as_os_str().is_empty());
    has_separator.then(|| std::env::current_dir().ok().map(|cwd| cwd.join(&argv0)))?
}

/// The environment variable a handover leaves for its successor, holding
/// the path of the blob it is to adopt.
///
/// Its presence is also the successor's only marker that it is one: an image
/// started any other way has no blob to read and boots normally.
pub const HANDOVER_ENV: &str = "SHEP_HANDOVER";

/// Replace this process with a fresh copy of the shep binary, handing it
/// `blob`'s flock.
///
/// Ordered: resolve the target, write the blob, clear `FD_CLOEXEC` on the
/// descriptors it names and only those, `execv`. A failed exec removes the
/// blob, which would describe a handover that never happened.
///
/// # Errors
/// No binary is safe to exec, the blob could not be written, a descriptor it
/// names is not open, or the exec failed. Each returns with no blob on disk
/// and `FD_CLOEXEC` back on every descriptor it cleared.
pub fn hand_over(blob: &Handover, paths: &ShepPaths) -> io::Result<Infallible> {
    exec_into(&exec_target()?, blob, paths)
}

/// [`hand_over`], against a caller-chosen binary.
///
/// # Errors
///
/// As [`hand_over`], minus the target resolution.
fn exec_into(target: &Path, blob: &Handover, paths: &ShepPaths) -> io::Result<Infallible> {
    let written = blob.write(paths)?;
    let failure = match exec_with_blob(target, blob, &written) {
        Ok(never) => match never {},
        Err(err) => err,
    };
    match fs::remove_file(&written) {
        Ok(()) | Err(_) => Err(failure),
    }
}

/// Clear `FD_CLOEXEC` on what `blob` names, then become `target`.
///
/// # Errors
///
/// A descriptor the blob names is not open, a path or an environment entry
/// holds an interior NUL, or the exec failed. On any of them this process is
/// still itself and `written` is still on disk, which [`exec_into`] cleans up.
fn exec_with_blob(target: &Path, blob: &Handover, written: &Path) -> io::Result<Infallible> {
    let mut cleared = Vec::new();
    let failure = match keep_and_exec(target, blob, written, &mut cleared) {
        Ok(never) => match never {},
        Err(err) => err,
    };
    // Put every descriptor back: the graceful-stop fallback leaves the
    // supervisor running, so a later spawn would inherit the listener, the
    // pidfile and every carried log descriptor.
    for fd in cleared {
        let _ = fds::close_raw_after_exec(fd);
    }
    Err(failure)
}

/// [`exec_with_blob`]'s body, recording what it cleared as it goes.
///
/// `cleared` is pushed to only after a clear succeeds, so it never names a
/// descriptor this process did not change.
///
/// # Errors
///
/// As [`exec_with_blob`].
fn keep_and_exec(
    target: &Path,
    blob: &Handover,
    written: &Path,
    cleared: &mut Vec<RawFd>,
) -> io::Result<Infallible> {
    for fd in blob.named_fds() {
        fds::keep_raw_across_exec(fd)?;
        cleared.push(fd);
    }

    let path = c_string(target.as_os_str().as_bytes())?;
    let argv = std::env::args_os()
        .map(|arg| c_string(arg.as_bytes()))
        .collect::<io::Result<Vec<_>>>()?;
    let env = successor_env(written)?;

    // `execve` rather than `execv`: `execv` inherits this process's `environ`,
    // so pointing the successor at the blob would need `std::env::set_var`,
    // unsafe in edition 2024 and unsound with this many threads.
    nix::unistd::execve(&path, &argv, &env).map_err(io::Error::from)
}

/// This process's environment, with [`HANDOVER_ENV`] set to `written`.
///
/// Any inherited value of that variable is dropped: a stale entry from an
/// earlier handover names a file that has already been read and unlinked.
///
/// # Errors
///
/// A name or value holds an interior NUL.
fn successor_env(written: &Path) -> io::Result<Vec<CString>> {
    let mut env = std::env::vars_os()
        .filter(|(name, _)| name != HANDOVER_ENV)
        .map(|(name, value)| {
            let mut entry = name.into_vec();
            entry.push(b'=');
            entry.extend(value.into_vec());
            c_string(&entry)
        })
        .collect::<io::Result<Vec<_>>>()?;

    let mut marker = HANDOVER_ENV.as_bytes().to_vec();
    marker.push(b'=');
    marker.extend(written.as_os_str().as_bytes());
    env.push(c_string(&marker)?);
    Ok(env)
}

/// `bytes` as a C string, with an interior NUL reported as an `io::Error`
/// rather than as a `NulError` nothing else in this module speaks.
fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::ProcessEntry;
    use crate::handover::fixtures::entry_fixture;

    #[test]
    fn the_exec_target_exists_and_is_a_file() {
        let p = exec_target().unwrap();
        assert!(p.is_file(), "{}", p.display());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_deleted_inode_path_is_never_returned() {
        let p = exec_target().unwrap();
        assert!(
            !p.to_string_lossy().contains("(deleted)"),
            "exec target resolved to a deleted inode: {}",
            p.display()
        );
    }

    #[test]
    fn resolve_target_refuses_a_synthetic_deleted_inode_candidate() {
        // `current_exe` cannot be made to return a `" (deleted)"` string on
        // this platform, so this hands `resolve_target` one directly.
        let deleted = PathBuf::from("/opt/shep/shep (deleted)");
        let err = resolve_target([None, Some(deleted)], None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no binary to exec: /opt/shep/shep (deleted) (names a deleted inode, not a file)"
        );
    }

    #[test]
    fn a_deleted_inode_candidate_is_refused_on_every_platform() {
        // The portable half of the Linux-only case, which a macOS run never
        // compiles.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep (deleted)");
        std::fs::write(&path, "an exec target that really is on disk").unwrap();

        assert_eq!(
            check_target(&path),
            Err(TargetProblem::DeletedInode),
            "existing on disk must not excuse the suffix"
        );
    }

    #[test]
    fn a_candidate_that_is_not_on_disk_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            check_target(&dir.path().join("never-written")),
            Err(TargetProblem::Missing)
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_target(dir.path()), Err(TargetProblem::NotAFile));
    }

    #[test]
    fn a_real_binary_passes_the_check() {
        assert_eq!(check_target(&std::env::current_exe().unwrap()), Ok(()));
    }

    #[test]
    fn argv0_resolves_against_the_startup_directory() {
        // The harness is invoked by an absolute path, so this proves the
        // argv[0] arm reaches a real file rather than that the join is right.
        let p = launch_path_from_argv().expect("argv[0] names a path");
        assert!(p.is_file(), "{}", p.display());
    }

    /// Names the directory the exec self-test's middle stage works in, and
    /// tells that stage it is not the ordinary run of the test.
    const SELFTEST_HOME: &str = "SHEP_HANDOVER_SELFTEST";

    /// The full path of the self-test, as libtest's `--exact` wants it.
    const SELFTEST_NAME: &str =
        "handover::tests::an_exec_replaces_the_image_and_keeps_a_descriptor";

    /// A blob naming real descriptors: `entry`'s sheep carries `fds`, and
    /// the listener and pidfile numbers are the caller's own open files.
    fn handover_with_fds(
        entry: &ProcessEntry,
        listener_fd: RawFd,
        pidfile_fd: RawFd,
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
    fn an_exec_replaces_the_image_and_keeps_a_descriptor() {
        // Three stages of the same test binary: the ordinary run re-runs this
        // one test in a child, which writes into a pipe and hands over, and
        // the image `hand_over` execs into reads that pipe back by number.
        if let Some(blob) = std::env::var_os(HANDOVER_ENV) {
            successor_stage(Path::new(&blob));
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
            "the child ran no test, so `{SELFTEST_NAME}` is not where this test lives any more;              `--exact` needs the full path and nothing updates it automatically: {stdout}"
        );
        // A pipe written before the exec is readable after it, on the same fd
        // number: the image changed and the descriptor crossed.
        assert!(stdout.contains("adopted: hello"), "{stdout}");
    }

    /// The middle stage: fill a pipe, name its read end in a blob, and hand
    /// over. Returns only if the exec failed, which is a test failure.
    fn exec_stage(home: &Path) -> ! {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;

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
            },
        );

        let err = hand_over(&blob, &paths).unwrap_err();
        panic!("the exec should not have returned: {err}");
    }

    /// The stage after the exec: read the blob this process was pointed at,
    /// and read the descriptor it names.
    fn successor_stage(blob_path: &Path) -> ! {
        let blob = Handover::read(blob_path).expect("the successor's blob");
        let fd = blob.sheep[0].fds.out_pipe.expect("a carried stdout pipe");
        let mut buf = [0_u8; 16];
        let read = nix::unistd::read(fd, &mut buf).expect("the carried descriptor is open");
        println!("adopted: {}", String::from_utf8_lossy(&buf[..read]));
        std::process::exit(0);
    }

    #[test]
    fn a_failed_exec_leaves_no_blob_behind() {
        use std::os::fd::AsRawFd as _;

        let dir = tempfile::tempdir().unwrap();
        let paths = selftest_paths(dir.path());
        let target = dir.path().join("not-a-binary");
        std::fs::write(&target, "this will never execute").unwrap();

        let listener = tempfile::tempfile().unwrap();
        let pidfile = tempfile::tempfile().unwrap();
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
            },
        );

        let err = exec_into(&target, &blob, &paths).unwrap_err();
        assert!(
            !Handover::path(&paths).exists(),
            "a failed exec left a blob behind: {err}"
        );
    }

    /// Both the daemon's own two descriptors and a carried log handle:
    /// `named_fds` yields them from two different places.
    #[test]
    fn a_failed_exec_makes_every_descriptor_close_on_exec_again() {
        use std::os::fd::{AsFd as _, AsRawFd as _};

        let dir = tempfile::tempdir().unwrap();
        let paths = selftest_paths(dir.path());
        let target = dir.path().join("not-a-binary");
        std::fs::write(&target, "this will never execute").unwrap();

        let listener = tempfile::tempfile().unwrap();
        let pidfile = tempfile::tempfile().unwrap();
        let out_log = tempfile::tempfile().unwrap();
        // The stdin write end and the channel's daemon end ride along: a
        // `named_fds` yielding four or five would leave them close-on-exec.
        let (_child_end, stdin) = std::io::pipe().unwrap();
        let (channel, _child_channel) = std::os::unix::net::UnixStream::pair().unwrap();
        let blob = handover_with_fds(
            &entry_fixture(|_| {}),
            listener.as_raw_fd(),
            pidfile.as_raw_fd(),
            CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: Some(out_log.as_raw_fd()),
                err_log: None,
                stdin: Some(stdin.as_raw_fd()),
                channel: Some(channel.as_raw_fd()),
            },
        );

        // The precondition, so the assertion below cannot pass on a
        // descriptor that was never cleared in the first place.
        for fd in [
            listener.as_fd(),
            pidfile.as_fd(),
            out_log.as_fd(),
            stdin.as_fd(),
            channel.as_fd(),
        ] {
            assert!(
                !fds::is_kept(fd).unwrap(),
                "the daemon opens everything close-on-exec, so this starts set"
            );
        }

        // Drives `keep_and_exec` directly so the clear is observable: with
        // `named_fds` yielding nothing, both assertions pass over untouched
        // flags.
        let mut cleared = Vec::new();
        let written = Handover::path(&paths);
        let _ = keep_and_exec(&target, &blob, &written, &mut cleared);
        cleared.sort_unstable();
        let mut expected = vec![
            listener.as_raw_fd(),
            pidfile.as_raw_fd(),
            out_log.as_raw_fd(),
            stdin.as_raw_fd(),
            channel.as_raw_fd(),
        ];
        expected.sort_unstable();
        assert_eq!(
            cleared, expected,
            "every named descriptor must be cleared, from both of the two \
             places `named_fds` draws them"
        );

        let err = exec_into(&target, &blob, &paths).unwrap_err();

        for fd in [
            listener.as_fd(),
            pidfile.as_fd(),
            out_log.as_fd(),
            stdin.as_fd(),
            channel.as_fd(),
        ] {
            assert!(
                !fds::is_kept(fd).unwrap(),
                "a failed exec left a descriptor exec-inheritable: {err}"
            );
        }
    }
}
