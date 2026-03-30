use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const IMAGE_PLACEHOLDER_PREFIX: &str = "[img#";

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

        if let Some((start, end)) = image_placeholder_range_ending_at(&self.text, self.cursor) {
            self.delete_char_range(start, end);
            self.cursor = start;
            return;
        }

        // If the cursor is after a space that was inserted after an image placeholder, treat
        // the placeholder as a single editable unit.
        if self
            .text
            .chars()
            .nth(self.cursor.saturating_sub(1))
            .is_some_and(|ch| ch == ' ')
        {
            if let Some((start, end)) =
                image_placeholder_range_ending_at(&self.text, self.cursor.saturating_sub(1))
            {
                self.delete_char_range(start, end.saturating_add(1));
                self.cursor = start;
                return;
            }
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

        if let Some((start, end)) = image_placeholder_range_starting_at(&self.text, self.cursor) {
            let mut end = end;
            if self.text.chars().nth(end).is_some_and(|ch| ch == ' ') {
                end = end.saturating_add(1);
            }
            self.delete_char_range(start, end);
            return;
        }

        let start = char_to_byte_index(&self.text, self.cursor);
        let end = char_to_byte_index(&self.text, self.cursor.saturating_add(1));
        self.text.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }

        if let Some((start, end)) = image_placeholder_range_ending_at(&self.text, self.cursor) {
            if end == self.cursor {
                self.cursor = start;
                return;
            }
        }

        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let len = self.text.chars().count();
        if self.cursor >= len {
            return;
        }

        if let Some((_, end)) = image_placeholder_range_starting_at(&self.text, self.cursor) {
            self.cursor = end.min(len);
            return;
        }

        self.cursor = (self.cursor + 1).min(len);
    }

    fn delete_char_range(&mut self, start_char: usize, end_char: usize) {
        if start_char >= end_char {
            return;
        }
        let start = char_to_byte_index(&self.text, start_char);
        let end = char_to_byte_index(&self.text, end_char);
        self.text.replace_range(start..end, "");
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

fn image_placeholder_range_ending_at(text: &str, end_char: usize) -> Option<(usize, usize)> {
    let end_byte = char_to_byte_index(text, end_char);
    let start_byte = text[..end_byte].rfind(IMAGE_PLACEHOLDER_PREFIX)?;
    let slice = text.get(start_byte..end_byte)?;
    if !slice.ends_with(']') {
        return None;
    }

    let digits = slice.get(IMAGE_PLACEHOLDER_PREFIX.len()..slice.len().saturating_sub(1))?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let start_char = text[..start_byte].chars().count();
    let len_chars = slice.chars().count();
    let end_char_calc = start_char.saturating_add(len_chars);
    if end_char_calc != end_char {
        return None;
    }
    Some((start_char, end_char_calc))
}

fn image_placeholder_range_starting_at(text: &str, start_char: usize) -> Option<(usize, usize)> {
    let start_byte = char_to_byte_index(text, start_char);
    let rest = text.get(start_byte..)?;
    if !rest.starts_with(IMAGE_PLACEHOLDER_PREFIX) {
        return None;
    }

    let close_rel = rest.find(']')?;
    let slice = rest.get(..=close_rel)?;
    let digits = slice.get(IMAGE_PLACEHOLDER_PREFIX.len()..slice.len().saturating_sub(1))?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let len_chars = slice.chars().count();
    Some((start_char, start_char.saturating_add(len_chars)))
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

    #[test]
    fn image_placeholder_is_deleted_atomically_with_backspace() {
        let mut input = InputLine::default();
        input.set_text("hi [img#12] there");

        // Place cursor right after the placeholder.
        input.cursor = "hi [img#12]".chars().count();

        let _ = input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text(), "hi  there");
        assert_eq!(input.cursor(), "hi ".chars().count());
    }

    #[test]
    fn backspace_after_placeholder_trailing_space_deletes_placeholder_and_space() {
        let mut input = InputLine::default();
        input.set_text("[img#1] ");

        let _ = input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn delete_at_placeholder_start_deletes_placeholder_and_trailing_space() {
        let mut input = InputLine::default();
        input.set_text("[img#7] ok");

        let _ = input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let _ = input.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.text(), "ok");
        assert_eq!(input.cursor(), 0);
    }
}
