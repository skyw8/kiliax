use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let ts = ThemeSet::load_defaults();
    for name in ["base16-ocean.dark", "Solarized (dark)", "Monokai Extended"].iter() {
        if let Some(theme) = ts.themes.get(*name) {
            return theme.clone();
        }
    }
    ts.themes
        .values()
        .next()
        .cloned()
        .unwrap_or_else(|| Theme::default())
});

pub fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    let lang = lang.trim().trim_matches(['{', '}']).trim();
    if lang.is_empty() {
        return plain_lines(code, Style::default());
    }

    let Some(syntax) = find_syntax(lang) else {
        return plain_lines(code, Style::default());
    };

    let mut highlighter = HighlightLines::new(syntax, &THEME);
    let mut out = Vec::new();

    for line in code.lines() {
        match highlighter.highlight_line(line, &SYNTAX_SET) {
            Ok(ranges) => {
                let spans: Vec<Span<'static>> = ranges
                    .into_iter()
                    .map(|(style, text)| Span::styled(text.to_string(), syntect_style(style)))
                    .collect();
                out.push(Line::from(spans));
            }
            Err(_) => out.push(Line::from(line.to_string())),
        }
    }

    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let ss = &SYNTAX_SET;
    let patched = match lang {
        "csharp" | "c-sharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => lang,
    };

    ss.find_syntax_by_token(patched)
        .or_else(|| ss.find_syntax_by_name(patched))
        .or_else(|| ss.find_syntax_by_extension(patched))
}

fn plain_lines(input: &str, style: Style) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = input
        .lines()
        .map(|l| Line::from(vec![Span::styled(l.to_string(), style)]))
        .collect();
    if out.is_empty() {
        out.push(Line::from(vec![Span::styled(String::new(), style)]));
    }
    out
}

fn syntect_style(style: SyntectStyle) -> Style {
    let mut out = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    out
}
