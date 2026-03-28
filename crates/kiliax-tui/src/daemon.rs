use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kiliax_core::config::ServerConfig;
use serde::{Deserialize, Serialize};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8123;
const PORT_SCAN_MAX: u16 = 8200;
const PING_TIMEOUT_FAST: std::time::Duration = std::time::Duration::from_millis(200);
const STARTUP_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(30);

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

fn state_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".kiliax").join("server.json")
}

fn log_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".kiliax").join("server.log")
}

fn connect_host_for_bind_host(bind_host: &str) -> &str {
    match bind_host.trim() {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    }
}

fn server_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let dir = exe
        .parent()
        .context("current executable missing parent dir")?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let sibling = dir.join(format!("kiliax-server{suffix}"));
    if sibling.exists() {
        Ok(sibling)
    } else {
        Ok(PathBuf::from(format!("kiliax-server{suffix}")))
    }
}

fn repo_manifest_from_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    for ancestor in exe.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == "target") {
            let root = ancestor.parent()?;
            let manifest = root.join("Cargo.toml");
            if manifest.is_file() {
                return Some(manifest);
            }
        }
    }
    None
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

pub async fn ensure_running(
    workspace_root: &Path,
    config_path: &Path,
    server_cfg: &ServerConfig,
) -> Result<DaemonState> {
    let state_file = state_path(workspace_root);
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

    if let Ok(text) = tokio::fs::read_to_string(&state_file).await {
        if let Ok(parsed) = serde_json::from_str::<DaemonState>(&text) {
            if ping(&parsed).await {
                return Ok(parsed);
            }
            if parsed
                .started_at_ms
                .is_some_and(|ms| now_ms().saturating_sub(ms) < STARTUP_GRACE_PERIOD.as_millis() as u64)
            {
                return Ok(parsed);
            }
            let _ = tokio::fs::remove_file(&state_file).await;
        }
    }

    tokio::fs::create_dir_all(workspace_root.join(".kiliax"))
        .await
        .context("failed to create .kiliax dir")?;

    let mut port = desired_port.unwrap_or(DEFAULT_PORT);
    if !port_is_free(&desired_bind_host, port) {
        let candidate = DaemonState {
            host: desired_host.clone(),
            port,
            token: desired_token.clone().unwrap_or_default(),
            pid: None,
            started_at_ms: None,
        };
        if ping(&candidate).await {
            let text = serde_json::to_string_pretty(&candidate).context("failed to serialize server state")?;
            tokio::fs::write(&state_file, text)
                .await
                .with_context(|| format!("failed to write server state file: {}", state_file.display()))?;
            return Ok(candidate);
        }

        if desired_port.is_some() {
            anyhow::bail!("server.port {port} is in use and no reachable kiliax-server is listening there");
        }

        let mut found = None;
        for p in DEFAULT_PORT..=PORT_SCAN_MAX {
            if port_is_free(&desired_bind_host, p) {
                found = Some(p);
                break;
            }
        }
        port = found.context("no free port found for kiliax-server")?;
    }

    let token = desired_token.unwrap_or_default();

    let log_file_path = log_path(workspace_root);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("failed to open server log file: {}", log_file_path.display()))?;
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
    if !token.trim().is_empty() {
        server_args.push("--token".to_string());
        server_args.push(token.clone());
    }

    let mut cmd = {
        let server_exe = server_executable()?;
        let mut cmd = Command::new(server_exe);
        cmd.args(&server_args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_file_err))
            .current_dir(workspace_root);
        cmd
    };

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

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let manifest = repo_manifest_from_current_exe()
                .context("kiliax-server not found (install it, or run from repo with cargo)")?;

            let log_file_path = log_path(workspace_root);
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file_path)
                .with_context(|| format!("failed to open server log file: {}", log_file_path.display()))?;
            let log_file_err = log_file
                .try_clone()
                .context("failed to clone server log file handle")?;

            let mut cmd = Command::new("cargo");
            cmd.arg("run")
                .arg("-p")
                .arg("kiliax-server")
                .arg("--manifest-path")
                .arg(&manifest)
                .arg("--")
                .args(&server_args)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log_file))
                .stderr(Stdio::from(log_file_err))
                .current_dir(
                    manifest
                        .parent()
                        .context("manifest path missing parent dir")?,
                );

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

            cmd.spawn().context("failed to spawn kiliax-server via cargo")?
        }
        Err(err) => return Err(anyhow::Error::new(err).context("failed to spawn kiliax-server")),
    };
    let state = DaemonState {
        host: desired_host,
        port,
        token,
        pid: Some(child.id()),
        started_at_ms: Some(now_ms()),
    };

    let text = serde_json::to_string_pretty(&state).context("failed to serialize server state")?;
    tokio::fs::write(&state_file, text)
        .await
        .with_context(|| format!("failed to write server state file: {}", state_file.display()))?;

    Ok(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    NotRunning,
    Stopped,
    NotReachable,
}

pub async fn stop(workspace_root: &Path) -> Result<StopOutcome> {
    let state_file = state_path(workspace_root);
    let text = match tokio::fs::read_to_string(&state_file).await {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(StopOutcome::NotRunning),
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
            let _ = tokio::fs::remove_file(&state_file).await;
            Ok(StopOutcome::Stopped)
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
            anyhow::bail!("unauthorized to stop kiliax-server (token mismatch?)");
        }
        Ok(resp) => {
            anyhow::bail!("failed to stop kiliax-server: HTTP {}", resp.status());
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
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
