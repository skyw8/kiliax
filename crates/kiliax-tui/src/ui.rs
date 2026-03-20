use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ChatRole};
use crate::markdown::render_markdown_lines;
use crate::wrap::wrap_lines;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [chat_area, input_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .areas(area);

    let chat_inner_width = chat_area.width.saturating_sub(2) as usize;
    let chat_inner_height = chat_area.height.saturating_sub(2) as usize;

    let wrapped = render_chat_tail(app, chat_inner_width, chat_inner_height);

    let chat_title = match app.status.as_deref() {
        Some(status) if !status.is_empty() => format!("Chat ({status})"),
        _ => "Chat".to_string(),
    };

    let chat = Paragraph::new(Text::from(wrapped))
        .block(Block::default().borders(Borders::ALL).title(chat_title));
    frame.render_widget(chat, chat_area);

    let prompt = Span::styled("> ", Style::default().fg(Color::Green).bold());
    let prompt_width = prompt.content.width() as u16;
    let input_inner_width = input_area
        .width
        .saturating_sub(2)
        .saturating_sub(prompt_width) as usize;

    let (visible, cursor_x) = input_view(app.input.text(), app.input.cursor(), input_inner_width);
    let input = Line::from(vec![prompt, Span::raw(visible)]);
    let input_title = if app.running {
        "Input (running…)"
    } else {
        "Input (Enter to send)"
    };
    let input = Paragraph::new(Text::from(input))
        .block(Block::default().borders(Borders::ALL).title(input_title));
    frame.render_widget(input, input_area);

    let cursor_x = input_area.x + 1 + prompt_width + cursor_x as u16;
    let cursor_y = input_area.y + 1;
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn render_chat_tail(app: &mut App, width: usize, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return vec![Line::from("")];
    }

    let mut out_rev: Vec<Line<'static>> = Vec::new();
    let mut has_newer = false;

    for entry in app.transcript.iter_mut().rev() {
        let (label, color) = match entry.role {
            ChatRole::User => ("User", Color::Green),
            ChatRole::Assistant => ("Assistant", Color::Cyan),
            ChatRole::Tool => ("Tool", Color::Yellow),
            ChatRole::Info => ("Info", Color::Magenta),
        };

        let header = Line::from(vec![Span::styled(
            format!("{label}:"),
            Style::default().fg(color).bold(),
        )]);

        let body = entry
            .rendered
            .get_or_insert_with(|| render_markdown_lines(&entry.content));

        let mut wrapped = wrap_lines(std::slice::from_ref(&header), width);
        wrapped.extend(wrap_lines(body, width));
        if has_newer {
            wrapped.push(Line::from(""));
        }

        for line in wrapped.into_iter().rev() {
            out_rev.push(line);
            if out_rev.len() >= height {
                break;
            }
        }

        has_newer = true;
        if out_rev.len() >= height {
            break;
        }
    }

    if out_rev.is_empty() {
        return vec![Line::from("")];
    }

    out_rev.reverse();
    out_rev
}

fn input_view(text: &str, cursor: usize, max_width: usize) -> (String, usize) {
    if max_width == 0 {
        return (String::new(), 0);
    }
    if text.width() <= max_width {
        let cursor_x = text
            .chars()
            .take(cursor)
            .collect::<String>()
            .width()
            .min(max_width);
        return (text.to_string(), cursor_x);
    }

    let chars: Vec<char> = text.chars().collect();
    let widths: Vec<usize> = chars
        .iter()
        .map(|c| unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0))
        .collect();

    let cursor = cursor.min(chars.len());
    let mut start = 0usize;
    let mut cursor_x: usize = widths.iter().take(cursor).sum();
    while start < cursor && cursor_x > max_width {
        cursor_x = cursor_x.saturating_sub(widths[start]);
        start += 1;
    }

    let mut end = start;
    let mut used = 0usize;
    while end < chars.len() && used + widths[end] <= max_width {
        used += widths[end];
        end += 1;
    }

    let visible: String = chars[start..end].iter().collect();
    (visible, cursor_x.min(max_width))
}
