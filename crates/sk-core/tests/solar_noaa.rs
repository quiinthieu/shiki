use proptest::prelude::*;
use serde::Deserialize;
use sk_core::solar::{
    solar_day_for_date, solar_position, solar_snapshot, AltitudeCrossing, Coordinates, PolarState,
    SolarError, SolarEventAngles, SolarEventKind, DEFAULT_MODEL_TOML,
};
use sk_core::time::UtcInstant;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct NoaaFixture {
    capture_url: String,
    details_url: String,
    capture_date: String,
    city: String,
    hemisphere_ns: String,
    region: String,
    latitude_deg: f64,
    longitude_deg: f64,
    standard_tz_offset_hours: i32,
    tolerances: NoaaTolerances,
    days: Vec<NoaaDayFixture>,
    samples: Vec<NoaaSampleFixture>,
}

#[derive(Debug, Deserialize)]
struct NoaaTolerances {
    elevation_deg: f64,
    azimuth_deg: f64,
    declination_deg: f64,
    equation_of_time_min: f64,
    solar_noon_secs: i64,
    sunrise_sunset_secs: i64,
}

#[derive(Debug, Deserialize)]
struct NoaaDayFixture {
    date: String,
    season_anchor: String,
    polar_state: PolarState,
    solar_noon_utc: String,
    noaa_noonbox_local: String,
    noon_elevation_deg: f64,
    sunrise_utc: Option<String>,
    noaa_risebox_local: Option<String>,
    sunset_utc: Option<String>,
    noaa_setbox_local: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NoaaSampleFixture {
    instant_utc: String,
    noaa_elbox: String,
    apparent_elevation_deg: f64,
    azimuth_deg: f64,
    declination_deg: f64,
    equation_of_time_min: f64,
}

fn solar_fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("tests")
        .join("fixtures")
        .join("solar")
}

fn load_noaa_fixtures() -> Vec<(String, String, NoaaFixture)> {
    let dir = solar_fixtures_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read tests/fixtures/solar")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("noaa_")
                    && Path::new(n)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            })
        })
        .collect();
    entries.sort();

    entries
        .into_iter()
        .map(|path| {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("utf-8 filename")
                .to_string();
            let raw = fs::read_to_string(&path).expect("read NOAA fixture TOML");
            let parsed: NoaaFixture =
                toml::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {file_name}: {e}"));
            (file_name, raw, parsed)
        })
        .collect()
}

fn parse_ymd(date: &str) -> (i32, u8, u8) {
    let parts: Vec<&str> = date.split('-').collect();
    assert_eq!(parts.len(), 3, "expected YYYY-MM-DD, got {date}");
    (
        parts[0].parse().expect("year"),
        parts[1].parse().expect("month"),
        parts[2].parse().expect("day"),
    )
}

fn angular_diff_deg(a: f64, b: f64) -> f64 {
    let diff = (a - b).rem_euclid(360.0);
    if diff > 180.0 {
        360.0 - diff
    } else {
        diff
    }
}

#[test]
fn model_toml_event_angles_parse_and_validate() {
    let parsed =
        SolarEventAngles::from_model_toml(DEFAULT_MODEL_TOML).expect("parse data/model.toml");
    assert_eq!(parsed, SolarEventAngles::default());
    parsed.validate().expect("default angles are valid");

    let custom_toml = r#"
        [solar.event_angles]
        astronomical_twilight_deg = -19.0
        nautical_twilight_deg = -12.5
        civil_twilight_deg = -5.5
        sunrise_sunset_deg = -0.833
        golden_hour_deg = 7.5
    "#;
    let custom = SolarEventAngles::from_model_toml(custom_toml).expect("parse custom model TOML");
    assert_eq!(custom.golden_hour_deg, 7.5);

    let coords = Coordinates::new(51.4769, 0.0).expect("Greenwich");
    let default_day =
        solar_day_for_date(2026, 3, 20, coords, &SolarEventAngles::default()).expect("default day");
    let custom_day = solar_day_for_date(2026, 3, 20, coords, &custom).expect("custom day");
    assert!(
        custom_day
            .events
            .golden_hour_end
            .instant()
            .expect("custom golden_hour_end")
            > default_day
                .events
                .golden_hour_end
                .instant()
                .expect("default golden_hour_end"),
        "raising golden_hour_deg from 6.0 to 7.5 must delay morning golden_hour_end"
    );

    // Non-monotonic event angles must be rejected.
    let inverted_toml = r#"
        [solar.event_angles]
        astronomical_twilight_deg = -18.0
        nautical_twilight_deg = -12.0
        civil_twilight_deg = -0.5
        sunrise_sunset_deg = -0.833
        golden_hour_deg = 6.0
    "#;
    assert!(matches!(
        SolarEventAngles::from_model_toml(inverted_toml),
        Err(SolarError::InvalidEventAngles(_))
    ));

    let nan_angles = SolarEventAngles {
        golden_hour_deg: f64::NAN,
        ..SolarEventAngles::default()
    };
    assert!(matches!(
        nan_angles.validate(),
        Err(SolarError::InvalidEventAngles(_))
    ));
}

#[test]
fn noaa_fixtures_headers_and_city_coverage_meet_plan_spec() {
    let fixtures = load_noaa_fixtures();
    assert!(
        fixtures.len() >= 6,
        "PLAN §9 requires >= 6 public city fixtures, found {}",
        fixtures.len()
    );

    let mut hemispheres = HashSet::new();
    let mut regions = HashSet::new();
    let expected_seasons: HashSet<&str> = [
        "march_equinox",
        "june_solstice",
        "september_equinox",
        "december_solstice",
    ]
    .into_iter()
    .collect();

    for (file_name, raw, fixture) in &fixtures {
        let header: String = raw
            .lines()
            .take_while(|line| line.starts_with('#') || line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            header.contains("Capture URL: https://gml.noaa.gov/grad/solcalc/"),
            "{file_name} header must record NOAA capture URL"
        );
        assert!(
            header.contains("Capture date: 2026-10-31"),
            "{file_name} header must record capture date"
        );
        assert_eq!(fixture.capture_url, "https://gml.noaa.gov/grad/solcalc/");
        assert_eq!(
            fixture.details_url,
            "https://gml.noaa.gov/grad/solcalc/calcdetails.html"
        );
        assert_eq!(fixture.capture_date, "2026-10-31");
        assert!((-12..=14).contains(&fixture.standard_tz_offset_hours));
        assert!(!fixture.city.is_empty());

        hemispheres.insert(fixture.hemisphere_ns.as_str());
        regions.insert(fixture.region.as_str());

        let seasons: HashSet<&str> = fixture
            .days
            .iter()
            .map(|d| d.season_anchor.as_str())
            .collect();
        assert_eq!(
            seasons, expected_seasons,
            "{file_name} must cover both solstices and both equinoxes"
        );
        assert!(
            fixture.samples.len() >= 16,
            "{file_name} must include instantaneous samples across all 4 seasonal dates"
        );
    }

    assert!(
        hemispheres.contains("north") && hemispheres.contains("south"),
        "NOAA fixtures must cover both hemispheres, got {hemispheres:?}"
    );
    assert!(
        regions.contains("equatorial"),
        "NOAA fixtures must include at least one equatorial city, got {regions:?}"
    );
    assert!(
        regions.contains("polar"),
        "NOAA fixtures must include at least one polar city, got {regions:?}"
    );
}

#[test]
fn noaa_fixtures_position_samples_match_within_documented_tolerance() {
    let fixtures = load_noaa_fixtures();

    for (file_name, _, fixture) in &fixtures {
        let coords =
            Coordinates::new(fixture.latitude_deg, fixture.longitude_deg).expect("valid coords");
        let tol = &fixture.tolerances;

        for sample in &fixture.samples {
            let instant =
                UtcInstant::parse_rfc3339(&sample.instant_utc).expect("valid sample RFC 3339");
            let pos = solar_position(instant, coords);

            let elev_err = (pos.elevation_deg - sample.apparent_elevation_deg).abs();
            assert!(
                elev_err <= tol.elevation_deg,
                "{file_name} @ {}: elevation {:.6} vs NOAA {:.6} (err {:.6} > tol {:.6})",
                sample.instant_utc,
                pos.elevation_deg,
                sample.apparent_elevation_deg,
                elev_err,
                tol.elevation_deg
            );

            let az_err = angular_diff_deg(pos.azimuth_deg, sample.azimuth_deg);
            assert!(
                az_err <= tol.azimuth_deg,
                "{file_name} @ {}: azimuth {:.6} vs NOAA {:.6} (err {:.6} > tol {:.6})",
                sample.instant_utc,
                pos.azimuth_deg,
                sample.azimuth_deg,
                az_err,
                tol.azimuth_deg
            );

            let dec_err = (pos.declination_deg - sample.declination_deg).abs();
            assert!(
                dec_err <= tol.declination_deg,
                "{file_name} @ {}: declination {:.6} vs NOAA {:.6} (err {:.6} > tol {:.6})",
                sample.instant_utc,
                pos.declination_deg,
                sample.declination_deg,
                dec_err,
                tol.declination_deg
            );

            let eqt_err = (pos.equation_of_time_min - sample.equation_of_time_min).abs();
            assert!(
                eqt_err <= tol.equation_of_time_min,
                "{file_name} @ {}: eq_time {:.6} vs NOAA {:.6} (err {:.6} > tol {:.6})",
                sample.instant_utc,
                pos.equation_of_time_min,
                sample.equation_of_time_min,
                eqt_err,
                tol.equation_of_time_min
            );

            // Verify consistency with NOAA UI's `#elbox` ("dark" below -18 deg apparent elevation).
            if sample.noaa_elbox == "dark" {
                assert!(
                    pos.elevation_deg < -18.0 + tol.elevation_deg,
                    "{file_name} @ {}: expected dark (< -18 deg), got {:.6}",
                    sample.instant_utc,
                    pos.elevation_deg
                );
            } else {
                let ui_el: f64 = sample.noaa_elbox.parse().expect("numeric NOAA elbox");
                assert!(
                    (pos.elevation_deg - ui_el).abs() <= tol.elevation_deg,
                    "{file_name} @ {}: elevation {:.4} vs NOAA UI elbox {:.2}",
                    sample.instant_utc,
                    pos.elevation_deg,
                    ui_el
                );
            }
        }
    }
}

#[test]
fn noaa_fixtures_daily_events_and_polar_states_match_within_documented_tolerance() {
    let fixtures = load_noaa_fixtures();
    let angles = SolarEventAngles::default();

    for (file_name, _, fixture) in &fixtures {
        let coords =
            Coordinates::new(fixture.latitude_deg, fixture.longitude_deg).expect("valid coords");
        let tol = &fixture.tolerances;

        for day_fix in &fixture.days {
            let (year, month, day) = parse_ymd(&day_fix.date);
            let solar_day =
                solar_day_for_date(year, month, day, coords, &angles).expect("valid date");

            assert_eq!(
                solar_day.polar_state, day_fix.polar_state,
                "{file_name} {}: polar_state {:?} != expected {:?}",
                day_fix.date, solar_day.polar_state, day_fix.polar_state
            );
            assert!(
                !day_fix.noaa_noonbox_local.is_empty(),
                "{file_name} {} must record NOAA local noonbox string",
                day_fix.date
            );

            let expected_noon =
                UtcInstant::parse_rfc3339(&day_fix.solar_noon_utc).expect("expected noon UTC");
            let noon_diff_secs = solar_day
                .events
                .solar_noon
                .duration_since(expected_noon)
                .abs();
            assert!(
                noon_diff_secs <= tol.solar_noon_secs,
                "{file_name} {}: solar_noon {} vs NOAA {} (diff {}s > tol {}s)",
                day_fix.date,
                solar_day.events.solar_noon,
                expected_noon,
                noon_diff_secs,
                tol.solar_noon_secs
            );

            let noon_elev_err = (solar_day.noon_elevation_deg - day_fix.noon_elevation_deg).abs();
            assert!(
                noon_elev_err <= tol.elevation_deg,
                "{file_name} {}: noon_elevation {:.6} vs NOAA {:.6} (err {:.6} > tol {:.6})",
                day_fix.date,
                solar_day.noon_elevation_deg,
                day_fix.noon_elevation_deg,
                noon_elev_err,
                tol.elevation_deg
            );

            match day_fix.polar_state {
                PolarState::Normal => {
                    assert!(day_fix.noaa_risebox_local.is_some());
                    assert!(day_fix.noaa_setbox_local.is_some());

                    let expected_rise = UtcInstant::parse_rfc3339(
                        day_fix.sunrise_utc.as_deref().expect("sunrise_utc"),
                    )
                    .expect("parse sunrise_utc");
                    let actual_rise =
                        solar_day.events.sunrise.instant().unwrap_or_else(|| {
                            panic!("{file_name} {}: missing sunrise", day_fix.date)
                        });
                    let rise_diff = actual_rise.duration_since(expected_rise).abs();
                    assert!(
                        rise_diff <= tol.sunrise_sunset_secs,
                        "{file_name} {}: sunrise {} vs NOAA {} (diff {}s > tol {}s)",
                        day_fix.date,
                        actual_rise,
                        expected_rise,
                        rise_diff,
                        tol.sunrise_sunset_secs
                    );

                    let expected_set = UtcInstant::parse_rfc3339(
                        day_fix.sunset_utc.as_deref().expect("sunset_utc"),
                    )
                    .expect("parse sunset_utc");
                    let actual_set =
                        solar_day.events.sunset.instant().unwrap_or_else(|| {
                            panic!("{file_name} {}: missing sunset", day_fix.date)
                        });
                    let set_diff = actual_set.duration_since(expected_set).abs();
                    assert!(
                        set_diff <= tol.sunrise_sunset_secs,
                        "{file_name} {}: sunset {} vs NOAA {} (diff {}s > tol {}s)",
                        day_fix.date,
                        actual_set,
                        expected_set,
                        set_diff,
                        tol.sunrise_sunset_secs
                    );
                }
                PolarState::PolarDay => {
                    assert_eq!(solar_day.events.sunrise, AltitudeCrossing::AlwaysAbove);
                    assert_eq!(solar_day.events.sunset, AltitudeCrossing::AlwaysAbove);
                }
                PolarState::PolarNight => {
                    assert_eq!(solar_day.events.sunrise, AltitudeCrossing::AlwaysBelow);
                    assert_eq!(solar_day.events.sunset, AltitudeCrossing::AlwaysBelow);
                }
            }
        }
    }
}

#[test]
fn polar_and_high_latitude_twilight_regimes_behave_accurately() {
    let angles = SolarEventAngles::default();

    // 1. Tromso, Norway (69.6492 N, 18.9553 E) on June solstice:
    //    Midnight sun (sun stays above -0.833 deg all day, nadir ~ +5.1 deg),
    //    so it dips below +6 deg golden hour around midnight while all twilights stay AlwaysAbove.
    let tromso = Coordinates::new(69.6492, 18.9553).expect("Tromso");
    let tromso_summer = solar_day_for_date(2026, 6, 21, tromso, &angles).expect("Tromso Jun 21");
    assert_eq!(tromso_summer.polar_state, PolarState::PolarDay);
    assert_eq!(tromso_summer.events.sunrise, AltitudeCrossing::AlwaysAbove);
    assert_eq!(tromso_summer.events.sunset, AltitudeCrossing::AlwaysAbove);
    assert_eq!(
        tromso_summer.events.civil_dawn,
        AltitudeCrossing::AlwaysAbove
    );
    assert_eq!(
        tromso_summer.events.astronomical_dawn,
        AltitudeCrossing::AlwaysAbove
    );
    assert!(
        tromso_summer.events.golden_hour_end.instant().is_some()
            && tromso_summer.events.golden_hour_start.instant().is_some(),
        "Tromso midnight sun dips to ~5.1 deg at midnight, crossing +6 deg golden hour"
    );

    // 2. Tromso on December solstice:
    //    Polar night (noon ~ -2.98 deg), so sunrise/sunset & golden hour are AlwaysBelow,
    //    but civil, nautical, and astronomical twilight still occur around midday!
    let tromso_winter = solar_day_for_date(2026, 12, 21, tromso, &angles).expect("Tromso Dec 21");
    assert_eq!(tromso_winter.polar_state, PolarState::PolarNight);
    assert_eq!(tromso_winter.events.sunrise, AltitudeCrossing::AlwaysBelow);
    assert_eq!(tromso_winter.events.sunset, AltitudeCrossing::AlwaysBelow);
    assert_eq!(
        tromso_winter.events.golden_hour_end,
        AltitudeCrossing::AlwaysBelow
    );
    assert!(tromso_winter.events.civil_dawn.instant().is_some());
    assert!(tromso_winter.events.civil_dusk.instant().is_some());
    assert!(tromso_winter.events.nautical_dawn.instant().is_some());
    assert!(tromso_winter.events.astronomical_dawn.instant().is_some());

    // 3. McMurdo Station (-77.8419 S, 166.6863 E) on December solstice:
    //    Deep Antarctic summer: nadir is ~ +11.3 deg, so even golden hour (+6 deg) is AlwaysAbove!
    let mcmurdo = Coordinates::new(-77.8419, 166.6863).expect("McMurdo");
    let mcmurdo_summer =
        solar_day_for_date(2026, 12, 21, mcmurdo, &angles).expect("McMurdo Dec 21");
    assert_eq!(mcmurdo_summer.polar_state, PolarState::PolarDay);
    assert_eq!(
        mcmurdo_summer.events.golden_hour_end,
        AltitudeCrossing::AlwaysAbove
    );
    assert_eq!(
        mcmurdo_summer.events.golden_hour_start,
        AltitudeCrossing::AlwaysAbove
    );

    // 4. McMurdo Station on June solstice:
    //    Antarctic winter (noon ~ -11.25 deg): civil twilight (-6 deg) is AlwaysBelow,
    //    while nautical (-12 deg) and astronomical (-18 deg) twilight still cross.
    let mcmurdo_winter = solar_day_for_date(2026, 6, 21, mcmurdo, &angles).expect("McMurdo Jun 21");
    assert_eq!(mcmurdo_winter.polar_state, PolarState::PolarNight);
    assert_eq!(
        mcmurdo_winter.events.civil_dawn,
        AltitudeCrossing::AlwaysBelow
    );
    assert_eq!(
        mcmurdo_winter.events.civil_dusk,
        AltitudeCrossing::AlwaysBelow
    );
    assert!(mcmurdo_winter.events.nautical_dawn.instant().is_some());
    assert!(mcmurdo_winter.events.astronomical_dawn.instant().is_some());

    // 5. Geographic North Pole (+90.0 N, 0.0 E) and South Pole (-90.0 S, 0.0 E):
    //    Zero division/NaN singularities; 24h polar day vs deep 24h polar night (below -18 deg).
    let north_pole = Coordinates::new(90.0, 0.0).expect("North Pole");
    let np_summer = solar_day_for_date(2026, 6, 21, north_pole, &angles).expect("NP Jun 21");
    assert_eq!(np_summer.polar_state, PolarState::PolarDay);
    assert!((np_summer.noon_elevation_deg - 23.45).abs() < 0.1);
    assert_eq!(
        np_summer.events.golden_hour_end,
        AltitudeCrossing::AlwaysAbove
    );

    let np_winter = solar_day_for_date(2026, 12, 21, north_pole, &angles).expect("NP Dec 21");
    assert_eq!(np_winter.polar_state, PolarState::PolarNight);
    assert!(np_winter.noon_elevation_deg < -23.0);
    assert_eq!(
        np_winter.events.astronomical_dawn,
        AltitudeCrossing::AlwaysBelow
    );
    assert_eq!(
        np_winter.events.astronomical_dusk,
        AltitudeCrossing::AlwaysBelow
    );

    let south_pole = Coordinates::new(-90.0, 0.0).expect("South Pole");
    let sp_summer = solar_day_for_date(2026, 12, 21, south_pole, &angles).expect("SP Dec 21");
    assert_eq!(sp_summer.polar_state, PolarState::PolarDay);

    // 6. Greenwich (51.4769 N) on June solstice:
    //    Normal sunrise/sunset, civil and nautical twilight cross, but astronomical twilight
    //    never ends ("continuous astronomical twilight" above ~48.6 deg latitude in midsummer).
    let greenwich = Coordinates::new(51.4769, 0.0).expect("Greenwich");
    let gw_summer = solar_day_for_date(2026, 6, 21, greenwich, &angles).expect("Greenwich Jun 21");
    assert_eq!(gw_summer.polar_state, PolarState::Normal);
    assert!(gw_summer.events.sunrise.instant().is_some());
    assert!(gw_summer.events.nautical_dawn.instant().is_some());
    assert_eq!(
        gw_summer.events.astronomical_dawn,
        AltitudeCrossing::AlwaysAbove
    );
    assert_eq!(
        gw_summer.events.astronomical_dusk,
        AltitudeCrossing::AlwaysAbove
    );
}

#[test]
fn is_morning_and_local_solar_day_remain_continuous_across_utc_midnight() {
    let angles = SolarEventAngles::default();

    // New York (40.7128 N, -74.0060 W): June 21 sunset is ~2026-06-22T00:31Z (after UTC midnight).
    // Querying at 2026-06-22T00:30:00Z must stay on New York's 2026-06-21 local solar day
    // with `is_morning == false` (evening dusk, not flipping to the next day!).
    let nyc = Coordinates::new(40.7128, -74.0060).expect("NYC");
    let nyc_evening = UtcInstant::utc_ymd_hms(2026, 6, 22, 0, 30, 0);
    let nyc_snap = solar_snapshot(nyc_evening, nyc, &angles);
    let nyc_june21 = solar_day_for_date(2026, 6, 21, nyc, &angles).expect("NYC Jun 21");
    assert!(!nyc_snap.is_morning, "NYC 20:30 EDT is evening");
    assert_eq!(nyc_snap.events.solar_noon, nyc_june21.events.solar_noon);
    assert_eq!(nyc_snap.events.sunset, nyc_june21.events.sunset);

    // Tokyo (35.6762 N, 139.6503 E): June 21 sunrise is ~2026-06-20T19:25Z (before UTC midnight).
    // Querying at 2026-06-20T19:30:00Z must already be on Tokyo's 2026-06-21 local solar day
    // with `is_morning == true`.
    let tokyo = Coordinates::new(35.6762, 139.6503).expect("Tokyo");
    let tokyo_dawn = UtcInstant::utc_ymd_hms(2026, 6, 20, 19, 30, 0);
    let tokyo_snap = solar_snapshot(tokyo_dawn, tokyo, &angles);
    let tokyo_june21 = solar_day_for_date(2026, 6, 21, tokyo, &angles).expect("Tokyo Jun 21");
    assert!(tokyo_snap.is_morning, "Tokyo 04:30 JST is morning");
    assert_eq!(tokyo_snap.events.solar_noon, tokyo_june21.events.solar_noon);
    assert_eq!(tokyo_snap.events.sunrise, tokyo_june21.events.sunrise);

    // `next_event_after` advances cleanly through the day's concrete milestones.
    let (next_kind, next_at) = tokyo_snap
        .events
        .next_event_after(tokyo_dawn)
        .expect("next event after sunrise");
    assert_eq!(next_kind, SolarEventKind::GoldenHourEnd);
    assert!(next_at > tokyo_dawn);
}

#[test]
fn coordinates_validation_and_privacy_redaction_never_leak_values() {
    let valid = Coordinates::new(35.6762, 139.6503).expect("valid coordinates");
    let debug_repr = format!("{valid:?}");
    assert!(
        !debug_repr.contains("35.6762") && !debug_repr.contains("139.6503"),
        "Coordinates Debug output must redact coordinates (`PLAN.md` §0 #4), got: {debug_repr}"
    );
    assert!(debug_repr.contains("<redacted>"));

    // Negative zero normalizes to +0.0.
    let zero = Coordinates::new(-0.0, -0.0).expect("zero coordinates");
    assert!(zero.latitude_deg().is_sign_positive());
    assert!(zero.longitude_deg().is_sign_positive());

    // Out-of-range and non-finite inputs fail without echoing the offending numbers.
    for bad_lat in [-90.0001, 90.0001, 123.456, f64::NAN, f64::INFINITY] {
        let err = Coordinates::new(bad_lat, 12.345).expect_err("invalid latitude");
        assert_eq!(err, SolarError::LatitudeOutOfRange);
        let msg = format!("{err} | {err:?}");
        assert!(
            !msg.contains("123.456") && !msg.contains("90.0001") && !msg.contains("12.345"),
            "error output leaked coordinate numbers: {msg}"
        );
    }

    for bad_lon in [-180.0001, 180.0001, 234.567, f64::NAN, f64::NEG_INFINITY] {
        let err = Coordinates::new(45.678, bad_lon).expect_err("invalid longitude");
        assert_eq!(err, SolarError::LongitudeOutOfRange);
        let msg = format!("{err} | {err:?}");
        assert!(
            !msg.contains("234.567") && !msg.contains("180.0001") && !msg.contains("45.678"),
            "error output leaked coordinate numbers: {msg}"
        );
    }

    // Invalid calendar date bubbles up cleanly.
    assert!(matches!(
        solar_day_for_date(2026, 2, 29, valid, &SolarEventAngles::default()),
        Err(SolarError::InvalidDate(_))
    ));
}

fn render_golden_solar_summary() -> String {
    let angles = SolarEventAngles::default();
    let cases = [
        ("greenwich_equinox", 51.4769, 0.0, (2026, 3, 20)),
        ("singapore_equinox", 1.3521, 103.8198, (2026, 3, 20)),
        ("sydney_summer", -33.8688, 151.2093, (2026, 12, 21)),
        ("tromso_midnight_sun", 69.6492, 18.9553, (2026, 6, 21)),
        ("tromso_polar_night", 69.6492, 18.9553, (2026, 12, 21)),
        ("mcmurdo_polar_night", -77.8419, 166.6863, (2026, 6, 21)),
    ];

    let fmt_crossing = |c: AltitudeCrossing| match c {
        AltitudeCrossing::At(t) => t.format_rfc3339_utc(),
        AltitudeCrossing::AlwaysAbove => "always_above".to_string(),
        AltitudeCrossing::AlwaysBelow => "always_below".to_string(),
    };

    let mut out = Vec::new();
    for (label, lat, lon, (y, m, d)) in cases {
        let coords = Coordinates::new(lat, lon).expect("case coords");
        let day = solar_day_for_date(y, m, d, coords, &angles).expect("case day");
        let e = day.events;
        out.push(format!(
            concat!(
                "[{}]\n",
                "polar_state = {:?}\n",
                "noon_elevation_deg = {:.4}\n",
                "nadir_elevation_deg = {:.4}\n",
                "solar_nadir = {}\n",
                "astronomical_dawn = {}\n",
                "nautical_dawn = {}\n",
                "civil_dawn = {}\n",
                "sunrise = {}\n",
                "golden_hour_end = {}\n",
                "solar_noon = {}\n",
                "golden_hour_start = {}\n",
                "sunset = {}\n",
                "civil_dusk = {}\n",
                "nautical_dusk = {}\n",
                "astronomical_dusk = {}"
            ),
            label,
            day.polar_state,
            day.noon_elevation_deg,
            day.nadir_elevation_deg,
            e.solar_nadir,
            fmt_crossing(e.astronomical_dawn),
            fmt_crossing(e.nautical_dawn),
            fmt_crossing(e.civil_dawn),
            fmt_crossing(e.sunrise),
            fmt_crossing(e.golden_hour_end),
            e.solar_noon,
            fmt_crossing(e.golden_hour_start),
            fmt_crossing(e.sunset),
            fmt_crossing(e.civil_dusk),
            fmt_crossing(e.nautical_dusk),
            fmt_crossing(e.astronomical_dusk),
        ));
    }
    out.join("\n\n")
}

#[test]
fn solar_model_is_deterministic_100x_in_a_loop() {
    let baseline = render_golden_solar_summary();
    for iteration in 0..100 {
        let current = render_golden_solar_summary();
        assert_eq!(current, baseline, "mismatch on iteration {iteration}");
    }
}

#[test]
fn solar_timelines_match_golden_snapshot() {
    let summary = render_golden_solar_summary();
    insta::assert_snapshot!("solar_representative_timelines", summary);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn solar_snapshot_invariants_hold_globally(
        lat in -90.0_f64..=90.0_f64,
        lon in -180.0_f64..=180.0_f64,
        // 2000-01-01T00:00:00Z ..= 2050-12-31T23:59:59Z
        unix_secs in 946_684_800_i64..=2_556_143_999_i64,
    ) {
        let coords = Coordinates::new(lat, lon).expect("valid generated coordinates");
        let instant = UtcInstant::from_unix_secs(unix_secs);
        let angles = SolarEventAngles::default();
        let snap = solar_snapshot(instant, coords, &angles);

        prop_assert!(snap.elevation_deg.is_finite());
        prop_assert!((-90.5..=90.5).contains(&snap.elevation_deg));
        prop_assert!(snap.position.geometric_elevation_deg.is_finite());
        prop_assert!((-90.0001..=90.0001).contains(&snap.position.geometric_elevation_deg));
        prop_assert!(snap.position.azimuth_deg.is_finite());
        prop_assert!((0.0..360.0).contains(&snap.position.azimuth_deg));
        prop_assert!(snap.position.declination_deg.is_finite());
        prop_assert!((-23.5..=23.5).contains(&snap.position.declination_deg));
        prop_assert!(snap.position.equation_of_time_min.is_finite());
        prop_assert!((-20.0..=20.0).contains(&snap.position.equation_of_time_min));
        prop_assert!(snap.position.hour_angle_deg >= -180.0 && snap.position.hour_angle_deg < 180.0);
        prop_assert_eq!(snap.is_morning, snap.position.hour_angle_deg < 0.0);
        prop_assert_eq!(snap.elevation_deg, snap.position.elevation_deg);

        // Refraction only lifts or preserves geometric elevation above -5 deg, and stays bounded everywhere.
        let refraction = snap.elevation_deg - snap.position.geometric_elevation_deg;
        prop_assert!(refraction.is_finite() && refraction.abs() < 1.5);

        // Noon vs nadir culmination ordering: strictly noon >= nadir up to 88 deg latitude,
        // and within 0.30 deg right at the poles where 12h seasonal declination drift (~0.20 deg)
        // can exceed the vanishing diurnal spin amplitude near an equinox.
        if lat.abs() <= 88.0 {
            prop_assert!(
                snap.noon_elevation_deg + 1e-6 >= snap.nadir_elevation_deg,
                "noon {:.6} < nadir {:.6} at lat {:.4}",
                snap.noon_elevation_deg,
                snap.nadir_elevation_deg,
                lat
            );
        } else {
            prop_assert!(snap.noon_elevation_deg + 0.30 >= snap.nadir_elevation_deg);
        }

        // Concrete events on the solar day must be monotonically non-decreasing in time.
        let ordered = snap.events.ordered_instants();
        prop_assert!(ordered.len() >= 2, "at least solar_nadir and solar_noon exist");
        for pair in ordered.windows(2) {
            prop_assert!(
                pair[0].1 <= pair[1].1,
                "non-monotonic events {:?} ({}) > {:?} ({}) at lat={lat}, lon={lon}, t={instant}",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }

        // PolarState consistency with sunrise & sunset crossings.
        match snap.polar_state {
            PolarState::PolarDay => {
                prop_assert!(snap.events.sunrise.instant().is_none());
                prop_assert!(snap.events.sunset.instant().is_none());
            }
            PolarState::PolarNight => {
                prop_assert!(snap.events.sunrise.instant().is_none());
                prop_assert!(snap.events.sunset.instant().is_none());
            }
            PolarState::Normal => {
                prop_assert!(
                    snap.events.sunrise.instant().is_some()
                        || snap.events.sunset.instant().is_some()
                );
            }
        }

        // 1-minute continuity: geometric elevation changes by at most Earth's 0.25 deg/min
        // spin rate, while apparent elevation adds at most ~0.18 deg/min across NOAA's
        // `-20.774 / tan(elev)` refraction ramp just below `-0.575 deg`.
        let snap_next_min = solar_snapshot(instant.add_minutes(1), coords, &angles);
        let delta_geom =
            (snap_next_min.position.geometric_elevation_deg - snap.position.geometric_elevation_deg)
                .abs();
        prop_assert!(
            delta_geom <= 0.26,
            "1-minute geometric elevation jump {:.6} deg at lat={lat}, lon={lon}, t={instant}",
            delta_geom
        );
        let delta_elev = (snap_next_min.elevation_deg - snap.elevation_deg).abs();
        prop_assert!(
            delta_elev <= 0.50,
            "1-minute apparent elevation jump {:.6} deg at lat={lat}, lon={lon}, t={instant}",
            delta_elev
        );
    }
}
