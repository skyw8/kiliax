use std::path::{Path, PathBuf};

use crate::agents::AgentProfile;
use crate::llm::Message;
use crate::tools::skills::Skill;

const TOOLS_PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/tools.md"));

#[derive(Debug, Clone, Default)]
pub struct PromptBuilder {
    workspace_root: Option<PathBuf>,
    agent_prompt: Option<String>,
    include_tools_prompt: bool,
    skills: Vec<SkillSnippet>,
    messages: Vec<Message>,
}

#[derive(Debug, Clone)]
struct SkillSnippet {
    name: String,
    markdown: String,
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

    pub fn add_skill_markdown(mut self, name: impl Into<String>, markdown: impl Into<String>) -> Self {
        self.skills.push(SkillSnippet {
            name: name.into(),
            markdown: markdown.into(),
        });
        self
    }

    pub fn add_skill(self, skill: Skill) -> Self {
        self.add_skill_markdown(skill.name, skill.content)
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
                    "Workspace root: {}\nAll file paths must be relative to this workspace root.",
                    root.display()
                ),
            });
        }

        if !self.skills.is_empty() {
            out.push(Message::System {
                content: render_skills_block(&self.skills),
            });
        }

        out.extend(self.messages);
        out
    }
}

fn render_skills_block(skills: &[SkillSnippet]) -> String {
    let mut s = String::new();
    s.push_str("# Skills\n");

    for skill in skills {
        s.push_str("\n## ");
        s.push_str(skill.name.trim());
        s.push('\n');
        s.push_str(skill.markdown.trim());
        s.push('\n');
    }

    s
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
            matches!(m, Message::System { content } if content.contains("# Skills") && content.contains("## demo"))
        });
        assert!(has_skills);
    }
}
