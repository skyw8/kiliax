#[derive(Debug, Default, Clone)]
pub(super) struct OutputTokenCounter {
    tokens: u64,
    carry_bytes: usize,
}

impl OutputTokenCounter {
    pub(super) fn reset(&mut self) {
        self.tokens = 0;
        self.carry_bytes = 0;
    }

    pub(super) fn estimate(&self) -> u64 {
        self.tokens + ((self.carry_bytes + 3) / 4) as u64
    }

    pub(super) fn finish_segment(&mut self) {
        if self.carry_bytes > 0 {
            self.tokens += ((self.carry_bytes + 3) / 4) as u64;
            self.carry_bytes = 0;
        }
    }

    pub(super) fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            if is_cjk_like(ch) {
                self.finish_segment();
                self.tokens += 1;
                continue;
            }

            if ch.is_whitespace() {
                self.finish_segment();
                continue;
            }

            if ch.is_ascii() {
                self.carry_bytes = self.carry_bytes.saturating_add(1);
                continue;
            }

            self.finish_segment();
            self.tokens += 1;
        }
    }
}

fn is_cjk_like(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0x20000..=0x2A6DF | // CJK Unified Ideographs Extension B
        0x2A700..=0x2B73F | // CJK Unified Ideographs Extension C
        0x2B740..=0x2B81F | // CJK Unified Ideographs Extension D
        0x2B820..=0x2CEAF | // CJK Unified Ideographs Extension E
        0x2CEB0..=0x2EBEF | // CJK Unified Ideographs Extension F
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0xAC00..=0xD7AF // Hangul Syllables
    )
}

