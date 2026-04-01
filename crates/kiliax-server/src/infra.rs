use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kiliax_core::session::SessionId;
use tokio::process::Command;

use crate::api;
use crate::error::ApiError;

fn home_kiliax_dir() -> Result<PathBuf, ApiError> {
    let home = dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"))?;
    Ok(home.join(".kiliax"))
}

fn expand_tilde(path: &str) -> Result<PathBuf, ApiError> {
    let trimmed = path.trim();
    if trimmed == "~" {
        return dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"));
    }
    let Some(rest) = trimmed.strip_prefix("~/") else {
        return Ok(PathBuf::from(trimmed));
    };
    let home = dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"))?;
    Ok(home.join(rest))
}

pub(crate) fn validate_client_workspace_root(input: &str) -> Result<PathBuf, ApiError> {
    let candidate = expand_tilde(input)?;
    if !candidate.is_absolute() {
        return Err(ApiError::invalid_argument("workspace_root must be an absolute path"));
    }
    for c in candidate.components() {
        if matches!(c, std::path::Component::ParentDir) {
            return Err(ApiError::invalid_argument(
                "workspace_root must not contain `..`",
            ));
        }
    }

    Ok(candidate)
}

pub(crate) fn validate_client_extra_workspace_roots(
    inputs: &[String],
    workspace_root: &Path,
) -> Result<Vec<String>, ApiError> {
    let workspace_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for raw in inputs {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = validate_client_workspace_root(trimmed)?;
        let meta = std::fs::metadata(&candidate).map_err(|_| {
            ApiError::invalid_argument(format!(
                "extra workspace root not found: {}",
                candidate.display()
            ))
        })?;
        if !meta.is_dir() {
            return Err(ApiError::invalid_argument(format!(
                "extra workspace root must be a directory: {}",
                candidate.display()
            )));
        }
        let canonical = std::fs::canonicalize(&candidate).map_err(|_| {
            ApiError::invalid_argument(format!(
                "extra workspace root not accessible: {}",
                candidate.display()
            ))
        })?;
        if canonical == workspace_root {
            continue;
        }
        let display = canonical.display().to_string();
        if seen.insert(display.clone()) {
            out.push(display);
        }
    }

    Ok(out)
}

pub(crate) fn default_tmp_workspace_root() -> Result<PathBuf, ApiError> {
    let base = home_kiliax_dir()?.join("workspace");
    Ok(base.join(format!("tmp_{}", SessionId::new())))
}

fn is_wsl() -> bool {
    if std::env::var_os("WSL_INTEROP").is_some() || std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .ok()
        .map(|v| v.to_lowercase())
        .is_some_and(|v| v.contains("microsoft") || v.contains("wsl"))
}

async fn wslpath_to_windows_path(path: &Path) -> Option<String> {
    let out = Command::new("wslpath")
        .arg("-w")
        .arg(path)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn wsl_unc_path(path: &Path) -> Option<String> {
    let distro = std::env::var("WSL_DISTRO_NAME").ok()?;
    let distro = distro.trim();
    if distro.is_empty() {
        return None;
    }
    let raw = path.to_string_lossy();
    let win = raw.replace('/', "\\");
    Some(format!("\\\\wsl$\\{distro}{win}"))
}

fn spawn_detached(program: &str, args: &[String]) -> Result<(), std::io::Error> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.spawn().map(|_| ())
}

pub(crate) async fn open_external(root: &Path, target: api::OpenWorkspaceTarget) -> Result<(), ApiError> {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let meta = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| ApiError::not_found("path not found"))?;
    if !meta.is_dir() {
        return Err(ApiError::invalid_argument("path must be a directory"));
    }

    let path = canonical.display().to_string();
    match target {
        api::OpenWorkspaceTarget::Vscode => spawn_detached("code", &[path]).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::invalid_argument("VS Code CLI `code` not found in PATH")
            } else {
                ApiError::internal_error(err)
            }
        }),
        api::OpenWorkspaceTarget::FileManager => {
            let (program, args): (&str, Vec<String>) = if is_wsl() {
                let win_path = wslpath_to_windows_path(&canonical)
                    .await
                    .or_else(|| wsl_unc_path(&canonical))
                    .unwrap_or(path);
                ("explorer.exe", vec![win_path])
            } else if std::env::consts::OS == "windows" {
                ("explorer.exe", vec![path])
            } else if std::env::consts::OS == "macos" {
                ("open", vec![path])
            } else {
                ("xdg-open", vec![path])
            };
            spawn_detached(program, &args).map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    ApiError::invalid_argument(format!("file manager launcher not found: {program}"))
                } else {
                    ApiError::internal_error(err)
                }
            })
        }
        api::OpenWorkspaceTarget::Terminal => {
            if is_wsl() {
                let distro = std::env::var("WSL_DISTRO_NAME")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                let mut wt_args: Vec<String> = vec!["wsl.exe".to_string()];
                if let Some(distro) = distro.clone() {
                    wt_args.push("-d".to_string());
                    wt_args.push(distro);
                }
                wt_args.push("--cd".to_string());
                wt_args.push(path.clone());

                match spawn_detached("wt.exe", &wt_args) {
                    Ok(()) => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        // fall through
                    }
                    Err(err) => return Err(ApiError::internal_error(err)),
                }

                let mut cmd_args: Vec<String> = vec![
                    "/c".to_string(),
                    "start".to_string(),
                    "".to_string(),
                    "wsl.exe".to_string(),
                ];
                if let Some(distro) = distro {
                    cmd_args.push("-d".to_string());
                    cmd_args.push(distro);
                }
                cmd_args.push("--cd".to_string());
                cmd_args.push(path);
                return spawn_detached("cmd.exe", &cmd_args).map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        ApiError::invalid_argument("terminal launcher not found: wt.exe/cmd.exe")
                    } else {
                        ApiError::internal_error(err)
                    }
                });
            }

            if std::env::consts::OS == "windows" {
                let wt_args: Vec<String> = vec!["-d".to_string(), path.clone()];
                match spawn_detached("wt.exe", &wt_args) {
                    Ok(()) => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        // fall through
                    }
                    Err(err) => return Err(ApiError::internal_error(err)),
                }

                let cmd_args: Vec<String> = vec![
                    "/c".to_string(),
                    "start".to_string(),
                    "".to_string(),
                    "cmd.exe".to_string(),
                    "/K".to_string(),
                    format!("cd /d {path}"),
                ];
                return spawn_detached("cmd.exe", &cmd_args).map_err(ApiError::internal_error);
            }

            if std::env::consts::OS == "macos" {
                let args: Vec<String> = vec!["-a".to_string(), "Terminal".to_string(), path];
                return spawn_detached("open", &args).map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        ApiError::invalid_argument("terminal launcher not found: open")
                    } else {
                        ApiError::internal_error(err)
                    }
                });
            }

            let candidates: [(&str, &[&str]); 4] = [
                ("x-terminal-emulator", &["--working-directory"]),
                ("gnome-terminal", &["--working-directory"]),
                ("xfce4-terminal", &["--working-directory"]),
                ("konsole", &["--workdir"]),
            ];
            for (program, prefix) in candidates {
                let mut args = prefix.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                args.push(path.clone());
                match spawn_detached(program, &args) {
                    Ok(()) => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(ApiError::internal_error(err)),
                }
            }

            Err(ApiError::invalid_argument(
                "terminal launcher not found (tried x-terminal-emulator/gnome-terminal/xfce4-terminal/konsole)",
            ))
        }
    }
}

