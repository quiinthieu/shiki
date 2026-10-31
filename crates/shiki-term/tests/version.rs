#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use sk_core::product;
use support::TestEnv;

#[test]
fn version_flag_uses_product_constants() {
    let env = TestEnv::new();
    let expected = format!("{} {}\n", product::CLI_BIN, product::VERSION);

    for flag in ["--version", "-V"] {
        let assert = Command::new(env!("CARGO_BIN_EXE_shiki-term"))
            .envs(env.env_vars())
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
