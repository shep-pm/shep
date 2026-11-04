//! Hints: one line under `shep flock` suggesting a dog.
//!
//! A hint reads the cached dog index, `shep.toml`, the listing it is handed
//! and file lengths, and nothing else: it never fetches and never asks the
//! shepherd. shep words every line itself, from the fixed list in [`Hint`].

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;

use crate::cli::Format;
use crate::commands::dogs;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{self, Need};
use crate::output::Streams;
use crate::terminal_safe::sanitise;

mod state;
#[cfg(test)]
mod tests;

pub(crate) use state::run;

/// A log file longer than this many bytes (100 MiB) brings up [`Hint::Logs`].
pub(crate) const LOG_FILE_HINT_BYTES: u64 = 100 * 1024 * 1024;

/// A sheep restarted this many times or more brings up [`Hint::Alerts`].
///
/// Counts [`ProcessInfo::restarts`] as the shepherd reports it: since the
/// sheep was registered, `shep restart` included, cleared by `shep reset`.
pub(crate) const RESTART_HINT_COUNT: u32 = 3;

/// The built-in dog an alerts hint always offers.
const BARK: &str = "bark";

/// A suggestion shep words itself, from a fixed list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    /// A log file has grown large.
    Logs,
    /// A sheep keeps restarting.
    Alerts,
}

impl Hint {
    /// Every hint, in the order one is picked.
    pub(crate) const ALL: [Self; 2] = [Self::Logs, Self::Alerts];

    /// The name `hints.json` spells this hint as.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Logs => "logs",
            Self::Alerts => "alerts",
        }
    }

    /// The hint named `name`, or `None` for a name this build does not know.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|hint| hint.name() == name)
    }

    /// The need an index row must hold to be offered for this hint.
    const fn need(self) -> Need {
        match self {
            Self::Logs => Need::Logs,
            Self::Alerts => Need::Alerts,
        }
    }
}

/// How far along an offered dog is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferState {
    /// No receipt: `shep install` is the next step.
    NotInstalled,
    /// A receipt exists but the dog is off: `shep enable` is the next step.
    Installed,
    /// Part of shep: `shep enable` is the next step.
    BuiltIn,
}

/// One dog a hint suggests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    /// The name to type: the index row's `adopt_as`, or `bark`.
    pub(crate) name: String,
    /// What typing it does.
    pub(crate) state: OfferState,
}

impl Offer {
    /// The command that takes the next step for this dog.
    pub(crate) fn command(&self) -> String {
        match self.state {
            OfferState::NotInstalled => format!("shep install {}", self.name),
            OfferState::Installed | OfferState::BuiltIn => format!("shep enable {}", self.name),
        }
    }
}

/// The hint [`next`] picked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    /// Which hint applies.
    pub(crate) hint: Hint,
    /// The one line to print, without a newline.
    pub(crate) line: String,
    /// The dogs the line suggests, in the order it names them.
    pub(crate) offers: Vec<Offer>,
}

/// The first hint, in [`Hint::ALL`] order, that applies to `flock`, is not
/// turned off and has not ended.
///
/// `index_url` names which cached index to read. A hint with nothing to offer
/// shows nothing.
pub(crate) fn next(paths: &ShepPaths, index_url: &str, flock: &[ProcessInfo]) -> Option<Shown> {
    let off = state::Off::read(&paths.home);
    let enabled = ShepToml::read_only(&paths.daemon_config)
        .map(|config| config.enabled_dog_names())
        .unwrap_or_default();
    let index = dog_index::cached_only(paths, index_url);
    Hint::ALL
        .into_iter()
        .filter(|hint| !off.contains(*hint))
        .find_map(|hint| show(hint, paths, index.as_ref(), &enabled, flock))
}

/// `hint`'s line, or `None` when it does not apply, has ended or has nothing
/// to offer.
fn show(
    hint: Hint,
    paths: &ShepPaths,
    index: Option<&dog_index::Index>,
    enabled: &[String],
    flock: &[ProcessInfo],
) -> Option<Shown> {
    let sheep = flock
        .iter()
        .filter(|info| info.dog.is_none() && applies(hint, info))
        .min_by_key(|info| info.id)?;
    let names = offered_names(hint, index);
    if names.iter().any(|name| enabled.contains(name)) {
        return None;
    }
    let offers = offers_of(paths, names);
    if offers.is_empty() {
        return None;
    }
    let (name, _) = sanitise(&sheep.name);
    let commands: Vec<String> = offers.iter().map(Offer::command).collect();
    let line = match hint {
        Hint::Logs => format!(
            "hint: {name}'s log is past {} MiB. Try: {}",
            LOG_FILE_HINT_BYTES / (1024 * 1024),
            commands.join(" or ")
        ),
        Hint::Alerts => format!(
            "hint: {name} has restarted {} times. Try: {}",
            sheep.restarts,
            commands.join(" or ")
        ),
    };
    Some(Shown { hint, line, offers })
}

/// `names` as offers, each with how far along its dog is.
fn offers_of(paths: &ShepPaths, names: Vec<String>) -> Vec<Offer> {
    names
        .into_iter()
        .map(|name| {
            let state = if name == BARK {
                OfferState::BuiltIn
            } else if dogs::installed(paths, &name).is_some() {
                OfferState::Installed
            } else {
                OfferState::NotInstalled
            };
            Offer { name, state }
        })
        .collect()
}

/// Whether `sheep` is what `hint` is about.
fn applies(hint: Hint, sheep: &ProcessInfo) -> bool {
    match hint {
        Hint::Logs => [&sheep.out_file, &sheep.err_file]
            .into_iter()
            .flatten()
            .filter_map(|path| std::fs::metadata(path).ok())
            .any(|meta| meta.len() > LOG_FILE_HINT_BYTES),
        Hint::Alerts => sheep.restarts >= RESTART_HINT_COUNT,
    }
}

/// The names `hint` suggests: `bark` for alerts, then every index row that
/// holds the hint's need.
fn offered_names(hint: Hint, index: Option<&dog_index::Index>) -> Vec<String> {
    let mut names = Vec::new();
    if hint == Hint::Alerts {
        names.push(BARK.to_owned());
    }
    for row in index.map_or(&[][..], |index| &index.dogs[..]) {
        if row.needs.contains(&hint.need()) && !names.contains(&row.adopt_as) {
            names.push(row.adopt_as.clone());
        }
    }
    names
}

/// Prints the hint, if any, under a `shep flock` listing just written.
///
/// Writes only for [`Format::Table`] on a terminal, with no `--quiet` and no
/// `--follow`. Lists the flock itself over the local socket, and prints
/// nothing when that fails.
pub(crate) async fn print_under_flock(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    quiet: bool,
    follow: bool,
    stdout_is_terminal: bool,
) {
    if streams.fmt != Format::Table || quiet || follow || !stdout_is_terminal {
        return;
    }
    let Ok(client) = Client::connect(&paths.socket).await else {
        return;
    };
    let Ok(flock) = client.list_flock().await else {
        return;
    };
    if let Some(shown) = next(paths, &dog_index::index_url(), &flock) {
        let _ = writeln!(streams.out, "{}", shown.line);
    }
}
