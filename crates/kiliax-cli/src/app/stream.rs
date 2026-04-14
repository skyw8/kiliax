use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::markdown::render_markdown_lines;

#[derive(Debug, Default, Clone)]
pub(super) struct MarkdownStreamCollector {
    buffer: String,
    committed_line_count: usize,
}

impl MarkdownStreamCollector {
    pub(super) fn clear(&mut self) {
        self.buffer.clear();
        self.committed_line_count = 0;
    }

    pub(super) fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
        if self.committed_line_count == 0 {
            trim_leading_newlines(&mut self.buffer);
        }
    }

    pub(super) fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        if self.committed_line_count == 0 {
            trim_leading_newlines(&mut self.buffer);
        }
    }

    pub(super) fn commit_complete_lines(&mut self) -> Vec<Line<'static>> {
        let Some(last_newline_idx) = self.buffer.rfind('\n') else {
            return Vec::new();
        };
        let source = &self.buffer[..=last_newline_idx];
        let mut rendered = render_markdown_lines(source);
        trim_trailing_blank_lines(&mut rendered);

        if self.committed_line_count >= rendered.len() {
            return Vec::new();
        }

        let out = rendered[self.committed_line_count..].to_vec();
        self.committed_line_count = rendered.len();
        out
    }

    pub(super) fn finalize_and_drain(&mut self) -> Vec<Line<'static>> {
        if self.buffer.is_empty() {
            self.clear();
            return Vec::new();
        }

        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }
        let mut rendered = render_markdown_lines(&source);
        trim_trailing_blank_lines(&mut rendered);

        let out = if self.committed_line_count >= rendered.len() {
            Vec::new()
        } else {
            rendered[self.committed_line_count..].to_vec()
        };

        self.clear();
        out
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct ThinkingStreamCollector {
    buffer: String,
}

impl ThinkingStreamCollector {
    pub(super) fn clear(&mut self) {
        self.buffer.clear();
    }

    pub(super) fn push_delta(&mut self, delta: &str, max_width: usize) -> Vec<Line<'static>> {
        self.buffer.push_str(delta);
        self.drain_ready_lines(max_width, false)
    }

    pub(super) fn finalize_and_drain(&mut self, max_width: usize) -> Vec<Line<'static>> {
        if self.buffer.trim().is_empty() {
            self.clear();
            return Vec::new();
        }

        let mut out = self.drain_ready_lines(max_width, true);
        trim_trailing_blank_lines(&mut out);
        self.clear();
        out
    }

    fn drain_ready_lines(&mut self, max_width: usize, flush: bool) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();

        loop {
            if let Some(newline_idx) = self.buffer.find('\n') {
                let mut chunk: String = self.buffer.drain(..=newline_idx).collect();
                if chunk.ends_with('\n') {
                    chunk.pop();
                }
                if chunk.ends_with('\r') {
                    chunk.pop();
                }
                self.emit_wrapped_text(chunk, max_width, &mut out);
                continue;
            }

            if self.buffer.is_empty() {
                break;
            }

            if flush || max_width == 0 {
                let chunk = std::mem::take(&mut self.buffer);
                self.emit_wrapped_text(chunk, max_width, &mut out);
                break;
            }

            let Some(split_idx) = soft_wrap_split_idx(&self.buffer, max_width) else {
                break;
            };

            let mut tail = self.buffer.split_off(split_idx);
            let mut head = std::mem::take(&mut self.buffer);
            trim_end_whitespace(&mut head);
            trim_start_whitespace(&mut tail);
            self.emit_line(head, &mut out);
            self.buffer = tail;
        }

        out
    }

    fn emit_wrapped_text(
        &mut self,
        mut text: String,
        max_width: usize,
        out: &mut Vec<Line<'static>>,
    ) {
        if max_width == 0 {
            self.emit_line(text, out);
            return;
        }

        loop {
            let Some(split_idx) = soft_wrap_split_idx(&text, max_width) else {
                break;
            };

            let mut tail = text.split_off(split_idx);
            let mut head = text;
            trim_end_whitespace(&mut head);
            trim_start_whitespace(&mut tail);

            self.emit_line(head, out);
            text = tail;
        }

        self.emit_line(text, out);
    }

    fn emit_line(&mut self, mut text: String, out: &mut Vec<Line<'static>>) {
        trim_end_whitespace(&mut text);
        if text.trim().is_empty() {
            return;
        }

        let thinking_style = Style::default().dim().italic();
        let mut rendered = Line::from(Span::raw(text));
        rendered.style = thinking_style;
        out.push(rendered);
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines.last().is_some_and(|line| line_is_blank(line)) {
        lines.pop();
    }
}

fn line_is_blank(line: &Line<'static>) -> bool {
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    text.trim().is_empty()
}

pub(super) fn soft_wrap_split_idx(text: &str, max_width: usize) -> Option<usize> {
    if max_width == 0 {
        return None;
    }

    let mut width = 0usize;
    let mut last_whitespace_idx = None;

    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch.is_whitespace() {
            last_whitespace_idx = Some(idx);
        }

        if width + ch_width > max_width {
            if let Some(ws_idx) = last_whitespace_idx.filter(|&i| i > 0) {
                return Some(ws_idx);
            }
            if idx == 0 {
                return Some(ch.len_utf8());
            }
            return Some(idx);
        }

        width = width.saturating_add(ch_width);
    }

    None
}

fn trim_end_whitespace(text: &mut String) {
    while text.chars().last().is_some_and(|ch| ch.is_whitespace()) {
        text.pop();
    }
}

fn trim_start_whitespace(text: &mut String) {
    let start = text
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    if start > 0 {
        text.drain(..start);
    }
}

fn trim_leading_newlines(text: &mut String) {
    let start = text
        .char_indices()
        .find(|(_, ch)| *ch != '\n' && *ch != '\r')
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    if start > 0 {
        text.drain(..start);
    }
}

