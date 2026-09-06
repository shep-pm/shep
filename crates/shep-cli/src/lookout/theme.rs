//! The design language's semantic colours, mapped onto a terminal.
//!
//! Maps `--meadow` (online, healthy), `--bark` (errored, refused,
//! destructive), `--butter` (attention) and `--ink-3` (muted) onto 16 or
//! 256 terminal colours, per `docs/shep-design/README.md`.
//!
//! `--paper` is painted in exactly two places now: the selected row and the
//! status bar, through [`Palette::ground`]. Everywhere else it still stays
//! [`Color::Reset`], so the operator's own terminal background shows
//! through ordinary text. `--barn` is scenery-only and has no analog here.
//!
//! Every coloured cell's text already says the same thing, so `NO_COLOR`
//! costs decoration, never information.

use ratatui::style::{Color, Modifier, Style};
use std::ffi::OsStr;

use shep_core::status::ProcStatus;

use crate::vocabulary::Reported;

/// The four semantic colours, resolved for one terminal.
///
/// Constructed once at startup by `lookout`, from `Palette::detect`, and
/// carried in `super::app::App`. Never re-derived per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    meadow: Option<Color>,
    bark: Option<Color>,
    butter: Option<Color>,
    ink3: Option<Color>,
    sky: Option<Color>,
    line: Option<Color>,
    paper2: Option<Color>,
    gauge_rest: Option<Color>,
}

impl Palette {
    /// Resolves the palette from the environment, taken as arguments rather
    /// than read here so callers can test it without touching `std::env`.
    ///
    /// An empty `NO_COLOR=` counts as unset, the cross-ecosystem convention.
    /// A terminal claiming truecolor, 24-bit or 256-colour support gets the
    /// indexed colours; anything else gets the 16 named ones, since a
    /// shallow terminal fed a 256-colour escape can print it as literal text.
    #[must_use]
    pub fn detect(
        no_color: Option<&OsStr>,
        term: Option<&OsStr>,
        colorterm: Option<&OsStr>,
    ) -> Self {
        if crate::style::no_color_set(no_color) {
            return Self {
                meadow: None,
                bark: None,
                butter: None,
                ink3: None,
                sky: None,
                line: None,
                paper2: None,
                gauge_rest: None,
            };
        }
        let deep = crate::style::deep_colour_terminal(term, colorterm);

        if deep {
            // xterm-256 indices chosen as the nearest neighbours of the design
            // language's own hexes: 29 #00875f for --meadow #2E8B57, 166
            // #d75f00 for --bark #E0552B, 221 #ffd75f for --butter #F3C44C,
            // 245 #8a8a8a for --ink-3 #7A8C80. 74, 238, 235 and 236 have no
            // hex counterpart in the design language: sky, line and the
            // gauge's ground are lookout-only, chosen for legibility against
            // 235's own dark ground.
            Self {
                meadow: Some(Color::Indexed(29)),
                bark: Some(Color::Indexed(166)),
                butter: Some(Color::Indexed(221)),
                ink3: Some(Color::Indexed(245)),
                sky: Some(Color::Indexed(74)),
                line: Some(Color::Indexed(238)),
                paper2: Some(Color::Indexed(235)),
                gauge_rest: Some(Color::Indexed(236)),
            }
        } else {
            Self {
                meadow: Some(Color::Green),
                bark: Some(Color::Red),
                butter: Some(Color::Yellow),
                ink3: Some(Color::DarkGray),
                sky: Some(Color::Blue),
                line: Some(Color::DarkGray),
                // The 16-colour set has no quiet dark ground, and a plain
                // `Black` background is wrong on a light terminal: callers
                // fall back to an ASCII marker instead.
                paper2: None,
                gauge_rest: Some(Color::DarkGray),
            }
        }
    }

    /// The style for a group row's STATUS cell, where only a bare
    /// `ProcStatus` is available.
    ///
    /// `Errored` is the only status that gets `--bark`; `waiting-restart` is
    /// `--butter`, a state to watch rather than damage that happened.
    /// `Stopping` and `Stopped` are muted.
    #[must_use]
    pub fn status(self, status: ProcStatus) -> Style {
        self.role_style(crate::vocabulary::role_of(status))
    }

    /// The style for one row's STATUS cell, sheep or dog. A silent dog wears
    /// `--butter` here exactly as it does in `shep flock`'s own table.
    #[must_use]
    pub fn reported(self, reported: Reported) -> Style {
        self.role_style(reported.role())
    }

    /// The mapping lives in `crate::vocabulary`, so the CLI's table and this
    /// pane cannot drift.
    fn role_style(self, role: crate::vocabulary::Role) -> Style {
        Self::fg(self.of(role))
    }

    /// One role's resolved colour, shared by [`Self::role_style`] and
    /// [`Self::band`] so the mapping is written once.
    fn of(self, role: crate::vocabulary::Role) -> Option<Color> {
        match role {
            crate::vocabulary::Role::Meadow => self.meadow,
            crate::vocabulary::Role::Butter => self.butter,
            crate::vocabulary::Role::Bark => self.bark,
            crate::vocabulary::Role::Ink3 => self.ink3,
            crate::vocabulary::Role::Sky => self.sky,
        }
    }

    /// Muted: column headers, the home path in the title, key hints.
    #[must_use]
    pub fn muted(self) -> Style {
        Self::fg(self.ink3)
    }

    /// Damage that has happened: the frozen banner, a failed poll.
    #[must_use]
    pub fn alarm(self) -> Style {
        Self::fg(self.bark)
    }

    /// A refused action: `--bark`'s third permitted use, alongside errored
    /// and destructive.
    #[must_use]
    pub fn refusal(self) -> Style {
        Self::fg(self.bark)
    }

    /// Something to look at that is not damage: reconnecting, a dropped-event
    /// notice.
    #[must_use]
    pub fn attention(self) -> Style {
        Self::fg(self.butter)
    }

    /// The memory gauge's filled portion.
    ///
    /// No non-test caller yet: a later task in the landing-pane revamp
    /// draws the gauge. `#[allow(dead_code)]` says so rather than inventing
    /// one.
    #[must_use]
    #[allow(dead_code)]
    pub fn sky(self) -> Style {
        Self::fg(self.sky)
    }

    /// Box-drawing lines: pane borders, the flock table's rules.
    ///
    /// No non-test caller yet; see [`Self::sky`].
    #[must_use]
    #[allow(dead_code)]
    pub fn line(self) -> Style {
        Self::fg(self.line)
    }

    /// The memory gauge's unfilled portion.
    ///
    /// No non-test caller yet; see [`Self::sky`].
    #[must_use]
    #[allow(dead_code)]
    pub fn gauge_rest(self) -> Style {
        Self::fg(self.gauge_rest)
    }

    /// Reverse video over a role's own colour, naming no background.
    ///
    /// The terminal supplies the text colour from its own background, so a
    /// light terminal is right without a second palette. `REVERSED` survives
    /// `NO_COLOR`: it is a modifier rather than a colour, and it is what keeps
    /// a band naming the mode on a monochrome terminal.
    ///
    /// No non-test caller yet; see [`Self::sky`].
    #[must_use]
    #[allow(dead_code)]
    pub fn band(self, role: crate::vocabulary::Role) -> Style {
        Self::fg(self.of(role)).add_modifier(Modifier::REVERSED)
    }

    /// The one painted background: the selected row and the status bar.
    ///
    /// Reverses the module doc's rule for exactly two rows. Ordinary ground
    /// still stays [`Color::Reset`], so the operator's own background shows
    /// through everywhere else. `None` under `NO_COLOR` and on the
    /// 16-colour tier, where the callers fall back to the ASCII marker.
    ///
    /// No non-test caller yet; see [`Self::sky`].
    #[must_use]
    #[allow(dead_code)]
    pub fn ground(self) -> Style {
        self.paper2
            .map_or_else(Style::default, |colour| Style::default().bg(colour))
    }

    fn fg(colour: Option<Color>) -> Style {
        colour.map_or_else(Style::default, |colour| Style::default().fg(colour))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn no_color_flattens_the_palette_and_an_empty_one_does_not() {
        let off = Palette::detect(Some(OsStr::new("1")), None, None);
        assert_eq!(off.status(ProcStatus::Errored), Style::default());
        assert_eq!(off.muted(), Style::default());

        let empty = Palette::detect(
            Some(OsStr::new("")),
            Some(OsStr::new("xterm-256color")),
            None,
        );
        assert_ne!(empty.status(ProcStatus::Errored), Style::default());
    }

    /// A 16-colour terminal sent an unrecognized escape can print it as
    /// literal text, which is worse than a flatter palette.
    #[test]
    fn an_unknown_terminal_gets_the_sixteen_colour_palette() {
        let sixteen = Palette::detect(None, Some(OsStr::new("vt100")), None);
        assert_eq!(sixteen.status(ProcStatus::Errored).fg, Some(Color::Red));
        assert_eq!(sixteen.status(ProcStatus::Online).fg, Some(Color::Green));

        let deep = Palette::detect(None, Some(OsStr::new("xterm-256color")), None);
        assert_eq!(
            deep.status(ProcStatus::Errored).fg,
            Some(Color::Indexed(166))
        );

        let truecolor = Palette::detect(
            None,
            Some(OsStr::new("dumb")),
            Some(OsStr::new("truecolor")),
        );
        assert_eq!(
            truecolor.status(ProcStatus::Online).fg,
            Some(Color::Indexed(29))
        );
    }

    /// `waiting-restart` is the live temptation: it is `--butter`, attention,
    /// not damage.
    #[test]
    fn bark_is_reserved_for_errored_and_nothing_else() {
        let p = Palette::detect(None, Some(OsStr::new("xterm-256color")), None);
        let bark = Some(Color::Indexed(166));
        assert_eq!(p.status(ProcStatus::Errored).fg, bark);
        for other in [
            ProcStatus::Online,
            ProcStatus::Starting,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::WaitingRestart,
        ] {
            assert_ne!(
                p.status(other).fg,
                bark,
                "{other} must not be bark-coloured"
            );
        }
        // The two non-status uses the design language does allow.
        assert_eq!(p.alarm().fg, bark);
        assert_eq!(p.refusal().fg, bark);
    }

    #[test]
    fn every_status_is_legible_with_no_colour_at_all() {
        let off = Palette::detect(Some(OsStr::new("1")), None, None);
        let mut seen = std::collections::BTreeSet::new();
        for status in [
            ProcStatus::Online,
            ProcStatus::Starting,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ] {
            assert_eq!(off.status(status), Style::default());
            assert!(
                seen.insert(status.to_string()),
                "two statuses share one word"
            );
        }
        assert_eq!(seen.len(), 6);
    }

    /// This binding and `output::paint`'s must resolve every role to the
    /// same colour, at both tiers. Compares the extracted index or name
    /// rather than a shared literal, so a renumbering on one side that
    /// forgets the other still fails this.
    #[test]
    fn the_anstyle_binding_agrees_with_this_ones_colours() {
        use crate::vocabulary::Role;

        let deep = Palette::detect(None, Some(OsStr::new("xterm-256color")), None);
        let shallow = Palette::detect(None, Some(OsStr::new("vt100")), None);

        for (status, role) in [
            (ProcStatus::Online, Role::Meadow),
            (ProcStatus::WaitingRestart, Role::Butter),
            (ProcStatus::Stopped, Role::Ink3),
            (ProcStatus::Errored, Role::Bark),
        ] {
            let ratatui_deep = deep
                .status(status)
                .fg
                .expect("the deep tier always sets a foreground");
            let anstyle_deep = crate::output::paint::style_for(role, true)
                .get_fg_color()
                .expect("style_for always sets a foreground");
            assert_eq!(
                ansi256_index(ratatui_deep),
                ansi256_index_anstyle(anstyle_deep),
                "{role:?} disagrees at the 256-colour tier"
            );

            let ratatui_shallow = shallow
                .status(status)
                .fg
                .expect("the shallow tier always sets a foreground");
            let anstyle_shallow = crate::output::paint::style_for(role, false)
                .get_fg_color()
                .expect("style_for always sets a foreground");
            assert_eq!(
                named_colour(ratatui_shallow),
                named_colour_anstyle(anstyle_shallow),
                "{role:?} disagrees at the 16-colour tier"
            );
        }

        // Sky has no `ProcStatus`, so it cannot ride the loop above: it is
        // compared directly instead of through `.status()`.
        let anstyle_sky_deep = crate::output::paint::style_for(Role::Sky, true)
            .get_fg_color()
            .expect("style_for always sets a foreground");
        assert_eq!(
            ansi256_index(deep.sky().fg.expect("deep sky is always set")),
            ansi256_index_anstyle(anstyle_sky_deep),
            "sky disagrees at the 256-colour tier"
        );
        let anstyle_sky_shallow = crate::output::paint::style_for(Role::Sky, false)
            .get_fg_color()
            .expect("style_for always sets a foreground");
        assert_eq!(
            named_colour(shallow.sky().fg.expect("shallow sky is always set")),
            named_colour_anstyle(anstyle_sky_shallow),
            "sky disagrees at the 16-colour tier"
        );
    }

    #[test]
    fn the_deep_tier_carries_every_new_role() {
        let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
        assert_eq!(deep.sky(), Style::default().fg(Color::Indexed(74)));
        assert_eq!(deep.line(), Style::default().fg(Color::Indexed(238)));
        assert_eq!(deep.gauge_rest(), Style::default().fg(Color::Indexed(236)));
    }

    #[test]
    fn the_shallow_tier_names_the_new_roles_too() {
        let shallow = Palette::detect(None, Some(OsStr::new("dumb")), None);
        assert_eq!(shallow.sky(), Style::default().fg(Color::Blue));
        assert_eq!(shallow.line(), Style::default().fg(Color::DarkGray));
        assert_eq!(shallow.gauge_rest(), Style::default().fg(Color::DarkGray));
    }

    #[test]
    fn a_band_is_reverse_video_and_never_names_a_background() {
        let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
        let band = deep.band(crate::vocabulary::Role::Meadow);
        assert_eq!(band.fg, Some(Color::Indexed(29)));
        assert_eq!(
            band.bg, None,
            "a band names no background: the terminal supplies the text colour"
        );
        assert!(band.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn no_color_drops_the_colour_and_keeps_the_reverse() {
        let off = Palette::detect(Some(OsStr::new("1")), None, None);
        let band = off.band(crate::vocabulary::Role::Meadow);
        assert_eq!(band.fg, None);
        assert_eq!(band.bg, None);
        assert!(
            band.add_modifier.contains(Modifier::REVERSED),
            "NO_COLOR is about colour; a band still has to name the mode"
        );
        assert_eq!(
            off.ground(),
            Style::default(),
            "no painted ground without colour"
        );
        assert_eq!(off.sky(), Style::default());
    }

    #[test]
    fn a_ground_is_the_one_painted_background() {
        let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
        let ground = deep.ground();
        assert_eq!(ground.bg, Some(Color::Indexed(235)));
        assert_eq!(
            ground.fg, None,
            "the row's own cells keep their own foreground"
        );
    }

    fn ansi256_index(c: Color) -> u8 {
        match c {
            Color::Indexed(i) => i,
            other => panic!("expected an indexed ratatui colour, got {other:?}"),
        }
    }

    fn ansi256_index_anstyle(c: anstyle::Color) -> u8 {
        match c {
            anstyle::Color::Ansi256(indexed) => indexed.0,
            other => panic!("expected an Ansi256 anstyle colour, got {other:?}"),
        }
    }

    /// A colour-family name independent of either crate's own enum spelling,
    /// so `ratatui::style::Color::DarkGray` and
    /// `anstyle::AnsiColor::BrightBlack` compare equal.
    fn named_colour(c: Color) -> &'static str {
        match c {
            Color::Green => "green",
            Color::Red => "red",
            Color::Yellow => "yellow",
            Color::DarkGray => "bright-black",
            Color::Blue => "blue",
            other => panic!("no name recorded for {other:?}"),
        }
    }

    fn named_colour_anstyle(c: anstyle::Color) -> &'static str {
        match c {
            anstyle::Color::Ansi(anstyle::AnsiColor::Green) => "green",
            anstyle::Color::Ansi(anstyle::AnsiColor::Red) => "red",
            anstyle::Color::Ansi(anstyle::AnsiColor::Yellow) => "yellow",
            anstyle::Color::Ansi(anstyle::AnsiColor::BrightBlack) => "bright-black",
            anstyle::Color::Ansi(anstyle::AnsiColor::Blue) => "blue",
            other => panic!("no name recorded for {other:?}"),
        }
    }
}
