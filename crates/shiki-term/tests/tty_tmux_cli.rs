//! CLI & daemon integration tests for `backend list|enable|disable`, `$TMUX` passthrough in
//! `apply`/`reset`, and live `daemon run` tty broadcast (`PLAN.md` §5, §6, §10 T10).

#![cfg(unix)]

#[path = "../../../tests/support/pty.rs"]
mod pty;
#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use rustix::process::{kill_process, Pid, Signal};
use serde_json::Value;
use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_deliver::osc::{encode_osc_frame, encode_osc_reset};
use sk_deliver::tmux::{encode_tmux_frame, encode_tmux_reset};
use sk_store::store::probe_running_daemon_pid;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::TestEnv;

const ALT_PALETTE_TOML: &str = r##"name = "alt"
description = "Constant amethyst nocturnal test palette"

[[anchors]]
d = 0.00
s = 0.00
half = "both"
bg = "#1c1326"
fg = "#f4edf9"
cursor = "#f2c17e"
selection_bg = "#352647"
selection_fg = "#ffffff"
ansi = [
  "#261a33", "#ef7d88", "#7fd4a4", "#eed084",
  "#78bcf4", "#d694ec", "#6accd2", "#dfd6e8",
  "#716282", "#f698a1", "#9ce4bb", "#f7df9d",
  "#94ccf9", "#e4aef5", "#89dce1", "#fbf8fe",
]

[[anchors]]
d = 1.00
s = 0.00
half = "both"
bg = "#1c1326"
fg = "#f4edf9"
cursor = "#f2c17e"
selection_bg = "#352647"
selection_fg = "#ffffff"
ansi = [
  "#261a33", "#ef7d88", "#7fd4a4", "#eed084",
  "#78bcf4", "#d694ec", "#6accd2", "#dfd6e8",
  "#716282", "#f698a1", "#9ce4bb", "#f7df9d",
  "#94ccf9", "#e4aef5", "#89dce1", "#fbf8fe",
]
"##;

fn shiki_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_shiki-term"))
}

fn shiki_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::new(shiki_bin_path());
    cmd.envs(env.env_vars());
    cmd.env("SHIKI_NO_SERVICE_MANAGER", "1");
    cmd.env_remove("TMUX");
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("COLORTERM");
    cmd
}

#[test]
fn backend_list_enable_and_disable_roundtrip_and_never_leak_coordinates() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let default_list = shiki_cmd(&env)
        .args(["backend", "list"])
        .output()
        .expect("run backend list");
    assert!(default_list.status.success());
    let default_text = String::from_utf8(default_list.stdout).unwrap();
    assert!(!default_text.contains("35.7") && !default_text.contains("139.7"));
    insta::assert_snapshot!("backend_list_defaults", default_text);

    // Passing `--passthrough` on `tty-broadcast` is rejected cleanly.
    let bad_flag = shiki_cmd(&env)
        .args(["backend", "enable", "tty-broadcast", "--passthrough"])
        .output()
        .expect("reject --passthrough on tty-broadcast");
    assert!(!bad_flag.status.success());

    // Enable `tty-broadcast` and `tmux --passthrough`.
    shiki_cmd(&env)
        .args(["backend", "enable", "tty-broadcast"])
        .assert()
        .success();
    shiki_cmd(&env)
        .args(["backend", "enable", "tmux", "--passthrough"])
        .assert()
        .success();

    let json_list = shiki_cmd(&env)
        .args(["backend", "list", "--json"])
        .output()
        .expect("run backend list --json");
    assert!(json_list.status.success());
    let json_str = String::from_utf8(json_list.stdout).unwrap();
    assert!(!json_str.contains("35.7") && !json_str.contains("139.7"));
    let parsed: Value = serde_json::from_str(&json_str).expect("valid backend list JSON");
    let arr = parsed["backends"].as_array().expect("backends array");
    let tty_entry = arr
        .iter()
        .find(|b| b["name"] == "tty-broadcast")
        .expect("tty-broadcast entry");
    assert_eq!(tty_entry["enabled"], true);
    let tmux_entry = arr
        .iter()
        .find(|b| b["name"] == "tmux")
        .expect("tmux entry");
    assert_eq!(tmux_entry["enabled"], true);
    assert_eq!(tmux_entry["passthrough"], true);

    // Toggle tmux passthrough off while keeping tmux enabled.
    shiki_cmd(&env)
        .args(["backend", "enable", "tmux", "--no-passthrough"])
        .assert()
        .success();
    let passthrough_val = shiki_cmd(&env)
        .args(["config", "get", "delivery.tmux_passthrough"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(passthrough_val.stdout).unwrap(),
        "false\n"
    );
    let tmux_val = shiki_cmd(&env)
        .args(["config", "get", "delivery.tmux"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(tmux_val.stdout).unwrap(), "true\n");

    // Disable both backends and verify `backend list` returns to defaults.
    shiki_cmd(&env)
        .args(["backend", "disable", "tty-broadcast"])
        .assert()
        .success();
    shiki_cmd(&env)
        .args(["backend", "disable", "tmux"])
        .assert()
        .success();

    let after_disable = shiki_cmd(&env)
        .args(["backend", "list"])
        .output()
        .expect("backend list after disable");
    assert_eq!(
        String::from_utf8(after_disable.stdout).unwrap(),
        default_text
    );
}

#[test]
fn apply_and_reset_inside_tmux_emit_combined_dcs_passthrough_when_enabled() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let expected_frame = compute_frame(
        UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap(),
        Coordinates::new(35.7, 139.7).unwrap(),
        &Palette::bundled_shiki(),
        &ModelConfig::bundled(),
    );
    let raw_osc = encode_osc_frame(&expected_frame.colors);
    let combined_tmux_osc = encode_tmux_frame(&expected_frame.colors, true);

    // 1. Inside `$TMUX` with `delivery.tmux_passthrough = false` (default): raw OSC only.
    let mut pty_default = pty::PtyPair::open_raw().expect("open PTY default");
    let mut cmd_default = StdCommand::new(shiki_bin_path());
    cmd_default
        .envs(env.env_vars())
        .env("TMUX", "/tmp/tmux-1000/default,4242,0")
        .args(["apply", "--time", "2026-03-20T20:35:00Z"]);
    let mut child_default = pty_default
        .spawn_command(cmd_default)
        .expect("spawn apply default");
    let rx_default = pty_default
        .read_exact_with_timeout(raw_osc.len(), Duration::from_secs(5))
        .expect("read raw OSC");
    assert!(child_default
        .wait_with_timeout(Duration::from_secs(5))
        .unwrap()
        .success());
    assert_eq!(rx_default, raw_osc);

    // 2. Enable `tmux --passthrough`: inside `$TMUX`, `apply` emits combined raw OSC + DCS
    // passthrough (`813` bytes), while `current.seq` still stores pure `383`-byte OSC for hooks.
    shiki_cmd(&env)
        .args(["backend", "enable", "tmux", "--passthrough"])
        .assert()
        .success();

    let mut pty_tmux = pty::PtyPair::open_raw().expect("open PTY tmux");
    let mut cmd_tmux = StdCommand::new(shiki_bin_path());
    cmd_tmux
        .envs(env.env_vars())
        .env("TMUX", "/tmp/tmux-1000/default,4242,0")
        .args(["apply", "--time", "2026-03-20T20:35:00Z"]);
    let mut child_tmux = pty_tmux
        .spawn_command(cmd_tmux)
        .expect("spawn apply inside TMUX");
    let rx_tmux = pty_tmux
        .read_exact_with_timeout(combined_tmux_osc.len(), Duration::from_secs(5))
        .expect("read combined tmux OSC + DCS");
    assert!(child_tmux
        .wait_with_timeout(Duration::from_secs(5))
        .unwrap()
        .success());
    assert_eq!(rx_tmux, combined_tmux_osc);
    assert_eq!(
        fs::read(env.paths().current_seq_file()).unwrap(),
        raw_osc,
        "current.seq must remain pure OSC so non-tmux shell hooks never emit DCS tmux;"
    );

    // 3. Outside `$TMUX`, even when `delivery.tmux_passthrough = true`, `apply` emits raw OSC.
    let mut pty_outside = pty::PtyPair::open_raw().expect("open PTY outside TMUX");
    let mut cmd_outside = StdCommand::new(shiki_bin_path());
    cmd_outside.envs(env.env_vars()).env_remove("TMUX").args([
        "apply",
        "--time",
        "2026-03-20T20:35:00Z",
    ]);
    let mut child_outside = pty_outside
        .spawn_command(cmd_outside)
        .expect("spawn apply outside TMUX");
    let rx_outside = pty_outside
        .read_exact_with_timeout(raw_osc.len(), Duration::from_secs(5))
        .expect("read raw OSC outside TMUX");
    assert!(child_outside
        .wait_with_timeout(Duration::from_secs(5))
        .unwrap()
        .success());
    assert_eq!(rx_outside, raw_osc);

    // 4. Inside `$TMUX` with `delivery.tmux_passthrough = true`, `reset` emits combined reset
    // (`73` bytes), and outside `$TMUX` emits standard `28`-byte reset.
    let expected_tmux_reset = encode_tmux_reset(true);
    let mut pty_reset = pty::PtyPair::open_raw().expect("open PTY for tmux reset");
    let mut cmd_reset = StdCommand::new(shiki_bin_path());
    cmd_reset
        .envs(env.env_vars())
        .env("TMUX", "/tmp/tmux-1000/default,4242,0")
        .arg("reset");
    let mut child_reset = pty_reset
        .spawn_command(cmd_reset)
        .expect("spawn reset inside TMUX");
    let rx_reset = pty_reset
        .read_exact_with_timeout(expected_tmux_reset.len(), Duration::from_secs(5))
        .expect("read combined tmux reset");
    assert!(child_reset
        .wait_with_timeout(Duration::from_secs(5))
        .unwrap()
        .success());
    assert_eq!(rx_reset, expected_tmux_reset);

    let out_reset = shiki_cmd(&env).arg("reset").output().unwrap();
    assert_eq!(out_reset.stdout, encode_osc_reset());
}

struct DaemonChildGuard {
    child: Option<Child>,
}

impl DaemonChildGuard {
    fn terminate_cleanly(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(raw_pid) = i32::try_from(child.id()) {
            if let Some(pid) = Pid::from_raw(raw_pid) {
                let _ = kill_process(pid, Signal::TERM);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                assert!(status.success(), "daemon must exit 0 on SIGTERM: {status}");
                return;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("daemon did not exit within 5s of SIGTERM");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DaemonChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn daemon_run_delivers_live_osc_frames_to_pty_on_startup_and_sighup_reload() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();
    shiki_cmd(&env)
        .args(["backend", "enable", "tty-broadcast"])
        .assert()
        .success();

    fs::create_dir_all(env.paths().palettes_dir()).unwrap();
    fs::write(
        env.paths().palettes_dir().join("alt.toml"),
        ALT_PALETTE_TOML,
    )
    .unwrap();

    let target_pty = pty::PtyPair::open_raw().expect("open target PTY for daemon broadcast");
    let tty_paths_env = format!("{}:/dev/null", target_pty.replica_path().display());

    let mut daemon_cmd = StdCommand::new(shiki_bin_path());
    daemon_cmd
        .envs(env.env_vars())
        .env("SHIKI_NO_SERVICE_MANAGER", "1")
        .env("SHIKI_TTY_PATHS", &tty_paths_env)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = daemon_cmd.spawn().expect("spawn shiki-term daemon run");
    let mut guard = DaemonChildGuard { child: Some(child) };

    // Wait for the daemon to acquire `daemon.pid` and broadcast the initial `shiki` frame (`383` bytes).
    let initial_rx = target_pty
        .read_exact_with_timeout(383, Duration::from_secs(5))
        .expect("read initial daemon broadcast frame from PTY");
    let deadline = Instant::now() + Duration::from_secs(5);
    while probe_running_daemon_pid(env.paths()).unwrap().is_none() {
        assert!(Instant::now() < deadline, "daemon pid lock not observed");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        initial_rx,
        fs::read(env.paths().current_seq_file()).unwrap(),
        "initial PTY broadcast must match current.seq byte-for-byte"
    );

    // Switching palette to `alt` via `config set` signals the sleeping daemon via `SIGHUP`,
    // causing an immediate single-write broadcast of the amethyst `alt` frame to `target_pty`!
    shiki_cmd(&env)
        .args(["config", "set", "palette", "alt"])
        .assert()
        .success();

    let reload_rx = target_pty
        .read_exact_with_timeout(383, Duration::from_secs(5))
        .expect("read SIGHUP-triggered alt palette broadcast from PTY");
    assert!(
        reload_rx.starts_with(b"\x1b]11;rgb:1c/13/26\x1b\\"),
        "expected alt bg #1c1326 on PTY after SIGHUP reload, got {:?}",
        String::from_utf8_lossy(&reload_rx)
    );
    assert_eq!(reload_rx, fs::read(env.paths().current_seq_file()).unwrap());

    guard.terminate_cleanly();
}
