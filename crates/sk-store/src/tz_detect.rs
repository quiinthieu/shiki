//! Offline host IANA timezone detection (`PLAN.md` §0 #4, §4, ADR 001).

use std::fs;
use std::path::Path;

const ZONEINFO_MARKER: &str = "zoneinfo/";

#[derive(Clone, Debug, Default)]
pub struct SystemTzSources<'a> {
    pub tz_env: Option<&'a str>,
    pub localtime_link_target: Option<&'a Path>,
    pub etc_timezone_contents: Option<&'a str>,
    pub sysconfig_clock_contents: Option<&'a str>,
    pub fallback_iana_name: Option<&'a str>,
}

/// Extracts an IANA zone identifier from a path under `.../zoneinfo/...` (e.g.
/// `/var/db/timezone/zoneinfo/Asia/Saigon` or `/usr/share/zoneinfo/posix/Europe/Kyiv`).
#[must_use]
pub fn extract_zoneinfo_suffix(path: &Path) -> Option<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let idx = normalized.rfind(ZONEINFO_MARKER)?;
    let mut suffix = normalized[idx + ZONEINFO_MARKER.len()..].trim_matches('/');
    for prefix in ["posix/", "right/"] {
        if let Some(stripped) = suffix.strip_prefix(prefix) {
            suffix = stripped.trim_matches('/');
        }
    }
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.to_string())
    }
}

/// Normalizes a `TZ` specification or distro timezone string into an IANA identifier candidate.
#[must_use]
pub fn normalize_tz_spec(raw: &str) -> Option<String> {
    let trimmed = raw.trim().strip_prefix(':').unwrap_or(raw.trim()).trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(from_zoneinfo) = extract_zoneinfo_suffix(Path::new(trimmed)) {
        return Some(from_zoneinfo);
    }
    Some(trimmed.to_string())
}

fn parse_etc_timezone(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let cleaned = line.split('#').next().unwrap_or("").trim();
        if let Some(zone) = normalize_tz_spec(cleaned) {
            return Some(zone);
        }
    }
    None
}

fn parse_sysconfig_clock(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let cleaned = line.split('#').next().unwrap_or("").trim();
        for key in ["ZONE", "TIMEZONE"] {
            if let Some(rest) = cleaned.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(val) = rest.strip_prefix('=') {
                    let unquoted = val.trim().trim_matches('"').trim_matches('\'');
                    if let Some(zone) = normalize_tz_spec(unquoted) {
                        return Some(zone);
                    }
                }
            }
        }
    }
    None
}

/// Resolves the active IANA timezone name from explicit `SystemTzSources`.
///
/// Precedence:
/// 1. Non-empty `TZ` environment variable (preserved even if unknown so `TzDb::lookup` surfaces
///    a helpful `locate --set` suggestion instead of silently falling back to `/etc/localtime`).
/// 2. `/etc/localtime` symlink target under `.../zoneinfo/...` (macOS, systemd Linux, BSD).
/// 3. `/etc/timezone` (Debian/Ubuntu/WSL).
/// 4. `/etc/sysconfig/clock` (`ZONE=...`, RHEL/Fedora/openSUSE).
/// 5. `jiff::tz::TimeZone::system().iana_name()` TZif-aware fallback.
#[must_use]
pub fn detect_system_timezone_from(sources: &SystemTzSources<'_>) -> Option<String> {
    if let Some(tz_raw) = sources.tz_env {
        if let Some(zone) = normalize_tz_spec(tz_raw) {
            return Some(zone);
        }
    }

    if let Some(link_target) = sources.localtime_link_target {
        if let Some(zone) = extract_zoneinfo_suffix(link_target) {
            return Some(zone);
        }
    }

    if let Some(etc_tz) = sources.etc_timezone_contents {
        if let Some(zone) = parse_etc_timezone(etc_tz) {
            return Some(zone);
        }
    }

    if let Some(sysconfig) = sources.sysconfig_clock_contents {
        if let Some(zone) = parse_sysconfig_clock(sysconfig) {
            return Some(zone);
        }
    }

    if let Some(fallback) = sources.fallback_iana_name {
        if let Some(zone) = normalize_tz_spec(fallback) {
            return Some(zone);
        }
    }

    None
}

/// Detects the host OS's configured IANA timezone completely offline (`PLAN.md` §0 #4 & ADR 001).
#[must_use]
pub fn detect_system_timezone() -> Option<String> {
    let tz_env = std::env::var("TZ").ok();
    let localtime_link = fs::read_link("/etc/localtime").ok();
    let etc_timezone = fs::read_to_string("/etc/timezone").ok();
    let sysconfig_clock = fs::read_to_string("/etc/sysconfig/clock").ok();
    let jiff_tz = jiff::tz::TimeZone::system();
    let jiff_iana = jiff_tz.iana_name();

    detect_system_timezone_from(&SystemTzSources {
        tz_env: tz_env.as_deref(),
        localtime_link_target: localtime_link.as_deref(),
        etc_timezone_contents: etc_timezone.as_deref(),
        sysconfig_clock_contents: sysconfig_clock.as_deref(),
        fallback_iana_name: jiff_iana,
    })
}
