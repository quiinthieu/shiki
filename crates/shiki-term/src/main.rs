use clap::{Args, Parser, Subcommand};
use sk_core::location::{ManualLocation, ResolvedLocation};
use sk_core::product;
use sk_store::config::{resolve_location, set_manual_location, set_timezone_location};
use sk_store::error::StoreError;
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
    }
}
