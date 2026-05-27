//! Abstraction-level errors.
//!
//! Provider implementations map their vendor-specific failure modes into
//! this small set. Higher layers don't see raw HTTP status codes or vendor
//! error envelopes.

use std::time::Duration;

use thiserror::Error;

/// Hint to the caller about whether the failed request can be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryHint {
    /// Don't retry — the request itself is malformed or the credentials
    /// are wrong; retrying won't help.
    Never,
    /// Retrying may succeed. If `after` is `Some`, the provider hinted at
    /// a minimum delay (usually from a `Retry-After` header).
    After { after: Option<Duration> },
}

/// All provider failures mapped into a small set. Display strings here
/// must NOT include credentials — providers redact secrets before
/// constructing the error.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// Authentication failed: missing key, expired key, wrong account.
    #[error("authentication failed")]
    Auth,

    /// We hit a quota/rate limit. `retry_after` is the provider's hint.
    #[error("rate limited; retry after {after:?}")]
    RateLimited {
        after: Option<Duration>,
    },

    /// 5xx, network blip, anything transient.
    #[error("transient backend error: {message}")]
    Transient { message: String },

    /// 4xx malformed request, validation failure, unknown model, etc.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// We couldn't parse the provider's response (their schema changed,
    /// we got HTML instead of JSON, an SSE event we didn't recognise).
    #[error("protocol error: {message}")]
    Protocol { message: String },

    /// Underlying transport failed (DNS, TLS, connection refused).
    #[error("transport error: {message}")]
    Transport { message: String },

    /// The request was cancelled by the caller.
    #[error("cancelled")]
    Cancelled,
}

impl ProviderError {
    /// Whether retrying the same request might succeed.
    pub const fn retry_hint(&self) -> RetryHint {
        match self {
            Self::RateLimited { after } => RetryHint::After { after: *after },
            Self::Transient { .. } | Self::Transport { .. } => {
                RetryHint::After { after: None }
            }
            Self::Auth
            | Self::InvalidRequest { .. }
            | Self::Protocol { .. }
            | Self::Cancelled => RetryHint::Never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_hints_match_intuition() {
        assert!(matches!(ProviderError::Auth.retry_hint(), RetryHint::Never));
        assert!(matches!(
            ProviderError::RateLimited { after: Some(Duration::from_secs(2)) }.retry_hint(),
            RetryHint::After { after: Some(_) }
        ));
        assert!(matches!(
            ProviderError::Transient { message: "boom".into() }.retry_hint(),
            RetryHint::After { after: None }
        ));
        assert!(matches!(
            ProviderError::Cancelled.retry_hint(),
            RetryHint::Never
        ));
    }

    #[test]
    fn auth_error_does_not_carry_secrets() {
        // Compile-time guarantee: the variant has no String/data field at
        // all. If someone adds one, this test breaks loudly.
        let err = ProviderError::Auth;
        let s = err.to_string();
        assert!(!s.contains("sk-"));
        assert!(!s.contains("Bearer"));
    }
}
