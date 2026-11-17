//! Error types for `sk-deliver`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeliverError {
    #[error("delivery I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{backend} delivery command failed: {message}")]
    BackendCommand {
        backend: &'static str,
        message: String,
    },
}
