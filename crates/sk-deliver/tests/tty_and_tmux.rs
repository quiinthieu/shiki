#![cfg(unix)]

#[path = "../../../tests/support/pty.rs"]
mod pty;

use proptest::prelude::*;
use rustix::fd::OwnedFd;
use rustix::process::geteuid;
use sk_core::color::Rgb8;
use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_deliver::osc::{encode_osc_frame, encode_osc_reset, format_osc_rgb};
use sk_deliver::tmux::{
    decode_tmux_dcs_passthrough, encode_tmux_dcs_passthrough, encode_tmux_frame, encode_tmux_reset,
    CliTmuxPaneLister, TmuxBackend, TmuxPaneLister, TMUX_DCS_PREFIX, TMUX_DCS_SUFFIX,
};
use sk_deliver::tty::{
    is_darwin_ttys_replica_name, is_devpts_replica_name, PosixTtyOps, TtyBroadcastBackend, TtyOps,
    TtyScanRoot, TtyStat,
};
use sk_deliver::Backend;
use sk_deliver::DeliverError;
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn escape_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

/// Spy wrapper around [`PosixTtyOps`] recording every `lstat`, `open_tty`, and `write_single`
/// invocation so security tests can prove other users' ttys and non-tty paths are NEVER opened.
#[derive(Default)]
struct SpyTtyOps {
    inner: PosixTtyOps,
    lstat_paths: RefCell<Vec<PathBuf>>,
    opened_paths: RefCell<Vec<PathBuf>>,
    writes: RefCell<Vec<usize>>,
    fstat_uid_override: Option<u32>,
}

impl SpyTtyOps {
    fn new() -> Self {
        Self::default()
    }

    fn with_fstat_uid_override(uid: u32) -> Self {
        Self {
            fstat_uid_override: Some(uid),
            ..Self::default()
        }
    }

    fn opened_paths(&self) -> Vec<PathBuf> {
        self.opened_paths.borrow().clone()
    }

    fn lstat_paths(&self) -> Vec<PathBuf> {
        self.lstat_paths.borrow().clone()
    }

    fn write_lengths(&self) -> Vec<usize> {
        self.writes.borrow().clone()
    }
}

impl TtyOps for SpyTtyOps {
    fn read_dir_names(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.inner.read_dir_names(dir)
    }

    fn lstat(&self, path: &Path) -> io::Result<TtyStat> {
        self.lstat_paths.borrow_mut().push(path.to_path_buf());
        self.inner.lstat(path)
    }

    fn open_tty(&self, path: &Path) -> io::Result<OwnedFd> {
        self.opened_paths.borrow_mut().push(path.to_path_buf());
        self.inner.open_tty(path)
    }

    fn fstat(&self, fd: &OwnedFd) -> io::Result<TtyStat> {
        let mut stat = self.inner.fstat(fd)?;
        if let Some(uid) = self.fstat_uid_override {
            stat.uid = uid;
        }
        Ok(stat)
    }

    fn isatty(&self, fd: &OwnedFd) -> bool {
        self.inner.isatty(fd)
    }

    fn write_single(&self, fd: &OwnedFd, payload: &[u8]) -> io::Result<usize> {
        let written = self.inner.write_single(fd, payload)?;
        self.writes.borrow_mut().push(written);
        Ok(written)
    }
}

struct StaticPaneLister {
    panes: Vec<PathBuf>,
}

impl TmuxPaneLister for StaticPaneLister {
    fn list_pane_ttys(&self) -> Result<Vec<PathBuf>, DeliverError> {
        Ok(self.panes.clone())
    }
}

fn sample_frames() -> (sk_core::palette::Frame, sk_core::palette::Frame) {
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let tokyo = Coordinates::new(35.7, 139.7).unwrap();
    let dawn = compute_frame(
        UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap(),
        tokyo,
        &palette,
        &model,
    );
    let midday = compute_frame(
        UtcInstant::parse_rfc3339("2026-06-22T02:45:00Z").unwrap(),
        tokyo,
        &palette,
        &model,
    );
    (dawn, midday)
}

#[test]
fn tty_replica_filename_filters_accept_devpts_and_darwin_ttys_and_reject_non_pty_entries() {
    for valid_pts in ["0", "1", "9", "42", "1024"] {
        assert!(
            is_devpts_replica_name(OsStr::new(valid_pts)),
            "expected /dev/pts/{valid_pts} to be accepted"
        );
    }
    for rejected_pts in ["", "ptmx", "tty", "tty0", "0a", ".", "..", "pts0"] {
        assert!(
            !is_devpts_replica_name(OsStr::new(rejected_pts)),
            "expected /dev/pts/{rejected_pts} to be rejected"
        );
    }

    for valid_darwin in ["ttys0", "ttys000", "ttys001", "ttys019", "ttys1024"] {
        assert!(
            is_darwin_ttys_replica_name(OsStr::new(valid_darwin)),
            "expected /dev/{valid_darwin} to be accepted"
        );
    }
    for rejected_darwin in [
        "",
        "tty",
        "ttys",
        "tty0",
        "tty1",
        "tty63",
        "tty.Bluetooth-Incoming-Port",
        "cu.usbmodem1",
        "console",
        "ptmx",
        "ptyp0",
        "null",
        "zero",
        "ttys00a",
    ] {
        assert!(
            !is_darwin_ttys_replica_name(OsStr::new(rejected_darwin)),
            "expected /dev/{rejected_darwin} to be rejected"
        );
    }
}

#[test]
fn tty_broadcast_delivers_exact_frame_and_reset_bytes_to_pty_pairs_in_single_write() {
    let (dawn_frame, _) = sample_frames();
    let pty_a = pty::PtyPair::open_raw().expect("open PTY A");
    let pty_b = pty::PtyPair::open_raw().expect("open PTY B");

    // Include `pty_a` twice to verify per-pass `(dev, ino, rdev)` deduplication.
    let roots = vec![TtyScanRoot::ExplicitPaths(vec![
        pty_a.replica_path().to_path_buf(),
        pty_b.replica_path().to_path_buf(),
        pty_a.replica_path().to_path_buf(),
    ])];
    let euid = geteuid().as_raw();
    let mut backend = TtyBroadcastBackend::with_roots_and_ops(roots, euid, SpyTtyOps::new());
    assert_eq!(backend.name(), "tty-broadcast");

    let owned = backend.enumerate_owned_ttys();
    assert_eq!(owned.len(), 2);
    assert!(owned.contains(&pty_a.replica_path().to_path_buf()));
    assert!(owned.contains(&pty_b.replica_path().to_path_buf()));

    let expected_frame_bytes = encode_osc_frame(&dawn_frame.colors);
    let report = backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver frame to PTY pairs");
    assert_eq!(report.delivered, 2);
    assert_eq!(report.skipped_other_uid, 0);
    assert_eq!(report.short_writes, 0);
    assert_eq!(
        backend.ops().opened_paths().len(),
        2,
        "each unique PTY must be opened once per delivery"
    );
    assert_eq!(
        backend.ops().write_lengths(),
        vec![expected_frame_bytes.len(), expected_frame_bytes.len()],
        "entire OSC frame must be delivered in one single write() syscall per PTY"
    );

    let rx_a = pty_a
        .read_exact_with_timeout(expected_frame_bytes.len(), Duration::from_secs(5))
        .expect("read frame from PTY A");
    let rx_b = pty_b
        .read_exact_with_timeout(expected_frame_bytes.len(), Duration::from_secs(5))
        .expect("read frame from PTY B");
    assert_eq!(rx_a, expected_frame_bytes);
    assert_eq!(rx_b, expected_frame_bytes);

    let expected_reset_bytes = encode_osc_reset();
    let reset_report = backend
        .reset_with_report()
        .expect("deliver reset to PTY pairs");
    assert_eq!(reset_report.delivered, 2);
    assert_eq!(
        backend.ops().write_lengths(),
        vec![
            expected_frame_bytes.len(),
            expected_frame_bytes.len(),
            expected_reset_bytes.len(),
            expected_reset_bytes.len(),
        ],
        "reset must also be delivered in one single write() syscall per PTY"
    );

    let reset_a = pty_a
        .read_exact_with_timeout(expected_reset_bytes.len(), Duration::from_secs(5))
        .expect("read reset from PTY A");
    let reset_b = pty_b
        .read_exact_with_timeout(expected_reset_bytes.len(), Duration::from_secs(5))
        .expect("read reset from PTY B");
    assert_eq!(reset_a, expected_reset_bytes);
    assert_eq!(reset_b, expected_reset_bytes);
}

#[test]
fn other_users_ttys_and_non_char_devices_are_never_opened() {
    let (dawn_frame, _) = sample_frames();
    let owned_pty = pty::PtyPair::open_raw().expect("open owned PTY");
    let euid = geteuid().as_raw();

    // Build a synthetic `/dev/pts`-style directory containing filesystem traps owned by `euid`:
    // - `0`: regular file
    // - `1`: symlink pointing to our real `owned_pty` replica
    // - `2`: FIFO named pipe (`mkfifo`)
    // - `ptmx`: multiplexor name (must be filtered out before `lstat` or `open`)
    let fake_devpts = tempfile::tempdir().expect("create fake devpts dir");
    let reg_path = fake_devpts.path().join("0");
    fs::write(&reg_path, b"not-a-tty").expect("create regular file");

    let sym_path = fake_devpts.path().join("1");
    symlink(owned_pty.replica_path(), &sym_path).expect("create symlink to pty");

    let fifo_path = fake_devpts.path().join("2");
    let mkfifo_status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("run mkfifo");
    assert!(mkfifo_status.success(), "mkfifo must succeed");

    let ptmx_path = fake_devpts.path().join("ptmx");
    fs::write(&ptmx_path, b"ptmx").expect("create ptmx trap");

    // Combine `fake_devpts` with real root-owned (`uid 0`) world-writable (`0666`) character
    // devices `/dev/null` and `/dev/tty`, plus our real `owned_pty`. Without pre-open `lstat`
    // UID verification, `open("/dev/null", O_WRONLY)` would succeed!
    let dev_null = PathBuf::from("/dev/null");
    let dev_tty = PathBuf::from("/dev/tty");
    let roots = vec![
        TtyScanRoot::DevPts(fake_devpts.path().to_path_buf()),
        TtyScanRoot::ExplicitPaths(vec![
            dev_null.clone(),
            dev_tty.clone(),
            owned_pty.replica_path().to_path_buf(),
        ]),
    ];

    let mut backend = TtyBroadcastBackend::with_roots_and_ops(roots, euid, SpyTtyOps::new());
    let report = backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver with mixed targets");

    // `ptmx` must never even reach `lstat`.
    assert!(
        !backend.ops().lstat_paths().contains(&ptmx_path),
        "ptmx must be excluded by filename filter before lstat"
    );

    // CRITICAL AC ("Other users' ttys are never opened"):
    // Only `owned_pty.replica_path()` may ever be passed to `open_tty`!
    assert_eq!(
        backend.ops().opened_paths(),
        vec![owned_pty.replica_path().to_path_buf()],
        "other-UID devices (/dev/null, /dev/tty), symlinks, FIFOs, and regular files must NEVER be opened"
    );
    assert_eq!(report.delivered, 1);
    assert_eq!(report.skipped_other_uid, 2);
    assert_eq!(report.skipped_not_tty, 3);

    let expected = encode_osc_frame(&dawn_frame.colors);
    let rx = owned_pty
        .read_exact_with_timeout(expected.len(), Duration::from_secs(5))
        .expect("read exact frame on owned PTY");
    assert_eq!(rx, expected);

    // Second pass: treat `owned_pty` itself as belonging to another user (`expected_uid = euid + 1`).
    // Even though the current process has kernel permission to open `owned_pty`, pre-open `lstat`
    // sees `stat.uid != expected_uid` and MUST NEVER call `open_tty`!
    let other_uid = euid.wrapping_add(1);
    let mut mismatched_uid_backend = TtyBroadcastBackend::with_roots_and_ops(
        vec![TtyScanRoot::ExplicitPaths(vec![owned_pty
            .replica_path()
            .to_path_buf()])],
        other_uid,
        SpyTtyOps::new(),
    );
    assert!(
        mismatched_uid_backend.enumerate_owned_ttys().is_empty(),
        "mismatched UID pty must not appear in enumerate_owned_ttys"
    );
    let mismatch_report = mismatched_uid_backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver with mismatched expected_uid");
    assert_eq!(mismatch_report.delivered, 0);
    assert_eq!(mismatch_report.skipped_other_uid, 1);
    assert!(
        mismatched_uid_backend.ops().opened_paths().is_empty(),
        "a real PTY owned by another UID must NEVER be opened"
    );

    // Third pass: simulate a TOCTOU race where `lstat` matched `euid` but post-open `fstat`
    // observes `other_uid` before `write_single` runs.
    let mut toctou_backend = TtyBroadcastBackend::with_roots_and_ops(
        vec![TtyScanRoot::ExplicitPaths(vec![owned_pty
            .replica_path()
            .to_path_buf()])],
        euid,
        SpyTtyOps::with_fstat_uid_override(other_uid),
    );
    let toctou_report = toctou_backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver under simulated TOCTOU uid change");
    assert_eq!(toctou_report.delivered, 0);
    assert_eq!(toctou_report.skipped_other_uid, 1);
    assert!(
        toctou_backend.ops().write_lengths().is_empty(),
        "post-open fstat UID mismatch must abort before writing any bytes"
    );
}

#[test]
fn tmux_dcs_passthrough_and_combined_payloads_match_golden_snapshots() {
    let (dawn_frame, _) = sample_frames();
    let raw_frame = encode_osc_frame(&dawn_frame.colors);
    let raw_reset = encode_osc_reset();

    assert_eq!(encode_tmux_frame(&dawn_frame.colors, false), raw_frame);
    assert_eq!(encode_tmux_reset(false), raw_reset);

    let dcs_frame = encode_tmux_dcs_passthrough(&raw_frame);
    assert!(dcs_frame.starts_with(TMUX_DCS_PREFIX));
    assert!(dcs_frame.ends_with(TMUX_DCS_SUFFIX));
    assert_eq!(
        decode_tmux_dcs_passthrough(&dcs_frame).as_deref(),
        Some(raw_frame.as_slice())
    );

    let combined_frame = encode_tmux_frame(&dawn_frame.colors, true);
    assert_eq!(combined_frame.len(), raw_frame.len() + dcs_frame.len());
    assert!(combined_frame.starts_with(&raw_frame));
    assert_eq!(&combined_frame[raw_frame.len()..], dcs_frame.as_slice());

    let combined_reset = encode_tmux_reset(true);
    let dcs_reset = encode_tmux_dcs_passthrough(&raw_reset);
    assert_eq!(combined_reset.len(), raw_reset.len() + dcs_reset.len());
    assert_eq!(
        decode_tmux_dcs_passthrough(&combined_reset[raw_reset.len()..]).as_deref(),
        Some(raw_reset.as_slice())
    );

    let reset_snapshot = format!(
        "total_byte_len = {}\nraw_byte_len = {}\ndcs_byte_len = {}\nraw = {}\ndcs = {}",
        combined_reset.len(),
        raw_reset.len(),
        dcs_reset.len(),
        escape_ascii(&raw_reset),
        escape_ascii(&dcs_reset),
    );
    insta::assert_snapshot!("tmux_combined_reset_bytes", reset_snapshot);

    let sample_single_osc = format!("\x1b]11;{}\x1b\\", format_osc_rgb(dawn_frame.colors.bg));
    let wrapped_single_osc = encode_tmux_dcs_passthrough(sample_single_osc.as_bytes());
    let frame_snapshot = format!(
        "combined_frame_byte_len = {}\nraw_frame_byte_len = {}\ndcs_frame_byte_len = {}\nsample_bg_dcs = {}",
        combined_frame.len(),
        raw_frame.len(),
        dcs_frame.len(),
        escape_ascii(&wrapped_single_osc),
    );
    insta::assert_snapshot!("tmux_combined_frame_summary", frame_snapshot);

    // Verify `TmuxBackend` delivers the entire combined buffer in 1 single write while refusing
    // root-owned `/dev/null`, and gracefully handles missing `tmux` binary or stopped server.
    let pane_pty = pty::PtyPair::open_raw().expect("open simulated tmux pane PTY");
    let lister = StaticPaneLister {
        panes: vec![
            pane_pty.replica_path().to_path_buf(),
            PathBuf::from("/dev/null"),
        ],
    };
    let euid = geteuid().as_raw();
    let mut tmux_backend = TmuxBackend::with_lister_and_ops(true, euid, lister, SpyTtyOps::new());
    assert_eq!(tmux_backend.name(), "tmux");
    assert!(tmux_backend.passthrough_outer());

    let report = tmux_backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver tmux frame");
    assert_eq!(report.delivered, 1);
    assert_eq!(report.skipped_other_uid, 1);
    assert_eq!(
        tmux_backend.ops().opened_paths(),
        vec![pane_pty.replica_path().to_path_buf()]
    );
    assert_eq!(
        tmux_backend.ops().write_lengths(),
        vec![combined_frame.len()],
        "combined raw OSC + DCS passthrough must be written in one single write() call"
    );

    let rx = pane_pty
        .read_exact_with_timeout(combined_frame.len(), Duration::from_secs(5))
        .expect("read combined tmux frame");
    assert_eq!(rx, combined_frame);

    // Missing `tmux` binary or non-running socket returns `0` panes cleanly without error.
    let missing_bin = CliTmuxPaneLister::with_program("/nonexistent/shiki-missing-tmux");
    assert!(missing_bin.list_pane_ttys().unwrap().is_empty());

    let tmp_sock_dir = tempfile::tempdir().unwrap();
    let dead_sock = CliTmuxPaneLister::with_socket_path(tmp_sock_dir.path().join("no-server.sock"));
    assert!(dead_sock.list_pane_ttys().unwrap().is_empty());
}

proptest! {
    #[test]
    fn tmux_dcs_passthrough_doubles_every_inner_esc_and_roundtrips(
        payload in prop::collection::vec(
            prop_oneof![
                Just(0x1b_u8),
                Just(b'\\'),
                Just(0x07_u8),
                Just(b';'),
                any::<u8>(),
            ],
            0..512
        )
    ) {
        let encoded = encode_tmux_dcs_passthrough(&payload);
        prop_assert!(encoded.starts_with(TMUX_DCS_PREFIX));
        prop_assert!(encoded.ends_with(TMUX_DCS_SUFFIX));

        let body = &encoded[TMUX_DCS_PREFIX.len()..encoded.len() - TMUX_DCS_SUFFIX.len()];
        let inner_esc_count = payload.iter().filter(|&&b| b == 0x1b).count();
        prop_assert_eq!(body.len(), payload.len() + inner_esc_count);

        // Verify zero un-doubled ESC bytes exist inside `body`.
        let mut idx = 0;
        while idx < body.len() {
            if body[idx] == 0x1b {
                prop_assert!(idx + 1 < body.len() && body[idx + 1] == 0x1b);
                idx += 2;
            } else {
                idx += 1;
            }
        }

        prop_assert_eq!(decode_tmux_dcs_passthrough(&encoded), Some(payload));
    }
}

fn find_tmux_binary() -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("tmux");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    [
        "/usr/bin/tmux",
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn contains_sgr_truecolor(haystack: &[u8], prefix: u8, rgb: Rgb8) -> bool {
    let variants = [
        format!("{prefix};2;{};{};{}", rgb.r, rgb.g, rgb.b),
        format!("{prefix}:2:{}:{}:{}", rgb.r, rgb.g, rgb.b),
        format!("{prefix}:2::{}:{}:{}", rgb.r, rgb.g, rgb.b),
    ];
    variants
        .iter()
        .any(|pat| contains_subslice(haystack, pat.as_bytes()))
}

struct TmuxServerGuard {
    tmux_bin: PathBuf,
    sock_path: PathBuf,
    _sock_dir: tempfile::TempDir,
}

impl TmuxServerGuard {
    fn run_ctl(&self, args: &[&str]) {
        let out = Command::new(&self.tmux_bin)
            .arg("-S")
            .arg(&self.sock_path)
            .args(args)
            .output()
            .expect("invoke tmux control command");
        assert!(
            out.status.success(),
            "`tmux -S {} {}` failed ({}): {}",
            self.sock_path.display(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux_bin)
            .arg("-S")
            .arg(&self.sock_path)
            .arg("kill-server")
            .output();
    }
}

#[test]
fn real_tmux_pane_changes_colour_and_forwards_dcs_passthrough() {
    let Some(tmux_bin) = find_tmux_binary() else {
        if cfg!(target_os = "linux") && std::env::var_os("CI").is_some() {
            panic!("tmux must be installed on Linux CI runners to verify T10 tmux AC");
        }
        eprintln!("skipping real_tmux_pane_changes_colour_and_forwards_dcs_passthrough: tmux not installed");
        return;
    };

    let (dawn_frame, midday_frame) = sample_frames();

    // Use `/tmp` so the Unix domain socket path stays well below the 104-byte `sun_path` limit.
    let sock_base = Path::new("/tmp");
    let sock_dir = if sock_base.is_dir() {
        tempfile::Builder::new()
            .prefix("sk-t-")
            .tempdir_in(sock_base)
            .expect("create short /tmp socket dir")
    } else {
        tempfile::Builder::new()
            .prefix("sk-t-")
            .tempdir()
            .expect("create temp socket dir")
    };
    let sock_path = sock_dir.path().join("t.sock");
    let guard = TmuxServerGuard {
        tmux_bin: tmux_bin.clone(),
        sock_path: sock_path.clone(),
        _sock_dir: sock_dir,
    };

    let mut outer_pty = pty::PtyPair::open_raw().expect("open outer client PTY for tmux");
    let mut tmux_cmd = Command::new(&tmux_bin);
    tmux_cmd
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env_remove("TMUX")
        .arg("-S")
        .arg(&sock_path)
        .args(["-f", "/dev/null", "-T", "RGB", "new-session", "/bin/sh"]);

    let mut tmux_client = outer_pty
        .spawn_session_command(tmux_cmd)
        .expect("spawn isolated tmux client + server on outer PTY");

    outer_pty
        .write_input(b"PS1=''; printf 'SHIKI_TMUX_READY\\n'\n")
        .expect("send ready marker command to tmux pane");
    let _startup = outer_pty
        .read_until(b"SHIKI_TMUX_READY", Duration::from_secs(10))
        .expect("wait for tmux pane shell startup");

    // Phase 1: Per-pane OSC update (`allow-passthrough` is tmux's default `off`,
    // `passthrough_outer = false`).
    let lister = CliTmuxPaneLister::with_socket_path(&sock_path).with_program_path(&tmux_bin);
    let panes = lister.list_pane_ttys().expect("list live tmux pane ttys");
    assert_eq!(
        panes.len(),
        1,
        "expected 1 isolated tmux pane, got {panes:?}"
    );

    let mut backend =
        TmuxBackend::with_lister_and_ops(false, geteuid().as_raw(), lister, PosixTtyOps);
    let report1 = backend
        .deliver_frame_with_report(&dawn_frame)
        .expect("deliver dawn frame to live tmux pane");
    assert_eq!(report1.delivered, 1);

    // Force a client refresh and emit ANSI slot 1 (`\033[31m`) + default fg/bg text inside the pane
    // so tmux renders the updated per-pane palette (`wp->palette`) onto `outer_pty`.
    guard.run_ctl(&["refresh-client"]);
    outer_pty
        .write_input(b"printf '\\033[31mSHIKI_PHASE_ONE_DONE\\033[0m\\n'\n")
        .expect("print marker after per-pane OSC delivery");
    let phase1_out = outer_pty
        .read_until(b"SHIKI_PHASE_ONE_DONE", Duration::from_secs(10))
        .expect("read tmux client redraw after Phase 1");

    assert!(
        contains_sgr_truecolor(&phase1_out, 48, dawn_frame.colors.bg),
        "tmux client output must render updated pane bg {:?} as 24-bit SGR, got:\n{}",
        dawn_frame.colors.bg,
        escape_ascii(&phase1_out)
    );
    assert!(
        contains_sgr_truecolor(&phase1_out, 38, dawn_frame.colors.ansi[1])
            || contains_sgr_truecolor(&phase1_out, 38, dawn_frame.colors.fg),
        "tmux client output must render updated pane ANSI 1 ({:?}) or fg ({:?}) as 24-bit SGR, got:\n{}",
        dawn_frame.colors.ansi[1],
        dawn_frame.colors.fg,
        escape_ascii(&phase1_out)
    );
    let dawn_osc_bg = format!("\x1b]11;{}\x1b\\", format_osc_rgb(dawn_frame.colors.bg));
    assert!(
        !contains_subslice(&phase1_out, dawn_osc_bg.as_bytes()),
        "with passthrough_outer=false and allow-passthrough=off, raw OSC 11 must not reach outer PTY"
    );

    // Phase 2: Enable `allow-passthrough on` in tmux and `passthrough_outer = true` in TmuxBackend.
    // One single-write delivery updates BOTH tmux's pane palette AND forwards unwrapped OSC bytes
    // through tmux to the outer terminal!
    guard.run_ctl(&["set-option", "-g", "allow-passthrough", "on"]);
    backend.set_passthrough_outer(true);
    let report2 = backend
        .deliver_frame_with_report(&midday_frame)
        .expect("deliver midday frame with DCS passthrough");
    assert_eq!(report2.delivered, 1);

    guard.run_ctl(&["refresh-client"]);
    outer_pty
        .write_input(b"printf 'SHIKI_PHASE_TWO_DONE\\n'\n")
        .expect("print marker after DCS passthrough delivery");
    let phase2_out = outer_pty
        .read_until(b"SHIKI_PHASE_TWO_DONE", Duration::from_secs(10))
        .expect("read outer PTY output after Phase 2");

    let midday_osc_bg = format!("\x1b]11;{}\x1b\\", format_osc_rgb(midday_frame.colors.bg));
    let midday_osc_fg = format!("\x1b]10;{}\x1b\\", format_osc_rgb(midday_frame.colors.fg));
    let midday_osc_cursor = format!(
        "\x1b]12;{}\x1b\\",
        format_osc_rgb(midday_frame.colors.cursor)
    );
    assert!(
        contains_subslice(&phase2_out, midday_osc_bg.as_bytes()),
        "outer PTY must receive unwrapped OSC 11 via tmux DCS passthrough, got:\n{}",
        escape_ascii(&phase2_out)
    );
    assert!(
        contains_subslice(&phase2_out, midday_osc_fg.as_bytes()),
        "outer PTY must receive unwrapped OSC 10 via tmux DCS passthrough"
    );
    assert!(
        contains_subslice(&phase2_out, midday_osc_cursor.as_bytes()),
        "outer PTY must receive unwrapped OSC 12 via tmux DCS passthrough"
    );
    assert!(
        contains_sgr_truecolor(&phase2_out, 48, midday_frame.colors.bg),
        "tmux pane's own rendered bg must also update to midday bg {:?}",
        midday_frame.colors.bg
    );

    // Phase 3: Reset with passthrough_outer = true forwards OSC 104/110/111/112 to outer PTY.
    let reset_report = backend
        .reset_with_report()
        .expect("deliver tmux reset with passthrough");
    assert_eq!(reset_report.delivered, 1);
    outer_pty
        .write_input(b"printf 'SHIKI_PHASE_THREE_DONE\\n'\n")
        .expect("print marker after tmux reset");
    let phase3_out = outer_pty
        .read_until(b"SHIKI_PHASE_THREE_DONE", Duration::from_secs(10))
        .expect("read outer PTY output after reset");
    assert!(
        contains_subslice(&phase3_out, &encode_osc_reset()),
        "outer PTY must receive unwrapped OSC 104/110/111/112 reset via tmux DCS passthrough"
    );

    // Cleanly exit the pane shell so the attached tmux client exits `0`.
    outer_pty.write_input(b"exit\n").expect("exit pane shell");
    let status = tmux_client
        .wait_with_timeout(Duration::from_secs(5))
        .expect("wait for tmux client exit");
    assert!(status.success());
}
