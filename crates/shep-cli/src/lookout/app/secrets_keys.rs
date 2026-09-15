//! Moving around the secrets pane: opening it, the environment tabs, the
//! cursor and a reload.

use super::*;

impl App {
    /// The secrets pane's own match box, in force while `self.body` is
    /// `Body::Secrets`.
    ///
    /// `S` and `Escape` both close it: `S` toggles, the way `s` toggles the
    /// settings screen, and `Escape` is the uniform "leave whatever is
    /// open" key every other pane answers to. A tab move reloads, because
    /// `in_force`, the value and the byte length on every row belong to one
    /// environment. `z` toggles the selected row's namespace rather than a
    /// fold, since the flock table is not what is on screen.
    pub(super) fn on_secrets_key(&mut self, key: KeyPress) -> Effect {
        // `view::status`'s armed prompt promises "enter confirms, any other
        // key cancels", so every other key cancels here, the way the
        // dashboard's own armed confirm already does. Unlike the dashboard
        // the key is not also swallowed: a cursor move that disarms still
        // moves, which is what the pane has always done.
        //
        // `Quit` is the exception the dashboard makes too: an operator whose
        // ctrl-c does nothing reaches for `kill -9`.
        let was_armed =
            !matches!(key, KeyPress::Confirm | KeyPress::Quit) && self.disarm_secret_delete();
        match key {
            // Mirrors `on_bleats_key`'s own arm: every full-screen pane
            // answers `q`/`ctrl-c`, the one key a cancelling armed action
            // does not swallow either (`on_key`'s own comment on that).
            // The `hide` here buys nothing on screen, since nothing is drawn
            // after this: it drops the plaintext a moment before the whole
            // `App` goes.
            KeyPress::Quit => {
                self.hide_revealed();
                Effect::Quit
            }
            KeyPress::Secrets => {
                self.hide_revealed();
                self.body = Body::FlockTable;
                Effect::None
            }
            // An armed delete eats the first `Escape` rather than also
            // closing the pane: a delete waiting on a confirm is a state
            // the operator should see cleared before anything else moves.
            KeyPress::Escape => {
                if was_armed {
                    return Effect::None;
                }
                self.hide_revealed();
                self.body = Body::FlockTable;
                Effect::None
            }
            KeyPress::Reveal => self.reveal_selected(),
            KeyPress::Copy => self.copy_revealed(),
            KeyPress::TabPrev | KeyPress::TabNext => {
                self.hide_revealed();
                let Some(pane) = self.secrets_pane_mut() else {
                    return Effect::None;
                };
                let last = pane.model.environments.len().saturating_sub(1);
                pane.tab = if key == KeyPress::TabPrev {
                    pane.tab.saturating_sub(1)
                } else {
                    (pane.tab + 1).min(last)
                };
                Effect::LoadSecrets
            }
            KeyPress::Collapse => {
                if let Some(pane) = self.secrets_pane_mut()
                    && let Some(row) = pane.model.rows.get(pane.selected)
                    && let Source::Namespace(namespace) = &row.source
                {
                    let namespace = namespace.clone();
                    if !pane.collapsed.remove(&namespace) {
                        pane.collapsed.insert(namespace);
                        // Folding away the group `selected` sits in leaves
                        // no marked row on screen, and a `v` past this
                        // point would reveal a value nobody can see. Reuse
                        // `move_by`'s hidden-selection fallback rather than
                        // duplicate the boundary search here.
                        pane.move_by(0);
                    }
                }
                Effect::None
            }
            // `j`/`k`/`g`/`G` move over the pane's visible rows
            // ([`SecretsPane::move_by`] and friends), the same clamped
            // rule every other pane's cursor follows. The value on screen
            // belongs to the row it was revealed from, so every one of
            // these clears it first, the way a tab move already does.
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                self.hide_revealed();
                if let Some(pane) = self.secrets_pane_mut() {
                    match key {
                        KeyPress::SelectUp => pane.move_by(-1),
                        KeyPress::SelectDown => pane.move_by(1),
                        KeyPress::SelectFirst => pane.move_to_first(),
                        KeyPress::SelectLast => pane.move_to_last(),
                        _ => unreachable!(),
                    }
                }
                Effect::None
            }
            // `r`: re-reads the store, the provider cache and the roll,
            // the same effect a tab move already returns and for the same
            // reason: `in_force`, the value and the byte length are read
            // off disk, not derived from what is already on screen.
            KeyPress::Refresh => {
                self.hide_revealed();
                Effect::LoadSecrets
            }
            // `Enter` means two things here: armed, it confirms the delete;
            // otherwise, it opens an input. Armed wins, or a delete waiting
            // on a confirm would silently reopen the value box instead.
            KeyPress::Confirm => {
                if self
                    .secrets_pane_mut()
                    .is_some_and(|pane| pane.armed.is_some())
                {
                    self.confirm_secret_delete()
                } else {
                    self.secrets_confirm()
                }
            }
            KeyPress::SecretDelete => self.arm_secret_delete(),
            KeyPress::Help => self.open_keymap(),
            // Nothing else means anything here. Listed rather than a
            // wildcard, so a new `KeyPress` variant cannot fall silently
            // into an arm that ignores it.
            KeyPress::Action(_)
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Edit
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Bleats
            | KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            // The config pane's own three. This pane groups its rows by
            // source rather than by a field group, files nothing, and so
            // has neither a group to walk nor an edit set to take back.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => Effect::None,
        }
    }

    /// The open secrets pane, or `None` on any other screen, for
    /// `on_secrets_key`'s handlers.
    pub(super) fn secrets_pane_mut(&mut self) -> Option<&mut SecretsPane> {
        match &mut self.body {
            Body::Secrets(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }
    }

    /// Whether the secrets pane owns the body.
    ///
    /// `config_pane`, `bleats_pane` and `sheep_pane` each answer this for
    /// their own body by handing back the pane; the secrets pane had no
    /// equivalent, so a test could only press `S` and trust it landed. That
    /// is why the loop over the bodies dropped the cases it could not verify,
    /// and this predicate is what lets the secrets case join it: the loop can
    /// assert it arrived before it renders.
    #[cfg(test)]
    pub(crate) const fn secrets_pane_is_open(&self) -> bool {
        matches!(&self.body, Body::Secrets(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::secrets::SecretRow;
    use crate::lookout::secrets::Source;
    use crate::lookout::view::fixtures;

    /// A model whose only interesting field is `environments`, in the order
    /// given: enough for every test below that only cares about the tab
    /// row, not what is on it.
    fn model_with_environments(envs: &[&str]) -> SecretsModel {
        SecretsModel {
            environments: envs.iter().map(|env| (*env).to_string()).collect(),
            ..SecretsModel::default()
        }
    }

    #[test]
    fn capital_s_opens_the_pane_and_pressing_it_again_closes_it() {
        let mut app = fixtures::full_app();

        let effect = app.update(Msg::Key(KeyPress::Secrets));

        assert!(matches!(effect, Effect::LoadSecrets));
        assert!(matches!(app.body(), Body::Secrets(_)), "pane is open");

        let effect = app.update(Msg::Key(KeyPress::Secrets));

        assert!(matches!(effect, Effect::None));
        assert!(matches!(app.body(), Body::FlockTable), "pane is closed");
    }

    #[test]
    fn escape_closes_the_secrets_pane_too() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(matches!(app.body(), Body::FlockTable));
    }

    #[test]
    fn the_tab_moves_and_stops_at_both_ends() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });

        let _ = app.update(Msg::Key(KeyPress::TabPrev));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 0, "the first tab does not wrap");

        for _ in 0..6 {
            let _ = app.update(Msg::Key(KeyPress::TabNext));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "the last of three does not wrap");
    }

    #[test]
    fn a_tab_move_reloads_because_in_force_is_per_environment() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });

        let effect = app.update(Msg::Key(KeyPress::TabNext));

        assert!(
            matches!(effect, Effect::LoadSecrets),
            "every row's IN FORCE, VALUE and byte length belong to one \
             environment, so the tab cannot move without rebuilding them"
        );
    }

    /// Unsetting the last key in an environment drops it from the union
    /// `secrets::model` recomputes on every load, so a tab sitting on the
    /// rightmost entry can be left pointing past the end of a shorter
    /// list. This pins `tab` staying in range and `environment()` still
    /// naming a real entry once that happens.
    #[test]
    fn a_shrinking_environment_list_leaves_the_tab_somewhere_valid() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });
        for _ in 0..2 {
            let _ = app.update(Msg::Key(KeyPress::TabNext));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "sitting on the rightmost tab, `prod`");

        let _ = app.update(Msg::Secrets {
            environment: "prod".into(),
            result: Ok(Box::new(model_with_environments(&["dev", "staging"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.tab < pane.model.environments.len(),
            "tab {} is past the end of {:?}",
            pane.tab,
            pane.model.environments
        );
        assert_eq!(
            pane.environment(),
            Some("staging"),
            "a subsequent load asks for a real environment, not a dangling one"
        );
    }

    #[test]
    fn z_collapses_a_namespace_group_and_leaves_its_header() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        // The only row, so it is `pane.selected`'s default (0) with no
        // navigation key needed.
        let row = SecretRow {
            key: "vercel/API_TOKEN".to_string(),
            source: Source::Namespace("vercel".to_string()),
            in_force: None,
            set_in: Vec::new(),
            byte_len: None,
            readers: Vec::new(),
        };
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![row],
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(pane.collapsed.contains("vercel"), "the group is collapsed");

        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.collapsed.is_empty(),
            "and pressing it again undoes that"
        );
    }

    /// A row for [`SecretsModel::rows`] carrying no value: every field
    /// besides `key` and `source` is irrelevant to where the cursor lands.
    fn plain_row(key: &str, source: Source) -> SecretRow {
        SecretRow {
            key: key.to_string(),
            source,
            in_force: None,
            set_in: Vec::new(),
            byte_len: None,
            readers: Vec::new(),
        }
    }

    /// Three operator rows and nothing else: the `+ new key` affordance has
    /// no namespace group to sit in front of, so it is the true last thing
    /// on screen, and a plain `j`/`G` from the last real row reaches it,
    /// the fix this pane's own reachability bug needed. `k` off the
    /// affordance lands back on that real row.
    #[test]
    fn j_k_g_and_shift_g_move_the_selection_over_the_pane_s_rows() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let rows = ["FIRST", "SECOND", "THIRD"]
            .into_iter()
            .map(|key| plain_row(key, Source::Operator))
            .collect();
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows,
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.selected, 1, "j moves one row down");

        for _ in 0..5 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "clamped at the new-key affordance, one past THIRD, not wrapping"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.selected, 2,
            "k leaves the affordance upward, back onto THIRD"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.selected, 0, "g jumps to the first row");

        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "G reaches the affordance directly: nothing follows the operator group here"
        );
    }

    /// A namespace group following the one operator row: `j` from that row
    /// reaches the affordance in a single press rather than needing a second
    /// `G` nothing on screen ever hinted at, and a further `j` carries on
    /// into the namespace group beyond it.
    #[test]
    fn j_from_the_last_operator_row_reaches_the_new_key_row() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                ],
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "one `j` from the only operator row reaches the affordance"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.model.rows[pane.selected].key, "vercel/A",
            "a further `j` carries on into the namespace group past it"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "`k` off the namespace row lands back on the affordance"
        );
    }

    /// `Enter` pressed before the first `Msg::Secrets` load lands: `Secrets`
    /// opens against a placeholder model with no rows, where `selected` (0)
    /// and `model.rows.len()` (also 0) coincide, so `selected_is_new_key_row`
    /// reads true over data that never resolved. Deterministic and needs no
    /// async round trip: `KeyPress::Secrets` sets the placeholder
    /// synchronously, and this drives `Confirm` before any `Msg::Secrets`
    /// ever arrives.
    #[test]
    fn confirm_before_the_first_load_lands_does_not_open_the_new_key_input() {
        let mut app = fixtures::full_app();
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let effect = app.update(Msg::Key(KeyPress::Confirm));

        assert_eq!(effect, Effect::None);
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.typing.is_none(),
            "the placeholder model must not open the value input"
        );
    }

    /// A cursor that stepped over `model.rows` itself, one index at a
    /// time, would land inside a namespace `z` just folded away: the two
    /// hidden rows between `FIRST` and `LAST` are exactly wide enough that
    /// a blind `+1` cannot reach `LAST` by accident. Only a `move_by` that
    /// walks [`SecretsPane::visible_row_indices`] does.
    #[test]
    fn selecting_down_skips_a_namespace_z_has_collapsed() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });

        // Select a member of the group before folding it: `Collapse` acts
        // on the selected row's own source.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        // Back to `FIRST`, a row `visible_row_indices` never hid, so the
        // step below tests `move_by`'s ordinary case rather than its
        // hidden-selection fallback.
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.model.rows[pane.selected].key, "LAST",
            "the two collapsed rows in between are not a landing row"
        );
    }

    /// Collapsing the very group `selected` sits in must not leave it
    /// naming a hidden row: `view::secrets::draw` skips a row its source
    /// is collapsed, so an untouched `selected` there would mark nothing
    /// on screen at all. Asserted through [`SecretsPane::is_collapsed`],
    /// the same predicate the view calls before drawing a gutter marker,
    /// rather than a raw index, so this fails the way the view would fail
    /// rather than the way an internal counter would.
    #[test]
    fn collapsing_the_selected_group_lands_on_a_still_visible_row() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });

        // Land on a member of the group before folding it.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        let row = &pane.model.rows[pane.selected];
        assert!(
            !pane.is_collapsed(&row.source),
            "selected still names a row the fold it sat in just hid"
        );
    }

    /// `move_by` cannot land `selected` inside the visible set when that
    /// set is empty (every row here belongs to the one namespace being
    /// folded), so `selected` is left naming a hidden row. `v` has to
    /// check for itself rather than trust the invariant `move_by` cannot
    /// keep.
    #[test]
    fn reveal_over_an_empty_visible_set_does_nothing() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                environments: vec!["dev".to_string()],
                rows: vec![
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                ],
                allow_read: true,
                ..SecretsModel::default()
            })),
        });
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let effect = app.update(Msg::Key(KeyPress::Reveal));

        assert_eq!(
            effect,
            Effect::None,
            "nothing on screen names a row to read"
        );
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.visible_row_indices().is_empty(),
            "the fold hid everything"
        );
        assert_eq!(
            pane.pending_reveal, None,
            "no read was asked for, so nothing is pending one"
        );
    }

    /// `collapsed` outlives a reload; row order does not. This pins the
    /// `Msg::Secrets` clamp against exactly that gap: the row count clamp
    /// alone would leave `selected` at the same numeric index, which the
    /// fresh model happens to give to a member of the still-folded
    /// `vercel` namespace, hiding it just as surely as a `Collapse` this
    /// reload never asked for.
    #[test]
    fn reloading_cannot_leave_selected_on_a_row_a_standing_fold_hides() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });
        // Fold `vercel` while sitting on one of its rows: the `Collapse`
        // fix carries `selected` forward to `LAST`, index 3.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        // A reload whose row order gives index 3 to a `vercel` row rather
        // than to `LAST`. `collapsed` is untouched by this message, so
        // `vercel` is still folded.
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("SECOND", Source::Operator),
                    plain_row("vercel/C", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/D", Source::Namespace("vercel".to_string())),
                ],
                ..SecretsModel::default()
            })),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        let row = &pane.model.rows[pane.selected];
        assert!(
            !pane.is_collapsed(&row.source),
            "a reload left selected on a row its own standing fold hides"
        );
    }

    /// The clear itself is pinned by `a_reveal_clears_on_every_one_of_its_
    /// triggers_that_exists_yet`, which reveals a value before every one
    /// of the ten triggers including this one. Nothing here reveals
    /// anything, so this test pins only the effect `r` returns.
    #[test]
    fn r_requests_a_reload() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![plain_row("KEY", Source::Operator)],
                ..SecretsModel::default()
            })),
        });

        let effect = app.update(Msg::Key(KeyPress::Refresh));

        assert_eq!(
            effect,
            Effect::LoadSecrets,
            "`r` re-reads the store, the provider cache and the roll"
        );
    }

    #[test]
    fn a_late_model_for_a_closed_pane_does_not_reopen_it() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Key(KeyPress::Escape));

        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::default()),
        });

        assert!(
            matches!(app.body(), Body::FlockTable),
            "a reply that outlived its pane must be dropped"
        );
    }

    /// The mutation this pins: `unwrap_or(0)` alone would make the tab
    /// index always 0, and it would still pass every test above, because
    /// `dev` is index 0 in each of their environment lists. `prod` here is
    /// not, and is also not first alphabetically, so a fixture that quietly
    /// switched to always-0 or always-sorted-first both fail this one.
    #[test]
    fn the_first_load_lands_on_the_requested_environments_tab() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Secrets {
            environment: "prod".into(),
            result: Ok(Box::new(model_with_environments(&["all", "dev", "prod"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "prod is the third tab, not the first");
    }

    /// Falls back to 0 rather than panicking or leaving the previous tab in
    /// place, when the requested environment is not in the fresh model
    /// (an operator who deleted it between the request and the reply).
    #[test]
    fn a_first_load_for_a_since_removed_environment_falls_back_to_tab_zero() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Secrets {
            environment: "gone".into(),
            result: Ok(Box::new(model_with_environments(&["all", "dev"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 0);
    }
}
