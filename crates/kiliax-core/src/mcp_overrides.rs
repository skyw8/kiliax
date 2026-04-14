use std::collections::{HashMap, HashSet};

use crate::config::Config;
use crate::session::SessionMcpServerSetting;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("mcp server not found: {0}")]
pub struct UnknownMcpServer(pub String);

pub fn session_mcp_servers_from_config(config: &Config) -> Vec<SessionMcpServerSetting> {
    config
        .mcp
        .servers
        .iter()
        .map(|server| SessionMcpServerSetting {
            id: server.name.clone(),
            enable: server.enable,
        })
        .collect()
}

pub fn config_with_session_mcp_overrides(
    base: &Config,
    overrides: &[SessionMcpServerSetting],
) -> Result<Config, UnknownMcpServer> {
    let mut cfg = base.clone();
    apply_mcp_enable_overrides(&mut cfg, overrides.iter().map(|s| (s.id.as_str(), s.enable)))?;
    Ok(cfg)
}

pub fn apply_mcp_enable_overrides<'a>(
    cfg: &mut Config,
    overrides: impl IntoIterator<Item = (&'a str, bool)>,
) -> Result<(), UnknownMcpServer> {
    let known: HashSet<&str> = cfg.mcp.servers.iter().map(|s| s.name.as_str()).collect();
    let mut enabled_by_id: HashMap<&str, bool> = HashMap::new();
    for (id, enable) in overrides {
        if !known.contains(id) {
            return Err(UnknownMcpServer(id.to_string()));
        }
        enabled_by_id.insert(id, enable);
    }
    for server in &mut cfg.mcp.servers {
        if let Some(enable) = enabled_by_id.get(server.name.as_str()) {
            server.enable = *enable;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpConfig, McpServerConfig};

    fn config_with_mcp_servers() -> Config {
        Config {
            mcp: McpConfig {
                servers: vec![
                    McpServerConfig {
                        name: "one".to_string(),
                        enable: true,
                        command: "echo".to_string(),
                        args: vec!["one".to_string()],
                    },
                    McpServerConfig {
                        name: "two".to_string(),
                        enable: false,
                        command: "echo".to_string(),
                        args: vec!["two".to_string()],
                    },
                ],
            },
            ..Default::default()
        }
    }

    #[test]
    fn apply_overrides_only_changes_selected_servers() {
        let mut cfg = config_with_mcp_servers();
        apply_mcp_enable_overrides(&mut cfg, [("one", false)]).unwrap();

        assert_eq!(cfg.mcp.servers[0].name, "one");
        assert!(!cfg.mcp.servers[0].enable);

        assert_eq!(cfg.mcp.servers[1].name, "two");
        assert!(!cfg.mcp.servers[1].enable);
    }

    #[test]
    fn unknown_ids_are_rejected() {
        let mut cfg = config_with_mcp_servers();
        let err = apply_mcp_enable_overrides(&mut cfg, [("missing", true)]).unwrap_err();
        assert_eq!(err, UnknownMcpServer("missing".to_string()));
    }

    #[test]
    fn session_helper_roundtrip() {
        let base = config_with_mcp_servers();
        let overrides = vec![SessionMcpServerSetting {
            id: "two".to_string(),
            enable: true,
        }];

        let cfg = config_with_session_mcp_overrides(&base, &overrides).unwrap();
        assert!(cfg.mcp.servers.iter().any(|s| s.name == "two" && s.enable));
        assert!(cfg.mcp.servers.iter().any(|s| s.name == "one" && s.enable));
    }
}

