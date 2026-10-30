//! The one-line text box every lookout editor holds.
//!
//! [`TextBox`] wraps `tui_input::Input` so the eight editors share one cursor,
//! one set of word and kill commands and one idea of what a paste may carry.
//! `Input` stays private: its derived `Debug` prints the text and its cut
//! buffer, and its request enum is wider than the keys lookout binds.

use tui_input::{Input, InputRequest};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::KeyPress;

/// The cursor, drawn as a character rather than a reversed cell because the
/// ANSI gallery renders foregrounds only.
const CURSOR: char = '\u{258f}';

/// What a secret's typed character is drawn as, the glyph the secrets pane
/// already uses for a value that is set.
const MASK: char = '\u{2588}';

/// The fewest columns [`TextBox::line`] keeps for the box before the text
/// after it gives way.
const MIN_BOX: u16 = 12;

/// A cursor move or a deletion that is not one typed character, named by
/// meaning. `input::map_key` builds these from `InputMode::Text` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEdit {
    /// `Left`: one character back.
    Left,
    /// `Right`: one character forward.
    Right,
    /// `Alt-Left` or `Alt-b`: to the start of the word before the cursor.
    WordLeft,
    /// `Alt-Right` or `Alt-f`: to the start of the next word.
    WordRight,
    /// `Home` or `Ctrl-a`: to the start of the line.
    Home,
    /// `End` or `Ctrl-e`: to the end of the line.
    End,
    /// `Delete`: the character under the cursor.
    Delete,
    /// `Ctrl-w` or `Alt-Backspace`: the word before the cursor.
    DeleteWordBack,
    /// `Ctrl-u`: everything before the cursor.
    DeleteToStart,
    /// `Ctrl-k`: everything from the cursor on.
    DeleteToEnd,
    /// `Ctrl-r`: draws a secret's box as typed, or masks it again.
    Reveal,
}

/// A line of text and a cursor in it.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below:
/// editors hold env values and secrets, and `Input`'s own `Debug` prints the
/// text and the last cut. `PartialEq` compares the text and the cursor and
/// nothing else, so the panes that hold one keep their derives. Whether the
/// box is revealed is a view of the text, not part of it, so it is left out
/// of both.
#[derive(Clone, Default)]
pub struct TextBox {
    input: Input,
    revealed: bool,
}

impl core::fmt::Debug for TextBox {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "TextBox {{ chars: {}, cursor: {} }}",
            self.char_count(),
            self.cursor()
        )
    }
}

impl PartialEq for TextBox {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str() && self.cursor() == other.cursor()
    }
}

impl Eq for TextBox {}

impl From<&str> for TextBox {
    /// A box holding `text`, cursor at the end.
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}

impl From<String> for TextBox {
    /// A box holding `text`, cursor at the end.
    fn from(text: String) -> Self {
        Self {
            input: Input::new(text),
            revealed: false,
        }
    }
}

impl TextBox {
    /// An empty box.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.input.value()
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }

    /// How many characters the box holds.
    #[must_use]
    pub fn char_count(&self) -> usize {
        self.as_str().chars().count()
    }

    /// How many characters precede the cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// Inserts one typed character at the cursor.
    pub fn insert(&mut self, typed: char) {
        self.input.handle(InputRequest::InsertChar(typed));
    }

    /// Removes the character before the cursor.
    pub fn backspace(&mut self) {
        self.input.handle(InputRequest::DeletePrevChar);
    }

    /// Applies one cursor move or deletion.
    pub fn edit(&mut self, edit: TextEdit) {
        let request = match edit {
            TextEdit::Reveal => {
                self.revealed = !self.revealed;
                return;
            }
            TextEdit::Left => InputRequest::GoToPrevChar,
            TextEdit::Right => InputRequest::GoToNextChar,
            TextEdit::WordLeft => InputRequest::GoToPrevWord,
            TextEdit::WordRight => InputRequest::GoToNextWord,
            TextEdit::Home => InputRequest::GoToStart,
            TextEdit::End => InputRequest::GoToEnd,
            TextEdit::Delete => InputRequest::DeleteNextChar,
            TextEdit::DeleteWordBack => InputRequest::DeletePrevWord,
            TextEdit::DeleteToStart => InputRequest::DeleteFromStart,
            TextEdit::DeleteToEnd => InputRequest::DeleteTillEnd,
        };
        self.input.handle(request);
    }

    /// Applies a text key: a typed character, a backspace or a
    /// [`KeyPress::TextEdit`]. Any other key is not the box's and is ignored.
    pub fn handle(&mut self, key: KeyPress) {
        match key {
            KeyPress::TextChar(typed) => self.insert(typed),
            KeyPress::TextBackspace => self.backspace(),
            KeyPress::TextEdit(edit) => self.edit(edit),
            _ => {}
        }
    }

    /// The characters of a paste that a box takes: everything but newlines
    /// and other control characters, so a pasted newline is never an `Enter`.
    pub fn pasted(text: &str) -> impl Iterator<Item = char> + '_ {
        text.chars().filter(|typed| !typed.is_control())
    }

    /// The text with the cursor drawn in it, at most `width` columns wide.
    ///
    /// A box wider than `width` scrolls sideways just far enough to keep the
    /// cursor on the last column, so what is being typed is always in view.
    #[must_use]
    pub fn drawn(&self, width: u16) -> String {
        self.scrolled(self.as_str().chars().collect(), width)
    }

    /// [`Self::drawn`] with each typed character drawn as one `█`.
    ///
    /// The cursor and the scroll are the same, so a secret being typed is
    /// as easy to edit as any text and shows nothing of what it holds.
    #[must_use]
    pub fn drawn_masked(&self, width: u16) -> String {
        self.scrolled(vec![MASK; self.char_count()], width)
    }

    /// [`Self::drawn_masked`] when `secret` and the box is not revealed,
    /// [`Self::drawn`] otherwise.
    #[must_use]
    pub fn drawn_for(&self, secret: bool, width: u16) -> String {
        if secret && !self.revealed {
            self.drawn_masked(width)
        } else {
            self.drawn(width)
        }
    }

    /// The text with no cursor: as typed, or one `█` per character when
    /// `secret` and the box is not revealed.
    #[must_use]
    pub fn shown(&self, secret: bool) -> String {
        if secret && !self.revealed {
            MASK.to_string().repeat(self.char_count())
        } else {
            self.as_str().to_owned()
        }
    }

    /// `cells` with the cursor put in at [`Self::cursor`], cut to `width`
    /// columns.
    fn scrolled(&self, mut cells: Vec<char>, width: u16) -> String {
        let width = usize::from(width);
        let at = self.cursor();
        cells.insert(at, CURSOR);
        let cursor_column: usize = cells[..at].iter().map(|c| c.width().unwrap_or(0)).sum();
        let scroll = (cursor_column + 1).saturating_sub(width);
        let mut column = 0;
        let mut drawn = String::new();
        let mut drawn_width = 0;
        for cell in cells {
            let cell_width = cell.width().unwrap_or(0);
            if column < scroll {
                column += cell_width;
                continue;
            }
            if drawn_width + cell_width > width {
                break;
            }
            drawn.push(cell);
            drawn_width += cell_width;
        }
        drawn
    }
}

impl TextBox {
    /// `before`, the box drawn with its cursor, then `after`, in `width`
    /// columns.
    ///
    /// The box gets what the two ends leave, so it scrolls to keep the
    /// cursor on screen instead of being cut with the line. When that is
    /// under [`MIN_BOX`] columns, `after` gives way first.
    #[must_use]
    pub fn line(&self, before: &str, after: &str, width: u16) -> String {
        let columns = |text: &str| u16::try_from(text.width()).unwrap_or(u16::MAX);
        let room = width.saturating_sub(columns(before));
        let beside = room.saturating_sub(columns(after));
        let budget = if beside >= MIN_BOX { beside } else { room };
        format!("{before}{}{after}", self.drawn(budget))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(text: &str, cursor: usize) -> TextBox {
        let mut text_box = TextBox::from(text);
        for _ in cursor..text.chars().count() {
            text_box.edit(TextEdit::Left);
        }
        text_box
    }

    #[test]
    fn a_character_typed_in_the_middle_lands_at_the_cursor() {
        let mut text_box = boxed("acd", 1);

        text_box.insert('b');

        assert_eq!(text_box.as_str(), "abcd");
        assert_eq!(text_box.cursor(), 2);
    }

    #[test]
    fn the_word_moves_land_on_word_starts_and_the_ends() {
        let mut text_box = TextBox::from("foo bar baz");

        text_box.edit(TextEdit::WordLeft);
        assert_eq!(text_box.cursor(), 8);
        text_box.edit(TextEdit::WordLeft);
        assert_eq!(text_box.cursor(), 4);
        text_box.edit(TextEdit::WordRight);
        assert_eq!(text_box.cursor(), 8);
        text_box.edit(TextEdit::WordRight);
        assert_eq!(text_box.cursor(), 11);
        text_box.edit(TextEdit::Home);
        assert_eq!(text_box.cursor(), 0);
        text_box.edit(TextEdit::End);
        assert_eq!(text_box.cursor(), 11);
    }

    #[test]
    fn the_character_moves_stop_at_both_ends() {
        let mut text_box = boxed("ab", 1);

        text_box.edit(TextEdit::Right);
        text_box.edit(TextEdit::Right);
        assert_eq!(text_box.cursor(), 2);
        text_box.edit(TextEdit::Left);
        text_box.edit(TextEdit::Left);
        text_box.edit(TextEdit::Left);
        assert_eq!(text_box.cursor(), 0);
    }

    #[test]
    fn delete_word_back_removes_the_word_before_the_cursor() {
        let mut text_box = TextBox::from("foo bar");

        text_box.edit(TextEdit::DeleteWordBack);

        assert_eq!(text_box.as_str(), "foo ");
        assert_eq!(text_box.cursor(), 4);
    }

    #[test]
    fn delete_to_start_keeps_what_follows_the_cursor() {
        let mut text_box = boxed("foo bar", 3);

        text_box.edit(TextEdit::DeleteToStart);

        assert_eq!(text_box.as_str(), " bar");
        assert_eq!(text_box.cursor(), 0);
    }

    #[test]
    fn delete_to_end_keeps_what_precedes_the_cursor() {
        let mut text_box = boxed("foo bar", 3);

        text_box.edit(TextEdit::DeleteToEnd);

        assert_eq!(text_box.as_str(), "foo");
        assert_eq!(text_box.cursor(), 3);
    }

    #[test]
    fn delete_removes_the_character_under_the_cursor_and_backspace_the_one_before() {
        let mut text_box = boxed("wxyz", 2);

        text_box.edit(TextEdit::Delete);
        assert_eq!(text_box.as_str(), "wxz");
        text_box.backspace();
        assert_eq!(text_box.as_str(), "wz");
        assert_eq!(text_box.cursor(), 1);
    }

    #[test]
    fn multibyte_text_moves_and_deletes_by_character() {
        let mut text_box = TextBox::from("héllo wörld");

        text_box.edit(TextEdit::WordLeft);
        text_box.edit(TextEdit::Left);
        text_box.insert('ü');
        assert_eq!(text_box.as_str(), "hélloü wörld");
        assert_eq!(text_box.cursor(), 6);
        text_box.backspace();
        text_box.edit(TextEdit::Left);
        text_box.edit(TextEdit::Delete);
        assert_eq!(text_box.as_str(), "héll wörld");
        assert_eq!(text_box.cursor(), 4);
    }

    #[test]
    fn backspace_takes_a_base_letter_and_its_combining_mark_together() {
        let mut text_box = TextBox::from("ae\u{301}");

        text_box.backspace();

        assert_eq!(text_box.as_str(), "a");
    }

    #[test]
    fn a_paste_keeps_its_printable_characters_and_drops_every_control_one() {
        let kept: String = TextBox::pasted("ab\r\ncd\t\u{1b}[31me\u{7f}\u{e9}").collect();

        assert_eq!(kept, "abcd[31me\u{e9}");
    }

    #[test]
    fn debug_shows_the_size_and_never_the_text() {
        let mut text_box = TextBox::from("hunter2");
        text_box.edit(TextEdit::DeleteWordBack);
        text_box.insert('s');

        assert_eq!(format!("{text_box:?}"), "TextBox { chars: 1, cursor: 1 }");
        assert_eq!(
            format!("{:?}", Some(TextBox::from("hunter2"))),
            "Some(TextBox { chars: 7, cursor: 7 })"
        );
    }

    #[test]
    fn equality_reads_the_text_and_the_cursor() {
        assert_eq!(boxed("ab", 1), boxed("ab", 1));
        assert_ne!(boxed("ab", 1), boxed("ab", 2));
        assert_ne!(boxed("ab", 2), boxed("ac", 2));
    }

    #[test]
    fn the_cursor_is_drawn_where_it_is() {
        assert_eq!(boxed("hello", 2).drawn(80), "he\u{258f}llo");
        assert_eq!(boxed("hello", 5).drawn(80), "hello\u{258f}");
        assert_eq!(TextBox::new().drawn(80), "\u{258f}");
    }

    #[test]
    fn a_narrow_cell_scrolls_to_keep_the_cursor_in_view() {
        assert_eq!(boxed("hello world", 11).drawn(6), "world\u{258f}");
        assert_eq!(boxed("hello world", 0).drawn(6), "\u{258f}hello");
        assert_eq!(boxed("hello world", 8).drawn(6), "lo wo\u{258f}");
        assert_eq!(boxed("jumps over", 3).drawn(6), "jum\u{258f}ps");
    }

    #[test]
    fn a_masked_cell_keeps_the_cursor_and_its_scroll() {
        assert_eq!(boxed("hunter2", 3).drawn_masked(80), "\u{2588}\u{2588}\u{2588}\u{258f}\u{2588}\u{2588}\u{2588}\u{2588}");
        assert_eq!(TextBox::new().drawn_masked(80), "\u{258f}");
        assert_eq!(
            boxed("hello world", 11).drawn_masked(6),
            "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{258f}"
        );
        assert_eq!(
            boxed("hello world", 0).drawn_masked(6),
            "\u{258f}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}"
        );
        assert_eq!(
            boxed("hello world", 8).drawn_masked(6),
            "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{258f}"
        );
        assert_eq!(TextBox::from("abc").drawn_masked(0), "");
    }

    #[test]
    fn a_masked_wide_character_is_one_block_and_nothing_else_survives() {
        let masked = TextBox::from("\u{65e5}\u{672c}\u{8a9e}").drawn_masked(5);

        assert_eq!(masked, "\u{2588}\u{2588}\u{2588}\u{258f}");
        assert!(!masked.contains('\u{65e5}'));
    }

    #[test]
    fn revealing_toggles_what_a_secret_draws_and_leaves_the_text_alone() {
        let mut text_box = boxed("ab", 1);

        assert_eq!(text_box.drawn_for(true, 80), "\u{2588}\u{258f}\u{2588}");
        text_box.edit(TextEdit::Reveal);
        assert_eq!(text_box.drawn_for(true, 80), "a\u{258f}b");
        assert_eq!(text_box.drawn_for(false, 80), "a\u{258f}b");
        text_box.edit(TextEdit::Reveal);
        assert_eq!(text_box.drawn_for(true, 80), "\u{2588}\u{258f}\u{2588}");
        assert_eq!(text_box.as_str(), "ab");
        assert_eq!(text_box.cursor(), 1);
    }

    #[test]
    fn a_secret_box_shown_without_a_cursor_is_a_run_of_blocks() {
        let mut text_box = TextBox::from("hunter2");

        assert_eq!(text_box.shown(true), "\u{2588}".repeat(7));
        assert_eq!(text_box.shown(false), "hunter2");
        text_box.edit(TextEdit::Reveal);
        assert_eq!(text_box.shown(true), "hunter2");
    }

    #[test]
    fn a_wide_character_is_never_split_by_the_scroll() {
        let drawn = TextBox::from("\u{65e5}\u{672c}\u{8a9e}").drawn(5);

        assert_eq!(drawn, "\u{672c}\u{8a9e}\u{258f}");
    }

    #[test]
    fn a_line_leaves_the_box_what_the_two_ends_leave() {
        let text_box = boxed("hello", 2);

        assert_eq!(
            text_box.line("filter  ", "   enter applies", 40),
            "filter  he\u{258f}llo   enter applies"
        );
    }

    #[test]
    fn a_line_scrolls_the_box_rather_than_cutting_it() {
        let text_box = TextBox::from("0123456789abcdefghij");

        let line = text_box.line("f  ", "   enter applies", 33);

        assert_eq!(line, "f  789abcdefghij\u{258f}   enter applies");
    }

    #[test]
    fn a_line_drops_the_text_after_the_box_before_squeezing_it() {
        let text_box = TextBox::from("0123456789abcdefghij");

        let line = text_box.line("f  ", "   enter applies   esc cancels", 20);

        assert_eq!(
            line,
            "f  456789abcdefghij\u{258f}   enter applies   esc cancels"
        );
    }

    #[test]
    fn handle_applies_text_keys_and_ignores_the_rest() {
        let mut text_box = TextBox::new();

        text_box.handle(KeyPress::TextChar('a'));
        text_box.handle(KeyPress::TextChar('b'));
        text_box.handle(KeyPress::TextEdit(TextEdit::Left));
        text_box.handle(KeyPress::TextBackspace);
        text_box.handle(KeyPress::Confirm);

        assert_eq!(text_box.as_str(), "b");
        assert_eq!(text_box.cursor(), 0);
    }

    #[test]
    fn a_cell_with_no_room_draws_nothing() {
        assert_eq!(TextBox::from("abc").drawn(0), "");
        assert_eq!(TextBox::from("abc").drawn(1), "\u{258f}");
    }
}
