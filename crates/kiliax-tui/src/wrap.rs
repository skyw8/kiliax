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
