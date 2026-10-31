#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_core::location::{LocationError, LocationMode, ManualLocation, ResolvedLocation, TzDb};
use sk_store::atomic::atomic_write;
use sk_store::config::{
    load_config, resolve_location_with, set_manual_location, set_timezone_location_with,
};
use sk_store::error::StoreError;
use sk_store::tz_detect::{
    detect_system_timezone_from, extract_zoneinfo_suffix, normalize_tz_spec, SystemTzSources,
};
use std::fs;
use std::path::Path;
use support::TestEnv;

#[test]
fn atomic_write_persists_bytes_sets_owner_only_permissions_and_leaves_no_temp_files() {
    let env = TestEnv::new();
    let target = env.paths().config_file();

    atomic_write(&target, b"[location]\nmode = \"timezone\"\n").expect("initial atomic_write");
    assert_eq!(
        fs::read_to_string(&target).expect("read config.toml"),
        "[location]\nmode = \"timezone\"\n"
    );

    atomic_write(&target, b"[location]\nmode = \"manual\"\n").expect("overwrite atomic_write");
    assert_eq!(
        fs::read_to_string(&target).expect("read overwritten config.toml"),
        "[location]\nmode = \"manual\"\n"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&target)
            .expect("stat config.toml")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "config.toml must be owner-only (0600), got {mode:o}"
        );
    }

    let entries: Vec<String> = fs::read_dir(env.paths().config_dir())
        .expect("read config_dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        !entries.iter().any(|name| name.contains(".tmp")),
        "atomic_write must not leave temporary files behind: {entries:?}"
    );
}

#[test]
fn system_timezone_detection_handles_env_symlinks_distro_files_and_jiff_fallback() {
    assert_eq!(
        extract_zoneinfo_suffix(Path::new("/var/db/timezone/zoneinfo/Asia/Saigon")),
        Some("Asia/Saigon".to_string())
    );
    assert_eq!(
        extract_zoneinfo_suffix(Path::new("/usr/share/zoneinfo/posix/Europe/Kyiv")),
        Some("Europe/Kyiv".to_string())
    );
    assert_eq!(
        extract_zoneinfo_suffix(Path::new(
            "/usr/share/zoneinfo/right/America/Argentina/Buenos_Aires"
        )),
        Some("America/Argentina/Buenos_Aires".to_string())
    );
    assert_eq!(extract_zoneinfo_suffix(Path::new("/etc/localtime")), None);

    assert_eq!(
        normalize_tz_spec(":/usr/share/zoneinfo/Asia/Ho_Chi_Minh"),
        Some("Asia/Ho_Chi_Minh".to_string())
    );
    assert_eq!(
        normalize_tz_spec(":Asia/Saigon"),
        Some("Asia/Saigon".to_string())
    );
    assert_eq!(normalize_tz_spec("   "), None);

    // 1. Explicit TZ env takes top precedence (even if unknown, so TzDb surfaces the error).
    assert_eq!(
        detect_system_timezone_from(&SystemTzSources {
            tz_env: Some("Unknown/Atlantis"),
            localtime_link_target: Some(Path::new("/var/db/timezone/zoneinfo/Asia/Ho_Chi_Minh")),
            fallback_iana_name: Some("Asia/Ho_Chi_Minh"),
            ..SystemTzSources::default()
        }),
        Some("Unknown/Atlantis".to_string())
    );

    // 2. Empty TZ="" falls through to /etc/localtime symlink.
    assert_eq!(
        detect_system_timezone_from(&SystemTzSources {
            tz_env: Some(""),
            localtime_link_target: Some(Path::new("/var/db/timezone/zoneinfo/Asia/Saigon")),
            etc_timezone_contents: Some("Europe/Berlin\n"),
            ..SystemTzSources::default()
        }),
        Some("Asia/Saigon".to_string())
    );

    // 3. /etc/timezone (Debian/Ubuntu) when /etc/localtime is not a symlink.
    assert_eq!(
        detect_system_timezone_from(&SystemTzSources {
            etc_timezone_contents: Some("# Debian timezone\n  Europe/Berlin  \n"),
            ..SystemTzSources::default()
        }),
        Some("Europe/Berlin".to_string())
    );

    // 4. /etc/sysconfig/clock (RHEL/Fedora/openSUSE).
    assert_eq!(
        detect_system_timezone_from(&SystemTzSources {
            sysconfig_clock_contents: Some("# clock\nZONE=\"Australia/Sydney\"\nUTC=true\n"),
            ..SystemTzSources::default()
        }),
        Some("Australia/Sydney".to_string())
    );

    // 5. `jiff` TZif fallback.
    assert_eq!(
        detect_system_timezone_from(&SystemTzSources {
            fallback_iana_name: Some("Pacific/Auckland"),
            ..SystemTzSources::default()
        }),
        Some("Pacific/Auckland".to_string())
    );

    assert_eq!(
        detect_system_timezone_from(&SystemTzSources::default()),
        None
    );
}

#[test]
fn config_store_roundtrips_manual_and_timezone_modes_preserving_unrelated_keys() {
    let env = TestEnv::new();
    let tzdb = TzDb::bundled();

    // Missing config.toml defaults cleanly to timezone mode without touching disk.
    let initial = load_config(env.paths()).expect("load missing config");
    assert_eq!(initial.location.mode, LocationMode::Timezone);
    assert!(!env.paths().config_file().exists());

    let default_resolved =
        resolve_location_with(&initial, Some("Asia/Saigon"), tzdb).expect("resolve default tz");
    assert!(
        matches!(default_resolved, ResolvedLocation::Timezone(ref tz) if tz.canonical_zone == "Asia/Ho_Chi_Minh")
    );

    // Seed unrelated future keys in config.toml to verify non-destructive updates.
    atomic_write(
        &env.paths().config_file(),
        b"palette = \"shiki\"\nstep_threshold = 0.015\n\n[location]\nmode = \"timezone\"\n",
    )
    .expect("seed config.toml");

    let manual = ManualLocation::new(10.76, 106.66).expect("manual coordinates");
    let resolved_manual =
        set_manual_location(env.paths(), manual).expect("persist manual coordinates");
    assert_eq!(resolved_manual, ResolvedLocation::Manual(manual));

    let raw_saved = fs::read_to_string(env.paths().config_file()).expect("read saved config.toml");
    assert!(raw_saved.contains("palette = \"shiki\""));
    assert!(raw_saved.contains("step_threshold = 0.015"));
    assert!(raw_saved.contains("mode = \"manual\""));
    assert!(raw_saved.contains("10.8"));
    assert!(raw_saved.contains("106.7"));
    // Unrounded input decimals must never be written to disk.
    assert!(!raw_saved.contains("10.76"));
    assert!(!raw_saved.contains("106.66"));

    // Attempting to switch back to timezone mode when the timezone is unknown fails BEFORE mutating config.toml.
    let bad_switch = set_timezone_location_with(env.paths(), Some("Unknown/Atlantis"), tzdb)
        .expect_err("unknown timezone must block --timezone switch");
    assert!(matches!(
        bad_switch,
        StoreError::Location(LocationError::UnknownTimezone { .. })
    ));
    let still_manual = load_config(env.paths()).expect("reload preserved manual config");
    assert_eq!(still_manual.location.mode, LocationMode::Manual);
    assert_eq!(still_manual.location.latitude, Some(10.8));

    // Switching back to timezone mode with a valid timezone clears stored manual coordinates from disk.
    let switched = set_timezone_location_with(env.paths(), Some("Asia/Saigon"), tzdb)
        .expect("switch to timezone mode");
    assert!(
        matches!(switched, ResolvedLocation::Timezone(ref tz) if tz.canonical_zone == "Asia/Ho_Chi_Minh")
    );

    let after_switch = fs::read_to_string(env.paths().config_file()).expect("read switched config");
    assert!(after_switch.contains("palette = \"shiki\""));
    assert!(after_switch.contains("mode = \"timezone\""));
    assert!(!after_switch.contains("latitude"));
    assert!(!after_switch.contains("longitude"));
    assert!(!after_switch.contains("10.8"));
    assert!(!after_switch.contains("106.7"));
}

#[test]
fn malformed_config_errors_include_line_info_and_never_leak_coordinates() {
    let env = TestEnv::new();

    for bad_toml in [
        "[location]\nmode = \"manual\"\nlatitude = 35.7\nlongitude = \"139.7\"\n",
        "[location]\nmode = \"manual\"\nlatitude = 35.7\nlongitude = 139.7\nbroken == syntax\n",
    ] {
        atomic_write(&env.paths().config_file(), bad_toml.as_bytes()).expect("write bad config");
        let err = load_config(env.paths()).expect_err("malformed config must fail");
        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(
            display.contains("line "),
            "ConfigParse error should report line number, got: {display}"
        );
        assert!(
            !display.contains("35.7")
                && !display.contains("139.7")
                && !debug.contains("35.7")
                && !debug.contains("139.7"),
            "ConfigParse leaked coordinates! display='{display}', debug='{debug}'"
        );
    }
}
