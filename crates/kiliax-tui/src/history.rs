use std::io;
use std::time::Duration;

use crossterm::cursor::{MoveDown, MoveTo, MoveToColumn, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::style::{Attribute, Color as CColor, Print, SetAttribute, SetForegroundColor};
use crossterm::style::SetBackgroundColor;
use crossterm::terminal::{Clear, ClearType};
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::custom_terminal::{
    DisableWraparound, EnableWraparound, ResetScrollRegion, ReverseIndex, SetScrollRegion,
};

pub const DIVIDER_MARKER_PREFIX: &str = "\u{001f}kiliax_divider:";
pub const USER_MESSAGE_MARKER_PREFIX: &str = "\u{001f}kiliax_user_message:";

const USER_MESSAGE_PREFIX_COLS: usize = 2; // "› "
const USER_MESSAGE_RIGHT_MARGIN_COLS: usize = 1;

pub fn insert_history_lines(
    out: &mut impl io::Write,
    lines: &[Line<'static>],
    viewport: &mut Rect,
    full_size: Size,
) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    if full_size.height == 0 || full_size.width == 0 {
        return Ok(());
    }

    let render_width = full_size.width.saturating_sub(1).max(1) as usize;

    let expanded = expand_special_lines(lines, render_width);
    let wrapped = crate::wrap::wrap_lines(&expanded, render_width);
    let wrapped_rows = wrapped.len().min(u16::MAX as usize) as u16;

    let cursor_top = viewport.top().saturating_sub(1);

    // If the viewport is not at the bottom of the screen, scroll the lower region down to make
    // room so the viewport can be pushed down inline (codex-style).
    if wrapped_rows > 0 && viewport.bottom() < full_size.height {
        let scroll_amount = wrapped_rows.min(full_size.height - viewport.bottom());
        if scroll_amount > 0 {
            let top_1based = viewport.top().saturating_add(1);
            queue!(
                out,
                SetScrollRegion(top_1based..full_size.height),
                MoveTo(0, viewport.top())
            )?;
            for _ in 0..scroll_amount {
                // Reverse Index (RI): ESC M
                queue!(out, ReverseIndex)?;
            }
            queue!(out, ResetScrollRegion)?;
            viewport.y = viewport.y.saturating_add(scroll_amount);
        }
    }

    let viewport_top = viewport.top();
    if viewport_top == 0 {
        out.flush()?;
        return Ok(());
    }

    // Disable wraparound while writing history lines. This avoids terminals (notably xterm.js) that
    // auto-wrap when output lands in the last column, which can manifest as extra blank lines.
    queue!(out, DisableWraparound)?;

    queue!(out, SetScrollRegion(1..viewport_top), MoveTo(0, cursor_top))?;

    for line in wrapped {
        // Don't emit '\n' or '\r' here: some PTY settings can translate them in ways that cause
        // double-spaced scrollback. Codex uses CRLF here; with raw mode enabled OPOST is off, so
        // the terminal should treat this as a single line advance.
        queue!(out, Print("\r\n"))?;

        // Some terminals can character-wrap long lines onto continuation rows. Pre-clear those
        // rows so stale content from a previously longer line is erased. This should usually be
        // a no-op because we pre-wrap, but is kept for codex parity.
        let physical_rows = line
            .width()
            .max(1)
            .div_ceil(render_width.max(1));
        if physical_rows > 1 {
            queue!(out, SavePosition)?;
            for _ in 1..physical_rows {
                queue!(out, MoveDown(1), MoveToColumn(0), Clear(ClearType::UntilNewLine))?;
            }
            queue!(out, RestorePosition)?;
        }

        apply_style(out, line.style)?;
        queue!(out, Clear(ClearType::UntilNewLine))?;
        write_spans(out, line.spans.iter(), line.style)?;
    }

    queue!(out, ResetScrollRegion, EnableWraparound)?;
    out.flush()?;
    Ok(())
}

fn expand_special_lines(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(content) = parse_user_message_marker(line) {
            out.extend(render_user_message_block(&content, width));
            continue;
        }
        if let Some(marker) = parse_divider_marker(line) {
            out.extend(render_divider_block(
                Duration::from_millis(marker.ms),
                marker.output_tokens,
                width,
            ));
            continue;
        }
        out.push(line.clone());
    }
    out
}

fn parse_user_message_marker(line: &Line<'static>) -> Option<String> {
    let text = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    let payload = text.strip_prefix(USER_MESSAGE_MARKER_PREFIX)?;
    serde_json::from_str::<String>(payload).ok()
}

#[derive(Debug, Clone, Copy)]
struct DividerMarker {
    ms: u64,
    output_tokens: Option<u64>,
}

fn parse_divider_marker(line: &Line<'static>) -> Option<DividerMarker> {
    let text = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    let payload = text.strip_prefix(DIVIDER_MARKER_PREFIX)?;
    let mut parts = payload.trim().splitn(2, ',');
    let ms = parts.next()?.trim().parse::<u64>().ok()?;
    let output_tokens = parts
        .next()
        .and_then(|rest| rest.trim().parse::<u64>().ok());
    Some(DividerMarker { ms, output_tokens })
}

fn render_divider_block(elapsed: Duration, output_tokens: Option<u64>, width: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        render_divider_line(elapsed, output_tokens, width),
        Line::from(""),
    ]
}

fn render_divider_line(elapsed: Duration, output_tokens: Option<u64>, width: usize) -> Line<'static> {
    let mut label = format!("Worked for {}", fmt_elapsed_compact(elapsed));
    if let Some(output_tokens) = output_tokens {
        label.push_str(&format!(" · {output_tokens} tok"));
    }
    let mut text = format!("─ {label} ─");
    let mut text_width = text.width();

    if text_width > width {
        text = take_prefix_by_width(&text, width);
        text_width = text.width();
    }
    if text_width < width {
        text.push_str(&"─".repeat(width - text_width));
    }

    let mut line = Line::from(Span::from(text));
    line.style = Style::default().dim();
    line
}

fn render_user_message_block(content: &str, width: usize) -> Vec<Line<'static>> {
    let bubble = crate::style::composer_background_style();

    let wrap_width = width
        .saturating_sub(USER_MESSAGE_PREFIX_COLS + USER_MESSAGE_RIGHT_MARGIN_COLS)
        .max(1);
    let rendered = crate::markdown::render_markdown_lines(content);
    let wrapped = crate::wrap::wrap_lines(&rendered, wrap_width);

    let mut out = Vec::with_capacity(wrapped.len().saturating_add(2));

    let mut top = Line::from("");
    top.style = bubble;
    out.push(top);

    for (idx, line) in wrapped.into_iter().enumerate() {
        let prefix: Span<'static> = if idx == 0 {
            Span::from("› ").bold().dim()
        } else {
            Span::from("  ")
        };

        let mut spans = Vec::with_capacity(line.spans.len().saturating_add(1));
        spans.push(prefix);
        spans.extend(line.spans.into_iter());

        let mut line_out = Line::from(spans);
        line_out.style = line.style.patch(bubble);
        line_out.alignment = line.alignment;
        out.push(line_out);
    }

    let mut bottom = Line::from("");
    bottom.style = bubble;
    out.push(bottom);

    out
}

fn fmt_elapsed_compact(elapsed: Duration) -> String {
    let ms = elapsed.as_millis() as u64;
    if ms >= 60_000 {
        let secs = ms / 1_000;
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{minutes}m{seconds:02}s")
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

fn take_prefix_by_width(input: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut seen = 0usize;
    for ch in input.chars() {
        let w = ch.width().unwrap_or(0);
        if seen + w > width {
            break;
        }
        out.push(ch);
        seen += w;
        if seen == width {
            break;
        }
    }
    if out.is_empty() {
        out.push(' ');
    }
    out
}

fn write_spans<'a, I>(w: &mut impl io::Write, spans: I, line_style: Style) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'static>>,
{
    for span in spans {
        let style = span.style.patch(line_style);
        apply_style(w, style)?;
        let content = span.content.as_ref();
        if !content.contains(['\n', '\r']) {
            queue!(w, Print(content))?;
            continue;
        }

        let mut start = 0usize;
        for (idx, ch) in content.char_indices() {
            if ch == '\n' || ch == '\r' {
                if start < idx {
                    queue!(w, Print(&content[start..idx]))?;
                }
                start = idx.saturating_add(ch.len_utf8());
            }
        }
        if start < content.len() {
            queue!(w, Print(&content[start..]))?;
        }
    }
    queue!(
        w,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset)
    )?;
    Ok(())
}

fn apply_style(w: &mut impl io::Write, style: Style) -> io::Result<()> {
    queue!(
        w,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset)
    )?;

    if let Some(color) = style.fg {
        queue!(w, SetForegroundColor(to_crossterm_color(color)))?;
    }
    if let Some(color) = style.bg {
        queue!(w, SetBackgroundColor(to_crossterm_color(color)))?;
    }

    let modifiers = style.add_modifier - style.sub_modifier;
    if modifiers.contains(Modifier::BOLD) {
        queue!(w, SetAttribute(Attribute::Bold))?;
    }
    if modifiers.contains(Modifier::DIM) {
        queue!(w, SetAttribute(Attribute::Dim))?;
    }
    if modifiers.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(Attribute::Italic))?;
    }
    if modifiers.contains(Modifier::UNDERLINED) {
        queue!(w, SetAttribute(Attribute::Underlined))?;
    }
    if modifiers.contains(Modifier::REVERSED) {
        queue!(w, SetAttribute(Attribute::Reverse))?;
    }

    Ok(())
}

fn to_crossterm_color(color: Color) -> CColor {
    match color {
        Color::Reset => CColor::Reset,
        Color::Black => CColor::Black,
        Color::Red => CColor::DarkRed,
        Color::Green => CColor::DarkGreen,
        Color::Yellow => CColor::DarkYellow,
        Color::Blue => CColor::DarkBlue,
        Color::Magenta => CColor::DarkMagenta,
        Color::Cyan => CColor::DarkCyan,
        Color::Gray => CColor::Grey,
        Color::DarkGray => CColor::DarkGrey,
        Color::LightRed => CColor::Red,
        Color::LightGreen => CColor::Green,
        Color::LightYellow => CColor::Yellow,
        Color::LightBlue => CColor::Blue,
        Color::LightMagenta => CColor::Magenta,
        Color::LightCyan => CColor::Cyan,
        Color::White => CColor::White,
        Color::Rgb(r, g, b) => CColor::Rgb { r, g, b },
        Color::Indexed(i) => CColor::AnsiValue(i),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn user_message_marker_roundtrip() {
        let content = "Hello\nWorld";
        let payload = serde_json::to_string(content).unwrap();
        let line = Line::from(Span::from(format!("{USER_MESSAGE_MARKER_PREFIX}{payload}")));
        assert_eq!(parse_user_message_marker(&line), Some(content.to_string()));
    }

    #[test]
    fn divider_marker_parses_ms_and_optional_tokens() {
        let line = Line::from(Span::from(format!("{DIVIDER_MARKER_PREFIX}123,456")));
        let marker = parse_divider_marker(&line).unwrap();
        assert_eq!(marker.ms, 123);
        assert_eq!(marker.output_tokens, Some(456));

        let line = Line::from(Span::from(format!("{DIVIDER_MARKER_PREFIX}789")));
        let marker = parse_divider_marker(&line).unwrap();
        assert_eq!(marker.ms, 789);
        assert_eq!(marker.output_tokens, None);
    }

    #[test]
    fn user_message_block_has_padding_prefixes_and_composer_style() {
        let bubble = crate::style::composer_background_style();
        let block = render_user_message_block("hello world", 8);

        assert!(block.len() >= 4, "expected wrapped block, got {block:?}");

        // top + bottom padding
        assert_eq!(plain(&block[0]), "");
        assert_eq!(plain(block.last().unwrap()), "");
        assert_eq!(block[0].style.bg, bubble.bg);
        assert_eq!(block.last().unwrap().style.bg, bubble.bg);

        // first content line uses arrow prefix, subsequent lines use spaces
        assert_eq!(block[1].spans[0].content.as_ref(), "› ");
        for line in block.iter().skip(2).take(block.len().saturating_sub(3)) {
            assert_eq!(line.spans[0].content.as_ref(), "  ");
        }

        for line in &block {
            assert_eq!(line.style.bg, bubble.bg);
        }
    }

    #[test]
    fn divider_line_is_exact_width_and_dimmed() {
        let line = render_divider_line(Duration::from_millis(1234), Some(99), 20);
        assert_eq!(plain(&line).width(), 20);
        let modifiers = line.style.add_modifier - line.style.sub_modifier;
        assert!(modifiers.contains(Modifier::DIM));
    }

    #[test]
    fn take_prefix_by_width_never_returns_empty_for_nonzero_width() {
        assert_eq!(take_prefix_by_width("你", 0), "");
        assert_eq!(take_prefix_by_width("你", 1), " ");
        assert_eq!(take_prefix_by_width("你", 2), "你");
    }
}
