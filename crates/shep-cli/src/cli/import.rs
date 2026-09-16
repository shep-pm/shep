//! Arguments for `shep import`: reading somebody else's config in.
//!
//! Two sources that share a noun and nothing else, a pm2 dump and a `.env`,
//! so the verb hosts a subcommand rather than a flag set. Both write
//! somewhere before anything starts, which is why neither needs a running
//! shepherd and both take a `--dry-run`.

use std::path::PathBuf;

/// Arguments to `shep import`.
///
/// A subcommand host rather than a flag set, for [`SecretArgs`](crate::cli::SecretArgs)' reason: the
/// two inputs share a noun and nothing else. `Debug` is derived; neither
/// subcommand carries a value.
#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// Which kind of file to read.
    #[command(subcommand)]
    pub command: ImportCommand,
}

/// `shep import`'s subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum ImportCommand {
    /// Write a Flockfile from a pm2 dump. Starts nothing.
    ///
    /// Reads `--from`, or `~/.pm2/dump.pm2` if it names nothing — whichever
    /// `pm2 save` last wrote. Every clustered app is named on stderr: shep
    /// binds nothing, so N instances on one port need the app to set
    /// `SO_REUSEPORT` itself, or the second instance hits EADDRINUSE at
    /// start. Every env key the dump carried that was neither declared nor
    /// recognizable session junk is named on stderr too, and left out of
    /// the Flockfile, for the operator to decide.
    Pm2(ImportPm2Args),
    /// Read a `.env` into the secret store and one sheep's own env.
    ///
    /// Every key the file holds goes to the named sheep's env, where it
    /// reaches the app at its next spawn. A key named by `--secret` has its
    /// value stored in `$SHEP_HOME/secrets.json` instead, and the sheep's
    /// env gets `{{secret:KEY}}`, so the value never reaches `flock.json`
    /// or the handover blob.
    ///
    /// A key that is not marked secret is stored in the clear, and is
    /// copied into both of those snapshots along with the rest of the
    /// sheep's config.
    ///
    /// The sheep has to exist already: this records an operator override,
    /// which is per sheep. Run `shep start` first.
    ///
    /// Any collision, any pattern that matches nothing, and any line the
    /// grammar does not accept refuses the whole import. A refusal reached
    /// before the secret store is written leaves both stores alone; one
    /// reached after counts the keys it left there, which nothing
    /// references until the import is re-run.
    Env(ImportEnvArgs),
}

/// Arguments to `shep import pm2`.
#[derive(Debug, clap::Args)]
pub struct ImportPm2Args {
    /// Read this pm2 dump instead of `~/.pm2/dump.pm2`
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// Write the Flockfile here instead of `./Flockfile.toml`
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Print the Flockfile that would be written, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite an existing Flockfile
    #[arg(long)]
    pub force: bool,
}

/// Arguments to `shep import env`.
///
/// `Debug` is derived: every field is a path, a name or a pattern. The
/// values live in the file this names, never in the arguments, which is
/// also why there is no `--stdin`.
#[derive(Debug, clap::Args)]
pub struct ImportEnvArgs {
    /// The `.env` to read
    ///
    /// A `{{...}}` in a plain value reaches the app as the literal text the
    /// file wrote, not as a shep template reference.
    pub file: PathBuf,
    /// The sheep whose env these keys belong to
    #[arg(long)]
    pub app: String,
    /// Store this key's value as a secret, and reference it from the env.
    ///
    /// An exact key, or a glob when it holds a metacharacter, the same rule
    /// a sheep-name selector takes. Repeatable. A pattern matching no key
    /// in the file refuses the import.
    #[arg(long)]
    pub secret: Vec<String>,
    /// Import only these keys; the default is all of them.
    ///
    /// Same grammar as `--secret`, and the same refusal.
    #[arg(long)]
    pub only: Vec<String>,
    /// Which environment's slot the secrets go in.
    ///
    /// The default is the sheep's own environment, which is its
    /// `environment` field, or `[daemon] environment` when it has none, and
    /// so is never the `all` slot that every environment reads. Naming
    /// `all` here writes that slot, as `shep secret set --env all` does.
    #[arg(long)]
    pub env: Option<String>,
    /// Print what would be written, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite keys that already hold a different value
    #[arg(long)]
    pub force: bool,
}
