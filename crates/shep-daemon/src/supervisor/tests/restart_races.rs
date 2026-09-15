//! Tests for a sheep that is already on its way out.
//!
//! A stopping sheep still receives reports from subsystems armed against it.
//! None of them may put it back.

use super::*;

// A liveness failure or memory breach reported against a reload's drainee
// must never claim its manual marker or send a second `Kill`: its kill
// ladder already owns its next exit.
#[test]
fn a_stopping_sheep_rejects_an_extra_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut ctl_rx) = actor_with_stopping_drainee(&dir, 4242, 0);

    actor.handle_extra_restart(0, 4242, None, None);

    let slot = actor.sheep.get(&0).expect("the sheep stays registered");
    assert_eq!(
        slot.entry.status,
        ProcStatus::Stopping,
        "a rejected extra restart must never touch status"
    );
    assert!(
        slot.manual.is_none(),
        "a Stopping sheep must never claim the manual marker off an extra restart"
    );
    assert!(
        ctl_rx.try_recv().is_err(),
        "a Stopping sheep must never receive a second Kill"
    );
}

/// A config-only re-arm changes neither the pid nor the status, so
/// `handle_extra_restart`'s first two guards pass and only the epoch stops
/// the replaced probe from restarting the sheep.
///
/// `slot.manual` is the observable: the actor is driven directly, so
/// `begin_manual` claiming the marker is the proof a restart got through.
#[tokio::test]
async fn a_stale_liveness_failure_from_a_replaced_probe_does_not_restart() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let mut app = AppConfig::minimal("web", "./srv");
    app.liveness_probe = Some(ProbeConfig {
        failure_threshold: 1,
        ..probe_config(ProbeKind::Tcp, "localhost:5432")
    });
    let app = normalize(app).unwrap();
    let pid = 1111;
    let entry = armed_entry(0, 0, pid, app, &paths);
    // A live `ctl`: `begin_manual_ids` reads `ctl.is_some()` to decide a
    // sheep is running, and without one this takes the "already stopped"
    // path instead of the guard under test.
    let (ctl_tx, mut ctl_rx) = mpsc::channel(1);
    let mut sheep = HashMap::new();
    sheep.insert(
        0,
        SheepSlot {
            ctl: Some(ctl_tx),
            ..SheepSlot::new(entry)
        },
    );
    let (tx, _mailbox) = mpsc::channel(16);
    let mut actor = test_actor(paths, Vec::new(), sheep, tx);
    let entry = actor
        .sheep
        .get(&0)
        .expect("the fixture registers id 0")
        .entry
        .clone();

    let supervisor = SupervisorHandle {
        tx: actor.tx.clone(),
    };
    let (breach_tx, _breaches) = mpsc::channel(1);
    let (live_tx, _liveness) = mpsc::channel(1);
    let extras = Extras {
        clock: Arc::new(SystemClock),
        enforcer: Arc::new(RecordingEnforcer::default()),
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports: ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        stats: idle_stats(),
    };
    // Never fails on its own: the epoch mismatch is driven directly.
    let prober: Arc<dyn Prober> = Arc::new(ScriptedProber::new(vec![]));

    // The epoch this arming gets is the stale one fed back in below.
    actor
        .registry
        .arm(&entry, Arc::clone(&prober), &extras, &supervisor);
    let stale_epoch = actor.registry.liveness_epoch(0);

    // Re-arm the same id: the pid and status stay, the epoch moves.
    actor.registry.arm(&entry, prober, &extras, &supervisor);
    assert_ne!(
        actor.registry.liveness_epoch(0),
        stale_epoch,
        "a re-arm must advance the epoch"
    );

    // A failure from the replaced probe, at the epoch it was armed under.
    actor.handle_extra_restart(0, pid, Some(stale_epoch), None);

    let slot = actor.sheep.get(&0).expect("the sheep stays registered");
    assert!(
        slot.manual.is_none(),
        "a stale liveness failure from a replaced probe must never claim the manual marker"
    );
    assert!(
        ctl_rx.try_recv().is_err(),
        "a stale liveness failure from a replaced probe must never queue a Kill"
    );
    assert_eq!(
        slot.entry.restarts, 0,
        "a dropped report must never bump the restart count"
    );
    assert_eq!(
        slot.entry.pid,
        Some(pid),
        "a dropped report must never touch the running pid"
    );
}

// A backoff timer scheduled before a reload started draining this sheep
// must never respawn it: the slot belongs to the fresh replacement.
#[test]
fn a_stopping_sheep_rejects_a_restart_due() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _ctl_rx) = actor_with_stopping_drainee(&dir, 4242, 7);

    actor.handle_restart_due(0, 7);

    let slot = actor.sheep.get(&0).expect("the sheep stays registered");
    assert_eq!(
        slot.entry.status,
        ProcStatus::Stopping,
        "a rejected restart-due must never touch status"
    );
    assert_eq!(
        slot.entry.restarts, 0,
        "a rejected restart-due must never respawn"
    );
}
