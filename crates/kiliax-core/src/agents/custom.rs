use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::AgentRuntimeConfig;
use crate::tools::builtin;
use crate::tools::{Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile, AgentSource, AgentToolFilter, ToolAllow};

const MANIFEST: &str = "AGENT.yaml";
const DEFAULT_PROMPT: &str = "PROMPT.md";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomAgentDiscoveryError {
    pub id: String,
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CustomAgentDiscovery {
    pub items: Vec<AgentProfile>,
    pub errors: Vec<CustomAgentDiscoveryError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomAgentManifest {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_prompt")]
    prompt: PathBuf,
    tools: ToolsManifest,
    permissions: PermissionsManifest,
    #[serde(default)]
    runtime: Option<AgentRuntimeConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsManifest {
    #[serde(default)]
    builtin: Vec<String>,
    #[serde(default)]
    mcp: AllowManifest,
    #[serde(default)]
    custom: AllowManifest,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowManifest {
    #[serde(default)]
    allow: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionsManifest {
    #[serde(default)]
    file_read: bool,
    #[serde(default)]
    file_write: bool,
    shell: ShellManifest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellManifest {
    mode: ShellMode,
    #[serde(default)]
    prefixes: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShellMode {
    DenyAll,
    AllowAll,
    AllowList,
}

pub fn discover_custom_agents() -> CustomAgentDiscovery {
    let mut out: BTreeMap<String, AgentProfile> = BTreeMap::new();
    let mut errors = Vec::new();

    for root in custom_agent_roots() {
        if !root.is_dir() {
            continue;
        }
        let rd = match std::fs::read_dir(&root) {
            Ok(v) => v,
            Err(err) => {
                errors.push(CustomAgentDiscoveryError {
                    id: "<root>".to_string(),
                    path: root,
                    error: err.to_string(),
                });
                continue;
            }
        };

        for entry in rd {
            let entry = match entry {
                Ok(v) => v,
                Err(err) => {
                    errors.push(CustomAgentDiscoveryError {
                        id: "<entry>".to_string(),
                        path: root.clone(),
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if out.contains_key(&id) {
                continue;
            }
            let manifest_path = dir.join(MANIFEST);
            if !manifest_path.is_file() {
                continue;
            }
            match load_custom_agent(&id, &dir, &manifest_path) {
                Ok(profile) => {
                    out.insert(profile.name.clone(), profile);
                }
                Err(err) => errors.push(CustomAgentDiscoveryError {
                    id,
                    path: manifest_path,
                    error: err,
                }),
            }
        }
    }

    CustomAgentDiscovery {
        items: out.into_values().collect(),
        errors,
    }
}

fn load_custom_agent(id: &str, dir: &Path, manifest_path: &Path) -> Result<AgentProfile, String> {
    let raw = std::fs::read_to_string(manifest_path).map_err(|err| err.to_string())?;
    let manifest: CustomAgentManifest =
        serde_yaml::from_str(&raw).map_err(|err| err.to_string())?;
    let name = manifest.name.trim();
    if !is_valid_agent_name(name) {
        return Err("agent name must contain only ASCII letters, digits, '_' or '-'".into());
    }
    if name != id {
        return Err("agent name must match its directory name".into());
    }
    if matches!(name, "general" | "plan") {
        return Err("custom agent cannot override a built-in agent".into());
    }

    let prompt_path = resolve_prompt_path(dir, &manifest.prompt)?;
    let prompt = std::fs::read_to_string(&prompt_path).map_err(|err| err.to_string())?;
    if prompt.trim().is_empty() {
        return Err("agent prompt must not be empty".into());
    }

    let tools = parse_tools(manifest.tools)?;
    let permissions = parse_permissions(manifest.permissions)?;

    Ok(AgentProfile {
        kind: AgentKind::Custom,
        source: AgentSource::Custom,
        name: name.to_string(),
        display_name: manifest
            .display_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        description: manifest
            .description
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        developer_prompt: prompt,
        tools,
        permissions,
        runtime: manifest.runtime,
    })
}

fn parse_tools(tools: ToolsManifest) -> Result<AgentToolFilter, String> {
    let mut builtin_ids = Vec::new();
    for name in tools.builtin {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("builtin tool name must not be empty".into());
        }
        let Some(id) = builtin::builtin_tool_id_by_name(trimmed) else {
            return Err(format!("unknown builtin tool: {trimmed}"));
        };
        builtin_ids.push(id);
    }
    Ok(AgentToolFilter::custom(
        builtin_ids,
        allow_from_vec(tools.mcp.allow, "mcp")?,
        allow_from_vec(tools.custom.allow, "custom")?,
    ))
}

fn parse_permissions(permissions: PermissionsManifest) -> Result<Permissions, String> {
    let shell = match permissions.shell.mode {
        ShellMode::DenyAll => ShellPermissions::DenyAll,
        ShellMode::AllowAll => ShellPermissions::AllowAll,
        ShellMode::AllowList => {
            if permissions.shell.prefixes.is_empty() {
                return Err("shell allow_list requires at least one prefix".into());
            }
            for prefix in &permissions.shell.prefixes {
                if prefix.is_empty() || prefix.iter().any(|part| part.trim().is_empty()) {
                    return Err("shell prefixes must not contain empty tokens".into());
                }
            }
            ShellPermissions::AllowList(permissions.shell.prefixes)
        }
    };

    Ok(Permissions {
        file_read: permissions.file_read,
        file_write: permissions.file_write,
        shell,
    })
}

fn allow_from_vec(values: Vec<String>, label: &str) -> Result<ToolAllow, String> {
    if values.is_empty() {
        return Ok(ToolAllow::None);
    }
    let mut set = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("{label} allow entry must not be empty"));
        }
        set.insert(value.to_string());
    }
    Ok(ToolAllow::Only(set))
}

fn resolve_prompt_path(dir: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Err("prompt path must be relative".into());
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err("prompt path must stay inside the agent directory".into());
        }
    }
    Ok(dir.join(path))
}

fn custom_agent_roots() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|home| vec![home.join(".kiliax").join("agents")])
        .unwrap_or_default()
}

fn default_prompt() -> PathBuf {
    PathBuf::from(DEFAULT_PROMPT)
}

fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
