//! Stable per-user path for the bundled `wrappers_mcp` MCP server.
//!
//! AI client config files (Claude Desktop, Cursor, Cline, etc.) point at
//! one durable path that survives runner upgrades. At boot the runner
//! refreshes that copy from whichever build output the current binary
//! ships beside.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fs_atomic;

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("launcher not built. Run `cargo build --release --bin wrappers_mcp` and reload.")]
    DevBuildMissing,
    #[error("io: {0}")]
    IoError(#[from] io::Error),
}

const LAUNCHER_BIN: &str = if cfg!(windows) {
    "wrappers_mcp.exe"
} else {
    "wrappers_mcp"
};

/// Stable per-user path that AI client configs reference.
pub fn launcher_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner")
        .join("bin")
        .join(LAUNCHER_BIN)
}

/// Copy the bundled launcher into the stable path if missing or stale.
/// Returns the stable path either way.
pub fn ensure_launcher_installed() -> Result<PathBuf, LauncherError> {
    let target = launcher_path();
    let source = find_bundled_launcher()?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    if !needs_refresh(&source, &target)? {
        return Ok(target);
    }

    let bytes = fs::read(&source)?;
    fs_atomic::atomic_write(&target, &bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&target, perms)?;
    }

    Ok(target)
}

fn needs_refresh(source: &Path, target: &Path) -> io::Result<bool> {
    let target_meta = match fs::metadata(target) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };
    let source_meta = fs::metadata(source)?;

    if source_meta.len() != target_meta.len() {
        return Ok(true);
    }

    let source_mtime = source_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let target_mtime = target_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(source_mtime > target_mtime)
}

/// Locate the bundled `wrappers_mcp` binary. Packaged installs drop it
/// next to the runner exe; dev builds live under `target*/release/`.
fn find_bundled_launcher() -> Result<PathBuf, LauncherError> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(LAUNCHER_BIN);
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }

    if let Some(workspace_root) = workspace_root() {
        if let Some(found) = newest_target_release_match(&workspace_root)? {
            return Ok(found);
        }
    }

    Err(LauncherError::DevBuildMissing)
}

fn workspace_root() -> Option<PathBuf> {
    if let Some(env_dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
        let manifest = PathBuf::from(env_dir);
        if let Some(parent) = manifest.parent() {
            if parent.join("Cargo.toml").exists() {
                return Some(parent.to_path_buf());
            }
        }
        if manifest.join("Cargo.toml").exists() {
            return Some(manifest);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let mut cur = exe.as_path();
    while let Some(parent) = cur.parent() {
        if parent.join("Cargo.toml").is_file() && parent.join("src-tauri").is_dir() {
            return Some(parent.to_path_buf());
        }
        cur = parent;
    }
    None
}

fn newest_target_release_match(workspace_root: &Path) -> io::Result<Option<PathBuf>> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let entries = match fs::read_dir(workspace_root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("target") {
            continue;
        }
        let candidate = entry.path().join("release").join(LAUNCHER_BIN);
        let meta = match fs::metadata(&candidate) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        match &best {
            Some((best_mtime, _)) if *best_mtime >= mtime => {}
            _ => best = Some((mtime, candidate)),
        }
    }
    Ok(best.map(|(_, p)| p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn launcher_path_is_under_local_data() {
        let p = launcher_path();
        let expected_parent = dirs::data_local_dir()
            .unwrap()
            .join("qontinui-runner")
            .join("bin");
        assert_eq!(p.parent().unwrap(), expected_parent);
        assert_eq!(p.file_name().unwrap().to_string_lossy(), LAUNCHER_BIN);
    }

    fn install_with_sources(source: &Path, target: &Path) -> io::Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if !needs_refresh(source, target)? {
            return Ok(());
        }
        let bytes = fs::read(source)?;
        fs_atomic::atomic_write(target, &bytes)
    }

    #[test]
    fn install_is_idempotent_when_size_and_mtime_match() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src_bin");
        let target = dir.path().join("out").join("bin");
        fs::write(&source, b"payload-v1").unwrap();

        install_with_sources(&source, &target).unwrap();
        let first_mtime = fs::metadata(&target).unwrap().modified().unwrap();

        sleep(Duration::from_millis(20));
        install_with_sources(&source, &target).unwrap();
        let second_mtime = fs::metadata(&target).unwrap().modified().unwrap();

        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn install_overwrites_when_source_is_newer() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src_bin");
        let target = dir.path().join("out").join("bin");
        fs::write(&source, b"payload-v1").unwrap();
        install_with_sources(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"payload-v1");

        sleep(Duration::from_millis(50));
        fs::write(&source, b"payload-v2-longer").unwrap();

        install_with_sources(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"payload-v2-longer");
    }
}
