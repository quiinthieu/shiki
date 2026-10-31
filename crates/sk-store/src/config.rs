//! `config.toml` schema, atomic persistence, and location resolution (`PLAN.md` §4 & §6).

use crate::atomic::atomic_write;
use crate::error::StoreError;
use crate::paths::ShikiPaths;
use crate::tz_detect::detect_system_timezone;
use serde::{Deserialize, Serialize};
use sk_core::location::{LocationError, LocationMode, ManualLocation, ResolvedLocation, TzDb};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocationSection {
    #[serde(default)]
    pub mode: LocationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub location: LocationSection,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

fn format_span_line_col(src: &str, span: Option<Range<usize>>) -> String {
    let Some(range) = span else {
        return String::new();
    };
    let offset = range.start.min(src.len());
    let prefix = &src[..offset];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = match prefix.rfind('\n') {
        Some(nl) => prefix[nl + 1..].chars().count() + 1,
        None => prefix.chars().count() + 1,
    };
    format!(" (line {line}, column {col})")
}

/// Replaces numeric literals in parser error messages so coordinates stored in a malformed
/// `config.toml` can never leak into error output (`PLAN.md` §0 #4 & §9).
fn redact_numeric_literals(msg: &str) -> String {
    let bytes = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;

    while i < bytes.len() {
        let starts_signed_number = (bytes[i] == b'+' || bytes[i] == b'-')
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit();
        if bytes[i].is_ascii_digit() || starts_signed_number {
            if starts_signed_number {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            out.push_str("<redacted>");
        } else {
            let ch = msg[i..].chars().next().expect("valid UTF-8 char");
            out.push(ch);
            i += ch.len_utf8();
        }
    }

    out
}

/// Loads `config.toml` from `paths.config_file()`, returning `AppConfig::default()` if the file
/// does not exist yet.
pub fn load_config(paths: &ShikiPaths) -> Result<AppConfig, StoreError> {
    let path = paths.config_file();
    let contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(AppConfig::default()),
        Err(err) => return Err(StoreError::Io(err)),
    };

    toml::from_str::<AppConfig>(&contents).map_err(|err| StoreError::ConfigParse {
        path,
        location: format_span_line_col(&contents, err.span()),
        message: redact_numeric_literals(err.message()),
    })
}

/// Atomically writes `config` to `paths.config_file()` (`temp -> fsync -> rename`).
pub fn save_config(paths: &ShikiPaths, config: &AppConfig) -> Result<(), StoreError> {
    let serialized = toml::to_string_pretty(config)
        .map_err(|err| StoreError::ConfigSerialize(redact_numeric_literals(&err.to_string())))?;
    atomic_write(&paths.config_file(), serialized.as_bytes())
}

/// Pure location resolver combining a parsed `AppConfig`, an optional detected system timezone,
/// and a `TzDb`.
pub fn resolve_location_with(
    config: &AppConfig,
    detected_tz: Option<&str>,
    tzdb: &TzDb,
) -> Result<ResolvedLocation, LocationError> {
    match config.location.mode {
        LocationMode::Manual => {
            let (Some(lat), Some(lon)) = (config.location.latitude, config.location.longitude)
            else {
                return Err(LocationError::MissingManualCoordinates);
            };
            let manual = ManualLocation::new(lat, lon)?;
            Ok(ResolvedLocation::Manual(manual))
        }
        LocationMode::Timezone => {
            let effective_tz = config
                .location
                .timezone
                .as_deref()
                .or(detected_tz)
                .ok_or(LocationError::MissingSystemTimezone)?;
            let resolved = tzdb.lookup(effective_tz)?;
            Ok(ResolvedLocation::Timezone(resolved))
        }
    }
}

/// Loads `config.toml` and resolves the active location (manual or system IANA timezone).
pub fn resolve_location(paths: &ShikiPaths) -> Result<ResolvedLocation, StoreError> {
    let config = load_config(paths)?;
    let detected_tz = if config.location.mode == LocationMode::Manual {
        None
    } else {
        detect_system_timezone()
    };
    Ok(resolve_location_with(
        &config,
        detected_tz.as_deref(),
        TzDb::bundled(),
    )?)
}

/// Validates/persists rounded manual coordinates in `config.toml`, preserving all unrelated
/// configuration keys.
pub fn set_manual_location(
    paths: &ShikiPaths,
    manual: ManualLocation,
) -> Result<ResolvedLocation, StoreError> {
    let mut config = load_config(paths)?;
    config.location.mode = LocationMode::Manual;
    config.location.latitude = Some(manual.latitude_deg());
    config.location.longitude = Some(manual.longitude_deg());
    save_config(paths, &config)?;
    Ok(ResolvedLocation::Manual(manual))
}

/// Switches `config.toml` back to `mode = "timezone"` and removes stored manual coordinates,
/// validating the effective timezone in `tzdb` before writing `config.toml`.
pub fn set_timezone_location_with(
    paths: &ShikiPaths,
    detected_tz: Option<&str>,
    tzdb: &TzDb,
) -> Result<ResolvedLocation, StoreError> {
    let mut config = load_config(paths)?;
    let effective_tz = config
        .location
        .timezone
        .as_deref()
        .or(detected_tz)
        .ok_or(LocationError::MissingSystemTimezone)?;
    let resolved = tzdb.lookup(effective_tz)?;

    config.location.mode = LocationMode::Timezone;
    config.location.latitude = None;
    config.location.longitude = None;
    save_config(paths, &config)?;
    Ok(ResolvedLocation::Timezone(resolved))
}

/// Switches `config.toml` back to `mode = "timezone"` using the detected host IANA timezone and
/// the bundled `TzDb`.
pub fn set_timezone_location(paths: &ShikiPaths) -> Result<ResolvedLocation, StoreError> {
    let detected = detect_system_timezone();
    set_timezone_location_with(paths, detected.as_deref(), TzDb::bundled())
}
