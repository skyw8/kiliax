use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::TOOL_EDIT_FILE;

const DESCRIPTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/tools/edit_file.md"
));

pub fn edit_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_EDIT_FILE.to_string(),
        description: Some(DESCRIPTION.to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "The absolute path to the file to modify" },
                "oldString": { "type": "string", "description": "The text to replace" },
                "newString": { "type": "string", "description": "The text to replace it with (must be different from oldString)" },
                "replaceAll": { "type": "boolean", "description": "Replace all occurrences of oldString (default false)" }
            },
            "required": ["filePath", "oldString", "newString"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "oldString")]
    old_string: String,
    #[serde(rename = "newString")]
    new_string: String,
    #[serde(default, rename = "replaceAll")]
    replace_all: bool,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_EDIT_FILE.to_string()));
    }
    let args: EditFileArgs = parse_args(call, TOOL_EDIT_FILE)?;
    if args.old_string == args.new_string {
        return Err(ToolError::InvalidCommand(
            "No changes to apply: oldString and newString are identical.".to_string(),
        ));
    }

    let abs = resolve_workspace_path(workspace_root, extra_workspace_roots, &args.file_path)?;

    if args.old_string.is_empty() {
        if let Some(meta) = metadata_if_exists(&abs).await? {
            if meta.is_dir() {
                return Err(ToolError::InvalidCommand(format!(
                    "path is a directory, not a file: {}",
                    abs.display()
                )));
            }
        }
        let source = read_text_with_bom_if_exists(&abs).await?;
        let next = TextWithBom::split(&args.new_string);
        let desired_bom = source.as_ref().is_some_and(|s| s.bom) || next.bom;
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&abs, join_bom(&next.text, desired_bom)).await?;
        return Ok("Edit applied successfully.".to_string());
    }

    let meta = tokio::fs::metadata(&abs).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::InvalidCommand(format!("File {} not found", abs.display()))
        } else {
            ToolError::Io(e)
        }
    })?;
    if meta.is_dir() {
        return Err(ToolError::InvalidCommand(format!(
            "Path is a directory, not a file: {}",
            abs.display()
        )));
    }

    let source = TextWithBom::split(&tokio::fs::read_to_string(&abs).await?);
    let ending = detect_line_ending(&source.text);
    let old_string = convert_to_line_ending(&normalize_line_endings(&args.old_string), ending);
    let replacement = convert_to_line_ending(&normalize_line_endings(&args.new_string), ending);
    let next = TextWithBom::split(&replacement);
    let content_new = replace_content(&source.text, &old_string, &next.text, args.replace_all)?;
    let desired_bom = source.bom || next.bom;

    tokio::fs::write(&abs, join_bom(&content_new, desired_bom)).await?;

    Ok("Edit applied successfully.".to_string())
}

#[derive(Debug, Clone)]
struct TextWithBom {
    bom: bool,
    text: String,
}

impl TextWithBom {
    fn split(text: &str) -> Self {
        if let Some(rest) = text.strip_prefix('\u{feff}') {
            Self {
                bom: true,
                text: rest.to_string(),
            }
        } else {
            Self {
                bom: false,
                text: text.to_string(),
            }
        }
    }
}

async fn metadata_if_exists(path: &Path) -> Result<Option<std::fs::Metadata>, ToolError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(Some(meta)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

async fn read_text_with_bom_if_exists(path: &Path) -> Result<Option<TextWithBom>, ToolError> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(TextWithBom::split(&text))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn join_bom(text: &str, bom: bool) -> String {
    if bom {
        format!("\u{feff}{text}")
    } else {
        text.to_string()
    }
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn detect_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn convert_to_line_ending(text: &str, ending: &str) -> String {
    if ending == "\n" {
        text.to_string()
    } else {
        text.replace('\n', "\r\n")
    }
}

fn replace_content(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String, ToolError> {
    if old_string == new_string {
        return Err(ToolError::InvalidCommand(
            "No changes to apply: oldString and newString are identical.".to_string(),
        ));
    }

    let mut not_found = true;
    for search in replacement_candidates(content, old_string) {
        let Some(index) = content.find(&search) else {
            continue;
        };
        not_found = false;
        if replace_all {
            return Ok(content.replace(&search, new_string));
        }
        if content.rfind(&search) != Some(index) {
            continue;
        }
        let mut out = String::new();
        out.push_str(&content[..index]);
        out.push_str(new_string);
        out.push_str(&content[index + search.len()..]);
        return Ok(out);
    }

    if not_found {
        Err(ToolError::InvalidCommand(
            "Could not find oldString in the file. It must match exactly, including whitespace, indentation, and line endings."
                .to_string(),
        ))
    } else {
        Err(ToolError::InvalidCommand(
            "Found multiple matches for oldString. Provide more surrounding context to make the match unique."
                .to_string(),
        ))
    }
}

fn replacement_candidates(content: &str, find: &str) -> Vec<String> {
    let mut out = Vec::new();
    push_candidate(&mut out, find.to_string());
    out.extend(line_trimmed_candidates(content, find));
    out.extend(block_anchor_candidates(content, find));
    out.extend(whitespace_normalized_candidates(content, find));
    out.extend(indentation_flexible_candidates(content, find));
    out.extend(escape_normalized_candidates(content, find));
    out.extend(trimmed_boundary_candidates(content, find));
    out.extend(context_aware_candidates(content, find));
    out.extend(multi_occurrence_candidates(content, find));
    out
}

fn push_candidate(out: &mut Vec<String>, value: String) {
    if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
        out.push(value);
    }
}

fn line_trimmed_candidates(content: &str, find: &str) -> Vec<String> {
    let original_lines: Vec<&str> = content.split('\n').collect();
    let mut search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.last() == Some(&"") {
        search_lines.pop();
    }
    if search_lines.is_empty() || original_lines.len() < search_lines.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=original_lines.len() - search_lines.len() {
        if search_lines
            .iter()
            .enumerate()
            .all(|(j, search)| original_lines[i + j].trim() == search.trim())
        {
            push_candidate(
                &mut out,
                original_lines[i..i + search_lines.len()].join("\n"),
            );
        }
    }
    out
}

fn block_anchor_candidates(content: &str, find: &str) -> Vec<String> {
    let original_lines: Vec<&str> = content.split('\n').collect();
    let mut search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.len() < 3 {
        return Vec::new();
    }
    if search_lines.last() == Some(&"") {
        search_lines.pop();
    }
    if search_lines.len() < 3 {
        return Vec::new();
    }

    let first = search_lines[0].trim();
    let last = search_lines[search_lines.len() - 1].trim();
    let mut candidates = Vec::new();
    for i in 0..original_lines.len() {
        if original_lines[i].trim() != first {
            continue;
        }
        for (j, line) in original_lines.iter().enumerate().skip(i + 2) {
            if line.trim() == last {
                candidates.push((i, j));
                break;
            }
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    if candidates.len() == 1 {
        let (start, end) = candidates[0];
        return vec![original_lines[start..=end].join("\n")];
    }

    let mut best = None;
    let mut best_similarity = -1.0f64;
    for (start, end) in candidates {
        let actual_len = end - start + 1;
        let lines_to_check =
            (search_lines.len().saturating_sub(2)).min(actual_len.saturating_sub(2));
        let similarity = if lines_to_check == 0 {
            1.0
        } else {
            let mut total = 0.0;
            for j in 1..=lines_to_check {
                let original = original_lines[start + j].trim();
                let search = search_lines[j].trim();
                let max_len = original.chars().count().max(search.chars().count());
                if max_len == 0 {
                    continue;
                }
                total += 1.0 - levenshtein(original, search) as f64 / max_len as f64;
            }
            total / lines_to_check as f64
        };
        if similarity > best_similarity {
            best_similarity = similarity;
            best = Some((start, end));
        }
    }

    if best_similarity >= 0.3 {
        if let Some((start, end)) = best {
            return vec![original_lines[start..=end].join("\n")];
        }
    }
    Vec::new()
}

fn whitespace_normalized_candidates(content: &str, find: &str) -> Vec<String> {
    let normalized_find = normalize_whitespace(find);
    let mut out = Vec::new();
    let lines: Vec<&str> = content.split('\n').collect();
    for line in &lines {
        let normalized_line = normalize_whitespace(line);
        if normalized_line == normalized_find {
            push_candidate(&mut out, (*line).to_string());
        } else if normalized_line.contains(&normalized_find) {
            if let Some(candidate) = whitespace_substring_candidate(line, find) {
                push_candidate(&mut out, candidate);
            }
        }
    }

    let find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() > 1 && lines.len() >= find_lines.len() {
        for i in 0..=lines.len() - find_lines.len() {
            let block = lines[i..i + find_lines.len()].join("\n");
            if normalize_whitespace(&block) == normalized_find {
                push_candidate(&mut out, block);
            }
        }
    }
    out
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn whitespace_substring_candidate(line: &str, find: &str) -> Option<String> {
    let words: Vec<&str> = find.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let first = words[0];
    let mut cursor = 0usize;
    while let Some(rel_start) = line[cursor..].find(first) {
        let start = cursor + rel_start;
        let mut pos = start + first.len();
        let mut ok = true;
        for word in words.iter().skip(1) {
            let ws_len = line[pos..]
                .chars()
                .take_while(|ch| ch.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>();
            if ws_len == 0 {
                ok = false;
                break;
            }
            pos += ws_len;
            if !line[pos..].starts_with(word) {
                ok = false;
                break;
            }
            pos += word.len();
        }
        if ok {
            return Some(line[start..pos].to_string());
        }
        cursor = start + first.len();
    }
    None
}

fn indentation_flexible_candidates(content: &str, find: &str) -> Vec<String> {
    let normalized_find = remove_indentation(find);
    let content_lines: Vec<&str> = content.split('\n').collect();
    let find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.is_empty() || content_lines.len() < find_lines.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=content_lines.len() - find_lines.len() {
        let block = content_lines[i..i + find_lines.len()].join("\n");
        if remove_indentation(&block) == normalized_find {
            push_candidate(&mut out, block);
        }
    }
    out
}

fn remove_indentation(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let min_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min();
    let Some(min_indent) = min_indent else {
        return text.to_string();
    };
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                (*line).to_string()
            } else {
                line.get(min_indent..).unwrap_or("").to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_normalized_candidates(content: &str, find: &str) -> Vec<String> {
    let unescaped_find = unescape_string(find);
    let mut out = Vec::new();
    if content.contains(&unescaped_find) {
        push_candidate(&mut out, unescaped_find.clone());
    }

    let lines: Vec<&str> = content.split('\n').collect();
    let find_lines: Vec<&str> = unescaped_find.split('\n').collect();
    if !find_lines.is_empty() && lines.len() >= find_lines.len() {
        for i in 0..=lines.len() - find_lines.len() {
            let block = lines[i..i + find_lines.len()].join("\n");
            if unescape_string(&block) == unescaped_find {
                push_candidate(&mut out, block);
            }
        }
    }
    out
}

fn unescape_string(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('`') => out.push('`'),
            Some('\\') => out.push('\\'),
            Some('\n') => out.push('\n'),
            Some('$') => out.push('$'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn trimmed_boundary_candidates(content: &str, find: &str) -> Vec<String> {
    let trimmed = find.trim();
    if trimmed == find {
        return Vec::new();
    }
    let mut out = Vec::new();
    if content.contains(trimmed) {
        push_candidate(&mut out, trimmed.to_string());
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let find_lines: Vec<&str> = find.split('\n').collect();
    if !find_lines.is_empty() && lines.len() >= find_lines.len() {
        for i in 0..=lines.len() - find_lines.len() {
            let block = lines[i..i + find_lines.len()].join("\n");
            if block.trim() == trimmed {
                push_candidate(&mut out, block);
            }
        }
    }
    out
}

fn context_aware_candidates(content: &str, find: &str) -> Vec<String> {
    let mut find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() < 3 {
        return Vec::new();
    }
    if find_lines.last() == Some(&"") {
        find_lines.pop();
    }
    if find_lines.len() < 3 {
        return Vec::new();
    }
    let content_lines: Vec<&str> = content.split('\n').collect();
    let first = find_lines[0].trim();
    let last = find_lines[find_lines.len() - 1].trim();
    let mut out = Vec::new();
    for i in 0..content_lines.len() {
        if content_lines[i].trim() != first {
            continue;
        }
        for j in i + 2..content_lines.len() {
            if content_lines[j].trim() != last {
                continue;
            }
            let block_lines = &content_lines[i..=j];
            if block_lines.len() == find_lines.len() {
                let mut matches = 0usize;
                let mut total = 0usize;
                for k in 1..block_lines.len() - 1 {
                    let block_line = block_lines[k].trim();
                    let find_line = find_lines[k].trim();
                    if !block_line.is_empty() || !find_line.is_empty() {
                        total += 1;
                        if block_line == find_line {
                            matches += 1;
                        }
                    }
                }
                if total == 0 || matches as f64 / total as f64 >= 0.5 {
                    push_candidate(&mut out, block_lines.join("\n"));
                    break;
                }
            }
            break;
        }
    }
    out
}

fn multi_occurrence_candidates(content: &str, find: &str) -> Vec<String> {
    if content.contains(find) {
        vec![find.to_string()]
    } else {
        Vec::new()
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len());
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCall;
    use crate::tools::{Permissions, ShellPermissions};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: TOOL_EDIT_FILE.to_string(),
            arguments: args.to_string(),
        }
    }

    fn permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::DenyAll,
        }
    }

    async fn run_edit(root: &Path, args: serde_json::Value) -> Result<String, ToolError> {
        execute(
            root,
            &[],
            &permissions(),
            &call(args),
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unknown_args() {
        let tmp = tempfile::tempdir().unwrap();
        let err = run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "old",
                "newString": "new",
                "extra": true
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidArgs { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_old_string_creates_new_file_with_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "nested/a.txt",
                "oldString": "",
                "newString": "new content"
            }),
        )
        .await
        .unwrap();

        assert_eq!(out, "Edit applied successfully.");
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("nested/a.txt"))
                .await
                .unwrap(),
            "new content"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_old_string_overwrites_existing_file_without_prior_read() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "old")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "",
                "newString": "new"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "new"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn identical_old_and_new_fails_even_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let err = run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "",
                "newString": ""
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidCommand(msg) if msg.contains("identical")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_lf_line_endings_when_args_use_crlf() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "alpha\nbeta\ngamma\n")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "alpha\r\nbeta\r\ngamma",
                "newString": "alpha\r\nBETA\r\ngamma"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_crlf_line_endings_when_args_use_lf() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "alpha\r\nbeta\r\ngamma\r\n")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "alpha\nbeta\ngamma",
                "newString": "alpha\nBETA\ngamma"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "alpha\r\nBETA\r\ngamma\r\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_existing_bom() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "\u{feff}using System;\n")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "using System;",
                "newString": "using Up;"
            }),
        )
        .await
        .unwrap();

        let out = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert!(out.starts_with('\u{feff}'));
        assert_eq!(out.trim_start_matches('\u{feff}'), "using Up;\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_bom_from_new_string() {
        let tmp = tempfile::tempdir().unwrap();
        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "",
                "newString": "\u{feff}new"
            }),
        )
        .await
        .unwrap();

        let out = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert!(out.starts_with('\u{feff}'));
        assert_eq!(out.trim_start_matches('\u{feff}'), "new");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_all_replaces_all_matches() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "foo bar foo")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "foo",
                "newString": "qux",
                "replaceAll": true
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "qux bar qux"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_replacement_rejects_multiple_matches() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "foo bar foo")
            .await
            .unwrap();

        let err = run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "foo",
                "newString": "qux"
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidCommand(msg) if msg.contains("multiple matches")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fuzzy_replacement_handles_trimmed_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "alpha\nbeta\ngamma")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "\n beta \n",
                "newString": "BETA"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "alpha\nBETA\ngamma"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fuzzy_replacement_handles_indentation_flexible_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "fn main() {\n    let x = 1;\n}")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "let x = 1;",
                "newString": "let x = 2;"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "fn main() {\n    let x = 2;\n}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fuzzy_replacement_handles_whitespace_normalized_text() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "let   name = value;")
            .await
            .unwrap();

        run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "oldString": "let name = value;",
                "newString": "let name = next;"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "let name = next;"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_directory_path() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(tmp.path().join("dir")).await.unwrap();
        let err = run_edit(
            tmp.path(),
            serde_json::json!({
                "filePath": "dir",
                "oldString": "old",
                "newString": "new"
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidCommand(msg) if msg.contains("directory")));
    }
}
