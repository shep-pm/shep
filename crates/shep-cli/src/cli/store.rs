//! Arguments for the two stores a shepherd keeps on disk: the key/value
//! store behind `set`, `get` and `unset`, and the secret store behind
//! `secret`.
//!
//! Both are read and written without a shepherd running, and both take a
//! flat key grammar. The secret half writes its own `Debug` rather than
//! deriving one, so a `{:?}` anywhere in the binary cannot print an
//! operator's plaintext value (IR-41).

use core::fmt;

/// Arguments to `shep set`.
#[derive(Debug, clap::Args)]
pub struct KvSetArgs {
    /// The key
    pub key: String,
    /// The value
    pub value: String,
}

/// Arguments to `shep get`.
#[derive(Debug, clap::Args)]
pub struct KvGetArgs {
    /// The key; omit to list every key
    pub key: Option<String>,
}

/// Arguments to `shep unset`.
///
/// `--all` rather than a reserved key name, for the reason [`FlushArgs`](crate::cli::FlushArgs)'s own
/// doc gives about `shep flush shep`: nothing stops an operator having a key
/// called `all`, and `shep unset all` would then mean something different
/// depending on their own store. A flag cannot collide.
#[derive(Debug, clap::Args)]
pub struct KvUnsetArgs {
    /// The key to remove
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub key: Option<String>,
    /// Remove every key
    #[arg(long)]
    pub all: bool,
}

/// Arguments to `shep secret`.
///
/// `Debug` is written out rather than derived (IR-41) so a field added here
/// cannot print in the clear by default; the value itself is redacted by
/// [`SecretCommand`]'s own impl.
#[derive(clap::Args)]
pub struct SecretArgs {
    /// Which of the four things to do to the store.
    #[command(subcommand)]
    pub command: SecretCommand,
}

impl fmt::Debug for SecretArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretArgs")
            .field("command", &self.command)
            .finish()
    }
}

/// `shep secret`'s subcommands.
///
/// `Set` carries the operator's own plaintext value, so `Debug` prints its
/// length and never the value (IR-41). Exact-string-tested below
/// (`secret_command_debug_does_not_leak`), so restoring the derive fails
/// that test instead of silently reopening the leak.
#[derive(clap::Subcommand)]
pub enum SecretCommand {
    /// Store a value
    ///
    /// The value is a positional argument by default, so it is visible in
    /// `ps` and in this shell's history for as long as the command runs.
    /// Pass `--stdin` to keep it out of both.
    Set {
        /// The key
        key: String,
        /// The value; required unless --stdin is given
        #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
        value: Option<String>,
        /// Which environment; omit for every environment
        #[arg(long)]
        env: Option<String>,
        /// Read the value from stdin; the positional form is visible in
        /// `ps` and in shell history
        #[arg(long)]
        stdin: bool,
    },
    /// Print a value back, if `[secrets] allow_read` is on
    Get {
        /// The key
        key: String,
        /// Which environment; omit to use `[daemon] environment`'s
        /// default, which an app's own environment may override
        #[arg(long)]
        env: Option<String>,
    },
    /// Remove a value
    Unset {
        /// The key
        key: String,
        /// Which environment; omit for the every-environment slot
        #[arg(long)]
        env: Option<String>,
    },
    /// List keys and the environments each has a value for
    List,
}

impl fmt::Debug for SecretCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set {
                key,
                value,
                env,
                stdin,
            } => {
                let value = value.as_ref().map_or_else(
                    || "None".to_string(),
                    |value| format!("Some(<{} bytes>)", value.len()),
                );
                f.debug_struct("Set")
                    .field("key", key)
                    .field("value", &format_args!("{value}"))
                    .field("env", env)
                    .field("stdin", stdin)
                    .finish()
            }
            Self::Get { key, env } => f
                .debug_struct("Get")
                .field("key", key)
                .field("env", env)
                .finish(),
            Self::Unset { key, env } => f
                .debug_struct("Unset")
                .field("key", key)
                .field("env", env)
                .finish(),
            Self::List => f.write_str("List"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};

    /// fails if `shep unset` with no key and no --all is accepted. It would have to
    /// mean either nothing or everything, and the everything reading is
    /// unrecoverable.
    #[test]
    fn unset_needs_a_key_or_the_all_flag() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "unset"]).is_err());
        assert!(Cli::try_parse_from(["shep", "unset", "a"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "unset", "--all"]).is_ok());
    }

    /// fails if `--all` composes with a key. `shep unset a --all` would be an
    /// operator asking for one thing and a flag doing something far larger —
    /// the same conflict `shep flush all --daemon` is a usage error for.
    #[test]
    fn unset_refuses_a_key_and_all_together() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "unset", "a", "--all"]).is_err());
    }

    /// fails if `shep get` starts requiring a key. Bare `get` listing the whole
    /// store is the discovery path — an operator who does not remember what they
    /// set has nowhere else to look.
    #[test]
    fn get_takes_an_optional_key() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "get"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "get", "a"]).is_ok());
    }

    /// fails if `set` becomes anything but two required positionals. A `set` with a
    /// defaultable value would let `shep set a` silently store an empty string.
    #[test]
    fn set_needs_both_a_key_and_a_value() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "set", "a"]).is_err());
        assert!(Cli::try_parse_from(["shep", "set", "a", "1"]).is_ok());
    }

    /// fails if the derive comes back on either type: `shep secret set`
    /// carries the operator's plaintext value, and a `{:?}` anywhere in the
    /// binary would print it (IR-41). Exact strings, not a `contains`, so a
    /// derive cannot pass by happening to omit this one value.
    #[test]
    fn secret_command_debug_does_not_leak() {
        use clap::Parser;
        let Commands::Secret(args) = Cli::try_parse_from([
            "shep",
            "secret",
            "set",
            "DB_PASSWORD",
            "hunter2",
            "--env",
            "staging",
        ])
        .unwrap()
        .command
        else {
            panic!("secret parses to its own variant")
        };

        assert_eq!(
            format!("{args:?}"),
            "SecretArgs { command: Set { key: \"DB_PASSWORD\", value: Some(<7 bytes>), \
             env: Some(\"staging\"), stdin: false } }"
        );
        assert_eq!(
            format!("{:?}", SecretCommand::List),
            "List",
            "a variant carrying no value still has to render"
        );
    }
}
