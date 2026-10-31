//! `--host`: which verbs may reach another machine's shepherd, and how a
//! verb dials the one it is told to.
//!
//! Every verb is classified by [`host_scope`], and a [`HostScope::LocalOnly`]
//! verb is refused before it touches anything. The failure this exists to
//! prevent is a verb that ignores `--host` and acts on this machine.

use std::time::Duration;

use shep_client::SshOptions;
use shep_core::paths::ShepPaths;

use crate::cli::{Commands, Format};
use crate::client::emit_error_locked;
use crate::exit::ExitCode;

/// What `--host` means to a verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostScope {
    /// The verb dials the host named by `--host`.
    Reaches,
    /// The verb acts on this machine, so `--host` is refused.
    LocalOnly,
}

/// Whether `command` reaches the host `--host` names.
///
/// No wildcard arm: a new verb fails to compile until someone decides, and
/// each slice that lets a verb reach a host moves it to
/// [`HostScope::Reaches`] here and nowhere else.
pub(crate) fn host_scope(command: &Commands) -> HostScope {
    match command {
        Commands::Flock(_)
        | Commands::Describe(_)
        | Commands::Fold(_)
        | Commands::Stop(_)
        | Commands::Thatlldo(_)
        | Commands::Restart(_)
        | Commands::Reset(_)
        | Commands::Reload(_)
        | Commands::Delete(_)
        | Commands::Disable(_)
        | Commands::Stock(_)
        | Commands::Trigger(_)
        | Commands::Signal(_)
        | Commands::Whisper(_)
        | Commands::Bleats(_)
        | Commands::Lookout(_) => HostScope::Reaches,
        // The hidden `--exec` spelling is `adopt`, which writes this
        // machine's `shep.toml`.
        Commands::Enable(args) if args.exec.is_some() => HostScope::LocalOnly,
        Commands::Enable(_) => HostScope::Reaches,
        // Reaches for a sheep name only. A token cannot be classed until the
        // remote's flock is listed, so the load path refuses the rest.
        Commands::Start(_) => HostScope::Reaches,
        // The one verb that boots a host's shepherd, from that host's roll.
        // `save` needs no boot and is not part of that.
        Commands::Muster => HostScope::Reaches,
        Commands::Add(_)
        | Commands::Serve(_)
        | Commands::Dogs(_)
        | Commands::Adopt(_)
        | Commands::Rehome(_)
        | Commands::Whistle
        | Commands::Reopen(_)
        | Commands::Flush(_)
        | Commands::Barks(_)
        | Commands::Kv(_)
        | Commands::Set(_)
        | Commands::Get(_)
        | Commands::Unset(_)
        | Commands::Secret(_)
        | Commands::Host(_)
        | Commands::Copy(_)
        | Commands::Ping
        | Commands::Kill
        | Commands::Save
        | Commands::Init(_)
        | Commands::Runtime(_)
        | Commands::Dev(_)
        | Commands::Import(_)
        | Commands::Startup(_)
        | Commands::Unstartup(_)
        | Commands::Completions(_)
        | Commands::Welcome
        | Commands::Style(_)
        | Commands::Daemon(_)
        | Commands::Dog(_)
        | Commands::Schema
        | Commands::DialStdio(_) => HostScope::LocalOnly,
    }
}

/// The one refusal every [`HostScope::LocalOnly`] verb gets.
///
/// Names no supported verb, so widening [`host_scope`] never edits it.
pub(crate) const HOST_REFUSAL: &str = "`--host` is not supported by this command yet, \
     and it would have run on this machine, not on the host";

/// Refuses a [`HostScope::LocalOnly`] verb: one error, exit
/// [`ExitCode::Usage`], and nothing touched.
pub(crate) fn refuse_host(fmt: Format) -> ExitCode {
    emit_error_locked(fmt, ExitCode::Usage, HOST_REFUSAL);
    ExitCode::Usage
}

/// `word` as one argument of a command line the remote's POSIX shell reads,
/// for a hint an operator pastes after `ssh <dest>`.
///
/// Not [`crate::commands`]'s own quoting: that one is unix-only, and this
/// machine's OS says nothing about the host's shell.
pub(crate) fn remote_word(word: &str) -> String {
    let safe = |b: &u8| b.is_ascii_alphanumeric() || b"_./:@%+=-".contains(b);
    if !word.is_empty() && word.as_bytes().iter().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The `ssh` line that runs `words` on `dest`, for an operator to paste.
///
/// Two shells read it: this machine's, which strips one layer of quoting, and
/// the host's, which gets the words joined by spaces. A word that needs
/// quoting is therefore quoted for the host, and the whole command again for
/// this machine. A line of safe words stays unquoted.
pub(crate) fn ssh_hint(dest: &str, words: &[String]) -> String {
    let quoted: Vec<String> = words.iter().map(|word| remote_word(word)).collect();
    let command = quoted.join(" ");
    if quoted
        .iter()
        .zip(words)
        .all(|(quoted, word)| quoted == word)
    {
        format!("ssh {dest} {command}")
    } else {
        format!("ssh {dest} {}", remote_word(&command))
    }
}

/// How long a followed host link waits for an interrupt before it reports the
/// link lost.
///
/// The ssh child shares the terminal's process group, so one Ctrl+C ends the
/// link at the same moment it is delivered to this process, and the link can
/// be seen ending first.
pub(crate) const HOST_INTERRUPT_GRACE: Duration = Duration::from_millis(250);

/// Whether `interrupt` resolves within [`HOST_INTERRUPT_GRACE`].
///
/// `interrupt` must resolve only for the operator's interrupt: any other
/// completion would pass for one and turn a lost link into a success.
pub(crate) async fn interrupt_within_grace(interrupt: impl Future<Output = ()>) -> bool {
    tokio::time::timeout(HOST_INTERRUPT_GRACE, interrupt)
        .await
        .is_ok()
}

/// Where a verb's shepherd is: this machine's socket, or a host over ssh.
///
/// Only a [`HostScope::Reaches`] verb is ever given [`Self::Host`].
#[derive(Debug)]
pub(crate) enum Link<'a> {
    /// The shepherd at `paths.socket`.
    Local(&'a ShepPaths),
    /// The shepherd on `dest`, dialled through `options`.
    Host {
        /// The ssh destination, already checked by `--host`'s parser.
        dest: &'a str,
        /// How ssh is run.
        options: SshOptions,
    },
}

impl<'a> Link<'a> {
    /// The link `--host` selects: the host when one is named, else `paths`.
    pub(crate) fn new(paths: &'a ShepPaths, host: Option<&'a str>) -> Self {
        match host {
            Some(dest) => Self::to_host(dest),
            None => Self::Local(paths),
        }
    }

    /// The link to `dest`, for a verb that runs before this machine's home is
    /// resolved because it never uses it.
    pub(crate) fn to_host(dest: &'a str) -> Self {
        Self::Host {
            dest,
            options: SshOptions::default(),
        }
    }

    /// The link to `dest` that boots the host's shepherd from its own roll
    /// when none answers. Only `muster` takes it.
    pub(crate) fn to_host_booting(dest: &'a str) -> Self {
        Self::Host {
            dest,
            options: SshOptions::default().autostart(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::Cli;

    /// At least one argv per verb, first word the verb's name. A new verb fails
    /// [`every_verb_has_a_sample_argv_here`] until it has a row.
    const SAMPLES: &[&[&str]] = &[
        &["start", "web"],
        &["add", "web"],
        &["serve", "dist"],
        &["stop", "web"],
        &["restart", "web"],
        &["reset", "web"],
        &["reload", "web"],
        &["delete", "web"],
        &["stock", "web", "2"],
        &["flock"],
        &["dogs"],
        &["dogs", "grant", "otel", "flock.read"],
        &["dogs", "revoke", "otel", "flock.read"],
        &["enable", "metrics"],
        &["disable", "metrics"],
        &["adopt", "/bin/true"],
        &["rehome", "metrics"],
        &["describe", "web"],
        &["trigger", "web", "run"],
        &["signal", "web", "TERM"],
        &["whisper", "web", "hi"],
        &["fold", "web"],
        &["bleats", "web"],
        &["lookout"],
        &["whistle"],
        &["reopen"],
        &["flush", "web"],
        &["barks"],
        &["kv", "get", "k"],
        &["set", "k", "v"],
        &["get", "k"],
        &["unset", "k"],
        &["secret", "list"],
        &["host", "list"],
        &["copy", "web", "web2"],
        &["ping"],
        &["kill"],
        &["save"],
        &["muster"],
        &["init"],
        &["runtime"],
        &["dev"],
        &["import", "env", "--app", "web", ".env"],
        &["startup"],
        &["unstartup"],
        &["completions", "bash"],
        &["welcome"],
        &["style"],
        &["thatlldo", "web"],
        &["daemon"],
        &["dog", "metrics"],
        &["schema"],
        &["dial-stdio"],
    ];

    fn parsed_with_host(sample: &[&str]) -> Cli {
        let argv: Vec<&str> = ["shep", "--host", "amp2"]
            .into_iter()
            .chain(sample.iter().copied())
            .collect();
        Cli::try_parse_from(&argv).unwrap_or_else(|err| panic!("{argv:?} must parse: {err}"))
    }

    /// Every argv that reaches a host.
    const REACHING: &[&[&str]] = &[
        &["flock"],
        &["flock", "--follow"],
        &["describe", "web"],
        &["fold", "web"],
        &["bleats", "web"],
        &["bleats", "web", "--no-follow"],
        &["lookout"],
    ];

    /// Every argv of a verb that changes the flock and reaches a host.
    const CHANGING: &[&[&str]] = &[
        &["stop", "web"],
        &["thatlldo", "web"],
        &["restart", "web"],
        &["reload", "web"],
        &["delete", "web"],
    ];

    /// Every argv of a verb that marks, restocks or messages a sheep, and
    /// reaches a host.
    const MARKING: &[&[&str]] = &[
        &["enable", "web"],
        &["disable", "web"],
        &["stock", "web", "2"],
        &["signal", "web", "TERM"],
        &["whisper", "web", "hi"],
        &["trigger", "web", "run"],
    ];

    /// Every argv of `start`, which reaches a host for a sheep name only.
    const STARTING: &[&[&str]] = &[&["start", "web"], &["start"]];

    /// Every argv of `muster`, the verb that boots a host's shepherd.
    const MUSTERING: &[&[&str]] = &[&["muster"]];

    /// Every argv of `reset`, which has no dog half and so reaches a host.
    const RESETTING: &[&[&str]] = &[&["reset", "web"], &["reset", "web", "api"]];

    #[test]
    fn reset_reaches_a_host() {
        for argv in RESETTING {
            let cli = parsed_with_host(argv);
            assert_eq!(host_scope(&cli.command), HostScope::Reaches, "{argv:?}");
        }
    }

    #[test]
    fn start_reaches_a_host_and_add_does_not() {
        for argv in STARTING {
            let cli = parsed_with_host(argv);
            assert_eq!(host_scope(&cli.command), HostScope::Reaches, "{argv:?}");
        }
        let add = parsed_with_host(&["add", "web"]);
        assert_eq!(host_scope(&add.command), HostScope::LocalOnly);
    }

    #[test]
    fn lookout_reaches_a_host() {
        let cli = parsed_with_host(&["lookout"]);
        assert_eq!(host_scope(&cli.command), HostScope::Reaches);
    }

    #[test]
    fn muster_reaches_a_host_and_save_does_not() {
        let muster = parsed_with_host(&["muster"]);
        assert_eq!(host_scope(&muster.command), HostScope::Reaches);
        let save = parsed_with_host(&["save"]);
        assert_eq!(host_scope(&save.command), HostScope::LocalOnly);
    }

    #[test]
    fn a_remote_word_is_quoted_only_when_the_shell_would_split_it() {
        assert_eq!(remote_word("./api"), "./api");
        assert_eq!(remote_word("my api"), "'my api'");
        assert_eq!(remote_word("it's"), r"'it'\''s'");
        assert_eq!(remote_word(""), "''");
    }

    #[test]
    fn an_ssh_hint_quotes_for_both_shells_only_when_a_word_needs_it() {
        let words = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            ssh_hint("amp2", &words(&["shep", "start", "./api"])),
            "ssh amp2 shep start ./api"
        );
        assert_eq!(
            ssh_hint("amp2", &words(&["shep", "start", "./my api.js"])),
            r"ssh amp2 'shep start '\''./my api.js'\'''"
        );
    }

    #[test]
    fn enable_disable_stock_signal_whisper_and_trigger_reach_a_host() {
        for argv in MARKING {
            let cli = parsed_with_host(argv);
            assert_eq!(host_scope(&cli.command), HostScope::Reaches, "{argv:?}");
        }
    }

    /// The hidden `--exec` spelling is `adopt`, which writes this machine's
    /// `shep.toml`.
    #[test]
    fn enable_with_the_hidden_exec_form_is_refused_over_a_host() {
        let cli = parsed_with_host(&["enable", "mydog", "--exec", "/bin/true"]);
        assert_eq!(host_scope(&cli.command), HostScope::LocalOnly);
    }

    #[test]
    fn stop_restart_reload_and_delete_reach_a_host() {
        for argv in CHANGING {
            let cli = parsed_with_host(argv);
            assert_eq!(host_scope(&cli.command), HostScope::Reaches, "{argv:?}");
        }
    }

    #[test]
    fn read_verbs_reach_a_host() {
        for argv in REACHING {
            let cli = parsed_with_host(argv);
            assert_eq!(host_scope(&cli.command), HostScope::Reaches, "{argv:?}");
        }
        for sample in SAMPLES {
            if REACHING
                .iter()
                .chain(CHANGING)
                .chain(MARKING)
                .chain(STARTING)
                .chain(MUSTERING)
                .chain(RESETTING)
                .any(|argv| argv[0] == sample[0])
            {
                continue;
            }
            let cli = parsed_with_host(sample);
            assert_eq!(host_scope(&cli.command), HostScope::LocalOnly, "{sample:?}");
        }
    }

    /// A verb added to the tree without a row here would be classified by
    /// the compiler but never parsed with `--host`, so the table is held
    /// against the tree.
    #[test]
    fn every_verb_has_a_sample_argv_here() {
        let mut in_tree: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_owned())
            .filter(|name| name != "help")
            .collect();
        let mut sampled: Vec<String> = SAMPLES.iter().map(|row| row[0].to_owned()).collect();
        in_tree.sort();
        sampled.sort();
        sampled.dedup();
        assert_eq!(sampled, in_tree);
    }
}
