# Performance & Power Measurements

This document records **only measured performance figures** (`PLAN.md` §0 #9 & §7), captured against optimized `--release` builds in isolated temporary `HOME`/`SHIKI_HOME` directories.

---

## T08 Shell Hook & `v0.1.0` MVP Baseline (measured 2026-10-31)

### Environment

- **Binary:** `shiki-term 0.1.0` (`cargo build --release --workspace --locked`)
- **Compiler:** `rustc 1.91.1 (ed61e7d7e 2025-11-07)`
- **Host OS:** `Darwin 27.2.0 (arm64)`
- **Shells tested:**
  - `zsh 5.9 (arm64-apple-darwin26.0)` (`/bin/zsh`)
  - `GNU bash, version 3.2.57(1)-release (arm64-apple-darwin26)` (`/bin/bash`)
- **Configuration:** isolated temporary `HOME`, `ZDOTDIR`, `SHIKI_HOME`, and `XDG_*` directories; manual public reference location `35.7, 139.7` (Tokyo); bundled `shiki` 24-anchor palette; default `hook_min_interval_secs = 60` and `step_threshold = 0.008`.

### Methodology

1. **`zsh` prompt hook fast path (`__shiki_precmd`):** Interactive-TTY `zsh -f` attached to a PTY (`[[ -t 1 ]]` true), `eval "$(shiki-term hook zsh)"`, initial cold prompt executed once, followed by `15` warmup calls and `200` timed calls bracketed in-process with `zsh/datetime` `$EPOCHREALTIME`.
2. **`bash 3.2` prompt hook fast path (`__shiki_prompt_command`):** Interactive-TTY `/bin/bash --noprofile --norc` attached to a PTY (`[ -t 1 ]` true), `eval "$(shiki-term hook bash)"`, initial cold prompt executed once, followed by `15` warmup calls, `200` baseline no-op PTY round-trips, and `200` `__shiki_prompt_command` PTY round-trips (reporting latency net of median PTY IPC round-trip, plus raw unsubtracted PTY `p95`).
3. **`shiki-term _emit --if-stale` (fresh store):** `15` warmup spawns followed by `200` full process invocations while `current.gen` is within `hook_min_interval_secs` (this also bounds the `fish` prompt path, where `$EPOCHSECONDS`/`$SECONDS` shell builtins are unavailable).
4. **`shiki-term _emit --if-stale` (stale store, unchanged frame):** `200` full process invocations advancing `--time` in `65 s` increments through astronomical night (`2026-03-20T15:01:05Z` onward, past `hook_min_interval_secs = 60`), exercising advisory `flock`, full NOAA solar + OKLCH + WCAG computation, `max ΔE_OK < step_threshold`, and 2 atomic `temp -> fsync -> rename` updates (`state.json`, `current.gen`).
5. **`shiki-term _emit --if-stale` (stale store, changed frame):** `200` full process invocations alternating between equinox midnight (`2026-03-20T15:00:00Z`) and summer solstice midday (`2026-06-21T03:00:00Z`) with expired `current.gen`, exercising `flock`, full frame + next-event computation, OSC 4/10/11/12 encoding, and 4 atomic `temp -> fsync -> rename` updates (`current.seq`, `snapshot.json`, `state.json`, `current.gen`).
6. **`shiki-term palette check` year sweep:** `2` warmup runs followed by `10` full process runs of `shiki-term palette check --lat 35.7 --lon 139.7 --year 2026` (`10`-minute steps = `52,560` frames across the full year).

### Measured Latencies vs `PLAN.md` §7 Targets

| Workload | `n` | `min` | `p50` | `p95` | `p99` | `max` | `mean` | `PLAN.md` §7 Target | Status |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| `zsh` hook fast path (`__shiki_precmd`) | 200 | 0.062 ms | 0.067 ms | **0.116 ms** | 0.224 ms | 0.407 ms | 0.077 ms | `p95 < 5 ms` | Pass |
| `bash 3.2` hook fast path (`__shiki_prompt_command`, net; raw PTY `p95 = 0.803 ms`) | 200 | 0.090 ms | 0.166 ms | **0.790 ms** | 2.463 ms | 6.401 ms | 0.314 ms | `p95 < 5 ms` | Pass |
| `shiki-term _emit --if-stale` (fresh store) | 200 | 2.661 ms | 3.805 ms | **5.116 ms** | 7.717 ms | 10.056 ms | 3.967 ms | `p95 < 15 ms` | Pass |
| `shiki-term _emit --if-stale` (stale, unchanged colours) | 200 | 4.292 ms | 5.652 ms | **7.934 ms** | 11.705 ms | 13.691 ms | 5.975 ms | `p95 < 15 ms` | Pass |
| `shiki-term _emit --if-stale` (stale, full colour change + 4 atomic writes) | 200 | 4.556 ms | 5.879 ms | **7.752 ms** | 11.475 ms | 14.300 ms | 6.169 ms | `p95 < 15 ms` | Pass |
| `shiki-term palette check` (`52,560` frames, full year) | 10 | 372.88 ms | 458.71 ms | **611.94 ms** | — | 611.94 ms | 468.99 ms | `< 2000 ms` (`< 2 s`) | Pass |

### Design Notes Behind the Measurements

- **Zero-fork shell fast paths:** `current.gen` stores `<generation> <fresh_until_unix_secs> <interval_secs>\n`. Both `zsh` (`zsh/datetime` `$EPOCHSECONDS`) and `bash` (`$EPOCHSECONDS` on Bash 5+, `$SECONDS` lease fallback on macOS's `/bin/bash` 3.2) check freshness and generation equality using pure shell builtins (`read`, arithmetic comparison, and `config.toml -nt current.gen`) without spawning `shiki-term` or `cat` on unchanged prompts.
- **Lock-free freshness short-circuit:** When `_emit --if-stale` is invoked while fresh (e.g. from `fish` or a newly opened shell tab), `is_hook_store_fresh` verifies `current.gen`, `current.seq`, `state.json` (`paused_until` and `config_hash`), and `config.toml` FNV-1a hash before acquiring `flock` or parsing bundled palettes / `TzDb`.
- **POSIX `fsync(2)` in `atomic_write`:** On macOS, `std::fs::File::sync_all()` issues `fcntl(F_FULLFSYNC)`, flushing physical drive hardware caches (~7–8 ms per file, pushing 4-file updates above 35 ms). Using POSIX `rustix::fs::fsync` on the temporary file and parent directory implements `PLAN.md` §6 (`temp -> fsync -> rename`) while keeping full 4-file updates at `p95 = 7.752 ms`.

### Upcoming `PLAN.md` §7 Measurements

- **T09 Daemon & scheduler:** Predicted-step wake count and zero-polling verification over a simulated 24-hour day.
- **T16 Swift menu bar app:** `DispatchSource` idle watch and Instruments Energy Impact verification.
- **T17 Perf, defaults & accessibility:** Automated `scripts/bench.sh` suite covering every `PLAN.md` §7 target.
