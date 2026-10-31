# Changelog

All notable changes to Shiki will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-31

### Added
- **T01 Workspace & tooling:** Cargo workspace with `sk-core`, `sk-store`, `sk-deliver`, `sk-import`, and `shiki-term` crate stubs.
- Single-source-of-truth product metadata in `crates/sk-core/src/product.rs`, wired into `shiki-term --version`.
- Workspace dependency-graph, allowlist, and single-source tests (`crates/shiki-term/tests/dependency_graph.rs`).
- Zero-network and no-`CoreLocation` privacy lint (`scripts/lint-privacy.sh`) with planted-violation integration tests (`crates/shiki-term/tests/privacy_lint.rs`).
- Toolchain pinning (`rust-toolchain.toml`), `rustfmt.toml`, `clippy.toml`, `deny.toml`, and GitHub Actions CI (`.github/workflows/ci.yml`) for Linux and macOS.
- **T02 Test harness:** `sk-core` purity lint (`scripts/lint-core-purity.sh`) wired into CI with planted-violation tests (`crates/shiki-term/tests/core_purity_lint.rs`).
- Pure UTC fixed-instant helpers (`UtcInstant`, `UtcDateTime`), `Clock` trait, and `FakeClock` in `crates/sk-core/src/time.rs`, plus `SystemClock` in `crates/sk-store/src/clock.rs`.
- Platform-aware `ShikiPaths` (`SHIKI_HOME`, macOS Application Support, Linux XDG), `FixtureDir` loader (`tests/fixtures/{solar,themes,palettes}/`), and isolated `TestEnv` (`tests/support/mod.rs`).
- Safe `rustix`-based `PtyPair` / `PtyChild` helper (`tests/support/pty.rs`), `insta` golden snapshots, and 100× loop determinism tests across `sk-core`, `sk-store`, `sk-deliver`, and `shiki-term`.
- **T03 Solar:** Pure NOAA GML solar position model (refraction-corrected apparent elevation, geometric elevation, azimuth, declination, equation of time, hour angle, and `is_morning`) in `crates/sk-core/src/solar.rs`.
- Tunable SunCalc solar event altitude thresholds (`-0.833°` sunrise/sunset, `-6°`/`-12°`/`-18°` civil/nautical/astronomical twilights, `+6°` golden hour) in `data/model.toml` (`SolarEventAngles`).
- Local solar day event solver (`solar_day_for_date`, `solar_snapshot`, `SolarEvents`, `AltitudeCrossing`) with polar day, polar night, high-latitude twilight, and geographic pole handling (`PolarState`), plus redacted `Coordinates` formatting so coordinates never appear in errors or debug logs.
- Eight public-city NOAA Solar Calculator reference fixtures (`tests/fixtures/solar/noaa_*.toml`) covering both hemispheres, equatorial, and Arctic/Antarctic polar regions across both solstices and equinoxes, verified alongside `insta` golden snapshots, 100× determinism loops, and `proptest` global invariants.
- **T04 Location:** ADR 001 (`docs/decisions/001-jiff-vs-time.md`) adopting `jiff` over `time` for impure civil datetime and system timezone detection while keeping `sk-core` pure and free of both.
- Vendored IANA Time Zone Database `2026f` (`data/tz/{VERSION,zone1970.tab,backward}`) and offline maintainer refresh script (`scripts/update-tzdata.sh`).
- Pure `ManualLocation` (validating bounds before `0.1°` rounding, with redacted `Debug` and coordinate-free errors) and `TzDb` (`zone1970.tab` ISO 6709 coordinate parser + `backward` `Link` resolver mapping aliases like `Asia/Saigon -> Asia/Ho_Chi_Minh` and suggesting `shiki-term locate --set <LAT,LON>` on unknown or non-geographic zones) in `crates/sk-core/src/location.rs`.
- Atomic file persistence (`temp -> fsync -> rename`, `0o600` on Unix) in `crates/sk-store/src/atomic.rs`, offline system IANA timezone detection in `crates/sk-store/src/tz_detect.rs`, and non-destructive `config.toml` `[location]` storage in `crates/sk-store/src/config.rs`.
- `shiki-term locate [--set <LAT,LON> | --timezone]` CLI subcommand (`crates/shiki-term/src/main.rs`) verified with unit, property, golden snapshot, `TestEnv` integration, and coordinate-privacy grep tests.
- **T05 Colour math:** Pure in-house sRGB (`Rgb8`, `Srgb`, `LinearSrgb`), Björn Ottosson OKLab/OKLCH conversions, shortest-arc hue helpers, and perceptual Euclidean `ΔE_OK` (`delta_e_ok`) in `crates/sk-core/src/color.rs`.
- Constant-`(L, h)` chroma-bisection sRGB gamut mapping (`gamut_map_oklch`, `max_in_gamut_chroma`) preserving lightness and hue angle while mapping out-of-gamut colours onto the sRGB boundary.
- WCAG 2.2 relative luminance and contrast ratio (`relative_luminance`, `contrast_ratio`, `contrast_hex`, with `contrast(#000, #fff) = 21.0`) plus tunable `[contrast]` thresholds (`fg_bg_min_ratio = 4.5`, `ansi_bg_min_ratio = 3.0`) in `data/model.toml` (`ContrastThresholds`).
- Oracle comparison against `palette` (dev-only test oracle), discrete 8-bit and continuous float round-trip property tests, metric-space invariants, `insta` golden snapshot, and 100× determinism checks (`crates/sk-core/tests/color_math.rs`).
- **T06 Palette & frames:** Astronomical season phase `s` (anchored at solstices and equinoxes via apparent solar ecliptic longitude, hemisphere-flipped for southern latitudes, and smoothly faded to zero at the equator across `[season] equator_fade_latitude_deg`) in `crates/sk-core/src/season.rs`.
- Piecewise-linear day phase `d` (`[day_phase]` in `data/model.toml`, normalized against `noon_elevation` so low winter suns reach full `day`, with separate morning/evening `dawn != dusk` grids and smooth culmination cross-fade for polar twilight and midnight sun) in `crates/sk-core/src/palette.rs`.
- Palette TOML schema (`Palette`, `Anchor`, `PaletteColors`) with exact `file:line:column` syntax and semantic error diagnostics (`PaletteParseError`), 2-D bilinear OKLCH interpolation with chroma-weighted shortest-arc hue blending and smooth antipodal cross-fade (`oklch_lerp`), and WCAG 2.2 contrast fix-up (`fix_up_colors`: minimal constant-`(C, h)` lightness step away from `bg`, nudging `bg` when required) plus pure `compute_frame` and `check_palette_year`.
- Bundled 24-anchor four-seasons `shiki` palette (`data/palettes/shiki.toml`) with lightness-separated ANSI red and green, palette store loader (`crates/sk-store/src/palette.rs`), and `shiki-term palette check` CLI subcommand (`--i-know` guard for lowered contrast thresholds, coordinate-free worst-moment report), verified by full-year 10-minute sweeps across both hemispheres, polar, and equatorial latitudes (`crates/sk-core/tests/palette_frames.rs`, `crates/sk-store/tests/palette_store.rs`, `crates/shiki-term/tests/palette_cli.rs`).
- **T07 OSC & CLI core:** Portable xterm `ctlseqs` OSC 4/10/11/12 frame encoder, OSC 104/110/111/112 reset encoder, `Backend` trait, and atomic single-write `OscBackend` in `crates/sk-deliver/src/osc.rs`.
- Perceptual OKLab `ΔE_OK` quantizer (`xterm256_color`, `rgb_to_xterm256`) in `crates/sk-core/src/color.rs` and horizon/twilight `next_solar_event` lookup in `crates/sk-core/src/solar.rs`.
- Advisory file lock (`shiki.lock`), versioned coordinate-free `snapshot.json` (`Snapshot`), generation-tracked `state.json` (`StoreState`), `current.seq` persistence (`crates/sk-store/src/store.rs`), `jiff`-backed `--date`/`--time` query resolution (`crates/sk-store/src/moment.rs`), and validated `config get|set` (`crates/sk-store/src/config.rs`).
- `shiki-term now`, `apply`, `reset`, `config get|set`, and inline SGR-only `preview` (swatches + sample text, 256-colour fallback, `NO_COLOR`, `--json`) verified by golden OSC/JSON/output snapshots and PTY tests (`crates/sk-deliver/tests/osc_encoder.rs`, `crates/sk-store/tests/store_and_config.rs`, `crates/shiki-term/tests/osc_cli_core.rs`).
- **T08 Shell hook (`v0.1.0` MVP release):** Plain-text generation and freshness lease file `current.gen` (`GenerationRecord`, `load_generation_record`, `expire_generation_freshness`), lock-free `is_hook_store_fresh` short-circuit, `step_threshold`-gated `refresh_hook_store`, and POSIX `rustix::fs::fsync` atomic persistence in `crates/sk-store/src/{store,atomic,paths,config}.rs`.
- `shiki-term hook zsh|bash|fish` prompt-hook generator (`$EPOCHSECONDS` zero-fork fast path for `zsh` and `bash` 5+, `$SECONDS` zero-fork lease fallback for `bash` 3.2, and `fish_prompt` event handler), hidden `shiki-term _emit [--if-stale] [--time <TIME>]` store refresher, and surgical `shiki-term hook install --write-rc` / `uninstall` marked-block rc editor with `<rc>.shiki-term.bak` backups and byte-for-byte restoration (`crates/shiki-term/src/{hook,main}.rs`).
- Golden hook snapshots, 48-case `proptest` byte-exact rc round-trip tests, live interactive PTY tests across real `zsh`, `bash`, and `fish` shells (`crates/shiki-term/tests/shell_hook.rs`, `crates/sk-store/tests/hook_store.rs`), and measured `--release` prompt-hook / `_emit --if-stale` / `palette check` latencies in `docs/perf.md`.
