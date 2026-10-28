//! The blob a shepherd leaves for its successor: its format and its version
//! gate.
//!
//! [`Handover`] is the whole of what crosses the exec besides the descriptors
//! it names by number. It crosses as the state of a `shep-handover` envelope,
//! which writes it to disk at mode `0600` because it carries every sheep's
//! environment. A blob a release before the envelope wrote is read back
//! through [`Handover::read`] and [`LoadError`], which refuse a blob whose
//! format version this image does not implement rather than adopting a
//! partial picture of a live flock.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd as _, RawFd};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{CarriedSheep, DogConnection};
use crate::supervisor::CarriedReload;

/// The blob format this daemon writes, and the only one it can read.
///
/// [`Handover::load_value`] refuses any other number outright: an image that
/// cannot understand the blob must not adopt a partial picture of a live
/// flock.
pub const VERSION: u32 = 1;

/// The file name a release before the envelope wrote its blob under, inside
/// `$SHEP_HOME/run`.
#[cfg(test)]
const FILE_NAME: &str = "handover.json";

/// Everything the successor needs to keep supervising a flock it did not
/// spawn.
///
/// Written just before the `execve`, read once by the incoming image, and
/// unlinked by that reader. Besides the descriptors it names by number, this
/// is the whole of what crosses. It carries each sheep's environment, so it
/// goes to disk at mode `0600`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    /// The format this blob was written in; see [`VERSION`].
    pub(super) version: u32,
    /// Every sheep the successor is to adopt, in no particular order.
    pub(super) sheep: Vec<CarriedSheep>,
    /// The control listener's descriptor number.
    pub(super) listener_fd: RawFd,
    /// The pidfile lock's descriptor number.
    ///
    /// `flock` is a property of the open file description, so holding this
    /// descriptor open holds the lock. Re-acquiring it instead would open a
    /// window for a second daemon to win it.
    pub(super) pidfile_fd: RawFd,
    /// The supervisor's next entry id.
    pub(super) next_id: u32,
    /// The supervisor's next reload-watchdog stamp.
    pub(super) next_deadline: u64,
    /// The supervisor's next action-wait stamp.
    pub(super) next_action_stamp: u64,
    /// Every app whose reload was still in flight at the exec.
    ///
    /// An absent key loads as `None`, which means no reload was in flight.
    /// Sorted by app name by its writer, since the blob is a file an operator
    /// may read.
    pub(super) reloads: Option<Vec<CarriedReload>>,
}

impl Handover {
    /// Describe a flock for the successor.
    #[must_use]
    pub fn new(
        sheep: Vec<CarriedSheep>,
        fds: DaemonFds,
        counters: Counters,
        reloads: Vec<CarriedReload>,
    ) -> Self {
        Self {
            version: VERSION,
            sheep,
            listener_fd: fds.listener,
            pidfile_fd: fds.pidfile,
            next_id: counters.next_id,
            next_deadline: counters.next_deadline,
            next_action_stamp: counters.next_action_stamp,
            reloads: Some(reloads),
        }
    }

    /// Every sheep this blob carries.
    #[must_use]
    #[allow(dead_code, reason = "read by this crate's own tests")]
    pub fn sheep(&self) -> &[CarriedSheep] {
        &self.sheep
    }

    /// Names each of `links` against the dog whose connection it is.
    ///
    /// Matched on name and pid together: a dog respawned since its connection
    /// was opened is a different process on a different connection, and the
    /// old one must not be handed to it. A link matching no sheep is left out
    /// and closes at the exec, which the dog meets as a lost connection and
    /// answers by falling back to the control socket.
    pub fn carry_dog_links(&mut self, links: &[DogConnection]) {
        for sheep in &mut self.sheep {
            let Some(link) = links
                .iter()
                .find(|link| sheep.pid() == Some(link.pid) && sheep.name() == link.name)
            else {
                continue;
            };
            sheep.carry_dog_link(link.fd.as_raw_fd(), link.topics.clone());
        }
    }

    /// The entry id the successor is to issue next.
    #[must_use]
    #[allow(dead_code, reason = "read by this crate's own tests")]
    pub const fn next_id(&self) -> u32 {
        self.next_id
    }

    /// The three counters the successor restores before installing any sheep.
    #[must_use]
    pub const fn counters(&self) -> Counters {
        Counters {
            next_id: self.next_id,
            next_deadline: self.next_deadline,
            next_action_stamp: self.next_action_stamp,
        }
    }

    /// Every app whose reload was still in flight at the exec.
    ///
    /// Empty both for a flock with nothing mid-reload and for a blob that
    /// carried none.
    #[must_use]
    pub fn reloads(&self) -> &[CarriedReload] {
        self.reloads.as_deref().unwrap_or_default()
    }

    /// Where a release before the envelope left its blob under `paths`:
    /// `$SHEP_HOME/run/handover.json`.
    #[cfg(test)]
    #[must_use]
    pub fn path(paths: &shep_core::paths::ShepPaths) -> std::path::PathBuf {
        paths.run.join(FILE_NAME)
    }

    /// Every descriptor number this blob names, listener and pidfile first.
    ///
    /// The exact set [`hand_over`](super::hand_over) carries. A descriptor
    /// carried without being named is one the successor never adopts.
    pub(super) fn named_fds(&self) -> impl Iterator<Item = RawFd> + '_ {
        [self.listener_fd, self.pidfile_fd].into_iter().chain(
            self.sheep
                .iter()
                .flat_map(|sheep| sheep.fds.all().into_iter().flatten()),
        )
    }

    /// Write the blob the way a release before the envelope did, at mode
    /// `0600` under `paths`, returning where it went.
    ///
    /// Only the tests write one: it is what a predecessor from that release
    /// leaves for `successor_handover` to find.
    ///
    /// # Errors
    ///
    /// The leftover blob could not be removed, the new one could not be
    /// created, or serializing to it failed.
    #[cfg(test)]
    pub fn write(&self, paths: &shep_core::paths::ShepPaths) -> io::Result<std::path::PathBuf> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt as _;

        let path = Self::path(paths);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        serde_json::to_writer(&file, self).map_err(io::Error::other)?;
        Ok(path)
    }

    /// Read the blob at `path`.
    ///
    /// Does not unlink: the successor does that once it has adopted what the
    /// blob describes, so a failure here leaves the file for an operator.
    ///
    /// # Errors
    ///
    /// The file could not be read, its bytes are not a handover blob, or it
    /// names a format version this image does not implement.
    pub fn read(path: &Path) -> Result<Self, LoadError> {
        let text = fs::read_to_string(path).map_err(LoadError::Io)?;
        let value = serde_json::from_str(&text).map_err(LoadError::Malformed)?;
        Self::load_value(value)
    }

    /// Check `value`'s format version, then deserialize it.
    ///
    /// The version is read off the raw JSON first, so an unknown format is
    /// refused by number rather than by a field that failed to deserialize.
    ///
    /// # Errors
    ///
    /// `value` carries no `version`, carries one other than [`VERSION`], or
    /// is not a handover blob.
    pub fn load_value(value: serde_json::Value) -> Result<Self, LoadError> {
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(found) if found == u64::from(VERSION) => {}
            Some(found) => return Err(LoadError::UnsupportedVersion { found }),
            None => return Err(LoadError::MissingVersion),
        }
        serde_json::from_value(value).map_err(LoadError::Malformed)
    }
}

impl shep_handover::State for Handover {
    /// The blob's own [`VERSION`], so one number answers both gates.
    const FORMAT: u32 = VERSION;
}

/// Why a handover blob could not be loaded.
///
/// Every variant means the successor must not adopt anything: it has no
/// picture of the flock, or only part of one.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// The blob could not be read off disk.
    Io(io::Error),
    /// The blob names no format version at all, so it is not one this image
    /// wrote.
    MissingVersion,
    /// The blob names a format version this image does not implement.
    UnsupportedVersion {
        /// The version the blob claims.
        found: u64,
    },
    /// The blob names a version this image implements, but its contents do
    /// not deserialize into one.
    Malformed(serde_json::Error),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "the handover blob could not be read: {err}"),
            Self::MissingVersion => f.write_str("the handover blob names no format version"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "the handover blob is format version {found}, and this shep implements \
                 version {VERSION}"
            ),
            Self::Malformed(err) => write!(f, "the handover blob is not readable: {err}"),
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Malformed(err) => Some(err),
            Self::MissingVersion | Self::UnsupportedVersion { .. } => None,
        }
    }
}

/// The daemon's own two descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonFds {
    /// The control listener's descriptor number.
    pub listener: RawFd,
    /// The pidfile lock's descriptor number.
    pub pidfile: RawFd,
}

/// The three supervisor counters a successor must not reissue.
///
/// They reset to zero in every constructor, so a successor that did not
/// carry them would hand out an entry id, a reload-watchdog stamp or an
/// action stamp a caller still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    /// The next entry id.
    pub next_id: u32,
    /// The next reload-watchdog stamp.
    pub next_deadline: u64,
    /// The next action-wait stamp.
    pub next_action_stamp: u64,
}

#[cfg(test)]
mod tests;
