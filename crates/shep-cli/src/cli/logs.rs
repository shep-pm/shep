//! Arguments for the verbs that work on the log plane: `bleats`, `reopen`,
//! `flush` and `barks`.
//!
//! Four verbs over the same files, and the interesting part is which of
//! them may default their selector. `bleats` and `reopen` share
//! [`DEFAULT_SELECTOR`] because neither destroys anything; `flush` empties
//! files and so demands a target, which is what its own doc argues at
//! length.

/// Arguments to `shep flush`.
///
/// `--daemon` is a flag, not a reserved selector name: an app can be called
/// `shep`, and a flag cannot collide. It conflicts with the selector, since
/// the two targets answer in different shapes and the daemon's logs must
/// always be named. `required_unless_present` keeps a bare `shep flush` a
/// usage error.
#[derive(Debug, clap::Args)]
pub struct FlushArgs {
    /// name, id, `name:slot`, `all`, `/regex/`, `fold:<name>` (required unless --daemon)
    #[arg(required_unless_present = "daemon", conflicts_with = "daemon")]
    pub selector: Option<String>,
    /// Empty the shepherd's own logs instead of any sheep's
    #[arg(long)]
    pub daemon: bool,
}

/// Arguments to `shep barks`.
///
/// No selector, and no `--daemon`-shaped flag either — `barks.jsonl` is one
/// file for the whole `$SHEP_HOME`, holding both the bark dog's own alerts
/// and the ones the shepherd wrote itself when an enabled dog exhausted its
/// restart budget, so there is no population within it to select a subset
/// of the way `flush` selects sheep.
#[derive(Debug, clap::Args)]
pub struct BarksArgs {
    /// Show only the last N barks
    #[arg(long)]
    pub tail: Option<usize>,
}

/// The selector the verbs that take an optional one fall back to.
///
/// One owner for the string, shared rather than spelled twice: `bleats` and
/// `reopen` default to the same thing on purpose, and a copy that drifted
/// would leave one of them quietly targeting something else.
const DEFAULT_SELECTOR: &str = "all";

/// How much history `shep bleats` prints before it starts following.
///
/// Fifteen because that is what pm2 has trained every operator to expect,
/// and because the number only has to be large enough to carry the reason a
/// sheep died. It is small enough that following a healthy flock still
/// starts nearly empty.
pub const DEFAULT_BLEAT_LINES: usize = 15;

/// Arguments to `shep bleats` (alias `logs`).
#[derive(Debug, clap::Args)]
pub struct BleatsArgs {
    /// Which sheep (default: all)
    #[arg(default_value = DEFAULT_SELECTOR)]
    pub selector: String,
    /// Print the tail of each sheep's log file and exit, instead of following
    #[arg(long)]
    pub no_follow: bool,
    /// How many existing lines of each stream to print before following
    ///
    /// A sheep that already crashed has said everything it is going to say,
    /// so following alone shows an empty screen while the reason sits in the
    /// file. This prints that much history first, then follows.
    ///
    /// Counted per stream, so the default prints up to this many lines of
    /// stdout and up to this many of stderr for each matched sheep. Narrow
    /// it with `--out` or `--err`.
    ///
    /// `0` prints no history at all, following only what arrives next.
    #[arg(long, default_value_t = DEFAULT_BLEAT_LINES, value_name = "N")]
    pub lines: usize,
    /// Only stderr
    #[arg(long, conflicts_with = "out")]
    pub err: bool,
    /// Only stdout
    #[arg(long, conflicts_with = "err")]
    pub out: bool,
}

/// Arguments to `shep reopen`.
///
/// The selector is optional, defaulting to [`DEFAULT_SELECTOR`], where
/// `stop`/`restart`/`delete` all demand one: those destroy something, and
/// a reopen destroys nothing — it swaps a file handle for another handle on
/// the same path. Rotating every sheep at once is also the ordinary case, a
/// `postrotate` stanza having just renamed the whole log directory.
#[derive(Debug, clap::Args)]
pub struct ReopenArgs {
    /// Which sheep (default: all)
    #[arg(default_value = DEFAULT_SELECTOR)]
    pub selector: String,
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};

    /// Pins [`DEFAULT_SELECTOR`] on both verbs that carry it.
    ///
    /// Fails if either loses its `default_value`: a bare `reopen` is the one
    /// way a signal, which carries no selector, can mean this verb. An
    /// explicit selector must still win.
    #[test]
    fn bleats_and_reopen_default_to_every_sheep() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "reopen"]).unwrap().command;
        let Commands::Reopen(args) = bare else {
            panic!("`shep reopen` must parse with no selector")
        };
        assert_eq!(args.selector, "all");

        let bare = Cli::try_parse_from(["shep", "bleats"]).unwrap().command;
        let Commands::Bleats(args) = bare else {
            panic!("`shep bleats` must parse with no selector")
        };
        assert_eq!(args.selector, "all");

        let named = Cli::try_parse_from(["shep", "reopen", "web"])
            .unwrap()
            .command;
        let Commands::Reopen(args) = named else {
            panic!("expected reopen")
        };
        assert_eq!(args.selector, "web");
    }

    /// The other side of [`bleats_and_reopen_default_to_every_sheep`]: `flush`
    /// destroys data and must NOT have a default.
    ///
    /// A `default_value` would turn a bare `shep flush` into "empty every log
    /// in the flock". `required_unless_present = "daemon"` keeps it an error;
    /// the explicit form is asserted too.
    #[test]
    fn flush_refuses_to_run_without_a_selector() {
        use clap::Parser;
        assert!(
            Cli::try_parse_from(["shep", "flush"]).is_err(),
            "`shep flush` with no selector must be a usage error, never the \
             whole flock"
        );

        let named = Cli::try_parse_from(["shep", "flush", "all"])
            .unwrap()
            .command;
        let Commands::Flush(args) = named else {
            panic!("expected flush")
        };
        assert_eq!(args.selector.as_deref(), Some("all"));
        assert!(
            !args.daemon,
            "a plain flush must not reach the shepherd's own logs"
        );
    }

    /// Fails if `--daemon` stops replacing the selector: the bare form must
    /// parse and the combined form must not (see [`FlushArgs`]).
    #[test]
    fn the_daemon_flag_replaces_the_selector_rather_than_riding_along_with_it() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "flush", "--daemon"])
            .expect("`shep flush --daemon` is the only spelling there is")
            .command;
        let Commands::Flush(args) = bare else {
            panic!("expected flush")
        };
        assert!(args.daemon);
        assert_eq!(args.selector, None);

        assert!(
            Cli::try_parse_from(["shep", "flush", "all", "--daemon"]).is_err(),
            "the shepherd's own logs are a separate act, never a rider on a \
             flock-wide flush"
        );
    }

    /// `shep barks` takes no selector and defaults `--tail` to `None` (every
    /// bark); `--tail N` parses to `Some(N)`. Fails if either the bare form
    /// stops parsing or `--tail` stops being optional — a `default_value`
    /// on it would turn "show everything" into a silent 10-line window with
    /// nothing to name why.
    #[test]
    fn barks_takes_no_selector_and_tail_defaults_to_everything() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "barks"]).unwrap().command;
        let Commands::Barks(args) = bare else {
            panic!("`shep barks` must parse with no selector")
        };
        assert_eq!(args.tail, None);

        let tailed = Cli::try_parse_from(["shep", "barks", "--tail", "20"])
            .unwrap()
            .command;
        let Commands::Barks(args) = tailed else {
            panic!("expected barks")
        };
        assert_eq!(args.tail, Some(20));
    }
}
