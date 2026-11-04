//! A dog's manifest [`Verb`], read off the clap tree it already has.
//!
//! [`Verb::from_clap`] is the shorthand for a Rust dog: the about becomes the
//! description, every visible subcommand becomes a [`Subcommand`], and the
//! `Examples:` block of the command's `after_help` becomes the examples. A dog
//! that builds its verb by hand, or is written in another language, declares
//! the JSON itself.

use shep_core::dogs::manifest::{MAX_EXAMPLES, Subcommand, Verb};

/// The heading [`Verb::from_clap`] reads examples under.
const EXAMPLES_HEADING: &str = "Examples:";

/// What every example line under it starts with. shep prints the same prefix.
const EXAMPLE_PREFIX: &str = "$ ";

/// A [`Verb`] read off a `clap` command tree.
///
/// Apply it to [`Verb`] and call [`Verb::from_clap`].
pub trait FromClap: Sized {
    /// The verb `command` describes.
    ///
    /// The description is the command's about. Each subcommand clap lists is
    /// a [`Subcommand`] with its name, its about, and the usage the command
    /// overrides, which is the words shep prints after the name. A hidden
    /// subcommand is left out.
    ///
    /// The examples are the block `after_help` carries under `Examples:`,
    /// one command line to a line, each starting `$ `:
    ///
    /// ```text
    /// Examples:
    ///   $ shep kelpie board
    /// ```
    ///
    /// shep stores each without the prefix. A block ends at the first line
    /// that is not an example.
    ///
    /// # Example
    ///
    /// ```
    /// use clap::Command;
    /// use shep_client::dogs::{FromClap as _, Verb};
    ///
    /// let command = Command::new("shep-kelpie")
    ///     .about("Run agents on a ticket board")
    ///     .subcommand(Command::new("board").about("Show the board"))
    ///     .after_help("Examples:\n  $ shep kelpie board");
    /// let verb = Verb::from_clap(&command).expect("three examples at most");
    ///
    /// assert_eq!(verb.description, "Run agents on a ticket board");
    /// assert_eq!(verb.examples, ["shep kelpie board"]);
    /// ```
    ///
    /// # Errors
    ///
    /// [`VerbClapError::TooManyExamples`] when the block holds more than
    /// [`MAX_EXAMPLES`] command lines.
    fn from_clap(command: &clap::Command) -> Result<Self, VerbClapError>;
}

/// Why a clap tree was not read as a [`Verb`].
///
/// `#[non_exhaustive]`: a later slice adds what else a tree can get wrong, and
/// a dog matching on this only ever reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerbClapError {
    /// The `Examples:` block holds `examples` command lines, and a manifest
    /// declares at most [`MAX_EXAMPLES`].
    TooManyExamples {
        /// The name of the command the block belongs to.
        command: String,
        /// How many command lines it holds.
        examples: usize,
    },
}

impl core::fmt::Display for VerbClapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyExamples { command, examples } => write!(
                f,
                "`{command}` carries {examples} examples, and a manifest declares at most \
                 {MAX_EXAMPLES}"
            ),
        }
    }
}

impl core::error::Error for VerbClapError {}

impl FromClap for Verb {
    fn from_clap(command: &clap::Command) -> Result<Self, VerbClapError> {
        let examples = examples(command);
        if examples.len() > MAX_EXAMPLES {
            return Err(VerbClapError::TooManyExamples {
                command: command.get_name().to_owned(),
                examples: examples.len(),
            });
        }
        let mut verb = Verb::new(about(command)).with_examples(examples);
        verb.subcommands = command
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| match sub.get_overridden_usage() {
                Some(usage) => Subcommand::new(sub.get_name(), about(sub))
                    .with_usage(usage.to_string().trim().to_owned()),
                None => Subcommand::new(sub.get_name(), about(sub)),
            })
            .collect();
        Ok(verb)
    }
}

/// `command`'s about, or nothing when it has none.
fn about(command: &clap::Command) -> String {
    command
        .get_about()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// The `Examples:` block of `command`'s `after_help`, each line without its
/// `$ ` prefix. Empty when there is no block.
fn examples(command: &clap::Command) -> Vec<String> {
    let Some(after) = command.get_after_help() else {
        return Vec::new();
    };
    let mut under_heading = false;
    let mut lines = Vec::new();
    for line in after.to_string().lines().map(str::trim) {
        if !under_heading {
            under_heading = line == EXAMPLES_HEADING;
            continue;
        }
        let Some(example) = line.strip_prefix(EXAMPLE_PREFIX) else {
            break;
        };
        lines.push(example.trim().to_owned());
    }
    lines
}

#[cfg(test)]
mod tests {
    use clap::Command;

    use super::*;

    #[test]
    fn a_clap_tree_becomes_a_verb() {
        let command = Command::new("shep-kelpie")
            .about("Run agents on a ticket board")
            .subcommand(Command::new("board").about("Show the board"))
            .subcommand(
                Command::new("rule")
                    .about("Answer a ruling")
                    .override_usage("<id> yes|no <note>"),
            )
            .subcommand(Command::new("draft").about("Draft a card").hide(true))
            .after_help(
                "Examples:\n  $ shep kelpie board\n  $ shep kelpie rule 42 yes looks good\n\
                 Anything after `shep kelpie` goes to the dog.",
            );

        let verb = Verb::from_clap(&command).unwrap();

        assert_eq!(verb.description, "Run agents on a ticket board");
        assert_eq!(
            verb.subcommands,
            vec![
                Subcommand::new("board", "Show the board"),
                Subcommand::new("rule", "Answer a ruling").with_usage("<id> yes|no <note>"),
            ]
        );
        assert_eq!(
            verb.examples,
            ["shep kelpie board", "shep kelpie rule 42 yes looks good"]
        );
    }

    #[test]
    fn four_examples_are_refused() {
        let command = Command::new("shep-otel")
            .about("Export metrics")
            .after_help("Examples:\n  $ a\n  $ b\n  $ c\n  $ d");

        let refused = Verb::from_clap(&command).expect_err("four examples is more than three");

        assert_eq!(
            refused,
            VerbClapError::TooManyExamples {
                command: "shep-otel".to_owned(),
                examples: 4,
            }
        );
        assert_eq!(
            refused.to_string(),
            "`shep-otel` carries 4 examples, and a manifest declares at most 3"
        );
    }

    #[test]
    fn a_tree_with_no_examples_declares_none() {
        let verb = Verb::from_clap(&Command::new("shep-otel").about("Export metrics")).unwrap();

        assert!(verb.examples.is_empty());
        assert!(verb.subcommands.is_empty());
    }
}
