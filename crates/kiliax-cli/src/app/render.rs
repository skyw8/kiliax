use std::path::{Path, PathBuf};
use std::time::Duration;

use kiliax_core::protocol::TokenUsage;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use super::{PendingToolCall, PendingToolCallKind};

pub(super) fn render_user_message_lines(content: &str) -> Vec<Line<'static>> {
    let payload = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".to_string());
    vec![Line::from(Span::from(format!(
        "{}{}",
        crate::history::USER_MESSAGE_MARKER_PREFIX,
        payload
    )))]
}

fn fmt_duration_compact(duration: Duration) -> String {
    let ms = duration.as_millis() as u64;
    if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

pub(super) fn turn_divider_marker(elapsed: Duration, output_tokens: u64) -> Line<'static> {
    Line::from(Span::from(format!(
        "{}{},{}",
        crate::history::DIVIDER_MARKER_PREFIX,
        elapsed.as_millis(),
        output_tokens
    )))
}

pub(super) fn render_token_usage_line(usage: TokenUsage) -> Line<'static> {
    let mut text = format!(
        "Tokens: in {} · out {} · total {}",
        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
    );
    if let Some(cached_tokens) = usage.cached_tokens.filter(|v| *v > 0) {
        text.push_str(&format!(" · cached {}", cached_tokens));
    }

    let mut line = Line::from(Span::from(text));
    line.style = Style::default().dim();
    line
}

pub(super) fn render_thinking_start_line(step: usize) -> Line<'static> {
    let summary_style = Style::default().dim().italic();
    let label = format!("Thinking (step {step})");
    Line::from(vec![
        Span::from("• ").dim(),
        Span::styled(label, summary_style),
    ])
}

#[derive(Debug, Clone)]
struct ToolSummary {
    tool: String,
    rest: String,
}

fn tool_name_span(tool: &str) -> Span<'static> {
    let style = match tool {
        "read_file" | "list_dir" | "grep_files" | "view_image" | "shell_command"
        | "write_stdin" | "apply_patch" | "update_plan" => Style::default().fg(Color::Cyan).bold(),
        _ => Style::default().bold(),
    };
    Span::styled(tool.to_string(), style)
}

pub(super) fn render_tool_result_fallback_lines(
    tool_call_id: &str,
    elapsed: Option<Duration>,
    content: &str,
) -> Vec<Line<'static>> {
    let summary_style = Style::default().dim();
    let duration = elapsed
        .map(fmt_duration_compact)
        .unwrap_or_else(|| "—".to_string());
    let spans = vec![
        Span::from("• ").dim(),
        Span::from("Tool").bold(),
        Span::from(" "),
        Span::styled(tool_call_id.to_string(), summary_style),
        Span::from(" "),
        Span::styled(format!("({duration})"), summary_style),
    ];

    let mut out = vec![Line::from(spans)];
    if !content.trim().is_empty() {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(truncate_one_line(content, 120), summary_style),
        ]));
    }
    out
}

pub(super) fn render_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
    tool_content: &str,
) -> Vec<Line<'static>> {
    if matches!(pending.kind, PendingToolCallKind::UpdatePlan { .. }) {
        return render_update_plan_tool_result_lines(pending, elapsed);
    }

    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, tool_content);

    let mut header = vec![Span::from("• ").dim(), tool_name_span(&summary.tool)];
    if !summary.rest.is_empty() {
        header.push(Span::from(" "));
        header.push(Span::from(summary.rest));
    }
    if let Some(duration) = duration {
        header.push(Span::from(" "));
        header.push(Span::styled(
            format!("({duration})"),
            Style::default().dim(),
        ));
    }

    let mut out = vec![Line::from(header)];
    if let Some(detail) = detail {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(detail, Style::default().dim()),
        ]));
    }
    out
}

fn render_update_plan_tool_result_lines(
    pending: &PendingToolCall,
    elapsed: Option<Duration>,
) -> Vec<Line<'static>> {
    let duration = elapsed.map(fmt_duration_compact);
    let (summary, detail) = summarize_tool_result(pending, "");

    let mut header = vec![Span::from("• ").dim(), tool_name_span(&summary.tool)];
    if !summary.rest.is_empty() {
        header.push(Span::from(" "));
        header.push(Span::from(summary.rest));
    }
    if let Some(duration) = duration {
        header.push(Span::from(" "));
        header.push(Span::styled(
            format!("({duration})"),
            Style::default().dim(),
        ));
    }

    let mut out = vec![Line::from(header)];
    if let Some(detail) = detail {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(detail, Style::default().dim()),
        ]));
    }

    let Ok(args) = serde_json::from_str::<UpdatePlanArgs>(&pending.arguments) else {
        return out;
    };

    for item in args.plan.iter().take(8) {
        let style = match item.status.as_str() {
            "completed" => Style::default().fg(Color::Green).dim().crossed_out(),
            "in_progress" => Style::default().fg(Color::Cyan).dim(),
            _ => Style::default().dim(),
        };
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled("[] ".to_string(), style),
            Span::styled(item.step.clone(), style),
        ]));
    }
    if args.plan.len() > 8 {
        out.push(Line::from(vec![
            Span::from("  └ ").dim(),
            Span::styled(
                format!("… ({} more steps)", args.plan.len().saturating_sub(8)),
                Style::default().dim(),
            ),
        ]));
    }

    out
}

fn truncate_one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            break;
        }
        if out.chars().count() >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn cmd_basename(cmd: &str) -> &str {
    cmd.rsplit(['/', '\\']).next().unwrap_or(cmd)
}

fn cmd_basename_no_ext(cmd: &str) -> &str {
    let base = cmd_basename(cmd);
    base.strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base)
}

fn is_env_assignment_token(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn quote_for_display(token: &str) -> String {
    if !token.chars().any(|ch| ch.is_whitespace()) {
        return token.to_string();
    }
    let mut out = String::with_capacity(token.len().saturating_add(2));
    out.push('"');
    for ch in token.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn abbreviate_long_arg(token: &str) -> String {
    const MAX_ARG_CHARS: usize = 40;
    if token.chars().count() <= MAX_ARG_CHARS {
        return token.to_string();
    }
    if token.contains('/') || token.contains('\\') {
        let parts: Vec<&str> = token.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        if parts.len() > 3 {
            return format!("…/{}", parts[parts.len() - 3..].join("/"));
        }
    }
    truncate_one_line(token, MAX_ARG_CHARS)
}

fn split_shell_words(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in script.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if !in_single && ch == '\\' {
            escape = true;
            continue;
        }
        if !in_double && ch == '\'' {
            in_single = !in_single;
            continue;
        }
        if !in_single && ch == '"' {
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(cur);
                cur = String::new();
            }
            continue;
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn split_shell_script(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut chars = script.chars().peekable();

    while let Some(ch) = chars.next() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if !in_single && ch == '\\' {
            escape = true;
            cur.push(ch);
            continue;
        }
        if !in_double && ch == '\'' {
            in_single = !in_single;
            cur.push(ch);
            continue;
        }
        if !in_single && ch == '"' {
            in_double = !in_double;
            cur.push(ch);
            continue;
        }
        if !in_single && !in_double {
            if ch == '&' && chars.peek().is_some_and(|c| *c == '&') {
                chars.next();
                let seg = cur.trim();
                if !seg.is_empty() {
                    out.push(seg.to_string());
                }
                cur.clear();
                continue;
            }
            if ch == '|' && chars.peek().is_some_and(|c| *c == '|') {
                chars.next();
                let seg = cur.trim();
                if !seg.is_empty() {
                    out.push(seg.to_string());
                }
                cur.clear();
                continue;
            }
            if ch == ';' || ch == '\n' || ch == '\r' {
                let seg = cur.trim();
                if !seg.is_empty() {
                    out.push(seg.to_string());
                }
                cur.clear();
                continue;
            }
        }
        cur.push(ch);
    }

    let seg = cur.trim();
    if !seg.is_empty() {
        out.push(seg.to_string());
    }
    out
}

fn split_shell_pipeline(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in script.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if !in_single && ch == '\\' {
            escape = true;
            cur.push(ch);
            continue;
        }
        if !in_double && ch == '\'' {
            in_single = !in_single;
            cur.push(ch);
            continue;
        }
        if !in_single && ch == '"' {
            in_double = !in_double;
            cur.push(ch);
            continue;
        }
        if !in_single && !in_double && ch == '|' {
            let seg = cur.trim();
            if !seg.is_empty() {
                out.push(seg.to_string());
            }
            cur.clear();
            continue;
        }
        cur.push(ch);
    }

    let seg = cur.trim();
    if !seg.is_empty() {
        out.push(seg.to_string());
    }
    out
}

fn is_setup_shell_segment(segment: &str) -> bool {
    let s = segment.trim_start();
    if s == "cd" || s.starts_with("cd ") || s.starts_with("cd\t") {
        return true;
    }
    for prefix in ["export ", "set ", "unset ", "source ", ". "] {
        if s.starts_with(prefix) {
            return true;
        }
    }
    let words = split_shell_words(s);
    !words.is_empty() && words.iter().all(|w| is_env_assignment_token(w))
}

fn summarize_command_tokens(tokens: &[String]) -> String {
    const MAX_TOKENS: usize = 8;
    const MAX_POSITIONALS: usize = 2;
    const MAX_CHARS: usize = 100;

    if tokens.is_empty() {
        return String::new();
    }

    let mut out: Vec<String> = Vec::new();
    out.push(cmd_basename_no_ext(&tokens[0]).to_string());

    let mut i = 1usize;
    let mut positionals = 0usize;
    let mut omitted = false;

    while i < tokens.len() && out.len() < MAX_TOKENS {
        let t = tokens[i].as_str();
        if t == "--" {
            out.push("--".to_string());
            i += 1;
            if i < tokens.len() && out.len() < MAX_TOKENS {
                out.push(quote_for_display(&abbreviate_long_arg(&tokens[i])));
                i += 1;
            }
            break;
        }
        if t.starts_with('-') {
            out.push(quote_for_display(&abbreviate_long_arg(t)));
            if i + 1 < tokens.len() && out.len() < MAX_TOKENS {
                let next = tokens[i + 1].as_str();
                if !next.starts_with('-') && next != "--" {
                    out.push(quote_for_display(&abbreviate_long_arg(next)));
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        positionals += 1;
        if positionals > MAX_POSITIONALS {
            omitted = true;
            break;
        }
        out.push(quote_for_display(&abbreviate_long_arg(t)));
        i += 1;
    }

    if i < tokens.len() {
        omitted = true;
    }

    let mut text = out.join(" ");
    if omitted && !text.ends_with('…') {
        text.push_str(" …");
    }
    truncate_one_line(&text, MAX_CHARS)
}

fn summarize_shell_script_command(script: &str) -> String {
    let segments = split_shell_script(script);
    if segments.is_empty() {
        return String::new();
    }

    let real_segments: Vec<&str> = segments
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !is_setup_shell_segment(s))
        .collect();
    let selected = real_segments
        .first()
        .copied()
        .unwrap_or_else(|| segments[0].as_str());

    let stages = split_shell_pipeline(selected);
    let mut rendered = Vec::new();
    for stage in stages.iter().take(2) {
        let mut words = split_shell_words(stage);
        if words.first().is_some_and(|w| w == "env") {
            words.remove(0);
        }
        while words.first().is_some_and(|w| is_env_assignment_token(w)) {
            words.remove(0);
        }
        if words.is_empty() {
            continue;
        }
        rendered.push(summarize_command_tokens(&words));
    }

    let mut summary = rendered.join(" | ");
    if stages.len() > 2 && !summary.is_empty() && !summary.ends_with('…') {
        summary.push_str(" | …");
    }
    if real_segments.len() > 1 && !summary.is_empty() && !summary.ends_with('…') {
        summary.push_str(" …");
    }
    if summary.is_empty() {
        summary = truncate_one_line(selected, 100);
    }
    summary
}

pub(super) fn summarize_shell_command(cmd: &str) -> String {
    let summary = summarize_shell_script_command(cmd);
    if !summary.trim().is_empty() {
        return summary;
    }
    truncate_one_line(cmd, 100)
}

pub(super) fn format_error_chain_text(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(src) = cur {
        out.push_str("\ncaused by: ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

pub(super) fn render_dir_list_lines(
    workspace_root: &Path,
    extra_roots: &[PathBuf],
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(vec![
        Span::from("• ").dim(),
        Span::from("workspace").bold(),
        Span::from(": ").dim(),
        Span::from(workspace_root.display().to_string()).dim(),
    ]));
    if extra_roots.is_empty() {
        out.push(Line::from(vec![
            Span::from("• ").dim(),
            Span::from("dir").bold(),
            Span::from(": ").dim(),
            Span::from("(none)").dim(),
        ]));
        return out;
    }
    for dir in extra_roots {
        out.push(Line::from(vec![
            Span::from("• ").dim(),
            Span::from("dir").bold(),
            Span::from(": ").dim(),
            Span::from(dir.display().to_string()).dim(),
        ]));
    }
    out
}

pub(super) fn render_error_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    out.push(Line::from(vec![
        Span::from("• ").dim(),
        Span::styled("error", Style::default().fg(Color::LightRed).bold()),
        Span::from(": "),
        Span::from(first.to_string()),
    ]));
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        out.push(Line::from(vec![
            Span::from("  ").dim(),
            Span::from(line.to_string()),
        ]));
    }
    out
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ListDirArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct GrepFilesArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ViewImageArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ShellCommandArgs {
    cmd: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteStdinArgs {
    session_id: u64,
}

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanArgs {
    #[allow(dead_code)]
    explanation: Option<String>,
    plan: Vec<UpdatePlanItem>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanItem {
    step: String,
    status: String,
}

fn extract_patch_files(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in patch.lines() {
        let line = raw.trim_end_matches('\r');
        for prefix in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let p = rest.trim();
                if !p.is_empty() {
                    out.push(p.to_string());
                }
                break;
            }
        }
    }
    out
}

pub(super) fn classify_tool_call(call: &kiliax_core::protocol::ToolCall) -> PendingToolCallKind {
    match call.name.as_str() {
        "read_file" => serde_json::from_str::<ReadFileArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ReadFile { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "list_dir" => serde_json::from_str::<ListDirArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ListDir { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "grep_files" => serde_json::from_str::<GrepFilesArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::GrepFiles {
                pattern: args.pattern,
                path: args.path,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "view_image" => serde_json::from_str::<ViewImageArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ViewImage { path: args.path })
            .unwrap_or(PendingToolCallKind::Other),
        "shell_command" => serde_json::from_str::<ShellCommandArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ShellCommand {
                cmd: args.cmd,
                cwd: args.cwd,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "write_stdin" => serde_json::from_str::<WriteStdinArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::WriteStdin {
                session_id: args.session_id,
            })
            .unwrap_or(PendingToolCallKind::Other),
        "apply_patch" => serde_json::from_str::<ApplyPatchArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::ApplyPatch {
                files: extract_patch_files(&args.patch),
            })
            .unwrap_or(PendingToolCallKind::Other),
        "update_plan" => serde_json::from_str::<UpdatePlanArgs>(&call.arguments)
            .ok()
            .map(|args| PendingToolCallKind::UpdatePlan {
                steps: args.plan.len(),
            })
            .unwrap_or(PendingToolCallKind::Other),
        _ => PendingToolCallKind::Other,
    }
}

fn summarize_tool_result(
    pending: &PendingToolCall,
    tool_content: &str,
) -> (ToolSummary, Option<String>) {
    match &pending.kind {
        PendingToolCallKind::ReadFile { path } => {
            let line_count = tool_content.lines().count();
            (
                ToolSummary {
                    tool: "read_file".to_string(),
                    rest: path.clone(),
                },
                Some(format!("{line_count} lines")),
            )
        }
        PendingToolCallKind::ListDir { path } => {
            let entry_count = tool_content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            (
                ToolSummary {
                    tool: "list_dir".to_string(),
                    rest: path.clone(),
                },
                Some(format!("{entry_count} entries")),
            )
        }
        PendingToolCallKind::GrepFiles { pattern, path } => {
            let match_count = tool_content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            let mut rest = pattern.clone();
            if let Some(path) = path.as_deref() {
                if !path.is_empty() && path != "." {
                    rest.push_str(&format!(" ({path})"));
                }
            }
            (
                ToolSummary {
                    tool: "grep_files".to_string(),
                    rest,
                },
                Some(format!("{match_count} matches")),
            )
        }
        PendingToolCallKind::ViewImage { path } => (
            ToolSummary {
                tool: "view_image".to_string(),
                rest: path.clone(),
            },
            None,
        ),
        PendingToolCallKind::ShellCommand { cmd, cwd } => {
            let cmd = summarize_shell_command(cmd);
            let mut detail = String::new();
            if let Ok(parsed) = serde_json::from_str::<ShellCommandOutput>(tool_content) {
                if parsed.running {
                    if let Some(id) = parsed.session_id {
                        detail.push_str(&format!("running · session {id}"));
                    } else {
                        detail.push_str("running");
                    }
                } else if let Some(code) = parsed.exit_code {
                    detail.push_str(&format!("exit {code}"));
                }
            } else if !tool_content.trim().is_empty() {
                detail.push_str(&truncate_one_line(tool_content, 120));
            }
            if let Some(cwd) = cwd.as_deref() {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(&format!("cwd {cwd}"));
            }
            (
                ToolSummary {
                    tool: "shell_command".to_string(),
                    rest: cmd,
                },
                if detail.is_empty() {
                    None
                } else {
                    Some(detail)
                },
            )
        }
        PendingToolCallKind::WriteStdin { session_id } => {
            let mut detail = String::new();
            if let Ok(parsed) = serde_json::from_str::<ShellCommandOutput>(tool_content) {
                if parsed.running {
                    detail.push_str("running");
                } else if let Some(code) = parsed.exit_code {
                    detail.push_str(&format!("exit {code}"));
                }
            } else if !tool_content.trim().is_empty() {
                detail.push_str(&truncate_one_line(tool_content, 120));
            }
            (
                ToolSummary {
                    tool: "write_stdin".to_string(),
                    rest: format!("session {session_id}"),
                },
                if detail.is_empty() {
                    None
                } else {
                    Some(detail)
                },
            )
        }
        PendingToolCallKind::ApplyPatch { files } => {
            let rest = match files.len() {
                0 => String::new(),
                1 => files[0].clone(),
                n => format!("{n} files"),
            };
            (
                ToolSummary {
                    tool: "apply_patch".to_string(),
                    rest,
                },
                None,
            )
        }
        PendingToolCallKind::UpdatePlan { steps } => (
            ToolSummary {
                tool: "update_plan".to_string(),
                rest: format!("{steps} steps"),
            },
            None,
        ),
        PendingToolCallKind::Other => (
            ToolSummary {
                tool: pending.name.clone(),
                rest: String::new(),
            },
            Some(truncate_one_line(&pending.arguments, 120)),
        ),
    }
}

pub(super) fn tool_status_label(pending: &PendingToolCall) -> String {
    match &pending.kind {
        PendingToolCallKind::ReadFile { path } => format!("read_file {path}"),
        PendingToolCallKind::ListDir { path } => format!("list_dir {path}"),
        PendingToolCallKind::GrepFiles { pattern, .. } => format!("grep_files {pattern}"),
        PendingToolCallKind::ViewImage { path } => format!("view_image {path}"),
        PendingToolCallKind::ShellCommand { cmd, .. } => {
            format!("shell_command {}", summarize_shell_command(cmd))
        }
        PendingToolCallKind::WriteStdin { session_id } => format!("write_stdin {session_id}"),
        PendingToolCallKind::ApplyPatch { files } => match files.len() {
            0 => "apply_patch".to_string(),
            1 => format!("apply_patch {}", files[0]),
            n => format!("apply_patch {n} files"),
        },
        PendingToolCallKind::UpdatePlan { steps } => format!("update_plan {steps}"),
        PendingToolCallKind::Other => pending.name.clone(),
    }
}

#[derive(Debug, Deserialize)]
struct ShellCommandOutput {
    #[allow(dead_code)]
    session_id: Option<u64>,
    #[allow(dead_code)]
    running: bool,
    #[allow(dead_code)]
    exit_code: Option<i32>,
    #[allow(dead_code)]
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}
