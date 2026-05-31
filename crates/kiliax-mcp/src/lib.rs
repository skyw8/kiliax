mod http_transport;
mod protocol;

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use http_transport::{serve_http, HttpServerOptions};
pub use protocol::tool_definitions;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
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

        let outcome = match serde_json::from_str::<Value>(line) {
            Ok(value) => handle_message(&client, value).await,
            Err(err) => {
                IncomingOutcome::Response(error_response(Value::Null, -32700, err.to_string()))
            }
        };

        if let Some(response) = match outcome {
            IncomingOutcome::Accepted => None,
            IncomingOutcome::Response(response) => Some(response),
        } {
            let encoded = serde_json::to_vec(&response)?;
            stdout.write_all(&encoded).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) enum IncomingOutcome {
    Accepted,
    Response(JsonRpcResponse),
}

#[derive(Debug)]
pub(crate) struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    params: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    code: i64,
    message: String,
}

pub(crate) async fn handle_message(client: &KiliaxHttpClient, value: Value) -> IncomingOutcome {
    match parse_incoming(value) {
        ParsedIncoming::Request(req) => match handle_request(client, req).await {
            Some(response) => IncomingOutcome::Response(response),
            None => IncomingOutcome::Accepted,
        },
        ParsedIncoming::Accepted => IncomingOutcome::Accepted,
        ParsedIncoming::Error(response) => IncomingOutcome::Response(response),
    }
}

enum ParsedIncoming {
    Request(JsonRpcRequest),
    Accepted,
    Error(JsonRpcResponse),
}

fn parse_incoming(value: Value) -> ParsedIncoming {
    let Some(obj) = value.as_object() else {
        return ParsedIncoming::Error(error_response(
            Value::Null,
            -32600,
            "JSON-RPC message must be an object".to_string(),
        ));
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return ParsedIncoming::Error(error_response(
            obj.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "jsonrpc must be \"2.0\"".to_string(),
        ));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        if obj.contains_key("result") || obj.contains_key("error") {
            return ParsedIncoming::Accepted;
        }
        return ParsedIncoming::Error(error_response(
            obj.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "missing method".to_string(),
        ));
    };
    let id = obj.get("id").cloned();
    if id == Some(Value::Null) {
        return ParsedIncoming::Error(error_response(
            Value::Null,
            -32600,
            "MCP request id must not be null".to_string(),
        ));
    }
    ParsedIncoming::Request(JsonRpcRequest {
        id,
        method: method.to_string(),
        params: obj
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(Default::default())),
    })
}

async fn handle_request(client: &KiliaxHttpClient, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let id = req.id;
    let method = req.method.as_str();
    id.as_ref()?;

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {},
                "resources": {},
                "prompts": {}
            },
            "serverInfo": {
                "name": "kiliax",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(client, req.params).await,
        "resources/list" => Ok(json!({ "resources": protocol::resource_definitions() })),
        "resources/templates/list" => {
            Ok(json!({ "resourceTemplates": protocol::resource_template_definitions() }))
        }
        "resources/read" => read_resource(client, req.params).await,
        "prompts/list" => Ok(json!({ "prompts": protocol::prompt_definitions() })),
        "prompts/get" => get_prompt(req.params),
        "completion/complete" => complete_argument(client, req.params).await,
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
        "list_skills" => {
            let req: ListSkillsArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            match req
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                Some(session_id) => {
                    client
                        .get_json(&format!("/v1/sessions/{session_id}/skills"))
                        .await?
                }
                None => client.get_json("/v1/skills").await?,
            }
        }
        "get_config_skills" => client.get_json("/v1/config/skills").await?,
        "set_config_skills" => {
            let req: SetConfigSkillsArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            client
                .patch_json(
                    "/v1/config/skills",
                    json!({
                        "default_enable": req.default_enable,
                        "skills": req.skills
                    }),
                )
                .await?
        }
        "set_session_skills" => {
            let req: SetSessionSkillsArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            client
                .patch_json(
                    &format!("/v1/sessions/{}/settings", req.session_id),
                    json!({
                        "skills": {
                            "default_enable": req.default_enable,
                            "overrides": req.overrides
                        }
                    }),
                )
                .await?
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
        "run_skill" => {
            let req: RunSkillArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            run_skill(client, req).await?
        }
        "continue_session" => {
            let req: ContinueSessionArgs = serde_json::from_value(args)
                .map_err(|e| McpError::invalid_params(e.to_string()))?;
            continue_session(client, req).await?
        }
        _ => return Err(McpError::invalid_params(format!("unknown tool: {name}"))),
    };

    Ok(tool_result(value))
}

fn tool_result(value: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }],
        "structuredContent": value,
        "isError": false
    })
}

async fn read_resource(
    client: &KiliaxHttpClient,
    params: Value,
) -> std::result::Result<Value, McpError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid_params("resources/read params.uri is required"))?;
    let value = match uri {
        "kiliax://capabilities" => client.get_json("/v1/capabilities").await?,
        "kiliax://sessions" => client.get_json("/v1/sessions?limit=50").await?,
        "kiliax://skills" => client.get_json("/v1/skills").await?,
        "kiliax://config/skills" => client.get_json("/v1/config/skills").await?,
        "kiliax://custom-tools" => client.get_json("/v1/custom-tools").await?,
        _ => read_dynamic_resource(client, uri).await?,
    };

    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }]
    }))
}

async fn read_dynamic_resource(
    client: &KiliaxHttpClient,
    uri: &str,
) -> std::result::Result<Value, McpError> {
    if let Some(rest) = uri.strip_prefix("kiliax://sessions/") {
        if let Some(session_id) = rest.strip_suffix("/messages") {
            return client
                .get_json(&format!("/v1/sessions/{session_id}/messages?limit=50"))
                .await;
        }
        if let Some(session_id) = rest.strip_suffix("/skills") {
            return client
                .get_json(&format!("/v1/sessions/{session_id}/skills"))
                .await;
        }
        if !rest.is_empty() && !rest.contains('/') {
            return client.get_json(&format!("/v1/sessions/{rest}")).await;
        }
    }
    if let Some(run_id) = uri.strip_prefix("kiliax://runs/") {
        if !run_id.is_empty() && !run_id.contains('/') {
            return client.get_json(&format!("/v1/runs/{run_id}")).await;
        }
    }
    Err(McpError::invalid_params(format!("unknown resource: {uri}")))
}

async fn complete_argument(
    client: &KiliaxHttpClient,
    params: Value,
) -> std::result::Result<Value, McpError> {
    let argument = params
        .get("argument")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            McpError::invalid_params("completion/complete params.argument is required")
        })?;
    let name = argument
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid_params("completion argument.name is required"))?;
    let prefix = argument
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    let values = match name {
        "session_id" => {
            let sessions = client.get_json("/v1/sessions?limit=50").await?;
            collect_string_items(&sessions, &["items"], "id", prefix)
        }
        "skill_id" => {
            let skills = client.get_json("/v1/skills").await?;
            collect_string_items(&skills, &["items"], "id", prefix)
        }
        "agent" => {
            let caps = client.get_json("/v1/capabilities").await?;
            collect_string_array(&caps, &["agents"], prefix)
        }
        "model_id" => {
            let caps = client.get_json("/v1/capabilities").await?;
            collect_string_array(&caps, &["models"], prefix)
        }
        _ => Vec::new(),
    };
    let total = values.len();

    Ok(json!({
        "completion": {
            "values": values.into_iter().take(100).collect::<Vec<_>>(),
            "total": total,
            "hasMore": total > 100
        }
    }))
}

fn collect_string_array(value: &Value, path: &[&str], prefix: &str) -> Vec<String> {
    get_path(value, path)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| item.starts_with(prefix))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn collect_string_items(value: &Value, path: &[&str], field: &str, prefix: &str) -> Vec<String> {
    get_path(value, path)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field).and_then(Value::as_str))
                .filter(|item| item.starts_with(prefix))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn get_path<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for key in path {
        value = value.get(*key)?;
    }
    Some(value)
}

fn get_prompt(params: Value) -> std::result::Result<Value, McpError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid_params("prompts/get params.name is required"))?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let workspace = args
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    let (description, text) = match name {
        "run_agent" => {
            let mut text =
                String::from("Use the kiliax MCP tool `run_agent` to execute this task.");
            if !workspace.is_empty() {
                text.push_str("\nWorkspace: ");
                text.push_str(workspace);
            }
            if !task.is_empty() {
                text.push_str("\nTask: ");
                text.push_str(task);
            }
            ("Create a new Kiliax agent run.", text)
        }
        "continue_session" => {
            if session_id.is_empty() {
                return Err(McpError::invalid_params(
                    "session_id is required for continue_session prompt",
                ));
            }
            let mut text =
                format!("Use the kiliax MCP tool `continue_session` for session `{session_id}`.");
            if !task.is_empty() {
                text.push_str("\nTask: ");
                text.push_str(task);
            }
            ("Continue an existing Kiliax session.", text)
        }
        _ => return Err(McpError::invalid_params(format!("unknown prompt: {name}"))),
    };

    Ok(json!({
        "description": description,
        "messages": [{
            "role": "user",
            "content": {
                "type": "text",
                "text": text
            }
        }]
    }))
}

async fn run_skill(
    client: &KiliaxHttpClient,
    req: RunSkillArgs,
) -> std::result::Result<Value, McpError> {
    let skill_id = req.skill_id.trim();
    if skill_id.is_empty() {
        return Err(McpError::invalid_params("skill_id must not be empty"));
    }

    let mut agent_req = RunAgentArgs {
        prompt: req.prompt,
        title: req.title.or_else(|| Some(format!("Skill: {skill_id}"))),
        workspace: req.workspace,
        extra_workspace_roots: req.extra_workspace_roots,
        agent: req.agent,
        model_id: req.model_id,
        mcp: req.mcp,
        skills: Some(json!({
            "default_enable": false,
            "overrides": [{ "id": skill_id, "enable": true }]
        })),
        custom_tools: req.custom_tools,
        overrides: req.overrides,
        attachments: req.attachments,
        wait: req.wait,
        timeout_seconds: req.timeout_seconds,
        message_limit: req.message_limit,
    };

    let run_overrides = agent_req.overrides.take().unwrap_or_else(|| json!({}));
    let mut run_overrides = run_overrides
        .as_object()
        .cloned()
        .ok_or_else(|| McpError::invalid_params("overrides must be an object"))?;
    run_overrides.insert(
        "skills".to_string(),
        json!({
            "default_enable": false,
            "overrides": [{ "id": skill_id, "enable": true }]
        }),
    );
    agent_req.overrides = Some(Value::Object(run_overrides));

    run_agent(client, agent_req).await
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
struct ListSkillsArgs {
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetConfigSkillsArgs {
    #[serde(default)]
    default_enable: Option<bool>,
    #[serde(default)]
    skills: Vec<EnableSetting>,
}

#[derive(Debug, Deserialize)]
struct SetSessionSkillsArgs {
    session_id: String,
    #[serde(default)]
    default_enable: Option<bool>,
    #[serde(default)]
    overrides: Vec<EnableSetting>,
}

#[derive(Debug, Deserialize, Serialize)]
struct EnableSetting {
    id: String,
    enable: bool,
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
struct RunSkillArgs {
    skill_id: String,
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

#[derive(Debug, Clone)]
pub(crate) struct KiliaxHttpClient {
    base_url: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl KiliaxHttpClient {
    pub(crate) fn new(options: McpServerOptions) -> Result<Self> {
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

    async fn patch_json(&self, path: &str, body: Value) -> std::result::Result<Value, McpError> {
        let mut req = self
            .client
            .patch(format!("{}{}", self.base_url, path))
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
        if text.trim().is_empty() {
            return Ok(json!({ "ok": true }));
        }
        serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON response: {text}"))
            .map_err(McpError::from_anyhow)
    }
}

#[derive(Debug)]
pub(crate) struct McpError {
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

pub(crate) fn error_response(id: Value, code: i64, message: String) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: Some(id),
        result: None,
        error: Some(JsonRpcError { code, message }),
    }
}
