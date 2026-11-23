//! `biocompass-mcp-core`: Pure Model Context Protocol (MCP) JSON-RPC 2.0 envelope,
//! version negotiation, tool definitions/annotations, error codes, and `ToolHost` trait.
//!
//! Intentionally depends ONLY on `biocompass-core-traits`, `async-trait`, `serde`, and `serde_json`
//! (zero dependency on `biocompass-agent-core` or any HTTP framework) so both `biocompass-agent-core`
//! (loopback CLI/ACP bridge) and `compass-hub` (enterprise hub MCP endpoint) share one spec-compliant
//! implementation.

use async_trait::async_trait;
use biocompass_core_traits::{CompassError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

/// JSON-RPC version string required by JSON-RPC 2.0 and MCP.
pub const JSONRPC_VERSION: &str = "2.0";

/// Explicit list of MCP protocol versions supported by BioCompass (newest preferred first).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05"];

/// Default (latest) MCP protocol version advertised when a client omits or requests an unsupported version.
pub const DEFAULT_PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[0];

/// HTTP header name carrying the Streamable HTTP MCP session identifier (`H2`).
pub const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";

/// Return `true` when `version` is in [`SUPPORTED_PROTOCOL_VERSIONS`].
pub fn is_supported_protocol_version(version: &str) -> bool {
    let trimmed = version.trim();
    SUPPORTED_PROTOCOL_VERSIONS.contains(&trimmed)
}

/// Negotiate an MCP protocol version against [`SUPPORTED_PROTOCOL_VERSIONS`].
///
/// Per the MCP lifecycle specification, if the client's requested `protocolVersion` is explicitly
/// supported, the server echoes that version; otherwise the server replies with
/// [`DEFAULT_PROTOCOL_VERSION`] (the latest version it supports) so the client can decide whether
/// to continue or close the session.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    if let Some(req) = requested.map(str::trim) {
        for &supported in SUPPORTED_PROTOCOL_VERSIONS {
            if supported == req {
                return supported;
            }
        }
    }
    DEFAULT_PROTOCOL_VERSION
}

/// Standard JSON-RPC 2.0 and BioCompass MCP error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i64)]
pub enum McpErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    // Server-defined JSON-RPC error codes (-32000..-32099)
    Unauthorized = -32001,
    PaymentRequired = -32002,
    Forbidden = -32003,
    NotFound = -32004,
    Conflict = -32009,
    RateLimited = -32029,
    SessionRequired = -32030,
    SessionNotFound = -32031,
}

impl McpErrorCode {
    /// Numeric JSON-RPC 2.0 error code.
    pub const fn as_i64(self) -> i64 {
        self as i64
    }

    /// Map a domain [`CompassError`] to the most specific JSON-RPC / MCP error code.
    pub fn from_compass_error(err: &CompassError) -> Self {
        match err.error_code() {
            ErrorCode::Unauthenticated
            | ErrorCode::AgentCredentialExpired
            | ErrorCode::AgentCredentialRevoked => Self::Unauthorized,
            ErrorCode::InsufficientFunds
            | ErrorCode::DailyActBreakerExceeded
            | ErrorCode::AgentCeilingExceeded
            | ErrorCode::PaymentError => Self::PaymentRequired,
            ErrorCode::OffListModule
            | ErrorCode::OffListCatalog
            | ErrorCode::ChildModuleNotAllowed
            | ErrorCode::AccessDenied => Self::Forbidden,
            ErrorCode::NotFound => Self::NotFound,
            ErrorCode::ValidationError | ErrorCode::InputLimitExceeded => Self::InvalidParams,
            ErrorCode::IntegrityViolation
            | ErrorCode::UiBundleTampered
            | ErrorCode::DocVersionConflict
            | ErrorCode::IdempotencyConflict => Self::Conflict,
            ErrorCode::RateLimited | ErrorCode::RowBudgetExceeded => Self::RateLimited,
            ErrorCode::StorageError
            | ErrorCode::ContainerEngineError
            | ErrorCode::ProtocolError
            | ErrorCode::UpstreamLlmError
            | ErrorCode::InternalError => Self::InternalError,
        }
    }
}

impl From<McpErrorCode> for i64 {
    fn from(code: McpErrorCode) -> Self {
        code.as_i64()
    }
}

impl fmt::Display for McpErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_i64())
    }
}

impl Serialize for McpErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.as_i64())
    }
}

impl<'de> Deserialize<'de> for McpErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = i64::deserialize(deserializer)?;
        match code {
            -32700 => Ok(Self::ParseError),
            -32600 => Ok(Self::InvalidRequest),
            -32601 => Ok(Self::MethodNotFound),
            -32602 => Ok(Self::InvalidParams),
            -32603 => Ok(Self::InternalError),
            -32001 => Ok(Self::Unauthorized),
            -32002 => Ok(Self::PaymentRequired),
            -32003 => Ok(Self::Forbidden),
            -32004 => Ok(Self::NotFound),
            -32009 => Ok(Self::Conflict),
            -32029 => Ok(Self::RateLimited),
            -32030 => Ok(Self::SessionRequired),
            -32031 => Ok(Self::SessionNotFound),
            other => Err(serde::de::Error::custom(format!(
                "unsupported MCP error code {other}"
            ))),
        }
    }
}

/// Incoming JSON-RPC 2.0 request or notification envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    /// Returns `true` when this envelope is a JSON-RPC notification (no `id`).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Validate JSON-RPC 2.0 version tag (when supplied) and non-empty method name.
    pub fn validate(&self) -> Result<(), JsonRpcErrorObject> {
        if let Some(version) = self.jsonrpc.as_deref()
            && version != JSONRPC_VERSION
        {
            return Err(JsonRpcErrorObject::new(
                McpErrorCode::InvalidRequest,
                format!("unsupported jsonrpc version '{version}'; expected '{JSONRPC_VERSION}'"),
            ));
        }
        if self.method.trim().is_empty() {
            return Err(JsonRpcErrorObject::new(
                McpErrorCode::InvalidRequest,
                "JSON-RPC method must be non-empty",
            ));
        }
        Ok(())
    }
}

/// Structured JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcErrorObject {
    pub fn new(code: McpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_i64(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: McpErrorCode, message: impl Into<String>, data: Value) -> Self {
        Self {
            code: code.as_i64(),
            message: message.into(),
            data: Some(data),
        }
    }

    /// Convert a [`CompassError`] into a JSON-RPC error object carrying its stable `ErrorCode`.
    pub fn from_compass_error(err: &CompassError) -> Self {
        Self {
            code: McpErrorCode::from_compass_error(err).as_i64(),
            message: err.user_message(),
            data: Some(json!({
                "error_code": err.code(),
                "hint": err.hint(),
            })),
        }
    }
}

/// Outgoing JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObject>,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: JsonRpcErrorObject) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn err_code(id: Value, code: McpErrorCode, message: impl Into<String>) -> Self {
        Self::err(id, JsonRpcErrorObject::new(code, message))
    }
}

/// Server metadata returned inside an MCP `initialize` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Incoming parameters for the MCP `initialize` method.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub client_info: Option<Value>,
}

/// Tool capability declaration inside [`ServerCapabilities`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    pub list_changed: bool,
}

/// MCP server capability declaration returned by `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

/// Result payload returned by `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// MCP tool behavioral annotations (`2025-03-26` specification).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

impl ToolAnnotations {
    /// Preset for read-only, non-destructive, idempotent, closed-world tools.
    pub const fn read_only() -> Self {
        Self {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        }
    }

    /// Preset for non-destructive mutating/action tools within BioCompass.
    pub const fn mutating(idempotent: bool) -> Self {
        Self {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(idempotent),
            open_world_hint: Some(false),
        }
    }
}

/// Single tool entry returned in MCP `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Single content item returned inside [`CallToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
}

/// Parameters for MCP `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Result payload returned by MCP `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl CallToolResult {
    /// Construct a successful text tool result (`isError: false`).
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: false,
            structured_content: None,
        }
    }

    /// Construct a successful JSON-encoded text tool result (`isError: false`).
    pub fn json_value(value: &Value) -> Result<Self, serde_json::Error> {
        let text = serde_json::to_string(value)?;
        Ok(Self {
            content: vec![ToolContent::Text { text }],
            is_error: false,
            structured_content: None,
        })
    }

    /// Construct a tool-level error text result (`isError: true`).
    pub fn error_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: true,
            structured_content: None,
        }
    }

    /// Construct a structured tool-level error carrying a BioCompass stable [`ErrorCode`].
    pub fn from_compass_error(err: &CompassError) -> Self {
        let payload = json!({
            "code": err.code(),
            "error": err.user_message(),
            "hint": err.hint(),
        });
        Self::error_text(serde_json::to_string(&payload).unwrap_or_else(|_| err.user_message()))
    }
}

/// Host backend supplying server identity, tool listing, and tool execution for an MCP endpoint.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Server name and version advertised in `initialize`.
    fn server_info(&self) -> ServerInfo;

    /// List all tools currently authorized for this caller/session.
    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, JsonRpcErrorObject>;

    /// Invoke one named tool with JSON arguments.
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, JsonRpcErrorObject>;
}

/// Outcome of dispatching one JSON-RPC 2.0 MCP message through [`dispatch_jsonrpc`].
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchOutcome {
    /// JSON-RPC notification (no `id`): HTTP transports respond with `202 Accepted` and no body.
    Notification,
    /// JSON-RPC response: HTTP transports respond with `200 OK` and JSON body.
    /// When `initialized_protocol_version` is `Some(ver)`, the request was a successful `initialize`,
    /// allowing stateful transports (such as `compass-hub`) to create a session and emit `Mcp-Session-Id`.
    Response {
        response: JsonRpcResponse,
        initialized_protocol_version: Option<String>,
    },
}

/// Dispatch one incoming JSON-RPC 2.0 MCP request or notification against a [`ToolHost`].
pub async fn dispatch_jsonrpc<H: ToolHost + ?Sized>(
    host: &H,
    req: JsonRpcRequest,
) -> DispatchOutcome {
    if let Err(err) = req.validate() {
        let Some(id) = req.id else {
            return DispatchOutcome::Notification;
        };
        return DispatchOutcome::Response {
            response: JsonRpcResponse::err(id, err),
            initialized_protocol_version: None,
        };
    }

    let Some(id) = req.id else {
        return DispatchOutcome::Notification;
    };

    match req.method.as_str() {
        "initialize" => {
            let requested = req
                .params
                .get("protocolVersion")
                .and_then(Value::as_str);
            let negotiated = negotiate_protocol_version(requested);
            let init_result = InitializeResult {
                protocol_version: negotiated.to_owned(),
                capabilities: ServerCapabilities {
                    tools: ToolsCapability {
                        list_changed: false,
                    },
                },
                server_info: host.server_info(),
            };
            let encoded = match serde_json::to_value(&init_result) {
                Ok(v) => v,
                Err(e) => {
                    return DispatchOutcome::Response {
                        response: JsonRpcResponse::err_code(
                            id,
                            McpErrorCode::InternalError,
                            format!("encode initialize result: {e}"),
                        ),
                        initialized_protocol_version: None,
                    };
                }
            };
            DispatchOutcome::Response {
                response: JsonRpcResponse::ok(id, encoded),
                initialized_protocol_version: Some(negotiated.to_owned()),
            }
        }
        "ping" => DispatchOutcome::Response {
            response: JsonRpcResponse::ok(id, json!({})),
            initialized_protocol_version: None,
        },
        "tools/list" => match host.list_tools().await {
            Ok(tools) => match serde_json::to_value(&tools) {
                Ok(val) => DispatchOutcome::Response {
                    response: JsonRpcResponse::ok(id, json!({ "tools": val })),
                    initialized_protocol_version: None,
                },
                Err(e) => DispatchOutcome::Response {
                    response: JsonRpcResponse::err_code(
                        id,
                        McpErrorCode::InternalError,
                        format!("encode tools/list: {e}"),
                    ),
                    initialized_protocol_version: None,
                },
            },
            Err(err) => DispatchOutcome::Response {
                response: JsonRpcResponse::err(id, err),
                initialized_protocol_version: None,
            },
        },
        "tools/call" => {
            let Some(name) = req
                .params
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return DispatchOutcome::Response {
                    response: JsonRpcResponse::err_code(
                        id,
                        McpErrorCode::InvalidParams,
                        "tools/call requires non-empty params.name",
                    ),
                    initialized_protocol_version: None,
                };
            };
            let arguments = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return DispatchOutcome::Response {
                    response: JsonRpcResponse::err_code(
                        id,
                        McpErrorCode::InvalidParams,
                        "tools/call params.arguments must be a JSON object",
                    ),
                    initialized_protocol_version: None,
                };
            }
            match host.call_tool(name, arguments).await {
                Ok(result) => match serde_json::to_value(&result) {
                    Ok(val) => DispatchOutcome::Response {
                        response: JsonRpcResponse::ok(id, val),
                        initialized_protocol_version: None,
                    },
                    Err(e) => DispatchOutcome::Response {
                        response: JsonRpcResponse::err_code(
                            id,
                            McpErrorCode::InternalError,
                            format!("encode tools/call result: {e}"),
                        ),
                        initialized_protocol_version: None,
                    },
                },
                Err(err) => DispatchOutcome::Response {
                    response: JsonRpcResponse::err(id, err),
                    initialized_protocol_version: None,
                },
            }
        }
        other => DispatchOutcome::Response {
            response: JsonRpcResponse::err_code(
                id,
                McpErrorCode::MethodNotFound,
                format!("method not found: {other}"),
            ),
            initialized_protocol_version: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubHost;

    #[async_trait]
    impl ToolHost for StubHost {
        fn server_info(&self) -> ServerInfo {
            ServerInfo {
                name: "biocompass-test-mcp".into(),
                version: "0.2.0".into(),
            }
        }

        async fn list_tools(&self) -> Result<Vec<ToolDefinition>, JsonRpcErrorObject> {
            Ok(vec![ToolDefinition {
                name: "list_modules".into(),
                description: "List modules".into(),
                input_schema: json!({"type": "object"}),
                annotations: Some(ToolAnnotations::read_only()),
            }])
        }

        async fn call_tool(
            &self,
            name: &str,
            arguments: Value,
        ) -> Result<CallToolResult, JsonRpcErrorObject> {
            if name == "list_modules" {
                Ok(CallToolResult::text(
                    serde_json::to_string(&json!({"ok": true, "args": arguments})).unwrap(),
                ))
            } else {
                Ok(CallToolResult::from_compass_error(
                    &CompassError::off_list_module(format!("tool '{name}' is off-list")),
                ))
            }
        }
    }

    #[test]
    fn negotiates_explicit_supported_versions_only() {
        assert_eq!(SUPPORTED_PROTOCOL_VERSIONS, &["2025-03-26", "2024-11-05"]);
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_protocol_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(
            negotiate_protocol_version(Some("2025-99-99")),
            DEFAULT_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), DEFAULT_PROTOCOL_VERSION);
        assert!(is_supported_protocol_version("2025-03-26"));
        assert!(is_supported_protocol_version("2024-11-05"));
        assert!(!is_supported_protocol_version("2024-01-01"));
    }

    #[test]
    fn maps_compass_errors_to_stable_mcp_error_codes() {
        assert_eq!(
            McpErrorCode::from_compass_error(&CompassError::insufficient_funds("need 5 EUR")),
            McpErrorCode::PaymentRequired
        );
        assert_eq!(
            McpErrorCode::from_compass_error(&CompassError::agent_ceiling_exceeded("cap")),
            McpErrorCode::PaymentRequired
        );
        assert_eq!(
            McpErrorCode::from_compass_error(&CompassError::off_list_module("denied")),
            McpErrorCode::Forbidden
        );
        assert_eq!(
            McpErrorCode::from_compass_error(&CompassError::agent_credential_expired("expired")),
            McpErrorCode::Unauthorized
        );
        assert_eq!(
            McpErrorCode::from_compass_error(&CompassError::doc_version_conflict("stale")),
            McpErrorCode::Conflict
        );
    }

    #[tokio::test]
    async fn dispatches_initialize_list_call_notifications_and_errors() {
        let host = StubHost;

        let init = dispatch_jsonrpc(
            &host,
            JsonRpcRequest {
                jsonrpc: Some("2.0".into()),
                id: Some(json!(1)),
                method: "initialize".into(),
                params: json!({"protocolVersion": "2024-11-05"}),
            },
        )
        .await;
        match init {
            DispatchOutcome::Response {
                response,
                initialized_protocol_version,
            } => {
                assert_eq!(initialized_protocol_version.as_deref(), Some("2024-11-05"));
                assert_eq!(response.result.unwrap()["protocolVersion"], "2024-11-05");
            }
            DispatchOutcome::Notification => panic!("expected response"),
        }

        let notif = dispatch_jsonrpc(
            &host,
            JsonRpcRequest {
                jsonrpc: Some("2.0".into()),
                id: None,
                method: "notifications/initialized".into(),
                params: json!({}),
            },
        )
        .await;
        assert_eq!(notif, DispatchOutcome::Notification);

        let list = dispatch_jsonrpc(
            &host,
            JsonRpcRequest {
                jsonrpc: Some("2.0".into()),
                id: Some(json!(2)),
                method: "tools/list".into(),
                params: json!({}),
            },
        )
        .await;
        let DispatchOutcome::Response { response, .. } = list else {
            panic!("expected response");
        };
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
        assert_eq!(tools[0]["annotations"]["idempotentHint"], true);

        let bad_version = dispatch_jsonrpc(
            &host,
            JsonRpcRequest {
                jsonrpc: Some("1.0".into()),
                id: Some(json!(3)),
                method: "ping".into(),
                params: json!({}),
            },
        )
        .await;
        let DispatchOutcome::Response { response, .. } = bad_version else {
            panic!("expected error response");
        };
        assert_eq!(
            response.error.unwrap().code,
            McpErrorCode::InvalidRequest.as_i64()
        );

        let missing_name = dispatch_jsonrpc(
            &host,
            JsonRpcRequest {
                jsonrpc: Some("2.0".into()),
                id: Some(json!(4)),
                method: "tools/call".into(),
                params: json!({}),
            },
        )
        .await;
        let DispatchOutcome::Response { response, .. } = missing_name else {
            panic!("expected error response");
        };
        assert_eq!(
            response.error.unwrap().code,
            McpErrorCode::InvalidParams.as_i64()
        );
    }
}
