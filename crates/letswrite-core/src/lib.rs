//! Core data model and services for letswrite.
//!
//! This crate is UI-independent. It owns: domain types (projects, documents,
//! entities, scenes), persistence, settings, i18n, and services consumed by
//! the UI and importers.

pub mod db;
pub mod error;
pub mod i18n;
pub mod settings;

pub use db::Database;
pub use error::{Error, Result};
pub use i18n::I18n;
pub use settings::Settings;
