//! Terminal colour delivery backends and terminal capability detection (`PLAN.md` §3 & §5).

pub mod error;
pub mod osc;

use error::DeliverError;
use sk_core::palette::Frame;

/// Common delivery interface across OSC, tty broadcast, tmux, and native terminal backends.
pub trait Backend {
    /// Short backend identifier (e.g. `"osc"`, `"tty-broadcast"`, `"kitty"`).
    fn name(&self) -> &'static str;

    /// Delivers a contrast-verified [`Frame`] to the target terminal(s).
    fn deliver(&mut self, frame: &Frame) -> Result<(), DeliverError>;

    /// Restores default terminal colours where supported.
    fn reset(&mut self) -> Result<(), DeliverError>;
}
