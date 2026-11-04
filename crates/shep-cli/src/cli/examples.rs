//! The examples every verb's help ends with.
//!
//! One table, [`EXAMPLES`], holds the command lines; [`section`] is the one
//! place that spells the heading and the `$ ` prefix, so every page reads the
//! same. [`super::global_help::with_global_footer`] applies it to each verb.

/// Each verb's command lines, one to three, each written without `$ `.
///
/// A verb with a line here and no such verb in the tree fails
/// `no_example_names_a_verb_that_does_not_exist`; a visible verb with none
/// fails `every_visible_verb_has_one_to_three_examples`.
pub(super) const EXAMPLES: &[(&str, &[&str])] = &[
    (
        "start",
        &[
            "shep start server.js --name api",
            "shep start Flockfile.toml",
            "shep start api",
        ],
    ),
    (
        "add",
        &[
            "shep add worker.py --name worker",
            "shep add Flockfile.toml",
        ],
    ),
    (
        "serve",
        &["shep serve ./public", "shep serve ./dist --port 3000 --spa"],
    ),
    (
        "stop",
        &["shep stop api", "shep stop 'api-*'", "shep stop fold:web"],
    ),
    ("restart", &["shep restart api", "shep restart all"]),
    ("reset", &["shep reset api", "shep reset all"]),
    ("reload", &["shep reload api", "shep reload fold:web"]),
    ("delete", &["shep delete api", "shep delete '/^tmp-/'"]),
    ("stock", &["shep stock api 4"]),
    ("enable", &["shep enable api", "shep enable kelpie --yes"]),
    ("disable", &["shep disable api", "shep disable fold:web"]),
    (
        "flock",
        &[
            "shep flock",
            "shep flock --follow --interval 2",
            "shep flock --format json",
        ],
    ),
    ("describe", &["shep describe api", "shep describe 'api-*'"]),
    (
        "bleats",
        &[
            "shep bleats api",
            "shep bleats api --no-follow --lines 100",
            "shep bleats --err",
        ],
    ),
    (
        "lookout",
        &[
            "shep lookout",
            "shep lookout api",
            "shep lookout api restart --read-only",
        ],
    ),
    ("fold", &["shep fold web"]),
    ("barks", &["shep barks", "shep barks --tail 20"]),
    ("save", &["shep save"]),
    ("muster", &["shep muster"]),
    (
        "startup",
        &["shep startup", "shep startup --init systemd --user deploy"],
    ),
    ("unstartup", &["shep unstartup"]),
    (
        "trigger",
        &[
            "shep trigger api reindex",
            "shep trigger api warm-cache /products",
            "shep trigger kelpie",
        ],
    ),
    (
        "signal",
        &["shep signal api SIGHUP", "shep signal 'worker-*' usr1"],
    ),
    ("whisper", &["shep whisper repl 'reload config'"]),
    ("ping", &["shep ping"]),
    ("kill", &["shep kill"]),
    ("reopen", &["shep reopen", "shep reopen api"]),
    ("flush", &["shep flush api", "shep flush --daemon"]),
    (
        "kv",
        &[
            "shep kv set deploy-id 4821",
            "shep kv get deploy-id",
            "shep kv unset deploy-id",
        ],
    ),
    (
        "get",
        &["shep get api.max_memory", "shep get api.dogs.kelpie.git"],
    ),
    (
        "set",
        &[
            "shep set api.restart_delay 3s",
            "shep set api.max_memory 512M --dry-run",
        ],
    ),
    ("unset", &["shep unset api.max_memory"]),
    (
        "copy",
        &[
            "shep copy web.restart_delay api.restart_delay",
            "shep copy web.env api.env --except SESSION_SECRET",
        ],
    ),
    (
        "secret",
        &[
            "shep secret set DATABASE_URL --stdin",
            "shep secret set API_TOKEN --stdin --env staging",
            "shep secret list",
        ],
    ),
    (
        "host",
        &[
            "shep host add deploy@build.example.com",
            "shep host list",
            "shep host remove amp2",
        ],
    ),
    (
        "dogs",
        &[
            "shep dogs",
            "shep dogs --available",
            "shep dogs grant kelpie flock.read",
        ],
    ),
    ("install", &["shep install", "shep install kelpie --yes"]),
    (
        "upgrade",
        &[
            "shep upgrade kelpie",
            "shep upgrade --dogs",
            "shep upgrade kelpie --rollback",
        ],
    ),
    (
        "adopt",
        &[
            "shep adopt ./shep-kelpie",
            "shep adopt ./target/release/mydog --name watcher",
        ],
    ),
    ("whistle", &["shep whistle"]),
    ("runtime", &["shep runtime", "shep runtime Flockfile.toml"]),
    ("dev", &["shep dev", "shep dev server.js --name api"]),
    (
        "import",
        &[
            "shep import pm2 --dry-run",
            "shep import env .env --app api --secret DATABASE_URL",
        ],
    ),
    ("welcome", &["shep welcome"]),
    (
        "init",
        &["shep init", "shep init Flockfile.yaml", "shep init --all"],
    ),
    (
        "completions",
        &["shep completions zsh", "shep completions bash"],
    ),
    ("style", &["shep style", "shep style plain"]),
];

/// The `Examples` section for `verb`, or `None` when the table holds none.
pub(super) fn section(verb: &str) -> Option<String> {
    let (_, lines) = EXAMPLES.iter().find(|(name, _)| *name == verb)?;
    let body: Vec<String> = lines.iter().map(|line| format!("  $ {line}")).collect();
    Some(format!("Examples:\n{}", body.join("\n")))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{Cli, with_global_footer};

    /// `line` split the way a shell splits it: on blanks, with single and
    /// double quotes grouping.
    fn shell_split(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut in_word = false;
        let mut quote: Option<char> = None;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => word.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    in_word = true;
                }
                (None, c) if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                (None, c) => {
                    word.push(c);
                    in_word = true;
                }
            }
        }
        assert!(quote.is_none(), "unclosed quote in {line:?}");
        if in_word {
            words.push(word);
        }
        words
    }

    /// The visible verbs of the unbuilt command tree.
    fn visible_verbs() -> Vec<String> {
        Cli::command()
            .get_subcommands()
            .filter(|verb| !verb.is_hide_set())
            .map(|verb| verb.get_name().to_owned())
            .collect()
    }

    fn lines_of(verb: &str) -> &'static [&'static str] {
        EXAMPLES
            .iter()
            .find(|(name, _)| *name == verb)
            .map_or(&[], |(_, lines)| *lines)
    }

    fn every_line() -> impl Iterator<Item = (&'static str, &'static str)> {
        EXAMPLES
            .iter()
            .flat_map(|(verb, lines)| lines.iter().map(move |line| (*verb, *line)))
    }

    #[test]
    fn every_visible_verb_has_one_to_three_examples() {
        for verb in visible_verbs() {
            let count = lines_of(&verb).len();
            assert!(
                (1..=3).contains(&count),
                "`shep {verb}` has {count} examples, wants one to three"
            );
        }
    }

    #[test]
    fn every_example_parses() {
        for (verb, line) in every_line() {
            let argv = shell_split(line);
            if let Err(err) = Cli::try_parse_from(&argv) {
                panic!("`shep {verb}` example {line:?} does not parse:\n{err}");
            }
        }
    }

    #[test]
    fn no_example_names_a_verb_that_does_not_exist() {
        let verbs = Cli::command()
            .get_subcommands()
            .map(|verb| verb.get_name().to_owned())
            .collect::<Vec<_>>();
        for (verb, line) in every_line() {
            assert!(
                verbs.iter().any(|name| name == verb),
                "the table holds examples for `{verb}`, which is not a verb"
            );
            let argv = shell_split(line);
            assert_eq!(
                argv.get(..2),
                Some(["shep".to_owned(), verb.to_owned()].as_slice()),
                "{line:?} is filed under `{verb}` but does not start `shep {verb}`"
            );
        }
    }

    #[test]
    fn no_example_has_an_em_dash() {
        for (verb, line) in every_line() {
            assert!(!line.contains('\u{2014}'), "`{verb}`: {line:?}");
        }
    }

    #[test]
    fn examples_sit_above_the_global_flags_footer() {
        let mut root = with_global_footer(Cli::command());
        root.build();
        for verb in visible_verbs() {
            let command = root.find_subcommand(&verb).expect("a verb of the tree");
            for (kind, page) in [
                ("-h", command.clone().render_help().to_string()),
                ("--help", command.clone().render_long_help().to_string()),
            ] {
                let heading = page
                    .find("Examples:")
                    .unwrap_or_else(|| panic!("`shep {verb} {kind}` has no examples:\n{page}"));
                let footer = page
                    .find("Global flags:")
                    .unwrap_or_else(|| panic!("`shep {verb} {kind}` has no footer:\n{page}"));
                assert!(heading < footer, "`shep {verb} {kind}`:\n{page}");
                assert_eq!(
                    page.matches("Examples:").count(),
                    1,
                    "{verb} {kind}:\n{page}"
                );
                for line in lines_of(&verb) {
                    assert!(
                        page[heading..footer].contains(&format!("\n  $ {line}\n")),
                        "`shep {verb} {kind}` lacks {line:?}:\n{page}"
                    );
                }
                assert!(
                    page.trim_end().ends_with("See shep help flags"),
                    "`shep {verb} {kind}`:\n{page}"
                );
            }
        }
    }
}
