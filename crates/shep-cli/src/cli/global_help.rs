//! Where the global flags show in help.
//!
//! A verb's help shows the verb. The five global flags are hidden from every
//! `--help` and `-h` by their own attributes in [`super::GlobalArgs`], and
//! each verb ends in one footer line naming the ones it takes. `shep help
//! flags` prints each flag's long text once.

/// The global flags, as the command tree declares them.
fn global_args() -> Vec<clap::Arg> {
    use clap::CommandFactory;
    super::Cli::command()
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .cloned()
        .collect()
}

/// The footer for a verb: the flags it takes, then where the rest is.
fn footer(flags: &[String], takes_host: bool) -> String {
    let names: Vec<&str> = flags
        .iter()
        .filter(|name| takes_host || *name != "--host")
        .map(String::as_str)
        .collect();
    format!("Global flags: {}. See shep help flags", names.join(", "))
}

/// `after` with `footer` under it, a blank line between.
fn with_footer(after: Option<&clap::builder::StyledStr>, footer: &str) -> String {
    match after {
        Some(text) => format!("{text}\n\n{footer}"),
        None => footer.to_owned(),
    }
}

/// `command` with the global-flags footer on every subcommand, at any depth.
///
/// `--host` is named only under a verb [`crate::remote::takes_host`] says
/// takes it.
pub(crate) fn with_global_footer(command: clap::Command) -> clap::Command {
    let flags: Vec<String> = global_args()
        .iter()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .collect();
    each_verb(command, &|verb| {
        let takes_host = crate::remote::takes_host(verb.get_name());
        footed(verb, &flags, takes_host)
    })
}

/// `command` with `change` applied to each of its direct subcommands.
///
/// By name, through `mut_subcommand`: `mut_subcommands` is newer than the
/// clap the minimal-versions job resolves.
fn each_verb(
    command: clap::Command,
    change: &dyn Fn(clap::Command) -> clap::Command,
) -> clap::Command {
    let names: Vec<String> = command
        .get_subcommands()
        .map(|verb| verb.get_name().to_owned())
        .collect();
    names.into_iter().fold(command, |command, name| {
        command.mut_subcommand(name, change)
    })
}

fn footed(command: clap::Command, flags: &[String], takes_host: bool) -> clap::Command {
    let footer = footer(flags, takes_host);
    let short = with_footer(command.get_after_help(), &footer);
    let long = with_footer(
        command
            .get_after_long_help()
            .or_else(|| command.get_after_help()),
        &footer,
    );
    // The generated `-V` is replaced by the same flag, unlisted: it is one more
    // line on every page, and `shep --version` is where it is read.
    let version = clap::Arg::new("version")
        .short('V')
        .long("version")
        .action(clap::ArgAction::Version)
        .hide_short_help(true)
        .hide_long_help(true);
    each_verb(command.disable_version_flag(true).arg(version), &|sub| {
        footed(sub, flags, takes_host)
    })
    .after_help(short)
    .after_long_help(long)
}

/// The topic `shep help flags` prints: each global flag, once, with its long
/// text.
///
/// Rendered from the same arguments the parser holds, shown again where a
/// verb's help hides them.
pub(super) fn flags_topic() -> clap::Command {
    global_args().into_iter().fold(
        clap::Command::new("shep")
            .bin_name("shep")
            .disable_help_flag(true)
            .disable_version_flag(true)
            .help_template("Global flags, valid on every verb:\n\n{options}"),
        |command, arg| {
            command.arg(
                arg.global(false)
                    .hide_short_help(false)
                    .hide_long_help(false),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use clap::{Args, CommandFactory};

    use super::*;
    use crate::cli::{Cli, GlobalArgs};

    /// The text with every run of whitespace as one space, so a wrapped line
    /// still matches.
    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The built, footed command tree.
    fn footed() -> clap::Command {
        let mut command = with_global_footer(Cli::command());
        command.build();
        command
    }

    /// Every command path in the tree, the root as an empty one.
    fn all_paths(command: &clap::Command) -> Vec<Vec<String>> {
        fn walk(command: &clap::Command, prefix: &[String], out: &mut Vec<Vec<String>>) {
            for sub in command.get_subcommands() {
                if sub.get_name() == "help" {
                    continue;
                }
                let mut path = prefix.to_vec();
                path.push(sub.get_name().to_owned());
                out.push(path.clone());
                walk(sub, &path, out);
            }
        }
        let mut out = vec![Vec::new()];
        walk(command, &[], &mut out);
        out
    }

    fn at<'a>(root: &'a clap::Command, path: &[String]) -> &'a clap::Command {
        path.iter().fold(root, |command, name| {
            command.find_subcommand(name).expect("path names a verb")
        })
    }

    /// What each of `GlobalArgs`' flags says that a verb's help must not: its
    /// long help, or its one-line help when it has none. Read from the struct
    /// itself, not from the command tree.
    fn long_texts() -> Vec<(String, Vec<String>)> {
        let command = GlobalArgs::augment_args(clap::Command::new("probe"));
        command
            .get_arguments()
            .map(|arg| {
                let texts: Vec<String> = arg
                    .get_long_help()
                    .or_else(|| arg.get_help())
                    .map(|text| flat(&text.to_string()))
                    .into_iter()
                    .collect();
                (arg.get_id().to_string(), texts)
            })
            .collect()
    }

    #[test]
    fn the_global_flags_are_the_five_the_issue_names() {
        let ids: Vec<String> = long_texts().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["format", "quiet", "style", "host", "home"]);
    }

    /// Fails when a global flag's long text reaches a verb's help again, in
    /// either the `-h` or the `--help` rendering.
    #[test]
    fn no_help_page_carries_a_global_flags_long_text() {
        let root = footed();
        for path in all_paths(&root) {
            let command = at(&root, &path);
            for (kind, rendered) in [
                ("-h", command.clone().render_help().to_string()),
                ("--help", command.clone().render_long_help().to_string()),
            ] {
                let page = flat(&rendered);
                for (id, texts) in long_texts() {
                    for text in texts {
                        assert!(
                            !page.contains(&text),
                            "`shep {} {kind}` carries --{id}'s text {text:?}:\n{rendered}",
                            path.join(" ")
                        );
                    }
                    // The top level lists them, one line each, on purpose.
                    let lists_it = !path.is_empty()
                        && rendered.lines().any(|line| {
                            let mut words = line.split_whitespace().take(2);
                            words.any(|word| word.trim_end_matches(',') == format!("--{id}"))
                        });
                    assert!(
                        !lists_it,
                        "`shep {} {kind}` lists --{id} as an option:\n{rendered}",
                        path.join(" ")
                    );
                }
            }
        }
    }

    #[test]
    fn a_verbs_help_ends_in_one_footer_line_naming_its_global_flags() {
        let root = footed();
        let stop = at(&root, &["stop".to_owned()])
            .clone()
            .render_help()
            .to_string();
        assert!(
            stop.trim_end().ends_with(
                "Global flags: --format, --quiet, --style, --host, --home. See shep help flags"
            ),
            "got:\n{stop}"
        );
        let ping = at(&root, &["ping".to_owned()])
            .clone()
            .render_help()
            .to_string();
        assert!(
            ping.trim_end()
                .ends_with("Global flags: --format, --quiet, --style, --home. See shep help flags"),
            "got:\n{ping}"
        );
    }

    #[test]
    fn a_nested_verb_gets_the_footer_and_keeps_its_own_after_help() {
        let root = footed();
        let startup = at(&root, &["startup".to_owned()])
            .clone()
            .render_long_help()
            .to_string();
        assert!(
            startup.contains("On Windows this registers"),
            "got:\n{startup}"
        );
        assert!(
            startup.contains("Global flags: --format"),
            "got:\n{startup}"
        );
        let grant = at(&root, &["dogs".to_owned(), "grant".to_owned()])
            .clone()
            .render_help()
            .to_string();
        assert!(grant.contains("Global flags: --format"), "got:\n{grant}");
    }

    /// Hiding `-V` from a page must not take it off the verb.
    #[test]
    fn a_verb_still_answers_dash_capital_v() {
        for (flag, wants) in [
            ("-V", env!("CARGO_PKG_VERSION")),
            ("--version", "speaks protocol"),
        ] {
            let err = footed()
                .try_get_matches_from(["shep", "stop", flag])
                .unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion, "{flag}");
            assert!(err.to_string().contains(wants), "{flag}: {err}");
        }
    }

    #[test]
    fn a_verbs_help_is_short() {
        let root = footed();
        let stop = at(&root, &["stop".to_owned()]);
        let long = stop.clone().render_long_help().to_string();
        let short = stop.clone().render_help().to_string();
        assert!(
            long.lines().count() < 30,
            "{} lines:\n{long}",
            long.lines().count()
        );
        assert!(
            short.lines().count() < 12,
            "{} lines:\n{short}",
            short.lines().count()
        );
    }

    /// Fails when `shep help flags` leaves out a flag `GlobalArgs` declares,
    /// or prints one more than once.
    #[test]
    fn the_flags_topic_prints_every_global_flag_once_with_its_long_text() {
        let page = flat(&flags_topic().render_long_help().to_string());
        for (id, texts) in long_texts() {
            assert!(
                page.contains(&format!("--{id}")),
                "--{id} is missing:\n{page}"
            );
            for text in texts {
                assert_eq!(
                    page.matches(&text).count(),
                    1,
                    "--{id}: {text:?} in:\n{page}"
                );
            }
        }
    }

    /// The top level lists the globals under a heading, one line each, and
    /// the template and `GlobalArgs` agree on which they are.
    #[test]
    fn the_top_level_help_lists_each_global_flag_on_one_line_under_a_heading() {
        let help = with_global_footer(Cli::command())
            .render_long_help()
            .to_string();
        let block: Vec<&str> = help
            .lines()
            .skip_while(|line| !line.starts_with("Global flags"))
            .skip(1)
            .take_while(|line| line.starts_with("  "))
            .collect();
        for (id, _) in long_texts() {
            let lines = block
                .iter()
                .filter(|line| line.contains(&format!("--{id}")))
                .count();
            assert_eq!(
                lines, 1,
                "--{id} wants one line under `Global flags`:\n{help}"
            );
        }
        assert_eq!(block.len(), 5, "got {block:?}");
    }
}
