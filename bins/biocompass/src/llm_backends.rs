//! Local subscription CLI / ACP agent discovery, redaction, validation, and connectivity probing.
//!
//! Strictly gated to `DeploymentMode::Local`; raw API keys are never returned over HTTP.

use biocompass_agent_core::cli_agent::{
    ProviderControlSpec, ProviderOptionChoice, probe_acp_backend,
};
use biocompass_agent_core::llm::{LlmProviderConfig, LlmWireProtocol, build_http_client};
use biocompass_core_traits::{CompassError, CompassResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CachedProviderControls {
    #[serde(default)]
    pub controls: Vec<ProviderControlSpec>,
    #[serde(default)]
    pub per_model_controls: BTreeMap<String, Vec<ProviderControlSpec>>,
}

pub type LlmControlsCache = Arc<RwLock<HashMap<String, CachedProviderControls>>>;

#[derive(Debug, Clone, Serialize)]
pub struct RedactedLlmConfig {
    pub protocol: LlmWireProtocol,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub config_options: BTreeMap<String, String>,
    pub has_api_key: bool,
    pub api_key_hint: Option<String>,
    pub display_name: String,
    pub is_local_cli: bool,
    pub provider_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendAuthState {
    SignedIn,
    NeedsLogin,
    Ready,
    NotInstalled,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredLlmBackend {
    pub id: String,
    pub label: String,
    pub subtitle: String,
    pub protocol: LlmWireProtocol,
    pub command: String,
    pub default_args: Vec<String>,
    pub installed: bool,
    pub resolved_path: Option<String>,
    pub auth_state: BackendAuthState,
    pub auth_detail: String,
    pub login_command: Option<String>,
    pub suggested_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmConfigResponse {
    pub configured: bool,
    pub display_name: Option<String>,
    pub active: Option<RedactedLlmConfig>,
    #[serde(default)]
    pub active_controls: Vec<ProviderControlSpec>,
    #[serde(default)]
    pub per_model_controls: BTreeMap<String, Vec<ProviderControlSpec>>,
    pub local_management_enabled: bool,
    pub discovered_backends: Vec<DiscoveredLlmBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLlmConfigRequest {
    #[serde(default)]
    pub disconnect: bool,
    pub protocol: Option<LlmWireProtocol>,
    #[serde(default)]
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: String,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub config_options: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestLlmConnectionResponse {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub available_models: Vec<String>,
    #[serde(default)]
    pub controls: Vec<ProviderControlSpec>,
    #[serde(default)]
    pub per_model_controls: BTreeMap<String, Vec<ProviderControlSpec>>,
}

/// Derive a stable provider identifier (`codex`, `claude`, `vibe`, `opencode`, `gemini`,
/// `openai`, `mistral`, `deepseek`, `ollama`, `acp`) for icons and UI grouping.
#[must_use]
pub fn infer_provider_id(cfg: &LlmProviderConfig) -> String {
    match cfg.protocol {
        LlmWireProtocol::CodexCli => "codex".into(),
        LlmWireProtocol::ClaudeCodeCli => "claude".into(),
        LlmWireProtocol::AcpStdio => {
            let cmd = Path::new(cfg.command.trim())
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if cmd.contains("vibe") {
                "vibe".into()
            } else if cmd.contains("opencode") {
                "opencode".into()
            } else if cmd.contains("gemini") {
                "gemini".into()
            } else if cmd.contains("codex") {
                "codex".into()
            } else if cmd.contains("claude") {
                "claude".into()
            } else {
                "acp".into()
            }
        }
        LlmWireProtocol::AnthropicMessages => "claude".into(),
        LlmWireProtocol::OpenAiChatCompletions => {
            let url = cfg.base_url.trim().to_ascii_lowercase();
            if url.contains("api.mistral.ai") {
                "mistral".into()
            } else if url.contains("api.deepseek.com") {
                "deepseek".into()
            } else if url.contains("generativelanguage.googleapis.com") {
                "gemini".into()
            } else if url.contains("api.anthropic.com") {
                "claude".into()
            } else if url.contains(":11434")
                || url.contains("localhost")
                || url.contains("127.0.0.1")
            {
                "ollama".into()
            } else {
                "openai".into()
            }
        }
    }
}

/// Redact secrets before returning active LLM configuration to the frontend.
#[must_use]
pub fn redact_llm_config(cfg: &LlmProviderConfig) -> RedactedLlmConfig {
    let trimmed_key = cfg.api_key.trim();
    let has_api_key = !trimmed_key.is_empty();
    let api_key_hint = if !has_api_key {
        None
    } else if trimmed_key.chars().count() >= 10 {
        let prefix: String = trimmed_key.chars().take(4).collect();
        let suffix: String = trimmed_key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some(format!("{prefix}…{suffix}"))
    } else {
        Some("••••".into())
    };

    RedactedLlmConfig {
        protocol: cfg.protocol,
        base_url: cfg.base_url.clone(),
        model: cfg.model.clone(),
        max_tokens: cfg.max_tokens,
        command: cfg.command.clone(),
        args: cfg.args.clone(),
        config_options: cfg.config_options.clone(),
        has_api_key,
        api_key_hint,
        display_name: cfg.display_name(),
        is_local_cli: cfg.is_local_cli(),
        provider_id: infer_provider_id(cfg),
    }
}

/// Defense-in-depth command validation for local CLI/ACP backends (`SEC-16`).
/// Only permits recognized coding/ACP CLI executables and forbids shells, scripting interpreters,
/// whitespace, and shell metacharacters.
fn validate_local_cli_command(command: &str, args: &[String]) -> CompassResult<()> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(CompassError::Validation("CLI backend command cannot be empty".into()));
    }
    if trimmed.starts_with('-')
        || trimmed
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, ';' | '&' | '|' | '`' | '$' | '<' | '>'))
    {
        return Err(CompassError::Validation(
            "CLI backend command contains disallowed characters".into(),
        ));
    }
    let basename = std::path::Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let allowed = matches!(
        basename.as_str(),
        "opencode"
            | "gemini"
            | "claude"
            | "claude-code"
            | "claude-code-acp"
            | "codex"
            | "codex-acp"
            | "vibe"
            | "vibe-acp"
            | "kiro-cli"
            | "goose"
            | "aider"
    );
    if !allowed {
        return Err(CompassError::Validation(format!(
            "CLI backend executable '{trimmed}' is not an allowed ACP/CLI agent binary"
        )));
    }
    if args
        .iter()
        .any(|a| a.chars().any(char::is_control))
    {
        return Err(CompassError::Validation(
            "CLI backend arguments must not contain control characters".into(),
        ));
    }
    Ok(())
}

/// Validate and normalize an incoming UI update payload into a `LlmProviderConfig`.
pub fn normalize_llm_config_request(
    req: UpdateLlmConfigRequest,
    previous: Option<&LlmProviderConfig>,
) -> CompassResult<Option<LlmProviderConfig>> {
    if req.disconnect {
        return Ok(None);
    }
    let protocol = req.protocol.ok_or_else(|| {
        CompassError::Validation("protocol is required unless disconnect=true".into())
    })?;
    let max_tokens = req.max_tokens.unwrap_or(4096).clamp(256, 128_000);
    let model = req.model.trim().to_owned();
    let mut command = req.command.trim().to_owned();
    let mut args: Vec<String> = req
        .args
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();

    if protocol == LlmWireProtocol::CodexCli && command.is_empty() {
        command = "codex".into();
    } else if protocol == LlmWireProtocol::ClaudeCodeCli && command.is_empty() {
        command = "claude".into();
    } else if protocol == LlmWireProtocol::AcpStdio {
        if command.is_empty() {
            return Err(CompassError::Validation(
                "ACP backend requires a non-empty command (e.g. `opencode` or `gemini`)".into(),
            ));
        }
        if command == "opencode" && args.is_empty() {
            args.push("acp".into());
        } else if command == "gemini" && args.is_empty() {
            args.push("--acp".into());
        }
    }

    if matches!(
        protocol,
        LlmWireProtocol::AcpStdio | LlmWireProtocol::CodexCli | LlmWireProtocol::ClaudeCodeCli
    ) {
        validate_local_cli_command(&command, &args)?;
    }

    let same_backend = previous.is_some_and(|prev| {
        prev.protocol == protocol
            && prev.command == command
            && prev.args == args
            && prev.base_url.trim_end_matches('/') == req.base_url.trim().trim_end_matches('/')
    });
    let config_options: BTreeMap<String, String> = match req.config_options {
        Some(map) => map
            .into_iter()
            .filter_map(|(k, v)| {
                let kt = k.trim().to_owned();
                let vt = v.trim().to_owned();
                if kt.is_empty() || vt.is_empty() {
                    None
                } else {
                    Some((kt, vt))
                }
            })
            .collect(),
        None if same_backend => previous
            .map(|p| p.config_options.clone())
            .unwrap_or_default(),
        None => BTreeMap::new(),
    };

    match protocol {
        LlmWireProtocol::AcpStdio | LlmWireProtocol::CodexCli | LlmWireProtocol::ClaudeCodeCli => {
            Ok(Some(LlmProviderConfig {
                protocol,
                base_url: String::new(),
                api_key: String::new(),
                model,
                max_tokens,
                command,
                args,
                config_options,
            }))
        }
        LlmWireProtocol::OpenAiChatCompletions | LlmWireProtocol::AnthropicMessages => {
            let base_url = req.base_url.trim().trim_end_matches('/').to_owned();
            if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
                return Err(CompassError::Validation(
                    "HTTP LLM base_url must start with http:// or https://".into(),
                ));
            }
            if model.is_empty() {
                return Err(CompassError::Validation(
                    "model name is required for HTTP API backends".into(),
                ));
            }
            let incoming_key = req.api_key.unwrap_or_default().trim().to_owned();
            let preserved_key = if incoming_key.is_empty()
                && let Some(prev) = previous
                && prev.protocol == protocol
                && prev.base_url.trim_end_matches('/') == base_url
            {
                prev.api_key.clone()
            } else {
                incoming_key
            };

            Ok(Some(LlmProviderConfig {
                protocol,
                base_url,
                api_key: preserved_key,
                model,
                max_tokens,
                command: String::new(),
                args: Vec::new(),
                config_options,
            }))
        }
    }
}

fn candidate_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path_env) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path_env));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

fn resolve_executable(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    candidate_bin_dirs()
        .into_iter()
        .map(|dir| dir.join(cmd))
        .find(|candidate| candidate.is_file())
}

async fn run_quick_cli(program: &Path, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(3), cmd.output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let combined = match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => String::new(),
    };
    Some((out.status.success(), combined))
}

fn parse_codex_config_toml_defaults(home: &Path) -> (String, String) {
    let Ok(raw) = std::fs::read_to_string(home.join(".codex/config.toml")) else {
        return (String::new(), String::new());
    };
    let mut model = String::new();
    let mut effort = String::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            break;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_owned();
            if key == "model" && !val.is_empty() {
                model = val;
            } else if key == "model_reasoning_effort" && !val.is_empty() {
                effort = val;
            }
        }
    }
    (model, effort)
}

/// Load available Codex CLI models & per-model reasoning effort options dynamically from
/// `~/.codex/models_cache.json` and `~/.codex/config.toml`. Zero hardcoded model lists.
#[must_use]
pub fn load_codex_controls(selected_model: &str, selected_effort: &str) -> CachedProviderControls {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return CachedProviderControls::default();
    };
    let (cfg_model, cfg_effort) = parse_codex_config_toml_defaults(&home);
    let Ok(raw) = std::fs::read_to_string(home.join(".codex/models_cache.json")) else {
        return CachedProviderControls::default();
    };
    let Ok(cache) = serde_json::from_str::<Value>(&raw) else {
        return CachedProviderControls::default();
    };
    let Some(models_arr) = cache.get("models").and_then(Value::as_array) else {
        return CachedProviderControls::default();
    };

    let mut entries: Vec<(i64, &Value)> = models_arr
        .iter()
        .filter(|m| {
            m.get("visibility")
                .and_then(Value::as_str)
                .unwrap_or("list")
                == "list"
        })
        .map(|m| (m.get("priority").and_then(Value::as_i64).unwrap_or(999), m))
        .collect();
    entries.sort_by_key(|(prio, _)| *prio);

    let mut model_choices = Vec::new();
    let mut per_model_controls = BTreeMap::new();

    for (_, entry) in entries {
        let Some(slug) = entry.get("slug").and_then(Value::as_str) else {
            continue;
        };
        let label = entry
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(slug)
            .to_owned();
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        model_choices.push(ProviderOptionChoice {
            value: slug.to_owned(),
            label,
            description,
        });

        let default_effort = entry
            .get("default_reasoning_level")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut effort_choices = Vec::new();
        if let Some(levels) = entry
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
        {
            for lvl in levels {
                if let Some(eff) = lvl.get("effort").and_then(Value::as_str) {
                    let desc = lvl
                        .get("description")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    effort_choices.push(ProviderOptionChoice {
                        value: eff.to_owned(),
                        label: eff.to_owned(),
                        description: desc,
                    });
                }
            }
        }
        if !effort_choices.is_empty() {
            let eff_current = if !selected_effort.trim().is_empty()
                && effort_choices.iter().any(|c| c.value == selected_effort.trim())
            {
                selected_effort.trim().to_owned()
            } else if !cfg_effort.is_empty()
                && effort_choices.iter().any(|c| c.value == cfg_effort)
            {
                cfg_effort.clone()
            } else if !default_effort.is_empty() {
                default_effort
            } else {
                effort_choices[0].value.clone()
            };
            per_model_controls.insert(
                slug.to_owned(),
                vec![ProviderControlSpec {
                    id: "effort".into(),
                    label: "Effort".into(),
                    category: "thought_level".into(),
                    current_value: eff_current,
                    options: effort_choices,
                }],
            );
        }
    }

    let active_model = if !selected_model.trim().is_empty() {
        selected_model.trim().to_owned()
    } else if !cfg_model.is_empty() {
        cfg_model
    } else {
        model_choices
            .first()
            .map(|c| c.value.clone())
            .unwrap_or_default()
    };

    let mut controls = Vec::new();
    if !model_choices.is_empty() {
        controls.push(ProviderControlSpec {
            id: "model".into(),
            label: "Model".into(),
            category: "model".into(),
            current_value: active_model.clone(),
            options: model_choices,
        });
    }
    if let Some(model_ctrls) = per_model_controls
        .get(&active_model)
        .or_else(|| per_model_controls.values().next())
    {
        controls.extend(model_ctrls.clone());
    }

    CachedProviderControls {
        controls,
        per_model_controls,
    }
}

fn parse_parenthetical_tokens_after(text: &str, flag: &str) -> Vec<String> {
    let Some(idx) = text.find(flag) else {
        return Vec::new();
    };
    let rest = &text[idx + flag.len()..];
    let limit = rest.len().min(320);
    let snippet = &rest[..limit];
    let Some(open) = snippet.find('(') else {
        return Vec::new();
    };
    let after_open = &snippet[open + 1..];
    let Some(close) = after_open.find(')') else {
        return Vec::new();
    };
    after_open[..close]
        .split([',', ' '])
        .filter_map(|tok| {
            let cleaned = tok.trim().trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.');
            if cleaned.is_empty() || cleaned == "e.g." || cleaned == "or" {
                None
            } else {
                Some(cleaned.to_owned())
            }
        })
        .collect()
}

/// Probe `claude --help` + `~/.claude/settings.json` + `~/.claude/stats-cache.json`
/// to discover Claude CLI model and effort controls without hardcoding.
async fn load_claude_controls(
    bin: &Path,
    selected_model: &str,
    selected_effort: &str,
) -> CachedProviderControls {
    let mut model_choices: Vec<ProviderOptionChoice> = Vec::new();
    let mut seen_models: HashSet<String> = HashSet::new();
    let mut effort_choices: Vec<ProviderOptionChoice> = Vec::new();

    if let Some((_, help_text)) = run_quick_cli(bin, &["--help"]).await {
        // Parse `--effort <level> ... (low, medium, high, xhigh, max)` dynamically from CLI help.
        for level in parse_parenthetical_tokens_after(&help_text, "--effort ") {
            effort_choices.push(ProviderOptionChoice {
                value: level.clone(),
                label: level,
                description: None,
            });
        }
        // Parse single-quoted model aliases documented next to `--model <model>` in CLI help.
        if let Some(m_idx) = help_text.find("--model ") {
            let snippet: String = help_text[m_idx..].chars().take(260).collect();
            for chunk in snippet.split('\'').skip(1).step_by(2) {
                let alias = chunk.trim();
                if !alias.is_empty()
                    && alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    && seen_models.insert(alias.to_owned())
                {
                    model_choices.push(ProviderOptionChoice {
                        value: alias.to_owned(),
                        label: alias.to_owned(),
                        description: Some("CLI alias".into()),
                    });
                }
            }
        }
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut settings_model = String::new();
    let mut settings_effort = String::new();
    if let Some(ref h) = home {
        if let Ok(raw) = std::fs::read_to_string(h.join(".claude/settings.json"))
            && let Ok(val) = serde_json::from_str::<Value>(&raw)
        {
            if let Some(m) = val.get("model").and_then(Value::as_str) {
                settings_model = m.trim().to_owned();
            }
            if let Some(e) = val.get("effortLevel").and_then(Value::as_str) {
                settings_effort = e.trim().to_owned();
            }
        }
        if let Ok(raw) = std::fs::read_to_string(h.join(".claude/stats-cache.json"))
            && let Ok(val) = serde_json::from_str::<Value>(&raw)
            && let Some(usage) = val.get("modelUsage").and_then(Value::as_object)
        {
            let mut ids: Vec<String> = usage.keys().cloned().collect();
            ids.sort();
            ids.reverse();
            for id in ids {
                if seen_models.insert(id.clone()) {
                    model_choices.push(ProviderOptionChoice {
                        value: id.clone(),
                        label: id,
                        description: None,
                    });
                }
            }
        }
    }

    let active_model = if !selected_model.trim().is_empty() {
        selected_model.trim().to_owned()
    } else if !settings_model.is_empty() {
        settings_model
    } else {
        model_choices
            .first()
            .map(|c| c.value.clone())
            .unwrap_or_default()
    };

    let mut controls = Vec::new();
    if !model_choices.is_empty() {
        controls.push(ProviderControlSpec {
            id: "model".into(),
            label: "Model".into(),
            category: "model".into(),
            current_value: active_model,
            options: model_choices,
        });
    }
    if !effort_choices.is_empty() {
        let active_effort = if !selected_effort.trim().is_empty() {
            selected_effort.trim().to_owned()
        } else if !settings_effort.is_empty() {
            settings_effort
        } else {
            effort_choices
                .iter()
                .find(|c| c.value == "medium" || c.value == "high")
                .unwrap_or(&effort_choices[0])
                .value
                .clone()
        };
        controls.push(ProviderControlSpec {
            id: "effort".into(),
            label: "Effort".into(),
            category: "thought_level".into(),
            current_value: active_effort,
            options: effort_choices,
        });
    }

    CachedProviderControls {
        controls,
        per_model_controls: BTreeMap::new(),
    }
}

async fn probe_codex_backend() -> DiscoveredLlmBackend {
    let resolved = resolve_executable("codex");
    let (installed, auth_state, auth_detail) = if let Some(ref path) = resolved {
        match run_quick_cli(path, &["login", "status"]).await {
            Some((ok, text)) if ok || text.to_ascii_lowercase().contains("logged in") => (
                true,
                BackendAuthState::SignedIn,
                if text.is_empty() {
                    "Signed in".into()
                } else {
                    text
                },
            ),
            Some((_, text)) => (
                true,
                BackendAuthState::NeedsLogin,
                if text.is_empty() {
                    "Not logged in".into()
                } else {
                    text
                },
            ),
            None => (
                true,
                BackendAuthState::NeedsLogin,
                "Unable to query `codex login status`".into(),
            ),
        }
    } else {
        (
            false,
            BackendAuthState::NotInstalled,
            "Install Codex CLI (`npm i -g @openai/codex`)".into(),
        )
    };

    let dynamic_models = load_codex_controls("", "")
        .controls
        .into_iter()
        .find(|c| c.id == "model")
        .map(|c| {
            let mut list = vec![String::new()];
            list.extend(c.options.into_iter().map(|o| o.value));
            list
        })
        .unwrap_or_default();

    DiscoveredLlmBackend {
        id: "codex".into(),
        label: "Codex CLI (ChatGPT Subscription)".into(),
        subtitle: "Uses your local ChatGPT Plus/Pro/Team login via `codex exec` + loopback MCP".into(),
        protocol: LlmWireProtocol::CodexCli,
        command: resolved
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "codex".into()),
        default_args: Vec::new(),
        installed,
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        auth_state,
        auth_detail,
        login_command: Some("codex login".into()),
        suggested_models: dynamic_models,
    }
}

async fn probe_claude_backend() -> DiscoveredLlmBackend {
    let resolved = resolve_executable("claude");
    let (installed, auth_state, auth_detail) = if let Some(ref path) = resolved {
        match run_quick_cli(path, &["auth", "status"]).await {
            Some((_, text)) => {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    let logged_in = val
                        .get("loggedIn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if logged_in {
                        let sub = val
                            .get("subscriptionType")
                            .and_then(Value::as_str)
                            .or_else(|| val.get("authMethod").and_then(Value::as_str))
                            .unwrap_or("Claude account");
                        (
                            true,
                            BackendAuthState::SignedIn,
                            format!("Signed in ({sub})"),
                        )
                    } else {
                        (
                            true,
                            BackendAuthState::NeedsLogin,
                            "Not logged in (`claude auth login`)".into(),
                        )
                    }
                } else {
                    (
                        true,
                        BackendAuthState::Ready,
                        if text.is_empty() {
                            "CLI installed".into()
                        } else {
                            text
                        },
                    )
                }
            }
            None => (
                true,
                BackendAuthState::NeedsLogin,
                "Unable to query `claude auth status`".into(),
            ),
        }
    } else {
        (
            false,
            BackendAuthState::NotInstalled,
            "Install Claude Code (`npm i -g @anthropic-ai/claude-code`)".into(),
        )
    };

    let dynamic_models = if let Some(ref bin) = resolved {
        load_claude_controls(bin, "", "")
            .await
            .controls
            .into_iter()
            .find(|c| c.id == "model")
            .map(|c| {
                let mut list = vec![String::new()];
                list.extend(c.options.into_iter().map(|o| o.value));
                list
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    DiscoveredLlmBackend {
        id: "claude_code".into(),
        label: "Claude Code CLI (Claude Pro / Max)".into(),
        subtitle: "Uses your local Claude subscription via `claude -p` + loopback MCP".into(),
        protocol: LlmWireProtocol::ClaudeCodeCli,
        command: resolved
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".into()),
        default_args: Vec::new(),
        installed,
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        auth_state,
        auth_detail,
        login_command: Some("claude auth login".into()),
        suggested_models: dynamic_models,
    }
}

async fn probe_opencode_backend() -> DiscoveredLlmBackend {
    let resolved = resolve_executable("opencode");
    let (installed, auth_state, auth_detail) = if resolved.is_some() {
        (
            true,
            BackendAuthState::Ready,
            "ACP v1 ready (`opencode acp`) — run Test or Connect to load live models".into(),
        )
    } else {
        (
            false,
            BackendAuthState::NotInstalled,
            "Install OpenCode (`brew install opencode`)".into(),
        )
    };

    DiscoveredLlmBackend {
        id: "opencode_acp".into(),
        label: "OpenCode (ACP Agent)".into(),
        subtitle: "Connects over Agent Client Protocol (`opencode acp`) with multi-provider auth".into(),
        protocol: LlmWireProtocol::AcpStdio,
        command: resolved
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "opencode".into()),
        default_args: vec!["acp".into()],
        installed,
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        auth_state,
        auth_detail,
        login_command: Some("opencode auth login".into()),
        suggested_models: Vec::new(),
    }
}

async fn probe_gemini_backend() -> DiscoveredLlmBackend {
    let resolved = resolve_executable("gemini");
    let has_env_key = std::env::var("GEMINI_API_KEY")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("GOOGLE_API_KEY")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let (installed, auth_state, auth_detail) = if resolved.is_some() {
        if has_env_key {
            (
                true,
                BackendAuthState::Ready,
                "ACP binary installed (`GEMINI_API_KEY` set)".into(),
            )
        } else {
            (
                true,
                BackendAuthState::Ready,
                "ACP binary installed (`gemini --acp`) — click Test to verify account".into(),
            )
        }
    } else {
        (
            false,
            BackendAuthState::NotInstalled,
            "Install Gemini CLI (`npm i -g @google/gemini-cli`)".into(),
        )
    };

    DiscoveredLlmBackend {
        id: "gemini_acp".into(),
        label: "Gemini CLI (ACP Agent)".into(),
        subtitle: "Connects over Agent Client Protocol (`gemini --acp`)".into(),
        protocol: LlmWireProtocol::AcpStdio,
        command: resolved
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "gemini".into()),
        default_args: vec!["--acp".into()],
        installed,
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        auth_state,
        auth_detail,
        login_command: Some("gemini".into()),
        suggested_models: Vec::new(),
    }
}

/// Probe Mistral Vibe CLI (`vibe-acp`) and local `~/.vibe` auth state.
async fn probe_vibe_backend() -> DiscoveredLlmBackend {
    let resolved = resolve_executable("vibe-acp");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let has_env_key = std::env::var("MISTRAL_API_KEY")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        || home.as_ref().is_some_and(|h| {
            std::fs::read_to_string(h.join(".vibe/.env"))
                .map(|c| c.lines().any(|l| l.trim_start().starts_with("MISTRAL_API_KEY=")))
                .unwrap_or(false)
        });
    let plan_name = home.as_ref().and_then(|h| {
        let raw = std::fs::read_to_string(h.join(".vibe/whoami_cache.json")).ok()?;
        let val = serde_json::from_str::<Value>(&raw).ok()?;
        val.as_object()?
            .values()
            .next()?
            .pointer("/payload/plan_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });

    let (installed, auth_state, auth_detail) = if resolved.is_some() {
        if let Some(ref plan) = plan_name {
            (
                true,
                BackendAuthState::SignedIn,
                format!("Signed in (Mistral {plan} via `vibe-acp`)"),
            )
        } else if has_env_key {
            (
                true,
                BackendAuthState::SignedIn,
                "Configured with MISTRAL_API_KEY via `vibe-acp`".into(),
            )
        } else {
            (
                true,
                BackendAuthState::NeedsLogin,
                "Not logged in (`vibe-acp --setup`)".into(),
            )
        }
    } else {
        (
            false,
            BackendAuthState::NotInstalled,
            "Install Mistral Vibe (`uv tool install mistral-vibe`)".into(),
        )
    };

    DiscoveredLlmBackend {
        id: "vibe_acp".into(),
        label: "Mistral Vibe (ACP Agent)".into(),
        subtitle: "Connects to Mistral AI / Devstral via `vibe-acp` + loopback MCP".into(),
        protocol: LlmWireProtocol::AcpStdio,
        command: resolved
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "vibe-acp".into()),
        default_args: Vec::new(),
        installed,
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        auth_state,
        auth_detail,
        login_command: Some("vibe-acp --setup".into()),
        suggested_models: Vec::new(),
    }
}

/// Probe locally installed subscription CLI and ACP agents in parallel.
pub async fn discover_local_llm_backends() -> Vec<DiscoveredLlmBackend> {
    let (codex, claude, vibe, opencode, gemini) = tokio::join!(
        probe_codex_backend(),
        probe_claude_backend(),
        probe_vibe_backend(),
        probe_opencode_backend(),
        probe_gemini_backend(),
    );
    vec![codex, claude, vibe, opencode, gemini]
}

fn backend_cache_key(cfg: &LlmProviderConfig) -> String {
    format!(
        "{:?}|{}|{}|{}",
        cfg.protocol,
        cfg.command.trim(),
        cfg.args.join(" "),
        cfg.base_url.trim().trim_end_matches('/')
    )
}

/// Apply current `cfg.model` and `cfg.config_options` onto cached provider controls so the UI
/// always sees both the full dynamic provider option choices AND the user's current selections.
fn apply_selected_control_values(
    snapshot: &CachedProviderControls,
    cfg: &LlmProviderConfig,
) -> CachedProviderControls {
    let selected_model = cfg.model.trim();
    let mut controls = snapshot.controls.clone();

    if !snapshot.per_model_controls.is_empty() {
        let model_key = if !selected_model.is_empty() {
            selected_model
        } else {
            controls
                .iter()
                .find(|c| c.id == "model")
                .map(|c| c.current_value.as_str())
                .unwrap_or("")
        };
        if let Some(model_specific) = snapshot.per_model_controls.get(model_key) {
            controls.retain(|c| !model_specific.iter().any(|ms| ms.id == c.id));
            controls.extend(model_specific.clone());
        }
    }

    for ctrl in &mut controls {
        if ctrl.id == "model" {
            if !selected_model.is_empty() {
                ctrl.current_value = selected_model.to_owned();
            }
        } else if let Some(val) = cfg.config_options.get(&ctrl.id)
            && !val.trim().is_empty()
        {
            ctrl.current_value = val.trim().to_owned();
        }
    }

    CachedProviderControls {
        controls,
        per_model_controls: snapshot.per_model_controls.clone(),
    }
}

async fn fetch_http_models_snapshot(
    cfg: &LlmProviderConfig,
) -> Result<CachedProviderControls, String> {
    let http = build_http_client().map_err(|e| e.to_string())?;
    let url = format!("{}/v1/models", cfg.base_url.trim_end_matches('/'));
    let mut req = http.get(&url).timeout(Duration::from_secs(10));
    if cfg.protocol == LlmWireProtocol::AnthropicMessages {
        req = req
            .header("x-api-key", cfg.api_key.trim())
            .header("anthropic-version", "2023-06-01");
    } else if !cfg.api_key.trim().is_empty() {
        req = req.bearer_auth(cfg.api_key.trim());
    }
    let resp = req
        .send()
        .await
        .map_err(|err| {
            biocompass_agent_core::llm::format_upstream_transport_error(&cfg.base_url, &err)
        })?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(biocompass_agent_core::llm::format_upstream_http_error(
            &cfg.base_url,
            status,
            &text,
        ));
    }
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    let mut choices = Vec::new();
    if let Some(arr) = body.get("data").and_then(Value::as_array) {
        for item in arr.iter().take(150) {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                let label = item
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned();
                choices.push(ProviderOptionChoice {
                    value: id.to_owned(),
                    label,
                    description: None,
                });
            }
        }
    }
    let controls = if choices.is_empty() {
        Vec::new()
    } else {
        vec![ProviderControlSpec {
            id: "model".into(),
            label: "Model".into(),
            category: "model".into(),
            current_value: cfg.model.clone(),
            options: choices,
        }]
    };
    Ok(CachedProviderControls {
        controls,
        per_model_controls: BTreeMap::new(),
    })
}

/// Resolve dynamic provider controls (`model`, `thinking`, `effort`, `mode`) for `cfg`,
/// consulting the in-memory `cache` first so repeated `GET /api/v1/llm/config` calls stay fast.
pub async fn resolve_provider_controls(
    cache: &LlmControlsCache,
    cfg: &LlmProviderConfig,
    cwd: &Path,
    force_refresh: bool,
) -> CachedProviderControls {
    let effort_override = cfg
        .config_options
        .get("effort")
        .or_else(|| cfg.config_options.get("reasoning_effort"))
        .map(String::as_str)
        .unwrap_or("");

    if cfg.protocol == LlmWireProtocol::CodexCli {
        // Codex `~/.codex/models_cache.json` is an instant local JSON read (~0.5ms).
        let snap = load_codex_controls(&cfg.model, effort_override);
        return apply_selected_control_values(&snap, cfg);
    }

    let key = backend_cache_key(cfg);
    if !force_refresh
        && let Some(cached) = cache.read().await.get(&key).cloned()
    {
        return apply_selected_control_values(&cached, cfg);
    }

    let fetched = match cfg.protocol {
        LlmWireProtocol::AcpStdio => {
            let cmd = if cfg.command.trim().is_empty() {
                "opencode"
            } else {
                cfg.command.trim()
            };
            let resolved_cmd = resolve_executable(cmd)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| cmd.to_owned());
            match probe_acp_backend(&resolved_cmd, &cfg.args, cwd).await {
                Ok(probe) => Some(CachedProviderControls {
                    controls: probe.controls,
                    per_model_controls: BTreeMap::new(),
                }),
                Err(_) => None,
            }
        }
        LlmWireProtocol::ClaudeCodeCli => {
            let cmd = if cfg.command.trim().is_empty() {
                "claude"
            } else {
                cfg.command.trim()
            };
            if let Some(bin) = resolve_executable(cmd) {
                Some(load_claude_controls(&bin, &cfg.model, effort_override).await)
            } else {
                None
            }
        }
        LlmWireProtocol::OpenAiChatCompletions | LlmWireProtocol::AnthropicMessages => {
            fetch_http_models_snapshot(cfg).await.ok()
        }
        LlmWireProtocol::CodexCli => unreachable!(),
    };

    if let Some(snap) = fetched {
        cache.write().await.insert(key, snap.clone());
        return apply_selected_control_values(&snap, cfg);
    }

    CachedProviderControls::default()
}

/// Execute a lightweight connectivity & auth verification check for `cfg` and populate
/// the provider's dynamic controls cache on success.
pub async fn test_llm_connection(
    cfg: &LlmProviderConfig,
    cwd: &Path,
    cache: Option<&LlmControlsCache>,
) -> TestLlmConnectionResponse {
    match cfg.protocol {
        LlmWireProtocol::CodexCli => {
            let cmd = if cfg.command.trim().is_empty() {
                "codex"
            } else {
                cfg.command.trim()
            };
            let Some(bin) = resolve_executable(cmd) else {
                return TestLlmConnectionResponse {
                    ok: false,
                    message: format!("Codex CLI binary `{cmd}` not found on PATH"),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                };
            };
            match run_quick_cli(&bin, &["login", "status"]).await {
                Some((ok, text)) if ok || text.to_ascii_lowercase().contains("logged in") => {
                    let effort_override = cfg
                        .config_options
                        .get("effort")
                        .or_else(|| cfg.config_options.get("reasoning_effort"))
                        .map(String::as_str)
                        .unwrap_or("");
                    let snap = load_codex_controls(&cfg.model, effort_override);
                    let available_models: Vec<String> = snap
                        .controls
                        .iter()
                        .find(|c| c.id == "model")
                        .map(|c| c.options.iter().map(|o| o.value.clone()).collect())
                        .unwrap_or_default();
                    TestLlmConnectionResponse {
                        ok: true,
                        message: format!(
                            "Codex CLI connected ({}; {} model(s))",
                            if text.is_empty() {
                                "Logged in"
                            } else {
                                text.as_str()
                            },
                            available_models.len()
                        ),
                        available_models,
                        controls: snap.controls,
                        per_model_controls: snap.per_model_controls,
                    }
                }
                Some((_, text)) => TestLlmConnectionResponse {
                    ok: false,
                    message: format!(
                        "Codex CLI is not authenticated ({text}). Run `codex login` in your terminal."
                    ),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                },
                None => TestLlmConnectionResponse {
                    ok: false,
                    message: "Timed out checking `codex login status`".into(),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                },
            }
        }
        LlmWireProtocol::ClaudeCodeCli => {
            let cmd = if cfg.command.trim().is_empty() {
                "claude"
            } else {
                cfg.command.trim()
            };
            let Some(bin) = resolve_executable(cmd) else {
                return TestLlmConnectionResponse {
                    ok: false,
                    message: format!("Claude Code CLI binary `{cmd}` not found on PATH"),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                };
            };
            match run_quick_cli(&bin, &["auth", "status"]).await {
                Some((_, text)) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text)
                        && val.get("loggedIn").and_then(Value::as_bool) == Some(true)
                    {
                        let sub = val
                            .get("subscriptionType")
                            .and_then(Value::as_str)
                            .or_else(|| val.get("authMethod").and_then(Value::as_str))
                            .unwrap_or("authenticated");
                        let effort_override = cfg
                            .config_options
                            .get("effort")
                            .map(String::as_str)
                            .unwrap_or("");
                        let snap = load_claude_controls(&bin, &cfg.model, effort_override).await;
                        if let Some(c) = cache {
                            c.write().await.insert(backend_cache_key(cfg), snap.clone());
                        }
                        let available_models = snap
                            .controls
                            .iter()
                            .find(|c| c.id == "model")
                            .map(|c| c.options.iter().map(|o| o.value.clone()).collect())
                            .unwrap_or_default();
                        return TestLlmConnectionResponse {
                            ok: true,
                            message: format!("Claude Code CLI connected ({sub})"),
                            available_models,
                            controls: snap.controls,
                            per_model_controls: snap.per_model_controls,
                        };
                    }
                    TestLlmConnectionResponse {
                        ok: false,
                        message: "Claude Code is not logged in. Run `claude auth login` in your terminal.".into(),
                        available_models: Vec::new(),
                        controls: Vec::new(),
                        per_model_controls: BTreeMap::new(),
                    }
                }
                None => TestLlmConnectionResponse {
                    ok: false,
                    message: "Timed out checking `claude auth status`".into(),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                },
            }
        }
        LlmWireProtocol::AcpStdio => {
            let cmd = if cfg.command.trim().is_empty() {
                "opencode"
            } else {
                cfg.command.trim()
            };
            let resolved_cmd = resolve_executable(cmd)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| cmd.to_owned());
            match probe_acp_backend(&resolved_cmd, &cfg.args, cwd).await {
                Ok(probe) => {
                    let snap = CachedProviderControls {
                        controls: probe.controls,
                        per_model_controls: BTreeMap::new(),
                    };
                    if let Some(c) = cache {
                        c.write().await.insert(backend_cache_key(cfg), snap.clone());
                    }
                    let applied = apply_selected_control_values(&snap, cfg);
                    TestLlmConnectionResponse {
                        ok: true,
                        message: format!(
                            "ACP handshake succeeded ({}; {} model option(s) advertised)",
                            probe.agent_name,
                            probe.available_models.len()
                        ),
                        available_models: probe.available_models,
                        controls: applied.controls,
                        per_model_controls: applied.per_model_controls,
                    }
                }
                Err(err) => TestLlmConnectionResponse {
                    ok: false,
                    message: err.to_string(),
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                },
            }
        }
        LlmWireProtocol::OpenAiChatCompletions | LlmWireProtocol::AnthropicMessages => {
            match fetch_http_models_snapshot(cfg).await {
                Ok(snap) => {
                    if let Some(c) = cache {
                        c.write().await.insert(backend_cache_key(cfg), snap.clone());
                    }
                    let available_models: Vec<String> = snap
                        .controls
                        .iter()
                        .find(|c| c.id == "model")
                        .map(|c| c.options.iter().map(|o| o.value.clone()).collect())
                        .unwrap_or_default();
                    let applied = apply_selected_control_values(&snap, cfg);
                    TestLlmConnectionResponse {
                        ok: true,
                        message: format!(
                            "Connected to {} ({} model(s) listed)",
                            cfg.base_url,
                            available_models.len()
                        ),
                        available_models,
                        controls: applied.controls,
                        per_model_controls: applied.per_model_controls,
                    }
                }
                Err(msg) => TestLlmConnectionResponse {
                    ok: false,
                    message: msg,
                    available_models: Vec::new(),
                    controls: Vec::new(),
                    per_model_controls: BTreeMap::new(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_provider_ids_for_all_supported_backends() {
        let base_cfg = LlmProviderConfig {
            protocol: LlmWireProtocol::AcpStdio,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            max_tokens: 4096,
            command: "/Users/me/.local/bin/vibe-acp".into(),
            args: Vec::new(),
            config_options: BTreeMap::new(),
        };
        assert_eq!(infer_provider_id(&base_cfg), "vibe");

        let opencode = LlmProviderConfig {
            command: "opencode".into(),
            args: vec!["acp".into()],
            ..base_cfg.clone()
        };
        assert_eq!(infer_provider_id(&opencode), "opencode");

        let mistral_http = LlmProviderConfig {
            protocol: LlmWireProtocol::OpenAiChatCompletions,
            base_url: "https://api.mistral.ai".into(),
            command: String::new(),
            args: Vec::new(),
            ..base_cfg
        };
        assert_eq!(infer_provider_id(&mistral_http), "mistral");
    }

    #[test]
    fn parse_parenthetical_choices_from_cli_help() {
        let snippet = "  --effort <level>  Effort level for the current session (low, medium, high, xhigh, max)\n";
        let choices = parse_parenthetical_tokens_after(snippet, "--effort ");
        assert_eq!(choices, vec!["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn normalize_preserves_or_replaces_config_options_properly() {
        let prev = normalize_llm_config_request(
            UpdateLlmConfigRequest {
                disconnect: false,
                protocol: Some(LlmWireProtocol::AcpStdio),
                base_url: String::new(),
                api_key: None,
                model: "mistral-medium-3.5".into(),
                max_tokens: None,
                command: "vibe-acp".into(),
                args: Vec::new(),
                config_options: Some(BTreeMap::from([("thinking".into(), "high".into())])),
            },
            None,
        )
        .expect("normalize")
        .expect("some");
        assert_eq!(prev.config_options.get("thinking").map(String::as_str), Some("high"));

        // Updating model without re-specifying config_options on same backend preserves `thinking=high`.
        let same_backend = normalize_llm_config_request(
            UpdateLlmConfigRequest {
                disconnect: false,
                protocol: Some(LlmWireProtocol::AcpStdio),
                base_url: String::new(),
                api_key: None,
                model: "devstral-small".into(),
                max_tokens: None,
                command: "vibe-acp".into(),
                args: Vec::new(),
                config_options: None,
            },
            Some(&prev),
        )
        .expect("normalize")
        .expect("some");
        assert_eq!(
            same_backend.config_options.get("thinking").map(String::as_str),
            Some("high")
        );

        // Switching to a different backend clears stale `config_options` unless explicitly sent.
        let switched = normalize_llm_config_request(
            UpdateLlmConfigRequest {
                disconnect: false,
                protocol: Some(LlmWireProtocol::CodexCli),
                base_url: String::new(),
                api_key: None,
                model: "gpt-6.1-sol".into(),
                max_tokens: None,
                command: "codex".into(),
                args: Vec::new(),
                config_options: None,
            },
            Some(&prev),
        )
        .expect("normalize")
        .expect("some");
        assert!(switched.config_options.is_empty());
    }
}
