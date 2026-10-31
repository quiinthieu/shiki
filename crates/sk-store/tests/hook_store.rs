#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_core::location::ManualLocation;
use sk_core::palette::{compute_frame, ModelConfig, Palette, PaletteColors};
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_store::atomic::atomic_write;
use sk_store::config::{set_config_value, set_manual_location};
use sk_store::store::{
    expire_generation_freshness, is_hook_store_fresh, load_generation_record, load_state,
    persist_applied_frame, persist_reset, refresh_hook_store, GenerationRecord, HookRefreshOutcome,
    Snapshot,
};
use std::fs;
use support::TestEnv;

fn sample_osc_bytes(colors: &PaletteColors) -> Vec<u8> {
    format!(
        "\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\\x1b]10;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        colors.bg.r, colors.bg.g, colors.bg.b, colors.fg.r, colors.fg.g, colors.fg.b
    )
    .into_bytes()
}

#[test]
fn generation_record_round_trips_and_tolerates_legacy_single_and_two_field_records() {
    let rec = GenerationRecord {
        generation: 7,
        fresh_until_unix_secs: 1_774_038_960,
        interval_secs: 60,
    };
    let encoded = String::from_utf8(rec.encode()).unwrap();
    assert_eq!(encoded, "7 1774038960 60\n");
    assert_eq!(GenerationRecord::parse(&encoded), Some(rec));
    assert_eq!(
        GenerationRecord::parse("7 1774038960\n"),
        Some(GenerationRecord {
            generation: 7,
            fresh_until_unix_secs: 1_774_038_960,
            interval_secs: 0,
        })
    );
    assert_eq!(
        GenerationRecord::parse("42\n"),
        Some(GenerationRecord {
            generation: 42,
            fresh_until_unix_secs: 0,
            interval_secs: 0,
        })
    );
    assert_eq!(GenerationRecord::parse(""), None);
    assert_eq!(GenerationRecord::parse("not-a-number 10 60"), None);
}

#[test]
fn persist_applied_frame_and_reset_keep_current_gen_in_sync_with_state_json() {
    let env = TestEnv::new();
    let paths = env.paths();
    set_manual_location(paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let coords = Coordinates::new(35.7, 139.7).unwrap();

    let t0 = UtcInstant::parse_rfc3339("2026-03-20T20:35:00Z").unwrap();
    assert!(!is_hook_store_fresh(paths, t0.as_unix_secs()).unwrap());
    assert_eq!(load_generation_record(paths).unwrap(), None);

    let frame0 = compute_frame(t0, coords, &palette, &model);
    let snap0 = Snapshot::from_frame("shiki", &frame0, coords, &model);
    let osc0 = sample_osc_bytes(&frame0.colors);

    let state1 = persist_applied_frame(paths, "shiki", &snap0, &osc0).unwrap();
    assert_eq!(state1.generation, 1);

    let gen1 = load_generation_record(paths).unwrap().expect("current.gen");
    assert_eq!(gen1.generation, 1);
    assert_eq!(gen1.fresh_until_unix_secs, t0.as_unix_secs() + 60);
    assert_eq!(gen1.interval_secs, 60);
    assert!(is_hook_store_fresh(paths, t0.as_unix_secs() + 59).unwrap());
    assert!(!is_hook_store_fresh(paths, t0.as_unix_secs() + 60).unwrap());

    // Coordinate privacy: `current.gen`, `state.json`, and `snapshot.json` never contain coordinates.
    for path in [
        paths.current_gen_file(),
        paths.state_file(),
        paths.snapshot_file(),
    ] {
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("35.7") && !raw.contains("139.7"),
            "coordinates leaked in {}",
            path.display()
        );
    }

    // `persist_reset` increments `current.gen` alongside `state.json` and clears `current.seq`.
    let t_reset = t0.add_secs(10);
    let state2 = persist_reset(paths, t_reset).unwrap().expect("reset state");
    assert_eq!(state2.generation, 2);
    let gen2 = load_generation_record(paths).unwrap().expect("current.gen");
    assert_eq!(gen2.generation, 2);
    assert_eq!(gen2.fresh_until_unix_secs, t_reset.as_unix_secs() + 60);
    assert!(fs::read(paths.current_seq_file()).unwrap().is_empty());
}

#[test]
fn refresh_hook_store_respects_staleness_window_step_threshold_and_config_changes() {
    let env = TestEnv::new();
    let paths = env.paths();
    set_manual_location(paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

    // Start deep in astronomical night in Tokyo (`2026-03-20T15:00:00Z` = `00:00 JST`, midnight).
    let t_night = UtcInstant::parse_rfc3339("2026-03-20T15:00:00Z").unwrap();

    // 1. Cold start with `if_stale = true` initializes store at generation 1.
    let mut encoder_calls = 0usize;
    let out1 = refresh_hook_store(paths, true, None, t_night, |colors| {
        encoder_calls += 1;
        sample_osc_bytes(colors)
    })
    .unwrap();
    assert_eq!(out1, HookRefreshOutcome::Updated { generation: 1 });
    assert_eq!(encoder_calls, 1);
    let seq1 = fs::read(paths.current_seq_file()).unwrap();
    assert!(!seq1.is_empty());

    // 2. Within `hook_min_interval_secs` (30s later), `if_stale = true` short-circuits cleanly.
    let out2 = refresh_hook_store(paths, true, None, t_night.add_secs(30), |colors| {
        encoder_calls += 1;
        sample_osc_bytes(colors)
    })
    .unwrap();
    assert_eq!(out2, HookRefreshOutcome::Fresh { generation: 1 });
    assert_eq!(encoder_calls, 1);

    // 3. Past `hook_min_interval_secs` (65s after midnight, still deep night `d = 0.0`),
    //    `refresh_hook_store` recomputes once, sees `max_delta_e < step_threshold` (0.0),
    //    keeps `generation = 1`, and extends `fresh_until_unix_secs`.
    let t_night_stale = t_night.add_secs(65);
    let out3 = refresh_hook_store(paths, true, None, t_night_stale, |colors| {
        encoder_calls += 1;
        sample_osc_bytes(colors)
    })
    .unwrap();
    assert_eq!(out3, HookRefreshOutcome::Unchanged { generation: 1 });
    assert_eq!(encoder_calls, 1, "encoder must not run on unchanged frame");
    let gen_after_unchanged = load_generation_record(paths).unwrap().unwrap();
    assert_eq!(gen_after_unchanged.generation, 1);
    assert_eq!(
        gen_after_unchanged.fresh_until_unix_secs,
        t_night_stale.as_unix_secs() + 60
    );

    // 4. Advance into Tokyo dawn (`2026-03-20T20:45:00Z` = `05:45 JST`): colours shift well above
    //    `step_threshold`, advancing `generation` to 2 and rewriting `current.seq`.
    let t_dawn = UtcInstant::parse_rfc3339("2026-03-20T20:45:00Z").unwrap();
    let out4 = refresh_hook_store(paths, true, None, t_dawn, |colors| {
        encoder_calls += 1;
        sample_osc_bytes(colors)
    })
    .unwrap();
    assert_eq!(out4, HookRefreshOutcome::Updated { generation: 2 });
    assert_eq!(encoder_calls, 2);
    let seq2 = fs::read(paths.current_seq_file()).unwrap();
    assert_ne!(seq2, seq1);

    // 5. Updating `config.toml` via `set_config_value` immediately expires `current.gen` freshness
    //    and invalidates `config_hash`, so even 1 second later `if_stale = true` re-evaluates.
    set_config_value(paths, "hook_min_interval_secs", "120", false).unwrap();
    let expired_gen = load_generation_record(paths).unwrap().unwrap();
    assert_eq!(expired_gen.generation, 2);
    assert_eq!(expired_gen.fresh_until_unix_secs, 0);
    assert_eq!(expired_gen.interval_secs, 0);
    assert!(!is_hook_store_fresh(paths, t_dawn.add_secs(1).as_unix_secs()).unwrap());

    // 6. `expire_generation_freshness` forces re-check on next prompt without losing generation.
    let _ = refresh_hook_store(paths, true, None, t_dawn.add_secs(1), sample_osc_bytes).unwrap();
    assert_eq!(
        load_generation_record(paths)
            .unwrap()
            .unwrap()
            .interval_secs,
        120
    );
    assert!(is_hook_store_fresh(paths, t_dawn.add_secs(90).as_unix_secs()).unwrap());
    expire_generation_freshness(paths).unwrap();
    assert!(!is_hook_store_fresh(paths, t_dawn.add_secs(90).as_unix_secs()).unwrap());
}

#[test]
fn refresh_hook_store_honours_future_paused_until_in_state_json() {
    let env = TestEnv::new();
    let paths = env.paths();
    set_manual_location(paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

    let t0 = UtcInstant::parse_rfc3339("2026-03-20T15:00:00Z").unwrap();
    let _ = refresh_hook_store(paths, false, None, t0, sample_osc_bytes).unwrap();

    // Mark paused until `2026-03-21T00:00:00Z` in `state.json`.
    let mut state = load_state(paths).unwrap();
    state.paused_until = Some("2026-03-21T00:00:00Z".to_string());
    let mut bytes = serde_json::to_vec_pretty(&state).unwrap();
    bytes.push(b'\n');
    atomic_write(&paths.state_file(), &bytes).unwrap();

    // Even hours later at dawn (well past `hook_min_interval_secs`), `if_stale = true` stays paused.
    let t_dawn = UtcInstant::parse_rfc3339("2026-03-20T20:45:00Z").unwrap();
    let outcome = refresh_hook_store(paths, true, None, t_dawn, sample_osc_bytes).unwrap();
    assert_eq!(outcome, HookRefreshOutcome::Fresh { generation: 1 });
    assert_eq!(load_state(paths).unwrap().generation, 1);
}
