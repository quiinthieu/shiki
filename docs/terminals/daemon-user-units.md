# Verified Per-User Daemon Service Units (`launchd` & `systemd --user`)

- **Verified:** 2026-10-31
- **Task:** T09 Daemon & scheduler (`PLAN.md` §0 #8, §6, §7, §8, §10)

This note records the verified per-user service unit paths, lifecycle commands, and unit keys used by `shiki-term daemon install|uninstall|status` and the reference templates under `packaging/launchd/` and `packaging/systemd/`.

---

## 1. macOS — `launchd` User LaunchAgent

### Sources checked (2026-10-31)
- Local macOS manual page `launchd.plist(5)` (Darwin BSD System Manager's Manual, dated `November 22, 2021`).
- Local macOS manual page `launchctl(1)` (Darwin BSD General Commands Manual, `bootstrap`, `bootout`, `kickstart` subcommands targeting `gui/<uid>`).
- Apple Developer Archive, *Creating Launch Daemons and Agents*:
  <https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html>

### Verified path & naming
- `launchd.plist(5)` ("FILES") defines:
  - `~/Library/LaunchAgents` — *"Per-user agents provided by the user."*
- Apple convention names the property list `<Label>.plist`, where `Label` uniquely identifies the job in the user's `gui/<uid>` domain and matches the reverse-DNS bundle identifier (`product::BUNDLE_ID` = `io.github.quiinthieu.shiki-term`):
  - **Installed path:** `$HOME/Library/LaunchAgents/io.github.quiinthieu.shiki-term.plist`
  - **Reference template:** `packaging/launchd/io.github.quiinthieu.shiki-term.plist`

### Verified `launchd.plist(5)` keys & daemon contract
- **Foreground execution:** `launchd.plist(5)` explicitly forbids `daemon(3)` / `fork()`-and-exit; `ProgramArguments` invokes `[<exe>, "daemon", "run"]` in the foreground and exits `0` on `SIGTERM`.
- **`Label`:** `io.github.quiinthieu.shiki-term`.
- **`AssociatedBundleIdentifiers`:** `["io.github.quiinthieu.shiki-term"]` so macOS System Settings -> General -> Login Items associates the LaunchAgent cleanly with `Shiki.app` (T16).
- **`RunAtLoad`:** `true` so the agent starts immediately upon `launchctl bootstrap` and at user login.
- **`KeepAlive`:** `{ "SuccessfulExit": false }` so `launchd` restarts the daemon if it exits non-zero or crashes, while respecting a clean exit (`0`) on `SIGTERM`.
- **`ProcessType`:** `Background` so macOS schedules the sleeping/waking daemon at background QoS (`PLAN.md` §7 power budget).
- **Validation:** Verified with `/usr/bin/plutil -lint` on macOS (enforced automatically in `crates/shiki-term/tests/daemon_cli.rs`).

### Lifecycle commands
- **Install / reload:**
  1. `launchctl bootout gui/<uid>/io.github.quiinthieu.shiki-term` (best-effort unload if an older definition is already loaded)
  2. `launchctl bootstrap gui/<uid> $HOME/Library/LaunchAgents/io.github.quiinthieu.shiki-term.plist`
  3. `launchctl kickstart -k gui/<uid>/io.github.quiinthieu.shiki-term`
- **Uninstall:**
  1. `launchctl bootout gui/<uid>/io.github.quiinthieu.shiki-term`

---

## 2. Linux & WSL — `systemd --user` Service Unit

### Sources checked (2026-10-31)
- `systemd.unit(5)` ("Unit File Load Path" -> "User Unit Search Path", systemd 256+):
  <https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html>
- `systemd.service(5)` ("Service Templates", `Type=`, `ExecStart=`, `ExecReload=`, `Restart=`):
  <https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html>

### Verified path & naming
- `systemd.unit(5)` Table 2 ("Load path when running in user mode (`--user`)") places persistent user-owned configuration highest in precedence at:
  - `$XDG_CONFIG_HOME/systemd/user` (or `$HOME/.config/systemd/user` when `XDG_CONFIG_HOME` is unset or empty).
- Naming the service after the CLI binary (`product::CLI_BIN` = `shiki-term`):
  - **Installed path:** `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/shiki-term.service`
  - **Reference template:** `packaging/systemd/shiki-term.service`

### Verified `systemd.service(5)` keys & daemon contract
- **`Type=simple`:** `shiki-term daemon run` runs in the foreground without forking.
- **`ExecStart=<escaped-exe> daemon run`:** `%` is escaped as `%%` (`systemd.unit(5)` specifier rules); executable paths containing whitespace, `"`, `\`, or `$` are C-quoted and escaped per `systemd.service(5)` command-line parsing rules.
- **`ExecReload=/bin/kill -HUP $MAINPID`:** Integrates `systemctl --user reload shiki-term.service` directly with the daemon's `SIGHUP` early-wakeup pipe (`PLAN.md` §7).
- **`Restart=on-failure` & `RestartSec=5`:** Restarts after unexpected non-zero exits or signals without tight respawn loops, while allowing clean `0` shutdown on `SIGTERM`.
- **`[Install] WantedBy=default.target`:** Standard target started by the per-user `systemd --user` manager on login.

### Lifecycle commands
- **Install / reload:**
  1. `systemctl --user daemon-reload`
  2. `systemctl --user enable --now shiki-term.service`
- **Uninstall:**
  1. `systemctl --user disable --now shiki-term.service`
  2. `systemctl --user daemon-reload`
