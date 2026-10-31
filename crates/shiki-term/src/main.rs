mod daemon;
mod hook;
mod preview;

use clap::{Args, Parser, Subcommand};
use daemon::{
    collect_daemon_status, default_user_unit_path, format_daemon_status_text, install_daemon_unit,
    notify_running_daemon, run_foreground_daemon, should_activate_service_manager,
    uninstall_daemon_unit, DaemonPlatformArg, HostServiceManager,
};
use hook::{
    install_hook_to_rc, preview_rc_block, render_hook_script, uninstall_hook_from_rc, HookShell,
};
use preview::{render_preview, ColorChoiceArg, PreviewColorMode};
use sk_core::location::{ManualLocation, ResolvedLocation};
use sk_core::palette::{
    check_palette_year, compute_frame, ModelConfig, PaletteCheckReport, WorstMoment,
};
use sk_core::product;
use sk_core::solar::{Coordinates, PolarState};
use sk_core::time::{Clock, UtcInstant};
use sk_deliver::osc::{encode_osc_frame, OscBackend};
use sk_deliver::Backend;
use sk_store::clock::SystemClock;
use sk_store::config::{
    get_config_value, list_config_entries, load_config, resolve_location, set_config_value,
    set_manual_location, set_timezone_location,
};
use sk_store::error::StoreError;
use sk_store::moment::{parse_pause_duration_secs, resolve_moment_query};
use sk_store::palette::load_palette;
use sk_store::paths::{PlatformKind, ShikiPaths};
use sk_store::store::{
    clear_pause, persist_applied_frame_at, persist_reset, refresh_hook_store, set_pause, Snapshot,
};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = product::CLI_BIN,
    bin_name = product::CLI_BIN,
    version = product::VERSION,
    about = "Terminal colours that drift slowly with local sunrise, sunset and the seasons"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show the current solar phase, season, contrast, and blended colours
    Now(NowArgs),
    /// Compute the current frame, update store files, and emit OSC sequences to stdout
    Apply(ApplyArgs),
    /// Emit OSC 104/110/111/112 to restore the terminal's default colours
    Reset,
    /// Preview swatches and sample text for any moment without changing terminal palette
    Preview(PreviewArgs),
    /// Pause circadian colour drift indefinitely or `--for <DURATION>`
    Pause(PauseArgs),
    /// Resume circadian colour drift immediately
    Resume,
    /// Read or update configuration keys in `config.toml`
    Config(ConfigArgs),
    /// Inspect or configure location (timezone-derived or manual coordinates)
    Locate(LocateArgs),
    /// Manage and verify terminal colour palettes
    Palette(PaletteArgs),
    /// Emit or install the shell prompt hook (`zsh`, `bash`, `fish`)
    Hook(HookArgs),
    /// Install, uninstall, inspect, or run the per-user background daemon
    Daemon(DaemonArgs),
    /// Internal prompt-hook helper that refreshes `current.seq` and `current.gen`
    #[command(name = "_emit", hide = true)]
    Emit(EmitArgs),
}

#[derive(Args, Debug)]
struct NowArgs {
    /// Output the coordinate-free versioned snapshot as pretty JSON
    #[arg(long)]
    json: bool,

    /// Optional RFC 3339 or `HH:MM[:SS]` moment override for deterministic inspection
    #[arg(long)]
    time: Option<String>,
}

#[derive(Args, Debug)]
struct ApplyArgs {
    /// Override palette name (`shiki`, user palette in `palettes/`) or path to a `.toml` file
    #[arg(long)]
    palette: Option<String>,

    /// Optional RFC 3339 or `HH:MM[:SS]` moment override for deterministic simulation
    #[arg(long)]
    time: Option<String>,
}

#[derive(Args, Debug)]
struct PreviewArgs {
    /// Time of day (`HH:MM`, `HH:MM:SS`, `HH:MM±HH:MM`) or full RFC 3339 timestamp
    #[arg(long, allow_hyphen_values = true)]
    time: Option<String>,

    /// Calendar date (`YYYY-MM-DD`)
    #[arg(long, allow_hyphen_values = true)]
    date: Option<String>,

    /// Override latitude in decimal degrees (`[-90, 90]`, requires `--lon`)
    #[arg(long, allow_hyphen_values = true, requires = "lon")]
    lat: Option<f64>,

    /// Override longitude in decimal degrees (`[-180, 180]`, requires `--lat`)
    #[arg(long, allow_hyphen_values = true, requires = "lat")]
    lon: Option<f64>,

    /// Palette name (`shiki`, user palette in `palettes/`) or path to a `.toml` file
    #[arg(long)]
    palette: Option<String>,

    /// Output the coordinate-free versioned snapshot as pretty JSON
    #[arg(long)]
    json: bool,

    /// ANSI colour mode (`auto` respects `NO_COLOR` and `$COLORTERM`, falling back to 256-colour)
    #[arg(long, value_enum, default_value_t = ColorChoiceArg::Auto)]
    color: ColorChoiceArg,
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Get one configuration key's effective value (or list all keys when omitted)
    Get(ConfigGetArgs),
    /// Atomically validate and update a configuration key in `config.toml`
    Set(ConfigSetArgs),
}

#[derive(Args, Debug)]
struct ConfigGetArgs {
    /// Dotted configuration key (e.g. `palette`, `step_threshold`, `location.mode`)
    key: Option<String>,
}

#[derive(Args, Debug)]
struct ConfigSetArgs {
    /// Dotted configuration key to update
    key: String,

    /// New value to validate and persist
    #[arg(allow_hyphen_values = true)]
    value: String,

    /// Acknowledge lowering WCAG contrast thresholds below defaults (`PLAN.md` §4)
    #[arg(long)]
    i_know: bool,
}

#[derive(Args, Debug)]
struct LocateArgs {
    /// Set manual coordinates (`<LAT,LON>` in decimal degrees, rounded to 0.1 deg)
    #[arg(
        long,
        value_name = "LAT,LON",
        allow_hyphen_values = true,
        conflicts_with = "timezone"
    )]
    set: Option<String>,

    /// Switch back to automatic IANA timezone-derived coordinates
    #[arg(long)]
    timezone: bool,
}

#[derive(Args, Debug)]
struct PaletteArgs {
    #[command(subcommand)]
    command: PaletteCommand,
}

#[derive(Subcommand, Debug)]
enum PaletteCommand {
    /// Sweep a full year and verify WCAG contrast, printing the worst moment
    Check(PaletteCheckArgs),
}

#[derive(Args, Debug)]
struct PaletteCheckArgs {
    /// Palette name (`shiki`, user palette in palettes/) or path to a `.toml` file
    #[arg(default_value = "shiki")]
    palette: String,

    /// Override latitude in decimal degrees (`[-90, 90]`, requires `--lon`)
    #[arg(long, allow_hyphen_values = true, requires = "lon")]
    lat: Option<f64>,

    /// Override longitude in decimal degrees (`[-180, 180]`, requires `--lat`)
    #[arg(long, allow_hyphen_values = true, requires = "lat")]
    lon: Option<f64>,

    /// Proleptic Gregorian UTC year to sweep (defaults to current UTC year)
    #[arg(long, allow_hyphen_values = true)]
    year: Option<i32>,

    /// Sweep time step in minutes (`1..=1440`)
    #[arg(long, default_value_t = 10)]
    step_minutes: u32,

    /// Custom minimum WCAG contrast ratio for `fg/bg` and `selection` (default `4.5`)
    #[arg(long)]
    fg_contrast: Option<f64>,

    /// Custom minimum WCAG contrast ratio for non-exempt ANSI slots vs `bg` (default `3.0`)
    #[arg(long)]
    ansi_contrast: Option<f64>,

    /// Acknowledge lowering WCAG contrast thresholds below defaults (`PLAN.md` §4)
    #[arg(long)]
    i_know: bool,
}

#[derive(Args, Debug)]
struct HookArgs {
    #[command(subcommand)]
    command: HookCommand,
}

#[derive(Subcommand, Debug)]
enum HookCommand {
    /// Print the zsh prompt hook script (or `install --write-rc` / `uninstall`)
    Zsh(HookShellTailArgs),
    /// Print the bash prompt hook script (or `install --write-rc` / `uninstall`)
    Bash(HookShellTailArgs),
    /// Print the fish prompt hook script (or `install --write-rc` / `uninstall`)
    Fish(HookShellTailArgs),
    /// Install the marked hook block into the shell rc file (requires `--write-rc`)
    Install(HookInstallArgs),
    /// Remove the marked hook block from the shell rc file
    Uninstall(HookUninstallArgs),
}

#[derive(Args, Debug)]
struct HookShellTailArgs {
    #[command(subcommand)]
    action: Option<HookShellAction>,
}

#[derive(Subcommand, Debug)]
enum HookShellAction {
    /// Install the marked hook block into this shell's rc file (requires `--write-rc`)
    Install(HookActionFlags),
    /// Remove the marked hook block from this shell's rc file
    Uninstall(HookActionFlags),
}

#[derive(Args, Debug)]
struct HookActionFlags {
    /// Explicitly authorize editing the shell rc file (with `.bak` backup)
    #[arg(long)]
    write_rc: bool,

    /// Optional custom rc file path override
    #[arg(long)]
    rc_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct HookInstallArgs {
    /// Target shell (`zsh`, `bash`, or `fish`; defaults to `$SHELL`)
    #[arg(value_enum, conflicts_with = "shell_flag")]
    shell: Option<HookShell>,

    /// Target shell (`zsh`, `bash`, or `fish`; defaults to `$SHELL`)
    #[arg(long = "shell", value_enum)]
    shell_flag: Option<HookShell>,

    /// Explicitly authorize editing the shell rc file (with `.bak` backup)
    #[arg(long)]
    write_rc: bool,

    /// Optional custom rc file path override
    #[arg(long)]
    rc_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct HookUninstallArgs {
    /// Target shell (`zsh`, `bash`, or `fish`; defaults to `$SHELL`)
    #[arg(value_enum, conflicts_with = "shell_flag")]
    shell: Option<HookShell>,

    /// Target shell (`zsh`, `bash`, or `fish`; defaults to `$SHELL`)
    #[arg(long = "shell", value_enum)]
    shell_flag: Option<HookShell>,

    /// Accepted for symmetry with `install --write-rc`
    #[arg(long)]
    write_rc: bool,

    /// Optional custom rc file path override
    #[arg(long)]
    rc_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct PauseArgs {
    /// Optional human duration (e.g. `30m`, `2h`, `1h30m`, `90s`, `1d`); pauses indefinitely if omitted
    #[arg(long = "for", value_name = "DURATION", allow_hyphen_values = true)]
    duration: Option<String>,
}

#[derive(Args, Debug)]
struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// Install (and activate) the per-user `launchd` LaunchAgent or `systemd --user` service
    Install(DaemonInstallArgs),
    /// Stop, deactivate, and remove the per-user `launchd` or `systemd --user` unit
    Uninstall(DaemonUninstallArgs),
    /// Show whether the daemon is running, installed, or paused
    Status(DaemonStatusArgs),
    /// Run the foreground predicted-step scheduler loop (invoked by `launchd` / `systemd --user`)
    Run,
}

#[derive(Args, Debug)]
struct DaemonInstallArgs {
    /// Custom service unit file path override
    #[arg(long)]
    unit_path: Option<PathBuf>,

    /// Write the unit file without calling `launchctl` or `systemctl --user`
    #[arg(long)]
    no_activate: bool,

    /// Target OS unit format override (used by cross-platform tests and packaging verification)
    #[arg(long, value_enum, hide = true)]
    platform: Option<DaemonPlatformArg>,

    /// Daemon executable path override recorded in the generated unit
    #[arg(long, hide = true)]
    exe_path: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct DaemonUninstallArgs {
    /// Custom service unit file path override
    #[arg(long)]
    unit_path: Option<PathBuf>,

    /// Remove the unit file without calling `launchctl` or `systemctl --user`
    #[arg(long)]
    no_activate: bool,

    /// Target OS unit format override (used by cross-platform tests and packaging verification)
    #[arg(long, value_enum, hide = true)]
    platform: Option<DaemonPlatformArg>,
}

#[derive(Args, Debug)]
struct DaemonStatusArgs {
    /// Output the coordinate-free daemon status report as pretty JSON
    #[arg(long)]
    json: bool,

    /// Custom service unit file path override
    #[arg(long)]
    unit_path: Option<PathBuf>,

    /// Target OS unit format override
    #[arg(long, value_enum, hide = true)]
    platform: Option<DaemonPlatformArg>,
}

#[derive(Args, Debug)]
struct EmitArgs {
    /// Recompute at most once per `hook_min_interval_secs`
    #[arg(long)]
    if_stale: bool,

    /// Optional RFC 3339 or `HH:MM[:SS]` moment override for deterministic simulation
    #[arg(long)]
    time: Option<String>,
}

fn polar_state_str(state: PolarState) -> &'static str {
    match state {
        PolarState::Normal => "normal",
        PolarState::PolarDay => "polar_day",
        PolarState::PolarNight => "polar_night",
    }
}

fn iana_zone_from_resolved(resolved: &ResolvedLocation) -> Option<&str> {
    match resolved {
        ResolvedLocation::Timezone(tz) => Some(tz.canonical_zone.as_str()),
        ResolvedLocation::Manual(_) => None,
    }
}

fn format_snapshot_json(snapshot: &Snapshot) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(snapshot).map_err(|e| e.to_string())?;
    json.push('\n');
    Ok(json)
}

fn format_now_text(snapshot: &Snapshot) -> String {
    let polar = polar_state_str(snapshot.polar_state);
    let next_event = match &snapshot.next_event {
        Some(ev) => format!("{} at {}", ev.kind, ev.instant_rfc3339),
        None => format!("none ({polar})"),
    };
    let c = &snapshot.colors;
    format!(
        "palette: {}\ninstant: {}\nphase: {} ({}, d={:.3}, elevation={:+.1} deg, noon={:+.1} deg, polar={polar})\nseason: {} (s={:.3}, strength={:.3})\nnext event: {next_event}\ncolors: bg={} fg={} cursor={} selection={} on {}\ncontrast: fg/bg={:.2}:1 selection={:.2}:1 min_ansi/bg={:.2}:1 (fix-up slots: {})\n",
        snapshot.palette,
        snapshot.instant_rfc3339,
        snapshot.solar_phase,
        snapshot.day_half,
        snapshot.day_phase,
        snapshot.elevation_deg,
        snapshot.noon_elevation_deg,
        snapshot.season,
        snapshot.season_phase,
        snapshot.season_strength,
        c.bg,
        c.fg,
        c.cursor,
        c.selection_fg,
        c.selection_bg,
        snapshot.contrast.fg_bg_ratio,
        snapshot.contrast.selection_ratio,
        snapshot.contrast.min_ansi_bg_ratio,
        snapshot.contrast.fixup_slots,
    )
}

fn run_now(args: NowArgs) -> Result<String, String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    let config = load_config(&paths).map_err(|e| e.to_string())?;
    let model = config.effective_model_config().map_err(|e| e.to_string())?;
    let resolved = resolve_location(&paths).map_err(|e| e.to_string())?;
    let palette = load_palette(&paths, config.effective_palette()).map_err(|e| e.to_string())?;

    let now_utc: UtcInstant = SystemClock.now();
    let instant = resolve_moment_query(
        None,
        args.time.as_deref(),
        iana_zone_from_resolved(&resolved).or(config.location.timezone.as_deref()),
        now_utc,
    )
    .map_err(|e| e.to_string())?;

    let coords = resolved.coordinates();
    let frame = compute_frame(instant, coords, &palette, &model);
    let snapshot = Snapshot::from_frame(palette.name(), &frame, coords, &model);

    if args.json {
        format_snapshot_json(&snapshot)
    } else {
        Ok(format_now_text(&snapshot))
    }
}

fn run_apply(args: ApplyArgs) -> Result<(), String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    let config = load_config(&paths).map_err(|e| e.to_string())?;
    let model = config.effective_model_config().map_err(|e| e.to_string())?;
    let resolved = resolve_location(&paths).map_err(|e| e.to_string())?;
    let palette_spec = args
        .palette
        .as_deref()
        .unwrap_or_else(|| config.effective_palette());
    let palette = load_palette(&paths, palette_spec).map_err(|e| e.to_string())?;

    let now_utc: UtcInstant = SystemClock.now();
    let instant = resolve_moment_query(
        None,
        args.time.as_deref(),
        iana_zone_from_resolved(&resolved).or(config.location.timezone.as_deref()),
        now_utc,
    )
    .map_err(|e| e.to_string())?;

    let coords = resolved.coordinates();
    let frame = compute_frame(instant, coords, &palette, &model);
    let snapshot = Snapshot::from_frame(palette.name(), &frame, coords, &model);
    let osc_bytes = encode_osc_frame(&frame.colors);

    persist_applied_frame_at(&paths, palette.name(), &snapshot, &osc_bytes, now_utc)
        .map_err(|e| e.to_string())?;

    let stdout = io::stdout();
    let mut backend = OscBackend::new(stdout.lock());
    backend.deliver(&frame).map_err(|e| e.to_string())
}

fn run_reset() -> Result<(), String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    persist_reset(&paths, SystemClock.now()).map_err(|e| e.to_string())?;

    let stdout = io::stdout();
    let mut backend = OscBackend::new(stdout.lock());
    backend.reset().map_err(|e| e.to_string())
}

fn run_preview(args: PreviewArgs) -> Result<String, String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    let config = load_config(&paths).map_err(|e| e.to_string())?;
    let model = config.effective_model_config().map_err(|e| e.to_string())?;

    let (coords, iana_zone_owned) = match (args.lat, args.lon) {
        (Some(lat), Some(lon)) => {
            let c = Coordinates::new(lat, lon).map_err(|e| e.to_string())?;
            (c, config.location.timezone.clone())
        }
        _ => {
            let resolved = resolve_location(&paths).map_err(|e| e.to_string())?;
            let zone = iana_zone_from_resolved(&resolved)
                .map(str::to_string)
                .or_else(|| config.location.timezone.clone());
            (resolved.coordinates(), zone)
        }
    };

    let palette_spec = args
        .palette
        .as_deref()
        .unwrap_or_else(|| config.effective_palette());
    let palette = load_palette(&paths, palette_spec).map_err(|e| e.to_string())?;

    let instant = resolve_moment_query(
        args.date.as_deref(),
        args.time.as_deref(),
        iana_zone_owned.as_deref(),
        SystemClock.now(),
    )
    .map_err(|e| e.to_string())?;

    let frame = compute_frame(instant, coords, &palette, &model);
    let snapshot = Snapshot::from_frame(palette.name(), &frame, coords, &model);

    if args.json {
        format_snapshot_json(&snapshot)
    } else {
        let mode = PreviewColorMode::resolve_from_env(args.color);
        Ok(render_preview(&snapshot, mode))
    }
}

fn run_config(args: ConfigArgs) -> Result<String, StoreError> {
    let paths = ShikiPaths::resolve()?;
    match args.command {
        ConfigCommand::Get(get_args) => match get_args.key {
            Some(key) => {
                let val = get_config_value(&paths, &key)?;
                Ok(format!("{val}\n"))
            }
            None => {
                let entries = list_config_entries(&paths)?;
                let mut out = String::new();
                for (k, v) in entries {
                    out.push_str(k);
                    out.push_str(" = ");
                    out.push_str(&v);
                    out.push('\n');
                }
                Ok(out)
            }
        },
        ConfigCommand::Set(set_args) => {
            let effective =
                set_config_value(&paths, &set_args.key, &set_args.value, set_args.i_know)?;
            let _ = notify_running_daemon(&paths);
            Ok(format!("{} = {effective}\n", set_args.key.trim()))
        }
    }
}

fn format_resolved_location(resolved: &ResolvedLocation) -> String {
    match resolved {
        ResolvedLocation::Timezone(tz) => {
            let tz_line = if tz.is_alias() {
                format!("{} (linked from {})", tz.canonical_zone, tz.requested_zone)
            } else {
                tz.canonical_zone.clone()
            };
            format!(
                "mode: timezone\ntimezone: {tz_line}\ncoordinates: {}\n",
                resolved.format_display_coords()
            )
        }
        ResolvedLocation::Manual(_) => {
            format!(
                "mode: manual\ncoordinates: {}\n",
                resolved.format_display_coords()
            )
        }
    }
}

fn run_locate(args: LocateArgs) -> Result<String, StoreError> {
    let paths = ShikiPaths::resolve()?;
    let resolved = if let Some(pair) = args.set {
        let manual = ManualLocation::parse(&pair)?;
        let out = set_manual_location(&paths, manual)?;
        let _ = notify_running_daemon(&paths);
        out
    } else if args.timezone {
        let out = set_timezone_location(&paths)?;
        let _ = notify_running_daemon(&paths);
        out
    } else {
        resolve_location(&paths)?
    };
    Ok(format_resolved_location(&resolved))
}

fn format_worst_moment(label: &str, moment: &WorstMoment) -> String {
    format!(
        "{label}: {} slot={} contrast={:.2}:1 (min {:.2}:1, raw {:.2}:1) fg={} bg={} d={:.3} ({}) s={:.3}",
        moment.instant,
        moment.slot,
        moment.final_contrast_ratio,
        moment.required_ratio,
        moment.raw_contrast_ratio,
        moment.fg_color,
        moment.bg_color,
        moment.day_phase,
        moment.solar_phase.as_str(),
        moment.season_phase,
    )
}

fn format_palette_check_report(report: &PaletteCheckReport) -> String {
    let hemisphere = if report.is_northern_hemisphere {
        "northern"
    } else {
        "southern"
    };
    let status = if report.all_passed { "pass" } else { "fail" };
    format!(
        "palette: {}\nyear: {}\nhemisphere: {hemisphere}\nstep: {}m ({} frames)\nthresholds: fg/bg >= {:.2}:1, ansi/bg >= {:.2}:1\nstatus: {status} (fix-up frames: {}/{})\n{}\n{}\n{}\n",
        report.palette_name,
        report.year,
        report.step_minutes,
        report.frames_checked,
        report.thresholds.fg_bg_min_ratio,
        report.thresholds.ansi_bg_min_ratio,
        report.frames_fixed_up,
        report.frames_checked,
        format_worst_moment("worst fg/selection", &report.worst_text),
        format_worst_moment("worst ansi", &report.worst_ansi),
        format_worst_moment("worst moment", &report.worst_overall),
    )
}

fn run_palette_check(args: PaletteCheckArgs) -> Result<String, String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    let mut model = ModelConfig::bundled();
    let default_contrast = model.contrast;

    if let Some(fg_ratio) = args.fg_contrast {
        model.contrast.fg_bg_min_ratio = fg_ratio;
    }
    if let Some(ansi_ratio) = args.ansi_contrast {
        model.contrast.ansi_bg_min_ratio = ansi_ratio;
    }
    model.contrast.validate().map_err(|e| e.to_string())?;

    let lowered = model.contrast.fg_bg_min_ratio < default_contrast.fg_bg_min_ratio
        || model.contrast.ansi_bg_min_ratio < default_contrast.ansi_bg_min_ratio;
    if lowered && !args.i_know {
        return Err(format!(
            "lowering WCAG contrast thresholds below defaults ({:.1}:1 fg/bg, {:.1}:1 ansi/bg) requires `--i-know`",
            default_contrast.fg_bg_min_ratio, default_contrast.ansi_bg_min_ratio
        ));
    }

    let coords = match (args.lat, args.lon) {
        (Some(lat), Some(lon)) => Coordinates::new(lat, lon).map_err(|e| e.to_string())?,
        _ => resolve_location(&paths)
            .map_err(|e| e.to_string())?
            .coordinates(),
    };

    let palette = load_palette(&paths, &args.palette).map_err(|e| e.to_string())?;
    let year = args
        .year
        .unwrap_or_else(|| SystemClock.now().to_utc_date_time().year);

    let report = check_palette_year(&palette, coords, year, args.step_minutes, &model)
        .map_err(|e| e.to_string())?;
    if !report.all_passed {
        return Err(format!(
            "palette '{}' failed WCAG contrast check ({})",
            report.palette_name,
            format_worst_moment("worst moment", &report.worst_overall)
        ));
    }
    Ok(format_palette_check_report(&report))
}

fn execute_hook_install(
    shell: HookShell,
    write_rc: bool,
    rc_override: Option<PathBuf>,
) -> Result<String, String> {
    let rc_path = match rc_override {
        Some(path) => path,
        None => shell.default_rc_path()?,
    };
    if !write_rc {
        let snippet = preview_rc_block(shell);
        return Err(format!(
            "refusing to edit {} without `--write-rc`; pass `{} hook {shell} install --write-rc` or add manually:\n\n{snippet}",
            rc_path.display(),
            product::CLI_BIN
        ));
    }
    install_hook_to_rc(shell, &rc_path)
}

fn execute_hook_uninstall(
    shell: HookShell,
    rc_override: Option<PathBuf>,
) -> Result<String, String> {
    let rc_path = match rc_override {
        Some(path) => path,
        None => shell.default_rc_path()?,
    };
    uninstall_hook_from_rc(shell, &rc_path)
}

fn handle_shell_tail(shell: HookShell, tail: HookShellTailArgs) -> Result<String, String> {
    match tail.action {
        None => Ok(render_hook_script(shell)),
        Some(HookShellAction::Install(flags)) => {
            execute_hook_install(shell, flags.write_rc, flags.rc_file)
        }
        Some(HookShellAction::Uninstall(flags)) => {
            let _ = flags.write_rc;
            execute_hook_uninstall(shell, flags.rc_file)
        }
    }
}

fn run_hook(args: HookArgs) -> Result<String, String> {
    match args.command {
        HookCommand::Zsh(tail) => handle_shell_tail(HookShell::Zsh, tail),
        HookCommand::Bash(tail) => handle_shell_tail(HookShell::Bash, tail),
        HookCommand::Fish(tail) => handle_shell_tail(HookShell::Fish, tail),
        HookCommand::Install(inst) => {
            let shell = match inst.shell.or(inst.shell_flag) {
                Some(sh) => sh,
                None => HookShell::detect_from_env()?,
            };
            execute_hook_install(shell, inst.write_rc, inst.rc_file)
        }
        HookCommand::Uninstall(uninst) => {
            let _ = uninst.write_rc;
            let shell = match uninst.shell.or(uninst.shell_flag) {
                Some(sh) => sh,
                None => HookShell::detect_from_env()?,
            };
            execute_hook_uninstall(shell, uninst.rc_file)
        }
    }
}

fn run_pause(args: PauseArgs) -> Result<String, StoreError> {
    let paths = ShikiPaths::resolve()?;
    let now_utc = SystemClock.now();
    let msg = match args.duration {
        Some(raw) => {
            let secs = parse_pause_duration_secs(&raw)?;
            let state = set_pause(&paths, Some(secs), now_utc)?;
            let until = state.paused_until.as_deref().unwrap_or("unknown");
            format!("paused drift until {until} ({})\n", raw.trim())
        }
        None => {
            let _ = set_pause(&paths, None, now_utc)?;
            format!(
                "paused drift indefinitely (run `{} resume` to resume)\n",
                product::CLI_BIN
            )
        }
    };
    let _ = notify_running_daemon(&paths);
    Ok(msg)
}

fn run_resume() -> Result<String, StoreError> {
    let paths = ShikiPaths::resolve()?;
    let _ = clear_pause(&paths)?;
    let _ = notify_running_daemon(&paths);
    Ok("resumed drift\n".to_string())
}

fn resolve_daemon_platform(arg: Option<DaemonPlatformArg>) -> PlatformKind {
    arg.map_or_else(PlatformKind::current, DaemonPlatformArg::into_platform_kind)
}

fn run_daemon(args: DaemonArgs) -> Result<String, String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    match args.command {
        DaemonCommand::Install(inst) => {
            let platform = resolve_daemon_platform(inst.platform);
            let unit_path = match inst.unit_path {
                Some(path) => path,
                None => default_user_unit_path(platform)?,
            };
            let exe_path = match inst.exe_path {
                Some(path) => path,
                None => std::env::current_exe()
                    .map_err(|e| format!("unable to determine current executable path: {e}"))?,
            };
            let activate = should_activate_service_manager(inst.no_activate);
            let mut mgr = HostServiceManager;
            install_daemon_unit(platform, &unit_path, &exe_path, activate, &mut mgr)
        }
        DaemonCommand::Uninstall(uninst) => {
            let platform = resolve_daemon_platform(uninst.platform);
            let unit_path = match uninst.unit_path {
                Some(path) => path,
                None => default_user_unit_path(platform)?,
            };
            let activate = should_activate_service_manager(uninst.no_activate);
            let mut mgr = HostServiceManager;
            uninstall_daemon_unit(&paths, platform, &unit_path, activate, &mut mgr)
        }
        DaemonCommand::Status(status_args) => {
            let platform = resolve_daemon_platform(status_args.platform);
            let unit_path = match status_args.unit_path {
                Some(path) => path,
                None => default_user_unit_path(platform)?,
            };
            let report = collect_daemon_status(&paths, platform, &unit_path, SystemClock.now())?;
            if status_args.json {
                let mut json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
                json.push('\n');
                Ok(json)
            } else {
                Ok(format_daemon_status_text(&report))
            }
        }
        DaemonCommand::Run => {
            run_foreground_daemon(&paths)?;
            Ok(String::new())
        }
    }
}

fn run_emit(args: EmitArgs) -> Result<(), String> {
    let paths = ShikiPaths::resolve().map_err(|e| e.to_string())?;
    refresh_hook_store(
        &paths,
        args.if_stale,
        args.time.as_deref(),
        SystemClock.now(),
        encode_osc_frame,
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn print_result<E: std::fmt::Display>(res: Result<String, E>) -> ExitCode {
    match res {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => ExitCode::SUCCESS,
        Some(Commands::Now(args)) => print_result(run_now(args)),
        Some(Commands::Apply(args)) => match run_apply(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
        Some(Commands::Reset) => match run_reset() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
        Some(Commands::Preview(args)) => print_result(run_preview(args)),
        Some(Commands::Pause(args)) => print_result(run_pause(args)),
        Some(Commands::Resume) => print_result(run_resume()),
        Some(Commands::Config(args)) => print_result(run_config(args)),
        Some(Commands::Locate(args)) => print_result(run_locate(args)),
        Some(Commands::Palette(palette_args)) => match palette_args.command {
            PaletteCommand::Check(args) => print_result(run_palette_check(args)),
        },
        Some(Commands::Hook(args)) => print_result(run_hook(args)),
        Some(Commands::Daemon(args)) => print_result(run_daemon(args)),
        Some(Commands::Emit(args)) => match run_emit(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
    }
}
