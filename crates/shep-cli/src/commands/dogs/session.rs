//! One install, upgrade or delete run without a terminal, for lookout.
//!
//! The verbs print to a terminal and ask on stdin. A session runs the same
//! code over in-memory streams, tells a sink which [`Step`] it is at, asks
//! through a callback, and hands back the exit code with everything the verb
//! wrote. The index comes from the cache and is never fetched.

use std::io::Write;
#[cfg(all(test, unix))]
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
#[cfg(all(test, unix))]
use std::time::Duration;

use shep_core::paths::ShepPaths;

use crate::cli::{Format, InstallArgs};
use crate::dog_index::{self, Index};
use crate::exit::ExitCode;
use crate::fetch;
#[cfg(all(test, unix))]
use crate::fetch::RedirectPolicy;
use crate::output::Streams;
use crate::style::Presentation;

use super::consent::{Input, Prompt};
use super::dog_upgrade::{Goal, upgrade_dog};
#[cfg(all(test, unix))]
use super::install::{Lookup, Published};
use super::install::{Reach, install_dog};
use super::install_depends::Catalog;
use super::remove::{Removal, remove_dog};

/// Where an install or upgrade has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Fetching the prebuilt binary the index hashes.
    Downloading,
    /// Running `cargo install`.
    Building,
    /// The binary is in place and its grant question and enable come next.
    Adopting,
}

/// Who hears each [`Step`].
#[derive(Clone)]
pub(crate) struct Progress(Arc<dyn Fn(Step) + Send + Sync>);

impl Progress {
    /// A sink that calls `sink` at each step.
    pub(crate) fn new(sink: impl Fn(Step) + Send + Sync + 'static) -> Self {
        Self(Arc::new(sink))
    }

    pub(super) fn tell(&self, step: Step) {
        (self.0)(step);
    }
}

impl core::fmt::Debug for Progress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Progress").finish_non_exhaustive()
    }
}

/// What a session does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    /// `shep install <adopt_as>`, asking the grant question.
    Install {
        /// The name the index row is adopted under.
        adopt_as: String,
    },
    /// `shep upgrade <name>`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "lookout's upgrade and delete slice names it")
    )]
    Upgrade {
        /// The adopted dog to move to what its receipt follows.
        name: String,
    },
    /// `shep delete <name>`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "lookout's upgrade and delete slice names it")
    )]
    Delete {
        /// The adopted dog to remove.
        name: String,
    },
}

impl Job {
    /// The dog this job is for, by the name it is adopted under.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Install { adopt_as } => adopt_as,
            Self::Upgrade { name } | Self::Delete { name } => name,
        }
    }
}

/// Runs `job` against the cached index for this build's index URL, reporting
/// to `progress` and asking the grant question through `answer`. Returns the
/// exit code the verb would have exited with and the text it would have
/// printed.
///
/// `reach` is [`Reach::real`] for a run that means it, and a test's own for
/// one that stands a stub in for `cargo`. `answer` blocks, so a caller on an
/// async runtime runs this on a blocking thread. Dropping the session
/// part-way leaves what the verb leaves.
pub(crate) async fn run(
    paths: &ShepPaths,
    job: Job,
    reach: Reach,
    progress: Progress,
    answer: impl FnMut(&Prompt) -> bool + Send + 'static,
) -> (ExitCode, String) {
    let reach = Reach {
        progress: Some(progress),
        ..reach
    };
    run_with(paths, &dog_index::index_url(), job, reach, answer).await
}

/// A [`Reach`] for a test: `cargo` is the stub at `cargo`, crates.io reports
/// no publisher, and no prebuilt download is ever attempted.
#[cfg(all(test, unix))]
pub(crate) fn stub_reach(cargo: &Path) -> Reach {
    Reach {
        cargo: Some(cargo.to_path_buf()),
        lookup: Lookup::Canned(Published::Unreported),
        assets: RedirectPolicy::loopback("127.0.0.1", &["localhost"]),
        lock_wait: Duration::from_millis(100),
        progress: None,
    }
}

/// [`run`] against the index cached for `url` and this `reach`.
pub(super) async fn run_with(
    paths: &ShepPaths,
    url: &str,
    job: Job,
    reach: Reach,
    answer: impl FnMut(&Prompt) -> bool + Send + 'static,
) -> (ExitCode, String) {
    let capture = Capture::default();
    let (mut out, mut err) = (capture.clone(), capture.clone());
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: Presentation::BARE,
        fmt: Format::Table,
    };
    let mut input = Input::answering(answer);
    let code = dispatch(&mut streams, paths, url, job, &reach, &mut input).await;
    (code, capture.text())
}

async fn dispatch(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    url: &str,
    job: Job,
    reach: &Reach,
    input: &mut Input,
) -> ExitCode {
    match job {
        Job::Delete { name } => remove_dog(streams, paths, &name, Removal::Delete).await,
        Job::Install { adopt_as } => {
            let index = match cached(streams, paths, url) {
                Ok(index) => index,
                Err(code) => return code,
            };
            let Some(dog) = index.dogs.iter().find(|dog| dog.adopt_as == adopt_as) else {
                return streams.fail(
                    ExitCode::NotFound,
                    &format!("the dog index has no dog called `{adopt_as}`"),
                );
            };
            let args = InstallArgs {
                dog: Some(adopt_as),
                yes: false,
                no_enable: false,
                refresh: false,
                cargo: false,
                git_ref: None,
            };
            let catalog = Catalog::new(url, &index.dogs);
            install_dog(streams, paths, &args, reach, input, dog, &catalog).await
        }
        Job::Upgrade { name } => {
            let index = match cached(streams, paths, url) {
                Ok(index) => index,
                Err(code) => return code,
            };
            let catalog = Catalog::new(url, &index.dogs);
            upgrade_dog(streams, paths, reach, input, &catalog, &name, &Goal::Follow).await
        }
    }
}

/// The index cached for `url`, of any age.
fn cached(streams: &mut Streams<'_>, paths: &ShepPaths, url: &str) -> Result<Index, ExitCode> {
    dog_index::cached_only(paths, url).ok_or_else(|| {
        streams.fail(
            ExitCode::Failure,
            &format!(
                "reading the dog index from {}: no cached copy; `shep install` fetches it",
                fetch::url_for_message(url)
            ),
        )
    })
}

/// A writer both of a session's streams share, so the text keeps the order
/// it was written in.
#[derive(Clone, Default)]
pub(super) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub(super) fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
