//! Civil `--date` and `--time` moment query resolution via `jiff` (`PLAN.md` §1.1 G6 & §6, T07).

use crate::error::StoreError;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use sk_core::time::{UtcInstant, SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE};

const MAX_PAUSE_DURATION_SECS: i64 = 365 * SECS_PER_DAY;

/// Parses a human duration string (`30s`, `45m`, `2h`, `1h30m`, `1d`) into positive whole seconds
/// for `pause --for <DURATION>` (`PLAN.md` §6 & §10 T09).
pub fn parse_pause_duration_secs(input: &str) -> Result<i64, StoreError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(StoreError::InvalidPauseDuration(input.to_string()));
    }

    let bytes = trimmed.as_bytes();
    let mut idx = 0_usize;
    let mut total_secs = 0_i64;
    let mut saw_segment = false;

    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }

        let num_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == num_start {
            return Err(StoreError::InvalidPauseDuration(input.to_string()));
        }
        let qty: i64 = trimmed[num_start..idx]
            .parse()
            .map_err(|_| StoreError::InvalidPauseDuration(input.to_string()))?;

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let unit_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_alphabetic() {
            idx += 1;
        }
        if idx == unit_start {
            return Err(StoreError::InvalidPauseDuration(input.to_string()));
        }

        let unit = trimmed[unit_start..idx].to_ascii_lowercase();
        let mult = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1_i64,
            "m" | "min" | "mins" | "minute" | "minutes" => SECS_PER_MINUTE,
            "h" | "hr" | "hrs" | "hour" | "hours" => SECS_PER_HOUR,
            "d" | "day" | "days" => SECS_PER_DAY,
            _ => return Err(StoreError::InvalidPauseDuration(input.to_string())),
        };

        let seg_secs = qty
            .checked_mul(mult)
            .ok_or_else(|| StoreError::InvalidPauseDuration(input.to_string()))?;
        total_secs = total_secs
            .checked_add(seg_secs)
            .ok_or_else(|| StoreError::InvalidPauseDuration(input.to_string()))?;
        saw_segment = true;
    }

    if !saw_segment || total_secs <= 0 || total_secs > MAX_PAUSE_DURATION_SECS {
        return Err(StoreError::InvalidPauseDuration(input.to_string()));
    }
    Ok(total_secs)
}

fn parse_ymd(input: &str) -> Result<(i32, u8, u8), StoreError> {
    let trimmed = input.trim();
    let err = || {
        StoreError::InvalidMomentQuery(format!(
            "invalid `--date` '{trimmed}': expected `YYYY-MM-DD`"
        ))
    };
    let bytes = trimmed.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(err());
    }
    let year: i32 = trimmed[0..4].parse().map_err(|_| err())?;
    let month: u8 = trimmed[5..7].parse().map_err(|_| err())?;
    let day: u8 = trimmed[8..10].parse().map_err(|_| err())?;
    // Validate calendar bounds in proleptic Gregorian calendar.
    let _ = UtcInstant::from_utc_ymd_hms(year, month, day, 0, 0, 0).map_err(|_| err())?;
    Ok((year, month, day))
}

struct ParsedWallTime {
    hour: u8,
    minute: u8,
    second: u8,
    explicit_offset_secs: Option<i64>,
}

fn parse_wall_time(input: &str) -> Result<ParsedWallTime, StoreError> {
    let trimmed = input.trim();
    let err = || {
        StoreError::InvalidMomentQuery(format!(
            "invalid `--time` '{trimmed}': expected `HH:MM`, `HH:MM:SS`, or RFC 3339 timestamp"
        ))
    };

    let (time_part, explicit_offset_secs) = if let Some(stripped) = trimmed
        .strip_suffix('Z')
        .or_else(|| trimmed.strip_suffix('z'))
    {
        (stripped, Some(0_i64))
    } else if trimmed.len() >= 11
        && matches!(trimmed.as_bytes()[trimmed.len() - 6], b'+' | b'-')
        && trimmed.as_bytes()[trimmed.len() - 3] == b':'
    {
        let split = trimmed.len() - 6;
        let offset_str = &trimmed[split..];
        let sign: i64 = if offset_str.as_bytes()[0] == b'+' {
            1
        } else {
            -1
        };
        let off_h: u8 = offset_str[1..3].parse().map_err(|_| err())?;
        let off_m: u8 = offset_str[4..6].parse().map_err(|_| err())?;
        if off_h >= 24 || off_m >= 60 {
            return Err(err());
        }
        let secs = sign * (i64::from(off_h) * SECS_PER_HOUR + i64::from(off_m) * SECS_PER_MINUTE);
        (&trimmed[..split], Some(secs))
    } else {
        (trimmed, None)
    };

    let tb = time_part.as_bytes();
    let (hour, minute, second) = match tb.len() {
        5 if tb[2] == b':' => (
            time_part[0..2].parse::<u8>().map_err(|_| err())?,
            time_part[3..5].parse::<u8>().map_err(|_| err())?,
            0_u8,
        ),
        8 if tb[2] == b':' && tb[5] == b':' => (
            time_part[0..2].parse::<u8>().map_err(|_| err())?,
            time_part[3..5].parse::<u8>().map_err(|_| err())?,
            time_part[6..8].parse::<u8>().map_err(|_| err())?,
        ),
        _ => return Err(err()),
    };

    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err(err());
    }

    Ok(ParsedWallTime {
        hour,
        minute,
        second,
        explicit_offset_secs,
    })
}

/// Resolves optional `--date` (`YYYY-MM-DD`) and `--time` (`HH:MM[:SS][Z|±HH:MM]` or full RFC 3339)
/// CLI arguments into an exact [`UtcInstant`].
pub fn resolve_moment_query(
    date_arg: Option<&str>,
    time_arg: Option<&str>,
    iana_timezone: Option<&str>,
    fallback_now: UtcInstant,
) -> Result<UtcInstant, StoreError> {
    let date_trimmed = date_arg.map(str::trim).filter(|s| !s.is_empty());
    let time_trimmed = time_arg.map(str::trim).filter(|s| !s.is_empty());

    match (date_trimmed, time_trimmed) {
        (None, None) => Ok(fallback_now),
        (Some(_), Some(t)) if t.contains(['T', 't', ' ']) => Err(StoreError::InvalidMomentQuery(
            "`--time` already contains a full RFC 3339 date-time; omit `--date` or pass `HH:MM[:SS]`"
                .to_string(),
        )),
        (None, Some(t)) if t.contains(['T', 't', ' ']) => UtcInstant::parse_rfc3339(t)
            .map_err(|err| StoreError::InvalidMomentQuery(err.to_string())),
        (date_opt, time_opt) => {
            let wall = match time_opt {
                Some(t) => parse_wall_time(t)?,
                None => ParsedWallTime {
                    hour: 12,
                    minute: 0,
                    second: 0,
                    explicit_offset_secs: None,
                },
            };

            if let Some(offset_secs) = wall.explicit_offset_secs {
                let (year, month, day) = match date_opt {
                    Some(d) => parse_ymd(d)?,
                    None => {
                        let shifted = fallback_now.add_secs(offset_secs).to_utc_date_time();
                        (shifted.year, shifted.month, shifted.day)
                    }
                };
                let local_as_utc = UtcInstant::from_utc_ymd_hms(
                    year,
                    month,
                    day,
                    wall.hour,
                    wall.minute,
                    wall.second,
                )
                .map_err(|err| StoreError::InvalidMomentQuery(err.to_string()))?;
                return Ok(local_as_utc.add_secs(-offset_secs));
            }

            let tz = match iana_timezone {
                Some(name) => TimeZone::get(name).map_err(|err| {
                    StoreError::InvalidMomentQuery(format!(
                        "unable to load IANA timezone '{name}': {err}"
                    ))
                })?,
                None => TimeZone::try_system().unwrap_or(TimeZone::UTC),
            };

            let (year, month, day) = match date_opt {
                Some(d) => parse_ymd(d)?,
                None => {
                    let ts = Timestamp::from_second(fallback_now.as_unix_secs()).map_err(|err| {
                        StoreError::InvalidMomentQuery(format!("timestamp out of range: {err}"))
                    })?;
                    let zoned = ts.to_zoned(tz.clone());
                    (
                        i32::from(zoned.year()),
                        zoned.month() as u8,
                        zoned.day() as u8,
                    )
                }
            };

            let y_i16 = i16::try_from(year).map_err(|_| {
                StoreError::InvalidMomentQuery(format!("year {year} out of supported civil range"))
            })?;
            let dt = civil::date(y_i16, month as i8, day as i8).at(
                wall.hour as i8,
                wall.minute as i8,
                wall.second as i8,
                0,
            );
            let zoned = dt.to_zoned(tz).map_err(|err| {
                StoreError::InvalidMomentQuery(format!(
                    "unable to resolve local civil date-time: {err}"
                ))
            })?;
            Ok(UtcInstant::from_unix_secs(zoned.timestamp().as_second()))
        }
    }
}
