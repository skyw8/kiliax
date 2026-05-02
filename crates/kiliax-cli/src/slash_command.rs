#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    New,
    Dir,
    Model,
    Agent,
    Mcp,
    Compact,
}

impl SlashCommand {
    pub fn command(self) -> &'static str {
        match self {
            SlashCommand::New => "new",
            SlashCommand::Dir => "dir",
            SlashCommand::Model => "model",
            SlashCommand::Agent => "agent",
            SlashCommand::Mcp => "mcp",
            SlashCommand::Compact => "compact",
        }
    }

    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            SlashCommand::New => &[],
            SlashCommand::Dir => &[],
            SlashCommand::Model => &[],
            SlashCommand::Agent => &["a"],
            SlashCommand::Mcp => &[],
            SlashCommand::Compact => &[],
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::New => "start a new session",
            SlashCommand::Dir => "add/list extra workspace dirs",
            SlashCommand::Model => "choose provider/model",
            SlashCommand::Agent => "switch agent (plan/general)",
            SlashCommand::Mcp => "toggle MCP servers",
            SlashCommand::Compact => "compact conversation context",
        }
    }

    pub fn takes_args(self) -> bool {
        matches!(self, SlashCommand::Agent | SlashCommand::Dir)
    }
}

pub fn all_commands() -> &'static [SlashCommand] {
    // Order is presentation order in the popup.
    &[
        SlashCommand::New,
        SlashCommand::Dir,
        SlashCommand::Model,
        SlashCommand::Agent,
        SlashCommand::Mcp,
        SlashCommand::Compact,
    ]
}

pub fn find_command(name: &str) -> Option<SlashCommand> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    for cmd in all_commands() {
        if name.eq_ignore_ascii_case(cmd.command()) {
            return Some(*cmd);
        }
        if cmd.aliases().iter().any(|a| name.eq_ignore_ascii_case(a)) {
            return Some(*cmd);
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct SlashPopupState {
    visible: bool,
    query: String,
    items: Vec<SlashCommand>,
    selected: usize,
}

impl SlashPopupState {
    pub fn visible(&self) -> bool {
        self.visible && !self.items.is_empty()
    }

    pub fn items(&self) -> &[SlashCommand] {
        &self.items
    }

    pub fn selected(&self) -> Option<SlashCommand> {
        if !self.visible() {
            return None;
        }
        self.items.get(self.selected).copied()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.query.clear();
        self.items.clear();
        self.selected = 0;
    }

    pub fn move_up(&mut self) {
        if !self.visible() || self.items.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.items.len().saturating_sub(1);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    pub fn move_down(&mut self) {
        if !self.visible() || self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    pub fn completion_text(&self) -> Option<String> {
        let cmd = self.selected()?;
        let suffix = if cmd.takes_args() { " " } else { "" };
        Some(format!("/{0}{suffix}", cmd.command()))
    }

    pub fn desired_height(&self, max_items: usize) -> u16 {
        if !self.visible() {
            return 0;
        }
        self.items.len().min(max_items).min(u16::MAX as usize) as u16
    }

    pub fn sync_from_input(&mut self, text: &str, cursor: usize) {
        let Some(query) = slash_query_for_popup(text, cursor) else {
            self.hide();
            return;
        };

        let next_items = filtered_commands(&query);
        if next_items.is_empty() {
            self.hide();
            return;
        }

        let query_changed = query != self.query;
        self.visible = true;
        self.query = query;
        self.items = next_items;
        if query_changed || self.selected >= self.items.len() {
            self.selected = 0;
        }
    }
}

fn filtered_commands(query: &str) -> Vec<SlashCommand> {
    let query = query.trim();
    if query.is_empty() {
        return all_commands().to_vec();
    }

    let mut out = Vec::new();
    for cmd in all_commands() {
        if cmd.command().starts_with(query) {
            out.push(*cmd);
            continue;
        }
        if cmd.aliases().iter().any(|a| a.starts_with(query)) {
            out.push(*cmd);
        }
    }
    out
}

fn slash_query_for_popup(text: &str, cursor: usize) -> Option<String> {
    let first_line = text.lines().next().unwrap_or("");
    let first_line_chars = first_line.chars().count();
    if cursor > first_line_chars {
        return None;
    }

    let trimmed = first_line.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    if rest.chars().any(|ch| ch.is_whitespace()) {
        return None;
    }

    // If there's another slash (e.g. /usr/bin), treat it as a normal message/path.
    if rest.contains('/') {
        return None;
    }

    Some(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_command_matches_alias() {
        assert_eq!(find_command("new"), Some(SlashCommand::New));
        assert_eq!(find_command("dir"), Some(SlashCommand::Dir));
        assert_eq!(find_command("agent"), Some(SlashCommand::Agent));
        assert_eq!(find_command("a"), Some(SlashCommand::Agent));
        assert_eq!(find_command("model"), Some(SlashCommand::Model));
        assert_eq!(find_command("mcp"), Some(SlashCommand::Mcp));
        assert_eq!(find_command("compact"), Some(SlashCommand::Compact));
        assert_eq!(find_command("unknown"), None);
    }

    #[test]
    fn popup_hides_for_paths() {
        let mut popup = SlashPopupState::default();
        popup.sync_from_input("/usr/bin", 8);
        assert!(!popup.visible());
    }
}
