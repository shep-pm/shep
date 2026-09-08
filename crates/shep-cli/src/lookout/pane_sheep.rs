//! The sheep pane: one sheep's histories, config, env keys and feed.
//!
//! The charts read [`App`](super::app::App)'s own ring buffers rather than
//! anything here, so history keeps filling while the pane is shut and a
//! pane opened on a sheep that has been running all along starts full.

use shep_core::protocol::SheepConfigView;

use super::app::RowKey;
use super::pane_bleats::BleatsPane;

/// One sheep, given the whole screen.
///
/// `Debug` is derived (IR-41): [`SheepConfigView`]'s own `Debug` is already
/// redacted, since it carries a key set; [`BleatsPane`]'s carries a row key
/// and a filter set, neither of which is a value the pane withholds; and a
/// [`RowKey`] is a bare integer.
///
/// No scroll state of its own yet: rows 2 to 46 are still blank, so there
/// is nothing to scroll into view. Tasks 8 through 10 add it alongside the
/// rows it scrolls.
#[derive(Debug)]
pub struct SheepPane {
    sheep: RowKey,
    /// `None` until `Request::SheepConfig` answers. The left column draws
    /// its own waiting line rather than an empty group list, which would
    /// read as a sheep with no config at all.
    config: Option<SheepConfigView>,
    /// The full feed, filtered to this sheep. Read by a later task; kept
    /// current from the moment the pane opens (and re-pinned on
    /// [`Self::set_sheep`]) so the feed is not starting cold the first time
    /// something draws it.
    feed: BleatsPane,
}

impl SheepPane {
    /// Opens on `sheep`, with no config yet and the feed following the tail.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        let feed = BleatsPane::new(sheep.clone());
        Self {
            sheep,
            config: None,
            feed,
        }
    }

    /// The sheep this pane describes right now. Changes under `J`/`K`
    /// without the pane closing.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }

    /// The sheep's config, or `None` while the read is still in flight, or
    /// after a refusal that left nothing to show.
    #[must_use]
    pub fn config(&self) -> Option<&SheepConfigView> {
        self.config.as_ref()
    }

    /// Adopts a `Request::SheepConfig` reply.
    pub fn adopt_config(&mut self, view: SheepConfigView) {
        self.config = Some(view);
    }

    /// Points the pane at a different sheep, the way `J`/`K` do.
    ///
    /// The old sheep's config is dropped rather than kept on screen under
    /// the new sheep's name, and the feed is re-pinned to match: both would
    /// otherwise describe a sheep the pane no longer names.
    pub(crate) fn set_sheep(&mut self, sheep: RowKey) {
        self.feed = BleatsPane::new(sheep.clone());
        self.sheep = sheep;
        self.config = None;
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::AppConfig;

    use super::*;

    /// [`SheepConfigView`]'s own `Debug` already withholds `args` and `cwd`;
    /// this pins that [`SheepPane`]'s derived one does not undo that by
    /// printing the feed or the viewport instead. Matches the shape
    /// `ConfigPane`'s own exact-string test uses (`pane.rs:786`).
    #[test]
    fn the_panes_debug_names_no_field_value() {
        let mut pane = SheepPane::new(RowKey::Sheep(9));
        let config = AppConfig {
            name: "web".into(),
            cwd: Some("/home/ada/secret-project".into()),
            args: vec!["--token".into(), "hunter2".into()],
            ..AppConfig::default()
        };
        pane.adopt_config(SheepConfigView::new(config, Vec::new(), Vec::new()));
        assert_eq!(
            format!("{pane:?}"),
            r#"SheepPane { sheep: Sheep(9), config: Some(SheepConfigView { name: "web", env_keys: 0, env_secrets: 0, overridden: 0, pending: 0 }), feed: BleatsPane { sheep: Sheep(9), filters: Filters { stream: None, min_level: None, matcher: None, order: [] }, match_snapshot: None, scroll_offset: 0, following: true, body_rows: 0, width: 0, wrap: false } }"#
        );
    }
}
