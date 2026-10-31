//! Pure UTC instant representation, fixed-instant constructors, `Clock` trait, and `FakeClock`.
//!
//! `sk-core` never reads the system clock; callers pass [`UtcInstant`] directly or inject a
//! [`Clock`] (`SystemClock` from `sk-store` in production, [`FakeClock`] in tests).

use core::fmt;
use core::sync::atomic::{AtomicI64, Ordering};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SECS_PER_MINUTE: i64 = 60;
pub const SECS_PER_HOUR: i64 = 3_600;
pub const SECS_PER_DAY: i64 = 86_400;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TimeError {
    #[error("invalid calendar date {year:04}-{month:02}-{day:02}")]
    InvalidDate { year: i32, month: u8, day: u8 },
    #[error("invalid time of day {hour:02}:{minute:02}:{second:02}")]
    InvalidTime { hour: u8, minute: u8, second: u8 },
    #[error("invalid RFC 3339 timestamp '{0}'")]
    InvalidRfc3339(String),
}

/// Broken-down proleptic Gregorian UTC civil date and time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UtcDateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Exact UTC instant in whole seconds since `1970-01-01T00:00:00Z`.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct UtcInstant(pub i64);

impl UtcInstant {
    #[must_use]
    pub const fn from_unix_secs(secs: i64) -> Self {
        Self(secs)
    }

    #[must_use]
    pub const fn as_unix_secs(self) -> i64 {
        self.0
    }

    pub fn from_utc_ymd_hms(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, TimeError> {
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return Err(TimeError::InvalidDate { year, month, day });
        }
        if hour >= 24 || minute >= 60 || second >= 60 {
            return Err(TimeError::InvalidTime {
                hour,
                minute,
                second,
            });
        }

        let days = days_from_civil(year, month, day);
        let secs_of_day = i64::from(hour) * SECS_PER_HOUR
            + i64::from(minute) * SECS_PER_MINUTE
            + i64::from(second);
        Ok(Self(days * SECS_PER_DAY + secs_of_day))
    }

    /// Panicking constructor intended for known-valid fixed-instant literals in tests and anchors.
    #[must_use]
    pub fn utc_ymd_hms(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self::from_utc_ymd_hms(year, month, day, hour, minute, second)
            .expect("valid fixed UTC date-time")
    }

    /// Parses an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DDTHH:MM:SS±HH:MM`)
    /// into a UTC [`UtcInstant`].
    pub fn parse_rfc3339(input: &str) -> Result<Self, TimeError> {
        let s = input.trim();
        let err = || TimeError::InvalidRfc3339(input.to_string());
        let bytes = s.as_bytes();

        // Minimum length: "YYYY-MM-DDTHH:MM:SSZ" = 20 bytes.
        if bytes.len() < 20 {
            return Err(err());
        }
        if bytes[4] != b'-'
            || bytes[7] != b'-'
            || !matches!(bytes[10], b'T' | b't' | b' ')
            || bytes[13] != b':'
            || bytes[16] != b':'
        {
            return Err(err());
        }

        let year: i32 = s.get(0..4).ok_or_else(err)?.parse().map_err(|_| err())?;
        let month: u8 = s.get(5..7).ok_or_else(err)?.parse().map_err(|_| err())?;
        let day: u8 = s.get(8..10).ok_or_else(err)?.parse().map_err(|_| err())?;
        let hour: u8 = s.get(11..13).ok_or_else(err)?.parse().map_err(|_| err())?;
        let minute: u8 = s.get(14..16).ok_or_else(err)?.parse().map_err(|_| err())?;
        let second: u8 = s.get(17..19).ok_or_else(err)?.parse().map_err(|_| err())?;

        let local_as_utc = Self::from_utc_ymd_hms(year, month, day, hour, minute, second)?;
        let tail = s.get(19..).ok_or_else(err)?;

        let offset_secs = if tail == "Z" || tail == "z" {
            0_i64
        } else {
            let tail_bytes = tail.as_bytes();
            if tail_bytes.len() != 6 || tail_bytes[3] != b':' {
                return Err(err());
            }
            let sign: i64 = match tail_bytes[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return Err(err()),
            };
            let off_h: u8 = tail.get(1..3).ok_or_else(err)?.parse().map_err(|_| err())?;
            let off_m: u8 = tail.get(4..6).ok_or_else(err)?.parse().map_err(|_| err())?;
            if off_h >= 24 || off_m >= 60 {
                return Err(err());
            }
            sign * (i64::from(off_h) * SECS_PER_HOUR + i64::from(off_m) * SECS_PER_MINUTE)
        };

        Ok(local_as_utc.add_secs(-offset_secs))
    }

    #[must_use]
    pub const fn to_utc_date_time(self) -> UtcDateTime {
        let days = self.0.div_euclid(SECS_PER_DAY);
        let secs_of_day = self.0.rem_euclid(SECS_PER_DAY);

        let (year, month, day) = civil_from_days(days);
        let hour = (secs_of_day / SECS_PER_HOUR) as u8;
        let minute = ((secs_of_day % SECS_PER_HOUR) / SECS_PER_MINUTE) as u8;
        let second = (secs_of_day % SECS_PER_MINUTE) as u8;

        UtcDateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[must_use]
    pub fn format_rfc3339_utc(self) -> String {
        let dt = self.to_utc_date_time();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
        )
    }

    #[must_use]
    pub const fn add_secs(self, delta_secs: i64) -> Self {
        Self(self.0.saturating_add(delta_secs))
    }

    #[must_use]
    pub const fn add_minutes(self, delta_mins: i64) -> Self {
        Self(
            self.0
                .saturating_add(delta_mins.saturating_mul(SECS_PER_MINUTE)),
        )
    }

    #[must_use]
    pub const fn duration_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for UtcInstant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dt = self.to_utc_date_time();
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
        )
    }
}

/// Pure clock abstraction injected into scheduler and hook staleness checks.
pub trait Clock: Send + Sync {
    fn now(&self) -> UtcInstant;
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> UtcInstant {
        (**self).now()
    }
}

/// Deterministic in-memory clock for unit, property, golden, and fake-clock scheduler tests.
#[derive(Debug)]
pub struct FakeClock {
    now_secs: AtomicI64,
}

impl FakeClock {
    #[must_use]
    pub const fn new(initial: UtcInstant) -> Self {
        Self {
            now_secs: AtomicI64::new(initial.0),
        }
    }

    #[must_use]
    pub const fn from_unix_secs(secs: i64) -> Self {
        Self::new(UtcInstant::from_unix_secs(secs))
    }

    #[must_use]
    pub fn from_utc_ymd_hms(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Self {
        Self::new(UtcInstant::utc_ymd_hms(
            year, month, day, hour, minute, second,
        ))
    }

    pub fn set(&self, instant: UtcInstant) {
        self.now_secs.store(instant.0, Ordering::SeqCst);
    }

    /// Advances the clock by `delta_secs` (negative values simulate backwards clock skew)
    /// and returns the updated instant.
    pub fn advance_secs(&self, delta_secs: i64) -> UtcInstant {
        let prev = self.now_secs.fetch_add(delta_secs, Ordering::SeqCst);
        UtcInstant(prev.wrapping_add(delta_secs))
    }

    pub fn advance_minutes(&self, delta_mins: i64) -> UtcInstant {
        self.advance_secs(delta_mins.saturating_mul(SECS_PER_MINUTE))
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::from_unix_secs(0)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UtcInstant {
        UtcInstant(self.now_secs.load(Ordering::SeqCst))
    }
}

#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[must_use]
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Howard Hinnant's `days_from_civil`: days from `1970-01-01` in the proleptic Gregorian calendar.
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = (year as i64) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

/// Howard Hinnant's `civil_from_days`: proleptic Gregorian `(year, month, day)` from Unix day count.
const fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8, d as u8)
}
