use clap::{Args, Parser, Subcommand};
use sk_core::location::{ManualLocation, ResolvedLocation};
use sk_core::palette::{check_palette_year, ModelConfig, PaletteCheckReport, WorstMoment};
use sk_core::product;
use sk_core::solar::Coordinates;
use sk_core::time::Clock;
use sk_store::clock::SystemClock;
use sk_store::config::{resolve_location, set_manual_location, set_timezone_location};
use sk_store::error::StoreError;
use sk_store::palette::load_palette;
use sk_store::paths::ShikiPaths;
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
    /// Inspect or configure location (timezone-derived or manual coordinates)
    Locate(LocateArgs),
    /// Manage and verify terminal colour palettes
    Palette(PaletteArgs),
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
        set_manual_location(&paths, manual)?
    } else if args.timezone {
        set_timezone_location(&paths)?
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => ExitCode::SUCCESS,
        Some(Commands::Locate(args)) => match run_locate(args) {
            Ok(out) => {
                print!("{out}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
        Some(Commands::Palette(palette_args)) => match palette_args.command {
            PaletteCommand::Check(args) => match run_palette_check(args) {
                Ok(out) => {
                    print!("{out}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    ExitCode::FAILURE
                }
            },
        },
    }
}
