//! The settings tree's rows: which columns fit, and what each cell says.
//!
//! NAME, VALUE, SOURCE and the apply cost, measured against a BODY
//! width ([`super::body_width`]) rather than the raw terminal. A section is a
//! row of its own with only a name.

use ratatui::text::{Line, Span};

use super::super::super::app::{DogKey, Settings, SettingsRow, apply_needs};
use super::super::super::field::{Field, shep_toml_shape};
use super::super::super::pane::{ChoiceRow, PaneRow, UNSET};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::super::pane::open_glyph;
use crate::commands::settings::{DogGrants, STYLE_LEVEL_KEY, ScalarView};
use crate::style::StyleSource;

/// NAME column width: fits `boot_first_dogs`, the longest key, two columns
/// in under its section, with a column of padding.
const ROW_NAME_W: u16 = 18;
/// VALUE column width where the terminal can afford it: fits
/// `/home/ada/.shep/run/shep.sock` (29 columns) whole. A cap, not a fixed
/// width: a narrow terminal gets [`ROW_VALUE_MIN`] instead, and anything
/// wider truncates through [`fit`].
const ROW_VALUE_W: u16 = 30;
/// The floor on VALUE: enough for `not set`, `waiting`, a level name or the
/// tail of a path, never a whole socket path. Below this the row stops
/// saying anything and there is nothing left to trade.
const ROW_VALUE_MIN: u16 = 10;
/// SOURCE column width: `$SHEP_STYLE` and `the default` are both 11 columns,
/// the widest two words [`crate::style::StyleSource`]'s `Display` ever
/// prints.
const ROW_SOURCE_W: u16 = 11;
/// The floor on the apply-cost column once the terminal is too narrow to
/// give it the remainder: enough for `needs`, never a whole sentence.
const ROW_COST_MIN: u16 = 8;

/// One column of a row.
///
/// `Debug` is derived, not redacted: a bare variant name leaks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowColumn {
    /// The document's own key for this field.
    Name,
    /// What the file says, or the compiled fallback when it says nothing.
    /// The flexible column, between [`ROW_VALUE_MIN`] and
    /// [`ROW_VALUE_W`].
    Value,
    /// Which layer the value came from.
    Source,
    /// What applying an edit to this field costs. Takes whatever the other
    /// three leave.
    Cost,
}

/// The columns of a row that has nothing but a name: its name, and nothing
/// after it. Padding the empty cells out to their column widths would push a
/// name fitted to the body past the terminal.
const NAME_ONLY: &[RowColumn] = &[RowColumn::Name];

const ROW_ALL: &[RowColumn] = &[
    RowColumn::Name,
    RowColumn::Value,
    RowColumn::Source,
    RowColumn::Cost,
];
const ROW_NO_COST: &[RowColumn] = &[RowColumn::Name, RowColumn::Value, RowColumn::Source];
const ROW_FLOOR: &[RowColumn] = &[RowColumn::Name, RowColumn::Value];

/// The narrowest the rows will draw into, as a body width: NAME, a
/// gap and VALUE at its floor. The terminal also pays
/// [`super::super::flock::GUTTER`], the same split
/// [`super::dogs::DOG_MIN_WIDTH`] documents.
const ROW_MIN_WIDTH: u16 = ROW_NAME_W + 2 + ROW_VALUE_MIN;

/// Width thresholds for the rows, widest first, the same shape
/// [`super::dogs::DOG_TIERS`] and `flock::columns::TIERS` both use.
///
/// The apply cost drops first, then SOURCE, the reverse of the dogs
/// table's order: the cost also shows in the confirm and the status bar,
/// while SOURCE appears nowhere else. NAME and VALUE are the floor.
const ROW_TIERS: &[(u16, &[RowColumn])] = &[
    (
        ROW_NAME_W + 2 + ROW_VALUE_MIN + 2 + ROW_SOURCE_W + 2 + ROW_COST_MIN,
        ROW_ALL,
    ),
    (
        ROW_NAME_W + 2 + ROW_VALUE_MIN + 2 + ROW_SOURCE_W,
        ROW_NO_COST,
    ),
    (ROW_MIN_WIDTH, ROW_FLOOR),
];

/// The widest scalar column set that fits `width`, a BODY width.
fn row_columns_for(width: u16) -> &'static [RowColumn] {
    ROW_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(ROW_FLOOR, |(_, columns)| *columns)
}

/// What VALUE and the apply cost get out of `width`, once NAME, SOURCE and
/// the separators are paid for.
///
/// VALUE takes the remainder up to [`ROW_VALUE_W`], never below
/// [`ROW_VALUE_MIN`]. The cost takes what is left, so a wide terminal spends
/// its extra columns on the sentence. The cost is zero when
/// [`RowColumn::Cost`] is not in `columns`.
fn row_widths(width: u16, columns: &[RowColumn]) -> (u16, u16) {
    let fixed = ROW_NAME_W
        + if columns.contains(&RowColumn::Source) {
            ROW_SOURCE_W
        } else {
            0
        };
    let gaps = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(0) * 2;
    let remainder = width.saturating_sub(fixed).saturating_sub(gaps);
    if columns.contains(&RowColumn::Cost) {
        let value = remainder
            .saturating_sub(ROW_COST_MIN)
            .clamp(ROW_VALUE_MIN, ROW_VALUE_W);
        (value, remainder.saturating_sub(value).max(ROW_COST_MIN))
    } else {
        (remainder.clamp(ROW_VALUE_MIN, ROW_VALUE_W), 0)
    }
}

/// The cell texts of one row.
struct Cells {
    name: String,
    value: String,
    source: String,
    cost: String,
}

/// What applying a change to `field` costs, given the layer its value came
/// from. `style.level` is the one key a layer outranks: lookout's own
/// `--style` and `$SHEP_STYLE` beat the file, which makes "the next command
/// reads it" untrue.
fn cost_of(field: &Field, source: StyleSource) -> String {
    if field.key == STYLE_LEVEL_KEY && matches!(source, StyleSource::Env | StyleSource::Flag) {
        return "written, but outranked".to_owned();
    }
    apply_needs(field.apply.as_deref()).to_owned()
}

/// `name` indented to `depth`, after the glyph if the row folds.
fn name_cell(depth: usize, open: Option<bool>, name: &str) -> String {
    let indent = "  ".repeat(depth);
    match open {
        Some(open) => format!("{indent}{} {name}", open_glyph(open)),
        None => format!("{indent}{name}"),
    }
}

/// The last segment of the dotted key `key`.
fn leaf_name(key: &str) -> String {
    shep_toml_shape()
        .paths
        .get(key)
        .and_then(|path| path.last().cloned())
        .unwrap_or_else(|| key.to_owned())
}

/// How many tables sit above the key `key`.
fn depth_of(key: &str) -> usize {
    shep_toml_shape()
        .paths
        .get(key)
        .map_or(0, |path| path.len().saturating_sub(1))
}

fn cells(settings: &Settings, row: SettingsRow) -> Option<Cells> {
    let shape = shep_toml_shape();
    let fields = shape.fields.fields();
    let snapshot = settings.snapshot();
    match row {
        SettingsRow::Table(index) => {
            let table = &shape.outline.tables[index];
            Some(Cells {
                name: name_cell(table.depth, Some(settings.is_open(&table.path)), &table.key),
                value: String::new(),
                source: String::new(),
                cost: String::new(),
            })
        }
        SettingsRow::Field(index) => {
            let field = &fields[index];
            let ScalarView { value, source } = snapshot.view(field);
            let open = (field.kind == crate::lookout::field::FieldKind::Map)
                .then(|| settings.is_open(&field.key));
            Some(Cells {
                name: name_cell(depth_of(&field.key), open, &leaf_name(&field.key)),
                cost: cost_of(field, source),
                value,
                source: source.to_string(),
            })
        }
        SettingsRow::Entry { map, index } => {
            let field = &fields[map];
            let (name, value) = snapshot.entries(&field.key).into_iter().nth(index)?;
            Some(Cells {
                name: name_cell(depth_of(&field.key) + 1, None, &name),
                value,
                source: StyleSource::Config.to_string(),
                cost: cost_of(field, StyleSource::Config),
            })
        }
        SettingsRow::AddEntry(map) => Some(Cells {
            name: name_cell(depth_of(&fields[map].key) + 1, None, "+ add an entry"),
            value: String::new(),
            source: String::new(),
            cost: String::new(),
        }),
        // The dog's own header row is not this function's: it answers four
        // questions in four columns and lives in [`super::dogs`].
        SettingsRow::Dog(_) => None,
        SettingsRow::DogKey { dog, key } => {
            let dog = settings.dog(dog)?;
            Some(Cells {
                name: name_cell(1, None, key.label()),
                value: dog_key_value(key, dog),
                source: String::new(),
                cost: key_cost(key).to_owned(),
            })
        }
        SettingsRow::DogGrant { dog, index } => {
            let dog = settings.dog(dog)?;
            let permission = dog.offered.get(index)?;
            // The checklist #801 put on a dog's pane, one indent deeper here
            // because it hangs under this screen's `grants` row rather than
            // under a field of a dog's own schema.
            let row = ChoiceRow {
                depth: 2,
                label: permission.clone(),
                held: dog.grants.grants(permission),
                known: true,
            };
            Some(Cells {
                name: name_cell(row.depth, None, &row.text()),
                value: String::new(),
                source: String::new(),
                cost: String::new(),
            })
        }
        SettingsRow::DogEnv(dog) => {
            let dog = settings.dog(dog)?;
            Some(Cells {
                name: name_cell(1, Some(settings.is_env_open(&dog.name)), "env"),
                value: String::new(),
                source: String::new(),
                cost: String::new(),
            })
        }
        // A value is never held here, so a key reads `<set>` the way a secret
        // does and the cell has nothing else it could say.
        SettingsRow::DogEnvKey { dog, index } => {
            let key = settings.dog(dog)?.env_keys.get(index)?;
            Some(Cells {
                name: name_cell(2, None, key),
                value: ENV_VALUE.to_owned(),
                source: String::new(),
                cost: DOG_ENV_COST.to_owned(),
            })
        }
        SettingsRow::DogEnvNew(_) => Some(Cells {
            name: name_cell(2, None, "+ new"),
            value: String::new(),
            source: String::new(),
            cost: String::new(),
        }),
        SettingsRow::DogField { dog, row } => {
            let pane = settings.dog_tree(dog)?.pane()?;
            let pane_row = pane.rows().get(row)?.to_owned();
            let tree = pane.tree_row(pane_row)?;
            let (value, cost) = match pane_row {
                PaneRow::Field(index) => {
                    let field = pane.fields().fields().get(index)?;
                    (
                        masked(field.secret, pane.display_value(&field.key)),
                        if pane.lock(&field.key).is_some() {
                            "read-only".to_owned()
                        } else {
                            DOG_SECTION_COST.to_owned()
                        },
                    )
                }
                _ => (String::new(), String::new()),
            };
            Some(Cells {
                name: name_cell(tree.depth + 1, tree.open, &tree.label),
                value,
                source: String::new(),
                cost,
            })
        }
    }
}

/// What a dog's own field costs to change, in one cell. The dog's section is
/// `dogs.toml`, the shepherd writes it, and the dog is told: none of that is a
/// reload, which is what the tree's own cost column is usually deciding.
const DOG_SECTION_COST: &str = "dogs.toml, it is told";

/// What an env key's cell says: that it is set, as a secret's does.
const ENV_VALUE: &str = "<set>";

/// What changing a dog's env key costs. A dog already up keeps the env it
/// started with, so the shepherd has to build its app again.
const DOG_ENV_COST: &str = "needs shep daemon reload";

/// A secret's cell: whether there is one, never the value. The same rule
/// `view::pane::layout::mask_secret` applies to a dog pane's row.
fn masked(secret: bool, raw: String) -> String {
    if secret && raw != UNSET {
        ENV_VALUE.to_owned()
    } else {
        raw
    }
}

/// A boolean as one cell.
fn yes_no(flag: bool) -> String {
    if flag { "yes" } else { "no" }.to_owned()
}

/// What the VALUE cell of one of a dog's `[daemon] key` rows says.
fn dog_key_value(key: DogKey, dog: &crate::commands::settings::DogView) -> String {
    match key {
        DogKey::BootFirst => yes_no(dog.boot_first),
        DogKey::Channel => yes_no(dog.channel),
        DogKey::Source => dog
            .adopted_path
            .as_ref()
            .map_or_else(|| "built-in".to_owned(), |path| path.display().to_string()),
        DogKey::Grants => match &dog.grants {
            // The same fact `shep dogs grant` refuses on, in the row that
            // would otherwise look like an empty checklist.
            DogGrants::Undeclared => "nothing recorded".to_owned(),
            DogGrants::Unreadable(why) => why.clone(),
            DogGrants::Recorded(_) if dog.offered.is_empty() => "nothing declared".to_owned(),
            DogGrants::Recorded(_) => format!(
                "{} of {} held",
                dog.offered.iter().filter(|p| dog.grants.grants(p)).count(),
                dog.offered.len()
            ),
        },
    }
}

/// What changing one of a dog's `[daemon] keys costs. None of them is a
/// reload: `boot_first_dogs` is read at boot, `dog_grants` when the shepherd
/// starts the dog, and the other two are not this screen's to change at all.
fn key_cost(key: DogKey) -> &'static str {
    match key {
        DogKey::BootFirst => "the next boot reads it",
        DogKey::Grants => "its next start reads it",
        DogKey::Source => "shep adopt writes it",
        DogKey::Channel => "the dog's own answer",
    }
}

fn cell_text(cells: &Cells, column: RowColumn) -> &str {
    match column {
        RowColumn::Name => &cells.name,
        RowColumn::Value => &cells.value,
        RowColumn::Source => &cells.source,
        RowColumn::Cost => &cells.cost,
    }
}

/// One row of the tree: name, value, source, apply cost, as many of those as
/// `width` (a BODY width) can pay for. A section and the add row carry a name
/// only, and are muted unless selected.
pub(super) fn tree_line(
    settings: &Settings,
    row: SettingsRow,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let Some(cells) = cells(settings, row) else {
        return Line::default();
    };
    let all = row_columns_for(width);
    let (value_width, cost_width) = row_widths(width, all);
    // A row with nothing but a name has no column to leave room for, so its
    // name takes the whole body and the empty cells are not drawn at all. A
    // checklist item's permission name is longer than the tree's NAME column
    // and has nowhere else it could go.
    let name_only = cells.value.is_empty() && cells.source.is_empty() && cells.cost.is_empty();
    let columns = if name_only { NAME_ONLY } else { all };
    let mut text = format!("{} ", mark(selected));
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            text.push_str("  ");
        }
        let cell_width = match column {
            RowColumn::Name => {
                if name_only {
                    width
                } else {
                    ROW_NAME_W
                }
            }
            RowColumn::Value => value_width,
            RowColumn::Source => ROW_SOURCE_W,
            RowColumn::Cost => cost_width,
        };
        text.push_str(&fit(cell_text(&cells, *column), cell_width));
    }
    let nameless = cells.value.is_empty() && cells.source.is_empty();
    if nameless && !selected {
        Line::from(Span::styled(text, palette.muted()))
    } else {
        Line::from(Span::raw(text))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fixtures;
    use super::super::content_lines;
    use super::*;
    use crate::style::StyleSource;

    /// Widths here are BODY widths,
    /// [`super::super::super::flock::GUTTER`] columns narrower than the
    /// terminal.
    #[test]
    fn the_apply_cost_drops_before_its_source() {
        assert!(row_columns_for(118).contains(&RowColumn::Cost));
        assert!(!row_columns_for(43).contains(&RowColumn::Cost));
        assert!(
            row_columns_for(43).contains(&RowColumn::Source),
            "SOURCE is the screen's own subject and outlives the cost"
        );
        assert!(!row_columns_for(33).contains(&RowColumn::Source));
        assert!(
            row_columns_for(33).contains(&RowColumn::Value),
            "NAME and VALUE are the floor"
        );
    }

    #[test]
    fn the_default_source_label_fits_the_column_it_was_sized_for() {
        let rendered = fit(&StyleSource::Default.to_string(), ROW_SOURCE_W);
        assert_eq!(
            rendered.chars().count(),
            usize::from(ROW_SOURCE_W),
            "fit always pads or truncates to the exact column width"
        );
        assert!(
            !rendered.contains('…'),
            "ROW_SOURCE_W must fit \"the default\" whole: got {rendered:?}"
        );
    }

    fn rows_at(app: &crate::lookout::app::App, width: u16) -> Vec<String> {
        content_lines(app, app.settings().unwrap(), app.palette(), width, 0)
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    }

    #[test]
    fn a_key_shows_its_name_value_source_and_what_changing_it_needs() {
        let app = fixtures::app_in_settings();
        let rows = rows_at(&app, 120);
        let row = rows
            .iter()
            .find(|line| line.contains("max_cron_sleep"))
            .unwrap_or_else(|| panic!("the key is a row: {rows:?}"));
        for cell in ["30s", "shep.toml", "needs shep daemon reload"] {
            assert!(row.contains(cell), "{cell} is missing: {row:?}");
        }
    }

    #[test]
    fn a_section_is_a_row_and_its_keys_sit_a_level_under_it() {
        let app = fixtures::app_in_settings();
        let rows = rows_at(&app, 120);
        let section = rows
            .iter()
            .position(|line| line.contains("▾ whistle"))
            .unwrap_or_else(|| panic!("whistle is a row: {rows:?}"));
        assert!(
            rows[section + 1].starts_with("    allow_control"),
            "indented under it: {rows:?}"
        );
    }

    #[test]
    fn the_style_cost_cell_stops_promising_the_next_command_when_it_is_outranked() {
        let app = fixtures::app_in_settings_with_shadowed_style(StyleSource::Env);
        let rows = rows_at(&app, 120);
        let row = rows
            .iter()
            .find(|line| {
                line.get(2..)
                    .is_some_and(|cells| cells.trim_start().starts_with("level "))
            })
            .unwrap_or_else(|| panic!("the style row is drawn: {rows:?}"));
        assert!(row.contains("$SHEP_STYLE"), "got: {row:?}");
        assert!(row.contains("written, but outranked"), "got: {row:?}");
        assert!(!row.contains("the next command reads it"), "got: {row:?}");
    }
}
