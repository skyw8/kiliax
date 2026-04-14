use std::path::{Component, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("failed to resolve home dir")]
    HomeDirUnavailable,

    #[error("path must be an absolute path")]
    NotAbsolute,

    #[error("path must not contain `..`")]
    ContainsParentDir,

    #[error("path not found: {path}")]
    NotFound { path: String },

    #[error("path must be a directory: {path}")]
    NotDir { path: String },

    #[error("path not accessible: {path}")]
    NotAccessible { path: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn expand_tilde(path: &str) -> Result<PathBuf, PathError> {
    let trimmed = path.trim();
    if trimmed == "~" {
        return dirs::home_dir().ok_or(PathError::HomeDirUnavailable);
    }
    let Some(rest) = trimmed.strip_prefix("~/") else {
        return Ok(PathBuf::from(trimmed));
    };
    let home = dirs::home_dir().ok_or(PathError::HomeDirUnavailable)?;
    Ok(home.join(rest))
}

pub fn validate_absolute_path(path: &str) -> Result<PathBuf, PathError> {
    let candidate = expand_tilde(path)?;
    if !candidate.is_absolute() {
        return Err(PathError::NotAbsolute);
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(PathError::ContainsParentDir);
    }
    Ok(candidate)
}

pub fn validate_existing_dir(path: &str) -> Result<PathBuf, PathError> {
    let candidate = validate_absolute_path(path)?;
    let meta = std::fs::metadata(&candidate).map_err(|_| PathError::NotFound {
        path: candidate.display().to_string(),
    })?;
    if !meta.is_dir() {
        return Err(PathError::NotDir {
            path: candidate.display().to_string(),
        });
    }
    std::fs::canonicalize(&candidate).map_err(|_| PathError::NotAccessible {
        path: candidate.display().to_string(),
    })
}
