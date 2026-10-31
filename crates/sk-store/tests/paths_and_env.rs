#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_store::error::StoreError;
use sk_store::paths::{PathEnv, PlatformKind, ShikiPaths};
use std::fs;
use std::path::{Path, PathBuf};
use support::{fake_clock_utc, fixed_rfc3339, fixed_utc, TestEnv};

fn norm(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn format_layout(label: &str, paths: &ShikiPaths) -> String {
    [
        format!("[{label}]"),
        format!("config_dir = {}", norm(paths.config_dir())),
        format!("state_dir = {}", norm(paths.state_dir())),
        format!("runtime_dir = {}", norm(paths.runtime_dir())),
        format!("config_file = {}", norm(&paths.config_file())),
        format!("palettes_dir = {}", norm(&paths.palettes_dir())),
        format!("state_file = {}", norm(&paths.state_file())),
        format!("snapshot_file = {}", norm(&paths.snapshot_file())),
        format!("current_seq_file = {}", norm(&paths.current_seq_file())),
        format!("lock_file = {}", norm(&paths.lock_file())),
    ]
    .join("\n")
}

#[test]
fn resolved_path_layouts_match_golden_snapshot() {
    let macos = ShikiPaths::resolve_with(
        PlatformKind::MacOs,
        &PathEnv {
            home: Some(PathBuf::from("/Users/alice")),
            ..PathEnv::default()
        },
    )
    .expect("resolve macOS default paths");

    let linux_defaults = ShikiPaths::resolve_with(
        PlatformKind::Linux,
        &PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            ..PathEnv::default()
        },
    )
    .expect("resolve Linux default XDG paths");

    let linux_explicit = ShikiPaths::resolve_with(
        PlatformKind::Linux,
        &PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            shiki_home: None,
            xdg_config_home: Some(PathBuf::from("/xdg/cfg")),
            xdg_state_home: Some(PathBuf::from("/xdg/st")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
        },
    )
    .expect("resolve Linux explicit XDG paths");

    let shiki_home_override = ShikiPaths::resolve_with(
        PlatformKind::MacOs,
        &PathEnv {
            home: None,
            shiki_home: Some(PathBuf::from("/custom/shiki-home")),
            ..PathEnv::default()
        },
    )
    .expect("SHIKI_HOME must resolve even without HOME");

    let linux_override = ShikiPaths::resolve_with(
        PlatformKind::Linux,
        &PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            shiki_home: Some(PathBuf::from("/custom/shiki-home")),
            xdg_config_home: Some(PathBuf::from("/xdg/cfg")),
            xdg_state_home: Some(PathBuf::from("/xdg/st")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
        },
    )
    .expect("SHIKI_HOME must override Linux XDG vars");
    assert_eq!(shiki_home_override, linux_override);

    let report = [
        format_layout("macos_default", &macos),
        format_layout("linux_xdg_defaults", &linux_defaults),
        format_layout("linux_xdg_explicit", &linux_explicit),
        format_layout("shiki_home_override", &shiki_home_override),
    ]
    .join("\n\n");

    insta::assert_snapshot!("resolved_path_layouts", report);
}

#[test]
fn missing_home_and_empty_env_values_are_handled_cleanly() {
    let err = ShikiPaths::resolve_with(
        PlatformKind::Linux,
        &PathEnv {
            home: Some(PathBuf::from("")),
            shiki_home: Some(PathBuf::from("")),
            ..PathEnv::default()
        },
    )
    .expect_err("empty HOME and SHIKI_HOME must fail with MissingHomeDirectory");
    assert!(matches!(err, StoreError::MissingHomeDirectory));

    // Empty XDG strings must fall back to XDG defaults per the XDG Base Directory Specification.
    let linux = ShikiPaths::resolve_with(
        PlatformKind::Linux,
        &PathEnv {
            home: Some(PathBuf::from("/home/bob")),
            shiki_home: Some(PathBuf::from("")),
            xdg_config_home: Some(PathBuf::from("")),
            xdg_state_home: Some(PathBuf::from("")),
            xdg_runtime_dir: Some(PathBuf::from("")),
        },
    )
    .expect("empty XDG vars fall back to HOME");
    assert_eq!(
        linux.config_dir(),
        Path::new("/home/bob/.config/shiki-term")
    );
    assert_eq!(
        linux.runtime_dir(),
        Path::new("/home/bob/.local/state/shiki-term")
    );
}

#[test]
fn test_env_isolates_directories_cleans_up_on_drop_and_loads_fixtures() {
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    let root_snapshot: PathBuf;

    {
        let env = TestEnv::new();
        root_snapshot = env.root().to_path_buf();
        assert!(root_snapshot.is_dir());

        if let Some(ref real) = real_home {
            assert_ne!(env.home_dir(), real.as_path());
            assert!(!env.shiki_home().starts_with(real));
        }

        for (_, path) in env.env_vars() {
            assert!(
                path.starts_with(env.root()),
                "{} must live inside TestEnv root {}",
                path.display(),
                env.root().display()
            );
            assert!(path.is_dir());
        }

        assert!(env.paths().palettes_dir().is_dir());

        // Fixed-instant helpers exposed by support module.
        let instant = fixed_utc(2026, 6, 21, 12, 0, 0);
        assert_eq!(instant, fixed_rfc3339("2026-06-21T19:00:00+07:00"));
        let clock = fake_clock_utc(2026, 6, 21, 12, 0, 0);
        assert_eq!(clock.advance_minutes(15), fixed_utc(2026, 6, 21, 12, 15, 0));

        // Fixture loading across all three fixture categories.
        let solar = env
            .fixtures()
            .read_solar("harness_smoke.toml")
            .expect("read solar smoke fixture");
        assert!(solar.contains("Greenwich"));

        let theme = env
            .fixtures()
            .read_theme("minimal_base16.yaml")
            .expect("read theme smoke fixture");
        assert!(theme.contains("base16"));

        let palette = env
            .fixtures()
            .read_palette("minimal_sample.toml")
            .expect("read palette smoke fixture");
        assert!(palette.contains("minimal-sample"));

        let staged = env
            .fixtures()
            .stage_palette("minimal_sample.toml", env.paths())
            .expect("stage palette into TestEnv palettes_dir");
        assert_eq!(
            staged,
            env.paths().palettes_dir().join("minimal_sample.toml")
        );
        assert_eq!(
            fs::read_to_string(&staged).expect("read staged palette"),
            palette
        );

        // Path traversal and nested paths must be rejected.
        for bad_name in [
            "",
            ".",
            "..",
            "../Cargo.toml",
            "sub/file.toml",
            "sub\\file.toml",
            "/etc/passwd",
        ] {
            let err = env
                .fixtures()
                .read_solar(bad_name)
                .expect_err("invalid fixture name must be rejected");
            assert!(
                matches!(err, StoreError::InvalidFixtureName(_)),
                "expected InvalidFixtureName for '{bad_name}', got {err:?}"
            );
        }

        let missing = env
            .fixtures()
            .read_solar("does_not_exist.toml")
            .expect_err("missing fixture must return FixtureNotFound");
        assert!(matches!(missing, StoreError::FixtureNotFound { .. }));
    }

    assert!(
        !root_snapshot.exists(),
        "TestEnv TempDir {} must be removed on Drop",
        root_snapshot.display()
    );
}

#[test]
fn test_env_and_fixture_staging_pass_100x_in_a_loop() {
    for iteration in 0..100 {
        let env = TestEnv::new();
        let root = env.root().to_path_buf();

        let staged = env
            .fixtures()
            .stage_palette("minimal_sample.toml", env.paths())
            .unwrap_or_else(|e| panic!("stage_palette failed on iteration {iteration}: {e}"));
        assert!(staged.is_file());

        fs::write(env.paths().config_file(), "palette = \"minimal-sample\"\n")
            .unwrap_or_else(|e| panic!("write config_file failed on iteration {iteration}: {e}"));
        assert!(env.paths().config_file().is_file());

        drop(env);
        assert!(
            !root.exists(),
            "TestEnv root leaked on iteration {iteration}"
        );
    }
}

#[test]
fn test_env_loads_noaa_solar_fixtures_for_pure_solar_model() {
    let env = TestEnv::new();
    let greenwich_raw = env
        .fixtures()
        .read_solar("noaa_greenwich.toml")
        .expect("read NOAA Greenwich fixture via TestEnv");
    assert!(greenwich_raw.contains("Capture URL: https://gml.noaa.gov/grad/solcalc/"));
    assert!(greenwich_raw.contains("Capture date: 2026-10-31"));

    let coords = sk_core::solar::Coordinates::new(51.4769, 0.0).expect("Greenwich coordinates");
    let angles =
        sk_core::solar::SolarEventAngles::from_model_toml(sk_core::solar::DEFAULT_MODEL_TOML)
            .expect("load default solar event angles");
    let equinox_noon = fixed_utc(2026, 3, 20, 12, 7, 26);
    let snap = sk_core::solar::solar_snapshot(equinox_noon, coords, &angles);
    assert_eq!(snap.polar_state, sk_core::solar::PolarState::Normal);
    assert!((snap.noon_elevation_deg - 38.5021).abs() < 0.02);
}
