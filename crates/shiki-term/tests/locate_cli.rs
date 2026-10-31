#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use sk_core::product;
use std::fs;
use std::path::{Path, PathBuf};
use support::TestEnv;

fn shiki_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shiki-term"));
    cmd.envs(env.env_vars());
    cmd
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn locate_resolves_asia_saigon_and_asia_ho_chi_minh_to_same_canonical_location() {
    let env = TestEnv::new();

    let saigon_assert = shiki_cmd(&env)
        .env("TZ", "Asia/Saigon")
        .arg("locate")
        .assert()
        .success();
    let saigon_out =
        String::from_utf8(saigon_assert.get_output().stdout.clone()).expect("UTF-8 stdout");

    let hcm_assert = shiki_cmd(&env)
        .env("TZ", "Asia/Ho_Chi_Minh")
        .arg("locate")
        .assert()
        .success();
    let hcm_out = String::from_utf8(hcm_assert.get_output().stdout.clone()).expect("UTF-8 stdout");

    assert!(saigon_out.contains("mode: timezone\n"));
    assert!(saigon_out.contains("timezone: Asia/Ho_Chi_Minh (linked from Asia/Saigon)\n"));
    assert!(saigon_out.contains("coordinates: 10.8,106.7\n"));

    assert_eq!(
        hcm_out,
        "mode: timezone\ntimezone: Asia/Ho_Chi_Minh\ncoordinates: 10.8,106.7\n"
    );

    // Read-only `locate` must not create `config.toml`.
    assert!(!env.paths().config_file().exists());
}

#[test]
fn locate_unknown_and_utc_timezones_fail_and_suggest_locate_set() {
    let env = TestEnv::new();
    let expected_hint = format!("{} locate --set", product::CLI_BIN);

    for bad_tz in ["Unknown/Atlantis", "UTC", "Etc/UTC", "EST5EDT"] {
        let assert = shiki_cmd(&env)
            .env("TZ", bad_tz)
            .arg("locate")
            .assert()
            .failure();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8 stdout");
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8 stderr");
        assert!(
            stdout.is_empty(),
            "expected empty stdout on error, got: {stdout}"
        );
        assert!(
            stderr.contains(&expected_hint),
            "stderr for TZ={bad_tz} must suggest `{expected_hint}`, got: {stderr}"
        );
    }
}

#[test]
fn locate_set_rounds_persists_supports_negative_latitudes_and_switches_back_to_timezone() {
    let env = TestEnv::new();

    // 1. Set manual coordinates (rounded to 1 decimal place).
    let set_assert = shiki_cmd(&env)
        .env("TZ", "Unknown/Atlantis")
        .args(["locate", "--set", "10.76,106.66"])
        .assert()
        .success();
    let set_out = String::from_utf8(set_assert.get_output().stdout.clone()).expect("UTF-8 stdout");
    assert_eq!(set_out, "mode: manual\ncoordinates: 10.8,106.7\n");

    // 2. Subsequent `locate` succeeds in manual mode even when TZ is unknown.
    let get_assert = shiki_cmd(&env)
        .env("TZ", "Unknown/Atlantis")
        .arg("locate")
        .assert()
        .success();
    let get_out = String::from_utf8(get_assert.get_output().stdout.clone()).expect("UTF-8 stdout");
    assert_eq!(get_out, "mode: manual\ncoordinates: 10.8,106.7\n");

    // 3. Negative Southern/Western hemisphere coordinates (`--set -33.87,-70.65`) parse cleanly in clap.
    let neg_assert = shiki_cmd(&env)
        .env("TZ", "Asia/Saigon")
        .args(["locate", "--set", "-33.87,-70.65"])
        .assert()
        .success();
    let neg_out = String::from_utf8(neg_assert.get_output().stdout.clone()).expect("UTF-8 stdout");
    assert_eq!(neg_out, "mode: manual\ncoordinates: -33.9,-70.7\n");

    // 4. Switch back to timezone mode (`--timezone`) and confirm manual coordinates are removed from config.toml.
    let tz_assert = shiki_cmd(&env)
        .env("TZ", "Asia/Saigon")
        .args(["locate", "--timezone"])
        .assert()
        .success();
    let tz_out = String::from_utf8(tz_assert.get_output().stdout.clone()).expect("UTF-8 stdout");
    assert_eq!(
        tz_out,
        "mode: timezone\ntimezone: Asia/Ho_Chi_Minh (linked from Asia/Saigon)\ncoordinates: 10.8,106.7\n"
    );

    let config_contents =
        fs::read_to_string(env.paths().config_file()).expect("read config.toml after --timezone");
    assert!(!config_contents.contains("-33.9"));
    assert!(!config_contents.contains("-70.7"));
}

#[test]
fn privacy_audit_configured_coordinates_never_appear_outside_config_toml_or_in_errors() {
    let env = TestEnv::new();

    // Configure public Tokyo coordinates (`35.7, 139.7`).
    shiki_cmd(&env)
        .env("TZ", "Asia/Tokyo")
        .args(["locate", "--set", "35.68,139.69"])
        .assert()
        .success();

    shiki_cmd(&env)
        .env("TZ", "Asia/Tokyo")
        .arg("locate")
        .assert()
        .success();

    // Walk every file inside TestEnv root: ONLY `config.toml` may contain the configured coordinates.
    let config_path = env
        .paths()
        .config_file()
        .canonicalize()
        .expect("canonicalize config.toml");
    let mut all_files = Vec::new();
    collect_files(env.root(), &mut all_files);
    assert!(!all_files.is_empty());

    for file in all_files {
        let canon = file.canonicalize().expect("canonicalize file");
        let text = fs::read_to_string(&file).unwrap_or_default();
        if canon == config_path {
            assert!(text.contains("35.7") && text.contains("139.7"));
            assert!(!text.contains("35.68") && !text.contains("139.69"));
        } else {
            assert!(
                !text.contains("35.7")
                    && !text.contains("139.7")
                    && !text.contains("35.68")
                    && !text.contains("139.69"),
                "privacy violation: coordinates found outside config.toml in {}",
                file.display()
            );
        }
    }

    // Trigger CLI error paths and verify zero coordinate values ever leak to stderr.
    for bad_args in [
        &["locate", "--set", "95.3,139.7"][..],
        &["locate", "--set", "35.7,199.9"][..],
        &["locate", "--set", "notanumber,139.7"][..],
        &["locate", "--timezone"][..],
    ] {
        let assert = shiki_cmd(&env)
            .env("TZ", "Unknown/Atlantis")
            .args(bad_args)
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8 stderr");
        for forbidden in ["35.7", "139.7", "95.3", "199.9"] {
            assert!(
                !stderr.contains(forbidden),
                "privacy violation: stderr leaked coordinate '{forbidden}' for args {bad_args:?}: {stderr}"
            );
        }
    }

    // Corrupt config.toml with configured coordinates present and verify stderr still leaks zero coordinates.
    fs::write(
        env.paths().config_file(),
        "[location]\nmode = \"manual\"\nlatitude = 35.7\nlongitude = \"139.7\"\n",
    )
    .expect("write malformed config");
    let bad_cfg_assert = shiki_cmd(&env)
        .env("TZ", "Asia/Tokyo")
        .arg("locate")
        .assert()
        .failure();
    let bad_cfg_stderr =
        String::from_utf8(bad_cfg_assert.get_output().stderr.clone()).expect("UTF-8 stderr");
    assert!(!bad_cfg_stderr.contains("35.7") && !bad_cfg_stderr.contains("139.7"));
}
