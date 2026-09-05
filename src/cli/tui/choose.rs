use crossterm::event::{KeyCode, KeyEvent};
use ratatui::widgets::ListState;

use super::{input::Input, search, state::move_selection};

/// A searchable list. Enter is handled by its owner, never by text insertion.
pub(super) struct Choose<T> {
    pub title: String,
    pub input: Input,
    pub items: search::Matches<T>,
    pub selection: ListState,
}

impl<T> Choose<T> {
    pub fn new(title: impl Into<String>, items: impl IntoIterator<Item = (T, String)>) -> Self {
        Self {
            title: title.into(),
            input: Input::default(),
            items: search::Matches::from_labels(items),
            selection: ListState::default().with_selected(Some(0)),
        }
    }

    pub fn selected(&self) -> Option<&T> {
        self.selection
            .selected()
            .and_then(|i| self.items.matches().nth(i))
    }

    pub fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down => move_selection(&mut self.selection, self.items.matches().len(), true),
            KeyCode::Up => move_selection(&mut self.selection, self.items.matches().len(), false),
            _ => {
                if self.input.key(key) {
                    self.filter();
                }
            }
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.input.insert(text) {
            self.filter();
        }
    }

    fn filter(&mut self) {
        self.items.filter(self.input.as_str());
        self.selection.select(Some(0));
    }
}
