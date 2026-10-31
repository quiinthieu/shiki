//! `config.toml` schema, atomic persistence, `config get|set`, and location resolution (`PLAN.md` §4 & §6).

use crate::atomic::atomic_write;
use crate::error::StoreError;
use crate::palette::load_palette;
use crate::paths::ShikiPaths;
use crate::tz_detect::detect_system_timezone;
use serde::{Deserialize, Serialize};
use sk_core::color::{ContrastThresholds, WCAG_MAX_CONTRAST_RATIO};
use sk_core::location::{LocationError, LocationMode, ManualLocation, ResolvedLocation, TzDb};
use sk_core::palette::ModelConfig;
use sk_core::scheduler::{
    DEFAULT_DAEMON_MAX_SLEEP_SECS, DEFAULT_DAEMON_STEP_THRESHOLD, MIN_DAEMON_SLEEP_SECS,
};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::Range;

pub const DEFAULT_PALETTE_NAME: &str = "shiki";
pub const DEFAULT_STEP_THRESHOLD: f64 = DEFAULT_DAEMON_STEP_THRESHOLD;
pub const DEFAULT_HOOK_MIN_INTERVAL_SECS: u64 = MIN_DAEMON_SLEEP_SECS;
pub const DEFAULT_MAX_SLEEP_SECS: u64 = DEFAULT_DAEMON_MAX_SLEEP_SECS;

pub const VALID_CONFIG_KEYS: &str = "palette, step_threshold, hook_min_interval_secs, max_sleep_secs, location.mode, location.latitude, location.longitude, location.timezone, contrast.fg_bg_min_ratio, contrast.ansi_bg_min_ratio, delivery.tty_broadcast, delivery.tmux_passthrough";

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
pub struct ContrastSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_bg_min_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ansi_bg_min_ratio: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl ContrastSection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fg_bg_min_ratio.is_none() && self.ansi_bg_min_ratio.is_none() && self.extra.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeliverySection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty_broadcast: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_passthrough: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl DeliverySection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tty_broadcast.is_none() && self.tmux_passthrough.is_none() && self.extra.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_min_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sleep_secs: Option<u64>,
    #[serde(default)]
    pub location: LocationSection,
    #[serde(default, skip_serializing_if = "ContrastSection::is_empty")]
    pub contrast: ContrastSection,
    #[serde(default, skip_serializing_if = "DeliverySection::is_empty")]
    pub delivery: DeliverySection,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl AppConfig {
    #[must_use]
    pub fn effective_palette(&self) -> &str {
        self.palette.as_deref().unwrap_or(DEFAULT_PALETTE_NAME)
    }

    #[must_use]
    pub fn effective_step_threshold(&self) -> f64 {
        self.step_threshold.unwrap_or(DEFAULT_STEP_THRESHOLD)
    }

    #[must_use]
    pub fn effective_hook_min_interval_secs(&self) -> u64 {
        self.hook_min_interval_secs
            .unwrap_or(DEFAULT_HOOK_MIN_INTERVAL_SECS)
    }

    #[must_use]
    pub fn effective_max_sleep_secs(&self) -> u64 {
        self.max_sleep_secs.unwrap_or(DEFAULT_MAX_SLEEP_SECS)
    }

    #[must_use]
    pub fn effective_tty_broadcast(&self) -> bool {
        self.delivery.tty_broadcast.unwrap_or(false)
    }

    #[must_use]
    pub fn effective_tmux_passthrough(&self) -> bool {
        self.delivery.tmux_passthrough.unwrap_or(false)
    }

    #[must_use]
    pub fn effective_contrast(&self, defaults: ContrastThresholds) -> ContrastThresholds {
        ContrastThresholds {
            fg_bg_min_ratio: self
                .contrast
                .fg_bg_min_ratio
                .unwrap_or(defaults.fg_bg_min_ratio),
            ansi_bg_min_ratio: self
                .contrast
                .ansi_bg_min_ratio
                .unwrap_or(defaults.ansi_bg_min_ratio),
        }
    }

    /// Builds a [`ModelConfig`] starting from [`ModelConfig::bundled()`] with any validated user
    /// `[contrast]` overrides applied.
    pub fn effective_model_config(&self) -> Result<ModelConfig, StoreError> {
        let mut model = ModelConfig::bundled();
        model.contrast = self.effective_contrast(model.contrast);
        model
            .contrast
            .validate()
            .map_err(|err| StoreError::InvalidConfigValue {
                key: "contrast".to_string(),
                reason: err.to_string(),
            })?;
        Ok(model)
    }
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

/// Atomically writes `config` to `paths.config_file()` (`temp -> fsync -> rename`) and expires
/// any cached prompt-hook freshness lease in `current.gen`.
pub fn save_config(paths: &ShikiPaths, config: &AppConfig) -> Result<(), StoreError> {
    let serialized = toml::to_string_pretty(config)
        .map_err(|err| StoreError::ConfigSerialize(redact_numeric_literals(&err.to_string())))?;
    atomic_write(&paths.config_file(), serialized.as_bytes())?;
    crate::store::expire_generation_freshness(paths)
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

fn format_opt_f64(val: Option<f64>) -> String {
    match val {
        Some(v) => format!("{v}"),
        None => "unset".to_string(),
    }
}

fn get_config_value_from(config: &AppConfig, key: &str) -> Result<String, StoreError> {
    let defaults = ModelConfig::bundled().contrast;
    let effective_contrast = config.effective_contrast(defaults);
    match key {
        "palette" => Ok(config.effective_palette().to_string()),
        "step_threshold" => Ok(format!("{}", config.effective_step_threshold())),
        "hook_min_interval_secs" => Ok(format!("{}", config.effective_hook_min_interval_secs())),
        "max_sleep_secs" => Ok(format!("{}", config.effective_max_sleep_secs())),
        "location.mode" => Ok(match config.location.mode {
            LocationMode::Timezone => "timezone".to_string(),
            LocationMode::Manual => "manual".to_string(),
        }),
        "location.latitude" => Ok(format_opt_f64(config.location.latitude)),
        "location.longitude" => Ok(format_opt_f64(config.location.longitude)),
        "location.timezone" => Ok(config
            .location
            .timezone
            .clone()
            .unwrap_or_else(|| "auto".to_string())),
        "contrast.fg_bg_min_ratio" => Ok(format!("{}", effective_contrast.fg_bg_min_ratio)),
        "contrast.ansi_bg_min_ratio" => Ok(format!("{}", effective_contrast.ansi_bg_min_ratio)),
        "delivery.tty_broadcast" => Ok(format!("{}", config.effective_tty_broadcast())),
        "delivery.tmux_passthrough" => Ok(format!("{}", config.effective_tmux_passthrough())),
        _ => Err(StoreError::UnknownConfigKey {
            key: key.to_string(),
            valid_keys: VALID_CONFIG_KEYS,
        }),
    }
}

/// Returns the effective string value of `key` from `config.toml` (falling back to defaults when
/// `config.toml` or the specific field is omitted).
pub fn get_config_value(paths: &ShikiPaths, key: &str) -> Result<String, StoreError> {
    let config = load_config(paths)?;
    get_config_value_from(&config, key.trim())
}

/// Lists all supported configuration keys and their effective values in deterministic order.
pub fn list_config_entries(paths: &ShikiPaths) -> Result<Vec<(&'static str, String)>, StoreError> {
    const KEYS: [&str; 12] = [
        "palette",
        "step_threshold",
        "hook_min_interval_secs",
        "max_sleep_secs",
        "location.mode",
        "location.latitude",
        "location.longitude",
        "location.timezone",
        "contrast.fg_bg_min_ratio",
        "contrast.ansi_bg_min_ratio",
        "delivery.tty_broadcast",
        "delivery.tmux_passthrough",
    ];
    let config = load_config(paths)?;
    let mut out = Vec::with_capacity(KEYS.len());
    for key in KEYS {
        out.push((key, get_config_value_from(&config, key)?));
    }
    Ok(out)
}

fn parse_bool_value(key: &str, raw: &str) -> Result<bool, StoreError> {
    match raw.trim() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(StoreError::InvalidConfigValue {
            key: key.to_string(),
            reason: "expected `true` or `false`".to_string(),
        }),
    }
}

/// Validates and atomically updates `key = value` in `config.toml`.
///
/// Enforces:
/// - Palette validity (`PLAN.md` §6: *"An invalid palette never applies"*)
/// - `--i-know` (`allow_lower_contrast`) when lowering WCAG contrast thresholds below defaults
/// - Coordinate range checks and `0.1°` rounding without leaking invalid numeric values in errors
pub fn set_config_value(
    paths: &ShikiPaths,
    key: &str,
    value: &str,
    allow_lower_contrast: bool,
) -> Result<String, StoreError> {
    let key = key.trim();
    let trimmed = value.trim();
    let mut config = load_config(paths)?;
    let defaults = ModelConfig::bundled().contrast;

    match key {
        "palette" => {
            if trimmed.is_empty() {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "palette name cannot be empty".to_string(),
                });
            }
            // Verify palette parses cleanly before saving (`PLAN.md` §6).
            let _ = load_palette(paths, trimmed)?;
            config.palette = Some(trimmed.to_string());
        }
        "step_threshold" => {
            let parsed: f64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected a finite number in (0.0, 1.0]".to_string(),
                })?;
            if !parsed.is_finite() || parsed <= 0.0 || parsed > 1.0 {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "step_threshold must be within (0.0, 1.0]".to_string(),
                });
            }
            config.step_threshold = Some(parsed);
        }
        "hook_min_interval_secs" => {
            let parsed: u64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected a positive integer number of seconds".to_string(),
                })?;
            if parsed == 0 || parsed > 86_400 {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "hook_min_interval_secs must be within 1..=86400".to_string(),
                });
            }
            config.hook_min_interval_secs = Some(parsed);
        }
        "max_sleep_secs" => {
            let parsed: u64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected an integer number of seconds >= 60".to_string(),
                })?;
            if !(60..=86_400).contains(&parsed) {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "max_sleep_secs must be within 60..=86400".to_string(),
                });
            }
            config.max_sleep_secs = Some(parsed);
        }
        "location.mode" => match trimmed {
            "timezone" => {
                let detected = detect_system_timezone();
                let effective_tz = config
                    .location
                    .timezone
                    .as_deref()
                    .or(detected.as_deref())
                    .ok_or(LocationError::MissingSystemTimezone)?;
                let _ = TzDb::bundled().lookup(effective_tz)?;
                config.location.mode = LocationMode::Timezone;
                config.location.latitude = None;
                config.location.longitude = None;
            }
            "manual" => {
                if config.location.latitude.is_none() || config.location.longitude.is_none() {
                    return Err(StoreError::Location(
                        LocationError::MissingManualCoordinates,
                    ));
                }
                config.location.mode = LocationMode::Manual;
            }
            _ => {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected `timezone` or `manual`".to_string(),
                });
            }
        },
        "location.latitude" => {
            let lat: f64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "latitude must be a decimal number in [-90, 90]".to_string(),
                })?;
            let lon = config.location.longitude.unwrap_or(0.0);
            let manual = ManualLocation::new(lat, lon)?;
            config.location.latitude = Some(manual.latitude_deg());
        }
        "location.longitude" => {
            let lon: f64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "longitude must be a decimal number in [-180, 180]".to_string(),
                })?;
            let lat = config.location.latitude.unwrap_or(0.0);
            let manual = ManualLocation::new(lat, lon)?;
            config.location.longitude = Some(manual.longitude_deg());
        }
        "location.timezone" => {
            if matches!(trimmed, "" | "auto" | "none") {
                config.location.timezone = None;
            } else {
                let _ = TzDb::bundled().lookup(trimmed)?;
                config.location.timezone = Some(trimmed.to_string());
            }
        }
        "contrast.fg_bg_min_ratio" => {
            let ratio: f64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected a finite contrast ratio in [1.0, 21.0]".to_string(),
                })?;
            if !ratio.is_finite() || !(1.0..=WCAG_MAX_CONTRAST_RATIO).contains(&ratio) {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "contrast ratio must be within [1.0, 21.0]".to_string(),
                });
            }
            if ratio < defaults.fg_bg_min_ratio && !allow_lower_contrast {
                return Err(StoreError::LowerContrastRequiresAck {
                    key: key.to_string(),
                    default_ratio: defaults.fg_bg_min_ratio,
                });
            }
            config.contrast.fg_bg_min_ratio = Some(ratio);
        }
        "contrast.ansi_bg_min_ratio" => {
            let ratio: f64 = trimmed
                .parse()
                .map_err(|_| StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "expected a finite contrast ratio in [1.0, 21.0]".to_string(),
                })?;
            if !ratio.is_finite() || !(1.0..=WCAG_MAX_CONTRAST_RATIO).contains(&ratio) {
                return Err(StoreError::InvalidConfigValue {
                    key: key.to_string(),
                    reason: "contrast ratio must be within [1.0, 21.0]".to_string(),
                });
            }
            if ratio < defaults.ansi_bg_min_ratio && !allow_lower_contrast {
                return Err(StoreError::LowerContrastRequiresAck {
                    key: key.to_string(),
                    default_ratio: defaults.ansi_bg_min_ratio,
                });
            }
            config.contrast.ansi_bg_min_ratio = Some(ratio);
        }
        "delivery.tty_broadcast" => {
            config.delivery.tty_broadcast = Some(parse_bool_value(key, trimmed)?);
        }
        "delivery.tmux_passthrough" => {
            config.delivery.tmux_passthrough = Some(parse_bool_value(key, trimmed)?);
        }
        _ => {
            return Err(StoreError::UnknownConfigKey {
                key: key.to_string(),
                valid_keys: VALID_CONFIG_KEYS,
            });
        }
    }

    save_config(paths, &config)?;
    get_config_value_from(&config, key)
}
