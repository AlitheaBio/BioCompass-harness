//! `biocompass-agent-core`: Real streaming SSE computational biology agent harness.
//!
//! Enforces:
//! - Real OpenAI/DeepSeek (`/v1/chat/completions`) and Anthropic (`/v1/messages`) SSE streaming (zero mock replies in `src/`).
//! - Two-Step Execution Consent Gate (`AgentExecutionPort` physically lacks `confirm_run`).
//! - `> 12k`-token (`48 KB`) tool result spill into `BlobStore`.
//! - `Model-visible <=> Logged` transcript persistence in `SessionStore`.

pub mod llm;
pub mod prompt;
pub mod tools;

use biocompass_core_traits::{
    BlobStore, CompassError, CompassResult, DatasetGrant, RunQuote, SessionEvent, SessionEventKind,
    SessionId, SessionStore,
};
use llm::{
    ConversationMessage, LlmProviderConfig, LlmToolCall, build_http_client, stream_model_turn,
};
use prompt::build_system_prompt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tools::{
    AgentExecutionPort, DEFAULT_SPILL_THRESHOLD_BYTES, allowed_tool_definitions,
    execute_agent_tool, spill_tool_output_if_needed,
};

/// Real-time SSE events emitted during an agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    TextDelta(String),
    ToolCall { id: String, name: String, arguments: Value },
    ToolResult { id: String, name: String, preview: String },
    RunPrepared(RunQuote),
    Completed { assistant_message: String },
    Error(String),
}

/// Runtime parameters for `AgentRuntime`.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub llm: Option<LlmProviderConfig>,
    pub max_tool_iterations: usize,
    pub spill_threshold_bytes: usize,
    pub scaffold_root_dir: PathBuf,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            llm: LlmProviderConfig::from_env(),
            max_tool_iterations: 6,
            spill_threshold_bytes: DEFAULT_SPILL_THRESHOLD_BYTES,
            scaffold_root_dir: PathBuf::from("plugins"),
        }
    }
}

/// Summary returned when an agent turn completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnSummary {
    pub session_id: SessionId,
    pub assistant_text: String,
    pub prepared_quotes: Vec<RunQuote>,
    pub tool_calls_executed: usize,
}

/// Cloud-agnostic BioCompass agent runtime.
pub struct AgentRuntime {
    http: Client,
    config: AgentConfig,
    session_store: Arc<dyn SessionStore>,
    blob_store: Arc<dyn BlobStore>,
    execution_port: Arc<dyn AgentExecutionPort>,
}

impl AgentRuntime {
    pub fn new(
        config: AgentConfig,
        session_store: Arc<dyn SessionStore>,
        blob_store: Arc<dyn BlobStore>,
        execution_port: Arc<dyn AgentExecutionPort>,
    ) -> CompassResult<Self> {
        Ok(Self {
            http: build_http_client()?,
            config,
            session_store,
            blob_store,
            execution_port,
        })
    }

    pub const fn is_llm_configured(&self) -> bool {
        self.config.llm.is_some()
    }

    pub fn llm_model_name(&self) -> Option<&str> {
        self.config.llm.as_ref().map(|c| c.model.as_str())
    }

    /// Execute a full multi-step tool-using turn on `session_id`, streaming live events to `stream_tx`.
    pub async fn run_turn(
        &self,
        session_id: &SessionId,
        user_message: &str,
        active_grant: Option<&DatasetGrant>,
        stream_tx: Option<mpsc::Sender<AgentStreamEvent>>,
    ) -> CompassResult<AgentTurnSummary> {
        let llm_cfg = self.config.llm.as_ref().ok_or_else(|| {
            CompassError::UpstreamLlm(
                "no upstream LLM configured; set DEEPSEEK_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY, or BIOCOMPASS_LLM_BASE_URL".into(),
            )
        })?;

        let trimmed = user_message.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("user message cannot be empty".into()));
        }

        let session = self.session_store.get_session(session_id).await?;
        self.session_store
            .append_event(
                session_id,
                SessionEventKind::UserMessage,
                trimmed,
                json!({}),
            )
            .await?;

        let prior_events = self.session_store.list_events(session_id).await?;
        let mut history = reconstruct_conversation_window(&prior_events);

        let catalogs = self.execution_port.list_catalogs().await;
        let plugins = self.execution_port.list_plugins().await;
        let system_prompt = build_system_prompt(active_grant, &catalogs, &plugins);
        let tools = allowed_tool_definitions();

        let mut prepared_quotes = Vec::new();
        let mut total_tool_calls = 0usize;

        for _step in 0..self.config.max_tool_iterations {
            let turn_out = stream_model_turn(
                &self.http,
                llm_cfg,
                &system_prompt,
                &history,
                &tools,
                stream_tx.as_ref(),
            )
            .await?;

            if turn_out.tool_calls.is_empty() {
                let final_text = turn_out.text;
                self.session_store
                    .append_event(
                        session_id,
                        SessionEventKind::AssistantMessage,
                        &final_text,
                        json!({ "model": llm_cfg.model }),
                    )
                    .await?;
                if let Some(tx) = &stream_tx {
                    let _ = tx
                        .send(AgentStreamEvent::Completed {
                            assistant_message: final_text.clone(),
                        })
                        .await;
                }
                return Ok(AgentTurnSummary {
                    session_id: session_id.clone(),
                    assistant_text: final_text,
                    prepared_quotes,
                    tool_calls_executed: total_tool_calls,
                });
            }

            // Persist assistant tool-call intent (`Model-visible <=> Logged`).
            for tc in &turn_out.tool_calls {
                self.session_store
                    .append_event(
                        session_id,
                        SessionEventKind::ToolCall,
                        &tc.name,
                        json!({
                            "id": tc.id,
                            "name": tc.name,
                            "assistant_text": turn_out.text,
                            "arguments": tc.arguments,
                        }),
                    )
                    .await?;
                if let Some(tx) = &stream_tx {
                    let _ = tx
                        .send(AgentStreamEvent::ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        })
                        .await;
                }
            }

            history.push(ConversationMessage::Assistant {
                text: turn_out.text,
                tool_calls: turn_out.tool_calls.clone(),
            });

            for tc in &turn_out.tool_calls {
                total_tool_calls += 1;
                let raw_val = execute_agent_tool(
                    tc,
                    &session.org_id,
                    session_id,
                    active_grant,
                    self.execution_port.as_ref(),
                    self.session_store.as_ref(),
                    &self.config.scaffold_root_dir,
                    stream_tx.as_ref(),
                )
                .await;

                if let Some(quote_val) = raw_val.get("quote")
                    && let Ok(q) = serde_json::from_value::<RunQuote>(quote_val.clone())
                {
                    prepared_quotes.push(q);
                }

                let raw_str = serde_json::to_string(&raw_val)
                    .map_err(|e| CompassError::Internal(format!("encode tool result: {e}")))?;
                let model_visible_content = spill_tool_output_if_needed(
                    self.blob_store.as_ref(),
                    session_id,
                    &tc.id,
                    &raw_str,
                    self.config.spill_threshold_bytes,
                )
                .await?;

                self.session_store
                    .append_event(
                        session_id,
                        SessionEventKind::ToolResult,
                        &model_visible_content,
                        json!({
                            "tool_call_id": tc.id,
                            "tool_name": tc.name,
                            "raw_bytes": raw_str.len(),
                        }),
                    )
                    .await?;

                if let Some(tx) = &stream_tx {
                    let short_preview: String = model_visible_content.chars().take(240).collect();
                    let _ = tx
                        .send(AgentStreamEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            preview: short_preview,
                        })
                        .await;
                }

                history.push(ConversationMessage::ToolResult {
                    tool_call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    content: model_visible_content,
                });
            }
        }

        Err(CompassError::UpstreamLlm(format!(
            "agent exceeded max_tool_iterations ({}) without final response",
            self.config.max_tool_iterations
        )))
    }
}

fn reconstruct_conversation_window(events: &[SessionEvent]) -> Vec<ConversationMessage> {
    let mut history = Vec::new();
    let mut idx = 0usize;
    while idx < events.len() {
        let ev = &events[idx];
        match ev.kind {
            SessionEventKind::UserMessage => {
                history.push(ConversationMessage::User(ev.content.clone()));
                idx += 1;
            }
            SessionEventKind::AssistantMessage => {
                history.push(ConversationMessage::Assistant {
                    text: ev.content.clone(),
                    tool_calls: Vec::new(),
                });
                idx += 1;
            }
            SessionEventKind::ToolCall => {
                let mut calls = Vec::new();
                let mut assistant_text = String::new();
                while idx < events.len() && events[idx].kind == SessionEventKind::ToolCall {
                    let meta = &events[idx].metadata;
                    if assistant_text.is_empty()
                        && let Some(t) = meta.get("assistant_text").and_then(Value::as_str)
                    {
                        assistant_text = t.to_owned();
                    }
                    calls.push(LlmToolCall {
                        id: meta
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("call_0")
                            .to_owned(),
                        name: events[idx].content.clone(),
                        arguments: meta
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    });
                    idx += 1;
                }
                history.push(ConversationMessage::Assistant {
                    text: assistant_text,
                    tool_calls: calls,
                });
            }
            SessionEventKind::ToolResult => {
                let meta = &ev.metadata;
                history.push(ConversationMessage::ToolResult {
                    tool_call_id: meta
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_0")
                        .to_owned(),
                    tool_name: meta
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned(),
                    content: ev.content.clone(),
                });
                idx += 1;
            }
            _ => {
                idx += 1;
            }
        }
    }
    history
}
