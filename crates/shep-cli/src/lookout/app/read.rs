//! What the renderer reads off the state, and nothing that changes it.

use super::*;

impl App {
    /// One sheep by id, whatever the filter hides: the lookup a
    /// [`RowKey::Sheep`] row's rendering needs.
    #[must_use]
    pub fn row(&self, id: u32) -> Option<&Row> {
        self.flock.get(&id)
    }

    /// The link state, as the status bar reports it.
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// The clock the view reads: the last [`Msg::Tick`]'s instant, or the
    /// one the link froze at.
    pub(crate) fn now(&self) -> Instant {
        self.now
    }

    /// How long the link has been [`Link::Lost`], as of the last
    /// [`Msg::Tick`]. Zero while it is up.
    #[must_use]
    pub fn frozen_for(&self) -> Duration {
        self.frozen_for
    }

    /// The palette every cell of data renders through.
    ///
    /// [`Palette::frozen`] once the link is [`Link::Lost`], so the table and
    /// the host strip go to one muted ink and no cell can be read as live.
    /// The chrome keeps [`Self::palette`]: the band still names the mode in
    /// bark and the status bar still paints its keys.
    #[must_use]
    pub fn data_palette(&self) -> Palette {
        if matches!(self.link, Link::Lost { .. }) {
            self.palette.frozen()
        } else {
            self.palette
        }
    }

    /// The current notice, if the last message left one.
    #[must_use]
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The resolved palette.
    #[must_use]
    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// Whether actions are permitted.
    #[must_use]
    pub fn control(&self) -> Control {
        self.control
    }

    /// The `$SHEP_HOME` this lookout watches.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The last host reading, or `None` if there has not been one.
    #[must_use]
    pub fn host(&self) -> Option<crate::lookout::source::HostSample> {
        self.host
    }

    /// Whether [`Self::host`] is `None` because the platform cannot be read,
    /// rather than because no heartbeat has fired yet. The strip says a
    /// different sentence for each: an operator shown the wrong one waits for
    /// numbers that are never coming.
    #[must_use]
    pub fn host_unsupported(&self) -> bool {
        self.host_unsupported
    }

    /// One sheep's uptime as of this dashboard's own clock, in milliseconds.
    ///
    /// A running sheep's uptime advances between polls, from the anchor its row
    /// carries. One that is not running does not: its `uptime_ms` is a fact
    /// about how long it ran. Nothing advances once the link is [`Link::Lost`].
    #[must_use]
    pub fn uptime_ms(&self, id: u32) -> Option<u64> {
        let row = self.flock.get(&id)?;
        if !matches!(row.info.status, ProcStatus::Online | ProcStatus::Starting) {
            return Some(row.info.uptime_ms);
        }
        let elapsed = self.now.saturating_duration_since(row.anchor);
        Some(row.info.uptime_ms.saturating_add(millis(elapsed)))
    }

    /// What the body between the title band and the status bar is showing.
    ///
    /// `view::draw` matches on this directly rather than calling
    /// [`Self::settings`] and [`Self::config_pane`] in sequence, which is
    /// the two-branch `if let` chain a new pane would otherwise have to
    /// insert itself into. Eight more panes are planned; each becomes a new
    /// `Body` arm instead.
    #[must_use]
    pub(crate) fn body(&self) -> &Body {
        &self.body
    }

    /// Tells every scrollable screen how tall the body is. Called by the
    /// event loop before each draw, so a screen's cursor never lands on a
    /// row that was not rendered.
    pub fn note_body_rows(&mut self, rows: u16) {
        if let Some(settings) = self.settings_mut() {
            settings.set_rows(usize::from(rows));
        }
        if let Some(pane) = self.config_pane_mut() {
            // One less than the settings screen gets: the pane spends its
            // first line on a title naming the sheep, which
            // `view::pane::pane_lines` draws before any row.
            let body = usize::from(rows.saturating_sub(1));
            pane.set_rows(body);
            if let Some(list) = pane.list_mut() {
                list.set_rows(body);
            }
        }
        if let Some(pane) = self.bleats_pane_mut() {
            // Every row `view::bleats_full::lines` spends before the first
            // feed line: the title band always, and the filter row whenever
            // a chip is set.
            //
            // Both, not just the title. A page jump larger than the body it
            // scrolls skips lines outright rather than merely overshooting:
            // with a chip showing, a jump of `rows - 1` over a body of
            // `rows - 2` leaves one line between consecutive pages that
            // `ctrl-d` alone never renders. Paging down and back up is
            // symmetric either way, which is why nothing noticed.
            let chrome = 1 + usize::from(!pane.filters().is_empty());
            pane.set_rows(usize::from(rows).saturating_sub(chrome));
        }
    }

    /// Tells the bleats pane how many columns its own area draws into, so a
    /// wrapped line's row cost can be measured against something.
    ///
    /// Its own method rather than a second parameter on
    /// [`Self::note_body_rows`]: the settings screen and the config pane
    /// never need a column count, and every existing caller of that method
    /// — production and test alike — would otherwise have to invent one it
    /// has no use for. Called by the event loop right alongside
    /// [`Self::note_body_rows`], from the same `Rect` both figures come
    /// from.
    pub fn note_body_width(&mut self, width: u16) {
        if let Some(pane) = self.bleats_pane_mut() {
            pane.set_width(width);
        }
    }

    /// Overrides the control gate a fixture built with. Every shipped fixture
    /// hard-codes [`Control::ReadOnly`].
    #[cfg(test)]
    pub(crate) fn set_control_for_tests(&mut self, control: Control) {
        self.control = control;
    }
}
