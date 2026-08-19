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
/// Stop-hook script template (bundled) — the continuation-verdict `Stop` hook
/// (plan `2026-07-17-session-autonomy-fabric.md` Phase 1). Like the
/// SessionStart hook it substitutes nothing: it reads the session key + the
/// runner API port from env (`QONTINUI_TERMINAL_ID`,
/// `QONTINUI_RUNNER_API_PORT`) and the Stop payload from stdin, so the same
/// bytes work for every terminal. Verdict policy lives entirely in the
/// runner's `POST /sessions/{id}/continuation-verdict` endpoint (D4) —
/// flag-gated `QONTINUI_STOP_HOOK_CONTINUATION` default `off`, so shipping the
/// hook to every session is behaviorally inert until the flag is armed.
const STOP_HOOK_SCRIPT: &str = include_str!("../../resources/session-restore/claude_stop_hook.sh");
/// PreCompact-hook script template (bundled) — the context-exhaustion signal
/// (plan `2026-07-17-session-autonomy-fabric.md` Phase 7). Same posture as
/// the Stop hook: a dumb curl reading its seam from env
/// (`QONTINUI_TERMINAL_ID`, `QONTINUI_RUNNER_API_PORT`) and the PreCompact
/// payload from stdin; all policy lives in the runner's
/// `POST /sessions/{id}/context-low` endpoint — flag-gated
/// `QONTINUI_CONTEXT_HANDOFF` default `off`, so shipping the hook to every
/// session is behaviorally inert until the flag is armed.
const PRECOMPACT_HOOK_SCRIPT: &str =
    include_str!("../../resources/session-restore/claude_precompact_hook.sh");
/// Policy-injection hook script template (bundled) — the SECOND `SessionStart`
/// command (plan `2026-08-08-runner-enforced-policy-pull.md` Phase 1). It rides
/// the same `SessionStart` block as [`HOOK_SCRIPT`] rather than extending it,
/// because that script is the confirmation/liveness carrier and must keep its
/// silent-stdout contract; this one exists precisely to PRINT — its stdout is
/// the `hookSpecificOutput.additionalContext` envelope Claude splices into the
/// session's context, so `policy/session-protocol` Step 0 is satisfied by
/// construction instead of by the agent volunteering. Same dumb-curl posture as
/// the Stop/PreCompact scripts: it reads its seam from env
/// (`QONTINUI_TERMINAL_ID`, `QONTINUI_RUNNER_API_PORT`) and prints the runner's
/// response verbatim; every decision lives in the runner's
/// `GET /sessions/{id}/policy-context` endpoint — flag-gated
/// `QONTINUI_POLICY_INJECTION` default `off`, which answers an EMPTY body, so
/// shipping the hook to every session is behaviorally inert until armed.
const POLICY_HOOK_SCRIPT: &str =
    include_str!("../../resources/session-restore/claude_policy_hook.sh");
/// Settings template (bundled). `@@HOOK_SCRIPT@@` → the absolute path of the
/// materialized SessionStart hook script; `@@STOP_HOOK_SCRIPT@@` → the
/// materialized Stop hook script; `@@PRECOMPACT_HOOK_SCRIPT@@` → the
/// materialized PreCompact hook script; `@@POLICY_HOOK_SCRIPT@@` → the
/// materialized SessionStart policy-injection script.
const HOOK_SETTINGS: &str =
    include_str!("../../resources/session-restore/claude_hook_settings.json");

/// File name of the materialized hook script.
const HOOK_SCRIPT_NAME: &str = "claude_session_hook.sh";
/// File name of the materialized Stop-hook script.
const STOP_HOOK_SCRIPT_NAME: &str = "claude_stop_hook.sh";
/// File name of the materialized PreCompact-hook script.
const PRECOMPACT_HOOK_SCRIPT_NAME: &str = "claude_precompact_hook.sh";
/// File name of the materialized SessionStart policy-injection script.
const POLICY_HOOK_SCRIPT_NAME: &str = "claude_policy_hook.sh";
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
///
/// CACHED PER `base_dir` (Phase 6, B2). The files are byte-identical for a
/// given `base_dir` — the scripts are `include_str!`'d constants and the
/// settings file only substitutes paths derived from `base_dir` — yet every
/// terminal spawn rewrote all of them plus a `chmod` each. After the first
/// materialize in this process a spawn costs one `stat` per file (proving they
/// are still there) and nothing else. An externally deleted or modified file falls
/// straight through to a full rewrite, so the cache can never serve a path that
/// is not on disk.
pub fn materialize(base_dir: &Path) -> Option<PathBuf> {
    if let Some(settings_path) = cached_materialization(base_dir) {
        return Some(settings_path);
    }
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

    // Stop hook (continuation verdict, session-autonomy-fabric Phase 1) —
    // rides the SAME settings file, so it inherits the identical delivery +
    // fail-open posture as the SessionStart hook.
    let stop_script_path = base_dir.join(STOP_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&stop_script_path, STOP_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %stop_script_path.display(), "session-restore: claude stop-hook script write failed");
        return None;
    }
    set_executable(&stop_script_path);

    // PreCompact hook (context-exhaustion handoff, session-autonomy-fabric
    // Phase 7) — same carrier, same fail-open posture.
    let precompact_script_path = base_dir.join(PRECOMPACT_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&precompact_script_path, PRECOMPACT_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %precompact_script_path.display(), "session-restore: claude precompact-hook script write failed");
        return None;
    }
    set_executable(&precompact_script_path);

    // SessionStart policy injection (runner-enforced-policy-pull Phase 1) —
    // same carrier, same fail-open posture. Registered as a SECOND command in
    // the EXISTING `SessionStart` block, so the confirmation hook above keeps
    // its silent-stdout contract while this one carries the injected text.
    let policy_script_path = base_dir.join(POLICY_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&policy_script_path, POLICY_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %policy_script_path.display(), "session-restore: claude policy-hook script write failed");
        return None;
    }
    set_executable(&policy_script_path);

    // Substitute the scripts' absolute paths into the settings `command`s.
    // JSON needs backslashes escaped (a Windows path) so the settings file
    // stays valid JSON Claude can parse.
    let script_for_json = script_path.to_string_lossy().replace('\\', "\\\\");
    let stop_script_for_json = stop_script_path.to_string_lossy().replace('\\', "\\\\");
    let precompact_script_for_json = precompact_script_path
        .to_string_lossy()
        .replace('\\', "\\\\");
    let policy_script_for_json = policy_script_path.to_string_lossy().replace('\\', "\\\\");
    let settings = HOOK_SETTINGS
        .replace("@@HOOK_SCRIPT@@", &script_for_json)
        .replace("@@STOP_HOOK_SCRIPT@@", &stop_script_for_json)
        .replace("@@PRECOMPACT_HOOK_SCRIPT@@", &precompact_script_for_json)
        .replace("@@POLICY_HOOK_SCRIPT@@", &policy_script_for_json);
    let settings_path = base_dir.join(HOOK_SETTINGS_NAME);
    if let Err(e) = std::fs::write(&settings_path, settings.as_bytes()) {
        tracing::warn!(error = %e, path = %settings_path.display(), "session-restore: claude hook settings write failed");
        return None;
    }

    if let Ok(mut done) = MATERIALIZED.lock() {
        done.insert(base_dir.to_path_buf(), settings_path.clone());
    }
    Some(settings_path)
}

/// Base dirs whose hook set this process has already materialized, mapped to
/// the settings path [`materialize`] returned for them.
static MATERIALIZED: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Every file [`materialize`] is responsible for, in `base_dir`.
fn hook_files(base_dir: &Path) -> [PathBuf; 5] {
    [
        base_dir.join(HOOK_SCRIPT_NAME),
        base_dir.join(STOP_HOOK_SCRIPT_NAME),
        base_dir.join(PRECOMPACT_HOOK_SCRIPT_NAME),
        base_dir.join(POLICY_HOOK_SCRIPT_NAME),
        base_dir.join(HOOK_SETTINGS_NAME),
    ]
}

/// The settings path for `base_dir` if this process already materialized it AND
/// all the files are still present. `None` (⇒ full rewrite) otherwise, so an
/// operator who deletes the dir gets it back on the next spawn.
fn cached_materialization(base_dir: &Path) -> Option<PathBuf> {
    let settings_path = MATERIALIZED.lock().ok()?.get(base_dir)?.clone();
    if hook_files(base_dir).iter().all(|p| p.exists()) {
        Some(settings_path)
    } else {
        None
    }
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

        // The SAME settings file registers the Stop continuation-verdict hook
        // (session-autonomy-fabric Phase 1) pointing at the materialized stop
        // script — one carrier delivers SessionStart + Stop + pre-approval.
        let stop_script_path = tmp.path().join(STOP_HOOK_SCRIPT_NAME);
        assert!(stop_script_path.exists(), "stop-hook script materialized");
        assert!(
            !settings_text.contains("@@STOP_HOOK_SCRIPT@@"),
            "stop placeholder substituted"
        );
        let stop_cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            stop_cmd.contains(STOP_HOOK_SCRIPT_NAME),
            "Stop command runs our stop-hook script"
        );

        // The stop script POSTs to the verdict route, reads the seam env, and
        // carries the fail-open loop guard (never re-block a hook-forced
        // continuation).
        let stop_text = std::fs::read_to_string(&stop_script_path).unwrap();
        assert!(stop_text.contains("/continuation-verdict"));
        assert!(stop_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(stop_text.contains("QONTINUI_TERMINAL_ID"));
        assert!(stop_text.contains("stop_hook_active"));

        // The SAME settings file registers the PreCompact context-exhaustion
        // hook (session-autonomy-fabric Phase 7) pointing at the materialized
        // precompact script — one carrier now delivers SessionStart + Stop +
        // PreCompact + pre-approval.
        let precompact_script_path = tmp.path().join(PRECOMPACT_HOOK_SCRIPT_NAME);
        assert!(
            precompact_script_path.exists(),
            "precompact-hook script materialized"
        );
        assert!(
            !settings_text.contains("@@PRECOMPACT_HOOK_SCRIPT@@"),
            "precompact placeholder substituted"
        );
        let precompact_cmd = v["hooks"]["PreCompact"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            precompact_cmd.contains(PRECOMPACT_HOOK_SCRIPT_NAME),
            "PreCompact command runs our precompact-hook script"
        );

        // The precompact script POSTs to the context-low route, reads the
        // seam env, and never blocks the compaction (exit 0 everywhere).
        let precompact_text = std::fs::read_to_string(&precompact_script_path).unwrap();
        assert!(precompact_text.contains("/context-low"));
        assert!(precompact_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(precompact_text.contains("QONTINUI_TERMINAL_ID"));

        // The SAME settings file registers the SessionStart POLICY-INJECTION
        // hook (runner-enforced-policy-pull Phase 1) as a SECOND command inside
        // the EXISTING `SessionStart` block — not a second `SessionStart` key,
        // which would be a distinct registration Claude has no obligation to
        // merge, and not an edit to the confirmation script, which must keep
        // its silent-stdout contract.
        let policy_script_path = tmp.path().join(POLICY_HOOK_SCRIPT_NAME);
        assert!(
            policy_script_path.exists(),
            "policy-hook script materialized"
        );
        assert!(
            !settings_text.contains("@@POLICY_HOOK_SCRIPT@@"),
            "policy placeholder substituted"
        );
        let session_start = v["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart is an array");
        assert_eq!(
            session_start.len(),
            1,
            "exactly ONE SessionStart registration — the policy hook is a \
             sibling command inside it, never a second matcher block"
        );
        let session_start_cmds = session_start[0]["hooks"]
            .as_array()
            .expect("SessionStart block has a hooks array");
        assert_eq!(
            session_start_cmds.len(),
            2,
            "confirmation hook + policy hook share the one SessionStart block"
        );
        let policy_cmd = session_start_cmds[1]["command"].as_str().unwrap();
        assert!(
            policy_cmd.contains(POLICY_HOOK_SCRIPT_NAME),
            "second SessionStart command runs our policy-hook script"
        );

        // The policy script GETs the policy-context route, reads the seam env,
        // and — the load-bearing difference from every other bundled hook —
        // PRINTS the runner's response, because its stdout IS the injection.
        let policy_text = std::fs::read_to_string(&policy_script_path).unwrap();
        assert!(policy_text.contains("/policy-context"));
        assert!(policy_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(policy_text.contains("QONTINUI_TERMINAL_ID"));
        assert!(
            policy_text.contains("printf '%s' \"$resp\""),
            "the response is printed VERBATIM — the script builds no JSON"
        );

        // The confirmation hook stays silent. If this script ever grows a
        // stdout write, Claude would try to read it as a hook envelope and the
        // two SessionStart commands would fight over the same channel.
        assert!(
            !script_text.contains("printf '%s' \"$resp\""),
            "claude_session_hook.sh must keep its silent-stdout contract"
        );

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
