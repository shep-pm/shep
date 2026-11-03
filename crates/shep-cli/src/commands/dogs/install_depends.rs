//! The dependency step of `shep install`: the dogs a manifest names that are
//! not adopted install first, each through its own grant question, and a no
//! at one stops the whole install.

use shep_core::dogs::manifest::Manifest;
use shep_core::paths::ShepPaths;

use crate::cli::InstallArgs;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::AvailableDog;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::consent::Input;
use super::depends;
use super::install::{Reach, install_noting};

/// The index an install reads dependencies from, and the dogs being
/// installed above the one in hand.
#[derive(Debug)]
pub(super) struct Catalog<'a> {
    /// Where the index was read from, recorded in each receipt.
    pub(super) url: &'a str,
    /// Every row the index carries.
    pub(super) dogs: &'a [AvailableDog],
    /// The dogs being installed above the current one, outermost first.
    above: Vec<String>,
}

impl<'a> Catalog<'a> {
    /// The index at `url` holding `dogs`, with nothing being installed yet.
    pub(super) fn new(url: &'a str, dogs: &'a [AvailableDog]) -> Self {
        Self {
            url,
            dogs,
            above: Vec::new(),
        }
    }

    /// This catalog seen from a dependency of `name`.
    fn below(&self, name: &str) -> Self {
        let mut above = self.above.clone();
        above.push(name.to_owned());
        Self {
            url: self.url,
            dogs: self.dogs,
            above,
        }
    }
}

/// Installs each dog `manifest` needs that is not adopted, ahead of `name`.
///
/// Everything the index must supply is checked before the first install. A
/// no at a dependency's question stops the run, leaving that dependency
/// installed and disabled.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn install_dependencies(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &InstallArgs,
    reach: &Reach,
    input: &mut Input,
    name: &str,
    manifest: &Manifest,
    catalog: &Catalog<'_>,
) -> Result<(), ExitCode> {
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .map(|config| config.adopted_dog_names())
        .map_err(|err| crate::commands::dogs::fail_config(streams, &err))?;
    let within = catalog.below(name);
    let wanted = depends::missing(manifest, name, &within.above, &adopted, catalog.dogs).map_err(
        |refusal| streams.fail(refusal.code(), &format!("{refusal}; nothing was adopted")),
    )?;
    for row in wanted {
        let dependency = InstallArgs {
            dog: Some(row.adopt_as.clone()),
            yes: args.yes,
            no_enable: args.no_enable,
            refresh: args.refresh,
            cargo: args.cargo,
        };
        let mut said_no = false;
        let code = Box::pin(install_noting(
            streams,
            paths,
            &dependency,
            reach,
            input,
            row,
            &within,
            &mut said_no,
        ))
        .await;
        if code != ExitCode::Success {
            return Err(code);
        }
        if said_no {
            return Err(streams.fail(
                ExitCode::Failure,
                &format!(
                    "{name} needs {dep}, and you said no to it, so {name} was not installed. \
                     {dep} stays installed and disabled",
                    dep = row.adopt_as
                ),
            ));
        }
    }
    Ok(())
}
