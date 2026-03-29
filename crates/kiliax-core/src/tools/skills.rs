use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::telemetry;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Skill {
    /// Stable identifier derived from the directory name.
    pub id: String,
    /// Display name (from SKILL.md front matter / heading, or fallback to id).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub path: PathBuf,
    /// Markdown content with optional front matter stripped.
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to parse SKILL.md front matter {path}: {source}")]
    ParseFrontMatter {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("invalid SKILL.md front matter {path}: {reason}")]
    InvalidFrontMatter { path: PathBuf, reason: String },
}

pub fn discover_skills(workspace_root: &Path) -> Result<Vec<Skill>, SkillError> {
    let roots = skill_roots(workspace_root);
    let started = Instant::now();
    let span = tracing::info_span!(
        "kiliax.skills.discover",
        skills.roots = roots.len() as u64,
        skills.discovered = tracing::field::Empty,
        skills.duration_ms = tracing::field::Empty,
    );
    let _enter = span.enter();

    let mut out: BTreeMap<String, Skill> = BTreeMap::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if out.contains_key(&id) {
                continue;
            }
            let md = path.join("SKILL.md");
            if !md.is_file() {
                continue;
            }
            let raw = std::fs::read_to_string(&md)?;

            let parsed = parse_skill_markdown(&md, &raw)?;
            let mut name = parsed
                .front_matter
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| infer_title_from_markdown(&parsed.content))
                .unwrap_or_else(|| id.clone());

            if name.trim().is_empty() {
                name = id.clone();
            }

            let description = parsed
                .front_matter
                .description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| infer_description_from_markdown(&parsed.content));

            out.insert(
                id.clone(),
                Skill {
                    id,
                    name,
                    path: md,
                    description,
                    content: parsed.content,
                },
            );
        }
    }

    let skills: Vec<Skill> = out.into_values().collect();
    span.record("skills.discovered", skills.len() as u64);
    span.record("skills.duration_ms", started.elapsed().as_millis() as u64);
    telemetry::metrics::record_skills_discovered(skills.len());
    Ok(skills)
}

pub fn skill_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(workspace_root.join("skills"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".kiliax").join("skills"));
    }
    roots
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SkillFrontMatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug)]
struct ParsedSkillMarkdown {
    front_matter: SkillFrontMatter,
    content: String,
}

fn parse_skill_markdown(path: &Path, raw: &str) -> Result<ParsedSkillMarkdown, SkillError> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    let (front_matter, body) = match split_yaml_front_matter(raw) {
        Ok(v) => v,
        Err(reason) => {
            return Err(SkillError::InvalidFrontMatter {
                path: path.to_path_buf(),
                reason,
            })
        }
    };

    let front_matter = match front_matter {
        Some(yaml) => serde_yaml::from_str::<SkillFrontMatter>(&yaml).map_err(|source| {
            SkillError::ParseFrontMatter {
                path: path.to_path_buf(),
                source,
            }
        })?,
        None => SkillFrontMatter::default(),
    };

    Ok(ParsedSkillMarkdown {
        front_matter,
        content: body.to_string(),
    })
}

fn split_yaml_front_matter(raw: &str) -> Result<(Option<String>, &str), String> {
    let mut lines = raw.split('\n');
    let Some(first) = lines.next() else {
        return Ok((None, raw));
    };

    let first_trim = first.trim_end_matches('\r').trim();
    if first_trim != "---" {
        return Ok((None, raw));
    }

    let mut yaml_lines = Vec::new();
    let mut cursor = first.len() + 1; // after first line + '\n'

    for line in lines {
        let line_trim = line.trim_end_matches('\r').trim();
        if line_trim == "---" {
            let mut body_start = cursor + line.len() + 1;
            if body_start > raw.len() {
                body_start = raw.len();
            }
            let body = &raw[body_start..];
            let body = body.trim_start_matches(&['\r', '\n'][..]);
            return Ok((Some(yaml_lines.join("\n")), body));
        }

        yaml_lines.push(line.trim_end_matches('\r'));
        cursor += line.len() + 1;
    }

    Err("front matter opened with `---` but not closed".to_string())
}

fn infer_title_from_markdown(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        let line = line.trim_end_matches('\r').trim();
        if let Some(rest) = line.strip_prefix("# ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn infer_description_from_markdown(markdown: &str) -> Option<String> {
    let mut seen_heading = false;
    let mut buf = Vec::new();

    for line in markdown.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if !buf.is_empty() {
                break;
            }
            continue;
        }

        if trimmed.starts_with('#') {
            seen_heading = true;
            continue;
        }

        if !seen_heading {
            continue;
        }

        buf.push(trimmed);
    }

    if buf.is_empty() {
        None
    } else {
        Some(buf.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_parses_and_strips_body() {
        let raw = "---\nname: Demo\ndescription: Hello\n---\n# Title\nBody\n";
        let parsed = parse_skill_markdown(Path::new("SKILL.md"), raw).unwrap();
        assert_eq!(parsed.front_matter.name.as_deref(), Some("Demo"));
        assert_eq!(parsed.front_matter.description.as_deref(), Some("Hello"));
        assert!(parsed.content.starts_with("# Title"));
        assert!(!parsed.content.contains("name: Demo"));
    }

    #[test]
    fn description_infers_from_first_paragraph_after_heading() {
        let raw = "# My Skill\n\nThis is a skill.\nSecond line.\n\n## More\nx\n";
        assert_eq!(
            infer_description_from_markdown(raw),
            Some("This is a skill. Second line.".to_string())
        );
    }
}
