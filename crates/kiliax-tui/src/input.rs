use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct InputLine {
    text: String,
    cursor: usize, // char index
}

#[derive(Debug, Clone)]
pub enum InputAction {
    None,
    Submit(String),
}

impl InputLine {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.chars().count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert_char(ch);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        match key.code {
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.text);
                self.cursor = 0;
                InputAction::Submit(text)
            }
            KeyCode::Backspace => {
                self.backspace();
                InputAction::None
            }
            KeyCode::Delete => {
                self.delete();
                InputAction::None
            }
            KeyCode::Left => {
                self.move_left();
                InputAction::None
            }
            KeyCode::Right => {
                self.move_right();
                InputAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                InputAction::None
            }
            KeyCode::End => {
                self.cursor = self.text.chars().count();
                InputAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear();
                InputAction::None
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return InputAction::None;
                }
                self.insert_char(c);
                InputAction::None
            }
            _ => InputAction::None,
        }
    }

    fn insert_char(&mut self, ch: char) {
        let byte_idx = char_to_byte_index(&self.text, self.cursor);
        self.text.insert(byte_idx, ch);
        self.cursor = self.cursor.saturating_add(1);
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte_index(&self.text, self.cursor.saturating_sub(1));
        let end = char_to_byte_index(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn delete(&mut self) {
        let len = self.text.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = char_to_byte_index(&self.text, self.cursor);
        let end = char_to_byte_index(&self.text, self.cursor.saturating_add(1));
        self.text.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let len = self.text.chars().count();
        self.cursor = (self.cursor + 1).min(len);
    }
}

fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_move_and_submit_roundtrip() {
        let mut input = InputLine::default();
        input.insert_str("hi");
        assert_eq!(input.text(), "hi");
        assert_eq!(input.cursor(), 2);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(input.cursor(), 1);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(input.text(), "h!i");
        assert_eq!(input.cursor(), 2);

        let action = input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let InputAction::Submit(text) = action else {
            panic!("expected submit action");
        };
        assert_eq!(text, "h!i");
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn ctrl_u_clears_input() {
        let mut input = InputLine::default();
        input.set_text("abc");
        assert_eq!(input.text(), "abc");
        assert_eq!(input.cursor(), 3);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn unicode_backspace_and_delete_are_char_based() {
        let mut input = InputLine::default();
        input.set_text("你a");
        assert_eq!(input.cursor(), 2);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text(), "你");
        assert_eq!(input.cursor(), 1);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);

        let _ = input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);

        input.set_text("你a");
        let _ = input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let _ = input.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.text(), "a");
        assert_eq!(input.cursor(), 0);
    }
}
