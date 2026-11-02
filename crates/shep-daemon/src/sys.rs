//! This crate's only `unsafe` on unix: adopting a descriptor into a [`File`]
//!
//! Daemonization re-execs the CLI detached, and the child reports
//! `{pid, version}` on an inherited pipe; `open_resolving_no_symlinks` gets
//! its descriptor from `openat2`. Building an owning [`File`] from a raw
//! number is the one step std offers no safe path for.
//!
//! [`adopt_fd`] is `unsafe fn` rather than a safe wrapper: its ordering
//! precondition is a caller obligation it cannot check from inside.
//! [`adopt_handover_fd`] is safe because inheritance discharges it.
#![allow(unsafe_code)] // every unsafe site in this crate's unix build is in this file

use core::fmt;

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd};
use std::os::unix::io::{FromRawFd, RawFd};
#[cfg(target_os = "linux")]
use std::path::Path;

/// Lowest fd number this daemon will ever adopt. 0/1/2 are stdio; adopting
/// one into an owning [`File`] would steal it from whatever already holds it.
pub(crate) const RESERVED_FD_FLOOR: RawFd = 3;

/// Whether `fd` is a number this process could adopt at all: not stdio, and
/// open right now.
///
/// [`adopt_fd`]'s two checks without taking ownership, for the predecessor's
/// pre-exec rehearsal in `handover::adopt::dry_run`. Safe because it owns
/// nothing afterwards, so the recycling hazard has nowhere to land.
///
/// # Errors
/// - [`SysError::ReservedFd`]: `fd` is below 3 (stdio is owned elsewhere).
/// - [`SysError::BadFd`]: `fd` names no open descriptor in this process,
///   which is what a blob naming a descriptor that did not survive the exec
///   looks like.
pub fn adoptable_fd(fd: RawFd) -> Result<(), SysError> {
    if fd < RESERVED_FD_FLOOR {
        return Err(SysError::ReservedFd(fd));
    }
    // `F_GETFD` proves only that the number is open right now, never who
    // opened it; that half of the contract is `adopt_fd`'s caller's.
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).map_err(|errno| SysError::BadFd {
        fd,
        errno: errno.to_string(),
    })?;
    Ok(())
}

/// Takes ownership of a descriptor inherited across `exec`.
///
/// # Errors
/// - [`SysError::ReservedFd`]: `fd` is below 3 (stdio is owned elsewhere).
/// - [`SysError::BadFd`]: `fd` names no open descriptor in this process.
///
/// # Safety
///
/// Call this before this process opens or closes any descriptor of its own.
/// `F_GETFD` proves only that a number is open now, never what it names, so
/// a later call can adopt a number the kernel recycled into something this
/// process already owns, and the returned [`File`] closes that on drop.
pub unsafe fn adopt_fd(fd: RawFd) -> Result<File, SysError> {
    adoptable_fd(fd)?;
    // SAFETY: `adoptable_fd` returned `Ok`, so `fd` is at or above
    // `RESERVED_FD_FLOOR` and names a descriptor open in this process. The
    // caller's `# Safety` contract is what proves it is the intended
    // inherited pipe; the returned `File` becomes that number's sole owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Takes ownership of a descriptor a handover blob names.
///
/// [`adopt_fd`] for one situation: this process is a handover's successor and
/// `fd` was named in the handover blob before the `execve`. Safe because an
/// inherited descriptor is open before the new image runs, so no number this process opens later can collide with it; do
/// not call it on a descriptor number from anywhere else.
/// `handover::adopt::adopt` refuses a repeated number.
///
/// # Errors
/// - [`SysError::ReservedFd`]: `fd` is below 3 (stdio is owned elsewhere).
/// - [`SysError::BadFd`]: `fd` names no open descriptor in this process.
pub fn adopt_handover_fd(fd: RawFd) -> Result<File, SysError> {
    // SAFETY: `fd` was inherited across `execve` from this process's own
    // predecessor, which carried it and named it in the handover blob. It was
    // open before this image ran, so no number this process opens can share
    // it.
    unsafe { adopt_fd(fd) }
}

/// A descriptor this process holds open and knows only by number, borrowed
/// for as long as the value lives.
///
/// The handover blob names its descriptors as numbers, and `shep-handover`
/// carries a borrowed one. The owner of each is the supervisor's handle that
/// the blob was built from, which stays alive across the whole handover.
#[derive(Debug)]
pub(crate) struct OpenFd(RawFd);

impl OpenFd {
    /// Wrap `fd`, which must name a descriptor open right now.
    ///
    /// # Errors
    /// As [`adoptable_fd`].
    pub(crate) fn new(fd: RawFd) -> Result<Self, SysError> {
        adoptable_fd(fd)?;
        Ok(Self(fd))
    }
}

impl AsFd for OpenFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: `new` found the number open, and its owner holds it open
        // for the whole handover, which is the only time this is borrowed.
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

/// Duplicate `fd` onto a fresh number, for a caller that needs to inspect a
/// descriptor it must not take.
///
/// The duplicate shares the open file description but not ownership, so
/// closing it leaves the original open. It is above stdio, so
/// [`adoptable_fd`] cannot refuse it as reserved. The caller owns the number
/// that comes back.
///
/// # Errors
/// `fd` names no open descriptor, or this process is at its descriptor limit.
pub(crate) fn duplicate_fd(fd: RawFd) -> io::Result<RawFd> {
    // `borrow_raw` panics on -1 and is unsound on a closed number, so ask the
    // kernel first.
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD)?;
    // SAFETY: the check above found `fd` open, and the borrow ends with the
    // clone, which is a separate descriptor.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    Ok(borrowed.try_clone_to_owned()?.into_raw_fd())
}

/// Opens `path` through `openat2` with `RESOLVE_NO_SYMLINKS`, so no component
/// of it may be a symlink at the moment of the open.
///
/// `path` is resolved from the working directory as given. Linux only: the
/// syscall is, and so is the 5.6 kernel it arrived in.
///
/// # Errors
/// What the syscall reported: `ELOOP` for a symlink in the path, `ENOSYS` on a
/// kernel below 5.6, `EPERM` under a seccomp filter that blocks it, and the
/// ordinary `open(2)` errors otherwise.
#[cfg(target_os = "linux")]
pub(crate) fn open_resolving_no_symlinks(
    path: &Path,
    flags: nix::fcntl::OFlag,
    mode: nix::sys::stat::Mode,
) -> io::Result<File> {
    use nix::fcntl::{OpenHow, ResolveFlag, openat2};

    let how = OpenHow::new()
        .flags(flags)
        .mode(mode)
        .resolve(ResolveFlag::RESOLVE_NO_SYMLINKS);
    let fd = openat2(nix::libc::AT_FDCWD, path, how)?;
    // SAFETY: `openat2` returned `Ok`, so `fd` is a descriptor the kernel just
    // opened for this call and nothing else holds; the returned `File`
    // becomes its sole owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// An environment block for a child whose `LISTEN_PID` is its own pid.
///
/// systemd's contract is that `LISTEN_PID` equals the pid of the process that
/// reads it, so the number exists only after the fork. [`set_listen_env`]
/// installs this block as the child's `environ` between the fork and the
/// exec, and [`Self::install`] writes the pid into it there.
// Not `setenv` in a `pre_exec` hook: std applies a `Command`'s env after every
// hook, and `setenv` allocates, which is unsafe between fork and exec in a
// threaded process. Everything the hook touches is allocated before the fork,
// and it only calls `getpid`, writes the digits and stores one pointer.
pub(crate) struct ListenEnv {
    /// Every `KEY=value\0` string back to back, then `LISTEN_PID=` and room
    /// for the digits. Held only to keep the allocation `table` and `pid_slot`
    /// point into alive; never resized once they exist.
    _block: Vec<u8>,
    /// `environ`'s shape: a null-terminated array of pointers into `block`.
    table: Vec<*mut core::ffi::c_char>,
    /// Where the digits go: the byte after `LISTEN_PID=`, inside `block`.
    pid_slot: *mut u8,
}

// SAFETY: the raw pointers address only `block`, which `self` owns and which
// does not move when `self` does. Sharing a `&ListenEnv` reads nothing through
// them, and `install` takes `&mut self`.
unsafe impl Send for ListenEnv {}
// SAFETY: as `Send`: a shared reference exposes no access through the pointers.
unsafe impl Sync for ListenEnv {}

/// Room for the largest `u32` in decimal, plus its NUL.
const PID_SLOT_LEN: usize = 11;

impl ListenEnv {
    /// Builds the block from `vars`, with a `LISTEN_PID` entry still unset.
    ///
    /// # Errors
    /// - `InvalidInput` if a name is empty or holds `=`, or a name or value
    ///   holds a NUL, which are the cases `Command` refuses too.
    pub(crate) fn new(vars: &std::collections::BTreeMap<String, String>) -> io::Result<Self> {
        let mut block = Vec::new();
        let mut starts = Vec::with_capacity(vars.len() + 1);
        for (key, value) in vars {
            if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("environment variable `{key}` cannot be passed to a child"),
                ));
            }
            starts.push(block.len());
            block.extend_from_slice(key.as_bytes());
            block.push(b'=');
            block.extend_from_slice(value.as_bytes());
            block.push(0);
        }
        starts.push(block.len());
        block.extend_from_slice(b"LISTEN_PID=");
        let slot_at = block.len();
        block.resize(slot_at + PID_SLOT_LEN, 0);

        // Every pointer derives from this one, taken after the last resize.
        let base = block.as_mut_ptr();
        // SAFETY: each offset is at most `block.len()`, so it stays inside the
        // allocation `base` points into.
        let mut table: Vec<*mut core::ffi::c_char> = starts
            .iter()
            .map(|at| unsafe { base.add(*at) }.cast())
            .collect();
        table.push(core::ptr::null_mut());
        // SAFETY: `slot_at + PID_SLOT_LEN` is `block.len()`.
        let pid_slot = unsafe { base.add(slot_at) };
        Ok(Self {
            _block: block,
            table,
            pid_slot,
        })
    }

    /// Writes this process's pid into `LISTEN_PID` and makes the block the
    /// process's environment. Runs in the forked child before the exec.
    fn install(&mut self) -> io::Result<()> {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let pid = unsafe { nix::libc::getpid() }.unsigned_abs();
        let mut reversed = [0u8; PID_SLOT_LEN];
        let (mut rest, mut len) = (pid, 0);
        loop {
            reversed[len] = b'0' + (rest % 10) as u8;
            rest /= 10;
            len += 1;
            if rest == 0 {
                break;
            }
        }
        debug_assert!(len < PID_SLOT_LEN, "a u32 is ten digits at most");
        for (at, digit) in reversed[..len].iter().rev().enumerate() {
            // SAFETY: `at < len < PID_SLOT_LEN`, and `pid_slot` addresses
            // `PID_SLOT_LEN` writable bytes of `self._block`.
            unsafe { self.pid_slot.add(at).write(*digit) };
        }
        // SAFETY: as above, `len < PID_SLOT_LEN`; this is the terminator.
        unsafe { self.pid_slot.add(len).write(0) };
        // SAFETY: `environ_slot` names this process's environment pointer. The
        // table is null-terminated and its strings are NUL-terminated and
        // live in `self._block`, which outlives the exec.
        unsafe { *environ_slot() = self.table.as_mut_ptr() };
        Ok(())
    }
}

/// The address of the process's `environ` pointer.
///
/// # Safety
/// Only meaningful in a process no other thread is reading the environment
/// of: a forked child between `fork` and `exec`.
#[cfg(target_vendor = "apple")]
unsafe fn environ_slot() -> *mut *mut *mut core::ffi::c_char {
    // SAFETY: the call takes no arguments and returns the process's slot.
    unsafe { nix::libc::_NSGetEnviron() }
}

/// The address of the process's `environ` pointer.
///
/// # Safety
/// As the macOS definition.
#[cfg(not(target_vendor = "apple"))]
unsafe fn environ_slot() -> *mut *mut *mut core::ffi::c_char {
    unsafe extern "C" {
        static mut environ: *mut *mut core::ffi::c_char;
    }
    // SAFETY: `environ` is the C library's own symbol, present on every unix
    // target that is not Apple; taking its address reads nothing.
    unsafe { &raw mut environ }
}

/// Makes `command`'s child run with `env` as its whole environment, `LISTEN_PID`
/// included.
///
/// `command` must not have had `env`, `envs` or `env_clear` called: std
/// applies those after this hook and would replace the block.
pub(crate) fn set_listen_env(command: &mut std::process::Command, mut env: ListenEnv) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: `pre_exec` requires the closure to be async-signal-safe.
    // `ListenEnv::install` calls `getpid`, writes into memory allocated
    // before the fork and stores one pointer; it allocates, locks and panics
    // nowhere.
    unsafe { command.pre_exec(move || env.install()) };
}

/// Errors adopting an inherited descriptor.
///
/// `#[non_exhaustive]`: a future adoption check would need its own variant
/// rather than stretching [`Self::BadFd`] past what `fcntl` reported.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysError {
    /// The descriptor number cannot be adopted: negative, or below 3
    /// (stdio is owned elsewhere).
    ReservedFd(RawFd),
    /// The descriptor is not open in this process (carries the errno name).
    BadFd {
        /// The descriptor number that was rejected.
        fd: RawFd,
        /// The OS error name/message `fcntl` reported.
        errno: String,
    },
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedFd(fd) if *fd < 0 => {
                write!(f, "fd {fd} is negative and cannot name a descriptor")
            }
            Self::ReservedFd(fd) => {
                write!(f, "fd {fd} is reserved for stdio and cannot be adopted")
            }
            Self::BadFd { fd, errno } => {
                write!(
                    f,
                    "fd {fd} is not an open descriptor in this process: {errno}"
                )
            }
        }
    }
}

impl core::error::Error for SysError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::io::{AsRawFd, IntoRawFd};

    #[test]
    fn a_real_inherited_descriptor_is_adopted_and_owned() {
        // `into_raw_fd` leaves the descriptor in the state an exec-inherited
        // one is in: live, and owned by nobody.
        let (parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = child.into_raw_fd();
        {
            // SAFETY: this test process has opened nothing else of its own
            // between creating the pair and adopting it.
            let mut adopted = unsafe { adopt_fd(fd) }.unwrap();
            std::io::Write::write_all(&mut adopted, b"hello\n").unwrap();
        } // dropping the File closes the descriptor
        let mut read = String::new();
        let mut parent = parent;
        parent.read_to_string(&mut read).unwrap();
        assert_eq!(
            read, "hello\n",
            "EOF proves the adopted descriptor was closed on drop"
        );
    }

    #[test]
    fn stdio_numbers_are_refused() {
        for fd in 0..3 {
            // SAFETY: adopt_fd refuses fd < 3 before touching anything, so
            // there is no precondition to uphold for a call that never adopts.
            let result = unsafe { adopt_fd(fd) };
            assert_eq!(result.unwrap_err(), SysError::ReservedFd(fd));
        }
    }

    #[test]
    fn a_negative_fd_is_refused_with_an_accurate_message() {
        // SAFETY: `adopt_fd` refuses fd < 3 before touching anything.
        let err = unsafe { adopt_fd(-1) }.unwrap_err();
        assert_eq!(err, SysError::ReservedFd(-1));
        assert!(
            err.to_string().contains("negative"),
            "a negative fd is not \"reserved for stdio\": {err}"
        );
    }

    #[test]
    fn a_closed_descriptor_is_refused_instead_of_adopted() {
        // A high number, not the pair's own: unix hands out the lowest free
        // fd, so once this one is closed no concurrent test in this binary is
        // handed it back ahead of the ~2048 lower free ones. That floor is
        // what makes the second adoption a real `BadFd` probe, not a race.
        const PROBE_FD: RawFd = 2048;
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let parked = nix::fcntl::fcntl(a.as_raw_fd(), nix::fcntl::FcntlArg::F_DUPFD(PROBE_FD))
            .expect("duplicating onto a high fd number must succeed");
        assert!(
            parked >= PROBE_FD,
            "F_DUPFD must return a number at or above its floor, got {parked}"
        );
        // SAFETY: `parked` is a descriptor this test just created and owns
        // outright; `a` keeps its own separate descriptor and its own Drop.
        drop(unsafe { adopt_fd(parked) }.unwrap()); // adopt once, closing it
        // SAFETY: `parked` is closed now, and being >= PROBE_FD it cannot be
        // reallocated while lower numbers remain free, so this second
        // attempt probes a genuinely closed descriptor. BadFd is expected to
        // refuse it before `from_raw_fd` ever runs.
        let second = unsafe { adopt_fd(parked) };
        assert!(matches!(second, Err(SysError::BadFd { .. })));
    }

    #[test]
    fn a_fd_this_process_never_owned_is_refused() {
        // fd 4096 is a number this process never owns: fd-table limits sit far
        // below it, so `F_GETFD` fails deterministically even under a parallel harness.
        // SAFETY: adopt_fd's F_GETFD probe rejects it before `from_raw_fd` ever runs,
        // so there is no ordering precondition to uphold for a call that never adopts.
        let err = unsafe { adopt_fd(4096) }.unwrap_err();
        assert!(
            matches!(err, SysError::BadFd { fd: 4096, .. }),
            "a fd this process never owned must be refused as BadFd: {err:?}"
        );
    }

    #[test]
    fn a_listen_child_reads_its_own_pid_in_a_block_that_replaces_the_environment() {
        let vars = std::collections::BTreeMap::from([("FOO".to_string(), "bar baz".to_string())]);
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            r#"printf '%s|%s|%s|[%s]' "$LISTEN_PID" "$$" "$FOO" "$HOME""#,
        ]);
        set_listen_env(&mut command, ListenEnv::new(&vars).unwrap());

        let out = command.output().unwrap();

        let text = String::from_utf8(out.stdout).unwrap();
        let parts: Vec<&str> = text.split('|').collect();
        assert_eq!(
            parts[0], parts[1],
            "LISTEN_PID is the child's own pid: {text}"
        );
        assert!(parts[0].parse::<u32>().is_ok(), "{text}");
        assert_eq!((parts[2], parts[3]), ("bar baz", "[]"), "{text}");
    }

    #[test]
    fn two_children_of_one_block_each_read_their_own_pid() {
        let mut pids = Vec::new();
        for _ in 0..2 {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", r#"printf '%s' "$LISTEN_PID""#]);
            set_listen_env(&mut command, ListenEnv::new(&Default::default()).unwrap());
            let out = command.output().unwrap();
            pids.push(String::from_utf8(out.stdout).unwrap());
        }
        assert_ne!(pids[0], pids[1]);
    }

    #[test]
    fn a_name_or_value_a_command_would_refuse_is_refused_up_front() {
        for (key, value) in [("", "v"), ("A=B", "v"), ("A\0B", "v"), ("A", "v\0")] {
            let vars = std::collections::BTreeMap::from([(key.to_string(), value.to_string())]);
            let error = ListenEnv::new(&vars).err().expect("refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{key:?}");
        }
    }

    /// The property that closes the race: whatever resolved the path before,
    /// a link in it when the syscall runs is refused. A caller's own walk
    /// cannot stand in for this, since a link can appear after it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_symlink_in_the_path_is_refused_by_the_syscall_itself() {
        use nix::fcntl::OFlag;
        use nix::sys::stat::Mode;

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("logs")).unwrap();
        let flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC;
        let mode = Mode::from_bits_truncate(0o600);

        let refused = open_resolving_no_symlinks(&dir.path().join("logs/web-out.log"), flags, mode);

        assert_eq!(
            refused.unwrap_err().raw_os_error(),
            Some(nix::libc::ELOOP),
            "a link met mid-path must be ELOOP"
        );
        assert!(!real.join("web-out.log").exists());
        open_resolving_no_symlinks(&real.join("web-out.log"), flags, mode)
            .expect("the same file by its real path opens");
    }
}
