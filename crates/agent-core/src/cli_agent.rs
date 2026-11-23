//! Subprocess drivers for ACP (`opencode acp`, `gemini --acp`, custom ACP binaries),
//! OpenAI Codex CLI (`codex exec --json`), and Claude Code CLI (`claude -p --output-format stream-json`).
//!
//! All three drivers talk to BioCompass exclusively through the ephemeral loopback
//! `ScopedMcpServer` HTTP URL passed into the child process for the duration of one turn.

use crate::AgentStreamEvent;
use crate::llm::{AssistantTurnOutput, ConversationMessage, LlmProviderConfig, LlmWireProtocol};
use crate::tools::allowed_tool_definitions;
use biocompass_core_traits::{CompassError, CompassResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

const CLI_TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if ch != '\r' {
            out.push(ch);
        }
    }
    out.trim().to_owned()
}

fn spawn_cli_error(label: &str, command: &str, err: &std::io::Error) -> CompassError {
    if err.kind() == std::io::ErrorKind::NotFound {
        CompassError::UpstreamLlm(format!(
            "'{command}' ({label}) was not found on PATH; install it or choose another provider"
        ))
    } else if err.kind() == std::io::ErrorKind::PermissionDenied {
        CompassError::UpstreamLlm(format!(
            "Permission denied launching '{command}' ({label}); check file execute permissions"
        ))
    } else {
        CompassError::UpstreamLlm(format!("Unable to launch {label} ('{command}'): {err}"))
    }
}

fn attach_stderr_tail(err: CompassError, stderr: &str) -> CompassError {
    let cleaned = strip_ansi_codes(stderr);
    if cleaned.is_empty() {
        return err;
    }
    let clipped: String = cleaned.chars().take(300).collect();
    let base = err.user_message();
    if base.contains(&clipped) {
        CompassError::UpstreamLlm(base)
    } else {
        CompassError::UpstreamLlm(format!("{base} — {clipped}"))
    }
}

/// One selectable value returned dynamically by a provider control (model, thinking, effort, mode).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderOptionChoice {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Dynamic runtime control advertised by an active LLM backend (ACP `configOptions`, Codex cache, CLI capabilities).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderControlSpec {
    pub id: String,
    pub label: String,
    pub category: String,
    pub current_value: String,
    #[serde(default)]
    pub options: Vec<ProviderOptionChoice>,
}

/// Result returned by probing an ACP subprocess (`initialize` + `session/new`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpProbeResult {
    pub agent_name: String,
    pub available_models: Vec<String>,
    pub controls: Vec<ProviderControlSpec>,
}

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Format system prompt + session history for single-turn CLI / ACP prompt delivery.
#[must_use]
pub fn format_cli_prompt(system_prompt: &str, history: &[ConversationMessage]) -> String {
    let mut out = String::new();
    out.push_str("<biocompass_system_instructions>\n");
    out.push_str(system_prompt.trim());
    out.push_str("\nDo NOT use shell, bash, or local filesystem write tools for dataset/module operations. ");
    out.push_str("Use ONLY the registered `biocompass` MCP tools (`list_catalogs`, `query_catalog`, `update_module_draft`, `prepare_plugin_run`, `list_runs`, `get_run_result`, `get_run_artifact`, `scaffold_plugin`) directly without preliminary tool-discovery narration.\n");
    out.push_str("</biocompass_system_instructions>\n\n");

    let prior_count = history.len().saturating_sub(1);
    if prior_count > 0 {
        out.push_str("<conversation_history>\n");
        for msg in &history[..prior_count] {
            match msg {
                ConversationMessage::User(text) => {
                    out.push_str("User: ");
                    out.push_str(text);
                    out.push('\n');
                }
                ConversationMessage::Assistant { text, tool_calls } => {
                    if !text.is_empty() {
                        out.push_str("Assistant: ");
                        out.push_str(text);
                        out.push('\n');
                    }
                    for tc in tool_calls {
                        out.push_str(&format!(
                            "Assistant tool call `{}` ({})\n",
                            tc.name, tc.arguments
                        ));
                    }
                }
                ConversationMessage::ToolResult {
                    tool_name, content, ..
                } => {
                    let clipped: String = content.chars().take(1200).collect();
                    out.push_str(&format!("Tool result `{tool_name}`: {clipped}\n"));
                }
            }
        }
        out.push_str("</conversation_history>\n\n");
    }

    if let Some(ConversationMessage::User(latest)) = history.last() {
        out.push_str(latest);
    } else if let Some(last) = history.last() {
        out.push_str(&format!("{last:?}"));
    }

    out
}

/// Execute one turn via the configured local ACP or subscription CLI backend.
pub async fn run_cli_agent_turn(
    cfg: &LlmProviderConfig,
    mcp_url: &str,
    cwd: &Path,
    system_prompt: &str,
    history: &[ConversationMessage],
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let prompt = format_cli_prompt(system_prompt, history);
    tokio::time::timeout(CLI_TURN_TIMEOUT, async {
        match cfg.protocol {
            LlmWireProtocol::AcpStdio => {
                run_acp_stdio_turn(cfg, mcp_url, cwd, &prompt, stream_tx).await
            }
            LlmWireProtocol::CodexCli => {
                run_codex_cli_turn(cfg, mcp_url, cwd, &prompt, stream_tx).await
            }
            LlmWireProtocol::ClaudeCodeCli => {
                run_claude_cli_turn(cfg, mcp_url, cwd, &prompt, stream_tx).await
            }
            LlmWireProtocol::OpenAiChatCompletions | LlmWireProtocol::AnthropicMessages => {
                Err(CompassError::UpstreamLlm(
                    "HTTP protocol passed to local CLI driver".into(),
                ))
            }
        }
    })
    .await
    .map_err(|_| {
        CompassError::UpstreamLlm(format!(
            "{} timed out after {}s",
            cfg.display_name(),
            CLI_TURN_TIMEOUT.as_secs()
        ))
    })?
}

fn spawn_stderr_collector<R>(stderr: Option<R>) -> Arc<Mutex<String>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let buf = Arc::new(Mutex::new(String::new()));
    if let Some(mut reader) = stderr {
        let buf_clone = Arc::clone(&buf);
        tokio::spawn(async move {
            let mut chunk = [0u8; 2048];
            while let Ok(n) = reader.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&chunk[..n]);
                let mut guard = buf_clone.lock().await;
                if guard.len() < 8192 {
                    guard.push_str(&text);
                }
            }
        });
    }
    buf
}

fn is_biocompass_mcp_permission(params: &Value, tool_titles: &HashMap<String, String>) -> bool {
    let allowed_names: Vec<String> = allowed_tool_definitions()
        .into_iter()
        .map(|t| t.name)
        .collect();

    let tool_call = params.get("toolCall").unwrap_or(params);
    let mut candidates = Vec::new();
    for key in ["title", "name", "toolName", "kind"] {
        if let Some(s) = tool_call.get(key).and_then(Value::as_str) {
            candidates.push(s.to_ascii_lowercase());
        }
    }
    if let Some(call_id) = tool_call.get("toolCallId").and_then(Value::as_str)
        && let Some(cached) = tool_titles.get(call_id)
    {
        candidates.push(cached.to_ascii_lowercase());
    }
    if let Some(raw_input) = tool_call.get("rawInput")
        && let Ok(s) = serde_json::to_string(raw_input)
    {
        candidates.push(s.to_ascii_lowercase());
    }

    let joined = candidates.join(" ");
    if joined.contains("confirm_run") {
        return false;
    }
    if joined.contains("biocompass") {
        return true;
    }
    allowed_names
        .iter()
        .any(|name| joined.contains(&name.to_ascii_lowercase()))
}

fn choose_acp_permission_outcome(params: &Value, allow: bool) -> Value {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if allow {
        for preferred in ["allow_once", "allow_always", "allow"] {
            if let Some(opt) = options.iter().find(|o| {
                o.get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|k| k.contains(preferred))
                    || o.get("optionId")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.contains(preferred))
            }) && let Some(id) = opt.get("optionId").and_then(Value::as_str)
            {
                return json!({
                    "outcome": { "outcome": "selected", "optionId": id }
                });
            }
        }
        if let Some(first_id) = options
            .first()
            .and_then(|o| o.get("optionId"))
            .and_then(Value::as_str)
        {
            return json!({
                "outcome": { "outcome": "selected", "optionId": first_id }
            });
        }
    } else {
        for preferred in ["reject_once", "reject_always", "reject", "deny"] {
            if let Some(opt) = options.iter().find(|o| {
                o.get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|k| k.contains(preferred))
                    || o.get("optionId")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.contains(preferred))
            }) && let Some(id) = opt.get("optionId").and_then(Value::as_str)
            {
                return json!({
                    "outcome": { "outcome": "selected", "optionId": id }
                });
            }
        }
    }

    json!({ "outcome": { "outcome": "cancelled" } })
}

async fn send_json_line(stdin: &mut tokio::process::ChildStdin, val: &Value) -> CompassResult<()> {
    let mut line = serde_json::to_string(val)
        .map_err(|e| CompassError::UpstreamLlm(format!("encode ACP JSON-RPC: {e}")))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("write ACP stdin: {e}")))?;
    stdin
        .flush()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("flush ACP stdin: {e}")))?;
    Ok(())
}

fn extract_acp_update_text(update: &Value) -> String {
    let mut chunk = String::new();
    if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
        chunk.push_str(text);
    } else if let Some(arr) = update.get("content").and_then(Value::as_array) {
        for part in arr {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                chunk.push_str(text);
            }
        }
    }
    chunk
}

/// Filter out unsolicited third-party MCP connector startup/auth boilerplate emitted by user CLI
/// profiles (e.g. external Drive/workspace MCP servers in `~/.vibe/config.toml`), while always
/// preserving any diagnostic that mentions BioCompass itself.
fn strip_external_mcp_setup_notice(chunk: &str) -> String {
    let lower = chunk.to_ascii_lowercase();
    if lower.contains("biocompass")
        || (!chunk.contains("The following MCP servers failed to connect:")
            && !chunk.contains("MCP OAuth login is required for:"))
    {
        return chunk.to_owned();
    }

    let mut kept_lines = Vec::new();
    let mut skipping_failed_list = false;
    for line in chunk.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("The following MCP servers failed to connect:") {
            skipping_failed_list = true;
            continue;
        }
        if skipping_failed_list {
            if trimmed.starts_with("- ") || trimmed.is_empty() {
                continue;
            }
            skipping_failed_list = false;
        }
        if trimmed.starts_with("MCP OAuth login is required for:") {
            continue;
        }
        kept_lines.push(line);
    }
    kept_lines.join("\n").trim_start_matches('\n').to_owned()
}

async fn wait_acp_response(
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    stdin: &mut tokio::process::ChildStdin,
    expected_id: u64,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
    accumulated_thought: &mut String,
    accumulated_text: &mut String,
    collect_chunks: bool,
) -> CompassResult<Value> {
    let mut tool_titles: HashMap<String, String> = HashMap::new();
    let mut saw_tool_since_text = false;
    let mut saw_tool_since_thought = false;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("read ACP stdout: {e}")))?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        // Handle matching JSON-RPC response.
        if msg.get("id").and_then(Value::as_u64) == Some(expected_id)
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            if let Some(err) = msg.get("error") {
                let err_msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("ACP request error");
                let err_data = err
                    .get("data")
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(CompassError::UpstreamLlm(format!("{err_msg}{err_data}")));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }

        // Handle incoming server-to-client request (permission / fs / terminal).
        if let (Some(req_id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
            let params = msg.get("params").unwrap_or(&Value::Null);
            let reply = if method == "session/request_permission" {
                let allow = is_biocompass_mcp_permission(params, &tool_titles);
                json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": choose_acp_permission_outcome(params, allow),
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32601,
                        "message": format!("BioCompass ACP client does not expose `{method}`"),
                    }
                })
            };
            send_json_line(stdin, &reply).await?;
            continue;
        }

        // Handle streaming `session/update` notifications.
        if msg.get("method").and_then(Value::as_str) == Some("session/update")
            && let Some(update) = msg.pointer("/params/update")
        {
            let kind = update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("");
            if (kind == "tool_call" || kind == "tool_call_update")
                && let Some(call_id) = update.get("toolCallId").and_then(Value::as_str)
            {
                let mut desc = Vec::new();
                for field in ["title", "name", "toolName", "kind"] {
                    if let Some(v) = update.get(field).and_then(Value::as_str) {
                        desc.push(v);
                    }
                }
                if let Some(meta_name) = update.pointer("/_meta/tool_name").and_then(Value::as_str)
                {
                    desc.push(meta_name);
                }
                if !desc.is_empty() {
                    tool_titles.insert(call_id.to_owned(), desc.join(" "));
                }
                if collect_chunks {
                    if !accumulated_text.is_empty() {
                        saw_tool_since_text = true;
                    }
                    if !accumulated_thought.is_empty() {
                        saw_tool_since_thought = true;
                    }
                }
            } else if kind == "agent_thought_chunk" && collect_chunks {
                let chunk = extract_acp_update_text(update);
                if !chunk.is_empty() {
                    if saw_tool_since_thought
                        && !accumulated_thought.ends_with('\n')
                        && !chunk.starts_with('\n')
                    {
                        accumulated_thought.push_str("\n\n");
                        if let Some(tx) = stream_tx {
                            let _ = tx.send(AgentStreamEvent::ThoughtDelta("\n\n".into())).await;
                        }
                    }
                    saw_tool_since_thought = false;
                    accumulated_thought.push_str(&chunk);
                    if let Some(tx) = stream_tx {
                        let _ = tx.send(AgentStreamEvent::ThoughtDelta(chunk)).await;
                    }
                }
            } else if kind == "agent_message_chunk" && collect_chunks {
                let raw_chunk = extract_acp_update_text(update);
                let chunk = strip_external_mcp_setup_notice(&raw_chunk);
                if !chunk.is_empty() {
                    if saw_tool_since_text
                        && !accumulated_text.is_empty()
                        && !accumulated_text.ends_with('\n')
                        && !chunk.starts_with('\n')
                    {
                        accumulated_text.push_str("\n\n");
                        if let Some(tx) = stream_tx {
                            let _ = tx.send(AgentStreamEvent::TextDelta("\n\n".into())).await;
                        }
                    }
                    saw_tool_since_text = false;
                    accumulated_text.push_str(&chunk);
                    if let Some(tx) = stream_tx {
                        let _ = tx.send(AgentStreamEvent::TextDelta(chunk)).await;
                    }
                }
            }
        }
    }

    Err(CompassError::UpstreamLlm(
        "ACP process closed stdout before completing response".into(),
    ))
}

async fn run_acp_stdio_turn(
    cfg: &LlmProviderConfig,
    mcp_url: &str,
    cwd: &Path,
    prompt: &str,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let command = if cfg.command.trim().is_empty() {
        "opencode"
    } else {
        cfg.command.trim()
    };
    let mut cmd = Command::new(command);
    if !cfg.args.is_empty() {
        cmd.args(&cfg.args);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_cli_error("ACP agent", command, &e))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("ACP child missing stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("ACP child missing stdout".into()))?;
    let stderr_buf = spawn_stderr_collector(child.stderr.take());
    let mut lines = BufReader::new(stdout).lines();
    let mut discard_thought = String::new();
    let mut discard_text = String::new();
    let mut streamed_thought = String::new();
    let mut streamed_text = String::new();
    let mut next_id = 1u64;

    let turn_res: CompassResult<AssistantTurnOutput> = async {
        // 1. initialize
        let init_id = next_id;
        next_id += 1;
        send_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {
                        "name": "biocompass",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }
            }),
        )
        .await?;
        let _init = wait_acp_response(
            &mut lines,
            &mut stdin,
            init_id,
            None,
            &mut discard_thought,
            &mut discard_text,
            false,
        )
        .await?;

        // 2. session/new with ephemeral loopback MCP server
        let session_req_id = next_id;
        next_id += 1;
        send_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": session_req_id,
                "method": "session/new",
                "params": {
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": [
                        {
                            "type": "http",
                            "name": "biocompass",
                            "url": mcp_url,
                            "headers": [],
                        }
                    ]
                }
            }),
        )
        .await?;
        let session_res = wait_acp_response(
            &mut lines,
            &mut stdin,
            session_req_id,
            None,
            &mut discard_thought,
            &mut discard_text,
            false,
        )
        .await?;

        let session_id = session_res
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CompassError::UpstreamLlm(format!(
                    "ACP session/new returned no sessionId: {session_res}"
                ))
            })?
            .to_owned();

        // 3. Apply model & dynamic configOptions via session/set_config_option when exposed by agent.
        let advertised_options: Vec<&Value> = session_res
            .get("configOptions")
            .and_then(Value::as_array)
            .map(|opts| opts.iter().collect())
            .unwrap_or_default();

        let mut pending_updates: Vec<(&str, &str)> = Vec::new();
        let desired_model = cfg.model.trim();
        if !desired_model.is_empty()
            && advertised_options
                .iter()
                .any(|o| o.get("id").and_then(Value::as_str) == Some("model"))
        {
            pending_updates.push(("model", desired_model));
        }
        for (k, v) in &cfg.config_options {
            let key = k.trim();
            let val = v.trim();
            if key.is_empty() || key == "model" || val.is_empty() {
                continue;
            }
            if let Some(opt_spec) = advertised_options
                .iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some(key))
            {
                let choices = extract_acp_option_choices(opt_spec);
                if choices.is_empty() || choices.iter().any(|c| c.value == val) {
                    pending_updates.push((key, val));
                }
            }
        }

        for (config_id, value) in pending_updates {
            let set_id = next_id;
            next_id += 1;
            send_json_line(
                &mut stdin,
                &json!({
                    "jsonrpc": "2.0",
                    "id": set_id,
                    "method": "session/set_config_option",
                    "params": {
                        "sessionId": session_id,
                        "configId": config_id,
                        "value": value,
                    }
                }),
            )
            .await?;
            let _ = wait_acp_response(
                &mut lines,
                &mut stdin,
                set_id,
                None,
                &mut discard_thought,
                &mut discard_text,
                false,
            )
            .await?;
        }

        // 4. session/prompt
        let prompt_id = next_id;
        send_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": prompt_id,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id,
                    "prompt": [
                        { "type": "text", "text": prompt }
                    ]
                }
            }),
        )
        .await?;
        let prompt_res = wait_acp_response(
            &mut lines,
            &mut stdin,
            prompt_id,
            stream_tx,
            &mut streamed_thought,
            &mut streamed_text,
            true,
        )
        .await?;

        if streamed_text.is_empty()
            && let Some(fallback) = prompt_res.get("text").and_then(Value::as_str)
            && !fallback.is_empty()
        {
            streamed_text.push_str(fallback);
            if let Some(tx) = stream_tx {
                let _ = tx
                    .send(AgentStreamEvent::TextDelta(fallback.to_owned()))
                    .await;
            }
        }

        Ok(AssistantTurnOutput {
            thinking: streamed_thought,
            text: streamed_text,
            tool_calls: Vec::new(),
        })
    }
    .await;

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;

    match turn_res {
        Ok(out) => Ok(out),
        Err(err) => {
            let stderr_text = stderr_buf.lock().await;
            Err(attach_stderr_tail(err, &stderr_text))
        }
    }
}

async fn run_codex_cli_turn(
    cfg: &LlmProviderConfig,
    mcp_url: &str,
    cwd: &Path,
    prompt: &str,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let command = if cfg.command.trim().is_empty() {
        "codex"
    } else {
        cfg.command.trim()
    };
    let mcp_url_toml = serde_json::to_string(mcp_url)
        .map_err(|e| CompassError::Internal(format!("encode mcp_url: {e}")))?;

    let mut cmd = Command::new(command);
    cmd.args([
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--sandbox",
        "read-only",
        "--json",
        "-c",
        "approval_policy=\"never\"",
        "-c",
        "mcp_servers.biocompass.enabled=true",
        "-c",
        "mcp_servers.biocompass.default_tools_approval_mode=\"auto\"",
        "-c",
        &format!("mcp_servers.biocompass.url={mcp_url_toml}"),
    ]);
    if !cfg.model.trim().is_empty() {
        cmd.args(["-m", cfg.model.trim()]);
    }
    if let Some(effort) = cfg
        .config_options
        .get("effort")
        .or_else(|| cfg.config_options.get("reasoning_effort"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let effort_toml = serde_json::to_string(effort)
            .map_err(|e| CompassError::Internal(format!("encode reasoning effort: {e}")))?;
        cmd.args(["-c", &format!("model_reasoning_effort={effort_toml}")]);
    }
    if !cfg.args.is_empty() {
        cmd.args(&cfg.args);
    }
    cmd.arg("-")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_cli_error("Codex CLI", command, &e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| CompassError::UpstreamLlm(format!("write Codex prompt stdin: {e}")))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("Codex child missing stdout".into()))?;
    let stderr_buf = spawn_stderr_collector(child.stderr.take());
    let mut lines = BufReader::new(stdout).lines();

    let mut final_thinking = String::new();
    let mut final_text = String::new();
    let mut last_error: Option<String> = None;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("read Codex stdout: {e}")))?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("");
        match ev_type {
            "item.completed" => {
                if let Some(item) = ev.get("item") {
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                    let text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if item_type == "reasoning" && !text.is_empty() {
                        if !final_thinking.is_empty() {
                            final_thinking.push_str("\n\n");
                            if let Some(tx) = stream_tx {
                                let _ = tx.send(AgentStreamEvent::ThoughtDelta("\n\n".into())).await;
                            }
                        }
                        final_thinking.push_str(text);
                        if let Some(tx) = stream_tx {
                            let _ = tx
                                .send(AgentStreamEvent::ThoughtDelta(text.to_owned()))
                                .await;
                        }
                    } else if item_type == "agent_message" && !text.is_empty() {
                        if !final_text.is_empty() {
                            final_text.push_str("\n\n");
                            if let Some(tx) = stream_tx {
                                let _ = tx.send(AgentStreamEvent::TextDelta("\n\n".into())).await;
                            }
                        }
                        final_text.push_str(text);
                        if let Some(tx) = stream_tx {
                            let _ = tx
                                .send(AgentStreamEvent::TextDelta(text.to_owned()))
                                .await;
                        }
                    }
                }
            }
            "turn.failed" | "error" => {
                let msg = ev
                    .pointer("/error/message")
                    .or_else(|| ev.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed");
                last_error = Some(msg.to_owned());
            }
            _ => {}
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("wait Codex child: {e}")))?;

    if (!status.success() && final_text.is_empty()) || (final_text.is_empty() && last_error.is_some())
    {
        let stderr_text = stderr_buf.lock().await;
        let detail = last_error
            .unwrap_or_else(|| format!("Codex CLI exited unexpectedly ({status})"));
        return Err(attach_stderr_tail(
            CompassError::UpstreamLlm(detail),
            &stderr_text,
        ));
    }

    Ok(AssistantTurnOutput {
        thinking: final_thinking,
        text: final_text,
        tool_calls: Vec::new(),
    })
}

async fn run_claude_cli_turn(
    cfg: &LlmProviderConfig,
    mcp_url: &str,
    cwd: &Path,
    prompt: &str,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> CompassResult<AssistantTurnOutput> {
    let command = if cfg.command.trim().is_empty() {
        "claude"
    } else {
        cfg.command.trim()
    };

    let cfg_path = std::env::temp_dir().join(format!("biocompass-claude-mcp-{}.json", Uuid::new_v4()));
    let mcp_cfg_json = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "biocompass": {
                "type": "http",
                "url": mcp_url,
            }
        }
    }))
    .map_err(|e| CompassError::Internal(format!("encode claude mcp config: {e}")))?;
    tokio::fs::write(&cfg_path, mcp_cfg_json)
        .await
        .map_err(|e| CompassError::Internal(format!("write temp claude mcp config: {e}")))?;
    let _guard = TempFileGuard(cfg_path.clone());

    let mut cmd = Command::new(command);
    cmd.args([
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
        "--mcp-config",
        &cfg_path.to_string_lossy(),
        "--tools",
        "",
        "--permission-mode",
        "bypassPermissions",
    ]);
    if !cfg.model.trim().is_empty() {
        cmd.args(["--model", cfg.model.trim()]);
    }
    if let Some(effort) = cfg
        .config_options
        .get("effort")
        .or_else(|| cfg.config_options.get("thinking"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        cmd.args(["--effort", effort]);
    }
    if !cfg.args.is_empty() {
        cmd.args(&cfg.args);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_cli_error("Claude CLI", command, &e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| CompassError::UpstreamLlm(format!("write Claude prompt stdin: {e}")))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("Claude child missing stdout".into()))?;
    let stderr_buf = spawn_stderr_collector(child.stderr.take());
    let mut lines = BufReader::new(stdout).lines();

    let mut streamed_thinking = String::new();
    let mut streamed_text = String::new();
    let mut fallback_text = String::new();
    let mut result_error: Option<String> = None;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("read Claude stdout: {e}")))?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("");
        match ev_type {
            "stream_event" => {
                if ev.pointer("/event/type").and_then(Value::as_str) == Some("content_block_delta")
                {
                    let delta_type = ev
                        .pointer("/event/delta/type")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if delta_type == "thinking_delta"
                        && let Some(delta) =
                            ev.pointer("/event/delta/thinking").and_then(Value::as_str)
                        && !delta.is_empty()
                    {
                        streamed_thinking.push_str(delta);
                        if let Some(tx) = stream_tx {
                            let _ = tx
                                .send(AgentStreamEvent::ThoughtDelta(delta.to_owned()))
                                .await;
                        }
                    } else if delta_type == "text_delta"
                        && let Some(delta) = ev.pointer("/event/delta/text").and_then(Value::as_str)
                        && !delta.is_empty()
                    {
                        streamed_text.push_str(delta);
                        if let Some(tx) = stream_tx {
                            let _ = tx
                                .send(AgentStreamEvent::TextDelta(delta.to_owned()))
                                .await;
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(parts) = ev.pointer("/message/content").and_then(Value::as_array) {
                    let mut combined = String::new();
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(text) = part.get("text").and_then(Value::as_str)
                        {
                            combined.push_str(text);
                        }
                    }
                    if !combined.is_empty() {
                        fallback_text = combined;
                    }
                }
            }
            "result" => {
                if ev.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let msg = ev
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("Claude CLI execution failed");
                    result_error = Some(msg.to_owned());
                } else if fallback_text.is_empty()
                    && let Some(res) = ev.get("result").and_then(Value::as_str)
                {
                    fallback_text = res.to_owned();
                }
            }
            _ => {}
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| CompassError::UpstreamLlm(format!("wait Claude child: {e}")))?;

    if let Some(err) = result_error {
        return Err(CompassError::UpstreamLlm(err));
    }

    let final_text = if !streamed_text.is_empty() {
        streamed_text
    } else {
        if !fallback_text.is_empty()
            && let Some(tx) = stream_tx
        {
            let _ = tx
                .send(AgentStreamEvent::TextDelta(fallback_text.clone()))
                .await;
        }
        fallback_text
    };

    if !status.success() && final_text.is_empty() {
        let stderr_text = stderr_buf.lock().await;
        return Err(attach_stderr_tail(
            CompassError::UpstreamLlm(format!("Claude CLI exited unexpectedly ({status})")),
            &stderr_text,
        ));
    }

    Ok(AssistantTurnOutput {
        thinking: streamed_thinking,
        text: final_text,
        tool_calls: Vec::new(),
    })
}

fn push_acp_choice_value(out: &mut Vec<ProviderOptionChoice>, item: &Value) {
    if let Some(value) = item.get("value").and_then(Value::as_str) {
        let label = item
            .get("name")
            .or_else(|| item.get("label"))
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_owned();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        out.push(ProviderOptionChoice {
            value: value.to_owned(),
            label,
            description,
        });
    } else if let Some(grouped) = item.get("options").and_then(Value::as_array) {
        for sub in grouped {
            push_acp_choice_value(out, sub);
        }
    }
}

fn extract_acp_option_choices(opt: &Value) -> Vec<ProviderOptionChoice> {
    let mut choices = Vec::new();
    if let Some(arr) = opt.get("options").and_then(Value::as_array) {
        for item in arr {
            push_acp_choice_value(&mut choices, item);
        }
    }
    choices
}

fn parse_acp_config_options(sess: &Value) -> Vec<ProviderControlSpec> {
    let mut controls = Vec::new();
    if let Some(opts) = sess.get("configOptions").and_then(Value::as_array) {
        for opt in opts {
            let Some(id) = opt.get("id").and_then(Value::as_str) else {
                continue;
            };
            let label = opt
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned();
            let category = opt
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned();
            let current_value = opt
                .get("currentValue")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let options = extract_acp_option_choices(opt);
            controls.push(ProviderControlSpec {
                id: id.to_owned(),
                label,
                category,
                current_value,
                options,
            });
        }
    }
    if !controls.iter().any(|c| c.id == "mode")
        && let Some(modes_obj) = sess.get("modes")
        && let Some(available) = modes_obj.get("availableModes").and_then(Value::as_array)
        && !available.is_empty()
    {
        let options = available
            .iter()
            .filter_map(|m| {
                let id = m.get("id").and_then(Value::as_str)?;
                let label = m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned();
                let description = m
                    .get("description")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                Some(ProviderOptionChoice {
                    value: id.to_owned(),
                    label,
                    description,
                })
            })
            .collect();
        controls.push(ProviderControlSpec {
            id: "mode".into(),
            label: "Session Mode".into(),
            category: "mode".into(),
            current_value: modes_obj
                .get("currentModeId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            options,
        });
    }
    controls
}

/// Probe an ACP agent command (`initialize` + `session/new` against empty MCP servers)
/// to verify binary health and read all advertised dynamic controls (`model`, `thinking`, `effort`, `mode`, ...).
pub async fn probe_acp_backend(
    command: &str,
    args: &[String],
    cwd: &Path,
) -> CompassResult<AcpProbeResult> {
    let mut cmd = Command::new(command);
    if !args.is_empty() {
        cmd.args(args);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_cli_error("ACP agent", command, &e))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("ACP probe child missing stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CompassError::UpstreamLlm("ACP probe child missing stdout".into()))?;
    let stderr_buf = spawn_stderr_collector(child.stderr.take());
    let mut lines = BufReader::new(stdout).lines();
    let mut dummy_thought = String::new();
    let mut dummy_text = String::new();

    let probe_res: CompassResult<AcpProbeResult> = tokio::time::timeout(
        Duration::from_secs(20),
        async {
            send_json_line(
                &mut stdin,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": 1,
                        "clientCapabilities": {},
                        "clientInfo": { "name": "biocompass-probe", "version": env!("CARGO_PKG_VERSION") }
                    }
                }),
            )
            .await?;
            let init = wait_acp_response(
                &mut lines,
                &mut stdin,
                1,
                None,
                &mut dummy_thought,
                &mut dummy_text,
                false,
            )
            .await?;
            let agent_name = init
                .pointer("/agentInfo/name")
                .and_then(Value::as_str)
                .unwrap_or(command)
                .to_owned();

            send_json_line(
                &mut stdin,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {
                        "cwd": cwd.to_string_lossy(),
                        "mcpServers": []
                    }
                }),
            )
            .await?;
            let sess = wait_acp_response(
                &mut lines,
                &mut stdin,
                2,
                None,
                &mut dummy_thought,
                &mut dummy_text,
                false,
            )
            .await?;

            let controls = parse_acp_config_options(&sess);
            let available_models = controls
                .iter()
                .find(|c| c.id == "model")
                .map(|c| c.options.iter().map(|o| o.value.clone()).collect())
                .unwrap_or_default();
            Ok(AcpProbeResult {
                agent_name,
                available_models,
                controls,
            })
        },
    )
    .await
    .map_err(|_| CompassError::UpstreamLlm(format!("ACP handshake timed out for `{command}`")))?;

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;

    match probe_res {
        Ok(res) => Ok(res),
        Err(err) => {
            let stderr_text = stderr_buf.lock().await;
            Err(attach_stderr_tail(err, &stderr_text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{attach_stderr_tail, strip_ansi_codes, strip_external_mcp_setup_notice};
    use biocompass_core_traits::CompassError;

    #[test]
    fn strip_external_mcp_notice_drops_unrelated_connector_failures_only() {
        let noise = "The following MCP servers failed to connect:\n- google_drive_mcp: Connector bootstrap issue: tools_or_system_prompt_failed\n- hla-compass: authentication was rejected";
        assert_eq!(strip_external_mcp_setup_notice(noise), "");

        let oauth = "MCP OAuth login is required for: google_drive_mcp. Run `/mcp login google_drive_mcp` in Vibe.";
        assert_eq!(strip_external_mcp_setup_notice(oauth), "");

        let real_biocompass = "The following MCP servers failed to connect:\n- biocompass: connection refused";
        assert_eq!(strip_external_mcp_setup_notice(real_biocompass), real_biocompass);

        let mixed = "The following MCP servers failed to connect:\n- hla-compass: authentication was rejected\nActual cohort summary.";
        assert_eq!(strip_external_mcp_setup_notice(mixed), "Actual cohort summary.");
    }

    #[test]
    fn strip_ansi_and_attach_stderr_tail_avoid_double_wrapping() {
        let ansi = "\x1b[31mError:\x1b[0m API token expired";
        assert_eq!(strip_ansi_codes(ansi), "Error: API token expired");

        let wrapped = CompassError::UpstreamLlm("CLI failed".into());
        let attached = attach_stderr_tail(wrapped, ansi);
        assert_eq!(attached.user_message(), "CLI failed — Error: API token expired");
    }
}
