//! Whether this whistle may act, and where that answer comes from.
//!
//! One source: `[whistle] allow_control` in `$SHEP_HOME/shep.toml`. Not a
//! flag, not an environment variable.

use shep_core::config::{DaemonConfig, DaemonConfigError};

/// Whether whistle's control tools exist.
///
/// Not a security boundary, just a fat-finger catch: whistle runs as the
/// operator's own uid. With the gate shut, text a sheep printed cannot reach
/// a tool that acts. Unlike lookout's `Control`, which reads the KV store,
/// this reads `shep.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// The four control tools are not registered. The default.
    ReadOnly,
    /// The four control tools are registered alongside the five read-only ones.
    Allowed,
}

impl Control {
    /// The one sentence that tells an operator how to open the gate.
    ///
    /// Named rather than inlined: the tool catalogue, `get_info` and the
    /// malformed-config notice all say it.
    #[must_use]
    pub const fn how_to_open(self) -> &'static str {
        "control tools are off; add `[whistle]` with `allow_control = true` to \
         $SHEP_HOME/shep.toml and restart whistle"
    }
}

/// Reads the gate out of an already-loaded `shep.toml`.
///
/// An unparsable file reads as `ReadOnly`: a broken config fails closed. The
/// caller prints the parse failure to stderr.
///
/// None of `DaemonConfig::load`'s env vars touch `allow_control`, so the
/// closure is always `&|_| None`.
#[must_use]
pub fn resolve_control(loaded: &Result<DaemonConfig, DaemonConfigError>) -> Control {
    match loaded {
        Ok(config) if config.whistle.allow_control => Control::Allowed,
        _ => Control::ReadOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads the way `whistle()` does, then resolves.
    fn control_of(text: Option<&str>) -> Control {
        resolve_control(&DaemonConfig::load(text, &|_| None))
    }

    /// fails if the file stops being read, or if the default stops being
    /// "no". Both halves matter: a gate that never opens is useless and a
    /// gate that opens by accident is worse than none.
    #[test]
    fn the_file_is_the_only_source_and_it_defaults_to_read_only() {
        assert_eq!(control_of(None), Control::ReadOnly);
        assert_eq!(control_of(Some("")), Control::ReadOnly);
        assert_eq!(
            control_of(Some("[daemon]\nlog_level = \"info\"\n")),
            Control::ReadOnly
        );
        assert_eq!(
            control_of(Some("[whistle]\nallow_control = true\n")),
            Control::Allowed
        );
        assert_eq!(
            control_of(Some("[whistle]\nallow_control = false\n")),
            Control::ReadOnly
        );
    }

    /// fails if a broken config file fails open. A `shep.toml` that will not
    /// parse is exactly the moment something is wrong with the machine, and
    /// a gate that disappears then is a gate that was never there.
    #[test]
    fn a_file_that_will_not_parse_is_read_as_no() {
        assert_eq!(control_of(Some("[whistle")), Control::ReadOnly);
        assert_eq!(
            control_of(Some("[whistle]\nallow_control = \"yes\"\n")),
            Control::ReadOnly,
            "a string where a bool belongs is a broken file, not a true"
        );
    }

    /// fails if the refusal text stops naming the exact edit. An operator
    /// told "control is off" and not told the two lines to write will guess,
    /// and the most likely guess is a flag that does not exist.
    #[test]
    fn the_refusal_names_the_file_and_the_key() {
        let notice = Control::ReadOnly.how_to_open();
        assert!(notice.contains("[whistle]"));
        assert!(notice.contains("allow_control = true"));
        assert!(notice.contains("shep.toml"));
        assert!(
            !notice.contains("--allow-control"),
            "whistle has no such flag; pointing at one would send an operator in a circle"
        );
    }
}
