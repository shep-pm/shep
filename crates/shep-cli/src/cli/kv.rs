//! Arguments for the key/value store: `shep kv get`, `shep kv set` and
//! `shep kv unset`.
//!
//! The store is read and written with no shepherd running, and takes a flat
//! key grammar. Nothing here connects, and nothing here parses a value: the
//! value is a string, and [`shep_core::kv`] caps it.
//!
//! `get`, `set` and `unset` are the settings verbs at the top level: they name
//! the settings read, write and drop, which are different stores of values.

/// Arguments to `shep kv`.
#[derive(Debug, clap::Args)]
pub struct KvArgs {
    /// Which of the three things to do to the store.
    #[command(subcommand)]
    pub command: KvCommand,
}

/// `shep kv`'s subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum KvCommand {
    /// Read one value from the store, or list the whole store with no key.
    Get(KvGetArgs),
    /// Store a value in the shepherd's key/value store.
    ///
    /// Reads and writes `$SHEP_HOME/kv.json` directly and never connects to
    /// the shepherd: the store is for ad-hoc notes and dog settings, and it
    /// has to work while nothing is running, exactly as `shep enable` does.
    ///
    /// Keys are flat: letters, digits, `.`, `_` and `-`, up to 128 bytes, not
    /// starting with a dot. A dot is part of the name. `bark.cooldown` is one
    /// key, not a path into anything.
    Set(KvSetArgs),
    /// Remove one key from the store, or every key with --all.
    Unset(KvUnsetArgs),
}

/// Arguments to `shep kv set`.
#[derive(Debug, clap::Args)]
pub struct KvSetArgs {
    /// The key
    pub key: String,
    /// The value
    pub value: String,
}

/// Arguments to `shep kv get`.
#[derive(Debug, clap::Args)]
pub struct KvGetArgs {
    /// The key; omit to list every key
    pub key: Option<String>,
}

/// Arguments to `shep kv unset`.
///
/// `--all` rather than a reserved key name, for the reason [`FlushArgs`](crate::cli::FlushArgs)'s own
/// doc gives about `shep flush shep`: nothing stops an operator having a key
/// called `all`, and `shep kv unset all` would then mean something different
/// depending on their own store. A flag cannot collide.
#[derive(Debug, clap::Args)]
pub struct KvUnsetArgs {
    /// The key to remove
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub key: Option<String>,
    /// Remove every key
    #[arg(short = 'a', long)]
    pub all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};

    /// The parsed `shep kv <verb>` command, or a panic naming the argv.
    fn kv_command(argv: &[&str]) -> KvCommand {
        use clap::Parser as _;

        let full: Vec<&str> = ["shep"].into_iter().chain(argv.iter().copied()).collect();
        let Commands::Kv(args) = Cli::try_parse_from(&full)
            .unwrap_or_else(|err| panic!("{full:?} must parse: {err}"))
            .command
        else {
            panic!("{full:?} is not `shep kv`")
        };
        args.command
    }

    /// fails if `shep kv` stops taking its three subcommands.
    #[test]
    fn kv_takes_get_set_and_unset() {
        assert!(matches!(
            kv_command(&["kv", "get"]),
            KvCommand::Get(KvGetArgs { key: None })
        ));
        assert!(matches!(
            kv_command(&["kv", "get", "a"]),
            KvCommand::Get(KvGetArgs { key: Some(_) })
        ));
        assert!(matches!(
            kv_command(&["kv", "set", "a", "1"]),
            KvCommand::Set(KvSetArgs { key, value }) if key == "a" && value == "1"
        ));
        assert!(matches!(
            kv_command(&["kv", "unset", "a"]),
            KvCommand::Unset(KvUnsetArgs {
                key: Some(_),
                all: false
            })
        ));
        assert!(matches!(
            kv_command(&["kv", "unset", "--all"]),
            KvCommand::Unset(KvUnsetArgs {
                key: None,
                all: true
            })
        ));
    }

    /// fails if bare `shep kv` becomes accepted. It would have to mean either
    /// nothing or the whole store, and `shep kv` printing the store is what
    /// `shep kv get` is for.
    #[test]
    fn bare_kv_is_a_usage_error() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "kv"]).is_err());
    }

    /// fails if `shep kv unset` with no key and no --all is accepted. It would
    /// have to mean either nothing or everything, and the everything reading
    /// is unrecoverable.
    #[test]
    fn kv_unset_needs_a_key_or_the_all_flag() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "kv", "unset"]).is_err());
        assert!(Cli::try_parse_from(["shep", "kv", "unset", "a"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "kv", "unset", "--all"]).is_ok());
    }

    /// fails if `--all` composes with a key. `shep kv unset a --all` would be
    /// an operator asking for one thing and a flag doing something far larger
    /// - the same conflict `shep flush all --daemon` is a usage error for.
    #[test]
    fn kv_unset_refuses_a_key_and_all_together() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "kv", "unset", "a", "--all"]).is_err());
    }

    /// fails if `shep kv get` starts requiring a key. Bare `get` listing the
    /// whole store is the discovery path - an operator who does not remember
    /// what they set has nowhere else to look.
    #[test]
    fn kv_get_takes_an_optional_key() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "kv", "get"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "kv", "get", "a"]).is_ok());
    }

    /// fails if `set` becomes anything but two required positionals. A `set`
    /// with a defaultable value would let `shep kv set a` silently store an
    /// empty string.
    #[test]
    fn kv_set_needs_both_a_key_and_a_value() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "kv", "set", "a"]).is_err());
        assert!(Cli::try_parse_from(["shep", "kv", "set", "a", "1"]).is_ok());
    }

    /// fails if `kv` stops being a verb an operator can find in `--help`, or if
    /// `get`, `set` or `unset` are read as the store's again. All three name
    /// the settings verbs now, and the store's spelling is `shep kv <verb>`.
    #[test]
    fn kv_is_visible_and_the_settings_verbs_are_not_its_aliases() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        assert!(
            !cmd.find_subcommand("kv").unwrap().is_hide_set(),
            "`kv` must be a verb `--help` lists"
        );
        for settings in ["get", "set", "unset"] {
            assert!(
                !cmd.find_subcommand(settings).unwrap().is_hide_set(),
                "`{settings}` is a settings verb, not a store alias"
            );
        }
    }

    /// fails if `shep kv` grows a subcommand `--help` does not reach, or loses
    /// one. `shep kv --help` is the only place the three spellings are named
    /// together.
    #[test]
    fn kv_help_lists_its_three_subcommands() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd
            .find_subcommand_mut("kv")
            .expect("`shep kv` exists")
            .render_help()
            .to_string();
        for verb in ["get", "set", "unset"] {
            assert!(help.contains(verb), "{help}");
        }
    }
}
