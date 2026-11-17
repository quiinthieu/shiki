//! Opt-in UID-scoped pseudo-terminal (`tty`) enumeration and single-write OSC broadcast
//! (`PLAN.md` §5 & §10 T10; verified in `docs/terminals/tty-broadcast.md`).

use crate::error::DeliverError;
use crate::osc::{encode_osc_frame, encode_osc_reset};
use crate::Backend;
use rustix::fd::OwnedFd;
use rustix::fs::{fstat, lstat, open, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use rustix::process::geteuid;
use rustix::termios::isatty;
use sk_core::palette::Frame;
use std::collections::{BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Returns `true` if `name` matches a Linux / WSL `devpts` replica filename (`/dev/pts/<digits>`),
/// rejecting `ptmx`, hidden entries, and any non-digit name (`docs/terminals/tty-broadcast.md`).
#[must_use]
pub fn is_devpts_replica_name(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

/// Returns `true` if `name` matches a macOS Darwin `devfs` UNIX98 pseudo-terminal replica filename
/// (`/dev/ttys<digits>`, e.g. `ttys000`), rejecting `/dev/tty`, `/dev/console`, `/dev/ptmx`,
/// `/dev/tty.*`, `/dev/cu.*`, and Linux virtual consoles `/dev/tty0`–`/dev/tty63`.
#[must_use]
pub fn is_darwin_ttys_replica_name(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    let Some(rest) = bytes.strip_prefix(b"ttys") else {
        return false;
    };
    !rest.is_empty() && rest.iter().all(u8::is_ascii_digit)
}

/// Directory or path source scanned for candidate user pseudo-terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtyScanRoot {
    /// Linux / WSL `devpts` directory (`/dev/pts`), matching [`is_devpts_replica_name`].
    DevPts(PathBuf),
    /// macOS Darwin `devfs` directory (`/dev`), matching [`is_darwin_ttys_replica_name`].
    DevTtys(PathBuf),
    /// Explicit candidate device paths (used by [`crate::tmux::TmuxBackend`] and isolated tests).
    ExplicitPaths(Vec<PathBuf>),
}

/// Returns the default Unix pseudo-terminal scan roots (`/dev/pts` and `/dev`).
#[must_use]
pub fn default_tty_scan_roots() -> Vec<TtyScanRoot> {
    vec![
        TtyScanRoot::DevPts(PathBuf::from("/dev/pts")),
        TtyScanRoot::DevTtys(PathBuf::from("/dev")),
    ]
}

/// Device identity and ownership metadata inspected before and after `open(2)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TtyStat {
    pub dev: u64,
    pub ino: u64,
    pub rdev: u64,
    pub uid: u32,
    pub is_char_device: bool,
    pub is_symlink: bool,
}

#[allow(clippy::unnecessary_cast)]
fn stat_to_tty_stat(stat: &Stat) -> TtyStat {
    let ft = FileType::from_raw_mode(stat.st_mode);
    TtyStat {
        dev: stat.st_dev as u64,
        ino: stat.st_ino as u64,
        rdev: stat.st_rdev as u64,
        uid: stat.st_uid,
        is_char_device: ft.is_char_device(),
        is_symlink: ft.is_symlink(),
    }
}

/// Per-pass summary of candidate ttys inspected, delivered, or skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TtyDeliveryReport {
    pub candidates: usize,
    pub delivered: usize,
    pub skipped_other_uid: usize,
    pub skipped_not_tty: usize,
    pub skipped_busy_or_closed: usize,
    pub short_writes: usize,
}

/// OS filesystem/tty operations abstracted so unit tests can wrap [`PosixTtyOps`] in a spy and
/// verify that other users' ttys and non-character-device paths are never opened.
pub trait TtyOps {
    fn read_dir_names(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    fn lstat(&self, path: &Path) -> io::Result<TtyStat>;
    fn open_tty(&self, path: &Path) -> io::Result<OwnedFd>;
    fn fstat(&self, fd: &OwnedFd) -> io::Result<TtyStat>;
    fn isatty(&self, fd: &OwnedFd) -> bool;
    fn write_single(&self, fd: &OwnedFd, payload: &[u8]) -> io::Result<usize>;
}

/// Production POSIX implementation backed by `rustix` (`unsafe_code = "forbid"`).
#[derive(Copy, Clone, Debug, Default)]
pub struct PosixTtyOps;

impl TtyOps for PosixTtyOps {
    fn read_dir_names(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        let entries = match fs::read_dir(dir) {
            Ok(it) => it,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            names.push(entry.file_name());
        }
        Ok(names)
    }

    fn lstat(&self, path: &Path) -> io::Result<TtyStat> {
        let stat = lstat(path).map_err(io::Error::from)?;
        Ok(stat_to_tty_stat(&stat))
    }

    fn open_tty(&self, path: &Path) -> io::Result<OwnedFd> {
        let flags =
            OFlags::WRONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        open(path, flags, Mode::empty()).map_err(io::Error::from)
    }

    fn fstat(&self, fd: &OwnedFd) -> io::Result<TtyStat> {
        let stat = fstat(fd).map_err(io::Error::from)?;
        Ok(stat_to_tty_stat(&stat))
    }

    fn isatty(&self, fd: &OwnedFd) -> bool {
        isatty(fd)
    }

    fn write_single(&self, fd: &OwnedFd, payload: &[u8]) -> io::Result<usize> {
        loop {
            match rustix::io::write(fd, payload) {
                Ok(n) => return Ok(n),
                Err(Errno::INTR) => continue,
                Err(err) => return Err(io::Error::from(err)),
            }
        }
    }
}

/// Collects candidate tty paths from `roots` in deterministic order (`DevPts` and `DevTtys`
/// entries are sorted lexicographically; `ExplicitPaths` preserve caller order).
pub fn collect_candidate_paths<O: TtyOps>(roots: &[TtyScanRoot], ops: &O) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        match root {
            TtyScanRoot::DevPts(dir) => {
                if let Ok(names) = ops.read_dir_names(dir) {
                    let mut sorted = BTreeSet::new();
                    for name in names {
                        if is_devpts_replica_name(&name) {
                            sorted.insert(dir.join(name));
                        }
                    }
                    out.extend(sorted);
                }
            }
            TtyScanRoot::DevTtys(dir) => {
                if let Ok(names) = ops.read_dir_names(dir) {
                    let mut sorted = BTreeSet::new();
                    for name in names {
                        if is_darwin_ttys_replica_name(&name) {
                            sorted.insert(dir.join(name));
                        }
                    }
                    out.extend(sorted);
                }
            }
            TtyScanRoot::ExplicitPaths(paths) => {
                out.extend(paths.iter().cloned());
            }
        }
    }
    out
}

/// Delivers `payload` in **one single `write(2)`** per unique tty owned by `expected_uid`.
///
/// Enforces `PLAN.md` §10 T10 (*"Other users' ttys are never opened"*):
/// 1. Pre-open `lstat` rejects symlinks, non-character devices, and any `uid != expected_uid`
///    **before `open(2)` is ever invoked**.
/// 2. `open(2)` uses `O_WRONLY | O_NOCTTY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW`.
/// 3. Post-open `fstat` + `isatty` verify `(dev, ino, rdev, uid, is_char_device)` match before
///    issuing one single `write(2)` syscall.
pub fn deliver_payload_with_ops<O: TtyOps>(
    candidates: &[PathBuf],
    expected_uid: u32,
    payload: &[u8],
    ops: &O,
) -> Result<TtyDeliveryReport, DeliverError> {
    let mut report = TtyDeliveryReport {
        candidates: candidates.len(),
        ..TtyDeliveryReport::default()
    };
    if payload.is_empty() {
        return Ok(report);
    }

    let mut seen_devices: HashSet<(u64, u64, u64)> = HashSet::with_capacity(candidates.len());

    for path in candidates {
        let pre = match ops.lstat(path) {
            Ok(stat) => stat,
            Err(_) => {
                report.skipped_busy_or_closed += 1;
                continue;
            }
        };

        if pre.is_symlink || !pre.is_char_device {
            report.skipped_not_tty += 1;
            continue;
        }
        if pre.uid != expected_uid {
            report.skipped_other_uid += 1;
            continue;
        }
        if !seen_devices.insert((pre.dev, pre.ino, pre.rdev)) {
            continue;
        }

        let fd = match ops.open_tty(path) {
            Ok(fd) => fd,
            Err(_) => {
                report.skipped_busy_or_closed += 1;
                continue;
            }
        };

        let post = match ops.fstat(&fd) {
            Ok(stat) => stat,
            Err(_) => {
                report.skipped_busy_or_closed += 1;
                continue;
            }
        };

        if !post.is_char_device
            || post.dev != pre.dev
            || post.ino != pre.ino
            || post.rdev != pre.rdev
        {
            report.skipped_not_tty += 1;
            continue;
        }
        if post.uid != expected_uid {
            report.skipped_other_uid += 1;
            continue;
        }
        if !ops.isatty(&fd) {
            report.skipped_not_tty += 1;
            continue;
        }

        match ops.write_single(&fd, payload) {
            Ok(written) if written == payload.len() => {
                report.delivered += 1;
            }
            Ok(_) => {
                report.short_writes += 1;
            }
            Err(_) => {
                report.skipped_busy_or_closed += 1;
            }
        }
    }

    Ok(report)
}

/// Opt-in tty broadcast backend enumerating the effective UID's live pseudo-terminals and writing
/// each OSC frame or reset sequence in a single `write(2)` syscall.
#[derive(Debug)]
pub struct TtyBroadcastBackend<O = PosixTtyOps> {
    roots: Vec<TtyScanRoot>,
    expected_uid: u32,
    ops: O,
    last_report: Option<TtyDeliveryReport>,
}

impl TtyBroadcastBackend<PosixTtyOps> {
    #[must_use]
    pub fn new() -> Self {
        Self::with_roots(default_tty_scan_roots())
    }

    #[must_use]
    pub fn with_roots(roots: Vec<TtyScanRoot>) -> Self {
        Self::with_roots_and_ops(roots, geteuid().as_raw(), PosixTtyOps)
    }
}

impl Default for TtyBroadcastBackend<PosixTtyOps> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: TtyOps> TtyBroadcastBackend<O> {
    #[must_use]
    pub const fn with_roots_and_ops(roots: Vec<TtyScanRoot>, expected_uid: u32, ops: O) -> Self {
        Self {
            roots,
            expected_uid,
            ops,
            last_report: None,
        }
    }

    #[must_use]
    pub const fn expected_uid(&self) -> u32 {
        self.expected_uid
    }

    #[must_use]
    pub const fn ops(&self) -> &O {
        &self.ops
    }

    #[must_use]
    pub const fn last_report(&self) -> Option<&TtyDeliveryReport> {
        self.last_report.as_ref()
    }

    /// Lists candidate tty paths matched by filename rules prior to `lstat` ownership checks.
    #[must_use]
    pub fn enumerate_candidates(&self) -> Vec<PathBuf> {
        collect_candidate_paths(&self.roots, &self.ops)
    }

    /// Enumerates unique character-device tty paths owned by `self.expected_uid` using `lstat`
    /// only (never opening any file descriptor).
    #[must_use]
    pub fn enumerate_owned_ttys(&self) -> Vec<PathBuf> {
        let candidates = self.enumerate_candidates();
        let mut seen = HashSet::with_capacity(candidates.len());
        let mut owned = Vec::new();
        for path in candidates {
            let Ok(stat) = self.ops.lstat(&path) else {
                continue;
            };
            if !stat.is_symlink
                && stat.is_char_device
                && stat.uid == self.expected_uid
                && seen.insert((stat.dev, stat.ino, stat.rdev))
            {
                owned.push(path);
            }
        }
        owned
    }

    pub fn deliver_payload(&mut self, payload: &[u8]) -> Result<TtyDeliveryReport, DeliverError> {
        let candidates = self.enumerate_candidates();
        let report = deliver_payload_with_ops(&candidates, self.expected_uid, payload, &self.ops)?;
        self.last_report = Some(report.clone());
        Ok(report)
    }

    pub fn deliver_frame_with_report(
        &mut self,
        frame: &Frame,
    ) -> Result<TtyDeliveryReport, DeliverError> {
        let payload = encode_osc_frame(&frame.colors);
        self.deliver_payload(&payload)
    }

    pub fn reset_with_report(&mut self) -> Result<TtyDeliveryReport, DeliverError> {
        let payload = encode_osc_reset();
        self.deliver_payload(&payload)
    }
}

impl<O: TtyOps> Backend for TtyBroadcastBackend<O> {
    fn name(&self) -> &'static str {
        "tty-broadcast"
    }

    fn deliver(&mut self, frame: &Frame) -> Result<(), DeliverError> {
        self.deliver_frame_with_report(frame).map(|_| ())
    }

    fn reset(&mut self) -> Result<(), DeliverError> {
        self.reset_with_report().map(|_| ())
    }
}
