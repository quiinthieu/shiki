#[path = "../../../tests/support/pty.rs"]
mod pty;
#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_deliver::osc::{contains_palette_mutating_osc, encode_osc_frame, encode_osc_reset};
use sk_store::store::{load_state, Snapshot, SNAPSHOT_SCHEMA_VERSION};
use std::fs;
use support::TestEnv;

fn shiki_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shiki-term"));
    cmd.envs(env.env_vars());
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("COLORTERM");
    cmd.env_remove("TERM");
    cmd
}

#[test]
fn apply_emits_golden_osc_bytes_and_populates_store_while_reset_emits_104_110_111_112() {
    let env = TestEnv::new();

    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let apply_out = shiki_cmd(&env)
        .args(["apply", "--time", "2026-03-20T20:35:00Z"])
        .output()
        .expect("run shiki-term apply");
    assert!(
        apply_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    let expected_frame = compute_frame(
        UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap(),
        Coordinates::new(35.7, 139.7).unwrap(),
        &Palette::bundled_shiki(),
        &ModelConfig::bundled(),
    );
    let expected_osc = encode_osc_frame(&expected_frame.colors);
    assert_eq!(apply_out.stdout, expected_osc);
    assert!(apply_out.stderr.is_empty());

    // Store files updated atomically by `apply`.
    assert_eq!(
        fs::read(env.paths().current_seq_file()).unwrap(),
        expected_osc
    );
    let raw_snapshot = fs::read_to_string(env.paths().snapshot_file()).unwrap();
    assert!(!raw_snapshot.contains("35.7") && !raw_snapshot.contains("139.7"));
    let parsed_snapshot: Snapshot = serde_json::from_str(&raw_snapshot).unwrap();
    assert_eq!(parsed_snapshot.schema_version, SNAPSHOT_SCHEMA_VERSION);
    assert_eq!(parsed_snapshot.palette, "shiki");

    let state_after_apply = load_state(env.paths()).unwrap();
    assert_eq!(state_after_apply.generation, 1);
    assert!(state_after_apply.frame_hash.is_some());

    // An invalid/nonexistent palette never applies (`PLAN.md` §6): zero OSC bytes, store untouched.
    let bad_apply = shiki_cmd(&env)
        .args([
            "apply",
            "--palette",
            "missing-palette",
            "--time",
            "2026-06-22T02:45:00Z",
        ])
        .output()
        .expect("run apply with missing palette");
    assert!(!bad_apply.status.success());
    assert!(
        bad_apply.stdout.is_empty(),
        "invalid palette must emit zero stdout bytes"
    );
    assert_eq!(
        fs::read(env.paths().current_seq_file()).unwrap(),
        expected_osc
    );
    assert_eq!(load_state(env.paths()).unwrap().generation, 1);

    // `reset` emits exact OSC 104 / 110 / 111 / 112 and clears `current.seq`.
    let reset_out = shiki_cmd(&env)
        .arg("reset")
        .output()
        .expect("run shiki-term reset");
    assert!(
        reset_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reset_out.stderr)
    );
    assert_eq!(reset_out.stdout, encode_osc_reset());
    assert_eq!(
        reset_out.stdout,
        b"\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\"
    );
    assert!(fs::read(env.paths().current_seq_file()).unwrap().is_empty());
    let state_after_reset = load_state(env.paths()).unwrap();
    assert_eq!(state_after_reset.generation, 2);
    assert_eq!(state_after_reset.frame_hash, None);
}

#[test]
fn now_text_and_json_match_golden_snapshot_and_never_leak_coordinates() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let now_out = shiki_cmd(&env)
        .args(["now", "--time", "2026-03-20T20:35:00Z"])
        .output()
        .expect("run shiki-term now");
    assert!(
        now_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&now_out.stderr)
    );
    let stdout = String::from_utf8(now_out.stdout).unwrap();
    assert!(!stdout.contains("35.7") && !stdout.contains("139.7"));
    assert!(!contains_palette_mutating_osc(stdout.as_bytes()));
    assert!(!env.paths().current_seq_file().exists());
    insta::assert_snapshot!("now_text_shiki_spring_dawn", stdout);

    let json_out = shiki_cmd(&env)
        .args(["now", "--time", "2026-03-20T20:35:00Z", "--json"])
        .output()
        .expect("run shiki-term now --json");
    assert!(json_out.status.success());
    let json_str = String::from_utf8(json_out.stdout).unwrap();
    assert!(!json_str.contains("35.7") && !json_str.contains("139.7"));
    let parsed: Snapshot = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.instant_rfc3339, "2026-03-20T20:35:00Z");
    assert_eq!(parsed.colors.bg.to_string(), "#27252b");
}

#[test]
fn preview_renders_truecolor_256_fallback_no_color_and_json_and_never_emits_osc_4_10_11() {
    let env = TestEnv::new();

    // 1. Truecolour mode (`COLORTERM=truecolor`).
    let tc_out = shiki_cmd(&env)
        .env("COLORTERM", "truecolor")
        .env("TERM", "xterm-256color")
        .args([
            "preview",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--time",
            "2026-03-20T20:35:00Z",
        ])
        .output()
        .expect("run preview truecolor");
    assert!(
        tc_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tc_out.stderr)
    );
    assert!(
        !contains_palette_mutating_osc(&tc_out.stdout),
        "preview must NEVER emit OSC 4/10/11/12"
    );
    assert!(
        !tc_out.stdout.windows(2).any(|w| w == b"\x1b]"),
        "preview must not emit any OSC sequence"
    );
    let tc_str = String::from_utf8(tc_out.stdout).unwrap();
    assert!(tc_str.contains("\x1b[48;2;"), "expected 24-bit SGR bg");
    assert!(tc_str.contains("\x1b[38;2;"), "expected 24-bit SGR fg");
    assert!(!tc_str.contains("35.7") && !tc_str.contains("139.7"));
    assert!(!env.paths().current_seq_file().exists());
    assert!(!env.paths().snapshot_file().exists());

    // 2. 256-colour fallback (`TERM=xterm-256color`, `COLORTERM` unset).
    let c256_out = shiki_cmd(&env)
        .env("TERM", "xterm-256color")
        .args([
            "preview",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--time",
            "2026-03-20T20:35:00Z",
        ])
        .output()
        .expect("run preview 256-colour");
    assert!(c256_out.status.success());
    assert!(!contains_palette_mutating_osc(&c256_out.stdout));
    assert!(!c256_out.stdout.windows(2).any(|w| w == b"\x1b]"));
    let c256_str = String::from_utf8(c256_out.stdout).unwrap();
    assert!(
        c256_str.contains("\x1b[48;5;"),
        "expected 256-colour SGR bg"
    );
    assert!(
        c256_str.contains("\x1b[38;5;"),
        "expected 256-colour SGR fg"
    );
    assert!(
        !c256_str.contains("\x1b[48;2;") && !c256_str.contains("\x1b[38;2;"),
        "256-colour fallback must not emit 24-bit truecolor SGR"
    );

    // 3. `NO_COLOR=1` suppresses all ANSI/SGR escape sequences even if `COLORTERM=truecolor`.
    let nc_out = shiki_cmd(&env)
        .env("COLORTERM", "truecolor")
        .env("TERM", "xterm-256color")
        .env("NO_COLOR", "1")
        .args([
            "preview",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--time",
            "2026-03-20T20:35:00Z",
        ])
        .output()
        .expect("run preview NO_COLOR");
    assert!(nc_out.status.success());
    assert!(!contains_palette_mutating_osc(&nc_out.stdout));
    let nc_str = String::from_utf8(nc_out.stdout).unwrap();
    assert!(
        !nc_str.contains('\x1b'),
        "NO_COLOR output must contain zero ESC bytes, got:\n{nc_str}"
    );
    assert!(!nc_str.contains("35.7") && !nc_str.contains("139.7"));
    insta::assert_snapshot!("preview_no_color_shiki_spring_dawn", nc_str);

    // 4. `preview --date --time --json` resolves civil wall time and emits coordinate-free JSON.
    let json_out = shiki_cmd(&env)
        .args([
            "preview",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--date",
            "2026-03-21",
            "--time",
            "05:35+09:00",
            "--json",
        ])
        .output()
        .expect("run preview --json");
    assert!(json_out.status.success());
    assert!(!contains_palette_mutating_osc(&json_out.stdout));
    let json_str = String::from_utf8(json_out.stdout).unwrap();
    assert!(!json_str.contains("35.7") && !json_str.contains("139.7"));
    let snapshot: Snapshot = serde_json::from_str(&json_str).unwrap();
    assert_eq!(snapshot.instant_rfc3339, "2026-03-20T20:35:00Z");
}

#[test]
fn config_get_and_set_cli_roundtrips_and_enforces_i_know_and_valid_palettes() {
    let env = TestEnv::new();

    let all_out = shiki_cmd(&env)
        .args(["config", "get"])
        .output()
        .expect("config get all");
    assert!(all_out.status.success());
    let all_str = String::from_utf8(all_out.stdout).unwrap();
    assert!(all_str.contains("palette = shiki\n"));
    assert!(all_str.contains("location.mode = timezone\n"));

    let single_out = shiki_cmd(&env)
        .args(["config", "get", "palette"])
        .output()
        .expect("config get palette");
    assert!(single_out.status.success());
    assert_eq!(String::from_utf8(single_out.stdout).unwrap(), "shiki\n");

    shiki_cmd(&env)
        .args(["config", "set", "step_threshold", "0.012"])
        .assert()
        .success();
    let updated = shiki_cmd(&env)
        .args(["config", "get", "step_threshold"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(updated.stdout).unwrap(), "0.012\n");

    // Lowering contrast without `--i-know` fails.
    let denied = shiki_cmd(&env)
        .args(["config", "set", "contrast.fg_bg_min_ratio", "3.5"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("--i-know"));

    // With `--i-know`, lowering contrast succeeds.
    shiki_cmd(&env)
        .args([
            "config",
            "set",
            "contrast.fg_bg_min_ratio",
            "3.5",
            "--i-know",
        ])
        .assert()
        .success();

    // Invalid palette name fails without mutating `palette`.
    let bad_pal = shiki_cmd(&env)
        .args(["config", "set", "palette", "no-such-palette"])
        .output()
        .unwrap();
    assert!(!bad_pal.status.success());
    let still_shiki = shiki_cmd(&env)
        .args(["config", "get", "palette"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(still_shiki.stdout).unwrap(), "shiki\n");
}

#[cfg(unix)]
#[test]
fn apply_and_reset_transmit_exact_osc_bytes_on_pty() {
    use std::process::Command as StdCommand;
    use std::time::Duration;

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
    let expected_apply = encode_osc_frame(&expected_frame.colors);

    let mut pair = pty::PtyPair::open_raw().expect("open PTY");
    let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_shiki-term"));
    cmd.envs(env.env_vars());
    cmd.args(["apply", "--time", "2026-03-20T20:35:00Z"]);
    let mut child = pair.spawn_command(cmd).expect("spawn apply on PTY");
    let received_apply = pair
        .read_exact_with_timeout(expected_apply.len(), Duration::from_secs(5))
        .expect("read apply OSC bytes from PTY");
    let status = child
        .wait_with_timeout(Duration::from_secs(5))
        .expect("wait apply exit");
    assert!(status.success());
    assert_eq!(received_apply, expected_apply);

    let expected_reset = encode_osc_reset();
    let mut pair_reset = pty::PtyPair::open_raw().expect("open PTY for reset");
    let mut reset_cmd = StdCommand::new(env!("CARGO_BIN_EXE_shiki-term"));
    reset_cmd.envs(env.env_vars());
    reset_cmd.arg("reset");
    let mut reset_child = pair_reset
        .spawn_command(reset_cmd)
        .expect("spawn reset on PTY");
    let received_reset = pair_reset
        .read_exact_with_timeout(expected_reset.len(), Duration::from_secs(5))
        .expect("read reset OSC bytes from PTY");
    let reset_status = reset_child
        .wait_with_timeout(Duration::from_secs(5))
        .expect("wait reset exit");
    assert!(reset_status.success());
    assert_eq!(received_reset, expected_reset);
}
