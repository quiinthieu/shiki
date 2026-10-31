//! Atomic persistence for `state.json`, `snapshot.json`, `current.seq`, and `shiki.lock` (`PLAN.md` §6, T07).
//!
//! Privacy (`PLAN.md` §0 #4): `snapshot.json` and `state.json` never contain coordinates.

use crate::atomic::atomic_write;
use crate::error::StoreError;
use crate::paths::ShikiPaths;
use rustix::fs::{flock, FlockOperation};
use serde::{Deserialize, Serialize};
use sk_core::palette::{DayHalf, Frame, ModelConfig, PaletteColors, SolarPhase};
use sk_core::season::Season;
use sk_core::solar::{next_solar_event, Coordinates, PolarState, SolarEventKind};
use sk_core::time::UtcInstant;
use std::fs::{self, File, OpenOptions};
use std::io;

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Exclusive advisory file lock over `paths.lock_file()` (`shiki.lock`), released automatically on drop.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

impl StoreLock {
    pub fn acquire(paths: &ShikiPaths) -> Result<Self, StoreError> {
        let lock_path = paths.lock_file();
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut opts = OpenOptions::new();
        opts.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&lock_path)?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(|err| StoreError::Io(io::Error::from_raw_os_error(err.raw_os_error())))?;
        Ok(Self { _file: file })
    }
}

fn round6(value: f64) -> f64 {
    let scaled = (value * 1_000_000.0).round() / 1_000_000.0;
    if scaled == 0.0 {
        0.0
    } else {
        scaled
    }
}

fn round4(value: f64) -> f64 {
    let scaled = (value * 10_000.0).round() / 10_000.0;
    if scaled == 0.0 {
        0.0
    } else {
        scaled
    }
}

/// Deterministic 64-bit FNV-1a hex digest over all 21 8-bit sRGB slots in [`PaletteColors`].
#[must_use]
pub fn compute_frame_hash(colors: &PaletteColors) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    let mut feed = |rgb: sk_core::color::Rgb8| {
        for byte in [rgb.r, rgb.g, rgb.b] {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };

    feed(colors.bg);
    feed(colors.fg);
    feed(colors.cursor);
    feed(colors.selection_bg);
    feed(colors.selection_fg);
    for &slot in &colors.ansi {
        feed(slot);
    }

    format!("{hash:016x}")
}

/// Persisted daemon/hook state (`state.json`, `PLAN.md` §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoreState {
    pub schema_version: u32,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_rfc3339: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_unix_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_colors: Option<PaletteColors>,
}

impl Default for StoreState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            generation: 0,
            frame_hash: None,
            palette: None,
            updated_at_rfc3339: None,
            updated_at_unix_secs: None,
            paused_until: None,
            last_colors: None,
        }
    }
}

/// Next upcoming horizon/twilight solar milestone serialized in `snapshot.json` and `preview --json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotNextEvent {
    pub kind: SolarEventKind,
    pub instant_rfc3339: String,
    pub unix_secs: i64,
}

/// WCAG contrast summary serialized in `snapshot.json` and `preview --json`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotContrast {
    pub fg_bg_ratio: f64,
    pub selection_ratio: f64,
    pub min_ansi_bg_ratio: f64,
    pub fixup_slots: u8,
}

/// Versioned, coordinate-free snapshot (`snapshot.json`, `now --json`, `preview --json`; `PLAN.md` §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub palette: String,
    pub instant_rfc3339: String,
    pub unix_secs: i64,
    pub solar_phase: SolarPhase,
    pub day_half: DayHalf,
    pub day_phase: f64,
    pub season: Season,
    pub season_phase: f64,
    pub season_strength: f64,
    pub elevation_deg: f64,
    pub noon_elevation_deg: f64,
    pub polar_state: PolarState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_event: Option<SnapshotNextEvent>,
    pub colors: PaletteColors,
    pub contrast: SnapshotContrast,
}

impl Snapshot {
    /// Constructs a deterministic, coordinate-free [`Snapshot`] from a computed [`Frame`].
    #[must_use]
    pub fn from_frame(
        palette_name: &str,
        frame: &Frame,
        coords: Coordinates,
        model: &ModelConfig,
    ) -> Self {
        let next_event = next_solar_event(frame.instant, coords, &model.event_angles).map(|ev| {
            SnapshotNextEvent {
                kind: ev.kind,
                instant_rfc3339: ev.instant.format_rfc3339_utc(),
                unix_secs: ev.instant.as_unix_secs(),
            }
        });

        let fg_bg_ratio = round4(frame.colors.fg.contrast_ratio(frame.colors.bg));
        let selection_ratio = round4(
            frame
                .colors
                .selection_fg
                .contrast_ratio(frame.colors.selection_bg),
        );
        let min_ansi_bg_ratio = round4(
            frame
                .colors
                .ansi
                .iter()
                .enumerate()
                .filter(|(idx, _)| !matches!(idx, 0 | 8))
                .map(|(_, &c)| c.contrast_ratio(frame.colors.bg))
                .fold(f64::INFINITY, f64::min),
        );

        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            palette: palette_name.to_string(),
            instant_rfc3339: frame.instant.format_rfc3339_utc(),
            unix_secs: frame.instant.as_unix_secs(),
            solar_phase: frame.solar_phase,
            day_half: frame.day_half,
            day_phase: round6(frame.day_phase),
            season: frame.season,
            season_phase: round6(frame.season_phase),
            season_strength: round6(frame.season_strength),
            elevation_deg: round6(frame.elevation_deg),
            noon_elevation_deg: round6(frame.noon_elevation_deg),
            polar_state: frame.polar_state,
            next_event,
            colors: frame.colors,
            contrast: SnapshotContrast {
                fg_bg_ratio,
                selection_ratio,
                min_ansi_bg_ratio,
                fixup_slots: frame.fixup_slots,
            },
        }
    }
}

/// Loads `state.json`, returning [`StoreState::default()`] if `state.json` does not exist yet.
pub fn load_state(paths: &ShikiPaths) -> Result<StoreState, StoreError> {
    let path = paths.state_file();
    let contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(StoreState::default()),
        Err(err) => return Err(StoreError::Io(err)),
    };
    serde_json::from_str(&contents).map_err(|err| StoreError::JsonParse {
        path,
        message: err.to_string(),
    })
}

/// Loads `snapshot.json`, returning `Ok(None)` if `snapshot.json` has not been written yet.
pub fn load_snapshot(paths: &ShikiPaths) -> Result<Option<Snapshot>, StoreError> {
    let path = paths.snapshot_file();
    let contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StoreError::Io(err)),
    };
    let parsed = serde_json::from_str(&contents).map_err(|err| StoreError::JsonParse {
        path,
        message: err.to_string(),
    })?;
    Ok(Some(parsed))
}

fn serialize_json_pretty<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let mut json = serde_json::to_string_pretty(value)
        .map_err(|err| StoreError::JsonSerialize(err.to_string()))?;
    json.push('\n');
    Ok(json.into_bytes())
}

/// Atomically persists `current.seq`, `snapshot.json`, and `state.json` under `shiki.lock`.
///
/// Increments `state.generation` whenever the encoded frame hash changed (or `current.seq` was
/// missing/empty after a `reset`), keeping `generation` unchanged on identical repeat frames.
pub fn persist_applied_frame(
    paths: &ShikiPaths,
    palette_name: &str,
    snapshot: &Snapshot,
    osc_bytes: &[u8],
) -> Result<StoreState, StoreError> {
    let _lock = StoreLock::acquire(paths)?;
    let mut state = load_state(paths)?;
    let new_hash = compute_frame_hash(&snapshot.colors);
    let seq_missing_or_empty = fs::metadata(paths.current_seq_file())
        .map(|m| m.len() == 0)
        .unwrap_or(true);

    if state.frame_hash.as_deref() != Some(new_hash.as_str()) || seq_missing_or_empty {
        state.generation = state.generation.saturating_add(1);
    }
    state.schema_version = STATE_SCHEMA_VERSION;
    state.frame_hash = Some(new_hash);
    state.palette = Some(palette_name.to_string());
    state.updated_at_rfc3339 = Some(snapshot.instant_rfc3339.clone());
    state.updated_at_unix_secs = Some(snapshot.unix_secs);
    state.last_colors = Some(snapshot.colors);

    let snapshot_bytes = serialize_json_pretty(snapshot)?;
    let state_bytes = serialize_json_pretty(&state)?;

    atomic_write(&paths.current_seq_file(), osc_bytes)?;
    atomic_write(&paths.snapshot_file(), &snapshot_bytes)?;
    atomic_write(&paths.state_file(), &state_bytes)?;

    Ok(state)
}

/// Clears `current.seq` and clears `frame_hash` in `state.json` (bumping `generation`) if store
/// state has already been initialized, leaving uninitialized systems untouched (`PLAN.md` T19).
pub fn persist_reset(
    paths: &ShikiPaths,
    reset_at: UtcInstant,
) -> Result<Option<StoreState>, StoreError> {
    let has_existing_store = paths.state_file().exists()
        || paths.current_seq_file().exists()
        || paths.snapshot_file().exists();
    if !has_existing_store {
        return Ok(None);
    }

    let _lock = StoreLock::acquire(paths)?;
    let mut state = load_state(paths)?;
    state.schema_version = STATE_SCHEMA_VERSION;
    state.generation = state.generation.saturating_add(1);
    state.frame_hash = None;
    state.updated_at_rfc3339 = Some(reset_at.format_rfc3339_utc());
    state.updated_at_unix_secs = Some(reset_at.as_unix_secs());

    let state_bytes = serialize_json_pretty(&state)?;
    atomic_write(&paths.current_seq_file(), b"")?;
    atomic_write(&paths.state_file(), &state_bytes)?;
    Ok(Some(state))
}
