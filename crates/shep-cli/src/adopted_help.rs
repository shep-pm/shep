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

use shep_core::dogs::manifest::{Subcommand, Verb};
use shep_core::paths::ShepPaths;

use crate::cli::HELP_TEMPLATE;
use crate::commands::dogs::{cached_manifest, collides_with_a_verb};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::{cli, home};

/// The `HELP_TEMPLATE` group line adopted dogs join, and the heading
/// `cli::help`'s `HELP_GROUPS` files `dogs`, `adopt` and `whistle` under.
const DOG_GROUP_HEADING: &str = "Dogs and agents";

/// The column every group line's verbs start at, in `HELP_TEMPLATE`: the
/// longest heading plus two spaces.
///
/// A literal rather than a computed one because the template is hand-written
/// copy and this is its layout, not data derived from it.
/// `the_dog_lines_line_up_under_the_group_verbs` pins it against the real
/// template.
const VERB_COLUMN: usize = 17;

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
    verbs.sort_by(|a, b| a.name.cmp(&b.name));
    verbs
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
/// subcommand whose name is not one word, and a verb that declares nothing
/// once its description is gone.
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
            Some(Subcommand::new(name, printable(&sub.about)))
        })
        .collect();
    if cleaned.description.is_empty() && cleaned.subcommands.is_empty() {
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

/// Render `verbs` into the Dogs group of `command`'s help template.
///
/// Does nothing without dogs, so a home with none renders `HELP_TEMPLATE` as is.
///
/// # Panics
///
/// Panics, naming the heading, when `HELP_TEMPLATE` has lost `Dogs and agents`.
#[track_caller]
pub(crate) fn list_in_help(command: &mut clap::Command, verbs: &[AdoptedVerb]) {
    if verbs.is_empty() {
        return;
    }

    let mut lines: Vec<String> = HELP_TEMPLATE.lines().map(str::to_string).collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with(DOG_GROUP_HEADING))
        .expect("HELP_TEMPLATE has a Dogs group");
    lines.splice(at + 1..at + 1, dog_lines(verbs));
    *command = std::mem::take(command).help_template(lines.join("\n"));
}

/// The lines the dogs get under the group heading: each name in the column
/// every group's verbs sit in, each description two columns past the longest
/// name. A dog with no description gets its name and nothing else.
///
/// Columns, not bytes: `café` is five bytes and four columns, `日本語` is
/// nine and six, and this is a table an operator reads.
fn dog_lines(verbs: &[AdoptedVerb]) -> Vec<String> {
    let widest = verbs
        .iter()
        .map(|verb| crate::output::width::text_columns(&verb.name))
        .max();
    verbs
        .iter()
        .map(|verb| {
            let mut line = " ".repeat(VERB_COLUMN);
            line.push_str(&verb.name);
            if let Some(description) = &verb.description {
                let name_columns = crate::output::width::text_columns(&verb.name);
                let pad = widest.unwrap_or(0) + 2 - name_columns;
                line.push_str(&" ".repeat(pad));
                line.push_str(description);
            }
            line
        })
        .collect()
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

/// `shep <dog> --help`, rendered from the manifest.
///
/// `None` for every other invocation, and for a dog that declares no
/// subcommands: `--help` reaches those dogs' own binaries, which is what a
/// dog that writes its own help gets.
pub(crate) fn describe_dog(
    name: &str,
    path: &Path,
    home: &Path,
    argv: &[OsString],
) -> Option<ExitCode> {
    if !matches!(argv, [flag] if flag == "--help" || flag == "-h") {
        return None;
    }
    let verb = cached_manifest(path, home)
        .and_then(|manifest| manifest.verb)
        .and_then(printable_verb)?;
    if verb.subcommands.is_empty() {
        return None;
    }
    // A failed write is a closed stdout, and there is nothing left to say
    // about it: `shep <dog> --help | head` is the ordinary reason.
    let _ = dog_help(name, &verb).print_long_help();
    Some(ExitCode::Success)
}

/// The help page for one dog's verb: its description, its declared
/// subcommands, and the one fact a manifest cannot state, that everything
/// else after the name goes to the dog.
fn dog_help(name: &str, verb: &Verb) -> clap::Command {
    let command = clap::Command::new(name.to_owned())
        .bin_name(format!("shep {name}"))
        .disable_help_subcommand(true)
        .after_help(format!(
            "Anything else after `shep {name}` goes to the dog unchanged."
        ));
    // A verb can declare subcommands and no description, and clap prints an
    // empty about as a blank line above the usage.
    let command = if verb.description.is_empty() {
        command
    } else {
        command.about(verb.description.clone())
    };
    verb.subcommands.iter().fold(command, |command, sub| {
        command.subcommand(clap::Command::new(sub.name.clone()).about(sub.about.clone()))
    })
}

/// [`dog_help`] rendered, for the tests that read it rather than print it.
#[cfg(test)]
pub(crate) fn dog_help_text(name: &str, verb: &Verb) -> String {
    dog_help(name, verb).render_long_help().to_string()
}

#[cfg(all(test, unix))]
mod tests;
