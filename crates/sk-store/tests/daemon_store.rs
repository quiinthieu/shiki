//! Integration tests for `daemon_tick`, `set_pause`/`clear_pause`, `parse_pause_duration_secs`,
//! and `DaemonPidGuard` (`PLAN.md` §6, §7, & §10 T09).

#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_core::location::ManualLocation;
use sk_core::palette::PaletteColors;
use sk_core::scheduler::MIN_DAEMON_SLEEP_SECS;
use sk_core::time::{Clock, FakeClock, UtcInstant, SECS_PER_DAY};
use sk_store::atomic::atomic_write;
use sk_store::config::{set_manual_location, DEFAULT_MAX_SLEEP_SECS};
use sk_store::error::StoreError;
use sk_store::moment::parse_pause_duration_secs;
use sk_store::paths::ShikiPaths;
use sk_store::store::{
    clear_pause, daemon_tick, is_hook_store_fresh, load_snapshot, load_state,
    probe_running_daemon_pid, refresh_hook_store, set_pause, DaemonPidGuard, DaemonTickOutcome,
    HookRefreshOutcome, PauseStatus, INDEFINITE_PAUSE,
};
use std::fs;
use std::os::unix::fs::MetadataExt;
use support::TestEnv;

fn sample_osc_bytes(colors: &PaletteColors) -> Vec<u8> {
    format!(
        "\x1b]10;rgb:{:02x}/{:02x}/{:02x}\x1b\\\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        colors.fg.r, colors.fg.g, colors.fg.b, colors.bg.r, colors.bg.g, colors.bg.b
    )
    .into_bytes()
}

#[derive(Debug, PartialEq, Eq)]
struct StoreFileIdentity {
    ino: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    bytes: Vec<u8>,
}

fn capture_store_identities(paths: &ShikiPaths) -> [StoreFileIdentity; 4] {
    [
        paths.current_seq_file(),
        paths.current_gen_file(),
        paths.snapshot_file(),
        paths.state_file(),
    ]
    .map(|path| {
        let meta = fs::metadata(&path).expect("store file metadata");
        let bytes = fs::read(&path).expect("store file bytes");
        StoreFileIdentity {
            ino: meta.ino(),
            mtime_sec: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            bytes,
        }
    })
}

#[test]
fn parse_pause_duration_accepts_human_units_and_rejects_invalid_inputs() {
    assert_eq!(parse_pause_duration_secs("30s").unwrap(), 30);
    assert_eq!(parse_pause_duration_secs("45m").unwrap(), 2_700);
    assert_eq!(parse_pause_duration_secs("2h").unwrap(), 7_200);
    assert_eq!(parse_pause_duration_secs("1h30m").unwrap(), 5_400);
    assert_eq!(parse_pause_duration_secs("2h 15m 30s").unwrap(), 8_130);
    assert_eq!(parse_pause_duration_secs("1d").unwrap(), 86_400);
    assert_eq!(parse_pause_duration_secs("15mins").unwrap(), 900);

    for bad in ["", "   ", "0s", "0m", "-10m", "30", "10w", "1h0", "400d"] {
        assert!(
            matches!(
                parse_pause_duration_secs(bad),
                Err(StoreError::InvalidPauseDuration(_))
            ),
            "expected InvalidPauseDuration for '{bad}'"
        );
    }
}

#[test]
fn fake_clock_day_daemon_ticks_respect_power_budget_and_perform_zero_writes_when_unchanged() {
    let env = TestEnv::new();
    let paths = env.paths();
    // Greenwich (51.5°N, 0.0°E) on vernal equinox (2026-03-20).
    set_manual_location(paths, ManualLocation::new(51.5, 0.0).unwrap()).unwrap();

    let start = UtcInstant::utc_ymd_hms(2026, 3, 20, 0, 0, 0);
    let end = start.add_secs(SECS_PER_DAY);
    let clock = FakeClock::new(start);

    let mut total_wakes = 0_usize;
    let mut updated_wakes = 0_usize;
    let mut unchanged_wakes = 0_usize;
    let mut encode_calls = 0_usize;
    let mut sleeps = Vec::new();

    while clock.now() < end {
        let now = clock.now();
        let before = if total_wakes > 0 {
            Some(capture_store_identities(paths))
        } else {
            None
        };

        let outcome = daemon_tick(paths, now, |colors| {
            encode_calls += 1;
            sample_osc_bytes(colors)
        })
        .expect("daemon_tick must succeed");

        total_wakes += 1;
        let sleep_secs = match outcome {
            DaemonTickOutcome::Updated {
                generation,
                sleep_secs,
                ..
            } => {
                updated_wakes += 1;
                assert_eq!(generation, updated_wakes as u64);
                sleep_secs
            }
            DaemonTickOutcome::Unchanged {
                generation,
                sleep_secs,
                ..
            } => {
                unchanged_wakes += 1;
                assert_eq!(generation, updated_wakes as u64);
                let after = capture_store_identities(paths);
                assert_eq!(
                    before.as_ref().unwrap(),
                    &after,
                    "Unchanged daemon tick at {now} must not write or replace any store file (inodes/mtimes/bytes must match)"
                );
                sleep_secs
            }
            DaemonTickOutcome::Paused { .. } => panic!("unexpected Paused outcome"),
        };

        assert!(
            sleep_secs >= MIN_DAEMON_SLEEP_SECS,
            "wake interval {sleep_secs}s violated 60s minimum"
        );
        assert!(
            sleep_secs <= DEFAULT_MAX_SLEEP_SECS,
            "wake interval {sleep_secs}s exceeded max_sleep {DEFAULT_MAX_SLEEP_SECS}s"
        );
        sleeps.push(sleep_secs);
        clock.advance_secs(i64::try_from(sleep_secs).unwrap());
    }

    assert_eq!(
        encode_calls, updated_wakes,
        "OSC encoder must only run when a frame actually changes"
    );
    assert!(
        unchanged_wakes >= 10,
        "night and midday plateaus must produce zero-write Unchanged wakes (got {unchanged_wakes})"
    );
    assert!(
        updated_wakes >= 15,
        "dawn and dusk twilights must produce smooth perceptual steps (got {updated_wakes})"
    );
    assert!(
        total_wakes <= 250,
        "daemon must not poll (got {total_wakes} wakes over 24h)"
    );
    assert!(
        sleeps.contains(&DEFAULT_MAX_SLEEP_SECS),
        "night/midday must stretch to max_sleep_secs"
    );
    assert!(
        sleeps.iter().any(|&s| s < 600),
        "twilight must schedule short wakes (< 10m)"
    );

    // Privacy check (`PLAN.md` §0 #4): neither `snapshot.json` nor `state.json` contains coordinates.
    for path in [paths.snapshot_file(), paths.state_file()] {
        let raw = fs::read_to_string(path).unwrap();
        assert!(!raw.contains("51.5"));
    }
}

#[test]
fn pause_for_duration_indefinite_pause_and_resume_coordinate_daemon_and_hook_store() {
    let env = TestEnv::new();
    let paths = env.paths();
    set_manual_location(paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

    // Initialize store at Tokyo midnight (`15:00Z`).
    let t_night = UtcInstant::parse_rfc3339("2026-03-20T15:00:00Z").unwrap();
    let init = daemon_tick(paths, t_night, sample_osc_bytes).unwrap();
    assert!(matches!(
        init,
        DaemonTickOutcome::Updated { generation: 1, .. }
    ));

    // 1. Timed pause (`--for 6h`, until `21:00Z`, spanning Tokyo dawn).
    let paused_state = set_pause(paths, Some(6 * 3_600), t_night).unwrap();
    assert_eq!(
        paused_state.paused_until.as_deref(),
        Some("2026-03-20T21:00:00Z")
    );
    assert_eq!(
        paused_state.pause_status_at(t_night.add_secs(3_600)),
        PauseStatus::Until(UtcInstant::parse_rfc3339("2026-03-20T21:00:00Z").unwrap())
    );

    let before_pause_tick = capture_store_identities(paths);
    let t_dawn = UtcInstant::parse_rfc3339("2026-03-20T20:55:00Z").unwrap();
    let tick_paused = daemon_tick(paths, t_dawn, sample_osc_bytes).unwrap();
    assert_eq!(
        tick_paused,
        DaemonTickOutcome::Paused {
            generation: 1,
            sleep_secs: 300, // 5 minutes remaining until 21:00Z
            until: Some(UtcInstant::parse_rfc3339("2026-03-20T21:00:00Z").unwrap()),
        }
    );
    assert_eq!(
        before_pause_tick,
        capture_store_identities(paths),
        "Paused daemon tick must perform zero disk writes"
    );

    // Hook refresh (both `if_stale = true` and `if_stale = false`) also stays paused.
    assert!(is_hook_store_fresh(paths, t_dawn.as_unix_secs()).unwrap());
    assert_eq!(
        refresh_hook_store(paths, false, None, t_dawn, sample_osc_bytes).unwrap(),
        HookRefreshOutcome::Fresh { generation: 1 }
    );

    // 2. At `21:00Z`, the timed pause expires automatically; daemon applies dawn colours and clears `paused_until`.
    let t_expiry = UtcInstant::parse_rfc3339("2026-03-20T21:00:00Z").unwrap();
    let tick_after_expiry = daemon_tick(paths, t_expiry, sample_osc_bytes).unwrap();
    assert!(matches!(
        tick_after_expiry,
        DaemonTickOutcome::Updated { generation: 2, .. }
    ));
    assert_eq!(load_state(paths).unwrap().paused_until, None);

    // 3. Indefinite pause (`pause` without `--for`) holds until explicit `clear_pause` (`resume`).
    let indef_state = set_pause(paths, None, t_expiry).unwrap();
    assert_eq!(indef_state.paused_until.as_deref(), Some(INDEFINITE_PAUSE));
    assert_eq!(
        indef_state.pause_status_at(t_expiry.add_secs(30 * SECS_PER_DAY)),
        PauseStatus::Indefinite
    );

    let t_noon = UtcInstant::parse_rfc3339("2026-03-21T03:00:00Z").unwrap();
    let indef_tick = daemon_tick(paths, t_noon, sample_osc_bytes).unwrap();
    assert_eq!(
        indef_tick,
        DaemonTickOutcome::Paused {
            generation: 2,
            sleep_secs: DEFAULT_MAX_SLEEP_SECS,
            until: None,
        }
    );

    // `clear_pause` clears `paused_until` and expires `current.gen` freshness so hook & daemon resume immediately.
    let resumed = clear_pause(paths).unwrap();
    assert_eq!(resumed.paused_until, None);
    assert!(!is_hook_store_fresh(paths, t_noon.as_unix_secs()).unwrap());

    let resumed_tick = daemon_tick(paths, t_noon, sample_osc_bytes).unwrap();
    assert!(matches!(
        resumed_tick,
        DaemonTickOutcome::Updated { generation: 3, .. }
    ));
}

#[test]
fn invalid_palette_never_overwrites_last_valid_store_files() {
    let env = TestEnv::new();
    let paths = env.paths();
    set_manual_location(paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

    let t0 = UtcInstant::parse_rfc3339("2026-03-20T21:00:00Z").unwrap();
    let _ = daemon_tick(paths, t0, sample_osc_bytes).unwrap();
    let valid_snapshot = load_snapshot(paths).unwrap().unwrap();
    let before = capture_store_identities(paths);

    // Simulate a user manually editing `config.toml` with a broken palette name.
    let broken_cfg =
        "palette = \"nonexistent-palette\"\n\n[location]\nmode = \"manual\"\nlatitude = 35.7\nlongitude = 139.7\n";
    atomic_write(&paths.config_file(), broken_cfg.as_bytes()).unwrap();

    let err = daemon_tick(paths, t0.add_secs(3_600), sample_osc_bytes)
        .expect_err("broken palette must return error");
    assert!(matches!(err, StoreError::PaletteNotFound { .. }));

    // Last good `current.seq`, `current.gen`, `snapshot.json`, and `state.json` remain untouched.
    assert_eq!(before, capture_store_identities(paths));
    assert_eq!(load_snapshot(paths).unwrap().unwrap(), valid_snapshot);
}

#[test]
fn daemon_pid_guard_prevents_duplicates_and_ignores_unlocked_stale_pid_files() {
    let env = TestEnv::new();
    let paths = env.paths();

    assert_eq!(probe_running_daemon_pid(paths).unwrap(), None);

    let guard = DaemonPidGuard::acquire_with_pid(paths, 42_424).unwrap();
    assert_eq!(guard.pid(), 42_424);
    assert_eq!(probe_running_daemon_pid(paths).unwrap(), Some(42_424));

    let dup_err = DaemonPidGuard::acquire_with_pid(paths, 99_999)
        .expect_err("second daemon lock must be rejected");
    assert!(matches!(
        dup_err,
        StoreError::DaemonAlreadyRunning { pid: Some(42_424) }
    ));

    drop(guard);

    // Even when `daemon.pid` still exists on disk and contains our own live PID, `probe_running_daemon_pid`
    // sees the released `flock` and reports `None` (immune to stale PID files and PID recycling).
    atomic_write(
        &paths.daemon_pid_file(),
        format!("{}\n", std::process::id()).as_bytes(),
    )
    .unwrap();
    assert_eq!(probe_running_daemon_pid(paths).unwrap(), None);
}
