//! Error types for `sk-store`.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("unable to determine user home directory; set HOME or SHIKI_HOME")]
    MissingHomeDirectory,

    #[error("fixture name '{0}' must be a single normal filename component")]
    InvalidFixtureName(String),

    #[error("{kind} fixture '{name}' not found at {path}")]
    FixtureNotFound {
        kind: &'static str,
        name: String,
        path: PathBuf,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Location(#[from] sk_core::location::LocationError),

    #[error("failed to parse configuration at {path}{location}: {message}")]
    ConfigParse {
        path: PathBuf,
        location: String,
        message: String,
    },

    #[error("failed to serialize configuration: {0}")]
    ConfigSerialize(String),
}
