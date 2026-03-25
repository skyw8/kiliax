use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::model_picker::ModelPickerFocus;

const LIVE_PREFIX_COLS: u16 = 2;
const IMAGE_PLACEHOLDER_PREFIX: &str = "[img#";
const COMPOSER_PAD_TOP: u16 = 1;
const COMPOSER_PAD_BOTTOM: u16 = 1;
const COMPOSER_PAD_RIGHT: u16 = 1;
const MIN_COMPOSER_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 1;
const STATUS_HEIGHT: u16 = 1;
const SLASH_POPUP_MAX_ITEMS: usize = 6;
const QUEUE_MAX_ITEMS: usize = 4;
const MODEL_PICKER_MIN_HEIGHT: u16 = 18;

pub fn desired_viewport_height(app: &App, width: u16) -> u16 {
    if app.model_picker().is_some() {
        return MODEL_PICKER_MIN_HEIGHT;
    }

    let status_height = if app.running { STATUS_HEIGHT } else { 0 };
    let footer_height = desired_footer_height(app);
    desired_composer_height(app, width)
        .saturating_add(status_height)
        .saturating_add(footer_height)
}

pub fn draw(frame: &mut Frame, app: &mut App, composer_style: Style) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if app.model_picker().is_some() {
        frame.render_widget(Block::default().style(composer_style), area);
        draw_model_picker(frame, app, area);
        return;
    }

    let footer_height = desired_footer_height(app).min(area.height.saturating_sub(1));
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

    if area.is_empty() {
        return;
    }

    let [search_area, _gap_area, main_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);

    let search_prefix = "Search: ";
    let search_line = Line::from(vec![
        Span::styled(search_prefix, Style::default().fg(Color::White)),
        Span::from(picker.search_text().to_string()),
    ]);
    frame.render_widget(Paragraph::new(search_line), search_area);

    let cursor_col = search_prefix
        .len()
        .saturating_add(display_width(picker.search_text(), picker.search_cursor()));
    frame.set_cursor_position((
        search_area.x.saturating_add(cursor_col as u16),
        search_area.y,
    ));

    let [providers_area, models_area] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)])
            .areas(main_area);

    frame.render_widget(
        Block::default().style(crate::style::model_picker_providers_panel_style()),
        providers_area,
    );

    let [providers_header, providers_list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(providers_area);
    let [models_header, models_list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(models_area);

    let header_style = Style::default().fg(Color::LightBlue);
    let focused_style = header_style.bold();
    let unfocused_style = header_style.dim();

    let providers_focused = picker.focus() == ModelPickerFocus::Providers;
    let models_focused = picker.focus() == ModelPickerFocus::Models;

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Providers",
            if providers_focused {
                focused_style
            } else {
                unfocused_style
            },
        ))),
        providers_header,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Models",
            if models_focused {
                focused_style
            } else {
                unfocused_style
            },
        ))),
        models_header,
    );

    draw_model_picker_providers(frame, picker, providers_list);
    draw_model_picker_models(frame, app, picker, models_list);
}

fn draw_model_picker_providers(
    frame: &mut Frame,
    picker: &crate::model_picker::ModelPicker,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }

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
    let visible = area.height as usize;
    let offset = selected
        .map(|s| list_offset(s, items.len(), visible))
        .unwrap_or(0);
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(offset);

    let list = List::new(items)
        .highlight_symbol("› ")
        .highlight_style(crate::style::model_picker_highlight_style());

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
    let visible = area.height as usize;
    let offset = selected
        .map(|s| list_offset(s, items.len(), visible))
        .unwrap_or(0);
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(offset);

    let list = List::new(items)
        .highlight_symbol("› ")
        .highlight_style(crate::style::model_picker_highlight_style());

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
    let queued_count = if text_width == 0 {
        0usize
    } else {
        let len = app.queued_len();
        if len == 0 {
            0
        } else {
            1 + len.min(QUEUE_MAX_ITEMS) + usize::from(len > QUEUE_MAX_ITEMS)
        }
    };
    let line_count = wrapped
        .len()
        .saturating_add(queued_count)
        .min(u16::MAX as usize) as u16;
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
    let mut spans = vec![
        Span::styled(indent, Style::default()),
        Span::from("working ").dim(),
    ];

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

    let textarea = textarea_rect(area);
    if textarea.is_empty() {
        return;
    }

    let text_width = textarea.width as usize;
    let queue = queued_submission_preview_lines(app, text_width);

    let mut remaining = textarea;
    let available_non_input = remaining.height.saturating_sub(1);
    let queue_height = (queue.len().min(u16::MAX as usize) as u16).min(available_non_input);

    let mut queue_area = Rect::ZERO;
    if queue_height > 0 {
        let [q, rest] = Layout::vertical([Constraint::Length(queue_height), Constraint::Min(1)])
            .areas(remaining);
        queue_area = q;
        remaining = rest;
    }

    let textarea = remaining;

    if !queue_area.is_empty() {
        frame.render_widget(Paragraph::new(Text::from(queue)), queue_area);
    }

    let prompt = Line::from(crate::style::composer_prompt_spans().to_vec());
    let prompt_rect = Rect::new(area.x, textarea.y, LIVE_PREFIX_COLS.min(area.width), 1);
    frame.render_widget(Paragraph::new(prompt), prompt_rect);

    let (lines, cursor_row, cursor_col) =
        input_display_lines(app.input.text(), app.input.cursor(), text_width);
    frame.render_widget(Paragraph::new(Text::from(lines)), textarea);

    let cursor_x = textarea.x.saturating_add(cursor_col as u16);
    let cursor_y = textarea.y.saturating_add(cursor_row as u16);
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn queued_submission_preview_lines(app: &App, max_width: usize) -> Vec<Line<'static>> {
    if max_width == 0 {
        return Vec::new();
    }

    let items = app.queued_submissions();
    if items.is_empty() {
        return Vec::new();
    }

    let header_style = Style::default().fg(Color::LightYellow).bold();
    let mut out = vec![Line::from(vec![
        Span::from("• ").dim(),
        Span::styled("queue", header_style),
        Span::from(" ").dim(),
        Span::styled(format!("({})", items.len()), Style::default().dim()),
    ])];

    let prefix = "  └ ";
    let prefix_width = display_width_str(prefix);
    let detail_width = max_width.saturating_sub(prefix_width);
    let summary_style = Style::default().dim();

    for queued in items.iter().take(QUEUE_MAX_ITEMS) {
        let mut summary = queued_summary_text(queued);
        if display_width_str(&summary) > detail_width {
            summary = take_prefix_by_width(&summary, detail_width.saturating_sub(1));
            summary.push('…');
        }
        out.push(Line::from(vec![
            Span::from(prefix).dim(),
            Span::styled(summary, summary_style),
        ]));
    }

    if items.len() > QUEUE_MAX_ITEMS {
        out.push(Line::from(vec![
            Span::from(prefix).dim(),
            Span::styled(
                format!("… ({} more)", items.len().saturating_sub(QUEUE_MAX_ITEMS)),
                summary_style,
            ),
        ]));
    }

    out
}

fn queued_summary_text(queued: &crate::app::QueuedSubmission) -> String {
    let text = queued.text.trim();

    let mut line_iter = text.lines();
    let first = line_iter.next().unwrap_or("").trim();
    let has_more = line_iter.next().is_some();

    let (mut out, show_image_suffix) = if first.is_empty() {
        if queued.images.is_empty() {
            ("(empty)".to_string(), false)
        } else {
            (format!("(images ×{})", queued.images.len()), false)
        }
    } else {
        (first.to_string(), !queued.images.is_empty())
    };

    if has_more {
        out.push_str(" …");
    }
    if show_image_suffix {
        out.push_str(&format!(" (+{} img)", queued.images.len()));
    }
    out
}

fn display_width_str(s: &str) -> usize {
    s.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn take_prefix_by_width(input: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut seen = 0usize;
    for ch in input.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if seen + w > width {
            break;
        }
        out.push(ch);
        seen += w;
        if seen == width {
            break;
        }
    }
    out
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

fn draw_slash_popup(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width <= LIVE_PREFIX_COLS {
        return;
    }

    let x = area.x.saturating_add(LIVE_PREFIX_COLS);
    let width = area
        .width
        .saturating_sub(LIVE_PREFIX_COLS + COMPOSER_PAD_RIGHT);
    let rect = Rect::new(x, area.y, width, area.height);
    if rect.is_empty() || rect.height == 0 {
        return;
    }

    let popup = app.slash_popup();
    let items = popup
        .items()
        .iter()
        .take(SLASH_POPUP_MAX_ITEMS)
        .map(|cmd| {
            let mut text = format!("/{}", cmd.command());
            if !cmd.aliases().is_empty() {
                text.push_str(&format!(" (/{})", cmd.aliases().join(", ")));
            }
            text.push_str("  ");
            text.push_str(cmd.description());
            ListItem::new(Line::from(text)).style(crate::style::slash_popup_item_style())
        })
        .collect::<Vec<_>>();

    let mut state = ListState::default();
    state.select(Some(popup.selected_index()));
    let list = List::new(items)
        .highlight_symbol("› ")
        .highlight_style(crate::style::slash_popup_highlight_style());

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
        .map(styled_input_line)
        .collect::<Vec<_>>();
    (lines, cursor_row, cursor_col)
}

fn styled_input_line(line: String) -> Line<'static> {
    let token_style = Style::default().fg(Color::LightBlue).bold();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut last = 0usize;
    let mut search = 0usize;
    while let Some(rel) = line[search..].find(IMAGE_PLACEHOLDER_PREFIX) {
        let start = search.saturating_add(rel);
        if let Some(end) = parse_image_placeholder_end(&line, start) {
            if start > last {
                spans.push(Span::raw(line[last..start].to_string()));
            }
            spans.push(Span::styled(line[start..end].to_string(), token_style));
            last = end;
            search = end;
            continue;
        }
        search = start.saturating_add(1);
    }

    if last < line.len() {
        spans.push(Span::raw(line[last..].to_string()));
    }

    if spans.is_empty() {
        Line::from(Span::raw(line))
    } else {
        Line::from(spans)
    }
}

fn parse_image_placeholder_end(text: &str, start: usize) -> Option<usize> {
    let rest = text.get(start..)?;
    if !rest.starts_with(IMAGE_PLACEHOLDER_PREFIX) {
        return None;
    }
    let close_rel = rest.find(']')?;
    let end = start.saturating_add(close_rel).saturating_add(1);
    if end <= start.saturating_add(IMAGE_PLACEHOLDER_PREFIX.len()) {
        return None;
    }
    let digits = text.get(start + IMAGE_PLACEHOLDER_PREFIX.len()..end.saturating_sub(1))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(end)
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

    if app.slash_popup().visible() {
        draw_slash_popup(frame, app, area);
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
        Span::styled(status, Style::default().fg(Color::Rgb(255, 165, 0)))
    } else if status.starts_with("running") {
        Span::styled(
            "running".to_string(),
            Style::default().fg(Color::LightYellow),
        )
    } else {
        Span::styled(status, Style::default().fg(Color::DarkGray))
    };

    let indent = " ".repeat(LIVE_PREFIX_COLS as usize);
    let mut spans = vec![Span::styled(indent, Style::default()), status];

    spans.push(Span::from(" · ").dim());
    spans.push(Span::styled(
        app.agent_name().to_string(),
        Style::default().fg(Color::LightBlue),
    ));
    spans.push(Span::from(" · ").dim());
    spans.push(Span::styled(
        model_id_without_provider(app.model_id()).to_string(),
        Style::default().fg(Color::LightMagenta),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn desired_footer_height(app: &App) -> u16 {
    let popup_height = app.slash_popup().desired_height(SLASH_POPUP_MAX_ITEMS);
    if popup_height > 0 {
        popup_height
    } else {
        FOOTER_HEIGHT
    }
}

fn model_id_without_provider(model_id: &str) -> &str {
    model_id
        .split_once('/')
        .map(|(_, model)| model)
        .unwrap_or(model_id)
}
