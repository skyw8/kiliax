use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CONFIG_FILENAME: &str = "kiliax.yaml";

fn default_true() -> bool {
    true
}

fn default_otel_environment() -> String {
    "dev".to_string()
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4318".to_string()
}

fn default_capture_max_bytes() -> usize {
    65536
}

fn default_server_max_live_sessions() -> usize {
    64
}

fn default_server_live_session_idle_ttl_secs() -> u64 {
    900
}

fn default_server_idempotency_max_entries() -> usize {
    1024
}

fn default_server_idempotency_ttl_secs() -> u64 {
    600
}

fn default_server_events_ring_size() -> usize {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentRuntimeConfig {
    /// Maximum number of ReAct steps in a single run.
    #[serde(default, alias = "maxSteps", alias = "max-steps")]
    pub max_steps: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    #[serde(default)]
    pub plan: AgentRuntimeConfig,

    #[serde(default)]
    pub general: AgentRuntimeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WebSearchConfig {
    #[serde(
        default,
        alias = "baseUrl",
        alias = "baseurl",
        alias = "base-url",
        alias = "tavily_base_url",
        alias = "tavilyBaseUrl"
    )]
    pub base_url: Option<String>,

    #[serde(
        default,
        alias = "apiKey",
        alias = "apikey",
        alias = "api-key",
        alias = "key",
        alias = "tavily_api_key",
        alias = "tavilyApiKey"
    )]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsConfig {
    #[serde(default = "default_true")]
    pub default_enable: bool,

    #[serde(default)]
    pub overrides: BTreeMap<String, bool>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            default_enable: default_true(),
            overrides: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_true", alias = "enabled")]
    pub enable: bool,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub token: Option<String>,

    #[serde(
        default = "default_server_max_live_sessions",
        alias = "maxLiveSessions",
        alias = "max-live-sessions"
    )]
    pub max_live_sessions: usize,

    #[serde(
        default = "default_server_live_session_idle_ttl_secs",
        alias = "liveSessionIdleTtlSecs",
        alias = "live-session-idle-ttl-secs"
    )]
    pub live_session_idle_ttl_secs: u64,

    #[serde(
        default = "default_server_idempotency_max_entries",
        alias = "idempotencyMaxEntries",
        alias = "idempotency-max-entries"
    )]
    pub idempotency_max_entries: usize,

    #[serde(
        default = "default_server_idempotency_ttl_secs",
        alias = "idempotencyTtlSecs",
        alias = "idempotency-ttl-secs"
    )]
    pub idempotency_ttl_secs: u64,

    #[serde(
        default = "default_server_events_ring_size",
        alias = "eventsRingSize",
        alias = "events-ring-size"
    )]
    pub events_ring_size: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: None,
            token: None,
            max_live_sessions: default_server_max_live_sessions(),
            live_session_idle_ttl_secs: default_server_live_session_idle_ttl_secs(),
            idempotency_max_entries: default_server_idempotency_max_entries(),
            idempotency_ttl_secs: default_server_idempotency_ttl_secs(),
            events_ring_size: default_server_events_ring_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtelOtlpProtocol {
    #[default]
    HttpProtobuf,
    HttpJson,
    Grpc,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct OtelTlsConfig {
    #[serde(default, alias = "caCert", alias = "ca-cert")]
    pub ca_cert: Option<PathBuf>,

    #[serde(default, alias = "clientCert", alias = "client-cert")]
    pub client_cert: Option<PathBuf>,

    #[serde(default, alias = "clientKey", alias = "client-key")]
    pub client_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtelOtlpConfig {
    #[serde(default = "default_otlp_endpoint")]
    pub endpoint: String,

    #[serde(default)]
    pub protocol: OtelOtlpProtocol,

    #[serde(default)]
    pub headers: BTreeMap<String, String>,

    #[serde(default)]
    pub tls: Option<OtelTlsConfig>,
}

impl Default for OtelOtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: default_otlp_endpoint(),
            protocol: Default::default(),
            headers: Default::default(),
            tls: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtelSignalsConfig {
    #[serde(default = "default_true")]
    pub logs: bool,

    #[serde(default = "default_true")]
    pub traces: bool,

    #[serde(default = "default_true")]
    pub metrics: bool,
}

impl Default for OtelSignalsConfig {
    fn default() -> Self {
        Self {
            logs: true,
            traces: true,
            metrics: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtelCaptureMode {
    Metadata,
    #[default]
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtelCaptureHash {
    None,
    #[default]
    Sha256,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtelCaptureConfig {
    #[serde(default)]
    pub mode: OtelCaptureMode,

    #[serde(
        default = "default_capture_max_bytes",
        alias = "maxBytes",
        alias = "max-bytes"
    )]
    pub max_bytes: usize,

    #[serde(
        default,
        alias = "includeImages",
        alias = "include-images",
        alias = "include_image",
        alias = "include-image"
    )]
    pub include_images: bool,

    #[serde(default)]
    pub hash: OtelCaptureHash,
}

impl Default for OtelCaptureConfig {
    fn default() -> Self {
        Self {
            mode: Default::default(),
            max_bytes: default_capture_max_bytes(),
            include_images: false,
            hash: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtelConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_otel_environment")]
    pub environment: String,

    #[serde(default)]
    pub otlp: OtelOtlpConfig,

    #[serde(default)]
    pub signals: OtelSignalsConfig,

    #[serde(default)]
    pub capture: OtelCaptureConfig,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            environment: default_otel_environment(),
            otlp: Default::default(),
            signals: Default::default(),
            capture: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub default_model: Option<String>,

    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub otel: OtelConfig,

    #[serde(default)]
    pub web_search: WebSearchConfig,

    #[serde(default)]
    pub skills: SkillsConfig,

    /// Default agent runtime options applied to all agents.
    #[serde(default)]
    pub runtime: AgentRuntimeConfig,

    /// Per-agent runtime overrides (applied after `runtime`).
    #[serde(default)]
    pub agents: AgentsConfig,

    #[serde(default)]
    pub mcp: McpConfig,
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
        if self.providers.is_empty() {
            return Err(ConfigError::Invalid(
                "providers is required and must not be empty".to_string(),
            ));
        }

        let (provider_name, model_name) = match split_qualified_model_id(model_id)
            .map_err(ConfigError::Invalid)?
        {
            Some((provider_candidate, model_candidate))
                if self.providers.contains_key(provider_candidate) =>
            {
                (provider_candidate, model_candidate)
            }
            _ => {
                if self.providers.len() == 1 {
                    let provider = self.providers.keys().next().expect("len checked above");
                    (provider.as_str(), model_id)
                } else {
                    let mut matches: Vec<&str> = Vec::new();
                    for (name, provider) in &self.providers {
                        if provider.models.is_empty() {
                            continue;
                        }
                        let qualified = format!("{name}/{model_id}");
                        if provider
                            .models
                            .iter()
                            .any(|m| m == model_id || m == &qualified)
                        {
                            matches.push(name.as_str());
                        }
                    }

                    if matches.len() == 1 {
                        (matches[0], model_id)
                    } else if matches.is_empty() {
                        return Err(ConfigError::Invalid(
                            "model id must be `<provider>/<model>` when multiple providers are configured"
                                .to_string(),
                        ));
                    } else {
                        return Err(ConfigError::Invalid(format!(
                            "model {model_id:?} matches multiple providers ({})",
                            matches.join(", ")
                        )));
                    }
                }
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
    pub server: ServerConfig,

    #[serde(default)]
    pub otel: OtelConfig,

    #[serde(default)]
    pub web_search: WebSearchConfig,

    #[serde(default)]
    pub skills: SkillsConfig,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default)]
    pub runtime: AgentRuntimeConfig,

    #[serde(default)]
    pub agents: AgentsConfig,

    #[serde(default)]
    pub mcp: McpConfig,

    // Shorthand for single-provider config:
    //
    // provider:
    //   base_url: ...
    //   api_key: ...
    //   models: [...]
    #[serde(default)]
    pub provider: Option<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
struct ToolsConfig {
    #[serde(default)]
    pub tavily: WebSearchConfig,
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
    Ok(Some((provider, model)))
}

pub fn candidate_paths(cwd: &Path, home_dir: Option<&Path>) -> Vec<PathBuf> {
    let _ = cwd;
    let mut paths = Vec::with_capacity(1);
    if let Some(home) = home_dir {
        paths.push(home.join(".kiliax").join(CONFIG_FILENAME));
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

pub fn load_from_str(yaml: &str) -> Result<Config, ConfigError> {
    let file: ConfigFile = serde_yaml::from_str(yaml).map_err(|source| ConfigError::Parse {
        path: PathBuf::from("<memory>"),
        source,
    })?;
    let config = resolve_config(file)?;
    validate(&config)?;
    Ok(config)
}

pub fn validate_config(config: &Config) -> Result<(), ConfigError> {
    validate(config)
}

fn resolve_config(file: ConfigFile) -> Result<Config, ConfigError> {
    let ConfigFile {
        default_model,
        mut providers,
        server,
        otel,
        mut web_search,
        skills,
        tools,
        runtime,
        agents,
        mcp,
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

    if web_search.base_url.is_none() {
        web_search.base_url = tools.tavily.base_url;
    }
    if web_search.api_key.is_none() {
        web_search.api_key = tools.tavily.api_key;
    }

    Ok(Config {
        default_model,
        providers,
        server,
        otel,
        web_search,
        skills,
        runtime,
        agents,
        mcp,
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

    if let Some(host) = config.server.host.as_deref() {
        if host.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "server.host must not be empty when set".to_string(),
            ));
        }
    }
    if let Some(port) = config.server.port {
        if port == 0 {
            return Err(ConfigError::Invalid(
                "server.port must not be 0 when set".to_string(),
            ));
        }
    }
    if let Some(token) = config.server.token.as_deref() {
        if token.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "server.token must not be empty when set".to_string(),
            ));
        }
    }
    if let Some(host) = config.server.host.as_deref() {
        let host = host.trim();
        let is_loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
        if !is_loopback && config.server.token.is_none() {
            return Err(ConfigError::Invalid(
                "server.token is required when server.host is not loopback".to_string(),
            ));
        }
    }

    validate_web_search_config(&config.web_search)?;

    validate_otel_config(&config.otel)?;

    validate_agent_runtime_config("runtime", &config.runtime)?;
    validate_agent_runtime_config("agents.plan", &config.agents.plan)?;
    validate_agent_runtime_config("agents.general", &config.agents.general)?;

    validate_mcp_config(&config.mcp)?;

    Ok(())
}

fn validate_otel_config(cfg: &OtelConfig) -> Result<(), ConfigError> {
    if !cfg.enabled {
        return Ok(());
    }

    if cfg.environment.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "otel.environment must not be empty when otel.enabled".to_string(),
        ));
    }

    let endpoint = cfg.otlp.endpoint.trim();
    if endpoint.is_empty() {
        return Err(ConfigError::Invalid(
            "otel.otlp.endpoint must not be empty when otel.enabled".to_string(),
        ));
    }

    match cfg.otlp.protocol {
        OtelOtlpProtocol::HttpProtobuf | OtelOtlpProtocol::HttpJson => {
            if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
                return Err(ConfigError::Invalid(
                    "otel.otlp.endpoint must start with http:// or https:// for OTLP HTTP"
                        .to_string(),
                ));
            }
        }
        OtelOtlpProtocol::Grpc => {
            if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
                return Err(ConfigError::Invalid(
                    "otel.otlp.endpoint must start with http:// or https:// for OTLP gRPC"
                        .to_string(),
                ));
            }
        }
    }

    let endpoint_no_trailing_slash = endpoint.trim_end_matches('/');
    let endpoint_no_trailing_slash = endpoint_no_trailing_slash.to_ascii_lowercase();
    if endpoint_no_trailing_slash.ends_with("/v1")
        || endpoint_no_trailing_slash.ends_with("/v1/traces")
        || endpoint_no_trailing_slash.ends_with("/v1/logs")
        || endpoint_no_trailing_slash.ends_with("/v1/metrics")
    {
        return Err(ConfigError::Invalid(
            "otel.otlp.endpoint must be the collector base URL (e.g. http://localhost:4318), not include OTLP signal paths like /v1/traces (Langfuse: use https://<host>/api/public/otel)"
                .to_string(),
        ));
    }

    if cfg.capture.max_bytes < 1024 {
        return Err(ConfigError::Invalid(
            "otel.capture.max_bytes must be >= 1024 when otel.enabled".to_string(),
        ));
    }

    if let Some(tls) = cfg.otlp.tls.as_ref() {
        match (&tls.client_cert, &tls.client_key) {
            (Some(_), Some(_)) | (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(ConfigError::Invalid(
                    "otel.otlp.tls.client_cert and otel.otlp.tls.client_key must both be set for mTLS"
                        .to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn validate_mcp_config(cfg: &McpConfig) -> Result<(), ConfigError> {
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

    for (idx, server) in cfg.servers.iter().enumerate() {
        let label = format!("mcp.servers[{idx}]");

        if server.name.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "{label}.name must not be empty"
            )));
        }
        if server.command.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "{label}.command must not be empty"
            )));
        }
        if !seen.insert(server.name.trim()) {
            return Err(ConfigError::Invalid(format!(
                "duplicate mcp server name: {:?}",
                server.name.trim()
            )));
        }
    }

    Ok(())
}

fn validate_web_search_config(cfg: &WebSearchConfig) -> Result<(), ConfigError> {
    if let Some(base) = &cfg.base_url {
        if base.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "web_search.base_url must not be empty when set".to_string(),
            ));
        }
    }
    if let Some(key) = &cfg.api_key {
        if key.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "web_search.api_key must not be empty when set".to_string(),
            ));
        }
    }
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
    fn agents_config_rejects_unknown_fields() {
        assert!(serde_yaml::from_str::<AgentsConfig>("build: {}\n").is_err());
        assert!(serde_yaml::from_str::<AgentsConfig>("general: {}\n").is_ok());
    }

    #[test]
    fn loads_home_config_even_if_project_configs_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        write_yaml(&home.join(".kiliax").join("kiliax.yaml"), "home");
        write_yaml(&cwd.join(".kiliax").join("kiliax.yaml"), "localdir");
        write_yaml(&cwd.join("kiliax.yaml"), "root");

        let loaded = load_from_locations(&cwd, Some(&home)).unwrap();
        assert_eq!(loaded.config.providers["p"].base_url, "home");
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
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], home.join(".kiliax").join("kiliax.yaml"));
    }

    #[test]
    fn supports_single_provider_shorthand() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        let path = home.join(".kiliax").join("kiliax.yaml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "provider:\n  base_url: https://example.com/v1\n  apikey: sk-test\n  models:\n    - gpt-test\n",
        )
        .unwrap();

        let loaded = load_from_locations(&cwd, Some(&home)).unwrap();
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
    fn tools_tavily_config_maps_to_web_search() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();

        let path = home.join(".kiliax").join("kiliax.yaml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "providers:\n  p:\n    base_url: https://example.com/v1\n    models:\n      - gpt-test\n\ntools:\n  tavily:\n    api_key: tvly-test\n    base_url: https://api.tavily.com\n",
        )
        .unwrap();

        let loaded = load_from_locations(&cwd, Some(&home)).unwrap();
        assert_eq!(
            loaded.config.web_search.api_key.as_deref(),
            Some("tvly-test")
        );
        assert_eq!(
            loaded.config.web_search.base_url.as_deref(),
            Some("https://api.tavily.com")
        );
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

    #[test]
    fn resolve_model_supports_models_with_slashes() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                base_url: "https://openrouter.ai/api/v1/chat/completions".to_string(),
                api_key: None,
                models: vec!["openai/gpt-4o-mini".to_string()],
            },
        );
        providers.insert(
            "zhipu".to_string(),
            ProviderConfig {
                base_url: "https://open.bigmodel.cn/api/paas/v4/".to_string(),
                api_key: None,
                models: vec!["glm-5".to_string()],
            },
        );

        let cfg = Config {
            default_model: None,
            providers,
            ..Default::default()
        };

        let resolved = cfg.resolve_model("openrouter/openai/gpt-4o-mini").unwrap();
        assert_eq!(resolved.provider, "openrouter");
        assert_eq!(resolved.model, "openai/gpt-4o-mini");

        let resolved = cfg.resolve_model("openai/gpt-4o-mini").unwrap();
        assert_eq!(resolved.provider, "openrouter");
        assert_eq!(resolved.model, "openai/gpt-4o-mini");
    }

    #[test]
    fn otel_config_defaults_when_missing() {
        let cfg = load_from_str(
            "providers:\n  p:\n    base_url: https://example.com/v1\n    models:\n      - gpt-test\n",
        )
        .unwrap();

        assert!(!cfg.otel.enabled);
        assert_eq!(cfg.otel.environment, "dev");
        assert_eq!(cfg.otel.otlp.endpoint, "http://localhost:4318");
        assert_eq!(cfg.otel.otlp.protocol, OtelOtlpProtocol::HttpProtobuf);
        assert!(cfg.otel.signals.logs);
        assert!(cfg.otel.signals.traces);
        assert!(cfg.otel.signals.metrics);
        assert_eq!(cfg.otel.capture.mode, OtelCaptureMode::Full);
        assert_eq!(cfg.otel.capture.max_bytes, 65536);
        assert_eq!(cfg.otel.capture.hash, OtelCaptureHash::Sha256);
    }

    #[test]
    fn otel_config_rejects_incomplete_mtls() {
        let err = load_from_str(
            "otel:\n  enabled: true\n  otlp:\n    endpoint: http://localhost:4318\n    tls:\n      client_cert: client.pem\nproviders:\n  p:\n    base_url: https://example.com/v1\n    models:\n      - gpt-test\n",
        )
        .unwrap_err();

        let ConfigError::Invalid(msg) = err else {
            panic!("unexpected error: {err:?}");
        };
        assert!(msg.contains("mTLS"), "{msg}");
    }

    #[test]
    fn otel_config_rejects_endpoint_with_signal_path() {
        let err = load_from_str(
            "otel:\n  enabled: true\n  otlp:\n    endpoint: http://localhost:4318/v1/traces\nproviders:\n  p:\n    base_url: https://example.com/v1\n    models:\n      - gpt-test\n",
        )
        .unwrap_err();

        let ConfigError::Invalid(msg) = err else {
            panic!("unexpected error: {err:?}");
        };
        assert!(msg.contains("base URL"), "{msg}");
    }
}
