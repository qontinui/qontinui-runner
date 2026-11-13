//! Codex read-back identity capture (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 6 — ported from qontinui-runner PR #651, `75198ffd4`, which merged
//! into a stacked base and never reached `main`).
//!
//! ## Why a read-back capture (and not a pin or a hook)
//!
//! Codex has **no flag that takes a session id** — it mints its own (a UUIDv7)
//! — so the runner cannot pin identity at spawn the way it does for Claude.
//! What Codex does do, from the first moment of a session, is write
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`, whose line 1 is
//! `{"type":"session_meta","payload":{"session_id":"<id>","cwd":"<dir>",…}}`
//! (observed against codex-cli 0.159.1; plan probe Q5). This module finds the
//! rollout of THIS terminal's session and reads the id out of it.
//!
//! The trigger is the Codex identity shim
//! (`resources/intercept/codex_identity_shim.{bash,cmd}`), which posts
//! `/control/session-open` with no session id just before it execs the real
//! `codex`; `session::provider_adapter::CodexAdapter` starts the capture.
//!
//! ## Why cwd + post-start CREATION time disambiguates
//!
//! The runner does NOT relocate `CODEX_HOME` for a hand-started `codex`: the
//! user's `codex login` lives there, and relocating it would log them out. The
//! capture therefore scans a home that a concurrent Codex session — in another
//! terminal, or outside the runner — may also be writing. What names this
//! terminal's session is:
//!
//! - the line-1 `session_meta.cwd`, matched against the cwd the shim posted,
//! - the rollout's CREATION time — `session_meta.timestamp`, else the
//!   `rollout-YYYY-MM-DDTHH-MM-SS-` stamp in its name — at or after the moment
//!   the shim signalled. Not its mtime: an OLDER session still running in the
//!   same cwd appends to its rollout on every turn, so its mtime is always
//!   "after the start" and it used to be captured as the new session, and
//! - its id not already being bound to ANOTHER open terminal — two Codex
//!   sessions started in one cwd a moment apart must not both capture the
//!   same rollout.
//!
//! ## Fail-open
//!
//! Every miss — no resolvable home, no sessions dir, no matching rollout before
//! the timeout, an unparseable line 1 — degrades to "did nothing" and logs it.
//! The terminal keeps whatever record it had; nothing panics.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qontinui_runner_lib::cli_profile::codex;

use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// Poll interval while waiting for the rollout file to appear.
pub const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Give-up timeout for the read-back. Generous because the interactive TUI may
/// sit at a login or onboarding screen before it writes the rollout.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(120);

/// Directory under the Codex home that holds rollouts.
const SESSIONS_SUBDIR: &str = "sessions";
/// Rollout filename prefix (`rollout-<ts>-<id>.jsonl`).
const ROLLOUT_PREFIX: &str = "rollout-";
/// Rollout filename extension.
const ROLLOUT_EXT: &str = "jsonl";
/// The `type` of a rollout's line 1.
const SESSION_META_TYPE: &str = "session_meta";
/// The id keys of the `session_meta` payload, in preference order (0.159.1
/// writes both with the same value).
const ID_FIELDS: [&str; 2] = ["session_id", "id"];
/// Tolerance for a creation time that lands a beat before the observed start
/// (the filename stamp's one-second granularity, clock skew, the CLI writing
/// its rollout a moment before the shim's signal is handled).
const CREATED_SKEW_MS: i64 = 5_000;
/// A rollout whose filename stamp is older than the start by more than this is
/// skipped without opening it. A day covers any offset between the local time
/// the stamp may be written in and UTC.
const FILENAME_PREFILTER_MS: i64 = 24 * 60 * 60 * 1000;
/// The close reason stamped on the terminal's other open records once the
/// Codex session is captured.
const SUPERSEDED_REASON: &str = "superseded-by-codex";

/// The Codex home to scan, resolved the way the CLI resolves it for the
/// session being captured: the `CODEX_HOME` that session ran under (posted by
/// the shim) if non-empty, else the runner's own `CODEX_HOME`, else
/// `~/.codex`. `None` only when none of those resolves.
pub fn effective_codex_home(session_codex_home: Option<&str>) -> Option<PathBuf> {
    let non_empty = |v: &str| !v.trim().is_empty();
    if let Some(home) = session_codex_home.filter(|v| non_empty(v)) {
        return Some(PathBuf::from(home));
    }
    if let Some(home) = std::env::var("CODEX_HOME").ok().filter(|v| non_empty(v)) {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

/// The `sessions/` root under a Codex home.
pub fn sessions_root(codex_home: &Path) -> PathBuf {
    codex_home.join(SESSIONS_SUBDIR)
}

/// Every `rollout-*.jsonl` under `sessions_root`, walked generically so a
/// change to the `YYYY/MM/DD/` layout does not break it. An unreadable
/// directory is skipped.
fn collect_rollouts(sessions_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![sessions_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !is_rollout_file(&path) {
                continue;
            }
            out.push(path);
        }
    }
    out
}

/// The creation time (epoch millis) a rollout's NAME states:
/// `rollout-YYYY-MM-DDTHH-MM-SS-<id>.jsonl`, read as local time (the frame
/// the CLI stamps it in). `None` for a name without that stamp.
fn filename_created_ms(path: &Path) -> Option<i64> {
    use chrono::TimeZone;
    let name = path.file_name()?.to_str()?;
    let stamp = name.strip_prefix(ROLLOUT_PREFIX)?.get(..19)?;
    let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H-%M-%S").ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp_millis())
}

/// One rollout's line-1 facts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMeta {
    id: String,
    cwd: Option<String>,
    /// `session_meta.timestamp` (RFC 3339) as epoch millis, when present.
    created_ms: Option<i64>,
}

/// Whether `path` names a Codex rollout (`rollout-*.jsonl`).
fn is_rollout_file(path: &Path) -> bool {
    let is_jsonl = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ROLLOUT_EXT));
    is_jsonl
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(ROLLOUT_PREFIX))
}

/// The session id and, when present, the cwd from a rollout's line 1. Both are
/// looked up under `payload` first (the observed shape) and at the top level
/// second. `None` when the line is not JSON, is not a `session_meta` line, or
/// carries no id.
pub fn parse_session_meta_from_line1(line1: &str) -> Option<(String, Option<String>)> {
    parse_meta(line1).map(|m| (m.id, m.cwd))
}

fn parse_meta(line1: &str) -> Option<SessionMeta> {
    let v: serde_json::Value = serde_json::from_str(line1.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some(SESSION_META_TYPE) {
        return None;
    }
    let payload = v.get("payload");
    let field = |name: &str| {
        payload
            .and_then(|p| p.get(name))
            .and_then(|s| s.as_str())
            .or_else(|| v.get(name).and_then(|s| s.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let id = ID_FIELDS.iter().find_map(|name| field(name))?;
    let created_ms = field("timestamp")
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
        .map(|t| t.timestamp_millis());
    Some(SessionMeta {
        id,
        cwd: field("cwd"),
        created_ms,
    })
}

/// Normalize a path for a tolerant cwd compare: forward slashes, lowercase, no
/// trailing separator, and the MSYS / Git-Bash drive form folded into the
/// Windows one. Pure.
///
/// The drive fold is load-bearing on Windows: Codex (a Windows binary) records
/// `C:\Users\…` in `session_meta`, while a Git-Bash `$PWD` reads `/c/Users/…`.
/// Unfolded, the two never compare equal and every Git-Bash-hosted capture
/// misses. The bash shim also posts `pwd -W` (already the Windows form) where
/// it can; this fold covers any caller that does not (#651, `6b38190c6`).
fn normalize_path_for_compare(p: &str) -> String {
    let unified = p.replace('\\', "/").to_ascii_lowercase();
    // `/c/users/…` ⇒ `c:/users/…`, and a bare `/c` ⇒ `c:`. Only a single ASCII
    // letter between the leading slash and the next slash (or the end) counts
    // as a drive, so `/home/…` is never mangled.
    let drive_fixed = match unified.as_bytes() {
        [b'/', letter, rest @ ..]
            if letter.is_ascii_lowercase() && matches!(rest.first(), None | Some(b'/')) =>
        {
            format!("{}:{}", char::from(*letter), unified.get(2..).unwrap_or(""))
        }
        _ => unified,
    };
    drive_fixed.trim_end_matches('/').to_string()
}

/// Whether two paths name the same directory under the tolerant compare.
fn cwd_matches(a: &str, b: &str) -> bool {
    normalize_path_for_compare(a) == normalize_path_for_compare(b)
}

/// Line 1 of a file, read without loading the rest — a rollout's later lines
/// can be large. `None` on any IO error.
fn read_first_line(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    Some(line)
}

/// The id of the newest rollout under `sessions_root` CREATED at or after
/// `started_ms` (within [`CREATED_SKEW_MS`]) whose `session_meta.cwd` matches
/// `want_cwd` and whose id `bound_elsewhere` does not claim. A rollout without
/// a cwd, or without any creation time, cannot be attributed and is skipped.
/// `None` when nothing qualifies.
pub fn capture_session_id_by_cwd(
    sessions_root: &Path,
    started_ms: i64,
    want_cwd: &str,
    bound_elsewhere: &dyn Fn(&str) -> bool,
) -> Option<String> {
    collect_rollouts(sessions_root)
        .into_iter()
        .filter(|path| {
            // Cheap: an old stamp in the name skips the open.
            filename_created_ms(path).is_none_or(|ms| ms + FILENAME_PREFILTER_MS >= started_ms)
        })
        .filter_map(|path| {
            let meta = parse_meta(&read_first_line(&path)?)?;
            let created_ms = meta.created_ms.or_else(|| filename_created_ms(&path))?;
            Some((meta, created_ms))
        })
        .filter(|(_, created_ms)| created_ms + CREATED_SKEW_MS >= started_ms)
        .filter(|(meta, _)| {
            meta.cwd
                .as_deref()
                .is_some_and(|c| cwd_matches(c, want_cwd))
        })
        .filter(|(meta, _)| !bound_elsewhere(&meta.id))
        .max_by_key(|(_, created_ms)| *created_ms)
        .map(|(meta, _)| meta.id)
}

/// A rollout's last-modified time as epoch millis.
fn modified_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The id of the session a RESUME launch (`codex resume …`) continued.
///
/// `codex resume <id>` appends to the EXISTING rollout, whose creation time
/// predates the start — [`capture_session_id_by_cwd`] never sees it. So:
///
/// * with an argv id that passes the strict id charset
///   ([`qontinui_runner_lib::cli_profile::is_valid_session_id`]), the rollout
///   carrying that id, once it has been written since the start (the resume
///   actually happened). Its recorded cwd is not required to match: a resume
///   may run from anywhere.
/// * without one (`resume --last`, the picker, or an id that fails the
///   charset), the newest rollout in `want_cwd` WRITTEN since the start,
///   whatever its creation time — a resume launch is expected to continue an
///   older rollout.
///
/// An id `bound_elsewhere` claims is skipped either way.
pub fn capture_resumed_session_id(
    sessions_root: &Path,
    started_ms: i64,
    want_cwd: &str,
    resume_id: Option<&str>,
    bound_elsewhere: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let resume_id =
        resume_id.filter(|id| qontinui_runner_lib::cli_profile::is_valid_session_id(id));
    collect_rollouts(sessions_root)
        .into_iter()
        .filter_map(|path| {
            let written_ms = modified_ms(&path)?;
            if written_ms + CREATED_SKEW_MS < started_ms {
                return None;
            }
            let meta = parse_meta(&read_first_line(&path)?)?;
            Some((meta, written_ms))
        })
        .filter(|(meta, _)| match resume_id {
            Some(id) => meta.id == id,
            None => meta
                .cwd
                .as_deref()
                .is_some_and(|c| cwd_matches(c, want_cwd)),
        })
        .filter(|(meta, _)| !bound_elsewhere(&meta.id))
        .max_by_key(|(_, written_ms)| *written_ms)
        .map(|(meta, _)| meta.id)
}

/// Record and confirm a captured Codex session for `terminal_id`, then close
/// every other open record of that terminal.
///
/// The record inherits the terminal's placement (page, zone, title) from the
/// record the identity seam wrote at spawn, so a restore puts the Codex
/// session back where it ran. That spawn-time record is a speculative pin for
/// a Claude launch that did not happen; it is closed as
/// [`SUPERSEDED_REASON`]. Every other open record of the terminal is closed
/// too — iterating them all, because the store's single-record lookup scans a
/// hash map in arbitrary order and could return the Codex record itself
/// (#651's Windows-only flake).
///
/// `codex_home` is recorded as the session's account dir only when the session
/// ran under an explicit `CODEX_HOME`; a default `~/.codex` is left unset, so a
/// restore does not pin an override the user never set.
pub fn record_captured_session(
    store: &SessionLifecycleStore,
    session_id: &str,
    terminal_id: &str,
    codex_home: Option<String>,
    working_dir: &str,
) {
    let terminal_open: Vec<_> = store
        .open_records()
        .into_iter()
        .filter(|r| r.terminal_id == terminal_id)
        .collect();
    let placement = terminal_open
        .iter()
        .find(|r| r.claude_session_id != session_id);
    let title = placement
        .and_then(|r| r.title.clone())
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            working_dir
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| codex::ID.to_string());
    crate::commands::terminal::record_pinned_session_open(
        store,
        session_id.to_string(),
        terminal_id.to_string(),
        codex_home.filter(|h| !h.trim().is_empty()),
        working_dir.to_string(),
        title,
        placement.map_or_else(|| "default".to_string(), |r| r.page_id.clone()),
        placement.map_or(0, |r| r.zone_index),
        codex::ID.to_string(),
    );
    // A rollout file proves the session exists — the read-back analogue of a
    // provider's SessionStart hook confirming it.
    store.confirm_session(session_id);

    for other in terminal_open {
        if other.claude_session_id != session_id {
            store.record_close(&other.claude_session_id, SUPERSEDED_REASON);
            tracing::info!(
                terminal_id = %terminal_id,
                superseded = %other.claude_session_id,
                codex_session = %session_id,
                "session-restore: closed the terminal's spawn-time record, superseded by the captured codex session"
            );
        }
    }
}

/// What a read-back capture starts from: the facts the Codex shim's start
/// signal carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureStart {
    /// The terminal the session started in (`QONTINUI_TERMINAL_ID`).
    pub terminal_id: String,
    /// The cwd the shim reported, in the frame Codex records it in.
    pub cwd: String,
    /// The `CODEX_HOME` the session ran under, when the shim saw one set.
    pub codex_home: Option<String>,
    /// When the signal arrived (epoch millis); older rollouts are ignored.
    pub started_ms: i64,
    /// `Some` when the launch was `codex resume …`: it continues an existing
    /// rollout rather than creating one ([`capture_resumed_session_id`]).
    /// The inner value is the id from argv, unvalidated (validated at use).
    pub resume: Option<Option<String>>,
}

/// Poll for the rollout of the session `start` describes and, on a match,
/// record it ([`record_captured_session`]) and hand its id and recorded
/// account dir to `on_recorded`. Fail-open: no resolvable home, or no match
/// within `timeout`, logs and returns.
pub async fn capture_and_record_by_cwd(
    store: std::sync::Arc<SessionLifecycleStore>,
    start: CaptureStart,
    interval: Duration,
    timeout: Duration,
    on_recorded: impl FnOnce(&str, &str) + Send,
) {
    let Some(home) = effective_codex_home(start.codex_home.as_deref()) else {
        tracing::info!(
            terminal_id = %start.terminal_id,
            "session-restore: codex read-back has no resolvable CODEX_HOME or ~/.codex — nothing captured"
        );
        return;
    };
    let root = sessions_root(&home);
    let deadline = std::time::Instant::now() + timeout;
    // A rollout whose session is already OPEN on another terminal is that
    // terminal's, however well its cwd and time match this one.
    let bound_elsewhere = |id: &str| {
        store
            .get(id)
            .is_some_and(|r| r.state == "open" && r.terminal_id != start.terminal_id)
    };
    loop {
        let found = match &start.resume {
            Some(resume_id) => capture_resumed_session_id(
                &root,
                start.started_ms,
                &start.cwd,
                resume_id.as_deref(),
                &bound_elsewhere,
            ),
            None => {
                capture_session_id_by_cwd(&root, start.started_ms, &start.cwd, &bound_elsewhere)
            }
        };
        if let Some(session_id) = found {
            record_captured_session(
                &store,
                &session_id,
                &start.terminal_id,
                start.codex_home.clone(),
                &start.cwd,
            );
            tracing::info!(
                terminal_id = %start.terminal_id,
                codex_session = %session_id,
                cwd = %start.cwd,
                "session-restore: codex read-back captured the session id by cwd + creation time"
            );
            let recorded_dir = store
                .get(&session_id)
                .and_then(|r| r.config_dir)
                .unwrap_or_default();
            on_recorded(&session_id, &recorded_dir);
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                terminal_id = %start.terminal_id,
                codex_home = %home.display(),
                cwd = %start.cwd,
                "session-restore: codex read-back timed out with no matching rollout — nothing captured"
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

    /// Set a file's mtime (`File::set_modified`, stable since Rust 1.75).
    fn set_mtime_ms(path: &Path, ms: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(ms))
            .unwrap();
    }

    fn write_rollout(day: &Path, name: &str, line1: &str, mtime_ms: u64) {
        let path = day.join(name);
        fs::write(&path, format!("{line1}\n{{\"type\":\"event_msg\"}}\n")).unwrap();
        set_mtime_ms(&path, mtime_ms);
    }

    #[test]
    fn line1_yields_the_nested_id_and_cwd() {
        let line =
            r#"{"type":"session_meta","payload":{"session_id":"abc","cwd":"C:\\repos\\widget"}}"#;
        assert_eq!(
            parse_session_meta_from_line1(line),
            Some(("abc".to_string(), Some("C:\\repos\\widget".to_string())))
        );
        // No cwd: the id still parses.
        assert_eq!(
            parse_session_meta_from_line1(
                r#"{"type":"session_meta","payload":{"session_id":"abc"}}"#
            ),
            Some(("abc".to_string(), None))
        );
    }

    #[test]
    fn line1_accepts_a_top_level_id_and_the_payload_id_spelling() {
        assert_eq!(
            parse_session_meta_from_line1(r#"{"type":"session_meta","session_id":"top-1"}"#),
            Some(("top-1".to_string(), None))
        );
        // A UUIDv7 under `payload.id` only — the second key 0.159.1 writes.
        let v7 = "01a0ef49-1234-7abc-8def-0123456789ab";
        assert_eq!(
            parse_session_meta_from_line1(&format!(
                r#"{{"type":"session_meta","payload":{{"id":"{v7}","cwd":"/w"}}}}"#
            )),
            Some((v7.to_string(), Some("/w".to_string())))
        );
    }

    #[test]
    fn line1_rejects_other_types_garbage_and_a_missing_id() {
        assert_eq!(
            parse_session_meta_from_line1(r#"{"type":"turn","payload":{"session_id":"x"}}"#),
            None
        );
        assert_eq!(parse_session_meta_from_line1("not json at all"), None);
        assert_eq!(
            parse_session_meta_from_line1(r#"{"type":"session_meta","payload":{}}"#),
            None
        );
    }

    /// Separator, trailing-slash and case differences match; so does the
    /// Git-Bash drive form against the Windows form Codex records
    /// (`6b38190c6`). A real POSIX path is never mangled into another.
    #[test]
    fn cwd_match_is_tolerant() {
        assert!(cwd_matches("C:\\Repos\\Widget", "c:/repos/widget/"));
        assert!(cwd_matches("/home/u/proj/", "/home/u/proj"));
        assert!(!cwd_matches("C:/repos/widget", "C:/repos/other"));
        assert!(cwd_matches("/c/users/jspin/proj", "C:\\Users\\jspin\\proj"));
        assert!(cwd_matches("/c/repos/widget", "c:/repos/widget"));
        assert!(cwd_matches("/c", "C:\\"));
        assert!(cwd_matches("/home/u/proj", "/home/u/proj"));
        assert!(!cwd_matches("/c/repos/widget", "/d/repos/widget"));
        assert!(!cwd_matches("/home/u", "h:/ome/u"));
    }

    /// Epoch millis → the RFC 3339 `session_meta.timestamp` form.
    fn ts(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339()
    }

    fn meta(id: &str, cwd: &str, created_ms: i64) -> String {
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{id}","timestamp":"{}","cwd":"{}"}}}}"#,
            ts(created_ms),
            cwd.replace('\\', "\\\\")
        )
    }

    fn nobody(_: &str) -> bool {
        false
    }

    /// A start well after the rollouts' filename stamps would pre-filter them;
    /// these tests key everything off `session_meta.timestamp`, so their
    /// filenames carry no stamp.
    const T0: i64 = 1_790_000_000_000;

    /// Among several rollouts, the newest post-start one whose cwd matches
    /// wins — even when a rollout from another cwd is newer. A pre-start
    /// rollout in the same cwd is ignored.
    #[test]
    fn capture_by_cwd_picks_the_matching_post_start_rollout() {
        let dir = tempdir().unwrap();
        let day = sessions_root(dir.path()).join("2026").join("06").join("25");
        fs::create_dir_all(&day).unwrap();
        let now = T0 as u64 + 1_000_000;
        write_rollout(
            &day,
            "rollout-pre.jsonl",
            &meta("pre-uuid", "C:/repos/widget", T0 - 60_000),
            now,
        );
        write_rollout(
            &day,
            "rollout-other.jsonl",
            &meta("other-uuid", "C:/repos/elsewhere", T0 + 300_000),
            now,
        );
        write_rollout(
            &day,
            "rollout-ours.jsonl",
            &meta("ours-uuid", "C:\\Repos\\Widget", T0 + 200_000),
            now,
        );
        // Not a rollout name: ignored even though its content would match.
        write_rollout(
            &day,
            "history.jsonl",
            &meta("nope", "C:/repos/widget", T0 + 400_000),
            now,
        );

        let root = sessions_root(dir.path());
        assert_eq!(
            capture_session_id_by_cwd(&root, T0, "C:/repos/widget", &nobody),
            Some("ours-uuid".to_string())
        );
        assert_eq!(
            capture_session_id_by_cwd(&root, T0, "C:/repos/nonexistent", &nobody),
            None
        );
        // Started after every rollout was created: nothing qualifies.
        assert_eq!(
            capture_session_id_by_cwd(&root, T0 + 900_000, "C:/repos/widget", &nobody),
            None
        );
    }

    /// `codex resume <id>` appends to a rollout CREATED long before the start:
    /// a resume launch captures it by its argv id (or, without a usable id,
    /// by a post-start write in the cwd), while a fresh launch never would.
    #[test]
    fn a_resume_launch_captures_the_continued_older_rollout() {
        let dir = tempdir().unwrap();
        let day = sessions_root(dir.path()).join("2026").join("06").join("25");
        fs::create_dir_all(&day).unwrap();
        let written_after_start = T0 as u64 + 10_000;
        write_rollout(
            &day,
            "rollout-old.jsonl",
            &meta("old-uuid", "/work/proj", T0 - 3_600_000),
            written_after_start,
        );
        // A same-cwd rollout NOT written since the start: not the resumed one.
        write_rollout(
            &day,
            "rollout-idle.jsonl",
            &meta("idle-uuid", "/work/proj", T0 - 7_200_000),
            T0 as u64 - 600_000,
        );
        let root = sessions_root(dir.path());

        // The fresh-launch rule cannot see it (created before the start).
        assert_eq!(
            capture_session_id_by_cwd(&root, T0, "/work/proj", &nobody),
            None
        );
        // By argv id — from another cwd, too.
        assert_eq!(
            capture_resumed_session_id(&root, T0, "/elsewhere", Some("old-uuid"), &nobody),
            Some("old-uuid".to_string())
        );
        // An idle rollout's id is not captured: it was not written since.
        assert_eq!(
            capture_resumed_session_id(&root, T0, "/work/proj", Some("idle-uuid"), &nobody),
            None
        );
        // No usable id (`resume --last`, or one failing the charset): the
        // post-start write in this cwd.
        assert_eq!(
            capture_resumed_session_id(&root, T0, "/work/proj", None, &nobody),
            Some("old-uuid".to_string())
        );
        assert_eq!(
            capture_resumed_session_id(&root, T0, "/work/proj", Some("x; rm -rf /"), &nobody),
            Some("old-uuid".to_string())
        );
        // Bound to another terminal: skipped.
        assert_eq!(
            capture_resumed_session_id(&root, T0, "/work/proj", Some("old-uuid"), &|id| {
                id == "old-uuid"
            }),
            None
        );
    }

    /// M4: an OLDER session still running in the same cwd keeps appending to
    /// its rollout, so its mtime is always after the new start — it is not
    /// the new session. Creation time decides, and an id already open on
    /// another terminal is skipped.
    #[test]
    fn a_running_older_session_in_the_same_cwd_is_not_captured() {
        let dir = tempdir().unwrap();
        let day = sessions_root(dir.path()).join("2026").join("10").join("03");
        fs::create_dir_all(&day).unwrap();
        let root = sessions_root(dir.path());
        // Created an hour before the start, written to a second ago.
        write_rollout(
            &day,
            "rollout-old.jsonl",
            &meta("old-running", "/w/proj", T0 - 3_600_000),
            T0 as u64 + 60_000,
        );
        assert_eq!(
            capture_session_id_by_cwd(&root, T0, "/w/proj", &nobody),
            None,
            "a busy older session's fresh mtime does not make it new"
        );

        // The real new session appears; another terminal already holds a
        // second new one in the same cwd.
        write_rollout(
            &day,
            "rollout-new.jsonl",
            &meta("new-1", "/w/proj", T0 + 1_000),
            T0 as u64,
        );
        write_rollout(
            &day,
            "rollout-new2.jsonl",
            &meta("new-2", "/w/proj", T0 + 2_000),
            T0 as u64,
        );
        assert_eq!(
            capture_session_id_by_cwd(&root, T0, "/w/proj", &|id| id == "new-2"),
            Some("new-1".to_string())
        );
    }

    /// The filename stamp is the creation time when line 1 carries none.
    #[test]
    fn the_filename_stamp_is_the_creation_time_fallback() {
        let dir = tempdir().unwrap();
        let day = sessions_root(dir.path()).join("2026").join("10").join("03");
        fs::create_dir_all(&day).unwrap();
        let name = "rollout-2026-10-03T10-00-00-abc.jsonl";
        write_rollout(
            &day,
            name,
            r#"{"type":"session_meta","payload":{"id":"abc","cwd":"/w"}}"#,
            1,
        );
        let created = filename_created_ms(&day.join(name)).expect("stamp parses");
        let root = sessions_root(dir.path());
        assert_eq!(
            capture_session_id_by_cwd(&root, created, "/w", &nobody),
            Some("abc".to_string())
        );
        assert_eq!(
            capture_session_id_by_cwd(&root, created + 60_000, "/w", &nobody),
            None
        );
        assert_eq!(
            filename_created_ms(Path::new("rollout-garbage.jsonl")),
            None
        );
    }

    #[test]
    fn capture_by_cwd_is_none_without_a_sessions_dir() {
        let dir = tempdir().unwrap();
        assert_eq!(
            capture_session_id_by_cwd(&sessions_root(dir.path()), 0, "/w", &nobody),
            None
        );
    }

    #[test]
    fn the_session_codex_home_wins_over_the_runner_default() {
        assert_eq!(
            effective_codex_home(Some("/accounts/work/.codex")),
            Some(PathBuf::from("/accounts/work/.codex"))
        );
        // Blank is not an override.
        assert_ne!(effective_codex_home(Some("  ")), Some(PathBuf::from("  ")));
    }

    /// The captured record is a confirmed, authoritative Codex record.
    #[test]
    fn record_captured_session_writes_a_confirmed_codex_record() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        record_captured_session(
            &store,
            "codex-sess-1",
            "term-1",
            Some("/accounts/work/.codex".to_string()),
            "C:/repo",
        );
        let rec = store.get("codex-sess-1").expect("record written");
        assert_eq!(rec.provider, codex::ID);
        assert!(
            rec.confirmed_at.is_some(),
            "a captured session is confirmed"
        );
        assert_eq!(rec.terminal_id, "term-1");
        assert_eq!(rec.title.as_deref(), Some("repo"));
        assert_eq!(rec.config_dir.as_deref(), Some("/accounts/work/.codex"));
        assert_eq!(
            rec.origin.as_deref(),
            Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
        );
    }

    /// The terminal's spawn-time pin is superseded: one open record remains,
    /// the Codex one, in the pin's page and zone.
    #[test]
    fn record_captured_session_supersedes_the_spawn_time_pin() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        crate::commands::terminal::record_pinned_session_open(
            &store,
            "claude-pin-uuid".to_string(),
            "term-9".to_string(),
            None,
            "C:/repo".to_string(),
            "My zone".to_string(),
            "page-2".to_string(),
            3,
            qontinui_runner_lib::cli_profile::claude::ID.to_string(),
        );
        assert_eq!(store.open_records().len(), 1);

        record_captured_session(&store, "codex-real-uuid", "term-9", None, "C:/repo");

        let open = store.open_records();
        assert_eq!(open.len(), 1, "the spawn-time pin was superseded");
        assert_eq!(open[0].claude_session_id, "codex-real-uuid");
        assert_eq!(open[0].provider, codex::ID);
        assert!(open[0].confirmed_at.is_some());
        assert_eq!(open[0].page_id, "page-2");
        assert_eq!(open[0].zone_index, 3);
        assert_eq!(open[0].title.as_deref(), Some("My zone"));
        assert_eq!(open[0].config_dir, None);

        // The store's own supersede scan closes an unconfirmed pin as soon as
        // the authoritative Codex record lands; the explicit sweep covers the
        // rows it leaves (a confirmed one, below). Either way it is closed as
        // superseded.
        let pin = store.get("claude-pin-uuid").expect("pin still present");
        assert_eq!(pin.state, "closed");
        assert!(pin
            .close_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("superseded")));
    }

    /// A CONFIRMED record on the terminal (a session that really ran there
    /// before Codex) is not a phantom the store reaps; the capture still closes
    /// it, so the terminal is left with exactly the Codex record.
    #[test]
    fn record_captured_session_closes_a_confirmed_earlier_record_too() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        crate::commands::terminal::record_pinned_session_open(
            &store,
            "earlier".to_string(),
            "term-5".to_string(),
            None,
            "/w".to_string(),
            "w".to_string(),
            "default".to_string(),
            0,
            qontinui_runner_lib::cli_profile::claude::ID.to_string(),
        );
        store.confirm_session("earlier");

        record_captured_session(&store, "codex-x", "term-5", None, "/w");

        let open: Vec<_> = store
            .open_records()
            .into_iter()
            .filter(|r| r.terminal_id == "term-5")
            .collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].claude_session_id, "codex-x");
        assert_eq!(
            store.get("earlier").map(|r| r.state),
            Some("closed".to_string())
        );
    }

    /// The async entry point finds a rollout that appears after it starts,
    /// records it, and reports the id; a capture that never matches gives up
    /// without recording or reporting anything.
    #[tokio::test]
    async fn capture_and_record_reports_the_id_it_recorded() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("codex-home");
        let day = sessions_root(&home).join("2026").join("10").join("03");
        fs::create_dir_all(&day).unwrap();
        let line =
            r#"{"type":"session_meta","payload":{"session_id":"live-1","cwd":"/work/proj"}}"#;
        fs::write(day.join("rollout-2026-10-03T10-00-00-live-1.jsonl"), line).unwrap();
        let store =
            std::sync::Arc::new(SessionLifecycleStore::open(dir.path().join("s.json")).unwrap());

        let reported = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = reported.clone();
        capture_and_record_by_cwd(
            store.clone(),
            CaptureStart {
                terminal_id: "term-a".to_string(),
                cwd: "/work/proj".to_string(),
                codex_home: Some(home.to_string_lossy().into_owned()),
                started_ms: 0,
                resume: None,
            },
            Duration::from_millis(1),
            Duration::from_millis(200),
            move |id, account| *sink.lock().unwrap() = Some((id.to_string(), account.to_string())),
        )
        .await;
        let (id, account) = reported.lock().unwrap().clone().expect("reported");
        assert_eq!(id, "live-1");
        assert_eq!(account, home.to_string_lossy());
        assert_eq!(
            store.get("live-1").map(|r| r.provider),
            Some(codex::ID.to_string())
        );

        let never = std::sync::Arc::new(std::sync::Mutex::new(false));
        let flag = never.clone();
        capture_and_record_by_cwd(
            store.clone(),
            CaptureStart {
                terminal_id: "term-b".to_string(),
                cwd: "/elsewhere".to_string(),
                codex_home: Some(home.to_string_lossy().into_owned()),
                started_ms: 0,
                resume: None,
            },
            Duration::from_millis(1),
            Duration::from_millis(20),
            move |_, _| *flag.lock().unwrap() = true,
        )
        .await;
        assert!(!*never.lock().unwrap());
        assert!(store
            .open_records()
            .iter()
            .all(|r| r.terminal_id != "term-b"));
    }
}
