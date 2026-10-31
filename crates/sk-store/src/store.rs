//! Atomic persistence for `state.json`, `snapshot.json`, `current.seq`, `current.gen`, and
//! `shiki.lock` (`PLAN.md` §6, T07 & T08).
//!
//! Privacy (`PLAN.md` §0 #4): `snapshot.json`, `state.json`, and `current.gen` never contain
//! coordinates.

use crate::atomic::atomic_write;
use crate::config::{load_config, resolve_location, DEFAULT_HOOK_MIN_INTERVAL_SECS};
use crate::error::StoreError;
use crate::moment::resolve_moment_query;
use crate::palette::load_palette;
use crate::paths::ShikiPaths;
use rustix::fs::{flock, fsync, FlockOperation};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sk_core::location::ResolvedLocation;
use sk_core::palette::{compute_frame, DayHalf, Frame, ModelConfig, PaletteColors, SolarPhase};
use sk_core::scheduler::{compute_pause_sleep_secs, predict_next_step_sleep_secs};
use sk_core::season::Season;
use sk_core::solar::{next_solar_event, Coordinates, PolarState, SolarEventKind};
use sk_core::time::{UtcInstant, SECS_PER_DAY};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Sentinel stored in [`StoreState::paused_until`] when `pause` is invoked without `--for`.
pub const INDEFINITE_PAUSE: &str = "indefinite";

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

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

/// Exclusive non-blocking advisory `flock` guard over `paths.daemon_pid_file()` (`daemon.pid`).
///
/// The single persistent inode is never unlinked, eliminating unlink/recreate races. While held,
/// [`probe_running_daemon_pid`] observes `EWOULDBLOCK` on a non-blocking shared `flock` and returns
/// the active daemon PID; once the daemon exits (cleanly or via `SIGKILL`), the kernel releases the
/// lock automatically so stale PID files and recycled OS PIDs are never misreported as running.
#[derive(Debug)]
pub struct DaemonPidGuard {
    _file: File,
    pid: u32,
}

impl DaemonPidGuard {
    pub fn acquire(paths: &ShikiPaths) -> Result<Self, StoreError> {
        Self::acquire_with_pid(paths, std::process::id())
    }

    pub fn acquire_with_pid(paths: &ShikiPaths, pid: u32) -> Result<Self, StoreError> {
        let pid_path = paths.daemon_pid_file();
        if let Some(parent) = pid_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut opts = OpenOptions::new();
        opts.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&pid_path)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                let existing_pid = read_pid_from_file(&mut file);
                return Err(StoreError::DaemonAlreadyRunning { pid: existing_pid });
            }
            Err(err) => {
                return Err(StoreError::Io(io::Error::from_raw_os_error(
                    err.raw_os_error(),
                )));
            }
        }

        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{pid}")?;
        fsync(&file)
            .map_err(|err| StoreError::Io(io::Error::from_raw_os_error(err.raw_os_error())))?;
        Ok(Self { _file: file, pid })
    }

    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }
}

fn read_pid_from_file(file: &mut File) -> Option<u32> {
    let _ = file.seek(SeekFrom::Start(0));
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    raw.trim().parse::<u32>().ok()
}

/// Probes `paths.daemon_pid_file()` via a non-blocking shared `flock` and returns `Ok(Some(pid))`
/// if and only if an active [`DaemonPidGuard`] holds the exclusive lock.
pub fn probe_running_daemon_pid(paths: &ShikiPaths) -> Result<Option<u32>, StoreError> {
    let pid_path = paths.daemon_pid_file();
    let mut file = match File::open(&pid_path) {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StoreError::Io(err)),
    };

    match flock(&file, FlockOperation::NonBlockingLockShared) {
        Ok(()) => {
            let _ = flock(&file, FlockOperation::Unlock);
            Ok(None)
        }
        Err(Errno::WOULDBLOCK) => Ok(read_pid_from_file(&mut file)),
        Err(err) => Err(StoreError::Io(io::Error::from_raw_os_error(
            err.raw_os_error(),
        ))),
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

fn fnv1a_bytes_hex(bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// Deterministic 64-bit FNV-1a hex digest over all 21 8-bit sRGB slots in [`PaletteColors`].
#[must_use]
pub fn compute_frame_hash(colors: &PaletteColors) -> String {
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

/// Computes a deterministic 64-bit FNV-1a hex fingerprint of `config.toml` (or `"none"` when
/// `config.toml` has not been created yet) so prompt-hook freshness detects configuration edits
/// deterministically under both real and simulated clocks.
pub fn compute_config_file_hash(paths: &ShikiPaths) -> Result<String, StoreError> {
    match fs::read(paths.config_file()) {
        Ok(bytes) => Ok(fnv1a_bytes_hex(&bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok("none".to_string()),
        Err(err) => Err(StoreError::Io(err)),
    }
}

/// Compact plain-text generation token and freshness lease persisted in `current.gen` (`PLAN.md` §10 T08).
///
/// Format: `"<generation> <fresh_until_unix_secs> <interval_secs>\n"`, enabling pure-shell `read`
/// fast paths (using `$EPOCHSECONDS` in `zsh` / `bash` 5+ and `$SECONDS` in `bash` 3.2) with zero
/// subprocesses or JSON parsing on unchanged prompts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GenerationRecord {
    pub generation: u64,
    pub fresh_until_unix_secs: i64,
    pub interval_secs: i64,
}

impl GenerationRecord {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split_whitespace();
        let generation = parts.next()?.parse::<u64>().ok()?;
        let fresh_until_unix_secs = match parts.next() {
            Some(tok) => tok.parse::<i64>().ok()?,
            None => 0,
        };
        let interval_secs = match parts.next() {
            Some(tok) => tok.parse::<i64>().ok()?,
            None => 0,
        };
        Some(Self {
            generation,
            fresh_until_unix_secs,
            interval_secs,
        })
    }

    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        format!(
            "{} {} {}\n",
            self.generation, self.fresh_until_unix_secs, self.interval_secs
        )
        .into_bytes()
    }
}

/// Reads `current.gen`, returning `Ok(None)` if it does not exist yet or cannot be parsed.
pub fn load_generation_record(paths: &ShikiPaths) -> Result<Option<GenerationRecord>, StoreError> {
    match fs::read_to_string(paths.current_gen_file()) {
        Ok(raw) => Ok(GenerationRecord::parse(&raw)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(StoreError::Io(err)),
    }
}

/// Sets `fresh_until_unix_secs = 0` and `interval_secs = 0` in `current.gen` (if present) without
/// altering `generation`, forcing the next shell prompt to invoke `_emit --if-stale`.
pub fn expire_generation_freshness(paths: &ShikiPaths) -> Result<(), StoreError> {
    if let Some(mut rec) = load_generation_record(paths)? {
        if rec.fresh_until_unix_secs != 0 || rec.interval_secs != 0 {
            rec.fresh_until_unix_secs = 0;
            rec.interval_secs = 0;
            atomic_write(&paths.current_gen_file(), &rec.encode())?;
        }
    }
    Ok(())
}

/// Effective pause status evaluated against a reference [`UtcInstant`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PauseStatus {
    NotPaused,
    Indefinite,
    Until(UtcInstant),
}

impl PauseStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::NotPaused)
    }

    #[must_use]
    pub const fn until_instant(self) -> Option<UtcInstant> {
        match self {
            Self::Until(until) => Some(until),
            Self::NotPaused | Self::Indefinite => None,
        }
    }
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
    pub checked_at_unix_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_colors: Option<PaletteColors>,
}

impl StoreState {
    #[must_use]
    pub fn pause_status_at(&self, now: UtcInstant) -> PauseStatus {
        let Some(raw) = self.paused_until.as_deref() else {
            return PauseStatus::NotPaused;
        };
        if raw.eq_ignore_ascii_case(INDEFINITE_PAUSE) {
            return PauseStatus::Indefinite;
        }
        match UtcInstant::parse_rfc3339(raw) {
            Ok(until) if now < until => PauseStatus::Until(until),
            _ => PauseStatus::NotPaused,
        }
    }
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
            checked_at_unix_secs: None,
            config_hash: None,
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

fn effective_hook_interval_i64(paths: &ShikiPaths) -> i64 {
    let secs = load_config(paths)
        .map(|cfg| cfg.effective_hook_min_interval_secs())
        .unwrap_or(DEFAULT_HOOK_MIN_INTERVAL_SECS);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

struct LeaseMeta {
    checked_at_unix_secs: i64,
    interval_secs: i64,
    cfg_hash: String,
}

fn write_applied_frame_locked(
    paths: &ShikiPaths,
    mut state: StoreState,
    palette_name: &str,
    snapshot: &Snapshot,
    osc_bytes: &[u8],
    lease: LeaseMeta,
) -> Result<StoreState, StoreError> {
    let new_hash = compute_frame_hash(&snapshot.colors);
    let seq_missing_or_empty = fs::metadata(paths.current_seq_file())
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let gen_missing = !paths.current_gen_file().exists();

    if state.frame_hash.as_deref() != Some(new_hash.as_str()) || seq_missing_or_empty || gen_missing
    {
        state.generation = state.generation.saturating_add(1);
    }
    state.schema_version = STATE_SCHEMA_VERSION;
    state.frame_hash = Some(new_hash);
    state.palette = Some(palette_name.to_string());
    state.updated_at_rfc3339 = Some(snapshot.instant_rfc3339.clone());
    state.updated_at_unix_secs = Some(snapshot.unix_secs);
    state.checked_at_unix_secs = Some(lease.checked_at_unix_secs);
    state.config_hash = Some(lease.cfg_hash);
    state.last_colors = Some(snapshot.colors);

    let fresh_until = lease
        .checked_at_unix_secs
        .saturating_add(lease.interval_secs);
    let gen_rec = GenerationRecord {
        generation: state.generation,
        fresh_until_unix_secs: fresh_until,
        interval_secs: lease.interval_secs,
    };

    let snapshot_bytes = serialize_json_pretty(snapshot)?;
    let state_bytes = serialize_json_pretty(&state)?;

    atomic_write(&paths.current_seq_file(), osc_bytes)?;
    atomic_write(&paths.snapshot_file(), &snapshot_bytes)?;
    atomic_write(&paths.state_file(), &state_bytes)?;
    atomic_write(&paths.current_gen_file(), &gen_rec.encode())?;

    Ok(state)
}

/// Atomically persists `current.seq`, `current.gen`, `snapshot.json`, and `state.json` under
/// `shiki.lock`, using `snapshot.unix_secs` as the freshness check timestamp.
pub fn persist_applied_frame(
    paths: &ShikiPaths,
    palette_name: &str,
    snapshot: &Snapshot,
    osc_bytes: &[u8],
) -> Result<StoreState, StoreError> {
    persist_applied_frame_at(
        paths,
        palette_name,
        snapshot,
        osc_bytes,
        UtcInstant::from_unix_secs(snapshot.unix_secs),
    )
}

/// Atomically persists `current.seq`, `current.gen`, `snapshot.json`, and `state.json` under
/// `shiki.lock` with an explicit wall-clock `checked_at` timestamp for `current.gen` freshness.
///
/// Increments `state.generation` whenever the encoded frame hash changed (or `current.seq` /
/// `current.gen` was missing/empty), keeping `generation` unchanged on identical repeat frames.
pub fn persist_applied_frame_at(
    paths: &ShikiPaths,
    palette_name: &str,
    snapshot: &Snapshot,
    osc_bytes: &[u8],
    checked_at: UtcInstant,
) -> Result<StoreState, StoreError> {
    let _lock = StoreLock::acquire(paths)?;
    let state = load_state(paths)?;
    let interval_secs = effective_hook_interval_i64(paths);
    let cfg_hash = compute_config_file_hash(paths)?;
    write_applied_frame_locked(
        paths,
        state,
        palette_name,
        snapshot,
        osc_bytes,
        LeaseMeta {
            checked_at_unix_secs: checked_at.as_unix_secs(),
            interval_secs,
            cfg_hash,
        },
    )
}

/// Clears `current.seq` and clears `frame_hash` in `state.json` (bumping `generation` and
/// `current.gen`) if store state has already been initialized, leaving uninitialized systems
/// untouched (`PLAN.md` T19).
pub fn persist_reset(
    paths: &ShikiPaths,
    reset_at: UtcInstant,
) -> Result<Option<StoreState>, StoreError> {
    let has_existing_store = paths.state_file().exists()
        || paths.current_seq_file().exists()
        || paths.current_gen_file().exists()
        || paths.snapshot_file().exists();
    if !has_existing_store {
        return Ok(None);
    }

    let _lock = StoreLock::acquire(paths)?;
    let mut state = load_state(paths)?;
    let interval_secs = effective_hook_interval_i64(paths);
    let cfg_hash = compute_config_file_hash(paths)?;
    let reset_unix = reset_at.as_unix_secs();

    state.schema_version = STATE_SCHEMA_VERSION;
    state.generation = state.generation.saturating_add(1);
    state.frame_hash = None;
    state.updated_at_rfc3339 = Some(reset_at.format_rfc3339_utc());
    state.updated_at_unix_secs = Some(reset_unix);
    state.checked_at_unix_secs = Some(reset_unix);
    state.config_hash = Some(cfg_hash);

    let gen_rec = GenerationRecord {
        generation: state.generation,
        fresh_until_unix_secs: reset_unix.saturating_add(interval_secs),
        interval_secs,
    };

    let state_bytes = serialize_json_pretty(&state)?;
    atomic_write(&paths.current_seq_file(), b"")?;
    atomic_write(&paths.state_file(), &state_bytes)?;
    atomic_write(&paths.current_gen_file(), &gen_rec.encode())?;
    Ok(Some(state))
}

/// Fast, zero-computation freshness probe for `_emit --if-stale` (`PLAN.md` §5 & §7).
///
/// Returns `Ok(true)` without parsing palettes, `TzDb`, or `ModelConfig` when drift is actively
/// paused (`pause` or `pause --for`), or when `current.gen` and `current.seq` exist, `config.toml`
/// matches `state.config_hash`, and `now_unix_secs` lies within
/// `[checked_at_unix_secs, fresh_until_unix_secs)`.
pub fn is_hook_store_fresh(paths: &ShikiPaths, now_unix_secs: i64) -> Result<bool, StoreError> {
    let state = load_state(paths)?;
    if state
        .pause_status_at(UtcInstant::from_unix_secs(now_unix_secs))
        .is_active()
    {
        return Ok(true);
    }

    let Some(gen_rec) = load_generation_record(paths)? else {
        return Ok(false);
    };
    if !paths.current_seq_file().exists() {
        return Ok(false);
    }

    let current_cfg_hash = compute_config_file_hash(paths)?;
    if state.config_hash.as_deref() != Some(current_cfg_hash.as_str()) {
        return Ok(false);
    }

    let Some(last_checked) = state.checked_at_unix_secs.or(state.updated_at_unix_secs) else {
        return Ok(false);
    };

    Ok(now_unix_secs >= last_checked && now_unix_secs < gen_rec.fresh_until_unix_secs)
}

/// Outcome of a prompt-hook store refresh (`_emit` / `_emit --if-stale`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HookRefreshOutcome {
    /// Store was fresh (within `hook_min_interval_secs`) or paused; no computation or write occurred.
    Fresh { generation: u64 },
    /// Store was stale and recomputed, but the frame did not cross `step_threshold` (or was
    /// identical), keeping `generation` and `current.seq` unchanged.
    Unchanged { generation: u64 },
    /// Store was initialized or crossed `step_threshold`, updating `current.seq`, `current.gen`,
    /// `snapshot.json`, and `state.json`.
    Updated { generation: u64 },
}

fn iana_zone_from_resolved(resolved: &ResolvedLocation) -> Option<&str> {
    match resolved {
        ResolvedLocation::Timezone(tz) => Some(tz.canonical_zone.as_str()),
        ResolvedLocation::Manual(_) => None,
    }
}

/// Refreshes the prompt-hook store (`current.seq`, `current.gen`, `snapshot.json`, `state.json`)
/// under `shiki.lock`, recomputing at most once per `hook_min_interval_secs` when `if_stale` is
/// `true` (`PLAN.md` §5, §7, §10 T08).
pub fn refresh_hook_store<F>(
    paths: &ShikiPaths,
    if_stale: bool,
    time_override: Option<&str>,
    now_utc: UtcInstant,
    encode_osc: F,
) -> Result<HookRefreshOutcome, StoreError>
where
    F: FnOnce(&PaletteColors) -> Vec<u8>,
{
    let pre_instant = match time_override {
        None => Some(now_utc),
        Some(raw) => UtcInstant::parse_rfc3339(raw).ok(),
    };

    if if_stale {
        if let Some(inst) = pre_instant {
            if is_hook_store_fresh(paths, inst.as_unix_secs())? {
                let generation = load_generation_record(paths)?.map_or(0, |r| r.generation);
                return Ok(HookRefreshOutcome::Fresh { generation });
            }
        }
    }

    let _lock = StoreLock::acquire(paths)?;

    if if_stale {
        if let Some(inst) = pre_instant {
            if is_hook_store_fresh(paths, inst.as_unix_secs())? {
                let generation = load_generation_record(paths)?.map_or(0, |r| r.generation);
                return Ok(HookRefreshOutcome::Fresh { generation });
            }
        }
    }

    let mut state = load_state(paths)?;
    if let Some(inst) = pre_instant {
        if state.pause_status_at(inst).is_active() {
            return Ok(HookRefreshOutcome::Fresh {
                generation: state.generation,
            });
        }
    }

    let config = load_config(paths)?;
    let model = config.effective_model_config()?;
    let resolved = resolve_location(paths)?;
    let instant = match pre_instant {
        Some(inst) => inst,
        None => {
            let zone = iana_zone_from_resolved(&resolved).or(config.location.timezone.as_deref());
            resolve_moment_query(None, time_override, zone, now_utc)?
        }
    };

    if state.pause_status_at(instant).is_active() {
        return Ok(HookRefreshOutcome::Fresh {
            generation: state.generation,
        });
    }

    if if_stale && pre_instant.is_none() && is_hook_store_fresh(paths, instant.as_unix_secs())? {
        let generation = load_generation_record(paths)?.map_or(0, |r| r.generation);
        return Ok(HookRefreshOutcome::Fresh { generation });
    }

    let palette = load_palette(paths, config.effective_palette())?;
    let coords = resolved.coordinates();
    let frame = compute_frame(instant, coords, &palette, &model);

    state.paused_until = None;
    let cfg_hash = compute_config_file_hash(paths)?;
    let interval_secs =
        i64::try_from(config.effective_hook_min_interval_secs()).unwrap_or(i64::MAX);
    let now_unix = instant.as_unix_secs();

    let new_hash = compute_frame_hash(&frame.colors);
    let seq_missing_or_empty = fs::metadata(paths.current_seq_file())
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let gen_missing = !paths.current_gen_file().exists();
    let first_or_missing = state.frame_hash.is_none() || seq_missing_or_empty || gen_missing;
    let meta_changed = state.palette.as_deref() != Some(palette.name())
        || state.config_hash.as_deref() != Some(cfg_hash.as_str());
    let hash_differs = state.frame_hash.as_deref() != Some(new_hash.as_str());
    let delta_e = state
        .last_colors
        .as_ref()
        .map_or(f64::INFINITY, |prev| prev.max_delta_e(&frame.colors));
    let color_stepped = hash_differs && delta_e >= config.effective_step_threshold();

    let should_apply = first_or_missing || (meta_changed && hash_differs) || color_stepped;

    if !should_apply {
        state.schema_version = STATE_SCHEMA_VERSION;
        state.checked_at_unix_secs = Some(now_unix);
        state.config_hash = Some(cfg_hash);
        let gen_rec = GenerationRecord {
            generation: state.generation,
            fresh_until_unix_secs: now_unix.saturating_add(interval_secs),
            interval_secs,
        };
        let state_bytes = serialize_json_pretty(&state)?;
        atomic_write(&paths.state_file(), &state_bytes)?;
        atomic_write(&paths.current_gen_file(), &gen_rec.encode())?;
        return Ok(HookRefreshOutcome::Unchanged {
            generation: state.generation,
        });
    }

    let snapshot = Snapshot::from_frame(palette.name(), &frame, coords, &model);
    let osc_bytes = encode_osc(&frame.colors);
    let updated_state = write_applied_frame_locked(
        paths,
        state,
        palette.name(),
        &snapshot,
        &osc_bytes,
        LeaseMeta {
            checked_at_unix_secs: now_unix,
            interval_secs,
            cfg_hash,
        },
    )?;
    Ok(HookRefreshOutcome::Updated {
        generation: updated_state.generation,
    })
}

/// Persists `paused_until` in `state.json` (either `"indefinite"` when `duration_secs` is `None`,
/// or `now_utc + secs` in RFC 3339 UTC) and extends any existing `current.gen` freshness lease so
/// active shell prompt hooks avoid spawning subprocesses while paused (`PLAN.md` §6 & §10 T09).
pub fn set_pause(
    paths: &ShikiPaths,
    duration_secs: Option<i64>,
    now_utc: UtcInstant,
) -> Result<StoreState, StoreError> {
    if let Some(secs) = duration_secs {
        if secs <= 0 || secs > 365 * SECS_PER_DAY {
            return Err(StoreError::InvalidPauseDuration(format!("{secs}s")));
        }
    }

    let _lock = StoreLock::acquire(paths)?;
    let mut state = load_state(paths)?;
    state.schema_version = STATE_SCHEMA_VERSION;

    let (paused_until_str, lease_until, lease_ttl) = match duration_secs {
        Some(secs) => {
            let until = now_utc.add_secs(secs);
            (
                until.format_rfc3339_utc(),
                until.as_unix_secs(),
                secs.min(SECS_PER_DAY),
            )
        }
        None => (
            INDEFINITE_PAUSE.to_string(),
            now_utc.as_unix_secs().saturating_add(365 * SECS_PER_DAY),
            SECS_PER_DAY,
        ),
    };
    state.paused_until = Some(paused_until_str);

    let state_bytes = serialize_json_pretty(&state)?;
    atomic_write(&paths.state_file(), &state_bytes)?;

    if let Some(mut gen_rec) = load_generation_record(paths)? {
        gen_rec.fresh_until_unix_secs = lease_until;
        gen_rec.interval_secs = lease_ttl;
        atomic_write(&paths.current_gen_file(), &gen_rec.encode())?;
    }

    Ok(state)
}

/// Clears `paused_until` in `state.json` and expires `current.gen` freshness so both the daemon
/// and shell prompt hooks resume drift immediately (`PLAN.md` §6 & §10 T09).
pub fn clear_pause(paths: &ShikiPaths) -> Result<StoreState, StoreError> {
    let _lock = StoreLock::acquire(paths)?;
    let mut state = load_state(paths)?;
    state.schema_version = STATE_SCHEMA_VERSION;
    state.paused_until = None;

    let state_bytes = serialize_json_pretty(&state)?;
    atomic_write(&paths.state_file(), &state_bytes)?;
    expire_generation_freshness(paths)?;
    Ok(state)
}

/// Outcome of a single daemon scheduler evaluation (`PLAN.md` §3, §7, & §10 T09).
#[derive(Clone, Debug, PartialEq)]
pub enum DaemonTickOutcome {
    /// Drift is paused (`pause` or `pause --for`); zero files were written.
    Paused {
        generation: u64,
        sleep_secs: u64,
        until: Option<UtcInstant>,
    },
    /// Frame did not cross `step_threshold` (or matched `frame_hash`); zero files were written.
    Unchanged {
        generation: u64,
        sleep_secs: u64,
        frame: Frame,
    },
    /// Store was initialized or crossed `step_threshold`, atomically writing `current.seq`,
    /// `snapshot.json`, `state.json`, and `current.gen`.
    Updated {
        generation: u64,
        sleep_secs: u64,
        frame: Frame,
    },
}

impl DaemonTickOutcome {
    #[must_use]
    pub const fn sleep_secs(&self) -> u64 {
        match self {
            Self::Paused { sleep_secs, .. }
            | Self::Unchanged { sleep_secs, .. }
            | Self::Updated { sleep_secs, .. } => *sleep_secs,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        match self {
            Self::Paused { generation, .. }
            | Self::Unchanged { generation, .. }
            | Self::Updated { generation, .. } => *generation,
        }
    }
}

/// Evaluates one daemon scheduler wake at `now_utc`, persisting store files **only on change** and
/// returning the predicted sleep duration until the next wake (`PLAN.md` §3, §7, & §10 T09).
pub fn daemon_tick<F>(
    paths: &ShikiPaths,
    now_utc: UtcInstant,
    encode_osc: F,
) -> Result<DaemonTickOutcome, StoreError>
where
    F: FnOnce(&PaletteColors) -> Vec<u8>,
{
    let _lock = StoreLock::acquire(paths)?;
    let config = load_config(paths)?;
    let model = config.effective_model_config()?;
    let mut state = load_state(paths)?;

    match state.pause_status_at(now_utc) {
        PauseStatus::Indefinite => {
            let sleep_secs =
                compute_pause_sleep_secs(now_utc, None, &model, config.effective_max_sleep_secs());
            return Ok(DaemonTickOutcome::Paused {
                generation: state.generation,
                sleep_secs,
                until: None,
            });
        }
        PauseStatus::Until(until) => {
            let sleep_secs = compute_pause_sleep_secs(
                now_utc,
                Some(until),
                &model,
                config.effective_max_sleep_secs(),
            );
            return Ok(DaemonTickOutcome::Paused {
                generation: state.generation,
                sleep_secs,
                until: Some(until),
            });
        }
        PauseStatus::NotPaused => {}
    }

    let resolved = resolve_location(paths)?;
    let palette = load_palette(paths, config.effective_palette())?;
    let coords = resolved.coordinates();
    let frame = compute_frame(now_utc, coords, &palette, &model);

    let new_hash = compute_frame_hash(&frame.colors);
    let seq_missing_or_empty = fs::metadata(paths.current_seq_file())
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let gen_missing = !paths.current_gen_file().exists();
    let snapshot_missing = !paths.snapshot_file().exists();
    let first_or_missing = state.frame_hash.is_none()
        || state.last_colors.is_none()
        || seq_missing_or_empty
        || gen_missing
        || snapshot_missing;
    let cfg_hash = compute_config_file_hash(paths)?;
    let meta_changed = state.palette.as_deref() != Some(palette.name())
        || state.config_hash.as_deref() != Some(cfg_hash.as_str());
    let hash_differs = state.frame_hash.as_deref() != Some(new_hash.as_str());
    let delta_e = state
        .last_colors
        .as_ref()
        .map_or(f64::INFINITY, |prev| prev.max_delta_e(&frame.colors));
    let color_stepped = hash_differs && delta_e >= config.effective_step_threshold();
    let should_apply = first_or_missing || (meta_changed && hash_differs) || color_stepped;

    let baseline_colors = if should_apply {
        &frame.colors
    } else {
        state.last_colors.as_ref().unwrap_or(&frame.colors)
    };
    let sleep_secs = predict_next_step_sleep_secs(
        now_utc,
        baseline_colors,
        coords,
        &palette,
        &model,
        config.effective_step_threshold(),
        config.effective_max_sleep_secs(),
    );

    if !should_apply {
        return Ok(DaemonTickOutcome::Unchanged {
            generation: state.generation,
            sleep_secs,
            frame,
        });
    }

    state.paused_until = None;
    let snapshot = Snapshot::from_frame(palette.name(), &frame, coords, &model);
    let osc_bytes = encode_osc(&frame.colors);
    let interval_secs = i64::try_from(sleep_secs).unwrap_or(i64::MAX);
    let updated_state = write_applied_frame_locked(
        paths,
        state,
        palette.name(),
        &snapshot,
        &osc_bytes,
        LeaseMeta {
            checked_at_unix_secs: now_utc.as_unix_secs(),
            interval_secs,
            cfg_hash,
        },
    )?;

    Ok(DaemonTickOutcome::Updated {
        generation: updated_state.generation,
        sleep_secs,
        frame,
    })
}
