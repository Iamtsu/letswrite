//! AI assistant abstraction for letswrite.
//!
//! Two-tier design (see `docs/tasks.md` #12 and the project memory file
//! `project_ai_abstraction.md` for the contract):
//!
//! - [`Provider`](provider::Provider) is the low-level, vendor-specific
//!   contract. One implementation per backend (Anthropic, `OpenAI`, local
//!   `llama.cpp`, …). Provider impls live in `letswrite_ai::providers::*`.
//! - [`Agent`](agent::Agent) is the high-level, UI-facing contract. It
//!   wraps a Provider with conversation state and context-assembly
//!   strategy. The UI only ever depends on `Agent`.
//!
//! Hard rules:
//!
//! 1. UI code may NOT import provider impls — talks to `Agent` only.
//! 2. Vendor-specific concerns (SSE event names, prompt caching headers,
//!    model IDs) stay inside the provider module. They never leak into
//!    `Agent`, [`AssistantContext`](context::AssistantContext), or the UI.
//! 3. Credentials go through [`CredentialStore`](credentials::CredentialStore)
//!    — the default is the OS keyring. Never logged, never written to
//!    disk in plaintext, redacted in error messages.
//! 4. Errors from a Provider map into the small abstraction-level
//!    [`ProviderError`](error::ProviderError) enum. Higher layers don't
//!    see raw HTTP status codes.
//! 5. Cancellation is end-to-end via
//!    [`tokio_util::sync::CancellationToken`].

pub mod agent;
pub mod context;
pub mod credentials;
pub mod error;
pub mod provider;
pub mod providers;
pub mod wire;

pub use agent::{Agent, AgentEvent, AgentInput, DefaultAgent};
pub use context::{AssistantContext, ContextWindow, EntityInScene};
pub use credentials::{CredentialError, CredentialStore, KeyringCredentialStore};
pub use error::{ProviderError, RetryHint};
pub use provider::{
    Capabilities, ModelInfo, MockProvider, Provider, ProviderRegistry,
};
pub use providers::AnthropicProvider;
pub use wire::{
    ChatDelta, ChatRequest, ContentBlock, Message, Role, Tool, ToolCall, Usage,
};
