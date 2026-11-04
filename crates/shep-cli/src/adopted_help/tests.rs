use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{CommandFactory as _, Parser as _};
use shep_core::dogs::manifest::{Manifest, Subcommand, Verb};
use shep_core::paths::ShepPaths;

use super::*;
use crate::cli::{Cli, Format, GlobalArgs};
use crate::commands::dogs::read_manifest;
use crate::commands::shep_toml::ShepToml;
use crate::home::resolve_paths;

/// The heading `shep --help` files `dogs`, `adopt` and `whistle` under.
const DOGS_HEADING: &str = "Dogs and agents";

/// Generous: the question is the answer, not the clock.
const BUDGET: Duration = Duration::from_secs(10);

/// A dog that answers `--shep-manifest` with a `verb`, and nothing else.
const DECLARING_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-otel","version":"0.2.0","protocol":12,"verb":{"description":"exports traces to an OTLP collector","subcommands":[{"name":"status","about":"show pipeline status"},{"name":"flush","about":"flush pending spans"}]}}'
fi"#;

/// A dog that answers no flag at all: adopted, and undeclared.
const SILENT_BODY: &str = "";

/// A dog whose `verb` names subcommands a shell cannot take as bare words: a
/// command substitution, and a `)` that closes a `case` pattern early. Safe
/// names here are `pipeline` and `drain`, words no other part of a generated
/// script contains.
const SHELL_UNSAFE_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-otel","version":"0.2.0","protocol":12,"verb":{"description":"exports traces","subcommands":[{"name":"pipeline","about":"show the pipeline"},{"name":"$(touch>marker)","about":"a command substitution"},{"name":"two)words","about":"a case pattern breaker"},{"name":"drain","about":"empty the export queue"}]}}'
fi"#;

/// A dog whose `verb` carries clap's own placeholder text, a line break, an
/// escape and a subcommand name that is two words.
const HOSTILE_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-otel","version":"0.2.0","protocol":12,"verb":{"description":"spans {all-args} and\u000amore\u001b[2J","subcommands":[{"name":"one word","about":"not one word"},{"name":"status","about":"show {options}"}]}}'
fi"#;

fn write_dog(dir: &Path, name: &str, body: &str) -> PathBuf {
    let binary = dir.join(name);
    std::fs::write(&binary, format!("#!/bin/sh\n{body}\nexit 0\n")).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

/// A `$SHEP_HOME` under `dir`, resolved to the paths `adopted_verbs` reads,
/// with each `(name, body)` dog adopted into its `shep.toml` and probed the
/// way `shep adopt` probes. The manifest behind each one is therefore a
/// cached answer, not a fixture written by hand.
fn a_home_with<'a>(dir: &Path, dogs: impl IntoIterator<Item = (&'a str, &'a str)>) -> ShepPaths {
    let paths = resolve_paths(&GlobalArgs {
        host: None,
        home: Some(dir.join(".shep")),
        format: Format::Table,
        quiet: false,
        style: None,
    })
    .expect("an absolute --home resolves");

    for (name, body) in dogs {
        let binary = write_dog(dir, name, body);
        ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog(name, &binary, false).unwrap();
        })
        .unwrap();
        read_manifest(&binary, &paths.home, name, BUDGET).unwrap();
    }
    paths
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn listed(verbs: &[AdoptedVerb]) -> Vec<(&str, Option<&str>)> {
    verbs
        .iter()
        .map(|verb| (verb.name.as_str(), verb.description.as_deref()))
        .collect()
}

mod receipt;

#[test]
fn a_manifest_dog_is_listed_with_its_description_and_an_undeclared_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(
        dir.path(),
        [("otel", DECLARING_BODY), ("log-rotate", SILENT_BODY)],
    );

    let verbs = adopted_verbs(&paths);

    assert_eq!(
        listed(&verbs),
        vec![
            ("log-rotate", None),
            ("otel", Some("exports traces to an OTLP collector")),
        ],
        "both dogs are listed, in name order, and only the one with a manifest carries a description"
    );
    assert_eq!(
        verbs
            .iter()
            .find(|verb| verb.name == "otel")
            .map(|verb| verb
                .subcommands
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()),
        Some(vec!["status", "flush"]),
        "the subcommands a manifest declares come with the dog"
    );
}

/// `shep adopt` refuses a name a verb owns; a hand-written `shep.toml` can
/// still hold one, and clap's verb wins the token every time.
#[test]
fn a_dog_named_like_a_verb_is_left_out_of_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("list", SILENT_BODY)]);

    assert_eq!(adopted_verbs(&paths), Vec::<AdoptedVerb>::new());
}

/// `shep --help` as `list_in_help` renders it for `verbs`.
fn help_with(verbs: &[AdoptedVerb]) -> String {
    let mut command = Cli::command();
    list_in_help(&mut command, verbs, true);
    command.render_long_help().to_string()
}

/// The lines of the Dogs group, heading first, up to the blank line.
fn dogs_group(help: &str) -> Vec<&str> {
    help.lines()
        .skip_while(|line| *line != DOGS_HEADING)
        .take_while(|line| !line.is_empty())
        .collect()
}

#[test]
fn the_dogs_join_the_dogs_group_of_the_rendered_help() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(
        dir.path(),
        [("otel", DECLARING_BODY), ("log-rotate", SILENT_BODY)],
    );
    let help = help_with(&adopted_verbs(&paths));

    let group = dogs_group(&help);
    let names: Vec<&str> = group
        .iter()
        .skip(1)
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "dogs",
            "install",
            "upgrade",
            "adopt",
            "whistle",
            "log-rotate",
            "otel"
        ],
        "the dogs sit under the group's own verbs, in name order"
    );
    assert_eq!(
        group[6], "  log-rotate",
        "a dog with no description is its name alone"
    );
    assert!(
        group[7].ends_with("  exports traces to an OTLP collector"),
        "{}",
        group[7]
    );
}

#[test]
fn a_home_with_no_adopted_dogs_lists_only_the_groups_own_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), []);
    assert!(adopted_verbs(&paths).is_empty());

    assert_eq!(dogs_group(&help_with(&[])).len(), 1 + 5);
}

/// A dog's description starts in the column the group's verbs' abouts do.
#[test]
fn a_dog_lines_up_with_the_verbs_in_its_group() {
    let help = help_with(&[AdoptedVerb {
        name: "otel".to_string(),
        description: Some("exports traces".to_string()),
        subcommands: Vec::new(),
    }]);
    let group = dogs_group(&help);
    let column = |line: &str, about: &str| {
        crate::output::width::text_columns(&line[..line.find(about).unwrap()])
    };

    let verb = group.iter().find(|l| l.starts_with("  dogs ")).unwrap();
    let dog = group.iter().find(|l| l.starts_with("  otel ")).unwrap();
    assert_eq!(
        column(dog, "exports traces"),
        column(verb, "List the dogs"),
        "{group:#?}"
    );
}

#[test]
fn completion_scripts_offer_an_adopted_dog_and_its_declared_subcommands() {
    use clap_complete::aot::Shell;

    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("otel", DECLARING_BODY)]);
    let verbs = adopted_verbs(&paths);

    let mut command = with_completion_entries(Cli::command(), &verbs);
    let mut buf = Vec::new();
    clap_complete::aot::generate(Shell::Bash, &mut command, "shep", &mut buf);
    let script = String::from_utf8(buf).unwrap();

    for word in ["otel", "status", "flush"] {
        assert!(script.contains(word), "`{word}` is missing from the script");
    }
}

#[test]
fn shep_answers_only_the_bare_help_forms() {
    for (before, argv) in [
        (&[][..], &["--help"][..]),
        (&[], &["-h"]),
        (&["help"], &[]),
        (&["--home", "help", "help"], &[]),
        (&["--format=json", "help"], &[]),
    ] {
        assert!(
            help_asked(&os(before), &os(argv)),
            "{before:?} <dog> {argv:?} is shep's page"
        );
    }

    for (before, argv) in [
        (&[][..], &["status"][..]),
        (&[], &[]),
        (&[], &["help"]),
        (&[], &["--help", "status"]),
        (&[], &["status", "--help"]),
        (&[], &["-hx"]),
        (&["help"], &["status"]),
        (&["--home", "help"], &[]),
        (&["flock"], &[]),
    ] {
        assert!(
            !help_asked(&os(before), &os(argv)),
            "{before:?} <dog> {argv:?} is the dog's own to answer"
        );
    }
}

#[test]
fn the_declared_subcommands_are_what_a_dogs_own_help_page_lists() {
    let mut verb = Verb::new("exports traces to an OTLP collector");
    verb.subcommands
        .push(Subcommand::new("status", "show pipeline status"));
    let help = dog_help_text("otel", &verb);

    assert!(help.contains("Usage: shep otel"), "{help}");
    assert!(
        help.contains("exports traces to an OTLP collector"),
        "{help}"
    );
    assert!(help.contains("status  show pipeline status"), "{help}");
    assert!(
        !help.contains("\n  help\n"),
        "a dog's verb has no shep subcommands of its own to list: {help}"
    );
}

#[test]
fn top_level_help_asked_reads_the_argv_a_parse_never_finished() {
    for argv in [
        os(&["shep"]),
        os(&["shep", "--help"]),
        os(&["shep", "-h"]),
        os(&["shep", "--home", "/tmp/s", "help"]),
        os(&["shep", "flock", "--help"]),
    ] {
        assert!(top_level_help_asked(&argv), "{argv:?}");
    }

    for argv in [
        os(&["shep", "flock"]),
        os(&["shep", "stop", "web"]),
        // A flag whose text merely contains the help flag's.
        os(&["shep", "--helper"]),
    ] {
        assert!(!top_level_help_asked(&argv), "{argv:?}");
    }
}

#[test]
fn a_manifest_that_declares_no_verb_leaves_the_dog_bare() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-otel","version":"0.2.0","protocol":12}'
fi"#;
    let paths = a_home_with(dir.path(), [("otel", body)]);

    assert_eq!(listed(&adopted_verbs(&paths)), vec![("otel", None)]);
}

#[test]
fn a_recorded_binary_that_is_gone_still_lists_the_dog() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("otel", DECLARING_BODY)]);
    std::fs::remove_file(dir.path().join("otel")).unwrap();

    assert_eq!(listed(&adopted_verbs(&paths)), vec![("otel", None)]);
}

/// The cache is keyed by the binary's sha256, so a listing reads the answer
/// `shep adopt` already paid for. Nothing here spawns a dog: a help screen or
/// a completion script for a binary that runs on being run at all would run
/// it, and an adopted dog is not vetted for what it does on a `--help`.
#[test]
fn the_listing_never_runs_a_dog() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("runs");
    let body = format!("echo run >> \"{}\"\n{DECLARING_BODY}", counter.display());
    let paths = a_home_with(dir.path(), [("otel", body.as_str())]);
    let runs = || std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs(), 1, "the adopt probe is the only spawn so far");

    let verbs = adopted_verbs(&paths);

    assert_eq!(
        verbs[0].description.as_deref(),
        Some("exports traces to an OTLP collector"),
        "the cached answer is what the listing reads"
    );
    assert_eq!(runs(), 1, "the listing spawned nothing");
}

/// The listing is a read of `shep.toml` and the cache. A `shep.toml` that
/// will not parse is the verb that reads it to report, not `--help`'s.
#[test]
fn an_unreadable_shep_toml_lists_nothing_rather_than_failing_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".shep");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("shep.toml"), "not = = toml").unwrap();
    let paths = resolve_paths(&GlobalArgs {
        host: None,
        home: Some(home),
        format: Format::Table,
        quiet: false,
        style: None,
    })
    .unwrap();

    assert_eq!(adopted_verbs(&paths), Vec::<AdoptedVerb>::new());
}

/// `Manifest` is what the cache holds and `AdoptedVerb` is what a listing
/// needs: a `defaults` section carrying a credential never travels with it.
#[test]
fn an_adopted_verb_debug_carries_no_manifest_body() {
    let mut manifest = Manifest::new("shep-otel", "0.2.0", 12);
    manifest.verb = Some(Verb::new("exports traces"));
    manifest.defaults = Some("token = \"hunter2\"\n".to_string());

    let verb = AdoptedVerb {
        name: "otel".to_string(),
        description: manifest.verb.map(|verb| verb.description),
        subcommands: Vec::new(),
    };

    assert_eq!(
        format!("{verb:?}"),
        "AdoptedVerb { name: \"otel\", description: Some(\"exports traces\"), subcommands: [] }"
    );
}

/// The finding this covers: a manifest is written by whoever built the dog,
/// and its text is spliced into a template clap parses. `{all-args}` there is
/// not copy, it is the options section, and a line break is not a wrap, it is
/// a line of output shep never wrote.
#[test]
fn a_manifest_carrying_a_placeholder_or_a_line_break_is_listed_as_text() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("otel", HOSTILE_BODY)]);

    let verbs = adopted_verbs(&paths);
    assert_eq!(
        verbs[0].description.as_deref(),
        Some("spans all-args and more[2J"),
        "the placeholder braces go, the line break becomes a space, the escape goes and its tail stays inert text"
    );
    assert_eq!(
        verbs[0]
            .subcommands
            .iter()
            .map(|sub| sub.name.as_str())
            .collect::<Vec<_>>(),
        vec!["status"],
        "a subcommand name that is two words is not a subcommand"
    );

    let listed = help_with(&verbs);
    let plain = help_with(&[]);
    let at = plain.lines().position(|line| line == DOGS_HEADING).unwrap();
    let end = at + dogs_group(&plain).len();
    let column = plain
        .lines()
        .find(|line| line.starts_with("  dogs "))
        .and_then(|line| line.find("List the dogs"))
        .expect("the dogs verb is listed with its about");
    let dog_line = format!("  {:<1$}spans all-args and more[2J", "otel", column - 2);
    let expected: Vec<&str> = plain
        .lines()
        .take(end)
        .chain([dog_line.as_str()])
        .chain(plain.lines().skip(end))
        .collect();

    assert_eq!(listed.lines().collect::<Vec<_>>(), expected, "{listed}");
    assert_eq!(
        listed.matches("Options").count(),
        plain.matches("Options").count(),
        "the spliced line rendered no extra section"
    );
}

/// The other render a manifest reaches: `shep <dog> --help`, built out of
/// clap's own template. `{options}` in a subcommand's one-liner is a value
/// here rather than a tag, and the cleaned text is what both paths print. The
/// page lists no options of its own, so a section of them is the injection.
#[test]
fn a_dogs_own_help_page_lists_the_cleaned_text_and_no_options_section() {
    let mut verb = Verb::new("spans {all-args} and\nmore\u{1b}[2J");
    verb.subcommands
        .push(Subcommand::new("status", "show {options}"));
    let cleaned = printable_verb(verb).expect("it declares a subcommand");

    let help = dog_help_text("otel", &cleaned);

    assert!(help.contains("spans all-args and more"), "{help}");
    assert!(help.contains("status  show options"), "{help}");
    assert_eq!(help.matches("Options:").count(), 0, "{help}");
}

/// A `verb` whose copy is nothing but a placeholder, or nothing but an
/// escape, declares nothing. The dog is listed bare rather than with an
/// empty line beside its name. A stripped escape's printable tail does
/// survive, which is [`crate::terminal_safe::sanitise`]'s rule, so `[2J` is
/// copy and not a reason to drop the verb.
#[test]
fn a_verb_that_declares_nothing_once_cleaned_is_no_verb() {
    for empty in ["{}", "{}{}", "\u{1b}", "\u{200b}\u{202e}"] {
        assert!(
            printable_verb(Verb::new(empty)).is_none(),
            "{empty:?} cleaned to something"
        );
    }
    assert_eq!(
        printable_verb(Verb::new("\u{1b}[2J"))
            .map(|verb| verb.description)
            .as_deref(),
        Some("[2J"),
        "the printable tail of a stripped escape stays as inert text"
    );
}

/// A name the listing would have to rewrite is not the name the operator
/// types, so the dog is not listed at all. `shep adopt` cannot produce one;
/// a hand-written `shep.toml` can.
#[test]
fn a_dog_name_the_listing_would_have_to_rewrite_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".shep");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("shep.toml"),
        "[daemon.adopted_dogs]\n\"otel{all-args}\" = \"/bin/true\"\n",
    )
    .unwrap();
    let paths = resolve_paths(&GlobalArgs {
        host: None,
        home: Some(home),
        format: Format::Table,
        quiet: false,
        style: None,
    })
    .unwrap();

    assert_eq!(adopted_verbs(&paths), Vec::<AdoptedVerb>::new());
}

/// `clap_complete` writes subcommand names unquoted, so a shell sourcing the
/// script would run `$(…)` and a `)` would stop the bash script parsing.
///
/// An unsafe name is dropped from the script. The dog's own help page keeps
/// it, because the manifest declared it.
#[test]
fn a_name_a_shell_cannot_take_as_a_bare_word_is_never_completed() {
    use clap_complete::aot::Shell;

    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("otel", SHELL_UNSAFE_BODY)]);
    let verbs = adopted_verbs(&paths);

    assert_eq!(
        verbs[0]
            .subcommands
            .iter()
            .map(|sub| sub.name.as_str())
            .collect::<Vec<_>>(),
        vec!["pipeline", "$(touch>marker)", "two)words", "drain"],
        "the help surface stays faithful to what the manifest declared"
    );

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let mut command = with_completion_entries(Cli::command(), &verbs);
        let mut buf = Vec::new();
        clap_complete::aot::generate(shell, &mut command, "shep", &mut buf);
        let script = String::from_utf8(buf).unwrap();
        for safe in ["pipeline", "drain"] {
            assert!(script.contains(safe), "{shell}: `{safe}` must be completed");
        }
        for dangerous in ["$(touch>marker)", "two)words"] {
            assert!(
                !script.contains(dangerous),
                "{shell}: `{dangerous}` is code to a shell reading the script: {script}"
            );
        }
    }
}

/// The same rule for the dog's own name, which reaches the script as a
/// top-level completion word. `shep adopt --name` takes any string, and a
/// hand-written `shep.toml` any key.
#[test]
fn a_dog_named_what_a_shell_cannot_take_is_listed_but_never_completed() {
    use clap_complete::aot::Shell;

    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(
        dir.path(),
        [("$(touch>marker)", SILENT_BODY), ("otel", DECLARING_BODY)],
    );
    let verbs = adopted_verbs(&paths);

    assert_eq!(
        listed(&verbs),
        vec![
            ("$(touch>marker)", None),
            ("otel", Some("exports traces to an OTLP collector"))
        ],
        "a running dog stays in `shep --help`, which prints text rather than writing a script"
    );

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let mut command = with_completion_entries(Cli::command(), &verbs);
        let mut buf = Vec::new();
        clap_complete::aot::generate(shell, &mut command, "shep", &mut buf);
        let script = String::from_utf8(buf).unwrap();
        assert!(
            script.contains("otel"),
            "{shell}: the safe dog must be completed"
        );
        assert!(
            !script.contains("touch>marker"),
            "{shell}: the unsafe dog must not be: {script}"
        );
    }
}

/// The grammar, named: a word a shell takes as a bare word. Letters of any
/// script and digits, separated by `-`, `_` or `.`, starting on a letter or
/// digit. Everything else, `$(…)` and `)` included, is out.
#[test]
fn shell_safe_is_the_words_a_shell_takes_as_a_bare_word() {
    for safe in [
        "otel",
        "log-rotate",
        "log_rotate",
        "v1.2",
        "drain2",
        "A1",
        "café",
        "日本語",
    ] {
        assert!(shell_safe(safe), "{safe} must be completed");
    }
    for dangerous in [
        "",
        "-x",
        "_x",
        ".hidden",
        "$(touch>marker)",
        "two)words",
        "a'b",
        "a\"b",
        "a;b",
        "a|b",
        "a*b",
        "a$b",
        "a`b",
        "a b",
        "a/b",
        "a:b",
        "a\nb",
    ] {
        assert!(!shell_safe(dangerous), "{dangerous} must not be completed");
    }
}

/// The Dogs group is a table, and a table is columns. `café` is five bytes
/// and four columns, `日本語` is nine bytes and six, so padding by byte
/// length starts one description three columns past another.
#[test]
fn dog_descriptions_line_up_by_display_width_and_not_byte_length() {
    use crate::output::width::text_columns;

    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(
        dir.path(),
        [
            ("otel", DECLARING_BODY),
            ("café", DECLARING_BODY),
            ("日本語", DECLARING_BODY),
        ],
    );

    let mut command = Cli::command();
    list_in_help(&mut command, &adopted_verbs(&paths), true);
    let help = command.render_long_help().to_string();

    let description = "exports traces to an OTLP collector";
    let columns: Vec<usize> = help
        .lines()
        .filter(|line| line.ends_with(description))
        .map(|line| text_columns(&line[..line.find(description).unwrap()]))
        .collect();

    assert_eq!(columns.len(), 3, "{help}");
    assert_eq!(
        columns,
        vec![columns[0]; 3],
        "every description starts in the same column: {help}"
    );
}
/// A `shep.toml` written by hand can name a dog `help`, and clap's own
/// `help` subcommand wins that token every time: `shep help` prints shep's
/// help and the dog is never reached. Listing it promises a command that
/// cannot run, which is what `adopted_verbs` says its filter prevents.
#[test]
fn a_dog_named_help_is_left_out_of_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".shep");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("shep.toml"),
        "[daemon.adopted_dogs]\nhelp = \"/bin/true\"\n",
    )
    .unwrap();
    let paths = resolve_paths(&GlobalArgs {
        host: None,
        home: Some(home),
        format: Format::Table,
        quiet: false,
        style: None,
    })
    .unwrap();

    assert_eq!(adopted_verbs(&paths), Vec::<AdoptedVerb>::new());
}

/// The verb the help standard's own example draws: a usage line on one
/// subcommand, two examples.
const KELPIE_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-kelpie","version":"0.9.2","protocol":12,"verb":{"description":"Run agents on a ticket board","subcommands":[{"name":"board","about":"show the board"},{"name":"rule","about":"answer a ruling","usage":"<id> yes|no <note>"}],"examples":["shep kelpie board","shep kelpie rule 42 yes looks good"]}}'
fi"#;

/// A dog whose manifest declares a version and no `verb`.
const VERSIONED_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-log-rotate","version":"0.2.5","protocol":12}'
fi"#;

/// An index URL nothing listens on, for a test that never fetches.
const INDEX: &str = "https://example.test/dogs.json";

/// The page `shep <name> --help` draws, as the text an operator reads.
fn page(paths: &ShepPaths, dir: &Path, name: &str) -> String {
    dog_page(name, &dir.join(name), paths, INDEX)
        .render_long_help()
        .to_string()
}

#[test]
fn a_dog_with_a_verb_gets_shep_s_page_with_examples() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("kelpie", KELPIE_BODY)]);

    assert_eq!(
        page(&paths, dir.path(), "kelpie"),
        "\
Run agents on a ticket board

Usage: shep kelpie <command> [args]

Commands:
  board                    show the board
  rule <id> yes|no <note>  answer a ruling

Examples:
  $ shep kelpie board
  $ shep kelpie rule 42 yes looks good

kelpie 0.9.2. Anything after `shep kelpie` goes to the dog.
"
    );
}

#[test]
fn a_dog_without_a_verb_gets_the_stub_not_its_own_output() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("runs");
    let body = format!("echo run >> \"{}\"\n{VERSIONED_BODY}", counter.display());
    let paths = a_home_with(dir.path(), [("log-rotate", body.as_str())]);
    let runs = || std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs(), 1, "the adopt probe is the only spawn so far");

    assert_eq!(
        page(&paths, dir.path(), "log-rotate"),
        "\
log-rotate 0.2.5

This dog declares no help. `shep log-rotate help` asks the dog itself.
"
    );

    let home = paths.home.clone().into_os_string();
    for asked in [["--help"], ["-h"]] {
        let argv: Vec<OsString> = ["shep".into(), "--home".into(), home.clone()]
            .into_iter()
            .chain(["log-rotate".into(), asked[0].into()])
            .collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert!(
            crate::adopted::dispatch_adopted_dog(&argv, &err).is_some(),
            "{argv:?} is answered"
        );
    }
    assert_eq!(runs(), 1, "no help form spawned the dog");
}

/// An index document with one row, for `package`, described by `description`.
fn index_row(package: &str, adopt_as: &str, description: &str) -> Vec<u8> {
    serde_json::json!({
        "version": 1,
        "dogs": [{
            "name": package,
            "package": package,
            "adopt_as": adopt_as,
            "description": description,
            "repo": format!("https://github.com/shep-pm/{package}"),
            "license": "MIT",
            "category": "logs",
            "source": { "kind": "cargo", "version": "0.2.5" }
        }]
    })
    .to_string()
    .into_bytes()
}

/// A loopback port that records whether anything connected to it.
fn a_listener_that_counts() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/dogs.json", listener.local_addr().unwrap());
    (listener, url)
}

#[test]
fn the_stub_reads_the_index_cache_and_never_fetches() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("log-rotate", VERSIONED_BODY)]);
    let (listener, url) = a_listener_that_counts();
    let stub = |paths: &ShepPaths| {
        dog_page("log-rotate", &dir.path().join("log-rotate"), paths, &url)
            .render_long_help()
            .to_string()
    };

    assert!(
        !stub(&paths).contains("Rotates"),
        "no cache means no description"
    );

    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        &url,
        &index_row("shep-log-rotate", "log-rotate", "Rotates logs as they grow"),
    )
    .unwrap();
    // Older than the cache's day: a stale copy is still the answer.
    std::fs::File::options()
        .write(true)
        .open(&paths.dog_index_cache)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60))
        .unwrap();

    assert_eq!(
        stub(&paths),
        "\
log-rotate 0.2.5
Rotates logs as they grow

This dog declares no help. `shep log-rotate help` asks the dog itself.
"
    );
    assert!(
        matches!(listener.accept(), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock),
        "nothing connected to the index url"
    );
}

#[test]
fn shep_dash_dash_help_lists_a_dog_with_no_verb_by_its_index_description() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(
        dir.path(),
        [("log-rotate", VERSIONED_BODY), ("quiet", SILENT_BODY)],
    );
    let (listener, url) = a_listener_that_counts();
    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        &url,
        &index_row("shep-log-rotate", "log-rotate", "Rotates logs as they grow"),
    )
    .unwrap();

    let verbs = adopted_verbs_in(&paths, &url);

    assert_eq!(
        listed(&verbs),
        vec![
            ("log-rotate", Some("Rotates logs as they grow")),
            ("quiet", None)
        ],
        "a dog the index does not describe is its name alone"
    );
    assert!(listener.accept().is_err(), "nothing was fetched");
}

/// The `(before, argv)` split `dispatch_adopted_dog` makes for `tokens`,
/// parsed by the real command.
fn split_for(tokens: &[&str]) -> (Vec<OsString>, Vec<OsString>) {
    let argv = os(tokens);
    let err = Cli::try_parse_from(&argv).unwrap_err();
    let (_, index) = crate::adopted::placed_dog(&argv, &err).expect("clap could not place a dog");
    (argv[1..index].to_vec(), argv[index + 1..].to_vec())
}

#[test]
fn shep_help_dog_matches_shep_dog_help() {
    for tokens in [
        ["shep", "kelpie", "--help"],
        ["shep", "kelpie", "-h"],
        ["shep", "help", "kelpie"],
    ] {
        let (before, argv) = split_for(&tokens);
        assert!(help_asked(&before, &argv), "{tokens:?} is shep's page");
    }

    let (before, argv) = split_for(&["shep", "--home", "/tmp/help", "help", "kelpie"]);
    assert!(help_asked(&before, &argv), "a --home is not the help verb");
}

#[test]
fn a_dogs_own_help_subcommand_still_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen");
    let body = format!(
        "if [ \"$1\" = --shep-manifest ]; then echo '{{\"manifest\":1,\"name\":\"shep-kelpie\",\"version\":\"0.9.2\",\"protocol\":12}}'; else echo \"$@\" >> \"{}\"; fi",
        seen.display()
    );
    let paths = a_home_with(dir.path(), [("kelpie", body.as_str())]);
    let home = paths.home.clone().into_os_string();

    for asked in [&["help"][..], &["board", "--help"], &["help", "board"]] {
        let argv: Vec<OsString> = [
            "shep".into(),
            "--home".into(),
            home.clone(),
            "kelpie".into(),
        ]
        .into_iter()
        .chain(asked.iter().map(OsString::from))
        .collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert!(
            crate::adopted::dispatch_adopted_dog(&argv, &err).is_some(),
            "{argv:?} is answered"
        );
    }

    assert_eq!(
        std::fs::read_to_string(&seen)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["help", "board --help", "help board"],
        "the dog got each argv as typed"
    );
}

#[test]
fn a_verb_of_examples_alone_still_gets_a_page() {
    let mut verb = Verb::new("");
    verb.examples = vec!["shep otel".to_owned(), "\u{1b}".to_owned()];

    let cleaned = printable_verb(verb).expect("an example is something to print");

    assert_eq!(cleaned.examples, ["shep otel"]);
}
