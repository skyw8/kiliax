use std::path::{Path, PathBuf};

use crate::agents::AgentProfile;
use crate::protocol::{Message, ToolDefinition, UserMessageContent};
use crate::tools::skills::Skill;
use crate::tools::{tool_parallelism, ToolParallelism};

const CODEX_PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/codex.md"));
const SKILLS_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/how_to_use_skills.md"
));
const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";
const ENV_OPEN_TAG: &str = "<env>";
const ENV_CLOSE_TAG: &str = "</env>";
const SUBAGENTS_LINE: &str = "Subagents: not supported.";

#[derive(Debug, Clone)]
pub struct PromptBuilder {
    workspace_root: Option<PathBuf>,
    model_id: Option<String>,
    include_model_prompt: bool,
    agent_prompt: Option<String>,
    include_environment_prompt: bool,
    include_tools_prompt: bool,
    include_project_prompt: bool,
    tool_definitions: Vec<ToolDefinition>,
    skills: Vec<Skill>,
    messages: Vec<Message>,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            workspace_root: None,
            model_id: None,
            include_model_prompt: true,
            agent_prompt: None,
            include_environment_prompt: true,
            include_tools_prompt: true,
            include_project_prompt: true,
            tool_definitions: Vec::new(),
            skills: Vec::new(),
            messages: Vec::new(),
        }
    }

    pub fn for_agent(profile: &AgentProfile) -> Self {
        Self::new().with_agent_prompt(profile.developer_prompt)
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    pub fn with_agent_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.agent_prompt = Some(prompt.into());
        self
    }

    pub fn include_model_prompt(mut self, on: bool) -> Self {
        self.include_model_prompt = on;
        self
    }

    pub fn include_environment_prompt(mut self, on: bool) -> Self {
        self.include_environment_prompt = on;
        self
    }

    pub fn include_tools_prompt(mut self, on: bool) -> Self {
        self.include_tools_prompt = on;
        self
    }

    pub fn include_project_prompt(mut self, on: bool) -> Self {
        self.include_project_prompt = on;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tool_definitions = tools;
        self
    }

    pub fn add_skill(mut self, skill: Skill) -> Self {
        self.skills.push(skill);
        self
    }

    pub fn add_skills<I>(mut self, skills: I) -> Self
    where
        I: IntoIterator<Item = Skill>,
    {
        for s in skills {
            self = self.add_skill(s);
        }
        self
    }

    pub fn push_message(mut self, msg: Message) -> Self {
        self.messages.push(msg);
        self
    }

    pub fn push_user(mut self, content: impl Into<String>) -> Self {
        self.messages.push(Message::User {
            content: UserMessageContent::Text(content.into()),
        });
        self
    }

    pub fn extend_messages<I>(mut self, msgs: I) -> Self
    where
        I: IntoIterator<Item = Message>,
    {
        self.messages.extend(msgs);
        self
    }

    pub fn build(self) -> Vec<Message> {
        let mut out = Vec::new();

        if self.include_model_prompt {
            if let Some(prompt) = model_prompt_for(self.model_id.as_deref()) {
                // Use system role for maximum OpenAI-compatible coverage.
                out.push(Message::System {
                    content: prompt.to_string(),
                });
            }
        }

        if let Some(prompt) = self.agent_prompt {
            // Use system role for maximum OpenAI-compatible coverage.
            out.push(Message::System { content: prompt });
        }

        if self.include_tools_prompt {
            out.push(Message::System {
                content: render_tools_prompt(&self.tool_definitions),
            });
        }

        if let Some(skills) = render_skills_section(&self.skills) {
            out.push(Message::System { content: skills });
        }

        if self.include_project_prompt {
            if let Some(project) = render_project_prompt(self.workspace_root.as_deref()) {
                out.push(Message::System { content: project });
            }
        }

        if self.include_environment_prompt {
            out.push(Message::System {
                content: render_environment_prompt(
                    self.workspace_root.as_deref(),
                    self.model_id.as_deref(),
                ),
            });
        }

        out.extend(self.messages);
        out
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn model_prompt_for(model_id: Option<&str>) -> Option<&'static str> {
    let Some(model_id) = model_id else {
        return Some(CODEX_PROMPT);
    };

    let model = model_id.rsplit('/').next().unwrap_or(model_id).trim();
    if model.starts_with("gpt") {
        return Some(CODEX_PROMPT);
    }
    None
}

fn render_environment_prompt(workspace_root: Option<&Path>, model_id: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(root) = workspace_root {
        lines.push(format!("PWD: {}", root.display()));
    }

    lines.push(format!(
        "Platform: {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    if let Some(model_id) = model_id.map(str::trim).filter(|s| !s.is_empty()) {
        let provider = model_id
            .split_once('/')
            .map(|(p, _)| p.trim())
            .filter(|p| !p.is_empty())
            .unwrap_or("unknown");
        lines.push(format!("Provider: {provider}"));
        lines.push(format!("Model ID: {model_id}"));
    }

    lines.push(format!("Date: {}", today_ymd()));
    lines.push(SUBAGENTS_LINE.to_string());

    let body = lines.join("\n");
    format!("{ENV_OPEN_TAG}\n{body}\n{ENV_CLOSE_TAG}")
}

fn today_ymd() -> String {
    use time::{macros::format_description, OffsetDateTime};

    let date = OffsetDateTime::now_utc().date();
    date.format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_else(|_| date.to_string())
}

fn render_project_prompt(workspace_root: Option<&Path>) -> Option<String> {
    let root = workspace_root?;

    let sections: Vec<String> = project_instruction_paths(root)
        .into_iter()
        .filter_map(|path| {
            let Ok(content) = std::fs::read_to_string(&path) else {
                return None;
            };
            let content = content.trim();
            if content.is_empty() {
                return None;
            }
            Some(format!("## {}\n{content}", path.display()))
        })
        .collect();

    if sections.is_empty() {
        None
    } else {
        Some(format!("# Project Instructions\n{}", sections.join("\n\n")))
    }
}

fn project_instruction_paths(workspace_root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<&Path> = workspace_root.ancestors().collect();
    dirs.reverse();

    let mut paths = Vec::new();
    for dir in dirs {
        if let Some(path) = preferred_project_instruction_path(dir) {
            paths.push(path);
        }
    }
    paths
}

fn preferred_project_instruction_path(dir: &Path) -> Option<PathBuf> {
    for filename in ["AGENTS.md", "CLAUDE.md"] {
        let path = dir.join(filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn render_tools_prompt(tools: &[ToolDefinition]) -> String {
    let mut lines: Vec<String> = vec![
        "# Tool Use".to_string(),
        "You can call tools to inspect or modify the workspace.".to_string(),
        String::new(),
        "## Available Tools".to_string(),
    ];

    if tools.is_empty() {
        lines.push("- (none)".to_string());
    } else {
        for tool in tools {
            let parallelism = match tool_parallelism(tool.name.as_str()) {
                ToolParallelism::Parallel => "parallel",
                ToolParallelism::Exclusive => "serial",
            };
            let desc = tool
                .description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("No description.");
            lines.push(format!("- `{}` ({parallelism}): {desc}", tool.name));
        }
    }

    let parallel: Vec<String> = tools
        .iter()
        .filter(|t| tool_parallelism(t.name.as_str()) == ToolParallelism::Parallel)
        .map(|t| format!("`{}`", t.name))
        .collect();
    let exclusive: Vec<String> = tools
        .iter()
        .filter(|t| tool_parallelism(t.name.as_str()) == ToolParallelism::Exclusive)
        .map(|t| format!("`{}`", t.name))
        .collect();

    lines.push(String::new());
    lines.push("## Parallel Tool Calls".to_string());
    if !parallel.is_empty() {
        lines.push(format!("- Parallel-safe: {}", parallel.join(", ")));
    }
    if !exclusive.is_empty() {
        lines.push(format!(
            "- Serial-only (won't run concurrently): {}",
            exclusive.join(", ")
        ));
    }
    lines.push(
        "- You MAY include multiple tool calls in one assistant message; parallel-safe calls may run concurrently."
            .to_string(),
    );
    lines.push("- Only parallelize independent tool calls.".to_string());

    lines.join("\n")
}

fn render_skills_section(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut skills: Vec<&Skill> = skills.iter().collect();
    skills.sort_by(|a, b| a.id.cmp(&b.id));

    let mut lines: Vec<String> = Vec::new();
    lines.push("## Skills".to_string());
    lines.push("A skill is a set of local instructions to follow that is stored in a `SKILL.md` file. Below is the list of skills that can be used. Each entry includes a name, description, and file path so you can open the source for full instructions when using a specific skill.".to_string());
    lines.push("### Available skills".to_string());

    let mut last_id: Option<&str> = None;
    for skill in skills {
        if last_id == Some(skill.id.as_str()) {
            continue;
        }
        last_id = Some(skill.id.as_str());
        let path_str = skill.path.to_string_lossy().replace('\\', "/");
        let name = skill.name.trim();
        let description = skill
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("No description.");
        lines.push(format!("- {name}: {description} (file: {path_str})"));
    }

    lines.push(SKILLS_PROMPT.to_string());

    let body = lines.join("\n");
    Some(format!(
        "{SKILLS_INSTRUCTIONS_OPEN_TAG}\n{body}\n{SKILLS_INSTRUCTIONS_CLOSE_TAG}"
    ))
}

pub fn workspace_relative_path<'a>(workspace_root: &'a Path, path: &'a Path) -> Option<&'a Path> {
    path.strip_prefix(workspace_root).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_in_stable_order() {
        let msgs = PromptBuilder::new()
            .with_agent_prompt("agent")
            .include_model_prompt(true)
            .include_project_prompt(false)
            .include_tools_prompt(false)
            .with_workspace_root("ws")
            .push_user("hi")
            .build();

        assert_eq!(msgs.len(), 4);
        assert!(matches!(&msgs[0], Message::System { content } if content.contains("Codex CLI")));
        assert!(matches!(&msgs[1], Message::System { content } if content == "agent"));
        assert!(
            matches!(&msgs[2], Message::System { content } if content.contains(ENV_OPEN_TAG) && content.contains("PWD:") && content.contains("Platform:") && content.contains("Date:") && content.contains("Subagents:") && content.contains(ENV_CLOSE_TAG))
        );
        assert!(
            matches!(&msgs[3], Message::User { content: UserMessageContent::Text(content) } if content == "hi")
        );
    }

    #[test]
    fn renders_skills_as_system_message() {
        let skill = Skill {
            id: "demo".to_string(),
            name: "demo".to_string(),
            description: Some("desc".to_string()),
            path: PathBuf::from("skills/demo/SKILL.md"),
            content: "hello".to_string(),
        };

        let msgs = PromptBuilder::new()
            .with_agent_prompt("agent")
            .include_tools_prompt(false)
            .add_skill(skill)
            .push_user("hi")
            .build();

        let has_skills = msgs.iter().any(|m| {
            matches!(m, Message::System { content } if content.contains(SKILLS_INSTRUCTIONS_OPEN_TAG) && content.contains("### Available skills") && content.contains("- demo: desc (file: skills/demo/SKILL.md)"))
        });
        assert!(has_skills);
    }

    #[test]
    fn includes_project_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "project rules").unwrap();

        let msgs = PromptBuilder::new()
            .include_tools_prompt(false)
            .with_workspace_root(dir.path())
            .build();

        let has_project = msgs.iter().any(|m| {
            matches!(m, Message::System { content } if content.contains("# Project Instructions") && content.contains("project rules"))
        });
        assert!(has_project);
    }

    #[test]
    fn includes_project_claude_md_when_agents_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "claude rules").unwrap();

        let msgs = PromptBuilder::new()
            .include_tools_prompt(false)
            .with_workspace_root(dir.path())
            .build();

        let has_project = msgs.iter().any(|m| {
            matches!(m, Message::System { content } if content.contains("# Project Instructions") && content.contains("claude rules"))
        });
        assert!(has_project);
    }

    #[test]
    fn includes_nested_project_instructions_in_scope_order() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let nested = repo.join("crates").join("kiliax-server");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(repo.join("crates").join("CLAUDE.md"), "crate rules").unwrap();
        std::fs::write(nested.join("AGENTS.md"), "nested rules").unwrap();

        let msgs = PromptBuilder::new()
            .include_tools_prompt(false)
            .with_workspace_root(&nested)
            .build();

        let project = msgs
            .iter()
            .find_map(|m| match m {
                Message::System { content } if content.contains("# Project Instructions") => {
                    Some(content.clone())
                }
                _ => None,
            })
            .expect("project prompt");

        let root_idx = project.find("root rules").unwrap();
        let crate_idx = project.find("crate rules").unwrap();
        let nested_idx = project.find("nested rules").unwrap();

        assert!(root_idx < crate_idx);
        assert!(crate_idx < nested_idx);
    }

    #[test]
    fn environment_prompt_is_last_system_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "project rules").unwrap();

        let tool = ToolDefinition {
            name: "read_file".to_string(),
            description: Some("Read a file.".to_string()),
            parameters: None,
            strict: None,
        };
        let skill = Skill {
            id: "demo".to_string(),
            name: "demo".to_string(),
            description: Some("desc".to_string()),
            path: PathBuf::from("skills/demo/SKILL.md"),
            content: "hello".to_string(),
        };

        let msgs = PromptBuilder::new()
            .with_agent_prompt("agent")
            .with_tools(vec![tool])
            .add_skill(skill)
            .with_workspace_root(dir.path())
            .push_user("hi")
            .build();

        let env_idx = msgs
            .iter()
            .position(
                |m| matches!(m, Message::System { content } if content.contains(ENV_OPEN_TAG)),
            )
            .unwrap();
        let last_system_idx = msgs
            .iter()
            .rposition(|m| matches!(m, Message::System { .. }))
            .unwrap();
        assert_eq!(env_idx, last_system_idx);

        let tools_idx = msgs
            .iter()
            .position(
                |m| matches!(m, Message::System { content } if content.contains("# Tool Use")),
            )
            .unwrap();
        assert!(tools_idx < env_idx);

        let skills_idx = msgs
            .iter()
            .position(|m| {
                matches!(m, Message::System { content } if content.contains(SKILLS_INSTRUCTIONS_OPEN_TAG))
            })
            .unwrap();
        assert!(skills_idx < env_idx);

        let project_idx = msgs
            .iter()
            .position(|m| {
                matches!(m, Message::System { content } if content.contains("# Project Instructions"))
            })
            .unwrap();
        assert!(project_idx < env_idx);

        let Message::System { content } = &msgs[env_idx] else {
            panic!("expected environment prompt to be a system message");
        };
        assert!(!content.contains("EXTRA_DIRS:"));
    }
}
