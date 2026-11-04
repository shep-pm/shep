//! The modal layer: a question or a long output that owns the keyboard.
//!
//! One modal shows at a time and a later one waits behind it, so a chain of
//! dependencies can ask one dog after another. The owner picks a `tag` and
//! reads it back from [`Effect::ModalAnswered`]; nothing here knows what a
//! yes does.

use super::*;
use crate::commands::dogs::Prompt;
use crate::terminal_safe::{sanitise, sanitise_multiline};

/// The most bytes of an output modal's text that are shown.
pub(in crate::lookout) const OUTPUT_CAP: usize = 4096;

/// What ends a text cut at [`OUTPUT_CAP`].
pub(in crate::lookout) const CUT_MARK: &str = "... (cut)";

/// Text a modal shows, already free of control characters and capped.
///
/// The only way to build one is [`Shown::of`], so a dog's or cargo's output
/// cannot reach a modal's body with an escape in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown(String);

impl Shown {
    /// `text` with control and invisible characters removed and a newline
    /// kept, then cut to [`OUTPUT_CAP`] bytes on a character boundary.
    fn of(text: &str) -> Self {
        let (mut clean, _) = sanitise_multiline(text);
        if clean.len() > OUTPUT_CAP {
            let mut end = OUTPUT_CAP;
            while !clean.is_char_boundary(end) {
                end -= 1;
            }
            clean.truncate(end);
            clean.push_str(CUT_MARK);
        }
        Self(clean)
    }

    /// The text to draw.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One thing the modal layer asks or shows.
///
/// Every kind carries the `tag` its owner chose, which comes back on the
/// answer. `Debug` is derived: a grant prompt is manifest text the operator
/// is about to review, and an output modal holds output already cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Modal {
    /// The grant prompt `shep adopt` prints, asked as yes or no.
    Grant {
        /// The owner's tag for this question.
        tag: u64,
        /// What is listed and asked.
        prompt: Prompt,
    },
    /// Long output, such as a failed install's, with a title.
    Output {
        /// The owner's tag.
        tag: u64,
        /// What the output is of, on one line.
        title: Shown,
        /// The output, capped.
        text: Shown,
    },
    /// One yes or no question.
    Confirm {
        /// The owner's tag for this question.
        tag: u64,
        /// The question, without keys.
        question: String,
    },
}

impl Modal {
    /// A grant prompt for `prompt`.
    pub(crate) fn grant(tag: u64, prompt: Prompt) -> Self {
        Self::Grant { tag, prompt }
    }

    /// `text` under `title`, with escapes removed and the text capped at
    /// [`OUTPUT_CAP`] bytes.
    pub(crate) fn output(tag: u64, title: &str, text: &str) -> Self {
        let (title, _) = sanitise(title);
        Self::Output {
            tag,
            title: Shown(title),
            text: Shown::of(text),
        }
    }

    /// A yes or no `question`.
    pub(crate) fn confirm(tag: u64, question: String) -> Self {
        Self::Confirm { tag, question }
    }

    /// The owner's tag.
    pub(crate) fn tag(&self) -> u64 {
        match self {
            Self::Grant { tag, .. } | Self::Output { tag, .. } | Self::Confirm { tag, .. } => *tag,
        }
    }
}

/// The open modal, the ones waiting, and how far the open one is scrolled.
#[derive(Debug, Default)]
pub(super) struct Modals {
    queue: VecDeque<Modal>,
    scroll: usize,
    reach: usize,
}

impl App {
    /// Opens `modal`, or queues it behind the one already open.
    pub(crate) fn open_modal(&mut self, modal: Modal) {
        self.modals.queue.push_back(modal);
    }

    /// The modal that has the keyboard, or `None`.
    pub(crate) fn modal(&self) -> Option<&Modal> {
        self.modals.queue.front()
    }

    /// How many lines the open modal's text is scrolled.
    pub(crate) fn modal_scroll(&self) -> usize {
        self.modals.scroll
    }

    /// Tells the layer how many lines of the open modal's text the box cannot
    /// show, so scrolling stops at the last line. The event loop calls it
    /// before each draw, as it does [`Self::note_body_rows`].
    pub(crate) fn note_modal_reach(&mut self, reach: usize) {
        self.modals.reach = reach;
        self.modals.scroll = self.modals.scroll.min(reach);
    }

    /// One key while a modal is open, ahead of every body and text box.
    ///
    /// A grant or a confirm answers yes on Enter and no on anything else, so
    /// a stray key grants nothing. When the text is taller than the box the
    /// four select keys scroll it instead, whatever the kind, since a
    /// permission list that cannot be read cannot be agreed to. Output closes
    /// on every other key.
    pub(super) fn on_modal_key(&mut self, key: KeyPress) -> Effect {
        let Some(modal) = self.modals.queue.front() else {
            return Effect::None;
        };
        let tag = modal.tag();
        let asks = !matches!(modal, Modal::Output { .. });
        if self.modals.reach > 0 && self.scroll_modal(key) {
            return Effect::None;
        }
        self.close_modal();
        if !asks {
            return Effect::None;
        }
        let yes = matches!(key, KeyPress::Confirm | KeyPress::TextApply);
        Effect::ModalAnswered { tag, yes }
    }

    /// Whether the open modal's text is taller than its box, so the select
    /// keys scroll it.
    pub(crate) fn modal_scrolls(&self) -> bool {
        self.modals.reach > 0
    }

    /// Moves the scroll for a select key, or returns `false` for any other.
    fn scroll_modal(&mut self, key: KeyPress) -> bool {
        let modals = &mut self.modals;
        match key {
            KeyPress::SelectUp => modals.scroll = modals.scroll.saturating_sub(1),
            KeyPress::SelectDown => modals.scroll = (modals.scroll + 1).min(modals.reach),
            KeyPress::SelectFirst => modals.scroll = 0,
            KeyPress::SelectLast => modals.scroll = modals.reach,
            _ => return false,
        }
        true
    }

    /// Drops the open modal; the next one, if any, starts at the top.
    fn close_modal(&mut self) {
        self.modals.queue.pop_front();
        self.modals.scroll = 0;
        self.modals.reach = 0;
    }
}

#[cfg(test)]
mod tests;
