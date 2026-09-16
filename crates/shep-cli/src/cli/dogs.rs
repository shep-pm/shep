//! Arguments for the dog verbs: `dogs`, `enable`, `disable`, `adopt`,
//! `rehome`, and the hidden `dog` re-exec target.
//!
//! A dog is named, never selected, so none of these takes the selector
//! grammar the flock verbs share. Three of the six get by on the one name
//! field [`DogArgs`] carries; the docs on the other two say what they add
//! and why it was not worth widening the shared struct.

use std::path::PathBuf;

/// `shep dogs`, and the index of dogs you could adopt.
#[derive(Debug, clap::Args)]
pub struct DogsArgs {
    /// List the dogs published in the community index instead of the ones
    /// this shepherd is running. Needs no shepherd.
    #[arg(long)]
    pub available: bool,
    /// Narrow the listing to entries whose name, package or description
    /// contains this text, case-insensitively.
    #[arg(value_name = "FILTER")]
    pub filter: Option<String>,
}

/// Arguments to `shep disable`/`shep rehome`, and to the hidden `shep dog`
/// re-exec target.
///
/// One struct for all three, matching [`StartupArgs`](crate::cli::StartupArgs)'s own precedent: a
/// dog is named, never selected — `SelectorArgs`' grammar (`all`, `/regex/`,
/// `fold:<name>`) answers "which of the flock", and a dog is not the flock.
/// `shep enable` shares this shape too, but carries a second, hidden field
/// ([`EnableArgs`]) that none of the three below has any use for, so it
/// gets a struct of its own rather than widening this one for verbs that
/// would never touch the extra field.
#[derive(Debug, clap::Args)]
pub struct DogArgs {
    /// The dog's name, the `[<name>]` config key in `dogs.toml`
    pub name: String,
}

/// Arguments to `shep enable`.
///
/// [`DogArgs`] plus one hidden field: `--exec` is pm2's own spelling of
/// `shep adopt`, kept as a working alias so muscle memory carries over —
/// `#[arg(hide = true)]`, not `#[command(alias = ..)]`, because the alias
/// is on an argument, not the subcommand itself. `shep enable --exec <path>
/// <name>` parses here and is routed to [`super::commands::dogs::adopt`](crate::commands::dogs::adopt) by
/// `main`'s own dispatch, never handled by `enable` itself: a dog already
/// built into this binary has no path to vet, so `enable` cannot carry out
/// what `adopt` does.
///
/// `name` here is a required positional, unlike [`AdoptArgs`]'s own
/// (optional, defaulted from the binary's file stem) — pm2's own spelling
/// carries no such default, and `enable --exec` exists only to keep that
/// spelling working verbatim, not to gain `adopt`'s newer conveniences. A
/// reader relying on the two shapes agreeing is a mistake nothing but a
/// test catches — see `main.rs`'s
/// `the_hidden_pm2_spelling_reaches_adopt_with_the_arguments_the_right_way_round`.
#[derive(Debug, clap::Args)]
pub struct EnableArgs {
    /// The dog's name, the `[<name>]` config key in `dogs.toml`
    pub name: String,
    /// Hidden pm2-spelling alias for `shep adopt`: routes to `adopt` with
    /// this flag's value as the binary path
    #[arg(long, hide = true)]
    pub exec: Option<PathBuf>,
}

/// Arguments to `shep adopt`.
///
/// `path` is the one positional; `--name` is optional and defaults to the
/// binary's file stem with a leading `shep-` stripped, the way `cargo`
/// strips `cargo-` from its own external subcommands (`shep-log-rotate`
/// defaults to `log-rotate`). Previously both were required positionals,
/// name first (`adopt <name> <path>`) — a breaking CLI change, decision
/// The maintainer: `shep adopt <path>` alone now works for a binary whose name is
/// already the name you want, matching `shep start <script>`'s own
/// optional `--name`.
#[derive(Debug, clap::Args)]
pub struct AdoptArgs {
    /// Path to the dog's binary, vetted before `shep.toml` is touched.
    /// Resolved before vetting: as given, with a leading `~/` expanded, or
    /// looked up on `$PATH` if it names no directory — first hit wins.
    pub path: PathBuf,
    /// The dog's name, the `[<name>]` config key in `dogs.toml`. Defaults
    /// to the binary's file stem with a leading `shep-` stripped.
    #[arg(long)]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};

    /// fails if `Commands::Dog` is wired to another verb, or if it is not
    /// hidden. It is a re-exec target, not something an operator runs.
    #[test]
    fn the_dog_subcommand_parses_and_stays_hidden() {
        use clap::{CommandFactory, Parser};

        let parsed = Cli::try_parse_from(["shep", "dog", "metrics"])
            .unwrap()
            .command;
        let Commands::Dog(args) = parsed else {
            panic!("expected dog")
        };
        assert_eq!(args.name, "metrics");

        let cmd = Cli::command();
        assert!(
            cmd.find_subcommand("dog").unwrap().is_hide_set(),
            "dog must stay hidden from --help"
        );
    }

    /// Fails if `enable`'s pm2-spelled `--exec` alias loses its `hide =
    /// true` and starts teaching itself in `--help` — the whole reason it
    /// is an argument-level hide rather than a documented flag: `shep
    /// adopt` is the verb the help text should point an operator at.
    #[test]
    fn the_exec_alias_stays_hidden_from_help() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let enable = cmd.find_subcommand("enable").unwrap();
        let exec_arg = enable
            .get_arguments()
            .find(|a| a.get_id().as_str() == "exec")
            .expect("EnableArgs must still carry a hidden `exec` field");
        assert!(
            exec_arg.is_hide_set(),
            "--exec must stay hidden from --help"
        );
    }
}
