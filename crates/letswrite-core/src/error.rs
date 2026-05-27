use std::path::PathBuf;

use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level error for `letswrite-core`.
///
/// Variants are coarse on purpose: callers usually want to know "could we load
/// settings" or "did the disk fail", not the exact serde row that broke. When
/// finer detail is needed, the source error is preserved via `#[from]`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    IoBare(#[from] std::io::Error),

    #[error("could not determine OS-standard config/data directory")]
    NoConfigDir,

    #[error("invalid settings file: {0}")]
    InvalidSettings(String),

    #[error("invalid frontmatter: {0}")]
    InvalidFrontmatter(String),

    #[error("invalid data: {0}")]
    InvalidData(String),

    #[error(transparent)]
    Toml(#[from] toml::de::Error),

    #[error(transparent)]
    TomlSer(#[from] toml::ser::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn io_at(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}
