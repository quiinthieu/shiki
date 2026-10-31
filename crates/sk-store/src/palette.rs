//! Palette loading from bundled palettes, `$SHIKI_HOME/palettes/<name>.toml`, or explicit file
//! paths (`PLAN.md` §3, §6, T06).

use crate::error::StoreError;
use crate::paths::ShikiPaths;
use sk_core::palette::Palette;
use std::fs;
use std::path::Path;

fn looks_like_file_path(spec: &str) -> bool {
    spec.contains('/')
        || spec.contains('\\')
        || spec.ends_with(".toml")
        || Path::new(spec).is_file()
}

/// Loads a [`Palette`] from an explicit `.toml` file path, attaching `path` to any file/line/column
/// diagnostics.
pub fn load_palette_file(path: &Path) -> Result<Palette, StoreError> {
    let contents = fs::read_to_string(path)?;
    let source_name = path.display().to_string();
    Ok(Palette::from_toml_with_source(&contents, &source_name)?)
}

/// Resolves `spec` as either a filesystem path (if it contains a path separator or ends in `.toml`),
/// a user palette `<spec>.toml` inside `paths.palettes_dir()`, or a bundled palette (`shiki`).
///
/// An invalid user palette file never falls back silently (`PLAN.md` §6).
pub fn load_palette(paths: &ShikiPaths, spec: &str) -> Result<Palette, StoreError> {
    let trimmed = spec.trim();
    if looks_like_file_path(trimmed) {
        return load_palette_file(Path::new(trimmed));
    }

    let user_candidate = paths.palettes_dir().join(format!("{trimmed}.toml"));
    if user_candidate.exists() {
        return load_palette_file(&user_candidate);
    }

    if let Some(bundled) = Palette::bundled(trimmed) {
        return Ok(bundled);
    }

    Err(StoreError::PaletteNotFound {
        name: trimmed.to_string(),
        searched_path: user_candidate,
    })
}
