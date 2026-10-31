# Changelog

All notable changes to Shiki will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
