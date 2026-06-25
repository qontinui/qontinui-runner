//! Codex read-back identity capture (session-restore-redesign Phase 3).
//!
//! ## Why a read-back capture (and not a hook)
//!
//! Codex has **no `--session-id` flag** — it GENERATES its own session id (a
//! UUIDv7 it calls `thread_id`/`session_id`) — so the runner cannot pin identity
//! at spawn the way it does for Claude. And Codex's `SessionStart` hook does not
//! fire reliably in `codex exec` (upstream bug), so there is no POSTing hook to
//! confirm a session either. Identity therefore rides a READ-BACK channel: at
//! session start Codex writes
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl`, whose line 1
//! is `{"type":"session_meta","payload":{"session_id":"<uuid>", ...}}`. This
//! module finds the NEWEST such file written AFTER a given spawn instant and
//! parses the id out of its line-1 meta — actuating
//! [`crate::session::provider_adapter::IdentityCapture::SessionFile`].
//!
//! ## Why "newest post-spawn under CODEX_HOME" is deterministic
//!
//! The runner isolates `CODEX_HOME` per account
//! ([`crate::session::provider_adapter::CodexAdapter::account_isolation`]), so the
//! sessions tree under it is not polluted by other accounts' concurrent codex
//! activity. The single rollout written at/after this terminal's spawn names THIS
//! session. (A shared, un-isolated CODEX_HOME would make this racy — hence the
//! isolation is load-bearing, not just a config nicety.)
//!
//! ## Deterministic + fail-open
//!
//! Every step degrades to "did nothing" on any miss (no `CODEX_HOME`, no sessions
//! dir, no post-spawn file, an unparseable line 1): the session is simply left to
//! the existing reconcile backstop ([`crate::session::reconcile`]) /
//! liveness-poll machinery, never a panic. The capture confirms via the same
//! durable store the Claude hook path uses
//! ([`crate::commands::terminal::record_pinned_session_open`] +
//! [`crate::session::session_lifecycle_store::SessionLifecycleStore::confirm_session`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// Provider id recorded for a captured codex session.
pub const CODEX_PROVIDER: &str = "codex";

/// Default poll interval while waiting for the rollout file to appear.
pub const CODEX_CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Default give-up timeout for the read-back wait — generous because the
/// interactive Codex TUI may sit at an auth/onboarding prompt before it writes
/// the rollout. Past this, the reconcile backstop owns recovery.
pub const CODEX_CAPTURE_TIMEOUT: Duration = Duration::from_secs(120);

/// The sub-directory under `CODEX_HOME` Codex writes rollouts into.
const SESSIONS_SUBDIR: &str = "sessions";
/// The rollout filename prefix (`rollout-<ISO-ts>-<uuid>.jsonl`).
const ROLLOUT_PREFIX: &str = "rollout-";
/// The rollout filename extension.
const ROLLOUT_EXT: &str = "jsonl";

/// Resolve the EFFECTIVE codex home the way the codex CLI does: the `CODEX_HOME`
/// env var if set+non-empty, else `~/.codex`. Returns `None` only when neither is
/// resolvable (no env AND no home dir) — fail-open: the caller then defers to the
/// reconcile backstop.
///
/// This is the REAL-HOME resolution (the user's `codex login` auth lives there) —
/// the runner does NOT relocate `CODEX_HOME` for a hand-started codex, so the
/// read-back must scan wherever codex actually wrote its rollout. The cwd+mtime
/// match below disambiguates THIS terminal's session from any other account's
/// concurrent codex activity under the same (shared, un-isolated) home.
pub fn effective_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var("CODEX_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

/// A discovered rollout file + the modified-time used to rank it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RolloutFile {
    path: PathBuf,
    /// Modified time as Unix epoch millis (the ranking key).
    modified_ms: i64,
}

/// Recursively collect every `rollout-*.jsonl` under `sessions_root`. Returns an
/// empty vec on any IO error (fail-open). Bounded recursion via the on-disk
/// `YYYY/MM/DD/` layout; we walk generically so a layout tweak doesn't break it.
fn collect_rollouts(sessions_root: &Path) -> Vec<RolloutFile> {
    let mut out = Vec::new();
    let mut stack = vec![sessions_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // unreadable dir — skip (fail-open)
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !is_rollout_file(&path) {
                continue;
            }
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(RolloutFile { path, modified_ms });
        }
    }
    out
}

/// Does `path` look like a codex rollout file (`rollout-*.jsonl`)?
fn is_rollout_file(path: &Path) -> bool {
    let is_jsonl = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(ROLLOUT_EXT))
        .unwrap_or(false);
    if !is_jsonl {
        return false;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with(ROLLOUT_PREFIX))
        .unwrap_or(false)
}

/// Pick the NEWEST rollout written at/after `spawn_ms` (epoch millis). A small
/// negative skew is tolerated because file mtime granularity / clock skew can put
/// a just-written rollout a beat before the observed spawn instant. Returns the
/// chosen file path, or `None` when nothing qualifies (fail-open ⇒ reconcile
/// backstop).
fn newest_post_spawn(rollouts: &[RolloutFile], spawn_ms: i64) -> Option<&RolloutFile> {
    const SKEW_MS: i64 = 5_000;
    rollouts
        .iter()
        .filter(|r| r.modified_ms + SKEW_MS >= spawn_ms)
        .max_by_key(|r| r.modified_ms)
}

/// Parse the session id out of a rollout file's line 1. Codex writes
/// `{"type":"session_meta","payload":{"session_id":"<uuid>","id":"<uuid>",...}}`
/// as the first line. `meta_line_type` is the expected `type`
/// (`"session_meta"`); `id_field` is the id key (`"session_id"`), looked up both
/// at the top level and under a nested `payload` object (the verified shape nests
/// it under `payload`). Returns `None` on any parse miss (fail-open).
pub fn parse_session_id_from_line1(
    line1: &str,
    meta_line_type: &str,
    id_field: &str,
) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line1.trim()).ok()?;
    // Guard on the meta line type so we don't misread a non-meta first line.
    if v.get("type").and_then(|t| t.as_str()) != Some(meta_line_type) {
        return None;
    }
    // The id lives under `payload` in the verified shape; accept a top-level
    // field too for forward-compat.
    let from_payload = v
        .get("payload")
        .and_then(|p| p.get(id_field))
        .and_then(|s| s.as_str());
    let from_top = v.get(id_field).and_then(|s| s.as_str());
    from_payload
        .or(from_top)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Parse BOTH the session id and the `cwd` out of a rollout file's line 1. Codex
/// writes `{"type":"session_meta","payload":{"session_id":"<uuid>","cwd":"<dir>",
/// ...}}`. Returns `(session_id, Option<cwd>)` — the cwd is optional (an older
/// rollout may omit it). `None` on any parse miss (wrong type / missing id /
/// garbage), mirroring [`parse_session_id_from_line1`].
pub fn parse_session_meta_from_line1(
    line1: &str,
    meta_line_type: &str,
    id_field: &str,
) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(line1.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some(meta_line_type) {
        return None;
    }
    let payload = v.get("payload");
    let id = payload
        .and_then(|p| p.get(id_field))
        .and_then(|s| s.as_str())
        .or_else(|| v.get(id_field).and_then(|s| s.as_str()))
        .filter(|s| !s.is_empty())?
        .to_string();
    let cwd = payload
        .and_then(|p| p.get("cwd"))
        .and_then(|s| s.as_str())
        .or_else(|| v.get("cwd").and_then(|s| s.as_str()))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some((id, cwd))
}

/// Normalize a path for a tolerant cwd compare: forward-slash separators, no
/// trailing separator, lowercased (Windows path-insensitivity + codex sometimes
/// reporting a drive-letter casing that differs from the shell's `$PWD`). Pure.
fn normalize_path_for_compare(p: &str) -> String {
    let unified = p.replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    trimmed.to_ascii_lowercase()
}

/// Whether two paths refer to the same directory under the tolerant compare.
fn cwd_matches(a: &str, b: &str) -> bool {
    normalize_path_for_compare(a) == normalize_path_for_compare(b)
}

/// Read line 1 of a file (best-effort). Returns `None` on any IO error.
fn read_first_line(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().next().map(|s| s.to_string())
}

/// Resolve the `sessions/` root under a `CODEX_HOME` dir.
pub fn sessions_root(codex_home: &Path) -> PathBuf {
    codex_home.join(SESSIONS_SUBDIR)
}

/// Pure capture core: given the sessions root, the spawn instant, and the
/// capture spec fields, find the newest post-spawn rollout and parse its session
/// id. `None` on any miss. Separated from the async task + store writes so the
/// whole find+parse flow is unit-testable against a real on-disk fixture without
/// a Tauri app handle or a running runtime.
pub fn capture_session_id(
    sessions_root: &Path,
    spawn_ms: i64,
    meta_line_type: &str,
    id_field: &str,
) -> Option<String> {
    let rollouts = collect_rollouts(sessions_root);
    let chosen = newest_post_spawn(&rollouts, spawn_ms)?;
    let line1 = read_first_line(&chosen.path)?;
    parse_session_id_from_line1(&line1, meta_line_type, id_field)
}

/// REAL-HOME capture core: scan EVERY post-spawn rollout under `sessions_root`
/// and pick the NEWEST whose line-1 `session_meta` `cwd` matches `want_cwd`
/// (path-normalized) — returning its parsed `session_id`. `None` on any miss.
///
/// ## Why cwd-match (not just newest-post-spawn)
///
/// [`capture_session_id`] assumed an ISOLATED `$CODEX_HOME` (one account's
/// rollouts only), so "newest post-spawn" alone names the session. A
/// hand-started codex uses the REAL, possibly-SHARED home (the runner never
/// relocates it — auth lives there), where another account's concurrent codex
/// could write a newer post-spawn rollout. The line-1 `cwd` disambiguates: this
/// terminal's rollout was written from THIS terminal's working directory.
/// Combined with the mtime ≥ spawn filter (a small skew tolerated, as in
/// [`newest_post_spawn`]), the cwd+mtime pair deterministically names this
/// session. `None` on no match ⇒ reconcile backstop owns recovery.
pub fn capture_session_id_by_cwd(
    sessions_root: &Path,
    spawn_ms: i64,
    want_cwd: &str,
    meta_line_type: &str,
    id_field: &str,
) -> Option<String> {
    const SKEW_MS: i64 = 5_000;
    let mut rollouts = collect_rollouts(sessions_root);
    // Newest first so the FIRST cwd-match we accept is the newest qualifying one.
    rollouts.sort_by_key(|r| std::cmp::Reverse(r.modified_ms));
    for r in &rollouts {
        if r.modified_ms + SKEW_MS < spawn_ms {
            continue; // pre-spawn — a foreign/older session
        }
        let Some(line1) = read_first_line(&r.path) else {
            continue;
        };
        let Some((id, cwd)) = parse_session_meta_from_line1(&line1, meta_line_type, id_field)
        else {
            continue;
        };
        match cwd {
            Some(c) if cwd_matches(&c, want_cwd) => return Some(id),
            // A rollout missing its cwd can't be matched to this terminal — skip
            // (the cwd is the disambiguator on a shared home).
            _ => continue,
        }
    }
    None
}

/// Record + confirm a captured codex session into the durable store. This is the
/// read-back analogue of the Claude hook's `/control/session-open` write: it
/// records the (now-known) real id authoritatively for `terminal_id` and
/// CONFIRMS it (provisional→confirmed), so Phase 4's restore classifier treats it
/// as a real session and the liveness poll keeps it alive. Idempotent —
/// `record_open` dedups by session id and `confirm_session` is monotonic.
pub fn record_captured_session(
    store: &SessionLifecycleStore,
    session_id: String,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
) {
    crate::commands::terminal::record_pinned_session_open(
        store,
        session_id.clone(),
        terminal_id.clone(),
        config_dir,
        working_dir,
        title,
        page_id,
        zone_index,
        CODEX_PROVIDER.to_string(),
    );
    // A real rollout file proves the session exists — confirm it (the read-back
    // analogue of the Claude SessionStart hook firing).
    store.confirm_session(&session_id);

    // ---- supersede the speculative spawn-time pin -----------------------------
    // The always-on identity seam pre-records a PROVISIONAL claude row for EVERY
    // terminal (keyed on the runner-pinned uuid). When codex turned out to host
    // this terminal, that claude row is a phantom (no real claude session). Close
    // it so the terminal has exactly ONE live row (the captured codex one).
    // Records are keyed by session_id, so the claude pin is a DIFFERENT open row
    // for the same terminal — find it and close it, skipping the codex row we
    // just wrote.
    if let Some(other) = store.find_open_by_terminal(&terminal_id) {
        if other.claude_session_id != session_id {
            store.record_close(&other.claude_session_id, "superseded-by-codex");
            tracing::info!(
                terminal_id = %terminal_id,
                superseded = %other.claude_session_id,
                codex_session = %session_id,
                "session-restore: closed speculative spawn-time pin superseded by captured codex session"
            );
        }
    }
}

/// Async read-back task: poll for the newest post-spawn rollout under
/// `codex_home/sessions`, and on success record+confirm the captured session.
/// Fail-open: on timeout (no rollout appeared) it logs and returns — the
/// reconcile backstop owns recovery from there. Never panics.
///
/// `spawn_ms` is the spawn instant (epoch millis) used to reject pre-existing
/// foreign rollouts. The provisional spawn-time record (keyed on a temporary
/// correlation, written by the seam) is NOT mutated here — this writes the real
/// id under a fresh authoritative+confirmed row; the provisional placeholder row
/// (if any) is reaped by the phantom-prune in [`crate::session::reconcile`].
#[allow(clippy::too_many_arguments)]
pub async fn capture_and_record(
    store: std::sync::Arc<SessionLifecycleStore>,
    codex_home: PathBuf,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
    spawn_ms: i64,
    meta_line_type: String,
    id_field: String,
    interval: Duration,
    timeout: Duration,
) {
    let root = sessions_root(&codex_home);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(session_id) = capture_session_id(&root, spawn_ms, &meta_line_type, &id_field) {
            record_captured_session(
                &store,
                session_id.clone(),
                terminal_id.clone(),
                config_dir.clone(),
                working_dir.clone(),
                title.clone(),
                page_id.clone(),
                zone_index,
            );
            tracing::info!(
                terminal_id = %terminal_id,
                codex_session = %session_id,
                "session-restore: codex read-back captured session id from rollout file"
            );
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                terminal_id = %terminal_id,
                codex_home = %codex_home.display(),
                "session-restore: codex read-back timed out (no post-spawn rollout) — reconcile backstop owns recovery"
            );
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

/// REAL-HOME async read-back task, triggered by the codex wrapper's
/// `/control/session-open` POST (provider=codex, NO session_id). Resolves the
/// EFFECTIVE codex home ([`effective_codex_home`] — `$CODEX_HOME` else `~/.codex`,
/// NEVER relocated), then polls for the newest post-spawn rollout whose line-1
/// `session_meta.cwd` matches `working_dir` ([`capture_session_id_by_cwd`]). On
/// match it records+confirms the codex session and supersedes the speculative
/// spawn-time claude pin (see [`record_captured_session`]).
///
/// Fail-open at every step: no resolvable home, or no cwd+post-spawn match within
/// `timeout`, logs and returns — the reconcile backstop owns recovery. Never
/// panics. `title`/`page_id`/`zone_index` are best-effort tile metadata.
#[allow(clippy::too_many_arguments)]
pub async fn capture_and_record_by_cwd(
    store: std::sync::Arc<SessionLifecycleStore>,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
    spawn_ms: i64,
    meta_line_type: String,
    id_field: String,
    interval: Duration,
    timeout: Duration,
) {
    let Some(codex_home) = effective_codex_home() else {
        tracing::info!(
            terminal_id = %terminal_id,
            "session-restore: codex read-back has no resolvable CODEX_HOME / ~/.codex — deferring to reconcile backstop"
        );
        return;
    };
    let root = sessions_root(&codex_home);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(session_id) =
            capture_session_id_by_cwd(&root, spawn_ms, &working_dir, &meta_line_type, &id_field)
        {
            record_captured_session(
                &store,
                session_id.clone(),
                terminal_id.clone(),
                config_dir.clone(),
                working_dir.clone(),
                title.clone(),
                page_id.clone(),
                zone_index,
            );
            tracing::info!(
                terminal_id = %terminal_id,
                codex_session = %session_id,
                cwd = %working_dir,
                "session-restore: codex read-back captured session id by cwd+mtime match"
            );
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                terminal_id = %terminal_id,
                codex_home = %codex_home.display(),
                cwd = %working_dir,
                "session-restore: codex read-back (by cwd) timed out — reconcile backstop owns recovery"
            );
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// The verified rollout line-1 shape (`session_meta` with the id nested under
    /// `payload`) parses out the session id.
    #[test]
    fn parse_line1_extracts_nested_session_id() {
        let line = r#"{"type":"session_meta","payload":{"session_id":"abc-123","id":"abc-123","cwd":"C:/repo"}}"#;
        assert_eq!(
            parse_session_id_from_line1(line, "session_meta", "session_id"),
            Some("abc-123".to_string())
        );
    }

    /// A top-level id field is accepted too (forward-compat).
    #[test]
    fn parse_line1_accepts_top_level_id() {
        let line = r#"{"type":"session_meta","session_id":"top-1"}"#;
        assert_eq!(
            parse_session_id_from_line1(line, "session_meta", "session_id"),
            Some("top-1".to_string())
        );
    }

    /// A non-meta first line, a wrong type, or garbage ⇒ None (fail-open).
    #[test]
    fn parse_line1_rejects_non_meta_and_garbage() {
        assert_eq!(
            parse_session_id_from_line1(
                r#"{"type":"turn","payload":{"session_id":"x"}}"#,
                "session_meta",
                "session_id"
            ),
            None,
            "wrong type rejected"
        );
        assert_eq!(
            parse_session_id_from_line1("not json at all", "session_meta", "session_id"),
            None
        );
        assert_eq!(
            parse_session_id_from_line1(
                r#"{"type":"session_meta","payload":{}}"#,
                "session_meta",
                "session_id"
            ),
            None,
            "missing id ⇒ None"
        );
    }

    /// End-to-end on a real on-disk fixture: a post-spawn rollout under
    /// `sessions/YYYY/MM/DD/` is found and its session id parsed; a pre-spawn
    /// foreign rollout is ignored.
    #[test]
    fn capture_session_id_finds_newest_post_spawn_rollout() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let day = sessions_root(home).join("2026").join("06").join("25");
        fs::create_dir_all(&day).unwrap();

        // A foreign rollout that already existed before the spawn — must be
        // ignored even though it's a valid session_meta.
        let foreign = day.join("rollout-2026-06-25T09-00-00-foreign-uuid.jsonl");
        fs::write(
            &foreign,
            r#"{"type":"session_meta","payload":{"session_id":"foreign-uuid"}}
{"type":"turn"}"#,
        )
        .unwrap();
        // Age its mtime well before the spawn instant.
        let early = std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(1_000);
        filetime_set(&foreign, early);

        // The spawn instant.
        let spawn_ms = 100_000;

        // Our post-spawn rollout.
        let ours = day.join("rollout-2026-06-25T10-00-00-ours-uuid.jsonl");
        fs::write(
            &ours,
            r#"{"type":"session_meta","payload":{"session_id":"ours-uuid","cwd":"C:/repo"}}
{"type":"turn"}"#,
        )
        .unwrap();
        let after = std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(200_000);
        filetime_set(&ours, after);

        let got = capture_session_id(&sessions_root(home), spawn_ms, "session_meta", "session_id");
        assert_eq!(
            got,
            Some("ours-uuid".to_string()),
            "newest post-spawn rollout wins; pre-spawn foreign ignored"
        );
    }

    /// No sessions dir / no post-spawn file ⇒ None (fail-open, no panic).
    #[test]
    fn capture_session_id_is_none_when_nothing_qualifies() {
        let dir = tempdir().unwrap();
        // No sessions dir at all.
        assert_eq!(
            capture_session_id(&sessions_root(dir.path()), 0, "session_meta", "session_id"),
            None
        );
    }

    /// record_captured_session writes an authoritative+confirmed codex row.
    #[test]
    fn record_captured_session_writes_confirmed_codex_row() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        record_captured_session(
            &store,
            "codex-sess-1".to_string(),
            "term-1".to_string(),
            Some("C:/codex-home".to_string()),
            "C:/repo".to_string(),
            "repo".to_string(),
            "default".to_string(),
            0,
        );
        let rec = store.get("codex-sess-1").expect("row written");
        assert_eq!(rec.provider, CODEX_PROVIDER);
        assert!(rec.confirmed_at.is_some(), "captured session is confirmed");
        assert_eq!(rec.terminal_id, "term-1");
        assert_eq!(
            rec.origin.as_deref(),
            Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
        );
    }

    /// `parse_session_meta_from_line1` returns both id and cwd.
    #[test]
    fn parse_meta_extracts_id_and_cwd() {
        let line =
            r#"{"type":"session_meta","payload":{"session_id":"abc","cwd":"C:\\repos\\widget"}}"#;
        assert_eq!(
            parse_session_meta_from_line1(line, "session_meta", "session_id"),
            Some(("abc".to_string(), Some("C:\\repos\\widget".to_string())))
        );
        // Missing cwd ⇒ id with None cwd.
        let no_cwd = r#"{"type":"session_meta","payload":{"session_id":"abc"}}"#;
        assert_eq!(
            parse_session_meta_from_line1(no_cwd, "session_meta", "session_id"),
            Some(("abc".to_string(), None))
        );
        // Wrong type ⇒ None.
        assert_eq!(
            parse_session_meta_from_line1(r#"{"type":"turn"}"#, "session_meta", "session_id"),
            None
        );
    }

    /// The cwd compare tolerates separator + trailing-sep + case differences.
    #[test]
    fn cwd_match_is_tolerant() {
        assert!(cwd_matches("C:\\Repos\\Widget", "c:/repos/widget/"));
        assert!(cwd_matches("/home/u/proj/", "/home/u/proj"));
        assert!(!cwd_matches("C:/repos/widget", "C:/repos/other"));
    }

    /// REAL-HOME match: among several post-spawn rollouts, the one whose line-1
    /// `cwd` matches THIS terminal's cwd wins — even if a DIFFERENT account's
    /// rollout (different cwd) is newer. The pre-spawn rollout is ignored.
    #[test]
    fn capture_by_cwd_picks_matching_cwd_post_spawn() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let day = sessions_root(home).join("2026").join("06").join("25");
        fs::create_dir_all(&day).unwrap();

        // A pre-spawn rollout in OUR cwd — ignored (mtime before spawn).
        let pre = day.join("rollout-2026-06-25T08-00-00-pre.jsonl");
        fs::write(
            &pre,
            r#"{"type":"session_meta","payload":{"session_id":"pre-uuid","cwd":"C:/repos/widget"}}"#,
        )
        .unwrap();
        filetime_set(
            &pre,
            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(1_000),
        );

        let spawn_ms = 100_000;

        // A NEWER post-spawn rollout from ANOTHER account/cwd — must NOT match.
        let other = day.join("rollout-2026-06-25T11-00-00-other.jsonl");
        fs::write(
            &other,
            r#"{"type":"session_meta","payload":{"session_id":"other-uuid","cwd":"C:/repos/elsewhere"}}"#,
        )
        .unwrap();
        filetime_set(
            &other,
            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(300_000),
        );

        // OUR post-spawn rollout — matching cwd (note backslash + casing diff).
        let ours = day.join("rollout-2026-06-25T10-00-00-ours.jsonl");
        fs::write(
            &ours,
            r#"{"type":"session_meta","payload":{"session_id":"ours-uuid","cwd":"C:\\Repos\\Widget"}}"#,
        )
        .unwrap();
        filetime_set(
            &ours,
            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(200_000),
        );

        let got = capture_session_id_by_cwd(
            &sessions_root(home),
            spawn_ms,
            "C:/repos/widget",
            "session_meta",
            "session_id",
        );
        assert_eq!(
            got,
            Some("ours-uuid".to_string()),
            "cwd match wins over a newer non-matching rollout; pre-spawn ignored"
        );

        // No cwd matches ⇒ None (fail-open).
        assert_eq!(
            capture_session_id_by_cwd(
                &sessions_root(home),
                spawn_ms,
                "C:/repos/nonexistent",
                "session_meta",
                "session_id"
            ),
            None
        );
    }

    /// `record_captured_session` supersedes the speculative spawn-time claude pin
    /// for the SAME terminal: the codex row is recorded+confirmed and the prior
    /// claude row is closed `superseded-by-codex`, leaving exactly one live row.
    #[test]
    fn record_captured_session_supersedes_spawn_time_claude_pin() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // The seam's speculative spawn-time claude pin for this terminal.
        crate::commands::terminal::record_pinned_session_open(
            &store,
            "claude-pin-uuid".to_string(),
            "term-9".to_string(),
            None,
            "C:/repo".to_string(),
            "repo".to_string(),
            "default".to_string(),
            0,
            "claude".to_string(),
        );
        assert_eq!(store.open_records().len(), 1);

        // Codex read-back captures the REAL id for the same terminal.
        record_captured_session(
            &store,
            "codex-real-uuid".to_string(),
            "term-9".to_string(),
            None,
            "C:/repo".to_string(),
            "repo".to_string(),
            "default".to_string(),
            0,
        );

        // Exactly one OPEN row remains — the codex one — confirmed.
        let open = store.open_records();
        assert_eq!(open.len(), 1, "the claude pin was superseded");
        assert_eq!(open[0].claude_session_id, "codex-real-uuid");
        assert_eq!(open[0].provider, CODEX_PROVIDER);
        assert!(open[0].confirmed_at.is_some());

        // The claude pin is closed with the supersede reason.
        let claude = store
            .get("claude-pin-uuid")
            .expect("claude row still present");
        assert_eq!(claude.state, "closed");
        assert_eq!(claude.close_reason.as_deref(), Some("superseded-by-codex"));
    }

    /// Set a file's mtime (test helper). Uses a small inline filetime shim so we
    /// don't add a dep — writes via `std::fs::File::set_modified` (stable since
    /// Rust 1.75).
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let _ = f.set_modified(when);
    }
}
