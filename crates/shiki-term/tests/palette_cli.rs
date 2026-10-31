#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use std::fs;
use support::TestEnv;

fn shiki_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shiki-term"));
    cmd.envs(env.env_vars());
    cmd
}

#[test]
fn palette_check_prints_worst_moment_in_both_hemispheres_and_never_leaks_coordinates() {
    let env = TestEnv::new();

    // Configure manual coordinates in `config.toml` via `locate --set` and run `palette check`:
    // privacy rule (`PLAN.md` §0 #4 & §9) requires zero occurrences of the configured coordinates
    // in `palette check` stdout/stderr!
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let north_out = shiki_cmd(&env)
        .args(["palette", "check", "--year", "2026"])
        .output()
        .expect("run palette check (north)");
    assert!(
        north_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&north_out.stderr)
    );
    let stdout = String::from_utf8(north_out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(north_out.stderr).expect("utf8 stderr");
    assert!(stdout.contains("palette: shiki"), "stdout:\n{stdout}");
    assert!(stdout.contains("year: 2026"), "stdout:\n{stdout}");
    assert!(stdout.contains("hemisphere: northern"), "stdout:\n{stdout}");
    assert!(stdout.contains("status: pass"), "stdout:\n{stdout}");
    assert!(stdout.contains("worst moment:"), "stdout:\n{stdout}");
    assert!(
        !stdout.contains("35.7") && !stdout.contains("139.7"),
        "configured coordinates leaked into palette check stdout:\n{stdout}"
    );
    assert!(!stderr.contains("35.7") && !stderr.contains("139.7"));

    // Southern hemisphere override (`--lat -33.9 --lon 151.2`).
    let south_out = shiki_cmd(&env)
        .args([
            "palette", "check", "shiki", "--lat", "-33.9", "--lon", "151.2", "--year", "2026",
        ])
        .output()
        .expect("run palette check (south)");
    assert!(south_out.status.success());
    let south_stdout = String::from_utf8(south_out.stdout).expect("utf8");
    assert!(south_stdout.contains("hemisphere: southern"));
    assert!(south_stdout.contains("status: pass"));
    assert!(south_stdout.contains("worst moment:"));
    assert!(!south_stdout.contains("-33.9") && !south_stdout.contains("151.2"));
}

#[test]
fn palette_check_requires_i_know_to_lower_thresholds_and_reports_file_line_on_invalid_toml() {
    let env = TestEnv::new();

    // Lowering `--fg-contrast` below 4.5 without `--i-know` must fail (`PLAN.md` §4).
    let denied = shiki_cmd(&env)
        .args([
            "palette",
            "check",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--year",
            "2026",
            "--fg-contrast",
            "3.5",
        ])
        .output()
        .expect("run lowered contrast without --i-know");
    assert!(!denied.status.success());
    let denied_err = String::from_utf8_lossy(&denied.stderr);
    assert!(
        denied_err.contains("--i-know"),
        "expected `--i-know` hint, got: {denied_err}"
    );

    // Passing `--i-know` allows the custom lower threshold.
    shiki_cmd(&env)
        .args([
            "palette",
            "check",
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--year",
            "2026",
            "--fg-contrast",
            "3.5",
            "--i-know",
        ])
        .assert()
        .success();

    // Malformed `.toml` file exits non-zero and prints exact `file:line:col` diagnostic.
    let bad_file = env.root().join("broken-palette.toml");
    fs::write(
        &bad_file,
        "name = \"broken\"\n\n[[anchors]]\nd = 2.0\nbg = \"#000000\"\nfg = \"#ffffff\"\ncursor = \"#ffffff\"\nselection = \"#222222\"\nansi = [\"#000\",\"#111\",\"#222\",\"#333\",\"#444\",\"#555\",\"#666\",\"#777\",\"#888\",\"#999\",\"#aaa\",\"#bbb\",\"#ccc\",\"#ddd\",\"#eee\",\"#fff\"]\n",
    )
    .expect("write broken-palette.toml");

    let bad_out = shiki_cmd(&env)
        .args([
            "palette",
            "check",
            &bad_file.to_string_lossy(),
            "--lat",
            "35.7",
            "--lon",
            "139.7",
            "--year",
            "2026",
        ])
        .output()
        .expect("run palette check on broken file");
    assert!(!bad_out.status.success());
    let bad_err = String::from_utf8_lossy(&bad_out.stderr);
    let expected_prefix = format!("{}:4:", bad_file.display());
    assert!(
        bad_err.contains(&expected_prefix),
        "expected `{expected_prefix}` in stderr, got: {bad_err}"
    );
}
