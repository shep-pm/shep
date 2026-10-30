//! Fixtures for the `ApplyConfig` cases: an actor over a running flock and the
//! declared apps a Flockfile merges onto it.

use super::*;
use crate::limits::LimitEnforcer;

// --- `ApplyConfig`: a Flockfile merged onto a running flock ---
//
// Actor-tier, all but one: a load leaves behind `spec`, `pending` and a pid
// that must not have moved, and no reply reports those three together.

/// The pid the first fixture instance carries. Distinctive, so a case
/// asserting the child was left alone cannot be reading a default.
pub(super) const APPLY_FIRST_PID: u32 = 7100;

/// An actor over one `Online` instance of each app, plus the recording
/// enforcer its extras are armed against.
///
/// One slot per instance the config declares, ids in order. The extras are
/// real but nothing is armed, since no instance went through
/// `went_online`. Every slot is `Online` with a pid and no `ctl`, so a
/// stopped instance written onto the entry is removed synchronously.
pub(super) fn actor_over(
    dir: &tempfile::TempDir,
    apps: &[ResolvedApp],
) -> (Actor<ScriptedRunner>, Arc<RecordingEnforcer>) {
    let paths = test_paths(dir);
    let mut sheep = HashMap::new();
    let mut next_id = 0;
    for app in apps {
        for instance in 0..app.config().instances {
            let id = next_id;
            next_id += 1;
            sheep.insert(
                id,
                SheepSlot::new(armed_entry(
                    id,
                    instance,
                    APPLY_FIRST_PID + id,
                    app.clone(),
                    &paths,
                )),
            );
        }
    }
    let enforcer = Arc::new(RecordingEnforcer::default());
    let (breach_tx, _breaches) = mpsc::channel(1);
    let (live_tx, _liveness) = mpsc::channel(1);
    let extras = Extras {
        clock: Arc::new(SystemClock),
        enforcer: Arc::clone(&enforcer) as Arc<dyn LimitEnforcer>,
        lamb_sweep: idle_sweep(),
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports: ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        stats: idle_stats(),
    };
    let (tx, _rx) = mpsc::channel(MAILBOX_CAPACITY);
    // Enough scripts for a scale-up to come up: without them a case
    // that scales would assert on a shortfall rather than the apply.
    let scripts = vec![ProcScript::never_exits(); 4];
    let actor = Actor {
        extras: Some(extras),
        ..test_actor(paths, scripts, sheep, tx)
    };
    (actor, enforcer)
}

/// A [`DeclaredApp`] whose document wrote exactly `keys`, plus every key
/// of its own `env` table when `keys` names `env`.
pub(super) fn declared_app(config: AppConfig, keys: &[&str]) -> DeclaredApp {
    let declared: BTreeSet<String> = keys.iter().map(|key| (*key).to_string()).collect();
    let declared_env = if declared.contains("env") {
        config.env.keys().cloned().collect()
    } else {
        BTreeSet::new()
    };
    DeclaredApp {
        config,
        declared,
        declared_env,
    }
}

/// The override record an earlier load of `keys` would have left, with
/// `fields` set since by an operator.
pub(super) fn established(
    keys: &[&str],
    fields: Vec<(&str, serde_json::Value)>,
) -> shep_core::overrides::AppOverrides {
    shep_core::overrides::AppOverrides {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        declared: keys.iter().map(|key| (*key).to_string()).collect(),
        ..Default::default()
    }
}

/// `handle_command` answers before it returns, so the `await` here
/// resolves rather than hopes.
pub(super) async fn apply_config(
    actor: &mut Actor<ScriptedRunner>,
    apps: Vec<DeclaredApp>,
    reset: ResetDepth,
) -> Vec<Applied> {
    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::ApplyConfig { apps, reset, reply });
    answer
        .await
        .expect("the actor answers an apply before it returns")
        .expect("the fixture flock is registered")
}

/// The file the parked-config cases load: `env` set to `MODE=blue`, a key
/// that needs a respawn to take effect.
pub(super) fn blue_env_file() -> DeclaredApp {
    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    declared_app(file, &["name", "script", "env"])
}

/// Load [`blue_env_file`] onto the flock, parking `env` on `web`.
pub(super) async fn park_env_mode_blue(actor: &mut Actor<ScriptedRunner>) -> Vec<Applied> {
    apply_config(actor, vec![blue_env_file()], ResetDepth::None).await
}

/// Load a moved `out_file` and `err_file` onto the flock, parking both on
/// `web`.
pub(super) async fn park_log_paths(actor: &mut Actor<ScriptedRunner>) {
    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/var/log/moved-out.log".to_string());
    file.err_file = Some("/var/log/moved-err.log".to_string());
    apply_config(
        actor,
        vec![declared_app(
            file,
            &["name", "script", "out_file", "err_file"],
        )],
        ResetDepth::None,
    )
    .await;
}

/// Restart `web` the way `shep restart` does on a running sheep:
/// `begin_manual` claims the next exit and `handle_exited` respawns. The
/// other door is `apply_immediate`'s `Restart` arm; both end in `respawn`.
pub(super) fn manual_restart_web(actor: &mut Actor<ScriptedRunner>) {
    let (reply, _answer) = oneshot::channel();
    actor.begin_manual(
        ProcessSelector::Name("web".to_string()),
        ManualKind::Restart,
        CommandOrigin::Operator,
        ReplyKind::Info(reply),
    );
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );
}
