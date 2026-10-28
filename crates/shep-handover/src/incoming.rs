//! The incoming side: finding out this image is a successor, and adopting
//! what the predecessor left.

use core::fmt;

use crate::carried::Carried;
use crate::error::BootError;
use crate::state::State;

/// What a predecessor handed over: its state, and the descriptors it carried.
///
/// `Debug` prints the keys of the descriptors and leaves the state out: it
/// may carry a secret.
pub struct Arrival<S> {
    /// The state the predecessor passed.
    pub state: S,
    /// The descriptors it carried, to be claimed by key.
    pub fds: Carried,
}

impl<S> fmt::Debug for Arrival<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arrival")
            .field("state", &"..")
            .field("fds", &self.fds)
            .finish()
    }
}

/// Find out whether this image is a successor, and adopt what its
/// predecessor left. `Ok(None)` is an image started any other way.
///
/// Call this first thing in `main`, before opening a descriptor of its own.
/// A carried descriptor is named by its number. A number counts as inherited
/// only when nothing here has opened it. The blob is read, removed, and
/// carried descriptors become close-on-exec again. A predecessor probes its
/// target first, so this image may be that probe. `boot` then answers for `S`
/// and exits without returning.
///
/// # Errors
///
/// The predecessor is gone; see [`BootError`] for each way.
pub fn boot<S: State>() -> Result<Option<Arrival<S>>, BootError> {
    #[cfg(unix)]
    {
        if let Some(code) =
            crate::probe::answer::<S>(std::env::var_os(crate::probe::PROBE_ENV).as_deref())
        {
            // A predecessor is asking whether this build reads its state,
            // before it commits to the exec: answer and leave.
            std::process::exit(code);
        }
        boot_from(std::env::var_os(crate::HANDOVER_ENV), std::process::id())
    }
    #[cfg(not(unix))]
    {
        Err(BootError::Unsupported)
    }
}

/// [`boot`], against a given marker and pid, so a test can drive every
/// refusal without writing the environment, which is process-global.
#[cfg(unix)]
fn boot_from<S: State>(
    marker: Option<std::ffi::OsString>,
    own_pid: u32,
) -> Result<Option<Arrival<S>>, BootError> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::path::PathBuf;

    let Some(marker) = marker else {
        return Ok(None);
    };
    let bad = || BootError::Marker {
        value: marker.to_string_lossy().into_owned(),
    };
    let bytes = marker.as_bytes();
    let colon = bytes.iter().position(|b| *b == b':').ok_or_else(bad)?;
    let pid: u32 = std::str::from_utf8(&bytes[..colon])
        .ok()
        .and_then(|pid| pid.parse().ok())
        .ok_or_else(bad)?;
    if pid != own_pid {
        return Ok(None);
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes[colon + 1..].to_vec()));

    let read = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    let bytes = read.map_err(BootError::Blob)?;

    // Whatever goes wrong from here, the predecessor is gone and the
    // descriptors it carried belong to nobody: close every one the blob names,
    // not only those adopted before the failure.
    adopt::<S>(&bytes).map(Some).inspect_err(|_| {
        crate::envelope::carried_numbers(&bytes)
            .into_iter()
            .for_each(crate::sys::release_inherited)
    })
}

/// Decode `bytes` and adopt the descriptors it names, all or nothing: an error
/// drops every descriptor adopted so far.
#[cfg(unix)]
fn adopt<S: State>(bytes: &[u8]) -> Result<Arrival<S>, BootError> {
    use crate::envelope::{EnvelopeError, Unreadable};

    let inbound = crate::envelope::decode::<S>(bytes).map_err(|err| match err {
        EnvelopeError::Malformed(err) => BootError::Malformed(err),
        EnvelopeError::Version(found) => BootError::Envelope { found },
    })?;

    // Adopted before the format is judged, so a refusal closes them.
    let mut fds = std::collections::BTreeMap::new();
    for (key, number) in inbound.fds {
        let fd = crate::sys::adopt_inherited(number).map_err(|source| BootError::Descriptor {
            key: key.clone(),
            number,
            source,
        })?;
        fds.insert(key, fd);
    }
    let state = inbound.state.map_err(|err| match err {
        Unreadable::Format { found, supported } => BootError::Format { found, supported },
        Unreadable::Malformed(err) => BootError::State(err),
    })?;
    Ok(Arrival {
        state,
        fds: Carried::new(fds),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;
    use std::io::{Read as _, Write as _};
    use std::os::fd::IntoRawFd as _;

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Note {
        text: String,
    }

    impl State for Note {
        const FORMAT: u32 = 1;
    }

    /// A descriptor with close-on-exec clear, standing in for an inherited one.
    fn inherited(read: bool) -> (i32, std::io::PipeWriter, std::io::PipeReader) {
        let (r, w) = std::io::pipe().unwrap();
        let (carried, other_w, other_r) = if read {
            (r.into_raw_fd(), w, std::io::pipe().unwrap().0)
        } else {
            (w.into_raw_fd(), std::io::pipe().unwrap().1, r)
        };
        crate::sys::keep_across_exec(carried).unwrap();
        (carried, other_w, other_r)
    }

    fn marker(pid: u32, path: &std::path::Path) -> Option<OsString> {
        Some(OsString::from(format!("{pid}:{}", path.display())))
    }

    #[test]
    fn an_image_with_no_marker_is_not_a_successor() {
        assert!(boot_from::<Note>(None, 1).unwrap().is_none());
    }

    #[test]
    fn a_marker_naming_another_pid_is_a_childs_inheritance_not_a_handover() {
        let arrival = boot_from::<Note>(Some(OsString::from("41:/nowhere")), 42).unwrap();
        assert!(arrival.is_none());
    }

    #[test]
    fn a_marker_that_is_not_a_pid_and_a_path_is_refused() {
        for value in ["nothing", "x:/a/path", ":/a/path"] {
            let err = boot_from::<Note>(Some(OsString::from(value)), 1).unwrap_err();
            assert!(matches!(err, BootError::Marker { .. }), "{value}: {err}");
        }
    }

    #[test]
    fn the_state_and_a_carried_pipe_arrive_and_the_blob_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob.json");
        let (fd, mut writer, _) = inherited(true);
        std::fs::write(
            &blob,
            format!(r#"{{"envelope":1,"format":1,"fds":{{"out":{fd}}},"state":{{"text":"hi"}}}}"#),
        )
        .unwrap();
        writer.write_all(b"from the child").unwrap();
        drop(writer);

        let mut arrival = boot_from::<Note>(marker(7, &blob), 7).unwrap().unwrap();

        assert_eq!(
            arrival.state,
            Note {
                text: "hi".to_owned()
            }
        );
        assert!(!blob.exists(), "the successor unlinks what it read");
        assert_eq!(arrival.fds.keys().collect::<Vec<_>>(), ["out"]);
        let mut text = String::new();
        std::fs::File::from(arrival.fds.take("out").unwrap().into_owned_fd())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "from the child");
        assert!(arrival.fds.take("out").is_none(), "a key is claimed once");
        assert!(arrival.fds.is_empty());
    }

    #[test]
    fn a_format_this_build_cannot_read_is_refused_with_both_numbers_and_closes_its_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob.json");
        let (fd, _writer, _) = inherited(true);
        std::fs::write(
            &blob,
            format!(r#"{{"envelope":1,"format":9,"fds":{{"out":{fd}}},"state":{{"other":1}}}}"#),
        )
        .unwrap();

        let err = boot_from::<Note>(marker(7, &blob), 7).unwrap_err();

        assert_eq!(
            err.to_string(),
            "the handover state is format 9, and this build reads format 1"
        );
        assert!(!blob.exists());
        assert!(
            crate::sys::adopt_inherited(fd).is_err(),
            "the refused descriptor was closed, so its number names nothing"
        );
    }

    #[test]
    fn a_blob_that_is_gone_is_a_blob_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = boot_from::<Note>(marker(7, &dir.path().join("gone")), 7).unwrap_err();
        assert!(matches!(err, BootError::Blob(_)), "{err}");
    }

    #[test]
    fn a_blob_naming_a_descriptor_this_image_opened_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob.json");
        let (mine, _w) = std::io::pipe().unwrap();
        let number = std::os::fd::AsRawFd::as_raw_fd(&mine);
        std::fs::write(
            &blob,
            format!(
                r#"{{"envelope":1,"format":1,"fds":{{"out":{number}}},"state":{{"text":"x"}}}}"#
            ),
        )
        .unwrap();
        let err = boot_from::<Note>(marker(7, &blob), 7).unwrap_err();
        assert!(
            matches!(&err, BootError::Descriptor { key, .. } if key == "out"),
            "{err}"
        );
    }

    fn is_open(fd: i32) -> bool {
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).is_ok()
    }

    #[test]
    fn a_refusal_partway_through_the_descriptors_closes_the_ones_it_never_reached() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob.json");
        let (later, _writer, _) = inherited(true);
        // "a" sorts first and is a standard stream, so adopting it fails before
        // "b" is looked at.
        std::fs::write(
            &blob,
            format!(
                r#"{{"envelope":1,"format":1,"fds":{{"a":1,"b":{later}}},"state":{{"text":"x"}}}}"#
            ),
        )
        .unwrap();

        let err = boot_from::<Note>(marker(7, &blob), 7).unwrap_err();

        assert!(
            matches!(&err, BootError::Descriptor { key, .. } if key == "a"),
            "{err}"
        );
        assert!(
            !is_open(later),
            "the descriptor it never reached was closed"
        );
    }

    #[test]
    fn an_envelope_it_cannot_decode_still_closes_what_the_blob_names() {
        for (name, body) in [
            ("unknown layout", r#"{"envelope":9,"fds":{"out":FD}}"#),
            (
                "fds of the wrong shape",
                r#"{"envelope":1,"fds":{"out":FD,"bad":"x"}}"#,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let blob = dir.path().join("blob.json");
            let (fd, _writer, _) = inherited(true);
            std::fs::write(&blob, body.replace("FD", &fd.to_string())).unwrap();

            let err = boot_from::<Note>(marker(7, &blob), 7).unwrap_err();

            assert!(
                matches!(err, BootError::Envelope { .. } | BootError::Malformed(_)),
                "{name}: {err}"
            );
            assert!(!is_open(fd), "{name}: the carried descriptor was closed");
        }
    }

    #[test]
    fn a_state_that_does_not_fit_closes_the_descriptors_too() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob.json");
        let (fd, _writer, _) = inherited(true);
        std::fs::write(
            &blob,
            format!(r#"{{"envelope":1,"format":1,"fds":{{"out":{fd}}},"state":{{"text":5}}}}"#),
        )
        .unwrap();

        let err = boot_from::<Note>(marker(7, &blob), 7).unwrap_err();

        assert!(matches!(err, BootError::State(_)), "{err}");
        assert!(!is_open(fd));
    }

    #[test]
    fn debug_shows_the_keys_and_never_the_state() {
        let arrival = Arrival {
            state: Note {
                text: "hunter2".to_owned(),
            },
            fds: Carried::default(),
        };
        assert_eq!(
            format!("{arrival:?}"),
            r#"Arrival { state: "..", fds: [] }"#
        );
    }
}
