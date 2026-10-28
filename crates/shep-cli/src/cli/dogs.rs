//! Arguments for the dog verbs: `dogs`, `enable`, `disable`, `adopt`,
//! the hidden `rehome`, and the hidden `dog` re-exec target.
//!
//! `enable` and `disable` also take a sheep's name, so each carries a
//! sheep-and-dog `name` of its own. The rest name a dog and never select
//! one, so none of them takes the selector grammar the flock verbs share.
//! Two of the six get by on the one name field [`DogArgs`] carries; the docs
//! on the others say what they add and why it was not worth widening the
//! shared struct.

use std::path::PathBuf;

/// `shep dogs`, the index of dogs you could adopt, and the two verbs that
/// change one permission of a dog.
///
/// The listing's flag and filter and the subcommands exclude each other:
/// `shep dogs grant` is a verb, so a dog named `grant` is reached with
/// `shep dogs -- grant`.
#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct DogsArgs {
    /// List the dogs published in the community index instead of the ones
    /// this shepherd is running. Needs no shepherd.
    #[arg(long)]
    pub available: bool,
    /// Narrow the listing to entries whose name, package or description
    /// contains this text, case-insensitively.
    #[arg(value_name = "FILTER")]
    pub filter: Option<String>,
    /// Change one permission instead of listing.
    #[command(subcommand)]
    pub command: Option<DogsCommand>,
}

/// `shep dogs`'s subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum DogsCommand {
    /// Grant a dog one permission it declared.
    ///
    /// Writes `[daemon.dog_grants.<dog>]` in `shep.toml` and tells a
    /// running shepherd, which judges the dog's next request by the new
    /// grants with no restart. Only a permission the dog's manifest declares
    /// can be granted; `shep adopt` and the first `shep enable` ask about
    /// the whole list.
    ///
    /// Grants keep a dog's requests in bounds and do not confine it: it
    /// runs as you and can do anything you can.
    Grant(GrantArgs),
    /// Revoke one permission from a dog.
    ///
    /// Writes `[daemon.dog_grants.<dog>]` in `shep.toml` and tells a
    /// running shepherd, which refuses the dog's next request that needs
    /// it with no restart.
    Revoke(GrantArgs),
}

/// Arguments to `shep dogs grant` and `shep dogs revoke`.
#[derive(Debug, clap::Args)]
pub struct GrantArgs {
    /// The dog's name, as `shep adopt` registered it
    pub name: String,
    /// The permission, as the dog's manifest spells it: `flock.read`,
    /// `events:process.`, `secrets.read:staging`
    pub permission: String,
}

/// Arguments to the hidden `shep rehome`, and to the hidden `shep dog`
/// re-exec target.
///
/// A dog is named, never selected: `SelectorArgs`' grammar answers "which of
/// the flock", and a dog is not the flock. `shep enable` and `shep disable`
/// also take a sheep's name, so each has a struct of its own
/// ([`EnableArgs`], [`DisableArgs`]).
#[derive(Debug, clap::Args)]
pub struct DogArgs {
    /// The dog's name, the `[<name>]` config key in `dogs.toml`
    pub name: String,
}

/// Arguments to `shep enable`.
///
/// [`DogArgs`] plus hidden `--exec`, pm2's spelling of `shep adopt`. `main`'s
/// dispatch routes it to [`crate::commands::dogs::adopt`], never to `enable`.
/// `name` is required, unlike [`AdoptArgs`]'s; pinned by
/// `the_hidden_pm2_spelling_reaches_adopt_with_the_arguments_the_right_way_round`.
#[derive(Debug, clap::Args)]
pub struct EnableArgs {
    /// A sheep's name, `fold:<fold>`, `/<regex>/` or `all`, or a dog's name
    /// (the `[<name>]` config key in `dogs.toml`)
    pub name: String,
    /// Hidden pm2-spelling alias for `shep adopt`: routes to `adopt` with
    /// this flag's value as the binary path
    #[arg(long, hide = true)]
    pub exec: Option<PathBuf>,
    /// Grant a dog everything it declares without asking; a dog's first
    /// enable asks, and with no terminal and no `--yes` refuses
    #[arg(long)]
    pub yes: bool,
}

/// Arguments to `shep disable`.
#[derive(Debug, clap::Args)]
pub struct DisableArgs {
    /// A sheep's name, `fold:<fold>`, `/<regex>/` or `all`, or a dog's name
    /// (the `[<name>]` config key in `dogs.toml`)
    pub name: String,
}

/// Arguments to `shep adopt`.
///
/// `path` is the one positional; `--name` is optional and defaults to the
/// binary's file stem with a leading `shep-` stripped, the way `cargo`
/// strips `cargo-` from its own external subcommands (`shep-log-rotate`
/// defaults to `log-rotate`).
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
    /// Grant the dog everything it declares without asking. With no
    /// terminal and no `--yes`, adopt refuses rather than granting.
    #[arg(long)]
    pub yes: bool,
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
