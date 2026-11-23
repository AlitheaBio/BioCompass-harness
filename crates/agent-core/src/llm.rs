//! Real HTTP SSE streaming clients for OpenAI/DeepSeek (`/v1/chat/completions`) and Anthropic (`/v1/messages`).
//!
//! Zero mock or canned fallback replies exist in production code: if no provider is configured,
//! `CompassError::UpstreamLlm` is returned explicitly.

use crate::AgentStreamEvent;
use biocompass_core_traits::{CompassError, CompassResult};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

/// Supported upstream LLM streaming wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmWireProtocol {
    OpenAiChatCompletions,
    AnthropicMessages,
    /// Agent Client Protocol (ACP) agent communicated over newline-delimited JSON-RPC stdio
    /// (e.g., `opencode acp`, `gemini --acp`, `claude-code-acp`, `codex-acp`).
    AcpStdio,
    /// Claude Code subscription CLI (`claude -p --output-format stream-json ...`).
    ClaudeCodeCli,
    /// OpenAI Codex subscription CLI (`codex exec --json ...`).
    CodexCli,
}

impl LlmWireProtocol {
    /// Returns `true` for backends that spawn a local CLI / ACP subprocess.
    #[must_use]
    pub const fn is_local_cli(self) -> bool {
        matches!(self, Self::AcpStdio | Self::ClaudeCodeCli | Self::CodexCli)
    }
}

const fn default_max_tokens() -> u32 {
    4096
}

/// Configuration for a real upstream LLM SSE endpoint or local subscription/ACP agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    pub protocol: LlmWireProtocol,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Local binary path or command name for `AcpStdio` / `ClaudeCodeCli` / `CodexCli`.
    #[serde(default)]
    pub command: String,
    /// CLI arguments for local backends (e.g. `["acp"]` for OpenCode or `["--acp"]` for Gemini).
    #[serde(default)]
    pub args: Vec<String>,
    /// Provider-supplied dynamic configuration overrides (e.g., `thinking`, `effort`, `mode`,
    /// `reasoning_effort`). Keys and values come dynamically from the provider's advertised
    /// controls (`session/new.configOptions`, Codex `models_cache.json`, CLI capabilities).
    #[serde(default)]
    pub config_options: BTreeMap<String, String>,
}

impl LlmProviderConfig {
    /// Returns `true` if this configuration executes via a local CLI/ACP subprocess.
    #[must_use]
    pub const fn is_local_cli(&self) -> bool {
        self.protocol.is_local_cli()
    }

    /// Human-readable label shown in health checks and UI badges.
    #[must_use]
    pub fn display_name(&self) -> String {
        let model = self.model.trim();
        match self.protocol {
            LlmWireProtocol::AcpStdio => {
                let cmd = Path::new(if self.command.trim().is_empty() {
                    "acp"
                } else {
                    self.command.trim()
                })
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("acp");
                if model.is_empty() {
                    format!("acp:{cmd}")
                } else {
                    format!("acp:{cmd} ({model})")
                }
            }
            LlmWireProtocol::CodexCli => {
                if model.is_empty() {
                    "codex-subscription".to_owned()
                } else {
                    format!("codex:{model}")
                }
            }
            LlmWireProtocol::ClaudeCodeCli => {
                if model.is_empty() {
                    "claude-subscription".to_owned()
                } else {
                    format!("claude:{model}")
                }
            }
            LlmWireProtocol::OpenAiChatCompletions | LlmWireProtocol::AnthropicMessages => {
                if model.is_empty() {
                    self.base_url.clone()
                } else {
                    model.to_owned()
                }
            }
        }
    }

    /// Resolve provider configuration from standard environment variables.
    /// Returns `None` if no LLM provider credentials or endpoint are configured.
    pub fn from_env() -> Option<Self> {
        let proto_env = std::env::var("BIOCOMPASS_LLM_PROTOCOL")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let model_env = std::env::var("BIOCOMPASS_LLM_MODEL").unwrap_or_default();
        let command_env = std::env::var("BIOCOMPASS_LLM_COMMAND").unwrap_or_default();
        let args_env: Vec<String> = std::env::var("BIOCOMPASS_LLM_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect();

        match proto_env.as_str() {
            "acp" | "acp_stdio" => {
                return Some(Self {
                    protocol: LlmWireProtocol::AcpStdio,
                    base_url: String::new(),
                    api_key: String::new(),
                    model: model_env.trim().to_owned(),
                    max_tokens: default_max_tokens(),
                    command: if command_env.trim().is_empty() {
                        "opencode".into()
                    } else {
                        command_env.trim().to_owned()
                    },
                    args: if args_env.is_empty() {
                        vec!["acp".into()]
                    } else {
                        args_env
                    },
                    config_options: BTreeMap::new(),
                });
            }
            "codex" | "codex_cli" => {
                return Some(Self {
                    protocol: LlmWireProtocol::CodexCli,
                    base_url: String::new(),
                    api_key: String::new(),
                    model: model_env.trim().to_owned(),
                    max_tokens: default_max_tokens(),
                    command: if command_env.trim().is_empty() {
                        "codex".into()
                    } else {
                        command_env.trim().to_owned()
                    },
                    args: args_env,
                    config_options: BTreeMap::new(),
                });
            }
            "claude_code" | "claude_code_cli" | "claude-code" => {
                return Some(Self {
                    protocol: LlmWireProtocol::ClaudeCodeCli,
                    base_url: String::new(),
                    api_key: String::new(),
                    model: model_env.trim().to_owned(),
                    max_tokens: default_max_tokens(),
                    command: if command_env.trim().is_empty() {
                        "claude".into()
                    } else {
                        command_env.trim().to_owned()
                    },
                    args: args_env,
                    config_options: BTreeMap::new(),
                });
            }
            _ => {}
        }

        if let Ok(base_url) = std::env::var("BIOCOMPASS_LLM_BASE_URL")
            && !base_url.trim().is_empty()
        {
            let protocol = match proto_env.as_str() {
                "anthropic" | "anthropic_messages" => LlmWireProtocol::AnthropicMessages,
                _ => LlmWireProtocol::OpenAiChatCompletions,
            };
            let api_key = std::env::var("BIOCOMPASS_LLM_API_KEY").unwrap_or_default();
            let model = if model_env.trim().is_empty() {
                "deepseek-chat".into()
            } else {
                model_env.trim().to_owned()
            };
            return Some(Self {
                protocol,
                base_url: base_url.trim_end_matches('/').to_owned(),
                api_key,
                model,
                max_tokens: default_max_tokens(),
                command: String::new(),
                args: Vec::new(),
                config_options: BTreeMap::new(),
            });
        }

        if let Ok(key) = std::env::var("DEEPSEEK_API_KEY")
            && !key.trim().is_empty()
        {
            return Some(Self {
                protocol: LlmWireProtocol::OpenAiChatCompletions,
                base_url: "https://api.deepseek.com".into(),
                api_key: key.trim().to_owned(),
                model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-chat".into()),
                max_tokens: default_max_tokens(),
                command: String::new(),
                args: Vec::new(),
                config_options: BTreeMap::new(),
            });
        }

        if let Ok(key) = std::env::var("OPENAI_API_KEY")
            && !key.trim().is_empty()
        {
            return Some(Self {
                protocol: LlmWireProtocol::OpenAiChatCompletions,
                base_url: "https://api.openai.com".into(),
                api_key: key.trim().to_owned(),
                model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into()),
                max_tokens: default_max_tokens(),
                command: String::new(),
                args: Vec::new(),
                config_options: BTreeMap::new(),
            });
        }

        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
            && !key.trim().is_empty()
        {
            return Some(Self {
                protocol: LlmWireProtocol::AnthropicMessages,
                base_url: "https://api.anthropic.com".into(),
                api_key: key.trim().to_owned(),
                model: std::env::var("ANTHROPIC_MODEL")
                    .unwrap_or_else(|_| "claude-sonnet-4-20250514".into()),
                max_tokens: default_max_tokens(),
                command: String::new(),
                args: Vec::new(),
                config_options: BTreeMap::new(),
            });
        }

        None
    }
}

/// Normalized tool call emitted by an upstream model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Normalized message item in the model conversation window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationMessage {
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<LlmToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: String,
    },
}

/// Completed output from one streaming LLM round-trip.
#[derive(Debug, Clone, Default)]
pub struct AssistantTurnOutput {
    pub thinking: String,
    pub text: String,
    pub tool_calls: Vec<LlmToolCall>,
}

/// Tool schema descriptor shared across OpenAI and Anthropic wire formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Stream a single model turn over HTTP SSE, forwarding incremental text chunks to `stream_tx`.
pub async fn stream_model_turn(
    http: &Client,
    cfg: &LlmProviderConfig,
    system_prompt: &str,
    history: &[ConversationMessage],
    tools: &[ToolDefinition],
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    match cfg.protocol {
        LlmWireProtocol::OpenAiChatCompletions => {
            stream_openai_sse(http, cfg, system_prompt, history, tools, stream_tx).await
        }
        LlmWireProtocol::AnthropicMessages => {
            stream_anthropic_sse(http, cfg, system_prompt, history, tools, stream_tx).await
        }
        LlmWireProtocol::AcpStdio | LlmWireProtocol::ClaudeCodeCli | LlmWireProtocol::CodexCli => {
            Err(CompassError::UpstreamLlm(
                "local CLI/ACP backends run through AgentRuntime scoped MCP turn execution"
                    .into(),
            ))
        }
    }
}

pub fn build_http_client() -> CompassResult<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| CompassError::UpstreamLlm(format!("build LLM HTTP client: {e}")))
}

/// Extract a concise, human-readable message from an upstream LLM HTTP error response.
pub fn format_upstream_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> String {
    let reason = status.canonical_reason().unwrap_or("Error");
    let trimmed = body.trim();
    let detail = if trimmed.is_empty() {
        None
    } else if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        val.pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| val.get("message").and_then(Value::as_str))
            .or_else(|| val.get("detail").and_then(Value::as_str))
            .or_else(|| val.get("error").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    } else if trimmed.starts_with('<')
        && (trimmed.to_ascii_lowercase().contains("<html")
            || trimmed.to_ascii_lowercase().contains("<!doctype"))
    {
        Some("upstream gateway returned an HTML error page".to_owned())
    } else {
        let single_line: String = trimmed
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let short: String = single_line.chars().take(240).collect();
        if short.is_empty() { None } else { Some(short) }
    };

    match detail {
        Some(msg) => format!(
            "{provider} returned HTTP {} ({reason}): {msg}",
            status.as_u16()
        ),
        None => format!("{provider} returned HTTP {} ({reason})", status.as_u16()),
    }
}

/// Summarize a network transport error reaching an upstream LLM endpoint cleanly.
pub fn format_upstream_transport_error(provider: &str, err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!("Request to {provider} timed out before the server responded.")
    } else if err.is_connect() {
        format!("Could not connect to {provider}: {err}")
    } else {
        format!("Network error communicating with {provider}: {err}")
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    args_buf: String,
}

async fn stream_openai_sse(
    http: &Client,
    cfg: &LlmProviderConfig,
    system_prompt: &str,
    history: &[ConversationMessage],
    tools: &[ToolDefinition],
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(json!({
        "role": "system",
        "content": system_prompt,
    }));

    for msg in history {
        match msg {
            ConversationMessage::User(text) => {
                messages.push(json!({ "role": "user", "content": text }));
            }
            ConversationMessage::Assistant { text, tool_calls } => {
                if tool_calls.is_empty() {
                    messages.push(json!({ "role": "assistant", "content": text }));
                } else {
                    let tc_json: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                                }
                            })
                        })
                        .collect();
                    messages.push(json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { json!(text) },
                        "tool_calls": tc_json,
                    }));
                }
            }
            ConversationMessage::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }));
            }
        }
    }

    let openai_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

    let url = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut body_obj = serde_json::Map::from_iter([
        ("model".into(), json!(cfg.model)),
        ("messages".into(), json!(messages)),
        ("tools".into(), json!(openai_tools)),
        ("max_tokens".into(), json!(cfg.max_tokens)),
        ("stream".into(), json!(true)),
    ]);
    if let Some(effort) = cfg
        .config_options
        .get("reasoning_effort")
        .or_else(|| cfg.config_options.get("effort"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        body_obj.insert("reasoning_effort".into(), json!(effort));
    }
    let mut req = http
        .post(&url)
        .header("Accept", "text/event-stream")
        .json(&Value::Object(body_obj));
    if !cfg.api_key.is_empty() {
        req = req.bearer_auth(&cfg.api_key);
    }

    let resp = req.send().await.map_err(|e| {
        CompassError::UpstreamLlm(format_upstream_transport_error("OpenAI-compatible LLM", &e))
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CompassError::UpstreamLlm(format_upstream_http_error(
            "OpenAI-compatible LLM",
            status,
            &body,
        )));
    }

    let mut byte_stream = resp.bytes_stream();
    let mut line_buf = String::new();
    let mut full_thinking = String::new();
    let mut full_text = String::new();
    let mut partials: BTreeMap<usize, PartialToolCall> = BTreeMap::new();

    while let Some(chunk) = byte_stream.next().await {
        let bytes =
            chunk.map_err(|e| CompassError::UpstreamLlm(format!("SSE stream read error: {e}")))?;
        line_buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(newline_idx) = line_buf.find('\n') {
            let line = line_buf[..newline_idx].trim_end_matches('\r').to_owned();
            line_buf.drain(..=newline_idx);

            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let trimmed = data.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }

            let frame: Value = serde_json::from_str(trimmed).map_err(|e| {
                CompassError::UpstreamLlm(format!("invalid SSE JSON frame '{trimmed}': {e}"))
            })?;
            let Some(delta) = frame
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("delta"))
            else {
                continue;
            };

            if let Some(thinking_piece) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
                && !thinking_piece.is_empty()
            {
                full_thinking.push_str(thinking_piece);
                if let Some(tx) = stream_tx {
                    let _ = tx
                        .send(AgentStreamEvent::ThoughtDelta(thinking_piece.to_owned()))
                        .await;
                }
            }

            if let Some(content_piece) = delta.get("content").and_then(Value::as_str)
                && !content_piece.is_empty()
            {
                full_text.push_str(content_piece);
                if let Some(tx) = stream_tx {
                    let _ = tx
                        .send(AgentStreamEvent::TextDelta(content_piece.to_owned()))
                        .await;
                }
            }

            if let Some(tc_arr) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tc_arr {
                    let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let entry = partials.entry(idx).or_default();
                    if let Some(id) = tc.get("id").and_then(Value::as_str)
                        && !id.is_empty()
                    {
                        entry.id = id.to_owned();
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(Value::as_str) {
                            entry.name.push_str(name);
                        }
                        if let Some(arg_delta) = func.get("arguments").and_then(Value::as_str) {
                            entry.args_buf.push_str(arg_delta);
                        }
                    }
                }
            }
        }
    }

    let mut tool_calls = Vec::with_capacity(partials.len());
    for (idx, p) in partials {
        let parsed_args = if p.args_buf.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&p.args_buf).map_err(|e| {
                CompassError::UpstreamLlm(format!(
                    "tool call '{}' emitted malformed JSON arguments '{}': {e}",
                    p.name, p.args_buf
                ))
            })?
        };
        tool_calls.push(LlmToolCall {
            id: if p.id.is_empty() {
                format!("call_{idx}")
            } else {
                p.id
            },
            name: p.name,
            arguments: parsed_args,
        });
    }

    Ok(AssistantTurnOutput {
        thinking: full_thinking,
        text: full_text,
        tool_calls,
    })
}

async fn stream_anthropic_sse(
    http: &Client,
    cfg: &LlmProviderConfig,
    system_prompt: &str,
    history: &[ConversationMessage],
    tools: &[ToolDefinition],
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let mut messages = Vec::new();
    for msg in history {
        match msg {
            ConversationMessage::User(text) => {
                messages.push(json!({ "role": "user", "content": text }));
            }
            ConversationMessage::Assistant { text, tool_calls } => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for tc in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
            ConversationMessage::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content,
                    }]
                }));
            }
        }
    }

    let anthropic_tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();

    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .header("Accept", "text/event-stream")
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": cfg.model,
            "max_tokens": cfg.max_tokens,
            "system": system_prompt,
            "messages": messages,
            "tools": anthropic_tools,
            "stream": true,
        }))
        .send()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format_upstream_transport_error("Anthropic API", &e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CompassError::UpstreamLlm(format_upstream_http_error(
            "Anthropic API",
            status,
            &body,
        )));
    }

    let mut byte_stream = resp.bytes_stream();
    let mut line_buf = String::new();
    let mut full_thinking = String::new();
    let mut full_text = String::new();
    let mut partials: BTreeMap<usize, PartialToolCall> = BTreeMap::new();

    while let Some(chunk) = byte_stream.next().await {
        let bytes = chunk
            .map_err(|e| CompassError::UpstreamLlm(format!("Anthropic SSE read error: {e}")))?;
        line_buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(newline_idx) = line_buf.find('\n') {
            let line = line_buf[..newline_idx].trim_end_matches('\r').to_owned();
            line_buf.drain(..=newline_idx);

            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let trimmed = data.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(frame) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            match frame.get("type").and_then(Value::as_str).unwrap_or("") {
                "content_block_start" => {
                    let idx = frame.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    if let Some(cb) = frame.get("content_block")
                        && cb.get("type").and_then(Value::as_str) == Some("tool_use")
                    {
                        let entry = partials.entry(idx).or_default();
                        entry.id = cb
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        entry.name = cb
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                    }
                }
                "content_block_delta" => {
                    let idx = frame.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    if let Some(delta) = frame.get("delta") {
                        match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                            "thinking_delta" => {
                                if let Some(txt) = delta.get("thinking").and_then(Value::as_str)
                                    && !txt.is_empty()
                                {
                                    full_thinking.push_str(txt);
                                    if let Some(tx) = stream_tx {
                                        let _ = tx
                                            .send(AgentStreamEvent::ThoughtDelta(txt.to_owned()))
                                            .await;
                                    }
                                }
                            }
                            "text_delta" => {
                                if let Some(txt) = delta.get("text").and_then(Value::as_str)
                                    && !txt.is_empty()
                                {
                                    full_text.push_str(txt);
                                    if let Some(tx) = stream_tx {
                                        let _ = tx
                                            .send(AgentStreamEvent::TextDelta(txt.to_owned()))
                                            .await;
                                    }
                                }
                            }
                            "input_json_delta" => {
                                if let Some(fragment) =
                                    delta.get("partial_json").and_then(Value::as_str)
                                {
                                    partials.entry(idx).or_default().args_buf.push_str(fragment);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut tool_calls = Vec::with_capacity(partials.len());
    for (idx, p) in partials {
        let parsed_args = if p.args_buf.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&p.args_buf).map_err(|e| {
                CompassError::UpstreamLlm(format!(
                    "Anthropic tool '{}' emitted invalid JSON '{}': {e}",
                    p.name, p.args_buf
                ))
            })?
        };
        tool_calls.push(LlmToolCall {
            id: if p.id.is_empty() {
                format!("toolu_{idx}")
            } else {
                p.id
            },
            name: p.name,
            arguments: parsed_args,
        });
    }

    Ok(AssistantTurnOutput {
        thinking: full_thinking,
        text: full_text,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::format_upstream_http_error;
    use reqwest::StatusCode;

    #[test]
    fn format_upstream_http_error_extracts_json_messages_and_strips_html() {
        let openai_err = format_upstream_http_error(
            "OpenAI",
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
        );
        assert_eq!(
            openai_err,
            "OpenAI returned HTTP 401 (Unauthorized): Incorrect API key provided"
        );

        let anthropic_err = format_upstream_http_error(
            "Anthropic",
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limit exceeded"}}"#,
        );
        assert_eq!(
            anthropic_err,
            "Anthropic returned HTTP 429 (Too Many Requests): Rate limit exceeded"
        );

        let html_err = format_upstream_http_error(
            "Gateway",
            StatusCode::BAD_GATEWAY,
            "<!DOCTYPE html><html><body>502 Bad Gateway</body></html>",
        );
        assert!(!html_err.contains("<html>"));
        assert_eq!(
            html_err,
            "Gateway returned HTTP 502 (Bad Gateway): upstream gateway returned an HTML error page"
        );
    }
}
