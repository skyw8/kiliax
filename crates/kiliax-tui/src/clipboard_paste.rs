use std::path::{Path, PathBuf};

use tempfile::Builder;

#[derive(Debug, Clone)]
pub enum PasteImageError {
    ClipboardUnavailable(String),
    NoImage(String),
    EncodeFailed(String),
    IoError(String),
}

impl std::fmt::Display for PasteImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteImageError::ClipboardUnavailable(msg) => write!(f, "clipboard unavailable: {msg}"),
            PasteImageError::NoImage(msg) => write!(f, "no image on clipboard: {msg}"),
            PasteImageError::EncodeFailed(msg) => write!(f, "could not encode image: {msg}"),
            PasteImageError::IoError(msg) => write!(f, "io error: {msg}"),
        }
    }
}

impl std::error::Error for PasteImageError {}

#[cfg(not(target_os = "android"))]
fn paste_image_as_png() -> Result<Vec<u8>, PasteImageError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()))?;

    // Images can be exposed as file paths or raw RGBA data. Prefer file paths.
    let files = clipboard
        .get()
        .file_list()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()));

    let dyn_img = if let Some(img) = files
        .unwrap_or_default()
        .into_iter()
        .find_map(|p| image::open(p).ok())
    {
        img
    } else {
        let img = clipboard
            .get_image()
            .map_err(|e| PasteImageError::NoImage(e.to_string()))?;
        let w = img.width as u32;
        let h = img.height as u32;

        let Some(rgba) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return Err(PasteImageError::EncodeFailed("invalid RGBA buffer".into()));
        };
        image::DynamicImage::ImageRgba8(rgba)
    };

    let mut png: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png);
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
    }

    Ok(png)
}

#[cfg(target_os = "android")]
fn paste_image_as_png() -> Result<Vec<u8>, PasteImageError> {
    Err(PasteImageError::ClipboardUnavailable(
        "clipboard image paste is unsupported on Android".into(),
    ))
}

/// Capture an image from the system clipboard and return a path to a temporary PNG.
///
/// On WSL, `arboard` often cannot access the Windows clipboard; in that case a PowerShell
/// fallback is attempted (Linux only).
pub fn paste_image_to_temp_png() -> Result<PathBuf, PasteImageError> {
    match paste_image_as_png() {
        Ok(png) => {
            let tmp = Builder::new()
                .prefix("kiliax-clipboard-")
                .suffix(".png")
                .tempfile()
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            std::fs::write(tmp.path(), &png)
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            let (_file, path) = tmp
                .keep()
                .map_err(|e| PasteImageError::IoError(e.error.to_string()))?;
            Ok(path)
        }
        Err(e) => {
            #[cfg(target_os = "linux")]
            {
                try_wsl_clipboard_fallback(&e).or(Err(e))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(e)
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn try_wsl_clipboard_fallback(error: &PasteImageError) -> Result<PathBuf, PasteImageError> {
    use PasteImageError::ClipboardUnavailable;
    use PasteImageError::NoImage;

    if !is_probably_wsl() || !matches!(error, ClipboardUnavailable(_) | NoImage(_)) {
        return Err(error.clone());
    }

    let Some(win_path) = try_dump_windows_clipboard_image() else {
        return Err(error.clone());
    };

    let Some(mapped) = convert_windows_path_to_wsl(&win_path) else {
        return Err(error.clone());
    };

    if image::image_dimensions(&mapped).is_err() {
        return Err(error.clone());
    };

    Ok(mapped)
}

#[cfg(target_os = "linux")]
fn try_dump_windows_clipboard_image() -> Option<String> {
    // Save clipboard image to a temp PNG and print its path.
    // Force UTF-8 output so stdout decoding is stable.
    let script = r#"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $img = Get-Clipboard -Format Image; if ($img -ne $null) { $p=[System.IO.Path]::GetTempFileName(); $p = [System.IO.Path]::ChangeExtension($p,'png'); $img.Save($p,[System.Drawing.Imaging.ImageFormat]::Png); Write-Output $p } else { exit 1 }"#;

    for cmd in ["powershell.exe", "pwsh", "powershell"] {
        match std::process::Command::new(cmd)
            .args(["-NoProfile", "-Command", script])
            .output()
        {
            Ok(output) if output.status.success() => {
                let win_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !win_path.is_empty() {
                    return Some(win_path);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return None;
    }

    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| pasted.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(pasted);

    if let Ok(url) = url::Url::parse(unquoted) {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                return Some(path);
            }
        }
    }

    // Unquoted Windows paths should bypass POSIX shlex (backslashes are escapes there).
    if let Some(path) = normalize_windows_path(unquoted) {
        return Some(path);
    }

    let parts: Vec<String> = shlex::Shlex::new(pasted).collect();
    if parts.len() == 1 {
        let part = parts.into_iter().next()?;
        if let Some(path) = normalize_windows_path(&part) {
            return Some(path);
        }
        return Some(PathBuf::from(part));
    }

    None
}

#[cfg(target_os = "linux")]
pub(crate) fn is_probably_wsl() -> bool {
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let version = version.to_lowercase();
        if version.contains("microsoft") || version.contains("wsl") {
            return true;
        }
    }
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn is_probably_wsl() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn convert_windows_path_to_wsl(input: &str) -> Option<PathBuf> {
    // Don't attempt to map UNC paths.
    if input.starts_with("\\\\") {
        return None;
    }

    let drive_letter = input.chars().next()?.to_ascii_lowercase();
    if !drive_letter.is_ascii_lowercase() {
        return None;
    }
    if input.get(1..2) != Some(":") {
        return None;
    }

    let mut out = PathBuf::from(format!("/mnt/{drive_letter}"));
    for component in input
        .get(2..)?
        .trim_start_matches(['\\', '/'])
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
    {
        out.push(component);
    }
    Some(out)
}

fn normalize_windows_path(input: &str) -> Option<PathBuf> {
    // Drive-letter: C:\ or C:/
    let drive = input
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && input.get(1..2) == Some(":")
        && input.get(2..3).is_some_and(|s| s == "\\" || s == "/");
    let unc = input.starts_with("\\\\");
    if !drive && !unc {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        if is_probably_wsl() {
            if let Some(converted) = convert_windows_path_to_wsl(input) {
                return Some(converted);
            }
        }
    }

    Some(PathBuf::from(input))
}

pub fn is_probably_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
            )
    )
}
