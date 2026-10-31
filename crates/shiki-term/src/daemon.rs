//! Foreground predicted-step daemon scheduler, `SIGHUP`/`SIGTERM`/`SIGINT` self-pipe wakeup, and
//! per-user `launchd` / `systemd --user` unit management (`PLAN.md` §6, §7, §8, §10 T09).

use clap::ValueEnum;
use rustix::event::{poll, Nsecs, PollFd, PollFlags, Secs, Timespec};
use rustix::fd::OwnedFd;
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::{fcntl_setfd, Errno, FdFlags};
use rustix::pipe::pipe;
use rustix::process::{getuid, kill_process, Pid, Signal};
use serde::Serialize;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use signal_hook::low_level::{pipe as sig_pipe, unregister};
use signal_hook::SigId;
use sk_core::palette::Frame;
use sk_core::product;
use sk_core::time::{Clock, UtcInstant};
use sk_deliver::osc::encode_osc_frame;
use sk_store::clock::SystemClock;
use sk_store::config::{load_config, DEFAULT_MAX_SLEEP_SECS};
use sk_store::paths::{PlatformKind, ShikiPaths};
use sk_store::store::{
    daemon_tick, load_state, probe_running_daemon_pid, DaemonPidGuard, DaemonTickOutcome,
    PauseStatus, INDEFINITE_PAUSE,
};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Environment variable that disables live `launchctl` / `systemctl --user` calls during automated
/// test runs so tests never touch the developer's or CI runner's host service manager.
pub const NO_SERVICE_MANAGER_ENV: &str = "SHIKI_NO_SERVICE_MANAGER";

const MANAGED_TAG_PREFIX: &str = "shiki-managed: created-dirs=";
static UNIT_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Target OS family for `daemon install|uninstall|status` unit resolution.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum DaemonPlatformArg {
    Macos,
    Linux,
}

impl DaemonPlatformArg {
    #[must_use]
    pub const fn into_platform_kind(self) -> PlatformKind {
        match self {
            Self::Macos => PlatformKind::MacOs,
            Self::Linux => PlatformKind::Linux,
        }
    }
}

/// Reason a sleeping daemon iteration woke from [`DaemonSleeper::sleep`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// The predicted-step timeout elapsed normally.
    Timer,
    /// `SIGHUP` arrived (`pause`, `resume`, `config set`, `locate`, or `systemctl reload`).
    Reload,
    /// `SIGTERM` or `SIGINT` arrived; exit cleanly with status `0`.
    Shutdown,
}

/// Blocking sleep abstraction over the predicted-step scheduler interval (`PLAN.md` §7).
pub trait DaemonSleeper {
    fn sleep(&mut self, sleep_secs: u64) -> io::Result<WakeReason>;
}

/// Zero-polling POSIX self-pipe sleeper woken by `poll(2)` deadline or `SIGHUP`/`SIGTERM`/`SIGINT`.
pub struct SignalWakePipe {
    read_fd: OwnedFd,
    hup_flag: Arc<AtomicBool>,
    term_flag: Arc<AtomicBool>,
    sig_ids: Vec<SigId>,
}

impl SignalWakePipe {
    pub fn install() -> io::Result<Self> {
        let (read_fd, write_fd) = pipe().map_err(io::Error::from)?;
        configure_nonblocking_cloexec(&read_fd)?;
        configure_nonblocking_cloexec(&write_fd)?;

        let hup_flag = Arc::new(AtomicBool::new(false));
        let term_flag = Arc::new(AtomicBool::new(false));
        let mut sig_ids = Vec::with_capacity(6);

        let register_res = (|| -> io::Result<()> {
            // Register atomic flags BEFORE pipe writers (`signal-hook` ordering contract): on wake,
            // draining `read_fd` first and checking flags second guarantees zero lost signals.
            sig_ids.push(flag::register(SIGTERM, Arc::clone(&term_flag))?);
            sig_ids.push(flag::register(SIGINT, Arc::clone(&term_flag))?);
            sig_ids.push(flag::register(SIGHUP, Arc::clone(&hup_flag))?);

            let write_term = write_fd.try_clone()?;
            let write_int = write_fd.try_clone()?;
            sig_ids.push(sig_pipe::register(SIGTERM, write_term)?);
            sig_ids.push(sig_pipe::register(SIGINT, write_int)?);
            sig_ids.push(sig_pipe::register(SIGHUP, write_fd)?);
            Ok(())
        })();

        if let Err(err) = register_res {
            for id in sig_ids {
                unregister(id);
            }
            return Err(err);
        }

        Ok(Self {
            read_fd,
            hup_flag,
            term_flag,
            sig_ids,
        })
    }

    fn drain_pipe(&self) {
        let mut buf = [0_u8; 64];
        loop {
            match rustix::io::read(&self.read_fd, &mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) | Err(_) => break,
            }
        }
    }

    fn take_pending_signal(&self) -> Option<WakeReason> {
        self.drain_pipe();
        if self.term_flag.swap(false, Ordering::SeqCst) {
            return Some(WakeReason::Shutdown);
        }
        if self.hup_flag.swap(false, Ordering::SeqCst) {
            return Some(WakeReason::Reload);
        }
        None
    }
}

impl Drop for SignalWakePipe {
    fn drop(&mut self) {
        for id in self.sig_ids.drain(..) {
            unregister(id);
        }
    }
}

fn configure_nonblocking_cloexec(fd: &OwnedFd) -> io::Result<()> {
    fcntl_setfd(fd, FdFlags::CLOEXEC).map_err(io::Error::from)?;
    let flags = fcntl_getfl(fd).map_err(io::Error::from)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
    Ok(())
}

fn duration_to_timespec(dur: Duration) -> Timespec {
    // Clamp per-poll slice to `i32::MAX` seconds (~68 years) so 32-bit `time_t` platforms never overflow.
    let secs = dur
        .as_secs()
        .min(u64::try_from(i32::MAX).unwrap_or(u64::MAX));
    let subsec_ns = i32::try_from(dur.subsec_nanos()).unwrap_or(0);
    Timespec {
        tv_sec: Secs::try_from(secs).unwrap_or(Secs::from(i32::MAX)),
        tv_nsec: Nsecs::from(subsec_ns),
    }
}

impl DaemonSleeper for SignalWakePipe {
    fn sleep(&mut self, sleep_secs: u64) -> io::Result<WakeReason> {
        if let Some(reason) = self.take_pending_signal() {
            return Ok(reason);
        }

        // Timer justification (`PLAN.md` §0 #7 & §7):
        // Single kernel `poll(2)` wait for the exact predicted next above-threshold step
        // (`60..=max_sleep_secs`, default `60..=1800`s from `data/model.toml`), performing zero
        // periodic wakes while remaining immediately interruptible by `SIGHUP`/`SIGTERM`/`SIGINT`.
        let deadline = Instant::now() + Duration::from_secs(sleep_secs);
        loop {
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return Ok(WakeReason::Timer);
            };
            if remaining.is_zero() {
                return Ok(WakeReason::Timer);
            }

            let ts = duration_to_timespec(remaining);
            let mut fds = [PollFd::new(&self.read_fd, PollFlags::IN)];
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => {
                    if let Some(reason) = self.take_pending_signal() {
                        return Ok(reason);
                    }
                    if Instant::now() >= deadline {
                        return Ok(WakeReason::Timer);
                    }
                }
                Ok(_) | Err(Errno::INTR) => {
                    if let Some(reason) = self.take_pending_signal() {
                        return Ok(reason);
                    }
                }
                Err(err) => return Err(io::Error::from(err)),
            }
        }
    }
}

/// Runs the predicted-step scheduler loop until `sleeper` yields [`WakeReason::Shutdown`].
pub fn run_daemon_loop<C, S, D>(
    paths: &ShikiPaths,
    clock: &C,
    sleeper: &mut S,
    mut on_updated: D,
) -> Result<(), String>
where
    C: Clock,
    S: DaemonSleeper,
    D: FnMut(&Frame),
{
    loop {
        let now: UtcInstant = clock.now();
        let sleep_secs = match daemon_tick(paths, now, encode_osc_frame) {
            Ok(outcome) => {
                if let DaemonTickOutcome::Updated { ref frame, .. } = outcome {
                    on_updated(frame);
                }
                outcome.sleep_secs()
            }
            Err(err) => {
                // `PLAN.md` §6 ("An invalid palette never applies"): keep last valid store files
                // intact and avoid tight respawn loops; `SIGHUP` from `config set` or `locate`
                // wakes this 30-minute fallback sleep (`DEFAULT_MAX_SLEEP_SECS`) immediately.
                eprintln!("warning: {err}");
                DEFAULT_MAX_SLEEP_SECS
            }
        };

        match sleeper.sleep(sleep_secs).map_err(|e| e.to_string())? {
            WakeReason::Timer | WakeReason::Reload => continue,
            WakeReason::Shutdown => return Ok(()),
        }
    }
}

/// Runs `daemon run` in the foreground, holding an exclusive `flock` on `daemon.pid`.
pub fn run_foreground_daemon(paths: &ShikiPaths) -> Result<(), String> {
    let _pid_guard = DaemonPidGuard::acquire(paths).map_err(|e| e.to_string())?;
    let mut wake_pipe = SignalWakePipe::install().map_err(|e| e.to_string())?;
    run_daemon_loop(paths, &SystemClock, &mut wake_pipe, |_frame| {})
}

fn pid_from_u32(raw_pid: u32) -> Option<Pid> {
    let signed = i32::try_from(raw_pid).ok().filter(|&p| p > 0)?;
    Pid::from_raw(signed)
}

/// Sends `SIGHUP` to the active user daemon (if one holds `daemon.pid`), returning `Ok(true)` when
/// a live daemon was signalled (`PLAN.md` §7).
pub fn notify_running_daemon(paths: &ShikiPaths) -> Result<bool, String> {
    let Some(raw_pid) = probe_running_daemon_pid(paths).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let Some(pid) = pid_from_u32(raw_pid) else {
        return Ok(false);
    };
    match kill_process(pid, Signal::HUP) {
        Ok(()) => Ok(true),
        Err(Errno::SRCH) => Ok(false),
        Err(err) => Err(io::Error::from(err).to_string()),
    }
}

/// Sends `SIGTERM` to any active user daemon during `daemon uninstall` and waits up to 2 seconds
/// for its `daemon.pid` `flock` to release cleanly.
pub fn stop_running_daemon(paths: &ShikiPaths) -> Result<bool, String> {
    let Some(raw_pid) = probe_running_daemon_pid(paths).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let Some(pid) = pid_from_u32(raw_pid) else {
        return Ok(false);
    };
    match kill_process(pid, Signal::TERM) {
        Ok(()) => {}
        Err(Errno::SRCH) => return Ok(false),
        Err(err) => return Err(io::Error::from(err).to_string()),
    }

    // Timer justification (`PLAN.md` §0 #7): bounded one-shot interactive CLI wait (20 ms poll up
    // to 2 s) during `daemon uninstall` so the departing daemon releases `daemon.pid` before exit.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if probe_running_daemon_pid(paths)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(true)
}

/// Resolves the verified per-user service unit path (`docs/terminals/daemon-user-units.md`):
/// - macOS (`launchd.plist(5)`): `$HOME/Library/LaunchAgents/<BUNDLE_ID>.plist`
/// - Linux (`systemd.unit(5)`): `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/<CLI_BIN>.service`
pub fn default_user_unit_path_with(
    platform: PlatformKind,
    home: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> Result<PathBuf, String> {
    let home = home
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "unable to determine HOME directory for user daemon unit".to_string())?;

    match platform {
        PlatformKind::MacOs => Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", product::BUNDLE_ID))),
        PlatformKind::Linux => {
            let cfg_base = xdg_config_home
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join(".config"));
            Ok(cfg_base
                .join("systemd")
                .join("user")
                .join(format!("{}.service", product::CLI_BIN)))
        }
    }
}

pub fn default_user_unit_path(platform: PlatformKind) -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let xdg_cfg = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    default_user_unit_path_with(platform, home.as_deref(), xdg_cfg.as_deref())
}

fn escape_xml_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn validate_exe_path_str(exe_path: &Path) -> Result<&str, String> {
    let raw = exe_path
        .to_str()
        .ok_or_else(|| "daemon executable path must be valid UTF-8".to_string())?;
    if raw.is_empty() || raw.contains(['\0', '\n', '\r']) {
        return Err(
            "daemon executable path cannot be empty or contain control newlines".to_string(),
        );
    }
    Ok(raw)
}

/// Renders the per-user macOS `launchd` LaunchAgent XML property list (`launchd.plist(5)`).
pub fn render_launchd_plist(exe_path: &Path, created_dirs: usize) -> Result<String, String> {
    let exe_raw = validate_exe_path_str(exe_path)?;
    let exe_xml = escape_xml_text(exe_raw);
    let label_xml = escape_xml_text(product::BUNDLE_ID);
    let managed_comment = if created_dirs > 0 {
        format!("<!-- {MANAGED_TAG_PREFIX}{created_dirs} -->\n")
    } else {
        String::new()
    };

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
{managed_comment}<plist version=\"1.0\">\n\
<dict>\n\
\t<key>AssociatedBundleIdentifiers</key>\n\
\t<array>\n\
\t\t<string>{label_xml}</string>\n\
\t</array>\n\
\t<key>KeepAlive</key>\n\
\t<dict>\n\
\t\t<key>SuccessfulExit</key>\n\
\t\t<false/>\n\
\t</dict>\n\
\t<key>Label</key>\n\
\t<string>{label_xml}</string>\n\
\t<key>ProcessType</key>\n\
\t<string>Background</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>{exe_xml}</string>\n\
\t\t<string>daemon</string>\n\
\t\t<string>run</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
</dict>\n\
</plist>\n"
    ))
}

fn format_systemd_exec_arg(raw: &str) -> String {
    // `systemd.unit(5)` expands `%` specifiers and `$` env references before command splitting.
    let spec_escaped = raw.replace('%', "%%").replace('$', "$$");
    let needs_quotes = spec_escaped
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '\''));
    if !needs_quotes {
        return spec_escaped;
    }

    let mut quoted = String::with_capacity(spec_escaped.len() + 4);
    quoted.push('"');
    for ch in spec_escaped.chars() {
        if matches!(ch, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// Renders the per-user `systemd --user` service unit (`systemd.service(5)`).
pub fn render_systemd_service(exe_path: &Path, created_dirs: usize) -> Result<String, String> {
    let exe_raw = validate_exe_path_str(exe_path)?;
    let exec_bin = format_systemd_exec_arg(exe_raw);
    let display = product::DISPLAY_NAME;
    let managed_comment = if created_dirs > 0 {
        format!("# {MANAGED_TAG_PREFIX}{created_dirs}\n")
    } else {
        String::new()
    };

    Ok(format!(
        "{managed_comment}[Unit]\n\
Description={display} circadian terminal colour daemon\n\
Documentation=https://github.com/quiinthieu/shiki\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={exec_bin} daemon run\n\
ExecReload=/bin/kill -HUP $MAINPID\n\
Restart=on-failure\n\
RestartSec=5\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    ))
}

fn parse_created_dirs_tag(contents: &str) -> usize {
    for line in contents.lines() {
        if let Some(rest) = line
            .find(MANAGED_TAG_PREFIX)
            .map(|i| &line[i + MANAGED_TAG_PREFIX.len()..])
        {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(n) = digits.parse::<usize>() {
                return n;
            }
        }
    }
    0
}

fn count_missing_ancestor_dirs(unit_path: &Path) -> usize {
    let mut count = 0_usize;
    let mut cur = unit_path.parent();
    while let Some(dir) = cur {
        if dir.as_os_str().is_empty() || dir.exists() {
            break;
        }
        count += 1;
        cur = dir.parent();
    }
    count
}

fn atomic_write_unit(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("unit path '{}' has no parent directory", target.display()))?;
    let base_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unit");

    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;

    let pid = std::process::id();
    let (mut file, tmp_path) = loop {
        let seq = UNIT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{base_name}.tmp.{pid}.{seq}"));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o644);
        }
        match opts.open(&candidate) {
            Ok(f) => break (f, candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("open temp unit {}: {err}", candidate.display())),
        }
    };

    let write_res = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("write {}: {err}", target.display()));
    }

    if let Err(err) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("rename into {}: {err}", target.display()));
    }

    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Service-manager command runner abstracted so unit tests verify exact `launchctl` and
/// `systemctl --user` command sequences without touching the host OS session.
pub trait ServiceManager {
    fn activate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String>;
    fn deactivate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String>;
}

pub struct HostServiceManager;

impl HostServiceManager {
    fn run_checked(program: &str, args: &[&str]) -> Result<(), String> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("failed to execute `{program}`: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!(
                "`{program} {}` exited with {}: {stderr}",
                args.join(" "),
                out.status
            ));
        }
        Ok(())
    }

    fn run_best_effort(program: &str, args: &[&str]) {
        let _ = Command::new(program).args(args).output();
    }
}

impl ServiceManager for HostServiceManager {
    fn activate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String> {
        match platform {
            PlatformKind::MacOs => {
                let uid = getuid().as_raw();
                let domain = format!("gui/{uid}");
                let service_target = format!("{domain}/{}", product::BUNDLE_ID);
                let plist_str = validate_exe_path_str(unit_path)?;
                Self::run_best_effort("launchctl", &["bootout", &service_target]);
                Self::run_checked("launchctl", &["bootstrap", &domain, plist_str])?;
                Self::run_checked("launchctl", &["kickstart", "-k", &service_target])
            }
            PlatformKind::Linux => {
                let unit_name = unit_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}.service", product::CLI_BIN));
                Self::run_checked("systemctl", &["--user", "daemon-reload"])?;
                Self::run_checked("systemctl", &["--user", "enable", "--now", &unit_name])
            }
        }
    }

    fn deactivate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String> {
        match platform {
            PlatformKind::MacOs => {
                let uid = getuid().as_raw();
                let service_target = format!("gui/{uid}/{}", product::BUNDLE_ID);
                Self::run_best_effort("launchctl", &["bootout", &service_target]);
                Ok(())
            }
            PlatformKind::Linux => {
                let unit_name = unit_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}.service", product::CLI_BIN));
                Self::run_best_effort("systemctl", &["--user", "disable", "--now", &unit_name]);
                Self::run_best_effort("systemctl", &["--user", "daemon-reload"]);
                Ok(())
            }
        }
    }
}

#[must_use]
pub fn should_activate_service_manager(no_activate_flag: bool) -> bool {
    if no_activate_flag {
        return false;
    }
    match env::var(NO_SERVICE_MANAGER_ENV) {
        Ok(val) => {
            let trimmed = val.trim();
            trimmed.is_empty() || trimmed == "0" || trimmed.eq_ignore_ascii_case("false")
        }
        Err(_) => true,
    }
}

pub const fn service_kind_label(platform: PlatformKind) -> &'static str {
    match platform {
        PlatformKind::MacOs => "launchd",
        PlatformKind::Linux => "systemd",
    }
}

/// Atomically installs (or idempotently updates) the user daemon service unit for `platform`.
pub fn install_daemon_unit<M: ServiceManager>(
    platform: PlatformKind,
    unit_path: &Path,
    exe_path: &Path,
    activate: bool,
    manager: &mut M,
) -> Result<String, String> {
    let existing = match fs::read_to_string(unit_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("failed to read {}: {err}", unit_path.display())),
    };

    let created_dirs = match existing.as_deref() {
        Some(prev) => parse_created_dirs_tag(prev),
        None => count_missing_ancestor_dirs(unit_path),
    };

    let rendered = match platform {
        PlatformKind::MacOs => render_launchd_plist(exe_path, created_dirs)?,
        PlatformKind::Linux => render_systemd_service(exe_path, created_dirs)?,
    };

    if existing.as_deref() != Some(rendered.as_str()) {
        atomic_write_unit(unit_path, rendered.as_bytes())?;
    }

    if activate {
        manager.activate(platform, unit_path)?;
    }

    Ok(format!(
        "installed {} user unit at {}\n",
        service_kind_label(platform),
        unit_path.display()
    ))
}

/// Deactivates and removes the user daemon service unit for `platform`, pruning any empty parent
/// directories originally created during [`install_daemon_unit`] for an exact round-trip.
pub fn uninstall_daemon_unit<M: ServiceManager>(
    paths: &ShikiPaths,
    platform: PlatformKind,
    unit_path: &Path,
    activate: bool,
    manager: &mut M,
) -> Result<String, String> {
    let _ = stop_running_daemon(paths)?;

    if activate {
        manager.deactivate(platform, unit_path)?;
    }

    let existing = match fs::read_to_string(unit_path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(format!(
                "no {} user unit found at {}\n",
                service_kind_label(platform),
                unit_path.display()
            ));
        }
        Err(err) => return Err(format!("failed to read {}: {err}", unit_path.display())),
    };

    let created_dirs = parse_created_dirs_tag(&existing);
    fs::remove_file(unit_path)
        .map_err(|e| format!("failed to remove {}: {e}", unit_path.display()))?;

    let mut cur = unit_path.parent();
    for _ in 0..created_dirs {
        let Some(dir) = cur else {
            break;
        };
        let next = dir.parent();
        // `fs::remove_dir` succeeds only when `dir` is completely empty, preserving sibling units.
        if fs::remove_dir(dir).is_err() {
            break;
        }
        cur = next;
    }

    Ok(format!(
        "uninstalled {} user unit from {}\n",
        service_kind_label(platform),
        unit_path.display()
    ))
}

/// Coordinate-free daemon status report (`PLAN.md` §0 #4 & §6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DaemonStatusReport {
    pub running: bool,
    pub pid: Option<u32>,
    pub installed: bool,
    pub service_kind: &'static str,
    pub unit_path: String,
    pub paused: bool,
    pub paused_until: Option<String>,
    pub generation: u64,
    pub palette: String,
    pub updated_at_rfc3339: Option<String>,
}

pub fn collect_daemon_status(
    paths: &ShikiPaths,
    platform: PlatformKind,
    unit_path: &Path,
    now_utc: UtcInstant,
) -> Result<DaemonStatusReport, String> {
    let pid = probe_running_daemon_pid(paths).map_err(|e| e.to_string())?;
    let state = load_state(paths).map_err(|e| e.to_string())?;
    let config = load_config(paths).map_err(|e| e.to_string())?;

    let (paused, paused_until) = match state.pause_status_at(now_utc) {
        PauseStatus::NotPaused => (false, None),
        PauseStatus::Indefinite => (true, Some(INDEFINITE_PAUSE.to_string())),
        PauseStatus::Until(until) => (true, Some(until.format_rfc3339_utc())),
    };

    let palette = state
        .palette
        .unwrap_or_else(|| config.effective_palette().to_string());

    Ok(DaemonStatusReport {
        running: pid.is_some(),
        pid,
        installed: unit_path.is_file(),
        service_kind: service_kind_label(platform),
        unit_path: unit_path.display().to_string(),
        paused,
        paused_until,
        generation: state.generation,
        palette,
        updated_at_rfc3339: state.updated_at_rfc3339,
    })
}

pub fn format_daemon_status_text(report: &DaemonStatusReport) -> String {
    let state_line = match report.pid {
        Some(pid) => format!("running (pid {pid})"),
        None => "stopped".to_string(),
    };
    let unit_line = if report.installed {
        format!("installed ({}, {})", report.service_kind, report.unit_path)
    } else {
        format!(
            "not installed ({}, {})",
            report.service_kind, report.unit_path
        )
    };
    let paused_line = match report.paused_until.as_deref() {
        None => "no".to_string(),
        Some(INDEFINITE_PAUSE) => INDEFINITE_PAUSE.to_string(),
        Some(ts) => format!("until {ts}"),
    };
    let updated = report.updated_at_rfc3339.as_deref().unwrap_or("never");

    format!(
        "daemon: {state_line}\nunit: {unit_line}\npaused: {paused_line}\ngeneration: {}\npalette: {}\nlast update: {updated}\n",
        report.generation, report.palette
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sk_core::location::ManualLocation;
    use sk_core::scheduler::MIN_DAEMON_SLEEP_SECS;
    use sk_core::time::{FakeClock, SECS_PER_DAY};
    use sk_store::config::{set_config_value, set_manual_location};
    use std::collections::VecDeque;
    use tempfile::TempDir;

    type ScriptedInterrupt = (u64, WakeReason, Box<dyn FnOnce()>);

    struct ScriptedFakeSleeper<'a> {
        clock: &'a FakeClock,
        end: UtcInstant,
        scripted_interrupts: VecDeque<ScriptedInterrupt>,
        recorded_sleeps: Vec<u64>,
    }

    impl DaemonSleeper for ScriptedFakeSleeper<'_> {
        fn sleep(&mut self, sleep_secs: u64) -> io::Result<WakeReason> {
            self.recorded_sleeps.push(sleep_secs);
            if let Some((advance_secs, reason, hook)) = self.scripted_interrupts.pop_front() {
                self.clock
                    .advance_secs(i64::try_from(advance_secs).unwrap());
                hook();
                return Ok(reason);
            }
            let next = self.clock.advance_secs(i64::try_from(sleep_secs).unwrap());
            if next >= self.end {
                Ok(WakeReason::Shutdown)
            } else {
                Ok(WakeReason::Timer)
            }
        }
    }

    #[derive(Default)]
    struct RecordingServiceManager {
        events: Vec<(String, PlatformKind, PathBuf)>,
    }

    impl ServiceManager for RecordingServiceManager {
        fn activate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String> {
            self.events
                .push(("activate".to_string(), platform, unit_path.to_path_buf()));
            Ok(())
        }

        fn deactivate(&mut self, platform: PlatformKind, unit_path: &Path) -> Result<(), String> {
            self.events
                .push(("deactivate".to_string(), platform, unit_path.to_path_buf()));
            Ok(())
        }
    }

    #[test]
    fn fake_clock_daemon_loop_runs_full_day_and_handles_early_sighup_reload() {
        let tmp = TempDir::new().unwrap();
        let paths = ShikiPaths::from_shiki_home(tmp.path());
        paths.ensure_dirs().unwrap();
        set_manual_location(&paths, ManualLocation::new(35.7, 139.7).unwrap()).unwrap();

        let start = UtcInstant::utc_ymd_hms(2026, 3, 20, 15, 0, 0);
        let clock = FakeClock::new(start);
        let paths_for_reload = paths.clone();

        let mut scripted = VecDeque::new();
        // Interrupt the initial 30-minute deep-night sleep after just 10 seconds with `SIGHUP`
        // (`WakeReason::Reload`) while tightening `max_sleep_secs` to `900`.
        scripted.push_back((
            10_u64,
            WakeReason::Reload,
            Box::new(move || {
                set_config_value(&paths_for_reload, "max_sleep_secs", "900", false).unwrap();
            }) as Box<dyn FnOnce()>,
        ));

        let mut sleeper = ScriptedFakeSleeper {
            clock: &clock,
            end: start.add_secs(SECS_PER_DAY),
            scripted_interrupts: scripted,
            recorded_sleeps: Vec::new(),
        };

        let mut deliveries = 0_usize;
        run_daemon_loop(&paths, &clock, &mut sleeper, |_frame| {
            deliveries += 1;
        })
        .unwrap();

        // First sleep used default 1800s; immediately after the 10s SIGHUP reload, the daemon
        // honoured the new `max_sleep_secs = 900` cap and never polled faster than 60s.
        assert_eq!(sleeper.recorded_sleeps[0], 1_800);
        assert_eq!(sleeper.recorded_sleeps[1], 900);
        assert!(sleeper.recorded_sleeps[1..]
            .iter()
            .all(|&s| (MIN_DAEMON_SLEEP_SECS..=900).contains(&s)));
        assert!(deliveries >= 15);
    }

    #[test]
    fn service_manager_receives_activate_and_deactivate_on_install_and_uninstall() {
        let tmp = TempDir::new().unwrap();
        let paths = ShikiPaths::from_shiki_home(tmp.path().join("store"));
        paths.ensure_dirs().unwrap();
        let exe = PathBuf::from(format!("/usr/local/bin/{}", product::CLI_BIN));

        for platform in [PlatformKind::MacOs, PlatformKind::Linux] {
            let unit_path = default_user_unit_path_with(
                platform,
                Some(&tmp.path().join("home")),
                Some(&tmp.path().join("xdg")),
            )
            .unwrap();
            let mut mgr = RecordingServiceManager::default();
            install_daemon_unit(platform, &unit_path, &exe, true, &mut mgr).unwrap();
            uninstall_daemon_unit(&paths, platform, &unit_path, true, &mut mgr).unwrap();
            assert_eq!(
                mgr.events,
                vec![
                    ("activate".to_string(), platform, unit_path.clone()),
                    ("deactivate".to_string(), platform, unit_path),
                ]
            );
        }
    }
}
