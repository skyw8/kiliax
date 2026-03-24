use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Span;

pub fn composer_background_style() -> Style {
    Style::default()
}

pub fn accent_colors() -> (Color, Color) {
    match terminal_theme_hint() {
        TerminalTheme::Light => (Color::Rgb(0, 92, 255), Color::Rgb(140, 0, 255)),
        TerminalTheme::Dark | TerminalTheme::Unknown => {
            (Color::Rgb(97, 175, 239), Color::Rgb(198, 120, 221))
        }
    }
}

pub fn composer_prompt_spans() -> [Span<'static>; 2] {
    let (blue, purple) = accent_colors();

    [
        Span::styled("›", Style::default().fg(blue).bold()),
        Span::styled("›", Style::default().fg(purple).bold()),
    ]
}

pub fn model_picker_providers_panel_style() -> Style {
    Style::default()
}

pub fn slash_popup_item_style() -> Style {
    match terminal_theme_hint() {
        TerminalTheme::Light => Style::default().fg(Color::DarkGray),
        TerminalTheme::Dark | TerminalTheme::Unknown => Style::default().fg(Color::Gray),
    }
}

pub fn slash_popup_highlight_style() -> Style {
    let (blue, _purple) = accent_colors();
    Style::default().fg(blue).bold()
}

pub fn model_picker_highlight_style() -> Style {
    let (_blue, purple) = accent_colors();
    Style::default().fg(purple).bold()
}

pub fn diff_insert_style() -> Style {
    match terminal_theme_hint() {
        TerminalTheme::Light => Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(218, 251, 225)),
        TerminalTheme::Dark | TerminalTheme::Unknown => {
            Style::default().bg(Color::Rgb(33, 58, 43))
        }
    }
}

pub fn diff_delete_style() -> Style {
    match terminal_theme_hint() {
        TerminalTheme::Light => Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(255, 235, 233)),
        TerminalTheme::Dark | TerminalTheme::Unknown => {
            Style::default().bg(Color::Rgb(74, 34, 29))
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TerminalTheme {
    Light,
    Dark,
    Unknown,
}

fn terminal_theme_hint() -> TerminalTheme {
    let Ok(val) = std::env::var("COLORFGBG") else {
        return TerminalTheme::Unknown;
    };
    let bg = val
        .split(';')
        .filter_map(|part| part.trim().parse::<u8>().ok())
        .last();
    let Some(bg) = bg else {
        return TerminalTheme::Unknown;
    };

    if bg >= 7 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}
