//! Pure in-house sRGB, OKLab/OKLCH, constant-(L, h) sRGB gamut mapping, WCAG 2.2 contrast, and
//! perceptual `ΔE_OK` math (`PLAN.md` §0 #3, §0 #5, §3, §4, T05).
//!
//! References:
//! - Björn Ottosson, *A perceptual color space for image processing* (2020-12-23):
//!   <https://bottosson.github.io/posts/oklab/>
//! - W3C WCAG 2.2 Relative Luminance & Contrast (Minimum) (SC 1.4.3 / SC 1.4.11):
//!   <https://www.w3.org/TR/WCAG22/#contrast-minimum>
//! - IEC 61966-2-1:1999 sRGB colour space transfer function

use core::fmt;
use core::str::FromStr;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub const WCAG_AA_NORMAL_TEXT_RATIO: f64 = 4.5;
pub const WCAG_AA_NON_TEXT_RATIO: f64 = 3.0;
pub const WCAG_AAA_NORMAL_TEXT_RATIO: f64 = 7.0;
pub const WCAG_MAX_CONTRAST_RATIO: f64 = 21.0;

/// Tolerance accommodating 10-digit OKLab matrix rounding on exact sRGB cube boundary vertices
/// (`~4e-8`), staying >300x smaller than a single 8-bit sRGB step near black (`~3.03e-4`).
const GAMUT_EPSILON: f64 = 1e-6;

/// Chroma floor below which `(a, b)` is purely 10-digit matrix rounding noise on achromatic greys
/// (`~3.7e-8 * L`, >20,000x smaller than the least-saturated non-grey 8-bit sRGB step `#fefeff`).
const ACHROMATIC_CHROMA_EPSILON: f64 = 1e-7;

/// Upper bound on sRGB chroma in OKLCH (`max` occurs near pure blue `#0000ff` at `C ≈ 0.313`).
const MAX_SRGB_CHROMA_SEARCH_UPPER: f64 = 0.5;

/// Bisection iterations for constant-`(L, h)` chroma gamut mapping (`0.5 * 2^-52 ≈ 1.1e-16`).
const GAMUT_BISECTION_STEPS: usize = 52;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ColorError {
    #[error("invalid hex colour '{0}': expected `#RGB` or `#RRGGBB`")]
    InvalidHex(String),
    #[error("colour component `{0}` must be finite")]
    NonFiniteComponent(&'static str),
    #[error("invalid contrast threshold configuration: {0}")]
    InvalidContrastThresholds(&'static str),
    #[error("failed to parse model configuration: {0}")]
    ModelParse(String),
}

/// Tunable WCAG 2.2 contrast thresholds loaded from `data/model.toml` (`PLAN.md` §0 #9 & §4).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContrastThresholds {
    pub fg_bg_min_ratio: f64,
    pub ansi_bg_min_ratio: f64,
}

#[derive(Deserialize)]
struct ModelTomlContrastRoot {
    contrast: ContrastThresholds,
}

impl Default for ContrastThresholds {
    fn default() -> Self {
        Self {
            fg_bg_min_ratio: WCAG_AA_NORMAL_TEXT_RATIO,
            ansi_bg_min_ratio: WCAG_AA_NON_TEXT_RATIO,
        }
    }
}

impl ContrastThresholds {
    pub fn validate(&self) -> Result<(), ColorError> {
        for ratio in [self.fg_bg_min_ratio, self.ansi_bg_min_ratio] {
            if !ratio.is_finite() || !(1.0..=WCAG_MAX_CONTRAST_RATIO).contains(&ratio) {
                return Err(ColorError::InvalidContrastThresholds(
                    "contrast ratios must be finite and within [1.0, 21.0]",
                ));
            }
        }
        Ok(())
    }

    pub fn from_model_toml(toml_str: &str) -> Result<Self, ColorError> {
        let root: ModelTomlContrastRoot =
            toml::from_str(toml_str).map_err(|err| ColorError::ModelParse(err.to_string()))?;
        root.contrast.validate()?;
        Ok(root.contrast)
    }

    #[must_use]
    pub fn meets_fg_bg(&self, a: Rgb8, b: Rgb8) -> bool {
        a.contrast_ratio(b) >= self.fg_bg_min_ratio
    }

    #[must_use]
    pub fn meets_ansi_bg(&self, ansi: Rgb8, bg: Rgb8) -> bool {
        ansi.contrast_ratio(bg) >= self.ansi_bg_min_ratio
    }
}

fn parse_hex_digits(raw_input: &str, digits: &str) -> Result<Rgb8, ColorError> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ColorError::InvalidHex(raw_input.to_string()));
    }

    match digits.len() {
        3 => {
            let r = u8::from_str_radix(&digits[0..1], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?
                * 17;
            let g = u8::from_str_radix(&digits[1..2], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?
                * 17;
            let b = u8::from_str_radix(&digits[2..3], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?
                * 17;
            Ok(Rgb8::new(r, g, b))
        }
        6 => {
            let r = u8::from_str_radix(&digits[0..2], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?;
            let g = u8::from_str_radix(&digits[2..4], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?;
            let b = u8::from_str_radix(&digits[4..6], 16)
                .map_err(|_| ColorError::InvalidHex(raw_input.to_string()))?;
            Ok(Rgb8::new(r, g, b))
        }
        _ => Err(ColorError::InvalidHex(raw_input.to_string())),
    }
}

/// 8-bit gamma-encoded sRGB colour (`#RRGGBB`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb8 {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#RGB` or `#RRGGBB` (case-insensitive, `#` prefix required).
    pub fn from_hex(hex: &str) -> Result<Self, ColorError> {
        let trimmed = hex.trim();
        let Some(digits) = trimmed.strip_prefix('#') else {
            return Err(ColorError::InvalidHex(hex.to_string()));
        };
        parse_hex_digits(hex, digits)
    }

    /// Parses `#RGB`, `#RRGGBB`, `RGB`, or `RRGGBB` (for theme formats that omit `#`).
    pub fn from_hex_or_bare(hex: &str) -> Result<Self, ColorError> {
        let trimmed = hex.trim();
        let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
        parse_hex_digits(hex, digits)
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    #[must_use]
    pub fn to_srgb(self) -> Srgb {
        Srgb {
            r: f64::from(self.r) / 255.0,
            g: f64::from(self.g) / 255.0,
            b: f64::from(self.b) / 255.0,
        }
    }

    #[must_use]
    pub fn to_linear_srgb(self) -> LinearSrgb {
        self.to_srgb().to_linear_srgb()
    }

    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        self.to_linear_srgb().to_oklab()
    }

    #[must_use]
    pub fn to_oklch(self) -> Oklch {
        self.to_oklab().to_oklch()
    }

    #[must_use]
    pub fn relative_luminance(self) -> f64 {
        self.to_linear_srgb().relative_luminance()
    }

    #[must_use]
    pub fn contrast_ratio(self, other: Self) -> f64 {
        contrast_ratio(self, other)
    }
}

impl fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Rgb8 {
    type Err = ColorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl Serialize for Rgb8 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb8 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_hex(&raw).map_err(de::Error::custom)
    }
}

/// Normalized gamma-encoded sRGB colour (in-gamut channels lie in `[0.0, 1.0]`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Srgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Srgb {
    #[must_use]
    pub const fn new(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub fn is_in_gamut(self) -> bool {
        (0.0..=1.0).contains(&self.r)
            && (0.0..=1.0).contains(&self.g)
            && (0.0..=1.0).contains(&self.b)
    }

    #[must_use]
    pub fn clamp_unit(self) -> Self {
        Self {
            r: self.r.clamp(0.0, 1.0),
            g: self.g.clamp(0.0, 1.0),
            b: self.b.clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub fn to_rgb8(self) -> Rgb8 {
        let clamped = self.clamp_unit();
        Rgb8 {
            r: (clamped.r * 255.0).round() as u8,
            g: (clamped.g * 255.0).round() as u8,
            b: (clamped.b * 255.0).round() as u8,
        }
    }

    #[must_use]
    pub fn to_linear_srgb(self) -> LinearSrgb {
        LinearSrgb {
            r: srgb_channel_to_linear(self.r),
            g: srgb_channel_to_linear(self.g),
            b: srgb_channel_to_linear(self.b),
        }
    }

    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        self.to_linear_srgb().to_oklab()
    }

    #[must_use]
    pub fn to_oklch(self) -> Oklch {
        self.to_oklab().to_oklch()
    }
}

/// Linear-light sRGB colour (in-gamut channels lie in `[0.0, 1.0]`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LinearSrgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl LinearSrgb {
    #[must_use]
    pub const fn new(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub fn is_in_gamut(self) -> bool {
        (0.0..=1.0).contains(&self.r)
            && (0.0..=1.0).contains(&self.g)
            && (0.0..=1.0).contains(&self.b)
    }

    #[must_use]
    pub fn is_in_gamut_with_epsilon(self, eps: f64) -> bool {
        let lo = -eps;
        let hi = 1.0 + eps;
        (lo..=hi).contains(&self.r) && (lo..=hi).contains(&self.g) && (lo..=hi).contains(&self.b)
    }

    #[must_use]
    pub fn clamp_unit(self) -> Self {
        Self {
            r: self.r.clamp(0.0, 1.0),
            g: self.g.clamp(0.0, 1.0),
            b: self.b.clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub fn to_srgb(self) -> Srgb {
        Srgb {
            r: linear_channel_to_srgb(self.r),
            g: linear_channel_to_srgb(self.g),
            b: linear_channel_to_srgb(self.b),
        }
    }

    /// Converts linear-light sRGB to Björn Ottosson's OKLab colour space.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let l = 0.4122214708 * self.r + 0.5363325363 * self.g + 0.0514459929 * self.b;
        let m = 0.2119034982 * self.r + 0.6806995451 * self.g + 0.1073969566 * self.b;
        let s = 0.0883024619 * self.r + 0.2817188376 * self.g + 0.6299787005 * self.b;

        let l_ = l.cbrt();
        let m_ = m.cbrt();
        let s_ = s.cbrt();

        Oklab {
            l: 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
            a: 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
            b: 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
        }
    }

    /// WCAG 2.2 / Rec. 709 relative luminance `Y = 0.2126 R + 0.7152 G + 0.0722 B`.
    #[must_use]
    pub fn relative_luminance(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

/// Cartesian perceptual OKLab colour (`L` lightness, `a` green-red, `b` blue-yellow).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Oklab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl Oklab {
    #[must_use]
    pub const fn new(l: f64, a: f64, b: f64) -> Self {
        Self { l, a, b }
    }

    /// Converts OKLab to linear-light sRGB using Björn Ottosson's inverse matrices.
    #[must_use]
    pub fn to_linear_srgb(self) -> LinearSrgb {
        let l_ = self.l + 0.3963377774 * self.a + 0.2158037573 * self.b;
        let m_ = self.l - 0.1055613458 * self.a - 0.0638541728 * self.b;
        let s_ = self.l - 0.0894841775 * self.a - 1.2914855480 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        LinearSrgb {
            r: 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
            g: -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
            b: -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
        }
    }

    #[must_use]
    pub fn to_oklch(self) -> Oklch {
        let c = self.a.hypot(self.b);
        let h_deg = if c < ACHROMATIC_CHROMA_EPSILON {
            0.0
        } else {
            normalize_hue_deg(self.b.atan2(self.a).to_degrees())
        };
        Oklch {
            l: self.l,
            c,
            h_deg,
        }
    }

    /// Perceptual Euclidean colour difference `ΔE_OK` in OKLab space.
    #[must_use]
    pub fn delta_e(self, other: Self) -> f64 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        (dl * dl + da * da + db * db).sqrt()
    }

    #[must_use]
    pub fn is_in_srgb_gamut(self) -> bool {
        (0.0..=1.0).contains(&self.l)
            && self
                .to_linear_srgb()
                .is_in_gamut_with_epsilon(GAMUT_EPSILON)
    }

    #[must_use]
    pub fn gamut_map_srgb(self) -> Self {
        self.to_oklch().gamut_map_srgb().to_oklab()
    }

    #[must_use]
    pub fn to_rgb8_gamut_mapped(self) -> Rgb8 {
        self.to_oklch().to_rgb8_gamut_mapped()
    }
}

/// Cylindrical perceptual OKLCH colour (`L` lightness, `C >= 0` chroma, `h_deg` hue in `[0, 360)`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h_deg: f64,
}

impl Oklch {
    #[must_use]
    pub fn new(l: f64, c: f64, h_deg: f64) -> Self {
        Self {
            l,
            c: if c <= 0.0 { 0.0 } else { c },
            h_deg: normalize_hue_deg(h_deg),
        }
    }

    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let c = if self.c <= 0.0 { 0.0 } else { self.c };
        let h_rad = self.h_deg.to_radians();
        let (sin_h, cos_h) = h_rad.sin_cos();
        Oklab {
            l: self.l,
            a: c * cos_h,
            b: c * sin_h,
        }
    }

    #[must_use]
    pub fn to_linear_srgb(self) -> LinearSrgb {
        self.to_oklab().to_linear_srgb()
    }

    #[must_use]
    pub fn is_in_srgb_gamut(self) -> bool {
        (0.0..=1.0).contains(&self.l)
            && self
                .to_linear_srgb()
                .is_in_gamut_with_epsilon(GAMUT_EPSILON)
    }

    /// Lowers chroma `C` at constant `L` and `h` until the colour lies inside the sRGB gamut
    /// (`PLAN.md` §4).
    #[must_use]
    pub fn gamut_map_srgb(self) -> Self {
        gamut_map_oklch(self)
    }

    #[must_use]
    pub fn to_srgb_gamut_mapped(self) -> Srgb {
        self.gamut_map_srgb()
            .to_linear_srgb()
            .clamp_unit()
            .to_srgb()
    }

    #[must_use]
    pub fn to_rgb8_gamut_mapped(self) -> Rgb8 {
        self.to_srgb_gamut_mapped().to_rgb8()
    }
}

/// Converts a single gamma-encoded sRGB channel (`0.0..=1.0`, sign-extended outside `[0, 1]`)
/// to linear light using IEC 61966-2-1 / WCAG 2.2 (`0.04045` threshold).
#[must_use]
pub fn srgb_channel_to_linear(c: f64) -> f64 {
    let sign = c.signum();
    let abs = c.abs();
    let lin = if abs <= 0.04045 {
        abs / 12.92
    } else {
        ((abs + 0.055) / 1.055).powf(2.4)
    };
    let signed = sign * lin;
    if signed == 0.0 {
        0.0
    } else {
        signed
    }
}

/// Converts a single linear-light sRGB channel to gamma-encoded sRGB (`0.0031308` threshold).
#[must_use]
pub fn linear_channel_to_srgb(c: f64) -> f64 {
    let sign = c.signum();
    let abs = c.abs();
    let srgb = if abs <= 0.0031308 {
        12.92 * abs
    } else {
        1.055 * abs.powf(1.0 / 2.4) - 0.055
    };
    let signed = sign * srgb;
    if signed == 0.0 {
        0.0
    } else {
        signed
    }
}

/// Normalizes a hue angle in degrees into `[0.0, 360.0)`.
#[must_use]
pub fn normalize_hue_deg(h_deg: f64) -> f64 {
    let wrapped = h_deg.rem_euclid(360.0);
    if wrapped >= 360.0 || wrapped == 0.0 {
        0.0
    } else {
        wrapped
    }
}

/// Returns the signed shortest-arc hue difference `to_deg - from_deg` in `(-180.0, 180.0]`.
#[must_use]
pub fn shortest_hue_delta_deg(from_deg: f64, to_deg: f64) -> f64 {
    let mut delta = (to_deg - from_deg).rem_euclid(360.0);
    if delta > 180.0 {
        delta -= 360.0;
    }
    if delta == 0.0 {
        0.0
    } else {
        delta
    }
}

fn bisect_chroma_to_srgb_boundary(l: f64, h_deg: f64, upper_c: f64) -> f64 {
    let mut lo = 0.0_f64;
    let mut hi = upper_c;
    for _ in 0..GAMUT_BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        let candidate = Oklch { l, c: mid, h_deg };
        if candidate.to_linear_srgb().is_in_gamut() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Returns the maximum chroma `C >= 0` at `(l, h_deg)` that lies strictly within the sRGB cube.
#[must_use]
pub fn max_in_gamut_chroma(l: f64, h_deg: f64) -> f64 {
    if !(0.0..=1.0).contains(&l) || l == 0.0 || l == 1.0 {
        return 0.0;
    }
    bisect_chroma_to_srgb_boundary(l, normalize_hue_deg(h_deg), MAX_SRGB_CHROMA_SEARCH_UPPER)
}

/// Maps `color` into the sRGB gamut by lowering chroma `C` at constant lightness `L` and hue `h`
/// (`PLAN.md` §4).
#[must_use]
pub fn gamut_map_oklch(color: Oklch) -> Oklch {
    let h_deg = normalize_hue_deg(color.h_deg);
    if color.l <= 0.0 {
        return Oklch {
            l: 0.0,
            c: 0.0,
            h_deg,
        };
    }
    if color.l >= 1.0 {
        return Oklch {
            l: 1.0,
            c: 0.0,
            h_deg,
        };
    }

    let c = if color.c <= 0.0 { 0.0 } else { color.c };
    let candidate = Oklch {
        l: color.l,
        c,
        h_deg,
    };
    if candidate.is_in_srgb_gamut() {
        return candidate;
    }

    let mapped_c = bisect_chroma_to_srgb_boundary(color.l, h_deg, c);
    Oklch {
        l: color.l,
        c: mapped_c,
        h_deg,
    }
}

/// Perceptual Euclidean colour difference `ΔE_OK` between any two convertible colours.
#[must_use]
pub fn delta_e_ok(a: impl Into<Oklab>, b: impl Into<Oklab>) -> f64 {
    a.into().delta_e(b.into())
}

/// WCAG 2.2 relative luminance `Y` (`0.0` for `#000000`, `1.0` for `#ffffff`).
#[must_use]
pub fn relative_luminance(color: impl Into<Srgb>) -> f64 {
    color
        .into()
        .clamp_unit()
        .to_linear_srgb()
        .relative_luminance()
        .clamp(0.0, 1.0)
}

/// WCAG 2.2 contrast ratio `(L_lighter + 0.05) / (L_darker + 0.05)` in `[1.0, 21.0]`.
#[must_use]
pub fn contrast_ratio(a: impl Into<Srgb>, b: impl Into<Srgb>) -> f64 {
    let y1 = relative_luminance(a);
    let y2 = relative_luminance(b);
    let lighter = y1.max(y2);
    let darker = y1.min(y2);
    (lighter + 0.05) / (darker + 0.05)
}

/// Parses two `#RGB` or `#RRGGBB` hex strings and returns their WCAG 2.2 contrast ratio.
pub fn contrast_hex(hex_a: &str, hex_b: &str) -> Result<f64, ColorError> {
    let a = Rgb8::from_hex(hex_a)?;
    let b = Rgb8::from_hex(hex_b)?;
    Ok(contrast_ratio(a, b))
}

impl From<Rgb8> for Srgb {
    fn from(c: Rgb8) -> Self {
        c.to_srgb()
    }
}

impl From<Rgb8> for LinearSrgb {
    fn from(c: Rgb8) -> Self {
        c.to_linear_srgb()
    }
}

impl From<Rgb8> for Oklab {
    fn from(c: Rgb8) -> Self {
        c.to_oklab()
    }
}

impl From<Rgb8> for Oklch {
    fn from(c: Rgb8) -> Self {
        c.to_oklch()
    }
}

impl From<Srgb> for LinearSrgb {
    fn from(c: Srgb) -> Self {
        c.to_linear_srgb()
    }
}

impl From<LinearSrgb> for Srgb {
    fn from(c: LinearSrgb) -> Self {
        c.to_srgb()
    }
}

impl From<LinearSrgb> for Oklab {
    fn from(c: LinearSrgb) -> Self {
        c.to_oklab()
    }
}

impl From<Oklab> for LinearSrgb {
    fn from(c: Oklab) -> Self {
        c.to_linear_srgb()
    }
}

impl From<Srgb> for Oklab {
    fn from(c: Srgb) -> Self {
        c.to_oklab()
    }
}

impl From<Srgb> for Oklch {
    fn from(c: Srgb) -> Self {
        c.to_oklch()
    }
}

impl From<Oklab> for Oklch {
    fn from(c: Oklab) -> Self {
        c.to_oklch()
    }
}

impl From<Oklch> for Oklab {
    fn from(c: Oklch) -> Self {
        c.to_oklab()
    }
}
