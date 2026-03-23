use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::highlight::highlight_code_to_lines;

#[derive(Clone)]
struct MarkdownStyles {
    heading: [Style; 6],
    code: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    link: Style,
    blockquote: Style,
    list_marker: Style,
}

impl Default for MarkdownStyles {
    fn default() -> Self {
        Self {
            heading: [
                Style::new().bold().underlined(),
                Style::new().bold(),
                Style::new().bold().italic(),
                Style::new().italic(),
                Style::new().italic(),
                Style::new().italic(),
            ],
            code: Style::new().fg(Color::Cyan),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().crossed_out(),
            link: Style::new().fg(Color::Cyan).underlined(),
            blockquote: Style::new().fg(Color::Green),
            list_marker: Style::new().fg(Color::LightBlue),
        }
    }
}

#[derive(Debug, Clone)]
enum PrefixContext {
    BlockQuote,
    ListItem {
        marker: Vec<Span<'static>>,
        indent: Vec<Span<'static>>,
        first_line: bool,
    },
}

pub fn render_markdown_lines(input: &str) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(input, options);

    let mut writer = Writer::new(parser);
    writer.run();

    while matches!(writer.lines.last(), Some(line) if line.spans.is_empty()) {
        writer.lines.pop();
    }

    if writer.lines.is_empty() {
        writer.lines.push(Line::from(""));
    }

    writer.lines
}

#[cfg(test)]
mod tests {
    use super::render_markdown_lines;

    fn to_plain_lines(rendered: Vec<ratatui::text::Line<'static>>) -> Vec<String> {
        rendered
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|s| s.content.as_ref().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn renders_sample_greeting_without_extra_blank_lines() {
        let input = "\
Hello! I'm the general agent, ready to help you implement changes in the repository.

What would you like me to work on? I can:

- Read and write files

- Run build and test commands

- Make incremental changes to the codebase

Just let me know what changes you'd like to make!
";

        let lines = to_plain_lines(render_markdown_lines(input));
        assert!(
            !lines.iter().any(|l| l.trim().is_empty()),
            "expected compact rendering without blank lines, got: {lines:?}"
        );
    }

    #[test]
    fn rendered_lines_never_contain_newline_characters() {
        let input = "a\nb\n- c\n";
        let rendered = render_markdown_lines(input);
        for line in rendered {
            for span in line.spans {
                let s = span.content.as_ref();
                assert!(
                    !s.contains('\n') && !s.contains('\r'),
                    "span contains newline characters: {s:?}"
                );
            }
        }
    }
}

struct Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    iter: I,
    styles: MarkdownStyles,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    prefix_stack: Vec<PrefixContext>,
    list_stack: Vec<Option<u64>>,
    in_code_block: bool,
    code_block_lang: Option<String>,
    code_block_buf: String,
}

impl<'a, I> Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    fn new(iter: I) -> Self {
        Self {
            iter,
            styles: MarkdownStyles::default(),
            lines: Vec::new(),
            current: Vec::new(),
            style_stack: vec![Style::default()],
            prefix_stack: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_block_lang: None,
            code_block_buf: String::new(),
        }
    }

    fn run(&mut self) {
        while let Some(ev) = self.iter.next() {
            self.handle(ev);
        }
        self.flush_line();
    }

    fn handle(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text.into_string()),
            Event::Code(code) => self.inline_code(code.into_string()),
            Event::SoftBreak | Event::HardBreak => self.new_line(),
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::from(vec![Span::raw("───")]));
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(html.into_string()),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.inline_code(math.into_string());
            }
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => {
                self.flush_line();
            }
            Tag::Heading { level, .. } => {
                self.flush_line();
                let style = self.heading_style(level);
                self.push_style(style);
            }
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.prefix_stack.push(PrefixContext::BlockQuote);
            }
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_style(self.styles.emphasis),
            Tag::Strong => self.push_style(self.styles.strong),
            Tag::Strikethrough => self.push_style(self.styles.strikethrough),
            Tag::Link { .. } => self.push_style(self.styles.link),
            Tag::CodeBlock(kind) => self.start_code_block(kind),
            Tag::HtmlBlock
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Image { .. }
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_line();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                if matches!(self.prefix_stack.last(), Some(PrefixContext::BlockQuote)) {
                    self.prefix_stack.pop();
                }
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.flush_line();
            }
            TagEnd::Item => {
                self.flush_line();
                if matches!(
                    self.prefix_stack.last(),
                    Some(PrefixContext::ListItem { .. })
                ) {
                    self.prefix_stack.pop();
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style();
            }
            TagEnd::CodeBlock => self.end_code_block(),
            TagEnd::HtmlBlock
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::Image
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_item(&mut self) {
        self.flush_line();

        let depth = self.list_stack.len().max(1);
        let indent = "  ".repeat(depth.saturating_sub(1));
        let marker = match self.list_stack.last_mut() {
            Some(Some(n)) => {
                let current = *n;
                *n = n.saturating_add(1);
                format!("{current}. ")
            }
            _ => "- ".to_string(),
        };

        let marker_width = (indent.clone() + &marker).width();
        let marker_spans = vec![
            Span::raw(indent),
            Span::styled(marker.clone(), self.styles.list_marker),
        ];
        let indent_spans = vec![Span::raw(" ".repeat(marker_width))];

        self.prefix_stack.push(PrefixContext::ListItem {
            marker: marker_spans,
            indent: indent_spans,
            first_line: true,
        });
    }

    fn start_code_block(&mut self, kind: CodeBlockKind<'a>) {
        self.flush_line();
        self.in_code_block = true;
        self.code_block_buf.clear();
        self.code_block_lang = match kind {
            CodeBlockKind::Fenced(lang) => Some(lang.into_string()),
            CodeBlockKind::Indented => None,
        };
    }

    fn end_code_block(&mut self) {
        self.flush_line();

        let lang = self.code_block_lang.clone();
        let code = std::mem::take(&mut self.code_block_buf);

        let mut code_lines = match lang.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(lang) => highlight_code_to_lines(&code, lang),
            None => code
                .lines()
                .map(|l| Line::from(vec![Span::styled(l.to_string(), self.styles.code)]))
                .collect(),
        };
        if code_lines.is_empty() {
            code_lines.push(Line::from(vec![Span::styled(
                String::new(),
                self.styles.code,
            )]));
        }

        for mut line in code_lines {
            let mut spans = self.prefix_for_new_line();
            spans.append(&mut line.spans);
            self.lines.push(Line::from(spans));
        }

        self.in_code_block = false;
        self.code_block_lang = None;
    }

    fn text(&mut self, mut text: String) {
        if self.in_code_block {
            self.code_block_buf.push_str(&text);
            return;
        }

        if text.contains('\t') {
            text = text.replace('\t', "    ");
        }

        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                self.new_line();
            }
            self.ensure_line_started();
            self.current
                .push(Span::styled(line.to_string(), self.current_style()));
        }
    }

    fn inline_code(&mut self, code: String) {
        if self.in_code_block {
            self.code_block_buf.push_str(&code);
            return;
        }
        self.ensure_line_started();
        self.current.push(Span::styled(code, self.styles.code));
    }

    fn new_line(&mut self) {
        self.flush_line();
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.current);
        self.lines.push(Line::from(spans));
    }

    fn ensure_line_started(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        let prefix = self.prefix_for_new_line();
        self.current.extend(prefix);
    }

    fn prefix_for_new_line(&mut self) -> Vec<Span<'static>> {
        let mut out: Vec<Span<'static>> = Vec::new();
        for ctx in &mut self.prefix_stack {
            match ctx {
                PrefixContext::BlockQuote => {
                    out.push(Span::styled("> ", self.styles.blockquote));
                }
                PrefixContext::ListItem {
                    marker,
                    indent,
                    first_line,
                } => {
                    if *first_line {
                        out.extend(marker.clone());
                        *first_line = false;
                    } else {
                        out.extend(indent.clone());
                    }
                }
            }
        }
        out
    }

    fn push_style(&mut self, style: Style) {
        let current = self.current_style();
        self.style_stack.push(current.patch(style));
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }

    fn heading_style(&self, level: HeadingLevel) -> Style {
        match level {
            HeadingLevel::H1 => self.styles.heading[0],
            HeadingLevel::H2 => self.styles.heading[1],
            HeadingLevel::H3 => self.styles.heading[2],
            HeadingLevel::H4 => self.styles.heading[3],
            HeadingLevel::H5 => self.styles.heading[4],
            HeadingLevel::H6 => self.styles.heading[5],
        }
    }
}
