//! The one dog job lookout runs at a time: its status-bar item, the grant
//! modals it asks through, and what quitting mid-job leaves behind.
//!
//! The job itself is `commands::dogs::session`, the same code `shep install`
//! runs, on its own thread with its own runtime. Everything here is the
//! lookout half: one job at a time, one modal per dependency in the CLI's own
//! order, and a status-bar item a narrow bar drops before the key hint.

#[cfg(all(test, unix))]
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use shep_core::paths::ShepPaths;

use super::*;
#[cfg(all(test, unix))]
use crate::commands::dogs::stub_reach;
use crate::commands::dogs::{Job, Progress, Prompt, Reach, Step, run};
use crate::exit::ExitCode;

#[cfg(test)]
mod tests;

/// How long `installed <name>` stays on the bar after a job lands.
///
/// Long enough to read once after the bar has gone back to the key hint, and
/// short enough to be gone before the next `i` reopens the catalogue.
const DONE_SHOWS: Duration = Duration::from_secs(6);

/// The answer a session's grant callback is blocked on.
///
/// Dropped with the job, which is what releases a callback still waiting for
/// an operator who has already left: its [`std::sync::mpsc::Receiver`]
/// disconnects and the session reads that as a no.
#[derive(Debug)]
pub(crate) struct GrantReply(mpsc::Sender<bool>);

impl GrantReply {
    /// Hands `yes` to the waiting callback. A closed channel means the job
    /// ended, which is a no.
    fn answer(&self, yes: bool) {
        let _ = self.0.send(yes);
    }
}

/// A running dog job: what its thread reports, and the handle that ends it.
///
/// Dropping the handle resolves the job's own cancel. It cannot release a
/// callback blocked on a [`GrantReply`]; dropping the job's replies does that.
#[derive(Debug)]
pub(crate) struct DogRun {
    /// The [`Msg`] the job's thread earns, in order.
    pub(crate) msgs: tokio::sync::mpsc::UnboundedReceiver<Msg>,
    cancel: Option<Cancel>,
}

impl DogRun {
    /// Ends the run. A callback blocked on a [`GrantReply`] is released by
    /// dropping the job's replies, which the reducer does with the job.
    pub(crate) fn end(&mut self) {
        self.cancel = None;
    }
}

/// The handle that ends a running dog job, read only by being dropped.
#[derive(Debug)]
struct Cancel(
    #[expect(
        dead_code,
        reason = "the run races its receiver against this sender; dropping the sender ends the run"
    )]
    tokio::sync::oneshot::Sender<()>,
);

/// How lookout runs a dog job: which `cargo`, and which crates.io.
#[derive(Debug)]
pub(crate) enum DogRunner {
    /// This process's `cargo` on `PATH`, and the real crates.io.
    Real,
    /// A stub `cargo` at this path, with a crates.io that reports no
    /// publisher, so a test needs no socket.
    #[cfg(all(test, unix))]
    Stub(PathBuf),
}

impl DogRunner {
    /// Runs `job` on its own thread with its own current-thread runtime,
    /// sending each [`Msg`] it earns over `sink`.
    ///
    /// Its own thread and runtime because the grant callback blocks: on the
    /// UI task it would stop the redraw, the tick and the bus drain, and
    /// `spawn_blocking` gives the run no runtime to start `cargo` on.
    pub(crate) fn start(&self, paths: &ShepPaths, job: Job) -> DogRun {
        let (sink, msgs) = tokio::sync::mpsc::unbounded_channel();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let paths = paths.clone();
        let reach = match self {
            Self::Real => Reach::real(),
            #[cfg(all(test, unix))]
            Self::Stub(cargo) => stub_reach(cargo),
        };
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let name = job.name().to_owned();
                let progress = {
                    let (sink, name) = (sink.clone(), name.clone());
                    Progress::new(move |step| {
                        let _ = sink.send(Msg::DogStep {
                            name: name.clone(),
                            step,
                        });
                    })
                };
                let answer = {
                    let (sink, name) = (sink.clone(), name.clone());
                    move |prompt: &Prompt| {
                        let (reply, waiting) = mpsc::channel();
                        if sink
                            .send(Msg::DogGrant {
                                name: name.clone(),
                                prompt: prompt.clone(),
                                reply: Arc::new(GrantReply(reply)),
                            })
                            .is_err()
                        {
                            return false;
                        }
                        waiting.recv().unwrap_or(false)
                    }
                };
                let ended = tokio::select! {
                    biased;
                    _ = cancelled => None,
                    got = run(&paths, job, reach, progress, answer) => Some(got),
                };
                if let Some((code, output)) = ended {
                    let _ = sink.send(Msg::DogJobDone {
                        name,
                        ok: code == ExitCode::Success,
                        output,
                    });
                }
            });
        });
        DogRun {
            msgs,
            cancel: Some(Cancel(cancel)),
        }
    }
}

/// The job the status bar is describing, and what its modals are waiting on.
#[derive(Debug)]
pub(crate) enum DogJob {
    /// Nothing is running and nothing has just landed.
    Idle,
    /// A job is running.
    Running {
        /// The job, which names the dog.
        job: Job,
        /// The step it has reached, or `None` before the first report.
        step: Option<Step>,
        /// The [`Msg::Tick`] instant it started at.
        started: Instant,
        /// How long it has been running, advanced by each [`Msg::Tick`]
        /// against that tick's own `now` the way [`App::frozen_for`] is, so
        /// a job keeps counting across a dead link.
        elapsed: Duration,
        /// The grant replies waiting on a modal answer, by the tag this job
        /// gave that modal.
        grants: HashMap<u64, Arc<GrantReply>>,
        /// The tag of the quit question, while this job is asking it.
        quitting: Option<u64>,
    },
    /// A job landed and the bar is still saying so.
    Done {
        /// The dog.
        name: String,
        /// The tick instant it landed at.
        at: Instant,
    },
}

impl DogJob {
    /// The status bar's item for this job, or `None` when it has nothing to
    /// say: the name, the step and the time spent while it runs, then the
    /// dog alone for [`DONE_SHOWS`] after it lands.
    pub(crate) fn line(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Running {
                job, step, elapsed, ..
            } => Some(format!(
                "{} {}  {}  {}s",
                job_word(job),
                job.name(),
                step_word(*step),
                elapsed.as_secs()
            )),
            Self::Done { name, .. } => Some(format!("installed {name}")),
        }
    }

    /// Advances the running job's clock and retires a finished one.
    pub(super) fn tick(&mut self, now: Instant) {
        match self {
            Self::Idle => {}
            Self::Running {
                started, elapsed, ..
            } => {
                *elapsed = now.saturating_duration_since(*started);
            }
            Self::Done { at, .. } if now.saturating_duration_since(*at) >= DONE_SHOWS => {
                *self = Self::Idle;
            }
            Self::Done { .. } => {}
        }
    }

    /// Takes the reply `tag` is waiting on, or `None` for a tag that is not
    /// an open grant question.
    fn grant(&mut self, tag: u64) -> Option<Arc<GrantReply>> {
        let Self::Running { grants, .. } = self else {
            return None;
        };
        grants.remove(&tag)
    }

    /// Holds `reply` against `tag` until it is answered or the job ends.
    fn hold(&mut self, tag: u64, reply: Arc<GrantReply>) {
        if let Self::Running { grants, .. } = self {
            grants.insert(tag, reply);
        }
    }

    /// Whether `tag` is one of this job's open grant questions.
    fn holds(&self, tag: u64) -> bool {
        matches!(self, Self::Running { grants, .. } if grants.contains_key(&tag))
    }

    /// Whether `tag` is this job's quit question, which it is no longer
    /// asking either way.
    fn is_quitting(&mut self, tag: u64) -> bool {
        let Self::Running { quitting, .. } = self else {
            return false;
        };
        quitting.take() == Some(tag)
    }

    /// Marks `tag` as the quit question this job is asking.
    fn ask_quit(&mut self, tag: u64) {
        if let Self::Running { quitting, .. } = self {
            *quitting = Some(tag);
        }
    }
}

/// The status bar's word for what this job is doing to the dog.
fn job_word(job: &Job) -> &'static str {
    match job {
        Job::Install { .. } => "installing",
        Job::Upgrade { .. } => "upgrading",
        Job::Delete { .. } => "deleting",
    }
}

/// The status bar's word for one step, and for the gap before the first one
/// is reported.
fn step_word(step: Option<Step>) -> &'static str {
    match step {
        None => "starting",
        Some(Step::Downloading) => "downloading",
        Some(Step::Building) => "building with cargo",
        Some(Step::Adopting) => "enabling",
    }
}

impl App {
    /// `shep install <adopt_as>`, from `Enter` on a catalogue entry.
    pub(crate) fn start_install(&mut self, adopt_as: &str) -> Effect {
        self.start_job(Job::Install {
            adopt_as: adopt_as.to_owned(),
        })
    }

    /// Starts `job`, or says why it will not start now.
    ///
    /// Refused with a notice when the control gate is closed, and with a
    /// modal when a job is already running: one at a time, and the second
    /// starts nothing.
    fn start_job(&mut self, job: Job) -> Effect {
        let Some(authority) = WriteAuthority::granted(self) else {
            self.notice = Some(Notice::grave(READ_ONLY_REFUSAL.to_string()));
            return Effect::None;
        };
        if let DogJob::Running { job: running, .. } = &self.dog_job {
            let (name, doing, tag) = (
                running.name().to_owned(),
                job_word(running),
                self.mint_tag(),
            );
            self.open_modal(Modal::output(
                tag,
                job_word(&job),
                &format!("{name} is still {doing}. This one waits for it."),
            ));
            return Effect::None;
        }
        self.dog_job = DogJob::Running {
            job: job.clone(),
            step: None,
            started: self.now,
            elapsed: Duration::ZERO,
            grants: HashMap::new(),
            quitting: None,
        };
        Effect::StartDogJob { job, authority }
    }

    /// Whether `row` has an install action here: the index names a source
    /// shep can build, nothing is installed under that name yet, and the
    /// gate is open.
    pub(crate) fn can_install(&self, row: &CatalogueRow) -> bool {
        row.state == DogState::NotInstalled
            && row.dog.is_installable()
            && WriteAuthority::granted(self).is_some()
    }

    /// `Msg::DogStep`'s arm: the bar's step word moves.
    pub(super) fn on_dog_step(&mut self, name: &str, step: Step) -> Effect {
        if let DogJob::Running {
            job,
            step: at,
            grants,
            ..
        } = &mut self.dog_job
            && job.name() == name
        {
            *at = Some(step);
            // A step past a question nobody answered releases that callback,
            // so a session never waits on a modal it has moved on from.
            grants.clear();
        }
        Effect::None
    }

    /// `Msg::DogGrant`'s arm: the prompt the CLI would print, as a modal,
    /// with the callback's reply held against the tag it was opened under.
    ///
    /// A grant for a job that is no longer running is dropped, which
    /// disconnects the reply and reads as a no.
    pub(super) fn on_dog_grant(
        &mut self,
        name: &str,
        prompt: Prompt,
        reply: Arc<GrantReply>,
    ) -> Effect {
        let DogJob::Running { job, .. } = &self.dog_job else {
            return Effect::None;
        };
        if job.name() != name {
            return Effect::None;
        }
        let tag = self.mint_tag();
        self.dog_job.hold(tag, reply);
        self.open_modal(Modal::grant(tag, prompt));
        Effect::None
    }

    /// `Msg::DogJobDone`'s arm: a success clears the running item, shows
    /// `installed <name>`, and re-reads the rows so STATE and VERSION catch
    /// up. A failure opens a modal with what the installer printed.
    pub(super) fn on_dog_job_done(&mut self, name: &str, ok: bool, output: String) -> Effect {
        let DogJob::Running { job, .. } = &self.dog_job else {
            return Effect::None;
        };
        if job.name() != name {
            return Effect::None;
        }
        let verb = job_word(job).to_owned();
        self.dog_job = if ok {
            DogJob::Done {
                name: name.to_owned(),
                at: self.now,
            }
        } else {
            DogJob::Idle
        };
        if ok {
            return Effect::LoadCatalogue { fetch: false };
        }
        let tag = self.mint_tag();
        self.open_modal(Modal::output(tag, &format!("{verb} {name}"), &output));
        Effect::None
    }

    /// `q` while a job runs asks first. `None` when no job is running, so
    /// the caller quits as it always has.
    pub(super) fn on_dog_quit(&mut self) -> Option<Effect> {
        let DogJob::Running { job, .. } = &self.dog_job else {
            return None;
        };
        let (name, doing, tag) = (job.name().to_owned(), job_word(job), self.mint_tag());
        self.dog_job.ask_quit(tag);
        self.open_modal(Modal::confirm(
            tag,
            format!("quit lookout and stop {doing} {name}?"),
        ));
        Some(Effect::None)
    }

    /// What an answered modal means for the job, or `None` when it answers
    /// nothing here.
    ///
    /// The quit question answers for the whole dashboard. A grant question's
    /// answer reaches the waiting callback through [`Effect::ModalAnswered`],
    /// which the caller delivers with [`App::answer_grant`].
    pub(super) fn on_dog_modal_answer(&mut self, tag: u64, yes: bool) -> Option<Effect> {
        if self.dog_job.is_quitting(tag) {
            return Some(if yes { Effect::Quit } else { Effect::None });
        }
        if self.dog_job.holds(tag) {
            return Some(Effect::ModalAnswered { tag, yes });
        }
        None
    }

    /// Hands the answer of the modal `tag` asked to the session waiting on
    /// it. A tag the job no longer holds answers nothing.
    pub(crate) fn answer_grant(&mut self, tag: u64, yes: bool) {
        if let Some(reply) = self.dog_job.grant(tag) {
            reply.answer(yes);
        }
    }

    /// The status bar's dog-job item, or `None` when there is none.
    pub(crate) fn dog_job_line(&self) -> Option<String> {
        self.dog_job.line()
    }

    /// The next modal tag. Tags only have to be unique among the modals open
    /// at once, and a job's own answers drop with it.
    fn mint_tag(&mut self) -> u64 {
        self.next_modal_tag += 1;
        self.next_modal_tag
    }
}
