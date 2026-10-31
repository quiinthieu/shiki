use assert_cmd::Command;
use sk_core::product;
use std::fs;
use tempfile::TempDir;

#[test]
fn version_flag_uses_product_constants() {
    let temp = TempDir::new().expect("temp test root");
    let home = temp.path().join("home");
    let shiki_home = temp.path().join("shiki-home");
    let xdg_config = temp.path().join("xdg-config");
    let xdg_state = temp.path().join("xdg-state");
    let xdg_data = temp.path().join("xdg-data");
    let xdg_runtime = temp.path().join("xdg-runtime");

    for dir in [
        &home,
        &shiki_home,
        &xdg_config,
        &xdg_state,
        &xdg_data,
        &xdg_runtime,
    ] {
        fs::create_dir_all(dir).expect("create isolated test dir");
    }

    let expected = format!("{} {}\n", product::CLI_BIN, product::VERSION);

    for flag in ["--version", "-V"] {
        let assert = Command::new(env!("CARGO_BIN_EXE_shiki-term"))
            .env("HOME", &home)
            .env("SHIKI_HOME", &shiki_home)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_STATE_HOME", &xdg_state)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("XDG_RUNTIME_DIR", &xdg_runtime)
            .arg(flag)
            .assert()
            .success();

        let stdout =
            String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8 version output");
        assert_eq!(
            stdout,
            expected,
            "`{} {flag}` output must match product::CLI_BIN and product::VERSION",
            product::CLI_BIN
        );
    }
}
