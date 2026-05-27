//! End-to-end test for the Anthropic provider against a `wiremock` server.
//! No live API key needed.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use letswrite_ai::credentials::InMemoryCredentialStore;
use letswrite_ai::{
    AnthropicProvider, ChatDelta, ChatRequest, CredentialStore, Message, ProviderError,
    Role,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "sk-test-secret";

fn build_provider(base_url: &str) -> AnthropicProvider {
    let creds = Arc::new(InMemoryCredentialStore::new());
    creds.set("anthropic-api-key", API_KEY).unwrap();
    AnthropicProvider::new(creds)
        .unwrap()
        .with_base_url(base_url)
}

fn sse(lines: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (event, data) in lines {
        s.push_str("event: ");
        s.push_str(event);
        s.push('\n');
        s.push_str("data: ");
        s.push_str(data);
        s.push_str("\n\n");
    }
    s
}

#[tokio::test]
async fn streams_text_response_end_to_end() {
    let server = MockServer::start().await;
    let body = sse(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"m1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":", world."}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", API_KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = build_provider(&server.uri());
    let request = ChatRequest {
        model: "claude-sonnet-4-6".into(),
        system: Some("test".into()),
        messages: vec![Message::text(Role::User, "hi")],
        max_tokens: 100,
        ..Default::default()
    };
    let mut stream = letswrite_ai::Provider::stream(
        &provider,
        request,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let mut text = String::new();
    let mut saw_stop = false;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            ChatDelta::TextDelta(t) => text.push_str(&t),
            ChatDelta::MessageStop { usage } => {
                saw_stop = true;
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 5);
            }
            _ => {}
        }
    }
    assert_eq!(text, "Hello, world.");
    assert!(saw_stop);
}

#[tokio::test]
async fn maps_401_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
        .mount(&server)
        .await;

    let provider = build_provider(&server.uri());
    let result = letswrite_ai::Provider::stream(
        &provider,
        ChatRequest {
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, ProviderError::Auth));
}

#[tokio::test]
async fn maps_429_with_retry_after_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "5")
                .set_body_string("{}"),
        )
        .mount(&server)
        .await;

    let provider = build_provider(&server.uri());
    let result = letswrite_ai::Provider::stream(
        &provider,
        ChatRequest {
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    match err {
        ProviderError::RateLimited { after } => {
            assert_eq!(after, Some(Duration::from_secs(5)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_mid_stream_yields_cancelled_error() {
    let server = MockServer::start().await;
    // A normal streaming response — we'll cancel before draining.
    let body = sse(&[(
        "content_block_delta",
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
    )]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = build_provider(&server.uri());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut stream = letswrite_ai::Provider::stream(
        &provider,
        ChatRequest {
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1,
            ..Default::default()
        },
        cancel,
    )
    .await
    .unwrap();
    let mut saw_cancelled = false;
    while let Some(item) = stream.next().await {
        if matches!(item, Err(ProviderError::Cancelled)) {
            saw_cancelled = true;
            break;
        }
    }
    assert!(saw_cancelled, "expected Cancelled error after pre-cancelled token");
}

#[tokio::test]
async fn missing_api_key_returns_auth_without_calling_http() {
    let creds = Arc::new(InMemoryCredentialStore::new()); // no key set
    let provider = AnthropicProvider::new(creds).unwrap();
    let result = letswrite_ai::Provider::stream(
        &provider,
        ChatRequest {
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
    let Err(err) = result else {
        panic!("expected error");
    };
    assert!(matches!(err, ProviderError::Auth));
}
