//! Credential storage. The default backing is the OS keyring; tests
//! and headless environments can swap in an in-memory store.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

/// Errors interacting with a credential store.
#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential not found for key {0:?}")]
    NotFound(String),

    #[error("credential store error: {0}")]
    Backend(String),
}

/// Read/write secrets keyed by an opaque string. Implementations MUST
/// never log or display the secret value.
pub trait CredentialStore: Send + Sync + std::fmt::Debug {
    /// Fetch the secret for `key`. Returns `NotFound` if the key isn't
    /// set, rather than failing — callers commonly probe for optional
    /// keys.
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError>;

    /// Set `key` to `value`. Replaces any existing value.
    fn set(&self, key: &str, value: &str) -> Result<(), CredentialError>;

    /// Delete `key`. Idempotent: deleting a missing key is not an error.
    fn delete(&self, key: &str) -> Result<(), CredentialError>;
}

/// OS-keyring backing — Secret Service on Linux, Keychain on macOS,
/// Credential Manager on Windows. Uses the [`keyring`] crate.
#[derive(Debug)]
pub struct KeyringCredentialStore {
    service: String,
}

impl KeyringCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }
}

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| CredentialError::Backend(e.to_string()))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError::Backend(e.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| CredentialError::Backend(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| CredentialError::Backend(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(&self.service, key)
            .map_err(|e| CredentialError::Backend(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CredentialError::Backend(e.to_string())),
        }
    }
}

/// In-memory credential store for tests and headless environments. Values
/// live only for the lifetime of the process and are NOT persisted.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    inner: Mutex<HashMap<String, String>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError> {
        Ok(self
            .inner
            .lock()
            .expect("credential mutex poisoned")
            .get(key)
            .cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), CredentialError> {
        self.inner
            .lock()
            .expect("credential mutex poisoned")
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        self.inner
            .lock()
            .expect("credential mutex poisoned")
            .remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_store_round_trips() {
        let s = InMemoryCredentialStore::new();
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "sekret").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("sekret"));
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
    }

    #[test]
    fn delete_missing_key_is_ok() {
        let s = InMemoryCredentialStore::new();
        s.delete("never-set").unwrap();
    }

    #[test]
    fn credential_error_display_does_not_leak_value() {
        let err = CredentialError::NotFound("anthropic-api-key".into());
        let s = err.to_string();
        // Only the key name is in the message, never the value.
        assert!(s.contains("anthropic-api-key"));
        assert!(!s.contains("sekret"));
    }
}
