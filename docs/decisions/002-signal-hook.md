# ADR 002: Adopt Minimal `signal-hook` (`default-features = false`) for Safe POSIX Signal Wakeups

- **Status:** Accepted
- **Date:** 2026-10-31
- **Task:** T09 Daemon & scheduler (`PLAN.md` §0 #10, §3, §7, §10; also reused by T15 `timelapse`)

## Context

`PLAN.md` §7 specifies the daemon power budget and wakeup contract:

> **Daemon:** no polling. It sleeps until the predicted next above-threshold step (short in twilight, long at night or midday), capped by `max_sleep`, at most one wake per minute, early on SIGHUP. It writes only on change.

In addition:
- `launchd.plist(5)` and `systemd.service(5)` terminate foreground user services with `SIGTERM` (and interactive `shiki-term daemon run` or T15 `shiki-term timelapse` receives `SIGINT`), requiring clean shutdown without leaving temporary files or half-written sequences.
- `shiki-term pause`, `shiki-term resume`, `shiki-term config set`, and `shiki-term locate` signal any active user daemon with `SIGHUP` (`systemd` `ExecReload=/bin/kill -HUP $MAINPID`) so configuration and pause transitions take effect immediately without polling.
- Workspace `Cargo.toml` enforces `unsafe_code = "forbid"` across every workspace crate.

While `rustix` (already in `PLAN.md` §3's allowlist) provides safe wrappers for *sending* signals (`rustix::process::kill_process`), creating pipes (`rustix::pipe::pipe`), and blocking without polling (`rustix::event::poll`), `rustix` deliberately omits `sigaction` signal-handler registration because installing raw POSIX signal handlers is `unsafe`.

## Options Considered

1. **`signal-hook` (`0.3`, `default-features = false`) + `rustix::pipe::pipe` + `rustix::event::poll`:**
   - Exposes safe, audited async-signal-safe primitives (`signal_hook::flag::register` and `signal_hook::low_level::pipe::register`) without requiring `unsafe` in any Shiki crate.
   - With `default-features = false`, omits background iterator threads and channel machinery, adding only `signal-hook` and `signal-hook-registry` (`Apache-2.0 OR MIT`), whose sole transitive dependency is `libc` (already present in `Cargo.lock` via `rustix`).
   - Zero networking APIs or crates; passes `cargo deny check` and `scripts/lint-privacy.sh`.
2. **Relaxing `unsafe_code = "forbid"` in `shiki-term` and writing raw `libc::sigaction` handlers:**
   - Rejected: weakens workspace memory-safety guarantees and re-implements subtle async-signal-safe self-pipe registration/unregistration logic already audited in `signal-hook`.
3. **Periodic short-interval polling loop without `SIGHUP` pipe wakeup:**
   - Rejected: directly violates `PLAN.md` §7 (*"no polling... at most one wake per minute, early on SIGHUP"*).

## Decision

1. Add `signal-hook = { version = "0.3", default-features = false }` to `[workspace.dependencies]` and scope it exclusively to the impure CLI/daemon binary crate (`crates/shiki-term`).
2. Keep `sk-core`, `sk-store`, `sk-deliver`, and `sk-import` completely free of `signal-hook` (enforced by `crates/shiki-term/tests/dependency_graph.rs`).
3. Combine `signal_hook::flag::register` (`AtomicBool` flags registered first) with `signal_hook::low_level::pipe::register` (writing a single wake byte into a non-blocking `rustix::pipe::pipe()` fd) and block on `rustix::event::poll` for the exact predicted-step sleep timeout.

## Consequences

- `shiki-term daemon run` sleeps for the full predicted interval (`60..=max_sleep_secs`, up to 30 minutes by default) with zero background threads and zero polling wakes, waking immediately on `SIGHUP`, `SIGTERM`, or `SIGINT`.
- Preserves `unsafe_code = "forbid"` across the entire workspace, `sk-core` purity (`scripts/lint-core-purity.sh`), license policy (`deny.toml`), and offline privacy (`scripts/lint-privacy.sh`).
- Provides the same safe `SIGINT` handler primitive needed in T15 (`timelapse` terminal restoration after `SIGINT`).
