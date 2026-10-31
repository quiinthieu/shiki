//! CLI integration, round-trip, golden, privacy, and live signal-waking daemon tests (`PLAN.md` §6, §7, §8, §10 T09).

#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use rustix::process::{kill_process, Pid, Signal};
use serde_json::Value;
use sk_core::product;
use sk_store::store::{load_snapshot, load_state, INDEFINITE_PAUSE};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::{workspace_root, TestEnv};

const DAEMON_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

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
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("COLORTERM");
    cmd.env("TERM", "vt100");
    cmd
}

/// Recursively records every directory (`None`) and file (`Some(bytes)`) under `root`, keyed by
/// relative path from `root`, for byte-exact and directory-exact round-trip assertions.
fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .map(|e| e.expect("dir entry"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .expect("relative path")
                .to_path_buf();
            if path.is_dir() {
                out.insert(rel, None);
                walk(base, &path, out);
            } else {
                let bytes =
                    fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                out.insert(rel, Some(bytes));
            }
        }
    }

    let mut out = BTreeMap::new();
    if root.exists() {
        walk(root, root, &mut out);
    }
    out
}

#[cfg(target_os = "macos")]
fn assert_plutil_lint_passes(plist_path: &Path) {
    let out = StdCommand::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(plist_path)
        .output()
        .expect("run /usr/bin/plutil -lint");
    assert!(
        out.status.success(),
        "plutil -lint failed on {}:\nstdout: {}\nstderr: {}",
        plist_path.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn daemon_unit_files_match_golden_snapshots_packaging_templates_and_plutil() {
    let env = TestEnv::new();

    // Pre-create standard unit directories so the clean reference units (`created_dirs == 0`) are
    // compared byte-for-byte against `packaging/launchd/` and `packaging/systemd/`.
    let launch_agents_dir = env.home_dir().join("Library").join("LaunchAgents");
    let systemd_user_dir = env.xdg_config_home().join("systemd").join("user");
    fs::create_dir_all(&launch_agents_dir).unwrap();
    fs::create_dir_all(&systemd_user_dir).unwrap();

    // 1. macOS LaunchAgent (`$HOME/Library/LaunchAgents/<BUNDLE_ID>.plist`).
    shiki_cmd(&env)
        .args([
            "daemon",
            "install",
            "--platform",
            "macos",
            "--exe-path",
            "/usr/local/bin/shiki-term",
            "--no-activate",
        ])
        .assert()
        .success();

    let expected_plist_path = launch_agents_dir.join(format!("{}.plist", product::BUNDLE_ID));
    assert!(expected_plist_path.is_file());
    let plist_text = fs::read_to_string(&expected_plist_path).unwrap();
    insta::assert_snapshot!("launchd_user_agent_plist", plist_text);

    let packaged_plist = fs::read_to_string(
        workspace_root()
            .join("packaging")
            .join("launchd")
            .join(format!("{}.plist", product::BUNDLE_ID)),
    )
    .expect("read packaging/launchd plist");
    assert_eq!(
        plist_text, packaged_plist,
        "packaging/launchd template must match generated launchd plist byte-for-byte"
    );

    #[cfg(target_os = "macos")]
    assert_plutil_lint_passes(&expected_plist_path);

    // 2. Linux systemd user service (`$XDG_CONFIG_HOME/systemd/user/<CLI_BIN>.service`).
    shiki_cmd(&env)
        .args([
            "daemon",
            "install",
            "--platform",
            "linux",
            "--exe-path",
            "/usr/local/bin/shiki-term",
            "--no-activate",
        ])
        .assert()
        .success();

    let expected_service_path = systemd_user_dir.join(format!("{}.service", product::CLI_BIN));
    assert!(expected_service_path.is_file());
    let service_text = fs::read_to_string(&expected_service_path).unwrap();
    insta::assert_snapshot!("systemd_user_service", service_text);

    let packaged_service = fs::read_to_string(
        workspace_root()
            .join("packaging")
            .join("systemd")
            .join(format!("{}.service", product::CLI_BIN)),
    )
    .expect("read packaging/systemd service");
    assert_eq!(
        service_text, packaged_service,
        "packaging/systemd template must match generated systemd service byte-for-byte"
    );

    // 3. When `XDG_CONFIG_HOME` is unset on Linux, `systemd.unit(5)` falls back to `$HOME/.config/systemd/user`.
    let mut fallback_cmd = shiki_cmd(&env);
    fallback_cmd.env_remove("XDG_CONFIG_HOME");
    fallback_cmd
        .args([
            "daemon",
            "install",
            "--platform",
            "linux",
            "--exe-path",
            "/usr/local/bin/shiki-term",
            "--no-activate",
        ])
        .assert()
        .success();
    let fallback_service = env
        .home_dir()
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{}.service", product::CLI_BIN));
    assert!(fallback_service.is_file());

    // 4. Executable paths containing spaces, XML characters (`&`), and `%` escape cleanly.
    let tricky_exe = "/Applications/My Tools/100% R&D/shiki-term";
    shiki_cmd(&env)
        .args([
            "daemon",
            "install",
            "--platform",
            "macos",
            "--exe-path",
            tricky_exe,
            "--no-activate",
        ])
        .assert()
        .success();
    let escaped_plist = fs::read_to_string(&expected_plist_path).unwrap();
    assert!(
        escaped_plist.contains("<string>/Applications/My Tools/100% R&amp;D/shiki-term</string>")
    );
    #[cfg(target_os = "macos")]
    assert_plutil_lint_passes(&expected_plist_path);

    shiki_cmd(&env)
        .args([
            "daemon",
            "install",
            "--platform",
            "linux",
            "--exe-path",
            tricky_exe,
            "--no-activate",
        ])
        .assert()
        .success();
    let escaped_service = fs::read_to_string(&expected_service_path).unwrap();
    assert!(
        escaped_service
            .contains("ExecStart=\"/Applications/My Tools/100%% R&D/shiki-term\" daemon run"),
        "unexpected escaped ExecStart:\n{escaped_service}"
    );
}

#[test]
fn daemon_install_and_uninstall_round_trip_directory_trees_across_macos_and_linux() {
    for platform in ["macos", "linux"] {
        for scenario in ["fresh", "empty_unit_dir", "populated_unit_dir"] {
            let env = TestEnv::new();
            let unit_dir = if platform == "macos" {
                env.home_dir().join("Library").join("LaunchAgents")
            } else {
                env.xdg_config_home().join("systemd").join("user")
            };

            match scenario {
                "fresh" => {}
                "empty_unit_dir" => {
                    fs::create_dir_all(&unit_dir).unwrap();
                }
                "populated_unit_dir" => {
                    fs::create_dir_all(&unit_dir).unwrap();
                    let sibling = if platform == "macos" {
                        unit_dir.join("com.example.other-agent.plist")
                    } else {
                        unit_dir.join("other-agent.service")
                    };
                    fs::write(&sibling, b"# pre-existing user unit\n").unwrap();
                }
                _ => unreachable!(),
            }

            let home_before = snapshot_tree(env.home_dir());
            let xdg_before = snapshot_tree(env.xdg_config_home());

            // Install twice to verify idempotency and preservation of `created-dirs` metadata.
            for _ in 0..2 {
                shiki_cmd(&env)
                    .args(["daemon", "install", "--platform", platform, "--no-activate"])
                    .assert()
                    .success();
            }

            #[cfg(target_os = "macos")]
            if platform == "macos" {
                let plist_path = unit_dir.join(format!("{}.plist", product::BUNDLE_ID));
                assert_plutil_lint_passes(&plist_path);
            }

            let status_out = shiki_cmd(&env)
                .args(["daemon", "status", "--platform", platform, "--json"])
                .output()
                .unwrap();
            assert!(status_out.status.success());
            let status_json: Value = serde_json::from_slice(&status_out.stdout).unwrap();
            assert_eq!(status_json["installed"], Value::Bool(true));

            // Uninstall twice to verify byte/directory-exact restoration and idempotent no-op.
            for _ in 0..2 {
                shiki_cmd(&env)
                    .args([
                        "daemon",
                        "uninstall",
                        "--platform",
                        platform,
                        "--no-activate",
                    ])
                    .assert()
                    .success();
            }

            assert_eq!(
                snapshot_tree(env.home_dir()),
                home_before,
                "HOME tree mismatch after install->uninstall round-trip ({platform}, {scenario})"
            );
            assert_eq!(
                snapshot_tree(env.xdg_config_home()),
                xdg_before,
                "XDG_CONFIG_HOME tree mismatch after install->uninstall round-trip ({platform}, {scenario})"
            );
        }
    }
}

#[test]
fn pause_and_resume_cli_persist_state_suppress_hook_drift_and_contain_zero_coordinates() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    // Seed initial generation 1 at Tokyo midnight.
    shiki_cmd(&env)
        .args(["_emit", "--time", "2026-03-20T15:00:00Z"])
        .assert()
        .success();
    assert_eq!(load_state(env.paths()).unwrap().generation, 1);

    // Invalid `--for` duration fails cleanly without mutating state.
    shiki_cmd(&env)
        .args(["pause", "--for", "not-a-duration"])
        .assert()
        .failure();
    assert_eq!(load_state(env.paths()).unwrap().paused_until, None);

    // Timed pause (`--for 45m`).
    let pause_timed = shiki_cmd(&env)
        .args(["pause", "--for", "45m"])
        .output()
        .unwrap();
    assert!(pause_timed.status.success());
    let timed_stdout = String::from_utf8(pause_timed.stdout).unwrap();
    assert!(timed_stdout.contains("paused drift until "));
    assert!(!timed_stdout.contains("35.7") && !timed_stdout.contains("139.7"));

    // Both plain-text and JSON `daemon status` report `paused` with zero coordinates (`PLAN.md` §0 #4).
    let status_txt = String::from_utf8(
        shiki_cmd(&env)
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(status_txt.contains("paused: until "));
    assert!(!status_txt.contains("35.7") && !status_txt.contains("139.7"));

    let status_json_raw = String::from_utf8(
        shiki_cmd(&env)
            .args(["daemon", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(!status_json_raw.contains("35.7") && !status_json_raw.contains("139.7"));
    let status_json: Value = serde_json::from_str(&status_json_raw).unwrap();
    assert_eq!(status_json["paused"], Value::Bool(true));

    // Indefinite pause (`pause` without `--for`) freezes prompt-hook `_emit` even across day/night.
    shiki_cmd(&env).arg("pause").assert().success();
    assert_eq!(
        load_state(env.paths()).unwrap().paused_until.as_deref(),
        Some(INDEFINITE_PAUSE)
    );
    shiki_cmd(&env)
        .args(["_emit", "--time", "2026-03-21T03:00:00Z"])
        .assert()
        .success();
    assert_eq!(
        load_state(env.paths()).unwrap().generation,
        1,
        "`_emit` must not advance generation while paused"
    );

    // `resume` clears `paused_until` and lets `_emit` update immediately.
    shiki_cmd(&env).arg("resume").assert().success();
    assert_eq!(load_state(env.paths()).unwrap().paused_until, None);
    shiki_cmd(&env)
        .args(["_emit", "--time", "2026-03-21T03:00:00Z"])
        .assert()
        .success();
    assert_eq!(load_state(env.paths()).unwrap().generation, 2);
}

struct DaemonChildGuard(Option<Child>);

impl DaemonChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("active daemon child")
    }
}

impl Drop for DaemonChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_until<F>(desc: &str, mut predicate: F)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + DAEMON_WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out after {DAEMON_WAIT_TIMEOUT:?} waiting for: {desc}");
}

#[test]
fn live_daemon_run_handles_startup_duplicate_lock_sighup_reload_pause_resume_and_sigterm() {
    let env = TestEnv::new();
    fs::write(
        env.paths().palettes_dir().join("alt.toml"),
        ALT_PALETTE_TOML,
    )
    .unwrap();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let mut spawn_cmd = StdCommand::new(shiki_bin_path());
    spawn_cmd
        .envs(env.env_vars())
        .env("SHIKI_NO_SERVICE_MANAGER", "1")
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut daemon_guard = DaemonChildGuard::new(spawn_cmd.spawn().expect("spawn daemon run"));
    let daemon_pid = daemon_guard.child_mut().id();

    // 1. Wait for initial tick (`generation == 1`) and `daemon.pid` lock acquisition.
    wait_until("daemon startup generation 1 and active PID lock", || {
        let out = shiki_cmd(&env)
            .args(["daemon", "status", "--json"])
            .output()
            .expect("daemon status");
        if !out.status.success() {
            return false;
        }
        let Ok(json) = serde_json::from_slice::<Value>(&out.stdout) else {
            return false;
        };
        json["running"] == Value::Bool(true)
            && json["pid"].as_u64() == Some(u64::from(daemon_pid))
            && json["generation"].as_u64() == Some(1)
    });

    assert_eq!(
        load_snapshot(env.paths()).unwrap().unwrap().palette,
        "shiki"
    );

    // 2. Starting a second `daemon run` fails immediately with `DaemonAlreadyRunning`.
    let dup = shiki_cmd(&env).args(["daemon", "run"]).output().unwrap();
    assert!(!dup.status.success());
    let dup_stderr = String::from_utf8_lossy(&dup.stderr);
    assert!(
        dup_stderr.contains("daemon is already running"),
        "unexpected duplicate stderr: {dup_stderr}"
    );

    // 3. While the daemon is blocked in `poll` (sleeping 60..1800s), `config set palette alt`
    //    signals `SIGHUP` so the daemon wakes immediately and writes generation 2!
    shiki_cmd(&env)
        .args(["config", "set", "palette", "alt"])
        .assert()
        .success();
    wait_until("early SIGHUP wakeup after `config set palette alt`", || {
        load_state(env.paths()).map(|s| s.generation).unwrap_or(0) == 2
    });
    assert_eq!(load_snapshot(env.paths()).unwrap().unwrap().palette, "alt");

    // 4. `pause` freezes the daemon: even if `config.toml` changes back to `shiki` and `SIGHUP` is
    //    delivered directly, the paused daemon performs zero store writes.
    shiki_cmd(&env).arg("pause").assert().success();
    let paused_cfg =
        "palette = \"shiki\"\n\n[location]\nmode = \"manual\"\nlatitude = 35.7\nlongitude = 139.7\n";
    fs::write(env.paths().config_file(), paused_cfg).unwrap();
    kill_process(Pid::from_child(daemon_guard.child_mut()), Signal::HUP).unwrap();
    thread::sleep(Duration::from_millis(120));
    assert_eq!(load_state(env.paths()).unwrap().generation, 2);
    assert_eq!(load_snapshot(env.paths()).unwrap().unwrap().palette, "alt");

    // 5. `resume` clears `paused_until` and signals `SIGHUP`, waking the daemon to write generation 3.
    shiki_cmd(&env).arg("resume").assert().success();
    wait_until("early SIGHUP wakeup after `resume`", || {
        load_state(env.paths()).map(|s| s.generation).unwrap_or(0) == 3
    });
    assert_eq!(
        load_snapshot(env.paths()).unwrap().unwrap().palette,
        "shiki"
    );

    // 6. `SIGTERM` wakes the daemon cleanly (`ExitCode::SUCCESS`) and releases `daemon.pid` flock.
    kill_process(Pid::from_child(daemon_guard.child_mut()), Signal::TERM).unwrap();
    let deadline = Instant::now() + DAEMON_WAIT_TIMEOUT;
    let exit_status = loop {
        if let Some(status) = daemon_guard.child_mut().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "daemon run did not exit within {DAEMON_WAIT_TIMEOUT:?} after SIGTERM"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        exit_status.success(),
        "daemon run must exit 0 on SIGTERM, got {exit_status}"
    );

    let after_json: Value = serde_json::from_slice(
        &shiki_cmd(&env)
            .args(["daemon", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(after_json["running"], Value::Bool(false));
    assert!(after_json["pid"].is_null());
}
