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

fn purity_script() -> PathBuf {
    repo_root().join("scripts").join("lint-core-purity.sh")
}

fn run_purity_on_planted_core(snippet: &str) -> (bool, String) {
    let temp = TempDir::new().expect("temp workspace");
    let src_dir = temp.path().join("crates").join("sk-core").join("src");
    fs::create_dir_all(&src_dir).expect("create temp sk-core/src");
    fs::write(src_dir.join("lib.rs"), snippet).expect("write planted lib.rs");

    let output = Command::new("bash")
        .arg(purity_script())
        .arg(temp.path())
        .output()
        .expect("lint-core-purity.sh must execute");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

#[test]
fn clean_sk_core_passes_purity_lint() {
    let output = Command::new("bash")
        .arg(purity_script())
        .output()
        .expect("lint-core-purity.sh must execute");

    assert!(
        output.status.success(),
        "sk-core failed purity lint:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn purity_lint_catches_std_fs() {
    let (ok_direct, out_direct) =
        run_purity_on_planted_core("pub fn bad() { let _ = std::fs::read(\"x\"); }\n");
    assert!(!ok_direct, "expected failure on std::fs");
    assert!(
        out_direct.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_direct}"
    );

    let (ok_grouped, out_grouped) =
        run_purity_on_planted_core("use std::{fmt, fs};\npub fn bad() {}\n");
    assert!(!ok_grouped, "expected failure on grouped std::fs import");
    assert!(
        out_grouped.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_grouped}"
    );

    let (ok_alias, out_alias) = run_purity_on_planted_core(
        "pub fn bad() { let _ = fs::read_to_string(\"config.toml\"); }\n",
    );
    assert!(!ok_alias, "expected failure on fs::read_to_string");
    assert!(
        out_alias.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_alias}"
    );
}

#[test]
fn purity_lint_catches_system_time_now_and_instant_now() {
    let (ok_sys, out_sys) =
        run_purity_on_planted_core("pub fn bad() { let _ = SystemTime::now(); }\n");
    assert!(!ok_sys, "expected failure on SystemTime::now");
    assert!(
        out_sys.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_sys}"
    );

    let (ok_inst, out_inst) = run_purity_on_planted_core(
        "use std::time::Instant;\npub fn bad() { let _ = Instant::now(); }\n",
    );
    assert!(!ok_inst, "expected failure on Instant::now");
    assert!(
        out_inst.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_inst}"
    );
}

#[test]
fn purity_lint_catches_std_env_while_allowing_compile_time_env_macro() {
    let (ok_macro, out_macro) =
        run_purity_on_planted_core("pub const VER: &str = env!(\"CARGO_PKG_VERSION\");\n");
    assert!(
        ok_macro,
        "compile-time env! macro must remain allowed in sk-core, got:\n{out_macro}"
    );

    let (ok_direct, out_direct) =
        run_purity_on_planted_core("pub fn bad() { let _ = std::env::var(\"HOME\"); }\n");
    assert!(!ok_direct, "expected failure on std::env");
    assert!(
        out_direct.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_direct}"
    );

    let (ok_grouped, out_grouped) = run_purity_on_planted_core(
        "use std::{fmt, env};\npub fn bad() { let _ = env::var_os(\"SHIKI_HOME\"); }\n",
    );
    assert!(!ok_grouped, "expected failure on grouped std::env usage");
    assert!(
        out_grouped.contains("Purity violation"),
        "expected Purity violation diagnostic, got:\n{out_grouped}"
    );
}

#[test]
fn purity_lint_catches_process_and_stdio() {
    let (ok_proc, out_proc) = run_purity_on_planted_core(
        "pub fn bad() { let _ = std::process::Command::new(\"sh\"); }\n",
    );
    assert!(!ok_proc, "expected failure on std::process");
    assert!(out_proc.contains("Purity violation"));

    let (ok_io, out_io) =
        run_purity_on_planted_core("pub fn bad() { let _ = std::io::stdout(); }\n");
    assert!(!ok_io, "expected failure on std::io::stdout");
    assert!(out_io.contains("Purity violation"));
}
