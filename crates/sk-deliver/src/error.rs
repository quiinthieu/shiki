//! Error types for `sk-deliver`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeliverError {
    #[error("delivery I/O error: {0}")]
    Io(#[from] std::io::Error),
}
