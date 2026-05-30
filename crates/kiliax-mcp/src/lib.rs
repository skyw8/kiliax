mod protocol;

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use protocol::tool_definitions;

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct McpServerOptions {
    pub base_url: String,
    pub token: Option<String>,
}

pub async fn serve_stdio(options: McpServerOptions) -> Result<()> {
    let client = KiliaxHttpClient::new(options)?;
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(line) {
            Ok(req) => handle_request(&client, req).await,
            Err(err) => Some(error_response(Value::Null, -32700, err.to_string())),
        };

        if let Some(response) = response {
            let encoded = serde_json::to_vec(&response)?;
            stdout.write_all(&encoded).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

async fn handle_request(client: &KiliaxHttpClient, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let id = req.id;
    let method = req.method.as_str();
    if id.is_none() {
        return None;
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "kiliax",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(client, req.params).await,
        _ => Err(McpError::method_not_found(method)),
    };

    Some(match result {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err(err) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code: err.code,
                message: err.message,
            }),
        },
    })
}

async fn call_tool(
    client: &KiliaxHttpClient,
    params: Value,
) -> std::result::Result<Value, McpError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid_params("tools/call params.name is required"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    let value = match name {
        "get_capabilities" => client.get_json("/v1/capabilities").await?,
        "list_agents" => {
            let caps = client.get_json("/v1/capabilities").await?;
            json!({
                "agents": caps.get("agents").cloned().unwrap_or_else(|| json!([])),
                "agent_errors": caps.get("agent_errors").cloned().unwrap_or_else(|| json!([]))
            })
        }
        "list_sessions" => {
            let req: ListSessionsArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            let mut path = format!("/v1/sessions?limit={}", req.limit.unwrap_or(50));
            if let Some(live) = req.live {
                path.push_str(&format!("&live={live}"));
            }
            if let Some(cursor) = req
                .cursor
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                path.push_str("&cursor=");
                path.push_str(cursor);
            }
            client.get_json(&path).await?
        }
        "get_session" => {
            let req: SessionIdArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            client
                .get_json(&format!("/v1/sessions/{}", req.session_id))
                .await?
        }
        "get_messages" => {
            let req: GetMessagesArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            let mut path = format!(
                "/v1/sessions/{}/messages?limit={}",
                req.session_id,
                req.limit.unwrap_or(50)
            );
            if let Some(before) = req
                .before
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                path.push_str("&before=");
                path.push_str(before);
            }
            client.get_json(&path).await?
        }
        "cancel_run" => {
            let req: RunIdArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            client
                .post_json(&format!("/v1/runs/{}/cancel", req.run_id), json!({}))
                .await?
        }
        "run_agent" => {
            let req: RunAgentArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            run_agent(client, req).await?
        }
        "continue_session" => {
            let req: ContinueSessionArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            continue_session(client, req).await?
        }
        _ => return Err(McpError::invalid_params(format!("unknown tool: {name}"))),
    };

    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }]
    }))
}

async fn run_agent(
    client: &KiliaxHttpClient,
    req: RunAgentArgs,
) -> std::result::Result<Value, McpError> {
    if req.prompt.trim().is_empty() {
        return Err(McpError::invalid_params("prompt must not be empty"));
    }

    let session = client
        .post_json(
            "/v1/sessions",
            json!({
                "title": req.title,
                "settings": {
                    "agent": req.agent,
                    "model_id": req.model_id,
                    "workspace_root": req.workspace,
                    "extra_workspace_roots": req.extra_workspace_roots,
                    "mcp": req.mcp,
                    "skills": req.skills,
                    "custom_tools": req.custom_tools
                }
            }),
        )
        .await?;
    let session_id = json_string(&session, "id")?;
    let run = create_text_run(
        client,
        &session_id,
        &req.prompt,
        req.attachments,
        req.overrides,
    )
    .await?;
    let run = maybe_wait_for_run(client, run, req.wait, req.timeout_seconds).await?;
    let messages = latest_messages(client, &session_id, req.message_limit).await?;

    Ok(json!({
        "session": session,
        "run": run,
        "messages": messages,
        "final_message": latest_assistant_text(&messages)
    }))
}

async fn continue_session(
    client: &KiliaxHttpClient,
    req: ContinueSessionArgs,
) -> std::result::Result<Value, McpError> {
    if req.prompt.trim().is_empty() {
        return Err(McpError::invalid_params("prompt must not be empty"));
    }

    let run = create_text_run(
        client,
        &req.session_id,
        &req.prompt,
        req.attachments,
        req.overrides,
    )
    .await?;
    let run = maybe_wait_for_run(client, run, req.wait, req.timeout_seconds).await?;
    let messages = latest_messages(client, &req.session_id, req.message_limit).await?;

    Ok(json!({
        "session_id": req.session_id,
        "run": run,
        "messages": messages,
        "final_message": latest_assistant_text(&messages)
    }))
}

async fn create_text_run(
    client: &KiliaxHttpClient,
    session_id: &str,
    prompt: &str,
    attachments: Vec<RunAttachment>,
    overrides: Option<Value>,
) -> std::result::Result<Value, McpError> {
    client
        .post_json(
            &format!("/v1/sessions/{session_id}/runs"),
            json!({
                "input": {
                    "type": "text",
                    "text": prompt,
                    "attachments": attachments
                },
                "overrides": overrides,
                "auto_resume": true
            }),
        )
        .await
}

async fn maybe_wait_for_run(
    client: &KiliaxHttpClient,
    mut run: Value,
    wait: Option<bool>,
    timeout_seconds: Option<u64>,
) -> std::result::Result<Value, McpError> {
    if wait == Some(false) {
        return Ok(run);
    }

    let run_id = json_string(&run, "id")?;
    let timeout = timeout_seconds
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAIT_TIMEOUT);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if is_terminal_run(&run) {
            return Ok(run);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(McpError::internal(format!(
                "timed out waiting for run {run_id} after {}s",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        run = client.get_json(&format!("/v1/runs/{run_id}")).await?;
    }
}

async fn latest_messages(
    client: &KiliaxHttpClient,
    session_id: &str,
    limit: Option<usize>,
) -> std::result::Result<Value, McpError> {
    client
        .get_json(&format!(
            "/v1/sessions/{session_id}/messages?limit={}",
            limit.unwrap_or(20)
        ))
        .await
}

fn latest_assistant_text(messages: &Value) -> Option<String> {
    messages
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().rev().find_map(|item| {
                if item.get("role").and_then(Value::as_str) == Some("assistant") {
                    item.get("content")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                } else {
                    None
                }
            })
        })
}

fn is_terminal_run(run: &Value) -> bool {
    matches!(
        run.get("state").and_then(Value::as_str),
        Some("done" | "error" | "cancelled")
    )
}

fn json_string(value: &Value, field: &str) -> std::result::Result<String, McpError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            McpError::internal(format!("server response missing string field {field:?}"))
        })
}

#[derive(Debug, Deserialize)]
struct ListSessionsArgs {
    #[serde(default)]
    live: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionIdArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct RunIdArgs {
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct GetMessagesArgs {
    session_id: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    before: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunAgentArgs {
    prompt: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    extra_workspace_roots: Option<Vec<String>>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    mcp: Option<Value>,
    #[serde(default)]
    skills: Option<Value>,
    #[serde(default)]
    custom_tools: Option<Value>,
    #[serde(default)]
    overrides: Option<Value>,
    #[serde(default)]
    attachments: Vec<RunAttachment>,
    #[serde(default)]
    wait: Option<bool>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    message_limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ContinueSessionArgs {
    session_id: String,
    prompt: String,
    #[serde(default)]
    overrides: Option<Value>,
    #[serde(default)]
    attachments: Vec<RunAttachment>,
    #[serde(default)]
    wait: Option<bool>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    message_limit: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RunAttachment {
    filename: String,
    media_type: String,
    data: String,
}

#[derive(Debug)]
struct KiliaxHttpClient {
    base_url: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl KiliaxHttpClient {
    fn new(options: McpServerOptions) -> Result<Self> {
        let base_url = options.base_url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("base_url must not be empty");
        }
        Ok(Self {
            base_url,
            token: options.token.filter(|v| !v.trim().is_empty()),
            client: reqwest::Client::new(),
        })
    }

    async fn get_json(&self, path: &str) -> std::result::Result<Value, McpError> {
        let mut req = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(token) = self.token.as_deref() {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.map_err(McpError::from_anyhow)?;
        self.decode_response(resp).await
    }

    async fn post_json(&self, path: &str, body: Value) -> std::result::Result<Value, McpError> {
        let mut req = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(&body);
        if let Some(token) = self.token.as_deref() {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.map_err(McpError::from_anyhow)?;
        self.decode_response(resp).await
    }

    async fn decode_response(
        &self,
        resp: reqwest::Response,
    ) -> std::result::Result<Value, McpError> {
        let status = resp.status();
        let text = resp.text().await.map_err(McpError::from_anyhow)?;
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(Value::as_str)
                        .or_else(|| v.get("error").and_then(Value::as_str))
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| text.clone());
            let code = if status == StatusCode::BAD_REQUEST {
                -32602
            } else {
                -32000
            };
            return Err(McpError {
                code,
                message: format!("HTTP {status}: {msg}"),
            });
        }
        serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON response: {text}"))
            .map_err(McpError::from_anyhow)
    }
}

#[derive(Debug)]
struct McpError {
    code: i64,
    message: String,
}

impl McpError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
        }
    }

    fn from_anyhow(err: impl std::fmt::Display) -> Self {
        Self::internal(err.to_string())
    }
}

fn error_response(id: Value, code: i64, message: String) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: Some(id),
        result: None,
        error: Some(JsonRpcError { code, message }),
    }
}
