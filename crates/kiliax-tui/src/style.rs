use ratatui::style::{Color, Style};

pub fn composer_background_style() -> Style {
    let bg = match terminal_theme_hint() {
        TerminalTheme::Light => Color::Rgb(240, 240, 240),
        TerminalTheme::Dark | TerminalTheme::Unknown => Color::Rgb(48, 48, 48),
    };
    Style::default().bg(bg)
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
