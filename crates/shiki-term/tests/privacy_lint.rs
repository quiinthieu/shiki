use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/shiki-term sits two levels below workspace root")
        .to_path_buf()
}

fn lint_script() -> PathBuf {
    repo_root().join("scripts").join("lint-privacy.sh")
}

fn run_lint_on(dir: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(lint_script())
        .arg(dir)
        .output()
        .expect("lint-privacy.sh must execute")
}

#[test]
fn clean_repo_passes_privacy_lint() {
    let output = Command::new("bash")
        .arg(lint_script())
        .output()
        .expect("lint-privacy.sh must execute");

    assert!(
        output.status.success(),
        "clean repo failed privacy lint:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn catches_planted_std_net_import() {
    let temp = TempDir::new().expect("temp workspace");
    let src_dir = temp.path().join("crates").join("sk-core").join("src");
    fs::create_dir_all(&src_dir).expect("create temp src dir");

    let planted = ["use std", "::", "net", "::", "Tcp", "Stream;\n"].concat();
    fs::write(src_dir.join("lib.rs"), planted).expect("write planted source");

    let output = run_lint_on(temp.path());
    assert!(
        !output.status.success(),
        "privacy lint should fail on planted std network import"
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_token = ["std", "::", "net"].concat();
    assert!(
        combined.contains("Privacy violation") && combined.contains(&expected_token),
        "expected lint output to report Privacy violation with planted import, got:\n{combined}"
    );
}

#[test]
fn catches_grouped_std_net_imports() {
    for planted in [
        ["use std::{io, ", "ne", "t::", "Tcp", "Listener};\n"].concat(),
        ["use std::{io, ", "ne", "t};\n"].concat(),
    ] {
        let temp = TempDir::new().expect("temp workspace");
        let src_dir = temp.path().join("crates").join("sk-core").join("src");
        fs::create_dir_all(&src_dir).expect("create temp src dir");
        fs::write(src_dir.join("lib.rs"), &planted).expect("write planted grouped import");

        let output = run_lint_on(temp.path());
        assert!(
            !output.status.success(),
            "privacy lint should fail on grouped network import: {planted}"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("Privacy violation"),
            "expected Privacy violation message, got:\n{combined}"
        );
    }
}

#[test]
fn allows_local_unix_domain_socket_ipc() {
    let temp = TempDir::new().expect("temp workspace");
    let src_dir = temp.path().join("crates").join("sk-deliver").join("src");
    fs::create_dir_all(&src_dir).expect("create temp src dir");

    fs::write(
        src_dir.join("lib.rs"),
        "use std::os::unix::net::UnixStream;\n",
    )
    .expect("write local unix socket import");

    let output = run_lint_on(temp.path());
    assert!(
        output.status.success(),
        "local AF_UNIX socket IPC must not trigger network privacy lint:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn catches_planted_network_crates_in_cargo_toml() {
    for forbidden_dep in [
        ["req", "west = \"0.12\""].concat(),
        ["hy", "per = \"1.0\""].concat(),
        ["ur", "eq = \"2.10\""].concat(),
    ] {
        let temp = TempDir::new().expect("temp workspace");
        let crate_dir = temp.path().join("crates").join("sample");
        fs::create_dir_all(&crate_dir).expect("create temp crate dir");
        let cargo_toml = format!(
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n\n[dependencies]\n{forbidden_dep}\n"
        );
        fs::write(crate_dir.join("Cargo.toml"), cargo_toml).expect("write planted Cargo.toml");

        let output = run_lint_on(temp.path());
        assert!(
            !output.status.success(),
            "privacy lint should fail on forbidden crate `{forbidden_dep}` in Cargo.toml"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("Privacy violation"),
            "expected Privacy violation message for Cargo.toml, got:\n{combined}"
        );
    }
}

#[test]
fn catches_planted_swift_network_and_core_location_apis() {
    for swift_snippet in [
        format!("let session = {}.shared\n", ["URL", "Session"].concat()),
        format!("import {}\n", ["Core", "Location"].concat()),
        format!("let mgr = {}()\n", ["CL", "Location", "Manager"].concat()),
    ] {
        let temp = TempDir::new().expect("temp workspace");
        let macos_dir = temp.path().join("macos").join("Sources");
        fs::create_dir_all(&macos_dir).expect("create temp macos dir");
        fs::write(macos_dir.join("App.swift"), &swift_snippet).expect("write planted Swift file");

        let output = run_lint_on(temp.path());
        assert!(
            !output.status.success(),
            "privacy lint should fail on forbidden Swift API: {swift_snippet}"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("Privacy violation"),
            "expected Privacy violation message for Swift source, got:\n{combined}"
        );
    }
}
