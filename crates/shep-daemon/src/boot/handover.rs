//! Both ends of a handover: becoming a successor, and handing over to one
//!
//! Incoming, [`successor_handover`] reads the blob `SHEP_HANDOVER` names and
//! [`rehydrate`] adopts the descriptors it lists, so a successor inherits the
//! home's lock, the bound listener and every sheep instead of rebinding
//! anything. Outgoing, the SIGHUP task calls [`hand_over_now`], which runs
//! both gates while this image still exists to fall back to.
//!
//! Unix only, gated once at the module rather than on every item: Windows has
//! no `execve`, so no image there can be a successor.

use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;
use shep_core::transport::Listener;

use super::BootError;
use super::pidfile::PidfileLock;
use crate::supervisor::SupervisorHandle;

/// The apps a successor records in its own registry, which is what the muster
/// roll on disk is written from.
///
/// Every carried sheep's, and no dog's: the roll outlives the daemon, so a dog
/// in it would come back on a later cold boot as an unmarked sheep, ahead of
/// `spawn_enabled_dogs`, and `shep disable metrics` could not take it out.
/// Filtered here because `record_config` takes bare
/// [`AppConfig`](shep_core::config::AppConfig)s with no marker to filter on.
pub(super) fn apps_for_the_roll(
    flock: &[crate::handover::adopt::AdoptedSheep],
) -> Vec<shep_core::config::AppConfig> {
    flock
        .iter()
        .filter(|sheep| sheep.carried.dog().is_none())
        .map(|sheep| sheep.carried.app().clone())
        .collect()
}

/// The handover blob this process was handed, if it is a successor.
///
/// A successor is a shep image an outgoing daemon `execve`d in its own place.
/// Its only marker is `SHEP_HANDOVER`, naming the blob to adopt.
///
/// An unusable blob logs at `error` and boots as if fresh: the predecessor has
/// already replaced itself, so refusing leaves the operator no shepherd. A
/// genuinely lost blob is self-limiting, since a real successor also inherited
/// the locked pidfile descriptor and the fresh boot stops at
/// [`BootError::AlreadyRunning`] before restoring anything.
///
/// Unix only: Windows has no `execve`, so no image can be a successor.
#[must_use]
pub(crate) fn successor_handover() -> Option<Successor> {
    let path = PathBuf::from(std::env::var_os(crate::handover::HANDOVER_ENV)?);
    let blob = successor_handover_at(&path)?;
    Some(Successor { path, blob })
}

/// Rebuild everything a successor was handed: the lock, the listener, and
/// every sheep's plumbing.
///
/// The blob is removed once its descriptors are adopted, and only then: one
/// left after a refusal is evidence, one left after a success would be adopted
/// again by the next boot. No partial success, since the predecessor has
/// already `execve`d itself away.
///
/// # Errors
///
/// - [`BootError::Adopt`] if a descriptor the blob names is not open in this
///   process, or is not the kind of object it was named as.
pub(super) fn rehydrate(carried: Successor, paths: &ShepPaths) -> Result<Rehydrated, BootError> {
    let Successor { path, blob } = carried;
    let counters = blob.counters();
    let adopted = crate::handover::adopt::adopt(&blob)
        .map_err(|source| BootError::Adopt(Box::new(source)))?;
    crate::handover::adopt::discard_blob(&path);
    let _ = paths;
    let reloads = blob.reloads().to_vec();
    Ok((
        PidfileLock::from_locked(adopted.pidfile),
        Listener::from_unix_listener(adopted.listener),
        (adopted.sheep, counters, reloads),
    ))
}

/// A handover blob, and where it was read from.
///
/// The path is kept so [`rehydrate`] can unlink the blob after adopting it.
#[derive(Debug)]
pub(crate) struct Successor {
    /// Where the blob was read from.
    pub path: PathBuf,
    /// What it said.
    pub blob: crate::handover::Handover,
}

/// [`successor_handover`], against a caller-named path.
///
/// Split out so a test can drive every refusal without writing the
/// environment, which is process-global and `unsafe` in edition 2024.
fn successor_handover_at(path: &Path) -> Option<crate::handover::Handover> {
    match crate::handover::Handover::read(path) {
        Ok(blob) => Some(blob),
        Err(error) => {
            tracing::error!(
                path = %path.display(),
                %error,
                "this process was handed a handover blob it cannot use, and is booting as if \
                 it were fresh; if a flock was running, it is no longer supervised"
            );
            None
        }
    }
}

/// What [`rehydrate`] rebuilds from a blob: the home's lock, the control
/// listener, and the flock to install with the counters and the in-flight
/// reloads it ran under.
type Rehydrated = (
    PidfileLock,
    Listener,
    (
        Vec<crate::handover::adopt::AdoptedSheep>,
        crate::handover::Counters,
        Vec<crate::supervisor::CarriedReload>,
    ),
);

/// Everything the SIGHUP task needs to replace this daemon's image.
///
/// Handed over a channel rather than as arguments, because none of it exists
/// when [`install_signals`](super::install_signals) runs. `Debug` carries nothing sensitive: two
/// descriptor numbers, a mailbox and the home's paths. The blob they end up in
/// does carry each sheep's environment; see [`crate::handover::Handover`].
#[derive(Debug)]
pub(crate) struct HandoverSeam {
    /// The flock to carry.
    pub(super) supervisor: SupervisorHandle,
    /// The daemon's own two descriptors, which the actor never sees.
    pub(super) fds: crate::handover::DaemonFds,
    /// The home, for the blob's path.
    pub(super) paths: ShepPaths,
}

/// Replace this process with a successor holding `seam`'s flock.
///
/// The gate runs here as well as in the client that asked before signalling:
/// anyone can send a signal, and the flock can change between the question and
/// the signal.
///
/// # Errors
///
/// The sentence to log, when the flock cannot be carried, when the actor is
/// gone, or when the exec failed. Each leaves this process still itself with
/// no blob on disk, and the caller falls back to a graceful stop.
pub(super) async fn hand_over_now(
    seam: &HandoverSeam,
) -> Result<core::convert::Infallible, String> {
    let (candidates, blob, parked) = seam
        .supervisor
        .handover_snapshot(seam.fds)
        .await
        .map_err(|err| format!("the supervisor could not describe its flock: {err}"))?;
    let refusal = match hand_over_carrying(&candidates, &blob, seam) {
        Ok(never) => match never {},
        Err(refusal) => refusal,
    };
    // Taking the snapshot stopped every pump it reached and nothing else ever
    // sends a resume, so without this the flock logs nothing through the
    // graceful stop the caller falls back to. Here rather than in
    // `exec_into`'s error path: this is where every way out meets.
    parked.resume().await;
    Err(refusal)
}

/// [`hand_over_now`]'s body, split out so a single resume can cover every
/// way it refuses.
///
/// Two gates, in order: whether this flock is a shape a handover carries, then
/// whether the blob describing it is one a successor could adopt, run against
/// duplicates while this image still exists to fall back to.
///
/// # Errors
///
/// The flock cannot be carried, a successor could not have adopted the blob,
/// or the exec failed. All three leave this process still itself, with no blob
/// on disk.
fn hand_over_carrying(
    candidates: &[crate::handover::OwnedCandidate],
    blob: &crate::handover::Handover,
    seam: &HandoverSeam,
) -> Result<core::convert::Infallible, String> {
    let borrowed: Vec<crate::handover::Candidate<'_>> = candidates
        .iter()
        .map(crate::handover::OwnedCandidate::as_candidate)
        .collect();
    if let crate::handover::Fitness::Refused(reason) = crate::handover::fitness(&borrowed) {
        return Err(reason.to_string());
    }
    // The gate with no way back if it is skipped: past the `execve` there is
    // no image to refuse to, and the flock runs on unsupervised. Not inside
    // `handover::hand_over`, because the rehearsal registers objects with the
    // tokio reactor and that fn's own self-test runs from a plain `#[test]`.
    crate::handover::adopt::dry_run(blob).map_err(|err| {
        format!(
            "a successor could not have adopted this flock, so none was started: {err}. This is \
             a shep bug worth reporting: the descriptors are ones this shepherd opened itself, \
             and the check that refused them is the successor's own. The flock is stopped and \
             started instead, which is the reload an operator had before handovers existed"
        )
    })?;
    crate::handover::hand_over(blob, &seam.paths).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::{BootOptions, SIGNAL_TEST_LOCK, boot, init_dirs};
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::testing::{SharedRunner, capture_logs, test_paths};
    use shep_core::config::{AppConfig, normalize};
    use shep_core::protocol::DogSource;
    use std::sync::Arc;
    use std::time::Duration;

    /// The daemon-side gate: anyone can send a signal, and the flock can
    /// change between a client's question and the delivery, so the SIGHUP path
    /// runs [`crate::handover::fitness`] again and refuses on its own.
    ///
    /// The descriptors are invalid on purpose: if the gate stopped refusing,
    /// `hand_over` would meet `EBADF` and return rather than exec this test
    /// binary into a re-run of the suite. The assertion on the message is what
    /// tells the two failures apart.
    #[tokio::test(start_paused = true)]
    async fn a_sighup_over_a_flock_it_cannot_carry_refuses_before_it_execs() {
        // A successful `boot()` installs signal listeners for this test's
        // whole duration, and a `raise()` elsewhere reaches them, hence the
        // lock. The paused clock is separate: it keeps the refusal this needs,
        // a pump missing `REPORT_DEADLINE`, from costing two real seconds.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // A wedged log pump: the one thing the gate still refuses. The case is
        // about the gate firing at all, not about what fires it.
        let daemon = boot(
            ScriptedRunner::new(vec![ProcScript::never_exits()])
                .with_a_pump_that_never_reports(&["wedged"]),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        ctx.supervisor
            .start(vec![
                normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
            ])
            .await
            .unwrap();

        let seam = HandoverSeam {
            supervisor: ctx.supervisor.clone(),
            fds: crate::handover::DaemonFds {
                listener: -1,
                pidfile: -1,
            },
            paths: paths.clone(),
        };
        let refusal = hand_over_now(&seam)
            .await
            .expect_err("a flock with a wedged log pump cannot be carried");
        assert!(
            refusal.contains("did not report its descriptors in time"),
            "the gate must refuse before anything is exec'd: {refusal}"
        );
        assert!(
            refusal.contains("wedged"),
            "the refusal must name the sheep that held the flock back: {refusal}"
        );

        ctx.shutdown();
        // Bounded: the lock is held across this await, so a hung teardown
        // would stop every other signal test rather than failing this one.
        tokio::time::timeout(Duration::from_secs(5), daemon.run())
            .await
            .unwrap()
            .unwrap();
    }

    /// The second gate: a flock the fitness check passes, described by a blob
    /// no successor could have adopted, refuses here rather than after the
    /// `execve`. Past the exec there is no predecessor to refuse to, so
    /// `rehydrate` returns [`BootError::Adopt`], the successor exits without
    /// serving, and the flock runs on unsupervised.
    ///
    /// The assertion is on which refusal, not on there being one: the
    /// `FD_CLOEXEC` sweep meeting `EBADF` refused these descriptors before the
    /// gate existed. They stay invalid on purpose, since a blob past both
    /// gates would exec this test binary into a re-run of the suite.
    #[tokio::test]
    async fn a_sighup_over_a_blob_no_successor_could_adopt_refuses_before_it_execs() {
        // A successful `boot()` installs real signal listeners for this
        // test's whole duration.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // No dog and no sheep, so the first gate passes: an empty flock is
        // carryable, and this case is about the second gate.
        let daemon = boot(
            ScriptedRunner::new(Vec::new()),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let ctx = daemon.context();

        let seam = HandoverSeam {
            supervisor: ctx.supervisor.clone(),
            fds: crate::handover::DaemonFds {
                listener: -1,
                pidfile: -2,
            },
            paths: paths.clone(),
        };
        let refusal = hand_over_now(&seam)
            .await
            .expect_err("a blob naming no real listener cannot be adopted");
        assert!(
            refusal.contains("a successor could not have adopted this flock"),
            "the rehearsal must be what refuses, not the `FD_CLOEXEC` sweep further on: {refusal}"
        );
        assert!(
            refusal.contains("-1"),
            "the refusal must name the descriptor it refused: {refusal}"
        );
        assert!(
            !crate::handover::Handover::path(&paths).exists(),
            "a refusal before the exec must leave no blob on disk"
        );

        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), daemon.run())
            .await
            .unwrap()
            .unwrap();
    }

    /// A handover that reports and then refuses has to leave every pump
    /// reading again.
    ///
    /// Taking the snapshot stops each pump where it stands, and nothing else
    /// in the daemon ever sends a resume, so a missing one is a flock that
    /// logs nothing more for the rest of the daemon's life.
    ///
    /// Two sheep, since the resume has to reach every pump that was reported
    /// to rather than the one the refusal named.
    #[tokio::test(start_paused = true)]
    async fn an_abandoned_handover_starts_every_pump_reading_again() {
        // A successful `boot()` installs real signal listeners for this test's
        // whole duration, and the paused clock keeps the missed
        // `REPORT_DEADLINE` this refusal needs from costing real seconds.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let runner = Arc::new(
            ScriptedRunner::new(vec![ProcScript::never_exits(); 3])
                .with_a_pump_that_never_reports(&["wedged"]),
        );
        // The refusal: a wedged log pump, read after every pump that answered
        // has been reported to and parked, which is what makes a resume owed.
        let daemon = boot(
            SharedRunner(Arc::clone(&runner)),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        ctx.supervisor
            .start(vec![
                normalize(AppConfig::minimal("quiet", "./srv")).unwrap(),
                normalize(AppConfig::minimal("chatty", "./srv")).unwrap(),
                normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
            ])
            .await
            .unwrap();
        for sheep in 0..3 {
            assert!(
                runner.log_ctl_live(sheep),
                "sheep {sheep} must have a live log pump before the report, or this case                  proves nothing"
            );
        }

        let seam = HandoverSeam {
            supervisor: ctx.supervisor.clone(),
            fds: crate::handover::DaemonFds {
                listener: -1,
                pidfile: -1,
            },
            paths: paths.clone(),
        };
        let refusal = hand_over_now(&seam)
            .await
            .expect_err("a flock with a wedged log pump cannot be carried");
        assert!(
            refusal.contains("did not report its descriptors in time"),
            "the gate must refuse before anything is exec'd: {refusal}"
        );

        let answered: Vec<usize> = ["quiet", "chatty"]
            .iter()
            .map(|name| runner.spawn_index_of(name).expect("started above"))
            .collect();
        let wedged = runner.spawn_index_of("wedged").expect("started above");
        // Polled: `LogCtl::Resume` carries no acknowledgement, so a send that
        // returned was queued rather than served. Bounded, so a pump that is
        // never told fails here instead of hanging.
        let all_resumed = async {
            while answered.iter().any(|sheep| runner.resumes(*sheep) == 0) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), all_resumed)
            .await
            .expect("every pump a refused handover reported to must be reading again");
        for sheep in &answered {
            assert_eq!(
                runner.resumes(*sheep),
                1,
                "sheep {sheep} must be resumed once rather than repeatedly"
            );
        }
        // A resume that reached only the first sheep reported to would satisfy
        // the single-pump sibling and fail here.
        assert_eq!(answered.len(), 2, "two pumps must have answered the report");
        assert_eq!(
            runner.resumes(wedged),
            0,
            "a pump that never answered never parked, so nothing may resume it"
        );

        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), daemon.run())
            .await
            .unwrap()
            .unwrap();
    }

    /// A snapshot abandoned because a pump went quiet still owes a resume to
    /// every pump that answered. A missed deadline is a different way into the
    /// refusal than the sibling's config gate, and it arrives with one pump
    /// parked and one that never was.
    ///
    /// No `boot()` and no signal: this is about what `hand_over_now` does with
    /// a snapshot, and building the supervisor directly is what lets the clock
    /// be paused so the deadline costs nothing to wait out.
    #[tokio::test(start_paused = true)]
    async fn a_handover_abandoned_on_a_wedged_pump_resumes_the_pumps_that_parked() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let (events, _rx) = crate::bus::test_bus(64);
        let runner = Arc::new(
            ScriptedRunner::new(vec![ProcScript::never_exits(); 2])
                .with_a_pump_that_never_reports(&["wedged"]),
        );
        let supervisor = crate::supervisor::spawn_supervisor(
            SharedRunner(Arc::clone(&runner)),
            paths.clone(),
            events,
        );
        supervisor
            .start(vec![
                normalize(AppConfig::minimal("answering", "./srv")).unwrap(),
                normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
            ])
            .await
            .unwrap();

        // Invalid on purpose: if the gate stopped refusing, `hand_over` would
        // meet `EBADF` and return rather than exec this test binary into a
        // re-run of the suite.
        let seam = HandoverSeam {
            supervisor: supervisor.clone(),
            fds: crate::handover::DaemonFds {
                listener: -1,
                pidfile: -1,
            },
            paths: paths.clone(),
        };
        let refusal = hand_over_now(&seam)
            .await
            .expect_err("a flock with a pump that never reported cannot be carried");
        assert!(
            refusal.contains("did not report its descriptors in time"),
            "the refusal must say the pump went quiet, not something else: {refusal}"
        );
        assert!(
            refusal.contains("wedged"),
            "the refusal must name the sheep whose pump went quiet: {refusal}"
        );

        let answering = runner.spawn_index_of("answering").expect("started above");
        let wedged = runner.spawn_index_of("wedged").expect("started above");
        // Polled: `LogCtl::Resume` carries no acknowledgement, so a send that
        // returned was queued rather than served. Bounded, so a pump that is
        // never told fails here instead of hanging.
        let delivered = async {
            while runner.resumes(answering) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), delivered)
            .await
            .expect("the pump that answered was parked, and a refusal owes it a resume");
        assert_eq!(
            runner.resumes(wedged),
            0,
            "a pump that never answered never parked, so nothing may resume it"
        );
    }

    /// One carried sheep, named, with `dog` set or not.
    fn carried_for_the_roll(
        name: &str,
        dog: Option<DogSource>,
    ) -> crate::handover::adopt::AdoptedSheep {
        let mut entry = crate::entry::ProcessEntry {
            id: 1,
            spec: normalize(AppConfig::minimal(name, "./srv")).unwrap(),
            pending: None,
            pending_reidentifies: false,
            overridden: Vec::new(),
            instance: 0,
            status: shep_core::status::ProcStatus::Online,
            pid: Some(4242),
            restarts: 0,
            started_at: None,
            budget: crate::entry::RestartBudget::default(),
            reload: crate::entry::ReloadState::None,
            credentials: crate::privilege::SpawnIdentity::Resolved(None),
            out_file: PathBuf::new(),
            err_file: PathBuf::new(),
            dog: None,
            last_exit: None,
        };
        entry.dog = dog;
        crate::handover::adopt::AdoptedSheep {
            carried: crate::handover::CarriedSheep::from_entry(
                &entry,
                0,
                crate::handover::CarriedFds::none(),
                false,
                None,
                false,
                None,
            ),
            out_pipe: None,
            err_pipe: None,
            out_log: None,
            err_log: None,
            stdin_pipe: None,
            channel: None,
        }
    }

    /// A successor rebuilds its registry from the blob, and the roll on disk
    /// is written from that registry within seconds. `spawn_enabled_dogs`
    /// never touches `FlockRegistry`, so a successor that recorded a dog would
    /// put one in the roll permanently: a later cold boot restores `metrics`
    /// as an unmarked sheep ahead of `spawn_enabled_dogs`, and `shep disable
    /// metrics` cannot take it out.
    ///
    /// Both rows are asserted, since a filter that dropped everything would
    /// satisfy the negative half and overwrite a good roll with an empty one.
    #[test]
    fn a_carried_dog_does_not_reach_the_muster_roll() {
        let flock = vec![
            carried_for_the_roll("web", None),
            carried_for_the_roll("metrics", Some(DogSource::BuiltIn)),
            carried_for_the_roll(
                "log-rotate",
                Some(DogSource::Adopted {
                    path: "/opt/bin/shep-log-rotate".to_string(),
                }),
            ),
        ];

        let names: Vec<String> = apps_for_the_roll(&flock)
            .into_iter()
            .map(|app| app.name)
            .collect();

        assert_eq!(
            names,
            vec!["web".to_string()],
            "the roll is the operator's flock; a dog belongs to `shep.toml`"
        );
    }

    /// A blob written by hand rather than by `Handover::write`, so this
    /// module's tests pin the on-disk shape a successor has to read rather
    /// than round-tripping whatever the writer happens to emit.
    fn write_blob(path: &Path, version: u32) {
        std::fs::write(
            path,
            format!(
                r#"{{"version":{version},"sheep":[],"listener_fd":3,"pidfile_fd":4,"next_id":0,"next_deadline":0,"next_action_stamp":0}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_blob_on_disk_makes_this_process_a_successor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handover.json");
        write_blob(&path, 1);

        assert!(successor_handover_at(&path).is_some());
    }

    #[test]
    fn a_missing_blob_is_refused_out_loud_rather_than_silently() {
        // A stale inherited variable and a lost blob look the same from
        // here, and neither may pass for a fresh boot without a word.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-written.json");

        let logs = capture_logs(|| assert!(successor_handover_at(&path).is_none()));

        assert!(logs.contains("never-written.json"), "{logs}");
    }

    #[test]
    fn a_blob_of_an_unknown_version_is_refused_out_loud() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handover.json");
        write_blob(&path, u32::MAX);

        let logs = capture_logs(|| assert!(successor_handover_at(&path).is_none()));

        assert!(logs.contains("version"), "{logs}");
    }

    #[test]
    fn a_refused_blob_is_left_on_disk_for_an_operator_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handover.json");
        write_blob(&path, u32::MAX);

        capture_logs(|| assert!(successor_handover_at(&path).is_none()));

        assert!(path.exists(), "a refused blob is evidence, not litter");
    }
}
