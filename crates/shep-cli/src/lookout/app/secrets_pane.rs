//! The secrets pane's own state, and the newtypes that keep a secret out of a
//! `Debug` string.

use super::*;

/// `Enter`'s refusal on a provider row: pushed by a dog, so nothing here is
/// this operator's to set.
pub(super) const PROVIDER_ROW_REFUSAL: &str = "read-only: pushed by a dog, not set here";

impl App {
    /// Refuse `row` when a provider pushed it: raise the grave
    /// [`PROVIDER_ROW_REFUSAL`] notice, and report whether it did.
    ///
    /// The check [`Self::secrets_confirm`] and [`Self::arm_secret_delete`]
    /// share, so the sentence is raised from one place.
    pub(super) fn refuse_provider_row(&mut self, row: &SecretRow) -> bool {
        if matches!(row.source, Source::Namespace(_)) {
            self.notice = Some(Notice::grave(PROVIDER_ROW_REFUSAL.to_string()));
            return true;
        }
        false
    }
}

/// `D`'s refusal on a row whose value comes from the `all` slot while a
/// named environment's tab is open.
///
/// Refuses rather than retargets, both ways round. Unsetting the tab's own
/// environment would remove nothing and report success, and unsetting `all`
/// from here would change every environment at once, which is not what a row
/// reading `all` under `IN FORCE` invites anyone to expect.
pub(super) const ALL_SLOT_REFUSAL: &str = "this value comes from the `all` slot: removing it \
     affects every environment, so switch to the `all` tab to mean it";

/// [`Msg::SecretWritten`]'s answer to an unset that found no slot, whether
/// the store changed under the pane or the row never resolved in this tab.
pub(super) const NOTHING_REMOVED: &str =
    "nothing to remove: this key holds no value in this environment";

/// Whether deleting `row` from the tab named `environment` would have to
/// remove the `all` slot, which only the `all` tab may do.
pub(super) fn deletes_the_all_slot(row: &SecretRow, environment: &str) -> bool {
    row.in_force.as_deref() == Some(ALL_ENVIRONMENTS) && environment != ALL_ENVIRONMENTS
}

/// `y`'s notice on a successful copy.
///
/// Says the value was sent, never that it arrived: OSC 52 is write-only, the
/// terminal never replies, and many terminals refuse the sequence by
/// default, so no caller can confirm anything landed. Names the system
/// clipboard's own reach in the same breath, since sending a value there is
/// handing it to every process on the desktop, not just the terminal.
pub(super) const COPY_SENT_NOTICE: &str = "sent to the terminal's clipboard over OSC 52 \u{b7} readable there by every process on the desktop";

/// How long a revealed value stays on screen.
///
/// The pane prints this number, so the two cannot drift.
pub(crate) const REVEAL_HOLDS: Duration = Duration::from_secs(10);

/// The secrets pane's state.
///
/// `Debug` is manual (IR-41): [`Self::reveal`] and [`Self::typing`] carry
/// an operator's plaintext.
pub(crate) struct SecretsPane {
    /// Everything drawn, rebuilt by every load.
    pub model: Box<SecretsModel>,
    /// Which environment tab is showing, an index into
    /// [`SecretsModel::environments`].
    pub tab: usize,
    /// Which row the panels describe, an index into
    /// [`SecretsModel::rows`].
    pub selected: usize,
    /// Namespace groups `z` has collapsed, mirroring the flock table's own
    /// collapsed-fold set for this pane's rows.
    pub collapsed: HashSet<String>,
    /// The value on screen and when it leaves, or `None`.
    pub reveal: Option<Reveal>,
    /// The key an [`Effect::RevealSecret`] is reading for, or `None`. The
    /// answer is drawn only while this still names its key, so every
    /// trigger that clears a reveal also drops one in flight.
    pub pending_reveal: Option<String>,
    /// The armed delete, or `None`. While this is set, `Enter` confirms the
    /// delete rather than opening the value input.
    pub armed: Option<ArmedDelete>,
    /// The open key/value editor, or `None`. The same [`KeyValues`] a sheep's
    /// env rows open, so `Tab`, `Shift-Tab`, `Enter` and `Esc` mean the same
    /// thing on both panes.
    pub typing: Option<KeyValues>,
}

/// A delete armed on the secrets pane: the key and when it armed, one value
/// rather than two so a caller cannot set one without the other, the same
/// pairing [`Pending::Armed`] keeps for the settings pane.
#[derive(Debug, Clone)]
pub(crate) struct ArmedDelete {
    /// The key waiting on `Enter` to confirm the delete.
    pub key: String,
    /// When it armed, for the expiry the tick runs.
    pub at: Instant,
}

impl SecretsPane {
    /// Takes the value off the screen, and abandons a read still in flight
    /// so its answer cannot put one back.
    ///
    /// One method rather than an assignment at each trigger: a trigger added
    /// later has one thing to call, and the ones that exist cannot drift
    /// apart.
    pub(crate) fn hide(&mut self) {
        self.reveal = None;
        self.pending_reveal = None;
    }

    /// The environment tab showing, or `None` before the first load.
    pub(crate) fn environment(&self) -> Option<&str> {
        self.model.environments.get(self.tab).map(String::as_str)
    }

    /// Whether `source`'s rows are folded away: only a provider namespace
    /// can be, mirroring `on_secrets_key`'s `Collapse` arm.
    ///
    /// `pub(crate)` so `view::secrets::draw` reads the same answer this
    /// pane's own cursor does, rather than a second copy of the match.
    pub(crate) fn is_collapsed(&self, source: &Source) -> bool {
        match source {
            Source::Operator => false,
            Source::Namespace(namespace) => self.collapsed.contains(namespace),
        }
    }

    /// Every index into `model.rows` this pane currently draws: a
    /// collapsed namespace's members contribute none, the same rows
    /// `view::secrets::draw` skips on screen. Never the `+ new key` row,
    /// which is not a `model.rows` index: [`App::reveal_selected`] reads
    /// this to decide whether anything real is even on screen, so it stays
    /// real-rows-only rather than growing the affordance into it.
    pub(super) fn visible_row_indices(&self) -> Vec<usize> {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !self.is_collapsed(&row.source))
            .map(|(index, _)| index)
            .collect()
    }

    /// The selected row as an owned [`SecretRow`], but only when the pane
    /// currently draws it: `None` for the `+ new key` affordance, for a row
    /// a standing fold hides, and for a `selected` past the rows.
    ///
    /// Owned rather than borrowed, so a caller can hold the row while it
    /// reaches back into the pane.
    pub(super) fn selected_visible_row(&self) -> Option<SecretRow> {
        if !self.visible_row_indices().contains(&self.selected) {
            return None;
        }
        self.model.rows.get(self.selected).cloned()
    }

    /// Whether unsetting `key` from the tab named `environment` would have to
    /// reach the `all` slot. Looked up by key rather than by the cursor,
    /// because a rename's removal half names the row the editor was opened
    /// under while the cursor is inside the editor.
    ///
    /// The fact [`deletes_the_all_slot`] already gives `D`, reached here so the
    /// delete and a rename's removal refuse on one rule rather than two.
    #[must_use]
    pub(crate) fn unset_reaches_the_all_slot(&self, key: &str, environment: &str) -> bool {
        self.model
            .rows
            .iter()
            .find(|row| row.key == key)
            .is_some_and(|row| deletes_the_all_slot(row, environment))
    }

    /// Whether `selected` names the `+ new key` row rather than a real one:
    /// one past every index [`Self::visible_row_indices`] can ever hand
    /// back.
    pub(crate) fn selected_is_new_key_row(&self) -> bool {
        self.selected == self.model.rows.len()
    }

    /// The `model.rows` index the `+ new key` affordance sits after: the
    /// highest-index operator row, or `None` when the store holds none, in
    /// which case the affordance is first on screen instead.
    ///
    /// [`Self::screen_slots`] and [`view::secrets::draw`](crate::lookout::view::secrets::draw)
    /// both derive their placement from this rather than each running its own
    /// scan, so the two cannot disagree.
    pub(crate) fn new_key_anchor(&self) -> Option<usize> {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.source == Source::Operator)
            .map(|(index, _)| index)
            .max()
    }

    /// Every screen slot `j`/`k`/`g`/`G` can land the cursor on, in the order
    /// [`view::secrets::draw`](crate::lookout::view::secrets::draw) draws
    /// them: `None` is the `+ new key` affordance, placed right after
    /// [`Self::new_key_anchor`], or first on screen when there is none.
    ///
    /// Real rows never move relative to each other here, so a row hidden by a
    /// fold is simply absent, as in [`Self::visible_row_indices`].
    fn screen_slots(&self) -> Vec<Option<usize>> {
        let anchor = self.new_key_anchor();
        let visible = self.visible_row_indices();
        let insert_at = match anchor {
            Some(anchor) => visible
                .iter()
                .position(|&index| index > anchor)
                .unwrap_or(visible.len()),
            None => 0,
        };
        let mut slots: Vec<Option<usize>> = visible.into_iter().map(Some).collect();
        slots.insert(insert_at, None);
        slots
    }

    /// Moves `selected` by `delta` positions over [`Self::screen_slots`],
    /// clamped rather than wrapping: the same rule the flock table and every
    /// other pane's cursor follows.
    ///
    /// A selection `z` just folded away is not itself in
    /// [`Self::screen_slots`]: this lands on the nearest surviving real row in
    /// the direction `delta` points, over [`Self::visible_row_indices`] alone.
    pub(crate) fn move_by(&mut self, delta: isize) {
        let slots = self.screen_slots();
        if slots.is_empty() {
            return;
        }
        let current = (!self.selected_is_new_key_row()).then_some(self.selected);
        if let Some(position) = slots.iter().position(|&slot| slot == current) {
            let next = position.saturating_add_signed(delta).min(slots.len() - 1);
            self.selected = slots[next].unwrap_or(self.model.rows.len());
            return;
        }
        // The selection itself just went hidden: `z` folded its own group
        // away, or a clamp landed on a row a standing fold hides. Land on
        // the nearest visible neighbour in the direction requested, rather
        // than guessing a position inside a list it is not part of.
        let visible = self.visible_row_indices();
        if visible.is_empty() {
            return;
        }
        let boundary = visible.partition_point(|&index| index < self.selected);
        self.selected = if delta < 0 {
            visible[boundary.saturating_sub(1).min(visible.len() - 1)]
        } else {
            visible[boundary.min(visible.len() - 1)]
        };
    }

    /// Jumps `selected` to the first screen slot, `g`'s effect: a real row
    /// unless the operator store holds none, in which case the affordance
    /// itself is first on screen.
    pub(crate) fn move_to_first(&mut self) {
        if let Some(&slot) = self.screen_slots().first() {
            self.selected = slot.unwrap_or(self.model.rows.len());
        }
    }

    /// Jumps `selected` to the last screen slot, `G`'s effect: the
    /// affordance itself when no namespace group follows the operator rows,
    /// otherwise the last namespace row, exactly what is last on screen.
    pub(crate) fn move_to_last(&mut self) {
        if let Some(&slot) = self.screen_slots().last() {
            self.selected = slot.unwrap_or(self.model.rows.len());
        }
    }
}

/// Redacted (IR-41): `reveal` and `typing` hold a plaintext value.
impl fmt::Debug for SecretsPane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsPane")
            .field("rows", &self.model.rows.len())
            .field("tab", &self.tab)
            .field("selected", &self.selected)
            .field("collapsed", &self.collapsed.len())
            .field("revealing", &self.reveal.is_some())
            .field("pending_reveal", &self.pending_reveal)
            .field("armed", &self.armed.as_ref().map(|a| &a.key))
            .field("typing", &self.typing.is_some())
            .finish()
    }
}

/// A value on screen, and the instant it leaves.
pub(crate) struct Reveal {
    /// The key it belongs to.
    pub key: String,
    /// The plaintext.
    pub value: String,
    /// When it clears, [`REVEAL_HOLDS`] after the keypress.
    pub until: Instant,
}

/// Redacted (IR-41): `value` is the secret.
impl fmt::Debug for Reveal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reveal")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .finish_non_exhaustive()
    }
}

/// A plaintext value on its way from the store to the pane, in
/// [`Msg::Revealed`].
///
/// A type of its own rather than a `String` field: [`Msg`] derives `Debug`,
/// so the redaction has to travel with the value (IR-41).
#[derive(Clone)]
pub struct RevealedValue(pub(crate) String);

/// Redacted (IR-41): the field is the secret.
impl fmt::Debug for RevealedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RevealedValue(<{} bytes>)", self.0.len())
    }
}

/// A plaintext value on its way to the terminal's clipboard, in
/// [`Effect::CopyToClipboard`].
///
/// A type of its own rather than a bare `String`: [`Effect`] derives
/// `Debug`, so the redaction has to travel with the value, the same reason
/// [`RevealedValue`] exists (IR-41).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClipboardValue(pub(crate) String);

/// Redacted (IR-41): the field is the secret.
impl fmt::Debug for ClipboardValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClipboardValue(<{} bytes>)", self.0.len())
    }
}

/// One change to the operator's store, carried by [`Effect::WriteSecret`].
///
/// `Debug` is manual (IR-41): `value` is the operator's plaintext.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SecretEdit {
    /// The key.
    pub key: String,
    /// Which environment's slot moves.
    pub environment: String,
    /// The new value, or `None` to remove the slot. Task 8's door: nothing
    /// in this task builds `None`.
    pub value: Option<String>,
}

/// Redacted (IR-41), matching `SecretCommand::Set`: a length, never a
/// value.
impl fmt::Debug for SecretEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value.as_ref().map_or_else(
            || "None".to_string(),
            |v| format!("Some(<{} bytes>)", v.len()),
        );
        f.debug_struct("SecretEdit")
            .field("key", &self.key)
            .field("environment", &self.environment)
            .field("value", &format_args!("{value}"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane's editor with a value halfway typed into it, which is the only
    /// way one gets here: no pane is ever told a stored value, so the text a
    /// `Debug` must not print is always text an operator typed.
    fn typing_editor() -> KeyValues {
        let mut editor = KeyValues::under(Grammar::Secret, "K");
        for typed in "hunter2".chars() {
            editor.handle(KeyPress::TextChar(typed));
        }
        editor
    }

    /// Exact-string, so restoring a derived `Debug` fails this test rather
    /// than silently reopening the leak (IR-41).
    #[test]
    fn the_pane_debug_never_prints_a_revealed_value() {
        let pane = SecretsPane {
            model: Box::default(),
            tab: 0,
            selected: 0,
            collapsed: HashSet::new(),
            reveal: Some(Reveal {
                key: "K".into(),
                value: "hunter2".into(),
                until: Instant::now(),
            }),
            pending_reveal: None,
            armed: None,
            typing: Some(typing_editor()),
        };

        let printed = format!("{pane:?}");

        assert_eq!(
            printed,
            "SecretsPane { rows: 0, tab: 0, selected: 0, collapsed: 0, \
             revealing: true, pending_reveal: None, armed: None, typing: true }"
        );
        assert!(!format!("{:?}", pane.reveal).contains("hunter2"));
        assert!(!format!("{:?}", pane.typing).contains("hunter2"));
    }

    #[test]
    fn a_secret_edit_debug_prints_a_length_and_never_the_value() {
        let edit = SecretEdit {
            key: "DB_PASSWORD".into(),
            environment: "production".into(),
            value: Some("hunter2".into()),
        };

        assert_eq!(
            format!("{edit:?}"),
            "SecretEdit { key: \"DB_PASSWORD\", environment: \"production\", \
             value: Some(<7 bytes>) }"
        );
    }

    /// Exact-string, so restoring a derived `Debug` fails this test rather
    /// than silently reopening the leak (IR-41). [`Msg`]'s own `Debug` is
    /// derived, so the redaction has to live in the field's type.
    #[test]
    fn the_msg_debug_never_prints_a_revealed_value() {
        let landed = Msg::Revealed {
            key: "K".to_string(),
            environment: "production".to_string(),
            value: Some(RevealedValue("hunter2".to_string())),
        };

        let printed = format!("{landed:?}");

        assert_eq!(
            printed,
            "Revealed { key: \"K\", environment: \"production\", \
             value: Some(RevealedValue(<7 bytes>)) }"
        );
    }

    /// Exact-string, [`the_msg_debug_never_prints_a_revealed_value`]'s own
    /// reason: [`Effect`]'s own `Debug` is derived, so the redaction has to
    /// live in [`ClipboardValue`] itself (IR-41).
    #[test]
    fn the_effect_debug_never_prints_a_copied_value() {
        let effect = Effect::CopyToClipboard(ClipboardValue("hunter2".to_string()));

        assert_eq!(
            format!("{effect:?}"),
            "CopyToClipboard(ClipboardValue(<7 bytes>))"
        );
    }
}
