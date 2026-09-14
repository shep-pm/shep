//! What the keymap overlay prints, derived from `map_key` rather than
//! listed beside it.
//!
//! [`binding`] is an exhaustive match over [`KeyPress`] with no wildcard
//! arm, so a new variant does not compile until it has a row here, and
//! [`rows`] builds the list by pushing [`PROBE`] through
//! [`super::input::map_key`] itself. A binding cannot reach the reducer
//! without also reaching the overlay, and a row cannot name a key nothing
//! is bound to.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::app::{ActionVerb, InputMode, KeyPress};
use super::input::map_key;
use crate::vocabulary::Role;

/// How many entry rows one column of the overlay has.
///
/// The box is nineteen rows: a border pair, a heading row, twelve entries,
/// a blank, the gate line, and two closing lines. Zero spare, which
/// `every_group_fits_its_column` is what guards.
pub(super) const ENTRY_ROWS: usize = 12;

/// How wide the key-caption cell is.
pub(super) const KEY_CELL: u16 = 12;

/// How wide the sentence cell is.
pub(super) const TEXT_CELL: u16 = 17;

/// One of the overlay's four columns, grouped by what the keys in it do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Moving,
    Looking,
    Changing,
    Doing,
    /// Not a column. The one row that draws on the overlay's closing line
    /// instead: `q  ctrl-c`, which has to come out of [`binding`] like
    /// every other variant but must not take one of `Looking`'s twelve
    /// rows, since `Looking` is already at twelve.
    Closing,
}

impl Group {
    /// The four that draw as columns, left to right. [`Self::Closing`] is
    /// not among them.
    pub(super) const DRAWN: [Self; 4] = [Self::Moving, Self::Looking, Self::Changing, Self::Doing];

    /// The column's heading, or `""` for [`Self::Closing`], which has no
    /// column to head.
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Moving => "MOVING",
            Self::Looking => "LOOKING",
            Self::Changing => "CHANGING",
            Self::Doing => "DOING",
            Self::Closing => "",
        }
    }

    /// The heading's colour role. `Doing` is bark because its three keys
    /// are the destructive ones; the rest are meadow.
    pub(super) const fn role(self) -> Role {
        match self {
            Self::Doing => Role::Bark,
            Self::Moving | Self::Looking | Self::Changing | Self::Closing => Role::Meadow,
        }
    }
}

/// One row: the key column, its group, and the sentence beside it.
///
/// `keys` is at most [`KEY_CELL`] characters and `does` at most
/// [`TEXT_CELL`]; `the_cells_fit_their_widths` asserts both rather than
/// leaving a long one to be cut on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Binding {
    pub keys: &'static str,
    pub group: Group,
    pub does: &'static str,
}

/// The overlay's row for `press`.
///
/// Exhaustive, with no wildcard arm and no `Action(_)` catch-all, so both
/// [`KeyPress`] and [`ActionVerb`] have to grow a row here before they
/// compile. That is the whole guard against a binding that dispatches and
/// does not print.
const fn binding(press: &KeyPress) -> Binding {
    // `row` keeps each arm to one line, so a reader checks thirty-six
    // captions rather than thirty-six struct literals. Thirty-six, not the
    // thirty-five this said: `the_rows_are_deduplicated` asserts the count,
    // and thirty-five is how many land in a DRAWN column, which is the
    // number the doc on `rows` gives.
    const fn row(keys: &'static str, group: Group, does: &'static str) -> Binding {
        Binding { keys, group, does }
    }
    match press {
        KeyPress::SelectDown | KeyPress::SelectUp => {
            row("j/k  \u{2191}/\u{2193}", Group::Moving, "select a row")
        }
        KeyPress::SelectFirst | KeyPress::SelectLast => {
            row("g/G home/end", Group::Moving, "first / last")
        }
        KeyPress::StepDown | KeyPress::StepUp => row("J/K", Group::Moving, "next / prev sheep"),
        KeyPress::PageDown | KeyPress::PageUp => row("ctrl-d/u", Group::Moving, "page the feed"),
        KeyPress::TabPrev | KeyPress::TabNext => {
            row("\u{2190}/\u{2192}", Group::Moving, "environment tab")
        }
        KeyPress::NextGroup => row("tab", Group::Moving, "next config group"),
        KeyPress::Group(_) => row("1-8", Group::Moving, "jump to a group"),
        KeyPress::MatchNext | KeyPress::MatchPrev => row("n/N", Group::Moving, "next / prev match"),
        KeyPress::Confirm => row("\u{21b5}", Group::Moving, "open selection"),
        KeyPress::Escape => row("esc", Group::Moving, "back one level"),

        KeyPress::Help => row("h  ?", Group::Looking, "this keymap"),
        KeyPress::Bleats => row("b", Group::Looking, "feed, full screen"),
        KeyPress::FoldView => row("F", Group::Looking, "gather by fold"),
        KeyPress::Collapse => row("z", Group::Looking, "collapse a fold"),
        KeyPress::StreamCycle => row("o", Group::Looking, "out / err / both"),
        KeyPress::LevelCycle => row("m", Group::Looking, "minimum level"),
        KeyPress::FollowToggle => row("f", Group::Looking, "follow the tail"),
        KeyPress::WrapToggle => row("w", Group::Looking, "wrap long lines"),
        KeyPress::FilterStart => row("/", Group::Looking, "filter, or match"),
        KeyPress::Refresh => row("r", Group::Looking, "refresh now"),
        KeyPress::Reveal => row("v", Group::Looking, "reveal for 10s"),
        KeyPress::Copy => row("y", Group::Looking, "copy the value"),

        KeyPress::Edit => row("e", Group::Changing, "the config pane"),
        KeyPress::Cycle => row("space", Group::Changing, "cycle a value"),
        KeyPress::Remove => row("d", Group::Changing, "restore default"),
        KeyPress::Undo => row("u", Group::Changing, "undo the edit"),
        KeyPress::Settings => row("s", Group::Changing, "shepherd settings"),
        KeyPress::Secrets => row("S", Group::Changing, "secrets"),
        KeyPress::SecretDelete => row("D", Group::Changing, "delete a secret"),
        KeyPress::Continue => row("c", Group::Changing, "leave it running"),
        KeyPress::TextChar(_) => row("a-z 0-9", Group::Changing, "types into a box"),
        KeyPress::TextBackspace | KeyPress::TextApply | KeyPress::TextAbandon => row(
            "\u{232b} \u{21b5} esc",
            Group::Changing,
            "erase, file, drop",
        ),

        KeyPress::Action(ActionVerb::Stop) => row("x", Group::Doing, "stop"),
        KeyPress::Action(ActionVerb::Restart) => row("R", Group::Doing, "restart"),
        KeyPress::Action(ActionVerb::Reload) => row("L", Group::Doing, "reload"),

        // `q` draws on the closing line beside `h or ? closes this`, not in
        // a column: `Looking` is at twelve of twelve and this would be its
        // thirteenth. `Group::Closing` is how it comes out of this match
        // without taking a column row.
        KeyPress::Quit => row("q  ctrl-c", Group::Closing, "quit"),
    }
}

/// Every key the overlay claims, as `map_key` would see it.
///
/// One entry per row rather than one per key: `Char('a')` stands for every
/// letter the text-mode row covers and `Char('1')` for the eight group
/// digits. `every_key_map_key_binds_is_in_the_probe` sweeps the whole
/// keyboard against this, so a key missing here is a test failure rather
/// than a row missing from the box.
const PROBE: &[(KeyCode, KeyModifiers)] = &[
    (KeyCode::Char('j'), KeyModifiers::NONE),
    (KeyCode::Char('g'), KeyModifiers::NONE),
    (KeyCode::Char('J'), KeyModifiers::SHIFT),
    (KeyCode::Char('d'), KeyModifiers::CONTROL),
    (KeyCode::Left, KeyModifiers::NONE),
    (KeyCode::Tab, KeyModifiers::NONE),
    (KeyCode::Char('1'), KeyModifiers::NONE),
    (KeyCode::Char('n'), KeyModifiers::NONE),
    (KeyCode::Enter, KeyModifiers::NONE),
    (KeyCode::Esc, KeyModifiers::NONE),
    (KeyCode::Char('h'), KeyModifiers::NONE),
    (KeyCode::Char('b'), KeyModifiers::NONE),
    (KeyCode::Char('F'), KeyModifiers::SHIFT),
    (KeyCode::Char('z'), KeyModifiers::NONE),
    (KeyCode::Char('o'), KeyModifiers::NONE),
    (KeyCode::Char('m'), KeyModifiers::NONE),
    (KeyCode::Char('f'), KeyModifiers::NONE),
    (KeyCode::Char('w'), KeyModifiers::NONE),
    (KeyCode::Char('/'), KeyModifiers::NONE),
    (KeyCode::Char('r'), KeyModifiers::NONE),
    (KeyCode::Char('v'), KeyModifiers::NONE),
    (KeyCode::Char('y'), KeyModifiers::NONE),
    (KeyCode::Char('e'), KeyModifiers::NONE),
    (KeyCode::Char(' '), KeyModifiers::NONE),
    (KeyCode::Char('d'), KeyModifiers::NONE),
    (KeyCode::Char('u'), KeyModifiers::NONE),
    (KeyCode::Char('s'), KeyModifiers::NONE),
    (KeyCode::Char('S'), KeyModifiers::SHIFT),
    (KeyCode::Char('D'), KeyModifiers::SHIFT),
    (KeyCode::Char('c'), KeyModifiers::NONE),
    (KeyCode::Char('a'), KeyModifiers::NONE),
    (KeyCode::Backspace, KeyModifiers::NONE),
    (KeyCode::Char('x'), KeyModifiers::NONE),
    (KeyCode::Char('R'), KeyModifiers::SHIFT),
    (KeyCode::Char('L'), KeyModifiers::SHIFT),
    (KeyCode::Char('q'), KeyModifiers::NONE),
];

/// [`rows`], generalised over which probe entries to run, so
/// `every_probe_entry_binds_something` can ask "what would this draw
/// without entry N" without a second copy of the loop.
fn rows_from(probe: &[(KeyCode, KeyModifiers)]) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    for group in Group::DRAWN.into_iter().chain([Group::Closing]) {
        for (code, modifiers) in probe {
            let event = Event::Key(KeyEvent::new(*code, *modifiers));
            let Some(press) =
                map_key(&event, InputMode::Normal).or_else(|| map_key(&event, InputMode::Text))
            else {
                continue;
            };
            let binding = binding(&press);
            if binding.group == group && !out.iter().any(|seen| seen.keys == binding.keys) {
                out.push(binding);
            }
        }
    }
    out
}

/// Every row the overlay draws, grouped, in the order each column lists
/// them.
///
/// Built by running [`PROBE`] through [`map_key`], so nothing here names a
/// key the reducer does not dispatch on.
pub(super) fn rows() -> Vec<Binding> {
    rows_from(PROBE)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::output::width::visible_width;

    /// Every `KeyPress` `map_key` can produce is produced by some `PROBE`
    /// entry too.
    ///
    /// The sweep is the whole surface: `map_key` matches on `Char`, `Esc`,
    /// `Enter`, `Backspace`, `Tab`, `BackTab`, the four arrows, `Home`,
    /// `End` and `F(n)`, and nothing else. A binding added to `map_key` and
    /// not to `PROBE` fails here rather than going missing from the
    /// overlay, which is the whole reason the rows are built by running
    /// `map_key` instead of listed beside it.
    #[test]
    fn every_key_map_key_binds_is_in_the_probe() {
        let probed: Vec<KeyPress> = PROBE
            .iter()
            .filter_map(|(code, modifiers)| {
                map_key(
                    &Event::Key(KeyEvent::new(*code, *modifiers)),
                    InputMode::Normal,
                )
            })
            .chain(PROBE.iter().filter_map(|(code, modifiers)| {
                map_key(
                    &Event::Key(KeyEvent::new(*code, *modifiers)),
                    InputMode::Text,
                )
            }))
            .collect();

        for (code, modifiers) in every_key() {
            for mode in [InputMode::Normal, InputMode::Text] {
                let Some(press) = map_key(&Event::Key(KeyEvent::new(code, modifiers)), mode) else {
                    continue;
                };
                assert!(
                    probed.iter().any(|seen| same_row(seen, &press)),
                    "{code:?} with {modifiers:?} in {mode:?} binds {press:?}, \
                     which no PROBE entry produces"
                );
            }
        }
    }

    /// And the reverse: `PROBE` cannot accumulate keys that stopped being
    /// bound.
    ///
    /// `map_key`'s `InputMode::Text` branch binds every non-ALT `Char` as
    /// `TextChar`, so `normal.is_some() || text.is_some()` alone is
    /// satisfied by any printable character whether or not it is still
    /// bound in `Normal` — the realistic way a `PROBE` entry goes stale.
    /// Classifying by the *observed* press does not fix this: a letter
    /// that lost its `Normal` binding collapses to exactly the same
    /// `TextChar` the text-only representative produces, so no property
    /// of the resulting [`KeyPress`] tells the two apart.
    ///
    /// What does tell them apart is [`rows`] itself: an entry that does
    /// not bind in `Normal` is only legitimate if it is the *only* one
    /// standing for its row — remove it and that row would disappear.
    /// `Char('a')` is necessary this way, since nothing else produces the
    /// `a-z 0-9` row; a stray unclaimed letter added later is not, since
    /// `Char('a')` (or whichever entry already claims the row) still
    /// covers it without the new one. This is derived from [`rows_from`]
    /// rather than a second list of which entries are text-only.
    #[test]
    fn every_probe_entry_binds_something() {
        for (index, (code, modifiers)) in PROBE.iter().enumerate() {
            let event = Event::Key(KeyEvent::new(*code, *modifiers));
            let normal = map_key(&event, InputMode::Normal);
            let text = map_key(&event, InputMode::Text);
            let press = normal.or(text).unwrap_or_else(|| {
                panic!("{code:?} with {modifiers:?} is in PROBE and binds nothing")
            });
            if normal.is_some() {
                continue;
            }
            let without_this: Vec<(KeyCode, KeyModifiers)> = PROBE
                .iter()
                .enumerate()
                .filter(|(seen, _)| *seen != index)
                .map(|(_, entry)| *entry)
                .collect();
            let row = binding(&press);
            let still_covered = rows_from(&without_this)
                .iter()
                .any(|seen| seen.keys == row.keys);
            assert!(
                !still_covered,
                "{code:?} with {modifiers:?} does not bind in InputMode::Normal, and \
                 its row ({}) is already covered without it, so it adds nothing \
                 the way a text-only representative would",
                row.keys
            );
        }
    }

    /// No group has more entries than its column has rows.
    ///
    /// Zero spare at the time of writing: `Looking` has twelve against
    /// twelve. A forty-third binding compiles, because `binding` gives it a
    /// row, and then overflows the box. This is the test that names which
    /// group grew.
    #[test]
    fn every_group_fits_its_column() {
        for group in Group::DRAWN {
            let count = rows().iter().filter(|row| row.group == group).count();
            assert!(
                count <= ENTRY_ROWS,
                "{group:?} has {count} entries against {ENTRY_ROWS} rows"
            );
        }
    }

    /// Thirty-six rows, thirty-five of them in a drawn column, and no two
    /// share a key caption.
    ///
    /// The thirty-sixth is `q  ctrl-c`, in `Group::Closing`: it has no row
    /// of its own in the box, which is what bought `Looking` its twelfth
    /// entry. Both counts are asserted, because one alone would pass if a
    /// row migrated between a column and the closing line.
    #[test]
    fn the_rows_are_deduplicated() {
        let rows = rows();
        let captions: Vec<&str> = rows.iter().map(|row| row.keys).collect();
        assert_eq!(rows.len(), 36, "{captions:?}");
        assert_eq!(
            rows.iter()
                .filter(|row| Group::DRAWN.contains(&row.group))
                .count(),
            35,
            "{captions:?}"
        );
        for (index, row) in rows.iter().enumerate() {
            assert!(
                !rows[index + 1..].iter().any(|other| other.keys == row.keys),
                "{} appears twice",
                row.keys
            );
        }
    }

    /// The two keys the design named wrongly, asserted here as well as in
    /// `input.rs`, because this is the file the overlay prints from.
    #[test]
    fn the_overlay_names_the_keys_that_shipped_not_the_ones_drawn() {
        let rows = rows();
        assert!(
            rows.iter()
                .any(|row| row.keys == "b" && row.does.contains("feed")),
            "the feed is on `b`, not the design's `l`"
        );
        assert!(
            rows.iter()
                .any(|row| row.keys == "S" && row.does.contains("secrets")),
            "secrets is on `S`, not the design's `g`"
        );
        assert!(
            !rows.iter().any(|row| row.keys == "l"),
            "`l` is unbound and must not appear"
        );
    }

    /// Every key the overlay prints in a caption is a key `map_key` binds.
    ///
    /// Catches the drift the derivation cannot: a caption edited to name a
    /// key that was never bound. Single-character captions only, since a
    /// caption like `g/G home/end` names four keys in one cell.
    ///
    /// ASCII only: a single non-ASCII character is a symbol standing for a
    /// named `KeyCode` rather than something anyone types — `\u{21b5}` for
    /// `Enter`, `\u{232b}` for `Backspace` — so looking it up as
    /// `Char(that_symbol)` is the wrong query and would fail for a caption
    /// that is correct. `view/status.rs`'s secrets hint (line ~429) writes
    /// this same `\u{21b5}` glyph for the identical job, confirming the
    /// glyph is the shipped convention rather than a mistake to route
    /// around here.
    #[test]
    fn a_single_character_caption_names_a_bound_key() {
        for row in rows() {
            let mut chars = row.keys.chars();
            let Some(only) = chars.next() else { continue };
            if chars.next().is_some() || !only.is_ascii() {
                continue;
            }
            assert!(
                map_key(
                    &Event::Key(KeyEvent::new(KeyCode::Char(only), KeyModifiers::NONE)),
                    InputMode::Normal
                )
                .is_some(),
                "the overlay prints `{only}` and `map_key` does not bind it"
            );
        }
    }

    /// `DRAWN`'s headings, which several `contains` assertions across two
    /// modules silently rest on.
    ///
    /// Non-empty first, and this is not hypothetical: `Closing`'s heading
    /// IS `""`, deliberately, because the quit row draws no bank header.
    /// `Closing` is not in `DRAWN`, so nothing iterating `DRAWN` meets it
    /// today. `str::contains("")` is always true, so the day a heading
    /// becomes empty, or `Closing` joins `DRAWN`, every "this heading
    /// reached the screen" assertion passes on a screen that drew nothing.
    ///
    /// Then pairwise, which is the other way those assertions go wrong: the
    /// column-count tests count how many of the four headings a bank row
    /// contains, so a heading that is a substring of another is counted for
    /// both and the count is right for the wrong reason. `MOVING`,
    /// `LOOKING`, `CHANGING` and `DOING` are distinct and none contains
    /// another.
    ///
    /// Both halves are mutation-checked, and the first mutation tried was
    /// the wrong one: renaming `Moving` to `MOVING FAST` survives, because
    /// there is then no separate `MOVING` for it to collide with. The
    /// pairwise half needs one DRAWN heading to contain ANOTHER DRAWN one,
    /// which `Looking => "DOING MORE"` does, and that fails.
    #[test]
    fn every_drawn_heading_is_a_word_of_its_own() {
        for group in Group::DRAWN {
            assert!(!group.heading().is_empty(), "{group:?} draws a bank header");
        }
        assert!(
            Group::Closing.heading().is_empty(),
            "the quit row draws no bank header, so its heading stays empty"
        );
        for (index, one) in Group::DRAWN.iter().enumerate() {
            for other in &Group::DRAWN[index + 1..] {
                assert!(
                    !one.heading().contains(other.heading())
                        && !other.heading().contains(one.heading()),
                    "{one:?} and {other:?} share a heading substring"
                );
            }
        }
    }

    /// Every caption fits the cell it draws into, so nothing is cut on
    /// screen. The widths are `view::keymap`'s, asserted here because this
    /// is where the strings are written.
    ///
    /// Both bounds are a pair, because `<=` alone cannot see a caption that
    /// is too SHORT: `visible_width("")` is 0 and fits every cell. An empty
    /// `does` would pass that and pass `every_derived_row_is_drawn` too,
    /// since `str::contains("")` is always true, so nothing in the suite
    /// would have said a row drew no words.
    #[test]
    fn the_cells_fit_their_widths() {
        for row in rows() {
            assert!(
                (1..=usize::from(KEY_CELL)).contains(&visible_width(row.keys)),
                "the `{}` caption is {} cells against {KEY_CELL}",
                row.keys,
                visible_width(row.keys)
            );
            assert!(
                (1..=usize::from(TEXT_CELL)).contains(&visible_width(row.does)),
                "`{}` is {} cells against {TEXT_CELL}",
                row.does,
                visible_width(row.does)
            );
        }
    }

    /// Every `KeyCode` variant `map_key` can match, bare and with CONTROL.
    fn every_key() -> Vec<(KeyCode, KeyModifiers)> {
        let mut keys: Vec<KeyCode> = (0x20u8..=0x7E).map(|b| KeyCode::Char(b as char)).collect();
        keys.extend([
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
        ]);
        keys.extend((1..=12).map(KeyCode::F));
        keys.into_iter()
            .flat_map(|code| {
                [
                    (code, KeyModifiers::NONE),
                    (code, KeyModifiers::CONTROL),
                    (code, KeyModifiers::SHIFT),
                ]
            })
            .collect()
    }

    /// Whether two presses land on the same overlay row. `TextChar('a')`
    /// and `TextChar('z')` are one row, and `Group(1)` and `Group(8)` are
    /// one row, so equality is the wrong comparison.
    fn same_row(left: &KeyPress, right: &KeyPress) -> bool {
        binding(left).keys == binding(right).keys
    }
}
