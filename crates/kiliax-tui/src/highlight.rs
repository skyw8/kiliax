use std::sync::LazyLock;
use std::str::FromStr;

use ratatui::style::{Color as TuiColor, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, FontStyle, ScopeSelectors, Style as SyntectStyle, StyleModifier, Theme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(vscode_dark_plus_theme);

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

fn vscode_dark_plus_theme() -> Theme {
    // Approximation of VS Code's built-in "Dark+" (default dark) colors.
    // We keep scope rules intentionally small; syntect's scopes vary by grammar.
    Theme {
        name: Some("VS Code Dark+".to_string()),
        author: Some("kiliax (built-in)".to_string()),
        settings: ThemeSettings {
            foreground: Some(hex_color(0xD4D4D4)),
            background: Some(hex_color(0x1E1E1E)),
            ..ThemeSettings::default()
        },
        scopes: vec![
            theme_item("comment, punctuation.definition.comment", fg(0x6A9955)),
            theme_item("string, punctuation.definition.string", fg(0xCE9178)),
            theme_item("constant.numeric", fg(0xB5CEA8)),
            theme_item(
                "constant.language, constant.character, constant.other",
                fg(0x569CD6),
            ),
            theme_item("keyword, storage, storage.type, storage.modifier", fg(0x569CD6)),
            theme_item(
                "entity.name.type, entity.other.inherited-class, support.type, support.class",
                fg(0x4EC9B0),
            ),
            theme_item(
                "entity.name.function, support.function, variable.function, meta.function-call",
                fg(0xDCDCAA),
            ),
            theme_item(
                "variable.parameter, variable.other.readwrite, variable",
                fg(0x9CDCFE),
            ),
            theme_item("entity.name.tag", fg(0x569CD6)),
            theme_item("entity.other.attribute-name", fg(0x9CDCFE)),
            theme_item("support.constant", fg(0x4FC1FF)),
            theme_item("invalid", fg(0xF44747)),
        ],
    }
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
    let mut out = Style::default().fg(TuiColor::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

fn theme_item(scope: &str, style: StyleModifier) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope)
            .unwrap_or_else(|_| panic!("invalid syntect scope selector: {scope}")),
        style,
    }
}

fn fg(rgb: u32) -> StyleModifier {
    StyleModifier {
        foreground: Some(hex_color(rgb)),
        ..StyleModifier::default()
    }
}

fn hex_color(rgb: u32) -> SyntectColor {
    SyntectColor {
        r: ((rgb >> 16) & 0xFF) as u8,
        g: ((rgb >> 8) & 0xFF) as u8,
        b: (rgb & 0xFF) as u8,
        a: 0xFF,
    }
}
