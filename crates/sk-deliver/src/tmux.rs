//! `tmux` per-pane OSC palette delivery and optional `\x1bPtmux;…\x1b\\` outer-terminal DCS
//! passthrough (`PLAN.md` §5 & §10 T10; verified in `docs/terminals/tmux.md`).

use crate::error::DeliverError;
use crate::osc::{encode_osc_frame, encode_osc_reset};
use crate::tty::{deliver_payload_with_ops, PosixTtyOps, TtyDeliveryReport, TtyOps};
use crate::Backend;
use rustix::process::geteuid;
use sk_core::palette::{Frame, PaletteColors};
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Opening 7-bit DCS prefix recognised by `tmux` (`ESC P tmux;`).
pub const TMUX_DCS_PREFIX: &[u8] = b"\x1bPtmux;";

/// Closing 7-bit String Terminator (`ESC \`) ending the outer `tmux` DCS envelope.
pub const TMUX_DCS_SUFFIX: &[u8] = b"\x1b\\";

/// Wraps an inner terminal escape byte slice in a `tmux(1)` DCS passthrough envelope
/// (`\x1bPtmux;<inner-with-every-\x1b-doubled>\x1b\\`).
#[must_use]
pub fn encode_tmux_dcs_passthrough(inner: &[u8]) -> Vec<u8> {
    let esc_count = inner.iter().filter(|&&b| b == 0x1b).count();
    let mut out =
        Vec::with_capacity(TMUX_DCS_PREFIX.len() + inner.len() + esc_count + TMUX_DCS_SUFFIX.len());
    out.extend_from_slice(TMUX_DCS_PREFIX);
    for &byte in inner {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(TMUX_DCS_SUFFIX);
    out
}

/// Decodes a `tmux(1)` DCS passthrough envelope back into its unwrapped inner escape sequence,
/// returning `None` if the prefix/suffix is missing or an un-doubled `0x1b` appears in the body.
#[must_use]
pub fn decode_tmux_dcs_passthrough(dcs: &[u8]) -> Option<Vec<u8>> {
    let body = dcs
        .strip_prefix(TMUX_DCS_PREFIX)?
        .strip_suffix(TMUX_DCS_SUFFIX)?;
    let mut out = Vec::with_capacity(body.len());
    let mut idx = 0;
    while idx < body.len() {
        if body[idx] == 0x1b {
            if idx + 1 >= body.len() || body[idx + 1] != 0x1b {
                return None;
            }
            out.push(0x1b);
            idx += 2;
        } else {
            out.push(body[idx]);
            idx += 1;
        }
    }
    Some(out)
}

/// Encodes a colour frame for a `tmux` pane tty (`docs/terminals/tmux.md`):
/// - When `passthrough_outer` is `false`, emits raw `OSC 11/10/12` + `OSC 4;0..15` (`383` bytes)
///   so `tmux` 3.3+ updates its own per-pane `wp->palette` even when `allow-passthrough` is `off`.
/// - When `passthrough_outer` is `true`, places the DCS passthrough envelope (`430` bytes)
///   **before** the raw OSC segment (`383` bytes) in the **same single buffer** (`813` bytes
///   total) so `tmux`'s `input_dcs_dispatch` forwards the unwrapped OSC sequence to the outer
///   terminal before `input_osc_11`/`input_osc_4` sets `PANE_REDRAW` on the pane.
#[must_use]
pub fn encode_tmux_frame(colors: &PaletteColors, passthrough_outer: bool) -> Vec<u8> {
    let raw = encode_osc_frame(colors);
    if passthrough_outer {
        let mut out = encode_tmux_dcs_passthrough(&raw);
        out.extend_from_slice(&raw);
        out
    } else {
        raw
    }
}

/// Encodes a palette reset (`OSC 104/110/111/112`) for a `tmux` pane tty, optionally prepending
/// the `tmux` DCS passthrough envelope into the same single-write buffer (`73` bytes total) before
/// the raw reset segment sets `PANE_REDRAW`.
#[must_use]
pub fn encode_tmux_reset(passthrough_outer: bool) -> Vec<u8> {
    let raw = encode_osc_reset();
    if passthrough_outer {
        let mut out = encode_tmux_dcs_passthrough(&raw);
        out.extend_from_slice(&raw);
        out
    } else {
        raw
    }
}

/// Discovers active `tmux` pane pseudo-terminal paths (`#{pane_tty}`).
pub trait TmuxPaneLister {
    fn list_pane_ttys(&self) -> Result<Vec<PathBuf>, DeliverError>;
}

/// Invokes `tmux list-panes -a -F "#{pane_tty}"` (with optional `-S` socket path or `-L` socket
/// name) and returns deduplicated pane tty paths, returning an empty list when `tmux` is not
/// installed or no `tmux` server is currently running.
#[derive(Clone, Debug)]
pub struct CliTmuxPaneLister {
    program: PathBuf,
    socket_path: Option<PathBuf>,
    socket_name: Option<String>,
}

impl Default for CliTmuxPaneLister {
    fn default() -> Self {
        Self::new()
    }
}

impl CliTmuxPaneLister {
    #[must_use]
    pub fn new() -> Self {
        Self {
            program: PathBuf::from("tmux"),
            socket_path: None,
            socket_name: None,
        }
    }

    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            socket_path: None,
            socket_name: None,
        }
    }

    #[must_use]
    pub fn with_socket_path(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            program: PathBuf::from("tmux"),
            socket_path: Some(socket_path.into()),
            socket_name: None,
        }
    }

    #[must_use]
    pub fn with_socket_name(socket_name: impl Into<String>) -> Self {
        Self {
            program: PathBuf::from("tmux"),
            socket_path: None,
            socket_name: Some(socket_name.into()),
        }
    }

    #[must_use]
    pub fn with_program_path(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    #[must_use]
    pub fn parse_pane_tty_lines(stdout: &str) -> Vec<PathBuf> {
        let mut unique = BTreeSet::new();
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('/') {
                unique.insert(PathBuf::from(trimmed));
            }
        }
        unique.into_iter().collect()
    }
}

fn is_no_tmux_server_stderr(stderr_lower: &str) -> bool {
    stderr_lower.is_empty()
        || stderr_lower.contains("no server running")
        || stderr_lower.contains("no such file or directory")
        || stderr_lower.contains("connection refused")
        || stderr_lower.contains("no sessions")
        || stderr_lower.contains("no current client")
}

impl TmuxPaneLister for CliTmuxPaneLister {
    fn list_pane_ttys(&self) -> Result<Vec<PathBuf>, DeliverError> {
        let mut cmd = Command::new(&self.program);
        cmd.stdin(Stdio::null());
        if let Some(ref sock_path) = self.socket_path {
            cmd.arg("-S").arg(sock_path);
        } else if let Some(ref sock_name) = self.socket_name {
            cmd.arg("-L").arg(sock_name);
        }
        cmd.args(["list-panes", "-a", "-F", "#{pane_tty}"]);

        let output = match cmd.output() {
            Ok(out) => out,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(DeliverError::Io(err)),
        };

        if !output.status.success() {
            let stderr_raw = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stderr_lower = stderr_raw.to_ascii_lowercase();
            if is_no_tmux_server_stderr(&stderr_lower) {
                return Ok(Vec::new());
            }
            return Err(DeliverError::BackendCommand {
                backend: "tmux",
                message: format!(
                    "`tmux list-panes` exited with {}: {stderr_raw}",
                    output.status
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(Self::parse_pane_tty_lines(&stdout))
    }
}

/// `tmux` delivery backend discovering live `#{pane_tty}` paths and delivering per-pane OSC (plus
/// optional outer-terminal DCS passthrough) through [`crate::tty::deliver_payload_with_ops`].
#[derive(Debug)]
pub struct TmuxBackend<L = CliTmuxPaneLister, O = PosixTtyOps> {
    passthrough_outer: bool,
    expected_uid: u32,
    lister: L,
    ops: O,
    last_report: Option<TtyDeliveryReport>,
}

impl TmuxBackend<CliTmuxPaneLister, PosixTtyOps> {
    #[must_use]
    pub fn new(passthrough_outer: bool) -> Self {
        Self::with_lister_and_ops(
            passthrough_outer,
            geteuid().as_raw(),
            CliTmuxPaneLister::new(),
            PosixTtyOps,
        )
    }
}

impl<L: TmuxPaneLister, O: TtyOps> TmuxBackend<L, O> {
    #[must_use]
    pub const fn with_lister_and_ops(
        passthrough_outer: bool,
        expected_uid: u32,
        lister: L,
        ops: O,
    ) -> Self {
        Self {
            passthrough_outer,
            expected_uid,
            lister,
            ops,
            last_report: None,
        }
    }

    #[must_use]
    pub const fn passthrough_outer(&self) -> bool {
        self.passthrough_outer
    }

    pub fn set_passthrough_outer(&mut self, passthrough_outer: bool) {
        self.passthrough_outer = passthrough_outer;
    }

    #[must_use]
    pub const fn lister(&self) -> &L {
        &self.lister
    }

    #[must_use]
    pub const fn ops(&self) -> &O {
        &self.ops
    }

    #[must_use]
    pub const fn last_report(&self) -> Option<&TtyDeliveryReport> {
        self.last_report.as_ref()
    }

    fn deliver_raw_payload(&mut self, payload: &[u8]) -> Result<TtyDeliveryReport, DeliverError> {
        let pane_ttys = self.lister.list_pane_ttys()?;
        let report = deliver_payload_with_ops(&pane_ttys, self.expected_uid, payload, &self.ops)?;
        self.last_report = Some(report.clone());
        Ok(report)
    }

    pub fn deliver_frame_with_report(
        &mut self,
        frame: &Frame,
    ) -> Result<TtyDeliveryReport, DeliverError> {
        let payload = encode_tmux_frame(&frame.colors, self.passthrough_outer);
        self.deliver_raw_payload(&payload)
    }

    pub fn reset_with_report(&mut self) -> Result<TtyDeliveryReport, DeliverError> {
        let payload = encode_tmux_reset(self.passthrough_outer);
        self.deliver_raw_payload(&payload)
    }
}

impl<L: TmuxPaneLister, O: TtyOps> Backend for TmuxBackend<L, O> {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn deliver(&mut self, frame: &Frame) -> Result<(), DeliverError> {
        self.deliver_frame_with_report(frame).map(|_| ())
    }

    fn reset(&mut self) -> Result<(), DeliverError> {
        self.reset_with_report().map(|_| ())
    }
}
