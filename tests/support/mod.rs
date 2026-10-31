//! Shared integration test harness (`TestEnv` and fixed-instant helpers).
//!
//! Isolates `HOME`, `SHIKI_HOME`, and XDG base directories inside a disposable `TempDir` so tests
//! never touch the real `$HOME`, shell rc files, or terminal configuration files (`PLAN.md` §0 #6).

#![allow(dead_code)]

use sk_core::time::{FakeClock, UtcInstant};
use sk_store::fixtures::FixtureDir;
use sk_store::paths::{PathEnv, PlatformKind, ShikiPaths};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TestEnv {
    _temp: TempDir,
    root_dir: PathBuf,
    home_dir: PathBuf,
    shiki_home_dir: PathBuf,
    xdg_config_home: PathBuf,
    xdg_state_home: PathBuf,
    xdg_data_home: PathBuf,
    xdg_runtime_dir: PathBuf,
    paths: ShikiPaths,
    fixtures: FixtureDir,
}

impl TestEnv {
    #[must_use]
    pub fn new() -> Self {
        let temp = TempDir::new().expect("create isolated TempDir for TestEnv");
        let root_dir = temp.path().to_path_buf();
        let home_dir = root_dir.join("home");
        let shiki_home_dir = root_dir.join("shiki-home");
        let xdg_config_home = root_dir.join("xdg-config");
        let xdg_state_home = root_dir.join("xdg-state");
        let xdg_data_home = root_dir.join("xdg-data");
        let xdg_runtime_dir = root_dir.join("xdg-runtime");

        for dir in [
            &home_dir,
            &shiki_home_dir,
            &xdg_config_home,
            &xdg_state_home,
            &xdg_data_home,
            &xdg_runtime_dir,
        ] {
            fs::create_dir_all(dir).expect("create isolated TestEnv directory");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&xdg_runtime_dir, fs::Permissions::from_mode(0o700))
                .expect("set 0700 mode on isolated XDG_RUNTIME_DIR");
        }

        let path_env = PathEnv {
            home: Some(home_dir.clone()),
            shiki_home: Some(shiki_home_dir.clone()),
            xdg_config_home: Some(xdg_config_home.clone()),
            xdg_state_home: Some(xdg_state_home.clone()),
            xdg_runtime_dir: Some(xdg_runtime_dir.clone()),
        };
        let paths = ShikiPaths::resolve_with(PlatformKind::current(), &path_env)
            .expect("resolve isolated TestEnv ShikiPaths");
        paths
            .ensure_dirs()
            .expect("ensure isolated ShikiPaths directories");

        let fixtures = FixtureDir::new(workspace_root().join("tests").join("fixtures"));

        Self {
            _temp: temp,
            root_dir,
            home_dir,
            shiki_home_dir,
            xdg_config_home,
            xdg_state_home,
            xdg_data_home,
            xdg_runtime_dir,
            paths,
            fixtures,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root_dir
    }

    #[must_use]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    #[must_use]
    pub fn shiki_home(&self) -> &Path {
        &self.shiki_home_dir
    }

    #[must_use]
    pub fn xdg_config_home(&self) -> &Path {
        &self.xdg_config_home
    }

    #[must_use]
    pub fn xdg_state_home(&self) -> &Path {
        &self.xdg_state_home
    }

    #[must_use]
    pub fn xdg_data_home(&self) -> &Path {
        &self.xdg_data_home
    }

    #[must_use]
    pub fn xdg_runtime_dir(&self) -> &Path {
        &self.xdg_runtime_dir
    }

    #[must_use]
    pub fn paths(&self) -> &ShikiPaths {
        &self.paths
    }

    #[must_use]
    pub fn fixtures(&self) -> &FixtureDir {
        &self.fixtures
    }

    /// Environment variable pairs for child processes so they remain strictly inside `TestEnv`.
    #[must_use]
    pub fn env_vars(&self) -> [(&'static str, &Path); 6] {
        [
            ("HOME", &self.home_dir),
            ("SHIKI_HOME", &self.shiki_home_dir),
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("XDG_DATA_HOME", &self.xdg_data_home),
            ("XDG_RUNTIME_DIR", &self.xdg_runtime_dir),
        ]
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate manifest must live under crates/<pkg>")
        .to_path_buf()
}

#[must_use]
pub fn fixed_utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> UtcInstant {
    UtcInstant::utc_ymd_hms(year, month, day, hour, minute, second)
}

#[must_use]
pub fn fixed_rfc3339(input: &str) -> UtcInstant {
    UtcInstant::parse_rfc3339(input).expect("valid fixed RFC 3339 timestamp")
}

#[must_use]
pub fn fake_clock_utc(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> FakeClock {
    FakeClock::from_utc_ymd_hms(year, month, day, hour, minute, second)
}
