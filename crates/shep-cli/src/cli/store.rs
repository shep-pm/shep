//! Arguments for the secret store behind `secret`: the values a Flockfile
//! refers to and never carries.
//!
//! Read and written without a shepherd running, on the same flat key grammar
//! the key/value store takes (see [`crate::cli::KvArgs`]). This half writes
//! its own `Debug` rather than deriving one, so a `{:?}` anywhere in the
//! binary cannot print an operator's plaintext value (IR-41).

use core::fmt;

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
        #[arg(short = 'E', long)]
        env: Option<String>,
        /// Read the value from stdin; the positional form is visible in
        /// `ps` and in shell history
        #[arg(short = 's', long)]
        stdin: bool,
    },
    /// Print a value back, if `[secrets] allow_read` is on
    Get {
        /// The key
        key: String,
        /// Which environment; omit to use `[daemon] environment`'s
        /// default, which an app's own environment may override
        #[arg(short = 'E', long)]
        env: Option<String>,
    },
    /// Remove a value
    Unset {
        /// The key
        key: String,
        /// Which environment; omit for the every-environment slot
        #[arg(short = 'E', long)]
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
