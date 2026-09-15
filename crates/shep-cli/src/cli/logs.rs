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
/// # Why a flag and not a reserved selector name
///
/// The shepherd's own `shepd.out.log`/`shepd.err.log` are the second thing
/// this verb can empty, and they are NOT a sheep — nothing about them is
/// expressible as a selector. Spelling them `shep flush shep` would make one
/// name mean something different depending on the Flockfile, since nothing
/// stops an app being called `shep`, and an operator who named one that would
/// find `shep flush shep` quietly emptying the wrong files. A flag cannot
/// collide with anything.
///
/// # Why it replaces the selector rather than composing with it
///
/// `--daemon` conflicts with the selector, so `shep flush all --daemon` is a
/// usage error rather than "both". Three reasons, in order of weight: the two
/// halves answer with different shapes — sheep against files — and one
/// invocation renders one payload into one envelope; the daemon's own logs
/// are the one target the maintainer asked never to be reached without being named, and
/// a flag that rode along with `all` would be reached by every operator who
/// ever typed `shep flush all --daemon` out of habit; and the shepherd's logs
/// are not a sheep's, so folding them into a flock answer would mean
/// inventing a row for something with no id and no name.
///
/// The selector stays required in every other case — `required_unless_present`
/// rather than a `default_value`, so a bare `shep flush` is still the usage
/// error it has always been, never "empty every log in the flock".
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
    /// Fails if either loses its `default_value`: the bare invocation
    /// becomes a clap usage error instead of targeting the flock, which for
    /// `reopen` is the whole reason a signal — which carries no selector —
    /// can mean this verb at all. Both halves matter: an explicit selector
    /// must still win, or the default would be a hardcoded `all` wearing a
    /// default's clothes.
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

    /// The other side of [`bleats_and_reopen_default_to_every_sheep`]: the
    /// log-plane verb that destroys data must NOT have a default.
    ///
    /// Fails if `flush` is ever given a `default_value`, or moved onto
    /// [`ReopenArgs`] — either of which turns a bare `shep flush`, the single
    /// most likely slip of the finger this CLI offers, from a usage error
    /// into "empty every log file in the flock" with nothing to undo it. The
    /// explicit form is asserted alongside, so a verb that rejected every
    /// selector could not pass the first half alone.
    ///
    /// `required_unless_present = "daemon"` is what keeps the first half true
    /// now that the selector is an `Option`: without it, a bare `shep flush`
    /// parses to `selector: None` and reaches the handler.
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

    /// Fails if `--daemon` stops replacing the selector — either by gaining a
    /// selector of its own (the bare form stops parsing) or by losing
    /// `conflicts_with` (the combined form starts parsing).
    ///
    /// Both halves are the decision [`FlushArgs`]'s doc argues for. The bare
    /// form must work, because it is the only spelling of "empty the
    /// shepherd's own logs" and requiring a sheep selector alongside it would
    /// be nonsense. The combined form must NOT, because an operator typing
    /// `shep flush all --daemon` out of habit is exactly the accident that
    /// keeping the two targets apart exists to prevent.
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
