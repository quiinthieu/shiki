//! Pure predicted-step daemon sleep tests (`PLAN.md` §7 & §10 T09).

use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::scheduler::{
    compute_pause_sleep_secs, predict_next_step_sleep_secs, SchedulerConfig, SchedulerError,
    MIN_DAEMON_SLEEP_SECS,
};
use sk_core::solar::Coordinates;
use sk_core::time::{Clock, FakeClock, UtcInstant, SECS_PER_DAY};

#[test]
fn bundled_scheduler_config_matches_model_toml_and_validates_bounds() {
    let model = ModelConfig::bundled();
    assert_eq!(model.scheduler, SchedulerConfig::bundled());
    assert_eq!(model.scheduler.min_sleep_secs, 60);
    assert_eq!(model.scheduler.max_sleep_secs, 1_800);
    assert!((model.scheduler.step_threshold - 0.008).abs() < 1e-12);
    assert_eq!(model.scheduler.probe_step_secs, 300);

    let mut invalid_min = model.scheduler;
    invalid_min.min_sleep_secs = 30;
    assert_eq!(
        invalid_min.validate(),
        Err(SchedulerError::InvalidMinSleepSecs)
    );

    let mut invalid_max = model.scheduler;
    invalid_max.max_sleep_secs = 45;
    assert_eq!(
        invalid_max.validate(),
        Err(SchedulerError::InvalidMaxSleepSecs)
    );

    let mut invalid_step = model.scheduler;
    invalid_step.step_threshold = 0.0;
    assert_eq!(
        invalid_step.validate(),
        Err(SchedulerError::InvalidStepThreshold)
    );

    let mut invalid_probe = model.scheduler;
    invalid_probe.probe_step_secs = 10;
    assert_eq!(
        invalid_probe.validate(),
        Err(SchedulerError::InvalidProbeStepSecs)
    );
}

#[test]
fn predicted_sleep_is_max_sleep_at_deep_night_and_midday_plateau_and_short_in_twilight() {
    let model = ModelConfig::bundled();
    let palette = &Palette::bundled_shiki();
    // Greenwich (51.5°N, 0.0°E) around the summer solstice so midday sun (~61.9°) sits well above
    // `day_full_elevation_deg` (24°) while midnight (-14.9° on 2026-06-21 vs -38° on 2026-03-20):
    // use vernal equinox (2026-03-20) where midnight is -38.5° (< -18°) and noon is +38.5° (> +24°).
    let coords = Coordinates::new(51.5, 0.0).expect("valid Greenwich coords");
    let step = model.scheduler.step_threshold;
    let max_sleep = model.scheduler.max_sleep_secs;

    // 1. Deep night (00:30 UTC, elevation ~ -38°): d = 0.0 across entire 30m window -> max_sleep.
    let midnight = UtcInstant::utc_ymd_hms(2026, 3, 20, 0, 30, 0);
    let midnight_frame = compute_frame(midnight, coords, palette, &model);
    let night_sleep = predict_next_step_sleep_secs(
        midnight,
        &midnight_frame.colors,
        coords,
        palette,
        &model,
        step,
        max_sleep,
    );
    assert_eq!(
        night_sleep, max_sleep,
        "deep night must sleep for the full max_sleep_secs ({max_sleep}s)"
    );

    // 2. Midday plateau (12:00 UTC, elevation ~ +38.5° > 24°): d = 1.0 across entire 30m window -> max_sleep.
    let midday = UtcInstant::utc_ymd_hms(2026, 3, 20, 11, 45, 0);
    let midday_frame = compute_frame(midday, coords, palette, &model);
    let midday_sleep = predict_next_step_sleep_secs(
        midday,
        &midday_frame.colors,
        coords,
        palette,
        &model,
        step,
        max_sleep,
    );
    assert_eq!(
        midday_sleep, max_sleep,
        "midday plateau must sleep for the full max_sleep_secs ({max_sleep}s)"
    );

    // 3. Active morning twilight (05:30 UTC, civil twilight rising toward sunrise ~06:02 UTC):
    // sleep must be much shorter than max_sleep, >= 60s, and land right on an above-threshold step.
    let twilight = UtcInstant::utc_ymd_hms(2026, 3, 20, 5, 30, 0);
    let twilight_frame = compute_frame(twilight, coords, palette, &model);
    let twilight_sleep = predict_next_step_sleep_secs(
        twilight,
        &twilight_frame.colors,
        coords,
        palette,
        &model,
        step,
        max_sleep,
    );
    assert!(
        twilight_sleep >= MIN_DAEMON_SLEEP_SECS,
        "twilight sleep ({twilight_sleep}s) must respect 60s floor"
    );
    assert!(
        twilight_sleep <= 600,
        "active twilight sleep ({twilight_sleep}s) must be short (<= 10m), not {max_sleep}s"
    );

    let woken_frame = compute_frame(
        twilight.add_secs(i64::try_from(twilight_sleep).expect("i64")),
        coords,
        palette,
        &model,
    );
    assert_ne!(woken_frame.colors, twilight_frame.colors);
    assert!(
        twilight_frame.colors.max_delta_e(&woken_frame.colors) >= step,
        "predicted wake instant must immediately satisfy max_delta_e >= step_threshold"
    );
    if twilight_sleep > MIN_DAEMON_SLEEP_SECS {
        let one_sec_earlier = compute_frame(
            twilight.add_secs(i64::try_from(twilight_sleep - 1).expect("i64")),
            coords,
            palette,
            &model,
        );
        let crossed_early = one_sec_earlier.colors != twilight_frame.colors
            && twilight_frame.colors.max_delta_e(&one_sec_earlier.colors) >= step;
        assert!(
            !crossed_early,
            "binary search must find the earliest crossing second"
        );
    }
}

#[test]
fn pre_dawn_window_catches_astronomical_twilight_onset_without_oversleeping() {
    let model = ModelConfig::bundled();
    let palette = &Palette::bundled_shiki();
    let coords = Coordinates::new(1.3, 103.8).expect("valid Singapore coords");
    let astro_dawn = sk_core::solar::next_solar_event(
        UtcInstant::utc_ymd_hms(2026, 3, 20, 20, 0, 0),
        coords,
        &model.event_angles,
    )
    .expect("equatorial astronomical dawn event");
    assert_eq!(
        astro_dawn.kind,
        sk_core::solar::SolarEventKind::AstronomicalDawn
    );

    // Start 5 minutes before astronomical dawn (-18°): currently deep night (`d = 0.0`), while
    // twilight onset and the first perceptible `ΔE_OK >= step_threshold` crossing occur mid-window.
    let pre_dawn = astro_dawn.instant.add_minutes(-5);
    let baseline = compute_frame(pre_dawn, coords, palette, &model);
    assert_eq!(baseline.day_phase, 0.0, "must start in night before -18°");

    let max_sleep = 1_800;
    let sleep_secs = predict_next_step_sleep_secs(
        pre_dawn,
        &baseline.colors,
        coords,
        palette,
        &model,
        model.scheduler.step_threshold,
        max_sleep,
    );
    assert!(
        sleep_secs < max_sleep,
        "must wake when twilight crosses step_threshold inside the 30m window (got {sleep_secs}s)"
    );
    assert!(sleep_secs >= MIN_DAEMON_SLEEP_SECS);
}

#[test]
fn tiny_step_threshold_never_wakes_faster_than_once_per_minute() {
    let model = ModelConfig::bundled();
    let palette = &Palette::bundled_shiki();
    let coords = Coordinates::new(1.3, 103.8).expect("valid equatorial coords");
    // Equatorial sunrise is the steepest elevation derivative on Earth (~0.25 deg/min).
    let steep_dawn = UtcInstant::utc_ymd_hms(2026, 3, 20, 23, 0, 0);
    let baseline = compute_frame(steep_dawn, coords, palette, &model);

    let sleep_secs = predict_next_step_sleep_secs(
        steep_dawn,
        &baseline.colors,
        coords,
        palette,
        &model,
        0.000_01,
        1_800,
    );
    assert_eq!(
        sleep_secs, MIN_DAEMON_SLEEP_SECS,
        "PLAN §7 forbids waking more than once per minute even for tiny step_threshold"
    );
}

#[test]
fn fake_clock_full_day_wakes_respect_plan_section_7_across_latitudes_and_polar_states() {
    let model = ModelConfig::bundled();
    let palette = &Palette::bundled_shiki();
    let step = model.scheduler.step_threshold;
    let max_sleep = model.scheduler.max_sleep_secs;

    let cases = [
        (
            "Tokyo equinox",
            Coordinates::new(35.7, 139.7).unwrap(),
            UtcInstant::utc_ymd_hms(2026, 3, 20, 0, 0, 0),
            true,
        ),
        (
            "Sydney winter solstice",
            Coordinates::new(-33.9, 151.2).unwrap(),
            UtcInstant::utc_ymd_hms(2026, 6, 21, 0, 0, 0),
            true,
        ),
        (
            "Tromso polar day",
            Coordinates::new(69.6, 18.9).unwrap(),
            UtcInstant::utc_ymd_hms(2026, 6, 21, 0, 0, 0),
            false,
        ),
        (
            "McMurdo polar night",
            Coordinates::new(-77.8, 166.7).unwrap(),
            UtcInstant::utc_ymd_hms(2026, 6, 21, 0, 0, 0),
            false,
        ),
    ];

    for (label, coords, start, has_diurnal_twilight) in cases {
        let clock = FakeClock::new(start);
        let end = start.add_secs(SECS_PER_DAY);

        let mut baseline = compute_frame(clock.now(), coords, palette, &model).colors;
        let mut sleeps = Vec::new();
        let mut updates = 0_usize;

        while clock.now() < end {
            let now = clock.now();
            let frame = compute_frame(now, coords, palette, &model);
            if frame.colors != baseline && baseline.max_delta_e(&frame.colors) >= step {
                baseline = frame.colors;
                updates += 1;
            }
            let sleep_secs = predict_next_step_sleep_secs(
                now, &baseline, coords, palette, &model, step, max_sleep,
            );
            assert!(
                sleep_secs >= MIN_DAEMON_SLEEP_SECS,
                "{label}: wake interval {sleep_secs}s violated 60s minimum"
            );
            assert!(
                sleep_secs <= max_sleep,
                "{label}: wake interval {sleep_secs}s exceeded max_sleep {max_sleep}s"
            );
            sleeps.push(sleep_secs);
            clock.advance_secs(i64::try_from(sleep_secs).expect("i64"));
        }

        // No polling: a 24h day has 1,440 minutes; predicted-step waking uses far fewer wakes.
        assert!(
            sleeps.len() <= 300,
            "{label}: expected non-polling wake count (<= 300/day), got {}",
            sleeps.len()
        );
        assert!(
            sleeps.contains(&max_sleep),
            "{label}: expected max_sleep ({max_sleep}s) stretches during flat solar periods"
        );

        if has_diurnal_twilight {
            assert!(
                updates >= 10,
                "{label}: expected smooth multi-step twilight transitions, got {updates}"
            );
            assert!(
                sleeps.iter().any(|&s| s < 600),
                "{label}: expected short twilight sleeps (< 600s), got min {:?}",
                sleeps.iter().min()
            );
        }
    }
}

#[test]
fn compute_pause_sleep_clamps_to_min_and_max_sleep_bounds() {
    let model = ModelConfig::bundled();
    let now = UtcInstant::utc_ymd_hms(2026, 10, 31, 12, 0, 0);

    // Indefinite pause sleeps for max_sleep_secs (wakes early on SIGHUP when resumed).
    assert_eq!(compute_pause_sleep_secs(now, None, &model, 1_800), 1_800);

    // Pause ending in 5 minutes sleeps right until expiry (300s).
    assert_eq!(
        compute_pause_sleep_secs(now, Some(now.add_secs(300)), &model, 1_800),
        300
    );

    // Pause ending in 20 seconds still respects the 60s §7 floor.
    assert_eq!(
        compute_pause_sleep_secs(now, Some(now.add_secs(20)), &model, 1_800),
        60
    );

    // Pause ending in 2 hours is capped at max_sleep_secs (1800s).
    assert_eq!(
        compute_pause_sleep_secs(now, Some(now.add_secs(7_200)), &model, 1_800),
        1_800
    );
}
