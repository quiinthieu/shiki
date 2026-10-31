# ADR 001: Choose `jiff` over `time` for Impure Civil Datetime and System Timezone Handling

- **Status:** Accepted
- **Date:** 2026-10-31
- **Task:** T04 Location (`PLAN.md` §0 #10, §3, §4, §10)

## Context

`PLAN.md` §3 lists `jiff` or `time` (mutually exclusive, gated on an ADR in `docs/decisions/NNN-*.md`) as the allowed datetime crate in the workspace, while `PLAN.md` §0 #3 requires `sk-core` to remain 100% pure (no filesystem I/O, no wall/monotonic clock calls, and no environment variable access).

Across v1, Shiki has three distinct time and timezone responsibilities:

1. **Pure UTC solar and scheduling math (`sk-core`):** Exact Unix-second UTC instants (`UtcInstant`), proleptic Gregorian UTC calendar conversions, `Clock` / `FakeClock` injection (T02), and NOAA Julian-century solar equations (T03).
2. **IANA timezone -> principal coordinates (`sk-core` + `sk-store`, T04):** Mapping an IANA timezone identifier (such as `Asia/Saigon` or `Asia/Ho_Chi_Minh`) via vendored IANA `backward` links to principal coordinates in vendored `zone1970.tab`, plus detecting the host OS's configured IANA timezone offline without network or `CoreLocation`.
3. **Local civil wall-clock parsing and formatting (`sk-store` + `shiki-term`, T07/T15):** Interpreting user-supplied local civil flags (`preview --time 06:30 --date 2026-06-21`, `timelapse --date`) and formatting solar event timestamps in the user's local timezone (including historical and future DST transitions and fold/gap disambiguation).

We evaluated `jiff` (`0.2`) against `time` (`0.3`) against these three requirements.

## Evaluation: `jiff` vs `time`

| Criterion | `jiff` (`0.2`) | `time` (`0.3`) |
|---|---|---|
| **IANA timezone database & TZif support** | First-class built-in TZif parser reading `/usr/share/zoneinfo` (`tzdb-zoneinfo`) and POSIX `TZ` strings (`tz-system`) with no extra crates. | None built in. `time::UtcOffset` represents fixed numeric offsets only; IANA zone lookup requires external crates (`iana-time-zone`, `tz-rs`, `chrono-tz`) outside `PLAN.md` §3's allowlist. |
| **System IANA timezone detection** | Built-in `jiff::tz::TimeZone::system()` with `.iana_name()` fallback across macOS, Linux, and WSL. | Cannot determine the system IANA zone name (only current numeric UTC offset via `local-offset`, which also has known soundness caveats around `localtime_r` and process env mutation on Unix). |
| **Local civil `--time` / `--date` + DST folds/gaps** | `jiff::Zoned` and `jiff::civil` resolve DST transitions, folds, and gaps deterministically by default (`Disambiguation::Compatible`). | Cannot resolve DST transitions for arbitrary dates in an IANA zone because a single fixed `UtcOffset` does not capture seasonal DST rules. |
| **Dependency & license footprint** | Dual-licensed `Unlicense OR MIT` (compatible with `deny.toml`); with `default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo"]`, pulls zero networking crates and omits redundant bundled tzdb blobs on Unix. | Dual-licensed `MIT OR Apache-2.0` (`time-core`, `num-conv`, `powerfmt`, `deranged`), small footprint, but insufficient on its own for IANA timezone operations without unapproved companion crates. |

## Decision

1. **Adopt `jiff` (and reject `time`)** in the workspace, configured with minimal Unix features:
   ```toml
   jiff = { version = "0.2", default-features = false, features = ["std", "tz-system", "tzdb-zoneinfo"] }
   ```
2. **Keep `sk-core` 100% pure and free of `jiff`:** `sk-core` continues to use its zero-dependency `UtcInstant` (`crates/sk-core/src/time.rs`) and its pure compile-time-vendored `zone1970.tab` + `backward` resolver (`crates/sk-core/src/location.rs`). Time and coordinates remain explicit function arguments in `sk-core`.
3. **Scope `jiff` to impure boundary crates (`sk-store`, and `shiki-term` when civil CLI formatting/parsing lands in T07):**
   - In `sk-store`, explicit `TZ` / `/etc/localtime` / `/etc/timezone` inspection runs first (so explicit unknown `TZ` values like `TZ=Unknown/Zone` are surfaced directly rather than silently falling back to `/etc/localtime` or `UTC`, and `TestEnv` tests remain 100% deterministic without mutating host files), with `jiff::tz::TimeZone::system().iana_name()` serving as the TZif-aware OS fallback.
   - In T07/T15, `jiff` will handle local civil `--time` / `--date` <-> `UtcInstant` conversions.

## Consequences

- Satisfies `PLAN.md` §3 (`jiff` or `time`, never both; verified by `crates/shiki-term/tests/dependency_graph.rs`).
- Preserves `sk-core` purity (`scripts/lint-core-purity.sh` and `sk_core_has_no_impure_dependencies`) and offline privacy (`scripts/lint-privacy.sh`, `cargo deny check`).
- Eliminates any need for extra unapproved timezone helper crates (`iana-time-zone`, `chrono-tz`).
