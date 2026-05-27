//! High-level Agent — the only AI surface the UI consumes.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::context::AssistantContext;
use crate::error::ProviderError;
use crate::provider::Provider;
use crate::wire::{ChatDelta, ChatRequest, ContentBlock, Message, Role, Usage};

/// The UI's hand-in to the Agent. `user_input` is free-form text; a
/// preset prompt (e.g. "critique this scene") is folded into the
/// system prompt by the agent.
#[derive(Debug, Clone)]
pub struct AgentInput {
    pub user_input: String,
    /// Optional preset id (matches a built-in template such as
    /// `"critique"`, `"continuity"`, etc.). `None` = free chat.
    pub preset: Option<String>,
}

impl AgentInput {
    pub fn user(text: impl Into<String>) -> Self {
        Self { user_input: text.into(), preset: None }
    }
}

/// UI-facing event stream. Does NOT expose raw provider deltas — the UI
/// would couple to vendor mechanics otherwise.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::derive_partial_eq_without_eq)] // Suggestion carries JSON.
pub enum AgentEvent {
    /// The agent has accepted the request and is preparing to call the
    /// provider. Useful for showing a spinner before the first token.
    Thinking,
    /// One chunk of plain text.
    TextChunk(String),
    /// The assistant invoked a tool. `args` are the parsed JSON.
    ToolInvocation { name: String, args: serde_json::Value },
    /// The result of a tool invocation, surfaced for UI display.
    ToolResult { name: String, output: String, is_error: bool },
    /// The assistant produced a structured suggestion the UI can act on.
    /// `payload` is JSON; the agent and the UI agree on the schema per
    /// suggestion kind (e.g. `"rename_character"`, `"add_scene"`).
    Suggestion { kind: String, payload: serde_json::Value },
    /// The turn is over.
    Done { usage: Usage },
    /// An error terminated the turn.
    Error(ProviderError),
}

/// Stream of agent events.
pub type AgentStream =
    Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// What the UI talks to.
#[async_trait]
pub trait Agent: Send + Sync + std::fmt::Debug {
    /// Hand the agent a user input + context bundle. The returned stream
    /// yields events until the turn ends. Cancellation propagates all
    /// the way down to the provider's HTTP request.
    async fn ask(
        &self,
        input: AgentInput,
        context: AssistantContext,
        cancel: CancellationToken,
    ) -> AgentStream;
}

// ---------------------------------------------------------------------------
// DefaultAgent
// ---------------------------------------------------------------------------

/// The default agent: wraps any `Provider`, renders `AssistantContext`
/// into a `ChatRequest`, and forwards deltas as `AgentEvent`s. The UI
/// uses this for v1; specialised agents can replace it later.
#[derive(Debug, Clone)]
pub struct DefaultAgent {
    provider: Arc<dyn Provider>,
    model: String,
    system_prompt: String,
    max_tokens: u32,
}

impl DefaultAgent {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            system_prompt: default_system_prompt(),
            max_tokens: 4096,
        }
    }

    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    #[must_use]
    pub const fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Render an `AssistantContext` + `AgentInput` into a `ChatRequest`.
    /// Public for tests; UI consumers should call [`ask`](Agent::ask).
    pub fn render_request(
        &self,
        input: &AgentInput,
        context: &AssistantContext,
    ) -> ChatRequest {
        let system = build_system_prompt(&self.system_prompt, context);
        let user_message = build_user_message(input, context);
        ChatRequest {
            model: self.model.clone(),
            system: Some(system),
            messages: vec![user_message],
            max_tokens: self.max_tokens,
            ..ChatRequest::default()
        }
    }
}

#[async_trait]
impl Agent for DefaultAgent {
    async fn ask(
        &self,
        input: AgentInput,
        context: AssistantContext,
        cancel: CancellationToken,
    ) -> AgentStream {
        let request = self.render_request(&input, &context);
        let provider = Arc::clone(&self.provider);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = tx.send(AgentEvent::Thinking);

        // No further use of `cancel` here; move it into the spawned task.
        tokio::spawn(async move {
            let cancel_for_task = cancel;
            let mut stream = match provider.stream(request, cancel_for_task).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error(e));
                    return;
                }
            };
            let mut tool_in_progress: Option<ToolAccum> = None;
            while let Some(delta) = stream.next().await {
                match delta {
                    Ok(ChatDelta::TextDelta(t)) => {
                        let _ = tx.send(AgentEvent::TextChunk(t));
                    }
                    Ok(ChatDelta::ToolUseStart { id, name }) => {
                        tool_in_progress = Some(ToolAccum {
                            id,
                            name,
                            partial_args: String::new(),
                        });
                    }
                    Ok(ChatDelta::ToolUseDelta(chunk)) => {
                        if let Some(t) = tool_in_progress.as_mut() {
                            t.partial_args.push_str(&chunk);
                        }
                    }
                    Ok(ChatDelta::ToolUseEnd { call }) => {
                        let _ = tx.send(AgentEvent::ToolInvocation {
                            name: call.name,
                            args: call.arguments,
                        });
                        tool_in_progress = None;
                    }
                    Ok(ChatDelta::MessageStop { usage }) => {
                        let _ = tx.send(AgentEvent::Done { usage });
                        return;
                    }
                    Ok(ChatDelta::Error(e)) | Err(e) => {
                        let _ = tx.send(AgentEvent::Error(e));
                        return;
                    }
                }
            }
            // Stream ended without MessageStop — surface a synthetic Done.
            let _ = tx.send(AgentEvent::Done { usage: Usage::default() });
        });

        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        Box::pin(stream)
    }
}

struct ToolAccum {
    #[allow(dead_code)] // surfaces in ToolUseEnd via the wire payload
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    partial_args: String,
}

/// Build the system prompt by appending project meta and language hint to
/// the agent's base prompt.
fn build_system_prompt(base: &str, context: &AssistantContext) -> String {
    let mut out = String::with_capacity(base.len() + 256);
    out.push_str(base);
    if !context.project_meta.is_empty() {
        out.push_str("\n\n# Project context\n");
        for meta in &context.project_meta {
            out.push_str("\n## ");
            out.push_str(&meta.label);
            out.push('\n');
            out.push_str(&meta.content);
        }
    }
    if let Some(lang) = &context.language {
        out.push_str("\n\nReply in ");
        out.push_str(&lang.to_string());
        out.push_str(" unless the user asks otherwise.");
    }
    out
}

/// Build the single user-role message, weaving in document, selection, and
/// entity context. We use plain `Markdown` sections rather than a tool-call
/// dance to keep the request shape compatible with any provider that just
/// does plain text — Anthropic, `OpenAI`, local models.
fn build_user_message(
    input: &AgentInput,
    context: &AssistantContext,
) -> Message {
    let mut text = String::new();

    if let Some(doc) = &context.document {
        text.push_str("## Document: ");
        text.push_str(&doc.title);
        text.push_str("\n_Path: ");
        text.push_str(&doc.rel_path);
        text.push_str("_\n\n");
        let window = &doc.body[doc.window.start..doc.window.end.min(doc.body.len())];
        text.push_str("```\n");
        text.push_str(window);
        text.push_str("\n```\n");
    }

    if let Some(sel) = &context.selection {
        if !sel.trim().is_empty() {
            text.push_str("\n## Selection\n\n```\n");
            text.push_str(sel);
            text.push_str("\n```\n");
        }
    }

    if !context.entities_in_scene.is_empty() {
        text.push_str("\n## Characters in scene\n\n");
        for entity in &context.entities_in_scene {
            text.push_str("- **");
            text.push_str(&entity.name);
            text.push_str("** (");
            text.push_str(&entity.kind);
            text.push_str(") — ");
            text.push_str(&entity.current_state);
            text.push('\n');
        }
    }

    text.push_str("\n## Request\n\n");
    if let Some(preset) = &input.preset {
        text.push_str("Preset: ");
        text.push_str(preset);
        text.push_str("\n\n");
    }
    text.push_str(&input.user_input);

    Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text }],
    }
}

fn default_system_prompt() -> String {
    "You are letswrite's assistant — a literary collaborator helping a novelist.\n\
     - Be concrete, not abstract: quote the prose you're reacting to.\n\
     - Match the user's voice and language.\n\
     - Do not rewrite unless asked. Suggest, don't dictate.\n\
     - When you don't know, say so."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ContextWindow, DocumentContext, EntityInScene, WindowKind};
    use crate::provider::MockProvider;
    use std::path::PathBuf;

    fn sample_context() -> AssistantContext {
        AssistantContext {
            document: Some(DocumentContext {
                abs_path: PathBuf::from("/x/Chapters/Chapter 2.md"),
                rel_path: "Chapters/Chapter 2.md".into(),
                title: "Chapter 2".into(),
                body: "## Beat 1: The Fog\n\nEvan drove through fog.\n".into(),
                cursor_offset: 0,
                window: ContextWindow {
                    kind: WindowKind::Scene,
                    start: 0,
                    end: 41,
                },
            }),
            selection: Some("Evan drove through fog.".into()),
            entities_in_scene: vec![EntityInScene {
                name: "Evan Calder".into(),
                kind: "character".into(),
                current_state: "deputy director, foggy commute, sense of unease".into(),
            }],
            project_meta: Vec::new(),
            language: Some("en".parse().unwrap()),
            token_budget: 8_000,
        }
    }

    #[test]
    fn render_request_includes_doc_selection_and_entities() {
        let agent = DefaultAgent::new(Arc::new(MockProvider::new()), "mock-1");
        let ctx = sample_context();
        let req = agent.render_request(&AgentInput::user("critique"), &ctx);
        let text = req.messages[0].flatten_text();
        assert!(text.contains("Chapter 2"));
        assert!(text.contains("Evan drove through fog"));
        assert!(text.contains("Selection"));
        assert!(text.contains("Evan Calder"));
        assert!(req.system.as_ref().unwrap().contains("literary collaborator"));
        // Language hint folded into system prompt.
        assert!(req.system.as_ref().unwrap().contains("Reply in en"));
    }

    #[tokio::test]
    async fn ask_streams_text_then_done() {
        let provider = MockProvider::new();
        provider.enqueue_text("looks fine to me");
        let agent = DefaultAgent::new(Arc::new(provider), "mock-1");
        let mut stream = agent
            .ask(
                AgentInput::user("crit"),
                AssistantContext::empty(),
                CancellationToken::new(),
            )
            .await;
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
        assert!(matches!(events[0], AgentEvent::Thinking));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextChunk(t) if t == "looks fine to me")));
        assert!(matches!(events.last().unwrap(), AgentEvent::Done { .. }));
    }

    #[tokio::test]
    async fn ask_surfaces_provider_error() {
        let provider = MockProvider::new();
        provider.enqueue_error(ProviderError::Auth);
        let agent = DefaultAgent::new(Arc::new(provider), "mock-1");
        let mut stream = agent
            .ask(
                AgentInput::user("hi"),
                AssistantContext::empty(),
                CancellationToken::new(),
            )
            .await;
        let mut saw_error = false;
        while let Some(ev) = stream.next().await {
            if matches!(ev, AgentEvent::Error(ProviderError::Auth)) {
                saw_error = true;
            }
        }
        assert!(saw_error, "expected an Error event from a failing provider");
    }

    #[tokio::test]
    async fn ask_propagates_cancellation() {
        let provider = MockProvider::new();
        provider.enqueue_text("slow response");
        let agent = DefaultAgent::new(Arc::new(provider), "mock-1");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut stream = agent
            .ask(
                AgentInput::user("hi"),
                AssistantContext::empty(),
                cancel,
            )
            .await;
        let mut saw_cancelled = false;
        while let Some(ev) = stream.next().await {
            if matches!(ev, AgentEvent::Error(ProviderError::Cancelled)) {
                saw_cancelled = true;
            }
        }
        assert!(saw_cancelled);
    }
}
