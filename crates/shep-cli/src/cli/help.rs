//! What `shep --help` and `shep --version` render.
//!
//! clap has no subcommand grouping, so the grouped listing is rendered here
//! from [`HELP_GROUPS`], the verbs' own abouts and [`TOPICS`], and spliced
//! into [`HELP_TEMPLATE`]. The tests below stop `HELP_GROUPS` drifting from
//! the real command tree.

use std::sync::LazyLock;

use shep_core::protocol::{MIN_SUPPORTED, PROTOCOL_VERSION};

use super::topics::TOPICS;
use crate::output::width::text_columns;

/// The verb groups the listing renders, checked against the command tree.
///
/// `every_visible_verb_appears_in_exactly_one_help_group` fails on a verb
/// filed nowhere.
const HELP_GROUPS: &[(&str, &[&str])] = &[
    (
        "Run things",
        &[
            "start", "add", "serve", "stop", "restart", "reset", "reload", "delete", "stock",
            "enable", "disable",
        ],
    ),
    (
        "See what's up",
        &[
            "flock", "describe", "bleats", "lookout", "fold", "barks", "hints",
        ],
    ),
    (
        "Survive reboots",
        &["save", "muster", "startup", "unstartup"],
    ),
    ("Talk to a sheep", &["trigger", "signal", "whisper"]),
    (
        "The shepherd",
        &[
            "ping", "kill", "reopen", "flush", "kv", "get", "set", "unset", "copy", "secret",
            "host",
        ],
    ),
    (
        "Dogs and agents",
        &["dogs", "install", "upgrade", "adopt", "whistle"],
    ),
    ("Foreground runs", &["runtime", "dev"]),
    ("Coming from pm2", &["import"]),
    ("Help", &["welcome", "init", "help", "completions", "style"]),
];

/// The group adopted dogs are listed in.
const DOG_GROUP_HEADING: &str = "Dogs and agents";

/// What `help` is about: clap generates the verb, so it declares none.
const HELP_VERB_ABOUT: &str = "Show a verb's or a topic's help";

/// The marker [`top_level_template`] swaps for the grouped listing.
const LISTING_MARKER: &str = "{subcommands}";

/// `--help`'s shape.
///
/// `{subcommands}` is clap's own alphabetical list until
/// [`top_level_template`] swaps it for the grouped one. There is no
/// `{options}`: the global flags are the footer line `after_help` carries.
pub(crate) const HELP_TEMPLATE: &str = "\
{about}

{usage-heading} {usage}

Getting started
  shep start server.js    start it and keep it alive
  shep flock              see what's running
  shep bleats server      follow its output
  shep startup            bring it back after a reboot

{subcommands}{after-help}";

/// A dog the listing shows in its group, beside the verbs.
pub(crate) struct DogLine<'a> {
    /// The name the operator types.
    pub(crate) name: &'a str,
    /// What it does. `None` lists the name alone.
    pub(crate) about: Option<&'a str>,
}

/// One line of the listing: the names a thing answers to, then what it does.
struct Row {
    names: Vec<String>,
    about: Option<String>,
}

impl Row {
    fn verb(command: &clap::Command, name: &str) -> Self {
        let found = command.find_subcommand(name);
        let mut names = vec![name.to_owned()];
        names.extend(
            found
                .into_iter()
                .flat_map(clap::Command::get_visible_aliases)
                .map(str::to_owned),
        );
        let about = match found {
            Some(verb) => verb.get_about().map(ToString::to_string),
            None if name == "help" => Some(HELP_VERB_ABOUT.to_owned()),
            None => None,
        };
        Self { names, about }
    }

    /// The names as written: `flock, list, ls`.
    fn label(&self) -> String {
        self.names.join(", ")
    }
}

/// The grouped verbs, then the help topics, as the text replacing
/// [`LISTING_MARKER`], painted with `dress`.
///
/// Every line's about starts in one column, the widest name plus two. A name
/// with no about does not widen it.
fn listing(command: &clap::Command, dogs: &[DogLine<'_>], dress: &Dress) -> String {
    let mut sections: Vec<(&str, Vec<Row>)> = HELP_GROUPS
        .iter()
        .map(|(heading, verbs)| {
            let mut rows: Vec<Row> = verbs.iter().map(|v| Row::verb(command, v)).collect();
            if *heading == DOG_GROUP_HEADING {
                rows.extend(dogs.iter().map(|dog| Row {
                    names: vec![dog.name.to_owned()],
                    about: dog.about.map(str::to_owned),
                }));
            }
            (*heading, rows)
        })
        .collect();
    sections.push((
        "Help topics",
        TOPICS
            .iter()
            .map(|topic| Row {
                names: vec![topic.name.to_owned()],
                about: Some(topic.about.to_owned()),
            })
            .collect(),
    ));

    let widest = sections
        .iter()
        .flat_map(|(_, rows)| rows)
        .filter(|row| row.about.is_some())
        .map(|row| text_columns(&row.label()))
        .max()
        .unwrap_or(0);
    sections
        .iter()
        .map(|(heading, rows)| {
            let lines = rows.iter().map(|row| {
                let names: Vec<String> = row
                    .names
                    .iter()
                    .map(|name| dress.paint(dress.verb, name))
                    .collect();
                let mut line = format!("  {}", names.join(", "));
                if let Some(about) = &row.about {
                    let pad = widest + 2 - text_columns(&row.label());
                    line.push_str(&" ".repeat(pad));
                    line.push_str(about);
                }
                line
            });
            let mut section = vec![dress.paint(dress.heading, heading)];
            section.extend(lines);
            section.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// [`HELP_TEMPLATE`] with the listing of `command`'s verbs and `dogs` in
/// place, painted at `deep`'s colour depth.
///
/// Only escape codes are added to the plain text, so clap strips them with the
/// rest when colour is off.
pub(crate) fn top_level_template(
    command: &clap::Command,
    dogs: &[DogLine<'_>],
    deep: bool,
) -> String {
    let dress = Dress::new(deep);
    let head = dress_head(HELP_TEMPLATE, &dress);
    head.replace(LISTING_MARKER, &listing(command, dogs, &dress))
}

/// The two styles the listing is painted with, as in
/// [`crate::output::paint::help_styles`]: headings, and the verbs' literals.
struct Dress {
    heading: anstyle::Style,
    verb: anstyle::Style,
}

impl Dress {
    fn new(deep: bool) -> Self {
        let styles = crate::output::paint::help_styles(deep);
        Self {
            heading: *styles.get_header(),
            verb: *styles.get_literal(),
        }
    }

    fn paint(&self, style: anstyle::Style, text: &str) -> String {
        format!("{style}{text}{style:#}")
    }
}

/// Where [`dress_head`] is in the template.
#[derive(Clone, Copy)]
enum Block {
    Prose,
    Examples,
}

/// `template` with the Getting started heading and its example verbs painted.
fn dress_head(template: &str, dress: &Dress) -> String {
    let mut block = Block::Prose;
    template
        .lines()
        .map(|line| match (block, line) {
            (Block::Prose, "Getting started") => {
                block = Block::Examples;
                dress.paint(dress.heading, line)
            }
            (Block::Examples, "") => {
                block = Block::Prose;
                String::new()
            }
            (Block::Examples, _) => match line.strip_prefix("  shep ") {
                Some(rest) => {
                    let (verb, tail) = rest.split_once(' ').unwrap_or((rest, ""));
                    let tail = if tail.is_empty() {
                        String::new()
                    } else {
                        format!(" {tail}")
                    };
                    format!("  shep {}{tail}", dress.paint(dress.verb, verb))
                }
                None => line.to_string(),
            },
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `shep --version`'s extra line: the protocol this build speaks and accepts.
///
/// `-V` still prints the bare crate version. clap's `long_version` wants a
/// `&'static str`. A `LazyLock` builds the text once and callers borrow it.
static VERSION_TEXT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\nspeaks protocol {PROTOCOL_VERSION}, accepts {MIN_SUPPORTED} and newer",
        env!("CARGO_PKG_VERSION")
    )
});

pub(super) fn version_text() -> &'static str {
    VERSION_TEXT.as_str()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::cli::Cli;

    /// An operator asking what a build speaks should not have to read
    /// source to learn how far back it reaches.
    #[test]
    fn version_output_names_both_the_protocol_and_the_floor() {
        let text = version_text();
        assert!(
            text.contains(&format!("protocol {PROTOCOL_VERSION}")),
            "got {text}"
        );
        assert!(
            text.contains(&format!("accepts {MIN_SUPPORTED}")),
            "got {text}"
        );
    }

    /// Renders the real `clap::Command`, so a `long_version` or
    /// `propagate_version` regression fails here.
    ///
    /// The free-function test above cannot catch either. Each constant is
    /// matched against its own label. Both are 7 today, so a swap would
    /// otherwise pass.
    #[test]
    fn the_rendered_version_names_the_protocol_and_the_floor_on_the_real_command() {
        use clap::CommandFactory;

        let top = Cli::command().render_long_version().to_string();
        assert!(
            top.contains(&format!("speaks protocol {PROTOCOL_VERSION}")),
            "top-level --version lost the protocol line: {top}"
        );
        assert!(
            top.contains(&format!("accepts {MIN_SUPPORTED} and newer")),
            "top-level --version lost the floor line: {top}"
        );

        // `shep daemon reload` is the deepest subcommand, so it is the
        // strongest check that `propagate_version` reaches leaves.
        // `Cli::command()` is unbuilt; `build()` pushes `long_version` down,
        // as a real invocation does.
        let mut top_command = Cli::command();
        top_command.build();
        let daemon = top_command
            .find_subcommand("daemon")
            .expect("shep daemon exists");
        let reload = daemon
            .find_subcommand("reload")
            .expect("shep daemon reload exists");
        let nested = reload.render_long_version().to_string();
        assert!(
            nested.contains(&format!("speaks protocol {PROTOCOL_VERSION}")),
            "shep daemon reload --version lost the protocol line: {nested}"
        );
        assert!(
            nested.contains(&format!("accepts {MIN_SUPPORTED} and newer")),
            "shep daemon reload --version lost the floor line: {nested}"
        );
    }

    /// `web/`, three directories above this file, only when it exists.
    ///
    /// Duplicates `dog_index::tests::workspace_web_dir`, which says why this
    /// is a runtime check rather than `include_str!`.
    fn workspace_web_dir() -> Option<PathBuf> {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web"));
        dir.is_dir().then(|| dir.to_path_buf())
    }

    /// Reads `web/{relative}`, or `None` outside the workspace checkout.
    ///
    /// Inside it a missing file is real drift, so this panics.
    ///
    /// # Panics
    /// Inside the workspace, if `relative` cannot be read.
    fn read_workspace_web_file(relative: &str) -> Option<String> {
        let dir = workspace_web_dir()?;
        Some(
            std::fs::read_to_string(dir.join(relative)).unwrap_or_else(|err| {
                panic!("web/{relative} exists in the workspace but could not be read: {err}")
            }),
        )
    }

    /// The workspace root, only when this build sits inside this repo.
    ///
    /// Two ancestors up is not proof: a registry or `cargo vendor` tree has
    /// them too. The marker is a root `Cargo.toml` opening a `[workspace]`
    /// that names `crates/shep-cli`. A foreign checkout with that exact member
    /// path is indistinguishable. There the assertion names the README.
    fn workspace_root() -> Option<PathBuf> {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        (manifest.contains("[workspace]") && manifest.contains("crates/shep-cli"))
            .then(|| dir.to_path_buf())
    }

    /// Reads the workspace root's `README.md`, or `None` outside the
    /// workspace checkout -- see [`workspace_root`] for what counts as one.
    ///
    /// # Panics
    /// Inside the workspace, if the file cannot be read.
    fn read_workspace_readme() -> Option<String> {
        let dir = workspace_root()?;
        Some(
            std::fs::read_to_string(dir.join("README.md")).unwrap_or_else(|err| {
                panic!("README.md exists in the workspace but could not be read: {err}")
            }),
        )
    }

    /// The README's block: each `HELP_GROUPS` heading padded to one column,
    /// then its verbs on one line.
    fn readme_group_block() -> String {
        let width = HELP_GROUPS.iter().map(|(h, _)| h.len()).max().unwrap_or(0) + 2;
        HELP_GROUPS
            .iter()
            .map(|(heading, verbs)| format!("{heading:<width$}{}", verbs.join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Fails when the README's grouped verb listing, under "Everything else",
    /// drifts from `HELP_GROUPS`, which `shep --help` renders.
    ///
    /// The generator does not touch that prose, so nothing else reads it
    /// back. Skips outside the workspace checkout; see [`read_workspace_readme`].
    #[test]
    fn the_readme_names_the_same_verb_groups_as_help_does() {
        let Some(readme) = read_workspace_readme() else {
            return;
        };

        let fence_start = readme
            .find("```text\n")
            .expect("the README has a fenced verb-grouping block")
            + "```text\n".len();
        let fence_end = readme[fence_start..]
            .find("```")
            .expect("the fenced block closes");
        let readme_block = readme[fence_start..fence_start + fence_end].trim_end();

        assert_eq!(
            readme_block,
            readme_group_block(),
            "README.md's verb grouping has drifted from HELP_GROUPS; \
             copy the headings and verbs from help.rs into README.md's \
             \"Everything else\" block"
        );
    }

    /// `shep --help` as an operator sees it with no dogs adopted.
    fn top_level_help() -> String {
        use clap::CommandFactory;
        let mut command = crate::cli::with_global_footer(Cli::command());
        crate::adopted_help::list_in_help(&mut command, &[], false);
        command.render_long_help().to_string()
    }

    /// Every visible verb, with its about and the names it answers to.
    fn visible_verbs() -> Vec<(String, Vec<String>, String)> {
        use clap::CommandFactory;
        Cli::command()
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| {
                (
                    s.get_name().to_string(),
                    s.get_visible_aliases().map(str::to_string).collect(),
                    s.get_about().map(ToString::to_string).unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn each_verb_line_carries_its_about() {
        let help = top_level_help();
        for (name, _, about) in visible_verbs() {
            if name == "help" {
                continue;
            }
            assert!(!about.is_empty(), "`{name}` has no about");
            let line = help
                .lines()
                .find(|line| {
                    line.strip_prefix("  ")
                        .and_then(|rest| rest.split([' ', ',']).next())
                        == Some(name.as_str())
                })
                .unwrap_or_else(|| panic!("`{name}` has no line in:\n{help}"));
            assert!(
                line.ends_with(&about),
                "`{name}` is listed without its about {about:?}: {line:?}"
            );
        }
    }

    #[test]
    fn every_verb_about_fits_sixty_columns() {
        use crate::output::width::text_columns;
        let too_long: Vec<String> = visible_verbs()
            .into_iter()
            .filter(|(_, _, about)| text_columns(about) > 60)
            .map(|(name, _, about)| format!("{name} ({}): {about}", text_columns(&about)))
            .collect();
        assert!(
            too_long.is_empty(),
            "abouts over 60 columns:\n{}",
            too_long.join("\n")
        );
    }

    #[test]
    fn aliases_sit_on_their_verbs_line() {
        let help = top_level_help();
        let mut seen = 0;
        for (name, aliases, _) in visible_verbs() {
            if aliases.is_empty() {
                continue;
            }
            seen += 1;
            let names = format!("{name}, {}", aliases.join(", "));
            assert!(
                help.lines()
                    .any(|line| line.starts_with(&format!("  {names}  "))),
                "`{names}` is not on one line:\n{help}"
            );
        }
        assert!(seen > 0, "no verb has an alias, so this checked nothing");
        assert!(
            !help.contains("Aliases"),
            "the Aliases line is back:\n{help}"
        );
        assert!(
            !help.contains("Upgrading"),
            "the Upgrading line is back:\n{help}"
        );
    }

    #[test]
    fn the_top_level_has_no_options_block() {
        let help = top_level_help();
        assert!(!help.contains("Options"), "{help}");
        assert!(!help.contains("--help"), "{help}");
        assert!(
            help.lines().any(|line| line
                == "Global flags: --format, --quiet, --style, --host, --home, -h, -V. See shep help flags"),
            "{help}"
        );
    }

    #[test]
    fn the_command_tree_parses_and_is_internally_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert(); // clap's own structural self-check
    }

    /// The generator's `VERBS` array, one entry per element, with a quoted
    /// multi-word entry like `"secret set"` kept whole.
    fn listed_verb_paths(generator: &str) -> Vec<String> {
        let (_, rest) = generator
            .split_once("VERBS=(")
            .expect("the generator declares a VERBS array");
        let (block, _) = rest.split_once(')').expect("the VERBS array closes");

        // Splitting on the quote character puts every quoted entry at an odd
        // index and everything unquoted at an even one.
        block
            .split('"')
            .enumerate()
            .flat_map(|(i, chunk)| {
                if i % 2 == 1 {
                    vec![chunk.to_string()]
                } else {
                    chunk.split_whitespace().map(str::to_string).collect()
                }
            })
            .collect()
    }

    /// Every visible command path, space-joined, parents before children:
    /// `secret`, `secret set`, `secret get`, and so on.
    ///
    /// A hidden command takes its whole subtree with it, which is how the
    /// `daemon` re-exec target and its own `reload` stay out. `help` is
    /// clap's own at every depth and the other deliberate omission.
    fn visible_command_paths(command: &clap::Command) -> Vec<String> {
        fn walk(command: &clap::Command, prefix: &str, found: &mut Vec<String>) {
            for sub in command.get_subcommands() {
                if sub.is_hide_set() || sub.get_name() == "help" {
                    continue;
                }
                let path = if prefix.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{prefix} {}", sub.get_name())
                };
                found.push(path.clone());
                walk(sub, &path, found);
            }
        }

        let mut found = Vec::new();
        walk(command, "", &mut found);
        found
    }

    /// Fails when a visible verb or subcommand is missing from the docs
    /// site's CLI reference generator.
    ///
    /// `VERBS` is hand-kept. A verb left out is invisible on the site, yet
    /// regeneration succeeds. Skips outside the workspace checkout; see
    /// [`read_workspace_web_file`].
    #[test]
    fn every_visible_verb_reaches_the_docs_site_generator() {
        use clap::CommandFactory;

        let Some(generator) = read_workspace_web_file("scripts/generate-cli-reference.sh") else {
            return;
        };
        let listed = listed_verb_paths(&generator);

        let command = Cli::command();
        let missing: Vec<String> = visible_command_paths(&command)
            .into_iter()
            .filter(|path| !listed.contains(path))
            .collect();

        assert!(
            missing.is_empty(),
            "these verbs would be missing from the published CLI reference: {missing:?}\n\
             add them to VERBS in web/scripts/generate-cli-reference.sh and re-run it"
        );
    }

    /// Fails when the committed CLI reference has no block for a verb in the
    /// generator's `VERBS` array.
    ///
    /// The array and the file are separate edits, so a stale file is the
    /// usual drift. Skips outside the workspace checkout; see
    /// [`read_workspace_web_file`].
    #[test]
    fn every_listed_verb_has_a_block_in_the_committed_reference() {
        let (Some(generator), Some(reference)) = (
            read_workspace_web_file("scripts/generate-cli-reference.sh"),
            read_workspace_web_file("src/data/cli-reference.generated.txt"),
        ) else {
            return;
        };

        let missing: Vec<String> = listed_verb_paths(&generator)
            .into_iter()
            .filter(|path| !reference.contains(&format!("\n@@VERB:{path}@@\n")))
            .collect();

        assert!(
            missing.is_empty(),
            "the committed CLI reference has no block for {missing:?}\n\
             run 'cargo build --release' then ./web/scripts/generate-cli-reference.sh"
        );
    }

    /// Every visible verb is filed under exactly one heading, and every name
    /// under a heading is a real verb. A hand-written list rots the first
    /// time somebody adds a command; this is what stops it, the way
    /// `docs/whistle/tools.md`'s catalogue test stops that list rotting.
    #[test]
    fn every_visible_verb_appears_in_exactly_one_help_group() {
        use clap::CommandFactory;
        let command = Cli::command();
        let visible: Vec<String> = command
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name().to_string())
            .collect();
        let filed: Vec<&str> = HELP_GROUPS
            .iter()
            .flat_map(|(_, verbs)| verbs.iter().copied())
            .collect();

        for verb in &visible {
            let times = filed.iter().filter(|f| *f == verb).count();
            assert_eq!(
                times, 1,
                "`{verb}` appears in {times} help groups; it must appear in exactly one"
            );
        }
        for name in &filed {
            // `help` is clap's own, generated rather than declared, so it is
            // the one filed name with no matching subcommand to find.
            assert!(
                *name == "help" || visible.iter().any(|v| v == name),
                "a help group names `{name}`, which is not a visible verb"
            );
        }
    }

    /// The five commands that get someone to a reboot-surviving process.
    #[test]
    fn the_help_opens_with_a_worked_example() {
        let help = top_level_help();
        assert!(
            help.contains("Getting started"),
            "no getting-started block:\n{help}"
        );
        assert!(
            help.contains("shep start server.js"),
            "no worked example:\n{help}"
        );
    }

    /// `--home` is plumbing, not a choice, and it was the first global option
    /// anyone read.
    #[test]
    fn home_is_the_last_global_option_a_reader_meets() {
        let help = top_level_help();
        let home = help.find("--home").expect("--home is still documented");
        let format = help.find("--format").expect("--format is documented");
        let quiet = help.find("--quiet").expect("--quiet is documented");
        assert!(
            home > format && home > quiet,
            "--home must come after the options people actually choose:\n{help}"
        );
    }

    /// `--help` is the first thing a stranger reads, and for three phases it
    /// opened with this crate's own reasoning about clap's `bin_name`.
    /// clap turns a doc comment into `long_about`, and nobody ran the
    /// command after writing the comment.
    #[test]
    fn the_top_level_help_carries_no_implementation_notes() {
        let help = top_level_help();
        for leak in [
            "bin_name",
            "Phase 15",
            "load-bearing",
            "argv[0]",
            // A clap template placeholder reaching the render means a doc
            // comment is discussing the template rather than the command.
            "{options}",
            "{all-args}",
            "help_heading",
        ] {
            assert!(
                !help.contains(leak),
                "`shep --help` still contains the internal note {leak:?}:\n{help}"
            );
        }
    }

    /// `--help` is the largest body of user-facing copy in the product, and
    /// `welcome.rs`, `status.rs` and `output/table.rs` each pin "no em or en
    /// dashes in copy a user reads" for their own copy while nothing pinned
    /// this one -- exactly how an em dash on `--quiet`'s help text and Rust
    /// intra-doc-link syntax on `--style`'s both reached a real terminal
    /// before anyone ran the binary and read the rendered output rather
    /// than the doc comment that produced it.
    #[test]
    fn the_top_level_help_has_no_dashes_or_doc_link_syntax() {
        let help = top_level_help();
        assert!(
            !help.contains('\u{2014}'),
            "an em dash reached --help, which this project's copy rules forbid:\n{help}"
        );
        assert!(
            !help.contains('\u{2013}'),
            "an en dash reached --help, which this project's copy rules forbid:\n{help}"
        );
        assert!(
            !help.contains("[`"),
            "Rust intra-doc-link syntax reached --help -- an aside meant for a reader of the \
             source, not the terminal, belongs on a `//` comment rather than a `///` one:\n{help}"
        );
    }

    /// The listing clap cannot style is painted by hand: every group heading
    /// and verb, the example verbs, and the topics.
    #[test]
    fn the_grouped_listing_is_painted_in_the_palette() {
        use clap::CommandFactory;
        let command = Cli::command()
            .styles(crate::output::paint::help_styles(true))
            .help_template(top_level_template(&Cli::command(), &[], true));
        let ansi = command.clone().render_long_help().ansi().to_string();
        let heading = |text: &str| format!("\x1b[1m\x1b[38;5;221m{text}\x1b[0m");
        let verb = |text: &str| format!("\x1b[38;5;29m{text}\x1b[0m");
        for expected in [
            heading("Getting started"),
            format!("shep {} server.js", verb("start")),
            heading("Run things"),
            verb("restart"),
            heading("See what's up"),
            heading("Help"),
            heading("Help topics"),
            verb("selectors"),
            format!("{}, {}, {}", verb("flock"), verb("list"), verb("ls")),
        ] {
            assert!(ansi.contains(&expected), "missing {expected:?} in {ansi:?}");
        }
    }

    #[test]
    fn colour_off_strips_every_escape_the_listing_adds() {
        use clap::CommandFactory;
        let plain = Cli::command()
            .help_template(top_level_template(&Cli::command(), &[], true))
            .render_long_help()
            .to_string();
        assert!(!plain.contains('\x1b'), "{plain:?}");
        assert!(plain.contains("\n  start  "), "{plain}");
    }

    #[test]
    fn painting_an_adopted_dogs_name_leaves_its_description_plain() {
        use clap::CommandFactory;
        let dog = DogLine {
            name: "otel",
            about: Some("exports traces"),
        };
        let template = top_level_template(&Cli::command(), &[dog], true);
        let name = "\x1b[38;5;29motel\x1b[0m";
        let at = template.find(name).expect("the dog's name is painted");
        let rest = template[at + name.len()..].trim_start_matches(' ');
        assert!(rest.starts_with("exports traces"), "got {rest:?}");
    }

    /// Every short flag in the tree, built so the globals reach each verb,
    /// mapped to the long flags that carry it and the verbs that have each.
    fn shorts_in_tree() -> std::collections::BTreeMap<char, std::collections::BTreeSet<String>> {
        fn walk(
            command: &clap::Command,
            found: &mut std::collections::BTreeMap<char, std::collections::BTreeSet<String>>,
        ) {
            for arg in command.get_arguments() {
                if let Some(short) = arg.get_short() {
                    found
                        .entry(short)
                        .or_default()
                        .insert(arg.get_long().unwrap_or(arg.get_id().as_str()).to_owned());
                }
            }
            for sub in command.get_subcommands() {
                walk(sub, found);
            }
        }
        use clap::CommandFactory;
        let mut command = Cli::command();
        command.build();
        let mut found = std::collections::BTreeMap::new();
        walk(&command, &mut found);
        found
    }

    /// clap's `debug_assert` only sees a clash inside one command, so a letter
    /// meaning `--follow` on one verb and `--fold` on another passes it.
    #[test]
    fn a_short_flag_names_one_long_flag_across_the_tree() {
        for (short, longs) in shorts_in_tree() {
            assert_eq!(longs.len(), 1, "-{short} names {longs:?}");
        }
    }

    #[test]
    fn quiet_is_the_only_global_flag_with_a_short() {
        use clap::CommandFactory;
        let command = Cli::command();
        let shorts: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .filter_map(|arg| arg.get_short().map(|short| (short, arg.get_id().as_str())))
            .collect();
        assert_eq!(shorts, [('q', "quiet")]);
    }

    /// Each letter, with a sample command it parses on, beside the long form.
    const SHORT_FORMS: &[(char, &[&str], &[&str])] = &[
        ('a', &["kv", "unset", "-a"], &["kv", "unset", "--all"]),
        ('A', &["dogs", "-A"], &["dogs", "--available"]),
        (
            'b',
            &["serve", "www", "-b", "0.0.0.0"],
            &["serve", "www", "--bind", "0.0.0.0"],
        ),
        (
            'c',
            &["start", "app.js", "-c", "/tmp"],
            &["start", "app.js", "--cwd", "/tmp"],
        ),
        (
            'd',
            &["set", "web.restart_delay", "1", "-d"],
            &["set", "web.restart_delay", "1", "--dry-run"],
        ),
        ('D', &["upgrade", "-D"], &["upgrade", "--dogs"]),
        ('e', &["bleats", "-e"], &["bleats", "--err"]),
        (
            'E',
            &["secret", "get", "K", "-E", "prod"],
            &["secret", "get", "K", "--env", "prod"],
        ),
        ('f', &["flock", "-f"], &["flock", "--follow"]),
        (
            'F',
            &["start", "app.js", "-F", "web"],
            &["start", "app.js", "--fold", "web"],
        ),
        (
            'i',
            &["flock", "-f", "-i", "5"],
            &["flock", "--follow", "--interval", "5"],
        ),
        ('l', &["bleats", "-l", "5"], &["bleats", "--lines", "5"]),
        (
            'n',
            &["start", "app.js", "-n", "api"],
            &["start", "app.js", "--name", "api"],
        ),
        ('N', &["bleats", "-N"], &["bleats", "--no-follow"]),
        ('o', &["bleats", "-o"], &["bleats", "--out"]),
        (
            'p',
            &["serve", "www", "-p", "9000"],
            &["serve", "www", "--port", "9000"],
        ),
        (
            'r',
            &["dogs", "-A", "-r"],
            &["dogs", "--available", "--refresh"],
        ),
        ('R', &["lookout", "-R"], &["lookout", "--read-only"]),
        (
            's',
            &["secret", "set", "K", "-s"],
            &["secret", "set", "K", "--stdin"],
        ),
        ('S', &["serve", "www", "-S"], &["serve", "www", "--spa"]),
        ('t', &["barks", "-t", "5"], &["barks", "--tail", "5"]),
        (
            'u',
            &["startup", "-u", "root"],
            &["startup", "--user", "root"],
        ),
        ('y', &["enable", "otel", "-y"], &["enable", "otel", "--yes"]),
    ];

    fn parsed(args: &[&str]) -> String {
        use clap::Parser;
        let argv = std::iter::once("shep").chain(args.iter().copied());
        match Cli::try_parse_from(argv) {
            Ok(cli) => format!("{cli:?}"),
            Err(error) => panic!("{args:?} did not parse: {error}"),
        }
    }

    #[test]
    fn a_short_flag_parses_as_its_long_form() {
        for (short, with_short, with_long) in SHORT_FORMS {
            assert_eq!(parsed(with_short), parsed(with_long), "-{short}");
        }
        let letters: Vec<char> = SHORT_FORMS.iter().map(|row| row.0).collect();
        let in_tree: Vec<char> = shorts_in_tree()
            .into_keys()
            .filter(|short| !matches!(short, 'q' | 'h' | 'V'))
            .collect();
        let mut sorted = letters.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted, in_tree,
            "the table covers every letter the tree has"
        );
        assert_eq!(letters.len(), 23);
    }

    #[test]
    fn short_flags_cluster_and_attach_their_values() {
        assert_eq!(
            parsed(&["set", "web.restart_delay", "1", "-dy"]),
            parsed(&["set", "web.restart_delay", "1", "--dry-run", "--yes"]),
        );
        assert_eq!(
            parsed(&["start", "app.js", "-napi"]),
            parsed(&["start", "app.js", "--name", "api"]),
        );
    }

    #[test]
    fn the_letters_left_out_stay_usage_errors() {
        use clap::Parser;
        for argv in [
            ["shep", "start", "app.js", "-i", "4"].as_slice(),
            ["shep", "bleats", "-f"].as_slice(),
            ["shep", "init", "-f"].as_slice(),
        ] {
            let error = Cli::try_parse_from(argv).expect_err("a usage error");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{argv:?}"
            );
        }
    }
}
