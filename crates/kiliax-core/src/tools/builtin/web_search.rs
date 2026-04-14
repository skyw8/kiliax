use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::ToolError;

use super::common::parse_args;
use super::TOOL_WEB_SEARCH;

pub fn web_search_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WEB_SEARCH.to_string(),
        description: Some(
            "Search the web via Tavily. Configure `web_search.api_key` (or `tools.tavily.api_key`) in kiliax.yaml (fallback: `TAVILY_API_KEY`). Returns JSON results."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query." },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Maximum number of results.", "default": 5 },
                "search_depth": { "type": "string", "enum": ["basic", "advanced"], "description": "Search depth.", "default": "basic" },
                "include_answer": { "type": "boolean", "description": "Include Tavily's short answer when available.", "default": false },
                "include_raw_content": { "type": "boolean", "description": "Include raw page content (may be large).", "default": false }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct WebSearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    search_depth: Option<SearchDepth>,
    #[serde(default)]
    include_answer: bool,
    #[serde(default)]
    include_raw_content: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchDepth {
    Basic,
    Advanced,
}

impl SearchDepth {
    fn as_tavily_str(self) -> &'static str {
        match self {
            SearchDepth::Basic => "basic",
            SearchDepth::Advanced => "advanced",
        }
    }
}

#[derive(Debug, Serialize)]
struct TavilySearchRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_results: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_depth: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_answer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_raw_content: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    raw_content: Option<String>,
}

#[derive(Debug, Serialize)]
struct WebSearchOutput {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<String>,
    results: Vec<WebSearchResult>,
}

#[derive(Debug, Serialize)]
struct WebSearchResult {
    title: String,
    url: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_content: Option<String>,
}

pub(super) async fn execute(
    config: &crate::config::Config,
    call: &ToolCall,
) -> Result<String, ToolError> {
    let args: WebSearchArgs = parse_args(call, TOOL_WEB_SEARCH)?;
    let query = args.query.trim();
    if query.is_empty() {
        return Err(ToolError::InvalidCommand(
            "query must not be empty".to_string(),
        ));
    }

    let api_key = config
        .web_search
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("TAVILY_API_KEY").ok())
        .ok_or_else(|| {
            ToolError::InvalidCommand(
                "missing web_search.api_key (or tools.tavily.api_key) in kiliax.yaml (or env var TAVILY_API_KEY)"
                    .to_string(),
            )
        })?;

    let max_results = args.max_results.unwrap_or(5).clamp(1, 10);
    let depth = args.search_depth.unwrap_or(SearchDepth::Basic);

    let base = config
        .web_search
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("TAVILY_API_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.tavily.com".to_string());
    let base = base.trim().trim_end_matches('/');
    let url = format!("{base}/search");

    let req_body = TavilySearchRequest {
        api_key: api_key.as_str(),
        query,
        max_results: Some(max_results),
        search_depth: Some(depth.as_tavily_str()),
        include_answer: Some(args.include_answer),
        include_raw_content: Some(args.include_raw_content),
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(to_io_error)?;

    let resp = client
        .post(url)
        .json(&req_body)
        .send()
        .await
        .map_err(to_io_error)?;

    let status = resp.status();
    let text = resp.text().await.map_err(to_io_error)?;
    if !status.is_success() {
        return Err(ToolError::InvalidCommand(format!(
            "tavily HTTP {status}: {}",
            truncate_chars(&text, 400)
        )));
    }

    let parsed: TavilySearchResponse = serde_json::from_str(&text)
        .map_err(|e| ToolError::InvalidCommand(format!("invalid tavily JSON response: {e}")))?;

    let out = simplify_response(parsed, args.include_answer, args.include_raw_content);
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string()))
}

fn simplify_response(
    resp: TavilySearchResponse,
    include_answer: bool,
    include_raw_content: bool,
) -> WebSearchOutput {
    const MAX_SNIPPET_CHARS: usize = 800;
    const MAX_RAW_CHARS: usize = 2_000;
    const MAX_ANSWER_CHARS: usize = 1_200;

    let query = resp.query.unwrap_or_default();
    let answer = if include_answer {
        resp.answer
            .as_deref()
            .map(|s| truncate_chars(s, MAX_ANSWER_CHARS))
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    };

    let mut results = Vec::new();
    for r in resp.results.into_iter().take(10) {
        let title = r.title.unwrap_or_default().trim().to_string();
        let url = r.url.unwrap_or_default().trim().to_string();
        let content = r
            .content
            .as_deref()
            .map(|s| truncate_chars(s, MAX_SNIPPET_CHARS))
            .unwrap_or_default()
            .trim()
            .to_string();
        let raw_content = if include_raw_content {
            r.raw_content
                .as_deref()
                .map(|s| truncate_chars(s, MAX_RAW_CHARS))
                .filter(|s| !s.trim().is_empty())
        } else {
            None
        };

        results.push(WebSearchResult {
            title,
            url,
            content,
            score: r.score,
            raw_content,
        });
    }

    WebSearchOutput {
        query,
        answer,
        results,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn to_io_error<E: std::fmt::Display>(err: E) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        err.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_text() {
        let s = "a".repeat(10);
        assert_eq!(truncate_chars(&s, 0), "");
        assert_eq!(truncate_chars(&s, 3), "aaa…");
        assert_eq!(truncate_chars("hi", 3), "hi");
    }

    #[test]
    fn simplifies_response_limits_fields() {
        let resp = TavilySearchResponse {
            query: Some("q".to_string()),
            answer: Some("a".repeat(2_000)),
            results: vec![TavilyResult {
                title: Some("t".to_string()),
                url: Some("u".to_string()),
                content: Some("c".repeat(2_000)),
                score: Some(0.9),
                raw_content: Some("r".repeat(5_000)),
            }],
        };

        let out = simplify_response(resp, true, true);
        assert_eq!(out.query, "q");
        assert!(out.answer.unwrap().chars().count() <= 1_201);
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].content.chars().count() <= 801);
        assert!(out.results[0].raw_content.as_ref().unwrap().chars().count() <= 2_001);
    }
}
