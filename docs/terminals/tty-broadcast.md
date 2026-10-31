# Verified TTY Broadcast Paths & Safety Contract (`/dev/pts/*`, `/dev/ttys*`, WSL)

- **Verified:** 2026-10-31
- **Task:** T10 tty broadcast & tmux (`PLAN.md` §0 #8, §5, §10)

This note records the verified pseudo-terminal device paths across Linux, WSL 2, and macOS Darwin, the UID isolation and TOCTOU safety checks enforced before and after `open(2)`, and the single-write delivery contract implemented in `crates/sk-deliver/src/tty.rs`.

---

## 1. Platform Pseudo-Terminal Replica Paths

### Sources checked (2026-10-31)
- Linux man-pages `pts(4)` & `ptmx(4)` (Linux man-pages 6.9+):
  <https://man7.org/linux/man-pages/man4/pts.4.html>
- Linux man-pages `console_codes(4)` (Linux virtual console escape sequences):
  <https://man7.org/linux/man-pages/man4/console_codes.4.html>
- Local macOS manual pages `pty(4)` (`BSD 4.2`, `November 30, 1993`) and `grantpt(3)` / `ptsname(3)` (`macOS`, `June 17, 2025`), plus live `posix_openpt(O_RDWR | O_NOCTTY)` + `ptsname(3)` inspection on macOS Darwin.
- Microsoft Learn, *Console Virtual Terminal Sequences* ("Screen Colors" — `OSC 4` and `OSC 10 / 11 / 12`):
  <https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences>
- Windows Terminal PR `#8999` (*Implement support for OSC 10, 11, 12*):
  <https://github.com/microsoft/terminal/pull/8999>

### Verified device layouts

| Platform | Scan root | Accepted replica filenames | Excluded entries & rationale |
|---|---|---|---|
| Linux | `/dev/pts` (`devpts`) | `<digits>` (`0`, `1`, `42`, …) | `/dev/pts/ptmx` (multiplexor, `0666` / `0000`); `/dev/tty0`–`/dev/tty63` Linux virtual consoles (`console_codes(4)` uses `ESC ] P nrrggbb`, not xterm `OSC 4/10–12`). |
| WSL 2 (Windows Terminal) | `/dev/pts` (`devpts`) | `<digits>` (`0`, `1`, …) | WSL 2 runs a Linux kernel with `devpts` mounted at `/dev/pts`. Each WSL session allocates `/dev/pts/<N>` owned by the Linux effective UID (`crw--w---- <uid>:tty`), bridged via `wslrelay.exe` / ConPTY to Windows Terminal, which parses `OSC 4` and individual or chained `OSC 10/11/12` (`#8999`). |
| macOS (Darwin) | `/dev` (`devfs`) | `ttys<digits>` (`ttys000`, `ttys001`, …) | `/dev/tty` (controlling-terminal alias, `crw-rw-rw- root:wheel`), `/dev/console`, `/dev/ptmx`, `/dev/tty.<name>` and `/dev/cu.<name>` (serial / Bluetooth modems), and `/dev/ttys` without trailing digits. |

---

## 2. Hard UID Isolation & TOCTOU Verification

 Acceptance criterion (`PLAN.md` §10 T10): **"Other users' ttys are never opened."**

On multi-user Unix systems and standard single-user desktops alike, `/dev` and `/dev/pts` contain character devices owned by `root` (`uid 0`, including world-writable `0666` devices such as `/dev/tty` and `/dev/null`) or other logged-in UIDs. Relying on `open(2)` permission errors (`EACCES`) is **insufficient** because a `0666` or `0620` device (via group `tty`) could still be opened if not checked first.

`sk_deliver::tty` enforces a four-step pipeline on every candidate path:

1. **Pre-open `lstat(2)` (no symlink traversal, no `open`):**
   - Calls `rustix::fs::lstat(path)`.
   - Requires `!FileType::is_symlink()`, `FileType::is_char_device()`, and `stat.st_uid == expected_uid.as_raw()` (default `geteuid()`).
   - Any path owned by another UID, or any symlink, FIFO, socket, directory, or regular file, is rejected **before `open(2)` is ever called**.
2. **Non-controlling, non-blocking, symlink-refusing `open(2)`:**
   - Calls `rustix::fs::open(path, OFlags::WRONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOFOLLOW, Mode::empty())`.
   - `O_WRONLY`: never reads terminal input.
   - `O_NOCTTY`: never acquires a controlling terminal for the daemon process.
   - `O_NONBLOCK`: never hangs if a session paused output (`^S` / `XOFF` / `TIOCSTOP`).
   - `O_CLOEXEC`: prevents descriptor inheritance across child processes (`tmux list-panes`).
   - `O_NOFOLLOW`: kernel-enforced protection if a path were replaced with a symlink between `lstat` and `open`.
3. **Post-open `fstat(2)` + `isatty(3)` TOCTOU check:**
   - Verifies the opened `OwnedFd` still satisfies `is_char_device`, `st_uid == expected_uid`, `(st_dev, st_ino, st_rdev) == pre_open_identity`, and `rustix::termios::isatty(&fd)`.
4. **Single `write(2)` syscall per frame:**
   - Emits the entire pre-encoded buffer (`383` bytes for an OSC frame, `28` bytes for an OSC reset, `813` bytes for a combined tmux + DCS passthrough frame) in **one** `rustix::io::write(&fd, payload)` call (retrying only `EINTR` prior to any bytes written).
   - Transient `ENOENT`, `ENXIO`, `EIO`, `EAGAIN`, `EPIPE`, or `EACCES` errors (for example, a terminal tab closing mid-broadcast or backpressured by `XOFF`) are recorded as skipped targets rather than aborting delivery to remaining sessions.

---

## 3. Output Interleaving Trade-Off (`PLAN.md` §5 & §11)

- Writing directly to a live replica tty (`tty-broadcast`) is **opt-in** (`delivery.tty_broadcast = false` by default).
- Although issuing one single `write(2)` per frame keeps all 19 OSC sequences contiguous in one kernel write buffer, a foreground program simultaneously writing multi-byte UTF-8 or CSI/OSC escape sequences to the same pty can still interleave at kernel buffer boundaries.
- Users running terminals with native out-of-band reload mechanisms (kitty remote socket, WezTerm/Alacritty/Ghostty config includes in T11) or the prompt-boundary shell hook (T08) avoid mid-output interleaving completely.
