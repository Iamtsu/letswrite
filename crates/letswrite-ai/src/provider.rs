//! Low-level provider trait — one impl per vendor backend.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, Stream};
use tokio_util::sync::CancellationToken;

use crate::error::ProviderError;
use crate::wire::{ChatDelta, ChatRequest, Usage};

/// Boxed stream of [`ChatDelta`]s. We don't bind to a concrete impl so
/// providers can use channels, SSE parsers, or any future combinator.
pub type ChatStream =
    Pin<Box<dyn Stream<Item = Result<ChatDelta, ProviderError>> + Send>>;

/// What a provider supports. The UI uses this to enable/disable features.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub streaming: bool,
    pub tool_use: bool,
    pub vision: bool,
    pub max_context_tokens: u32,
}

/// One model offered by a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub max_output_tokens: u32,
}

/// One vendor backend. Providers MUST:
///
/// - Map vendor errors to [`ProviderError`] before returning.
/// - Honour cancellation: when the token fires, the stream should yield
///   `Err(ProviderError::Cancelled)` and stop.
/// - Not log credentials, request bodies that may contain user prose, or
///   anything sensitive at info level. Use `debug`/`trace` for that.
#[async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Stable identifier for the provider (e.g. `"anthropic"`,
    /// `"openai"`). Used by the registry and persisted in settings.
    fn name(&self) -> &str;

    /// What this provider's models can do. May be model-agnostic;
    /// per-model overrides go in [`Self::models`].
    fn capabilities(&self) -> Capabilities;

    /// Models this provider exposes. The registry-listing UI calls this.
    fn models(&self) -> &[ModelInfo];

    /// Stream a chat completion. The returned future resolves once the
    /// request has been sent and the response stream is ready — the
    /// stream itself is driven by the caller.
    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<ChatStream, ProviderError>;
}

/// Discovers and serves provider implementations by name. Settings store
/// the provider name as a string; the registry maps it back to a concrete
/// impl at runtime.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider. Replaces any prior registration under the same name.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.name().to_owned(), provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }
}

// ---------------------------------------------------------------------------
// MockProvider
// ---------------------------------------------------------------------------

/// In-memory provider for tests and the agent layer's own unit tests.
/// Emits whatever script the constructor provides, with optional delays.
#[derive(Clone)]
pub struct MockProvider {
    name: String,
    models: Vec<ModelInfo>,
    capabilities: Capabilities,
    script: Arc<Mutex<MockScript>>,
}

#[derive(Default)]
struct MockScript {
    /// The next request's response. Consumed when `stream` is called; if
    /// empty, returns an empty stream with a default `MessageStop`.
    next: Option<MockResponse>,
    /// History of every request the mock has seen — useful in tests for
    /// asserting what the agent sent.
    requests: Vec<ChatRequest>,
}

#[derive(Clone, Default)]
struct MockResponse {
    deltas: Vec<ChatDelta>,
    /// Inject an immediate error instead of streaming.
    immediate_error: Option<ProviderError>,
}

impl std::fmt::Debug for MockProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockProvider")
            .field("name", &self.name)
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}

impl MockProvider {
    /// Build a mock with one model `mock-1`. Tests can `enqueue_text` or
    /// `enqueue_error` to script the next call.
    pub fn new() -> Self {
        Self {
            name: "mock".into(),
            models: vec![ModelInfo {
                id: "mock-1".into(),
                display_name: "Mock Model".into(),
                max_output_tokens: 4096,
            }],
            capabilities: Capabilities {
                streaming: true,
                tool_use: true,
                vision: true,
                max_context_tokens: 200_000,
            },
            script: Arc::new(Mutex::new(MockScript::default())),
        }
    }

    /// Queue a text-only response. Subsequent `stream` calls return this
    /// once, then go back to the empty default.
    pub fn enqueue_text(&self, text: impl Into<String>) {
        let mut script = self.script.lock().expect("mock mutex poisoned");
        script.next = Some(MockResponse {
            deltas: vec![
                ChatDelta::TextDelta(text.into()),
                ChatDelta::MessageStop { usage: Usage::default() },
            ],
            immediate_error: None,
        });
    }

    /// Queue an error to be returned from the next `stream` call before
    /// any deltas are produced.
    pub fn enqueue_error(&self, err: ProviderError) {
        let mut script = self.script.lock().expect("mock mutex poisoned");
        script.next = Some(MockResponse {
            deltas: Vec::new(),
            immediate_error: Some(err),
        });
    }

    /// Snapshot of every request received so far.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.script
            .lock()
            .expect("mock mutex poisoned")
            .requests
            .clone()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<ChatStream, ProviderError> {
        // Take what we need then drop the lock before any await.
        let response = {
            let mut script = self.script.lock().expect("mock mutex poisoned");
            script.requests.push(request);
            script.next.take().unwrap_or_else(|| MockResponse {
                deltas: vec![ChatDelta::MessageStop { usage: Usage::default() }],
                immediate_error: None,
            })
        };
        if let Some(err) = response.immediate_error {
            return Err(err);
        }
        let deltas = response.deltas;
        // Yield deltas one-by-one; honour cancellation between them.
        let s = stream::unfold(
            (deltas.into_iter(), cancel),
            |(mut iter, cancel)| async move {
                if cancel.is_cancelled() {
                    return Some((Err(ProviderError::Cancelled), (iter, cancel)));
                }
                iter.next().map(|d| (Ok(d), (iter, cancel)))
            },
        );
        Ok(Box::pin(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Message, Role};
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_returns_enqueued_text() {
        let p = MockProvider::new();
        p.enqueue_text("hello");
        let req = ChatRequest {
            model: "mock-1".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..Default::default()
        };
        let mut s = p.stream(req, CancellationToken::new()).await.unwrap();
        let mut collected = Vec::new();
        while let Some(d) = s.next().await {
            collected.push(d.unwrap());
        }
        assert!(matches!(collected[0], ChatDelta::TextDelta(ref t) if t == "hello"));
        assert!(matches!(collected[1], ChatDelta::MessageStop { .. }));
    }

    #[tokio::test]
    async fn mock_returns_enqueued_error() {
        let p = MockProvider::new();
        p.enqueue_error(ProviderError::Auth);
        let result = p
            .stream(ChatRequest::default(), CancellationToken::new())
            .await;
        // ChatStream isn't Debug, so unwrap_err can't be used directly.
        let Err(err) = result else {
            panic!("expected error");
        };
        assert!(matches!(err, ProviderError::Auth));
    }

    #[tokio::test]
    async fn mock_honours_cancellation_mid_stream() {
        let p = MockProvider::new();
        p.enqueue_text("hello");
        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancel before draining
        let mut s = p.stream(ChatRequest::default(), cancel).await.unwrap();
        let first = s.next().await.unwrap();
        assert!(matches!(first, Err(ProviderError::Cancelled)));
    }

    #[tokio::test]
    async fn mock_records_requests() {
        let p = MockProvider::new();
        p.enqueue_text("hi");
        let req = ChatRequest {
            model: "mock-1".into(),
            messages: vec![Message::text(Role::User, "first")],
            ..Default::default()
        };
        let mut s = p.stream(req.clone(), CancellationToken::new()).await.unwrap();
        while s.next().await.is_some() {}
        let logged = p.requests();
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0], req);
    }

    #[test]
    fn registry_round_trips_names() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(MockProvider::new()));
        assert_eq!(reg.names(), vec!["mock".to_owned()]);
        assert!(reg.get("mock").is_some());
        assert!(reg.get("nonexistent").is_none());
    }
}
