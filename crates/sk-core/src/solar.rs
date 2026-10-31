//! Pure NOAA solar position, SunCalc twilight/golden-hour events, and polar state calculations.
//!
//! References:
//! - NOAA GML Solar Calculation Details: <https://gml.noaa.gov/grad/solcalc/calcdetails.html>
//! - Jean Meeus, *Astronomical Algorithms* (2nd ed., Willmann-Bell, 1998)
//! - Vladimir Agafonkin, `mourner/suncalc` event altitude thresholds

use crate::time::{validated_days_from_civil, TimeError, UtcInstant, SECS_PER_DAY};
use core::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Embedded repository `data/model.toml` containing tunable solar and colour parameters.
pub const DEFAULT_MODEL_TOML: &str = include_str!("../../../data/model.toml");

const J2000_UNIX_DAYS: f64 = 10_957.5;
const DAYS_PER_JULIAN_CENTURY: f64 = 36_525.0;
const MINUTES_PER_DAY: f64 = 1_440.0;
const SECS_PER_MINUTE_F64: f64 = 60.0;
const SECS_PER_DAY_F64: f64 = SECS_PER_DAY as f64;
const SECS_PER_DEGREE_LONGITUDE: f64 = 240.0;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SolarError {
    #[error("latitude must be finite and within [-90, 90] degrees")]
    LatitudeOutOfRange,
    #[error("longitude must be finite and within [-180, 180] degrees")]
    LongitudeOutOfRange,
    #[error("invalid solar event angle configuration: {0}")]
    InvalidEventAngles(&'static str),
    #[error("failed to parse model configuration: {0}")]
    ModelParse(String),
    #[error("invalid calendar date: {0}")]
    InvalidDate(#[from] TimeError),
}

/// Validated geographic coordinates (`latitude_deg` in `[-90, 90]`, `longitude_deg` in `[-180, 180]`).
///
/// Privacy (`PLAN.md` §0 #4): `Debug` output redacts numeric coordinate values so coordinates
/// never leak into logs, panic messages, or error chains.
#[derive(Copy, Clone, PartialEq)]
pub struct Coordinates {
    latitude_deg: f64,
    longitude_deg: f64,
}

impl Coordinates {
    pub fn new(latitude_deg: f64, longitude_deg: f64) -> Result<Self, SolarError> {
        if !latitude_deg.is_finite() || !(-90.0..=90.0).contains(&latitude_deg) {
            return Err(SolarError::LatitudeOutOfRange);
        }
        if !longitude_deg.is_finite() || !(-180.0..=180.0).contains(&longitude_deg) {
            return Err(SolarError::LongitudeOutOfRange);
        }
        Ok(Self {
            latitude_deg: if latitude_deg == 0.0 {
                0.0
            } else {
                latitude_deg
            },
            longitude_deg: if longitude_deg == 0.0 {
                0.0
            } else {
                longitude_deg
            },
        })
    }

    #[must_use]
    pub const fn latitude_deg(self) -> f64 {
        self.latitude_deg
    }

    #[must_use]
    pub const fn longitude_deg(self) -> f64 {
        self.longitude_deg
    }
}

impl fmt::Debug for Coordinates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Coordinates")
            .field("latitude_deg", &"<redacted>")
            .field("longitude_deg", &"<redacted>")
            .finish()
    }
}

/// Tunable geometric sun-center altitude thresholds (in degrees) for solar events (`data/model.toml`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolarEventAngles {
    pub astronomical_twilight_deg: f64,
    pub nautical_twilight_deg: f64,
    pub civil_twilight_deg: f64,
    pub sunrise_sunset_deg: f64,
    pub golden_hour_deg: f64,
}

impl Default for SolarEventAngles {
    fn default() -> Self {
        Self {
            astronomical_twilight_deg: -18.0,
            nautical_twilight_deg: -12.0,
            civil_twilight_deg: -6.0,
            sunrise_sunset_deg: -0.833,
            golden_hour_deg: 6.0,
        }
    }
}

#[derive(Deserialize)]
struct ModelTomlRoot {
    solar: ModelTomlSolar,
}

#[derive(Deserialize)]
struct ModelTomlSolar {
    event_angles: SolarEventAngles,
}

impl SolarEventAngles {
    pub fn validate(&self) -> Result<(), SolarError> {
        let ordered = [
            self.astronomical_twilight_deg,
            self.nautical_twilight_deg,
            self.civil_twilight_deg,
            self.sunrise_sunset_deg,
            self.golden_hour_deg,
        ];
        if ordered
            .iter()
            .any(|angle| !angle.is_finite() || !(-90.0..=90.0).contains(angle))
        {
            return Err(SolarError::InvalidEventAngles(
                "all event angles must be finite and within [-90, 90] degrees",
            ));
        }
        if !ordered.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(SolarError::InvalidEventAngles(
                "event angles must be strictly ascending from astronomical twilight to golden hour",
            ));
        }
        Ok(())
    }

    pub fn from_model_toml(toml_str: &str) -> Result<Self, SolarError> {
        let root: ModelTomlRoot =
            toml::from_str(toml_str).map_err(|err| SolarError::ModelParse(err.to_string()))?;
        root.solar.event_angles.validate()?;
        Ok(root.solar.event_angles)
    }
}

/// Result of solving for when the sun crosses a target altitude during a solar half-day.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "at", rename_all = "snake_case")]
pub enum AltitudeCrossing {
    /// Sun crosses the target altitude at this UTC instant.
    At(UtcInstant),
    /// Sun remains strictly above the target altitude throughout the half-day.
    AlwaysAbove,
    /// Sun remains strictly below the target altitude throughout the half-day.
    AlwaysBelow,
}

impl AltitudeCrossing {
    #[must_use]
    pub const fn instant(self) -> Option<UtcInstant> {
        match self {
            Self::At(instant) => Some(instant),
            Self::AlwaysAbove | Self::AlwaysBelow => None,
        }
    }
}

/// Diurnal horizon regime for a local solar day.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolarState {
    /// At least one horizon crossing (sunrise and/or sunset) occurs during the solar day.
    Normal,
    /// Midnight sun: the sun remains above the sunrise/sunset horizon all solar day.
    PolarDay,
    /// Polar night: the sun remains below the sunrise/sunset horizon all solar day.
    PolarNight,
}

/// Named solar milestones in chronological order within a local solar day.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolarEventKind {
    SolarNadir,
    AstronomicalDawn,
    NauticalDawn,
    CivilDawn,
    Sunrise,
    GoldenHourEnd,
    SolarNoon,
    GoldenHourStart,
    Sunset,
    CivilDusk,
    NauticalDusk,
    AstronomicalDusk,
}

/// Full set of solar events for one local solar day.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolarEvents {
    pub solar_nadir: UtcInstant,
    pub astronomical_dawn: AltitudeCrossing,
    pub nautical_dawn: AltitudeCrossing,
    pub civil_dawn: AltitudeCrossing,
    pub sunrise: AltitudeCrossing,
    pub golden_hour_end: AltitudeCrossing,
    pub solar_noon: UtcInstant,
    pub golden_hour_start: AltitudeCrossing,
    pub sunset: AltitudeCrossing,
    pub civil_dusk: AltitudeCrossing,
    pub nautical_dusk: AltitudeCrossing,
    pub astronomical_dusk: AltitudeCrossing,
}

impl SolarEvents {
    /// Returns all events that occur at a concrete [`UtcInstant`] on this solar day in
    /// chronological definition order.
    #[must_use]
    pub fn ordered_instants(&self) -> Vec<(SolarEventKind, UtcInstant)> {
        let mut out = Vec::with_capacity(12);
        out.push((SolarEventKind::SolarNadir, self.solar_nadir));
        for (kind, crossing) in [
            (SolarEventKind::AstronomicalDawn, self.astronomical_dawn),
            (SolarEventKind::NauticalDawn, self.nautical_dawn),
            (SolarEventKind::CivilDawn, self.civil_dawn),
            (SolarEventKind::Sunrise, self.sunrise),
            (SolarEventKind::GoldenHourEnd, self.golden_hour_end),
        ] {
            if let AltitudeCrossing::At(at) = crossing {
                out.push((kind, at));
            }
        }
        out.push((SolarEventKind::SolarNoon, self.solar_noon));
        for (kind, crossing) in [
            (SolarEventKind::GoldenHourStart, self.golden_hour_start),
            (SolarEventKind::Sunset, self.sunset),
            (SolarEventKind::CivilDusk, self.civil_dusk),
            (SolarEventKind::NauticalDusk, self.nautical_dusk),
            (SolarEventKind::AstronomicalDusk, self.astronomical_dusk),
        ] {
            if let AltitudeCrossing::At(at) = crossing {
                out.push((kind, at));
            }
        }
        out
    }

    /// Returns the earliest event on this solar day strictly after `instant`, if any.
    #[must_use]
    pub fn next_event_after(&self, instant: UtcInstant) -> Option<(SolarEventKind, UtcInstant)> {
        self.ordered_instants()
            .into_iter()
            .find(|(_, at)| *at > instant)
    }
}

/// Instantaneous NOAA solar position at a given [`UtcInstant`] and [`Coordinates`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolarPosition {
    /// Refraction-corrected apparent solar elevation above the horizon, in degrees.
    pub elevation_deg: f64,
    /// Exoatmospheric (unrefracted) geometric solar elevation, in degrees.
    pub geometric_elevation_deg: f64,
    /// Solar azimuth clockwise from true North (`[0.0, 360.0)`), in degrees.
    pub azimuth_deg: f64,
    /// Apparent solar declination, in degrees.
    pub declination_deg: f64,
    /// Equation of time, in minutes.
    pub equation_of_time_min: f64,
    /// Local true solar hour angle in `[-180.0, 180.0)`, in degrees (`< 0` morning, `>= 0` afternoon).
    pub hour_angle_deg: f64,
    /// `true` during the ascending half of the local solar day (`hour_angle_deg < 0.0`).
    pub is_morning: bool,
}

/// Solar culmination elevations, polar regime, and milestone events for one local solar day.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolarDay {
    /// Local solar day count since `1970-01-01`.
    pub solar_day_index: i64,
    /// Refraction-corrected apparent elevation at `events.solar_noon`, in degrees.
    pub noon_elevation_deg: f64,
    /// Refraction-corrected apparent elevation at `events.solar_nadir`, in degrees.
    pub nadir_elevation_deg: f64,
    /// Normal rise/set day, midnight sun, or polar night.
    pub polar_state: PolarState,
    /// Dawn, sunrise, golden hour, noon, sunset, dusk, and nadir events.
    pub events: SolarEvents,
}

/// Complete solar model output (`PLAN.md` §4) for a single [`UtcInstant`] and [`Coordinates`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolarSnapshot {
    pub instant: UtcInstant,
    pub position: SolarPosition,
    /// Apparent refraction-corrected elevation (`position.elevation_deg`), in degrees.
    pub elevation_deg: f64,
    /// Ascending (`true`) vs descending (`false`) half of the local solar day.
    pub is_morning: bool,
    /// Apparent elevation at local solar noon of the enclosing local solar day, in degrees.
    pub noon_elevation_deg: f64,
    /// Apparent elevation at local solar nadir of the enclosing local solar day, in degrees.
    pub nadir_elevation_deg: f64,
    /// Horizon crossing regime of the enclosing local solar day.
    pub polar_state: PolarState,
    /// Solar events of the enclosing local solar day.
    pub events: SolarEvents,
}

#[derive(Copy, Clone)]
struct OrbitalElements {
    apparent_longitude_deg: f64,
    declination_deg: f64,
    equation_of_time_min: f64,
}

#[derive(Copy, Clone)]
enum HourAngleSolution {
    Crossing(f64),
    AlwaysAbove,
    AlwaysBelow,
}

fn julian_centuries_from_unix_secs(unix_secs: f64) -> f64 {
    ((unix_secs / SECS_PER_DAY_F64) - J2000_UNIX_DAYS) / DAYS_PER_JULIAN_CENTURY
}

fn julian_centuries_from_day_offset(day_index: i64, offset_minutes: f64) -> f64 {
    (((day_index as f64) - J2000_UNIX_DAYS) + (offset_minutes / MINUTES_PER_DAY))
        / DAYS_PER_JULIAN_CENTURY
}

/// NOAA GML / Meeus solar declination and equation of time at Julian century `t` from J2000.0.
fn orbital_elements(t: f64) -> OrbitalElements {
    let geom_mean_long_deg = (280.46646 + t * (36_000.769_83 + t * 0.000_303_2)).rem_euclid(360.0);
    let geom_mean_anom_deg = 357.52911 + t * (35_999.050_29 - 0.000_153_7 * t);
    let eccentricity = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);

    let m_rad = geom_mean_anom_deg.to_radians();
    let sin_m = m_rad.sin();
    let sin_2m = (2.0 * m_rad).sin();
    let sin_3m = (3.0 * m_rad).sin();

    let eq_of_center_deg = sin_m * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + sin_2m * (0.019_993 - 0.000_101 * t)
        + sin_3m * 0.000_289;

    let sun_true_long_deg = geom_mean_long_deg + eq_of_center_deg;
    let omega_rad = (125.04 - 1_934.136 * t).to_radians();
    let apparent_longitude_deg =
        (sun_true_long_deg - 0.00569 - 0.00478 * omega_rad.sin()).rem_euclid(360.0);
    let sun_apparent_long_rad = apparent_longitude_deg.to_radians();

    let obliq_seconds = 21.448 - t * (46.8150 + t * (0.00059 - t * 0.001_813));
    let mean_obliq_deg = 23.0 + (26.0 + (obliq_seconds / 60.0)) / 60.0;
    let obliq_corr_rad = (mean_obliq_deg + 0.00256 * omega_rad.cos()).to_radians();

    let sin_dec = (obliq_corr_rad.sin() * sun_apparent_long_rad.sin()).clamp(-1.0, 1.0);
    let declination_deg = sin_dec.asin().to_degrees();

    let y = (0.5 * obliq_corr_rad).tan().powi(2);
    let l0_rad = geom_mean_long_deg.to_radians();
    let sin_2l0 = (2.0 * l0_rad).sin();
    let cos_2l0 = (2.0 * l0_rad).cos();
    let sin_4l0 = (4.0 * l0_rad).sin();

    let etime_rad = y * sin_2l0 - 2.0 * eccentricity * sin_m
        + 4.0 * eccentricity * y * sin_m * cos_2l0
        - 0.5 * y * y * sin_4l0
        - 1.25 * eccentricity * eccentricity * sin_2m;
    let equation_of_time_min = 4.0 * etime_rad.to_degrees();

    OrbitalElements {
        apparent_longitude_deg,
        declination_deg,
        equation_of_time_min,
    }
}

/// Returns the Sun's apparent geocentric ecliptic longitude in `[0.0, 360.0)` degrees (`0°` March
/// equinox, `90°` June solstice, `180°` September equinox, `270°` December solstice).
#[must_use]
pub fn apparent_solar_longitude_deg(instant: UtcInstant) -> f64 {
    let t = julian_centuries_from_unix_secs(instant.as_unix_secs() as f64);
    let lon = orbital_elements(t).apparent_longitude_deg;
    if lon >= 360.0 || lon == 0.0 {
        0.0
    } else {
        lon
    }
}

/// NOAA atmospheric refraction correction (in degrees) from exoatmospheric geometric elevation.
fn atmospheric_refraction_deg(exoatm_elev_deg: f64) -> f64 {
    if exoatm_elev_deg > 85.0 {
        return 0.0;
    }

    let te = exoatm_elev_deg.to_radians().tan();
    let correction_arcsec = if exoatm_elev_deg > 5.0 {
        58.1 / te - 0.07 / te.powi(3) + 0.000_086 / te.powi(5)
    } else if exoatm_elev_deg > -0.575 {
        let e = exoatm_elev_deg;
        1_735.0 + e * (-518.2 + e * (103.4 + e * (-12.79 + e * 0.711)))
    } else if te.abs() < 1e-12 {
        0.0
    } else {
        -20.774 / te
    };

    correction_arcsec / 3_600.0
}

fn solar_position_at_unix_f64(unix_secs: f64, coords: Coordinates) -> SolarPosition {
    let t = julian_centuries_from_unix_secs(unix_secs);
    let orb = orbital_elements(t);

    let utc_minutes = unix_secs.rem_euclid(SECS_PER_DAY_F64) / SECS_PER_MINUTE_F64;
    let true_solar_time_min =
        (utc_minutes + orb.equation_of_time_min + 4.0 * coords.longitude_deg())
            .rem_euclid(MINUTES_PER_DAY);
    let mut hour_angle_deg = true_solar_time_min / 4.0 - 180.0;
    if hour_angle_deg >= 180.0 {
        hour_angle_deg -= 360.0;
    }
    let is_morning = hour_angle_deg < 0.0;

    let lat_rad = coords.latitude_deg().to_radians();
    let dec_rad = orb.declination_deg.to_radians();
    let ha_rad = hour_angle_deg.to_radians();

    let csz = (lat_rad.sin() * dec_rad.sin() + lat_rad.cos() * dec_rad.cos() * ha_rad.cos())
        .clamp(-1.0, 1.0);
    let zenith_deg = csz.acos().to_degrees();
    let geometric_elevation_deg = 90.0 - zenith_deg;
    let elevation_deg =
        geometric_elevation_deg + atmospheric_refraction_deg(geometric_elevation_deg);

    let zenith_rad = zenith_deg.to_radians();
    let az_denom = lat_rad.cos() * zenith_rad.sin();
    let azimuth_deg = if az_denom.abs() > 0.001 {
        let az_cos =
            ((lat_rad.sin() * zenith_rad.cos() - dec_rad.sin()) / az_denom).clamp(-1.0, 1.0);
        let mut az = 180.0 - az_cos.acos().to_degrees();
        if hour_angle_deg > 0.0 {
            az = -az;
        }
        let wrapped = az.rem_euclid(360.0);
        if wrapped >= 360.0 {
            0.0
        } else {
            wrapped
        }
    } else if coords.latitude_deg() > 0.0 {
        180.0
    } else {
        0.0
    };

    SolarPosition {
        elevation_deg,
        geometric_elevation_deg,
        azimuth_deg,
        declination_deg: orb.declination_deg,
        equation_of_time_min: orb.equation_of_time_min,
        hour_angle_deg,
        is_morning,
    }
}

/// Computes the instantaneous NOAA solar position for `instant` at `coords`.
#[must_use]
pub fn solar_position(instant: UtcInstant, coords: Coordinates) -> SolarPosition {
    solar_position_at_unix_f64(instant.as_unix_secs() as f64, coords)
}

/// Pole-safe solver for `cos(H) = (sin(alpha) - sin(lat)*sin(dec)) / (cos(lat)*cos(dec))`.
fn hour_angle_for_altitude_rad(
    latitude_deg: f64,
    declination_deg: f64,
    altitude_deg: f64,
) -> HourAngleSolution {
    let alpha_rad = altitude_deg.to_radians();
    let lat_rad = latitude_deg.to_radians();
    let dec_rad = declination_deg.to_radians();

    let num = alpha_rad.sin() - lat_rad.sin() * dec_rad.sin();
    let den = lat_rad.cos() * dec_rad.cos();

    if den.abs() < 1e-12 {
        if num <= 0.0 {
            HourAngleSolution::AlwaysAbove
        } else {
            HourAngleSolution::AlwaysBelow
        }
    } else {
        let cos_h = num / den;
        if cos_h < -1.0 {
            HourAngleSolution::AlwaysAbove
        } else if cos_h > 1.0 {
            HourAngleSolution::AlwaysBelow
        } else {
            HourAngleSolution::Crossing(cos_h.acos())
        }
    }
}

fn solve_altitude_crossing(
    solar_day_index: i64,
    noon_offset_min: f64,
    coords: Coordinates,
    altitude_deg: f64,
    is_rising: bool,
) -> AltitudeCrossing {
    let day_start_unix = (solar_day_index as f64) * SECS_PER_DAY_F64;
    let mut offset_min = noon_offset_min;

    for _ in 0..3 {
        let t = julian_centuries_from_day_offset(solar_day_index, offset_min);
        let orb = orbital_elements(t);
        match hour_angle_for_altitude_rad(coords.latitude_deg(), orb.declination_deg, altitude_deg)
        {
            HourAngleSolution::AlwaysAbove => return AltitudeCrossing::AlwaysAbove,
            HourAngleSolution::AlwaysBelow => return AltitudeCrossing::AlwaysBelow,
            HourAngleSolution::Crossing(h_rad) => {
                let signed_ha_deg = if is_rising {
                    h_rad.to_degrees()
                } else {
                    -h_rad.to_degrees()
                };
                offset_min = 720.0
                    - 4.0 * (coords.longitude_deg() + signed_ha_deg)
                    - orb.equation_of_time_min;
            }
        }
    }

    let event_unix_secs = (day_start_unix + offset_min * SECS_PER_MINUTE_F64).round() as i64;
    AltitudeCrossing::At(UtcInstant::from_unix_secs(event_unix_secs))
}

fn solar_day_for_index(
    solar_day_index: i64,
    coords: Coordinates,
    angles: &SolarEventAngles,
) -> SolarDay {
    let lon_deg = coords.longitude_deg();
    let day_start_unix = (solar_day_index as f64) * SECS_PER_DAY_F64;

    // 3-pass iterative refinement for local true solar noon (meridian transit, H = 0 deg).
    let mut noon_offset_min = 720.0 - 4.0 * lon_deg;
    for _ in 0..3 {
        let t_noon = julian_centuries_from_day_offset(solar_day_index, noon_offset_min);
        let eq_min = orbital_elements(t_noon).equation_of_time_min;
        noon_offset_min = 720.0 - 4.0 * lon_deg - eq_min;
    }
    let noon_unix_f64 = day_start_unix + noon_offset_min * SECS_PER_MINUTE_F64;
    let solar_noon = UtcInstant::from_unix_secs(noon_unix_f64.round() as i64);
    let noon_elevation_deg = solar_position_at_unix_f64(noon_unix_f64, coords).elevation_deg;

    // 3-pass iterative refinement for local true solar nadir at the start of this solar day (H = -180 deg).
    let mut nadir_offset_min = -4.0 * lon_deg;
    for _ in 0..3 {
        let t_nadir = julian_centuries_from_day_offset(solar_day_index, nadir_offset_min);
        let eq_min = orbital_elements(t_nadir).equation_of_time_min;
        nadir_offset_min = -4.0 * lon_deg - eq_min;
    }
    let nadir_unix_f64 = day_start_unix + nadir_offset_min * SECS_PER_MINUTE_F64;
    let solar_nadir = UtcInstant::from_unix_secs(nadir_unix_f64.round() as i64);
    let nadir_elevation_deg = solar_position_at_unix_f64(nadir_unix_f64, coords).elevation_deg;

    let astronomical_dawn = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.astronomical_twilight_deg,
        true,
    );
    let nautical_dawn = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.nautical_twilight_deg,
        true,
    );
    let civil_dawn = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.civil_twilight_deg,
        true,
    );
    let sunrise = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.sunrise_sunset_deg,
        true,
    );
    let golden_hour_end = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.golden_hour_deg,
        true,
    );

    let golden_hour_start = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.golden_hour_deg,
        false,
    );
    let sunset = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.sunrise_sunset_deg,
        false,
    );
    let civil_dusk = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.civil_twilight_deg,
        false,
    );
    let nautical_dusk = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.nautical_twilight_deg,
        false,
    );
    let astronomical_dusk = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.astronomical_twilight_deg,
        false,
    );

    let polar_state = match (sunrise, sunset) {
        (AltitudeCrossing::AlwaysAbove, AltitudeCrossing::AlwaysAbove) => PolarState::PolarDay,
        (AltitudeCrossing::AlwaysBelow, AltitudeCrossing::AlwaysBelow) => PolarState::PolarNight,
        (AltitudeCrossing::At(_), _) | (_, AltitudeCrossing::At(_)) => PolarState::Normal,
        _ => {
            if noon_elevation_deg >= 0.0 {
                PolarState::PolarDay
            } else {
                PolarState::PolarNight
            }
        }
    };

    SolarDay {
        solar_day_index,
        noon_elevation_deg,
        nadir_elevation_deg,
        polar_state,
        events: SolarEvents {
            solar_nadir,
            astronomical_dawn,
            nautical_dawn,
            civil_dawn,
            sunrise,
            golden_hour_end,
            solar_noon,
            golden_hour_start,
            sunset,
            civil_dusk,
            nautical_dusk,
            astronomical_dusk,
        },
    }
}

/// Computes solar noon, nadir, polar state, and twilight/sunrise/golden-hour events for the
/// local solar day anchored at proleptic Gregorian calendar date `(year, month, day)`.
pub fn solar_day_for_date(
    year: i32,
    month: u8,
    day: u8,
    coords: Coordinates,
    angles: &SolarEventAngles,
) -> Result<SolarDay, SolarError> {
    angles.validate()?;
    let solar_day_index = validated_days_from_civil(year, month, day)?;
    Ok(solar_day_for_index(solar_day_index, coords, angles))
}

pub(crate) fn solar_day_index_at(
    instant: UtcInstant,
    coords: Coordinates,
    position: &SolarPosition,
) -> i64 {
    let true_solar_secs = (instant.as_unix_secs() as f64)
        + coords.longitude_deg() * SECS_PER_DEGREE_LONGITUDE
        + position.equation_of_time_min * SECS_PER_MINUTE_F64;
    (true_solar_secs / SECS_PER_DAY_F64).floor() as i64
}

pub(crate) fn solar_day_summary_for_index(
    solar_day_index: i64,
    coords: Coordinates,
    angles: &SolarEventAngles,
) -> (f64, PolarState) {
    let lon_deg = coords.longitude_deg();
    let day_start_unix = (solar_day_index as f64) * SECS_PER_DAY_F64;

    let mut noon_offset_min = 720.0 - 4.0 * lon_deg;
    for _ in 0..3 {
        let t_noon = julian_centuries_from_day_offset(solar_day_index, noon_offset_min);
        let eq_min = orbital_elements(t_noon).equation_of_time_min;
        noon_offset_min = 720.0 - 4.0 * lon_deg - eq_min;
    }
    let noon_unix_f64 = day_start_unix + noon_offset_min * SECS_PER_MINUTE_F64;
    let noon_elevation_deg = solar_position_at_unix_f64(noon_unix_f64, coords).elevation_deg;

    let sunrise = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.sunrise_sunset_deg,
        true,
    );
    let sunset = solve_altitude_crossing(
        solar_day_index,
        noon_offset_min,
        coords,
        angles.sunrise_sunset_deg,
        false,
    );
    let polar_state = match (sunrise, sunset) {
        (AltitudeCrossing::AlwaysAbove, AltitudeCrossing::AlwaysAbove) => PolarState::PolarDay,
        (AltitudeCrossing::AlwaysBelow, AltitudeCrossing::AlwaysBelow) => PolarState::PolarNight,
        (AltitudeCrossing::At(_), _) | (_, AltitudeCrossing::At(_)) => PolarState::Normal,
        _ => {
            if noon_elevation_deg >= 0.0 {
                PolarState::PolarDay
            } else {
                PolarState::PolarNight
            }
        }
    };

    (noon_elevation_deg, polar_state)
}

/// Computes the instantaneous solar position together with the enclosing local solar day's
/// `noon_elevation_deg`, `polar_state`, and milestone `events` (`PLAN.md` §4).
#[must_use]
pub fn solar_snapshot(
    instant: UtcInstant,
    coords: Coordinates,
    angles: &SolarEventAngles,
) -> SolarSnapshot {
    let position = solar_position(instant, coords);
    let solar_day_index = solar_day_index_at(instant, coords, &position);
    let day = solar_day_for_index(solar_day_index, coords, angles);

    SolarSnapshot {
        instant,
        position,
        elevation_deg: position.elevation_deg,
        is_morning: position.is_morning,
        noon_elevation_deg: day.noon_elevation_deg,
        nadir_elevation_deg: day.nadir_elevation_deg,
        polar_state: day.polar_state,
        events: day.events,
    }
}
