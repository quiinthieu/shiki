//! Architectural dependency-graph, allowlist, and product single-source tests (`DependencyGraphTests`).

use serde_json::Value;
use sk_core::product;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/shiki-term must sit two levels under workspace root")
        .join("Cargo.toml")
}

fn workspace_root() -> PathBuf {
    workspace_manifest_path()
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn load_workspace_metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(workspace_manifest_path())
        .output()
        .expect("cargo metadata must execute");

    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("cargo metadata output must be valid JSON")
}

fn has_adr_mentioning(root: &Path, dep_name: &str) -> bool {
    let decisions_dir = root.join("docs").join("decisions");
    let Ok(entries) = fs::read_dir(decisions_dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Ok(contents) = fs::read_to_string(&path) {
                if contents.contains(dep_name) {
                    return true;
                }
            }
        }
    }
    false
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("valid dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn dependency_graph_matches_plan_spec() {
    let root = workspace_root();
    let metadata = load_workspace_metadata();

    let packages = metadata["packages"]
        .as_array()
        .expect("packages must be an array");

    let workspace_members: HashSet<String> = packages
        .iter()
        .map(|pkg| {
            pkg["name"]
                .as_str()
                .expect("package name must be string")
                .to_string()
        })
        .collect();

    let expected_members: HashSet<String> = [
        "sk-core",
        "sk-store",
        "sk-deliver",
        "sk-import",
        product::CLI_CRATE,
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(
        workspace_members, expected_members,
        "workspace members must match PLAN §3"
    );

    // Allowed third-party dependencies from PLAN §3.
    let allowed_runtime: HashSet<&str> = [
        "serde",
        "serde_json",
        "toml",
        "plist",
        "clap",
        "crossterm",
        "directories",
        "thiserror",
        "anyhow",
        "fs4",
        "rustix",
    ]
    .into_iter()
    .collect();

    // Dependencies explicitly listed in PLAN §3 as gated on an ADR in docs/decisions/NNN-*.md.
    let adr_gated_runtime: HashSet<&str> = [
        "jiff",
        "time",
        "sunrise",
        "spa",
        "serde_yml",
        "serde_yaml",
        "yaml-rust2",
        "saphyr",
    ]
    .into_iter()
    .collect();

    // Allowed dev-only dependencies from PLAN §3 (`palette` is a test oracle only).
    let allowed_dev_only: HashSet<&str> =
        ["insta", "proptest", "assert_cmd", "tempfile", "palette"]
            .into_iter()
            .collect();

    let mut internal_deps: HashMap<String, HashSet<String>> = HashMap::new();
    let mut external_runtime_used: HashSet<String> = HashSet::new();

    for pkg in packages {
        let pkg_name = pkg["name"].as_str().expect("package name");
        let deps = pkg["dependencies"]
            .as_array()
            .expect("dependencies must be array");

        if pkg_name == product::CLI_CRATE {
            let bin_names: HashSet<&str> = pkg["targets"]
                .as_array()
                .expect("targets array")
                .iter()
                .filter(|target| {
                    target["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")))
                })
                .filter_map(|target| target["name"].as_str())
                .collect();
            assert!(
                bin_names.contains(product::CLI_BIN),
                "CLI crate '{}' must expose binary '{}', got {:?}",
                product::CLI_CRATE,
                product::CLI_BIN,
                bin_names
            );
        }

        let mut pkg_internal = HashSet::new();
        for dep in deps {
            let dep_name = dep["name"].as_str().expect("dep name");
            let dep_kind = dep["kind"].as_str(); // None => normal, Some("dev") => dev, Some("build") => build

            if workspace_members.contains(dep_name) {
                pkg_internal.insert(dep_name.to_string());
                continue;
            }

            if dep_name == "anyhow" {
                assert_eq!(
                    pkg_name,
                    product::CLI_CRATE,
                    "anyhow is allowed only in the binary crate (PLAN §3)"
                );
            }

            if dep_name == "palette" {
                assert_eq!(
                    dep_kind,
                    Some("dev"),
                    "palette is allowed as a dev-dependency test oracle only (PLAN §3)"
                );
            }

            let is_adr_approved =
                adr_gated_runtime.contains(dep_name) && has_adr_mentioning(&root, dep_name);

            match dep_kind {
                Some("dev") => {
                    assert!(
                        allowed_runtime.contains(dep_name)
                            || allowed_dev_only.contains(dep_name)
                            || is_adr_approved,
                        "dev dependency '{dep_name}' in '{pkg_name}' is not in PLAN §3 (requires an ADR in docs/decisions/)"
                    );
                }
                _ => {
                    external_runtime_used.insert(dep_name.to_string());
                    assert!(
                        allowed_runtime.contains(dep_name) || is_adr_approved,
                        "dependency '{dep_name}' in '{pkg_name}' is not in PLAN §3 runtime allowlist (requires an ADR in docs/decisions/)"
                    );
                }
            }
        }
        internal_deps.insert(pkg_name.to_string(), pkg_internal);
    }

    assert!(
        !(external_runtime_used.contains("jiff") && external_runtime_used.contains("time")),
        "PLAN §3 permits either `jiff` or `time` via ADR, not both"
    );

    // Expected internal dependencies from PLAN §3:
    // "Every crate may depend on `sk-core`, and only `shiki-term` depends on the others.
    //  `sk-core` depends on nothing internal."
    assert_eq!(
        internal_deps.get("sk-core").unwrap(),
        &HashSet::new(),
        "sk-core must depend on nothing internal"
    );

    let only_core: HashSet<String> = ["sk-core".to_string()].into_iter().collect();
    for sibling in ["sk-store", "sk-deliver", "sk-import"] {
        let actual = internal_deps.get(sibling).unwrap();
        assert!(
            actual.is_subset(&only_core),
            "{sibling} may only depend on sk-core internally, got {actual:?}"
        );
        assert_eq!(actual, &only_core, "{sibling} must depend only on sk-core");
    }

    let cli_expected: HashSet<String> = [
        "sk-core".to_string(),
        "sk-store".to_string(),
        "sk-deliver".to_string(),
        "sk-import".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        internal_deps.get(product::CLI_CRATE).unwrap(),
        &cli_expected,
        "{} must depend on sk-core, sk-store, sk-deliver, and sk-import",
        product::CLI_CRATE
    );
}

#[test]
fn sk_core_has_no_impure_dependencies() {
    let metadata = load_workspace_metadata();
    let packages = metadata["packages"].as_array().expect("packages array");
    let sk_core = packages
        .iter()
        .find(|pkg| pkg["name"].as_str() == Some("sk-core"))
        .expect("sk-core package");

    let impure_crates: HashSet<&str> = [
        "anyhow",
        "assert_cmd",
        "clap",
        "crossterm",
        "directories",
        "fs4",
        "plist",
        "rustix",
        "tempfile",
    ]
    .into_iter()
    .collect();

    for dep in sk_core["dependencies"].as_array().expect("deps array") {
        let dep_name = dep["name"].as_str().expect("dep name");
        assert!(
            !impure_crates.contains(dep_name),
            "sk-core is pure and must not depend on `{dep_name}` (PLAN §0 #3)"
        );
    }
}

#[test]
fn product_names_are_single_sourced_in_product_rs() {
    let root = workspace_root();
    let product_rs = root
        .join("crates/sk-core/src/product.rs")
        .canonicalize()
        .expect("crates/sk-core/src/product.rs must exist");

    let mut rs_files = Vec::new();
    for crate_dir in [
        "sk-core",
        "sk-store",
        "sk-deliver",
        "sk-import",
        product::CLI_CRATE,
    ] {
        collect_rs_files(
            &root.join("crates").join(crate_dir).join("src"),
            &mut rs_files,
        );
    }

    let forbidden_literals = [
        format!("\"{}\"", product::DISPLAY_NAME),
        format!("\"{}\"", product::CLI_BIN),
        format!("\"{}\"", product::HOMEBREW_CASK),
        format!("\"{}\"", product::BUNDLE_ID),
    ];

    for file in rs_files {
        if file.canonicalize().expect("canonicalize source file") == product_rs {
            continue;
        }
        let src = fs::read_to_string(&file).expect("read source file");
        for literal in &forbidden_literals {
            assert!(
                !src.contains(literal),
                "hardcoded product string {literal} found in {}; use `sk_core::product` constants instead",
                file.display()
            );
        }
    }

    let main_rs = fs::read_to_string(
        root.join("crates")
            .join(product::CLI_CRATE)
            .join("src/main.rs"),
    )
    .expect("read main.rs");
    assert!(
        main_rs.contains("product::CLI_BIN") && main_rs.contains("product::VERSION"),
        "main.rs must wire CLI binary name and version from sk_core::product"
    );

    let xcconfig = root.join("macos/Config/Product.xcconfig");
    if xcconfig.exists() {
        let contents = fs::read_to_string(&xcconfig).expect("read Product.xcconfig");
        for expected in [product::DISPLAY_NAME, product::CLI_BIN, product::BUNDLE_ID] {
            assert!(
                contents.contains(expected),
                "macos/Config/Product.xcconfig must mirror `{expected}` from product.rs"
            );
        }
    }
}

#[test]
fn toolchain_versions_stay_in_sync() {
    let root = workspace_root();

    let rust_toolchain =
        fs::read_to_string(root.join("rust-toolchain.toml")).expect("read rust-toolchain.toml");
    let channel = rust_toolchain
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("channel")
                .map(|rest| rest.trim().trim_start_matches('=').trim().trim_matches('"'))
        })
        .expect("channel in rust-toolchain.toml");

    let root_cargo = fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    assert!(
        root_cargo.contains(&format!("rust-version = \"{channel}\"")),
        "root Cargo.toml rust-version must match rust-toolchain.toml channel ({channel})"
    );

    let clippy_toml = fs::read_to_string(root.join("clippy.toml")).expect("read clippy.toml");
    assert!(
        clippy_toml.contains(&format!("msrv = \"{channel}\"")),
        "clippy.toml msrv must match rust-toolchain.toml channel ({channel})"
    );

    let ci_yml = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    assert!(
        ci_yml.contains(&format!("toolchain: \"{channel}\"")),
        ".github/workflows/ci.yml must explicitly pin toolchain: \"{channel}\" to match rust-toolchain.toml"
    );
    assert!(
        ci_yml.contains("tool: cargo-deny"),
        ".github/workflows/ci.yml must pass tool: cargo-deny to taiki-e/install-action"
    );
    assert!(
        ci_yml.contains("./scripts/lint-privacy.sh"),
        ".github/workflows/ci.yml must run ./scripts/lint-privacy.sh"
    );
    assert!(
        ci_yml.contains("./scripts/lint-core-purity.sh"),
        ".github/workflows/ci.yml must run ./scripts/lint-core-purity.sh"
    );
}
