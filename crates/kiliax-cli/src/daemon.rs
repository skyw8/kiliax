use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kiliax_core::config::ServerConfig;
use serde::{Deserialize, Serialize};
use url::Url;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8123;
const PORT_SCAN_MAX: u16 = 8200;
const PING_TIMEOUT_FAST: std::time::Duration = std::time::Duration::from_millis(200);
const STARTUP_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(30);
const PORT_RELEASE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub host: String,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub started_at_ms: Option<u64>,
}

impl DaemonState {
    fn url_base(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

fn home_kiliax_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home directory for ~/.kiliax")?;
    Ok(home.join(".kiliax"))
}

fn state_path() -> Result<PathBuf> {
    Ok(home_kiliax_dir()?.join("server.json"))
}

fn connect_host_for_bind_host(bind_host: &str) -> &str {
    match bind_host.trim() {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    }
}

fn port_is_free(host: &str, port: u16) -> bool {
    std::net::TcpListener::bind((host, port)).is_ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn generate_token_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).context("getrandom failed")?;
    let mut out = String::with_capacity(bytes * 2);
    for b in buf {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0x0f));
    }
    Ok(out)
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

async fn ping(state: &DaemonState) -> bool {
    let url = format!("{}/v1/capabilities", state.url_base());
    let client = reqwest::Client::new();
    let mut req = client.get(url).timeout(PING_TIMEOUT_FAST);
    if !state.token.trim().is_empty() {
        req = req.bearer_auth(state.token.trim());
    }
    match req.send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn ping_web(state: &DaemonState) -> bool {
    let Ok(mut url) = Url::parse(&format!("{}/", state.url_base())) else {
        return false;
    };
    url.query_pairs_mut()
        .append_pair("token", state.token.trim());

    let client = reqwest::Client::new();
    match client.get(url).timeout(PING_TIMEOUT_FAST).send().await {
        Ok(resp) => resp.status().is_success() || resp.status().is_redirection(),
        Err(_) => false,
    }
}

async fn try_stop_http(state: &DaemonState) -> bool {
    let url = format!("{}/v1/admin/stop", state.url_base());
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .timeout(std::time::Duration::from_millis(800));
    if !state.token.trim().is_empty() {
        req = req.bearer_auth(state.token.trim());
    }
    match req.send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn token_is_required(state: &DaemonState) -> bool {
    let url = format!("{}/v1/capabilities", state.url_base());
    let client = reqwest::Client::new();
    match client.get(url).timeout(PING_TIMEOUT_FAST).send().await {
        Ok(resp) => resp.status() == reqwest::StatusCode::UNAUTHORIZED,
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AdminInfo {
    workspace_root: String,
    config_path: String,
}

async fn fetch_admin_info(state: &DaemonState) -> Option<AdminInfo> {
    let url = format!("{}/v1/admin/info", state.url_base());
    let client = reqwest::Client::new();
    let mut req = client.get(url).timeout(PING_TIMEOUT_FAST);
    if !state.token.trim().is_empty() {
        req = req.bearer_auth(state.token.trim());
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<AdminInfo>().await.ok()
}

async fn wait_for_port_to_free(host: &str, port: u16) -> bool {
    let deadline = std::time::Instant::now() + PORT_RELEASE_GRACE;
    while std::time::Instant::now() < deadline {
        if port_is_free(host, port) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    port_is_free(host, port)
}

pub async fn ensure_running(
    workspace_root: &Path,
    config_path: &Path,
    server_cfg: &ServerConfig,
) -> Result<DaemonState> {
    let state_file = state_path()?;
    let desired_bind_host = server_cfg
        .host
        .as_deref()
        .unwrap_or(DEFAULT_HOST)
        .trim()
        .to_string();
    let desired_host = connect_host_for_bind_host(&desired_bind_host).to_string();
    let desired_port = server_cfg.port;
    let desired_token = server_cfg
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string());

    let mut prior: Option<DaemonState> = None;
    if let Ok(text) = tokio::fs::read_to_string(&state_file).await {
        if let Ok(parsed) = serde_json::from_str::<DaemonState>(&text) {
            prior = Some(parsed);
        } else {
            let _ = tokio::fs::remove_file(&state_file).await;
        }
    }

    let token = if let Some(t) = desired_token.clone() {
        t
    } else if let Some(t) = prior.as_ref().and_then(|s| {
        let t = s.token.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }) {
        t
    } else {
        generate_token_hex(32)?
    };

    if let Some(mut parsed) = prior {
        let check = DaemonState {
            token: token.clone(),
            ..parsed.clone()
        };
        let mut should_start_new = false;

        if ping(&parsed).await {
            let api_ok = ping(&check).await;
            let web_ok = ping_web(&check).await;
            let token_required = token_is_required(&check).await;

            let mut identity_ok = true;
            if api_ok && web_ok && token_required {
                if let Some(info) = fetch_admin_info(&check).await {
                    let desired_root = workspace_root.display().to_string();
                    let desired_config_path = config_path.display().to_string();
                    identity_ok = info.workspace_root == desired_root
                        && info.config_path == desired_config_path;
                }
            }

            if api_ok
                && web_ok
                && token_required
                && identity_ok
                && (desired_token.is_none() || parsed.token.trim() == token)
            {
                parsed.token = token.clone();
                let text = serde_json::to_string_pretty(&parsed)
                    .context("failed to serialize server state")?;
                tokio::fs::write(&state_file, text).await.with_context(|| {
                    format!(
                        "failed to write server state file: {}",
                        state_file.display()
                    )
                })?;
                return Ok(parsed);
            }

            let stop_state = if parsed.token.trim().is_empty() {
                check.clone()
            } else {
                parsed.clone()
            };
            if try_stop_http(&stop_state).await {
                let _ = wait_for_port_to_free(&desired_bind_host, parsed.port).await;
                let _ = tokio::fs::remove_file(&state_file).await;
                should_start_new = true;
            } else {
                anyhow::bail!(
                    "kiliax server is reachable at {}:{}, but cannot be restarted (token mismatch?)",
                    parsed.host,
                    parsed.port
                );
            }
        }

        if !should_start_new
            && parsed.started_at_ms.is_some_and(|ms| {
                now_ms().saturating_sub(ms) < STARTUP_GRACE_PERIOD.as_millis() as u64
            })
        {
            parsed.token = token.clone();
            return Ok(parsed);
        }

        let _ = tokio::fs::remove_file(&state_file).await;
    }

    let kiliax_dir = home_kiliax_dir()?;
    tokio::fs::create_dir_all(&kiliax_dir)
        .await
        .context("failed to create ~/.kiliax dir")?;

    let mut port = desired_port.unwrap_or(DEFAULT_PORT);
    if !port_is_free(&desired_bind_host, port) {
        let candidate = DaemonState {
            host: desired_host.clone(),
            port,
            token: token.clone(),
            pid: None,
            started_at_ms: None,
        };
        if ping(&candidate).await {
            let web_ok = ping_web(&candidate).await;
            let token_required = token_is_required(&candidate).await;

            let mut identity_ok = true;
            if web_ok && token_required {
                if let Some(info) = fetch_admin_info(&candidate).await {
                    let desired_root = workspace_root.display().to_string();
                    let desired_config_path = config_path.display().to_string();
                    identity_ok = info.workspace_root == desired_root
                        && info.config_path == desired_config_path;
                }
            }

            if web_ok && token_required && identity_ok {
                let text = serde_json::to_string_pretty(&candidate)
                    .context("failed to serialize server state")?;
                tokio::fs::write(&state_file, text).await.with_context(|| {
                    format!(
                        "failed to write server state file: {}",
                        state_file.display()
                    )
                })?;
                return Ok(candidate);
            }

            if try_stop_http(&candidate).await {
                let _ = wait_for_port_to_free(&desired_bind_host, port).await;
            } else if desired_port.is_some() {
                anyhow::bail!(
                    "server.port {port} is in use and no web UI is reachable at that address"
                );
            }
        }

        if desired_port.is_some() {
            if !port_is_free(&desired_bind_host, port) {
                anyhow::bail!("server.port {port} is in use and no reachable kiliax server is listening there");
            }
        } else {
            let mut found = None;
            for p in DEFAULT_PORT..=PORT_SCAN_MAX {
                if port_is_free(&desired_bind_host, p) {
                    found = Some(p);
                    break;
                }
            }
            port = found.context("no free port found for kiliax server")?;
        }
    }

    let log_file_path = kiliax_dir.join("server.jsonl");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| {
            format!(
                "failed to open server log file: {}",
                log_file_path.display()
            )
        })?;
    let log_file_err = log_file
        .try_clone()
        .context("failed to clone server log file handle")?;

    let mut server_args = vec![
        "--host".to_string(),
        desired_bind_host.clone(),
        "--port".to_string(),
        port.to_string(),
        "--workspace-root".to_string(),
        workspace_root.display().to_string(),
        "--config".to_string(),
        config_path.display().to_string(),
    ];
    server_args.push("--token".to_string());
    server_args.push(token.clone());

    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let mut cmd = Command::new(exe);
    cmd.arg("server")
        .arg("run")
        .args(&server_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .current_dir(workspace_root);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = cmd.spawn().context("failed to spawn `kiliax server run`")?;
    let state = DaemonState {
        host: desired_host,
        port,
        token,
        pid: Some(child.id()),
        started_at_ms: Some(now_ms()),
    };

    let text = serde_json::to_string_pretty(&state).context("failed to serialize server state")?;
    tokio::fs::write(&state_file, text).await.with_context(|| {
        format!(
            "failed to write server state file: {}",
            state_file.display()
        )
    })?;

    Ok(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    NotRunning,
    Stopped,
    NotReachable,
}

pub async fn stop() -> Result<StopOutcome> {
    let state_file = state_path()?;
    let text = match tokio::fs::read_to_string(&state_file).await {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StopOutcome::NotRunning)
        }
        Err(err) => return Err(err.into()),
    };
    let state: DaemonState = serde_json::from_str(&text).context("invalid server.json")?;

    let url = format!("{}/v1/admin/stop", state.url_base());
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .timeout(std::time::Duration::from_millis(800));
    if !state.token.trim().is_empty() {
        req = req.bearer_auth(state.token.trim());
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let _ = wait_for_port_to_free(&state.host, state.port).await;
            let _ = tokio::fs::remove_file(&state_file).await;
            Ok(StopOutcome::Stopped)
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
            anyhow::bail!("unauthorized to stop kiliax server (token mismatch?)");
        }
        Ok(resp) => {
            anyhow::bail!("failed to stop kiliax server: HTTP {}", resp.status());
        }
        Err(_) => {
            let grace_ms = STARTUP_GRACE_PERIOD.as_millis() as u64;
            let should_remove = match state.started_at_ms {
                Some(ms) => now_ms().saturating_sub(ms) > grace_ms,
                None => true,
            };
            if should_remove {
                let _ = tokio::fs::remove_file(&state_file).await;
            }
            Ok(StopOutcome::NotReachable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_hex() {
        let token = generate_token_hex(32).expect("token");
        assert_eq!(token.len(), 64);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
