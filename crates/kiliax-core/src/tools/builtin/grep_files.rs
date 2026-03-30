use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::TOOL_GREP_FILES;

pub fn grep_files_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_GREP_FILES.to_string(),
        description: Some(
            "Search files under the workspace for a regex pattern (ripgrep semantics; respects .gitignore/.ignore by default)."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex pattern to search for." },
                "path": { "type": "string", "description": "Directory path relative to workspace root (no `..`).", "default": "." },
                "case_sensitive": { "type": "boolean", "description": "Case-sensitive search.", "default": true },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum matches to return." },
                "max_bytes_per_file": { "type": "integer", "minimum": 1, "description": "Skip files larger than this size in bytes." },
                "include_hidden": { "type": "boolean", "description": "Include hidden files and directories.", "default": false }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct GrepFilesArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    max_bytes_per_file: Option<u64>,
    #[serde(default)]
    include_hidden: bool,
}

fn default_true() -> bool {
    true
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_GREP_FILES.to_string()));
    }
    let args: GrepFilesArgs = parse_args(call, TOOL_GREP_FILES)?;
    if args.pattern.trim().is_empty() {
        return Err(ToolError::InvalidCommand(
            "pattern must not be empty".to_string(),
        ));
    }
    let dir = args.path.as_deref().unwrap_or(".");
    let max_results = args.max_results.unwrap_or(100).max(1);
    let max_bytes_per_file = args.max_bytes_per_file.unwrap_or(2_000_000).max(1);
    let base_abs = resolve_workspace_path(workspace_root, extra_workspace_roots, dir)?;
    let workspace_root = workspace_root.to_path_buf();
    let pattern = args.pattern.clone();
    let case_sensitive = args.case_sensitive;
    let include_hidden = args.include_hidden;

    let matches = tokio::task::spawn_blocking(move || {
        grep_files_rust(
            &workspace_root,
            &base_abs,
            &pattern,
            case_sensitive,
            max_results,
            max_bytes_per_file,
            include_hidden,
        )
    })
    .await
    .map_err(|e| ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

    Ok(matches.join("\n"))
}

fn grep_files_rust(
    workspace_root: &Path,
    base: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_results: usize,
    max_bytes_per_file: u64,
    include_hidden: bool,
) -> Result<Vec<String>, ToolError> {
    use grep_regex::RegexMatcherBuilder;
    use grep_searcher::Searcher;
    use ignore::WalkBuilder;

    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(!case_sensitive);
    let matcher = builder
        .build(pattern)
        .map_err(|e| ToolError::InvalidCommand(format!("invalid regex: {e}")))?;

    let mut out: Vec<String> = Vec::new();
    let mut searcher = Searcher::new();

    if base.is_file() {
        if let Ok(meta) = std::fs::metadata(base) {
            if meta.len() > max_bytes_per_file {
                return Ok(out);
            }
        }
        grep_one_file(
            &mut out,
            &mut searcher,
            &matcher,
            workspace_root,
            base,
            max_results,
        );
        return Ok(out);
    }

    if base.is_dir() {
        let mut walker = WalkBuilder::new(base);
        walker
            .current_dir(workspace_root)
            .hidden(!include_hidden)
            .git_global(false)
            .filter_entry(|entry| {
                if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                    let name = entry.file_name().to_string_lossy();
                    return !is_vcs_dir_name(name.as_ref());
                }
                true
            });

        for result in walker.build() {
            if out.len() >= max_results {
                break;
            }
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            let Some(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_file() {
                continue;
            }
            if entry.path_is_symlink() {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > max_bytes_per_file {
                continue;
            }
            let path = entry.path();
            grep_one_file(
                &mut out,
                &mut searcher,
                &matcher,
                workspace_root,
                path,
                max_results,
            );
        }

        return Ok(out);
    }

    Err(ToolError::InvalidPath {
        path: base.to_string_lossy().to_string(),
        reason: "path must be a file or directory within the workspace".to_string(),
    })
}

fn is_vcs_dir_name(name: &str) -> bool {
    matches!(name, ".git" | ".hg" | ".svn" | ".bzr" | "_darcs" | ".pijul")
}

fn grep_one_file(
    out: &mut Vec<String>,
    searcher: &mut grep_searcher::Searcher,
    matcher: &grep_regex::RegexMatcher,
    workspace_root: &Path,
    path: &Path,
    max_results: usize,
) {
    use grep_matcher::Matcher;
    use grep_searcher::sinks::Bytes;

    if out.len() >= max_results {
        return;
    }

    let rel = crate::prompt::workspace_relative_path(workspace_root, path).unwrap_or(path);
    let rel = rel.to_string_lossy().replace('\\', "/");

    let sink = Bytes(|lnum, bytes| {
        if out.len() >= max_results {
            return Ok(false);
        }
        let bytes = strip_line_terminator(bytes);
        let m = match matcher
            .find(bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?
        {
            Some(m) => m,
            None => return Ok(true),
        };
        let col = m.start() + 1;
        let line = String::from_utf8_lossy(bytes);
        out.push(format!("{rel}:{lnum}:{col}: {line}"));
        Ok(out.len() < max_results)
    });

    // Best-effort: skip unreadable files, continue searching.
    let _ = searcher.search_path(matcher, path, sink);
}

fn strip_line_terminator(mut bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len().saturating_sub(1)];
    }
    if bytes.ends_with(b"\r") {
        bytes = &bytes[..bytes.len().saturating_sub(1)];
    }
    bytes
}
