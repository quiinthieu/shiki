//! Inline terminal preview renderer (`swatches + sample text`, 256-colour fallback, `NO_COLOR`).
//!
//! Safety (`PLAN.md` §10 T07 AC): uses only SGR (`CSI ... m`) attributes and **never** emits
//! palette-mutating OSC 4 / 10 / 11 / 12 sequences.

use clap::ValueEnum;
use sk_core::color::{rgb_to_xterm256, Rgb8};
use sk_deliver::osc::contains_palette_mutating_osc;
use sk_store::store::Snapshot;
use std::fmt::Write;

const SGR_RESET: &str = "\x1b[0m";
const SAMPLE_PANEL_WIDTH: usize = 66;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorChoiceArg {
    #[default]
    Auto,
    Truecolor,
    #[value(name = "256", alias = "ansi256")]
    Ansi256,
    None,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PreviewColorMode {
    TrueColor,
    Ansi256,
    NoColor,
}

impl PreviewColorMode {
    #[must_use]
    pub fn resolve_from_env(choice: ColorChoiceArg) -> Self {
        let no_color = std::env::var("NO_COLOR").ok();
        let colorterm = std::env::var("COLORTERM").ok();
        let term = std::env::var("TERM").ok();
        Self::resolve(
            choice,
            no_color.as_deref(),
            colorterm.as_deref(),
            term.as_deref(),
        )
    }

    #[must_use]
    pub fn resolve(
        choice: ColorChoiceArg,
        no_color: Option<&str>,
        colorterm: Option<&str>,
        term: Option<&str>,
    ) -> Self {
        match choice {
            ColorChoiceArg::None => Self::NoColor,
            ColorChoiceArg::Truecolor => Self::TrueColor,
            ColorChoiceArg::Ansi256 => Self::Ansi256,
            ColorChoiceArg::Auto => {
                // https://no-color.org/: when present and non-empty, suppress ANSI colour escapes.
                if no_color.is_some_and(|val| !val.is_empty()) {
                    return Self::NoColor;
                }
                if term.is_some_and(|t| t.eq_ignore_ascii_case("dumb")) {
                    return Self::NoColor;
                }
                let ct_true = colorterm.is_some_and(|ct| {
                    ct.eq_ignore_ascii_case("truecolor") || ct.eq_ignore_ascii_case("24bit")
                });
                let term_true = term.is_some_and(|t| {
                    let lower = t.to_ascii_lowercase();
                    lower.contains("truecolor")
                        || lower.contains("24bit")
                        || lower.contains("direct")
                });
                if ct_true || term_true {
                    Self::TrueColor
                } else {
                    Self::Ansi256
                }
            }
        }
    }

    fn sgr_fg(self, rgb: Rgb8) -> String {
        match self {
            Self::TrueColor => format!("\x1b[38;2;{};{};{}m", rgb.r, rgb.g, rgb.b),
            Self::Ansi256 => format!("\x1b[38;5;{}m", rgb_to_xterm256(rgb)),
            Self::NoColor => String::new(),
        }
    }

    fn sgr_bg(self, rgb: Rgb8) -> String {
        match self {
            Self::TrueColor => format!("\x1b[48;2;{};{};{}m", rgb.r, rgb.g, rgb.b),
            Self::Ansi256 => format!("\x1b[48;5;{}m", rgb_to_xterm256(rgb)),
            Self::NoColor => String::new(),
        }
    }

    fn swatch_chip(self, bg: Rgb8, fg: Rgb8, label: &str) -> String {
        match self {
            Self::NoColor => format!("[{label}]"),
            Self::TrueColor | Self::Ansi256 => {
                format!("{}{} {label} {SGR_RESET}", self.sgr_bg(bg), self.sgr_fg(fg))
            }
        }
    }
}

fn contrasting_chip_fg(bg: Rgb8) -> Rgb8 {
    let white = Rgb8::new(255, 255, 255);
    let black = Rgb8::new(0, 0, 0);
    if white.contrast_ratio(bg) >= black.contrast_ratio(bg) {
        white
    } else {
        black
    }
}

struct SampleSeg<'a> {
    text: &'a str,
    fg: Rgb8,
    bg: Rgb8,
}

fn push_sample_line(
    out: &mut String,
    mode: PreviewColorMode,
    default_bg: Rgb8,
    segs: &[SampleSeg<'_>],
) {
    let used_cols: usize = segs.iter().map(|s| s.text.chars().count()).sum();
    let pad_cols = SAMPLE_PANEL_WIDTH.saturating_sub(used_cols);

    match mode {
        PreviewColorMode::NoColor => {
            out.push_str("  | ");
            for seg in segs {
                out.push_str(seg.text);
            }
            let _ = writeln!(out, "{:pad_cols$} |", "");
        }
        PreviewColorMode::TrueColor | PreviewColorMode::Ansi256 => {
            out.push_str("  ");
            for seg in segs {
                let _ = write!(
                    out,
                    "{}{}{}",
                    mode.sgr_bg(seg.bg),
                    mode.sgr_fg(seg.fg),
                    seg.text
                );
            }
            if pad_cols > 0 {
                let _ = write!(out, "{}{:pad_cols$}", mode.sgr_bg(default_bg), "");
            }
            let _ = writeln!(out, "{SGR_RESET}");
        }
    }
}

/// Renders an inline terminal preview (metadata, base & ANSI 0–15 swatches, and sample code/shell
/// panel) without emitting any OSC sequences.
#[must_use]
pub fn render_preview(snapshot: &Snapshot, mode: PreviewColorMode) -> String {
    let mut out = String::with_capacity(2048);
    let c = &snapshot.colors;

    let next_ev = match &snapshot.next_event {
        Some(ev) => format!("{} at {}", ev.kind, ev.instant_rfc3339),
        None => format!("none ({:?})", snapshot.polar_state).to_ascii_lowercase(),
    };

    let _ = writeln!(
        out,
        "palette: {}  instant: {}",
        snapshot.palette, snapshot.instant_rfc3339
    );
    let _ = writeln!(
        out,
        "phase: {} ({}, d={:.3}, elev={:+.1} deg)  season: {} (s={:.3})",
        snapshot.solar_phase,
        snapshot.day_half,
        snapshot.day_phase,
        snapshot.elevation_deg,
        snapshot.season,
        snapshot.season_phase,
    );
    let _ = writeln!(
        out,
        "next event: {next_ev}  contrast: fg/bg {:.2}:1, sel {:.2}:1, min ansi {:.2}:1",
        snapshot.contrast.fg_bg_ratio,
        snapshot.contrast.selection_ratio,
        snapshot.contrast.min_ansi_bg_ratio,
    );

    out.push('\n');
    out.push_str("swatches:\n");
    let _ = writeln!(
        out,
        "  base: bg {} {}  fg {} {}  cursor {} {}  sel {} {}",
        mode.swatch_chip(c.bg, c.fg, "bg"),
        c.bg,
        mode.swatch_chip(c.fg, c.bg, "fg"),
        c.fg,
        mode.swatch_chip(c.cursor, contrasting_chip_fg(c.cursor), "cur"),
        c.cursor,
        mode.swatch_chip(c.selection_bg, c.selection_fg, "sel"),
        c.selection_bg,
    );

    for (row_label, range) in [("00-07", 0..8_usize), ("08-15", 8..16_usize)] {
        let _ = write!(out, "  ansi {row_label}:");
        for idx in range {
            let rgb = c.ansi[idx];
            let chip_label = format!("{idx:02}");
            let chip = mode.swatch_chip(rgb, contrasting_chip_fg(rgb), &chip_label);
            let _ = write!(out, " {chip} {rgb}");
        }
        out.push('\n');
    }

    out.push('\n');
    out.push_str("sample:\n");
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[SampleSeg {
            text: " ",
            fg: c.fg,
            bg: c.bg,
        }],
    );
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[
            SampleSeg {
                text: "  ~/workspace ",
                fg: c.ansi[4],
                bg: c.bg,
            },
            SampleSeg {
                text: "git:(main) ",
                fg: c.ansi[6],
                bg: c.bg,
            },
            SampleSeg {
                text: "$ ",
                fg: c.ansi[2],
                bg: c.bg,
            },
            SampleSeg {
                text: "cargo test --workspace",
                fg: c.fg,
                bg: c.bg,
            },
            SampleSeg {
                text: " ",
                fg: c.bg,
                bg: c.cursor,
            },
        ],
    );
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[SampleSeg {
            text: "  // Continuous 2-D OKLCH drift with WCAG 2.2 contrast guarantees",
            fg: c.ansi[8],
            bg: c.bg,
        }],
    );
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[
            SampleSeg {
                text: "  fn ",
                fg: c.ansi[5],
                bg: c.bg,
            },
            SampleSeg {
                text: "blend_sky",
                fg: c.ansi[4],
                bg: c.bg,
            },
            SampleSeg {
                text: "(d: ",
                fg: c.fg,
                bg: c.bg,
            },
            SampleSeg {
                text: "f64",
                fg: c.ansi[6],
                bg: c.bg,
            },
            SampleSeg {
                text: ") -> &str { ",
                fg: c.fg,
                bg: c.bg,
            },
            SampleSeg {
                text: "\"golden_hour\"",
                fg: c.ansi[2],
                bg: c.bg,
            },
            SampleSeg {
                text: " }",
                fg: c.fg,
                bg: c.bg,
            },
        ],
    );
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[
            SampleSeg {
                text: "  status: ",
                fg: c.fg,
                bg: c.bg,
            },
            SampleSeg {
                text: "ok ",
                fg: c.ansi[2],
                bg: c.bg,
            },
            SampleSeg {
                text: "warn(0) ",
                fg: c.ansi[3],
                bg: c.bg,
            },
            SampleSeg {
                text: "err(0) ",
                fg: c.ansi[1],
                bg: c.bg,
            },
            SampleSeg {
                text: " selected span ",
                fg: c.selection_fg,
                bg: c.selection_bg,
            },
        ],
    );
    push_sample_line(
        &mut out,
        mode,
        c.bg,
        &[SampleSeg {
            text: " ",
            fg: c.fg,
            bg: c.bg,
        }],
    );

    assert!(
        !contains_palette_mutating_osc(out.as_bytes()),
        "preview output must never contain palette-mutating OSC sequences"
    );
    out
}
