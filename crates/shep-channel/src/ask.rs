//! Asking the operator a question over the channel.

use crate::{Answer, ChannelError, ChildMessage, QuestionId, QuestionText, Shepherd, Takes};

impl Shepherd {
    /// Puts a question to the operator.
    ///
    /// Blocks only until the message is queued, like [`Shepherd::ready`]. The
    /// answer, if one comes, reaches the handler set with
    /// [`Shepherd::on_answer`]. A question is answered at most once.
    ///
    /// ```
    /// use shep_channel::{QuestionId, QuestionText, Takes};
    ///
    /// let shepherd = shep_channel::serve();
    /// shepherd.on_answer(|answer| println!("{} said {}", answer.question, answer.answer));
    /// shepherd
    ///     .ask(
    ///         QuestionId::new("koji-3").unwrap(),
    ///         QuestionText::new("Merge #12 into main?").unwrap(),
    ///         Takes::YesNo,
    ///     )
    ///     .unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] when the shepherd has gone away. Without a
    /// channel this always returns `Ok(())`.
    pub fn ask(
        &self,
        question: QuestionId,
        text: QuestionText,
        takes: Takes,
    ) -> Result<(), ChannelError> {
        self.push_blocking(ChildMessage::Ask {
            question,
            text,
            takes,
        })
    }

    /// Takes back a question this app no longer needs answered.
    ///
    /// Blocks only until the message is queued, like [`Shepherd::ready`].
    ///
    /// ```
    /// use shep_channel::QuestionId;
    ///
    /// let shepherd = shep_channel::serve();
    /// shepherd.withdraw(QuestionId::new("koji-3").unwrap()).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] when the shepherd has gone away. Without a
    /// channel this always returns `Ok(())`.
    pub fn withdraw(&self, question: QuestionId) -> Result<(), ChannelError> {
        self.push_blocking(ChildMessage::Withdraw { question })
    }

    /// Registers the handler run when the operator answers a question,
    /// replacing any prior one.
    ///
    /// Without one, an answer warns on stderr and is lost. The reader
    /// thread runs the handler, so a slow one delays the next message.
    pub fn on_answer<H>(&self, handler: H) -> &Self
    where
        H: Fn(&Answer) + Send + Sync + 'static,
    {
        self.register_answer(Box::new(handler));
        self
    }
}
