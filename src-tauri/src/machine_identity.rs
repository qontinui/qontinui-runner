//! The machine's canonical device identity: `~/.qontinui/machine.json`.
//!
//! **One physical machine has exactly ONE `device_id`, for its lifetime.** It
//! is minted once (first launch, by [`crate::pair::ensure_device_initialized`])
//! and looked up — never re-minted — by everything else. Coord UPSERTs
//! `ON CONFLICT (device_id)`, so re-presenting the stored id is what makes
//! pairing and registration idempotent; presenting a fresh one grows a new
//! `coord.devices` row per attempt for the same box.
//!
//! This module is the reader on the paths that can WRITE or MINT an identity —
//! [`crate::pair`], [`crate::auth`] and the `qontinui_profile device`
//! subcommand — which is where a duplicated reader actually costs a second
//! `coord.devices` row. It is owned by the lib and imported by the runner bin,
//! and `auth.rs` must be able to consult the canonical identity before falling
//! back to its own encrypted cache.
//!
//! The PATH and the PARSE both come from `qontinui_runner_lib::ambient` (plan
//! `2026-09-03-runner-tests-read-ambient-machine-state`): the dozen hand-rolled
//! `dirs::home_dir()/.qontinui/machine.json` readers this module used to list
//! as a follow-up are folded into that one seam, and a test that reaches it
//! without an `isolated_ambient()` guard panics naming the file. What this
//! module adds on top is the *never mint* contract and the operator-facing
//! error text of [`read_device_id_at`].
//!
//! `QONTINUI_HOME` overrides the path (`machine_file_path` honours it through
//! the seam), but it is no override the supervisor sets: a supervisor-spawned
//! temp runner inherits none, so it shares the primary's identity rather than
//! minting a second one.

use std::path::{Path, PathBuf};

/// Path to the per-device identity file — [`qontinui_runner_lib::ambient::machine_json_path`].
pub fn machine_file_path() -> Option<PathBuf> {
    qontinui_runner_lib::ambient::machine_json_path()
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
    let machine =
        qontinui_runner_lib::ambient::read_machine_json_at(path).map_err(|e| e.to_string())?;
    machine.device_id.ok_or_else(|| {
        format!(
            "{} has an empty device_id — inspect it, or `rm` it and re-run \
             `qontinui_profile device init`",
            path.display()
        )
    })
}

/// [`read_device_id_at`] against the real `~/.qontinui/machine.json`.
pub fn read_device_id() -> Result<String, String> {
    let path = machine_file_path().ok_or_else(|| "could not resolve home directory".to_string())?;
    read_device_id_at(&path)
}

/// The device id the device-JWT refresher's credential doors present:
/// `QONTINUI_MACHINE_ID` (the per-instance override) first, then
/// `machine.json`. `None` when neither resolves. Never mints.
///
/// One resolution for every step that names the device — the
/// pending-redeem poll, the redeem it triggers, and the anchor the secure
/// store keeps — because a code web bound to one id is refused for another.
///
/// Under `cfg(test)` it reads only [`set_test_device_id`]'s thread-local
/// value: the real `machine.json` is ambient state a test must not reach.
pub fn resolve_device_id() -> Option<String> {
    #[cfg(test)]
    {
        TEST_DEVICE_ID.with(|c| c.borrow().clone())
    }
    #[cfg(not(test))]
    {
        std::env::var("QONTINUI_MACHINE_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| read_device_id().ok())
    }
}

#[cfg(test)]
thread_local! {
    static TEST_DEVICE_ID: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam for [`resolve_device_id`], scoped to the calling thread.
#[cfg(test)]
pub fn set_test_device_id(id: Option<&str>) {
    TEST_DEVICE_ID.with(|c| *c.borrow_mut() = id.map(str::to_string));
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
