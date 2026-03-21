use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Line;
use tokio_stream::wrappers::ReceiverStream;

use kiliax_core::{
    agents::AgentProfile,
    llm::Message,
    runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions},
    session::{FileSessionStore, SessionState},
};

use crate::input::{InputAction, InputLine};
use crate::markdown::render_markdown_lines;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
    Info,
}

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
        }
    }

    pub fn session_id(&self) -> &str {
        self.session.meta.id.as_str()
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
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
            .extend(render_entry_lines(ChatRole::User, &text));

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
            }
            AgentEvent::AssistantDelta { delta } => {
                if self.assistant_stream.is_none() {
                    self.pending_history_lines
                        .push(render_role_header(ChatRole::Assistant));
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

                if let Message::Assistant { content, .. } = message {
                    let content = content.unwrap_or_default();
                    if content.is_empty() {
                        self.assistant_stream = None;
                        return Ok(());
                    }

                    if self.assistant_stream.is_none() {
                        self.pending_history_lines
                            .push(render_role_header(ChatRole::Assistant));
                        self.assistant_stream = Some(MarkdownStreamCollector::default());
                    }

                    if let Some(stream) = self.assistant_stream.as_mut() {
                        stream.set_text(&content);
                        self.pending_history_lines
                            .extend(stream.finalize_and_drain());
                    }
                    self.pending_history_lines.push(Line::from(""));
                    self.assistant_stream = None;
                }
            }
            AgentEvent::ToolCall { call } => {
                self.pending_history_lines.extend(render_entry_lines(
                    ChatRole::Tool,
                    &format!("`{}` `{}`", call.name, call.arguments),
                ));
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
                    self.pending_history_lines.extend(render_entry_lines(
                        ChatRole::Tool,
                        &format!("`tool_result` `{tool_call_id}`\n\n```text\n{content}\n```"),
                    ));
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
                self.assistant_stream = None;
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
        self.pending_history_lines.extend(render_entry_lines(
            ChatRole::Info,
            &format!("error: {text}"),
        ));
        self.running = false;
        self.status = Some("error".to_string());
        self.assistant_stream = None;
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

fn render_entry_lines(role: ChatRole, content: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(render_role_header(role));
    out.extend(render_markdown_lines(content));
    out.push(Line::from(""));
    out
}

fn render_role_header(role: ChatRole) -> Line<'static> {
    use ratatui::style::Stylize;
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    let (label, color) = match role {
        ChatRole::User => ("User", Color::Green),
        ChatRole::Assistant => ("Assistant", Color::Cyan),
        ChatRole::Tool => ("Tool", Color::Yellow),
        ChatRole::Info => ("Info", Color::Magenta),
    };

    Line::from(vec![Span::styled(
        format!("{label}:"),
        Style::default().fg(color).bold(),
    )])
}
