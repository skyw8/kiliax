use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ChatRole};
use crate::markdown::render_markdown_lines;
use crate::wrap::wrap_lines;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [chat_area, input_area, info_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .areas(area);

    let chat_inner_width = chat_area.width as usize;
    let chat_inner_height = chat_area.height as usize;
    let wrapped = render_chat_tail(app, chat_inner_width, chat_inner_height);
    frame.render_widget(Clear, chat_area);
    let chat = Paragraph::new(Text::from(wrapped));
    frame.render_widget(chat, chat_area);

    let prompt = Span::styled("> ", Style::default().fg(Color::Green).bold());
    let prompt_width = prompt.content.width() as u16;
    let input_inner_width = input_area
        .width
        .saturating_sub(2)
        .saturating_sub(prompt_width) as usize;

    let (visible, cursor_x) = input_view(app.input.text(), app.input.cursor(), input_inner_width);
    let input = Line::from(vec![prompt, Span::raw(visible)]);
    let input = Paragraph::new(Text::from(input))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(input, input_area);

    let cursor_x = input_area.x + 1 + prompt_width + cursor_x as u16;
    let cursor_y = input_area.y + 1;
    frame.set_cursor_position((cursor_x, cursor_y));

    let info = info_text(app);
    frame.render_widget(Clear, info_area);
    let info = Paragraph::new(info).wrap(Wrap { trim: true });
    frame.render_widget(info, info_area);
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

fn info_text(app: &App) -> Text<'_> {
    use ratatui::style::Color;

    let status = app
        .status
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let status = if status.is_empty() {
        Span::styled("idle".to_string(), Style::default().fg(Color::DarkGray))
    } else if status.starts_with("error") {
        Span::styled(status, Style::default().fg(Color::LightRed).bold())
    } else if status.starts_with("done") {
        Span::styled("done".to_string(), Style::default().fg(Color::LightGreen).bold())
    } else if status.starts_with("step ") {
        Span::styled(status, Style::default().fg(Color::LightYellow))
    } else if status.starts_with("running") {
        Span::styled("running".to_string(), Style::default().fg(Color::LightYellow))
    } else {
        Span::styled(status, Style::default().fg(Color::DarkGray))
    };

    let header = Line::from(vec![
        Span::styled("Session ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.session_id(),
            Style::default().fg(Color::Magenta).bold(),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(app.agent_name(), Style::default().fg(Color::Cyan)),
        Span::styled("  ", Style::default()),
        Span::styled(app.model_id(), Style::default().fg(Color::DarkGray)),
    ]);

    let keys = Line::from(vec![
        status,
        Span::styled("  ↑/↓ history", Style::default().fg(Color::DarkGray)),
        Span::styled("  Ctrl+C clear", Style::default().fg(Color::DarkGray)),
        Span::styled("  Ctrl+D/Esc quit", Style::default().fg(Color::DarkGray)),
    ]);

    Text::from(vec![header, keys])
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
