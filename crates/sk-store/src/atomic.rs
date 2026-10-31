//! Atomic file persistence (`temp -> fsync -> rename`, `PLAN.md` §0 #6 & §6).

use crate::error::StoreError;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_temp_path(parent: &Path, base_name: &str) -> PathBuf {
    let pid = std::process::id();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{base_name}.tmp.{pid}.{seq}"))
}

/// Atomically writes `bytes` to `target` via a sibling temporary file, `sync_all()` (`fsync`),
/// and `rename()`, enforcing `0o600` owner-only permissions on Unix.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write target path must have a parent directory",
        )
    })?;
    let base_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("shiki");

    fs::create_dir_all(parent)?;

    let (mut file, tmp_path) = loop {
        let candidate = next_temp_path(parent, base_name);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        match opts.open(&candidate) {
            Ok(f) => break (f, candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(StoreError::Io(err)),
        }
    };

    let write_and_sync = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    drop(file);

    if let Err(err) = write_and_sync {
        let _ = fs::remove_file(&tmp_path);
        return Err(StoreError::Io(err));
    }

    if let Err(err) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(StoreError::Io(err));
    }

    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}
