use std::path::{Path, PathBuf};

use crate::agents::AgentProfile;
use crate::llm::Message;
use crate::tools::skills::Skill;

const TOOLS_PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/tools.md"));
const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";

#[derive(Debug, Clone, Default)]
pub struct PromptBuilder {
    workspace_root: Option<PathBuf>,
    agent_prompt: Option<String>,
    include_tools_prompt: bool,
    skills: Vec<Skill>,
    messages: Vec<Message>,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            workspace_root: None,
            agent_prompt: None,
            include_tools_prompt: true,
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

    pub fn with_agent_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.agent_prompt = Some(prompt.into());
        self
    }

    pub fn include_tools_prompt(mut self, on: bool) -> Self {
        self.include_tools_prompt = on;
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
            content: content.into(),
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

        if let Some(prompt) = self.agent_prompt {
            // Use system role for maximum OpenAI-compatible coverage.
            out.push(Message::System { content: prompt });
        }

        if self.include_tools_prompt {
            out.push(Message::System {
                content: TOOLS_PROMPT.to_string(),
            });
        }

        if let Some(root) = self.workspace_root {
            out.push(Message::System {
                content: format!(
                    "Workspace root: {}\nFor read/write tools, prefer paths relative to this workspace root (no `..`).\nSkill source files may live outside the workspace; use the exact `SKILL.md` paths listed in the skills section when needed.",
                    root.display()
                ),
            });
        }

        if let Some(skills) = render_skills_section(&self.skills) {
            out.push(Message::System { content: skills });
        }

        out.extend(self.messages);
        out
    }
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

    lines.push("### How to use skills".to_string());
    lines.push(
        r###"- Discovery: The list above is the skills available in this session (name + description + file path). Skill bodies live on disk at the listed paths.
- Trigger rules: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches a skill's description shown above, you must use that skill for that turn. Multiple mentions mean use them all. Do not carry skills across turns unless re-mentioned.
- Missing/blocked: If a named skill isn't in the list or the path can't be read, say so briefly and continue with the best fallback.
- How to use a skill (progressive disclosure):
  1) After deciding to use a skill, open its `SKILL.md`. Read only enough to follow the workflow.
  2) When `SKILL.md` references relative paths (e.g., `scripts/foo.py`), resolve them relative to the skill directory listed above first, and only consider other paths if needed.
  3) If `SKILL.md` points to extra folders such as `references/`, load only the specific files needed for the request; don't bulk-load everything.
  4) If `scripts/` exist, prefer running or patching them instead of retyping large code blocks.
  5) If `assets/` or templates exist, reuse them instead of recreating from scratch.
- Coordination and sequencing:
  - If multiple skills apply, choose the minimal set that covers the request and state the order you'll use them.
  - Announce which skill(s) you're using and why (one short line). If you skip an obvious skill, say why.
- Context hygiene:
  - Keep context small: summarize long sections instead of pasting them; only load extra files when needed.
  - Avoid deep reference-chasing: prefer opening only files directly linked from `SKILL.md` unless you're blocked.
  - When variants exist (frameworks, providers, domains), pick only the relevant reference file(s) and note that choice.
- Safety and fallback: If a skill can't be applied cleanly (missing files, unclear instructions), state the issue, pick the next-best approach, and continue."###
            .to_string(),
    );

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
            .include_tools_prompt(false)
            .with_workspace_root("ws")
            .push_user("hi")
            .build();

        assert_eq!(msgs.len(), 3);
        assert!(matches!(&msgs[0], Message::System { content } if content == "agent"));
        assert!(matches!(&msgs[1], Message::System { content } if content.contains("Workspace root:")));
        assert!(matches!(&msgs[2], Message::User { content } if content == "hi"));
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
}
