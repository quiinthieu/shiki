//! Test and verification fixture loader for `tests/fixtures/{solar,themes,palettes}/`.

use crate::error::StoreError;
use crate::paths::ShikiPaths;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FixtureKind {
    Solar,
    Themes,
    Palettes,
}

impl FixtureKind {
    #[must_use]
    pub const fn subdir(self) -> &'static str {
        match self {
            Self::Solar => "solar",
            Self::Themes => "themes",
            Self::Palettes => "palettes",
        }
    }
}

fn validate_fixture_filename(name: &str) -> Result<(), StoreError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(StoreError::InvalidFixtureName(name.to_string()));
    }

    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(StoreError::InvalidFixtureName(name.to_string())),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureDir {
    root: PathBuf,
}

impl FixtureDir {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, kind: FixtureKind, name: &str) -> Result<PathBuf, StoreError> {
        validate_fixture_filename(name)?;
        Ok(self.root.join(kind.subdir()).join(name))
    }

    pub fn solar_path(&self, name: &str) -> Result<PathBuf, StoreError> {
        self.path_for(FixtureKind::Solar, name)
    }

    pub fn theme_path(&self, name: &str) -> Result<PathBuf, StoreError> {
        self.path_for(FixtureKind::Themes, name)
    }

    pub fn palette_path(&self, name: &str) -> Result<PathBuf, StoreError> {
        self.path_for(FixtureKind::Palettes, name)
    }

    fn read_kind(&self, kind: FixtureKind, name: &str) -> Result<String, StoreError> {
        let path = self.path_for(kind, name)?;
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(contents),
            Err(err) if err.kind() == ErrorKind::NotFound => Err(StoreError::FixtureNotFound {
                kind: kind.subdir(),
                name: name.to_string(),
                path,
            }),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    pub fn read_solar(&self, name: &str) -> Result<String, StoreError> {
        self.read_kind(FixtureKind::Solar, name)
    }

    pub fn read_theme(&self, name: &str) -> Result<String, StoreError> {
        self.read_kind(FixtureKind::Themes, name)
    }

    pub fn read_palette(&self, name: &str) -> Result<String, StoreError> {
        self.read_kind(FixtureKind::Palettes, name)
    }

    pub fn stage_palette(&self, name: &str, paths: &ShikiPaths) -> Result<PathBuf, StoreError> {
        let src = self.palette_path(name)?;
        let palettes_dir = paths.palettes_dir();
        fs::create_dir_all(&palettes_dir)?;
        let dst = palettes_dir.join(name);
        match fs::copy(&src, &dst) {
            Ok(_) => Ok(dst),
            Err(err) if err.kind() == ErrorKind::NotFound => Err(StoreError::FixtureNotFound {
                kind: FixtureKind::Palettes.subdir(),
                name: name.to_string(),
                path: src,
            }),
            Err(err) => Err(StoreError::Io(err)),
        }
    }
}
