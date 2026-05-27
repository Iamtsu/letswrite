//! Core data model and services for letswrite.
//!
//! This crate is UI-independent. It owns: domain types (projects, documents,
//! entities, scenes), persistence, settings, i18n, and services consumed by
//! the UI and importers.

pub mod db;
pub mod document;
pub mod error;
pub mod i18n;
pub mod project;
pub mod settings;
pub mod watcher;

pub use db::Database;
pub use document::{Document, DocumentKind};
pub use error::{Error, Result};
pub use i18n::I18n;
pub use project::{Project, SyncOutcome};
pub use settings::Settings;
pub use watcher::{ProjectWatcher, WatchEvent};
