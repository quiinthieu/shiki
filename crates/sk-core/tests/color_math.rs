use palette::color_difference::EuclideanDistance;
use palette::{
    FromColor, LinSrgb as OracleLinSrgb, Oklab as OracleOklab, Oklch as OracleOklch,
    Srgb as OracleSrgb,
};
use proptest::prelude::*;
use sk_core::color::{
    contrast_hex, contrast_ratio, delta_e_ok, gamut_map_oklch, linear_channel_to_srgb,
    max_in_gamut_chroma, normalize_hue_deg, relative_luminance, shortest_hue_delta_deg,
    srgb_channel_to_linear, ColorError, ContrastThresholds, Oklch, Rgb8, Srgb,
    WCAG_AAA_NORMAL_TEXT_RATIO, WCAG_AA_NON_TEXT_RATIO, WCAG_AA_NORMAL_TEXT_RATIO,
    WCAG_MAX_CONTRAST_RATIO,
};

fn oracle_lin_from_rgb8(c: Rgb8) -> OracleLinSrgb<f64> {
    OracleSrgb::<f64>::new(
        f64::from(c.r) / 255.0,
        f64::from(c.g) / 255.0,
        f64::from(c.b) / 255.0,
    )
    .into_linear()
}

fn oracle_oklab_from_rgb8(c: Rgb8) -> OracleOklab<f64> {
    OracleOklab::from_color(oracle_lin_from_rgb8(c))
}

#[test]
fn wcag_contrast_black_white_is_21_and_known_pairs_match_wcag22() {
    assert_eq!(WCAG_AA_NORMAL_TEXT_RATIO, 4.5);
    assert_eq!(WCAG_AA_NON_TEXT_RATIO, 3.0);
    assert_eq!(WCAG_AAA_NORMAL_TEXT_RATIO, 7.0);
    assert_eq!(WCAG_MAX_CONTRAST_RATIO, 21.0);

    // Explicit T05 acceptance criterion: `contrast(#000,#fff) = 21`.
    assert_eq!(contrast_hex("#000", "#fff").expect("#000 vs #fff"), 21.0);
    assert_eq!(contrast_hex("#fff", "#000").expect("#fff vs #000"), 21.0);
    assert_eq!(
        contrast_hex("#000000", "#ffffff").expect("#000000 vs #ffffff"),
        21.0
    );
    assert_eq!(contrast_ratio(Rgb8::BLACK, Rgb8::WHITE), 21.0);
    assert_eq!(contrast_ratio(Rgb8::WHITE, Rgb8::BLACK), 21.0);

    // Self-contrast is always 1:1.
    for hex in ["#000000", "#ffffff", "#767676", "#14161b", "#e6e8ee"] {
        let ratio = contrast_hex(hex, hex).expect("valid hex");
        assert!(
            (ratio - 1.0).abs() < 1e-12,
            "self contrast for {hex}: {ratio}"
        );
    }

    // Exact sRGB primary relative luminances (Rec. 709 / WCAG 2.2 coefficients).
    assert_eq!(relative_luminance(Rgb8::BLACK), 0.0);
    assert!((relative_luminance(Rgb8::WHITE) - 1.0).abs() < 1e-15);
    assert!((relative_luminance(Rgb8::new(255, 0, 0)) - 0.2126).abs() < 1e-15);
    assert!((relative_luminance(Rgb8::new(0, 255, 0)) - 0.7152).abs() < 1e-15);
    assert!((relative_luminance(Rgb8::new(0, 0, 255)) - 0.0722).abs() < 1e-15);

    // Primary contrast ratios against black & white.
    assert!((contrast_hex("#ff0000", "#000000").unwrap() - 5.252).abs() < 1e-12);
    assert!((contrast_hex("#00ff00", "#000000").unwrap() - 15.304).abs() < 1e-12);
    assert!((contrast_hex("#0000ff", "#000000").unwrap() - 2.444).abs() < 1e-12);
    assert!((contrast_hex("#0000ff", "#ffffff").unwrap() - (1.05 / 0.1222)).abs() < 1e-12);

    // Famous WCAG 2.2 AA boundary greys against white (#767676 passes 4.5:1; #777777 fails 4.5:1)
    // and AAA boundary (#595959 passes 7.0:1).
    let aa_pass = contrast_hex("#767676", "#ffffff").unwrap();
    let aa_fail = contrast_hex("#777777", "#ffffff").unwrap();
    let aaa_pass = contrast_hex("#595959", "#ffffff").unwrap();
    assert!(
        (4.5..4.6).contains(&aa_pass),
        "expected ~4.54, got {aa_pass}"
    );
    assert!(
        (4.4..4.5).contains(&aa_fail),
        "expected ~4.48, got {aa_fail}"
    );
    assert!(
        (7.0..7.1).contains(&aaa_pass),
        "expected ~7.00, got {aaa_pass}"
    );

    // Model TOML contrast thresholds load from data/model.toml.
    let thresholds = ContrastThresholds::from_model_toml(sk_core::solar::DEFAULT_MODEL_TOML)
        .expect("parse [contrast] from data/model.toml");
    assert_eq!(thresholds, ContrastThresholds::default());
    assert!(thresholds.meets_fg_bg(Rgb8::from_hex("#767676").unwrap(), Rgb8::WHITE));
    assert!(!thresholds.meets_fg_bg(Rgb8::from_hex("#777777").unwrap(), Rgb8::WHITE));
    assert!(thresholds.meets_ansi_bg(Rgb8::from_hex("#ff0000").unwrap(), Rgb8::WHITE));
    assert!(!thresholds.meets_ansi_bg(Rgb8::from_hex("#0000ff").unwrap(), Rgb8::BLACK));

    assert!(matches!(
        (ContrastThresholds {
            fg_bg_min_ratio: 0.9,
            ansi_bg_min_ratio: 3.0,
        })
        .validate(),
        Err(ColorError::InvalidContrastThresholds(_))
    ));
    assert!(matches!(
        (ContrastThresholds {
            fg_bg_min_ratio: 4.5,
            ansi_bg_min_ratio: 22.0,
        })
        .validate(),
        Err(ColorError::InvalidContrastThresholds(_))
    ));
}

#[test]
fn hex_parsing_formatting_and_serde_roundtrip() {
    assert_eq!(Rgb8::from_hex("#000").unwrap(), Rgb8::BLACK);
    assert_eq!(Rgb8::from_hex("#fff").unwrap(), Rgb8::WHITE);
    assert_eq!(Rgb8::from_hex("#FFF").unwrap(), Rgb8::WHITE);
    assert_eq!(
        Rgb8::from_hex("#1a2b3c").unwrap(),
        Rgb8::new(0x1a, 0x2b, 0x3c)
    );
    assert_eq!(
        Rgb8::from_hex("#1A2B3C").unwrap(),
        Rgb8::new(0x1a, 0x2b, 0x3c)
    );
    assert_eq!(Rgb8::from_hex("#abc").unwrap(), Rgb8::new(0xaa, 0xbb, 0xcc));
    assert_eq!(
        Rgb8::from_hex_or_bare("14161b").unwrap(),
        Rgb8::new(0x14, 0x16, 0x1b)
    );
    assert_eq!(
        Rgb8::from_hex_or_bare("#14161b").unwrap(),
        Rgb8::new(0x14, 0x16, 0x1b)
    );

    let c = Rgb8::new(0x14, 0x16, 0x1b);
    assert_eq!(c.to_hex(), "#14161b");
    assert_eq!(c.to_string(), "#14161b");
    assert_eq!(c.to_hex().parse::<Rgb8>().unwrap(), c);

    let json = serde_json::to_string(&c).expect("serialize Rgb8");
    assert_eq!(json, "\"#14161b\"");
    let decoded: Rgb8 = serde_json::from_str(&json).expect("deserialize Rgb8");
    assert_eq!(decoded, c);

    for invalid in [
        "", "#", "#1", "#12", "#1234", "#12345", "#1234567", "14161b", "#gggggg",
    ] {
        assert!(
            matches!(Rgb8::from_hex(invalid), Err(ColorError::InvalidHex(_))),
            "expected InvalidHex for '{invalid}'"
        );
    }
}

#[test]
fn oklab_oklch_linear_srgb_and_delta_e_match_palette_oracle_across_srgb_cube() {
    // Sweep a uniform 17x17x17 = 4,913-point grid across the entire 3-D sRGB cube (0, 15, ..., 255),
    // plus piecewise threshold bytes around 0.04045 * 255 (~10.31).
    let mut levels: Vec<u8> = (0..=17).map(|i| (i * 15) as u8).collect();
    levels.extend([1, 9, 10, 11, 12, 254]);
    levels.sort_unstable();
    levels.dedup();

    let anchor_dark = Rgb8::new(0x14, 0x16, 0x1b);
    let oracle_anchor = oracle_oklab_from_rgb8(anchor_dark);
    let our_anchor = anchor_dark.to_oklab();

    for &r in &levels {
        for &g in &levels {
            for &b in &levels {
                let rgb = Rgb8::new(r, g, b);

                // 1. Linear sRGB vs `palette::LinSrgb<f64>` (note IEC 61966-2-1 / WCAG 2.2 uses
                // `0.04045` whereas `palette` uses rational `0.040449936`, agreeing within < 3e-9).
                let our_lin = rgb.to_linear_srgb();
                let ora_lin = oracle_lin_from_rgb8(rgb);
                assert!(
                    (our_lin.r - ora_lin.red).abs() < 1e-8
                        && (our_lin.g - ora_lin.green).abs() < 1e-8
                        && (our_lin.b - ora_lin.blue).abs() < 1e-8,
                    "LinearSrgb mismatch at {rgb}: ours={our_lin:?}, oracle={ora_lin:?}"
                );

                // 2. OKLab vs `palette::Oklab<f64>`
                let our_lab = rgb.to_oklab();
                let ora_lab = oracle_oklab_from_rgb8(rgb);
                assert!(
                    (our_lab.l - ora_lab.l).abs() < 1e-6
                        && (our_lab.a - ora_lab.a).abs() < 1e-6
                        && (our_lab.b - ora_lab.b).abs() < 1e-6,
                    "Oklab mismatch at {rgb}: ours={our_lab:?}, oracle={ora_lab:?}"
                );

                // 3. OKLCH vs `palette::Oklch<f64>`
                let our_lch = rgb.to_oklch();
                let ora_lch = OracleOklch::from_color(ora_lab);
                assert!(
                    (our_lch.l - ora_lch.l).abs() < 1e-6
                        && (our_lch.c - ora_lch.chroma).abs() < 1e-6,
                    "Oklch (L, C) mismatch at {rgb}: ours={our_lch:?}, oracle={ora_lch:?}"
                );
                if our_lch.c >= 1e-3 {
                    let ora_hue = ora_lch.hue.into_positive_degrees();
                    let hue_err = shortest_hue_delta_deg(our_lch.h_deg, ora_hue).abs();
                    assert!(
                        hue_err < 1e-3,
                        "Oklch hue mismatch at {rgb}: ours={}, oracle={ora_hue}, err={hue_err}",
                        our_lch.h_deg
                    );
                }

                // 4. `ΔE_OK` vs `palette::Oklab::distance`
                let our_de = delta_e_ok(our_lab, our_anchor);
                let ora_de = ora_lab.distance(oracle_anchor);
                assert!(
                    (our_de - ora_de).abs() < 1e-6,
                    "delta_e_ok mismatch at {rgb}: ours={our_de}, oracle={ora_de}"
                );

                // 5. Full round-trip `Rgb8 -> Oklch -> Oklab -> LinearSrgb -> Srgb -> Rgb8`
                let roundtripped = our_lch.to_oklab().to_linear_srgb().to_srgb().to_rgb8();
                assert_eq!(roundtripped, rgb, "exact 8-bit round-trip failed for {rgb}");
                assert_eq!(
                    our_lch.to_rgb8_gamut_mapped(),
                    rgb,
                    "gamut-mapped 8-bit round-trip failed for in-gamut {rgb}"
                );
            }
        }
    }
}

#[test]
fn gamut_mapping_lowers_chroma_at_constant_l_and_h_onto_srgb_boundary() {
    // Extreme out-of-gamut colours across 12 hue angles and multiple lightness levels.
    for l in [0.15, 0.35, 0.55, 0.75, 0.92] {
        for hue_step in 0..12 {
            let h_deg = f64::from(hue_step) * 30.0;
            let over_saturated = Oklch::new(l, 0.45, h_deg);
            assert!(!over_saturated.is_in_srgb_gamut());

            let mapped = gamut_map_oklch(over_saturated);
            assert_eq!(mapped.l, l, "gamut mapping must keep L constant");
            assert_eq!(mapped.h_deg, h_deg, "gamut mapping must keep h constant");
            assert!(
                mapped.c > 0.0 && mapped.c < over_saturated.c,
                "expected 0 < mapped.c < 0.45 at (L={l}, h={h_deg}), got {}",
                mapped.c
            );
            assert!(
                mapped.is_in_srgb_gamut(),
                "gamut-mapped colour must lie inside sRGB gamut: {:?}",
                mapped.to_linear_srgb()
            );

            // Stepping slightly past `mapped.c` must exit the strict `[0, 1]^3` sRGB cube.
            let nudged_out = Oklch::new(l, mapped.c + 1e-4, h_deg);
            assert!(
                !nudged_out.to_linear_srgb().is_in_gamut(),
                "mapped chroma {} at (L={l}, h={h_deg}) was not on the sRGB gamut boundary",
                mapped.c
            );

            let max_c = max_in_gamut_chroma(l, h_deg);
            assert!((mapped.c - max_c).abs() < 1e-9);
        }
    }

    // Lightness poles collapse cleanly to pure black and pure white.
    let black = gamut_map_oklch(Oklch::new(0.0, 0.3, 145.0));
    assert_eq!(black.l, 0.0);
    assert_eq!(black.c, 0.0);
    assert_eq!(black.to_rgb8_gamut_mapped(), Rgb8::BLACK);

    let below_black = gamut_map_oklch(Oklch::new(-0.2, 0.3, 145.0));
    assert_eq!(below_black.to_rgb8_gamut_mapped(), Rgb8::BLACK);

    let white = gamut_map_oklch(Oklch::new(1.0, 0.3, 40.0));
    assert_eq!(white.l, 1.0);
    assert_eq!(white.c, 0.0);
    assert_eq!(white.to_rgb8_gamut_mapped(), Rgb8::WHITE);

    let above_white = gamut_map_oklch(Oklch::new(1.2, 0.3, 40.0));
    assert_eq!(above_white.to_rgb8_gamut_mapped(), Rgb8::WHITE);
}

#[test]
fn hue_angle_normalization_and_shortest_arc_handle_wraparound() {
    assert_eq!(normalize_hue_deg(0.0), 0.0);
    assert_eq!(normalize_hue_deg(360.0), 0.0);
    assert_eq!(normalize_hue_deg(-30.0), 330.0);
    assert_eq!(normalize_hue_deg(725.0), 5.0);

    assert!((shortest_hue_delta_deg(350.0, 10.0) - 20.0).abs() < 1e-12);
    assert!((shortest_hue_delta_deg(10.0, 350.0) + 20.0).abs() < 1e-12);
    assert!((shortest_hue_delta_deg(90.0, 270.0).abs() - 180.0).abs() < 1e-12);
}

#[test]
fn color_math_summary_matches_golden_snapshot_and_is_deterministic_100x() {
    let build_report = || {
        let mut lines = Vec::new();
        for hex in [
            "#000000", "#ffffff", "#ff0000", "#00ff00", "#0000ff", "#14161b", "#e6e8ee", "#767676",
        ] {
            let rgb = Rgb8::from_hex(hex).expect("valid hex");
            let lab = rgb.to_oklab();
            let lch = rgb.to_oklch();
            lines.push(format!(
                "{} -> Y={:.6} OKLab=({:.6},{:.6},{:.6}) OKLCH=({:.6},{:.6},{:.3}) vs_black={:.3} vs_white={:.3}",
                rgb.to_hex(),
                rgb.relative_luminance(),
                lab.l,
                lab.a,
                lab.b,
                lch.l,
                lch.c,
                lch.h_deg,
                rgb.contrast_ratio(Rgb8::BLACK),
                rgb.contrast_ratio(Rgb8::WHITE),
            ));
        }

        for (l, c, h) in [(0.65, 0.35, 30.0), (0.75, 0.35, 145.0), (0.50, 0.35, 265.0)] {
            let raw = Oklch::new(l, c, h);
            let mapped = raw.gamut_map_srgb();
            lines.push(format!(
                "gamut_map OKLCH({:.2},{:.2},{:.1}) -> C={:.6} hex={} dE={:.6}",
                l,
                c,
                h,
                mapped.c,
                mapped.to_rgb8_gamut_mapped(),
                delta_e_ok(raw, mapped),
            ));
        }
        lines.join("\n")
    };

    let baseline = build_report();
    for _ in 0..100 {
        assert_eq!(build_report(), baseline);
    }
    insta::assert_snapshot!("color_math_reference_summary", baseline);
}

proptest! {
    #[test]
    fn srgb_transfer_and_oklab_oklch_roundtrip_and_match_oracle(
        r in 0u8..=255u8,
        g in 0u8..=255u8,
        b in 0u8..=255u8,
        u in 0.0f64..=1.0f64,
    ) {
        // sRGB piecewise transfer round-trip on [0, 1].
        let lin_u = srgb_channel_to_linear(u);
        prop_assert!((0.0..=1.0).contains(&lin_u));
        prop_assert!((linear_channel_to_srgb(lin_u) - u).abs() < 1e-11);

        // Full 8-bit round-trip & oracle agreement.
        let rgb = Rgb8::new(r, g, b);
        let lch = rgb.to_oklch();
        let back = lch.to_oklab().to_linear_srgb().to_srgb().to_rgb8();
        prop_assert_eq!(back, rgb);
        prop_assert_eq!(Rgb8::from_hex(&rgb.to_hex()).unwrap(), rgb);

        let ora = oracle_oklab_from_rgb8(rgb);
        let ours = rgb.to_oklab();
        prop_assert!((ours.l - ora.l).abs() < 1e-6);
        prop_assert!((ours.a - ora.a).abs() < 1e-6);
        prop_assert!((ours.b - ora.b).abs() < 1e-6);
    }

    #[test]
    fn continuous_srgb_oklab_oklch_roundtrips_and_matches_oracle(
        rf in 0.0f64..=1.0f64,
        gf in 0.0f64..=1.0f64,
        bf in 0.0f64..=1.0f64,
    ) {
        let srgb = Srgb::new(rf, gf, bf);
        let lin = srgb.to_linear_srgb();
        let lab = lin.to_oklab();
        let lch = lab.to_oklch();

        let lab_back = lch.to_oklab();
        prop_assert!((lab_back.l - lab.l).abs() < 2e-7);
        prop_assert!((lab_back.a - lab.a).abs() < 2e-7);
        prop_assert!((lab_back.b - lab.b).abs() < 2e-7);

        let lin_back = lab_back.to_linear_srgb();
        prop_assert!((lin_back.r - lin.r).abs() < 5e-7);
        prop_assert!((lin_back.g - lin.g).abs() < 5e-7);
        prop_assert!((lin_back.b - lin.b).abs() < 5e-7);

        let srgb_back = lin_back.to_srgb();
        prop_assert!((srgb_back.r - srgb.r).abs() < 5e-6);
        prop_assert!((srgb_back.g - srgb.g).abs() < 5e-6);
        prop_assert!((srgb_back.b - srgb.b).abs() < 5e-6);

        let ora_lab = OracleOklab::from_color(OracleLinSrgb::new(lin.r, lin.g, lin.b));
        prop_assert!((lab.l - ora_lab.l).abs() < 1e-6);
        prop_assert!((lab.a - ora_lab.a).abs() < 1e-6);
        prop_assert!((lab.b - ora_lab.b).abs() < 1e-6);
    }

    #[test]
    fn gamut_map_preserves_l_and_h_lowers_c_and_is_idempotent(
        l in 0.001f64..=0.999f64,
        c in 0.0f64..=0.5f64,
        h in -720.0f64..=720.0f64,
    ) {
        let input = Oklch::new(l, c, h);
        let mapped = gamut_map_oklch(input);

        prop_assert_eq!(mapped.l, input.l);
        prop_assert_eq!(mapped.h_deg, input.h_deg);
        prop_assert!(mapped.c >= 0.0 && mapped.c <= input.c + 1e-15);
        prop_assert!(mapped.is_in_srgb_gamut());

        let twice = gamut_map_oklch(mapped);
        prop_assert!((twice.c - mapped.c).abs() < 1e-12);
    }

    #[test]
    fn delta_e_ok_and_wcag_contrast_satisfy_metric_and_range_invariants(
        r1 in 0u8..=255u8, g1 in 0u8..=255u8, b1 in 0u8..=255u8,
        r2 in 0u8..=255u8, g2 in 0u8..=255u8, b2 in 0u8..=255u8,
        r3 in 0u8..=255u8, g3 in 0u8..=255u8, b3 in 0u8..=255u8,
    ) {
        let c1 = Rgb8::new(r1, g1, b1);
        let c2 = Rgb8::new(r2, g2, b2);
        let c3 = Rgb8::new(r3, g3, b3);

        // WCAG contrast invariants: symmetric, self == 1.0, bounded in [1.0, 21.0].
        let cr12 = contrast_ratio(c1, c2);
        let cr21 = contrast_ratio(c2, c1);
        prop_assert!((cr12 - cr21).abs() < 1e-12);
        prop_assert!((1.0..=21.0).contains(&cr12));
        prop_assert!((contrast_ratio(c1, c1) - 1.0).abs() < 1e-12);

        // ΔE_OK metric space axioms: identity, symmetry, non-negativity, triangle inequality.
        let d11 = delta_e_ok(c1, c1);
        let d12 = delta_e_ok(c1, c2);
        let d21 = delta_e_ok(c2, c1);
        let d23 = delta_e_ok(c2, c3);
        let d13 = delta_e_ok(c1, c3);

        prop_assert_eq!(d11, 0.0);
        prop_assert!(d12 >= 0.0);
        prop_assert!((d12 - d21).abs() < 1e-15);
        prop_assert!(d13 <= d12 + d23 + 1e-12);
    }
}
