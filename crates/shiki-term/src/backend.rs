//! `backend list|enable|disable` CLI configuration and `$TMUX` environment helpers
//! (`PLAN.md` §5, §6, §10 T10).

use clap::ValueEnum;
use serde::Serialize;
use sk_store::config::{load_config, save_config, AppConfig};
use sk_store::paths::ShikiPaths;
use std::env;

/// Opt-in delivery backend targets configurable via `backend enable|disable`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackendTarget {
    /// Broadcast each frame to every pseudo-terminal owned by the effective UID (`/dev/pts/*`, `/dev/ttys*`).
    #[value(name = "tty-broadcast", alias = "tty_broadcast")]
    TtyBroadcast,
    /// Deliver per-pane OSC palettes (and optional DCS outer passthrough) to live `tmux` panes.
    #[value(name = "tmux")]
    Tmux,
}

impl BackendTarget {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TtyBroadcast => "tty-broadcast",
            Self::Tmux => "tmux",
        }
    }
}

/// Coordinate-free status entry for a single delivery backend (`backend list [--json]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendEntryReport {
    pub name: &'static str,
    pub enabled: bool,
    pub always_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<bool>,
}

/// Coordinate-free `backend list --json` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendListReport {
    pub backends: Vec<BackendEntryReport>,
}

#[must_use]
pub fn is_tmux_backend_active(config: &AppConfig) -> bool {
    config.effective_tmux() || config.effective_tmux_passthrough()
}

/// Returns `true` when the current process is executing inside an active `tmux` pane (`$TMUX` set
/// and non-empty).
#[must_use]
pub fn is_inside_tmux_env() -> bool {
    env::var_os("TMUX").is_some_and(|val| !val.is_empty())
}

pub fn collect_backend_list(paths: &ShikiPaths) -> Result<BackendListReport, String> {
    let config = load_config(paths).map_err(|e| e.to_string())?;
    Ok(BackendListReport {
        backends: vec![
            BackendEntryReport {
                name: "osc",
                enabled: true,
                always_available: true,
                passthrough: None,
            },
            BackendEntryReport {
                name: "tty-broadcast",
                enabled: config.effective_tty_broadcast(),
                always_available: false,
                passthrough: None,
            },
            BackendEntryReport {
                name: "tmux",
                enabled: is_tmux_backend_active(&config),
                always_available: false,
                passthrough: Some(config.effective_tmux_passthrough()),
            },
        ],
    })
}

#[must_use]
pub fn format_backend_list_text(report: &BackendListReport) -> String {
    let mut out = String::new();
    for entry in &report.backends {
        if entry.always_available {
            out.push_str(&format!(
                "{}: always available (apply, reset, shell hook)\n",
                entry.name
            ));
            continue;
        }
        let state = if entry.enabled { "enabled" } else { "disabled" };
        match entry.name {
            "tty-broadcast" => {
                out.push_str(&format!(
                    "tty-broadcast: {state} (delivery.tty_broadcast = {})\n",
                    entry.enabled
                ));
            }
            "tmux" => {
                let passthrough = entry.passthrough.unwrap_or(false);
                out.push_str(&format!(
                    "tmux: {state} (delivery.tmux = {}, delivery.tmux_passthrough = {passthrough})\n",
                    entry.enabled
                ));
            }
            other => {
                out.push_str(&format!("{other}: {state}\n"));
            }
        }
    }
    out
}

pub fn enable_backend(
    paths: &ShikiPaths,
    target: BackendTarget,
    passthrough_override: Option<bool>,
) -> Result<String, String> {
    if target != BackendTarget::Tmux && passthrough_override.is_some() {
        return Err(format!(
            "`--passthrough` / `--no-passthrough` applies only to the `tmux` backend (not `{}`)",
            target.as_str()
        ));
    }

    let mut config = load_config(paths).map_err(|e| e.to_string())?;
    let summary = match target {
        BackendTarget::TtyBroadcast => {
            config.delivery.tty_broadcast = Some(true);
            "enabled backend tty-broadcast (delivery.tty_broadcast = true)\n".to_string()
        }
        BackendTarget::Tmux => {
            config.delivery.tmux = Some(true);
            if let Some(pt) = passthrough_override {
                config.delivery.tmux_passthrough = Some(pt);
            }
            let effective_pt = config.effective_tmux_passthrough();
            format!(
                "enabled backend tmux (delivery.tmux = true, delivery.tmux_passthrough = {effective_pt})\n"
            )
        }
    };
    save_config(paths, &config).map_err(|e| e.to_string())?;
    Ok(summary)
}

pub fn disable_backend(paths: &ShikiPaths, target: BackendTarget) -> Result<String, String> {
    let mut config = load_config(paths).map_err(|e| e.to_string())?;
    let summary = match target {
        BackendTarget::TtyBroadcast => {
            config.delivery.tty_broadcast = Some(false);
            "disabled backend tty-broadcast (delivery.tty_broadcast = false)\n".to_string()
        }
        BackendTarget::Tmux => {
            config.delivery.tmux = Some(false);
            config.delivery.tmux_passthrough = Some(false);
            "disabled backend tmux (delivery.tmux = false, delivery.tmux_passthrough = false)\n"
                .to_string()
        }
    };
    save_config(paths, &config).map_err(|e| e.to_string())?;
    Ok(summary)
}
