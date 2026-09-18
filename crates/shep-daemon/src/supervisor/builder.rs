//! Starting the actor task.
//!
//! [`spawn_supervisor`] is the ordinary entry point and takes the defaults.
//! `SupervisorBuilder` is the long form the daemon's boot path uses when it
//! has to override the clock, the runner or the paths, as the tests do.

use super::*;

/// Builds a supervisor actor.
#[derive(Debug)]
pub(crate) struct SupervisorBuilder<R: ProcessRunner> {
    pub(super) runner: R,
    pub(super) paths: ShepPaths,
    pub(super) events: Bus,
    pub(super) extras: Option<Extras>,
    pub(super) environment: String,
    pub(super) provider_secrets: Option<Arc<ProviderSecrets>>,
}

impl<R: ProcessRunner> SupervisorBuilder<R> {
    /// A builder with no lifecycle extras: the engine spawns, restarts and
    /// kills, and nothing watches, schedules or probes. `events` receives
    /// [`BusEvent::Process`] plus logs forwarded from each sheep.
    pub(crate) fn new(runner: R, paths: ShepPaths, events: Bus) -> Self {
        Self {
            runner,
            paths,
            events,
            extras: None,
            environment: DEFAULT_ENVIRONMENT.to_string(),
            provider_secrets: None,
        }
    }

    /// Wires in the lifecycle extras.
    #[must_use]
    pub(crate) fn extras(mut self, extras: Extras) -> Self {
        self.extras = Some(extras);
        self
    }

    /// The environment a sheep that names none of its own resolves its
    /// secrets in, from `[daemon] environment`.
    ///
    /// Left at [`DEFAULT_ENVIRONMENT`] when unset, which is what
    /// `DaemonSection` itself defaults to, so a builder nobody told and a
    /// file that says nothing agree.
    #[must_use]
    pub(crate) fn environment(mut self, environment: String) -> Self {
        self.environment = environment;
        self
    }

    /// The registry a provider dog pushes into, shared with the connection
    /// tasks that serve `Request::PutSecrets`.
    ///
    /// Left unset, the actor loads one of its own from
    /// [`ShepPaths::secrets_cache`], which is what a cold boot with no dog
    /// yet running resolves against anyway. `boot` passes the shared one
    /// so a push reaches the next spawn.
    #[must_use]
    pub(crate) fn provider_secrets(mut self, secrets: Arc<ProviderSecrets>) -> Self {
        self.provider_secrets = Some(secrets);
        self
    }

    /// Spawns the actor.
    ///
    /// Must be called from within a Tokio runtime context.
    pub(crate) fn spawn(self) -> SupervisorHandle {
        let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
        let actor = self.build(tx.clone());
        tokio::spawn(actor.run(rx));
        SupervisorHandle { tx }
    }

    /// Spawns the actor around a flock this image inherited, restoring
    /// `counters` before any slot is installed. Every sheep in `flock` goes in
    /// under the id, epoch, status and history the blob carried, around the
    /// descriptors that crossed the `execve`. Nothing here spawns, signals or
    /// reopens anything. `reloads` is the apps that were mid-swap, restored
    /// once the flock is in. Must be called from within a Tokio runtime
    /// context.
    ///
    /// # Errors
    ///
    /// - [`AdoptError::Spec`]: a carried config does not normalize.
    /// - [`AdoptError::Runner`]: the runner refused the inherited handles.
    #[cfg(unix)]
    pub(crate) fn spawn_adopted(
        self,
        flock: Vec<AdoptedSheep>,
        counters: Counters,
        reloads: Vec<CarriedReload>,
    ) -> Result<SupervisorHandle, AdoptError> {
        let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
        let mut actor = self.build(tx.clone());
        // Counters before slots: a fresh sheep must not be handed an id a
        // caller is still holding.
        actor.next_id = counters.next_id;
        actor.next_deadline = counters.next_deadline;
        actor.next_action_stamp = counters.next_action_stamp;
        // One reaper for the whole adopted flock: a status can be collected
        // once, so two reapers racing on one pid would have one take the exit
        // and the other meet `ECHILD`.
        let reaper = Arc::new(AdoptedReaper::new());
        for sheep in flock {
            actor.install_adopted(sheep, &reaper)?;
        }
        // After the loop and before the actor runs: a job names two entries,
        // so it has nowhere to go until every sheep is in, and the readiness
        // waits `install_adopted` armed report into a mailbox nothing drains.
        actor.install_carried_reloads(reloads);
        tokio::spawn(actor.run(rx));
        Ok(SupervisorHandle { tx })
    }

    /// The actor both spawn paths start from: no sheep, counters at zero.
    fn build(self, tx: mpsc::Sender<Msg>) -> Actor<R> {
        let provider_secrets = self
            .provider_secrets
            .unwrap_or_else(|| Arc::new(ProviderSecrets::load(&self.paths.secrets_cache)));
        Actor {
            runner: self.runner,
            paths: self.paths,
            events: self.events,
            host_environment: self.environment,
            provider_secrets,
            tx,
            sheep: HashMap::new(),
            next_id: 0,
            next_deadline: 0,
            next_action_stamp: 0,
            pending: Vec::new(),
            shutting_down: false,
            extras: self.extras,
            registry: ExtrasRegistry::default(),
            reloads: HashMap::new(),
            smits: Smits::new(),
        }
    }
}

/// Spawns the actor with no lifecycle extras: shorthand for
/// `SupervisorBuilder::new(runner, paths, events).spawn()`.
///
/// Must be called from within a Tokio runtime context: it spawns the actor
/// task immediately.
pub fn spawn_supervisor<R: ProcessRunner>(
    runner: R,
    paths: ShepPaths,
    events: Bus,
) -> SupervisorHandle {
    SupervisorBuilder::new(runner, paths, events).spawn()
}
