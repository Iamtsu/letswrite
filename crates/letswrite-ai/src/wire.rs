//! Vendor-neutral wire types.
//!
//! Designed to cover Anthropic, `OpenAI`, and other provider schemas without
//! leaking any one of them. `ContentBlock` includes `Text`, `ToolUse`,
//! `ToolResult`, and `Image` from day one — adding variants later would
//! be a breaking change for downstream code.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Who is speaking in a [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Output of a tool call. Used as a follow-up message after the
    /// assistant produced a [`ContentBlock::ToolUse`].
    Tool,
}

/// One piece of a message's content. Designed multi-modal from day one.
//
// Can't derive Eq because ToolUse carries a serde_json::Value, and
// serde_json::Value::Number includes f64 (NaN ≠ NaN).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::derive_partial_eq_without_eq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// The assistant is asking to invoke a tool.
    ToolUse {
        id: String,
        name: String,
        input: JsonValue,
    },
    /// The output of a previous [`Self::ToolUse`]. Sent as a
    /// `Role::Tool` message.
    ToolResult {
        tool_use_id: String,
        content: String,
        /// `true` if the tool call itself errored. Providers signal this
        /// differently on the wire — this field normalises it.
        is_error: bool,
    },
    /// Image input. `data` is base64-encoded bytes. v1 doesn't have UI
    /// surface for this yet but the type exists so adding it later doesn't
    /// break consumers.
    Image {
        media_type: String,
        data: String,
    },
}

/// One conversation turn.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // ContentBlock isn't Eq.
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Convenience for the common case of a text-only message.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Concatenate every `Text` block's contents into a single string. Used
    /// by display code that doesn't render tool calls or images.
    pub fn flatten_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// One tool the assistant may call.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // input_schema is JSON.
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema (draft 7-ish) describing the tool's input. Providers
    /// pass this through to the model; we don't validate it here.
    pub input_schema: JsonValue,
}

/// A tool invocation produced by the model. The `arguments` are pre-parsed
/// JSON; providers that stream partial JSON (like Anthropic) accumulate
/// chunks into a complete value before emitting this.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // arguments is JSON.
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonValue,
}

/// Token-usage report from one request. Filled in piece-by-piece as the
/// provider reports cached tokens, input tokens, output tokens, etc.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u64 {
        u64::from(self.input_tokens)
            + u64::from(self.output_tokens)
            + u64::from(self.cache_creation_input_tokens)
            + u64::from(self.cache_read_input_tokens)
    }
}

/// One streaming chat request from `Agent` to `Provider`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // messages contain JSON.
pub struct ChatRequest {
    /// Vendor-specific model identifier (e.g. `claude-sonnet-4-6`).
    pub model: String,
    /// System prompt. Some providers (Anthropic) accept this as a
    /// top-level field rather than a `Role::System` message — providers
    /// map this however they need to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Optional stop sequences (the model will halt if it produces one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Arbitrary key/value pairs the agent wants threaded through for
    /// telemetry or routing. Providers ignore unknown keys.
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub metadata: JsonValue,
}

impl Default for ChatRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 4096,
            temperature: None,
            stop: Vec::new(),
            metadata: JsonValue::Null,
        }
    }
}

/// One streamed delta from `Provider` back to `Agent`. The agent translates
/// these into UI-facing [`crate::AgentEvent`]s.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::derive_partial_eq_without_eq)] // ToolUseEnd carries JSON.
pub enum ChatDelta {
    /// Plain-text content from the assistant.
    TextDelta(String),
    /// The model started a tool call. Followed by zero or more
    /// `ToolUseDelta` chunks of partial JSON arguments, then `ToolUseEnd`.
    ToolUseStart { id: String, name: String },
    /// One chunk of partial JSON for the in-progress tool call.
    ToolUseDelta(String),
    /// The current tool call's arguments are complete. The full call
    /// object is delivered here so consumers don't need to assemble JSON.
    ToolUseEnd { call: ToolCall },
    /// The message is finished; carries the final usage report.
    MessageStop { usage: Usage },
    /// The provider reported a recoverable error mid-stream (e.g.
    /// rate-limited at byte N). Aborts the stream — the agent decides
    /// whether to retry.
    Error(crate::ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_text_helper() {
        let m = Message::text(Role::User, "hi");
        assert_eq!(m.role, Role::User);
        assert_eq!(m.content.len(), 1);
        assert_eq!(m.flatten_text(), "hi");
    }

    #[test]
    fn flatten_text_skips_tool_blocks() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: "before".into() },
                ContentBlock::ToolUse {
                    id: "x".into(),
                    name: "noop".into(),
                    input: JsonValue::Null,
                },
                ContentBlock::Text { text: "after".into() },
            ],
        };
        assert_eq!(m.flatten_text(), "beforeafter");
    }

    #[test]
    fn chat_request_roundtrips_through_json() {
        let req = ChatRequest {
            model: "claude-sonnet-4-6".into(),
            system: Some("you are helpful".into()),
            messages: vec![Message::text(Role::User, "test")],
            max_tokens: 1024,
            ..Default::default()
        };
        let s = serde_json::to_string(&req).unwrap();
        let parsed: ChatRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn usage_total_sums_all_buckets() {
        let u = Usage {
            input_tokens: 100,
            output_tokens: 200,
            cache_creation_input_tokens: 50,
            cache_read_input_tokens: 10,
        };
        assert_eq!(u.total(), 360);
    }
}
