//! Replacing a dog adopted from a path with a receipt-backed catalogue build.

use shep_core::paths::ShepPaths;

use crate::cli::InstallArgs;
use crate::commands::copy::confirm::Answer;
use crate::commands::shep_toml::ShepToml;
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
        || receipt::receipt_path(paths, name).exists()
    {
        return Ok(true);
    }
    let config = ShepToml::read_only(&paths.daemon_config)
        .map_err(|err| super::fail_config(streams, &err))?;
    let Some(path) = config.adopted_dog_path(name) else {
        return Ok(true);
    };
    if args.yes {
        return Ok(true);
    }
    let question = format!(
        "Replace adopted {name} at {} with the catalogue build?",
        path.display()
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
