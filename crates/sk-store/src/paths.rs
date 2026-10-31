//! Platform-aware filesystem layout and `SHIKI_HOME` override resolution (`PLAN.md` §6).

use crate::error::StoreError;
use directories::BaseDirs;
use sk_core::product;
use std::fs;
use std::path::{Path, PathBuf};

pub const SHIKI_HOME_ENV: &str = "SHIKI_HOME";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PlatformKind {
    MacOs,
    Linux,
}

impl PlatformKind {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathEnv {
    pub home: Option<PathBuf>,
    pub shiki_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShikiPaths {
    config_dir: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
}

fn non_empty_path(path: &Option<PathBuf>) -> Option<&Path> {
    path.as_deref().filter(|p| !p.as_os_str().is_empty())
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

impl ShikiPaths {
    #[must_use]
    pub fn from_shiki_home(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_dir: root.clone(),
            state_dir: root.clone(),
            runtime_dir: root,
        }
    }

    pub fn resolve() -> Result<Self, StoreError> {
        let home = non_empty_env_path("HOME")
            .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()));
        let env = PathEnv {
            home,
            shiki_home: non_empty_env_path(SHIKI_HOME_ENV),
            xdg_config_home: non_empty_env_path("XDG_CONFIG_HOME"),
            xdg_state_home: non_empty_env_path("XDG_STATE_HOME"),
            xdg_runtime_dir: non_empty_env_path("XDG_RUNTIME_DIR"),
        };
        Self::resolve_with(PlatformKind::current(), &env)
    }

    pub fn resolve_with(platform: PlatformKind, env: &PathEnv) -> Result<Self, StoreError> {
        if let Some(shiki_home) = non_empty_path(&env.shiki_home) {
            return Ok(Self::from_shiki_home(shiki_home));
        }

        let home = non_empty_path(&env.home).ok_or(StoreError::MissingHomeDirectory)?;

        match platform {
            PlatformKind::MacOs => {
                let app_support = home
                    .join("Library")
                    .join("Application Support")
                    .join(product::DISPLAY_NAME);
                Ok(Self {
                    config_dir: app_support.clone(),
                    state_dir: app_support.clone(),
                    runtime_dir: app_support,
                })
            }
            PlatformKind::Linux => {
                let config_base = non_empty_path(&env.xdg_config_home)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| home.join(".config"));
                let state_base = non_empty_path(&env.xdg_state_home)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| home.join(".local").join("state"));

                let config_dir = config_base.join(product::CLI_BIN);
                let state_dir = state_base.join(product::CLI_BIN);
                let runtime_dir = match non_empty_path(&env.xdg_runtime_dir) {
                    Some(rt) => rt.join(product::CLI_BIN),
                    None => state_dir.clone(),
                };

                Ok(Self {
                    config_dir,
                    state_dir,
                    runtime_dir,
                })
            }
        }
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    #[must_use]
    pub fn palettes_dir(&self) -> PathBuf {
        self.config_dir.join("palettes")
    }

    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    #[must_use]
    pub fn snapshot_file(&self) -> PathBuf {
        self.state_dir.join("snapshot.json")
    }

    #[must_use]
    pub fn current_seq_file(&self) -> PathBuf {
        self.runtime_dir.join("current.seq")
    }

    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("shiki.lock")
    }

    pub fn ensure_dirs(&self) -> Result<(), StoreError> {
        for dir in [
            self.config_dir.as_path(),
            self.palettes_dir().as_path(),
            self.state_dir.as_path(),
            self.runtime_dir.as_path(),
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
