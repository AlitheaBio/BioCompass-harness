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
use std::time::Duration;
use tokio::sync::mpsc;

/// Supported upstream LLM streaming wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmWireProtocol {
    OpenAiChatCompletions,
    AnthropicMessages,
}

/// Configuration for a real upstream LLM SSE endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    pub protocol: LlmWireProtocol,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl LlmProviderConfig {
    /// Resolve provider configuration from standard environment variables.
    /// Returns `None` if no LLM provider credentials or endpoint are configured.
    pub fn from_env() -> Option<Self> {
        if let Ok(base_url) = std::env::var("BIOCOMPASS_LLM_BASE_URL")
            && !base_url.trim().is_empty()
        {
            let protocol = match std::env::var("BIOCOMPASS_LLM_PROTOCOL")
                .unwrap_or_else(|_| "openai".into())
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "anthropic" => LlmWireProtocol::AnthropicMessages,
                _ => LlmWireProtocol::OpenAiChatCompletions,
            };
            let api_key = std::env::var("BIOCOMPASS_LLM_API_KEY").unwrap_or_default();
            let model = std::env::var("BIOCOMPASS_LLM_MODEL")
                .unwrap_or_else(|_| "deepseek-chat".into());
            return Some(Self {
                protocol,
                base_url: base_url.trim_end_matches('/').to_owned(),
                api_key,
                model,
                max_tokens: 4096,
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
                max_tokens: 4096,
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
                max_tokens: 4096,
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
                max_tokens: 4096,
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
#[derive(Debug, Clone)]
pub struct AssistantTurnOutput {
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
    }
}

pub fn build_http_client() -> CompassResult<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| CompassError::UpstreamLlm(format!("build LLM HTTP client: {e}")))
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
    let mut req = http
        .post(&url)
        .header("Accept", "text/event-stream")
        .json(&json!({
            "model": cfg.model,
            "messages": messages,
            "tools": openai_tools,
            "max_tokens": cfg.max_tokens,
            "stream": true,
        }));
    if !cfg.api_key.is_empty() {
        req = req.bearer_auth(&cfg.api_key);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("POST {url} failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CompassError::UpstreamLlm(format!(
            "upstream LLM {url} returned HTTP {status}: {body}"
        )));
    }

    let mut byte_stream = resp.bytes_stream();
    let mut line_buf = String::new();
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
        .map_err(|e| CompassError::UpstreamLlm(format!("POST {url} failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(CompassError::UpstreamLlm(format!(
            "Anthropic API {url} returned HTTP {status}: {body}"
        )));
    }

    let mut byte_stream = resp.bytes_stream();
    let mut line_buf = String::new();
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
        text: full_text,
        tool_calls,
    })
}
