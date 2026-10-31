use proptest::prelude::*;
use sk_core::location::{
    parse_iso6709_coords, round_manual_coord, vendored_tzdb_version, LocationError, LocationMode,
    ManualLocation, ResolvedLocation, TzDb, MANUAL_COORD_DECIMALS, VENDORED_BACKWARD,
    VENDORED_ZONE1970_TAB,
};
use sk_core::product;
use sk_core::solar::{solar_snapshot, SolarEventAngles};
use sk_core::time::UtcInstant;

#[test]
fn vendored_tzdb_version_matches_iana_release() {
    assert_eq!(MANUAL_COORD_DECIMALS, 1);
    assert_eq!(vendored_tzdb_version(), "2026f");

    let db = TzDb::bundled();
    assert_eq!(db.version(), "2026f");
    assert!(
        db.zone_count() >= 300,
        "expected >= 300 canonical zone1970.tab entries, got {}",
        db.zone_count()
    );
    assert!(
        db.alias_count() >= 240,
        "expected >= 240 backward Link aliases, got {}",
        db.alias_count()
    );
}

#[test]
fn asia_saigon_equals_asia_ho_chi_minh_and_produces_identical_solar_snapshots() {
    let db = TzDb::bundled();
    let saigon = db.lookup("Asia/Saigon").expect("resolve Asia/Saigon");
    let hcm = db
        .lookup("Asia/Ho_Chi_Minh")
        .expect("resolve Asia/Ho_Chi_Minh");

    assert!(saigon.is_alias());
    assert!(!hcm.is_alias());
    assert_eq!(saigon.requested_zone, "Asia/Saigon");
    assert_eq!(hcm.requested_zone, "Asia/Ho_Chi_Minh");
    assert_eq!(saigon.canonical_zone, "Asia/Ho_Chi_Minh");
    assert_eq!(saigon.canonical_zone, hcm.canonical_zone);
    assert_eq!(saigon.coordinates, hcm.coordinates);
    assert_eq!(saigon.country_codes, vec!["VN".to_string()]);
    assert_eq!(saigon.country_codes, hcm.country_codes);
    assert_eq!(saigon.comment, hcm.comment);
    assert!(!saigon.is_multi_country());

    assert!((saigon.coordinates.latitude_deg() - 10.75).abs() < 1e-12);
    assert!((saigon.coordinates.longitude_deg() - (106.0 + 40.0 / 60.0)).abs() < 1e-12);

    let angles = SolarEventAngles::from_model_toml(sk_core::solar::DEFAULT_MODEL_TOML)
        .expect("load default solar event angles");
    let instant = UtcInstant::utc_ymd_hms(2026, 3, 20, 5, 0, 0);
    let snap_saigon = solar_snapshot(instant, saigon.coordinates, &angles);
    let snap_hcm = solar_snapshot(instant, hcm.coordinates, &angles);
    assert_eq!(snap_saigon, snap_hcm);
}

#[test]
fn backward_links_resolve_to_identical_canonical_zones_and_coordinates() {
    let db = TzDb::bundled();
    let expected_pairs = [
        ("Asia/Saigon", "Asia/Ho_Chi_Minh"),
        ("Europe/Kiev", "Europe/Kyiv"),
        ("Asia/Calcutta", "Asia/Kolkata"),
        ("Asia/Katmandu", "Asia/Kathmandu"),
        ("Asia/Rangoon", "Asia/Yangon"),
        ("US/Eastern", "America/New_York"),
        ("US/Pacific", "America/Los_Angeles"),
        ("Japan", "Asia/Tokyo"),
        ("GB", "Europe/London"),
        ("Australia/ACT", "Australia/Sydney"),
        ("Antarctica/McMurdo", "Pacific/Auckland"),
        ("Arctic/Longyearbyen", "Europe/Berlin"),
        ("Europe/Amsterdam", "Europe/Brussels"),
        ("Asia/Phnom_Penh", "Asia/Bangkok"),
    ];

    for (alias, canonical) in expected_pairs {
        let via_alias = db
            .lookup(alias)
            .unwrap_or_else(|e| panic!("failed to resolve alias {alias}: {e}"));
        let via_canonical = db
            .lookup(canonical)
            .unwrap_or_else(|e| panic!("failed to resolve canonical {canonical}: {e}"));

        assert!(via_alias.is_alias());
        assert!(!via_canonical.is_alias());
        assert_eq!(via_alias.canonical_zone, canonical);
        assert_eq!(via_alias.canonical_zone, via_canonical.canonical_zone);
        assert_eq!(via_alias.coordinates, via_canonical.coordinates);
        assert_eq!(via_alias.country_codes, via_canonical.country_codes);
    }

    // Multi-country canonical zones are flagged for future `doctor` coarse-zone warnings.
    let bangkok = db.lookup("Asia/Bangkok").expect("Asia/Bangkok");
    assert!(bangkok.is_multi_country());
}

#[test]
fn every_bundled_zone1970_entry_and_geographic_backward_link_resolves() {
    let db = TzDb::bundled();
    let mut zone_rows = 0usize;

    for line in VENDORED_ZONE1970_TAB.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        zone_rows += 1;
        let cols: Vec<&str> = line.split('\t').collect();
        assert!(cols.len() == 3 || cols.len() == 4);
        let tz = cols[2];
        let resolved = db
            .lookup(tz)
            .unwrap_or_else(|e| panic!("zone1970.tab entry {tz} failed lookup: {e}"));
        assert_eq!(resolved.canonical_zone, tz);
        assert!(!resolved.is_alias());
    }
    assert_eq!(zone_rows, db.zone_count());

    let mut link_rows = 0usize;
    let mut non_geo_links = 0usize;
    for line in VENDORED_BACKWARD.lines() {
        let without_comment = line.split('#').next().unwrap_or("").trim();
        if without_comment.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = without_comment.split_whitespace().collect();
        if tokens.first().copied() != Some("Link") {
            continue;
        }
        link_rows += 1;
        let target = tokens[1];
        let alias = tokens[2];
        if target == "Etc/UTC" || target == "Etc/GMT" {
            non_geo_links += 1;
            let err = db
                .lookup(alias)
                .expect_err("non-geographic Etc/* link must fail lookup");
            assert!(err.to_string().contains("locate --set"));
        } else {
            let resolved_alias = db
                .lookup(alias)
                .unwrap_or_else(|e| panic!("backward link {alias} -> {target} failed: {e}"));
            let resolved_target = db
                .lookup(target)
                .unwrap_or_else(|e| panic!("backward target {target} failed: {e}"));
            assert_eq!(
                resolved_alias.canonical_zone,
                resolved_target.canonical_zone
            );
            assert_eq!(resolved_alias.coordinates, resolved_target.coordinates);
        }
    }
    assert_eq!(link_rows, db.alias_count());
    assert_eq!(non_geo_links, 15);
}

#[test]
fn unknown_and_non_geographic_zones_suggest_locate_set() {
    let db = TzDb::bundled();
    let expected_hint = format!("{} locate --set", product::CLI_BIN);

    for bad_zone in [
        "",
        "   ",
        "Unknown/Zone",
        "Mars/Olympus_Mons",
        "UTC",
        "Etc/UTC",
        "Etc/GMT",
        "GMT",
        "Zulu",
        "EST5EDT",
        "PST8PDT",
    ] {
        let err = db
            .lookup(bad_zone)
            .expect_err("unknown or non-geographic zone must fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&expected_hint),
            "error for '{bad_zone}' must suggest `{expected_hint}`, got: {msg}"
        );
    }

    let missing_sys = LocationError::MissingSystemTimezone.to_string();
    assert!(missing_sys.contains(&expected_hint));
}

#[test]
fn synthetic_tables_support_multi_hop_links_and_reject_cycles_and_malformed_rows() {
    let zone_tab = "VN\t+1045+10640\tAsia/Ho_Chi_Minh\tsouth Vietnam\n";
    let multi_hop_backward = "\
Link\tAsia/Ho_Chi_Minh\tLegacy/Mid
Link\tLegacy/Mid\tLegacy/Old
";
    let db = TzDb::from_tables("test", zone_tab, multi_hop_backward).expect("valid synthetic db");
    let resolved = db.lookup("Legacy/Old").expect("resolve 2-hop alias");
    assert_eq!(resolved.canonical_zone, "Asia/Ho_Chi_Minh");
    assert!(resolved.is_alias());

    let cyclic_backward = "\
Link\tCycle/B\tCycle/A
Link\tCycle/A\tCycle/B
";
    let cyclic_db =
        TzDb::from_tables("test", zone_tab, cyclic_backward).expect("parse cyclic table");
    let cycle_err = cyclic_db
        .lookup("Cycle/A")
        .expect_err("alias cycle must be rejected");
    assert!(matches!(cycle_err, LocationError::AliasCycle { .. }));

    // Malformed zone1970.tab rows.
    for bad_zone_tab in [
        "VN\t+1045+10640\n",
        "VN\t+1065+10640\tAsia/Bad_Minutes\n",
        "VN\t+9100+10640\tAsia/Bad_Lat\n",
    ] {
        let err = TzDb::from_tables("test", bad_zone_tab, "")
            .expect_err("malformed zone1970.tab must fail");
        assert!(matches!(
            err,
            LocationError::TzTableParse {
                table: "zone1970.tab",
                ..
            }
        ));
    }

    // Malformed backward rows.
    for bad_backward in ["Link\tAsia/Ho_Chi_Minh\n", "BogusDirective\tfoo\tbar\n"] {
        let err = TzDb::from_tables("test", zone_tab, bad_backward)
            .expect_err("malformed backward table must fail");
        assert!(matches!(
            err,
            LocationError::TzTableParse {
                table: "backward",
                ..
            }
        ));
    }
}

#[test]
fn iso6709_parser_handles_ddmm_and_ddmmss_in_all_quadrants() {
    let hcm = parse_iso6709_coords("+1045+10640").expect("+1045+10640");
    assert!((hcm.latitude_deg() - 10.75).abs() < 1e-12);
    assert!((hcm.longitude_deg() - (106.0 + 40.0 / 60.0)).abs() < 1e-12);

    let london = parse_iso6709_coords("+513030-0000731").expect("+513030-0000731");
    assert!((london.latitude_deg() - (51.0 + 30.0 / 60.0 + 30.0 / 3_600.0)).abs() < 1e-12);
    assert!((london.longitude_deg() + (7.0 / 60.0 + 31.0 / 3_600.0)).abs() < 1e-12);

    let troll = parse_iso6709_coords("-720041+0023206").expect("-720041+0023206");
    assert!((troll.latitude_deg() + (72.0 + 41.0 / 3_600.0)).abs() < 1e-12);
    assert!((troll.longitude_deg() - (2.0 + 32.0 / 60.0 + 6.0 / 3_600.0)).abs() < 1e-12);

    let montevideo = parse_iso6709_coords("-345433-0561245").expect("-345433-0561245");
    assert!((montevideo.latitude_deg() + (34.0 + 54.0 / 60.0 + 33.0 / 3_600.0)).abs() < 1e-12);
    assert!((montevideo.longitude_deg() + (56.0 + 12.0 / 60.0 + 45.0 / 3_600.0)).abs() < 1e-12);

    let neg_zero = parse_iso6709_coords("-0000-00000").expect("-0000-00000");
    assert!(neg_zero.latitude_deg().is_sign_positive());
    assert!(neg_zero.longitude_deg().is_sign_positive());

    let poles = parse_iso6709_coords("+9000-18000").expect("+9000-18000");
    assert_eq!(poles.latitude_deg(), 90.0);
    assert_eq!(poles.longitude_deg(), -180.0);

    for invalid in [
        "",
        "1045+10640",
        "+104+10640",
        "+1045+1064",
        "+9001+00000",
        "+0000+18001",
        "+1060+10640",
        "+1045+10660",
        "+104560+1064000",
        "+1045+10640+0000",
    ] {
        assert!(
            parse_iso6709_coords(invalid).is_err(),
            "expected ISO 6709 parse failure for '{invalid}'"
        );
    }
}

#[test]
fn manual_location_validates_before_rounding_and_redacts_coordinates_in_debug_and_errors() {
    assert_eq!(round_manual_coord(10.76), 10.8);
    assert_eq!(round_manual_coord(-33.86), -33.9);
    assert!(round_manual_coord(-0.04).is_sign_positive());

    let manual = ManualLocation::parse("10.76,106.66").expect("parse manual pair");
    assert_eq!(manual.latitude_deg(), 10.8);
    assert_eq!(manual.longitude_deg(), 106.7);
    assert_eq!(manual.format_pair(), "10.8,106.7");

    let southern = ManualLocation::parse(" -33.8688 , -70.6693 ").expect("parse negative pair");
    assert_eq!(southern.latitude_deg(), -33.9);
    assert_eq!(southern.longitude_deg(), -70.7);
    assert_eq!(southern.format_pair(), "-33.9,-70.7");

    let zeroed = ManualLocation::new(-0.04, -0.01).expect("near-zero negative pair");
    assert!(zeroed.latitude_deg().is_sign_positive());
    assert!(zeroed.longitude_deg().is_sign_positive());
    assert_eq!(zeroed.format_pair(), "0.0,0.0");

    // Privacy (`PLAN.md` §0 #4): Debug output must redact numeric coordinate values.
    let debug_str = format!("{manual:?}");
    assert!(debug_str.contains("<redacted>"));
    assert!(!debug_str.contains("10.8"));
    assert!(!debug_str.contains("106.7"));

    let resolved = ResolvedLocation::Manual(manual);
    assert_eq!(resolved.mode(), LocationMode::Manual);
    assert_eq!(resolved.format_display_coords(), "10.8,106.7");
    let resolved_debug = format!("{resolved:?}");
    assert!(!resolved_debug.contains("10.8"));
    assert!(!resolved_debug.contains("106.7"));

    // Validation happens BEFORE rounding: 90.04 and -180.04 must be rejected, not rounded in-range.
    assert_eq!(
        ManualLocation::new(90.04, 0.0),
        Err(LocationError::LatitudeOutOfRange)
    );
    assert_eq!(
        ManualLocation::new(-90.04, 0.0),
        Err(LocationError::LatitudeOutOfRange)
    );
    assert_eq!(
        ManualLocation::new(0.0, 180.04),
        Err(LocationError::LongitudeOutOfRange)
    );
    assert_eq!(
        ManualLocation::new(0.0, -180.04),
        Err(LocationError::LongitudeOutOfRange)
    );
    assert_eq!(
        ManualLocation::new(f64::NAN, 0.0),
        Err(LocationError::LatitudeOutOfRange)
    );
    assert_eq!(
        ManualLocation::new(0.0, f64::INFINITY),
        Err(LocationError::LongitudeOutOfRange)
    );

    // Invalid coordinate inputs must NEVER echo coordinate numbers in Display or Debug errors.
    for (bad_input, forbidden_substrings) in [
        ("95.3,106.7", &["95.3", "106.7"][..]),
        ("10.8,195.4", &["10.8", "195.4"][..]),
        ("10.8", &["10.8"][..]),
        ("10.8,106.7,1.5", &["10.8", "106.7", "1.5"][..]),
        ("bad_lat,106.7", &["bad_lat", "106.7"][..]),
        ("10.8,bad_lon", &["10.8", "bad_lon"][..]),
    ] {
        let err = ManualLocation::parse(bad_input).expect_err("invalid pair must fail");
        let display = err.to_string();
        let debug = format!("{err:?}");
        for needle in forbidden_substrings {
            assert!(
                !display.contains(needle) && !debug.contains(needle),
                "error output leaked '{needle}' for input '{bad_input}': display='{display}', debug='{debug}'"
            );
        }
    }
}

#[test]
fn location_summary_matches_golden_snapshot_and_is_deterministic_100x() {
    let zones = [
        "Asia/Saigon",
        "Asia/Ho_Chi_Minh",
        "Europe/London",
        "Europe/Kiev",
        "America/New_York",
        "Antarctica/Troll",
        "Pacific/Auckland",
    ];

    let build_report = || {
        let db = TzDb::bundled();
        let mut lines = vec![format!(
            "tzdb_version={} zones={} aliases={}",
            db.version(),
            db.zone_count(),
            db.alias_count()
        )];
        for zone in zones {
            let resolved = db.lookup(zone).expect("lookup public zone");
            let loc = ResolvedLocation::Timezone(resolved.clone());
            lines.push(format!(
                "{} -> canonical={} alias={} countries={} exact=({:.6},{:.6}) display={}",
                resolved.requested_zone,
                resolved.canonical_zone,
                resolved.is_alias(),
                resolved.country_codes.join(","),
                resolved.coordinates.latitude_deg(),
                resolved.coordinates.longitude_deg(),
                loc.format_display_coords(),
            ));
        }
        lines.join("\n")
    };

    let baseline = build_report();
    for _ in 0..100 {
        assert_eq!(build_report(), baseline);
    }
    insta::assert_snapshot!("vendored_tzdb_public_zones", baseline);
}

proptest! {
    #[test]
    fn manual_coordinates_round_idempotently_and_roundtrip_via_parse(
        lat in -90.0f64..=90.0f64,
        lon in -180.0f64..=180.0f64,
    ) {
        let manual = ManualLocation::new(lat, lon).expect("valid in-range coordinates");
        prop_assert!((-90.0..=90.0).contains(&manual.latitude_deg()));
        prop_assert!((-180.0..=180.0).contains(&manual.longitude_deg()));
        prop_assert!((manual.latitude_deg() - lat).abs() <= 0.050_000_1);
        prop_assert!((manual.longitude_deg() - lon).abs() <= 0.050_000_1);

        let again = ManualLocation::new(manual.latitude_deg(), manual.longitude_deg())
            .expect("rounded coordinates stay valid");
        prop_assert_eq!(manual, again);

        let formatted = manual.format_pair();
        let parsed = ManualLocation::parse(&formatted).expect("formatted pair parses back");
        prop_assert_eq!(manual, parsed);
    }
}
