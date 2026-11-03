//! The dependency step of `shep install`: the dogs a manifest names that are
//! not adopted are fetched with the dog, every refusal the index can cause
//! comes before the first adopt, and each is then adopted through its own
//! grant question, dependencies first. A no at one stops the install.

use shep_core::paths::ShepPaths;

use crate::cli::InstallArgs;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::AvailableDog;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::consent::Input;
use super::depends;
use super::install::{Fetched, Reach, adopt_fetched, fetch_dog};

/// The index an install reads dependencies from.
#[derive(Debug)]
pub(super) struct Catalog<'a> {
    /// Where the index was read from, recorded in each receipt.
    url: &'a str,
    /// Every row the index carries.
    dogs: &'a [AvailableDog],
}

impl<'a> Catalog<'a> {
    /// The index at `url` holding `dogs`.
    pub(super) fn new(url: &'a str, dogs: &'a [AvailableDog]) -> Self {
        Self { url, dogs }
    }
}

/// Fetches `dog` and every dependency of it that is not adopted into
/// `fetched`, each after its own dependencies.
///
/// A dependency met twice is fetched once. Nothing is adopted, so a refusal
/// leaves `shep.toml` as it was and dropping `fetched` puts back what the
/// fetches replaced.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(super) async fn gather(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &InstallArgs,
    reach: &Reach,
    catalog: &Catalog<'_>,
    dog: &AvailableDog,
    fetched: &mut Vec<Fetched>,
) -> Result<(), ExitCode> {
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .map(|config| config.adopted_dog_names())
        .map_err(|err| crate::commands::dogs::fail_config(streams, &err))?;
    let mut trail = Vec::new();
    gather_below(
        streams,
        paths,
        &(args, reach, catalog, adopted.as_slice()),
        dog,
        &mut trail,
        fetched,
    )
    .await
}

/// What every level of [`gather`] reads and none changes.
type Settings<'a> = (&'a InstallArgs, &'a Reach, &'a Catalog<'a>, &'a [String]);

async fn gather_below(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    settings: &Settings<'_>,
    dog: &AvailableDog,
    trail: &mut Vec<String>,
    fetched: &mut Vec<Fetched>,
) -> Result<(), ExitCode> {
    let (args, reach, catalog, adopted) = *settings;
    // `--ref` builds the dog asked for; what it needs comes from the index.
    let git_ref = if trail.is_empty() {
        args.git_ref.as_deref()
    } else {
        None
    };
    let here = fetch_dog(streams, paths, args, reach, dog, git_ref).await?;
    trail.push(dog.adopt_as.clone());
    if let Some(manifest) = &here.manifest {
        let wanted = depends::missing(manifest, &dog.adopt_as, trail, adopted, catalog.dogs)
            .map_err(|refusal| {
                streams.fail(refusal.code(), &format!("{refusal}; nothing was adopted"))
            })?;
        for row in wanted {
            if fetched.iter().any(|met| met.dog.adopt_as == row.adopt_as) {
                continue;
            }
            Box::pin(gather_below(streams, paths, settings, row, trail, fetched)).await?;
        }
    }
    trail.pop();
    fetched.push(here);
    Ok(())
}

/// Adopts each of `fetched` in order, the dog last. A no at a dependency's
/// question stops the run, leaving that dependency installed and disabled and
/// the rest not adopted.
pub(super) async fn adopt_in_order(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &InstallArgs,
    input: &mut Input,
    catalog: &Catalog<'_>,
    fetched: Vec<Fetched>,
) -> ExitCode {
    let count = fetched.len();
    let dog = fetched.last().map(|last| last.dog.adopt_as.clone());
    for (at, one) in fetched.into_iter().enumerate() {
        let name = one.dog.adopt_as.clone();
        let mut said_no = false;
        if at + 1 == count {
            return adopt_fetched(streams, paths, args, input, one, catalog.url, &mut said_no)
                .await;
        }
        // A dependency's result is not the command's: its envelope stays off
        // stdout so one document answers for the dog asked for.
        let mut sink = std::io::sink();
        let mut quiet = Streams {
            out: &mut sink,
            err: &mut *streams.err,
            style: streams.style,
            fmt: streams.fmt,
        };
        let code = adopt_fetched(
            &mut quiet,
            paths,
            args,
            input,
            one,
            catalog.url,
            &mut said_no,
        )
        .await;
        if code != ExitCode::Success {
            return code;
        }
        let dog = dog.as_deref().unwrap_or_default();
        if said_no {
            return streams.fail(
                ExitCode::Failure,
                &format!(
                    "{dog} needs {name}, and you said no to it, so {dog} was not installed. \
                     {name} stays installed and disabled"
                ),
            );
        }
        streams.aside("install", &format!("installed {name}, which {dog} needs"));
    }
    ExitCode::Success
}
