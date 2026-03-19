use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn discover_skills(workspace_root: &Path) -> Result<Vec<Skill>, SkillError> {
    let mut out = Vec::new();

    for root in skill_roots(workspace_root) {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let md = path.join("SKILL.md");
            if !md.is_file() {
                continue;
            }
            let content = std::fs::read_to_string(&md)?;
            let name = entry.file_name().to_string_lossy().to_string();
            out.push(Skill {
                name,
                path: md,
                content,
            });
        }
    }

    Ok(out)
}

pub fn skill_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(workspace_root.join("skills"));
    roots.push(workspace_root.join(".killiax").join("skills"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".killiax").join("skills"));
    }
    roots
}

