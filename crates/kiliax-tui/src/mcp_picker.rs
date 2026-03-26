use crossterm::event::{KeyCode, KeyEvent};

use kiliax_core::tools::{McpServerConnectionState, McpServerStatus};

#[derive(Debug, Clone)]
pub enum McpPickerEvent {
    None,
    Cancel,
    Toggle { server: String, enable: bool },
}

#[derive(Debug, Clone)]
pub struct McpPicker {
    servers: Vec<McpServerStatus>,
    cursor: usize,
}

impl McpPicker {
    pub fn new(servers: Vec<McpServerStatus>) -> Self {
        let cursor = if servers.is_empty() { 0 } else { 0 };
        Self { servers, cursor }
    }

    pub fn servers(&self) -> &[McpServerStatus] {
        &self.servers
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_servers(&mut self, servers: Vec<McpServerStatus>) {
        let selected_name = self
            .servers
            .get(self.cursor)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.servers = servers;
        if self.servers.is_empty() {
            self.cursor = 0;
            return;
        }
        if !selected_name.is_empty() {
            if let Some(pos) = self.servers.iter().position(|s| s.name == selected_name) {
                self.cursor = pos;
                return;
            }
        }
        self.cursor = self.cursor.min(self.servers.len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> McpPickerEvent {
        match key.code {
            KeyCode::Esc => return McpPickerEvent::Cancel,
            KeyCode::Up => {
                self.move_up();
                return McpPickerEvent::None;
            }
            KeyCode::Down => {
                self.move_down();
                return McpPickerEvent::None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let Some(server) = self.servers.get(self.cursor) else {
                    return McpPickerEvent::None;
                };
                let enabled = !matches!(server.state, McpServerConnectionState::Disabled);
                return McpPickerEvent::Toggle {
                    server: server.name.clone(),
                    enable: !enabled,
                };
            }
            _ => {}
        }

        McpPickerEvent::None
    }

    fn move_up(&mut self) {
        if self.servers.is_empty() {
            return;
        }
        if self.cursor == 0 {
            self.cursor = self.servers.len().saturating_sub(1);
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    fn move_down(&mut self) {
        if self.servers.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1) % self.servers.len();
    }
}
