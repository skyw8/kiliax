use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::protocol::ToolCall;
use crate::tools::ToolError;

pub(super) fn parse_args<T: for<'de> Deserialize<'de>>(
    call: &ToolCall,
    tool_name: &str,
) -> Result<T, ToolError> {
    serde_json::from_str::<T>(&call.arguments).map_err(|source| ToolError::InvalidArgs {
        tool: tool_name.to_string(),
        source,
    })
}

pub(super) fn resolve_workspace_path(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    path: &str,
) -> Result<PathBuf, ToolError> {
    let mut allowed_roots = Vec::new();
    allowed_roots.push(workspace_root.to_path_buf());
    allowed_roots.extend(extra_workspace_roots.iter().cloned());
    resolve_path_under_roots(workspace_root, path, &allowed_roots, || {
        "path must be within workspace roots".to_string()
    })
}

pub(super) fn resolve_read_path(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    path: &str,
) -> Result<PathBuf, ToolError> {
    let mut allowed_roots = Vec::new();
    allowed_roots.push(workspace_root.to_path_buf());
    allowed_roots.extend(extra_workspace_roots.iter().cloned());
    allowed_roots.extend(crate::tools::skills::skill_roots(workspace_root));

    resolve_path_under_roots(workspace_root, path, &allowed_roots, || {
        "path must be within workspace roots or skills roots".to_string()
    })
}

fn resolve_path_under_roots(
    base_root: &Path,
    path: &str,
    allowed_roots: &[PathBuf],
    out_of_roots_reason: impl FnOnce() -> String,
) -> Result<PathBuf, ToolError> {
    let input = Path::new(path);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        base_root.join(input)
    };

    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ToolError::InvalidPath {
            path: path.to_string(),
            reason: "path must not contain `..`".to_string(),
        });
    }

    let mut canonical_roots = Vec::new();
    for root in allowed_roots {
        if std::fs::symlink_metadata(root).is_err() {
            continue;
        }
        canonical_roots.push(std::fs::canonicalize(root)?);
    }

    let check_path = if std::fs::symlink_metadata(&candidate).is_ok() {
        std::fs::canonicalize(&candidate)?
    } else {
        let ancestor =
            nearest_existing_ancestor(&candidate).unwrap_or_else(|| base_root.to_path_buf());
        std::fs::canonicalize(&ancestor)?
    };

    if canonical_roots
        .iter()
        .any(|root| check_path.starts_with(root))
    {
        Ok(candidate)
    } else {
        Err(ToolError::InvalidPath {
            path: path.to_string(),
            reason: out_of_roots_reason(),
        })
    }
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cursor = path.to_path_buf();
    loop {
        if std::fs::symlink_metadata(&cursor).is_ok() {
            return Some(cursor);
        }
        let parent = cursor.parent()?.to_path_buf();
        cursor = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workspace_path_rejects_parent_dir_components() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let err = resolve_workspace_path(root, &[], "a/../b").unwrap_err();
        assert!(matches!(err, ToolError::InvalidPath { .. }));
    }

    #[test]
    fn resolve_workspace_path_rejects_absolute_paths_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = tempfile::tempdir().unwrap();
        let err = resolve_workspace_path(root, &[], outside.path().to_str().unwrap()).unwrap_err();
        assert!(matches!(err, ToolError::InvalidPath { .. }));
    }

    #[test]
    fn resolve_workspace_path_allows_relative_paths_under_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let path = resolve_workspace_path(root, &[], "a/b.txt").unwrap();
        assert_eq!(path, root.join("a").join("b.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_workspace_path_rejects_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = tempfile::tempdir().unwrap();

        let link = root.join("link");
        symlink(outside.path(), &link).unwrap();

        let err = resolve_workspace_path(root, &[], "link/escape.txt").unwrap_err();
        assert!(matches!(err, ToolError::InvalidPath { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_read_path_rejects_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = tempfile::tempdir().unwrap();

        let link = root.join("link");
        symlink(outside.path(), &link).unwrap();

        let err = resolve_read_path(root, &[], "link/escape.txt").unwrap_err();
        assert!(matches!(err, ToolError::InvalidPath { .. }));
    }

    #[test]
    fn resolve_read_path_rejects_absolute_paths_outside_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = tempfile::tempdir().unwrap();
        let err = resolve_read_path(root, &[], outside.path().to_str().unwrap()).unwrap_err();
        assert!(matches!(err, ToolError::InvalidPath { .. }));
    }
}
