use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Span;

pub fn composer_background_style() -> Style {
    Style::default()
}

pub fn primary_text_style() -> Style {
    Style::default().fg(Color::White)
}

pub fn picker_disabled_style() -> Style {
    match terminal_theme_hint() {
        TerminalTheme::Light => Style::default().fg(Color::DarkGray),
        TerminalTheme::Dark | TerminalTheme::Unknown => Style::default().fg(Color::Gray),
    }
}

pub fn accent_colors() -> (Color, Color) {
    match terminal_theme_hint() {
        TerminalTheme::Light => (Color::Rgb(0, 92, 255), Color::Rgb(140, 0, 255)),
        TerminalTheme::Dark | TerminalTheme::Unknown => {
            (Color::Rgb(97, 175, 239), Color::Rgb(198, 120, 221))
        }
    }
}

pub fn picker_title_style() -> Style {
    let (_blue, purple) = accent_colors();
    Style::default().fg(purple)
}

pub fn picker_hint_style() -> Style {
    let (blue, _purple) = accent_colors();
    Style::default().fg(blue)
}

pub fn picker_selected_style() -> Style {
    let (blue, _purple) = accent_colors();
    Style::default().fg(blue).bold()
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
    primary_text_style()
}

pub fn slash_popup_highlight_style() -> Style {
    picker_selected_style()
}

pub fn model_picker_highlight_style() -> Style {
    picker_selected_style()
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
        .next_back();
    let Some(bg) = bg else {
        return TerminalTheme::Unknown;
    };

    if bg >= 7 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}
