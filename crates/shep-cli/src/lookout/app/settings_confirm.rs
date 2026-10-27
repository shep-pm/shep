//! The sentence a settings confirm asks.
//!
//! What a change needs comes off the key's `x-shep-apply` in the schema, so a
//! key added later says the right thing with no sentence written for it.

use shep_core::config::{APPLY_NEXT_COMMAND, APPLY_RELOAD, APPLY_RESTART, APPLY_WHISTLE};

use crate::commands::settings::{STYLE_LEVEL_KEY, SettingEdit};
use crate::lookout::field::shep_toml_shape;
use crate::style::StyleSource;

/// The keys the shepherd's own `SHEP_*` variable or flag outranks, and
/// lookout cannot see either.
const OUTRANKED_AT_BOOT: [&str; 4] = [
    "daemon.log_level",
    "daemon.log_json",
    "daemon.socket",
    "daemon.max_cron_sleep",
];

/// The confirm sentence for `edit`.
///
/// Only `style.level` reads `source`: its resolved level can be outranked by
/// lookout's own flag or environment, which lookout does see.
pub(super) fn confirm_text(edit: &SettingEdit, source: StyleSource) -> String {
    let key = edit.key();
    match edit {
        SettingEdit::Set { value, .. } if key == STYLE_LEVEL_KEY => {
            style_confirm_text(value, source)
        }
        SettingEdit::Set { value, .. } => {
            format!("set {key} to {value}? {}{}", cost(&key), outranked(&key))
        }
        SettingEdit::Unset { .. } if shep_toml_shape().fields.by_key(&key).is_some() => format!(
            "unset {key}? it goes back to the default, and {}",
            cost(&key)
        ),
        SettingEdit::Unset { .. } => format!("remove {key}? {}", cost(&key)),
    }
}

/// What a change to a key with this `x-shep-apply` needs before it takes
/// hold, as the screen's cost column says it.
pub(crate) fn apply_needs(apply: Option<&str>) -> &'static str {
    match apply {
        Some(APPLY_RELOAD) => "needs shep daemon reload",
        Some(APPLY_RESTART) => "needs the shepherd stopped and started",
        Some(APPLY_WHISTLE) => "needs shep whistle restarted",
        Some(APPLY_NEXT_COMMAND) => "the next command reads it",
        _ => "takes hold when shep.toml is next read",
    }
}

/// What a change to `key` needs, for the confirm sentence. A map's entry
/// needs what the map does.
fn cost(key: &str) -> String {
    let shape = shep_toml_shape();
    let field = shape.fields.by_key(key).or_else(|| {
        key.rsplit_once('.')
            .and_then(|(map, _)| shape.fields.by_key(map))
    });
    let apply = field.and_then(|field| field.apply.as_deref());
    let needs = apply_needs(apply);
    if apply == Some(APPLY_RESTART) {
        format!("{needs}; a reload will not move it")
    } else {
        needs.to_owned()
    }
}

/// The warning for a key the shepherd's boot environment can outrank, or
/// nothing.
fn outranked(key: &str) -> String {
    let Some(name) = OUTRANKED_AT_BOOT
        .contains(&key)
        .then(|| key.rsplit('.').next())
        .flatten()
    else {
        return String::new();
    };
    format!(
        ", and will not apply if the shepherd was booted with SHEP_{} or --{}",
        name.to_uppercase(),
        name.replace('_', "-")
    )
}

/// The `[style] level` half of [`confirm_text`].
///
/// Under `Env` or `Flag` the write lands and the level in force does not move,
/// so the sentence names the layer that keeps winning.
fn style_confirm_text(value: &str, source: StyleSource) -> String {
    match source {
        StyleSource::Config | StyleSource::Default => {
            format!("set style level to {value}? the next command reads it")
        }
        StyleSource::Env => format!(
            "set style level to {value}? it goes in the file, but $SHEP_STYLE is set and keeps winning until it is unset"
        ),
        StyleSource::Flag => format!(
            "set style level to {value}? it goes in the file, but --style was passed to this lookout and keeps winning for as long as it runs"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::settings::Shape;

    /// The row of a map draws the map's cost, so the confirm for one of its
    /// entries says the same.
    #[test]
    fn the_confirm_for_a_map_entry_names_the_cost_its_map_row_shows() {
        let map = shep_toml_shape().fields.by_key("interpreters").unwrap();
        let shown = apply_needs(map.apply.as_deref());
        let entry = |edit: SettingEdit| confirm_text(&edit, StyleSource::Default);

        let set = entry(SettingEdit::Set {
            path: vec!["interpreters".into(), "py".into()],
            shape: Shape::Text,
            value: "python3".into(),
        });
        let remove = entry(SettingEdit::Unset {
            path: vec!["interpreters".into(), "py".into()],
        });

        assert_eq!(set, format!("set interpreters.py to python3? {shown}"));
        assert_eq!(remove, format!("remove interpreters.py? {shown}"));
    }
}
