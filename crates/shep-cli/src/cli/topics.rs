//! The help topics: prose several verbs share, printed once by `shep help
//! <topic>` so a verb's page can point at it.

use std::ffi::OsString;

use crate::exit::ExitCode;

/// One help topic.
pub(crate) struct Topic {
    /// What follows `shep help`.
    pub(crate) name: &'static str,
    /// One line for a listing of the topics.
    pub(crate) about: &'static str,
    /// The page, as a command so the caller applies help colour to it.
    pub(crate) page: fn() -> clap::Command,
}

/// Every topic, in the order a listing shows them.
pub(crate) const TOPICS: &[Topic] = &[
    Topic {
        name: "flags",
        about: "every global flag, in full",
        page: super::global_help::flags_topic,
    },
    Topic {
        name: "selectors",
        about: "names, globs, /regex/ and fold:<name>",
        page: selectors,
    },
    Topic {
        name: "exit-codes",
        about: "what each exit code means",
        page: exit_codes,
    },
];

/// The topic `argv` asks for: `shep help <topic>` and nothing more.
pub(crate) fn asked(argv: &[OsString]) -> Option<&'static Topic> {
    match argv {
        [_, help, name] if help == "help" => TOPICS.iter().find(|topic| name == topic.name),
        _ => None,
    }
}

/// A page of `text`, printed as written: no usage line, no re-wrapping.
fn plain(text: String) -> clap::Command {
    clap::Command::new("shep")
        .bin_name("shep")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .override_help(text)
}

/// What a selector is, for every verb that takes one.
fn selectors() -> clap::Command {
    plain(
        "\
Selectors, for every verb that acts on part of the flock:

  name           one sheep, exact: web.1 is the sheep called web.1
  id             one sheep, by its id
  name:slot      one slot of a sheep with several instances
  all            every sheep
  api-*          a glob, anchored
  /regex/        a regular expression
  fold:<name>    every sheep in that fold

A pattern carrying *, ?, [ or { is a glob, anchored: api-* selects api-auth
and not my-api-auth. Quote it. Your shell expands api-* against filenames
first, and zsh refuses outright when none match. A name with no such
character is exact.

Give several and they are applied in turn, not atomically: shep stop a b c,
where b matches nothing, still stops a and c, and the exit code is the first
failure.
"
        .to_owned(),
    )
}

/// Each exit code with its number, from [`ExitCode::ALL`].
fn exit_codes() -> clap::Command {
    let mut page = String::from("Exit codes, the same on every verb:\n\n");
    for code in ExitCode::ALL {
        page.push_str(&format!("  {:<3}{}\n", *code as u8, code.summary()));
    }
    plain(page)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::cli::{Cli, with_global_footer};

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// The text of a page with every run of whitespace as one space.
    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn text(topic: &Topic) -> String {
        (topic.page)().render_long_help().to_string()
    }

    #[test]
    fn every_topic_answers_shep_help_topic() {
        assert!(!TOPICS.is_empty());
        for topic in TOPICS {
            let found = asked(&argv(&["shep", "help", topic.name]))
                .unwrap_or_else(|| panic!("shep help {} is not answered", topic.name));
            assert_eq!(found.name, topic.name);
            assert!(!text(found).trim().is_empty(), "{} is empty", topic.name);
        }
        let names: Vec<&str> = TOPICS.iter().map(|topic| topic.name).collect();
        assert_eq!(names, ["flags", "selectors", "exit-codes"]);
    }

    #[test]
    fn a_topic_is_asked_for_by_help_and_its_name_alone() {
        assert!(asked(&argv(&["shep", "help"])).is_none());
        assert!(asked(&argv(&["shep", "help", "stop"])).is_none());
        assert!(asked(&argv(&["shep", "flags"])).is_none());
        assert!(asked(&argv(&["shep", "help", "flags", "x"])).is_none());
    }

    #[test]
    fn exit_codes_lists_every_exit_code_with_its_number() {
        let topic = TOPICS.iter().find(|t| t.name == "exit-codes").unwrap();
        let page = text(topic);
        let lines: Vec<&str> = page.lines().map(str::trim).collect();
        let all = [
            (ExitCode::Success, "0"),
            (ExitCode::Failure, "1"),
            (ExitCode::Usage, "2"),
            (ExitCode::NotFound, "3"),
            (ExitCode::InvalidConfig, "4"),
            (ExitCode::DaemonUnreachable, "5"),
            (ExitCode::ProtocolMismatch, "6"),
            (ExitCode::SpawnFailed, "7"),
            (ExitCode::DeadlineExceeded, "8"),
            (ExitCode::Internal, "9"),
            (ExitCode::DaemonAlreadyRunning, "10"),
            (ExitCode::FlockEmpty, "11"),
            (ExitCode::VersionSkew, "12"),
            (ExitCode::Unsupported, "13"),
        ];
        assert_eq!(ExitCode::ALL.len(), all.len(), "a variant is not listed");
        for (code, number) in all {
            assert_eq!(code as u8, number.parse::<u8>().unwrap());
            assert!(
                lines.iter().any(|l| l
                    .strip_prefix(number)
                    .is_some_and(|rest| rest.starts_with(' ') && rest.trim().len() > 10)),
                "exit {number} ({code:?}) has no line in:\n{page}"
            );
        }
    }

    #[test]
    fn the_selector_grammar_lives_in_its_topic_not_on_each_verb() {
        let mut command = with_global_footer(Cli::command());
        command.build();
        for verb in ["stop", "restart", "reset", "reload", "delete", "describe"] {
            let page = flat(
                &command
                    .find_subcommand(verb)
                    .unwrap()
                    .clone()
                    .render_long_help()
                    .to_string(),
            );
            assert!(page.contains("See shep help selectors"), "{verb}: {page}");
            assert!(!page.contains("anchored"), "{verb}: {page}");
        }
        let topic = TOPICS.iter().find(|t| t.name == "selectors").unwrap();
        let page = flat(&text(topic));
        assert!(page.contains("anchored"), "{page}");
        assert!(page.contains("not atomically"), "{page}");
    }

    #[test]
    fn no_topic_carries_an_em_dash() {
        for topic in TOPICS {
            for page in [topic.about.to_owned(), text(topic)] {
                assert!(!page.contains('\u{2014}'), "{}: {page}", topic.name);
            }
        }
    }
}
