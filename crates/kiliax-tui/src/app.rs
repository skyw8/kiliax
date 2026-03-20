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

    profile: AgentProfile,
    runtime: AgentRuntime,
    options: AgentRuntimeOptions,
    messages: Vec<Message>,
    store: FileSessionStore,
    session: SessionState,
    streaming_assistant: Option<usize>,
}

impl App {
    pub fn new(
        profile: AgentProfile,
        runtime: AgentRuntime,
        options: AgentRuntimeOptions,
        store: FileSessionStore,
        session: SessionState,
        messages: Vec<Message>,
        intro: String,
    ) -> Self {
        Self {
            transcript: vec![ChatEntry {
                role: ChatRole::Info,
                content: intro,
                rendered: None,
            }],
            input: InputLine::default(),
            should_quit: false,
            running: false,
            status: None,
            profile,
            runtime,
            options,
            messages,
            store,
            session,
            streaming_assistant: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        if self.running && key.code == KeyCode::Enter {
            return None;
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

    pub async fn submit_user_message(&mut self, text: String) -> Result<()> {
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
        Ok(())
    }
}
