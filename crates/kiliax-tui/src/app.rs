use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Span;
use tokio_stream::wrappers::ReceiverStream;
use unicode_width::UnicodeWidthChar;

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
        if self.committed_line_count == 0 {
            trim_leading_newlines(&mut self.buffer);
        }
    }

    fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        if self.committed_line_count == 0 {
            trim_leading_newlines(&mut self.buffer);
        }
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

#[derive(Debug, Default, Clone)]
struct ThinkingStreamCollector {
    buffer: String,
}

impl ThinkingStreamCollector {
    fn clear(&mut self) {
        self.buffer.clear();
    }

    fn push_delta(&mut self, delta: &str, max_width: usize) -> Vec<Line<'static>> {
        self.buffer.push_str(delta);
        self.drain_ready_lines(max_width, false)
    }

    fn finalize_and_drain(&mut self, max_width: usize) -> Vec<Line<'static>> {
        if self.buffer.trim().is_empty() {
            self.clear();
            return Vec::new();
        }

        let mut out = self.drain_ready_lines(max_width, true);
        trim_trailing_blank_lines(&mut out);
        self.clear();
        out
    }

    fn drain_ready_lines(&mut self, max_width: usize, flush: bool) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();

        loop {
            if let Some(newline_idx) = self.buffer.find('\n') {
                let mut chunk: String = self.buffer.drain(..=newline_idx).collect();
                if chunk.ends_with('\n') {
                    chunk.pop();
                }
                if chunk.ends_with('\r') {
                    chunk.pop();
                }
                self.emit_wrapped_text(chunk, max_width, &mut out);
                continue;
            }

            if self.buffer.is_empty() {
                break;
            }

            if flush || max_width == 0 {
                let chunk = std::mem::take(&mut self.buffer);
                self.emit_wrapped_text(chunk, max_width, &mut out);
                break;
            }

            let Some(split_idx) = soft_wrap_split_idx(&self.buffer, max_width) else {
                break;
            };

            let mut tail = self.buffer.split_off(split_idx);
            let mut head = std::mem::take(&mut self.buffer);
            trim_end_whitespace(&mut head);
            trim_start_whitespace(&mut tail);
            self.emit_line(head, &mut out);
            self.buffer = tail;
        }

        out
    }

    fn emit_wrapped_text(&mut self, mut text: String, max_width: usize, out: &mut Vec<Line<'static>>) {
        if max_width == 0 {
            self.emit_line(text, out);
            return;
        }

        loop {
            let Some(split_idx) = soft_wrap_split_idx(&text, max_width) else {
                break;
            };

            let mut tail = text.split_off(split_idx);
            let mut head = text;
            trim_end_whitespace(&mut head);
            trim_start_whitespace(&mut tail);

            self.emit_line(head, out);
            text = tail;
        }

        self.emit_line(text, out);
    }

    fn emit_line(&mut self, mut text: String, out: &mut Vec<Line<'static>>) {
        trim_end_whitespace(&mut text);
        if text.trim().is_empty() {
            return;
        }

        let thinking_style = Style::default().dim().italic();
        let mut rendered = Line::from(Span::raw(text));
        rendered.style = thinking_style;
        out.push(rendered);
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

fn soft_wrap_split_idx(text: &str, max_width: usize) -> Option<usize> {
    if max_width == 0 {
        return None;
    }

    let mut width = 0usize;
    let mut last_whitespace_idx = None;

    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch.is_whitespace() {
            last_whitespace_idx = Some(idx);
        }

        if width + ch_width > max_width {
            if let Some(ws_idx) = last_whitespace_idx.filter(|&i| i > 0) {
                return Some(ws_idx);
            }
            if idx == 0 {
                return Some(ch.len_utf8());
            }
            return Some(idx);
        }

        width = width.saturating_add(ch_width);
    }

    None
}

fn trim_end_whitespace(text: &mut String) {
    while text.chars().last().is_some_and(|ch| ch.is_whitespace()) {
        text.pop();
    }
}

fn trim_start_whitespace(text: &mut String) {
    let start = text
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    if start > 0 {
        text.drain(..start);
    }
}

fn trim_leading_newlines(text: &mut String) {
    let start = text
        .char_indices()
        .find(|(_, ch)| *ch != '\n' && *ch != '\r')
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    if start > 0 {
        text.drain(..start);
    }
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
    ReadFile { path: String },
    ListDir { path: String },
    GrepFiles { pattern: String, path: Option<String> },
    ShellCommand { argv: Vec<String>, cwd: Option<String> },
    WriteStdin { session_id: u64 },
    ApplyPatch { files: Vec<String> },
    UpdatePlan { steps: usize },
    Other,
}

pub struct App {
    pub input: InputLine,
    pub should_quit: bool,
    pub running: bool,
    pub status: Option<String>,
    screen_width: u16,
    pending_history_lines: Vec<Line<'static>>,

    profile: AgentProfile,
    runtime: AgentRuntime,
    model_id: String,
    options: AgentRuntimeOptions,
    messages: Vec<Message>,
    store: FileSessionStore,
    session: SessionState,
    assistant_stream: Option<MarkdownStreamCollector>,
    assistant_thinking_stream: Option<ThinkingStreamCollector>,
    accepting_thinking: bool,

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
            screen_width: 0,
            pending_history_lines: Vec::new(),
            profile,
            runtime,
            model_id,
            options,
            messages,
            store,
            session,
            assistant_stream: None,
            assistant_thinking_stream: None,
            accepting_thinking: false,
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

    pub fn set_screen_width(&mut self, width: u16) {
        self.screen_width = width;
    }

    fn history_wrap_width(&self) -> usize {
        if self.screen_width == 0 {
            80
        } else {
            self.screen_width as usize
        }
    }

    fn close_thinking_stream(&mut self) {
        let width = self.history_wrap_width();
        if let Some(stream) = self.assistant_thinking_stream.as_mut() {
            self.pending_history_lines
                .extend(stream.finalize_and_drain(width));
        }
        self.assistant_thinking_stream = None;
        self.accepting_thinking = false;
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
        self.close_thinking_stream();
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
        self.assistant_thinking_stream = None;
        self.accepting_thinking = false;
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
                self.assistant_thinking_stream = None;
                self.accepting_thinking = true;
                self.step_started_at = Some(Instant::now());
                self.current_step = Some(step);
                self.step_output_tokens.reset();
                self.saw_delta_in_step = false;
                self.pending_history_lines
                    .push(render_thinking_start_line(step));
            }
            AgentEvent::AssistantThinkingDelta { delta } => {
                if !self.accepting_thinking {
                    return Ok(());
                }
                self.turn_output_tokens.push_str(&delta);
                self.step_output_tokens.push_str(&delta);

                if self.assistant_thinking_stream.is_none() {
                    self.assistant_thinking_stream = Some(ThinkingStreamCollector::default());
                }

                let width = self.history_wrap_width();
                if let Some(stream) = self.assistant_thinking_stream.as_mut() {
                    self.pending_history_lines
                        .extend(stream.push_delta(&delta, width));
                }
            }
            AgentEvent::AssistantDelta { delta } => {
                if self.accepting_thinking {
                    self.close_thinking_stream();
                }
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
                    self.close_thinking_stream();

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
                self.close_thinking_stream();

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
        self.close_thinking_stream();
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
        self.assistant_thinking_stream = None;
        self.accepting_thinking = false;
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
        "read_file"
        | "list_dir"
        | "grep_files"
        | "shell_command"
        | "write_stdin"
        | "apply_patch"
        | "update_plan" => Style::default().fg(Color::Cyan).bold(),
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
    if matches!(pending.kind, PendingToolCallKind::ApplyPatch { .. }) {
        return render_apply_patch_tool_result_lines(pending, elapsed, tool_content);
    }
    if matches!(pending.kind, PendingToolCallKind::UpdatePlan { .. }) {
        return render_update_plan_tool_result_lines(pending, elapsed);
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

fn render_apply_patch_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
    tool_content: &str,
) -> Vec<Line<'static>> {
    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, tool_content);
    let parsed = serde_json::from_str::<ApplyPatchOutput>(tool_content).ok();

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

    let Some(parsed) = parsed else {
        return out;
    };

    for file in parsed.files.iter().take(6) {
        let mut label = format!("{} {}", file.action, file.path);
        if let Some(dest) = file.moved_to.as_deref() {
            label.push_str(&format!(" -> {dest}"));
        }
        if let (Some(added), Some(removed)) = (file.added_lines, file.removed_lines) {
            label.push_str(&format!(" (+{added}/-{removed})"));
        }
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(label, Style::default().dim()),
        ]));

        if let Some(diff) = file.diff.as_deref() {
            out.extend(render_diff_block_with_prefix(diff, "    └ ", "      "));
        }
    }

    if parsed.files.len() > 6 {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(
                format!("… ({} more files)", parsed.files.len().saturating_sub(6)),
                Style::default().dim(),
            ),
        ]));
    }

    out
}

fn render_update_plan_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
) -> Vec<Line<'static>> {
    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, "");

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

    let Ok(args) = serde_json::from_str::<UpdatePlanArgs>(&pending.arguments) else {
        return out;
    };

    for item in args.plan.iter().take(8) {
        let status_style = match item.status.as_str() {
            "completed" => Style::default().fg(Color::Green).dim(),
            "in_progress" => Style::default().fg(Color::Cyan).dim(),
            _ => Style::default().dim(),
        };
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(format!("[{}] ", item.status), status_style),
            Span::styled(item.step.clone(), Style::default().dim()),
        ]));
    }
    if args.plan.len() > 8 {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(
                format!("… ({} more steps)", args.plan.len().saturating_sub(8)),
                Style::default().dim(),
            ),
        ]));
    }

    out
}

fn render_diff_block_with_prefix(
    diff: &str,
    first_prefix: &'static str,
    rest_prefix: &'static str,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut first = true;
    for raw in diff.split('\n') {
        let prefix = if first { first_prefix } else { rest_prefix };
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
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ListDirArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct GrepFilesArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShellCommandArgs {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteStdinArgs {
    session_id: u64,
}

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanArgs {
    #[allow(dead_code)]
    explanation: Option<String>,
    plan: Vec<UpdatePlanItem>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanItem {
    step: String,
    status: String,
}

fn extract_patch_files(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in patch.lines() {
        let line = raw.trim_end_matches('\r');
        for prefix in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let p = rest.trim();
                if !p.is_empty() {
                    out.push(p.to_string());
                }
                break;
            }
        }
    }
    out
}

fn classify_tool_call(call: &kiliax_core::llm::ToolCall) -> PendingToolCallKind {
    match call.name.as_str() {
        "read_file" => serde_json::from_str::<ReadFileArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ReadFile { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "list_dir" => serde_json::from_str::<ListDirArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ListDir { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "grep_files" => serde_json::from_str::<GrepFilesArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::GrepFiles {
                pattern: args.pattern,
                path: args.path,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "shell_command" => serde_json::from_str::<ShellCommandArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ShellCommand {
                argv: args.argv,
                cwd: args.cwd,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "write_stdin" => serde_json::from_str::<WriteStdinArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::WriteStdin {
                session_id: args.session_id,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "apply_patch" => serde_json::from_str::<ApplyPatchArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ApplyPatch {
                files: extract_patch_files(&args.patch),
            })
            .unwrap_or(PendingToolCallKind::Other),
        "update_plan" => serde_json::from_str::<UpdatePlanArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::UpdatePlan { steps: args.plan.len() })
            .unwrap_or(PendingToolCallKind::Other),
        _ => PendingToolCallKind::Other,
    }
}

fn summarize_tool_result(pending: &PendingToolCall, tool_content: &str) -> (ToolSummary, Option<String>) {
    match &pending.kind {
        PendingToolCallKind::ReadFile { path } => {
            let line_count = tool_content.lines().count();
            (
                ToolSummary {
                    tool: "read_file".to_string(),
                    rest: path.clone(),
                },
                Some(format!("{line_count} lines")),
            )
        }
        PendingToolCallKind::ListDir { path } => {
            let entry_count = tool_content.lines().filter(|l| !l.trim().is_empty()).count();
            (
                ToolSummary {
                    tool: "list_dir".to_string(),
                    rest: path.clone(),
                },
                Some(format!("{entry_count} entries")),
            )
        }
        PendingToolCallKind::GrepFiles { pattern, path } => {
            let match_count = tool_content.lines().filter(|l| !l.trim().is_empty()).count();
            let mut rest = pattern.clone();
            if let Some(path) = path.as_deref() {
                if !path.is_empty() && path != "." {
                    rest.push_str(&format!(" ({path})"));
                }
            }
            (
                ToolSummary {
                    tool: "grep_files".to_string(),
                    rest,
                },
                Some(format!("{match_count} matches")),
            )
        }
        PendingToolCallKind::ShellCommand { argv, cwd } => {
            let cmd = argv.join(" ");
            let mut detail = String::new();
            if let Ok(parsed) = serde_json::from_str::<ShellCommandOutput>(tool_content) {
                if parsed.running {
                    if let Some(id) = parsed.session_id {
                        detail.push_str(&format!("running · session {id}"));
                    } else {
                        detail.push_str("running");
                    }
                } else if let Some(code) = parsed.exit_code {
                    detail.push_str(&format!("exit {code}"));
                }
            } else if !tool_content.trim().is_empty() {
                detail.push_str(&truncate_one_line(tool_content, 120));
            }
            if let Some(cwd) = cwd.as_deref() {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(&format!("cwd {cwd}"));
            }
            (
                ToolSummary {
                    tool: "shell_command".to_string(),
                    rest: cmd,
                },
                if detail.is_empty() { None } else { Some(detail) },
            )
        }
        PendingToolCallKind::WriteStdin { session_id } => {
            let mut detail = String::new();
            if let Ok(parsed) = serde_json::from_str::<ShellCommandOutput>(tool_content) {
                if parsed.running {
                    detail.push_str("running");
                } else if let Some(code) = parsed.exit_code {
                    detail.push_str(&format!("exit {code}"));
                }
            } else if !tool_content.trim().is_empty() {
                detail.push_str(&truncate_one_line(tool_content, 120));
            }
            (
                ToolSummary {
                    tool: "write_stdin".to_string(),
                    rest: format!("session {session_id}"),
                },
                if detail.is_empty() { None } else { Some(detail) },
            )
        }
        PendingToolCallKind::ApplyPatch { files } => {
            let rest = match files.len() {
                0 => String::new(),
                1 => files[0].clone(),
                n => format!("{n} files"),
            };
            (
                ToolSummary {
                    tool: "apply_patch".to_string(),
                    rest,
                },
                None,
            )
        }
        PendingToolCallKind::UpdatePlan { steps } => (
            ToolSummary {
                tool: "update_plan".to_string(),
                rest: format!("{steps} steps"),
            },
            None,
        ),
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
        PendingToolCallKind::ReadFile { path } => format!("read_file {path}"),
        PendingToolCallKind::ListDir { path } => format!("list_dir {path}"),
        PendingToolCallKind::GrepFiles { pattern, .. } => format!("grep_files {pattern}"),
        PendingToolCallKind::ShellCommand { argv, .. } => format!("shell_command {}", argv.join(" ")),
        PendingToolCallKind::WriteStdin { session_id } => format!("write_stdin {session_id}"),
        PendingToolCallKind::ApplyPatch { files } => match files.len() {
            0 => "apply_patch".to_string(),
            1 => format!("apply_patch {}", files[0]),
            n => format!("apply_patch {n} files"),
        },
        PendingToolCallKind::UpdatePlan { steps } => format!("update_plan {steps}"),
        PendingToolCallKind::Other => pending.name.clone(),
    }
}

#[derive(Debug, Deserialize)]
struct ShellCommandOutput {
    #[allow(dead_code)]
    session_id: Option<u64>,
    #[allow(dead_code)]
    running: bool,
    #[allow(dead_code)]
    exit_code: Option<i32>,
    #[allow(dead_code)]
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}

#[derive(Debug, Deserialize)]
struct ApplyPatchOutput {
    #[allow(dead_code)]
    ok: bool,
    #[serde(default)]
    files: Vec<PatchedFile>,
}

#[derive(Debug, Deserialize)]
struct PatchedFile {
    action: String,
    path: String,
    #[serde(default)]
    moved_to: Option<String>,
    #[serde(default)]
    diff: Option<String>,
    #[serde(default)]
    added_lines: Option<usize>,
    #[serde(default)]
    removed_lines: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiliax_core::config::ResolvedModel;
    use kiliax_core::llm::LlmClient;
    use kiliax_core::tools::ToolEngine;
    use ratatui::style::Modifier;

    fn plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn soft_wrap_prefers_whitespace_boundaries() {
        assert_eq!(soft_wrap_split_idx("hello world", 5), Some(5));
        assert_eq!(soft_wrap_split_idx("a", 1), None);
        assert_eq!(soft_wrap_split_idx("a", 0), None);

        // Wide characters (e.g. CJK) should wrap by display width.
        let idx = soft_wrap_split_idx("你好啊", 3).unwrap();
        assert_eq!(&"你好啊"[..idx], "你");
    }

    #[test]
    fn thinking_stream_wraps_and_styles_lines() {
        let mut stream = ThinkingStreamCollector::default();
        let out = stream.push_delta("hello world", 5);
        assert_eq!(out.len(), 1);
        assert_eq!(plain(&out[0]), "hello");

        let modifiers = out[0].style.add_modifier - out[0].style.sub_modifier;
        assert!(modifiers.contains(Modifier::DIM));
        assert!(modifiers.contains(Modifier::ITALIC));

        let out = stream.finalize_and_drain(5);
        assert_eq!(out.len(), 1);
        assert_eq!(plain(&out[0]), "world");
    }

    #[test]
    fn thinking_stream_ignores_blank_output() {
        let mut stream = ThinkingStreamCollector::default();
        let out = stream.push_delta("   \n\n", 80);
        assert!(out.is_empty());
        let out = stream.finalize_and_drain(80);
        assert!(out.is_empty());
    }

    #[test]
    fn markdown_stream_commits_complete_lines_and_trims_leading_newlines() {
        let mut stream = MarkdownStreamCollector::default();
        stream.push_delta("\n\nHello\nWorld");

        let out = stream.commit_complete_lines();
        assert_eq!(out.len(), 1);
        assert_eq!(plain(&out[0]), "Hello");

        let out = stream.finalize_and_drain();
        assert_eq!(out.len(), 1);
        assert_eq!(plain(&out[0]), "World");
    }

    #[test]
    fn output_token_counter_is_reasonable_for_ascii_and_cjk() {
        let mut counter = OutputTokenCounter::default();
        counter.push_str("abcd");
        assert_eq!(counter.estimate(), 1);

        counter.push_str(" efgh");
        assert_eq!(counter.estimate(), 2);

        counter.finish_segment();
        assert_eq!(counter.estimate(), 2);

        counter.push_str("你");
        counter.push_str("🙂");
        assert_eq!(counter.estimate(), 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn app_does_not_interleave_thinking_after_assistant_output_begins() {
        let tmp = tempfile::tempdir().unwrap();

        let store = FileSessionStore::new(tmp.path());
        let messages = vec![Message::User {
            content: "hi".to_string(),
        }];
        let session = store
            .create(
                "build",
                Some("p/m".to_string()),
                None,
                None,
                messages.clone(),
            )
            .await
            .unwrap();

        let llm = LlmClient::new(ResolvedModel {
            provider: "p".to_string(),
            model: "m".to_string(),
            base_url: "https://example.com/v1".to_string(),
            api_key: None,
        });
        let tools = ToolEngine::new(tmp.path());
        let runtime = AgentRuntime::new(llm, tools);

        let mut app = App::new(
            AgentProfile::build(),
            runtime,
            AgentRuntimeOptions::default(),
            store,
            session,
            messages,
        );
        app.set_screen_width(40);

        app.handle_agent_event(AgentEvent::StepStart { step: 1 })
            .await
            .unwrap();
        app.handle_agent_event(AgentEvent::AssistantThinkingDelta {
            delta: "plan".to_string(),
        })
        .await
        .unwrap();
        app.handle_agent_event(AgentEvent::AssistantDelta {
            delta: "Hello\n".to_string(),
        })
        .await
        .unwrap();
        app.handle_agent_event(AgentEvent::AssistantThinkingDelta {
            delta: "SHOULD_BE_IGNORED\n".to_string(),
        })
        .await
        .unwrap();

        let lines = app.drain_history_lines();
        let plain_lines: Vec<String> = lines.iter().map(plain).collect();

        assert_eq!(plain_lines.len(), 3);
        assert_eq!(plain_lines[0], "• Thinking (step 1)");
        assert_eq!(plain_lines[1], "plan");
        assert_eq!(plain_lines[2], "Hello");

        let modifiers = lines[1].style.add_modifier - lines[1].style.sub_modifier;
        assert!(modifiers.contains(Modifier::DIM));
        assert!(modifiers.contains(Modifier::ITALIC));
    }
}
