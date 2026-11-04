//! The dogs section on the settings screen: one header row per dog, folded.
//! The dog's own `[daemon] keys` and config tree hang under that header.
//!
//! A header row joins the file's own `[daemon] enabled_dogs` entry against the
//! live flock by name. `IN FILE` answers the file and `RUNNING` answers the
//! flock. Neither is a claim about the other. The column tiers survive because
//! the header row answers the same four questions in the same order.
//!
//! A dog whose config could not be read says so on its own row, naming
//! `dogs.toml`.

use ratatui::text::{Line, Span};

use super::super::super::app::{App, DogState, DogTree, Settings};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::super::pane::open_glyph;
use crate::vocabulary::Reported;

/// The dogs caption, verbatim: arms and confirms like every other lookout
/// action (`x`, `R`, `L`), and `e` opens a dog's own tree.
pub(super) const DOGS_CAPTION: &str =
    "space arms, Enter applies; e opens a dog; a dog needs no reload";

/// The label of the section's last row.
const BROWSE_LABEL: &str = "browse dogs";

/// What `Enter` does on [`BROWSE_LABEL`]'s row.
const BROWSE_HINT: &str = "Enter opens the dog index";

/// The floor on the dog header row's NAME column, mirroring
/// [`super::super::flock::columns::NAME_MIN`]: never shrinks below a name worth
/// reading.
const DOG_NAME_MIN: u16 = 8;

/// One column of a dog's header row.
///
/// `Debug` is derived, not redacted: a bare variant name leaks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DogColumn {
    /// The dog's name, with the glyph that says whether its tree is open. The
    /// flexible column.
    Name,
    /// Whether `[daemon] enabled_dogs` names it, not whether the shepherd
    /// has started it.
    InFile,
    /// `built-in`, `installed`, `installed, community` or `adopted`.
    Source,
    /// The dog's version, with `\u{2192} <new>` when the cached index holds
    /// a newer one.
    Version,
    /// Whether the dog is up: [`running_of`] joins it against the live flock,
    /// reading `not running` when no dog by that name is running.
    Running,
}

impl DogColumn {
    /// The fixed width of this column's cells. `Name` reports `0`; see
    /// [`super::super::flock::columns::Column::width`].
    #[must_use]
    const fn width(self) -> u16 {
        match self {
            Self::Name => 0,
            // `IN FILE` header; yes/no cells are shorter.
            Self::InFile => 7,
            // `installed, community`.
            Self::Source => 20,
            // `0.23.0 \u{2192} 0.24.0` and a little over.
            Self::Version => 16,
            // matches `super::super::flock::Column::Status`'s width.
            Self::Running => 15,
        }
    }
}

const ALL_DOG_COLUMNS: &[DogColumn] = &[
    DogColumn::Name,
    DogColumn::InFile,
    DogColumn::Source,
    DogColumn::Version,
    DogColumn::Running,
];
const NO_VERSION: &[DogColumn] = &[
    DogColumn::Name,
    DogColumn::InFile,
    DogColumn::Source,
    DogColumn::Running,
];
const NO_SOURCE: &[DogColumn] = &[DogColumn::Name, DogColumn::InFile, DogColumn::Running];
const FLOOR_DOG_COLUMNS: &[DogColumn] = &[DogColumn::Name, DogColumn::Running];

/// The narrowest the dog header rows will draw into: [`DogColumn::Running`] plus
/// [`DOG_NAME_MIN`] plus one gap.
///
/// A row width, not a terminal width, like [`super::super::flock::MIN_WIDTH`]:
/// the narrowest terminal this row fits is `DOG_MIN_WIDTH + GUTTER`.
/// [`DOG_TIERS`]'s thresholds compare against [`super::body_width`], never the
/// raw terminal.
pub(super) const DOG_MIN_WIDTH: u16 = DogColumn::Running.width() + DOG_NAME_MIN + 2;

/// Width thresholds, widest first, mirroring `flock::columns::TIERS`:
/// least-diagnostic column drops first. VERSION drops, then SOURCE, then IN
/// FILE; NAME and RUNNING are the floor.
pub(super) const DOG_TIERS: &[(u16, &[DogColumn])] = &[
    (75, ALL_DOG_COLUMNS),
    (57, NO_VERSION),
    (34, NO_SOURCE),
    (DOG_MIN_WIDTH, FLOOR_DOG_COLUMNS),
];

/// The widest dog-header column set that fits `width`.
///
/// Includes [`DogColumn::Running`] in every tier, alongside NAME: it is the
/// floor, and [`dog_header_line`] renders every column this returns.
#[must_use]
fn columns_for(width: u16) -> &'static [DogColumn] {
    DOG_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(FLOOR_DOG_COLUMNS, |(_, columns)| *columns)
}

/// What NAME gets once the fixed columns and their separators are paid for.
/// [`super::super::flock::columns::name_width`]'s own twin.
fn dog_name_width(width: u16, columns: &[DogColumn]) -> u16 {
    let fixed: u16 = columns.iter().map(|column| column.width()).sum();
    let gaps = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(0) * 2;
    width
        .saturating_sub(fixed)
        .saturating_sub(gaps)
        .max(DOG_NAME_MIN)
}

/// How much of a refusal the header row tries to make room for.
///
/// The longest of the sentences a probe failure puts here ("bark publishes no
/// schema; edit `dogs.toml` with $EDITOR") is about this wide, and NAME is the
/// only column that can pay for it: without a reservation the flexible column
/// spends the whole row and the refusal is fitted to nothing.
const REFUSAL_ROOM: u16 = 54;

/// What the running flock says about the dog named `name`, or [`None`] when no
/// dog of that name is running.
///
/// Joined by name rather than by row: the settings snapshot comes from
/// `shep.toml` and the flock comes from the shepherd, and the two can disagree
/// about a dog the operator removed from one and not the other.
#[must_use]
fn running_of(app: &App, name: &str) -> Option<String> {
    app.all_rows()
        .into_iter()
        .find(|row| row.info.dog.is_some() && row.info.name == name)
        .map(|row| Reported::of(row.info.status, row.info.handshook).word())
}

/// One index dog this machine lacks, [`SettingsRow::AvailableDog`]: its name,
/// the words `not installed`, and its category, fitted to `width`, a BODY
/// width.
///
/// Greyed with [`Palette::muted`], and said in words too, so a terminal with
/// no colour still tells it from an installed dog's row. The name takes what
/// the two words after it leave, so a narrow terminal cuts the name first.
///
/// [`SettingsRow::AvailableDog`]: super::super::super::app::SettingsRow::AvailableDog
pub(super) fn available_dog_line(
    settings: &Settings,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let Some(row) = settings.available().get(index) else {
        return Line::default();
    };
    let chars = |text: &str| u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    // Names line up across the dogs this screen lists, and give way before
    // the state and the category do.
    let longest = settings
        .available()
        .iter()
        .map(|row| chars(&row.dog.name))
        .max()
        .unwrap_or(0);
    let state = DogState::NotInstalled.as_str();
    let tail = format!("{state}  {}", row.dog.category);
    let name_width = longest
        .min(width.saturating_sub(chars(&tail) + 2))
        .max(DOG_NAME_MIN)
        .min(width);
    let body = format!("{}  {tail}", fit(&row.dog.name, name_width));
    // `mark` spends two columns of the width, so the rest is fitted to what
    // is left.
    let room = width.saturating_sub(2);
    Line::from(vec![
        Span::raw(format!("{} ", mark(selected))),
        Span::styled(fit(&body, room).trim_end().to_owned(), palette.muted()),
    ])
}

/// The row that ends the section, [`SettingsRow::BrowseDogs`]: its label and
/// a hint for `Enter`, fitted to `width`, a BODY width.
///
/// The label is fitted first and the hint takes what it leaves, so a narrow
/// terminal cuts the hint and keeps the name.
///
/// [`SettingsRow::BrowseDogs`]: super::super::super::app::SettingsRow::BrowseDogs
pub(super) fn browse_dogs_line(selected: bool, width: u16, palette: Palette) -> Line<'static> {
    let label_width = u16::try_from(BROWSE_LABEL.chars().count())
        .unwrap_or(width)
        .min(width);
    let label = fit(BROWSE_LABEL, label_width);
    let room = width.saturating_sub(label_width).saturating_sub(2);
    let hint = if room == 0 {
        String::new()
    } else {
        format!("  {}", fit(BROWSE_HINT, room).trim_end())
    };
    Line::from(vec![
        Span::raw(format!("{} {}", mark(selected), label)),
        Span::styled(hint, palette.muted()),
    ])
}

/// One dog's header row: the fold glyph and its name, whether the file has it
/// enabled, where its binary came from, its version, and whether it is up.
///
/// A refusal is a fifth thing, so it lands after the last column, in
/// whatever width is left. It is fitted rather than wrapped: a narrow
/// terminal cuts the sentence. `e` on the row says the whole of it in the
/// status bar.
pub(super) fn dog_header_line(
    app: &App,
    settings: &Settings,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let Some(dog) = settings.dog(index) else {
        return Line::default();
    };
    let open = settings
        .dog_tree(index)
        .is_some_and(|tree| tree.is_open() || matches!(tree, DogTree::Probing));
    let columns = columns_for(width);
    // A refusal is a fifth cell on a row that only has room for four, and NAME
    // is the flexible one, so NAME pays for it. Without this the name column
    // spends the whole row and the sentence is fitted to nothing.
    let refusal = match settings.dog_tree(index) {
        Some(DogTree::Refused { why }) => Some(why.clone()),
        _ => None,
    };
    let owed = u16::try_from(
        refusal
            .as_ref()
            .map_or(0, |why| why.chars().count().min(usize::from(REFUSAL_ROOM))),
    )
    .unwrap_or(REFUSAL_ROOM)
        + 2;
    let name_width = dog_name_width(width, columns)
        .saturating_sub(owed)
        .max(DOG_NAME_MIN);
    let mut text = format!("{} ", mark(selected));
    let mut used = 0u16;
    for (at, column) in columns.iter().enumerate() {
        if at > 0 {
            text.push_str("  ");
            used += 2;
        }
        let cell_width = if *column == DogColumn::Name {
            name_width
        } else {
            column.width()
        };
        let cell = match column {
            DogColumn::Name => format!("{} {}", open_glyph(open), dog.name),
            DogColumn::InFile => if dog.enabled { "yes" } else { "no" }.to_string(),
            DogColumn::Source => dog.origin.source.as_str().to_string(),
            DogColumn::Version => dog.origin.version_cell(),
            DogColumn::Running => {
                running_of(app, &dog.name).unwrap_or_else(|| "not running".into())
            }
        };
        text.push_str(&fit(&cell, cell_width));
        used += cell_width;
    }

    let mut spans = vec![Span::raw(text)];
    if let Some(why) = refusal {
        let room = width.saturating_sub(used).saturating_sub(2);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(fit(&why, room), palette.refusal()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::super::fixtures;
    use super::super::super::flock::GUTTER;
    use super::super::content_lines;
    use super::*;
    use crate::commands::settings::DogView;
    use crate::output::width::visible_width;

    /// VERSION is the least diagnostic, then SOURCE; RUNNING outlives both.
    #[test]
    fn the_dogs_version_column_drops_before_source_and_source_before_the_rest() {
        assert!(columns_for(120).contains(&DogColumn::Version));
        assert!(!columns_for(70).contains(&DogColumn::Version));
        assert!(columns_for(70).contains(&DogColumn::Source));
        assert!(!columns_for(50).contains(&DogColumn::Source));
        assert!(
            columns_for(50).contains(&DogColumn::Running),
            "RUNNING is the diagnostic half and outlives SOURCE"
        );
    }

    fn header_of(dog: DogView, width: u16) -> String {
        let mut app = fixtures::app_in_settings_with_dog_drift();
        let named = dog.name.clone();
        app.update(crate::lookout::app::Msg::Settings {
            result: Ok(crate::commands::settings::SettingsSnapshot {
                dogs: vec![dog],
                ..app.settings().unwrap().snapshot().clone()
            }),
        });
        let settings = app.settings().unwrap();
        assert_eq!(
            settings.dog(0).map(|d| d.name.as_str()),
            Some(named.as_str())
        );
        dog_header_line(&app, settings, 0, false, width, app.palette())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn the_header_row_names_each_dogs_source_and_version() {
        use crate::dog_origin::{DogOrigin, Source};
        let installed = DogView {
            origin: DogOrigin {
                source: Source::Community,
                version: Some("0.2.4".to_owned()),
                newer: Some("0.3.0".to_owned()),
            },
            ..DogView::adopted("rotate", true, std::path::Path::new("/opt/rotate"))
        };
        let line = header_of(installed, 120);
        assert!(line.contains("installed, community"), "{line:?}");
        assert!(line.contains("0.2.4 \u{2192} 0.3.0"), "{line:?}");

        let line = header_of(
            DogView::adopted("otel", true, std::path::Path::new("/opt/otel")),
            120,
        );
        assert!(line.contains("adopted"), "{line:?}");
        assert!(
            !line.contains("/opt/otel"),
            "the path is not the source: {line:?}"
        );

        let line = header_of(DogView::built_in("bark", true), 120);
        assert!(line.contains("built-in"), "{line:?}");
        assert!(line.contains("1.0.0"), "{line:?}");
    }

    /// Measures the rendered lines, not the declared widths: each line
    /// carries `mark`'s own two-column prefix, which a width sum would miss.
    #[test]
    fn every_dogs_tier_fits_the_width_it_claims() {
        let app = fixtures::app_in_settings_with_dog_drift();
        let settings = app.settings().unwrap();
        let palette = app.palette();
        for width in (DOG_MIN_WIDTH + GUTTER)..=200 {
            let rendered: Vec<String> = content_lines(&app, settings, palette, width, 0)
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            // Everything from the `[dogs]` caption down: the caption and every
            // row under it, open or closed.
            let table = rendered
                .iter()
                .position(|line| line.contains("[dogs]"))
                .expect("the dogs section is always drawn");
            for line in &rendered[table..] {
                assert!(
                    visible_width(line) <= usize::from(width),
                    "width {width} drew {} columns: {line:?}",
                    visible_width(line)
                );
            }
        }
    }

    #[test]
    fn the_dogs_caption_names_the_key_that_opens_a_dog() {
        assert_eq!(
            DOGS_CAPTION,
            "space arms, Enter applies; e opens a dog; a dog needs no reload"
        );
    }

    /// `otel` runs while the file has it disabled, which is what "a removed
    /// name keeps running" looks like. `ledger` is enabled and absent,
    /// which is a dog that failed to start.
    #[test]
    fn the_dogs_row_joins_the_file_against_the_running_flock() {
        let app = fixtures::app_in_settings_with_dog_drift();
        let otel = app
            .settings()
            .unwrap()
            .snapshot()
            .dogs
            .iter()
            .position(|dog| dog.name == "otel")
            .expect("the fixture carries otel");
        let line = dog_header_line(
            &app,
            app.settings().unwrap(),
            otel,
            false,
            120,
            app.palette(),
        )
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
        assert!(line.contains("no"), "in file: {line:?}");
        assert!(line.contains("online"), "running: {line:?}");

        let ledger = app
            .settings()
            .unwrap()
            .snapshot()
            .dogs
            .iter()
            .position(|dog| dog.name == "ledger")
            .expect("the fixture carries ledger");
        let line = dog_header_line(
            &app,
            app.settings().unwrap(),
            ledger,
            false,
            120,
            app.palette(),
        )
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
        assert!(line.contains("yes"), "in file: {line:?}");
        assert!(line.contains("not running"), "running: {line:?}");
    }

    /// A dog that never completed a handshake reads `silent`, not `online`.
    #[test]
    fn a_dog_that_never_handshook_reads_silent_here_too() {
        let app = fixtures::app_in_settings_with_silent_dog();
        let settings = app.settings().unwrap();
        let bark = settings
            .snapshot()
            .dogs
            .iter()
            .position(|dog| dog.name == "bark")
            .expect("the fixture carries bark");
        assert_eq!(running_of(&app, "bark").as_deref(), Some("silent"));
        let line = dog_header_line(&app, settings, bark, false, 120, app.palette())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(line.contains("silent"), "{line:?}");
    }

    fn available_row(app: &App, index: usize, width: u16, palette: Palette) -> Line<'static> {
        available_dog_line(app.settings().unwrap(), index, false, width, palette)
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Greyed is a colour, and a terminal with none shows the words instead.
    #[test]
    fn the_greyed_rows_say_not_installed_and_the_category_and_differ_with_no_colour() {
        let app = fixtures::app_in_settings_with_an_index();
        let otel = text_of(&available_row(&app, 0, 80, fixtures::plain()));
        let tail = text_of(&available_row(&app, 1, 80, fixtures::plain()));
        for (row, name, category) in [(&otel, "otel", "metrics"), (&tail, "tail-grep", "logs")] {
            assert!(row.contains(name), "{row:?}");
            assert!(row.contains("not installed"), "{row:?}");
            assert!(row.contains(category), "{row:?}");
        }
        assert_ne!(otel, tail);

        let drawn = content_lines(&app, app.settings().unwrap(), fixtures::plain(), 80, 0)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let installed = drawn
            .iter()
            .find(|line| line.contains("rotate"))
            .unwrap_or_else(|| panic!("the adopted dog is drawn: {drawn:?}"));
        assert!(
            !installed.contains("not installed"),
            "an installed dog's row is not marked: {installed:?}"
        );
        let at = |name: &str| drawn.iter().position(|line| line.contains(name));
        assert!(at("rotate") < at("otel"), "after the installed: {drawn:?}");
        assert!(
            at("tail-grep") < at("browse dogs"),
            "before browse: {drawn:?}"
        );
    }

    #[test]
    fn a_greyed_row_is_painted_muted_where_there_is_colour() {
        let app = fixtures::app_in_settings_with_an_index();
        let palette = fixtures::coloured();
        let line = available_row(&app, 0, 80, palette);
        let painted = line.spans.last().expect("the row has a body");
        assert_eq!(painted.style, palette.muted());
        assert_ne!(palette.muted(), ratatui::style::Style::default());
    }

    #[test]
    fn a_greyed_row_fits_the_narrow_floor() {
        use crate::lookout::view::MIN_TERM_WIDTH;
        let app = fixtures::app_in_settings_with_an_index();
        for width in [MIN_TERM_WIDTH, 50, 120] {
            let line = text_of(&available_row(&app, 1, width, fixtures::plain()));
            assert!(
                visible_width(&line) <= usize::from(width),
                "{width}: {line:?}"
            );
        }
    }
}
