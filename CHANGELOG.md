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
