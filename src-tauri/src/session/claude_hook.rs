//! Bundled Claude `SessionStart` hook materializer + `--settings` delivery
//! (session-restore-redesign plan §4 `capture_hook_delivery`, Phase 2).
//!
//! ## What this delivers and why it never touches `~/.claude`
//!
//! The runner needs Claude to POST a confirmation/liveness signal to its
//! loopback control server on `SessionStart` (startup AND `--resume`). The
//! Phase-0 probe PROVED that a `SessionStart` hook supplied ONLY via
//! `claude --settings <file>` fires on both, additively — Claude MERGES the
//! `--settings` file's hooks on top of any `~/.claude` config WITHOUT writing
//! to it. So the entire delivery is two runner-owned files in the runner's OWN
//! app-data dir (`~/.qontinui/runner/session-restore/`):
//!
//!   * `claude_session_hook.sh` — the hook script (POSTs `{session_id, source,
//!     terminal_id, provider, cwd}` to `/control/session-open`).
//!   * `claude_hook_settings.json` — `{ "hooks": { "SessionStart": [...] } }`
//!     pointing `command` at the script above.
//!
//! The identity shim appends `--settings <that settings file>` to the real
//! `claude` argv (alongside `--session-id`), so a HAND-STARTED `claude` gets the
//! hook out of the box. **Nothing is ever written to or read from
//! `~/.claude/settings.json`** — the out-of-box, zero-touch guarantee (plan §2
//! Principle 2). The hook is confirmation-only: identity is already pinned +
//! recorded synchronously at spawn (the §3b determinism mechanism).
//!
//! ## Materialization
//!
//! [`materialize`] is idempotent — it (re)writes both files every call (cheap,
//! a few hundred bytes) so a runner upgrade that ships a newer template
//! refreshes them, and returns the absolute settings-file path. Fail-open: any
//! IO error returns `None` (the launch then omits `--settings` — identity still
//! rides the spawn-time `--session-id` pin; only the confirmation hook is
//! absent). The settings/script live OUTSIDE any session cwd so they are never
//! committed by a user inspecting their repo.

use std::path::{Path, PathBuf};

/// Hook-script template (bundled). Substitutes nothing — it reads everything it
/// needs from env (`QONTINUI_TERMINAL_ID`, `QONTINUI_INSTALL_INTERCEPT_PORT`)
/// and stdin, so the same bytes work for every terminal.
const HOOK_SCRIPT: &str = include_str!("../../resources/session-restore/claude_session_hook.sh");
/// Settings template (bundled). `@@HOOK_SCRIPT@@` → the absolute path of the
/// materialized hook script.
const HOOK_SETTINGS: &str =
    include_str!("../../resources/session-restore/claude_hook_settings.json");

/// File name of the materialized hook script.
const HOOK_SCRIPT_NAME: &str = "claude_session_hook.sh";
/// File name of the materialized `--settings` file.
const HOOK_SETTINGS_NAME: &str = "claude_hook_settings.json";

/// Env var the runner injects at spawn carrying the absolute path of the
/// materialized Claude `--settings` hook file. The identity shim's `claude`
/// wrapper reads it and appends `--settings $that` to the real argv. Empty/unset
/// ⇒ the shim appends nothing (fail-open — identity still rides `--session-id`).
pub const CLAUDE_SETTINGS_ENV: &str = "QONTINUI_CLAUDE_HOOK_SETTINGS";

/// The runner's OWN app-data dir for session-restore artifacts —
/// `~/.qontinui/runner/session-restore/`. Co-located with the lifecycle store +
/// shutdown marker (`~/.qontinui/runner/`). NEVER `~/.claude`.
pub fn session_restore_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join("session-restore")
}

/// Materialize the bundled Claude SessionStart hook + its `--settings` file into
/// `base_dir` (prod: [`session_restore_dir`]; tests: a tempdir), substituting
/// the hook-script absolute path into the settings, and return the absolute path
/// of the settings file (for `claude --settings <path>` / [`DeliverySpec`]).
///
/// Idempotent + fail-open: any IO failure logs at warn and returns `None`, so a
/// launch that can't write the hook simply omits `--settings` (identity still
/// pinned via `--session-id`; only the confirmation hook is absent). The hook
/// script is marked executable on Unix.
pub fn materialize(base_dir: &Path) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(base_dir) {
        tracing::warn!(
            error = %e,
            dir = %base_dir.display(),
            "session-restore: claude hook dir create failed — --settings hook delivery off (identity still pinned)"
        );
        return None;
    }

    let script_path = base_dir.join(HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&script_path, HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %script_path.display(), "session-restore: claude hook script write failed");
        return None;
    }
    set_executable(&script_path);

    // Substitute the script's absolute path into the settings `command`. JSON
    // needs backslashes escaped (a Windows path) so the settings file stays
    // valid JSON Claude can parse.
    let script_for_json = script_path.to_string_lossy().replace('\\', "\\\\");
    let settings = HOOK_SETTINGS.replace("@@HOOK_SCRIPT@@", &script_for_json);
    let settings_path = base_dir.join(HOOK_SETTINGS_NAME);
    if let Err(e) = std::fs::write(&settings_path, settings.as_bytes()) {
        tracing::warn!(error = %e, path = %settings_path.display(), "session-restore: claude hook settings write failed");
        return None;
    }

    Some(settings_path)
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_writes_both_files_and_points_settings_at_script() {
        let tmp = tempfile::tempdir().unwrap();
        let settings_path = materialize(tmp.path()).expect("materialize ok");

        // Settings file exists at the returned path with the expected name.
        assert!(settings_path.exists());
        assert_eq!(
            settings_path.file_name().unwrap().to_string_lossy(),
            HOOK_SETTINGS_NAME
        );

        // Hook script exists alongside it.
        let script_path = tmp.path().join(HOOK_SCRIPT_NAME);
        assert!(script_path.exists(), "hook script materialized");

        // Settings is valid JSON registering a SessionStart command hook that
        // points at the materialized script (no unsubstituted placeholder).
        let settings_text = std::fs::read_to_string(&settings_path).unwrap();
        assert!(
            !settings_text.contains("@@HOOK_SCRIPT@@"),
            "placeholder substituted"
        );
        let v: serde_json::Value = serde_json::from_str(&settings_text).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        // The command references the script basename (path is embedded, escaped).
        assert!(
            cmd.contains(HOOK_SCRIPT_NAME),
            "command runs our hook script"
        );

        // The SAME delivered settings file pre-approves the coord-mcp tools, so a
        // fresh user's first coord tool call isn't blocked by a per-tool prompt
        // (mcp-config-universal-provisioning Phase 2). This rides the `--settings`
        // the shim already appends — one file delivers both hook + pre-approval.
        let allow = v["permissions"]["allow"]
            .as_array()
            .expect("permissions.allow present");
        assert!(
            allow.iter().any(|a| a.as_str() == Some("mcp__coord-mcp")),
            "coord-mcp tools pre-approved in the delivered settings"
        );

        // The hook script POSTs to the control route and reads the seam env.
        let script_text = std::fs::read_to_string(&script_path).unwrap();
        assert!(script_text.contains("/control/session-open"));
        assert!(script_text.contains("QONTINUI_INSTALL_INTERCEPT_PORT"));
        assert!(script_text.contains("QONTINUI_TERMINAL_ID"));

        // The DELIVERY never writes to / reads from the user's config: both
        // materialized files live under the runner's own app-data dir (the
        // tempdir here), NOT `~/.claude`. (The hook script's prose explains it
        // never touches `~/.claude`, so we assert on the materialized PATHS, the
        // load-bearing guarantee — not on the comment text.)
        let tmp_str = tmp.path().to_string_lossy();
        assert!(settings_path
            .to_string_lossy()
            .starts_with(tmp_str.as_ref()));
        assert!(script_path.to_string_lossy().starts_with(tmp_str.as_ref()));
    }

    #[test]
    fn materialize_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let a = materialize(tmp.path()).unwrap();
        let b = materialize(tmp.path()).unwrap();
        assert_eq!(a, b, "stable settings path across calls");
        assert!(a.exists());
    }

    #[test]
    fn session_restore_dir_is_under_qontinui_runner_not_dot_claude() {
        let dir = session_restore_dir();
        let s = dir.to_string_lossy();
        assert!(s.contains("runner"), "lives under ~/.qontinui/runner");
        assert!(s.ends_with("session-restore"));
        assert!(!s.contains(".claude"), "NEVER under the user's ~/.claude");
    }
}
