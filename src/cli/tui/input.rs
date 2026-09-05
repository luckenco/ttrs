use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A single line with a byte cursor at a grapheme boundary.
#[derive(Default)]
pub(super) struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Inserts literal text, flattening line breaks and excluding terminal controls.
    /// A paste never submits a form or invokes dashboard shortcuts.
    pub fn insert(&mut self, text: &str) -> bool {
        let text: String = text
            .chars()
            .filter_map(|c| match c {
                '\r' | '\n' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        if text.is_empty() {
            return false;
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.snap_cursor();
        true
    }

    /// Returns whether content changed; cursor motion does not rerun searches.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let changed = !self.text.is_empty();
                self.text.clear();
                self.cursor = 0;
                return changed;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                return self.insert(&c.to_string());
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let start = self.previous();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
                self.snap_cursor();
                return true;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.replace_range(self.cursor..self.next(), "");
                self.snap_cursor();
                return true;
            }
            _ => {}
        }
        false
    }

    /// Returns a horizontally scrolled slice and the cursor's terminal column.
    pub fn window(&self, width: u16) -> (&str, u16) {
        if width == 0 {
            return ("", 0);
        }
        let mut start = self.cursor;
        let mut column = 0;
        for (index, grapheme) in self.text[..self.cursor].grapheme_indices(true).rev() {
            let next = column + grapheme.width();
            if next >= usize::from(width) {
                break;
            }
            start = index;
            column = next;
        }
        let mut used = 0;
        let mut end = start;
        for grapheme in self.text[start..].graphemes(true) {
            used += grapheme.width();
            if used > usize::from(width) {
                break;
            }
            end += grapheme.len();
        }
        (&self.text[start..end], column as u16)
    }

    fn snap_cursor(&mut self) {
        // Edits can join adjacent combining marks or emoji into one grapheme.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= self.cursor)
            .unwrap_or(self.text.len());
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .graphemes(true)
                .next()
                .map(str::len)
                .unwrap_or(0)
    }
}
