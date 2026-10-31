# Verified `tmux` Per-Pane OSC Palette vs Outer DCS Passthrough

- **Verified:** 2026-10-31
- **Task:** T10 tty broadcast & tmux (`PLAN.md` §0 #8, §5, §10)

This note records the verified distinction between `tmux`'s per-pane OSC palette handling and outer-terminal DCS passthrough (`\x1bPtmux;…\x1b\\`), implemented in `crates/sk-deliver/src/tmux.rs`.

---

## 1. Sources Checked (2026-10-31)

- `tmux` manual page `tmux(1)` (`allow-passthrough` window/session option, `terminal-features` `RGB`, `-T` client feature flag, `list-panes -a -F "#{pane_tty}"`):
  <https://github.com/tmux/tmux/blob/master/tmux.1>
- `tmux` `CHANGES` (3.2 `allow-passthrough` introduction; 3.3 per-pane `OSC 4`, `10`, `11`, `12`, `104`, `110`, `111`, `112` storage; 3.4 `allow-passthrough` default `off` and `all` mode):
  <https://github.com/tmux/tmux/blob/master/CHANGES>
- `tmux` `input.c` (`input_osc_4`, `input_osc_10`, `input_osc_11`, `input_osc_12`, `input_osc_104`, `input_osc_110`, `input_osc_111`, `input_osc_112`, and `input_dcs_dispatch` `"tmux;"` handler):
  <https://github.com/tmux/tmux/blob/master/input.c>

---

## 2. Per-Pane OSC Storage vs Outer Terminal Passthrough

| Mechanism | Sequences written to `#{pane_tty}` | Requires `allow-passthrough`? | What updates |
|---|---|---|---|
| **Raw OSC** (`passthrough_outer = false`) | `OSC 11`, `OSC 10`, `OSC 12`, `OSC 4;0..15` (`383` bytes) or `OSC 104/110/111/112` (`28` bytes), terminated with 7-bit `ST` (`\x1b\\`) | **No** (works with `tmux` 3.3+ default `allow-passthrough off`) | Updates `tmux`'s internal per-pane palette (`wp->palette`: pane background, foreground, cursor colour, and ANSI 0–15). `tmux` redraws the pane to attached clients using 24-bit RGB SGR (`38;2;R;G;B` / `48;2;R;G;B`) when the client supports truecolour (`-T RGB` / `terminal-features` `RGB` / `Tc`). Does **not** alter the outer terminal emulator's window margin/padding or non-tmux tabs. |
| **Combined Raw OSC + DCS Passthrough** (`passthrough_outer = true`) | `[raw_osc, \x1bPtmux;<raw_osc_with_doubled_\x1b>\x1b\\]` concatenated in **one** buffer (`813` bytes frame / `73` bytes reset) | `set -g allow-passthrough on` (visible panes, `tmux` 3.2+) or `set -g allow-passthrough all` (all panes, `tmux` 3.4+) for outer delivery | Updates **both** `tmux`'s per-pane `wp->palette` (via the leading raw OSC segment) **and** forwards the unwrapped OSC sequences through `tmux` to the outer terminal emulator (updating window padding/border background and outer cursor/palette). When `allow-passthrough` is `off`, `tmux`'s DCS state machine silently discards the `tmux;` DCS block while still applying the leading raw OSC segment to the pane. |

---

## 3. Exact DCS Passthrough Framing Rules (`tmux(1)`)

1. Outer envelope prefix: `ESC P tmux;` (`\x1bPtmux;`, 7 bytes).
2. Wrapped payload: every `ESC` byte (`0x1b`) inside the inner sequence—including both the opening `\x1b]` of each OSC command and the `\x1b` of each 7-bit `ST` (`\x1b\\`)—**must be doubled** to `\x1b\x1b`.
3. Outer envelope terminator: single `7-bit ST` (`\x1b\\`, 2 bytes, **not** doubled).
4. Total payload size for a full 19-sequence frame (`bg`, `fg`, `cursor`, `ANSI 0–15`):
   - Raw OSC segment: `383` bytes (`38` `ESC` bytes).
   - DCS passthrough segment: `7 + (383 + 38) + 2 = 430` bytes.
   - Combined single-write buffer: `813` bytes (well inside `tmux`'s `INPUT_LIMIT` and kernel PTY write buffers).

---

## 4. Pane Enumeration & Security

- Active pane ttys are queried with `tmux list-panes -a -F "#{pane_tty}"` (with optional `-S <socket_path>` or `-L <socket_name>` overrides for isolated testing and custom sockets).
- If `tmux` is not installed (`ENOENT`) or no server is running (non-zero exit with `"no server running"` / socket `ENOENT` / `ECONNREFUSED`), `TmuxBackend` treats the active pane list as empty (`0` panes) without error.
- Every `#{pane_tty}` returned by `tmux` is passed through `sk_deliver::tty`'s UID-verified delivery pipeline (`lstat` `st_uid == geteuid()` & `is_char_device` before `open`, `O_WRONLY | O_NOCTTY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW`, post-open `fstat` + `isatty`, single `write(2)`).
