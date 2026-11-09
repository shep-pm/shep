//! Replacing a dog adopted from a path with a receipt-backed catalogue build.

use std::path::PathBuf;

use shep_core::paths::ShepPaths;

use crate::cli::InstallArgs;
use crate::commands::copy::confirm::Answer;
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::dog_index::AvailableDog;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::consent::{Input, Prompt};
use super::receipt;

/// Asks before fetching a replacement for an adopted dog without a receipt.
///
/// # Errors
/// The config cannot be read, or the answer cannot be obtained.
pub(super) fn confirm(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &InstallArgs,
    input: &mut Input,
    dog: &AvailableDog,
) -> Result<bool, ExitCode> {
    let name = dog.adopt_as.as_str();
    if !super::install::is_path_safe(name)
        || (!dog.is_installable() && args.git_ref.is_none())
    {
        return Ok(true);
    }
    let Some(replacement) = replacement(paths, name)
        .map_err(|err| super::fail_config(streams, &err))? else {
        return Ok(true);
    };
    if args.yes {
        return Ok(true);
    }
    let question = format!(
        "Replace adopted {name} at {} with the catalogue build?",
        replacement.path.display()
    );
    if !input.is_terminal() {
        return Err(streams.fail(
            ExitCode::Usage,
            &format!("nothing was changed: {question} Pass --yes to replace it without asking"),
        ));
    }
    let prompt = Prompt::new(String::new(), question, None);
    match input.ask(streams, &prompt) {
        Answer::Yes => Ok(true),
        Answer::No => Ok(false),
        Answer::Failed(reason) => Err(streams.fail(
            ExitCode::Failure,
            &format!("nothing was changed: the question could not be read ({reason})"),
        )),
    }
}

/// The adopted path and enabled state a catalogue install replaces.
#[derive(Debug)]
pub(super) struct Replacement {
    pub(super) path: PathBuf,
    pub(super) enable: bool,
}

/// Reads an adoption only when no install receipt backs it.
///
/// # Errors
/// `shep.toml` cannot be read or parsed.
pub(super) fn replacement(paths: &ShepPaths, name: &str) -> Result<Option<Replacement>, ShepTomlError> {
    if receipt::receipt_path(paths, name).exists() {
        return Ok(None);
    }
    let config = ShepToml::read_only(&paths.daemon_config)?;
    Ok(config.adopted_dog_path(name).map(|path| Replacement {
        path,
        enable: config.enabled_dog_names().iter().any(|enabled| enabled == name),
    }))
}
