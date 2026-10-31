//! xterm OSC 4 / 10 / 11 / 12 and 104 / 110 / 111 / 112 escape sequence encoder (`PLAN.md` §5, T07).
//!
//! Reference: Thomas E. Dickey, *XTerm Control Sequences* (`ctlseqs`):
//! <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>

use crate::error::DeliverError;
use crate::Backend;
use sk_core::color::Rgb8;
use sk_core::palette::{Frame, PaletteColors};
use std::io::Write;

/// 7-bit String Terminator (`ESC \`) used across all Shiki OSC sequences (`PLAN.md` §5).
pub const ST: &str = "\x1b\\";

/// Formats an [`Rgb8`] in xterm `rgb:rr/gg/bb` syntax (lowercase two-digit hex channels).
#[must_use]
pub fn format_osc_rgb(rgb: Rgb8) -> String {
    format!("rgb:{:02x}/{:02x}/{:02x}", rgb.r, rgb.g, rgb.b)
}

fn push_osc_color(buf: &mut Vec<u8>, code: u8, rgb: Rgb8) {
    let _ = write!(
        buf,
        "\x1b]{code};rgb:{:02x}/{:02x}/{:02x}{ST}",
        rgb.r, rgb.g, rgb.b
    );
}

fn push_osc_palette_slot(buf: &mut Vec<u8>, slot: usize, rgb: Rgb8) {
    let _ = write!(
        buf,
        "\x1b]4;{slot};rgb:{:02x}/{:02x}/{:02x}{ST}",
        rgb.r, rgb.g, rgb.b
    );
}

/// Encodes a complete terminal colour frame as a single concatenated OSC byte buffer:
/// `OSC 11` (`bg`), `OSC 10` (`fg`), `OSC 12` (`cursor`), and `OSC 4;0..=15` (`ansi[0..16]`),
/// each terminated with 7-bit `ST` (`\x1b\\`).
#[must_use]
pub fn encode_osc_frame(colors: &PaletteColors) -> Vec<u8> {
    // 19 sequences * ~24 bytes = ~456 bytes.
    let mut out = Vec::with_capacity(480);
    push_osc_color(&mut out, 11, colors.bg);
    push_osc_color(&mut out, 10, colors.fg);
    push_osc_color(&mut out, 12, colors.cursor);
    for (slot, &rgb) in colors.ansi.iter().enumerate() {
        push_osc_palette_slot(&mut out, slot, rgb);
    }
    out
}

/// Encodes the portable xterm reset sequence restoring ANSI 0–15 (`OSC 104`), foreground
/// (`OSC 110`), background (`OSC 111`), and cursor (`OSC 112`), each terminated with `ST` (`\x1b\\`).
#[must_use]
pub fn encode_osc_reset() -> Vec<u8> {
    b"\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\".to_vec()
}

/// Returns `true` if `bytes` contains any palette-mutating OSC control sequence
/// (`OSC 4`, `10`, `11`, `12`, `104`, `110`, `111`, or `112`).
///
/// Used by tests and safety assertions to guarantee `preview` never mutates the active terminal
/// palette (`PLAN.md` §10 T07 AC).
#[must_use]
pub fn contains_palette_mutating_osc(bytes: &[u8]) -> bool {
    const PREFIXES: [&[u8]; 12] = [
        b"\x1b]4;",
        b"\x1b]10;",
        b"\x1b]11;",
        b"\x1b]12;",
        b"\x1b]104;",
        b"\x1b]104\x1b",
        b"\x1b]104\x07",
        b"\x1b]110",
        b"\x1b]111",
        b"\x1b]112",
        b"\x1b]17;",
        b"\x1b]19;",
    ];
    bytes.windows(2).enumerate().any(|(idx, pair)| {
        if pair != b"\x1b]" {
            return false;
        }
        let tail = &bytes[idx..];
        PREFIXES.iter().any(|prefix| tail.starts_with(prefix))
    })
}

/// Direct writer-backed OSC delivery backend emitting an entire frame or reset payload in a
/// single `write_all` call.
#[derive(Debug)]
pub struct OscBackend<W> {
    writer: W,
}

impl<W: Write> OscBackend<W> {
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    #[must_use]
    pub const fn writer(&self) -> &W {
        &self.writer
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> Backend for OscBackend<W> {
    fn name(&self) -> &'static str {
        "osc"
    }

    fn deliver(&mut self, frame: &Frame) -> Result<(), DeliverError> {
        let payload = encode_osc_frame(&frame.colors);
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), DeliverError> {
        let payload = encode_osc_reset();
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        Ok(())
    }
}
