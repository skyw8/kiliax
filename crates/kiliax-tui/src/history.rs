use std::fmt;
use std::io;
use std::io::Write;
use std::time::Duration;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Attribute, Color as CColor, Print, SetAttribute, SetForegroundColor};
use crossterm::style::SetBackgroundColor;
use crossterm::terminal::{Clear, ClearType};
use crossterm::Command;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

pub const DIVIDER_MARKER_PREFIX: &str = "\u{001f}kiliax_divider:";

pub fn insert_history_lines(
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

    let mut out = io::stdout();

    let expanded = expand_special_lines(lines, full_size.width as usize);
    let wrapped = crate::wrap::wrap_lines(&expanded, full_size.width as usize);
    let wrapped_rows = wrapped.len().min(u16::MAX as usize) as u16;

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
                queue!(out, Print("\x1bM"))?;
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

    let cursor_row = viewport_top.saturating_sub(1);
    queue!(out, SetScrollRegion(1..viewport_top), MoveTo(0, cursor_row))?;

    for line in wrapped {
        queue!(out, Print("\r\n"))?;
        apply_style(&mut out, line.style)?;
        queue!(out, Clear(ClearType::UntilNewLine))?;
        write_spans(&mut out, line.spans.iter(), line.style)?;
    }

    queue!(out, ResetScrollRegion)?;
    out.flush()?;
    Ok(())
}

fn expand_special_lines(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(ms) = parse_divider_marker(line) {
            out.push(render_divider_line(Duration::from_millis(ms), width));
            continue;
        }
        out.push(line.clone());
    }
    out
}

fn parse_divider_marker(line: &Line<'static>) -> Option<u64> {
    let text = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    text.strip_prefix(DIVIDER_MARKER_PREFIX)
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn render_divider_line(elapsed: Duration, width: usize) -> Line<'static> {
    let label = format!("Worked for {}", fmt_elapsed_compact(elapsed));
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

fn write_spans<'a, I>(w: &mut impl Write, spans: I, line_style: Style) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'static>>,
{
    for span in spans {
        let style = span.style.patch(line_style);
        apply_style(w, style)?;
        queue!(w, Print(span.content.as_ref()))?;
    }
    queue!(
        w,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset)
    )?;
    Ok(())
}

fn apply_style(w: &mut impl Write, style: Style) -> io::Result<()> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "SetScrollRegion requires ANSI support",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "ResetScrollRegion requires ANSI support",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}
