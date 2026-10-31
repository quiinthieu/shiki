//! Pure palette schema with file/line/column diagnostics, day phase `d`, 2-D OKLCH bilinear blend,
//! WCAG 2.2 contrast fix-up, bundled `shiki` palette, and year-sweep verification (`PLAN.md` §3, §4, T06).

use crate::color::{
    contrast_ratio, delta_e_ok, normalize_hue_deg, shortest_hue_delta_deg, ColorError,
    ContrastThresholds, Oklab, Oklch, Rgb8,
};
use crate::season::{season_state, Season, SeasonConfig, SeasonError};
use crate::solar::{
    solar_day_index_at, solar_day_summary_for_index, solar_position, Coordinates, PolarState,
    SolarError, SolarEventAngles, DEFAULT_MODEL_TOML,
};
use crate::time::{TimeError, UtcInstant, SECS_PER_MINUTE};
use core::cmp::Ordering;
use core::fmt;
use core::ops::Range;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use toml::Spanned;

/// Embedded bundled `shiki` palette (`data/palettes/shiki.toml`).
pub const BUNDLED_SHIKI_TOML: &str = include_str!("../../../data/palettes/shiki.toml");

const GRID_COORD_SCALE: f64 = 1_000_000.0;
const CHROMA_WEIGHT_EPSILON: f64 = 1e-7;
const FIXUP_BISECTION_STEPS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ModelConfigError {
    #[error(transparent)]
    Solar(#[from] SolarError),
    #[error(transparent)]
    Color(#[from] ColorError),
    #[error(transparent)]
    Season(#[from] SeasonError),
    #[error("invalid day-phase configuration: {0}")]
    InvalidDayPhase(&'static str),
    #[error("failed to parse model configuration: {0}")]
    Parse(String),
}

/// Tunable day-phase `d` keyframes and normalization parameters (`[day_phase]` in `data/model.toml`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayPhaseConfig {
    pub night_d: f64,
    pub nautical_d: f64,
    pub civil_d: f64,
    pub sunrise_sunset_d: f64,
    pub golden_d: f64,
    pub day_d: f64,
    pub day_full_elevation_deg: f64,
    pub culmination_blend_half_width_deg: f64,
}

impl Default for DayPhaseConfig {
    fn default() -> Self {
        Self {
            night_d: 0.0,
            nautical_d: 0.18,
            civil_d: 0.38,
            sunrise_sunset_d: 0.58,
            golden_d: 0.80,
            day_d: 1.0,
            day_full_elevation_deg: 24.0,
            culmination_blend_half_width_deg: 8.0,
        }
    }
}

impl DayPhaseConfig {
    pub fn validate(&self, angles: &SolarEventAngles) -> Result<(), ModelConfigError> {
        let keyframes = [
            self.night_d,
            self.nautical_d,
            self.civil_d,
            self.sunrise_sunset_d,
            self.golden_d,
            self.day_d,
        ];
        if keyframes
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err(ModelConfigError::InvalidDayPhase(
                "all day_phase keyframes must be finite and within [0.0, 1.0]",
            ));
        }
        if (self.night_d - 0.0).abs() > 1e-12 || (self.day_d - 1.0).abs() > 1e-12 {
            return Err(ModelConfigError::InvalidDayPhase(
                "night_d must equal 0.0 and day_d must equal 1.0",
            ));
        }
        if !keyframes.windows(2).all(|w| w[0] < w[1]) {
            return Err(ModelConfigError::InvalidDayPhase(
                "day_phase keyframes must be strictly ascending from night_d to day_d",
            ));
        }
        if !self.day_full_elevation_deg.is_finite()
            || self.day_full_elevation_deg <= angles.golden_hour_deg
            || self.day_full_elevation_deg > 90.0
        {
            return Err(ModelConfigError::InvalidDayPhase(
                "day_full_elevation_deg must be finite and within (golden_hour_deg, 90.0]",
            ));
        }
        if !self.culmination_blend_half_width_deg.is_finite()
            || !(0.0..=45.0).contains(&self.culmination_blend_half_width_deg)
        {
            return Err(ModelConfigError::InvalidDayPhase(
                "culmination_blend_half_width_deg must be finite and within [0.0, 45.0]",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelToml {
    solar: RawModelSolar,
    contrast: ContrastThresholds,
    day_phase: DayPhaseConfig,
    season: SeasonConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelSolar {
    event_angles: SolarEventAngles,
}

/// Complete tunable solar, day-phase, season, and contrast configuration (`data/model.toml`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub event_angles: SolarEventAngles,
    pub day_phase: DayPhaseConfig,
    pub season: SeasonConfig,
    pub contrast: ContrastThresholds,
}

impl ModelConfig {
    pub fn validate(&self) -> Result<(), ModelConfigError> {
        self.event_angles.validate()?;
        self.day_phase.validate(&self.event_angles)?;
        self.season.validate()?;
        self.contrast.validate()?;
        Ok(())
    }

    pub fn from_model_toml(toml_str: &str) -> Result<Self, ModelConfigError> {
        let raw: RawModelToml =
            toml::from_str(toml_str).map_err(|err| ModelConfigError::Parse(err.to_string()))?;
        let cfg = Self {
            event_angles: raw.solar.event_angles,
            day_phase: raw.day_phase,
            season: raw.season,
            contrast: raw.contrast,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    #[must_use]
    pub fn bundled() -> Self {
        Self::from_model_toml(DEFAULT_MODEL_TOML).expect("bundled data/model.toml must be valid")
    }
}

/// Active diurnal half (`dawn` ascending vs `dusk` descending).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayHalf {
    Dawn,
    Dusk,
}

impl DayHalf {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dawn => "dawn",
            Self::Dusk => "dusk",
        }
    }
}

/// Anchor diurnal applicability in palette TOML (`"both"`, `"dawn"`, or `"dusk"`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorHalf {
    #[default]
    Both,
    Dawn,
    Dusk,
}

impl AnchorHalf {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Dawn => "dawn",
            Self::Dusk => "dusk",
        }
    }

    fn parse_str(raw: &str) -> Option<Self> {
        match raw.trim() {
            "both" | "any" => Some(Self::Both),
            "dawn" | "morning" => Some(Self::Dawn),
            "dusk" | "evening" => Some(Self::Dusk),
            _ => None,
        }
    }

    const fn applies_to(self, half: DayHalf) -> bool {
        matches!(
            (self, half),
            (Self::Both, _) | (Self::Dawn, DayHalf::Dawn) | (Self::Dusk, DayHalf::Dusk)
        )
    }
}

/// Named diurnal light phase derived from `(day_phase, day_half)` keyframes (`PLAN.md` §4).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolarPhase {
    Night,
    AstronomicalDawn,
    NauticalDawn,
    CivilDawn,
    GoldenDawn,
    Day,
    GoldenDusk,
    CivilDusk,
    NauticalDusk,
    AstronomicalDusk,
}

impl SolarPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Night => "night",
            Self::AstronomicalDawn => "astronomical_dawn",
            Self::NauticalDawn => "nautical_dawn",
            Self::CivilDawn => "civil_dawn",
            Self::GoldenDawn => "golden_dawn",
            Self::Day => "day",
            Self::GoldenDusk => "golden_dusk",
            Self::CivilDusk => "civil_dusk",
            Self::NauticalDusk => "nautical_dusk",
            Self::AstronomicalDusk => "astronomical_dusk",
        }
    }
}

/// Structured palette schema/TOML error carrying exact file, 1-based line, and 1-based column.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub struct PaletteParseError {
    source_name: String,
    line: Option<usize>,
    column: Option<usize>,
    message: String,
}

impl PaletteParseError {
    #[must_use]
    pub fn new(
        source_name: impl Into<String>,
        line: Option<usize>,
        column: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_name: source_name.into(),
            line,
            column,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.line
    }

    #[must_use]
    pub const fn column(&self) -> Option<usize> {
        self.column
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PaletteParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(col)) => {
                write!(f, "{}:{line}:{col}: {}", self.source_name, self.message)
            }
            (Some(line), None) => write!(f, "{}:{line}: {}", self.source_name, self.message),
            _ => write!(f, "{}: {}", self.source_name, self.message),
        }
    }
}

fn span_to_line_col(src: &str, span: Option<Range<usize>>) -> (Option<usize>, Option<usize>) {
    let Some(range) = span else {
        return (None, None);
    };
    let offset = range.start.min(src.len());
    let prefix = &src[..offset];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = match prefix.rfind('\n') {
        Some(nl) => prefix[nl + 1..].chars().count() + 1,
        None => prefix.chars().count() + 1,
    };
    (Some(line), Some(col))
}

fn err_at_span(
    source_name: &str,
    src: &str,
    span: Range<usize>,
    message: impl Into<String>,
) -> PaletteParseError {
    let (line, col) = span_to_line_col(src, Some(span));
    PaletteParseError::new(source_name, line, col, message)
}

/// Complete 8-bit sRGB terminal colour slots for one anchor or rendered frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteColors {
    pub bg: Rgb8,
    pub fg: Rgb8,
    pub cursor: Rgb8,
    pub selection_bg: Rgb8,
    pub selection_fg: Rgb8,
    pub ansi: [Rgb8; 16],
}

impl PaletteColors {
    #[must_use]
    pub fn passes_contrast(&self, thresholds: &ContrastThresholds) -> bool {
        if !thresholds.meets_fg_bg(self.fg, self.bg) {
            return false;
        }
        if !thresholds.meets_fg_bg(self.selection_fg, self.selection_bg) {
            return false;
        }
        for (idx, &slot) in self.ansi.iter().enumerate() {
            if idx == 0 || idx == 8 {
                continue;
            }
            if !thresholds.meets_ansi_bg(slot, self.bg) {
                return false;
            }
        }
        true
    }

    #[must_use]
    pub fn max_delta_e(&self, other: &Self) -> f64 {
        let mut max_de = delta_e_ok(self.bg, other.bg)
            .max(delta_e_ok(self.fg, other.fg))
            .max(delta_e_ok(self.cursor, other.cursor))
            .max(delta_e_ok(self.selection_bg, other.selection_bg))
            .max(delta_e_ok(self.selection_fg, other.selection_fg));
        for (a, b) in self.ansi.iter().zip(other.ansi.iter()) {
            max_de = max_de.max(delta_e_ok(*a, *b));
        }
        max_de
    }
}

/// Continuous OKLCH representation of all terminal colour slots prior to contrast fix-up.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PaletteOklchColors {
    pub bg: Oklch,
    pub fg: Oklch,
    pub cursor: Oklch,
    pub selection_bg: Oklch,
    pub selection_fg: Oklch,
    pub ansi: [Oklch; 16],
}

impl PaletteOklchColors {
    #[must_use]
    pub fn from_rgb8(colors: &PaletteColors) -> Self {
        Self {
            bg: colors.bg.to_oklch(),
            fg: colors.fg.to_oklch(),
            cursor: colors.cursor.to_oklch(),
            selection_bg: colors.selection_bg.to_oklch(),
            selection_fg: colors.selection_fg.to_oklch(),
            ansi: colors.ansi.map(Rgb8::to_oklch),
        }
    }

    #[must_use]
    pub fn gamut_map_srgb(&self) -> Self {
        Self {
            bg: self.bg.gamut_map_srgb(),
            fg: self.fg.gamut_map_srgb(),
            cursor: self.cursor.gamut_map_srgb(),
            selection_bg: self.selection_bg.gamut_map_srgb(),
            selection_fg: self.selection_fg.gamut_map_srgb(),
            ansi: self.ansi.map(Oklch::gamut_map_srgb),
        }
    }

    #[must_use]
    pub fn to_rgb8_gamut_mapped(&self) -> PaletteColors {
        PaletteColors {
            bg: self.bg.to_rgb8_gamut_mapped(),
            fg: self.fg.to_rgb8_gamut_mapped(),
            cursor: self.cursor.to_rgb8_gamut_mapped(),
            selection_bg: self.selection_bg.to_rgb8_gamut_mapped(),
            selection_fg: self.selection_fg.to_rgb8_gamut_mapped(),
            ansi: self.ansi.map(Oklch::to_rgb8_gamut_mapped),
        }
    }

    fn lerp(&self, other: &Self, t: f64) -> Self {
        let mut ansi = [Oklch::new(0.0, 0.0, 0.0); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            *slot = oklch_lerp(self.ansi[i], other.ansi[i], t);
        }
        Self {
            bg: oklch_lerp(self.bg, other.bg, t),
            fg: oklch_lerp(self.fg, other.fg, t),
            cursor: oklch_lerp(self.cursor, other.cursor, t),
            selection_bg: oklch_lerp(self.selection_bg, other.selection_bg, t),
            selection_fg: oklch_lerp(self.selection_fg, other.selection_fg, t),
            ansi,
        }
    }
}

/// Validated palette anchor at `(d, s, half)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub d: f64,
    pub s: f64,
    pub half: AnchorHalf,
    pub colors: PaletteColors,
}

#[derive(Clone, Debug, PartialEq)]
struct PaletteGrid {
    d_coords: Vec<f64>,
    s_coords: Vec<f64>,
    cells: Vec<PaletteOklchColors>,
    neutral_by_d: Vec<PaletteOklchColors>,
}

impl PaletteGrid {
    fn cell(&self, d_idx: usize, s_idx: usize) -> &PaletteOklchColors {
        &self.cells[d_idx * self.s_coords.len() + s_idx]
    }

    fn find_d_segment(&self, d: f64) -> (usize, usize, f64) {
        let d_clamped = d.clamp(0.0, 1.0);
        let n = self.d_coords.len();
        if d_clamped <= self.d_coords[0] {
            return (0, 0, 0.0);
        }
        if d_clamped >= self.d_coords[n - 1] {
            return (n - 1, n - 1, 0.0);
        }
        for i in 0..(n - 1) {
            let lo = self.d_coords[i];
            let hi = self.d_coords[i + 1];
            if (d_clamped - lo).abs() <= 1e-12 {
                return (i, i, 0.0);
            }
            if (d_clamped - hi).abs() <= 1e-12 {
                return (i + 1, i + 1, 0.0);
            }
            if d_clamped > lo && d_clamped < hi {
                let td = (d_clamped - lo) / (hi - lo);
                return (i, i + 1, td);
            }
        }
        (n - 1, n - 1, 0.0)
    }

    fn find_s_segment(&self, s: f64) -> (usize, usize, f64) {
        let m = self.s_coords.len();
        if m <= 1 {
            return (0, 0, 0.0);
        }
        let s_wrapped = s.rem_euclid(1.0);
        for j in 0..m {
            if (s_wrapped - self.s_coords[j]).abs() <= 1e-12 {
                return (j, j, 0.0);
            }
        }
        for j in 0..(m - 1) {
            let lo = self.s_coords[j];
            let hi = self.s_coords[j + 1];
            if s_wrapped > lo && s_wrapped < hi {
                return (j, j + 1, (s_wrapped - lo) / (hi - lo));
            }
        }
        // Wraps across `[s_last, 1.0 + s_first)`.
        let last = self.s_coords[m - 1];
        let first_wrapped = self.s_coords[0] + 1.0;
        let s_adj = if s_wrapped < self.s_coords[0] {
            s_wrapped + 1.0
        } else {
            s_wrapped
        };
        let span = first_wrapped - last;
        let ts = if span > 0.0 {
            ((s_adj - last) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (m - 1, 0, ts)
    }

    fn blend(&self, d: f64, s: f64, season_strength: f64) -> PaletteOklchColors {
        let (d0, d1, td) = self.find_d_segment(d);
        let (s0, s1, ts) = self.find_s_segment(s);

        let c_s0 = if d0 == d1 {
            *self.cell(d0, s0)
        } else {
            self.cell(d0, s0).lerp(self.cell(d1, s0), td)
        };
        let seasonal = if s0 == s1 {
            c_s0
        } else {
            let c_s1 = if d0 == d1 {
                *self.cell(d0, s1)
            } else {
                self.cell(d0, s1).lerp(self.cell(d1, s1), td)
            };
            c_s0.lerp(&c_s1, ts)
        };

        let strength = season_strength.clamp(0.0, 1.0);
        if strength >= 1.0 || self.s_coords.len() <= 1 {
            return seasonal;
        }

        let neutral = if d0 == d1 {
            self.neutral_by_d[d0]
        } else {
            self.neutral_by_d[d0].lerp(&self.neutral_by_d[d1], td)
        };
        neutral.lerp(&seasonal, strength)
    }
}

fn season_cycle_weights(s_coords: &[f64]) -> Vec<f64> {
    let m = s_coords.len();
    if m <= 1 {
        return vec![1.0; m];
    }
    let mut weights = vec![0.0; m];
    for j in 0..m {
        let prev = if j == 0 {
            s_coords[m - 1] - 1.0
        } else {
            s_coords[j - 1]
        };
        let next = if j + 1 == m {
            s_coords[0] + 1.0
        } else {
            s_coords[j + 1]
        };
        weights[j] = 0.5 * (next - prev);
    }
    weights
}

fn average_oklch_cycle(row: &[PaletteOklchColors], weights: &[f64]) -> PaletteOklchColors {
    let avg_slot = |pick: fn(&PaletteOklchColors) -> Oklch| -> Oklch {
        let mut l = 0.0;
        let mut a = 0.0;
        let mut b = 0.0;
        for (colors, &w) in row.iter().zip(weights.iter()) {
            let lab = pick(colors).to_oklab();
            l += w * lab.l;
            a += w * lab.a;
            b += w * lab.b;
        }
        Oklab::new(l, a, b).to_oklch().gamut_map_srgb()
    };

    let mut ansi = [Oklch::new(0.0, 0.0, 0.0); 16];
    for (idx, slot) in ansi.iter_mut().enumerate() {
        let mut l = 0.0;
        let mut a = 0.0;
        let mut b = 0.0;
        for (colors, &w) in row.iter().zip(weights.iter()) {
            let lab = colors.ansi[idx].to_oklab();
            l += w * lab.l;
            a += w * lab.a;
            b += w * lab.b;
        }
        *slot = Oklab::new(l, a, b).to_oklch().gamut_map_srgb();
    }

    PaletteOklchColors {
        bg: avg_slot(|c| c.bg),
        fg: avg_slot(|c| c.fg),
        cursor: avg_slot(|c| c.cursor),
        selection_bg: avg_slot(|c| c.selection_bg),
        selection_fg: avg_slot(|c| c.selection_fg),
        ansi,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPaletteToml {
    name: Spanned<String>,
    #[serde(default)]
    description: Option<String>,
    anchors: Vec<RawAnchor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnchor {
    d: Spanned<f64>,
    #[serde(default)]
    s: Option<Spanned<f64>>,
    #[serde(default)]
    half: Option<Spanned<String>>,
    bg: Spanned<String>,
    fg: Spanned<String>,
    cursor: Spanned<String>,
    #[serde(default, alias = "selection")]
    selection_bg: Option<Spanned<String>>,
    #[serde(default)]
    selection_fg: Option<Spanned<String>>,
    ansi: Spanned<Vec<Spanned<String>>>,
}

fn quantize_grid_coord(v: f64) -> i64 {
    (v * GRID_COORD_SCALE).round() as i64
}

fn parse_spanned_hex(
    field: &Spanned<String>,
    source_name: &str,
    src: &str,
) -> Result<Rgb8, PaletteParseError> {
    Rgb8::from_hex(field.get_ref())
        .map_err(|err| err_at_span(source_name, src, field.span(), err.to_string()))
}

/// Validated 2-D `(d, s)` terminal colour palette with precomputed OKLCH grids for dawn and dusk.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    name: String,
    description: Option<String>,
    anchors: Vec<Anchor>,
    dawn_grid: PaletteGrid,
    dusk_grid: PaletteGrid,
}

impl Palette {
    pub fn from_toml(toml_str: &str) -> Result<Self, PaletteParseError> {
        Self::from_toml_with_source(toml_str, "<palette.toml>")
    }

    pub fn from_toml_with_source(
        toml_str: &str,
        source_name: &str,
    ) -> Result<Self, PaletteParseError> {
        let raw: RawPaletteToml = toml::from_str(toml_str).map_err(|err| {
            let (line, col) = span_to_line_col(toml_str, err.span());
            PaletteParseError::new(source_name, line, col, err.message())
        })?;

        let name = raw.name.get_ref().trim().to_string();
        if name.is_empty() {
            return Err(err_at_span(
                source_name,
                toml_str,
                raw.name.span(),
                "palette `name` must not be empty",
            ));
        }

        if raw.anchors.is_empty() {
            return Err(PaletteParseError::new(
                source_name,
                Some(1),
                Some(1),
                "palette must define at least two `[[anchors]]` (d = 0.0 and d = 1.0)",
            ));
        }

        let mut parsed_anchors = Vec::with_capacity(raw.anchors.len());
        let mut anchor_spans = Vec::with_capacity(raw.anchors.len());
        let mut s_map: BTreeMap<i64, f64> = BTreeMap::new();

        for raw_anchor in &raw.anchors {
            let d_span = raw_anchor.d.span();
            let d = *raw_anchor.d.get_ref();
            if !d.is_finite() || !(0.0..=1.0).contains(&d) {
                return Err(err_at_span(
                    source_name,
                    toml_str,
                    d_span,
                    format!("day phase `d` must be finite and within [0.0, 1.0], got {d}"),
                ));
            }

            let s = match &raw_anchor.s {
                Some(s_spanned) => {
                    let s_val = *s_spanned.get_ref();
                    if !s_val.is_finite() || !(0.0..1.0).contains(&s_val) {
                        return Err(err_at_span(
                            source_name,
                            toml_str,
                            s_spanned.span(),
                            format!(
                                "season phase `s` must be finite and within [0.0, 1.0), got {s_val}"
                            ),
                        ));
                    }
                    s_val
                }
                None => 0.0,
            };

            let half = match &raw_anchor.half {
                Some(half_spanned) => {
                    AnchorHalf::parse_str(half_spanned.get_ref()).ok_or_else(|| {
                        err_at_span(
                            source_name,
                            toml_str,
                            half_spanned.span(),
                            format!(
                                "invalid anchor `half` '{}': expected `both`, `dawn`, or `dusk`",
                                half_spanned.get_ref()
                            ),
                        )
                    })?
                }
                None => AnchorHalf::Both,
            };

            let bg = parse_spanned_hex(&raw_anchor.bg, source_name, toml_str)?;
            let fg = parse_spanned_hex(&raw_anchor.fg, source_name, toml_str)?;
            let cursor = parse_spanned_hex(&raw_anchor.cursor, source_name, toml_str)?;

            let selection_bg_spanned = raw_anchor.selection_bg.as_ref().ok_or_else(|| {
                err_at_span(
                    source_name,
                    toml_str,
                    d_span.clone(),
                    "missing required field `selection_bg` (or `selection`)",
                )
            })?;
            let selection_bg = parse_spanned_hex(selection_bg_spanned, source_name, toml_str)?;
            let selection_fg = match &raw_anchor.selection_fg {
                Some(sel_fg_spanned) => parse_spanned_hex(sel_fg_spanned, source_name, toml_str)?,
                None => fg,
            };

            let raw_ansi = raw_anchor.ansi.get_ref();
            if raw_ansi.len() != 16 {
                return Err(err_at_span(
                    source_name,
                    toml_str,
                    raw_anchor.ansi.span(),
                    format!(
                        "expected exactly 16 ANSI colours (`ansi[0..=15]`), got {}",
                        raw_ansi.len()
                    ),
                ));
            }
            let mut ansi = [Rgb8::BLACK; 16];
            for (idx, item) in raw_ansi.iter().enumerate() {
                ansi[idx] = parse_spanned_hex(item, source_name, toml_str)?;
            }

            let s_key = quantize_grid_coord(s);
            let canonical_s = *s_map.entry(s_key).or_insert(if s == 0.0 { 0.0 } else { s });
            let canonical_d = if d == 0.0 { 0.0 } else { d };

            parsed_anchors.push(Anchor {
                d: canonical_d,
                s: canonical_s,
                half,
                colors: PaletteColors {
                    bg,
                    fg,
                    cursor,
                    selection_bg,
                    selection_fg,
                    ansi,
                },
            });
            anchor_spans.push(d_span);
        }

        let s_keys: Vec<i64> = s_map.keys().copied().collect();
        let s_coords: Vec<f64> = s_map.values().copied().collect();
        let dawn_grid = Self::build_half_grid(
            DayHalf::Dawn,
            &parsed_anchors,
            &anchor_spans,
            &s_keys,
            &s_coords,
            source_name,
            toml_str,
        )?;
        let dusk_grid = Self::build_half_grid(
            DayHalf::Dusk,
            &parsed_anchors,
            &anchor_spans,
            &s_keys,
            &s_coords,
            source_name,
            toml_str,
        )?;

        Ok(Self {
            name,
            description: raw.description,
            anchors: parsed_anchors,
            dawn_grid,
            dusk_grid,
        })
    }

    fn build_half_grid(
        half: DayHalf,
        anchors: &[Anchor],
        spans: &[Range<usize>],
        s_keys: &[i64],
        s_coords: &[f64],
        source_name: &str,
        src: &str,
    ) -> Result<PaletteGrid, PaletteParseError> {
        let mut d_map: BTreeMap<i64, f64> = BTreeMap::new();
        let mut cell_map: BTreeMap<(i64, i64), PaletteOklchColors> = BTreeMap::new();
        let mut first_span = 0..0;

        for (anchor, span) in anchors.iter().zip(spans.iter()) {
            if !anchor.half.applies_to(half) {
                continue;
            }
            if first_span.is_empty() {
                first_span = span.clone();
            }
            let d_key = quantize_grid_coord(anchor.d);
            let s_key = quantize_grid_coord(anchor.s);
            d_map.entry(d_key).or_insert(anchor.d);
            if cell_map
                .insert(
                    (d_key, s_key),
                    PaletteOklchColors::from_rgb8(&anchor.colors),
                )
                .is_some()
            {
                return Err(err_at_span(
                    source_name,
                    src,
                    span.clone(),
                    format!(
                        "duplicate anchor for {} grid at (d={:.3}, s={:.3})",
                        half.as_str(),
                        anchor.d,
                        anchor.s
                    ),
                ));
            }
        }

        let d_keys: BTreeSet<i64> = d_map.keys().copied().collect();
        let zero_key = quantize_grid_coord(0.0);
        let one_key = quantize_grid_coord(1.0);
        if !d_keys.contains(&zero_key) || !d_keys.contains(&one_key) {
            let missing = if !d_keys.contains(&zero_key) {
                "0.0"
            } else {
                "1.0"
            };
            return Err(err_at_span(
                source_name,
                src,
                spans.first().cloned().unwrap_or(0..0),
                format!(
                    "palette {} grid is missing required boundary anchor at d = {missing} (both d = 0.0 and d = 1.0 are required)",
                    half.as_str()
                ),
            ));
        }

        let d_key_vec: Vec<i64> = d_map.keys().copied().collect();
        let d_coords: Vec<f64> = d_map.values().copied().collect();
        let mut cells = Vec::with_capacity(d_coords.len() * s_coords.len());
        let weights = season_cycle_weights(s_coords);
        let mut neutral_by_d = Vec::with_capacity(d_coords.len());

        for (&d_key, &d_val) in d_key_vec.iter().zip(d_coords.iter()) {
            let mut row = Vec::with_capacity(s_coords.len());
            for (&s_key, &s_val) in s_keys.iter().zip(s_coords.iter()) {
                let Some(&oklch_colors) = cell_map.get(&(d_key, s_key)) else {
                    return Err(err_at_span(
                        source_name,
                        src,
                        first_span,
                        format!(
                            "incomplete palette {} grid: missing anchor at (d={d_val:.3}, s={s_val:.3})",
                            half.as_str()
                        ),
                    ));
                };
                row.push(oklch_colors);
                cells.push(oklch_colors);
            }
            neutral_by_d.push(average_oklch_cycle(&row, &weights));
        }

        Ok(PaletteGrid {
            d_coords,
            s_coords: s_coords.to_vec(),
            cells,
            neutral_by_d,
        })
    }

    #[must_use]
    pub fn bundled_names() -> &'static [&'static str] {
        &["shiki"]
    }

    #[must_use]
    pub fn bundled(name: &str) -> Option<Self> {
        match name {
            "shiki" => Some(Self::bundled_shiki()),
            _ => None,
        }
    }

    #[must_use]
    pub fn bundled_shiki() -> Self {
        Self::from_toml_with_source(BUNDLED_SHIKI_TOML, "data/palettes/shiki.toml")
            .expect("bundled data/palettes/shiki.toml must be valid")
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// Blends the palette in OKLCH at `(d, s)` with `morning_weight` (`1.0` pure dawn, `0.0` pure
    /// dusk) and `season_strength` (`1.0` full seasonal amplitude, `0.0` equatorial neutral),
    /// returning unadjusted OKLCH colours gamut-mapped to sRGB at constant `(L, h)`.
    #[must_use]
    pub fn blend_oklch(
        &self,
        d: f64,
        s: f64,
        morning_weight: f64,
        season_strength: f64,
    ) -> PaletteOklchColors {
        let mw = morning_weight.clamp(0.0, 1.0);
        let blended = if mw >= 1.0 {
            self.dawn_grid.blend(d, s, season_strength)
        } else if mw <= 0.0 {
            self.dusk_grid.blend(d, s, season_strength)
        } else {
            let dawn = self.dawn_grid.blend(d, s, season_strength);
            let dusk = self.dusk_grid.blend(d, s, season_strength);
            dusk.lerp(&dawn, mw)
        };
        blended.gamut_map_srgb()
    }
}

/// Blended and contrast-verified terminal colour frame for a single [`UtcInstant`].
///
/// Privacy (`PLAN.md` §0 #4): never stores coordinates.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub instant: UtcInstant,
    pub day_phase: f64,
    pub day_half: DayHalf,
    pub solar_phase: SolarPhase,
    pub season_phase: f64,
    pub season: Season,
    pub season_strength: f64,
    pub elevation_deg: f64,
    pub noon_elevation_deg: f64,
    pub polar_state: PolarState,
    pub colors: PaletteColors,
    pub fixup_slots: u8,
}

impl Frame {
    #[must_use]
    pub fn passes_contrast(&self, thresholds: &ContrastThresholds) -> bool {
        self.colors.passes_contrast(thresholds)
    }

    #[must_use]
    pub fn max_delta_e(&self, other: &Self) -> f64 {
        self.colors.max_delta_e(&other.colors)
    }
}

/// Checked contrast-bearing slot identifier for `palette check` worst-moment reporting.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckedSlot {
    Fg,
    Selection,
    Ansi(u8),
}

impl fmt::Display for CheckedSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fg => f.write_str("fg"),
            Self::Selection => f.write_str("selection"),
            Self::Ansi(idx) => write!(f, "ansi[{idx}]"),
        }
    }
}

/// Worst contrast moment observed across a year sweep (`PLAN.md` §4, T06).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorstMoment {
    pub instant: UtcInstant,
    pub slot: CheckedSlot,
    pub fg_color: Rgb8,
    pub bg_color: Rgb8,
    pub raw_contrast_ratio: f64,
    pub final_contrast_ratio: f64,
    pub required_ratio: f64,
    pub day_phase: f64,
    pub day_half: DayHalf,
    pub solar_phase: SolarPhase,
    pub season_phase: f64,
    pub elevation_deg: f64,
}

impl WorstMoment {
    #[must_use]
    pub fn margin(&self) -> f64 {
        self.final_contrast_ratio - self.required_ratio
    }

    #[must_use]
    pub fn raw_margin(&self) -> f64 {
        self.raw_contrast_ratio - self.required_ratio
    }

    fn is_worse_than(&self, incumbent: &Self) -> bool {
        let self_margin = self.margin();
        let inc_margin = incumbent.margin();
        if (self_margin - inc_margin).abs() > 1e-9 {
            return self_margin < inc_margin;
        }
        let self_raw = self.raw_margin();
        let inc_raw = incumbent.raw_margin();
        if (self_raw - inc_raw).abs() > 1e-9 {
            return self_raw < inc_raw;
        }
        match self.instant.cmp(&incumbent.instant) {
            Ordering::Less => true,
            Ordering::Equal => self.slot < incumbent.slot,
            Ordering::Greater => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PaletteCheckError {
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error(transparent)]
    Model(#[from] ModelConfigError),
    #[error("step_minutes must be within 1..=1440, got {0}")]
    InvalidStepMinutes(u32),
}

/// Summary report produced by [`check_palette_year`] (`shiki-term palette check`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaletteCheckReport {
    pub palette_name: String,
    pub year: i32,
    pub is_northern_hemisphere: bool,
    pub step_minutes: u32,
    pub frames_checked: usize,
    pub frames_fixed_up: usize,
    pub slots_fixed_up: usize,
    pub all_passed: bool,
    pub thresholds: ContrastThresholds,
    pub worst_text: WorstMoment,
    pub worst_ansi: WorstMoment,
    pub worst_overall: WorstMoment,
}

/// Interpolates two OKLCH colours with linear `L` and `C`, and shortest-arc hue down-weighted as
/// `C -> 0` so near-achromatic greys never spin (`PLAN.md` §4).
#[must_use]
pub fn oklch_lerp(a: Oklch, b: Oklch, t: f64) -> Oklch {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }

    let c0 = a.c.max(0.0);
    let c1 = b.c.max(0.0);
    let l = a.l + t * (b.l - a.l);
    let c = c0 + t * (c1 - c0);

    let eff_c0 = if c0 < CHROMA_WEIGHT_EPSILON { 0.0 } else { c0 };
    let eff_c1 = if c1 < CHROMA_WEIGHT_EPSILON { 0.0 } else { c1 };
    let w0 = (1.0 - t) * eff_c0;
    let w1 = t * eff_c1;
    let w_sum = w0 + w1;
    let delta_h = shortest_hue_delta_deg(a.h_deg, b.h_deg);
    let t_hue = if w_sum <= CHROMA_WEIGHT_EPSILON {
        if eff_c1 > 0.0 {
            1.0
        } else if eff_c0 > 0.0 {
            0.0
        } else {
            t
        }
    } else {
        w1 / w_sum
    };
    let h_deg = normalize_hue_deg(a.h_deg + t_hue * delta_h);
    let polar = Oklch { l, c, h_deg };

    if eff_c0 > 0.0 && eff_c1 > 0.0 && delta_h.abs() > 90.0 {
        let beta = smoothstep(90.0, 180.0, delta_h.abs());
        let p_lab = polar.to_oklab();
        let a_lab = a.to_oklab();
        let b_lab = b.to_oklab();
        let cart_a = a_lab.a + t * (b_lab.a - a_lab.a);
        let cart_b = a_lab.b + t * (b_lab.b - a_lab.b);
        return Oklab::new(
            l,
            p_lab.a + beta * (cart_a - p_lab.a),
            p_lab.b + beta * (cart_b - p_lab.b),
        )
        .to_oklch();
    }

    polar
}

fn lerp_segment(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    if (x1 - x0).abs() <= 1e-12 {
        return y1;
    }
    let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
    y0 + t * (y1 - y0)
}

/// Computes day phase `d` in `[0.0, 1.0]` from solar `elevation_deg` normalized against
/// `noon_elevation_deg` (`PLAN.md` §4).
#[must_use]
pub fn day_phase_from_elevation(
    elevation_deg: f64,
    noon_elevation_deg: f64,
    angles: &SolarEventAngles,
    config: &DayPhaseConfig,
) -> f64 {
    if elevation_deg <= angles.astronomical_twilight_deg {
        return config.night_d;
    }
    if elevation_deg <= angles.nautical_twilight_deg {
        return lerp_segment(
            elevation_deg,
            angles.astronomical_twilight_deg,
            angles.nautical_twilight_deg,
            config.night_d,
            config.nautical_d,
        );
    }
    if elevation_deg <= angles.civil_twilight_deg {
        return lerp_segment(
            elevation_deg,
            angles.nautical_twilight_deg,
            angles.civil_twilight_deg,
            config.nautical_d,
            config.civil_d,
        );
    }
    if elevation_deg <= angles.sunrise_sunset_deg {
        return lerp_segment(
            elevation_deg,
            angles.civil_twilight_deg,
            angles.sunrise_sunset_deg,
            config.civil_d,
            config.sunrise_sunset_d,
        );
    }

    let e_horizon = angles.sunrise_sunset_deg;
    let e_day = noon_elevation_deg.clamp(angles.golden_hour_deg, config.day_full_elevation_deg);
    let golden_frac =
        (angles.golden_hour_deg - e_horizon) / (config.day_full_elevation_deg - e_horizon);
    let e_golden_eff = e_horizon + golden_frac * (e_day - e_horizon);

    if elevation_deg >= e_day {
        config.day_d
    } else if elevation_deg <= e_golden_eff {
        lerp_segment(
            elevation_deg,
            e_horizon,
            e_golden_eff,
            config.sunrise_sunset_d,
            config.golden_d,
        )
    } else {
        lerp_segment(
            elevation_deg,
            e_golden_eff,
            e_day,
            config.golden_d,
            config.day_d,
        )
    }
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if (edge1 - edge0).abs() <= 1e-12 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Computes the smooth morning weight in `[0.0, 1.0]` (`1.0` dawn, `0.0` dusk) from true-solar
/// `hour_angle_deg` in `[-180.0, 180.0)`.
#[must_use]
pub fn morning_weight_from_hour_angle(hour_angle_deg: f64, half_width_deg: f64) -> f64 {
    let mut ha = (hour_angle_deg + 180.0).rem_euclid(360.0) - 180.0;
    if ha >= 180.0 {
        ha -= 360.0;
    }
    let w = half_width_deg.clamp(0.0, 45.0);
    if w <= 1e-12 {
        return if ha < 0.0 { 1.0 } else { 0.0 };
    }

    if ha.abs() < w {
        // Around solar noon (`ha = 0`): smoothly transitions from `1.0` at `-w` to `0.0` at `+w`.
        1.0 - smoothstep(-w, w, ha)
    } else if ha < -180.0 + w {
        // Just after solar nadir (`ha` in `[-180, -180 + w)`): rises from `0.5` at `-180` to `1.0` at `-180 + w`.
        smoothstep(-180.0 - w, -180.0 + w, ha)
    } else if ha > 180.0 - w {
        // Just before solar nadir (`ha` in `(180 - w, 180)`): rises from `0.0` at `180 - w` to `0.5` at `180`.
        smoothstep(180.0 - w, 180.0 + w, ha)
    } else if ha < 0.0 {
        1.0
    } else {
        0.0
    }
}

/// Classifies the named [`SolarPhase`] from `day_phase`, `day_half`, and [`DayPhaseConfig`].
#[must_use]
pub fn classify_solar_phase(
    day_phase: f64,
    day_half: DayHalf,
    config: &DayPhaseConfig,
) -> SolarPhase {
    if day_phase <= config.night_d + 1e-9 {
        return SolarPhase::Night;
    }
    if day_phase >= config.day_d - 1e-9 {
        return SolarPhase::Day;
    }
    match day_half {
        DayHalf::Dawn => {
            if day_phase < config.nautical_d {
                SolarPhase::AstronomicalDawn
            } else if day_phase < config.civil_d {
                SolarPhase::NauticalDawn
            } else if day_phase < config.sunrise_sunset_d {
                SolarPhase::CivilDawn
            } else {
                SolarPhase::GoldenDawn
            }
        }
        DayHalf::Dusk => {
            if day_phase < config.nautical_d {
                SolarPhase::AstronomicalDusk
            } else if day_phase < config.civil_d {
                SolarPhase::NauticalDusk
            } else if day_phase < config.sunrise_sunset_d {
                SolarPhase::CivilDusk
            } else {
                SolarPhase::GoldenDusk
            }
        }
    }
}

fn max_achievable_contrast(bg_rgb: Rgb8) -> f64 {
    contrast_ratio(bg_rgb, Rgb8::WHITE).max(contrast_ratio(bg_rgb, Rgb8::BLACK))
}

fn nudge_background_if_needed(bg: Oklch, min_required_ratio: f64) -> (Rgb8, bool) {
    let initial_rgb = bg.to_rgb8_gamut_mapped();
    if max_achievable_contrast(initial_rgb) >= min_required_ratio {
        return (initial_rgb, false);
    }

    let prefer_darker =
        contrast_ratio(initial_rgb, Rgb8::WHITE) >= contrast_ratio(initial_rgb, Rgb8::BLACK);
    for go_darker in [prefer_darker, !prefer_darker] {
        let l_end = if go_darker { 0.0 } else { 1.0 };
        let end_rgb = Oklch {
            l: l_end,
            c: bg.c,
            h_deg: bg.h_deg,
        }
        .to_rgb8_gamut_mapped();
        if max_achievable_contrast(end_rgb) < min_required_ratio {
            continue;
        }

        let mut l_fail = bg.l.clamp(0.0, 1.0);
        let mut l_pass = l_end;
        let mut best_rgb = end_rgb;
        for _ in 0..FIXUP_BISECTION_STEPS {
            let mid = 0.5 * (l_fail + l_pass);
            let mid_rgb = Oklch {
                l: mid,
                c: bg.c,
                h_deg: bg.h_deg,
            }
            .to_rgb8_gamut_mapped();
            if max_achievable_contrast(mid_rgb) >= min_required_ratio {
                l_pass = mid;
                best_rgb = mid_rgb;
            } else {
                l_fail = mid;
            }
        }
        return (best_rgb, best_rgb != initial_rgb);
    }

    (initial_rgb, false)
}

fn search_slot_l_direction(
    slot: Oklch,
    bg_rgb: Rgb8,
    min_ratio: f64,
    go_lighter: bool,
) -> Option<Rgb8> {
    let l_end = if go_lighter { 1.0 } else { 0.0 };
    let end_rgb = Oklch {
        l: l_end,
        c: slot.c,
        h_deg: slot.h_deg,
    }
    .to_rgb8_gamut_mapped();
    if contrast_ratio(end_rgb, bg_rgb) < min_ratio {
        return None;
    }

    let mut l_fail = slot.l.clamp(0.0, 1.0);
    let mut l_pass = l_end;
    let mut best_rgb = end_rgb;
    for _ in 0..FIXUP_BISECTION_STEPS {
        let mid = 0.5 * (l_fail + l_pass);
        let mid_rgb = Oklch {
            l: mid,
            c: slot.c,
            h_deg: slot.h_deg,
        }
        .to_rgb8_gamut_mapped();
        if contrast_ratio(mid_rgb, bg_rgb) >= min_ratio {
            l_pass = mid;
            best_rgb = mid_rgb;
        } else {
            l_fail = mid;
        }
    }
    Some(best_rgb)
}

fn fix_slot_against_bg(slot: Oklch, bg_rgb: Rgb8, min_ratio: f64) -> (Rgb8, bool) {
    let initial_rgb = slot.to_rgb8_gamut_mapped();
    if contrast_ratio(initial_rgb, bg_rgb) >= min_ratio {
        return (initial_rgb, false);
    }

    let y_bg = bg_rgb.relative_luminance();
    let y_slot = initial_rgb.relative_luminance();
    let prefer_lighter = if (y_slot - y_bg).abs() > 1e-9 {
        y_slot > y_bg
    } else {
        contrast_ratio(Rgb8::WHITE, bg_rgb) >= contrast_ratio(Rgb8::BLACK, bg_rgb)
    };

    if let Some(fixed) = search_slot_l_direction(slot, bg_rgb, min_ratio, prefer_lighter) {
        return (fixed, true);
    }
    if let Some(fixed) = search_slot_l_direction(slot, bg_rgb, min_ratio, !prefer_lighter) {
        return (fixed, true);
    }

    (initial_rgb, false)
}

/// Applies WCAG 2.2 contrast fix-up (`PLAN.md` §4): moves any failing non-exempt slot's `L` away
/// from `bg` by the smallest passing step at constant `(C, h)`, else nudges `bg`'s `L`.
#[must_use]
pub fn fix_up_colors(
    raw: &PaletteOklchColors,
    thresholds: &ContrastThresholds,
) -> (PaletteColors, u8) {
    let mut adjusted_slots = 0_u8;

    let max_bg_req = thresholds.fg_bg_min_ratio.max(thresholds.ansi_bg_min_ratio);
    let (bg, bg_nudged) = nudge_background_if_needed(raw.bg, max_bg_req);
    if bg_nudged {
        adjusted_slots = adjusted_slots.saturating_add(1);
    }

    let (fg, fg_adjusted) = fix_slot_against_bg(raw.fg, bg, thresholds.fg_bg_min_ratio);
    if fg_adjusted {
        adjusted_slots = adjusted_slots.saturating_add(1);
    }

    let (selection_bg, sel_bg_nudged) =
        nudge_background_if_needed(raw.selection_bg, thresholds.fg_bg_min_ratio);
    if sel_bg_nudged {
        adjusted_slots = adjusted_slots.saturating_add(1);
    }
    let (selection_fg, sel_fg_adjusted) =
        fix_slot_against_bg(raw.selection_fg, selection_bg, thresholds.fg_bg_min_ratio);
    if sel_fg_adjusted {
        adjusted_slots = adjusted_slots.saturating_add(1);
    }

    let cursor = raw.cursor.to_rgb8_gamut_mapped();
    let mut ansi = [Rgb8::BLACK; 16];
    for (idx, slot) in ansi.iter_mut().enumerate() {
        if idx == 0 || idx == 8 {
            *slot = raw.ansi[idx].to_rgb8_gamut_mapped();
        } else {
            let (fixed_slot, did_fix) =
                fix_slot_against_bg(raw.ansi[idx], bg, thresholds.ansi_bg_min_ratio);
            *slot = fixed_slot;
            if did_fix {
                adjusted_slots = adjusted_slots.saturating_add(1);
            }
        }
    }

    let colors = PaletteColors {
        bg,
        fg,
        cursor,
        selection_bg,
        selection_fg,
        ansi,
    };
    debug_assert!(
        colors.passes_contrast(thresholds),
        "PLAN §0 #5 hard rule violated: frame failed contrast check after fix-up"
    );
    (colors, adjusted_slots)
}

fn evaluate_frame_with_day_summary(
    instant: UtcInstant,
    coords: Coordinates,
    position: crate::solar::SolarPosition,
    noon_elevation_deg: f64,
    polar_state: PolarState,
    palette: &Palette,
    model: &ModelConfig,
) -> (PaletteOklchColors, Frame) {
    let day_phase = day_phase_from_elevation(
        position.elevation_deg,
        noon_elevation_deg,
        &model.event_angles,
        &model.day_phase,
    );
    let day_half = if position.is_morning {
        DayHalf::Dawn
    } else {
        DayHalf::Dusk
    };
    let morning_weight = morning_weight_from_hour_angle(
        position.hour_angle_deg,
        model.day_phase.culmination_blend_half_width_deg,
    );
    let solar_phase = classify_solar_phase(day_phase, day_half, &model.day_phase);
    let season_st = season_state(instant, coords, &model.season);

    let raw_oklch = palette.blend_oklch(
        day_phase,
        season_st.phase,
        morning_weight,
        season_st.strength,
    );
    let (colors, fixup_slots) = fix_up_colors(&raw_oklch, &model.contrast);
    assert!(
        colors.passes_contrast(&model.contrast),
        "PLAN §0 #5 hard rule: no frame may leave sk-core without passing contrast"
    );

    let frame = Frame {
        instant,
        day_phase,
        day_half,
        solar_phase,
        season_phase: season_st.phase,
        season: season_st.season,
        season_strength: season_st.strength,
        elevation_deg: position.elevation_deg,
        noon_elevation_deg,
        polar_state,
        colors,
        fixup_slots,
    };
    (raw_oklch, frame)
}

/// Pure runtime frame evaluation (`PLAN.md` §3: `frame(instant, location, palette, config) -> Frame`).
#[must_use]
pub fn compute_frame(
    instant: UtcInstant,
    coords: Coordinates,
    palette: &Palette,
    model: &ModelConfig,
) -> Frame {
    let position = solar_position(instant, coords);
    let day_index = solar_day_index_at(instant, coords, &position);
    let (noon_elevation_deg, polar_state) =
        solar_day_summary_for_index(day_index, coords, &model.event_angles);
    let (_, frame) = evaluate_frame_with_day_summary(
        instant,
        coords,
        position,
        noon_elevation_deg,
        polar_state,
        palette,
        model,
    );
    frame
}

/// Sweeps an entire proleptic Gregorian UTC `year` at `step_minutes` resolution for `coords`,
/// checking every frame against `model.contrast` and recording the year's worst moment.
pub fn check_palette_year(
    palette: &Palette,
    coords: Coordinates,
    year: i32,
    step_minutes: u32,
    model: &ModelConfig,
) -> Result<PaletteCheckReport, PaletteCheckError> {
    if step_minutes == 0 || step_minutes > 1_440 {
        return Err(PaletteCheckError::InvalidStepMinutes(step_minutes));
    }
    model.validate()?;

    let start = UtcInstant::from_utc_ymd_hms(year, 1, 1, 0, 0, 0)?;
    let end = UtcInstant::from_utc_ymd_hms(year + 1, 1, 1, 0, 0, 0)?;
    let step_secs = i64::from(step_minutes) * SECS_PER_MINUTE;

    let mut cached_day: Option<(i64, f64, PolarState)> = None;
    let mut frames_checked = 0_usize;
    let mut frames_fixed_up = 0_usize;
    let mut slots_fixed_up = 0_usize;
    let mut all_passed = true;

    let mut worst_text: Option<WorstMoment> = None;
    let mut worst_ansi: Option<WorstMoment> = None;
    let mut worst_overall: Option<WorstMoment> = None;

    let mut cur = start;
    while cur < end {
        let position = solar_position(cur, coords);
        let day_index = solar_day_index_at(cur, coords, &position);
        let (noon_elevation_deg, polar_state) = match cached_day {
            Some((idx, noon_elev, pstate)) if idx == day_index => (noon_elev, pstate),
            _ => {
                let (noon_elev, pstate) =
                    solar_day_summary_for_index(day_index, coords, &model.event_angles);
                cached_day = Some((day_index, noon_elev, pstate));
                (noon_elev, pstate)
            }
        };

        let (raw_oklch, frame) = evaluate_frame_with_day_summary(
            cur,
            coords,
            position,
            noon_elevation_deg,
            polar_state,
            palette,
            model,
        );
        let raw_rgb = if frame.fixup_slots == 0 {
            frame.colors
        } else {
            raw_oklch.to_rgb8_gamut_mapped()
        };

        frames_checked += 1;
        if frame.fixup_slots > 0 {
            frames_fixed_up += 1;
            slots_fixed_up += usize::from(frame.fixup_slots);
        }
        if !frame.passes_contrast(&model.contrast) {
            all_passed = false;
        }

        let observe = |target: &mut Option<WorstMoment>, candidate: WorstMoment| match target {
            None => *target = Some(candidate),
            Some(incumbent) if candidate.is_worse_than(incumbent) => *target = Some(candidate),
            _ => {}
        };

        for (slot, fg_col, bg_col, raw_fg, raw_bg) in [
            (
                CheckedSlot::Fg,
                frame.colors.fg,
                frame.colors.bg,
                raw_rgb.fg,
                raw_rgb.bg,
            ),
            (
                CheckedSlot::Selection,
                frame.colors.selection_fg,
                frame.colors.selection_bg,
                raw_rgb.selection_fg,
                raw_rgb.selection_bg,
            ),
        ] {
            let moment = WorstMoment {
                instant: cur,
                slot,
                fg_color: fg_col,
                bg_color: bg_col,
                raw_contrast_ratio: contrast_ratio(raw_fg, raw_bg),
                final_contrast_ratio: contrast_ratio(fg_col, bg_col),
                required_ratio: model.contrast.fg_bg_min_ratio,
                day_phase: frame.day_phase,
                day_half: frame.day_half,
                solar_phase: frame.solar_phase,
                season_phase: frame.season_phase,
                elevation_deg: frame.elevation_deg,
            };
            observe(&mut worst_text, moment);
            observe(&mut worst_overall, moment);
        }

        for idx in 1..16_u8 {
            if idx == 8 {
                continue;
            }
            let fg_col = frame.colors.ansi[usize::from(idx)];
            let raw_fg = raw_rgb.ansi[usize::from(idx)];
            let moment = WorstMoment {
                instant: cur,
                slot: CheckedSlot::Ansi(idx),
                fg_color: fg_col,
                bg_color: frame.colors.bg,
                raw_contrast_ratio: contrast_ratio(raw_fg, raw_rgb.bg),
                final_contrast_ratio: contrast_ratio(fg_col, frame.colors.bg),
                required_ratio: model.contrast.ansi_bg_min_ratio,
                day_phase: frame.day_phase,
                day_half: frame.day_half,
                solar_phase: frame.solar_phase,
                season_phase: frame.season_phase,
                elevation_deg: frame.elevation_deg,
            };
            observe(&mut worst_ansi, moment);
            observe(&mut worst_overall, moment);
        }

        cur = cur.add_secs(step_secs);
    }

    Ok(PaletteCheckReport {
        palette_name: palette.name().to_string(),
        year,
        is_northern_hemisphere: coords.latitude_deg() >= 0.0,
        step_minutes,
        frames_checked,
        frames_fixed_up,
        slots_fixed_up,
        all_passed,
        thresholds: model.contrast,
        worst_text: worst_text.expect("year sweep has >= 1 frame"),
        worst_ansi: worst_ansi.expect("year sweep has >= 1 frame"),
        worst_overall: worst_overall.expect("year sweep has >= 1 frame"),
    })
}
