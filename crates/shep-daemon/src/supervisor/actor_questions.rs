//! A sheep's questions: asked and withdrawn on its channel, answered by an
//! operator, and closed as `gone` when the process that asked them goes.
//!
//! Nothing here awaits. An answer goes onto the channel with `try_send`, so
//! a child that has stopped reading fd 3 cannot park the actor.

use shep_core::protocol::{Answer, Settled};

use super::questions::{Asked, MAX_OPEN, Refusal};
use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Opens or replaces one question on `id`, while `root_pid` is still the
    /// process running there.
    pub(super) fn handle_ask(&mut self, id: u32, root_pid: u32, question: OpenQuestion) {
        let Some(slot) = self.asking_slot(id, root_pid) else {
            return;
        };
        let asked = question.question.clone();
        if slot.questions.ask(question) == Asked::Full {
            tracing::warn!(
                sheep = %slot.entry.spec.config().name,
                question = %asked,
                "{MAX_OPEN} questions are already open; this one was dropped"
            );
        }
    }

    /// Closes one question as withdrawn, while `root_pid` is still the
    /// process running on `id`. An id that is not open is ignored.
    pub(super) fn handle_withdraw(&mut self, id: u32, root_pid: u32, question: QuestionId) {
        let Some(slot) = self.asking_slot(id, root_pid) else {
            return;
        };
        if slot.questions.withdraw(&question) {
            let name = slot.entry.spec.config().name.clone();
            self.publish_settled(id, name, question, Settled::Withdrawn);
        }
    }

    /// Delivers an operator's answer to the one sheep holding `question`,
    /// and answers with that sheep's id and name.
    ///
    /// Only a name can match several instances holding one id, so that is
    /// refused rather than answered on whichever is found first. The answer
    /// is checked on a copy of the store, which replaces the original only
    /// once the channel took the message.
    ///
    /// # Errors
    ///
    /// [`SupervisorError::NotFound`] when nothing matches,
    /// [`SupervisorError::QuestionNotOpen`] when nothing matched can take
    /// the answer, and [`SupervisorError::InvalidAnswer`] for a refusal the
    /// caller can ask differently.
    pub(super) fn handle_answer(
        &mut self,
        selector: &ProcessSelector,
        question: &str,
        answer: String,
        note: Option<String>,
        via: Option<String>,
        who: Option<String>,
    ) -> Result<(u32, String), SupervisorError> {
        if !selector.is_exact() {
            return Err(SupervisorError::InvalidAnswer(
                "an answer goes to one sheep: name it by id, name or name:slot, \
                 not all, a pattern or a fold"
                    .to_string(),
            ));
        }
        let matched = self.matching_ids(selector);
        let Some(&first) = matched.first() else {
            return Err(SupervisorError::NotFound);
        };
        let channelled: Vec<u32> = matched
            .iter()
            .copied()
            .filter(|id| self.sheep[id].open_channel().is_some())
            .collect();
        let Some(&first_channelled) = channelled.first() else {
            let name = &self.sheep[&first].entry.spec.config().name;
            let why = if matched.iter().all(|id| self.sheep[id].entry.pid.is_none()) {
                "is not running, so it has no open questions"
            } else {
                "has no open shepherd channel, so its questions cannot be answered"
            };
            return Err(SupervisorError::QuestionNotOpen(format!("{name} {why}")));
        };
        let holders: Vec<u32> = channelled
            .iter()
            .copied()
            .filter(|id| self.sheep[id].questions.holds(question))
            .collect();
        let target = match holders.as_slice() {
            // Not held anywhere: the store refuses without changing, from
            // the instance that remembers how the question closed if one does.
            [] => channelled
                .iter()
                .copied()
                .find(|id| self.sheep[id].questions.remembers(question))
                .unwrap_or(first_channelled),
            [one] => *one,
            many => {
                let ids: Vec<String> = many.iter().map(u32::to_string).collect();
                return Err(SupervisorError::InvalidAnswer(format!(
                    "question {question} is open on more than one sheep (ids {}); answer by id",
                    ids.join(", ")
                )));
            }
        };

        let slot = self
            .sheep
            .get_mut(&target)
            .expect("handle_answer: the id was read off this map a moment ago");
        let name = slot.entry.spec.config().name.clone();
        let mut questions = slot.questions.clone();
        let closed = questions
            .answer(
                question,
                &answer,
                note.as_deref(),
                via.as_deref(),
                who.as_deref(),
            )
            .map_err(|refusal| refused(&name, question, refusal))?;
        let mut message = Answer::new(closed.clone(), answer);
        message.note = note;
        message.via = via.clone();
        message.who = who.clone();
        let unsent = match slot.open_channel() {
            Some(to_child) => match to_child.try_send(ShepherdMessage::Answer(message)) {
                Ok(()) => None,
                Err(mpsc::error::TrySendError::Full(_)) => Some(format!(
                    "{name} is not reading its shepherd channel, so the answer was not \
                     delivered; question {question} is still open"
                )),
                // The process is going, and its exit closes the question as `gone`.
                Err(mpsc::error::TrySendError::Closed(_)) => Some(format!(
                    "{name} is exiting, so question {question} was not delivered"
                )),
            },
            None => Some(format!(
                "{name} is exiting, so question {question} was not delivered"
            )),
        };
        if let Some(why) = unsent {
            return Err(SupervisorError::QuestionNotOpen(why));
        }
        slot.questions = questions;
        self.publish_settled(target, name.clone(), closed, Settled::Answered { via, who });
        Ok((target, name))
    }

    /// Closes every question `id`'s process has open, publishing `gone` for
    /// each. Called wherever [`SheepSlot::to_child`] is cleared.
    pub(super) fn close_questions(&mut self, id: u32) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        let closed = slot.questions.close_all();
        if closed.is_empty() {
            return;
        }
        let name = slot.entry.spec.config().name.clone();
        for question in closed {
            self.publish_settled(id, name.clone(), question, Settled::Gone);
        }
    }

    /// `id`'s slot, unless it is gone or a later process has replaced
    /// `root_pid` there.
    fn asking_slot(&mut self, id: u32, root_pid: u32) -> Option<&mut SheepSlot> {
        self.sheep
            .get_mut(&id)
            .filter(|slot| slot.entry.pid == Some(root_pid))
    }

    /// Publishes one `question.settled`. A bus with no subscriber is not an
    /// error, as for [`Self::emit`].
    fn publish_settled(&self, id: u32, name: String, question: QuestionId, settled: Settled) {
        let _ = self
            .events
            .send(SharedEvent::new(BusEvent::QuestionSettled {
                id,
                name,
                question,
                settled,
                at_ms: crate::now_ms(),
            }));
    }
}

/// The error an answer the store refused becomes, naming the sheep.
fn refused(name: &str, question: &str, refusal: Refusal) -> SupervisorError {
    let how = match refusal {
        Refusal::WrongForm(err) => return SupervisorError::InvalidAnswer(err.to_string()),
        Refusal::NotOpen(None) => format!("{name} has no open question {question}"),
        Refusal::NotOpen(Some(Settled::Answered { via, who })) => {
            let via = via.map(|via| format!(" via {via}")).unwrap_or_default();
            let who = who.map(|who| format!(" by {who}")).unwrap_or_default();
            format!("question {question} on {name} was already answered{via}{who}")
        }
        Refusal::NotOpen(Some(Settled::Withdrawn)) => {
            format!("{name} withdrew question {question}")
        }
        Refusal::NotOpen(Some(_)) => format!("question {question} on {name} is closed"),
    };
    SupervisorError::QuestionNotOpen(how)
}
