# Shiki: Implementation Plan (v1)

*Shiki (四季, "four seasons", formerly "Dusk Theme"): your terminal colours drift slowly with local sunrise, sunset and the seasons, like a window onto the day outside.*

> **Names:** display name **Shiki**. The CLI binary and crate are **`shiki-term`**, the Homebrew formula is **`shiki-term`**, the menu bar cask is **`shiki-term-app`** and the bundle id is **`io.github.quiinthieu.shiki-term`**. They're defined once in `crates/sk-core/src/product.rs` and mirrored in `macos/Config/Product.xcconfig`. **Don't hardcode them elsewhere.** §1.4 has the clash check.

Shiki computes the sun's position **locally** and blends a palette in **OKLCH** by **time of day** (sun elevation, morning vs evening) and **season**. It pushes imperceptible steps to every open session and keeps WCAG contrast. There's no network, account or telemetry; location is set manually or guessed from the timezone.

---

## Repository & workflow

- **Repo:** **`git@github.com:quiinthieu/shiki.git`**.
- **Bootstrap (first agent only):**
  ```sh
  git clone git@github.com:quiinthieu/shiki.git && cd shiki
  git checkout -b main
  # add PLAN.md, AGENTS.md (+ CLAUDE.md containing "@AGENTS.md"), .gitignore (target/, DerivedData/, .build/, .DS_Store)
  git add . && git commit -m "chore: add implementation plan and agent instructions"
  git push -u origin main
  ```
  Then make `main` the default branch and protect it (PR required, CI green).
- **Per task:** branch from the latest `main` as `task/TNN-short-slug` (e.g. `task/T03-solar`), one task per branch. Use Conventional Commits (`feat(core): …`, `fix(deliver): …`, `test:`, `chore:`, `docs:`). Open a PR titled `TNN: <task title>` with the acceptance criteria as a checklist. Squash-merge once CI passes, then delete the branch.
- **CI:** T01 adds `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, `cargo deny check` and `scripts/lint-privacy.sh` on `ubuntu-latest` and `macos-latest`. From T16 it also runs `xcodebuild test` for `macos/`. T18 adds `release.yml` on `v*` tags.
- **Never** force-push `main`, and never commit secrets, signing or notarization credentials (they go in Actions secrets), or anyone's real coordinates (fixtures use public city coordinates).

## 0. Instructions for agents (read first)

1. **One task at a time, in order:** the lowest-numbered unchecked task in §10. Split an oversized task here first (`T06a`, `T06b`).
2. **Tests first.** Every PR leaves `cargo test --workspace` green.
3. **`sk-core` is pure:** no I/O, no clock, no env. Time and location are arguments, so `--time` simulation, previews and tests are deterministic.
4. **Privacy is a hard rule.** No network, ever. `scripts/lint-privacy.sh` fails CI on `reqwest`, `hyper`, `ureq`, `std::net`, `URLSession` and `CoreLocation`. Coordinates live only in `config.toml`, never in logs, snapshots or errors.
5. **Contrast is a hard rule.** No frame leaves `sk-core` without passing the §4 contrast check.
6. **Only `sk-store` writes Shiki's files.** `sk-deliver` writes only escape sequences to the user's own ttys and opted-in terminal include files. rc and terminal configs are edited only with `--write-rc`/`--write-config`, as a marked block with a backup. Tests use a temp `HOME` (`TestEnv`).
7. **Stay inside the power budget (§7).** Every timer has a comment justifying its interval.
8. **Anything marked *(verify)* is an assumption.** Check the docs or a real terminal first, and record URL, version and date in `docs/terminals/<name>.md`.
9. **Never invent numbers.** Anchors and thresholds live in `data/*.toml`, labelled tunable. Docs cite only measured figures. Solar fixtures come from the NOAA calculator, with the capture date.
10. **No new dependencies** beyond §3 without an ADR (`docs/decisions/NNN-*.md`). When a task is done, tick it in §10 and update `CHANGELOG.md`.

## 1. Goals, non-goals, landscape

### 1.1 Goals (v1)
- **G1 Continuous drift** by sun elevation and season, in sub-threshold steps. Never a light/dark flip.
- **G2 Local and private:** sun computed on-device, no network. **G3 Always readable:** WCAG contrast at every moment (§4).
- **G4 Every session:** shell hook, opt-in tty broadcast, tmux, native kitty/WezTerm/Alacritty/Ghostty backends.
- **G5 Your palettes:** bundled, user TOML, or a two-theme gradient from any light + dark pair. **G6 Preview** any moment (`preview --time/--date`, `timelapse`).
- **G7 Cheap** (§7). **G8 Homebrew:** formula `quiinthieu/tap/shiki-term`, optional cask `quiinthieu/tap/shiki-term-app`.

### 1.2 Non-goals (v1)
Network geolocation or weather. Apps with their own truecolour themes (Neovim `termguicolors`, bat, delta) don't follow; `snapshot.json` is there for them. No native Windows (WSL is fine), no Linux tray, no wallpaper palettes.

### 1.3 Landscape (checked 2026-10-31)
- **[Ghostty light/dark](https://ghostty.org/docs/features/theme)** and the **[kitty themes kitten](https://sw.kovidgoyal.net/kitty/kittens/themes/)** (`*-theme.auto.conf`): follow the OS appearance. Discrete, one terminal each.
- **[darkman](https://darkman.whynothugo.nl/) (Linux), [Auto Dark Mode](https://github.com/AutoDarkMode/Windows-Auto-Night-Mode) (Windows), [dark-notify](https://github.com/cormacrelf/dark-notify) (macOS):** OS-level light/dark switching (darkman at sunrise/sundown) or watching. Binary.
- **[pywal](https://github.com/dylanaraps/pywal) (archived) / [wallust](https://codeberg.org/explosion-mental/wallust):** wallpaper palettes. pywal writes sequences to every `/dev/pts/*` ([sequences.py](https://github.com/dylanaraps/pywal/blob/master/pywal/sequences.py)); Shiki borrows that. No time axis.
- **[tinty](https://github.com/tinted-theming/tinty) ([base16/base24](https://github.com/tinted-theming/home)):** applies one chosen scheme across apps. Shiki imports such schemes as endpoints.
- **[alacritty-circadian](https://github.com/Dr-Dd/alacritty-circadian):** Alacritty theme switches at times or sun phases. Discrete, and no push since 2022-02.
- **[diurnal-terminal](https://github.com/cboone/diurnal-terminal):** Terminal.app day/night at sunrise/sunset via launchd + [heliocron](https://github.com/mfreeborn/heliocron). Binary.
- **[audamo](https://github.com/braun-steven/audamo):** GTK light/dark by sun for tiling WMs. No terminal palettes.
- **[Solar Theme (VS Code)](https://marketplace.visualstudio.com/items?itemName=debiday.solar-theme):** the closest concept, smooth SunCalc-driven interpolation. Editor-only, no season axis, auto-detects location *(verify how)*.

**Shiki's difference:** a continuous **2-D gradient (sun elevation × season)** in OKLCH, with **contrast guarantees**, sent to **every session across terminals**, **offline**.

### 1.4 Name clash check (Homebrew API, crates.io, npm, PyPI, GitHub; 2026-10-31)
- **`shiki`: taken (confirmed).** npm `shiki` is shikijs (v4.5.0), Homebrew core **formula `shiki`** installs a `shiki` binary, and crates.io `shiki` is an unrelated tokenizer. PyPI and the cask are free, but the binary would clash.
- **`shiki-cli`: rejected.** Taken on crates.io (sazardev/shiki), and `@shikijs/cli` **also installs a `shiki-cli` binary**.
- **`shikiterm`:** free everywhere. A fallback, but harder to read.
- **`shiki-term`: chosen.** Free on Homebrew (formula and cask), crates.io, npm and PyPI. There's no GitHub repo by that name, only look-alikes (`styleio/ShikishaTerm`, `Hell13Cat/TermShiki`). Cask **`shiki-term-app`** is free too. The README says we're not shikijs. Re-check before T18.

## 2. Glossary
**`d` / `s`:** day and season phase (§4). **Anchor:** a full palette (bg, fg, cursor, selection, ANSI 0–15) at one `(d, s)`. **Frame:** the blended palette for one moment. **Backend:** a delivery route (§5).

## 3. Architecture

**Stack: a Rust core, CLI and daemon, plus an optional thin Swift menu bar app (the same approach as Niwa).**
1. Rust gives one small static binary for macOS, Linux and WSL: fast in a prompt hook, cheap as a daemon, and the math tests cleanly as pure functions.
2. It reuses Niwa's scaffolding (layout, `product.rs`, privacy lint, CI/release, `quiinthieu/homebrew-tap`, the Swift pattern). The repos stay separate, with no shared crate in v1.
3. The menu bar is native SwiftUI `MenuBarExtra` with no colour logic. It reads `snapshot.json` and shells out to `shiki-term`, so there's no FFI.

| Unit | Responsibility |
|---|---|
| `crates/sk-core` | Pure: `solar` (NOAA), `color` (OKLab/OKLCH, gamut map, WCAG, ΔE_OK), `palette` (anchors, 2-D blend, contrast fix-up), `season`, `location` (tz → coordinates), `product.rs`. |
| `crates/sk-store` | Paths, `config.toml`, `state.json`, `snapshot.json`, `current.seq`, palette loading, lock, atomic writes. **The only writer of Shiki's own files.** |
| `crates/sk-deliver` | `Backend` trait: `osc`, `tty-broadcast`, `tmux`, `kitty`, `wezterm`, `alacritty`, `ghostty`; terminal detection. No colour logic. |
| `crates/sk-import` | base16/base24, Ghostty, kitty, Alacritty, WezTerm and iTerm2 themes → `Anchor`. |
| `crates/shiki-term` | The binary: clap CLI, daemon and scheduler, `hook`/`_emit`, `preview`/`timelapse`. |
| `macos/Shiki` | Swift 6 `MenuBarExtra` (macOS 14+ *(verify)*): sun-arc glyph, phase, preview scrubber, pause, palette picker. |

Every crate may depend on `sk-core`, and only `shiki-term` depends on the others. `sk-core` depends on nothing internal. `DependencyGraphTests` (T01) enforces this.

**Allowed dependencies:** `serde`, `serde_json`, `toml`, a YAML parser (ADR), `plist`, `clap`, `crossterm`, `directories`, `thiserror`, `anyhow` (binary only), `fs4`, `jiff` or `time` (ADR), `rustix` (ttys). Dev: `insta`, `proptest`, `assert_cmd`, `tempfile`, and `palette` **as a test oracle only**. The solar and OKLCH math is in-house; an ADR may adopt [`sunrise`](https://docs.rs/sunrise) or `spa`. Swift: no packages.

**Runtime flow:** `frame(instant, location, palette, config) → Frame` is pure. The **daemon** computes a frame, delivers it if the max ΔE_OK since the last delivery exceeds `step_threshold` (or a new session appeared), then sleeps until the predicted next step (§7). The **hook** prints `current.seq` only when its generation changed, so new or idle shells catch up at their next prompt. `reset` emits OSC 104/110/111/112.

## 4. Colour & sun model (all numbers tunable in `data/model.toml`)
- **Sun:** the [NOAA solar equations](https://gml.noaa.gov/grad/solcalc/calcdetails.html) (refraction-corrected elevation), with [SunCalc](https://github.com/mourner/suncalc)'s event angles (−0.833° sunrise; −6/−12/−18° twilights). Outputs: `elevation`, `is_morning`, `noon_elevation`, events. Polar day and night are valid states.
- **Location:** `timezone` (default) or `manual` (`locate --set 10.8,106.7`; one decimal is plenty). Timezone mode maps the IANA zone, via `backward` links (`Asia/Saigon` → `Asia/Ho_Chi_Minh`), to the coordinates in a vendored tzdb [`zone1970.tab`](https://data.iana.org/time-zones/tzdb/zone1970.tab). `doctor` warns that wide zones are coarse.
- **Day phase `d`:** elevation through keyframes `night` → astronomical, nautical, civil twilight → `golden` → `day`. Morning and evening are separate (dawn ≠ dusk), and elevation is normalised against `noon_elevation` so a low winter sun still reaches "day".
- **Season phase `s`:** a smooth cycle anchored at the solstices and equinoxes, with the hemisphere from the latitude sign. The amplitude fades toward the equator.
- **Blend:** bilinear over the four surrounding anchors in OKLCH ([Ottosson](https://bottosson.github.io/posts/oklab/)). L and C are linear; hue takes the shortest arc and is down-weighted as C → 0 so greys don't spin. Gamut mapping lowers C at constant L and h.
- **Contrast** (WCAG ratio, [1.4.3](https://www.w3.org/TR/WCAG22/#contrast-minimum)): fg/bg and selection ≥ **4.5:1** (AA normal text). ANSI 1–7 and 9–15 vs bg ≥ **3:1** (WCAG's large-text / [non-text](https://www.w3.org/TR/WCAG22/#non-text-contrast) level). ANSI 0/8 are exempt by default. Lowering a threshold needs `--i-know`. **Fix-up:** move a failing slot's L away from the bg by the smallest passing step (C, h kept), else nudge the bg. `palette check` reports the year's worst moment.
- **Palettes:** bundled `shiki` (cool night, warm dawn/dusk, seasonal tints), `shiki-dark` (temperature drift only) and `shiki-mono`. `palette from --day <theme> --night <theme>` turns any light/dark pair into a gradient.

## 5. Delivery
OSC 4 (palette), 10 (fg), 11 (bg), 12 (cursor) and the resets 104/110/111/112 are specified in [xterm ctlseqs](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html). Colours are encoded as `rgb:RR/GG/BB` and terminated with ST.

| Terminal | Mechanism | Status |
|---|---|---|
| iTerm2 | OSC via tty, or `OSC 1337;SetColors=` ([docs](https://iterm2.com/documentation-escape-codes.html)) | SetColors documented; OSC set *(verify)* |
| Terminal.app | OSC 11 set reportedly works, but OSC 111 reportedly doesn't restore | *(verify)*; AppleScript in v2 |
| Ghostty | OSC [4](https://ghostty.org/docs/vt/osc/4), [10–19](https://ghostty.org/docs/vt/osc/1x); optional `config-file` include + `reload_config` | external reload *(verify)* |
| kitty | `kitten @ set-colors --all` with a scoped `remote_control_password` ([docs](https://sw.kovidgoyal.net/kitty/remote-control/)); OSC fallback | documented |
| WezTerm | OSC ([docs](https://wezterm.org/escape-sequences.html)); optional Lua include + `add_to_config_reload_watch_list` ([docs](https://wezterm.org/config/files.html)) | documented |
| Alacritty | OSC 4/10–12/104/110–112 "Implemented" ([escapes(7)](https://github.com/alacritty/alacritty/blob/master/extra/man/alacritty-escapes.7.scd)); optional `import` + `live_config_reload` | import reload *(verify)* |
| Windows Terminal (WSL) | OSC 4 and chained 10–12 ([MS docs](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences), [#8999](https://github.com/microsoft/terminal/pull/8999)) | WSL tty path *(verify)* |
| tmux | OSC 4/10/11 into a pane are stored per pane ([CHANGES](https://github.com/tmux/tmux/blob/master/CHANGES)); the outer terminal needs `allow-passthrough` + `\ePtmux;…` ([tmux(1)](https://github.com/tmux/tmux/blob/master/tmux.1)) | pane vs outer *(verify)* |

**Modes (they combine):**
1. **Shell hook (MVP default):** `eval "$(shiki-term hook zsh|bash|fish)"`. Without a daemon, it runs `_emit --if-stale`, which recomputes at most once per `hook_min_interval`.
2. **Daemon + tty broadcast (opt-in):** one `write()` per frame to each tty owned by the uid (`/dev/pts/*`, `/dev/ttys*` *(verify)*). Interleaving with program output is a documented risk.
3. **Native backends (opt-in):** the kitty socket, and WezTerm/Alacritty/Ghostty include files (`backend enable <name> --write-config`).

`doctor` detects the terminal (`TERM_PROGRAM`, `KITTY_WINDOW_ID`, `WEZTERM_PANE`, `TMUX`, `WT_SESSION`, …) and says what to enable.

## 6. Files, settings & CLI
- **Paths** (`SHIKI_HOME` in tests): `~/Library/Application Support/Shiki/` (macOS), XDG config/state/runtime dirs (Linux).
- **Files:** `config.toml`, `state.json` (frame hash, generation, `paused_until`), `snapshot.json` (versioned, **no coordinates**), `current.seq`, `palettes/`, `shiki.lock`. Writes go temp → fsync → rename. An invalid palette never applies.
- **CLI:** `now`, `apply`, `reset`, `preview [--time --date --lat --lon --palette --json]`, `timelapse`, `pause [--for]`/`resume`, `palette list|check|import|from`, `locate`, `hook <shell> [install --write-rc|uninstall]`, `daemon install|uninstall|status`, `backend list|enable|disable`, `doctor`, `config get|set`.

## 7. Power & performance budget
These are **targets**, checked in T17, not measured claims.
- **Daemon:** no polling. It sleeps until the predicted next above-threshold step (short in twilight, long at night or midday), capped by `max_sleep`, at most one wake per minute, early on SIGHUP. It writes only on change.
- **Hook:** the fast path does no computation; target p95 under 5 ms added per prompt. `_emit --if-stale` p95 under 15 ms.
- **Menu bar:** idle apart from a `DispatchSource` watch on `snapshot.json`. Instruments shouldn't flag high energy impact.
- **`palette check`** year sweep: under 2 s in release on the CI runner.

## 8. Repo layout
`crates/{sk-core,sk-store,sk-deliver,sk-import,shiki-term}/` · `data/{model.toml,palettes/,tz/}` · `macos/` (XcodeGen, `Sources`, `Tests`, `Config/Product.xcconfig`) · `packaging/{homebrew,launchd,systemd}/` · `scripts/{lint-privacy.sh,bench.sh,update-tzdata.sh}` · `docs/{decisions,terminals,privacy.md,perf.md,palette-format.md,snapshot-schema.md}` · `tests/fixtures/{solar,themes,palettes}/`.

## 9. Test strategy
- **Unit:** solar vs NOAA fixtures (≥ 6 public cities: both hemispheres, one polar, one equatorial; solstices and equinoxes), OKLab vs the oracle, WCAG known pairs (black/white = 21:1), tz links.
- **Property:** every frame for any time, location and bundled palette passes §4. Frames a minute apart differ by a bounded ΔE. A blend at an anchor returns the anchor.
- **Golden:** OSC bytes, generated terminal includes, snapshot JSON, `preview`/`now` output, importer results.
- **Integration:** temp `SHIKI_HOME`, real zsh/bash/fish hooks on a pty, tmux panes, byte-exact installer round-trips. **Privacy:** lint + grep for the configured coordinates (zero hits). **macOS:** XCTest + UI smoke. **Manual:** `docs/terminals/matrix.md`, real runs only.

## 10. Milestones & tasks
Tick `[x]` when done. **M1 is the MVP**: new prompts drift via the hook. Everything after it is additive.

### M0 Skeleton
- [x] **T01 Workspace & tooling.** Stub crates, `product.rs`, toolchain, `deny.toml`, `ci.yml`, `lint-privacy.sh`, `DependencyGraphTests`.
  *AC:* CI green on macOS and Linux. `--version` uses `product.rs`. The lint catches a planted `std::net`.
- [ ] **T02 Test harness.** `TestEnv`, fixed-instant helpers, fixture loaders, insta, a pty helper.
  *AC:* a lint proves `sk-core` has no `std::fs`, `SystemTime::now` or `std::env`. Sample tests pass 100× in a loop.

### M1 MVP: drift in your shell
- [ ] **T03 Solar.** NOAA position, events, polar cases. *AC:* fixtures within the documented tolerance (capture URL and date in the header). Polar cases pass.
- [ ] **T04 Location.** Manual lat/lon (validated, rounded), vendored tzdb + `backward`, `locate`, ADR jiff vs time. *AC:* `Asia/Saigon` = `Asia/Ho_Chi_Minh`. An unknown zone suggests `locate --set`.
- [ ] **T05 Colour math.** OKLab/OKLCH, gamut map, WCAG, ΔE_OK. *AC:* oracle and round-trip tests pass. `contrast(#000,#fff) = 21`.
- [ ] **T06 Palette & frames.** Schema with file/line errors, `d`/`s`, OKLCH blend, fix-up, the `shiki` palette, `palette check`.
  *AC:* property tests pass over a year at 10-minute steps in both hemispheres. `palette check` prints the worst moment.
- [ ] **T07 OSC & CLI core.** Encoder, `apply`, `reset`, `now`, `preview` (swatches + sample text, 256-colour fallback, `NO_COLOR`), `config`, store.
  *AC:* golden OSC bytes. `preview` never emits OSC 4/10/11. `reset` emits 104/110/111/112.
- [ ] **T08 Shell hook (MVP release).** `hook zsh|bash|fish`, `_emit --if-stale`, generation file, `hook install --write-rc`/`uninstall`.
  *AC:* real shells emit on the first prompt and afterwards only on change. The rc round-trips byte for byte. Latency measured. `v0.1.0` builds.

### M2 Every session, cheaply
- [ ] **T09 Daemon & scheduler.** Predicted-step sleep, `pause`/`resume`, launchd/systemd user units *(verify paths)*.
  *AC:* a fake-clock day's wakes respect §7. No writes when unchanged. Install/uninstall round-trips.
- [ ] **T10 tty broadcast & tmux.** Enumerate the uid's ttys, single-write delivery, tmux passthrough option.
  *AC:* a pty pair gets the exact bytes. Other users' ttys are never opened. A tmux pane's colour changes.
- [ ] **T11 Native backends.** kitty socket + scoped password; WezTerm, Alacritty and Ghostty includes; `backend enable --write-config`.
  *AC:* golden files. Install/uninstall round-trips. A matrix row per terminal.
- [ ] **T12 Doctor.** Detection, capability report, warnings (tz distance, Terminal.app reset, passthrough off, kitty RC off). *AC:* goldens for env fixtures of every §5 terminal, alone and inside tmux.

### M3 Palettes & surfaces
- [ ] **T13 Importers & two-theme gradient.** All `sk-import` formats, `palette import`, `palette from`. *AC:* goldens from licence-compatible fixtures. A two-theme palette passes the year sweep.
- [ ] **T14 More palettes & seasons.** `shiki-dark`, `shiki-mono`, equator fade, dawn ≠ dusk, high latitudes. *AC:* year sweeps pass for every palette at every fixture city.
- [ ] **T15 Timelapse.** `timelapse --date --speed` (`--year` at noon) in preview mode. *AC:* the terminal is restored after exit and after SIGINT (pty test). The fps cap is commented.
- [ ] **T16 Swift menu bar app.** `MenuBarExtra`: glyph, phase, next event, scrubber (`preview --json`), pause, palette picker, login item (`SMAppService` *(verify)*), missing-CLI state.
  *AC:* XCTests + UI smoke pass. Writes go only through `shiki-term config set`. Instruments result in `docs/perf.md`.

### M4 Polish & release
- [ ] **T17 Perf, defaults & accessibility.** `bench.sh` for §7; `step_threshold` chosen by eye with a rationale; colour-vision simulation previews.
  *AC:* every §7 target measured in `docs/perf.md` (or an issue filed). Bundled ANSI red and green differ in L, not just hue.
- [ ] **T18 Packaging & Homebrew.** `release.yml` (macOS universal, Linux x86_64/arm64, checksums; signed and notarised `Shiki.app` DMG). Tap `quiinthieu/homebrew-tap`: formula `shiki-term` (`service do` *(verify)*), cask `shiki-term-app` (`depends_on formula:` *(verify)*). Re-run §1.4 first.
  *AC:* `brew install --build-from-source`, `brew audit --strict` and `brew test` pass in CI. A tag dry run builds every artifact. `spctl -a` accepts the app.
- [ ] **T19 Docs & v1.0.** README (real screenshots/GIFs, no invented numbers, the "not shikijs" note), privacy doc, terminal guides, uninstall guide, CHANGELOG, `v1.0.0`.
  *AC:* on fresh macOS and Ubuntu VMs, install → uninstall leaves no residue (rc and terminal configs byte-identical).

## 11. Risks & mitigations
*Unreadable moments:* fix-up + year sweeps. *Distracting changes:* sub-threshold steps, `pause`. *Broadcast interleaving:* opt-in, single writes, native backends first. *Terminal quirks:* *(verify)* docs, `doctor`. *Coarse tz:* `locate --set`. *shikijs confusion:* the `shiki-term` name.

## 12. v2 backlog (don't implement in v1)
Neovim/Helix/bat/delta integrations; `on_change` hooks; a Terminal.app AppleScript backend; native Windows; a Linux tray; moon phase; opt-in CoreLocation/geoclue; opt-in weather tint (network ADR).
