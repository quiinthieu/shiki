//! Pure manual coordinate validation/rounding and vendored IANA tzdb (`zone1970.tab` + `backward`)
//! resolution (`PLAN.md` §0 #3–#4, §4, T04).

use crate::product;
use crate::solar::Coordinates;
use core::fmt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use thiserror::Error;

pub const VENDORED_TZDB_VERSION_FILE: &str = include_str!("../../../data/tz/VERSION");
pub const VENDORED_ZONE1970_TAB: &str = include_str!("../../../data/tz/zone1970.tab");
pub const VENDORED_BACKWARD: &str = include_str!("../../../data/tz/backward");
pub const MANUAL_COORD_DECIMALS: u32 = 1;

static BUNDLED_TZDB: OnceLock<TzDb> = OnceLock::new();

/// Returns the IANA tzdb release identifier recorded in `data/tz/VERSION` (e.g. `"2026f"`).
#[must_use]
pub fn vendored_tzdb_version() -> &'static str {
    VENDORED_TZDB_VERSION_FILE
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
}

/// Errors produced during coordinate validation or IANA timezone lookup.
///
/// Privacy (`PLAN.md` §0 #4): coordinate error variants intentionally carry no coordinate floats
/// or raw coordinate substrings so coordinates can never leak into logs or error output.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LocationError {
    #[error("latitude must be finite and within [-90, 90] degrees")]
    LatitudeOutOfRange,
    #[error("longitude must be finite and within [-180, 180] degrees")]
    LongitudeOutOfRange,
    #[error("coordinates must be formatted as `<LAT>,<LON>` in decimal degrees")]
    InvalidCoordinatePairFormat,
    #[error("latitude is not a valid decimal number")]
    InvalidLatitudeNumber,
    #[error("longitude is not a valid decimal number")]
    InvalidLongitudeNumber,
    #[error(
        "manual location mode requires both `latitude` and `longitude` in config.toml (or run `{} locate --set <LAT,LON>`)",
        product::CLI_BIN
    )]
    MissingManualCoordinates,
    #[error(
        "timezone name cannot be empty; set coordinates manually with `{} locate --set <LAT,LON>`",
        product::CLI_BIN
    )]
    EmptyTimezone,
    #[error(
        "unknown or non-geographic timezone '{zone}'; set coordinates manually with `{} locate --set <LAT,LON>`",
        product::CLI_BIN
    )]
    UnknownTimezone { zone: String },
    #[error(
        "unable to detect system IANA timezone; set coordinates manually with `{} locate --set <LAT,LON>`",
        product::CLI_BIN
    )]
    MissingSystemTimezone,
    #[error("timezone alias cycle detected while resolving '{zone}'")]
    AliasCycle { zone: String },
    #[error("failed to parse {table} at line {line}: {reason}")]
    TzTableParse {
        table: &'static str,
        line: usize,
        reason: &'static str,
    },
}

/// Rounds a validated coordinate in decimal degrees to 1 decimal place (`0.1°`), normalizing
/// `-0.0` to `+0.0` (`PLAN.md` §4).
#[must_use]
pub fn round_manual_coord(deg: f64) -> f64 {
    let rounded = (deg * 10.0).round() / 10.0;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

/// Validated manual coordinates rounded to `0.1°` (`PLAN.md` §4).
///
/// Privacy (`PLAN.md` §0 #4): `Debug` redacts numeric coordinate values.
#[derive(Copy, Clone, PartialEq)]
pub struct ManualLocation {
    coords: Coordinates,
}

impl ManualLocation {
    /// Validates `latitude_deg` in `[-90, 90]` and `longitude_deg` in `[-180, 180]` before
    /// rounding both components to 1 decimal place (`0.1°`).
    pub fn new(latitude_deg: f64, longitude_deg: f64) -> Result<Self, LocationError> {
        if !latitude_deg.is_finite() || !(-90.0..=90.0).contains(&latitude_deg) {
            return Err(LocationError::LatitudeOutOfRange);
        }
        if !longitude_deg.is_finite() || !(-180.0..=180.0).contains(&longitude_deg) {
            return Err(LocationError::LongitudeOutOfRange);
        }

        let lat_rounded = round_manual_coord(latitude_deg);
        let lon_rounded = round_manual_coord(longitude_deg);
        let coords = Coordinates::new(lat_rounded, lon_rounded).map_err(|err| match err {
            crate::solar::SolarError::LatitudeOutOfRange => LocationError::LatitudeOutOfRange,
            _ => LocationError::LongitudeOutOfRange,
        })?;
        Ok(Self { coords })
    }

    /// Parses a `"<LAT>,<LON>"` pair in decimal degrees, validating bounds before rounding to
    /// 1 decimal place.
    pub fn parse(pair: &str) -> Result<Self, LocationError> {
        let trimmed = pair.trim();
        let (lat_raw, lon_raw) = trimmed
            .split_once(',')
            .ok_or(LocationError::InvalidCoordinatePairFormat)?;
        if lon_raw.contains(',') {
            return Err(LocationError::InvalidCoordinatePairFormat);
        }

        let lat_str = lat_raw.trim();
        let lon_str = lon_raw.trim();
        if lat_str.is_empty() || lon_str.is_empty() {
            return Err(LocationError::InvalidCoordinatePairFormat);
        }

        let lat = lat_str
            .parse::<f64>()
            .map_err(|_| LocationError::InvalidLatitudeNumber)?;
        let lon = lon_str
            .parse::<f64>()
            .map_err(|_| LocationError::InvalidLongitudeNumber)?;
        Self::new(lat, lon)
    }

    #[must_use]
    pub const fn coordinates(self) -> Coordinates {
        self.coords
    }

    #[must_use]
    pub const fn latitude_deg(self) -> f64 {
        self.coords.latitude_deg()
    }

    #[must_use]
    pub const fn longitude_deg(self) -> f64 {
        self.coords.longitude_deg()
    }

    /// Formats the rounded coordinates as `"<LAT>,<LON>"` with 1 decimal place.
    #[must_use]
    pub fn format_pair(self) -> String {
        format!(
            "{:.1},{:.1}",
            round_manual_coord(self.latitude_deg()),
            round_manual_coord(self.longitude_deg())
        )
    }
}

impl fmt::Debug for ManualLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualLocation")
            .field("latitude_deg", &"<redacted>")
            .field("longitude_deg", &"<redacted>")
            .finish()
    }
}

fn parse_iso6709_component(
    part: &str,
    deg_digits: usize,
    max_deg: u32,
    line: usize,
) -> Result<f64, LocationError> {
    let bytes = part.as_bytes();
    let sign = match bytes.first().copied() {
        Some(b'+') => 1.0,
        Some(b'-') => -1.0,
        _ => {
            return Err(LocationError::TzTableParse {
                table: "zone1970.tab",
                line,
                reason: "ISO 6709 coordinate component must begin with '+' or '-'",
            });
        }
    };

    let digits = &part[1..];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "ISO 6709 coordinate component contains non-digit characters",
        });
    }

    let expected_dm = deg_digits + 2;
    let expected_dms = deg_digits + 4;
    if digits.len() != expected_dm && digits.len() != expected_dms {
        return Err(LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "ISO 6709 coordinate component has unexpected digit count",
        });
    }

    let deg: u32 = digits[..deg_digits]
        .parse()
        .map_err(|_| LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "invalid degree digits in ISO 6709 coordinate",
        })?;
    let min: u32 =
        digits[deg_digits..deg_digits + 2]
            .parse()
            .map_err(|_| LocationError::TzTableParse {
                table: "zone1970.tab",
                line,
                reason: "invalid minute digits in ISO 6709 coordinate",
            })?;
    let sec: u32 = if digits.len() == expected_dms {
        digits[deg_digits + 2..]
            .parse()
            .map_err(|_| LocationError::TzTableParse {
                table: "zone1970.tab",
                line,
                reason: "invalid second digits in ISO 6709 coordinate",
            })?
    } else {
        0
    };

    if deg > max_deg || min >= 60 || sec >= 60 || (deg == max_deg && (min > 0 || sec > 0)) {
        return Err(LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "ISO 6709 coordinate degrees/minutes/seconds out of range",
        });
    }

    let value = sign * (f64::from(deg) + f64::from(min) / 60.0 + f64::from(sec) / 3_600.0);
    Ok(if value == 0.0 { 0.0 } else { value })
}

fn parse_iso6709_at_line(raw: &str, line: usize) -> Result<Coordinates, LocationError> {
    let bytes = raw.as_bytes();
    if !matches!(bytes.first(), Some(b'+' | b'-')) {
        return Err(LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "ISO 6709 coordinates must start with '+' or '-'",
        });
    }

    let mut split_idx = None;
    for (idx, &b) in bytes.iter().enumerate().skip(1) {
        if b == b'+' || b == b'-' {
            if split_idx.is_some() {
                return Err(LocationError::TzTableParse {
                    table: "zone1970.tab",
                    line,
                    reason: "ISO 6709 coordinates contain more than two signed components",
                });
            }
            split_idx = Some(idx);
        }
    }

    let Some(split) = split_idx else {
        return Err(LocationError::TzTableParse {
            table: "zone1970.tab",
            line,
            reason: "ISO 6709 coordinates missing longitude sign",
        });
    };

    let lat = parse_iso6709_component(&raw[..split], 2, 90, line)?;
    let lon = parse_iso6709_component(&raw[split..], 3, 180, line)?;
    Coordinates::new(lat, lon).map_err(|_| LocationError::TzTableParse {
        table: "zone1970.tab",
        line,
        reason: "ISO 6709 coordinates outside geographic bounds",
    })
}

/// Parses an IANA `zone1970.tab` ISO 6709 coordinate token (`±DDMM±DDDMM` or `±DDMMSS±DDDMMSS`).
pub fn parse_iso6709_coords(raw: &str) -> Result<Coordinates, LocationError> {
    parse_iso6709_at_line(raw, 1)
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocationMode {
    #[default]
    Timezone,
    Manual,
}

impl LocationMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timezone => "timezone",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TzZoneEntry {
    pub canonical_zone: String,
    pub coordinates: Coordinates,
    pub country_codes: Vec<String>,
    pub comment: Option<String>,
}

impl TzZoneEntry {
    #[must_use]
    pub fn is_multi_country(&self) -> bool {
        self.country_codes.len() > 1
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTzLocation {
    pub requested_zone: String,
    pub canonical_zone: String,
    pub coordinates: Coordinates,
    pub country_codes: Vec<String>,
    pub comment: Option<String>,
}

impl ResolvedTzLocation {
    #[must_use]
    pub fn is_alias(&self) -> bool {
        self.requested_zone != self.canonical_zone
    }

    #[must_use]
    pub fn is_multi_country(&self) -> bool {
        self.country_codes.len() > 1
    }
}

#[derive(Clone, Debug)]
pub struct TzDb {
    version: String,
    zones: BTreeMap<String, TzZoneEntry>,
    aliases: BTreeMap<String, String>,
}

impl TzDb {
    /// Parses `zone1970.tab` and `backward` text tables into an in-memory timezone database.
    pub fn from_tables(
        version: impl Into<String>,
        zone1970_tab: &str,
        backward: &str,
    ) -> Result<Self, LocationError> {
        let mut zones = BTreeMap::new();

        for (idx, raw_line) in zone1970_tab.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw_line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() != 3 && cols.len() != 4 {
                return Err(LocationError::TzTableParse {
                    table: "zone1970.tab",
                    line: line_no,
                    reason: "expected 3 or 4 tab-separated columns",
                });
            }

            let countries_raw = cols[0].trim();
            let coords_raw = cols[1].trim();
            let tz_name = cols[2].trim();
            if countries_raw.is_empty() || tz_name.is_empty() {
                return Err(LocationError::TzTableParse {
                    table: "zone1970.tab",
                    line: line_no,
                    reason: "country codes and timezone name must be non-empty",
                });
            }

            let country_codes: Vec<String> = countries_raw
                .split(',')
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .map(str::to_string)
                .collect();
            if country_codes.is_empty() {
                return Err(LocationError::TzTableParse {
                    table: "zone1970.tab",
                    line: line_no,
                    reason: "at least one ISO 3166 country code is required",
                });
            }

            let coordinates = parse_iso6709_at_line(coords_raw, line_no)?;
            let comment = cols
                .get(3)
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .map(str::to_string);

            if zones.contains_key(tz_name) {
                return Err(LocationError::TzTableParse {
                    table: "zone1970.tab",
                    line: line_no,
                    reason: "duplicate canonical timezone entry",
                });
            }

            zones.insert(
                tz_name.to_string(),
                TzZoneEntry {
                    canonical_zone: tz_name.to_string(),
                    coordinates,
                    country_codes,
                    comment,
                },
            );
        }

        let mut aliases = BTreeMap::new();
        let mut in_zone_block = false;

        for (idx, raw_line) in backward.lines().enumerate() {
            let line_no = idx + 1;
            let without_comment = raw_line.split('#').next().unwrap_or("").trim_end();
            if without_comment.trim().is_empty() {
                continue;
            }

            if in_zone_block && without_comment.starts_with([' ', '\t']) {
                continue;
            }
            in_zone_block = false;

            let tokens: Vec<&str> = without_comment.split_whitespace().collect();
            match tokens.first().copied() {
                Some("Link") => {
                    if tokens.len() != 3 {
                        return Err(LocationError::TzTableParse {
                            table: "backward",
                            line: line_no,
                            reason: "Link directive requires TARGET and LINK-NAME",
                        });
                    }
                    let target = tokens[1];
                    let alias = tokens[2];
                    if aliases.contains_key(alias) {
                        return Err(LocationError::TzTableParse {
                            table: "backward",
                            line: line_no,
                            reason: "duplicate backward Link alias",
                        });
                    }
                    aliases.insert(alias.to_string(), target.to_string());
                }
                Some("Rule") => {}
                Some("Zone") => {
                    in_zone_block = true;
                }
                _ => {
                    return Err(LocationError::TzTableParse {
                        table: "backward",
                        line: line_no,
                        reason: "unrecognized zic directive in backward table",
                    });
                }
            }
        }

        Ok(Self {
            version: version.into(),
            zones,
            aliases,
        })
    }

    /// Returns the lazily-parsed vendored IANA `zone1970.tab` + `backward` database.
    #[must_use]
    pub fn bundled() -> &'static Self {
        BUNDLED_TZDB.get_or_init(|| {
            Self::from_tables(
                vendored_tzdb_version(),
                VENDORED_ZONE1970_TAB,
                VENDORED_BACKWARD,
            )
            .expect("vendored data/tz/zone1970.tab and data/tz/backward must be valid")
        })
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    #[must_use]
    pub fn alias_count(&self) -> usize {
        self.aliases.len()
    }

    /// Follows `backward` `Link` chains to a target zone name, detecting cycles.
    pub fn canonicalize(&self, zone: &str) -> Result<String, LocationError> {
        let trimmed = zone.trim();
        if trimmed.is_empty() {
            return Err(LocationError::EmptyTimezone);
        }

        let mut current = trimmed;
        let mut visited = Vec::new();
        while let Some(target) = self.aliases.get(current) {
            visited.push(current);
            if visited.contains(&target.as_str()) {
                return Err(LocationError::AliasCycle {
                    zone: trimmed.to_string(),
                });
            }
            current = target.as_str();
        }
        Ok(current.to_string())
    }

    /// Resolves `zone` (following `backward` links such as `Asia/Saigon -> Asia/Ho_Chi_Minh`)
    /// to its `zone1970.tab` entry and principal coordinates.
    pub fn lookup(&self, zone: &str) -> Result<ResolvedTzLocation, LocationError> {
        let requested = zone.trim();
        if requested.is_empty() {
            return Err(LocationError::EmptyTimezone);
        }

        let canonical = self.canonicalize(requested)?;
        let Some(entry) = self.zones.get(&canonical) else {
            return Err(LocationError::UnknownTimezone {
                zone: requested.to_string(),
            });
        };

        Ok(ResolvedTzLocation {
            requested_zone: requested.to_string(),
            canonical_zone: entry.canonical_zone.clone(),
            coordinates: entry.coordinates,
            country_codes: entry.country_codes.clone(),
            comment: entry.comment.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedLocation {
    Timezone(ResolvedTzLocation),
    Manual(ManualLocation),
}

impl ResolvedLocation {
    #[must_use]
    pub const fn mode(&self) -> LocationMode {
        match self {
            Self::Timezone(_) => LocationMode::Timezone,
            Self::Manual(_) => LocationMode::Manual,
        }
    }

    #[must_use]
    pub const fn coordinates(&self) -> Coordinates {
        match self {
            Self::Timezone(tz) => tz.coordinates,
            Self::Manual(manual) => manual.coordinates(),
        }
    }

    /// Formats coordinates at `0.1°` precision (`"<LAT>,<LON>"`) for `shiki-term locate` output.
    #[must_use]
    pub fn format_display_coords(&self) -> String {
        let coords = self.coordinates();
        format!(
            "{:.1},{:.1}",
            round_manual_coord(coords.latitude_deg()),
            round_manual_coord(coords.longitude_deg())
        )
    }
}
