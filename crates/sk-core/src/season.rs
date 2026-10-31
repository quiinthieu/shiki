//! Pure astronomical season phase `s` and equator amplitude fade (`PLAN.md` §3, §4, T06).
//!
//! References:
//! - Jean Meeus, *Astronomical Algorithms* (2nd ed., Ch. 22, 25, 27): apparent solar ecliptic
//!   longitude (`0°` March equinox, `90°` June solstice, `180°` September equinox, `270°`
//!   December solstice) and J2000.0 mean obliquity of the ecliptic (`23°26'21.448"`).

use crate::solar::{apparent_solar_longitude_deg, Coordinates};
use crate::time::UtcInstant;
use core::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// J2000.0 mean obliquity of the ecliptic (`23°26'21.448"`, Jean Meeus Ch. 22).
pub const DEFAULT_EQUATOR_FADE_LATITUDE_DEG: f64 = 23.439291;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SeasonError {
    #[error("invalid season configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to parse model configuration: {0}")]
    ModelParse(String),
}

/// Tunable season parameters loaded from `[season]` in `data/model.toml`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeasonConfig {
    pub equator_fade_latitude_deg: f64,
}

#[derive(Deserialize)]
struct ModelTomlSeasonRoot {
    season: SeasonConfig,
}

impl Default for SeasonConfig {
    fn default() -> Self {
        Self {
            equator_fade_latitude_deg: DEFAULT_EQUATOR_FADE_LATITUDE_DEG,
        }
    }
}

impl SeasonConfig {
    pub fn validate(&self) -> Result<(), SeasonError> {
        if !self.equator_fade_latitude_deg.is_finite()
            || !(0.0..=90.0).contains(&self.equator_fade_latitude_deg)
            || self.equator_fade_latitude_deg == 0.0
        {
            return Err(SeasonError::InvalidConfig(
                "equator_fade_latitude_deg must be finite and within (0.0, 90.0]",
            ));
        }
        Ok(())
    }

    pub fn from_model_toml(toml_str: &str) -> Result<Self, SeasonError> {
        let root: ModelTomlSeasonRoot =
            toml::from_str(toml_str).map_err(|err| SeasonError::ModelParse(err.to_string()))?;
        root.season.validate()?;
        Ok(root.season)
    }
}

/// Named astronomical quarter of the hemisphere-adjusted season cycle `s`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spring => "spring",
            Self::Summer => "summer",
            Self::Autumn => "autumn",
            Self::Winter => "winter",
        }
    }

    #[must_use]
    pub fn from_phase(phase: f64) -> Self {
        let s = phase.rem_euclid(1.0);
        if s < 0.25 {
            Self::Spring
        } else if s < 0.50 {
            Self::Summer
        } else if s < 0.75 {
            Self::Autumn
        } else {
            Self::Winter
        }
    }
}

impl fmt::Display for Season {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pure season model output for a [`UtcInstant`] and [`Coordinates`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeasonState {
    /// Hemisphere-adjusted season phase `s` in `[0.0, 1.0)` (`0.0` spring equinox, `0.25` summer
    /// solstice, `0.50` autumn equinox, `0.75` winter solstice).
    pub phase: f64,
    /// Equator amplitude fade in `[0.0, 1.0]` (`0.0` at `0°` latitude, `1.0` at and beyond
    /// `equator_fade_latitude_deg`).
    pub strength: f64,
    /// Named season quarter corresponding to `phase`.
    pub season: Season,
}

/// Computes the smooth Hermite (`3x^2 - 2x^3`) equator-fade amplitude in `[0.0, 1.0]` for
/// `latitude_deg`.
#[must_use]
pub fn equator_season_strength(latitude_deg: f64, config: &SeasonConfig) -> f64 {
    let span = config.equator_fade_latitude_deg;
    if span <= 0.0 {
        return 1.0;
    }
    let x = (latitude_deg.abs() / span).clamp(0.0, 1.0);
    let smooth = x * x * (3.0 - 2.0 * x);
    if smooth == 0.0 {
        0.0
    } else {
        smooth
    }
}

/// Computes the hemisphere-adjusted season phase `s` and equator fade strength at `instant` and
/// `coords` (`PLAN.md` §4).
#[must_use]
pub fn season_state(
    instant: UtcInstant,
    coords: Coordinates,
    config: &SeasonConfig,
) -> SeasonState {
    let north_phase = apparent_solar_longitude_deg(instant) / 360.0;
    let raw_phase = if coords.latitude_deg() < 0.0 {
        (north_phase + 0.5).rem_euclid(1.0)
    } else {
        north_phase.rem_euclid(1.0)
    };
    let phase = if raw_phase >= 1.0 || raw_phase == 0.0 {
        0.0
    } else {
        raw_phase
    };
    let strength = equator_season_strength(coords.latitude_deg(), config);
    SeasonState {
        phase,
        strength,
        season: Season::from_phase(phase),
    }
}
