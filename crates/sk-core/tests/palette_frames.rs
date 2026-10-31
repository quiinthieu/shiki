use proptest::prelude::*;
use sk_core::color::{
    contrast_ratio, delta_e_ok, shortest_hue_delta_deg, ContrastThresholds, Rgb8,
};
use sk_core::palette::{
    check_palette_year, classify_solar_phase, compute_frame, day_phase_from_elevation,
    fix_up_colors, morning_weight_from_hour_angle, oklch_lerp, AnchorHalf, CheckedSlot, DayHalf,
    DayPhaseConfig, ModelConfig, Palette, PaletteColors, PaletteOklchColors, SolarPhase,
};
use sk_core::season::{equator_season_strength, season_state, Season, SeasonConfig};
use sk_core::solar::{Coordinates, SolarEventAngles, DEFAULT_MODEL_TOML};
use sk_core::time::UtcInstant;

fn minimal_two_anchor_toml(night_bg: &str, night_fg: &str, day_bg: &str, day_fg: &str) -> String {
    format!(
        r##"
name = "two-anchor"

[[anchors]]
d = 0.0
s = 0.0
bg = "{night_bg}"
fg = "{night_fg}"
cursor = "#e0b070"
selection_bg = "#223044"
selection_fg = "#f0f4fa"
ansi = [
  "#18202c", "#e06c75", "#7ec699", "#e5c07b",
  "#61afef", "#c678dd", "#56b6c2", "#c8d0dc",
  "#5c6778", "#f08088", "#96d9ae", "#f0cf8e",
  "#7cc0f8", "#d690eb", "#70cad5", "#eef2f8",
]

[[anchors]]
d = 1.0
s = 0.0
bg = "{day_bg}"
fg = "{day_fg}"
cursor = "#c06020"
selection_bg = "#d0dcf0"
selection_fg = "#141c28"
ansi = [
  "#e4e8ee", "#a62626", "#1d6e38", "#805600",
  "#1858a8", "#842898", "#0e6874", "#36404e",
  "#6c7684", "#b83232", "#247a42", "#8e6000",
  "#2062b8", "#9232a8", "#147482", "#1c2430",
]
"##
    )
}

#[test]
fn model_config_matches_bundled_model_toml() {
    let parsed =
        ModelConfig::from_model_toml(DEFAULT_MODEL_TOML).expect("parse bundled data/model.toml");
    assert_eq!(parsed, ModelConfig::default());
    assert_eq!(parsed, ModelConfig::bundled());
}

#[test]
fn schema_reports_exact_file_line_and_column_for_syntax_and_semantic_errors() {
    // 1. TOML syntax error on line 3.
    let bad_syntax = "name = \"broken\"\n\n[[anchors]\nd = 0.0\n";
    let err = Palette::from_toml_with_source(bad_syntax, "custom/broken.toml").unwrap_err();
    assert_eq!(err.source_name(), "custom/broken.toml");
    assert_eq!(err.line(), Some(3));
    assert!(err.column().is_some());
    assert!(err.to_string().starts_with("custom/broken.toml:3:"));

    // 2. Invalid hex colour inside `ansi` array on a specific line.
    let bad_hex = r##"name = "bad-hex"

[[anchors]]
d = 0.0
bg = "#10141c"
fg = "#e0e6f0"
cursor = "#e0b070"
selection = "#243448"
ansi = [
  "#18202c", "#e06c75", "#7ec699", "#e5c07b",
  "#61afef", "#zzzzzz", "#56b6c2", "#c8d0dc",
  "#5c6778", "#f08088", "#96d9ae", "#f0cf8e",
  "#7cc0f8", "#d690eb", "#70cad5", "#eef2f8",
]

[[anchors]]
d = 1.0
bg = "#304050"
fg = "#f4f7fb"
cursor = "#e0b070"
selection = "#1d2936"
ansi = [
  "#18202c", "#f08c88", "#8ee2b0", "#f2d68a",
  "#8ccbf8", "#e2a8f0", "#80dee4", "#e0e8f0",
  "#788c9e", "#f9a4a0", "#a8ecc2", "#f8e1a2",
  "#a4d8fc", "#ecbef6", "#9ae7ec", "#f7fafd",
]
"##;
    let err = Palette::from_toml_with_source(bad_hex, "palettes/bad-hex.toml").unwrap_err();
    assert_eq!(err.source_name(), "palettes/bad-hex.toml");
    assert_eq!(err.line(), Some(11));
    assert!(
        err.message().contains("#zzzzzz"),
        "expected offending literal in error: {err}"
    );

    // 3. Wrong ANSI slot count (15 instead of 16) on line 9.
    let short_ansi = r##"name = "short-ansi"

[[anchors]]
d = 0.0
bg = "#10141c"
fg = "#e0e6f0"
cursor = "#e0b070"
selection = "#243448"
ansi = [
  "#18202c", "#e06c75", "#7ec699", "#e5c07b",
  "#61afef", "#c678dd", "#56b6c2", "#c8d0dc",
  "#5c6778", "#f08088", "#96d9ae", "#f0cf8e",
  "#7cc0f8", "#d690eb", "#70cad5",
]
"##;
    let err = Palette::from_toml_with_source(short_ansi, "palettes/short.toml").unwrap_err();
    assert_eq!(err.source_name(), "palettes/short.toml");
    assert_eq!(err.line(), Some(9));
    assert!(err.message().contains("16"), "unexpected message: {err}");

    // 4. Out-of-range `d = 1.25` on line 4.
    let bad_d = minimal_two_anchor_toml("#10141c", "#e0e6f0", "#304050", "#f5f8fc")
        .replace("d = 0.0", "d = 1.25");
    let err = Palette::from_toml_with_source(&bad_d, "palettes/bad-d.toml").unwrap_err();
    assert_eq!(err.line(), Some(5));
    assert!(err.message().contains("[0.0, 1.0]"), "got: {err}");

    // 5. Out-of-range `s = 1.0` (`s` must be in `[0.0, 1.0)`).
    let bad_s = minimal_two_anchor_toml("#10141c", "#e0e6f0", "#304050", "#f5f8fc")
        .replace("s = 0.0", "s = 1.0");
    let err = Palette::from_toml_with_source(&bad_s, "palettes/bad-s.toml").unwrap_err();
    assert_eq!(err.line(), Some(6));
    assert!(err.message().contains("[0.0, 1.0)"), "got: {err}");

    // 6. Duplicate anchor `(d = 0.0, s = 0.0)` reported on the second anchor's line.
    let dup = minimal_two_anchor_toml("#10141c", "#e0e6f0", "#304050", "#f5f8fc")
        .replace("d = 1.0", "d = 0.0");
    let err = Palette::from_toml_with_source(&dup, "palettes/dup.toml").unwrap_err();
    assert_eq!(err.source_name(), "palettes/dup.toml");
    assert!(
        err.line().unwrap_or(0) >= 18,
        "expected duplicate line on 2nd anchor: {err}"
    );
    assert!(err.message().contains("duplicate"), "got: {err}");

    // 7. Missing `d = 1.0` day anchor.
    let missing_day = minimal_two_anchor_toml("#10141c", "#e0e6f0", "#304050", "#f5f8fc")
        .replace("d = 1.0", "d = 0.6");
    let err =
        Palette::from_toml_with_source(&missing_day, "palettes/missing-day.toml").unwrap_err();
    assert_eq!(err.source_name(), "palettes/missing-day.toml");
    assert!(err.line().is_some());
    assert!(err.message().contains("1.0"), "got: {err}");
}

#[test]
fn day_phase_keyframes_normalize_low_winter_sun_and_classify_phases() {
    let angles = SolarEventAngles::default();
    let cfg = DayPhaseConfig::default();

    // Standard high-sun day (`noon_elevation = 55 deg >= 24 deg`):
    assert_eq!(day_phase_from_elevation(-25.0, 55.0, &angles, &cfg), 0.0);
    assert_eq!(
        day_phase_from_elevation(-18.0, 55.0, &angles, &cfg),
        cfg.night_d
    );
    assert!((day_phase_from_elevation(-12.0, 55.0, &angles, &cfg) - cfg.nautical_d).abs() < 1e-12);
    assert!((day_phase_from_elevation(-6.0, 55.0, &angles, &cfg) - cfg.civil_d).abs() < 1e-12);
    assert!(
        (day_phase_from_elevation(-0.833, 55.0, &angles, &cfg) - cfg.sunrise_sunset_d).abs()
            < 1e-12
    );
    assert!((day_phase_from_elevation(6.0, 55.0, &angles, &cfg) - cfg.golden_d).abs() < 1e-12);
    assert_eq!(day_phase_from_elevation(24.0, 55.0, &angles, &cfg), 1.0);
    assert_eq!(day_phase_from_elevation(55.0, 55.0, &angles, &cfg), 1.0);

    // Low winter sun (`London` ~15.1 deg and `Oslo` ~6.6 deg at winter solstice) still reaches
    // full `day` (`d = 1.0`) at solar noon (`PLAN.md` §4).
    assert!(
        (day_phase_from_elevation(15.1, 15.1, &angles, &cfg) - 1.0).abs() < 1e-12,
        "London winter noon must reach d = 1.0"
    );
    assert!(
        (day_phase_from_elevation(6.6, 6.6, &angles, &cfg) - 1.0).abs() < 1e-12,
        "Oslo winter noon must reach d = 1.0"
    );

    // Phase classification distinguishes morning vs evening (`dawn != dusk`).
    assert_eq!(
        classify_solar_phase(0.0, DayHalf::Dawn, &cfg),
        SolarPhase::Night
    );
    assert_eq!(
        classify_solar_phase(0.10, DayHalf::Dawn, &cfg),
        SolarPhase::AstronomicalDawn
    );
    assert_eq!(
        classify_solar_phase(0.10, DayHalf::Dusk, &cfg),
        SolarPhase::AstronomicalDusk
    );
    assert_eq!(
        classify_solar_phase(0.70, DayHalf::Dawn, &cfg),
        SolarPhase::GoldenDawn
    );
    assert_eq!(
        classify_solar_phase(0.70, DayHalf::Dusk, &cfg),
        SolarPhase::GoldenDusk
    );
    assert_eq!(
        classify_solar_phase(1.0, DayHalf::Dusk, &cfg),
        SolarPhase::Day
    );

    // Morning weight is 1.0 during morning, 0.0 during evening, and 0.5 right at culmination/nadir.
    assert_eq!(morning_weight_from_hour_angle(-90.0, 8.0), 1.0);
    assert_eq!(morning_weight_from_hour_angle(90.0, 8.0), 0.0);
    assert!((morning_weight_from_hour_angle(0.0, 8.0) - 0.5).abs() < 1e-12);
    assert!((morning_weight_from_hour_angle(-180.0, 8.0) - 0.5).abs() < 1e-12);
}

#[test]
fn season_phase_anchors_at_equinoxes_and_solstices_flips_hemisphere_and_fades_at_equator() {
    let cfg = SeasonConfig::default();
    let north = Coordinates::new(45.0, 0.0).unwrap();
    let south = Coordinates::new(-45.0, 0.0).unwrap();

    // 2026 astronomical equinoxes & solstices (UTC within ~1h):
    // - March equinox: 2026-03-20T14:46Z -> s ≈ 0.00 (North Spring) / 0.50 (South Autumn)
    // - June solstice: 2026-06-21T08:24Z -> s ≈ 0.25 (North Summer) / 0.75 (South Winter)
    // - Sept equinox:  2026-09-23T00:05Z -> s ≈ 0.50 (North Autumn) / 0.00 (South Spring)
    // - Dec solstice:  2026-12-21T20:50Z -> s ≈ 0.75 (North Winter) / 0.25 (South Summer)
    let mar_eq = UtcInstant::utc_ymd_hms(2026, 3, 20, 14, 46, 0);
    let jun_sol = UtcInstant::utc_ymd_hms(2026, 6, 21, 8, 24, 0);
    let sep_eq = UtcInstant::utc_ymd_hms(2026, 9, 23, 0, 5, 0);
    let dec_sol = UtcInstant::utc_ymd_hms(2026, 12, 21, 20, 50, 0);

    let wrap_dist = |a: f64, b: f64| {
        let d = (a - b).rem_euclid(1.0);
        d.min(1.0 - d)
    };

    let n_mar = season_state(mar_eq, north, &cfg);
    let s_mar = season_state(mar_eq, south, &cfg);
    assert!(wrap_dist(n_mar.phase, 0.0) < 0.002, "got {}", n_mar.phase);
    assert!(wrap_dist(s_mar.phase, 0.5) < 0.002, "got {}", s_mar.phase);
    assert_eq!(n_mar.strength, 1.0);
    assert_eq!(s_mar.strength, 1.0);

    let n_jun = season_state(jun_sol, north, &cfg);
    let s_jun = season_state(jun_sol, south, &cfg);
    assert!(wrap_dist(n_jun.phase, 0.25) < 0.002, "got {}", n_jun.phase);
    assert!(wrap_dist(s_jun.phase, 0.75) < 0.002, "got {}", s_jun.phase);
    assert_eq!(n_jun.season, Season::Summer);
    assert_eq!(s_jun.season, Season::Winter);

    let n_sep = season_state(sep_eq, north, &cfg);
    let s_sep = season_state(sep_eq, south, &cfg);
    assert!(wrap_dist(n_sep.phase, 0.50) < 0.002, "got {}", n_sep.phase);
    assert!(wrap_dist(s_sep.phase, 0.00) < 0.002, "got {}", s_sep.phase);

    let n_dec = season_state(dec_sol, north, &cfg);
    let s_dec = season_state(dec_sol, south, &cfg);
    assert!(wrap_dist(n_dec.phase, 0.75) < 0.002, "got {}", n_dec.phase);
    assert!(wrap_dist(s_dec.phase, 0.25) < 0.002, "got {}", s_dec.phase);

    // Equator fade: 0 at 0 deg, strictly monotonic on (0, 23.439), 1 at >= 23.44 deg.
    assert_eq!(equator_season_strength(0.0, &cfg), 0.0);
    assert!(equator_season_strength(1.35, &cfg) < 0.02); // Singapore
    let hcmc = equator_season_strength(10.8, &cfg);
    assert!((0.35..0.55).contains(&hcmc), "got {hcmc}");
    assert_eq!(equator_season_strength(35.0, &cfg), 1.0);

    // Crossing the equator (+0.05 deg vs -0.05 deg) produces virtually identical frames without a
    // hemisphere flip jump.
    let shiki = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let eq_north = Coordinates::new(0.05, 103.8).unwrap();
    let eq_south = Coordinates::new(-0.05, 103.8).unwrap();
    let f_n = compute_frame(jun_sol, eq_north, &shiki, &model);
    let f_s = compute_frame(jun_sol, eq_south, &shiki, &model);
    assert!(
        f_n.max_delta_e(&f_s) < 0.005,
        "equator crossing jumped by ΔE = {}",
        f_n.max_delta_e(&f_s)
    );
}

#[test]
fn oklch_blend_downweights_grey_hue_so_greys_do_not_spin_and_reproduces_anchors_exactly() {
    // Pure grey `#202020` (`C = 0`, `h = 0 deg`) blended into slate blue `#1c2a40` (`h ≈ 255 deg`)
    // must lock onto the blue hue immediately rather than spinning through magenta (`~310 deg`).
    let grey = Rgb8::from_hex("#202020").unwrap().to_oklch();
    let blue = Rgb8::from_hex("#1c2a40").unwrap().to_oklch();
    assert_eq!(grey.h_deg, 0.0);
    assert!(blue.c > 0.02);

    for step in 1..=10 {
        let t = f64::from(step) / 10.0;
        let mid = oklch_lerp(grey, blue, t);
        let hue_err = shortest_hue_delta_deg(blue.h_deg, mid.h_deg).abs();
        assert!(
            hue_err < 1e-6,
            "grey->blue blend spun away from blue hue at t={t}: got h={}, expected {}",
            mid.h_deg,
            blue.h_deg
        );
    }

    // `PLAN.md` §9: "A blend at an anchor returns the anchor."
    let shiki = Palette::bundled_shiki();
    assert_eq!(shiki.anchors().len(), 24);

    for anchor in shiki.anchors() {
        let morning_weights: &[f64] = match anchor.half {
            AnchorHalf::Dawn => &[1.0],
            AnchorHalf::Dusk => &[0.0],
            AnchorHalf::Both => &[0.0, 0.5, 1.0],
        };
        for &mw in morning_weights {
            let blended = shiki
                .blend_oklch(anchor.d, anchor.s, mw, 1.0)
                .to_rgb8_gamut_mapped();
            assert_eq!(
                blended, anchor.colors,
                "blend at anchor (d={}, s={}, half={:?}, mw={mw}) did not return exact anchor",
                anchor.d, anchor.s, anchor.half
            );
        }

        // Bundled accessibility check (T17 preview): ANSI red (1, 9) and green (2, 10) differ in L.
        let red_l = anchor.colors.ansi[1].to_oklab().l;
        let green_l = anchor.colors.ansi[2].to_oklab().l;
        let bright_red_l = anchor.colors.ansi[9].to_oklab().l;
        let bright_green_l = anchor.colors.ansi[10].to_oklab().l;
        assert!(
            (green_l - red_l).abs() >= 0.06,
            "ANSI green and red too close in L at (d={}, s={}): red={red_l}, green={green_l}",
            anchor.d,
            anchor.s
        );
        assert!(
            (bright_green_l - bright_red_l).abs() >= 0.06,
            "ANSI bright green and red too close in L at (d={}, s={})",
            anchor.d,
            anchor.s
        );
    }

    // Dawn != dusk on `shiki` at intermediate twilight (`d = 0.40`) and golden hour (`d = 0.75`).
    for &d in &[0.40, 0.75] {
        let dawn = shiki.blend_oklch(d, 0.25, 1.0, 1.0).to_rgb8_gamut_mapped();
        let dusk = shiki.blend_oklch(d, 0.25, 0.0, 1.0).to_rgb8_gamut_mapped();
        assert_ne!(dawn.bg, dusk.bg, "dawn and dusk bg should differ at d={d}");
        assert!(
            delta_e_ok(dawn.bg, dusk.bg) > 0.01,
            "dawn vs dusk should be perceptibly distinct at d={d}"
        );
    }

    // Regression test for antipodal hue branch-cut continuity in 2-D bilinear OKLCH blend.
    let model = ModelConfig::bundled();
    let coords = Coordinates::new(53.37160349153995, -84.87615279985705).unwrap();
    let t0 = UtcInstant::from_unix_secs(1_776_206_863);
    let f0 = compute_frame(t0, coords, &shiki, &model);
    let f1 = compute_frame(t0.add_secs(60), coords, &shiki, &model);
    assert!(
        f0.max_delta_e(&f1) < 0.02,
        "antipodal hue branch cut caused 1-minute jump: {}",
        f0.max_delta_e(&f1)
    );
}

#[test]
fn contrast_fixup_moves_slot_l_at_constant_chroma_hue_preserves_exempt_slots_and_nudges_bg_when_needed(
) {
    let thresholds = ContrastThresholds::default();

    // Dark bg `#18202c` (`L ≈ 0.24`), failing `fg` `#4a586e` (`contrast < 2.5:1`), failing `ansi[1]`,
    // and intentionally low-contrast exempt `ansi[0]` / `ansi[8]`.
    let bg = Rgb8::from_hex("#18202c").unwrap();
    let low_fg = Rgb8::from_hex("#4a586e").unwrap();
    let low_red = Rgb8::from_hex("#5a2c32").unwrap();
    let exempt_0 = Rgb8::from_hex("#1c2430").unwrap();
    let exempt_8 = Rgb8::from_hex("#2a3444").unwrap();
    let passing_green = Rgb8::from_hex("#7ec699").unwrap();

    let mut ansi = [passing_green; 16];
    ansi[0] = exempt_0;
    ansi[1] = low_red;
    ansi[8] = exempt_8;

    let raw_colors = PaletteColors {
        bg,
        fg: low_fg,
        cursor: Rgb8::from_hex("#e0b070").unwrap(),
        selection_bg: Rgb8::from_hex("#2a384c").unwrap(),
        selection_fg: Rgb8::from_hex("#48566a").unwrap(), // also failing against selection_bg!
        ansi,
    };
    assert!(!raw_colors.passes_contrast(&thresholds));

    let raw_oklch = PaletteOklchColors::from_rgb8(&raw_colors);
    let (fixed, adjusted_count) = fix_up_colors(&raw_oklch, &thresholds);

    assert!(fixed.passes_contrast(&thresholds));
    assert_eq!(
        fixed.bg, bg,
        "bg must not move when slot L adjustment suffices"
    );
    assert_eq!(
        adjusted_count, 3,
        "fg, selection_fg, and ansi[1] should be adjusted"
    );
    assert_eq!(fixed.ansi[0], exempt_0, "ANSI 0 is exempt by default");
    assert_eq!(fixed.ansi[8], exempt_8, "ANSI 8 is exempt by default");
    assert_eq!(
        fixed.ansi[2], passing_green,
        "already-passing slots stay untouched"
    );

    // Minimality & hue preservation on `ansi[1]`:
    assert!(contrast_ratio(fixed.ansi[1], fixed.bg) >= 3.0);
    assert!(
        contrast_ratio(fixed.ansi[1], fixed.bg) < 3.25,
        "fix-up should take the smallest passing L step, got {}",
        contrast_ratio(fixed.ansi[1], fixed.bg)
    );
    let h_before = low_red.to_oklch().h_deg;
    let h_after = fixed.ansi[1].to_oklch().h_deg;
    assert!(
        shortest_hue_delta_deg(h_before, h_after).abs() < 3.0,
        "hue drifted during L fix-up: {h_before} -> {h_after}"
    );

    // "else nudge the bg": with AAA `7.0:1` threshold on mid-grey `#767676` (`Y ≈ 0.184`), neither
    // pure white (`~4.49:1`) nor pure black (`~4.68:1`) can reach `7.0:1` without nudging `bg`!
    let aaa = ContrastThresholds {
        fg_bg_min_ratio: 7.0,
        ansi_bg_min_ratio: 4.5,
    };
    let mid_grey = Rgb8::from_hex("#767676").unwrap();
    let mid_raw = PaletteOklchColors::from_rgb8(&PaletteColors {
        bg: mid_grey,
        fg: Rgb8::from_hex("#e0e0e0").unwrap(),
        cursor: Rgb8::from_hex("#ffffff").unwrap(),
        selection_bg: mid_grey,
        selection_fg: Rgb8::from_hex("#101010").unwrap(),
        ansi: [Rgb8::from_hex("#d0d0d0").unwrap(); 16],
    });
    let (aaa_fixed, aaa_adjusted) = fix_up_colors(&mid_raw, &aaa);
    assert!(aaa_adjusted > 0);
    assert_ne!(
        aaa_fixed.bg, mid_grey,
        "bg must nudge when 7.0:1 is impossible at mid-grey"
    );
    assert!(
        aaa_fixed.passes_contrast(&aaa),
        "nudged bg frame must pass 7.0:1 / 4.5:1: fg/bg={}, sel={}",
        contrast_ratio(aaa_fixed.fg, aaa_fixed.bg),
        contrast_ratio(aaa_fixed.selection_fg, aaa_fixed.selection_bg)
    );

    // Cross-polarity two-theme palette (dark night `#10141c` -> light day `#f6f3eb`) passes §4
    // contrast at every step `d` in `0.0..=1.0`.
    let cross_toml = minimal_two_anchor_toml("#10141c", "#e4eaf2", "#f6f3eb", "#1c222b");
    let cross_palette = Palette::from_toml(&cross_toml).expect("parse two-anchor cross palette");
    for step in 0..=100 {
        let d = f64::from(step) / 100.0;
        let blended = cross_palette.blend_oklch(d, 0.0, 1.0, 1.0);
        let (colors, _) = fix_up_colors(&blended, &thresholds);
        assert!(
            colors.passes_contrast(&thresholds),
            "cross-polarity frame failed contrast at d={d}"
        );
    }
}

#[test]
fn year_sweep_at_10_minute_steps_passes_in_both_hemispheres_polar_and_equator() {
    let shiki = Palette::bundled_shiki();
    let model = ModelConfig::bundled();

    // T06 AC: "property tests pass over a year at 10-minute steps in both hemispheres."
    // Test Northern temperate (Tokyo), Northern polar (Tromsø), Equatorial (Singapore),
    // Southern temperate (Sydney), and Southern subpolar (Ushuaia).
    let cities = [
        ("Tokyo", 35.6762, 139.6503, true),
        ("Tromso", 69.6492, 18.9553, true),
        ("Singapore", 1.3521, 103.8198, true),
        ("Sydney", -33.8688, 151.2093, false),
        ("Ushuaia", -54.8019, -68.3030, false),
    ];

    for (city, lat, lon, expected_north) in cities {
        let coords = Coordinates::new(lat, lon).unwrap();
        let report = check_palette_year(&shiki, coords, 2026, 10, &model)
            .unwrap_or_else(|e| panic!("year sweep failed for {city}: {e}"));

        assert_eq!(report.palette_name, "shiki");
        assert_eq!(report.year, 2026);
        assert_eq!(report.is_northern_hemisphere, expected_north);
        assert_eq!(report.step_minutes, 10);
        assert_eq!(report.frames_checked, 52_560);
        assert!(report.all_passed, "{city} failed year contrast sweep");
        assert_eq!(
            report.frames_fixed_up, 0,
            "bundled `shiki` palette should pass naturally with 0 fix-ups at {city}"
        );
        assert!(
            report.worst_text.final_contrast_ratio >= model.contrast.fg_bg_min_ratio,
            "{city} worst text contrast {} < {}",
            report.worst_text.final_contrast_ratio,
            model.contrast.fg_bg_min_ratio
        );
        assert!(
            matches!(
                report.worst_text.slot,
                CheckedSlot::Fg | CheckedSlot::Selection
            ),
            "unexpected worst_text slot: {:?}",
            report.worst_text.slot
        );
        assert!(
            report.worst_ansi.final_contrast_ratio >= model.contrast.ansi_bg_min_ratio,
            "{city} worst ansi contrast {} < {}",
            report.worst_ansi.final_contrast_ratio,
            model.contrast.ansi_bg_min_ratio
        );
        assert!(
            matches!(report.worst_ansi.slot, CheckedSlot::Ansi(1..=7 | 9..=15)),
            "unexpected worst_ansi slot: {:?}",
            report.worst_ansi.slot
        );
        assert!(report.worst_overall.margin() >= 0.0);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_every_frame_passes_wcag_and_adjacent_minutes_have_bounded_delta_e(
        // Sweep across 2026..2028 (including leap year 2028) and all inhabited/polar latitudes.
        unix_secs in 1_767_225_600_i64..1_861_920_000_i64,
        lat in -82.0_f64..=82.0_f64,
        lon in -180.0_f64..=180.0_f64,
    ) {
        let shiki = Palette::bundled_shiki();
        let model = ModelConfig::bundled();
        let coords = Coordinates::new(lat, lon).unwrap();
        let t0 = UtcInstant::from_unix_secs(unix_secs);
        let t1 = t0.add_secs(60);

        let f0 = compute_frame(t0, coords, &shiki, &model);
        let f1 = compute_frame(t1, coords, &shiki, &model);

        prop_assert!(f0.passes_contrast(&model.contrast));
        prop_assert!(f1.passes_contrast(&model.contrast));

        // `PLAN.md` §1.1 G1 & §9: "Never a light/dark flip." "Frames a minute apart differ by a bounded ΔE."
        let step_de = f0.max_delta_e(&f1);
        prop_assert!(
            step_de < 0.03,
            "1-minute ΔE_OK too large ({step_de}) at t={t0}, lat_sign={}",
            lat.signum()
        );
    }
}

#[test]
fn shiki_seasonal_and_year_check_summary_matches_golden_snapshot() {
    let shiki = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let tokyo = Coordinates::new(35.6762, 139.6503).unwrap();
    let sydney = Coordinates::new(-33.8688, 151.2093).unwrap();

    let samples = [
        (
            "tokyo_spring_dawn",
            UtcInstant::utc_ymd_hms(2026, 3, 20, 20, 35, 0),
            tokyo,
        ),
        (
            "tokyo_summer_noon",
            UtcInstant::utc_ymd_hms(2026, 6, 22, 2, 45, 0),
            tokyo,
        ),
        (
            "tokyo_autumn_dusk",
            UtcInstant::utc_ymd_hms(2026, 9, 23, 8, 45, 0),
            tokyo,
        ),
        (
            "tokyo_winter_night",
            UtcInstant::utc_ymd_hms(2026, 12, 22, 15, 0, 0),
            tokyo,
        ),
        (
            "sydney_winter_noon_jun",
            UtcInstant::utc_ymd_hms(2026, 6, 22, 2, 0, 0),
            sydney,
        ),
    ];

    let mut lines = Vec::new();
    for (label, instant, coords) in samples {
        let f = compute_frame(instant, coords, &shiki, &model);
        lines.push(format!(
            "{label}: t={instant} phase={} season={} d={:.3} s={:.3} bg={} fg={} sel_bg={} ansi1={} ansi2={} fixup={}",
            f.solar_phase.as_str(),
            f.season.as_str(),
            f.day_phase,
            f.season_phase,
            f.colors.bg,
            f.colors.fg,
            f.colors.selection_bg,
            f.colors.ansi[1],
            f.colors.ansi[2],
            f.fixup_slots,
        ));
    }

    let north_report = check_palette_year(&shiki, tokyo, 2026, 10, &model).unwrap();
    let south_report = check_palette_year(&shiki, sydney, 2026, 10, &model).unwrap();
    for (tag, rep) in [("north_2026", &north_report), ("south_2026", &south_report)] {
        lines.push(format!(
            "{tag}: frames={} fixed={} worst_overall=({} {} ratio={:.2} min={:.2} d={:.3} s={:.3}) worst_text=({} ratio={:.2})",
            rep.frames_checked,
            rep.frames_fixed_up,
            rep.worst_overall.instant,
            rep.worst_overall.slot,
            rep.worst_overall.final_contrast_ratio,
            rep.worst_overall.required_ratio,
            rep.worst_overall.day_phase,
            rep.worst_overall.season_phase,
            rep.worst_text.slot,
            rep.worst_text.final_contrast_ratio,
        ));
    }

    let summary = lines.join("\n");
    insta::assert_snapshot!("shiki_palette_frames_summary", summary);
}
