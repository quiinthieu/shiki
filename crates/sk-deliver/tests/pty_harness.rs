#![cfg(unix)]

#[path = "../../../tests/support/pty.rs"]
mod pty;

use pty::PtyPair;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);

fn escape_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

#[test]
fn pty_pair_transmits_osc_sequences_byte_for_byte_and_matches_snapshot() {
    let mut pair = PtyPair::open_raw().expect("open raw PTY pair");
    assert!(
        pair.replica_path().exists(),
        "replica path {} must exist while open",
        pair.replica_path().display()
    );

    let osc_payload =
        b"\x1b]11;rgb:14/16/1b\x1b\\\x1b]10;rgb:e6/e8/ee\x1b\\\x1b]4;1;rgb:ab/46/42\x1b\\\x1b]111\x1b\\";
    pair.write_to_replica(osc_payload)
        .expect("write OSC bytes to PTY replica");

    let received = pair
        .read_exact_with_timeout(osc_payload.len(), TIMEOUT)
        .expect("read exact OSC bytes from PTY controller");
    assert_eq!(received, osc_payload);

    pair.close_replica();
    let trailing = pair
        .read_to_eof(TIMEOUT)
        .expect("drain EOF after closing replica");
    assert!(trailing.is_empty());

    let snapshot_text = format!(
        "byte_len = {}\nescaped = {}",
        received.len(),
        escape_ascii(&received)
    );
    insta::assert_snapshot!("pty_raw_osc_roundtrip", snapshot_text);
}

#[test]
fn pty_pair_bidirectional_child_io_works() {
    let mut pair = PtyPair::open_raw().expect("open raw PTY pair");

    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "IFS= read -r line && printf 'ack:%s' \"$line\""]);

    let mut child = pair.spawn_command(cmd).expect("spawn /bin/sh on PTY");
    pair.write_input(b"osc-ready\n")
        .expect("write input line to PTY controller");

    let output = pair
        .read_to_eof(TIMEOUT)
        .expect("read child PTY output until EOF");
    let status = child
        .wait_with_timeout(TIMEOUT)
        .expect("wait for /bin/sh exit");

    assert!(status.success(), "/bin/sh exited with {status}");
    assert_eq!(
        String::from_utf8(output).expect("UTF-8 child output"),
        "ack:osc-ready"
    );
}

#[test]
fn pty_pair_passes_100x_in_a_loop_without_fd_leaks() {
    for iteration in 0..100 {
        let mut pair = PtyPair::open_raw()
            .unwrap_or_else(|e| panic!("PtyPair::open_raw failed on iteration {iteration}: {e}"));
        let slot = iteration % 16;
        let payload = format!("\x1b]4;{slot};rgb:1a/2b/3c\x1b\\");

        pair.write_to_replica(payload.as_bytes())
            .unwrap_or_else(|e| panic!("write_to_replica failed on iteration {iteration}: {e}"));

        let received = pair
            .read_until(b"\x1b\\", TIMEOUT)
            .unwrap_or_else(|e| panic!("read_until failed on iteration {iteration}: {e}"));
        assert_eq!(
            received,
            payload.as_bytes(),
            "byte mismatch on iteration {iteration}"
        );

        pair.close_replica();
        let eof = pair
            .read_to_eof(TIMEOUT)
            .unwrap_or_else(|e| panic!("read_to_eof failed on iteration {iteration}: {e}"));
        assert!(eof.is_empty());
    }
}
