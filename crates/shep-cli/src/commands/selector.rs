//! One `parse_selector`, shared by `lifecycle`, `logs`, `query`, `bleats`
//! and `trigger`.

use shep_core::protocol::SelectorSpec;
use shep_core::selector::ProcessSelector;

use crate::exit::ExitCode;
use crate::output::Streams;

/// Parses `raw` client-side, so a malformed selector is a local usage error
/// rather than a round trip. The daemon re-parses it anyway.
///
/// Returns a [`ProcessSelector`], for the two callers that match it locally
/// rather than sending it: `bleats` filters its subscription and `start`
/// tries a token as a name before treating it as a target. Everything that
/// puts one on the wire wants [`parse_selector_spec`].
pub(crate) fn parse_selector(
    streams: &mut Streams<'_>,
    raw: &str,
) -> Result<ProcessSelector, ExitCode> {
    match ProcessSelector::parse(raw) {
        Ok(selector) => Ok(selector),
        Err(err) => Err(streams.fail(ExitCode::Usage, &err.to_string())),
    }
}

/// [`parse_selector`] for a caller that puts the result on the wire.
///
/// The conversion is here rather than at each verb so `SelectorSpec::from`
/// is applied one way, by everything that sends a selector.
///
/// # Errors
///
/// The [`ExitCode`] [`parse_selector`] already reported.
pub(crate) fn parse_selector_spec(
    streams: &mut Streams<'_>,
    raw: &str,
) -> Result<SelectorSpec, ExitCode> {
    parse_selector(streams, raw).map(|selector| SelectorSpec::from(&selector))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;

    #[test]
    fn a_well_formed_selector_parses_without_touching_streams() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let selector = parse_selector(&mut streams, "web").unwrap();
        assert!(matches!(selector, ProcessSelector::Name(name) if name == "web"));
        assert!(out.is_empty());
        assert!(err.is_empty());
    }

    /// `/[/` is one of only three inputs the selector grammar rejects.
    #[test]
    fn a_malformed_selector_is_a_local_usage_error() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let code = parse_selector(&mut streams, "/[/").unwrap_err();
        assert_eq!(code, ExitCode::Usage);
        assert!(out.is_empty(), "a usage error goes to stderr, not stdout");
        assert!(
            !err.is_empty(),
            "the caller must be told why the selector was rejected"
        );
    }
}
