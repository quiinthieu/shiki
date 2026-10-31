#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_core::palette::{compute_frame, ModelConfig, Palette, PaletteColors};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_store::atomic::atomic_write;
use sk_store::config::{get_config_value, list_config_entries, load_config, set_config_value};
use sk_store::moment::resolve_moment_query;
use sk_store::store::{
    compute_frame_hash, load_snapshot, load_state, persist_applied_frame, persist_reset, Snapshot,
    SNAPSHOT_SCHEMA_VERSION, STATE_SCHEMA_VERSION,
};
use std::fs;
use support::TestEnv;

#[test]
fn config_get_and_set_validate_values_enforce_i_know_and_preserve_unknown_keys() {
    let env = TestEnv::new();
    let paths = env.paths();

    assert_eq!(get_config_value(paths, "palette").unwrap(), "shiki");
    assert_eq!(
        get_config_value(paths, "location.mode").unwrap(),
        "timezone"
    );
    assert_eq!(
        get_config_value(paths, "contrast.fg_bg_min_ratio").unwrap(),
        "4.5"
    );
    assert_eq!(
        get_config_value(paths, "contrast.ansi_bg_min_ratio").unwrap(),
        "3"
    );
    assert_eq!(
        get_config_value(paths, "delivery.tty_broadcast").unwrap(),
        "false"
    );

    let all_defaults = list_config_entries(paths).unwrap();
    assert!(all_defaults
        .iter()
        .any(|(k, v)| k == &"palette" && v == "shiki"));

    // Seed an unknown user/future key to verify non-destructive config updates.
    atomic_write(&paths.config_file(), b"custom_note = \"keep-me\"\n").unwrap();

    // Setting an invalid/nonexistent palette must fail without modifying config.toml.
    let bad_palette = set_config_value(paths, "palette", "does-not-exist", false);
    assert!(bad_palette.is_err());
    assert_eq!(get_config_value(paths, "palette").unwrap(), "shiki");

    // Lowering WCAG contrast below defaults without `allow_lower_contrast` (`--i-know`) fails.
    let denied = set_config_value(paths, "contrast.fg_bg_min_ratio", "3.8", false)
        .expect_err("lowering contrast without --i-know must fail");
    assert!(denied.to_string().contains("--i-know"));
    assert_eq!(
        get_config_value(paths, "contrast.fg_bg_min_ratio").unwrap(),
        "4.5"
    );

    // Passing `allow_lower_contrast = true` succeeds.
    set_config_value(paths, "contrast.fg_bg_min_ratio", "3.8", true).unwrap();
    assert_eq!(
        get_config_value(paths, "contrast.fg_bg_min_ratio").unwrap(),
        "3.8"
    );

    // Raising contrast above defaults does not require `--i-know`.
    set_config_value(paths, "contrast.fg_bg_min_ratio", "7.0", false).unwrap();
    assert_eq!(
        get_config_value(paths, "contrast.fg_bg_min_ratio").unwrap(),
        "7"
    );

    // Coordinate values round to 0.1 deg and validate bounds without leaking raw input on error.
    set_config_value(paths, "location.latitude", "35.6895", false).unwrap();
    set_config_value(paths, "location.longitude", "139.6917", false).unwrap();
    set_config_value(paths, "location.mode", "manual", false).unwrap();
    assert_eq!(
        get_config_value(paths, "location.latitude").unwrap(),
        "35.7"
    );
    assert_eq!(
        get_config_value(paths, "location.longitude").unwrap(),
        "139.7"
    );

    let out_of_range = set_config_value(paths, "location.latitude", "95.4321", false)
        .expect_err("latitude > 90 must fail");
    assert!(!out_of_range.to_string().contains("95.4321"));

    // Unknown key is rejected cleanly.
    assert!(get_config_value(paths, "unknown.key").is_err());
    assert!(set_config_value(paths, "unknown.key", "1", false).is_err());

    // Unrelated TOML keys are preserved.
    let saved_raw = fs::read_to_string(paths.config_file()).unwrap();
    assert!(saved_raw.contains("custom_note = \"keep-me\""));
    let parsed = load_config(paths).unwrap();
    assert_eq!(parsed.effective_palette(), "shiki");
}

fn sample_seq_bytes(colors: &PaletteColors) -> Vec<u8> {
    format!(
        "\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\\x1b]10;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        colors.bg.r, colors.bg.g, colors.bg.b, colors.fg.r, colors.fg.g, colors.fg.b
    )
    .into_bytes()
}

#[test]
fn store_persists_snapshot_state_current_seq_under_lock_without_coordinates() {
    let env = TestEnv::new();
    let paths = env.paths();
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let tokyo = Coordinates::new(35.7, 139.7).unwrap();

    let t0 = UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap();
    let frame0 = compute_frame(t0, tokyo, &palette, &model);
    let snapshot0 = Snapshot::from_frame("shiki", &frame0, tokyo, &model);
    let osc0 = sample_seq_bytes(&frame0.colors);

    let state1 = persist_applied_frame(paths, "shiki", &snapshot0, &osc0).unwrap();
    assert_eq!(state1.schema_version, STATE_SCHEMA_VERSION);
    assert_eq!(state1.generation, 1);
    assert_eq!(
        state1.frame_hash.as_deref(),
        Some(compute_frame_hash(&frame0.colors).as_str())
    );
    assert_eq!(fs::read(paths.current_seq_file()).unwrap(), osc0);
    assert!(paths.lock_file().exists());

    // Re-applying identical frame colours keeps `generation` unchanged.
    let state1_repeat = persist_applied_frame(paths, "shiki", &snapshot0, &osc0).unwrap();
    assert_eq!(state1_repeat.generation, 1);

    // Check persisted `snapshot.json`: versioned, coordinate-free, matches golden snapshot.
    let raw_snapshot_json = fs::read_to_string(paths.snapshot_file()).unwrap();
    assert!(!raw_snapshot_json.contains("35.7"));
    assert!(!raw_snapshot_json.contains("139.7"));
    assert!(!raw_snapshot_json.contains("latitude"));
    assert!(!raw_snapshot_json.contains("longitude"));

    let loaded_snapshot = load_snapshot(paths).unwrap().expect("snapshot exists");
    assert_eq!(loaded_snapshot.schema_version, SNAPSHOT_SCHEMA_VERSION);
    assert_eq!(loaded_snapshot, snapshot0);
    insta::assert_snapshot!("snapshot_json_shiki_spring_dawn", raw_snapshot_json);

    // Applying a distinct frame increments `generation`.
    let t1 = UtcInstant::parse_rfc3339("2026-06-22T02:45:00Z").unwrap();
    let frame1 = compute_frame(t1, tokyo, &palette, &model);
    let snapshot1 = Snapshot::from_frame("shiki", &frame1, tokyo, &model);
    let osc1 = sample_seq_bytes(&frame1.colors);
    let state2 = persist_applied_frame(paths, "shiki", &snapshot1, &osc1).unwrap();
    assert_eq!(state2.generation, 2);

    // Reset clears `current.seq`, clears `frame_hash`, and increments `generation`.
    let state3 = persist_reset(paths, t1.add_secs(60))
        .unwrap()
        .expect("state exists");
    assert_eq!(state3.generation, 3);
    assert_eq!(state3.frame_hash, None);
    assert!(fs::read(paths.current_seq_file()).unwrap().is_empty());
    assert_eq!(load_state(paths).unwrap(), state3);
}

#[test]
fn moment_query_resolves_rfc3339_explicit_offsets_and_iana_local_wall_times() {
    let fallback = UtcInstant::parse_rfc3339("2026-03-20T12:00:00Z").unwrap();

    assert_eq!(
        resolve_moment_query(None, None, Some("Asia/Tokyo"), fallback).unwrap(),
        fallback
    );

    let expected_dawn = UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap();
    assert_eq!(
        resolve_moment_query(None, Some("2026-03-20T20:35:00Z"), None, fallback).unwrap(),
        expected_dawn
    );
    assert_eq!(
        resolve_moment_query(Some("2026-03-21"), Some("05:35+09:00"), None, fallback).unwrap(),
        expected_dawn
    );
    assert_eq!(
        resolve_moment_query(
            Some("2026-03-21"),
            Some("05:35"),
            Some("Asia/Tokyo"),
            fallback
        )
        .unwrap(),
        expected_dawn
    );

    // Date-only defaults to 12:00:00 local noon in the resolved timezone (`12:00 JST = 03:00Z`).
    assert_eq!(
        resolve_moment_query(Some("2026-06-21"), None, Some("Asia/Tokyo"), fallback).unwrap(),
        UtcInstant::parse_rfc3339("2026-06-21T03:00:00Z").unwrap()
    );

    // Time-only uses fallback's local calendar date in `Asia/Tokyo` (`2026-03-20T12:00Z` is `2026-03-20 21:00 JST`).
    assert_eq!(
        resolve_moment_query(None, Some("05:35:00"), Some("Asia/Tokyo"), fallback).unwrap(),
        UtcInstant::parse_rfc3339("2026-03-19T20:35:00Z").unwrap()
    );

    // Passing both `--date` and a full RFC 3339 `--time` is rejected with a clear message.
    assert!(resolve_moment_query(
        Some("2026-03-20"),
        Some("2026-03-20T20:35:00Z"),
        None,
        fallback
    )
    .is_err());
}
