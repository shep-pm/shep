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

/// The config pane's `dogs` group: the only tab with a dog's table on it.
///
/// The name [`crate::lookout::pane::ConfigPane::shows_dogs`] compares on, and
/// the one group name this module writes out. The tab list itself is
/// [`shep_core::config::GROUP_ORDER`], and
/// `every_tab_is_a_group_the_pane_draws_and_nothing_else_parses` pins that
/// this one is in it.
const DOG_TAB: &str = "dogs";

/// Arguments to `shep lookout`.
///
/// [`LookoutSpec`] is the clap surface: the gate flag and the three
/// positionals, with the help an operator reads. [`LookoutArgs`] is that plus
/// the one rule clap has no way to state, that a dog's table sits on the
/// `dogs` tab, so a third positional after any other tab is a usage error.
#[derive(Debug)]
pub struct LookoutArgs {
    /// Whether the dashboard's action gate is closed.
    pub read_only: bool,
    /// The sheep to open on, or `None` for the dashboard.
    pub sheep: Option<String>,
    /// The config pane's group to open on, one of
    /// [`shep_core::config::GROUP_ORDER`].
    pub tab: Option<String>,
    /// The dog whose table to open, which [`clap::FromArgMatches::from_arg_matches`]
    /// admits only with [`DOG_TAB`] as its tab.
    pub dog: Option<String>,
}

/// The clap surface for `shep lookout`.
#[derive(Debug, clap::Args)]
struct LookoutSpec {
    /// Close the dashboard's action gate. Actions are permitted by default.
    ///
    /// With the gate open, `x` (stop), `R` (restart) and `L` (reload) each
    /// arm a confirm instead of acting on the keypress that pressed it;
    /// Enter sends the request, any other key cancels, and an unanswered
    /// confirm expires after ten seconds. Closed, all three refuse outright.
    ///
    /// A guard against a keystroke in a window you were reading, not a
    /// security boundary: lookout runs as you, so anything it could do you can
    /// already do with `shep stop`. Can also be closed with `shep kv set
    /// lookout.allow_control false`.
    #[arg(long)]
    read_only: bool,
    /// Open on one sheep's own screen
    ///
    /// The name is the one `shep describe` takes. A fold, a fold selector or
    /// a `fold/app` selector is not one sheep and is not accepted here.
    /// With no tab, lookout opens that sheep's status page, the screen
    /// Enter opens from the dashboard.
    #[arg(value_name = "SHEEP")]
    sheep: Option<String>,
    /// Open that sheep's config pane on one tab
    ///
    /// The tabs are the ones the pane itself draws, in the order it draws
    /// them. With a tab, lookout opens the config pane `e` opens, already
    /// sitting on it.
    #[arg(
        value_name = "TAB",
        value_parser = clap::builder::PossibleValuesParser::new(
            shep_core::config::GROUP_ORDER.iter().copied()
        )
    )]
    tab: Option<String>,
    /// Open one dog's table, on the dogs tab
    ///
    /// The dog is one this shepherd lists, or one the sheep already carries a
    /// table for. Refused unless the tab before it is `dogs`: a dog's table is
    /// that tab's own, and no other tab has one.
    #[arg(value_name = "DOG")]
    dog: Option<String>,
}

impl clap::Args for LookoutArgs {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        <LookoutSpec as clap::Args>::augment_args(cmd)
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        <LookoutSpec as clap::Args>::augment_args_for_update(cmd)
    }
}

impl clap::FromArgMatches for LookoutArgs {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Self::finish(<LookoutSpec as clap::FromArgMatches>::from_arg_matches(
            matches,
        )?)
    }

    fn from_arg_matches_mut(matches: &mut clap::ArgMatches) -> Result<Self, clap::Error> {
        Self::finish(<LookoutSpec as clap::FromArgMatches>::from_arg_matches_mut(
            matches,
        )?)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }

    fn update_from_arg_matches_mut(
        &mut self,
        matches: &mut clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches_mut(matches)?;
        Ok(())
    }
}

impl LookoutArgs {
    /// Builds the arguments, refusing a dog that follows any tab but
    /// [`DOG_TAB`].
    ///
    /// The one grammar rule clap cannot say: clap's requirements are between
    /// arguments, never between one argument's value and another's presence.
    /// Positional order already guarantees a tab before a dog, so this is the
    /// whole check, and it runs inside the parse, before any terminal is
    /// opened.
    fn finish(spec: LookoutSpec) -> Result<Self, clap::Error> {
        if let (Some(tab), Some(dog)) = (spec.tab.as_deref(), spec.dog.as_ref())
            && tab != DOG_TAB
        {
            return Err(usage_error(format!(
                "{dog} names a dog's table, and {tab} has none: the third positional needs `dogs` as its tab"
            )));
        }
        Ok(Self {
            read_only: spec.read_only,
            sheep: spec.sheep,
            tab: spec.tab,
            dog: spec.dog,
        })
    }
}

/// A usage error carrying `shep lookout`'s own usage line.
///
/// The error is built here rather than left to clap because the rule it
/// reports is checked here. `bin_name` is spelled out for the same reason
/// [`crate::cli::Cli`] spells it out: every rendering of a verb's usage reads
/// `shep <verb>`, whichever binary produced it.
fn usage_error(why: String) -> clap::Error {
    let mut cmd = <LookoutSpec as clap::Args>::augment_args(
        clap::Command::new("lookout").bin_name("shep lookout"),
    );
    clap::Error::raw(clap::error::ErrorKind::ValueValidation, why).format(&mut cmd)
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

    /// Each of the four shapes the verb takes, read out of the parse rather
    /// than inferred from a usage line: the dashboard, one sheep's status
    /// page, one tab of its config pane, and one dog's table on the `dogs`
    /// tab.
    #[test]
    fn lookout_takes_up_to_three_positionals() {
        use clap::Parser;

        let focus = |argv: &[&str]| {
            let Commands::Lookout(args) = Cli::try_parse_from(argv).unwrap().command else {
                panic!("lookout parses to its own variant")
            };
            (args.sheep, args.tab, args.dog)
        };

        assert_eq!(
            focus(&["shep", "lookout"]),
            (None, None, None),
            "bare lookout is still the dashboard"
        );
        assert_eq!(
            focus(&["shep", "lookout", "web"]),
            (Some("web".to_string()), None, None)
        );
        assert_eq!(
            focus(&["shep", "lookout", "web", "watch"]),
            (Some("web".to_string()), Some("watch".to_string()), None)
        );
        assert_eq!(
            focus(&["shep", "lookout", "web", "dogs", "jobs"]),
            (
                Some("web".to_string()),
                Some("dogs".to_string()),
                Some("jobs".to_string())
            )
        );
    }

    /// Every tab clap will name is a group the pane draws, and the list is
    /// `GROUP_ORDER` itself: a tenth group added there reaches this parse
    /// with no edit here, and a name invented here fails it.
    #[test]
    fn every_tab_is_a_group_the_pane_draws_and_nothing_else_parses() {
        use clap::Parser;

        for group in shep_core::config::GROUP_ORDER {
            let Commands::Lookout(args) = Cli::try_parse_from(["shep", "lookout", "web", group])
                .unwrap()
                .command
            else {
                panic!("lookout parses to its own variant")
            };
            assert_eq!(args.tab.as_deref(), Some(*group), "{group}");
        }

        for argv in [
            vec!["shep", "lookout", "web", "processes"],
            vec!["shep", "lookout", "web", "Dogs"],
            vec!["shep", "lookout", "web", ""],
        ] {
            let err = Cli::try_parse_from(&argv).expect_err(&format!("{argv:?}"));
            assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue, "{argv:?}");
            assert!(err.to_string().contains("possible values"), "{err}");
        }
    }

    /// A dog is a table on the `dogs` tab. On any other tab there is nothing
    /// for the third positional to name, and it is refused by the parse, not
    /// by lookout once the screen is up.
    #[test]
    fn a_third_positional_after_another_tab_is_refused() {
        use clap::Parser;

        for tab in shep_core::config::GROUP_ORDER.iter().copied() {
            let argv = vec!["shep", "lookout", "web", tab, "jobs"];
            let parsed = Cli::try_parse_from(&argv);
            if tab == DOG_TAB {
                assert!(parsed.is_ok(), "{argv:?}");
                continue;
            }
            let err = parsed.expect_err(&format!("{argv:?}"));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{argv:?}"
            );
            let rendered = err.to_string();
            assert!(rendered.contains("jobs"), "{rendered}");
            assert!(rendered.contains(tab), "{rendered}");
            assert!(rendered.contains("shep lookout"), "{rendered}");
        }
    }

    /// `dash` is lookout's visible alias, so it takes the same three. The
    /// alias is what an operator who already has a habit types.
    #[test]
    fn the_alias_takes_the_same_positionals() {
        use clap::Parser;

        let Commands::Lookout(args) = Cli::try_parse_from(["shep", "dash", "web", "dogs", "jobs"])
            .unwrap()
            .command
        else {
            panic!("dash parses to lookout's variant")
        };
        assert_eq!(args.sheep.as_deref(), Some("web"));
        assert_eq!(args.tab.as_deref(), Some("dogs"));
        assert_eq!(args.dog.as_deref(), Some("jobs"));
    }

    /// `DOG_TAB` is the one group name this module spells out, so it is the
    /// one that can drift from the pane's own list without a parse test
    /// noticing.
    #[test]
    fn the_dogs_tab_is_one_of_the_groups_the_pane_draws() {
        assert!(
            shep_core::config::GROUP_ORDER.contains(&DOG_TAB),
            "{DOG_TAB} is not a config pane group"
        );
    }

    /// The positionals sit alongside the gate flag and the host flag, which
    /// is where an operator actually types them.
    #[test]
    fn the_positionals_sit_alongside_the_flags() {
        use clap::Parser;

        let Commands::Lookout(args) = Cli::try_parse_from([
            "shep",
            "--host",
            "amp2",
            "lookout",
            "web",
            "dogs",
            "jobs",
            "--read-only",
        ])
        .unwrap()
        .command
        else {
            panic!("lookout parses to its own variant")
        };
        assert!(args.read_only);
        assert_eq!(args.sheep.as_deref(), Some("web"));
        assert_eq!(args.tab.as_deref(), Some("dogs"));
        assert_eq!(args.dog.as_deref(), Some("jobs"));
    }
}
