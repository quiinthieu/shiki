//! Pure predicted-step daemon sleep calculation (`PLAN.md` §3, §7, & §10 T09).
//!
//! `sk-core` never sleeps or reads the system clock directly. Given a current [`UtcInstant`] and
//! the last applied [`PaletteColors`], [`predict_next_step_sleep_secs`] predicts how many seconds
//! the daemon can sleep until the next perceptible step (`max_delta_e >= step_threshold`), bounded
//! by `[min_sleep_secs, max_sleep_secs]` (`PLAN.md` §7: at most one wake per minute, capped by
//! `max_sleep`, short in twilight, long at night or midday).

use crate::palette::{compute_frame, ModelConfig, Palette, PaletteColors};
use crate::solar::{Coordinates, DEFAULT_MODEL_TOML};
use crate::time::{UtcInstant, SECS_PER_DAY};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Hard lower bound on scheduled daemon sleep intervals (`PLAN.md` §7: "at most one wake per
/// minute", 60 seconds; `SIGHUP` may still wake the daemon early).
pub const MIN_DAEMON_SLEEP_SECS: u64 = 60;

/// Default maximum scheduled daemon sleep interval (1800 seconds = 30 minutes) during flat solar
/// regimes such as deep night or midday (`[scheduler].max_sleep_secs` in `data/model.toml`).
pub const DEFAULT_DAEMON_MAX_SLEEP_SECS: u64 = 1_800;

/// Default perceptual OKLab step threshold (`ΔE_OK = 0.008`, `[scheduler].step_threshold` in
/// `data/model.toml`).
pub const DEFAULT_DAEMON_STEP_THRESHOLD: f64 = 0.008;

/// Default coarse look-ahead stride (300 seconds = 5 minutes) used to bracket the next threshold
/// crossing before 1-second binary refinement (`[scheduler].probe_step_secs` in `data/model.toml`).
pub const DEFAULT_DAEMON_PROBE_STEP_SECS: u64 = 300;

const MAX_ALLOWED_SLEEP_SECS: u64 = SECS_PER_DAY as u64;
const MAX_ALLOWED_PROBE_STEP_SECS: u64 = 3_600;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SchedulerError {
    #[error("scheduler min_sleep_secs must be within 60..=86400 (PLAN §7: at most one wake/min)")]
    InvalidMinSleepSecs,
    #[error("scheduler max_sleep_secs must be within min_sleep_secs..=86400")]
    InvalidMaxSleepSecs,
    #[error("scheduler step_threshold must be finite and within (0.0, 1.0]")]
    InvalidStepThreshold,
    #[error("scheduler probe_step_secs must be within 60..=3600")]
    InvalidProbeStepSecs,
}

/// Tunable predicted-step scheduler parameters (`[scheduler]` in `data/model.toml`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Minimum scheduled sleep interval in seconds (>= 60 s per `PLAN.md` §7).
    pub min_sleep_secs: u64,
    /// Default maximum scheduled sleep interval in seconds (30 min = 1800 s by default).
    pub max_sleep_secs: u64,
    /// Default perceptual OKLab `ΔE_OK` step threshold.
    pub step_threshold: f64,
    /// Coarse look-ahead stride in seconds (5 min = 300 s by default) before binary search.
    pub probe_step_secs: u64,
}

#[derive(Deserialize)]
struct ModelTomlSchedulerRoot {
    scheduler: SchedulerConfig,
}

impl SchedulerConfig {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        if !(MIN_DAEMON_SLEEP_SECS..=MAX_ALLOWED_SLEEP_SECS).contains(&self.min_sleep_secs) {
            return Err(SchedulerError::InvalidMinSleepSecs);
        }
        if self.max_sleep_secs < self.min_sleep_secs || self.max_sleep_secs > MAX_ALLOWED_SLEEP_SECS
        {
            return Err(SchedulerError::InvalidMaxSleepSecs);
        }
        if !self.step_threshold.is_finite()
            || self.step_threshold <= 0.0
            || self.step_threshold > 1.0
        {
            return Err(SchedulerError::InvalidStepThreshold);
        }
        if !(MIN_DAEMON_SLEEP_SECS..=MAX_ALLOWED_PROBE_STEP_SECS).contains(&self.probe_step_secs) {
            return Err(SchedulerError::InvalidProbeStepSecs);
        }
        Ok(())
    }

    #[must_use]
    pub fn bundled() -> Self {
        let parsed: ModelTomlSchedulerRoot =
            toml::from_str(DEFAULT_MODEL_TOML).expect("bundled data/model.toml [scheduler] valid");
        parsed
            .scheduler
            .validate()
            .expect("bundled data/model.toml [scheduler] bounds valid");
        parsed.scheduler
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            min_sleep_secs: MIN_DAEMON_SLEEP_SECS,
            max_sleep_secs: DEFAULT_DAEMON_MAX_SLEEP_SECS,
            step_threshold: DEFAULT_DAEMON_STEP_THRESHOLD,
            probe_step_secs: DEFAULT_DAEMON_PROBE_STEP_SECS,
        }
    }
}

/// Predicts the number of seconds until the next perceptible palette step (`ΔE_OK >= step_threshold`
/// with distinct 8-bit [`PaletteColors`]) relative to `baseline_colors`.
///
/// Timer justification (`PLAN.md` §0 #7 & §7):
/// - Lower bound: `max(model.scheduler.min_sleep_secs, 60)` enforces at most one scheduled wake per
///   minute even during steep equatorial twilight or small `step_threshold` values.
/// - Coarse scan: steps forward in `probe_step_secs` (default 300 s = 5 min) increments up to
///   `max_sleep_secs` so pre-dawn or pre-dusk windows that begin changing mid-interval are never
///   missed.
/// - Binary refinement: once a `(lo, hi]` bracket is found where `crosses(lo) == false` and
///   `crosses(hi) == true`, bisection to 1-second precision preserves `crosses(hi) == true`, so the
///   predicted wake lands directly on an actionable step without wasted sub-threshold wakes.
/// - Upper bound: returns `max_sleep_secs` (clamped `>= min_sleep`) when no crossing occurs within
///   the look-ahead horizon (e.g. deep night, midday plateau, polar day, or polar night).
#[must_use]
pub fn predict_next_step_sleep_secs(
    now: UtcInstant,
    baseline_colors: &PaletteColors,
    coords: Coordinates,
    palette: &Palette,
    model: &ModelConfig,
    step_threshold: f64,
    max_sleep_secs: u64,
) -> u64 {
    // PLAN §7: at most one scheduled wake per minute (60 s floor).
    let min_sleep = model.scheduler.min_sleep_secs.max(MIN_DAEMON_SLEEP_SECS);
    // PLAN §7: capped by `max_sleep` (clamped >= `min_sleep`).
    let max_sleep = max_sleep_secs.max(min_sleep);
    // 5-minute coarse stride prevents stepping over twilight transitions for large `max_sleep`.
    let probe_stride = model.scheduler.probe_step_secs.max(MIN_DAEMON_SLEEP_SECS);

    let crosses = |offset_secs: u64| -> bool {
        let delta_i64 = i64::try_from(offset_secs).unwrap_or(i64::MAX);
        let candidate = compute_frame(now.add_secs(delta_i64), coords, palette, model);
        candidate.colors != *baseline_colors
            && baseline_colors.max_delta_e(&candidate.colors) >= step_threshold
    };

    if crosses(min_sleep) {
        return min_sleep;
    }
    if min_sleep == max_sleep {
        return max_sleep;
    }

    let mut lo = min_sleep;
    loop {
        let next_probe = lo.saturating_add(probe_stride).min(max_sleep);
        if crosses(next_probe) {
            let mut hi = next_probe;
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if crosses(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return hi;
        }
        if next_probe == max_sleep {
            return max_sleep;
        }
        lo = next_probe;
    }
}

/// Computes the scheduled daemon sleep duration while drift is paused (`pause` / `pause --for`).
///
/// Timer justification (`PLAN.md` §0 #7 & §7):
/// - Indefinite pause (`paused_until == None`): sleeps for the full `max_sleep_secs` cap (default
///   1800 s = 30 min) with zero computation; `resume` sends `SIGHUP` for immediate wake.
/// - Timed pause (`Some(until)`): sleeps until `until`, clamped to `[min_sleep_secs, max_sleep_secs]`
///   so expiry is picked up promptly without violating the 1-minute floor or `max_sleep` ceiling.
#[must_use]
pub fn compute_pause_sleep_secs(
    now: UtcInstant,
    paused_until: Option<UtcInstant>,
    model: &ModelConfig,
    max_sleep_secs: u64,
) -> u64 {
    let min_sleep = model.scheduler.min_sleep_secs.max(MIN_DAEMON_SLEEP_SECS);
    let max_sleep = max_sleep_secs.max(min_sleep);
    match paused_until {
        None => max_sleep,
        Some(until) => {
            let remaining = u64::try_from(until.duration_since(now).max(0)).unwrap_or(0);
            remaining.clamp(min_sleep, max_sleep)
        }
    }
}
