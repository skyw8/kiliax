use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::TOOL_APPLY_PATCH;

pub fn apply_patch_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_APPLY_PATCH.to_string(),
        description: Some(
            "Apply a file-oriented patch to the workspace. File paths in the patch must be workspace-relative (no `..`)."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "Patch text in the *** Begin Patch / *** End Patch format (paths are workspace-relative; no `..`)." }
            },
            "required": ["patch"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
}

#[derive(Debug, Serialize)]
struct ApplyPatchOutput {
    ok: bool,
    files: Vec<PatchedFile>,
}

#[derive(Debug, Serialize)]
struct PatchedFile {
    action: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    moved_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added_lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_lines: Option<usize>,
}

pub(super) async fn execute(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_APPLY_PATCH.to_string()));
    }
    let args: ApplyPatchArgs = parse_args(call, TOOL_APPLY_PATCH)?;

    let ops = parse_patch(&args.patch)
        .map_err(|e| ToolError::InvalidCommand(format!("invalid patch: {e}")))?;

    let mut out_files = Vec::new();

    for op in ops {
        match op {
            PatchOp::AddFile { path, content } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                if abs.exists() {
                    return Err(ToolError::InvalidCommand(format!(
                        "add file failed: {path} already exists"
                    )));
                }
                if let Some(parent) = abs.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&abs, &content).await?;
                let diff = small_unified_diff("", &content, &path);
                out_files.push(PatchedFile {
                    action: "add".to_string(),
                    path,
                    moved_to: None,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
            PatchOp::DeleteFile { path } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                let old = tokio::fs::read_to_string(&abs).await.unwrap_or_default();
                tokio::fs::remove_file(&abs).await?;
                let diff = small_unified_diff(&old, "", &path);
                out_files.push(PatchedFile {
                    action: "delete".to_string(),
                    path,
                    moved_to: None,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
            PatchOp::UpdateFile {
                path,
                move_to,
                hunks,
            } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                let old = tokio::fs::read_to_string(&abs).await?;
                let new = apply_update_hunks(&old, &hunks)
                    .map_err(|e| ToolError::InvalidCommand(format!("patch failed: {e}")))?;

                tokio::fs::write(&abs, &new).await?;
                let mut final_path = path.clone();

                if let Some(dest) = move_to.clone() {
                    let dest_abs = resolve_workspace_path(workspace_root, &dest)?;
                    if let Some(parent) = dest_abs.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::rename(&abs, &dest_abs).await?;
                    final_path = dest;
                }

                let diff = small_unified_diff(&old, &new, &final_path);
                out_files.push(PatchedFile {
                    action: "update".to_string(),
                    path,
                    moved_to: move_to,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
        }
    }

    let out = ApplyPatchOutput {
        ok: true,
        files: out_files,
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
}

#[derive(Debug)]
enum PatchOp {
    AddFile {
        path: String,
        content: String,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_to: Option<String>,
        hunks: Vec<UpdateHunk>,
    },
}

#[derive(Debug, Default)]
struct UpdateHunk {
    #[allow(dead_code)]
    header: Option<String>,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
enum HunkLine {
    Context(String),
    Add(String),
    Del(String),
}

fn parse_patch(input: &str) -> Result<Vec<PatchOp>, String> {
    let mut lines: Vec<&str> = input.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let mut i = 0usize;
    let first = lines
        .get(i)
        .ok_or("missing *** Begin Patch")?
        .trim_end_matches('\r');
    if first != "*** Begin Patch" {
        return Err("expected *** Begin Patch".to_string());
    }
    i += 1;

    let mut ops = Vec::new();
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if line == "*** End Patch" {
            return Ok(ops);
        }

        if let Some(path) = line.strip_prefix("*** Add File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("add file missing path".to_string());
            }
            i += 1;
            let mut content_lines = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if l.starts_with("*** ") || l == "*** End Patch" {
                    break;
                }
                let rest = l
                    .strip_prefix('+')
                    .ok_or_else(|| "add file lines must start with '+'".to_string())?;
                content_lines.push(rest.to_string());
                i += 1;
            }
            let mut content = content_lines.join("\n");
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            ops.push(PatchOp::AddFile {
                path: path.to_string(),
                content,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("delete file missing path".to_string());
            }
            i += 1;
            ops.push(PatchOp::DeleteFile {
                path: path.to_string(),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("update file missing path".to_string());
            }
            i += 1;

            let mut move_to: Option<String> = None;
            if i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if let Some(dest) = l.strip_prefix("*** Move to:") {
                    let dest = dest.trim();
                    if dest.is_empty() {
                        return Err("move to missing path".to_string());
                    }
                    move_to = Some(dest.to_string());
                    i += 1;
                }
            }

            let mut hunks = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if l == "*** End Patch" || l.starts_with("*** ") {
                    break;
                }
                if !l.starts_with("@@") {
                    return Err(format!("expected @@ hunk header, got {l:?}"));
                }
                let header = l.strip_prefix("@@").unwrap().trim();
                let header = if header.is_empty() {
                    None
                } else {
                    Some(header.to_string())
                };
                i += 1;

                let mut hunk = UpdateHunk {
                    header,
                    lines: Vec::new(),
                };
                while i < lines.len() {
                    let l2 = lines[i].trim_end_matches('\r');
                    if l2.starts_with("@@") || l2.starts_with("*** ") || l2 == "*** End Patch" {
                        break;
                    }
                    let mut chars = l2.chars();
                    let Some(prefix) = chars.next() else {
                        return Err("empty hunk line".to_string());
                    };
                    let rest = chars.as_str().to_string();
                    match prefix {
                        ' ' => hunk.lines.push(HunkLine::Context(rest)),
                        '+' => hunk.lines.push(HunkLine::Add(rest)),
                        '-' => hunk.lines.push(HunkLine::Del(rest)),
                        _ => return Err(format!("invalid hunk line prefix {prefix:?}")),
                    }
                    i += 1;
                }
                hunks.push(hunk);
            }

            ops.push(PatchOp::UpdateFile {
                path: path.to_string(),
                move_to,
                hunks,
            });
            continue;
        }

        return Err(format!("unexpected line: {line:?}"));
    }

    Err("missing *** End Patch".to_string())
}

fn apply_update_hunks(original: &str, hunks: &[UpdateHunk]) -> Result<String, String> {
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if had_trailing_newline && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let mut cursor = 0usize;
    for hunk in hunks {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(s) => {
                    before.push(s.as_str());
                    after.push(s.as_str());
                }
                HunkLine::Del(s) => before.push(s.as_str()),
                HunkLine::Add(s) => after.push(s.as_str()),
            }
        }

        let pos = if before.is_empty() {
            cursor.min(lines.len())
        } else if let Some(p) = find_subsequence(&lines, cursor, &before) {
            p
        } else if let Some(p) = find_subsequence(&lines, 0, &before) {
            p
        } else {
            return Err("hunk context not found".to_string());
        };

        let end = pos.saturating_add(before.len()).min(lines.len());
        let replacement: Vec<String> = after.iter().map(|s| (*s).to_string()).collect();
        lines.splice(pos..end, replacement.clone());
        cursor = pos.saturating_add(replacement.len());
    }

    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

fn find_subsequence(haystack: &[String], start: usize, needle: &[&str]) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(haystack.len()));
    }
    if haystack.len() < needle.len() || start >= haystack.len() {
        return None;
    }

    for i in start..=haystack.len().saturating_sub(needle.len()) {
        let mut ok = true;
        for (j, n) in needle.iter().enumerate() {
            if haystack[i + j] != *n {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    None
}

#[derive(Debug, Clone)]
struct UnifiedDiff {
    text: String,
    added_lines: usize,
    removed_lines: usize,
}

fn small_unified_diff(old: &str, new: &str, path: &str) -> Option<UnifiedDiff> {
    if old == new {
        return None;
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    const MAX_LINES: usize = 2_000;
    if old_lines.len() > MAX_LINES || new_lines.len() > MAX_LINES {
        return None;
    }

    let ops = diff_ops(&old_lines, &new_lines);
    let mut added_lines = 0usize;
    let mut removed_lines = 0usize;
    let mut change_indices = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        match op.kind {
            DiffOpKind::Add => {
                added_lines += 1;
                change_indices.push(idx);
            }
            DiffOpKind::Del => {
                removed_lines += 1;
                change_indices.push(idx);
            }
            DiffOpKind::Eq => {}
        }
    }

    const MAX_CHANGED_LINES: usize = 80;
    if added_lines + removed_lines > MAX_CHANGED_LINES {
        return None;
    }

    let hunks = diff_hunks(&ops, &change_indices, 3);
    let mut out_lines = Vec::new();
    out_lines.push(format!("diff --git a/{path} b/{path}"));
    out_lines.push(format!("--- a/{path}"));
    out_lines.push(format!("+++ b/{path}"));

    let mut rendered_lines = out_lines.len();
    for hunk in hunks {
        out_lines.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
        ));
        rendered_lines += 1;
        for op in &ops[hunk.start..hunk.end] {
            let prefix = match op.kind {
                DiffOpKind::Eq => ' ',
                DiffOpKind::Del => '-',
                DiffOpKind::Add => '+',
            };
            out_lines.push(format!("{prefix}{}", op.text));
            rendered_lines += 1;
        }
    }

    const MAX_RENDERED_LINES: usize = 180;
    if rendered_lines > MAX_RENDERED_LINES {
        return None;
    }

    Some(UnifiedDiff {
        text: out_lines.join("\n"),
        added_lines,
        removed_lines,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOpKind {
    Eq,
    Del,
    Add,
}

#[derive(Debug, Clone)]
struct DiffOp<'a> {
    kind: DiffOpKind,
    text: &'a str,
}

fn diff_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp<'a>> {
    let n = old.len();
    let m = new.len();

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if old[i] == new[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut ops = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(DiffOp {
                kind: DiffOpKind::Eq,
                text: old[i],
            });
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp {
                kind: DiffOpKind::Del,
                text: old[i],
            });
            i += 1;
        } else {
            ops.push(DiffOp {
                kind: DiffOpKind::Add,
                text: new[j],
            });
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp {
            kind: DiffOpKind::Del,
            text: old[i],
        });
        i += 1;
    }
    while j < m {
        ops.push(DiffOp {
            kind: DiffOpKind::Add,
            text: new[j],
        });
        j += 1;
    }
    ops
}

#[derive(Debug, Clone)]
struct DiffHunk {
    start: usize,
    end: usize,
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

fn diff_hunks(ops: &[DiffOp<'_>], change_indices: &[usize], context: usize) -> Vec<DiffHunk> {
    if change_indices.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut current = (
        change_indices[0].saturating_sub(context),
        (change_indices[0] + context + 1).min(ops.len()),
    );

    for &idx in change_indices.iter().skip(1) {
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(ops.len());
        if start <= current.1 {
            current.1 = current.1.max(end);
        } else {
            ranges.push(current);
            current = (start, end);
        }
    }
    ranges.push(current);

    let mut old_pos = vec![0usize; ops.len() + 1];
    let mut new_pos = vec![0usize; ops.len() + 1];
    for (i, op) in ops.iter().enumerate() {
        old_pos[i + 1] = old_pos[i] + usize::from(op.kind != DiffOpKind::Add);
        new_pos[i + 1] = new_pos[i] + usize::from(op.kind != DiffOpKind::Del);
    }

    ranges
        .into_iter()
        .map(|(start, end)| DiffHunk {
            start,
            end,
            old_start: old_pos[start] + 1,
            old_len: old_pos[end].saturating_sub(old_pos[start]),
            new_start: new_pos[start] + 1,
            new_len: new_pos[end].saturating_sub(new_pos[start]),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_update_hunks_replaces_block() {
        let original = "a\nb\nc\n";
        let hunks = vec![UpdateHunk {
            header: None,
            lines: vec![
                HunkLine::Context("a".into()),
                HunkLine::Del("b".into()),
                HunkLine::Add("bb".into()),
                HunkLine::Context("c".into()),
            ],
        }];
        let out = apply_update_hunks(original, &hunks).unwrap();
        assert_eq!(out, "a\nbb\nc\n");
    }

    #[test]
    fn parse_patch_add_update_delete() {
        let patch = "\
*** Begin Patch
*** Add File: a.txt
+hello
*** Update File: a.txt
@@
 hello
+world
*** Delete File: a.txt
*** End Patch
";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
    }
}
