//! The reducer itself: a `Msg` in, an `Effect` out, and no I/O in between.
//!
//! Three children, one per family of messages: [`link`] takes what the link,
//! the clock and the reads report, [`replies`] takes the answers to a sent
//! action, and [`panes`] takes what a pane came back with.

mod link;
mod panes;
mod replies;

use super::*;

impl App {
    /// A dashboard with an empty flock, a live link, and no notice.
    #[must_use]
    pub fn new(palette: Palette, control: Control, home: String, now: Instant) -> Self {
        Self {
            flock: BTreeMap::new(),
            selected: None,
            filter: TextBox::new(),
            mode: InputMode::Normal,
            next_write_ticket: 0,
            sheep_dogs_ask: 0,
            link: Link::Live,
            notice: None,
            palette,
            control,
            home,
            remote_host: None,
            now,
            froze_at: None,
            frozen_for: Duration::ZERO,
            host: None,
            host_unsupported: false,
            feed: crate::lookout::tail::Tail::default(),
            log_size: None,
            lambs: None,
            action: None,
            body: Body::FlockTable,
            config_target: None,
            config_for: None,
            dog_target: None,
            closing: false,
            config_read_in_flight: false,
            close_dialog: None,
            held: None,
            style: (StyleLevel::Full, StyleSource::Default),
            cpu_history: HashMap::new(),
            flock_cpu: VecDeque::new(),
            rss_history: HashMap::new(),
            cpu_last: HashMap::new(),
            grouping: Grouping::Flat,
            collapsed_folds: HashSet::new(),
            keymap_open: false,
        }
    }

    /// Applies one message and reports what the caller must do next.
    pub fn update(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Snapshot { rows, at } => self.on_flock_snapshot(rows, at),
            Msg::Event(event) => self.on_event(event),
            Msg::BusLagged { count } => self.on_bus_lagged(count),
            Msg::Retrying { attempt } => self.on_retrying(attempt),
            Msg::Relinked => self.on_relinked(),
            Msg::Frozen { at_local, why } => self.on_frozen(at_local, why),
            Msg::Tick { now } => self.on_tick(now),
            Msg::Resize => Effect::None,
            Msg::Key(key) => self.on_key(key),
            Msg::Paste(text) => self.on_paste(text.as_str()),
            Msg::Host { sample } => self.on_host(sample),
            Msg::LogSize { id, total_bytes } => self.on_log_size(id, total_bytes),
            Msg::Bleats { tail } => self.on_bleats(tail),
            Msg::Replied { sent, result } => self.on_replied(sent, result),
            Msg::Unsent { sent } => self.on_unsent(sent),
            Msg::BatchSent { unsent } => self.on_batch_sent(unsent),
            Msg::Settings { result } => self.on_settings(result),
            Msg::SettingWritten {
                edit,
                ticket,
                result,
            } => self.on_setting_written(edit, ticket, result),
            Msg::DogPane {
                name,
                adopted_path,
                result,
            } => self.on_dog_pane_answer(name, adopted_path, result),
            Msg::SheepDogNames { sheep, ask, dogs } => {
                self.on_sheep_dog_names_answer(sheep, ask, dogs)
            }
            Msg::SheepDog { sheep, entry } => self.on_sheep_dog_answer(&sheep, entry),
            Msg::DogWritten {
                edit,
                ticket,
                result,
            } => self.on_dog_written(edit, ticket, result),
            Msg::Secrets {
                environment,
                result,
            } => self.on_secrets(environment, result),
            Msg::Revealed {
                key,
                environment,
                value,
            } => self.on_revealed_answer(key, environment, value),
            Msg::SecretWritten { result } => self.on_secret_written(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;

    #[test]
    fn every_action_key_refuses_while_the_gate_is_closed() {
        for verb in [ActionVerb::Stop, ActionVerb::Restart, ActionVerb::Reload] {
            let (mut app, _t0) = started();
            app.update(Msg::Key(KeyPress::Action(verb)));
            assert!(
                app.action().is_none(),
                "{verb:?} armed behind a closed gate"
            );
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some("read-only: from --read-only or lookout.allow_control"),
                "{verb:?}"
            );
        }
    }

    #[test]
    fn the_next_keypress_clears_the_notice() {
        let (mut app, _) = started();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.notice().is_some());
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(app.notice().is_none());
    }
}
