use sk_core::time::{
    Clock, FakeClock, UtcDateTime, UtcInstant, SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE,
};

#[test]
fn utc_instant_epoch_leap_years_and_rfc3339_offsets() {
    assert_eq!(SECS_PER_MINUTE, 60);
    assert_eq!(SECS_PER_HOUR, 3_600);
    assert_eq!(SECS_PER_DAY, 86_400);

    let epoch = UtcInstant::utc_ymd_hms(1970, 1, 1, 0, 0, 0);
    assert_eq!(epoch.as_unix_secs(), 0);
    assert_eq!(epoch.format_rfc3339_utc(), "1970-01-01T00:00:00Z");

    let pre_epoch = UtcInstant::from_unix_secs(-1);
    assert_eq!(
        pre_epoch.to_utc_date_time(),
        UtcDateTime {
            year: 1969,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
        }
    );
    assert_eq!(pre_epoch.format_rfc3339_utc(), "1969-12-31T23:59:59Z");

    // Leap-year rules: divisible by 4, except centuries not divisible by 400.
    assert!(UtcInstant::from_utc_ymd_hms(2000, 2, 29, 12, 0, 0).is_ok());
    assert!(UtcInstant::from_utc_ymd_hms(2024, 2, 29, 23, 59, 59).is_ok());
    assert!(UtcInstant::from_utc_ymd_hms(1900, 2, 29, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 2, 29, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2100, 2, 29, 0, 0, 0).is_err());

    // Invalid calendar components.
    assert!(UtcInstant::from_utc_ymd_hms(2026, 0, 1, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 13, 1, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 4, 0, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 4, 31, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 1, 1, 24, 0, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 1, 1, 0, 60, 0).is_err());
    assert!(UtcInstant::from_utc_ymd_hms(2026, 1, 1, 0, 0, 60).is_err());

    // RFC 3339 parsing with Z and signed UTC offsets.
    let saigon_noon = UtcInstant::parse_rfc3339("2026-10-31T12:00:00+07:00").expect("parse +07:00");
    let utc_05 = UtcInstant::parse_rfc3339("2026-10-31T05:00:00Z").expect("parse Z");
    assert_eq!(saigon_noon, utc_05);

    let ny_morning = UtcInstant::parse_rfc3339("2026-06-21T08:30:15-04:00").expect("parse -04:00");
    assert_eq!(ny_morning.format_rfc3339_utc(), "2026-06-21T12:30:15Z");

    assert!(UtcInstant::parse_rfc3339("not-a-timestamp").is_err());
    assert!(UtcInstant::parse_rfc3339("2026-10-31T12:00:00+25:00").is_err());
    assert!(UtcInstant::parse_rfc3339("2026-10-31T12:00:00+07:60").is_err());

    // Serde JSON round-trip.
    let json = serde_json::to_string(&utc_05).expect("serialize UtcInstant");
    let decoded: UtcInstant = serde_json::from_str(&json).expect("deserialize UtcInstant");
    assert_eq!(decoded, utc_05);
}

#[test]
fn fake_clock_advances_rewinds_and_injects_via_trait() {
    let clock = FakeClock::from_utc_ymd_hms(2026, 3, 20, 6, 0, 0);

    fn read_via_trait(clock: &dyn Clock) -> UtcInstant {
        clock.now()
    }

    assert_eq!(
        read_via_trait(&clock).format_rfc3339_utc(),
        "2026-03-20T06:00:00Z"
    );
    assert_eq!(
        clock.advance_secs(90).format_rfc3339_utc(),
        "2026-03-20T06:01:30Z"
    );
    assert_eq!(
        read_via_trait(&&clock).format_rfc3339_utc(),
        "2026-03-20T06:01:30Z"
    );
    assert_eq!(
        clock.advance_minutes(10).format_rfc3339_utc(),
        "2026-03-20T06:11:30Z"
    );
    assert_eq!(
        clock.advance_secs(-30).format_rfc3339_utc(),
        "2026-03-20T06:11:00Z"
    );

    let reset = UtcInstant::utc_ymd_hms(2026, 12, 21, 18, 0, 0);
    clock.set(reset);
    assert_eq!(clock.now(), reset);
    assert_eq!(
        reset.duration_since(UtcInstant::utc_ymd_hms(2026, 12, 21, 17, 0, 0)),
        3_600
    );
}

fn sample_seasonal_clock_trace() -> String {
    let anchors = [
        (
            "vernal_equinox",
            UtcInstant::utc_ymd_hms(2026, 3, 20, 5, 30, 0),
        ),
        (
            "summer_solstice",
            UtcInstant::utc_ymd_hms(2026, 6, 21, 12, 0, 0),
        ),
        (
            "autumn_equinox",
            UtcInstant::utc_ymd_hms(2026, 9, 22, 18, 15, 0),
        ),
        (
            "winter_solstice",
            UtcInstant::utc_ymd_hms(2026, 12, 21, 0, 0, 0),
        ),
    ];

    let mut lines = Vec::new();
    for (label, start) in anchors {
        let clock = FakeClock::new(start);
        let t0 = clock.now();
        let t1 = clock.advance_minutes(45);
        let t2 = clock.advance_secs(3_600);
        lines.push(format!(
            "{label}: t0={t0} ({}) -> t1={t1} -> t2={t2} (+{}s)",
            t0.as_unix_secs(),
            t2.duration_since(t0)
        ));
    }
    lines.join("\n")
}

#[test]
fn fixed_instants_and_fake_clock_pass_100x_in_a_loop() {
    let baseline = sample_seasonal_clock_trace();
    for iteration in 0..100 {
        let current = sample_seasonal_clock_trace();
        assert_eq!(current, baseline, "mismatch at loop iteration {iteration}");
    }
}

#[test]
fn fixed_instants_and_fake_clock_match_golden_snapshot() {
    let trace = sample_seasonal_clock_trace();
    insta::assert_snapshot!("fixed_instants_and_fake_clock_trace", trace);
}
