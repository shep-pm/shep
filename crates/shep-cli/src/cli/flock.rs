//! Arguments for the verbs that show you what is running: `flock`, `fold`
//! and `lookout`.
//!
//! None of the three changes anything by itself, and all three answer the
//! same question at different widths: one listing, one fold, or a live
//! screen. The redraw floor the follow and the dashboard share is the one
//! real constant here, and its doc carries the measurement behind it.

/// The floor and the default for `shep flock --follow`'s `--interval`, in
/// seconds.
///
/// One second, from two measurements rather than taste. `sysinfo` reports no
/// CPU at all over a window shorter than its own
/// `MINIMUM_CPU_UPDATE_INTERVAL`, which is 200 ms, so a faster cadence buys
/// a column of dashes. And one redraw costs the shepherd a
/// `Request::ListFlock` plus a host sample measured at 5.6 ms a tick on
/// macOS (`crate::host`'s own module doc): 0.6% of a core at this floor, and
/// 5.6% of one at the 100 ms somebody would otherwise reach for.
///
/// Seconds rather than milliseconds because the operator types it, and
/// nothing between 200 ms and 1 s is worth the unit confusion.
pub(crate) const FOLLOW_INTERVAL_FLOOR_SECONDS: u64 = 1;

/// Arguments to `shep flock`.
///
/// A listing is a moment, and this moment drifts: a sheep that started
/// after the list printed, one restarting. `--follow` keeps the moment
/// current.
#[derive(Debug, clap::Args)]
pub struct FlockArgs {
    /// Keep the listing current: redraw it in place until Ctrl+C
    ///
    /// One listing to start, then a fresh one every `--interval` seconds
    /// while the shepherd stays reachable, each one painted over the last.
    /// A summary of the machine rides above the tables. Ctrl+C leaves an
    /// exit of zero, a stopped shepherd leaves the code its refusal
    /// carries: an interrupted follow and a dead shepherd must not read as
    /// the same thing.
    ///
    /// Needs a live shepherd: a saved roll is a single moment, and a
    /// follow's whole reason is the moments after it. Needs a terminal
    /// too, and refuses `--format json`, since neither has anywhere to put
    /// a redraw.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub follow: bool,
    /// Seconds between redraws, with `--follow`
    ///
    /// The floor is one second, which is also the default. Below it the
    /// listing costs the shepherd more than it tells the operator, and
    /// `sysinfo` reports no CPU at all over a window that short.
    #[arg(
        long,
        default_value_t = FOLLOW_INTERVAL_FLOOR_SECONDS,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(FOLLOW_INTERVAL_FLOOR_SECONDS..),
        requires = "follow"
    )]
    pub interval: u64,
}

/// Arguments to `shep fold`.
#[derive(Debug, clap::Args)]
pub struct FoldArgs {
    /// The fold to list
    pub name: String,
}

/// Arguments to `shep lookout`.
#[derive(Debug, clap::Args)]
pub struct LookoutArgs {
    /// Close the dashboard's action gate. Actions are permitted by default.
    ///
    /// With the gate open, `x` (stop), `R` (restart) and `L` (reload) each
    /// arm a confirm instead of acting on the keypress that pressed it;
    /// Enter sends the request, any other key cancels, and an unanswered
    /// confirm expires after ten seconds. Closed, all three refuse outright.
    ///
    /// A guard against a keystroke in a window you were reading, not a
    /// security boundary: lookout runs as you, so anything it could do you can
    /// already do with `shep stop`. Can also be closed with `shep set
    /// lookout.allow_control false`.
    #[arg(long)]
    pub read_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};

    /// Pins the three facts `flock`'s own arguments settle: a bare listing
    /// does not follow, the interval defaults to the floor, and an interval
    /// without a follow is a usage error rather than a silent no-op.
    #[test]
    fn flock_follows_only_when_asked_and_defaults_to_a_one_second_interval() {
        use clap::Parser;

        let Commands::Flock(args) = Cli::try_parse_from(["shep", "flock"]).unwrap().command else {
            panic!("expected flock")
        };
        assert!(!args.follow, "a bare listing is still one moment");
        assert_eq!(args.interval, FOLLOW_INTERVAL_FLOOR_SECONDS);

        let Commands::Flock(args) =
            Cli::try_parse_from(["shep", "flock", "--follow", "--interval", "5"])
                .unwrap()
                .command
        else {
            panic!("expected flock")
        };
        assert!(args.follow);
        assert_eq!(args.interval, 5);

        assert!(
            Cli::try_parse_from(["shep", "flock", "--interval", "5"]).is_err(),
            "an interval with nothing to pace is a usage error"
        );
    }

    /// fails if the floor is dropped from `--interval`. Zero would spin the
    /// redraw against the shepherd as fast as the socket answers, and
    /// `sysinfo` reports no CPU at all over a window under 200 ms.
    #[test]
    fn a_zero_interval_is_refused() {
        use clap::Parser;

        assert!(Cli::try_parse_from(["shep", "flock", "--follow", "--interval", "0"]).is_err());
    }

    /// fails if the control gate stops being on by default, or stops being
    /// closeable from the flag.
    #[test]
    fn actions_are_on_unless_read_only_says_otherwise() {
        use clap::Parser;
        let Commands::Lookout(default) = Cli::try_parse_from(["shep", "lookout"]).unwrap().command
        else {
            panic!("lookout parses to its own variant")
        };
        assert!(!default.read_only);

        let Commands::Lookout(flagged) = Cli::try_parse_from(["shep", "lookout", "--read-only"])
            .unwrap()
            .command
        else {
            panic!("lookout parses to its own variant")
        };
        assert!(flagged.read_only);
    }

    /// fails if `--allow-control` starts parsing again: lookout takes no
    /// such flag.
    #[test]
    fn allow_control_no_longer_parses() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "lookout", "--allow-control"]).is_err());
    }
}
