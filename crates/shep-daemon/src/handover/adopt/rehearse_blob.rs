use super::super::{Handover, SheepFd, fds};
use super::adopt_descriptors::{
    adopt_channel, adopt_fd, adopt_listener, adopt_log, adopt_pipe, adopt_stdin,
};
use crate::sys;
use std::io;
use std::os::fd::RawFd;

/// Refuses a blob naming one descriptor number more than once.
///
/// Before the first adoption, never during. `sys::adopt_handover_fd`'s
/// safety argument rests on each adoption being its number's only owner: a
/// second owner closes it again on drop, reaching whatever this process
/// opened in between. A blob the daemon wrote cannot repeat a number; this
/// covers one that was edited, or left by a handover that never completed.
///
/// # Errors
///
/// Names the repeated number, which is all anything here can know.
pub(super) fn refuse_repeated_fds(blob: &Handover) -> io::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for fd in blob.named_fds() {
        if !seen.insert(fd) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the handover blob names descriptor {fd} more than once, so adopting it \
                     would build two owners of one number"
                ),
            ));
        }
    }
    Ok(())
}

/// Run everything [`adopt`](crate::handover::adopt::adopt) will run, in the predecessor, while there is
/// still an image to refuse back to. No check is re-stated: each number
/// reaches the successor's own adoption, on a duplicate, from a reparsed blob.
///
/// # Errors
///
/// The blob does not parse as a successor would, or names a number that is
/// repeated, reserved, closed, or the wrong kind for its slot.
///
/// # Panics
///
/// Panics if called outside a tokio runtime with IO enabled.
#[track_caller]
pub fn dry_run(blob: &Handover) -> io::Result<()> {
    let value = serde_json::to_value(blob).map_err(io::Error::other)?;
    let blob = Handover::load_value(value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a successor could not have read this blob back: {error}"),
        )
    })?;

    // `adopt`'s own order, so a rehearsal that stops early stops where the
    // successor would.
    refuse_repeated_fds(&blob)?;
    rehearse(blob.listener_fd, "the control listener", adopt_listener)?;
    for carried in &blob.sheep {
        let name = &carried.name;
        for (fd, slot) in carried.fds.all_kinded() {
            let Some(fd) = fd else { continue };
            rehearse(
                fd,
                &format!("sheep '{name}' {}", slot.describe()),
                |dup| match slot {
                    SheepFd::OutPipe => adopt_pipe(Some(dup), name, "stdout").map(drop),
                    SheepFd::ErrPipe => adopt_pipe(Some(dup), name, "stderr").map(drop),
                    SheepFd::OutLog => adopt_log(Some(dup), name, "stdout").map(drop),
                    SheepFd::ErrLog => adopt_log(Some(dup), name, "stderr").map(drop),
                    SheepFd::Stdin => adopt_stdin(Some(dup), name).map(drop),
                    SheepFd::Channel => adopt_channel(Some(dup), name).map(drop),
                },
            )?;
        }
    }
    rehearse(blob.pidfile_fd, "the pidfile lock", |dup| {
        adopt_fd(dup, "the pidfile lock").map(drop)
    })?;
    Ok(())
}

/// Hand `adopt_one` a duplicate of `fd`, so an adoption that takes ownership
/// can be run against a descriptor this process must keep.
///
/// # Errors
///
/// `fd` is reserved or not open, it could not be duplicated, or the adoption
/// refused the duplicate.
pub(super) fn rehearse<T>(
    fd: RawFd,
    what: &str,
    adopt_one: impl FnOnce(RawFd) -> io::Result<T>,
) -> io::Result<()> {
    // The blob's number, never the duplicate's: a duplicate is always open
    // and above the floor, so a rehearsal that only saw duplicates would wave
    // through exactly the two blobs the successor is certain to refuse.
    // Labelled, because `sys::adoptable_fd` names only the number.
    sys::adoptable_fd(fd)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{what}: {error}")))?;
    let duplicate = fds::duplicate_raw(fd)?;
    // `adopt_one` owns `duplicate` on both arms, so nothing leaks: the one
    // arm that returns without consuming is `sys::adopt_handover_fd`
    // refusing, which the check above rules out for a number just created
    // above the floor.
    adopt_one(duplicate).map(drop)
}

/// Remove the blob at `path`, now that its descriptors are adopted.
///
/// Called only after [`adopt`](crate::handover::adopt::adopt) has succeeded: a blob left after a refusal is
/// evidence an operator can read, while one left after a success would be
/// adopted again by the next boot. A failure to remove it is logged rather
/// than returned, the flock being rehydrated already.
pub fn discard_blob(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "the handover blob could not be removed after it was adopted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::adopt_descriptors::adopt;

    use std::os::fd::RawFd;

    use super::super::super::{CarriedFds, SheepFd, fds};

    use std::os::fd::IntoRawFd as _;

    use crate::handover::VERSION;
    use tokio::io::AsyncWriteExt as _;

    use super::super::testing::*;
    use super::*;

    /// `merge_logs` points every instance at one path, and
    /// [`refuse_repeated_fds`] refuses a blob naming any number twice. Each
    /// instance's pump runs its own `open_append`, so one inode is reached
    /// through two descriptions with two numbers; the interleaved file proves
    /// they were independent rather than merely distinct.
    #[tokio::test]
    async fn two_instances_sharing_one_log_file_are_both_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        // One path for both slots: `assemble` drops the `-<instance>`
        // suffix under `merge_logs`.
        let merged = dir.path().join("web-out.log");
        let zero = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&merged)
            .unwrap();
        let one = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&merged)
            .unwrap();
        let (zero_fd, one_fd) = (zero.into_raw_fd(), one.into_raw_fd());
        assert_ne!(
            zero_fd, one_fd,
            "two `open`s on one path must yield two numbers, or the premise \
                 of this whole case is wrong"
        );
        let blob = blob_with(
            &socket,
            vec![
                carried_slot(
                    0,
                    CarriedFds {
                        out_pipe: None,
                        err_pipe: None,
                        out_log: Some(zero_fd),
                        err_log: None,
                        stdin: None,
                        channel: None,
                    },
                ),
                carried_slot(
                    1,
                    CarriedFds {
                        out_pipe: None,
                        err_pipe: None,
                        out_log: Some(one_fd),
                        err_log: None,
                        stdin: None,
                        channel: None,
                    },
                ),
            ],
        );

        let mut adopted = adopt(&blob).expect("a merged-log clustered app must be adoptable");

        assert_eq!(adopted.sheep.len(), 2, "one adopted sheep per instance");
        let zero = adopted.sheep[0].out_log.take().expect("slot 0's log");
        let one = adopted.sheep[1].out_log.take().expect("slot 1's log");
        // Alternated, so a second handle that had become an alias of the
        // first shows up as lost text rather than two clean halves. Flushed
        // per line because a `tokio::fs::File` hands the real `write(2)` to
        // the blocking pool, which finishes in its own order.
        let mut handles = [zero, one];
        for (line, slot) in [
            ("zero-1\n", 0),
            ("one-1\n", 1),
            ("zero-2\n", 0),
            ("one-2\n", 1),
        ] {
            let handle = &mut handles[slot];
            handle.write_all(line.as_bytes()).await.unwrap();
            handle.flush().await.unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&merged).unwrap(),
            "zero-1\none-1\nzero-2\none-2\n",
            "both instances append into the one file, in the order written"
        );
    }

    /// Without this the suite would pass with a `dry_run` that refused
    /// everything, turning every handover into a stop-and-start.
    #[tokio::test]
    async fn a_blob_a_successor_could_adopt_passes_the_rehearsal() {
        let predecessor = Predecessor::new();

        dry_run(&predecessor.blob()).expect("every descriptor here is the kind its slot wants");
    }

    /// Nothing about a carried reload is a descriptor, so the rehearsal
    /// covers a swap in flight through the parse alone,
    /// [`Handover::load_value`](super::super::Handover::load_value) running before any descriptor is touched.
    #[tokio::test]
    async fn a_blob_carrying_a_swap_in_flight_passes_the_rehearsal() {
        use crate::entry::ReloadState;
        use crate::supervisor::{CarriedReload, ReloadMode, ReloadPhase, ReloadSwap};

        let predecessor = Predecessor::new();
        let mut blob = predecessor.blob();
        blob.sheep[0].reload = Some(ReloadState::Drainee { new_id: Some(9) });
        blob.reloads = Some(vec![CarriedReload {
            app: "web".to_owned(),
            queue: vec![4, 5],
            mode: ReloadMode::Overlap,
            swap: ReloadSwap {
                old_id: 1,
                new_id: Some(9),
                phase: ReloadPhase::DrainOld,
            },
        }]);

        dry_run(&blob).expect("a flock mid-reload is one a successor can adopt");
    }

    /// A field added to [`CarriedSheep`](super::super::CarriedSheep) rides the reparse the rehearsal
    /// already runs, and one it could not parse would be found after the
    /// predecessor was gone.
    #[tokio::test]
    async fn a_blob_carrying_a_failed_readiness_verdict_passes_the_rehearsal() {
        let predecessor = Predecessor::new();
        let mut blob = predecessor.blob();
        blob.sheep[0].ready_failed = Some(true);

        dry_run(&blob).expect("an instance a reload gave up on is one a successor can adopt");
    }

    /// The predecessor is still supervising this flock, so everything it
    /// holds has to work afterwards. Each of the four takes ownership a
    /// different way and would close the fixture's handle without the
    /// duplicate.
    ///
    /// The connection is queued before the rehearsal, since `O_NONBLOCK`
    /// reaches the original through the duplicate and this fixture's listener
    /// is a plain `std` one that really does change. Queueing first leaves
    /// the accept below something waiting either way.
    #[tokio::test]
    async fn a_rehearsal_leaves_every_descriptor_it_checked_working() {
        use std::io::{Read as _, Write as _};

        let mut predecessor = Predecessor::new();
        predecessor.out_write.write_all(b"a bleat").unwrap();
        let socket = predecessor.socket();
        let connecting = tokio::task::spawn_blocking(move || {
            std::os::unix::net::UnixStream::connect(&socket).unwrap()
        });
        let client = connecting.await.unwrap();

        dry_run(&predecessor.blob()).expect("the fixture is adoptable");

        // The listener still listens.
        predecessor
            .listener
            .accept()
            .expect("the checked listener still accepts");
        drop(client);

        // The stdout pipe still carries what was written before the check.
        let mut buf = [0_u8; 7];
        predecessor
            .out_read
            .read_exact(&mut buf)
            .expect("the checked read end still reads");
        assert_eq!(&buf, b"a bleat");

        // The stdin pipe still carries a line the other way.
        predecessor.stdin_write.write_all(b"whisper").unwrap();
        let mut buf = [0_u8; 7];
        predecessor
            .stdin_read
            .read_exact(&mut buf)
            .expect("the checked write end still writes");
        assert_eq!(&buf, b"whisper");

        // The shepherd channel still has its child on the far end.
        predecessor.channel.write_all(b"ping").unwrap();
        let mut buf = [0_u8; 4];
        predecessor
            .child_channel
            .read_exact(&mut buf)
            .expect("the checked channel still reaches the child");
        assert_eq!(&buf, b"ping");
    }

    /// A duplicate taken and never handed to an adoption leaks one
    /// descriptor per named number, on a path that runs on every reload.
    ///
    /// Counted over a hundred passes, because the count is the whole
    /// process's and other cases run in other threads. Six hundred leaked
    /// descriptors sit well clear of a few dozen of concurrent noise.
    #[tokio::test]
    async fn a_rehearsal_leaks_no_descriptors() {
        let predecessor = Predecessor::new();
        let blob = predecessor.blob();
        let before = open_fd_count();

        for _ in 0..100 {
            dry_run(&blob).expect("the fixture is adoptable");
        }

        let after = open_fd_count();
        assert!(
            after < before + 100,
            "a hundred rehearsals of a blob naming six descriptors must not grow this \
                 process's descriptor table: {before} -> {after}"
        );
    }

    /// A closed number is already refused before the exec, since clearing
    /// `FD_CLOEXEC` on it meets `EBADF`. An open one of the wrong kind sails
    /// through that and would reach a successor with no predecessor left.
    #[tokio::test]
    async fn a_descriptor_that_is_open_but_not_a_pipe_is_refused_before_the_exec() {
        use std::os::fd::AsRawFd as _;

        let predecessor = Predecessor::new();
        let not_a_pipe = std::fs::File::open("/dev/null").unwrap();
        let mut blob = predecessor.blob();
        blob.sheep[0].fds.out_pipe = Some(not_a_pipe.as_raw_fd());

        // The premise: this number is open, so nothing before the exec
        // would have stopped it.
        crate::sys::adoptable_fd(not_a_pipe.as_raw_fd())
            .expect("the number must be open, or this proves nothing");
        fds::keep_raw_across_exec(not_a_pipe.as_raw_fd())
            .expect("the `FD_CLOEXEC` sweep must not refuse it either");

        let err = dry_run(&blob).expect_err("/dev/null is not a readable pipe");

        assert!(
            err.to_string().contains("web") && err.to_string().contains("stdout"),
            "the refusal must name the sheep and the stream: {err}"
        );
    }

    /// The rehearsal and the adoption must agree, slot by slot.
    ///
    /// A rehearsal that passes a blob the successor refuses still reaches
    /// the `execve`. Every slot is walked because four mechanisms refuse
    /// them: a readable-pipe check, a writable-pipe check, a `getpeername`,
    /// and for the two log slots no kind check at all.
    #[tokio::test]
    async fn the_rehearsal_and_the_adoption_agree_on_every_slot() {
        use std::os::fd::AsRawFd as _;

        for slot in [
            SheepFd::OutPipe,
            SheepFd::ErrPipe,
            SheepFd::OutLog,
            SheepFd::ErrLog,
            SheepFd::Stdin,
            SheepFd::Channel,
        ] {
            let predecessor = Predecessor::new();
            let wrong = std::fs::File::open("/dev/null").unwrap();
            let wrong = Some(wrong.as_raw_fd());
            let mut blob = predecessor.blob();
            let fds = &mut blob.sheep[0].fds;
            match slot {
                SheepFd::OutPipe => fds.out_pipe = wrong,
                SheepFd::ErrPipe => fds.err_pipe = wrong,
                SheepFd::OutLog => fds.out_log = wrong,
                SheepFd::ErrLog => fds.err_log = wrong,
                SheepFd::Stdin => fds.stdin = wrong,
                SheepFd::Channel => fds.channel = wrong,
            }

            assert_eq!(
                dry_run(&blob).is_err(),
                adopt_a_copy(&blob).is_err(),
                "the rehearsal and the adoption disagree about {slot:?}, so one of them is \
                     checking something the other is not"
            );
        }
    }

    /// A repeated number is refused by the same function the successor
    /// refuses it with, rather than by a second copy of the rule.
    ///
    /// The sweep before the exec cannot catch it: clearing `FD_CLOEXEC`
    /// twice on one number succeeds, so a repeat reaches the successor
    /// untouched.
    #[tokio::test]
    async fn a_blob_naming_one_descriptor_twice_is_refused_before_the_exec() {
        let predecessor = Predecessor::new();
        let mut blob = predecessor.blob();
        blob.sheep[0].fds.err_log = Some(blob.pidfile_fd);

        let err = dry_run(&blob).expect_err("one number cannot have two owners");

        assert!(
            err.to_string().contains("more than once"),
            "the refusal must be `refuse_repeated_fds`'s own: {err}"
        );
    }

    /// A number below the stdio floor is refused here, not after the exec.
    ///
    /// The check runs against the blob's number, not the duplicate's: a
    /// duplicate is always open and above the floor, so a rehearsal that only
    /// inspected duplicates would wave through exactly the two blobs
    /// `sys::adopt_handover_fd` is certain to refuse.
    #[tokio::test]
    async fn a_reserved_or_closed_number_is_refused_before_the_exec() {
        let predecessor = Predecessor::new();

        let mut reserved = predecessor.blob();
        reserved.sheep[0].fds.out_log = Some(0);
        let err = dry_run(&reserved).expect_err("stdio is owned elsewhere");
        assert!(
            err.to_string().contains("reserved for stdio"),
            "the refusal must be the successor's own wording: {err}"
        );

        // `RawFd::MAX` rather than a number this case opened and closed: a
        // closed number can be handed straight back to another thread of this
        // same process. A number above any descriptor limit is `EBADF` with
        // nothing to race against.
        let mut gone = predecessor.blob();
        gone.sheep[0].fds.err_log = Some(RawFd::MAX);
        let err = dry_run(&gone).expect_err("a number this high names nothing");
        assert!(
            err.to_string().contains("not an open descriptor"),
            "the refusal must be the successor's own wording: {err}"
        );
    }

    /// The successor reads the blob back off disk rather than being handed
    /// this struct, so the parse is one of its checks too.
    ///
    /// A successor is a different build, so one that has moved `VERSION`
    /// refuses the blob at `load_value`, past the exec and with the
    /// predecessor gone.
    #[tokio::test]
    async fn a_blob_a_successor_could_not_read_back_is_refused_before_the_exec() {
        let predecessor = Predecessor::new();
        let mut blob = predecessor.blob();
        blob.version = VERSION + 1;

        let err = dry_run(&blob).expect_err("a version this image cannot read");

        assert!(
            err.to_string()
                .contains("could not have read this blob back"),
            "the refusal must say the parse failed, not the descriptors: {err}"
        );
    }
}
