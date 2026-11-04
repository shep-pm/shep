//! Adopted dogs as part of shep's own help and completion surface.
//!
//! A dog's verb is `shep <name>`, and the registry in `shep.toml` decides
//! which names are dogs. [`crate::adopted`] runs one only for a token clap
//! could not place. A manifest's `verb` says how that name is described.
//! `description` is the line `shep --help` lists. `subcommands` are what
//! `shep <dog> --help` prints and a completion script offers.
//!
//! Every read goes to the manifest cache `shep adopt` wrote, never to the
//! binary, so a help screen never spawns a dog.

use std::ffi::OsString;
use std::path::Path;

use shep_core::dogs::manifest::{MAX_EXAMPLES, Subcommand, Verb};
use shep_core::paths::ShepPaths;

use crate::cli::{DogLine, top_level_template};
use crate::commands::dogs::{cached_manifest, collides_with_a_verb, installed};
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{self, Index};
use crate::exit::ExitCode;
use crate::{cli, home};

/// One adopted dog, as `--help` and a completion script need it.
///
/// Every string went through [`printable`]: one line, no control characters,
/// no clap placeholder braces.
///
/// `Debug` needs no redaction: the manifest's `defaults`, which can carry a
/// credential, never reach this type.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdoptedVerb {
    /// The name the operator types: `shep <name>`.
    pub name: String,
    /// The manifest's `verb.description`. `None` for a dog that declared
    /// none, which is every dog adopted before manifests existed.
    pub description: Option<String>,
    /// The manifest's `verb.subcommands`, empty for a dog that declared none.
    pub subcommands: Vec<Subcommand>,
}

/// Every adopted dog in name order, with what its cached manifest declares.
///
/// A `shep.toml` that cannot be read or parsed lists nothing, so help still
/// prints. A name a verb or alias owns is left out, since clap's verb wins
/// the token. A name [`printable`] would rewrite is left out too.
#[must_use]
pub(crate) fn adopted_verbs(paths: &ShepPaths) -> Vec<AdoptedVerb> {
    adopted_verbs_in(paths, &dog_index::index_url())
}

/// [`adopted_verbs`] with the cached index read for `index_url`.
///
/// A dog whose manifest declares no description is described by its row in
/// that cached index when it has one. The index is never fetched.
fn adopted_verbs_in(paths: &ShepPaths, index_url: &str) -> Vec<AdoptedVerb> {
    let Ok(config) = ShepToml::read_only(&paths.daemon_config) else {
        return Vec::new();
    };

    let mut verbs: Vec<AdoptedVerb> = config
        .adopted_dog_names()
        .into_iter()
        .filter(|name| !collides_with_a_verb(name))
        .filter(|name| printable(name) == *name)
        .map(|name| {
            let verb = config
                .adopted_dog_path(&name)
                .as_deref()
                .and_then(|path| cached_manifest(path, &paths.home))
                .and_then(|manifest| manifest.verb)
                .and_then(printable_verb);
            AdoptedVerb {
                description: verb
                    .as_ref()
                    .map(|verb| verb.description.clone())
                    .filter(|description| !description.is_empty()),
                subcommands: verb.map_or_else(Vec::new, |verb| verb.subcommands),
                name,
            }
        })
        .collect();
    if verbs.iter().any(|verb| verb.description.is_none())
        && let Some(index) = dog_index::cached_only(paths, index_url)
    {
        for verb in verbs.iter_mut().filter(|verb| verb.description.is_none()) {
            let receipt = installed(paths, &verb.name);
            let package = receipt.as_ref().map(|receipt| receipt.package.as_str());
            verb.description = indexed_description(&index, &verb.name, package);
        }
    }
    verbs.sort_by(|a, b| a.name.cmp(&b.name));
    verbs
}

/// What the cached `index` says dog `name` does, or `None` when it has no
/// row for it or the row says nothing.
///
/// The row is the dog's `package` when it was installed, and no other: a
/// package the index dropped has no description. A dog with no receipt gets
/// the row that expects to be adopted under `name`.
fn indexed_description(index: &Index, name: &str, package: Option<&str>) -> Option<String> {
    let row = match package {
        Some(package) => index.dogs.iter().find(|dog| dog.package == package)?,
        None => index.dogs.iter().find(|dog| dog.adopt_as == name)?,
    };
    Some(printable(&row.description)).filter(|description| !description.is_empty())
}

/// One string from a manifest, fit for a help line or a completion script.
///
/// [`crate::terminal_safe::sanitise`] first, then the placeholder braces go.
/// clap's help template has no escape for a literal `{`, and a stray one
/// swallows the rest of the template.
fn printable(field: &str) -> String {
    let (cleaned, _) = crate::terminal_safe::sanitise(field);
    cleaned.replace(['{', '}'], "")
}

/// The manifest's `verb` with every string shep prints run through
/// [`printable`], and the pieces a cleaned string cannot be dropped: a
/// subcommand whose name is not one word, an example that cleaned to nothing,
/// and a verb that declares nothing once its copy is gone.
fn printable_verb(verb: Verb) -> Option<Verb> {
    let mut cleaned = Verb::new(printable(&verb.description));
    cleaned.subcommands = verb
        .subcommands
        .into_iter()
        .filter_map(|sub| {
            let name = printable(&sub.name);
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                return None;
            }
            let cleaned = Subcommand::new(name, printable(&sub.about));
            Some(match sub.usage.map(|usage| printable(&usage)) {
                Some(usage) if !usage.is_empty() => cleaned.with_usage(usage),
                _ => cleaned,
            })
        })
        .collect();
    cleaned.examples = verb
        .examples
        .iter()
        .map(|example| printable(example))
        .filter(|example| !example.is_empty())
        .take(MAX_EXAMPLES)
        .collect();
    if cleaned.description.is_empty()
        && cleaned.subcommands.is_empty()
        && cleaned.examples.is_empty()
    {
        return None;
    }
    Some(cleaned)
}

/// Whether this argv can want the top-level help.
///
/// A token scan, not a parse: building the listing reads `shep.toml` and
/// hashes every adopted binary, and `shep stop web` must not pay that. It
/// answers `true` for a `--help` aimed at one verb, which costs but is harmless.
#[must_use]
pub(crate) fn top_level_help_asked(argv: &[OsString]) -> bool {
    argv.len() == 1
        || argv[1..]
            .iter()
            .any(|arg| arg == "-h" || arg == "--help" || arg == "help")
}

/// The `$SHEP_HOME` this argv names, for an invocation clap never parsed to
/// the end.
///
/// `None` when the home cannot be resolved at all, which is an ordinary
/// answer here and never a refusal: `--help` prints whatever it would have
/// printed.
pub(crate) fn paths_from_argv(argv: &[OsString]) -> Option<ShepPaths> {
    let global = cli::GlobalArgs {
        host: None,
        home: crate::adopted::home_before(&argv[1..]),
        format: cli::Format::Table,
        quiet: false,
        style: None,
    };
    home::resolve_paths(&global).ok()
}

/// Render `verbs` into the Dogs group of `command`'s `--help` listing,
/// painted at `deep`'s colour depth.
pub(crate) fn list_in_help(command: &mut clap::Command, verbs: &[AdoptedVerb], deep: bool) {
    let dogs: Vec<DogLine<'_>> = verbs
        .iter()
        .map(|verb| DogLine {
            name: &verb.name,
            about: verb.description.as_deref(),
        })
        .collect();
    let template = top_level_template(command, &dogs, deep);
    *command = std::mem::take(command).help_template(template);
}

/// Whether `name` is a word a shell takes as a bare word.
///
/// `clap_complete` 4.6.9 writes subcommand names into scripts unquoted, so
/// `$(touch>x)` runs when the script completes. An unsafe name is dropped
/// from the script, not rewritten. Letters of any script and digits,
/// separated by `-`, `_` or `.`, starting on a letter or digit.
fn shell_safe(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_alphanumeric() && chars.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Add each dog and its declared subcommands to a tree built for completion.
///
/// The tree is never parsed, so a dog's argv stays a pass-through and a
/// manifest cannot claim a verb. A name [`shell_safe`] rejects is left out
/// of this tree alone: the dog stays in `shep --help` and still runs.
pub(crate) fn with_completion_entries(
    command: clap::Command,
    verbs: &[AdoptedVerb],
) -> clap::Command {
    verbs
        .iter()
        .filter(|verb| shell_safe(&verb.name))
        .fold(command, |command, verb| {
            command.subcommand(completion_entry(verb))
        })
}

/// The completion tree's entry for one dog.
fn completion_entry(verb: &AdoptedVerb) -> clap::Command {
    let mut command = clap::Command::new(verb.name.clone())
        .about(verb.description.clone().unwrap_or_default())
        .disable_help_subcommand(true);
    for sub in &verb.subcommands {
        if !shell_safe(&sub.name) {
            continue;
        }
        command = command.subcommand(clap::Command::new(sub.name.clone()).about(sub.about.clone()));
    }
    command
}

/// The page for `shep <dog> --help`, drawn when this invocation asks for it.
///
/// `before` is the argv between `shep` and the dog's name, `argv` what follows
/// the name. `None` for every other invocation: those reach the dog.
pub(crate) fn describe_dog(
    name: &str,
    path: &Path,
    paths: &ShepPaths,
    before: &[OsString],
    argv: &[OsString],
) -> Option<ExitCode> {
    if !help_asked(before, argv) {
        return None;
    }
    // A failed write is a closed stdout, and there is nothing left to say
    // about it: `shep <dog> --help | head` is the ordinary reason.
    let _ = dog_page(name, path, paths, &dog_index::index_url()).print_long_help();
    Some(ExitCode::Success)
}

/// Whether this invocation asks shep for a dog's page: `shep <dog> --help`,
/// `shep <dog> -h` or `shep help <dog>`, and nothing more.
///
/// `shep <dog> help` and `shep <dog> <sub> --help` are the dog's to answer.
fn help_asked(before: &[OsString], argv: &[OsString]) -> bool {
    match argv {
        [flag] => flag == "--help" || flag == "-h",
        [] => help_verb_precedes(before),
        _ => false,
    }
}

/// Whether the one verb before the dog's name is `help`.
///
/// A global flag and its value are skipped, so `--home help help otel` reads
/// `help` as the verb and not as the home.
fn help_verb_precedes(before: &[OsString]) -> bool {
    let mut words = Vec::new();
    let mut tokens = before.iter();
    while let Some(token) = tokens.next() {
        if ["--home", "--host", "--format", "--style"]
            .iter()
            .any(|flag| token == flag)
        {
            tokens.next();
        } else if !token.to_string_lossy().starts_with('-') {
            words.push(token);
        }
    }
    matches!(words.as_slice(), [word] if *word == "help")
}

/// The help page for the dog `name` adopted at `path`, drawn from its cached
/// manifest and its receipt and never from the binary.
fn dog_page(name: &str, path: &Path, paths: &ShepPaths, index_url: &str) -> clap::Command {
    let manifest = cached_manifest(path, &paths.home);
    let receipt = installed(paths, name);
    let running = manifest.as_ref().map(|manifest| manifest.version.as_str());
    let recorded = receipt
        .as_ref()
        .and_then(|receipt| receipt.version.as_deref());
    let version = running.or(recorded).map(printable);
    // A receipt describes the binary it installed. When the binary now
    // registered reports another version, the receipt's origin is not its own.
    let source = receipt
        .as_ref()
        .filter(|_| running.is_none() || recorded.is_none() || running == recorded)
        .map(|receipt| printable(&receipt.source()));
    let verb = manifest
        .and_then(|manifest| manifest.verb)
        .and_then(printable_verb);
    match verb {
        Some(verb) => {
            let who = [
                Some(name.to_owned()),
                version,
                source.map(|source| format!("from {source}")),
            ];
            let who = who.into_iter().flatten().collect::<Vec<_>>().join(" ");
            dog_help(
                name,
                &verb,
                &format!("{who}. Anything after `shep {name}` goes to the dog."),
            )
        }
        None => {
            let description = dog_index::cached_only(paths, index_url).and_then(|index| {
                let package = receipt.as_ref().map(|receipt| receipt.package.as_str());
                indexed_description(&index, name, package)
            });
            stub(name, version.as_deref(), description.as_deref())
        }
    }
}

/// The page for a dog that declares no help: who it is, what the index says
/// it does when there is a cached row, and how to ask the dog itself.
fn stub(name: &str, version: Option<&str>, description: Option<&str>) -> clap::Command {
    let who = version.map_or_else(|| name.to_owned(), |version| format!("{name} {version}"));
    let described = description.map_or_else(String::new, |text| format!("\n{text}"));
    let text = format!(
        "{who}{described}\n\nThis dog declares no help. `shep {name} help` asks the dog itself."
    );
    clap::Command::new(name.to_owned())
        .disable_help_flag(true)
        .help_template("{about}")
        .about(text)
}

/// The help page for one dog's verb: its description, its declared
/// subcommands with their usage, its examples, and `closing`, the one line a
/// manifest cannot state.
fn dog_help(name: &str, verb: &Verb, closing: &str) -> clap::Command {
    let usage = if verb.subcommands.is_empty() {
        format!("shep {name} [args]")
    } else {
        format!("shep {name} <command> [args]")
    };
    let examples = shep_client::dogs::render_examples(&verb.examples);
    let after = if examples.is_empty() {
        closing.to_owned()
    } else {
        format!("{examples}\n\n{closing}")
    };
    let command = clap::Command::new(name.to_owned())
        .bin_name(format!("shep {name}"))
        .override_usage(usage)
        .disable_help_subcommand(true)
        .disable_help_flag(true)
        .after_help(after);
    // A verb can declare subcommands and no description, and clap prints an
    // empty about as a blank line above the usage.
    let command = if verb.description.is_empty() {
        command
    } else {
        command.about(verb.description.clone())
    };
    verb.subcommands.iter().fold(command, |command, sub| {
        let label = match &sub.usage {
            Some(usage) => format!("{} {usage}", sub.name),
            None => sub.name.clone(),
        };
        command.subcommand(clap::Command::new(label).about(sub.about.clone()))
    })
}

/// [`dog_help`] rendered, for the tests that read it rather than print it.
#[cfg(test)]
pub(crate) fn dog_help_text(name: &str, verb: &Verb) -> String {
    dog_help(name, verb, "").render_long_help().to_string()
}

#[cfg(all(test, unix))]
mod tests;
