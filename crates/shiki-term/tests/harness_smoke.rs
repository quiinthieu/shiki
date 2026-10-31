#![cfg(unix)]

#[path = "../../../tests/support/mod.rs"]
mod support;

#[path = "../../../tests/support/pty.rs"]
mod pty;

use pty::PtyPair;
use sk_core::product;
use std::process::Command;
use std::time::Duration;
use support::TestEnv;

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn cli_runs_on_pty_inside_test_env_100x_in_a_loop() {
    let expected = format!("{} {}\n", product::CLI_BIN, product::VERSION);

    for iteration in 0..100 {
        let env = TestEnv::new();
        let mut pair = PtyPair::open_raw()
            .unwrap_or_else(|e| panic!("open_raw failed on iteration {iteration}: {e}"));

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_shiki-term"));
        cmd.envs(env.env_vars()).arg("--version");

        let mut child = pair
            .spawn_command(cmd)
            .unwrap_or_else(|e| panic!("spawn_command failed on iteration {iteration}: {e}"));

        let output = pair
            .read_to_eof(TIMEOUT)
            .unwrap_or_else(|e| panic!("read_to_eof failed on iteration {iteration}: {e}"));
        let status = child
            .wait_with_timeout(TIMEOUT)
            .unwrap_or_else(|e| panic!("wait_with_timeout failed on iteration {iteration}: {e}"));

        assert!(
            status.success(),
            "CLI exited with {status} on iteration {iteration}"
        );
        assert_eq!(
            String::from_utf8(output).expect("UTF-8 version output"),
            expected,
            "unexpected PTY version output on iteration {iteration}"
        );
    }
}
