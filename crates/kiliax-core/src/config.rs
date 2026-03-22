use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CONFIG_FILENAME: &str = "killiax.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentRuntimeConfig {
    /// Maximum number of ReAct steps in a single run.
    #[serde(default, alias = "maxSteps", alias = "max-steps")]
    pub max_steps: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentsConfig {
    #[serde(default)]
    pub plan: AgentRuntimeConfig,

    #[serde(default)]
    pub build: AgentRuntimeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub default_model: Option<String>,

    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    /// Default agent runtime options applied to all agents.
    #[serde(default)]
    pub runtime: AgentRuntimeConfig,

    /// Per-agent runtime overrides (applied after `runtime`).
    #[serde(default)]
    pub agents: AgentsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    #[serde(alias = "baseUrl", alias = "baseurl", alias = "base-url")]
    pub base_url: String,

    #[serde(
        default,
        alias = "apiKey",
        alias = "apikey",
        alias = "api-key",
        alias = "key"
    )]
    pub api_key: Option<String>,

    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub provider: String,
    /// Model name to send to the provider's OpenAI-compatible endpoint.
    pub model: String,

    pub base_url: String,
    pub api_key: Option<String>,
}

impl ResolvedModel {
    pub fn model_id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

impl Config {
    /// Resolve a model id into a concrete provider route.
    ///
    /// Supported forms:
    /// - `<provider>/<model>` (recommended, e.g. `moonshot_cn/kimi-k2-turbo-preview`)
    /// - `<model>` (only works when there is a single provider configured)
    pub fn resolve_model(&self, model_id: &str) -> Result<ResolvedModel, ConfigError> {
        let model_id = model_id.trim();
        if model_id.is_empty() {
            return Err(ConfigError::Invalid(
                "model id must not be empty".to_string(),
            ));
        }

        let (provider_name, model_name) = match split_qualified_model_id(model_id)
            .map_err(ConfigError::Invalid)?
        {
            Some((provider, model)) => (provider, model),
            None => {
                let provider = self
                    .providers
                    .keys()
                    .next()
                    .map(|s| s.as_str())
                    .ok_or_else(|| {
                        ConfigError::Invalid(
                            "providers is required and must not be empty".to_string(),
                        )
                    })?;

                if self.providers.len() != 1 {
                    return Err(ConfigError::Invalid(
                            "model id must be `<provider>/<model>` when multiple providers are configured".to_string(),
                        ));
                }

                (provider, model_id)
            }
        };

        let provider = self.providers.get(provider_name).ok_or_else(|| {
            ConfigError::Invalid(format!("provider {provider_name:?} not found in providers"))
        })?;

        if !provider.models.is_empty() {
            let qualified = format!("{provider_name}/{model_name}");
            let ok = provider
                .models
                .iter()
                .any(|m| m == model_name || m == &qualified);
            if !ok {
                return Err(ConfigError::Invalid(format!(
                    "model {qualified:?} not found in provider {provider_name:?} models"
                )));
            }
        }

        Ok(ResolvedModel {
            provider: provider_name.to_string(),
            model: model_name.to_string(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
struct ConfigFile {
    #[serde(default)]
    pub default_model: Option<String>,

    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    #[serde(default)]
    pub runtime: AgentRuntimeConfig,

    #[serde(default)]
    pub agents: AgentsConfig,

    // Shorthand for single-provider config:
    //
    // provider:
    //   base_url: ...
    //   api_key: ...
    //   models: [...]
    #[serde(default)]
    pub provider: Option<ProviderConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub config: Config,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to get current dir: {0}")]
    CurrentDir(std::io::Error),

    #[error("no config file found (checked: {paths:?})")]
    NotFound { paths: Vec<PathBuf> },

    #[error("failed to read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse YAML config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("invalid config: {0}")]
    Invalid(String),
}

fn split_qualified_model_id(model_id: &str) -> Result<Option<(&str, &str)>, String> {
    let Some((provider, model)) = model_id.split_once('/') else {
        return Ok(None);
    };

    if provider.is_empty() || model.is_empty() {
        return Err("model id must be `<provider>/<model>`".to_string());
    }
    if model.contains('/') {
        return Err("model id must contain exactly one `/`".to_string());
    }
    Ok(Some((provider, model)))
}

pub fn candidate_paths(cwd: &Path, home_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(3);
    paths.push(cwd.join(CONFIG_FILENAME));
    paths.push(cwd.join(".killiax").join(CONFIG_FILENAME));
    if let Some(home) = home_dir {
        paths.push(home.join(".killiax").join(CONFIG_FILENAME));
    }
    paths
}

pub fn find_config_path(cwd: &Path, home_dir: Option<&Path>) -> Option<PathBuf> {
    candidate_paths(cwd, home_dir)
        .into_iter()
        .find(|p| p.is_file())
}

pub fn load() -> Result<LoadedConfig, ConfigError> {
    let cwd = std::env::current_dir().map_err(ConfigError::CurrentDir)?;
    let home_dir = dirs::home_dir();
    load_from_locations(&cwd, home_dir.as_deref())
}

pub fn load_from_locations(
    cwd: &Path,
    home_dir: Option<&Path>,
) -> Result<LoadedConfig, ConfigError> {
    let paths = candidate_paths(cwd, home_dir);
    let Some(path) = paths.iter().find(|p| p.is_file()).cloned() else {
        return Err(ConfigError::NotFound { paths });
    };
    load_from_path(path)
}

pub fn load_from_path(path: impl Into<PathBuf>) -> Result<LoadedConfig, ConfigError> {
    let path: PathBuf = path.into();
    let text = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let file: ConfigFile = serde_yaml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.clone(),
        source,
    })?;
    let config = resolve_config(file)?;
    validate(&config)?;
    Ok(LoadedConfig { path, config })
}

fn resolve_config(file: ConfigFile) -> Result<Config, ConfigError> {
    let ConfigFile {
        default_model,
        mut providers,
        runtime,
        agents,
        provider,
    } = file;

    if provider.is_some() && !providers.is_empty() {
        return Err(ConfigError::Invalid(
            "cannot set both `provider` and `providers`".to_string(),
        ));
    }

    if let Some(p) = provider {
        providers.insert("default".to_string(), p);
    }

    Ok(Config {
        default_model,
        providers,
        runtime,
        agents,
    })
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    if config.providers.is_empty() {
        return Err(ConfigError::Invalid(
            "providers is required and must not be empty".to_string(),
        ));
    }
    for (name, p) in &config.providers {
        if name.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "provider name must not be empty".to_string(),
            ));
        }
        if p.base_url.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "provider {name} base_url must not be empty"
            )));
        }
        if let Some(key) = &p.api_key {
            if key.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "provider {name} api_key must not be empty when set"
                )));
            }
        }
        for m in &p.models {
            if m.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "provider {name} models must not contain empty strings"
                )));
            }
        }
    }
    if let Some(default_model) = &config.default_model {
        config.resolve_model(default_model)?;
    }

    validate_agent_runtime_config("runtime", &config.runtime)?;
    validate_agent_runtime_config("agents.plan", &config.agents.plan)?;
    validate_agent_runtime_config("agents.build", &config.agents.build)?;

    Ok(())
}

fn validate_agent_runtime_config(label: &str, cfg: &AgentRuntimeConfig) -> Result<(), ConfigError> {
    if cfg.max_steps.is_some_and(|v| v == 0) {
        return Err(ConfigError::Invalid(format!(
            "{label}.max_steps must be greater than 0"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_yaml(path: &Path, base_url: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "providers:\n  p:\n    base_url: {base_url}\n    api_key: sk-test\n    models:\n      - gpt-test\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn priority_uses_project_root_first() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        write_yaml(&home.join(".killiax").join("killiax.yaml"), "home");
        write_yaml(&cwd.join(".killiax").join("killiax.yaml"), "localdir");
        write_yaml(&cwd.join("killiax.yaml"), "root");

        let loaded = load_from_locations(&cwd, Some(&home)).unwrap();
        assert_eq!(loaded.config.providers["p"].base_url, "root");
    }

    #[test]
    fn priority_uses_dot_dir_when_root_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        write_yaml(&home.join(".killiax").join("killiax.yaml"), "home");
        write_yaml(&cwd.join(".killiax").join("killiax.yaml"), "localdir");

        let loaded = load_from_locations(&cwd, Some(&home)).unwrap();
        assert_eq!(loaded.config.providers["p"].base_url, "localdir");
    }

    #[test]
    fn not_found_contains_all_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        let err = load_from_locations(&cwd, Some(&home)).unwrap_err();
        let ConfigError::NotFound { paths } = err else {
            panic!("unexpected error: {err:?}");
        };
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], cwd.join("killiax.yaml"));
        assert_eq!(paths[1], cwd.join(".killiax").join("killiax.yaml"));
        assert_eq!(paths[2], home.join(".killiax").join("killiax.yaml"));
    }

    #[test]
    fn supports_single_provider_shorthand() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();

        let path = cwd.join("killiax.yaml");
        fs::write(
            &path,
            "provider:\n  base_url: https://example.com/v1\n  apikey: sk-test\n  models:\n    - gpt-test\n",
        )
        .unwrap();

        let loaded = load_from_locations(&cwd, None).unwrap();
        assert_eq!(
            loaded.config.providers["default"].base_url,
            "https://example.com/v1"
        );
    }

    #[test]
    fn resolve_model_routes_with_provider_prefix() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "moonshot_cn".to_string(),
            ProviderConfig {
                base_url: "https://api.moonshot.cn/v1".to_string(),
                api_key: Some("sk-test".to_string()),
                models: vec!["kimi-k2-turbo-preview".to_string()],
            },
        );

        let cfg = Config {
            default_model: None,
            providers,
            ..Default::default()
        };

        let resolved = cfg
            .resolve_model("moonshot_cn/kimi-k2-turbo-preview")
            .unwrap();
        assert_eq!(resolved.provider, "moonshot_cn");
        assert_eq!(resolved.model, "kimi-k2-turbo-preview");
        assert_eq!(resolved.base_url, "https://api.moonshot.cn/v1");
    }

    #[test]
    fn resolve_model_requires_qualified_id_with_multiple_providers() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "p1".to_string(),
            ProviderConfig {
                base_url: "https://example.com/v1".to_string(),
                api_key: None,
                models: Vec::new(),
            },
        );
        providers.insert(
            "p2".to_string(),
            ProviderConfig {
                base_url: "https://example.com/v1".to_string(),
                api_key: None,
                models: Vec::new(),
            },
        );

        let cfg = Config {
            default_model: None,
            providers,
            ..Default::default()
        };

        let err = cfg.resolve_model("m").unwrap_err();
        let ConfigError::Invalid(msg) = err else {
            panic!("unexpected error: {err:?}");
        };
        assert!(msg.contains("`<provider>/<model>`"));
    }

    #[test]
    fn resolve_model_accepts_qualified_entries_in_models_list() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "p".to_string(),
            ProviderConfig {
                base_url: "https://example.com/v1".to_string(),
                api_key: None,
                models: vec!["p/m".to_string()],
            },
        );

        let cfg = Config {
            default_model: None,
            providers,
            ..Default::default()
        };

        let resolved = cfg.resolve_model("m").unwrap();
        assert_eq!(resolved.provider, "p");
        assert_eq!(resolved.model, "m");

        let resolved = cfg.resolve_model("p/m").unwrap();
        assert_eq!(resolved.provider, "p");
        assert_eq!(resolved.model, "m");
    }
}
