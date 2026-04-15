use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use tokio_stream::wrappers::ReceiverStream;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kiliax_core::{
    agents::AgentProfile,
    config::Config,
    mcp_overrides::{config_with_session_mcp_overrides, session_mcp_servers_from_config},
    protocol::{Message, UserContentPart, UserMessageContent},
    runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions},
    session::{FileSessionStore, SessionMcpServerSetting, SessionState},
};

use crate::clipboard_paste;
use crate::input::{InputAction, InputLine};
use crate::mcp_picker::{McpPicker, McpPickerEvent};
use crate::model_picker::{ModelPicker, ModelPickerEvent};
use crate::slash_command::SlashPopupState;

mod stream;
mod tokens;
mod render;
mod submissions;
mod history_nav;
mod preamble;

pub use submissions::PendingImage;
pub(crate) use submissions::QueuedSubmission;

use stream::{MarkdownStreamCollector, ThinkingStreamCollector};
use tokens::OutputTokenCounter;
use render::{
    classify_tool_call, format_error_chain_text, render_dir_list_lines, render_error_lines,
    render_thinking_start_line, render_token_usage_line, render_tool_result_fallback_lines,
    render_tool_result_lines, render_user_message_lines, tool_status_label,
    turn_divider_marker,
};
use preamble::{build_preamble, preamble_updates};

#[derive(Debug, Clone)]
pub enum AppAction {
    None,
    Submitted(String),
    ModelPicked(String),
    McpToggled { server: String, enable: bool },
}

impl Default for AppAction {
    fn default() -> Self {
        AppAction::None
    }
}

#[derive(Debug)]
enum UiMode {
    Chat,
    ModelPicker(ModelPicker),
    McpPicker(McpPicker),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitDisposition {
    Handled,
    StartRun,
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
    ReadFile {
        path: String,
    },
    ListDir {
        path: String,
    },
    GrepFiles {
        pattern: String,
        path: Option<String>,
    },
    ViewImage {
        path: String,
    },
    ShellCommand {
        argv: Vec<String>,
        cwd: Option<String>,
    },
    WriteStdin {
        session_id: u64,
    },
    ApplyPatch {
        files: Vec<String>,
    },
    UpdatePlan {
        steps: usize,
    },
    Other,
}

pub struct App {
    pub input: InputLine,
    pub should_quit: bool,
    pub running: bool,
    pub status: Option<String>,
    screen_width: u16,
    pending_history_lines: Vec<Line<'static>>,

    ui_mode: UiMode,
    slash_popup: SlashPopupState,

    workspace_root: PathBuf,
    extra_workspace_roots: Vec<PathBuf>,
    config_path: PathBuf,
    config: kiliax_core::config::Config,

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

    next_image_placeholder_id: u64,
    pending_images: Vec<PendingImage>,
    queued_submissions: VecDeque<QueuedSubmission>,
}

impl App {
    pub fn new(
        profile: AgentProfile,
        runtime: AgentRuntime,
        options: AgentRuntimeOptions,
        store: FileSessionStore,
        session: SessionState,
        messages: Vec<Message>,
        workspace_root: PathBuf,
        extra_workspace_roots: Vec<PathBuf>,
        config_path: PathBuf,
        config: kiliax_core::config::Config,
    ) -> Self {
        let model_id = runtime.llm().route().model_id();
        let prompt_history: Vec<String> = messages
            .iter()
            .filter_map(|msg| match msg {
                Message::User { content } => content.first_text().map(|t| t.to_string()),
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
            ui_mode: UiMode::Chat,
            slash_popup: SlashPopupState::default(),
            workspace_root,
            extra_workspace_roots,
            config_path,
            config,
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
            next_image_placeholder_id: 1,
            pending_images: Vec::new(),
            queued_submissions: VecDeque::new(),
        }
    }

    fn session_mcp_servers(&self) -> Vec<SessionMcpServerSetting> {
        if self.session.meta.mcp_servers.is_empty() {
            session_mcp_servers_from_config(&self.config)
        } else {
            self.session.meta.mcp_servers.clone()
        }
    }

    fn session_config(&self) -> Result<Config> {
        Ok(config_with_session_mcp_overrides(
            &self.config,
            &self.session_mcp_servers(),
        )?)
    }

    pub fn has_user_messages(&self) -> bool {
        self.session
            .messages
            .iter()
            .any(|msg| matches!(msg, Message::User { .. }))
    }

    pub async fn cleanup_empty_session(&self) {
        if self.has_user_messages() {
            return;
        }
        let _ = self.store.delete(self.session.id()).await;
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn agent_name(&self) -> &str {
        self.profile.name
    }

    pub fn set_screen_width(&mut self, width: u16) {
        self.screen_width = width;
    }

    pub fn slash_popup(&self) -> &SlashPopupState {
        &self.slash_popup
    }

    pub fn model_picker(&self) -> Option<&ModelPicker> {
        match &self.ui_mode {
            UiMode::ModelPicker(picker) => Some(picker),
            UiMode::Chat | UiMode::McpPicker(_) => None,
        }
    }

    pub fn mcp_picker(&self) -> Option<&McpPicker> {
        match &self.ui_mode {
            UiMode::McpPicker(picker) => Some(picker),
            UiMode::Chat | UiMode::ModelPicker(_) => None,
        }
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
            self.pending_history_lines
                .extend(stream.finalize_and_drain());
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

    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        match &mut self.ui_mode {
            UiMode::Chat => self.handle_chat_key(key),
            UiMode::ModelPicker(picker) => match picker.handle_key(key) {
                ModelPickerEvent::None => AppAction::None,
                ModelPickerEvent::Cancel => {
                    self.ui_mode = UiMode::Chat;
                    AppAction::None
                }
                ModelPickerEvent::Picked(id) => {
                    self.ui_mode = UiMode::Chat;
                    AppAction::ModelPicked(id)
                }
            },
            UiMode::McpPicker(picker) => match picker.handle_key(key) {
                McpPickerEvent::None => AppAction::None,
                McpPickerEvent::Cancel => {
                    self.ui_mode = UiMode::Chat;
                    AppAction::None
                }
                McpPickerEvent::Toggle { server, enable } => {
                    AppAction::McpToggled { server, enable }
                }
            },
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) -> AppAction {
        if key.code == KeyCode::Esc {
            if self.slash_popup.visible() {
                self.slash_popup.hide();
            } else {
                self.should_quit = true;
            }
            return AppAction::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.pop_last_queued_submission().is_some() {
                self.reset_history_nav();
                self.slash_popup.hide();
                return AppAction::None;
            }
            self.input.clear();
            self.clear_pending_images();
            self.reset_history_nav();
            self.slash_popup.hide();
            return AppAction::None;
        }

        if key.code == KeyCode::Char('v')
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            match clipboard_paste::paste_image_to_temp_png() {
                Ok(path) => {
                    if let Err(err) = self.attach_image(path) {
                        self.pending_history_lines
                            .extend(render_error_lines(&err.to_string()));
                    }
                }
                Err(err) => {
                    self.pending_history_lines
                        .extend(render_error_lines(&format!("failed to paste image: {err}")));
                }
            }
            self.slash_popup
                .sync_from_input(self.input.text(), self.input.cursor());
            return AppAction::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
            self.clear_pending_images();
        }

        if self.slash_popup.visible() {
            match key.code {
                KeyCode::Up => {
                    self.slash_popup.move_up();
                    return AppAction::None;
                }
                KeyCode::Down => {
                    self.slash_popup.move_down();
                    return AppAction::None;
                }
                KeyCode::Tab => {
                    if let Some(text) = self.slash_popup.completion_text() {
                        self.input.set_text(text);
                        self.ensure_pending_image_placeholders();
                        self.reset_history_nav();
                    }
                    self.slash_popup.hide();
                    return AppAction::None;
                }
                KeyCode::Enter => {
                    let Some(selected) = self.slash_popup.selected() else {
                        self.slash_popup.hide();
                        return AppAction::None;
                    };

                    if selected.takes_args() {
                        if let Some(text) = self.slash_popup.completion_text() {
                            self.input.set_text(text);
                            self.ensure_pending_image_placeholders();
                            self.reset_history_nav();
                        }
                        self.slash_popup.hide();
                        return AppAction::None;
                    }

                    // Dispatch bare commands like `/model` directly from the popup.
                    self.slash_popup.hide();
                    self.input.clear();
                    let text = format!("/{}", selected.command());
                    if self.running {
                        self.enqueue_submission(text, Vec::new());
                        self.ensure_pending_image_placeholders();
                        self.slash_popup
                            .sync_from_input(self.input.text(), self.input.cursor());
                        return AppAction::None;
                    }
                    self.ensure_pending_image_placeholders();
                    self.slash_popup
                        .sync_from_input(self.input.text(), self.input.cursor());
                    return AppAction::Submitted(text);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Up => {
                self.clear_pending_images();
                self.history_prev();
                self.slash_popup
                    .sync_from_input(self.input.text(), self.input.cursor());
                return AppAction::None;
            }
            KeyCode::Down => {
                self.clear_pending_images();
                self.history_next();
                self.slash_popup
                    .sync_from_input(self.input.text(), self.input.cursor());
                return AppAction::None;
            }
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {
                if self.history_index.is_some() {
                    self.reset_history_nav();
                }
            }
            _ => {}
        }

        let key_code = key.code;
        match self.input.handle_key(key) {
            InputAction::None => {
                if matches!(key_code, KeyCode::Backspace | KeyCode::Delete) {
                    self.prune_pending_images_missing_placeholders();
                }
            }
            InputAction::Submit(text) => {
                let text = self.strip_image_placeholders_from_text(&text);
                let text = text.trim().to_string();
                if !text.is_empty() || !self.pending_images.is_empty() {
                    self.slash_popup.hide();
                    if parse_known_slash_command(&text).is_some() {
                        self.ensure_pending_image_placeholders();
                    }
                    if self.running {
                        if parse_known_slash_command(&text).is_some() {
                            self.enqueue_submission(text, Vec::new());
                            self.slash_popup
                                .sync_from_input(self.input.text(), self.input.cursor());
                            return AppAction::None;
                        }

                        let images = std::mem::take(&mut self.pending_images);
                        self.enqueue_submission(text, images);
                        return AppAction::None;
                    }
                    self.slash_popup
                        .sync_from_input(self.input.text(), self.input.cursor());
                    return AppAction::Submitted(text);
                }
            }
        }
        self.slash_popup
            .sync_from_input(self.input.text(), self.input.cursor());
        AppAction::None
    }

    pub fn handle_paste(&mut self, text: &str) {
        match &mut self.ui_mode {
            UiMode::Chat => {
                if self.history_index.is_some() {
                    self.reset_history_nav();
                }
                if let Some(path) = clipboard_paste::normalize_pasted_path(text) {
                    if clipboard_paste::is_probably_image_path(&path) && path.is_file() {
                        if let Err(err) = self.attach_image(path) {
                            self.pending_history_lines
                                .extend(render_error_lines(&err.to_string()));
                        }
                    } else {
                        self.input.insert_str(text);
                    }
                } else {
                    self.input.insert_str(text);
                }
                self.slash_popup
                    .sync_from_input(self.input.text(), self.input.cursor());
            }
            UiMode::ModelPicker(picker) => picker.handle_paste(text),
            UiMode::McpPicker(_) => {}
        }
    }

    pub async fn handle_submit(&mut self, text: String) -> Result<SubmitDisposition> {
        if let Some((cmd, args)) = parse_known_slash_command(&text) {
            self.handle_known_slash_command(cmd, args).await?;
            self.ensure_pending_image_placeholders();
            return Ok(SubmitDisposition::Handled);
        }

        let images = std::mem::take(&mut self.pending_images);
        self.submit_user_message(text, images, true).await?;
        Ok(SubmitDisposition::StartRun)
    }

    pub async fn handle_queued_submission(
        &mut self,
        queued: QueuedSubmission,
    ) -> Result<SubmitDisposition> {
        if let Some((cmd, args)) = parse_known_slash_command(&queued.text) {
            self.handle_known_slash_command(cmd, args).await?;
            self.ensure_pending_image_placeholders();
            return Ok(SubmitDisposition::Handled);
        }

        self.submit_user_message(queued.text, queued.images, false)
            .await?;
        Ok(SubmitDisposition::StartRun)
    }

    async fn handle_known_slash_command(
        &mut self,
        cmd: crate::slash_command::SlashCommand,
        args: String,
    ) -> Result<()> {
        match cmd {
            crate::slash_command::SlashCommand::New => {
                if !args.trim().is_empty() {
                    self.pending_history_lines
                        .extend(render_error_lines("usage: /new"));
                    return Ok(());
                }
                if let Err(err) = self.start_new_session().await {
                    let text = format_error_chain_text(err.as_ref());
                    tracing::error!(
                        event = "tui.session.new_error",
                        session_id = %self.session.id(),
                        error = %text,
                    );
                    self.pending_history_lines.extend(render_error_lines(&text));
                }
            }
            crate::slash_command::SlashCommand::Model => {
                self.open_model_picker(args);
            }
            crate::slash_command::SlashCommand::Dir => {
                if args.trim().is_empty() {
                    self.pending_history_lines.extend(render_dir_list_lines(
                        &self.workspace_root,
                        &self.extra_workspace_roots,
                    ));
                    return Ok(());
                }
                if let Err(err) = self.add_extra_workspace_root(args.trim()).await {
                    let text = format_error_chain_text(err.as_ref());
                    tracing::error!(
                        event = "tui.dir.add_error",
                        session_id = %self.session.id(),
                        error = %text,
                    );
                    self.pending_history_lines.extend(render_error_lines(&text));
                }
            }
            crate::slash_command::SlashCommand::Agent => {
                let agent = args.split_whitespace().next().unwrap_or("");
                if agent.is_empty() {
                    self.pending_history_lines.extend(render_error_lines(
                        "usage: /agent <plan|general> (alias: /a)",
                    ));
                    return Ok(());
                }
                if let Err(err) = self.switch_agent(agent).await {
                    let text = format_error_chain_text(err.as_ref());
                    tracing::error!(
                        event = "tui.agent.switch_error",
                        session_id = %self.session.id(),
                        error = %text,
                    );
                    self.pending_history_lines.extend(render_error_lines(&text));
                }
            }
            crate::slash_command::SlashCommand::Mcp => {
                if !args.trim().is_empty() {
                    self.pending_history_lines
                        .extend(render_error_lines("usage: /mcp"));
                    return Ok(());
                }
                self.open_mcp_picker().await;
            }
        }
        Ok(())
    }

    async fn open_mcp_picker(&mut self) {
        self.slash_popup.hide();
        let statuses = self.runtime.tools().mcp_status().await;
        self.ui_mode = UiMode::McpPicker(McpPicker::new(statuses));
    }

    async fn add_extra_workspace_root(&mut self, input: &str) -> Result<()> {
        if self.running {
            anyhow::bail!("cannot modify dirs while a run is in progress");
        }

        let candidate = validate_extra_workspace_root(input)?;

        let main = std::fs::canonicalize(&self.workspace_root)
            .unwrap_or_else(|_| self.workspace_root.clone());
        if candidate == main {
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("dir").bold(),
                Span::from(": ").dim(),
                Span::from(candidate.display().to_string()).dim(),
            ]));
            return Ok(());
        }
        if self.extra_workspace_roots.contains(&candidate) {
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("dir").bold(),
                Span::from(": ").dim(),
                Span::from(candidate.display().to_string()).dim(),
            ]));
            return Ok(());
        }

        self.extra_workspace_roots.push(candidate.clone());
        self.runtime
            .tools()
            .set_extra_workspace_roots(self.extra_workspace_roots.clone())
            .map_err(|e| anyhow::anyhow!(e))?;

        self.session.meta.extra_workspace_roots = self
            .extra_workspace_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        self.session.meta.updated_at_ms = now_ms();

        self.store.checkpoint(&mut self.session).await?;

        self.pending_history_lines.push(Line::from(vec![
            Span::from("• ").dim(),
            Span::from("dir").bold(),
            Span::from(": ").dim(),
            Span::from("+ ").dim(),
            Span::from(candidate.display().to_string()).dim(),
        ]));

        tracing::info!(
            event = "tui.dir.added",
            session_id = %self.session.id(),
            dir = %candidate.display(),
        );

        Ok(())
    }

    async fn start_new_session(&mut self) -> Result<()> {
        if self.running {
            anyhow::bail!("cannot start a new session while a run is in progress");
        }

        self.extra_workspace_roots.clear();
        self.runtime
            .tools()
            .set_extra_workspace_roots(Vec::new())
            .map_err(|e| anyhow::anyhow!(e))?;

        let prev_id = self.session.meta.id.clone();
        let prev_empty = !self.has_user_messages();
        let prev = prev_id.to_string();
        let preamble = build_preamble(
            &self.profile,
            &self.model_id,
            &self.workspace_root,
            self.runtime.tools(),
            &self.config.skills,
        )
        .await;
        let session = self
            .store
            .create(
                self.profile.name.to_string(),
                Some(self.model_id.clone()),
                Some(self.config_path.display().to_string()),
                Some(self.workspace_root.display().to_string()),
                self.extra_workspace_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                preamble,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let next = session.meta.id.to_string();
        self.session = session;
        self.runtime = AgentRuntime::new(
            self.runtime
                .llm()
                .clone()
                .with_prompt_cache_key(self.session.meta.prompt_cache_key.clone()),
            self.runtime.tools().clone(),
        );
        self.messages = self.session.messages.clone();
        self.prompt_history = self
            .messages
            .iter()
            .filter_map(|msg| match msg {
                Message::User { content } => content.first_text().map(|t| t.to_string()),
                _ => None,
            })
            .collect();
        self.reset_history_nav();

        self.status = None;
        self.assistant_stream = None;
        self.assistant_thinking_stream = None;
        self.accepting_thinking = false;
        self.turn_started_at = None;
        self.step_started_at = None;
        self.current_step = None;
        self.pending_tool_calls.clear();
        self.turn_output_tokens.reset();
        self.step_output_tokens.reset();
        self.saw_delta_in_step = false;

        self.next_image_placeholder_id = 1;
        self.clear_pending_images();

        self.ui_mode = UiMode::Chat;
        self.slash_popup.hide();

        self.pending_history_lines.push(Line::from(vec![
            Span::from("• ").dim(),
            Span::from("session").bold(),
            Span::from(": ").dim(),
            Span::from(prev).dim(),
            Span::from(" -> ").dim(),
            Span::from(next).dim(),
        ]));

        if prev_empty {
            let _ = self.store.delete(&prev_id).await;
        }

        tracing::info!(
            event = "tui.session.new",
            from = %prev_id,
            to = %self.session.id(),
            agent = %self.agent_name(),
            model_id = %self.model_id(),
            prev_empty,
        );
        Ok(())
    }

    async fn submit_user_message(
        &mut self,
        text: String,
        images: Vec<PendingImage>,
        record_prompt_history: bool,
    ) -> Result<()> {
        let text = text.trim().to_string();
        if record_prompt_history && !text.is_empty() {
            self.prompt_history.push(text.clone());
        }
        if record_prompt_history {
            self.reset_history_nav();
        }

        let attachments_dir = self
            .store
            .session_dir(&self.session.meta.id)
            .join("attachments");
        if !images.is_empty() {
            tokio::fs::create_dir_all(&attachments_dir).await?;
        }

        let ts = now_ms();
        let mut parts: Vec<UserContentPart> = Vec::new();
        if !text.is_empty() {
            parts.push(UserContentPart::Text { text });
        }

        for (idx, img) in images.iter().enumerate() {
            let src = &img.source_path;
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png")
                .trim();
            let filename = format!(
                "img_{ts}_{idx}.{}",
                if ext.is_empty() { "png" } else { ext }
            );
            let dest = attachments_dir.join(filename);

            let path_for_message = if src.starts_with(&attachments_dir) || src == &dest {
                src.clone()
            } else {
                match tokio::fs::copy(src, &dest).await {
                    Ok(_) => dest,
                    Err(err) => {
                        self.pending_history_lines
                            .extend(render_error_lines(&format!(
                                "failed to copy image `{}`: {err}",
                                src.display()
                            )));
                        src.clone()
                    }
                }
            };

            let rel = path_for_message
                .strip_prefix(&self.workspace_root)
                .unwrap_or(path_for_message.as_path());
            let path = rel.to_string_lossy().replace('\\', "/");
            parts.push(UserContentPart::Image { path, detail: None });
        }

        let content = if parts.len() == 1 {
            match parts.pop().unwrap() {
                UserContentPart::Text { text } => UserMessageContent::Text(text),
                part => UserMessageContent::Parts(vec![part]),
            }
        } else {
            UserMessageContent::Parts(parts)
        };

        let display_text = content.display_text();
        self.pending_history_lines
            .extend(render_user_message_lines(&display_text));

        let msg = Message::User { content };
        self.messages.push(msg.clone());
        self.store.record_message(&mut self.session, msg).await?;
        Ok(())
    }

    fn open_model_picker(&mut self, initial_query: String) {
        self.slash_popup.hide();
        let mut picker = ModelPicker::new(&self.config, Some(self.model_id.as_str()));
        if !initial_query.trim().is_empty() {
            picker.set_search_text(initial_query);
        }
        self.ui_mode = UiMode::ModelPicker(picker);
    }

    async fn switch_agent(&mut self, agent_name: &str) -> Result<()> {
        let Some(next) = AgentProfile::from_name(agent_name) else {
            anyhow::bail!("unknown agent: {agent_name:?} (expected plan/general)");
        };

        let prev = self.profile.name.to_string();
        if prev == next.name {
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("agent").bold(),
                Span::from(": ").dim(),
                Span::from(prev).dim(),
            ]));
            return Ok(());
        }

        self.profile = next;
        self.options = AgentRuntimeOptions::from_config(&self.profile, &self.config);
        self.session.meta.agent = self.profile.name.to_string();
        self.session.meta.updated_at_ms = now_ms();

        let skills_config = self
            .session
            .meta
            .skills
            .clone()
            .unwrap_or_else(|| self.config.skills.clone());
        let new_preamble = build_preamble(
            &self.profile,
            &self.model_id,
            &self.workspace_root,
            self.runtime.tools(),
            &skills_config,
        )
        .await;
        for msg in preamble_updates(self.session.messages.as_slice(), new_preamble) {
            self.store.record_message(&mut self.session, msg).await?;
        }
        self.messages = self.session.messages.clone();

        self.store.checkpoint(&mut self.session).await?;

        self.pending_history_lines.push(Line::from(vec![
            Span::from("• ").dim(),
            Span::from("agent").bold(),
            Span::from(": ").dim(),
            Span::from(prev.clone()).dim(),
            Span::from(" -> ").dim(),
            Span::from(self.profile.name.to_string()).dim(),
        ]));

        tracing::info!(
            event = "tui.agent.changed",
            session_id = %self.session.id(),
            from = %prev,
            to = %self.profile.name,
            model_id = %self.model_id(),
        );
        Ok(())
    }

    pub async fn apply_model_selection(&mut self, model_id: String) -> Result<()> {
        if let Err(err) = self.apply_model_selection_inner(model_id).await {
            let text = format_error_chain_text(err.as_ref());
            tracing::error!(
                event = "tui.model.switch_error",
                session_id = %self.session.id(),
                error = %text,
            );
            self.pending_history_lines.extend(render_error_lines(&text));
        }
        Ok(())
    }

    pub async fn apply_mcp_toggle(&mut self, server: String, enable: bool) -> Result<()> {
        if let Err(err) = self.apply_mcp_toggle_inner(server, enable).await {
            let text = format_error_chain_text(err.as_ref());
            tracing::error!(
                event = "tui.mcp.toggle_error",
                session_id = %self.session.id(),
                error = %text,
            );
            self.pending_history_lines.extend(render_error_lines(&text));
        }
        Ok(())
    }

    async fn apply_model_selection_inner(&mut self, model_id: String) -> Result<()> {
        let model_id = model_id.trim().to_string();
        if model_id.is_empty() {
            anyhow::bail!("model id must not be empty");
        }
        if self.running {
            anyhow::bail!("cannot switch model while a run is in progress");
        }

        let prev = self.model_id.clone();
        if prev == model_id {
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("model").bold(),
                Span::from(": ").dim(),
                Span::from(prev).dim(),
            ]));
            return Ok(());
        }

        let prev_runtime = self.runtime.clone();
        let prev_model_id = self.model_id.clone();
        let prev_options = self.options.clone();
        let prev_session = self.session.clone();
        let prev_messages = self.messages.clone();

        let res: Result<()> = async {
            self.config.resolve_model(&model_id)?;
            let session_config = self.session_config()?;
            let llm = kiliax_core::llm::LlmClient::from_config(&session_config, Some(&model_id))?
                .with_prompt_cache_key(self.session.meta.prompt_cache_key.clone());
            let tools = self.runtime.tools().clone();
            tools
                .set_config(session_config)
                .map_err(|e| anyhow::anyhow!(e))?;
            self.runtime = AgentRuntime::new(llm, tools);
            self.model_id = self.runtime.llm().route().model_id();
            self.options = AgentRuntimeOptions::from_config(&self.profile, &self.config);

            self.session.meta.model_id = Some(self.model_id.clone());
            self.session.meta.updated_at_ms = now_ms();

            let skills_config = self
                .session
                .meta
                .skills
                .clone()
                .unwrap_or_else(|| self.config.skills.clone());
            let new_preamble = build_preamble(
                &self.profile,
                &self.model_id,
                &self.workspace_root,
                self.runtime.tools(),
                &skills_config,
            )
            .await;
            for msg in preamble_updates(self.session.messages.as_slice(), new_preamble) {
                self.store.record_message(&mut self.session, msg).await?;
            }
            self.messages = self.session.messages.clone();

            let _ = self.store.checkpoint(&mut self.session).await;

            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("model").bold(),
                Span::from(": ").dim(),
                Span::from(prev.clone()).dim(),
                Span::from(" -> ").dim(),
                Span::from(self.model_id.clone()).dim(),
            ]));

            tracing::info!(
                event = "tui.model.changed",
                session_id = %self.session.id(),
                from = %prev,
                to = %self.model_id(),
            );
            Ok(())
        }
        .await;

        if let Err(err) = res {
            self.runtime = prev_runtime;
            self.model_id = prev_model_id;
            self.options = prev_options;
            self.session = prev_session;
            self.messages = prev_messages;
            if let Ok(cfg) = self.session_config() {
                let _ = self.runtime.tools().set_config(cfg);
            }
            return Err(err);
        }

        Ok(())
    }

    async fn apply_mcp_toggle_inner(&mut self, server: String, enable: bool) -> Result<()> {
        let server = server.trim().to_string();
        if server.is_empty() {
            anyhow::bail!("mcp server name must not be empty");
        }
        if self.running {
            anyhow::bail!("cannot toggle MCP servers while a run is in progress");
        }
        if !self.config.mcp.servers.iter().any(|s| s.name == server) {
            anyhow::bail!("unknown mcp server: {server:?}");
        }

        let prev_session = self.session.clone();
        let prev_messages = self.messages.clone();
        let mut next_servers = self.session_mcp_servers();
        let current = next_servers
            .iter()
            .find(|entry| entry.id == server)
            .map(|entry| entry.enable)
            .unwrap_or_else(|| {
                self.config
                    .mcp
                    .servers
                    .iter()
                    .find(|entry| entry.name == server)
                    .is_some_and(|entry| entry.enable)
            });
        if current == enable {
            let action = if enable { "enabled" } else { "disabled" };
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("mcp").bold(),
                Span::from(": ").dim(),
                Span::from(server).dim(),
                Span::from(" ").dim(),
                Span::from(action).dim(),
            ]));
            return Ok(());
        }
        if let Some(entry) = next_servers.iter_mut().find(|entry| entry.id == server) {
            entry.enable = enable;
        } else {
            next_servers.push(SessionMcpServerSetting {
                id: server.clone(),
                enable,
            });
        }

        let res: Result<()> = async {
            self.session.meta.mcp_servers = next_servers;
            let session_config = self.session_config()?;
            let tools = self.runtime.tools().clone();
            tools
                .set_config(session_config)
                .map_err(|e| anyhow::anyhow!(e))?;

            let statuses = tools.mcp_status().await;
            if let UiMode::McpPicker(picker) = &mut self.ui_mode {
                picker.set_servers(statuses);
            }

            self.session.meta.updated_at_ms = now_ms();

            let skills_config = self
                .session
                .meta
                .skills
                .clone()
                .unwrap_or_else(|| self.config.skills.clone());
            let new_preamble = build_preamble(
                &self.profile,
                &self.model_id,
                &self.workspace_root,
                self.runtime.tools(),
                &skills_config,
            )
            .await;
            for msg in preamble_updates(self.session.messages.as_slice(), new_preamble) {
                self.store.record_message(&mut self.session, msg).await?;
            }
            self.messages = self.session.messages.clone();
            let _ = self.store.checkpoint(&mut self.session).await;

            let action = if enable { "enabled" } else { "disabled" };
            self.pending_history_lines.push(Line::from(vec![
                Span::from("• ").dim(),
                Span::from("mcp").bold(),
                Span::from(": ").dim(),
                Span::from(server.clone()).dim(),
                Span::from(" ").dim(),
                Span::from(action).dim(),
            ]));

            tracing::info!(
                event = "tui.mcp.toggled",
                session_id = %self.session.id(),
                server = %server,
                enable,
            );
            Ok(())
        }
        .await;

        if let Err(err) = res {
            self.session = prev_session;
            self.messages = prev_messages;
            if let Ok(cfg) = self.session_config() {
                let _ = self.runtime.tools().set_config(cfg);
            }
            return Err(err);
        }

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
        tracing::info!(
            event = "tui.run.started",
            session_id = %self.session.id(),
            agent = %self.agent_name(),
            model_id = %self.model_id(),
            messages = self.messages.len() as u64,
        );
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

                if let Message::Assistant { content, usage, .. } = message {
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
                    if let Some(usage) = usage {
                        self.pending_history_lines
                            .push(render_token_usage_line(usage));
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

                match message {
                    Message::Tool {
                        tool_call_id,
                        content,
                    } => {
                        let started_at = self
                            .pending_tool_calls
                            .get(&tool_call_id)
                            .map(|pending| pending.started_at);
                        let elapsed = started_at.map(|t| t.elapsed());
                        let pending = self.pending_tool_calls.remove(&tool_call_id);
                        if let Some(pending) = pending {
                            self.pending_history_lines
                                .extend(render_tool_result_lines(&pending, elapsed, &content));
                        } else {
                            self.pending_history_lines
                                .extend(render_tool_result_fallback_lines(
                                    &tool_call_id,
                                    elapsed,
                                    &content,
                                ));
                        }
                    }
                    Message::User { content } => {
                        let display = content.display_text();
                        if !display.trim().is_empty() {
                            self.pending_history_lines
                                .extend(render_user_message_lines(&display));
                        }
                    }
                    _ => {}
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

                tracing::info!(
                    event = "tui.run.finished",
                    session_id = %self.session.id(),
                    agent = %self.agent_name(),
                    model_id = %self.model_id(),
                    steps = out.steps as u64,
                    finish_reason = ?out.finish_reason,
                );

                self.messages = out.messages;
                self.running = false;
                self.status = Some(format!(
                    "done (steps={}, reason={:?})",
                    out.steps, out.finish_reason
                ));
                if let Some(stream) = self.assistant_stream.as_mut() {
                    self.pending_history_lines
                        .extend(stream.finalize_and_drain());
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
        let text = format_error_chain_text(&err);
        tracing::error!(
            event = "tui.run.error",
            session_id = %self.session.id(),
            agent = %self.agent_name(),
            model_id = %self.model_id(),
            error = %text,
        );
        let _ = self
            .store
            .record_error(&mut self.session, text.clone())
            .await;
        self.close_thinking_stream();
        self.pending_history_lines.extend(render_error_lines(&text));
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
}

fn parse_known_slash_command(text: &str) -> Option<(crate::slash_command::SlashCommand, String)> {
    let first_line = text.lines().next().unwrap_or("");
    let trimmed = first_line.trim_start();
    let rest = trimmed.strip_prefix('/')?.trim_start();
    if rest.is_empty() {
        return None;
    }

    let mut split_idx: Option<usize> = None;
    for (idx, ch) in rest.char_indices() {
        if ch.is_whitespace() {
            split_idx = Some(idx);
            break;
        }
    }

    let (name, args) = match split_idx {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };

    let cmd = crate::slash_command::find_command(name)?;
    Some((cmd, args.to_string()))
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn validate_extra_workspace_root(input: &str) -> Result<PathBuf> {
    kiliax_core::paths::validate_existing_dir(input).map_err(|err| anyhow::anyhow!(err))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::render::summarize_shell_command_argv;
    use kiliax_core::config::ResolvedModel;
    use kiliax_core::config::{Config, ProviderConfig};
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
        assert_eq!(super::stream::soft_wrap_split_idx("hello world", 5), Some(5));
        assert_eq!(super::stream::soft_wrap_split_idx("a", 1), None);
        assert_eq!(super::stream::soft_wrap_split_idx("a", 0), None);

        // Wide characters (e.g. CJK) should wrap by display width.
        let idx = super::stream::soft_wrap_split_idx("你好啊", 3).unwrap();
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

    #[test]
    fn update_plan_tool_result_renders_as_checkbox_list() {
        let pending = PendingToolCall {
            name: "update_plan".to_string(),
            arguments: serde_json::json!({
                "plan": [
                    { "step": "one", "status": "pending" },
                    { "step": "two", "status": "in_progress" },
                    { "step": "three", "status": "completed" }
                ]
            })
            .to_string(),
            started_at: Instant::now(),
            kind: PendingToolCallKind::UpdatePlan { steps: 3 },
        };

        let lines = render_tool_result_lines(&pending, None, "");
        let text = lines.iter().map(plain).collect::<Vec<_>>().join("\n");

        assert!(text.contains("[] one"));
        assert!(text.contains("[] two"));
        assert!(text.contains("[] three"));

        // Should not leak internal status strings into the UI.
        assert!(!text.contains("pending"));
        assert!(!text.contains("in_progress"));
        assert!(!text.contains("completed"));

        let completed_line = lines
            .iter()
            .find(|line| plain(line).contains("[] three"))
            .expect("expected completed plan line");
        assert!(completed_line.spans.iter().any(|span| {
            let modifiers = span.style.add_modifier - span.style.sub_modifier;
            modifiers.contains(Modifier::CROSSED_OUT)
        }));
    }

    #[test]
    fn shell_command_summary_omits_wrapper_and_setup_steps() {
        let argv = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "cd /home/skywo/github/kiliax && rg -n shell_command crates/kiliax-cli/src | head -n 5"
                .to_string(),
        ];
        let out = summarize_shell_command_argv(&argv);
        assert_eq!(out, "rg -n shell_command crates/kiliax-cli/src | head -n 5");
    }

    #[test]
    fn shell_command_summary_strips_env_assignments() {
        let argv = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "FOO=bar BAR=baz rg -n shell_command crates/kiliax-cli/src".to_string(),
        ];
        let out = summarize_shell_command_argv(&argv);
        assert_eq!(out, "rg -n shell_command crates/kiliax-cli/src");
    }

    #[test]
    fn shell_command_summary_falls_back_to_plain_argv() {
        let argv = vec![
            "cargo".to_string(),
            "test".to_string(),
            "-p".to_string(),
            "kiliax-core".to_string(),
            "--".to_string(),
            "--nocapture".to_string(),
        ];
        let out = summarize_shell_command_argv(&argv);
        assert_eq!(out, "cargo test -p kiliax-core -- --nocapture");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn app_does_not_interleave_thinking_after_assistant_output_begins() {
        let tmp = tempfile::tempdir().unwrap();

        let store = FileSessionStore::new(tmp.path());
        let messages = vec![Message::User {
            content: UserMessageContent::Text("hi".to_string()),
        }];
        let session = store
            .create(
                "general",
                Some("p/m".to_string()),
                None,
                None,
                Vec::new(),
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

        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "p".to_string(),
            ProviderConfig {
                base_url: "https://example.com/v1".to_string(),
                api_key: None,
                models: vec!["m".to_string()],
            },
        );
        let config = Config {
            default_model: Some("p/m".to_string()),
            providers,
            ..Default::default()
        };

        let tools = ToolEngine::new(tmp.path(), config.clone());
        let runtime = AgentRuntime::new(llm, tools);

        let mut app = App::new(
            AgentProfile::general(),
            runtime,
            AgentRuntimeOptions::default(),
            store,
            session,
            messages,
            tmp.path().to_path_buf(),
            Vec::new(),
            tmp.path().join("kiliax.yaml"),
            config,
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

    #[test]
    fn session_mcp_servers_follow_config_defaults() {
        let mut config = Config::default();
        config.mcp.servers = vec![
            kiliax_core::config::McpServerConfig {
                name: "context7".to_string(),
                enable: true,
                command: "npx".to_string(),
                args: vec!["context7".to_string()],
            },
            kiliax_core::config::McpServerConfig {
                name: "filesystem".to_string(),
                enable: false,
                command: "npx".to_string(),
                args: vec!["filesystem".to_string()],
            },
        ];

        assert_eq!(
            session_mcp_servers_from_config(&config),
            vec![
                SessionMcpServerSetting {
                    id: "context7".to_string(),
                    enable: true,
                },
                SessionMcpServerSetting {
                    id: "filesystem".to_string(),
                    enable: false,
                },
            ]
        );
    }

    #[test]
    fn session_mcp_overrides_only_change_session_selected_servers() {
        let mut config = Config::default();
        config.mcp.servers = vec![
            kiliax_core::config::McpServerConfig {
                name: "context7".to_string(),
                enable: true,
                command: "npx".to_string(),
                args: vec!["context7".to_string()],
            },
            kiliax_core::config::McpServerConfig {
                name: "filesystem".to_string(),
                enable: false,
                command: "npx".to_string(),
                args: vec!["filesystem".to_string()],
            },
        ];

        let session_config = config_with_session_mcp_overrides(
            &config,
            &[SessionMcpServerSetting {
                id: "filesystem".to_string(),
                enable: true,
            }],
        )
        .unwrap();

        assert!(session_config
            .mcp
            .servers
            .iter()
            .find(|server| server.name == "context7")
            .is_some_and(|server| server.enable));
        assert!(session_config
            .mcp
            .servers
            .iter()
            .find(|server| server.name == "filesystem")
            .is_some_and(|server| server.enable));
        assert!(!config
            .mcp
            .servers
            .iter()
            .find(|server| server.name == "filesystem")
            .is_some_and(|server| server.enable));
    }
}
