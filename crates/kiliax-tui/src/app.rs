use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Span;
use tokio_stream::wrappers::ReceiverStream;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use kiliax_core::{
    agents::AgentProfile,
    llm::Message,
    runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions},
    session::{FileSessionStore, SessionState},
};

use crate::input::{InputAction, InputLine};
use crate::markdown::render_markdown_lines;

#[derive(Debug, Default, Clone)]
struct MarkdownStreamCollector {
    buffer: String,
    committed_line_count: usize,
}

impl MarkdownStreamCollector {
    fn clear(&mut self) {
        self.buffer.clear();
        self.committed_line_count = 0;
    }

    fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
    }

    fn commit_complete_lines(&mut self) -> Vec<Line<'static>> {
        let Some(last_newline_idx) = self.buffer.rfind('\n') else {
            return Vec::new();
        };
        let source = &self.buffer[..=last_newline_idx];
        let mut rendered = render_markdown_lines(source);
        trim_trailing_blank_lines(&mut rendered);

        if self.committed_line_count >= rendered.len() {
            return Vec::new();
        }

        let out = rendered[self.committed_line_count..].to_vec();
        self.committed_line_count = rendered.len();
        out
    }

    fn finalize_and_drain(&mut self) -> Vec<Line<'static>> {
        if self.buffer.is_empty() {
            self.clear();
            return Vec::new();
        }

        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }
        let mut rendered = render_markdown_lines(&source);
        trim_trailing_blank_lines(&mut rendered);

        let out = if self.committed_line_count >= rendered.len() {
            Vec::new()
        } else {
            rendered[self.committed_line_count..].to_vec()
        };

        self.clear();
        out
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines.last().is_some_and(|line| line_is_blank(line)) {
        lines.pop();
    }
}

fn line_is_blank(line: &Line<'static>) -> bool {
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    text.trim().is_empty()
}

#[derive(Debug, Default, Clone)]
struct OutputTokenCounter {
    tokens: u64,
    carry_bytes: usize,
}

impl OutputTokenCounter {
    fn reset(&mut self) {
        self.tokens = 0;
        self.carry_bytes = 0;
    }

    fn estimate(&self) -> u64 {
        self.tokens + ((self.carry_bytes + 3) / 4) as u64
    }

    fn finish_segment(&mut self) {
        if self.carry_bytes > 0 {
            self.tokens += ((self.carry_bytes + 3) / 4) as u64;
            self.carry_bytes = 0;
        }
    }

    fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            if is_cjk_like(ch) {
                self.finish_segment();
                self.tokens += 1;
                continue;
            }

            if ch.is_whitespace() {
                self.finish_segment();
                continue;
            }

            if ch.is_ascii() {
                self.carry_bytes = self.carry_bytes.saturating_add(1);
                continue;
            }

            // For non-ASCII, non-CJK characters (e.g. emoji), fall back to 1 char ≈ 1 token.
            self.finish_segment();
            self.tokens += 1;
        }
    }
}

fn is_cjk_like(ch: char) -> bool {
    matches!(ch as u32,
        0x3400..=0x4DBF | // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0x20000..=0x2A6DF | // CJK Unified Ideographs Extension B
        0x2A700..=0x2B73F | // CJK Unified Ideographs Extension C
        0x2B740..=0x2B81F | // CJK Unified Ideographs Extension D
        0x2B820..=0x2CEAF | // CJK Unified Ideographs Extension E
        0x2CEB0..=0x2EBEF | // CJK Unified Ideographs Extension F
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0xAC00..=0xD7AF   // Hangul Syllables
    )
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    name: String,
    arguments: String,
    started_at: Instant,
    kind: PendingToolCallKind,
}

#[derive(Debug, Clone)]
enum PendingToolCallKind {
    Read { path: String },
    Write { path: String },
    Shell { argv: Vec<String>, cwd: Option<String> },
    Other,
}

pub struct App {
    pub input: InputLine,
    pub should_quit: bool,
    pub running: bool,
    pub status: Option<String>,
    pending_history_lines: Vec<Line<'static>>,

    profile: AgentProfile,
    runtime: AgentRuntime,
    model_id: String,
    options: AgentRuntimeOptions,
    messages: Vec<Message>,
    store: FileSessionStore,
    session: SessionState,
    assistant_stream: Option<MarkdownStreamCollector>,

    prompt_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,

    turn_started_at: Option<Instant>,
    step_started_at: Option<Instant>,
    current_step: Option<usize>,
    pending_tool_calls: HashMap<String, PendingToolCall>,

    turn_output_tokens: OutputTokenCounter,
    step_output_tokens: OutputTokenCounter,
    saw_delta_in_step: bool,
}

impl App {
    pub fn new(
        profile: AgentProfile,
        runtime: AgentRuntime,
        options: AgentRuntimeOptions,
        store: FileSessionStore,
        session: SessionState,
        messages: Vec<Message>,
    ) -> Self {
        let model_id = runtime.llm().route().model_id();
        let prompt_history: Vec<String> = messages
            .iter()
            .filter_map(|msg| match msg {
                Message::User { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        Self {
            input: InputLine::default(),
            should_quit: false,
            running: false,
            status: None,
            pending_history_lines: Vec::new(),
            profile,
            runtime,
            model_id,
            options,
            messages,
            store,
            session,
            assistant_stream: None,
            prompt_history,
            history_index: None,
            history_draft: String::new(),
            turn_started_at: None,
            step_started_at: None,
            current_step: None,
            pending_tool_calls: HashMap::new(),
            turn_output_tokens: OutputTokenCounter::default(),
            step_output_tokens: OutputTokenCounter::default(),
            saw_delta_in_step: false,
        }
    }

    pub fn session_id(&self) -> &str {
        self.session.meta.id.as_str()
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn turn_elapsed(&self) -> Option<Duration> {
        self.turn_started_at.map(|t| t.elapsed())
    }

    pub fn step_elapsed(&self) -> Option<(usize, Duration)> {
        match (self.current_step, self.step_started_at) {
            (Some(step), Some(started_at)) => Some((step, started_at.elapsed())),
            _ => None,
        }
    }

    pub fn turn_output_tokens(&self) -> u64 {
        self.turn_output_tokens.estimate()
    }

    pub fn step_output_tokens(&self) -> u64 {
        self.step_output_tokens.estimate()
    }

    pub fn active_tool_elapsed(&self) -> Option<(String, Duration)> {
        let pending = self
            .pending_tool_calls
            .values()
            .max_by_key(|p| p.started_at)?;
        Some((tool_status_label(pending), pending.started_at.elapsed()))
    }

    pub fn interrupt_run(&mut self) {
        if let Some(stream) = self.assistant_stream.as_mut() {
            self.pending_history_lines.extend(stream.finalize_and_drain());
        }
        self.assistant_stream = None;
        self.running = false;
        self.status = Some("interrupted".to_string());
        self.messages = self.session.messages.clone();
        self.turn_started_at = None;
        self.step_started_at = None;
        self.current_step = None;
        self.pending_tool_calls.clear();
        self.turn_output_tokens.reset();
        self.step_output_tokens.reset();
        self.saw_delta_in_step = false;
    }

    pub fn drain_history_lines(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.pending_history_lines)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        if self.running && key.code == KeyCode::Enter {
            return None;
        }

        if key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.input.clear();
            self.reset_history_nav();
            return None;
        }

        match key.code {
            KeyCode::Up => {
                self.history_prev();
                return None;
            }
            KeyCode::Down => {
                self.history_next();
                return None;
            }
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {
                if self.history_index.is_some() {
                    self.reset_history_nav();
                }
            }
            _ => {}
        }

        match self.input.handle_key(key) {
            InputAction::None => {}
            InputAction::Submit(text) => {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.history_index.is_some() {
            self.reset_history_nav();
        }
        self.input.insert_str(text);
    }

    pub async fn submit_user_message(&mut self, text: String) -> Result<()> {
        self.prompt_history.push(text.clone());
        self.reset_history_nav();
        self.pending_history_lines
            .extend(render_user_message_lines(&text));

        let msg = Message::User { content: text };
        self.messages.push(msg.clone());
        self.store.record_message(&mut self.session, msg).await?;
        Ok(())
    }

    pub async fn start_run(
        &mut self,
    ) -> Result<ReceiverStream<Result<AgentEvent, AgentRuntimeError>>> {
        self.running = true;
        self.status = Some("running".to_string());
        self.assistant_stream = None;
        self.turn_started_at = Some(Instant::now());
        self.step_started_at = None;
        self.current_step = None;
        self.pending_tool_calls.clear();
        self.turn_output_tokens.reset();
        self.step_output_tokens.reset();
        self.saw_delta_in_step = false;
        Ok(self
            .runtime
            .run_stream(&self.profile, self.messages.clone(), self.options.clone())
            .await?)
    }

    pub async fn handle_agent_event(&mut self, event: AgentEvent) -> Result<()> {
        match event {
            AgentEvent::StepStart { step } => {
                self.status = Some(format!("step {step}"));
                self.assistant_stream = None;
                self.step_started_at = Some(Instant::now());
                self.current_step = Some(step);
                self.step_output_tokens.reset();
                self.saw_delta_in_step = false;
                self.pending_history_lines
                    .push(render_thinking_start_line(step));
            }
            AgentEvent::AssistantDelta { delta } => {
                self.turn_output_tokens.push_str(&delta);
                self.step_output_tokens.push_str(&delta);
                self.saw_delta_in_step = true;

                if self.assistant_stream.is_none() {
                    self.assistant_stream = Some(MarkdownStreamCollector::default());
                }

                if let Some(stream) = self.assistant_stream.as_mut() {
                    stream.push_delta(&delta);
                    if delta.contains('\n') {
                        self.pending_history_lines
                            .extend(stream.commit_complete_lines());
                    }
                }
            }
            AgentEvent::AssistantMessage { message } => {
                self.store
                    .record_message(&mut self.session, message.clone())
                    .await?;

                if let Message::Assistant {
                    content,
                    tool_calls: _,
                } = message
                {
                    let content = content.unwrap_or_default();
                    if !self.saw_delta_in_step && !content.is_empty() {
                        self.turn_output_tokens.push_str(&content);
                        self.step_output_tokens.push_str(&content);
                    }
                    self.turn_output_tokens.finish_segment();
                    self.step_output_tokens.finish_segment();

                    if !content.is_empty() {
                        if self.assistant_stream.is_none() {
                            self.assistant_stream = Some(MarkdownStreamCollector::default());
                        }
                        if let Some(stream) = self.assistant_stream.as_mut() {
                            stream.set_text(&content);
                            self.pending_history_lines
                                .extend(stream.finalize_and_drain());
                        }
                    }
                    self.assistant_stream = None;
                    self.step_started_at = None;
                }
            }
            AgentEvent::ToolCall { call } => {
                let started_at = Instant::now();
                let kind = classify_tool_call(&call);
                self.turn_output_tokens.push_str(&call.name);
                self.turn_output_tokens.push_str(" ");
                self.turn_output_tokens.push_str(&call.arguments);
                self.turn_output_tokens.finish_segment();
                self.pending_tool_calls.insert(
                    call.id.clone(),
                    PendingToolCall {
                        name: call.name,
                        arguments: call.arguments,
                        started_at,
                        kind,
                    },
                );
            }
            AgentEvent::ToolResult { message } => {
                self.store
                    .record_message(&mut self.session, message.clone())
                    .await?;

                if let Message::Tool {
                    tool_call_id,
                    content,
                } = message
                {
                    let started_at = self
                        .pending_tool_calls
                        .get(&tool_call_id)
                        .map(|pending| pending.started_at);
                    let elapsed = started_at.map(|t| t.elapsed());
                    let pending = self.pending_tool_calls.remove(&tool_call_id);
                    if let Some(pending) = pending {
                        self.pending_history_lines.extend(render_tool_result_lines(
                            &pending,
                            elapsed,
                            &content,
                        ));
                    } else {
                        self.pending_history_lines.extend(render_tool_result_fallback_lines(
                            &tool_call_id,
                            elapsed,
                            &content,
                        ));
                    }
                }
            }
            AgentEvent::StepEnd { .. } => {}
            AgentEvent::Done(out) => {
                self.store
                    .record_finish(
                        &mut self.session,
                        out.finish_reason.as_ref().map(|r| format!("{r:?}")),
                    )
                    .await?;

                self.messages = out.messages;
                self.running = false;
                self.status = Some(format!(
                    "done (steps={}, reason={:?})",
                    out.steps, out.finish_reason
                ));
                if let Some(stream) = self.assistant_stream.as_mut() {
                    self.pending_history_lines.extend(stream.finalize_and_drain());
                }
                self.assistant_stream = None;

                if let Some(started_at) = self.turn_started_at.take() {
                    self.turn_output_tokens.finish_segment();
                    let output_tokens = self.turn_output_tokens.estimate();
                    self.pending_history_lines
                        .push(turn_divider_marker(started_at.elapsed(), output_tokens));
                }
                self.step_started_at = None;
                self.current_step = None;
                self.pending_tool_calls.clear();
            }
        }
        Ok(())
    }

    pub async fn handle_agent_error(&mut self, err: AgentRuntimeError) -> Result<()> {
        let text = err.to_string();
        let _ = self
            .store
            .record_error(&mut self.session, text.clone())
            .await;
        self.pending_history_lines
            .extend(render_error_lines(&text));
        if let Some(started_at) = self.turn_started_at.take() {
            self.turn_output_tokens.finish_segment();
            let output_tokens = self.turn_output_tokens.estimate();
            self.pending_history_lines
                .push(turn_divider_marker(started_at.elapsed(), output_tokens));
        }
        self.running = false;
        self.status = Some("error".to_string());
        self.assistant_stream = None;
        self.step_started_at = None;
        self.current_step = None;
        self.pending_tool_calls.clear();
        Ok(())
    }

    fn reset_history_nav(&mut self) {
        self.history_index = None;
        self.history_draft.clear();
    }

    fn history_prev(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }

        let idx = match self.history_index {
            Some(idx) => idx.saturating_sub(1),
            None => {
                self.history_draft = self.input.text().to_string();
                self.prompt_history.len().saturating_sub(1)
            }
        };

        self.history_index = Some(idx);
        if let Some(text) = self.prompt_history.get(idx).cloned() {
            self.input.set_text(text);
        }
    }

    fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };

        let next = idx.saturating_add(1);
        if next < self.prompt_history.len() {
            self.history_index = Some(next);
            if let Some(text) = self.prompt_history.get(next).cloned() {
                self.input.set_text(text);
            }
            return;
        }

        self.history_index = None;
        let draft = std::mem::take(&mut self.history_draft);
        self.input.set_text(draft);
    }
}

fn render_user_message_lines(content: &str) -> Vec<Line<'static>> {
    let payload = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".to_string());
    vec![Line::from(Span::from(format!(
        "{}{}",
        crate::history::USER_MESSAGE_MARKER_PREFIX,
        payload
    )))]
}

fn fmt_duration_compact(duration: Duration) -> String {
    let ms = duration.as_millis() as u64;
    if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

fn turn_divider_marker(elapsed: Duration, output_tokens: u64) -> Line<'static> {
    Line::from(Span::from(format!(
        "{}{},{}",
        crate::history::DIVIDER_MARKER_PREFIX,
        elapsed.as_millis(),
        output_tokens
    )))
}

fn render_thinking_start_line(step: usize) -> Line<'static> {
    let summary_style = Style::default().dim().italic();
    let label = format!("Thinking (step {step})");
    Line::from(vec![
        Span::from("• ").dim(),
        Span::styled(label, summary_style),
    ])
}

#[derive(Debug, Clone)]
struct ToolSummary {
    tool: String,
    rest: String,
}

fn tool_name_span(tool: &str) -> Span<'static> {
    let style = match tool {
        "read" | "write" | "shell" => Style::default().fg(Color::Cyan).bold(),
        _ => Style::default().bold(),
    };
    Span::styled(tool.to_string(), style)
}

fn render_tool_result_fallback_lines(
    tool_call_id: &str,
    elapsed: Option<Duration>,
    content: &str,
) -> Vec<Line<'static>> {
    let summary_style = Style::default().dim();
    let duration = elapsed.map(fmt_duration_compact).unwrap_or_else(|| "—".to_string());
    let spans = vec![
        Span::from("• ").dim(),
        Span::from("Tool").bold(),
        Span::from(" "),
        Span::styled(tool_call_id.to_string(), summary_style),
        Span::from(" "),
        Span::styled(format!("({duration})"), summary_style),
    ];

    let mut out = vec![Line::from(spans)];
    if !content.trim().is_empty() {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(truncate_one_line(content, 120), summary_style),
        ]));
    }
    out
}

fn render_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
    tool_content: &str,
) -> Vec<Line<'static>> {
    if matches!(pending.kind, PendingToolCallKind::Write { .. }) {
        return render_write_tool_result_lines(pending, elapsed, tool_content);
    }

    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, tool_content);

    let mut header = vec![Span::from("• ").dim(), tool_name_span(&summary.tool)];
    if !summary.rest.is_empty() {
        header.push(Span::from(" "));
        header.push(Span::from(summary.rest));
    }
    if let Some(duration) = duration {
        header.push(Span::from(" "));
        header.push(Span::styled(format!("({duration})"), Style::default().dim()));
    }

    let mut out = vec![Line::from(header)];
    if let Some(detail) = detail {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(detail, Style::default().dim()),
        ]));
    }
    out
}

fn render_write_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
    tool_content: &str,
) -> Vec<Line<'static>> {
    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, tool_content);
    let parsed = serde_json::from_str::<WriteToolOutput>(tool_content).ok();

    let mut header = vec![Span::from("• ").dim(), tool_name_span(&summary.tool)];
    if !summary.rest.is_empty() {
        header.push(Span::from(" "));
        header.push(Span::from(summary.rest));
    }
    if let Some(duration) = duration {
        header.push(Span::from(" "));
        header.push(Span::styled(format!("({duration})"), Style::default().dim()));
    }

    let mut out = vec![Line::from(header)];
    if let Some(detail) = detail {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(detail, Style::default().dim()),
        ]));
    }

    if let Some(diff) = parsed.and_then(|o| o.diff) {
        out.extend(render_diff_block(&diff));
    }

    out
}

fn render_diff_block(diff: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut first = true;
    for raw in diff.split('\n') {
        let prefix = if first { "  └ " } else { "    " };
        first = false;

        let style = diff_line_style(raw);
        let mut line = Line::from(vec![
            Span::from(prefix).dim(),
            Span::from(raw.to_string()),
        ]);
        line.style = style;
        out.push(line);
    }
    out
}

fn diff_line_style(line: &str) -> Style {
    use crate::style;

    if line.starts_with("@@") || line.starts_with("diff ") || line.starts_with("index ") {
        Style::default().dim()
    } else if line.starts_with("+++ ") || line.starts_with("--- ") {
        Style::default().dim()
    } else if line.starts_with('+') {
        style::diff_insert_style()
    } else if line.starts_with('-') {
        style::diff_delete_style()
    } else {
        Style::default().dim()
    }
}

fn truncate_one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            break;
        }
        if out.chars().count() >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn render_error_lines(text: &str) -> Vec<Line<'static>> {
    vec![Line::from(vec![
        Span::from("• ").dim(),
        Span::styled("error", Style::default().fg(Color::LightRed).bold()),
        Span::from(": "),
        Span::from(text.to_string()),
    ])]
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ShellArgs {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

fn classify_tool_call(call: &kiliax_core::llm::ToolCall) -> PendingToolCallKind {
    match call.name.as_str() {
        "read" => serde_json::from_str::<ReadArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::Read { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "write" => serde_json::from_str::<WriteArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::Write { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "shell" => serde_json::from_str::<ShellArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::Shell {
                argv: args.argv,
                cwd: args.cwd,
            })
            .unwrap_or(PendingToolCallKind::Other),
        _ => PendingToolCallKind::Other,
    }
}

fn summarize_tool_result(pending: &PendingToolCall, tool_content: &str) -> (ToolSummary, Option<String>) {
    match &pending.kind {
        PendingToolCallKind::Read { path } => {
            let line_count = tool_content.lines().count();
            (
                ToolSummary {
                    tool: "read".to_string(),
                    rest: path.clone(),
                },
                Some(format!("{line_count} lines")),
            )
        }
        PendingToolCallKind::Write { path } => {
            let parsed = serde_json::from_str::<WriteToolOutput>(tool_content).ok();
            let created = parsed.as_ref().map(|o| o.created);
            let added = parsed.as_ref().and_then(|o| o.added_lines);
            let removed = parsed.as_ref().and_then(|o| o.removed_lines);

            let what = match created {
                Some(true) => "created",
                Some(false) => "updated",
                None => "wrote",
            };

            let mut detail = what.to_string();
            if let (Some(added), Some(removed)) = (added, removed) {
                detail.push_str(&format!(" (+{added}/-{removed})"));
            }

            (
                ToolSummary {
                    tool: "write".to_string(),
                    rest: path.clone(),
                },
                Some(detail),
            )
        }
        PendingToolCallKind::Shell { argv, cwd } => {
            let cmd = argv.join(" ");
            let code = tool_content
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("exit_code: "))
                .unwrap_or("")
                .trim();
            let mut detail = String::new();
            if !code.is_empty() {
                detail.push_str(&format!("exit {code}"));
            }
            if let Some(cwd) = cwd.as_deref() {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(&format!("cwd {cwd}"));
            }
            (
                ToolSummary {
                    tool: "shell".to_string(),
                    rest: cmd,
                },
                if detail.is_empty() { None } else { Some(detail) },
            )
        }
        PendingToolCallKind::Other => (
            ToolSummary {
                tool: pending.name.clone(),
                rest: String::new(),
            },
            Some(truncate_one_line(&pending.arguments, 120)),
        ),
    }
}

fn tool_status_label(pending: &PendingToolCall) -> String {
    match &pending.kind {
        PendingToolCallKind::Read { path } => format!("read {path}"),
        PendingToolCallKind::Write { path } => format!("write {path}"),
        PendingToolCallKind::Shell { argv, .. } => format!("shell {}", argv.join(" ")),
        PendingToolCallKind::Other => pending.name.clone(),
    }
}

#[derive(Debug, Deserialize)]
struct WriteToolOutput {
    #[allow(dead_code)]
    ok: bool,
    #[allow(dead_code)]
    path: String,
    created: bool,
    #[allow(dead_code)]
    bytes: usize,
    #[serde(default)]
    diff: Option<String>,
    #[serde(default)]
    added_lines: Option<usize>,
    #[serde(default)]
    removed_lines: Option<usize>,
}
