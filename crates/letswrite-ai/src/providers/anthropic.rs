//! Anthropic Messages API provider.
//!
//! - Endpoint: `POST {base_url}/v1/messages` with `stream: true`.
//! - Auth header: `x-api-key: <key>` (Anthropic doesn't use Bearer here).
//! - Version header: `anthropic-version: 2023-06-01` (the date is a contract,
//!   not a deploy timestamp — see Anthropic API versioning docs).
//! - Default `base_url` is `https://api.anthropic.com`. Tests and proxies
//!   can override via the `ANTHROPIC_BASE_URL` env var or
//!   [`AnthropicProvider::with_base_url`].
//!
//! The streaming protocol is documented at
//! <https://docs.anthropic.com/en/api/messages-streaming>. We handle:
//! `message_start`, `content_block_start`, `content_block_delta` (text +
//! `input_json`), `content_block_stop`, `message_delta`, `message_stop`,
//! `ping`, and `error`.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::credentials::CredentialStore;
use crate::error::ProviderError;
use crate::provider::{Capabilities, ChatStream, ModelInfo, Provider};
use crate::wire::{
    ChatDelta, ChatRequest, ContentBlock, Message, Role, ToolCall, Usage,
};

/// Provider name as registered with [`crate::ProviderRegistry`]. Also the
/// credential-store key prefix (we ask for `{NAME}-api-key`).
pub const NAME: &str = "anthropic";

/// Env override for the API base URL. Used in tests with a `wiremock`
/// server, and by users behind corporate proxies.
const ENV_BASE_URL: &str = "ANTHROPIC_BASE_URL";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const API_KEY_CREDENTIAL: &str = "anthropic-api-key";

/// Anthropic provider for the letswrite agent layer.
#[derive(Clone)]
pub struct AnthropicProvider {
    http: Client,
    base_url: String,
    credentials: Arc<dyn CredentialStore>,
    models: Vec<ModelInfo>,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(credentials: Arc<dyn CredentialStore>) -> Result<Self, ProviderError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ProviderError::Transport { message: e.to_string() })?;
        let base_url =
            std::env::var(ENV_BASE_URL).unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Ok(Self {
            http,
            base_url,
            credentials,
            models: default_models(),
        })
    }

    /// Override the base URL (primarily for tests and proxies).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn fetch_api_key(&self) -> Result<String, ProviderError> {
        match self.credentials.get(API_KEY_CREDENTIAL) {
            Ok(Some(key)) => Ok(key),
            Ok(None) => Err(ProviderError::Auth),
            Err(e) => Err(ProviderError::Transport { message: e.to_string() }),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tool_use: true,
            vision: true,
            max_context_tokens: 200_000,
        }
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<ChatStream, ProviderError> {
        let api_key = self.fetch_api_key()?;
        let body = AnthropicRequest::from_chat(&request);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&api_key)
                .map_err(|_| ProviderError::Auth)?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| map_reqwest_err(&e))?;

        let status = response.status();
        if !status.is_success() {
            return Err(map_http_status(status, &response));
        }

        let (tx, rx) = mpsc::unbounded_channel::<Result<ChatDelta, ProviderError>>();
        let cancel_for_task = cancel;
        tokio::spawn(async move {
            let sse_stream = response.bytes_stream().eventsource();
            tokio::pin!(sse_stream);
            let mut tool_accums: Vec<Option<ToolAccum>> = Vec::new();
            let mut final_usage = Usage::default();
            loop {
                tokio::select! {
                    biased;
                    () = cancel_for_task.cancelled() => {
                        let _ = tx.send(Err(ProviderError::Cancelled));
                        break;
                    }
                    next = sse_stream.next() => {
                        let Some(event) = next else { break; };
                        let event = match event {
                            Ok(e) => e,
                            Err(e) => {
                                let _ = tx.send(Err(ProviderError::Transport {
                                    message: e.to_string(),
                                }));
                                break;
                            }
                        };
                        match handle_sse(&event.event, &event.data, &mut tool_accums, &mut final_usage) {
                            DeltaOutcome::Emit(delta) => {
                                if tx.send(Ok(delta)).is_err() {
                                    break;
                                }
                            }
                            DeltaOutcome::Skip => {}
                            DeltaOutcome::MessageStop => {
                                let _ = tx.send(Ok(ChatDelta::MessageStop {
                                    usage: final_usage,
                                }));
                                break;
                            }
                            DeltaOutcome::Error(e) => {
                                let _ = tx.send(Err(e));
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

fn default_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "claude-opus-4-7".into(),
            display_name: "Claude Opus 4.7".into(),
            max_output_tokens: 32_000,
        },
        ModelInfo {
            id: "claude-sonnet-4-6".into(),
            display_name: "Claude Sonnet 4.6".into(),
            max_output_tokens: 64_000,
        },
        ModelInfo {
            id: "claude-haiku-4-5-20251001".into(),
            display_name: "Claude Haiku 4.5".into(),
            max_output_tokens: 32_000,
        },
    ]
}

// ---------------------------------------------------------------------------
// Wire mapping (abstraction → Anthropic JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    stream: bool,
}

impl AnthropicRequest {
    fn from_chat(req: &ChatRequest) -> Self {
        Self {
            model: req.model.clone(),
            max_tokens: req.max_tokens,
            system: req.system.clone(),
            messages: req.messages.iter().map(AnthropicMessage::from_wire).collect(),
            tools: req.tools.iter().map(AnthropicTool::from_wire).collect(),
            temperature: req.temperature,
            stop_sequences: req.stop.clone(),
            stream: true,
        }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContent>,
}

impl AnthropicMessage {
    fn from_wire(m: &Message) -> Self {
        // Anthropic accepts `user` and `assistant`. System lives at the
        // top level; tool results come back as user-role messages with
        // a tool_result content block.
        let role = match m.role {
            Role::User | Role::Tool | Role::System => "user",
            Role::Assistant => "assistant",
        };
        Self {
            role: role.to_owned(),
            content: m.content.iter().map(AnthropicContent::from_wire).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContent {
    Text { text: String },
    ToolUse { id: String, name: String, input: JsonValue },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Image { source: AnthropicImageSource },
}

#[derive(Debug, Serialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
}

impl AnthropicContent {
    fn from_wire(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => Self::Text { text: text.clone() },
            ContentBlock::ToolUse { id, name, input } => Self::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ContentBlock::ToolResult { tool_use_id, content, is_error } => Self::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            ContentBlock::Image { media_type, data } => Self::Image {
                source: AnthropicImageSource {
                    kind: "base64",
                    media_type: media_type.clone(),
                    data: data.clone(),
                },
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: JsonValue,
}

impl AnthropicTool {
    fn from_wire(t: &crate::wire::Tool) -> Self {
        Self {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// SSE handling
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ToolAccum {
    id: String,
    name: String,
    partial_args: String,
}

enum DeltaOutcome {
    Emit(ChatDelta),
    Skip,
    MessageStop,
    Error(ProviderError),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStartPayload },
    #[serde(rename = "content_block_start")]
    ContentBlockStart { index: usize, content_block: ContentBlockStart },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: BlockDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        #[allow(dead_code)]
        delta: serde_json::Value,
        #[serde(default)]
        usage: Option<UsageDelta>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicErrorPayload },
}

#[derive(Debug, Deserialize)]
struct MessageStartPayload {
    #[serde(default)]
    usage: Option<UsageDelta>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlockStart {
    #[serde(rename = "text")]
    Text {
        #[allow(dead_code)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum BlockDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

// Field names must match Anthropic's JSON exactly, so we accept the
// `_tokens` postfix and silence the clippy structural lint.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Deserialize, Default)]
struct UsageDelta {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorPayload {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

fn handle_sse(
    event_name: &str,
    data: &str,
    tool_accums: &mut Vec<Option<ToolAccum>>,
    usage: &mut Usage,
) -> DeltaOutcome {
    // Anthropic's `ping` events come with an empty body; serde_json would
    // fail to parse. Short-circuit.
    if event_name == "ping" {
        return DeltaOutcome::Skip;
    }
    let parsed: AnthropicEvent = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(e) => {
            return DeltaOutcome::Error(ProviderError::Protocol {
                message: format!("unparseable SSE event '{event_name}': {e}"),
            });
        }
    };
    match parsed {
        AnthropicEvent::MessageStart { message } => {
            if let Some(u) = message.usage {
                apply_usage_delta(usage, &u);
            }
            DeltaOutcome::Skip
        }
        AnthropicEvent::ContentBlockStart { index, content_block } => {
            ensure_accum_slot(tool_accums, index);
            match content_block {
                ContentBlockStart::Text { .. } => DeltaOutcome::Skip,
                ContentBlockStart::ToolUse { id, name } => {
                    tool_accums[index] = Some(ToolAccum {
                        id: id.clone(),
                        name: name.clone(),
                        partial_args: String::new(),
                    });
                    DeltaOutcome::Emit(ChatDelta::ToolUseStart { id, name })
                }
            }
        }
        AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
            BlockDelta::TextDelta { text } => DeltaOutcome::Emit(ChatDelta::TextDelta(text)),
            BlockDelta::InputJsonDelta { partial_json } => {
                if let Some(slot) = tool_accums.get_mut(index).and_then(Option::as_mut) {
                    slot.partial_args.push_str(&partial_json);
                }
                DeltaOutcome::Emit(ChatDelta::ToolUseDelta(partial_json))
            }
        },
        AnthropicEvent::ContentBlockStop { index } => {
            if let Some(slot) = tool_accums.get_mut(index).and_then(Option::take) {
                let arguments: JsonValue = if slot.partial_args.is_empty() {
                    JsonValue::Object(serde_json::Map::default())
                } else {
                    match serde_json::from_str(&slot.partial_args) {
                        Ok(v) => v,
                        Err(e) => {
                            return DeltaOutcome::Error(ProviderError::Protocol {
                                message: format!(
                                    "tool_use input_json was not valid JSON: {e}"
                                ),
                            });
                        }
                    }
                };
                let call = ToolCall {
                    id: slot.id,
                    name: slot.name,
                    arguments,
                };
                return DeltaOutcome::Emit(ChatDelta::ToolUseEnd { call });
            }
            DeltaOutcome::Skip
        }
        AnthropicEvent::MessageDelta { usage: u, .. } => {
            if let Some(u) = u {
                apply_usage_delta(usage, &u);
            }
            DeltaOutcome::Skip
        }
        AnthropicEvent::MessageStop => DeltaOutcome::MessageStop,
        AnthropicEvent::Ping => DeltaOutcome::Skip,
        AnthropicEvent::Error { error } => DeltaOutcome::Error(map_event_error(&error)),
    }
}

fn ensure_accum_slot(slots: &mut Vec<Option<ToolAccum>>, index: usize) {
    if slots.len() <= index {
        slots.resize_with(index + 1, || None);
    }
}

const fn apply_usage_delta(usage: &mut Usage, delta: &UsageDelta) {
    if let Some(v) = delta.input_tokens {
        usage.input_tokens = v;
    }
    if let Some(v) = delta.output_tokens {
        usage.output_tokens = v;
    }
    if let Some(v) = delta.cache_creation_input_tokens {
        usage.cache_creation_input_tokens = v;
    }
    if let Some(v) = delta.cache_read_input_tokens {
        usage.cache_read_input_tokens = v;
    }
}

fn map_event_error(err: &AnthropicErrorPayload) -> ProviderError {
    match err.kind.as_str() {
        "overloaded_error" | "api_error" => ProviderError::Transient {
            message: err.message.clone(),
        },
        "rate_limit_error" => ProviderError::RateLimited { after: None },
        "authentication_error" | "permission_error" => ProviderError::Auth,
        _ => ProviderError::Protocol { message: err.message.clone() },
    }
}

// ---------------------------------------------------------------------------
// HTTP error mapping
// ---------------------------------------------------------------------------

fn map_reqwest_err(e: &reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Transient { message: "request timed out".into() }
    } else {
        ProviderError::Transport { message: e.to_string() }
    }
}

/// Synchronous — we don't read the body here because we only have a
/// `&Response`; the status + headers are usually enough to classify.
fn map_http_status(status: StatusCode, response: &reqwest::Response) -> ProviderError {
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);
    let body_message = format!("HTTP {} from Anthropic", status.as_u16());
    match status.as_u16() {
        401 | 403 => ProviderError::Auth,
        429 => ProviderError::RateLimited { after: retry_after },
        400..=499 => ProviderError::InvalidRequest { message: body_message },
        500..=599 => ProviderError::Transient { message: body_message },
        _ => ProviderError::Protocol { message: body_message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::InMemoryCredentialStore;
    use crate::wire::{ChatRequest, Message, Role};

    #[test]
    fn from_wire_maps_user_and_assistant_messages() {
        let req = ChatRequest {
            model: "claude-sonnet-4-6".into(),
            system: Some("you are helpful".into()),
            messages: vec![
                Message::text(Role::User, "hi"),
                Message::text(Role::Assistant, "hello"),
            ],
            max_tokens: 1024,
            ..Default::default()
        };
        let a = AnthropicRequest::from_chat(&req);
        assert_eq!(a.system.as_deref(), Some("you are helpful"));
        assert_eq!(a.messages.len(), 2);
        assert_eq!(a.messages[0].role, "user");
        assert_eq!(a.messages[1].role, "assistant");
        assert!(a.stream);
    }

    #[test]
    fn from_wire_collapses_tool_and_system_to_user() {
        let req = ChatRequest {
            model: "claude-sonnet-4-6".into(),
            messages: vec![
                Message::text(Role::System, "out-of-band system"),
                Message::text(Role::Tool, "tool output"),
            ],
            max_tokens: 1,
            ..Default::default()
        };
        let a = AnthropicRequest::from_chat(&req);
        assert!(a.messages.iter().all(|m| m.role == "user"));
    }

    #[test]
    fn handle_sse_text_delta_emits_chunk() {
        let mut accums = Vec::new();
        let mut usage = Usage::default();
        let outcome = handle_sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            &mut accums,
            &mut usage,
        );
        match outcome {
            DeltaOutcome::Emit(ChatDelta::TextDelta(t)) => assert_eq!(t, "hi"),
            _ => panic!("expected text delta"),
        }
    }

    #[test]
    fn handle_sse_tool_use_round_trip() {
        let mut accums = Vec::new();
        let mut usage = Usage::default();
        // start
        let _ = handle_sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"x1","name":"echo"}}"#,
            &mut accums,
            &mut usage,
        );
        // partial json
        let _ = handle_sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"key\":"}}"#,
            &mut accums,
            &mut usage,
        );
        let _ = handle_sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"v\"}"}}"#,
            &mut accums,
            &mut usage,
        );
        // stop -> ToolUseEnd
        let outcome = handle_sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
            &mut accums,
            &mut usage,
        );
        match outcome {
            DeltaOutcome::Emit(ChatDelta::ToolUseEnd { call }) => {
                assert_eq!(call.id, "x1");
                assert_eq!(call.name, "echo");
                assert_eq!(call.arguments, serde_json::json!({"key": "v"}));
            }
            _ => panic!("expected ToolUseEnd"),
        }
    }

    #[test]
    fn handle_sse_message_stop_emits_terminal() {
        let mut accums = Vec::new();
        let mut usage = Usage::default();
        let outcome = handle_sse(
            "message_stop",
            r#"{"type":"message_stop"}"#,
            &mut accums,
            &mut usage,
        );
        assert!(matches!(outcome, DeltaOutcome::MessageStop));
    }

    #[test]
    fn handle_sse_ping_is_ignored() {
        let mut accums = Vec::new();
        let mut usage = Usage::default();
        let outcome = handle_sse("ping", "", &mut accums, &mut usage);
        assert!(matches!(outcome, DeltaOutcome::Skip));
    }

    #[test]
    fn handle_sse_error_event_maps_to_provider_error() {
        let mut accums = Vec::new();
        let mut usage = Usage::default();
        let outcome = handle_sse(
            "error",
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            &mut accums,
            &mut usage,
        );
        match outcome {
            DeltaOutcome::Error(ProviderError::RateLimited { .. }) => {}
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn fetch_api_key_returns_auth_error_when_missing() {
        let creds = Arc::new(InMemoryCredentialStore::new());
        let provider = AnthropicProvider::new(creds).unwrap();
        let err = provider.fetch_api_key().unwrap_err();
        assert!(matches!(err, ProviderError::Auth));
    }

    #[test]
    fn fetch_api_key_returns_value_when_set() {
        let creds = Arc::new(InMemoryCredentialStore::new());
        creds.set(API_KEY_CREDENTIAL, "sk-test-secret").unwrap();
        let provider = AnthropicProvider::new(creds).unwrap();
        let key = provider.fetch_api_key().unwrap();
        assert_eq!(key, "sk-test-secret");
    }

    impl std::fmt::Debug for DeltaOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Emit(d) => write!(f, "Emit({d:?})"),
                Self::Skip => f.write_str("Skip"),
                Self::MessageStop => f.write_str("MessageStop"),
                Self::Error(e) => write!(f, "Error({e:?})"),
            }
        }
    }
}
