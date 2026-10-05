//! One process's open questions, and the last few it settled.
//!
//! No IO and no clock: the caller stamps each question and decides when a
//! process is gone.

use std::collections::VecDeque;

use shep_core::protocol::{OpenQuestion, QuestionError, QuestionId, Settled, check_via, check_who};

/// The most questions one process may hold open at once.
///
/// The design spec's number. No benchmark stands behind it.
pub(super) const MAX_OPEN: usize = 64;

/// How many settled questions are remembered, so a late answer can be told
/// how its question closed.
///
/// The design spec's number. No benchmark stands behind it.
pub(super) const MAX_SETTLED: usize = 64;

/// What [`Questions::ask`] did with a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Asked {
    /// The id was new and the question is now open.
    Opened,
    /// An open question had this id; the new one took its place and position.
    Replaced,
    /// [`MAX_OPEN`] distinct questions are already open; nothing changed.
    Full,
}

/// Why [`Questions::answer`] did not close a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Not open; `Some` when it is among the remembered settled ones.
    NotOpen(Option<Settled>),
    /// The answer does not fit what the question takes.
    WrongForm(QuestionError),
}

/// One process's open questions, and the last few it settled.
#[derive(Debug, Default, Clone)]
pub(super) struct Questions {
    /// Open questions, in the order first asked.
    open: Vec<OpenQuestion>,
    /// Settled questions, oldest first, at most [`MAX_SETTLED`]. An id
    /// appears at most once.
    settled: VecDeque<(QuestionId, Settled)>,
}

impl Questions {
    /// Opens `question`, or replaces the open one with the same id.
    pub(super) fn ask(&mut self, question: OpenQuestion) -> Asked {
        if let Some(slot) = self
            .open
            .iter_mut()
            .find(|q| q.question == question.question)
        {
            *slot = question;
            return Asked::Replaced;
        }
        if self.open.len() >= MAX_OPEN {
            return Asked::Full;
        }
        self.open.push(question);
        Asked::Opened
    }

    /// Closes the open question `id` as withdrawn. `false` when it is not open.
    pub(super) fn withdraw(&mut self, id: &QuestionId) -> bool {
        match self.take(id.as_str()) {
            Some(closed) => {
                self.remember(closed.question, Settled::Withdrawn);
                true
            }
            None => false,
        }
    }

    /// Checks `answer` against the open question, and on success closes it
    /// as [`Settled::Answered`] and returns its id.
    ///
    /// # Errors
    ///
    /// [`Refusal::WrongForm`] when `id`, `via`, `who` or the answer break
    /// the grammar, and [`Refusal::NotOpen`] when no open question has `id`.
    /// A refusal leaves the store unchanged.
    pub(super) fn answer(
        &mut self,
        id: &str,
        answer: &str,
        note: Option<&str>,
        via: Option<&str>,
        who: Option<&str>,
    ) -> Result<QuestionId, Refusal> {
        let id = QuestionId::new(id).map_err(Refusal::WrongForm)?;
        let Some(open) = self.open.iter().find(|q| q.question == id) else {
            return Err(Refusal::NotOpen(self.recall(&id)));
        };
        if let Some(via) = via {
            check_via(via).map_err(Refusal::WrongForm)?;
        }
        if let Some(who) = who {
            check_who(who).map_err(Refusal::WrongForm)?;
        }
        open.takes.check(answer, note).map_err(Refusal::WrongForm)?;
        self.take(id.as_str());
        self.remember(
            id.clone(),
            Settled::Answered {
                via: via.map(str::to_owned),
                who: who.map(str::to_owned),
            },
        );
        Ok(id)
    }

    /// Whether a question with this id is open.
    pub(super) fn holds(&self, id: &str) -> bool {
        self.open.iter().any(|q| q.question.as_str() == id)
    }

    /// Whether how a question with this id closed is still remembered.
    pub(super) fn remembers(&self, id: &str) -> bool {
        self.settled.iter().any(|(seen, _)| seen.as_str() == id)
    }

    /// The open questions, in the order first asked.
    pub(super) fn open(&self) -> &[OpenQuestion] {
        &self.open
    }

    /// Empties the store as the process goes, returning the ids that were
    /// open. Nothing is remembered: nobody can ask about a process that is
    /// gone.
    pub(super) fn close_all(&mut self) -> Vec<QuestionId> {
        self.settled.clear();
        self.open.drain(..).map(|q| q.question).collect()
    }

    /// Removes and returns the open question `id`, keeping the order of the rest.
    fn take(&mut self, id: &str) -> Option<OpenQuestion> {
        let at = self.open.iter().position(|q| q.question.as_str() == id)?;
        Some(self.open.remove(at))
    }

    /// Records how `id` closed, evicting the oldest memory past [`MAX_SETTLED`].
    fn remember(&mut self, id: QuestionId, how: Settled) {
        self.settled.retain(|(seen, _)| *seen != id);
        if self.settled.len() >= MAX_SETTLED {
            self.settled.pop_front();
        }
        self.settled.push_back((id, how));
    }

    /// How `id` closed, when it is still remembered.
    fn recall(&self, id: &QuestionId) -> Option<Settled> {
        self.settled
            .iter()
            .find(|(seen, _)| seen == id)
            .map(|(_, how)| how.clone())
    }
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::{QuestionText, Takes};

    use super::*;

    fn q(id: &str, text: &str, takes: Takes, at: u64) -> OpenQuestion {
        OpenQuestion::new(
            QuestionId::new(id).unwrap(),
            QuestionText::new(text).unwrap(),
            takes,
            at,
        )
    }

    fn id(raw: &str) -> QuestionId {
        QuestionId::new(raw).unwrap()
    }

    fn ids(store: &Questions) -> Vec<&str> {
        store.open().iter().map(|q| q.question.as_str()).collect()
    }

    #[test]
    fn opened_questions_list_oldest_first() {
        let mut store = Questions::default();
        assert_eq!(store.ask(q("first", "a?", Takes::YesNo, 1)), Asked::Opened);
        assert_eq!(store.ask(q("second", "b?", Takes::Text, 2)), Asked::Opened);
        assert_eq!(ids(&store), ["first", "second"]);
    }

    #[test]
    fn asking_an_open_id_replaces_it_in_place() {
        let mut store = Questions::default();
        store.ask(q("alpha", "old?", Takes::YesNo, 10));
        store.ask(q("beta", "other?", Takes::YesNo, 11));
        assert_eq!(
            store.ask(q("alpha", "new?", Takes::Text, 20)),
            Asked::Replaced
        );
        assert_eq!(ids(&store), ["alpha", "beta"]);
        assert_eq!(store.open()[0], q("alpha", "new?", Takes::Text, 20));
    }

    #[test]
    fn the_question_past_the_cap_is_refused_and_the_rest_untouched() {
        let mut store = Questions::default();
        for n in 0..MAX_OPEN {
            assert_eq!(
                store.ask(q(&format!("id{n}"), "t?", Takes::YesNo, 0)),
                Asked::Opened
            );
        }
        assert_eq!(store.ask(q("overflow", "t?", Takes::YesNo, 0)), Asked::Full);
        assert_eq!(store.open().len(), MAX_OPEN);
        assert!(!store.holds("overflow"));
        assert_eq!(store.open()[0].question.as_str(), "id0");
    }

    #[test]
    fn withdrawing_an_open_id_is_remembered() {
        let mut store = Questions::default();
        store.ask(q("gone-soon", "t?", Takes::YesNo, 0));
        assert!(store.withdraw(&id("gone-soon")));
        assert!(!store.holds("gone-soon"));
        assert_eq!(
            store.answer("gone-soon", "yes", None, None, None),
            Err(Refusal::NotOpen(Some(Settled::Withdrawn)))
        );
    }

    #[test]
    fn withdrawing_an_unknown_id_returns_false() {
        let mut store = Questions::default();
        assert!(!store.withdraw(&id("never-asked")));
    }

    #[test]
    fn a_right_answer_closes_the_question() {
        let mut store = Questions::default();
        store.ask(q("deploy", "ship it?", Takes::YesNo, 0));
        let closed = store.answer("deploy", "yes", None, None, None).unwrap();
        assert_eq!(closed, id("deploy"));
        assert!(store.open().is_empty());
    }

    #[test]
    fn a_second_answer_learns_who_settled_it() {
        let mut store = Questions::default();
        store.ask(q("migrate", "run it?", Takes::YesNo, 0));
        store
            .answer("migrate", "no", None, Some("slack"), Some("ada"))
            .unwrap();
        assert_eq!(
            store.answer("migrate", "yes", None, Some("cli"), Some("bob")),
            Err(Refusal::NotOpen(Some(Settled::Answered {
                via: Some("slack".into()),
                who: Some("ada".into()),
            })))
        );
    }

    #[test]
    fn a_yes_no_question_refuses_another_word_and_stays_open() {
        let mut store = Questions::default();
        store.ask(q("proceed", "ok?", Takes::YesNo, 0));
        assert_eq!(
            store.answer("proceed", "maybe", None, None, None),
            Err(Refusal::WrongForm(QuestionError::NotYesOrNo {
                found: "maybe".into()
            }))
        );
        assert!(store.holds("proceed"));
    }

    #[test]
    fn a_text_question_refuses_a_note_and_stays_open() {
        let mut store = Questions::default();
        store.ask(q("name", "which name?", Takes::Text, 0));
        assert_eq!(
            store.answer("name", "orion", Some("careful"), None, None),
            Err(Refusal::WrongForm(QuestionError::NoteOnText))
        );
        assert!(store.holds("name"));
    }

    #[test]
    fn an_id_with_a_space_is_a_wrong_form() {
        let mut store = Questions::default();
        assert_eq!(
            store.answer("two words", "yes", None, None, None),
            Err(Refusal::WrongForm(QuestionError::IdCharacter {
                found: ' '
            }))
        );
    }

    #[test]
    fn an_empty_store_answers_not_open_with_no_memory() {
        let mut store = Questions::default();
        assert_eq!(
            store.answer("absent", "yes", None, None, None),
            Err(Refusal::NotOpen(None))
        );
    }

    #[test]
    fn the_oldest_settlement_is_forgotten_past_the_cap() {
        let mut store = Questions::default();
        for n in 0..=MAX_SETTLED {
            let name = format!("s{n}");
            store.ask(q(&name, "t?", Takes::YesNo, 0));
            assert!(store.withdraw(&id(&name)));
        }
        assert_eq!(
            store.answer("s0", "yes", None, None, None),
            Err(Refusal::NotOpen(None))
        );
        assert_eq!(
            store.answer("s1", "yes", None, None, None),
            Err(Refusal::NotOpen(Some(Settled::Withdrawn)))
        );
    }

    #[test]
    fn closing_all_returns_the_open_ids_in_order_and_empties_the_store() {
        let mut store = Questions::default();
        store.ask(q("one", "t?", Takes::YesNo, 0));
        store.ask(q("two", "t?", Takes::Text, 0));
        assert_eq!(store.close_all(), vec![id("one"), id("two")]);
        assert!(store.open().is_empty());
    }

    #[test]
    fn asking_an_open_id_on_a_full_store_replaces_it() {
        let mut store = Questions::default();
        for n in 0..MAX_OPEN {
            store.ask(q(&format!("full{n}"), "t?", Takes::YesNo, 0));
        }
        assert_eq!(
            store.ask(q("full7", "again?", Takes::Text, 9)),
            Asked::Replaced
        );
        assert_eq!(store.open().len(), MAX_OPEN);
    }

    #[test]
    fn a_bad_via_is_a_wrong_form_and_the_question_stays_open() {
        let mut store = Questions::default();
        store.ask(q("via-q", "t?", Takes::YesNo, 0));
        assert_eq!(
            store.answer("via-q", "yes", None, Some(""), None),
            Err(Refusal::WrongForm(QuestionError::Empty { field: "via" }))
        );
        assert!(store.holds("via-q"));
    }

    #[test]
    fn a_bad_who_is_a_wrong_form_and_the_question_stays_open() {
        let mut store = Questions::default();
        store.ask(q("who-q", "t?", Takes::YesNo, 0));
        assert_eq!(
            store.answer("who-q", "yes", None, None, Some("a\tb")),
            Err(Refusal::WrongForm(QuestionError::ControlCharacter {
                field: "who"
            }))
        );
        assert!(store.holds("who-q"));
    }

    #[test]
    fn closing_all_leaves_no_memory_of_settled_questions() {
        let mut store = Questions::default();
        store.ask(q("settled-first", "t?", Takes::YesNo, 0));
        store
            .answer("settled-first", "yes", None, None, None)
            .unwrap();
        store.close_all();
        assert_eq!(
            store.answer("settled-first", "yes", None, None, None),
            Err(Refusal::NotOpen(None))
        );
    }

    #[test]
    fn a_question_settled_twice_recalls_its_newest_settlement() {
        let mut store = Questions::default();
        store.ask(q("twice", "t?", Takes::YesNo, 0));
        assert!(store.withdraw(&id("twice")));
        store.ask(q("twice", "t?", Takes::YesNo, 1));
        store
            .answer("twice", "yes", None, Some("cli"), None)
            .unwrap();
        for n in 0..3 {
            let name = format!("filler{n}");
            store.ask(q(&name, "t?", Takes::YesNo, 0));
            assert!(store.withdraw(&id(&name)));
        }
        assert_eq!(
            store.answer("twice", "no", None, None, None),
            Err(Refusal::NotOpen(Some(Settled::Answered {
                via: Some("cli".into()),
                who: None,
            })))
        );
    }

    #[test]
    fn a_question_settled_twice_takes_one_slot_of_the_memory() {
        let mut store = Questions::default();
        store.ask(q("oldest", "t?", Takes::YesNo, 0));
        assert!(store.withdraw(&id("oldest")));
        for round in 0..2 {
            store.ask(q("repeat", "t?", Takes::YesNo, round));
            assert!(store.withdraw(&id("repeat")));
        }
        for n in 0..MAX_SETTLED - 2 {
            let name = format!("pad{n}");
            store.ask(q(&name, "t?", Takes::YesNo, 0));
            assert!(store.withdraw(&id(&name)));
        }
        assert_eq!(
            store.answer("oldest", "yes", None, None, None),
            Err(Refusal::NotOpen(Some(Settled::Withdrawn)))
        );
    }
}
