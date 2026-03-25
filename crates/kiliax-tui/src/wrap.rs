use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

pub fn wrap_lines(lines: &[Line<'static>], max_width: usize) -> Vec<Line<'static>> {
    if max_width == 0 {
        return vec![Line::from("")];
    }

    let mut out = Vec::new();
    for line in lines {
        out.extend(wrap_line(line, max_width));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

fn wrap_line(line: &Line<'static>, max_width: usize) -> Vec<Line<'static>> {
    if line.spans.is_empty() {
        let mut blank = Line::from("");
        blank.style = line.style;
        blank.alignment = line.alignment;
        return vec![blank];
    }

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<(ratatui::style::Style, String)> = Vec::new();
    let mut width = 0usize;

    let push_current = |out: &mut Vec<Line<'static>>,
                        current: &mut Vec<(ratatui::style::Style, String)>| {
        let spans: Vec<Span<'static>> = current
            .drain(..)
            .filter(|(_, s)| !s.is_empty())
            .map(|(style, s)| Span::styled(s, style))
            .collect();
        let mut line_out = Line::from(spans);
        line_out.style = line.style;
        line_out.alignment = line.alignment;
        out.push(line_out);
    };

    for span in &line.spans {
        let style = span.style;
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > max_width && width > 0 {
                push_current(&mut out, &mut current);
                width = 0;
            }

            if let Some((last_style, last_text)) = current.last_mut() {
                if *last_style == style {
                    last_text.push(ch);
                } else {
                    current.push((style, ch.to_string()));
                }
            } else {
                current.push((style, ch.to_string()));
            }
            width += ch_width;
        }
    }

    if !current.is_empty() || out.is_empty() {
        push_current(&mut out, &mut current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    fn line_to_plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn wraps_and_preserves_span_styles_across_boundaries() {
        let a = Style::default().fg(Color::Red);
        let b = Style::default().fg(Color::Blue);

        let line = Line::from(vec![Span::styled("ab", a), Span::styled("cd", b)]);

        let wrapped = wrap_lines(&[line], 3);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(line_to_plain(&wrapped[0]), "abc");
        assert_eq!(line_to_plain(&wrapped[1]), "d");

        assert_eq!(wrapped[0].spans.len(), 2);
        assert_eq!(wrapped[0].spans[0].style, a);
        assert_eq!(wrapped[0].spans[0].content.as_ref(), "ab");
        assert_eq!(wrapped[0].spans[1].style, b);
        assert_eq!(wrapped[0].spans[1].content.as_ref(), "c");

        assert_eq!(wrapped[1].spans.len(), 1);
        assert_eq!(wrapped[1].spans[0].style, b);
        assert_eq!(wrapped[1].spans[0].content.as_ref(), "d");
    }

    #[test]
    fn wraps_wide_characters_by_display_width() {
        let line = Line::from(Span::raw("你a"));
        let wrapped = wrap_lines(&[line], 2);
        let plain: Vec<String> = wrapped.iter().map(line_to_plain).collect();
        assert_eq!(plain, vec!["你".to_string(), "a".to_string()]);
    }

    #[test]
    fn width_zero_returns_single_blank_line() {
        let wrapped = wrap_lines(&[Line::from("x")], 0);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(line_to_plain(&wrapped[0]), "");
    }

    #[test]
    fn empty_input_returns_single_blank_line() {
        let wrapped = wrap_lines(&[], 10);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(line_to_plain(&wrapped[0]), "");
    }
}
