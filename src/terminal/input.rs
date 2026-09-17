use std::ops::Deref;

use tui_input::{Input, InputRequest};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Default)]
pub(super) struct EditorInput {
    inner: Input,
}

impl EditorInput {
    pub(super) fn clear(&mut self) {
        self.inner.reset();
    }

    pub(super) fn replace(&mut self, value: String) {
        self.inner = Input::new(value);
    }

    pub(super) fn take(&mut self) -> String {
        self.inner.value_and_reset()
    }

    pub(super) fn cursor(&self) -> usize {
        let codepoint_cursor = self.inner.cursor();
        let mut codepoints = 0;
        self.inner
            .value()
            .graphemes(true)
            .take_while(|grapheme| {
                if codepoints >= codepoint_cursor {
                    return false;
                }
                codepoints += grapheme.chars().count();
                true
            })
            .count()
    }

    #[cfg(test)]
    pub(super) fn set_cursor(&mut self, grapheme_cursor: usize) {
        let codepoint_cursor = self
            .inner
            .value()
            .graphemes(true)
            .take(grapheme_cursor)
            .map(str::chars)
            .map(Iterator::count)
            .sum();
        self.inner.handle(InputRequest::SetCursor(codepoint_cursor));
    }

    pub(super) fn insert_text(&mut self, text: &str) {
        for character in text.chars() {
            self.inner.handle(InputRequest::InsertChar(character));
        }
    }

    /// Paste is text, never a stream of terminal key events.
    pub(super) fn insert_paste(&mut self, text: &str, multiline: bool) {
        let mut previous_cr = false;
        for character in text.chars() {
            if !(previous_cr && character == '\n') {
                if multiline && matches!(character, '\r' | '\n') {
                    self.inner.handle(InputRequest::InsertChar('\n'));
                } else if matches!(character, '\r' | '\n' | '\t')
                    || (character.is_whitespace() && !character.is_control())
                {
                    self.inner.handle(InputRequest::InsertChar(' '));
                } else if !character.is_control() {
                    self.inner.handle(InputRequest::InsertChar(character));
                }
            }
            previous_cr = character == '\r';
        }
    }

    pub(super) fn move_to_start(&mut self) {
        self.inner.handle(InputRequest::GoToStart);
    }

    pub(super) fn move_to_end(&mut self) {
        self.inner.handle(InputRequest::GoToEnd);
    }

    pub(super) fn move_left(&mut self) {
        self.inner.handle(InputRequest::GoToPrevChar);
    }

    pub(super) fn move_right(&mut self) {
        self.inner.handle(InputRequest::GoToNextChar);
    }

    pub(super) fn move_word_left(&mut self) {
        self.inner.handle(InputRequest::GoToPrevWord);
    }

    pub(super) fn move_word_right(&mut self) {
        self.inner.handle(InputRequest::GoToNextWord);
    }

    pub(super) fn delete_before_cursor(&mut self) {
        self.inner.handle(InputRequest::DeletePrevChar);
    }

    pub(super) fn delete_at_cursor(&mut self) {
        self.inner.handle(InputRequest::DeleteNextChar);
    }

    pub(super) fn delete_previous_word(&mut self) {
        self.inner.handle(InputRequest::DeletePrevWord);
    }

    pub(super) fn delete_to_end(&mut self) {
        self.inner.handle(InputRequest::DeleteTillEnd);
    }

    pub(super) fn previous_grapheme(&self) -> Option<&str> {
        let cursor = self.cursor();
        self.inner
            .value()
            .graphemes(true)
            .nth(cursor.checked_sub(1)?)
    }
}

impl Deref for EditorInput {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.inner.value()
    }
}

impl From<String> for EditorInput {
    fn from(value: String) -> Self {
        Self {
            inner: Input::new(value),
        }
    }
}

impl From<&str> for EditorInput {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl PartialEq<&str> for EditorInput {
    fn eq(&self, other: &&str) -> bool {
        self.inner.value() == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_graphemes_without_splitting_them() {
        let mut input = EditorInput::from("a👨‍👩‍👧‍👦你");
        input.move_left();
        input.delete_before_cursor();
        assert_eq!(input, "a你");
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn inserts_text_at_the_current_grapheme_cursor() {
        let mut input = EditorInput::from("甲乙");
        input.set_cursor(1);
        input.insert_text("🙂");
        assert_eq!(input, "甲🙂乙");
        assert_eq!(input.cursor(), 2);
    }
    #[test]
    fn paste_normalizes_line_endings_without_inserting_terminal_controls() {
        for (multiline, expected) in [(false, "甲A B C乙"), (true, "甲A\nB C乙")] {
            let mut input = EditorInput::from("甲乙");
            input.set_cursor(1);
            input.insert_paste("A\r\nB\tC\u{1b}\u{3}\u{10}\u{7f}\u{85}", multiline);
            assert_eq!(input, expected);
        }
    }
}
