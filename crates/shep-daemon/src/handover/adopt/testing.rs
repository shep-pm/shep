//! Fixtures and helpers shared by this module's tests.

use super::super::{CarriedFds, CarriedSheep, Handover, fds};
use super::adopt_descriptors::adopt;
use crate::handover::VERSION;
use crate::privilege::SpawnIdentity;
use shep_core::status::ProcStatus;
use std::io;
use std::os::fd::IntoRawFd as _;
use std::os::fd::RawFd;
use std::path::Path;

/// A number this process will never own, the same floor `sys`'s own
/// refusal tests use.
pub(super) const NEVER_OPEN: RawFd = 4096;

/// One carried sheep named `web`, whose descriptors are `fds`.
pub(super) fn carried(fds: CarriedFds) -> CarriedSheep {
    carried_slot(0, fds)
}

/// [`carried`], for a named instance slot of `web`.
///
/// The id and the pid move with the slot, so two of these describe two
/// instances of one app rather than the same one twice.
pub(super) fn carried_slot(instance: u32, fds: CarriedFds) -> CarriedSheep {
    CarriedSheep {
        id: instance + 1,
        name: "web".to_owned(),
        instance,
        pid: Some(u32::from(100 + u16::try_from(instance).unwrap())),
        restarts: 0,
        epoch: 7,
        status: ProcStatus::Online,
        last_exit: None,
        credentials: SpawnIdentity::Resolved(None),
        fds,
        pending_delete: Some(false),
        manual: None,
        reload: Some(crate::entry::ReloadState::None),
        dog: None,
        pending: None,
        pending_reidentifies: Some(false),
        ready_failed: Some(false),
        restart_due: None,
        app: crate::testing::app_with("web", |_| {}).into_config(),
    }
}

/// A blob naming a real listener bound at `socket`, a real pidfile, and
/// `sheep`.
///
/// Both are real: `adopt` refuses a blob whose listener or pidfile is
/// not open, so there is no such thing as a test blob without them.
pub(super) fn blob_with(socket: &Path, sheep: Vec<CarriedSheep>) -> Handover {
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let pidfile = tempfile::tempfile().unwrap();
    Handover {
        version: VERSION,
        sheep,
        listener_fd: listener.into_raw_fd(),
        pidfile_fd: pidfile.into_raw_fd(),
        next_id: 9,
        next_deadline: 5,
        next_action_stamp: 2,
        reloads: Some(Vec::new()),
    }
}

/// A predecessor's live descriptors: one of everything a blob names,
/// still owned here rather than leaked into a number.
///
/// [`blob_with`] hands its listener and pidfile to `into_raw_fd`, which
/// suits a case about `adopt`. [`dry_run`](crate::handover::adopt::dry_run)'s
/// contract is that the caller
/// still owns everything afterwards, which nothing can check against
/// numbers no value holds.
pub(super) struct Predecessor {
    dir: tempfile::TempDir,
    pub(super) listener: std::os::unix::net::UnixListener,
    pidfile: std::fs::File,
    out_log: std::fs::File,
    pub(super) out_read: std::io::PipeReader,
    pub(super) out_write: std::io::PipeWriter,
    pub(super) stdin_read: std::io::PipeReader,
    pub(super) stdin_write: std::io::PipeWriter,
    pub(super) channel: std::os::unix::net::UnixStream,
    pub(super) child_channel: std::os::unix::net::UnixStream,
}

impl Predecessor {
    /// One of each kind, all open, all the right way round.
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (out_read, out_write) = std::io::pipe().unwrap();
        let (stdin_read, stdin_write) = std::io::pipe().unwrap();
        let (channel, child_channel) = std::os::unix::net::UnixStream::pair().unwrap();
        Self {
            listener: std::os::unix::net::UnixListener::bind(dir.path().join("shep.sock")).unwrap(),
            pidfile: tempfile::tempfile().unwrap(),
            out_log: tempfile::tempfile().unwrap(),
            out_read,
            out_write,
            stdin_read,
            stdin_write,
            channel,
            child_channel,
            dir,
        }
    }

    /// Where this fixture's listener is bound.
    pub(super) fn socket(&self) -> std::path::PathBuf {
        self.dir.path().join("shep.sock")
    }

    /// A blob naming every one of them, for one sheep called `web`.
    pub(super) fn blob(&self) -> Handover {
        use std::os::fd::AsRawFd as _;
        Handover {
            version: VERSION,
            sheep: vec![carried(CarriedFds {
                out_pipe: Some(self.out_read.as_raw_fd()),
                err_pipe: None,
                out_log: Some(self.out_log.as_raw_fd()),
                err_log: None,
                stdin: Some(self.stdin_write.as_raw_fd()),
                channel: Some(self.channel.as_raw_fd()),
            })],
            listener_fd: self.listener.as_raw_fd(),
            pidfile_fd: self.pidfile.as_raw_fd(),
            next_id: 9,
            next_deadline: 5,
            next_action_stamp: 2,
            reloads: Some(Vec::new()),
        }
    }
}

/// How many descriptors this process holds, counted the only way a
/// portable test can: by asking about every number up to a bound.
///
/// The bound is generous rather than exact: this is used as a before and
/// after pair, so what matters is that the same numbers are asked about
/// both times.
pub(super) fn open_fd_count() -> usize {
    (0..512)
        .filter(|fd| crate::sys::adoptable_fd(*fd).is_ok())
        .count()
}

/// Run the real [`adopt`] over DUPLICATES of `blob`'s descriptors.
///
/// `adopt` takes ownership, so a case run against a [`Predecessor`]'s own
/// numbers would close the fixture. Duplicating renumbers, so a blob
/// naming one descriptor twice stops naming it twice here.
///
/// Closes every duplicate it holds, unless [`Self::release`] is called
/// first: `fds::duplicate_raw` hands back a bare number with no owner,
/// and a `Handover` holding it closes nothing on drop.
struct Duplicates(Vec<RawFd>);

impl Duplicates {
    fn of(&mut self, fd: RawFd) -> io::Result<RawFd> {
        let duplicate = fds::duplicate_raw(fd)?;
        self.0.push(duplicate);
        Ok(duplicate)
    }

    fn release(mut self) {
        self.0.clear();
    }
}

impl Drop for Duplicates {
    fn drop(&mut self) {
        for fd in self.0.drain(..) {
            let _ = nix::unistd::close(fd);
        }
    }
}

pub(super) fn adopt_a_copy(blob: &Handover) -> io::Result<()> {
    // Propagated, never `unwrap_or(fd)`: falling back to the original
    // hands `adopt` the fixture's own live descriptor to close.
    let mut dups = Duplicates(Vec::new());
    let mut copy = blob.clone();
    copy.listener_fd = dups.of(copy.listener_fd)?;
    copy.pidfile_fd = dups.of(copy.pidfile_fd)?;
    for sheep in &mut copy.sheep {
        sheep.fds = CarriedFds {
            out_pipe: sheep.fds.out_pipe.map(|fd| dups.of(fd)).transpose()?,
            err_pipe: sheep.fds.err_pipe.map(|fd| dups.of(fd)).transpose()?,
            out_log: sheep.fds.out_log.map(|fd| dups.of(fd)).transpose()?,
            err_log: sheep.fds.err_log.map(|fd| dups.of(fd)).transpose()?,
            stdin: sheep.fds.stdin.map(|fd| dups.of(fd)).transpose()?,
            channel: sheep.fds.channel.map(|fd| dups.of(fd)).transpose()?,
        };
    }
    // Released before the fallible call: `adopt` consumes the numbers it
    // reaches and says nothing about which, so holding the guard across it
    // would close some twice. A leak bounded by a test binary is better.
    dups.release();
    adopt(&copy).map(drop)
}
