//! What `shep --help` and `shep --version` render.
//!
//! The grouped verb listing in [`HELP_TEMPLATE`] is hand-written, because
//! clap has no subcommand grouping of its own. `HELP_GROUPS` is the same
//! listing as data, and the tests below are what stop the two drifting from
//! each other or from the real command tree.

use std::sync::LazyLock;

use shep_core::protocol::{MIN_SUPPORTED, PROTOCOL_VERSION};

/// The verb groups [`HELP_TEMPLATE`] renders, and the source of truth the
/// drift test checks the real command tree against.
///
/// clap 4.6 has no subcommand grouping -- `#[command(help_heading = ..)]` on
/// a subcommand variant does not compile, checked against 4.6.6 -- so the
/// section is hand-written and this table is what keeps it from rotting. Add
/// a verb without filing it here and
/// `every_visible_verb_appears_in_exactly_one_help_group` fails.
///
/// `#[cfg(test)]` because it is exactly that: the assertions' structured copy
/// of what [`HELP_TEMPLATE`] states in prose. The template is what ships.
#[cfg(test)]
const HELP_GROUPS: &[(&str, &[&str])] = &[
    (
        "Run things",
        &[
            "start", "add", "serve", "stop", "restart", "reload", "delete", "stock",
        ],
    ),
    (
        "See what's up",
        &["flock", "describe", "bleats", "lookout", "fold", "barks"],
    ),
    (
        "Survive reboots",
        &["save", "muster", "startup", "unstartup"],
    ),
    ("Talk to a sheep", &["trigger", "signal", "whisper"]),
    (
        "The shepherd",
        &[
            "ping", "kill", "reopen", "flush", "set", "get", "unset", "secret",
        ],
    ),
    (
        "Dogs and agents",
        &["dogs", "enable", "disable", "adopt", "rehome", "whistle"],
    ),
    ("Foreground runs", &["runtime", "dev"]),
    ("Coming from pm2", &["import"]),
    ("Help", &["welcome", "init", "help", "completions", "style"]),
];

/// `--help`'s shape.
///
/// `{options}`, not `{all-args}`: the latter re-emits clap's own
/// alphabetical `Commands:` list underneath the grouped one, which is the
/// wall this replaces. The options section stays generated, so `--home`'s
/// `Less common` heading is still clap's work.
pub(super) const HELP_TEMPLATE: &str = "\
{about}

{usage-heading} {usage}

Getting started
  shep start server.js    start it and keep it alive
  shep flock              see what's running
  shep bleats server      follow its output
  shep startup            bring it back after a reboot

Run things       start add serve stop restart reload delete stock
See what's up    flock describe bleats lookout fold barks
Survive reboots  save muster startup unstartup
Talk to a sheep  trigger signal whisper
The shepherd     ping kill reopen flush set get unset secret
Dogs and agents  dogs enable disable adopt rehome whistle
Foreground runs  runtime dev
Coming from pm2  import
Help             welcome init help completions style

Aliases          flock: list, ls   bleats: logs   lookout: dash   stock: scale   whisper: sendline
Upgrading        cargo install shep replaces the binary, not the running shepherd: shep daemon reload

{options}{after-help}";

/// `shep --version`'s extra line, naming the protocol this build speaks and
/// how far back it reaches.
///
/// `-V` still prints the bare crate version (clap's `version` attribute
/// covers that); this is `--version`'s `long_version`, which clap falls
/// back to `version` for when unset, so this is purely additive.
///
/// `clap::Command::long_version` wants `&'static str` in the clap version
/// this workspace pins, not `String`. A `LazyLock` builds the text once, on
/// first access, and every caller after that borrows the same allocation.
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

    /// The test above exercises `version_text()`, the free function. It
    /// cannot catch a `#[command(long_version = ..)]` attribute that stops
    /// wiring that function in, or a `propagate_version` that stops
    /// carrying it to subcommands -- both survive a refactor that never
    /// touches `version_text()` itself. This renders the real `clap::Command`
    /// clap builds from the derive, the way `shep --version` and `shep
    /// daemon reload --version` actually do, so either regression fails
    /// here.
    ///
    /// `PROTOCOL_VERSION` and `MIN_SUPPORTED` are both 7 today, so a bare
    /// "does '7' appear" assertion would pass even if the two numbers were
    /// swapped. Matching each constant against the label `version_text()`
    /// prints next to it (`"speaks protocol"` / `"accepts .. and newer"`)
    /// checks the two numbers in their roles rather than merely finding "7"
    /// twice -- it would fail if the labels were swapped even though the
    /// values are equal. It would not fail if both constants moved to the
    /// same new value together; nothing can, while they are pinned equal.
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

        // `propagate_version = true` on `Cli` is supposed to carry the same
        // `long_version` down to every subcommand. `shep daemon reload` is
        // two levels deep (`daemon` -> `reload`), the deepest nesting this
        // command tree has, so it is the strongest check available that
        // propagation actually reaches leaves rather than just top-level
        // verbs.
        // `propagate_version` is applied by `Command::build`, which
        // `get_matches`/`parse` calls internally on the real CLI path but
        // `Cli::command()` alone does not -- an un-built `Command` has not
        // pushed `long_version` down to its subcommands yet, so `build()`
        // here is what makes this test see what a real invocation sees.
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

    /// `web/`, three directories above this file, only when it actually
    /// exists.
    ///
    /// Same helper, and the same reasoning, as
    /// `dog_index::tests::workspace_web_dir` -- see that one for why this
    /// has to be a runtime check rather than `include_str!`. Duplicated
    /// rather than shared: two call sites and six lines each did not earn a
    /// crate-wide test-support module.
    fn workspace_web_dir() -> Option<PathBuf> {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web"));
        dir.is_dir().then(|| dir.to_path_buf())
    }

    /// Reads `web/{relative}`, or `None` outside the workspace checkout
    /// (see [`workspace_web_dir`]).
    ///
    /// `web/` present but `relative` missing is real drift inside the
    /// checkout, not a published-crate build, and panics accordingly.
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

    /// fails when a visible verb or subcommand is missing from the docs
    /// site's CLI reference generator.
    ///
    /// The generator's `VERBS` array is hand-kept and regenerating the
    /// reference refreshes only what it already names, so a verb left out
    /// is invisible on the published site while the generator reports
    /// success. `style` and `welcome` shipped that way, found 2026-08-23
    /// when `init` did the same. So did every `shep secret` flag: a host's
    /// own `--help` names its subcommands without their flags, and this
    /// walk used to stop at the top level.
    ///
    /// Skips outside the workspace checkout -- see
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

    /// fails when the committed CLI reference has no block for something
    /// the generator's `VERBS` array names.
    ///
    /// The array and the generated file are two separate edits and only the
    /// first one is typing, so a stale file is the ordinary way this drifts.
    /// `web/src/data/cli-reference.ts` reads its verb list off the
    /// `@@VERB:...@@` markers rather than keeping a second copy of the
    /// array, which leaves an entry with no block as a verb the site
    /// quietly does not have.
    ///
    /// Skips outside the workspace checkout -- see
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

    /// Every visible alias is named in `--help`, and only real ones are.
    /// The grouped listing replaces clap's own `Commands:` block, which
    /// would otherwise render `[aliases: list, ls]` beside each verb, so
    /// nothing else guarantees an alias stays mentioned while it keeps
    /// working.
    ///
    /// Derived from clap rather than compared against a second list, so
    /// adding `visible_alias` to a verb fails here until the line says so.
    #[test]
    fn the_help_template_names_every_visible_alias() {
        use clap::CommandFactory;
        let command = Cli::command();
        let mut expected: Vec<String> = command
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .filter_map(|s| {
                let aliases: Vec<&str> = s.get_visible_aliases().collect();
                (!aliases.is_empty()).then(|| format!("{}: {}", s.get_name(), aliases.join(", ")))
            })
            .collect();
        expected.sort();

        let line = HELP_TEMPLATE
            .lines()
            .find(|l| l.starts_with("Aliases"))
            .expect("HELP_TEMPLATE has an Aliases line");

        for entry in &expected {
            assert!(
                line.contains(entry.as_str()),
                "`--help`'s Aliases line does not name `{entry}`: {line}"
            );
        }

        // And nothing invented: every `verb: ` on the line is a real one.
        for token in line.trim_start_matches("Aliases").split_whitespace() {
            if let Some(verb) = token.strip_suffix(':') {
                assert!(
                    expected.iter().any(|e| e.starts_with(&format!("{verb}:"))),
                    "`--help` names aliases for `{verb}`, which has none"
                );
            }
        }
    }

    /// `daemon reload` is hidden along with `daemon`, since `daemon` itself
    /// is `#[command(hide = true)]` -- it is the internal re-exec path, not
    /// a verb an operator picks off a menu. The version-skew refusal (Task
    /// 5) names `shep daemon reload` as the fix, so `--help` must name it
    /// too or an operator who goes looking for it finds nothing.
    #[test]
    fn the_help_template_names_the_upgrade_path() {
        let line = HELP_TEMPLATE
            .lines()
            .find(|l| l.starts_with("Upgrading"))
            .expect("HELP_TEMPLATE has an Upgrading line");
        assert_eq!(
            line,
            "Upgrading        cargo install shep replaces the binary, not the running shepherd: shep daemon reload"
        );
    }

    /// `HELP_TEMPLATE` is a literal and `HELP_GROUPS` is structured data, so
    /// the two can disagree. They may not: the template is what users read
    /// and the table is what the drift test above checks.
    #[test]
    fn the_help_template_and_the_group_table_agree() {
        for (heading, verbs) in HELP_GROUPS {
            let line = HELP_TEMPLATE
                .lines()
                .find(|l| l.starts_with(heading))
                .unwrap_or_else(|| panic!("`{heading}` is missing from HELP_TEMPLATE"));
            for verb in *verbs {
                assert!(
                    line.split_whitespace().any(|w| w == *verb),
                    "`{verb}` is filed under `{heading}` but is not on that line: {line}"
                );
            }
        }
    }

    /// The five commands that get someone to a reboot-surviving process.
    #[test]
    fn the_help_opens_with_a_worked_example() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
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
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
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
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
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
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
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
}
