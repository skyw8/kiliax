use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::App;

const LIVE_PREFIX_COLS: u16 = 2;
const COMPOSER_PAD_TOP: u16 = 1;
const COMPOSER_PAD_BOTTOM: u16 = 1;
const COMPOSER_PAD_RIGHT: u16 = 1;
const MIN_COMPOSER_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 2;
const STATUS_HEIGHT: u16 = 1;

pub fn desired_viewport_height(app: &App, width: u16) -> u16 {
    let status_height = if app.running { STATUS_HEIGHT } else { 0 };
    desired_composer_height(app, width)
        .saturating_add(status_height)
        .saturating_add(FOOTER_HEIGHT)
}

pub fn draw(frame: &mut Frame, app: &mut App, composer_style: Style) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let footer_height = if area.height <= FOOTER_HEIGHT {
        0
    } else {
        FOOTER_HEIGHT.min(area.height)
    };
    let [composer_area, footer_area] = if footer_height == 0 {
        [area, Rect::ZERO]
    } else {
        Layout::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(area)
    };

    let (status_area, composer_area) = split_status_and_composer(app, composer_area);
    if !status_area.is_empty() {
        draw_status_line(frame, app, status_area);
    }
    draw_composer(frame, app, composer_area);
    if !footer_area.is_empty() {
        draw_footer(frame, app, footer_area);
    }

    frame.render_widget(Block::default().style(composer_style), composer_area);
}

fn split_status_and_composer(app: &App, area: Rect) -> (Rect, Rect) {
    if !app.running || area.height <= STATUS_HEIGHT {
        return (Rect::ZERO, area);
    }
    let [status, composer] =
        Layout::vertical([Constraint::Length(STATUS_HEIGHT), Constraint::Min(1)]).areas(area);
    (status, composer)
}

fn desired_composer_height(app: &App, width: u16) -> u16 {
    let text_width = inner_text_width(width);
    let wrapped = wrap_input(app.input.text(), text_width as usize);
    let line_count = wrapped.len().min(u16::MAX as usize) as u16;
    MIN_COMPOSER_HEIGHT.max(line_count.saturating_add(COMPOSER_PAD_TOP + COMPOSER_PAD_BOTTOM))
}

fn inner_text_width(width: u16) -> u16 {
    width.saturating_sub(LIVE_PREFIX_COLS + COMPOSER_PAD_RIGHT)
}

fn fmt_duration_compact(duration: std::time::Duration) -> String {
    let ms = duration.as_millis() as u64;
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

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let indent = " ".repeat(LIVE_PREFIX_COLS as usize);
    let mut spans = vec![Span::styled(indent, Style::default()), Span::from("working ").dim()];

    if let Some(elapsed) = app.turn_elapsed() {
        spans.push(Span::from(fmt_duration_compact(elapsed)).dim());
    } else {
        spans.push(Span::from("—").dim());
    }

    spans.push(Span::from(" · ").dim());
    spans.push(Span::from(format!("{} tok", app.turn_output_tokens())).dim());

    if let Some((tool, elapsed)) = app.active_tool_elapsed() {
        spans.push(Span::from(" · ").dim());
        let (name, rest) = match tool.split_once(' ') {
            Some((name, rest)) => (name.to_string(), rest.to_string()),
            None => (tool, String::new()),
        };
        let name_style = match name.as_str() {
            "read" | "write" | "shell" => Style::default().fg(Color::Cyan).bold(),
            _ => Style::default().bold(),
        };
        spans.push(Span::styled(name, name_style));
        if !rest.is_empty() {
            spans.push(Span::from(" "));
            spans.push(Span::from(rest).dim());
        }
        spans.push(Span::from(" ").dim());
        spans.push(Span::from(fmt_duration_compact(elapsed)).dim());
    } else if let Some((step, elapsed)) = app.step_elapsed() {
        spans.push(Span::from(" · ").dim());
        spans.push(Span::from(format!("thinking (step {step})")).dim());
        spans.push(Span::from(" ").dim());
        spans.push(Span::from(fmt_duration_compact(elapsed)).dim());
        spans.push(Span::from(" · ").dim());
        spans.push(Span::from(format!("{} tok", app.step_output_tokens())).dim());
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_composer(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let textarea = textarea_rect(area);
    if textarea.is_empty() {
        return;
    }

    let prompt = if app.running {
        Span::from("›").dim()
    } else {
        Span::from("›").bold()
    };
    let prompt_rect = Rect::new(area.x, textarea.y, LIVE_PREFIX_COLS.min(area.width), 1);
    frame.render_widget(Paragraph::new(prompt), prompt_rect);

    let text_width = textarea.width as usize;
    let (lines, cursor_row, cursor_col) =
        input_display_lines(app.input.text(), app.input.cursor(), text_width);
    frame.render_widget(Paragraph::new(Text::from(lines)), textarea);

    let cursor_x = textarea.x.saturating_add(cursor_col as u16);
    let cursor_y = textarea.y.saturating_add(cursor_row as u16);
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn textarea_rect(area: Rect) -> Rect {
    let x = area.x.saturating_add(LIVE_PREFIX_COLS);
    let y = area.y.saturating_add(COMPOSER_PAD_TOP);
    let width = area
        .width
        .saturating_sub(LIVE_PREFIX_COLS + COMPOSER_PAD_RIGHT);
    let height = area
        .height
        .saturating_sub(COMPOSER_PAD_TOP + COMPOSER_PAD_BOTTOM);
    Rect::new(x, y, width, height)
}

fn input_display_lines(
    text: &str,
    cursor: usize,
    max_width: usize,
) -> (Vec<Line<'static>>, usize, usize) {
    if max_width == 0 {
        return (vec![Line::from("")], 0, 0);
    }

    if text.is_empty() {
        let placeholder = Span::styled("Type a message…", Style::default().fg(Color::DarkGray));
        return (vec![Line::from(placeholder)], 0, 0);
    }

    let (wrapped, cursor_row, cursor_col) = wrap_with_cursor(text, cursor, max_width);
    let lines = wrapped
        .into_iter()
        .map(|s| Line::from(Span::raw(s)))
        .collect::<Vec<_>>();
    (lines, cursor_row, cursor_col)
}

fn wrap_input(text: &str, max_width: usize) -> Vec<String> {
    let (lines, _, _) = wrap_with_cursor(text, text.chars().count(), max_width);
    lines
}

fn wrap_with_cursor(text: &str, cursor: usize, max_width: usize) -> (Vec<String>, usize, usize) {
    if max_width == 0 {
        return (vec![String::new()], 0, 0);
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut col = 0usize;
    let mut row = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;

    let mut seen = 0usize;
    for ch in text.chars() {
        if seen == cursor {
            cursor_row = row;
            cursor_col = col.min(max_width);
        }

        if ch == '\n' {
            lines.push(std::mem::take(&mut current));
            row = row.saturating_add(1);
            col = 0;
            seen += 1;
            continue;
        }

        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col + w > max_width && col > 0 {
            lines.push(std::mem::take(&mut current));
            row = row.saturating_add(1);
            col = 0;
        }

        current.push(ch);
        col = col.saturating_add(w);
        seen += 1;
    }

    if seen == cursor {
        cursor_row = row;
        cursor_col = col.min(max_width);
    }

    lines.push(current);
    if lines.is_empty() {
        lines.push(String::new());
    }

    (lines, cursor_row, cursor_col)
}

fn draw_footer(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let status = app.status.as_deref().unwrap_or("").trim().to_string();
    let status = if status.is_empty() {
        Span::styled("idle".to_string(), Style::default().fg(Color::DarkGray))
    } else if status.starts_with("error") {
        Span::styled(status, Style::default().fg(Color::LightRed).bold())
    } else if status.starts_with("done") {
        Span::styled(
            "done".to_string(),
            Style::default().fg(Color::LightGreen).bold(),
        )
    } else if status.starts_with("step ") {
        Span::styled(status, Style::default().fg(Color::LightYellow))
    } else if status.starts_with("running") {
        Span::styled(
            "running".to_string(),
            Style::default().fg(Color::LightYellow),
        )
    } else {
        Span::styled(status, Style::default().fg(Color::DarkGray))
    };

    let indent = " ".repeat(LIVE_PREFIX_COLS as usize);
    let model = Line::from(vec![
        Span::styled(indent.clone(), Style::default()),
        Span::from("model: ").dim(),
        Span::from(app.model_id().to_string()).cyan(),
    ]);
    let keys = Line::from(vec![
        Span::styled(indent, Style::default()),
        status,
        Span::styled("  ↑/↓ history", Style::default().fg(Color::DarkGray)),
        Span::styled("  Ctrl+C clear", Style::default().fg(Color::DarkGray)),
        Span::styled("  Esc stop/quit", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(Text::from(vec![model, keys])), area);
}
