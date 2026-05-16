use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Output;

use kiliax_core::paths::PathError;
use kiliax_core::session::SessionId;
use tokio::process::Command;

use crate::api;
use crate::error::ApiError;

fn home_kiliax_dir() -> Result<PathBuf, ApiError> {
    let home = dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"))?;
    Ok(home.join(".kiliax"))
}

pub(crate) fn kiliax_workspace_dir() -> Result<PathBuf, ApiError> {
    Ok(home_kiliax_dir()?.join("workspace"))
}

pub(crate) fn is_tmp_workspace_root(root: &Path) -> Result<bool, ApiError> {
    let base = kiliax_workspace_dir()?;
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if root.parent() != Some(base.as_path()) {
        return Ok(false);
    }
    let Some(name) = root.file_name().and_then(|n| n.to_str()) else {
        return Ok(false);
    };
    Ok(name.starts_with("tmp_"))
}

pub(crate) fn validate_client_workspace_root(input: &str) -> Result<PathBuf, ApiError> {
    match kiliax_core::paths::validate_absolute_path(input) {
        Ok(p) => Ok(p),
        Err(PathError::HomeDirUnavailable) => Err(ApiError::internal("failed to resolve home dir")),
        Err(PathError::NotAbsolute) => Err(ApiError::invalid_argument(
            "workspace_root must be an absolute path",
        )),
        Err(PathError::ContainsParentDir) => Err(ApiError::invalid_argument(
            "workspace_root must not contain `..`",
        )),
        Err(other) => Err(ApiError::internal_error(other)),
    }
}

pub(crate) fn validate_client_extra_workspace_roots(
    inputs: &[String],
    workspace_root: &Path,
) -> Result<Vec<PathBuf>, ApiError> {
    let workspace_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for raw in inputs {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let canonical = match kiliax_core::paths::validate_existing_dir(trimmed) {
            Ok(p) => p,
            Err(PathError::HomeDirUnavailable) => {
                return Err(ApiError::internal("failed to resolve home dir"))
            }
            Err(PathError::NotAbsolute) => {
                return Err(ApiError::invalid_argument(
                    "extra workspace root must be an absolute path",
                ))
            }
            Err(PathError::ContainsParentDir) => {
                return Err(ApiError::invalid_argument(
                    "extra workspace root must not contain `..`",
                ))
            }
            Err(PathError::NotFound { path }) => {
                return Err(ApiError::invalid_argument(format!(
                    "extra workspace root not found: {path}"
                )))
            }
            Err(PathError::NotDir { path }) => {
                return Err(ApiError::invalid_argument(format!(
                    "extra workspace root must be a directory: {path}"
                )))
            }
            Err(PathError::NotAccessible { path }) => {
                return Err(ApiError::invalid_argument(format!(
                    "extra workspace root not accessible: {path}"
                )))
            }
            Err(other) => return Err(ApiError::internal_error(other)),
        };
        if canonical == workspace_root {
            continue;
        }
        if seen.insert(canonical.clone()) {
            out.push(canonical);
        }
    }

    Ok(out)
}

pub(crate) fn default_tmp_workspace_root() -> Result<PathBuf, ApiError> {
    Ok(kiliax_workspace_dir()?.join(format!("tmp_{}", SessionId::new())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativePickMode {
    File,
    Directory,
}

impl NativePickMode {
    fn default_title(self) -> &'static str {
        match self {
            Self::File => "Choose file",
            Self::Directory => "Choose folder",
        }
    }

    fn wants_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

const PICKER_CANCELLED: &str = "__KILIAX_CANCELLED__";

pub(crate) async fn native_pick_path(
    mode: NativePickMode,
    title: Option<&str>,
    start_path: Option<&Path>,
) -> Result<Option<PathBuf>, ApiError> {
    let title = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| mode.default_title());
    let start = existing_picker_start(start_path);
    let in_wsl = is_wsl();

    let picked = if in_wsl {
        pick_windows_path(mode, title, start.as_deref(), true).await?
    } else if std::env::consts::OS == "windows" {
        pick_windows_path(mode, title, start.as_deref(), false).await?
    } else if std::env::consts::OS == "macos" {
        pick_macos_path(mode, title, start.as_deref()).await?
    } else {
        pick_linux_path(mode, title, start.as_deref()).await?
    };

    let Some(path) = picked else {
        return Ok(None);
    };
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| ApiError::not_found("selected path not found"))?;
    if mode.wants_directory() && !meta.is_dir() {
        return Err(ApiError::invalid_argument(
            "selected path must be a directory",
        ));
    }
    if !mode.wants_directory() && !meta.is_file() {
        return Err(ApiError::invalid_argument("selected path must be a file"));
    }
    Ok(Some(path))
}

fn existing_picker_start(path: Option<&Path>) -> Option<PathBuf> {
    let mut current = path?.to_path_buf();
    if current.as_os_str().is_empty() {
        return None;
    }
    if current.is_file() {
        current.pop();
    }
    loop {
        if current.is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
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

async fn windows_path_to_wsl_path(path: &str) -> Option<PathBuf> {
    let out = Command::new("wslpath")
        .arg("-u")
        .arg(path)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return wsl_unc_to_linux_path(path);
    }
    let s = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    if s.is_empty() {
        wsl_unc_to_linux_path(path)
    } else {
        Some(PathBuf::from(s))
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

fn wsl_unc_to_linux_path(path: &str) -> Option<PathBuf> {
    let normalized = path.replace('/', "\\");
    let prefixes = [r"\\wsl$\", r"\\wsl.localhost\"];
    let rest = prefixes
        .iter()
        .find_map(|prefix| normalized.strip_prefix(prefix))?;
    let mut parts = rest.split('\\');
    let distro = parts.next()?.trim();
    if distro.is_empty() {
        return None;
    }
    if let Ok(current) = std::env::var("WSL_DISTRO_NAME") {
        if !current.eq_ignore_ascii_case(distro) {
            return None;
        }
    }
    let linux = parts.collect::<Vec<_>>().join("/");
    Some(PathBuf::from(format!("/{linux}")))
}

fn external_display_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let s = path.display().to_string();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
        s
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn picker_stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

fn picker_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

fn picker_stderr_is_cancel_noise(stderr: &str) -> bool {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return true;
    }

    stderr.lines().all(|line| {
        let line = line.trim().to_lowercase();
        line.contains("mapped without a transient parent") || line.contains("this is discouraged")
    })
}

fn picker_output(output: Output) -> Result<Option<String>, String> {
    let stdout = picker_stdout(&output);
    let stderr = picker_stderr(&output);
    if output.status.success() {
        if stdout.is_empty() || stdout == PICKER_CANCELLED {
            return Ok(None);
        }
        return Ok(Some(stdout));
    }

    let combined = format!("{stdout}\n{stderr}").to_lowercase();
    if combined.contains("cancel")
        || (stdout.is_empty() && stderr.is_empty())
        || (stdout.is_empty()
            && output.status.code() == Some(1)
            && picker_stderr_is_cancel_noise(&stderr))
    {
        return Ok(None);
    }
    Err(if stderr.is_empty() {
        format!("picker exited with status {}", output.status)
    } else {
        stderr
    })
}

async fn pick_windows_path(
    mode: NativePickMode,
    title: &str,
    start: Option<&Path>,
    convert_wsl_paths: bool,
) -> Result<Option<PathBuf>, ApiError> {
    let start_for_dialog = if convert_wsl_paths {
        match start {
            Some(path) => wslpath_to_windows_path(path).await,
            None => None,
        }
    } else {
        start.map(external_display_path)
    };
    let candidates = if convert_wsl_paths {
        ["powershell.exe", "pwsh.exe", "pwsh", "powershell"]
    } else {
        ["powershell.exe", "powershell", "pwsh.exe", "pwsh"]
    };
    let script = windows_picker_script(mode);

    for program in candidates {
        let mut cmd = Command::new(program);
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-STA",
            "-Command",
            script,
        ]);
        cmd.env("KILIAX_PICKER_TITLE", title);
        if let Some(start) = start_for_dialog.as_deref() {
            cmd.env("KILIAX_PICKER_START", start);
        }

        let output = match cmd.output().await {
            Ok(output) => output,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(ApiError::internal_error(err)),
        };
        let selected = picker_output(output)
            .map_err(|err| ApiError::invalid_argument(format!("native picker failed: {err}")))?;
        let Some(selected) = selected else {
            return Ok(None);
        };
        if convert_wsl_paths {
            let path = windows_path_to_wsl_path(&selected)
                .await
                .unwrap_or_else(|| PathBuf::from(selected));
            return Ok(Some(path));
        }
        return Ok(Some(PathBuf::from(selected)));
    }

    Err(ApiError::invalid_argument(
        "native picker unavailable: PowerShell not found",
    ))
}

fn windows_picker_script(mode: NativePickMode) -> &'static str {
    match mode {
        NativePickMode::Directory => {
            r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Windows.Forms
$title = [Environment]::GetEnvironmentVariable('KILIAX_PICKER_TITLE')
$start = [Environment]::GetEnvironmentVariable('KILIAX_PICKER_START')
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = $title
$dialog.ShowNewFolderButton = $true
if ($start -and [System.IO.Directory]::Exists($start)) { $dialog.SelectedPath = $start }
$result = $dialog.ShowDialog()
if ($result -eq [System.Windows.Forms.DialogResult]::OK) { Write-Output $dialog.SelectedPath } else { Write-Output '__KILIAX_CANCELLED__' }
"#
        }
        NativePickMode::File => {
            r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Windows.Forms
$title = [Environment]::GetEnvironmentVariable('KILIAX_PICKER_TITLE')
$start = [Environment]::GetEnvironmentVariable('KILIAX_PICKER_START')
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Title = $title
$dialog.CheckFileExists = $true
$dialog.Multiselect = $false
$dialog.RestoreDirectory = $true
if ($start -and [System.IO.Directory]::Exists($start)) { $dialog.InitialDirectory = $start }
$result = $dialog.ShowDialog()
if ($result -eq [System.Windows.Forms.DialogResult]::OK) { Write-Output $dialog.FileName } else { Write-Output '__KILIAX_CANCELLED__' }
"#
        }
    }
}

async fn pick_macos_path(
    mode: NativePickMode,
    title: &str,
    start: Option<&Path>,
) -> Result<Option<PathBuf>, ApiError> {
    let action = if mode.wants_directory() {
        "choose folder"
    } else {
        "choose file"
    };
    let mut cmd = Command::new("osascript");
    cmd.arg("-e")
        .arg(r#"set promptText to system attribute "KILIAX_PICKER_TITLE""#)
        .arg("-e")
        .arg(r#"set startPath to system attribute "KILIAX_PICKER_START""#)
        .arg("-e")
        .arg(r#"if promptText is "" then set promptText to "Choose""#)
        .arg("-e")
        .arg("if startPath is not \"\" then")
        .arg("-e")
        .arg(format!(
            "POSIX path of ({action} with prompt promptText default location POSIX file startPath)"
        ))
        .arg("-e")
        .arg("else")
        .arg("-e")
        .arg(format!("POSIX path of ({action} with prompt promptText)"))
        .arg("-e")
        .arg("end if")
        .env("KILIAX_PICKER_TITLE", title);
    if let Some(start) = start {
        cmd.env("KILIAX_PICKER_START", start);
    }

    let output = cmd.output().await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::invalid_argument("native picker unavailable: osascript not found")
        } else {
            ApiError::internal_error(err)
        }
    })?;
    let selected = picker_output(output)
        .map_err(|err| ApiError::invalid_argument(format!("native picker failed: {err}")))?;
    Ok(selected.map(PathBuf::from))
}

async fn pick_linux_path(
    mode: NativePickMode,
    title: &str,
    start: Option<&Path>,
) -> Result<Option<PathBuf>, ApiError> {
    let mut last_error: Option<String> = None;
    for (program, args) in linux_picker_commands(mode, title, start) {
        let output = match Command::new(&program).args(&args).output().await {
            Ok(output) => output,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(ApiError::internal_error(err)),
        };
        match picker_output(output) {
            Ok(selected) => return Ok(selected.map(PathBuf::from)),
            Err(err) => {
                last_error = Some(format!("{program}: {err}"));
            }
        }
    }

    if let Some(err) = last_error {
        Err(ApiError::invalid_argument(format!(
            "native picker failed: {err}"
        )))
    } else {
        Err(ApiError::invalid_argument(
            "native picker unavailable: install zenity, kdialog, yad, or qarma",
        ))
    }
}

fn linux_picker_commands(
    mode: NativePickMode,
    title: &str,
    start: Option<&Path>,
) -> Vec<(String, Vec<String>)> {
    let start = start.map(|p| p.display().to_string());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default()
        .to_lowercase();
    let programs: Vec<&str> = if desktop.contains("kde") {
        vec!["kdialog", "zenity", "qarma", "yad"]
    } else {
        vec!["zenity", "qarma", "yad", "kdialog"]
    };

    let mut commands = Vec::new();
    for program in programs {
        let args = match program {
            "kdialog" => {
                let mut args = vec!["--title".to_string(), title.to_string()];
                args.push(if mode.wants_directory() {
                    "--getexistingdirectory".to_string()
                } else {
                    "--getopenfilename".to_string()
                });
                if let Some(start) = start.as_ref() {
                    args.push(start.clone());
                }
                args
            }
            "yad" => {
                let mut args = vec![
                    "--file".to_string(),
                    "--title".to_string(),
                    title.to_string(),
                ];
                if mode.wants_directory() {
                    args.push("--directory".to_string());
                }
                if let Some(start) = start.as_ref() {
                    args.push("--filename".to_string());
                    args.push(start.clone());
                }
                args
            }
            _ => {
                let mut args = vec![
                    "--file-selection".to_string(),
                    "--title".to_string(),
                    title.to_string(),
                ];
                if mode.wants_directory() {
                    args.push("--directory".to_string());
                }
                if let Some(start) = start.as_ref() {
                    args.push("--filename".to_string());
                    args.push(start.clone());
                }
                args
            }
        };
        commands.push((program.to_string(), args));
    }
    commands
}

#[cfg(windows)]
fn windows_find_vscode_exe() -> Option<PathBuf> {
    fn env_path(name: &str) -> Option<PathBuf> {
        std::env::var_os(name)
            .and_then(|v| if v.is_empty() { None } else { Some(v) })
            .map(PathBuf::from)
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(local) = env_path("LOCALAPPDATA") {
        candidates.push(
            local
                .join("Programs")
                .join("Microsoft VS Code")
                .join("Code.exe"),
        );
        candidates.push(
            local
                .join("Programs")
                .join("Microsoft VS Code Insiders")
                .join("Code - Insiders.exe"),
        );
    }
    if let Some(pf) = env_path("ProgramFiles") {
        candidates.push(pf.join("Microsoft VS Code").join("Code.exe"));
        candidates.push(
            pf.join("Microsoft VS Code Insiders")
                .join("Code - Insiders.exe"),
        );
    }
    if let Some(pf86) = env_path("ProgramFiles(x86)") {
        candidates.push(pf86.join("Microsoft VS Code").join("Code.exe"));
        candidates.push(
            pf86.join("Microsoft VS Code Insiders")
                .join("Code - Insiders.exe"),
        );
    }

    for p in candidates {
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(not(windows))]
fn windows_find_vscode_exe() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn windows_percent_encode_path(input: &str) -> String {
    fn hex_digit(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'A' + (n - 10)) as char,
            _ => '?',
        }
    }

    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            other => {
                out.push('%');
                out.push(hex_digit(other >> 4));
                out.push(hex_digit(other & 0x0f));
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn windows_percent_encode_path(input: &str) -> String {
    input.to_string()
}

fn spawn_detached(program: &str, args: &[String]) -> Result<(), std::io::Error> {
    spawn_detached_in_dir(program, args, None)
}

fn spawn_detached_in_dir(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<(), std::io::Error> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().map(|_| ())
}

fn windows_cmd_start_in_dir_args(
    path: &str,
    program: &str,
    program_args: &[String],
) -> Vec<String> {
    let mut args = vec![
        "/c".to_string(),
        "start".to_string(),
        "".to_string(),
        "/D".to_string(),
        path.to_string(),
        program.to_string(),
    ];
    args.extend(program_args.iter().cloned());
    args
}

fn wsl_command_args(distro: Option<&str>, path: &str) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(distro) = distro {
        args.push("-d".to_string());
        args.push(distro.to_string());
    }
    args.push("--cd".to_string());
    args.push(path.to_string());
    args
}

fn windows_terminal_args(start_dir: &str, program: &str, program_args: &[String]) -> Vec<String> {
    let mut args = vec!["-d".to_string(), start_dir.to_string(), program.to_string()];
    args.extend(program_args.iter().cloned());
    args
}

fn linux_terminal_launchers(path: &str) -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("x-terminal-emulator", Vec::new()),
        (
            "gnome-terminal",
            vec!["--working-directory".to_string(), path.to_string()],
        ),
        (
            "xfce4-terminal",
            vec!["--working-directory".to_string(), path.to_string()],
        ),
        ("konsole", vec!["--workdir".to_string(), path.to_string()]),
    ]
}

pub(crate) async fn open_external(
    root: &Path,
    target: api::OpenWorkspaceTarget,
) -> Result<(), ApiError> {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let meta = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| ApiError::not_found("path not found"))?;
    if !meta.is_dir() {
        return Err(ApiError::invalid_argument("path must be a directory"));
    }

    let path = external_display_path(&canonical);
    match target {
        api::OpenWorkspaceTarget::Vscode => {
            if std::env::consts::OS == "windows" {
                match spawn_detached("code", std::slice::from_ref(&path)) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        if let Some(exe) = windows_find_vscode_exe() {
                            let program = exe.display().to_string();
                            return spawn_detached(&program, &[path])
                                .map_err(ApiError::internal_error);
                        }

                        let url_path = windows_percent_encode_path(&path.replace('\\', "/"));
                        let url = format!("vscode://file/{url_path}");
                        let args: Vec<String> =
                            vec!["/c".to_string(), "start".to_string(), "".to_string(), url];
                        spawn_detached("cmd.exe", &args).map_err(|_| {
                            ApiError::invalid_argument(
                                "VS Code not found (install `code` on PATH or reinstall VS Code)",
                            )
                        })
                    }
                    Err(err) => Err(ApiError::internal_error(err)),
                }
            } else {
                spawn_detached("code", &[path]).map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        ApiError::invalid_argument("VS Code CLI `code` not found in PATH")
                    } else {
                        ApiError::internal_error(err)
                    }
                })
            }
        }
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
                    ApiError::invalid_argument(format!(
                        "file manager launcher not found: {program}"
                    ))
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
                let win_start_dir = if let Some(unc) = wsl_unc_path(&canonical) {
                    unc
                } else if let Some(win_path) = wslpath_to_windows_path(&canonical).await {
                    win_path
                } else {
                    path.clone()
                };
                let wsl_args = wsl_command_args(distro.as_deref(), &path);
                let wt_args = windows_terminal_args(&win_start_dir, "wsl.exe", &wsl_args);

                match spawn_detached("wt.exe", &wt_args) {
                    Ok(()) => return Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        // fall through
                    }
                    Err(err) => return Err(ApiError::internal_error(err)),
                }

                let cmd_args = windows_cmd_start_in_dir_args(&win_start_dir, "wsl.exe", &wsl_args);
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

                let cmd_args = windows_cmd_start_in_dir_args(&path, "cmd.exe", &[]);
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

            for (program, args) in linux_terminal_launchers(&path) {
                match spawn_detached_in_dir(program, &args, Some(&canonical)) {
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        linux_picker_commands, linux_terminal_launchers, windows_cmd_start_in_dir_args,
        windows_picker_script, windows_terminal_args, wsl_command_args, wsl_unc_to_linux_path,
        NativePickMode,
    };

    #[test]
    fn linux_terminal_launchers_use_inherited_cwd_for_generic_launcher() {
        let launchers = linux_terminal_launchers("/home/user/Desktop/github/kiliax");

        assert_eq!(launchers[0], ("x-terminal-emulator", Vec::new()));
        assert_eq!(
            launchers[1],
            (
                "gnome-terminal",
                vec![
                    "--working-directory".to_string(),
                    "/home/user/Desktop/github/kiliax".to_string(),
                ],
            )
        );
    }

    #[test]
    fn windows_cmd_start_in_dir_args_sets_working_directory_without_pushd() {
        let args = windows_cmd_start_in_dir_args(
            r#"D:\github code\kiliax\target\release"#,
            "cmd.exe",
            &[],
        );

        assert_eq!(
            args,
            vec![
                "/c".to_string(),
                "start".to_string(),
                "".to_string(),
                "/D".to_string(),
                r#"D:\github code\kiliax\target\release"#.to_string(),
                "cmd.exe".to_string(),
            ]
        );
    }

    #[test]
    fn wsl_terminal_args_set_windows_and_linux_working_directories() {
        let wsl_args = wsl_command_args(Some("Ubuntu"), "/home/user/Desktop/github/kiliax");
        assert_eq!(
            wsl_args,
            vec![
                "-d".to_string(),
                "Ubuntu".to_string(),
                "--cd".to_string(),
                "/home/user/Desktop/github/kiliax".to_string(),
            ]
        );

        let wt_args = windows_terminal_args(
            r#"\\wsl$\Ubuntu\home\user\Desktop\github\kiliax"#,
            "wsl.exe",
            &wsl_args,
        );
        assert_eq!(
            wt_args,
            vec![
                "-d".to_string(),
                r#"\\wsl$\Ubuntu\home\user\Desktop\github\kiliax"#.to_string(),
                "wsl.exe".to_string(),
                "-d".to_string(),
                "Ubuntu".to_string(),
                "--cd".to_string(),
                "/home/user/Desktop/github/kiliax".to_string(),
            ]
        );
    }

    #[test]
    fn wsl_unc_paths_convert_back_to_linux_paths() {
        let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".to_string());
        let unc = format!(r"\\wsl$\{distro}\home\user\Desktop\github\kiliax");

        assert_eq!(
            wsl_unc_to_linux_path(&unc),
            Some(PathBuf::from("/home/user/Desktop/github/kiliax"))
        );
    }

    #[test]
    fn native_picker_commands_cover_directory_and_file_modes() {
        assert!(windows_picker_script(NativePickMode::Directory).contains("FolderBrowserDialog"));
        assert!(windows_picker_script(NativePickMode::File).contains("OpenFileDialog"));

        let commands = linux_picker_commands(
            NativePickMode::Directory,
            "Add folder",
            Some(Path::new("/tmp")),
        );
        assert!(commands.iter().any(|(program, args)| {
            program == "zenity"
                && args.contains(&"--file-selection".to_string())
                && args.contains(&"--directory".to_string())
                && args.contains(&"/tmp".to_string())
        }));
        assert!(commands.iter().any(|(program, args)| {
            program == "kdialog"
                && args.contains(&"--getexistingdirectory".to_string())
                && args.contains(&"/tmp".to_string())
        }));
    }

    #[test]
    fn native_picker_cancel_allows_gtk_transient_parent_warning() {
        assert!(super::picker_stderr_is_cancel_noise(
            "Gtk-Message: 12:45:52.367: GtkDialog mapped without a transient parent. This is discouraged."
        ));
        assert!(!super::picker_stderr_is_cancel_noise(
            "Gtk-WARNING **: cannot open display"
        ));
    }
}
