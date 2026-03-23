use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::model_picker::ModelPickerFocus;

const LIVE_PREFIX_COLS: u16 = 2;
const COMPOSER_PAD_TOP: u16 = 1;
const COMPOSER_PAD_BOTTOM: u16 = 1;
const COMPOSER_PAD_RIGHT: u16 = 1;
const MIN_COMPOSER_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 2;
const STATUS_HEIGHT: u16 = 1;
const SLASH_POPUP_MAX_ITEMS: usize = 6;
const MODEL_PICKER_MIN_HEIGHT: u16 = 18;

pub fn desired_viewport_height(app: &App, width: u16) -> u16 {
    if app.model_picker().is_some() {
        return MODEL_PICKER_MIN_HEIGHT;
    }

    let status_height = if app.running { STATUS_HEIGHT } else { 0 };
    desired_composer_height(app, width)
        .saturating_add(status_height)
        .saturating_add(FOOTER_HEIGHT)
}

pub fn draw(frame: &mut Frame, app: &mut App, composer_style: Style) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if app.model_picker().is_some() {
        draw_model_picker(frame, app, area);
        frame.render_widget(Block::default().style(composer_style), area);
        return;
    }

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

fn draw_model_picker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = app.model_picker() else {
        return;
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Model")
        .border_style(Style::default().dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let [search_area, main_area, help_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let search_prefix = "Search: ";
    let search_line = Line::from(vec![
        Span::from(search_prefix).dim(),
        Span::from(picker.search_text().to_string()),
    ]);
    frame.render_widget(Paragraph::new(search_line), search_area);

    let cursor_col = search_prefix.len().saturating_add(display_width(
        picker.search_text(),
        picker.search_cursor(),
    ));
    frame.set_cursor_position((
        search_area.x.saturating_add(cursor_col as u16),
        search_area.y,
    ));

    let [providers_area, models_area] = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(66),
    ])
    .areas(main_area);

    draw_model_picker_providers(frame, picker, providers_area);
    draw_model_picker_models(frame, app, picker, models_area);

    let mut help = vec![
        Span::from("↑/↓").dim(),
        Span::from(" navigate  ").dim(),
        Span::from("Tab").dim(),
        Span::from(" switch  ").dim(),
        Span::from("Enter").dim(),
        Span::from(" select  ").dim(),
        Span::from("Esc").dim(),
        Span::from(" back").dim(),
    ];
    if let Some(model) = picker.selected_model() {
        help.push(Span::from("  ·  ").dim());
        help.push(Span::from(model.id.clone()).dim());
    }
    frame.render_widget(Paragraph::new(Line::from(help)), help_area);
}

fn draw_model_picker_providers(
    frame: &mut Frame,
    picker: &crate::model_picker::ModelPicker,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }

    let focused = picker.focus() == ModelPickerFocus::Providers;
    let mut block = Block::default().borders(Borders::ALL).title("Providers");
    block = if focused {
        block.border_style(Style::default().fg(Color::Cyan).bold())
    } else {
        block.border_style(Style::default().dim())
    };

    let items = picker
        .filtered_providers()
        .iter()
        .filter_map(|&idx| picker.providers().get(idx))
        .map(|p| {
            let count = p.models.len();
            ListItem::new(Line::from(vec![
                Span::from(p.name.clone()),
                Span::from(format!(" ({count})")).dim(),
            ]))
        })
        .collect::<Vec<_>>();

    let selected = if items.is_empty() {
        None
    } else {
        Some(picker.provider_cursor())
    };
    let visible = area.height.saturating_sub(2) as usize;
    let offset = selected
        .map(|s| list_offset(s, items.len(), visible))
        .unwrap_or(0);
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(offset);

    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(Style::default().bg(Color::Rgb(70, 70, 70)));

    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_model_picker_models(
    frame: &mut Frame,
    app: &App,
    picker: &crate::model_picker::ModelPicker,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }

    let focused = picker.focus() == ModelPickerFocus::Models;
    let mut block = Block::default().borders(Borders::ALL).title("Models");
    block = if focused {
        block.border_style(Style::default().fg(Color::Cyan).bold())
    } else {
        block.border_style(Style::default().dim())
    };

    let current_model_id = app.model_id();
    let provider_idx = picker.selected_provider_index();
    let provider_models = provider_idx
        .and_then(|idx| picker.providers().get(idx))
        .map(|p| p.models.as_slice())
        .unwrap_or(&[]);

    let items = picker
        .filtered_models()
        .iter()
        .filter_map(|&idx| provider_models.get(idx))
        .map(|m| {
            let is_current = m.id == current_model_id;
            let mut spans = Vec::new();
            if is_current {
                spans.push(Span::from("• ").fg(Color::LightGreen));
            } else {
                spans.push(Span::from("  "));
            }
            spans.push(Span::from(m.display.clone()));
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();

    let selected = if items.is_empty() {
        None
    } else {
        Some(picker.model_cursor())
    };
    let visible = area.height.saturating_sub(2) as usize;
    let offset = selected
        .map(|s| list_offset(s, items.len(), visible))
        .unwrap_or(0);
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(offset);

    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(Style::default().bg(Color::Rgb(70, 70, 70)));

    frame.render_stateful_widget(list, area, &mut state);
}

fn display_width(text: &str, cursor_chars: usize) -> usize {
    let mut width = 0usize;
    let mut seen = 0usize;
    for ch in text.chars() {
        if seen >= cursor_chars {
            break;
        }
        width = width.saturating_add(UnicodeWidthChar::width(ch).unwrap_or(0));
        seen += 1;
    }
    width
}

fn list_offset(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        return 0;
    }
    let half = visible / 2;
    let mut offset = selected.saturating_sub(half);
    let max_offset = len.saturating_sub(visible);
    if offset > max_offset {
        offset = max_offset;
    }
    offset
}

fn desired_composer_height(app: &App, width: u16) -> u16 {
    let text_width = inner_text_width(width);
    let wrapped = wrap_input(app.input.text(), text_width as usize);
    let line_count = wrapped.len().min(u16::MAX as usize) as u16;
    let mut height =
        MIN_COMPOSER_HEIGHT.max(line_count.saturating_add(COMPOSER_PAD_TOP + COMPOSER_PAD_BOTTOM));
    let popup_height = app.slash_popup().desired_height(SLASH_POPUP_MAX_ITEMS);
    if popup_height > 0 {
        // Extra gap so the popup doesn't visually collide with the input.
        height = height.saturating_add(popup_height.saturating_add(1));
    }
    height
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
        let thinking_style = Style::default().dim().italic();
        spans.push(Span::from(" · ").dim());
        spans.push(Span::styled(
            format!("thinking (step {step})"),
            thinking_style,
        ));
        spans.push(Span::from(" ").dim());
        spans.push(Span::styled(fmt_duration_compact(elapsed), thinking_style));
        spans.push(Span::from(" · ").dim());
        spans.push(Span::from(format!("{} tok", app.step_output_tokens())).dim());
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_composer(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let popup_height = app.slash_popup().desired_height(SLASH_POPUP_MAX_ITEMS);
    let popup_and_gap = if popup_height > 0 {
        popup_height.saturating_add(1)
    } else {
        0
    };

    if popup_height > 0 {
        draw_slash_popup(frame, app, area, popup_height);
    }

    let textarea = textarea_rect(area, popup_and_gap);
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

fn textarea_rect(area: Rect, y_offset: u16) -> Rect {
    let x = area.x.saturating_add(LIVE_PREFIX_COLS);
    let y = area.y.saturating_add(COMPOSER_PAD_TOP).saturating_add(y_offset);
    let width = area
        .width
        .saturating_sub(LIVE_PREFIX_COLS + COMPOSER_PAD_RIGHT);
    let height = area
        .height
        .saturating_sub(COMPOSER_PAD_TOP + COMPOSER_PAD_BOTTOM + y_offset);
    Rect::new(x, y, width, height)
}

fn draw_slash_popup(frame: &mut Frame, app: &App, area: Rect, popup_height: u16) {
    if popup_height < 3 || area.width <= LIVE_PREFIX_COLS {
        return;
    }

    let x = area.x.saturating_add(LIVE_PREFIX_COLS);
    let y = area.y.saturating_add(COMPOSER_PAD_TOP);
    let width = area
        .width
        .saturating_sub(LIVE_PREFIX_COLS + COMPOSER_PAD_RIGHT);
    let rect = Rect::new(x, y, width, popup_height.min(area.height));
    if rect.is_empty() {
        return;
    }

    let popup = app.slash_popup();
    let items = popup
        .items()
        .iter()
        .take(SLASH_POPUP_MAX_ITEMS)
        .map(|cmd| {
            let mut spans = vec![
                Span::styled(
                    format!("/{}", cmd.command()),
                    Style::default().fg(Color::Cyan).bold(),
                ),
            ];
            if !cmd.aliases().is_empty() {
                spans.push(Span::from(" ").dim());
                spans.push(Span::from(format!("(/{})", cmd.aliases().join(", "))).dim());
            }
            spans.push(Span::from("  ").dim());
            spans.push(Span::from(cmd.description()).dim());
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Commands")
        .border_style(Style::default().dim());

    let mut state = ListState::default();
    state.select(Some(popup.selected_index()));
    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(Style::default().bg(Color::Rgb(70, 70, 70)));

    frame.render_widget(Clear, rect);
    frame.render_stateful_widget(list, rect, &mut state);
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
        Span::from("agent: ").dim(),
        Span::from(app.agent_name().to_string()).cyan(),
        Span::from("  ").dim(),
        Span::from("model: ").dim(),
        Span::from(app.model_id().to_string()).cyan(),
    ]);
    let keys = Line::from(vec![
        Span::styled(indent, Style::default()),
        status,
        Span::styled("  ↑/↓ history", Style::default().fg(Color::DarkGray)),
        Span::styled("  / commands", Style::default().fg(Color::DarkGray)),
        Span::styled("  Tab complete", Style::default().fg(Color::DarkGray)),
        Span::styled("  Ctrl+C clear", Style::default().fg(Color::DarkGray)),
        Span::styled("  Esc stop/quit", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(Text::from(vec![model, keys])), area);
}
