//! The machine's canonical device identity: `~/.qontinui/machine.json`.
//!
//! **One physical machine has exactly ONE `device_id`, for its lifetime.** It
//! is minted once (first launch, by [`crate::pair::ensure_device_initialized`])
//! and looked up — never re-minted — by everything else. Coord UPSERTs
//! `ON CONFLICT (device_id)`, so re-presenting the stored id is what makes
//! pairing and registration idempotent; presenting a fresh one grows a new
//! `coord.devices` row per attempt for the same box.
//!
//! This module exists so that reader has ONE home. It is declared in both
//! `lib.rs` and `main.rs` because [`crate::auth`] compiles into both crates
//! while [`crate::pair`] is lib-only, and `auth.rs` must be able to consult
//! the canonical identity before falling back to its own encrypted cache.
//! Plan `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// On-disk shape of `~/.qontinui/machine.json` — only the field every reader
/// needs. The full file carries more keys (`hostname`, `name`,
/// `active_tenant_id`); extras are tolerated and, on the write paths,
/// preserved verbatim.
///
/// The wire field name is `device_id`; pre-rename files spell it `machine_id`
/// (see `DeviceFile` in `bin/qontinui_profile.rs`). The serde alias accepts
/// both spellings, so a legacy file loads without migration.
#[derive(Debug, Deserialize)]
struct MachineFile {
    #[serde(alias = "machine_id")]
    device_id: String,
}

/// Path to the per-device identity file. Resolved from the OS home directory
/// unconditionally — there is deliberately **no env override**, so a
/// supervisor-spawned temp runner shares the primary's identity rather than
/// minting a second one.
pub fn machine_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

/// Read the stored `device_id` from an explicit `machine.json` path.
///
/// **This function NEVER mints.** A missing file is a hard error, not a cue to
/// generate an identity: minting on read would hand coord a brand-new
/// `device_id` on every pairing attempt. The only mint site in the runner is
/// `pair::ensure_device_initialized_at`'s absent-file branch.
pub fn read_device_id_at(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Err(format!(
            "device not initialized — run `qontinui_profile device init` first \
             (no {} on disk)",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let parsed: MachineFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let id = parsed.device_id.trim().to_string();
    if id.is_empty() {
        return Err(format!(
            "{} has an empty device_id — inspect it, or `rm` it and re-run \
             `qontinui_profile device init`",
            path.display()
        ));
    }
    Ok(id)
}

/// [`read_device_id_at`] against the real `~/.qontinui/machine.json`.
pub fn read_device_id() -> Result<String, String> {
    let path = machine_file_path().ok_or_else(|| "could not resolve home directory".to_string())?;
    read_device_id_at(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_canonical_and_legacy_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("machine.json");
        std::fs::write(&canonical, br#"{"device_id":"abc","hostname":"h"}"#).unwrap();
        assert_eq!(read_device_id_at(&canonical).unwrap(), "abc");

        let legacy = dir.path().join("legacy.json");
        std::fs::write(&legacy, br#"{"machine_id":"def","hostname":"h"}"#).unwrap();
        assert_eq!(read_device_id_at(&legacy).unwrap(), "def");
    }

    #[test]
    fn absent_file_errors_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let err = read_device_id_at(&path).expect_err("absent must error");
        assert!(err.contains("device not initialized"), "got: {err}");
        assert!(!path.exists(), "a read must never create the identity file");
    }

    #[test]
    fn blank_and_corrupt_ids_error() {
        let dir = tempfile::tempdir().unwrap();
        let blank = dir.path().join("blank.json");
        std::fs::write(&blank, br#"{"device_id":"  "}"#).unwrap();
        assert!(read_device_id_at(&blank)
            .expect_err("blank must error")
            .contains("empty device_id"));

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();
        assert!(read_device_id_at(&corrupt).is_err());
    }
}
