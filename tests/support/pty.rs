//! Safe Unix pseudoterminal (`pty`) test harness built on `rustix`.
//!
//! Workspace crates forbid `unsafe` (`unsafe_code = "forbid"`), so PTY allocation, `termios`
//! configuration, window sizing, and non-blocking `poll` deadlines use `rustix`'s safe wrappers.

#![cfg(unix)]
#![allow(dead_code)]

use rustix::event::{poll, Nsecs, PollFd, PollFlags, Secs, Timespec};
use rustix::fd::OwnedFd;
use rustix::fs::{fcntl_getfl, fcntl_setfl, open, Mode, OFlags};
use rustix::io::{fcntl_setfd, Errno, FdFlags};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcgetattr, tcsetattr, tcsetwinsize, OptionalActions, Winsize};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn duration_to_timespec(dur: Duration) -> Timespec {
    let subsec_ns = i32::try_from(dur.subsec_nanos()).unwrap_or(0);
    Timespec {
        tv_sec: Secs::try_from(dur.as_secs()).unwrap_or(Secs::from(i32::MAX)),
        tv_nsec: Nsecs::from(subsec_ns),
    }
}

fn write_all_fd(fd: &OwnedFd, mut buf: &[u8], timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while !buf.is_empty() {
        match rustix::io::write(fd, buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "PTY write returned 0 bytes",
                ));
            }
            Ok(written) => {
                buf = &buf[written..];
            }
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for PTY fd to become writable",
                    ));
                }
                let ts = duration_to_timespec(remaining);
                let mut fds = [PollFd::new(fd, PollFlags::OUT)];
                match poll(&mut fds, Some(&ts)) {
                    Ok(_) | Err(Errno::INTR) => continue,
                    Err(err) => return Err(io::Error::from(err)),
                }
            }
            Err(err) => return Err(io::Error::from(err)),
        }
    }
    Ok(())
}

pub struct PtyPair {
    controller: OwnedFd,
    replica: Option<OwnedFd>,
    replica_path: PathBuf,
    rx_buf: RefCell<Vec<u8>>,
}

impl PtyPair {
    /// Opens an `80x24` PTY pair in raw mode (no echo, no CRLF translation) for byte-exact OSC tests.
    pub fn open_raw() -> io::Result<Self> {
        Self::open_with_options(true, DEFAULT_ROWS, DEFAULT_COLS)
    }

    /// Opens an `80x24` PTY pair in default cooked `termios` mode for interactive shell tests.
    pub fn open_cooked() -> io::Result<Self> {
        Self::open_with_options(false, DEFAULT_ROWS, DEFAULT_COLS)
    }

    pub fn open_with_options(raw_mode: bool, rows: u16, cols: u16) -> io::Result<Self> {
        let controller =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(io::Error::from)?;
        fcntl_setfd(&controller, FdFlags::CLOEXEC).map_err(io::Error::from)?;
        grantpt(&controller).map_err(io::Error::from)?;
        unlockpt(&controller).map_err(io::Error::from)?;

        let c_name = ptsname(&controller, Vec::new()).map_err(io::Error::from)?;
        let replica_path = PathBuf::from(OsStr::from_bytes(c_name.as_bytes()));
        let replica = open(
            &c_name,
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;

        tcsetwinsize(
            &replica,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .map_err(io::Error::from)?;

        if raw_mode {
            let mut termios = tcgetattr(&replica).map_err(io::Error::from)?;
            termios.make_raw();
            tcsetattr(&replica, OptionalActions::Now, &termios).map_err(io::Error::from)?;
        }

        let status_flags = fcntl_getfl(&controller).map_err(io::Error::from)?;
        fcntl_setfl(&controller, status_flags | OFlags::NONBLOCK).map_err(io::Error::from)?;

        Ok(Self {
            controller,
            replica: Some(replica),
            replica_path,
            rx_buf: RefCell::new(Vec::new()),
        })
    }

    #[must_use]
    pub fn replica_path(&self) -> &Path {
        &self.replica_path
    }

    /// Closes the parent's handle to the replica side so controller reads see EOF once any child exits.
    pub fn close_replica(&mut self) {
        self.replica = None;
    }

    /// Writes bytes directly to the replica tty (simulating terminal escape output).
    pub fn write_to_replica(&self, bytes: &[u8]) -> io::Result<()> {
        let replica = self.replica.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PTY replica has already been closed in parent",
            )
        })?;
        write_all_fd(replica, bytes, WRITE_TIMEOUT)
    }

    /// Writes bytes to the controller side (simulating user input sent to a process on the replica).
    pub fn write_input(&self, bytes: &[u8]) -> io::Result<()> {
        write_all_fd(&self.controller, bytes, WRITE_TIMEOUT)
    }

    pub fn read_exact_with_timeout(&self, len: usize, timeout: Duration) -> io::Result<Vec<u8>> {
        let mut out = std::mem::take(&mut *self.rx_buf.borrow_mut());
        if out.len() < len {
            out.reserve(len - out.len());
        }
        self.read_loop(timeout, &mut out, |buf| buf.len() >= len)?;
        if out.len() < len {
            let captured = out.len();
            *self.rx_buf.borrow_mut() = out;
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("PTY reached EOF after {captured} of {len} expected bytes"),
            ));
        }
        let remainder = out.split_off(len);
        *self.rx_buf.borrow_mut() = remainder;
        Ok(out)
    }

    pub fn read_until(&self, needle: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
        assert!(!needle.is_empty(), "read_until needle must not be empty");
        let mut out = std::mem::take(&mut *self.rx_buf.borrow_mut());
        self.read_loop(timeout, &mut out, |buf| {
            buf.windows(needle.len()).any(|window| window == needle)
        })?;
        let Some(pos) = out
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "PTY reached EOF before needle appeared (captured {} bytes: {:?})",
                    out.len(),
                    String::from_utf8_lossy(&out)
                ),
            ));
        };
        let split_at = pos + needle.len();
        let remainder = out.split_off(split_at);
        *self.rx_buf.borrow_mut() = remainder;
        Ok(out)
    }

    pub fn read_to_eof(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        self.close_replica();
        let mut out = std::mem::take(self.rx_buf.get_mut());
        self.read_loop(timeout, &mut out, |_| false)?;
        Ok(out)
    }

    fn read_loop<F>(&self, timeout: Duration, out: &mut Vec<u8>, mut stop: F) -> io::Result<()>
    where
        F: FnMut(&[u8]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 4096];

        loop {
            if stop(out) {
                return Ok(());
            }

            // Always drain buffered bytes before checking poll/HUP: fast-exiting children on macOS
            // surface `POLLIN | POLLHUP` simultaneously, and Linux signals post-drain EOF via `EIO`.
            loop {
                match rustix::io::read(&self.controller, &mut chunk) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        out.extend_from_slice(&chunk[..n]);
                        if stop(out) {
                            return Ok(());
                        }
                    }
                    Err(Errno::INTR) => continue,
                    Err(Errno::IO) => return Ok(()),
                    Err(Errno::AGAIN) => break,
                    Err(err) => return Err(io::Error::from(err)),
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out reading from PTY after {:?} (captured {} bytes)",
                        timeout,
                        out.len()
                    ),
                ));
            }

            let ts = duration_to_timespec(remaining);
            let mut fds = [PollFd::new(&self.controller, PollFlags::IN)];
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "poll timed out reading from PTY after {:?} (captured {} bytes)",
                            timeout,
                            out.len()
                        ),
                    ));
                }
                Ok(_) => {
                    let revents = fds[0].revents();
                    if revents.contains(PollFlags::HUP) && !revents.contains(PollFlags::IN) {
                        match rustix::io::read(&self.controller, &mut chunk) {
                            Ok(0) | Err(Errno::AGAIN | Errno::IO) => return Ok(()),
                            Ok(n) => out.extend_from_slice(&chunk[..n]),
                            Err(Errno::INTR) => continue,
                            Err(err) => return Err(io::Error::from(err)),
                        }
                    }
                }
                Err(Errno::INTR) => continue,
                Err(err) => return Err(io::Error::from(err)),
            }
        }
    }

    /// Spawns `cmd` with `stdin`, `stdout`, and `stderr` attached to the PTY replica, and closes
    /// the parent's copy of the replica descriptor so `read_to_eof` terminates when `cmd` exits.
    pub fn spawn_command(&mut self, mut cmd: Command) -> io::Result<PtyChild> {
        let replica = self.replica.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY replica already taken or closed",
            )
        })?;

        let stdin_fd = replica.try_clone()?;
        let stdout_fd = replica.try_clone()?;
        let stderr_fd = replica;

        cmd.stdin(Stdio::from(stdin_fd))
            .stdout(Stdio::from(stdout_fd))
            .stderr(Stdio::from(stderr_fd));

        let child = cmd.spawn()?;
        Ok(PtyChild { child: Some(child) })
    }

    /// Spawns `cmd` as the leader of a new POSIX session (`setsid`) with the PTY replica attached
    /// as its controlling terminal (`TIOCSCTTY`), required by interactive shells like `fish` whose
    /// job-control startup checks `tcgetpgrp(STDIN_FILENO)`.
    ///
    /// Uses `setsid -w -c` on Linux (`util-linux`) or `python3` (`os.setsid` + `TIOCSCTTY` +
    /// `os.execv`) on macOS so workspace crates stay 100% free of `unsafe` (`pre_exec`).
    pub fn spawn_session_command(&mut self, cmd: Command) -> io::Result<PtyChild> {
        let setsid_candidate = ["/usr/bin/setsid", "/bin/setsid"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file());

        let mut wrapped = if let Some(setsid_bin) = setsid_candidate {
            let mut c = Command::new(setsid_bin);
            c.args(["-w", "-c"]).arg(cmd.get_program());
            c.args(cmd.get_args());
            c
        } else {
            let py_bin = ["/usr/bin/python3", "/opt/homebrew/bin/python3"]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from("python3"));
            let mut c = Command::new(py_bin);
            c.args([
                "-c",
                "import fcntl, os, sys, termios; os.setsid(); fcntl.ioctl(0, termios.TIOCSCTTY, 0); os.execv(sys.argv[1], sys.argv[1:])",
            ])
            .arg(cmd.get_program());
            c.args(cmd.get_args());
            c
        };

        if cmd.get_envs().next().is_some() {
            wrapped.env_clear();
        }
        for (key, val) in cmd.get_envs() {
            match val {
                Some(v) => {
                    wrapped.env(key, v);
                }
                None => {
                    wrapped.env_remove(key);
                }
            }
        }
        if let Some(dir) = cmd.get_current_dir() {
            wrapped.current_dir(dir);
        }

        self.spawn_command(wrapped)
    }
}

pub struct PtyChild {
    child: Option<Child>,
}

impl PtyChild {
    pub fn wait_with_timeout(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "child already awaited"))?;

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                self.child = None;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting {:?} for PTY child process", timeout),
                ));
            }
            // 5 ms test-only reap backoff: runs only if the child hasn't exited immediately after
            // closing its PTY stdio descriptors, avoiding busy-wait CPU spin during test teardown.
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
