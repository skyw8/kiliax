use ratatui::style::{Color, Style};

pub fn composer_background_style() -> Style {
    let bg = match terminal_theme_hint() {
        TerminalTheme::Light => Color::Rgb(240, 240, 240),
        TerminalTheme::Dark | TerminalTheme::Unknown => Color::Rgb(48, 48, 48),
    };
    Style::default().bg(bg)
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
