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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
    Info,
}

#[derive(Debug, Clone)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub content: String,
    pub rendered: Option<Vec<Line<'static>>>,
}

pub struct App {
    pub transcript: Vec<ChatEntry>,
    pub input: InputLine,
    pub should_quit: bool,
    pub running: bool,
    pub status: Option<String>,
    pub flush_requested: bool,

    profile: AgentProfile,
    runtime: AgentRuntime,
    model_id: String,
    options: AgentRuntimeOptions,
    messages: Vec<Message>,
    store: FileSessionStore,
    session: SessionState,
    streaming_assistant: Option<usize>,

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
            transcript: Vec::new(),
            input: InputLine::default(),
            should_quit: false,
            running: false,
            status: None,
            flush_requested: false,
            profile,
            runtime,
            model_id,
            options,
            messages,
            store,
            session,
            streaming_assistant: None,
            prompt_history,
            history_index: None,
            history_draft: String::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        self.session.meta.id.as_str()
    }

    pub fn agent_name(&self) -> &str {
        self.profile.name
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        if self.running && key.code == KeyCode::Enter {
            return None;
        }

        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
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
        self.transcript.push(ChatEntry {
            role: ChatRole::User,
            content: text.clone(),
            rendered: None,
        });

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
        self.streaming_assistant = None;
        Ok(self
            .runtime
            .run_stream(&self.profile, self.messages.clone(), self.options.clone())
            .await?)
    }

    pub async fn handle_agent_event(&mut self, event: AgentEvent) -> Result<()> {
        match event {
            AgentEvent::StepStart { step } => {
                self.status = Some(format!("step {step}"));
                self.streaming_assistant = None;
            }
            AgentEvent::AssistantDelta { delta } => {
                let idx = match self.streaming_assistant {
                    Some(idx) => idx,
                    None => {
                        self.transcript.push(ChatEntry {
                            role: ChatRole::Assistant,
                            content: String::new(),
                            rendered: None,
                        });
                        let idx = self.transcript.len().saturating_sub(1);
                        self.streaming_assistant = Some(idx);
                        idx
                    }
                };

                if let Some(entry) = self.transcript.get_mut(idx) {
                    entry.content.push_str(&delta);
                    entry.rendered = None;
                }
            }
            AgentEvent::AssistantMessage { message } => {
                self.store
                    .record_message(&mut self.session, message.clone())
                    .await?;

                if let Message::Assistant { content, .. } = message {
                    let content = content.unwrap_or_default();
                    if let Some(idx) = self.streaming_assistant {
                        if let Some(entry) = self.transcript.get_mut(idx) {
                            if !content.is_empty() {
                                entry.content = content;
                                entry.rendered = None;
                            }
                        }
                    } else if !content.is_empty() {
                        self.transcript.push(ChatEntry {
                            role: ChatRole::Assistant,
                            content,
                            rendered: None,
                        });
                    }
                }
            }
            AgentEvent::ToolCall { call } => {
                self.transcript.push(ChatEntry {
                    role: ChatRole::Tool,
                    content: format!("`{}` `{}`", call.name, call.arguments),
                    rendered: None,
                });
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
                    self.transcript.push(ChatEntry {
                        role: ChatRole::Tool,
                        content: format!(
                            "`tool_result` `{tool_call_id}`\n\n```text\n{content}\n```"
                        ),
                        rendered: None,
                    });
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
                self.streaming_assistant = None;
                self.flush_requested = true;
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
        self.transcript.push(ChatEntry {
            role: ChatRole::Info,
            content: format!("error: {text}"),
            rendered: None,
        });
        self.running = false;
        self.status = Some("error".to_string());
        self.streaming_assistant = None;
        self.flush_requested = true;
        Ok(())
    }

    pub fn flush_transcript_to_history(&mut self, width: usize) -> Vec<Line<'static>> {
        use ratatui::style::Stylize;
        use ratatui::style::{Color, Style};
        use ratatui::text::Span;

        let mut out: Vec<Line<'static>> = Vec::new();

        for (idx, entry) in self.transcript.iter_mut().enumerate() {
            if idx > 0 {
                out.push(Line::from(""));
            }

            let (label, color) = match entry.role {
                ChatRole::User => ("User", Color::Green),
                ChatRole::Assistant => ("Assistant", Color::Cyan),
                ChatRole::Tool => ("Tool", Color::Yellow),
                ChatRole::Info => ("Info", Color::Magenta),
            };

            let header = Line::from(vec![Span::styled(
                format!("{label}:"),
                Style::default().fg(color).bold(),
            )]);

            let body = entry
                .rendered
                .get_or_insert_with(|| crate::markdown::render_markdown_lines(&entry.content));

            out.extend(crate::wrap::wrap_lines(
                std::slice::from_ref(&header),
                width,
            ));
            out.extend(crate::wrap::wrap_lines(body, width));
        }

        self.transcript.clear();
        out
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
