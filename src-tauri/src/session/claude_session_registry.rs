//! Reader for **Claude Code's own live-session registry** — the authoritative
//! source for the name an operator sees in a session window and in `/resume`.
//!
//! Claude Code writes one JSON file per running interactive process:
//!
//! ```text
//! <config_dir>/sessions/<pid>.json
//! {"pid":2804,"sessionId":"b770ae37-…","cwd":"D:\\qontinui-root",
//!  "startedAt":1784712055852,"version":"2.1.217","peerProtocol":1,
//!  "kind":"interactive","entrypoint":"cli",
//!  "name":"per-agent coord-mcp proxy","status":"idle",
//!  "updatedAt":…,"statusUpdatedAt":…}
//! ```
//!
//! `name` is *exactly* the window/`/resume` string. Verified against
//! operator-supplied ground truth on 2026-07-23.
//!
//! # Why this module exists (and why the sibling readers are not enough)
//!
//! [`super::past_sessions`] derives a `resume_name` by scraping the transcript
//! (last `/rename` custom-title → auto-summary → first-user-message preview →
//! registry title). Measured against this registry on 2026-07-23 that
//! derivation was wrong on both axes:
//!
//! | | |
//! |---|---|
//! | sessions in Claude's registry | 80 |
//! | known to the runner's lifecycle store | 33 |
//! | of those 33, name matched the window name | **11** |
//!
//! The transcript preview is simply a *different string* from the window name
//! (`"PR qontinui/qontinui-coord#1151 just merged…"` vs `"qontinui-coord-73"`),
//! so no amount of fallback-chain tuning reconciles them. Read the registry.
//!
//! # Scope and limits
//!
//! - **Live processes only.** Claude Code removes the file when a process
//!   exits, so this answers "what is open right now" and says nothing about
//!   already-dead sessions. That is the right shape for the write-down →
//!   rebuild → resume loop; use `past_sessions` for history.
//! - **A session id may have several live processes.** 22 of 80 did at time of
//!   writing, each carrying its own auto-generated `<dir>-<2hex>` name. The
//!   registry is keyed by PID, so this reader returns one entry per *process*
//!   and lets the caller collapse by `session_id` if it wants.
//! - **Liveness is re-checked here.** A crashed process cannot clean up after
//!   itself, so an entry whose PID is absent from the live process table is
//!   dropped rather than reported as open.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::past_sessions::{account_from_config_dir, PastSessionAccount};

/// Raw on-disk shape of `<config_dir>/sessions/<pid>.json`.
///
/// Only the fields this reader needs are modelled; Claude Code adds others
/// (`version`, `peerProtocol`, `entrypoint`, `statusUpdatedAt`) that are
/// deliberately ignored so a new key in a future CLI release cannot break
/// deserialization.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryFile {
    pid: u32,
    session_id: String,
    /// Absent on very old CLI builds — such a row is unusable for display and
    /// is dropped by [`parse_registry_file`].
    name: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    updated_at: Option<i64>,
}

/// One live Claude Code process, as the operator sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveClaudeSession {
    /// The `--resume` key.
    pub session_id: String,
    /// The name shown in the session window and in `/resume`. Ground truth.
    pub name: String,
    /// OS process id that reported this entry.
    pub pid: u32,
    /// Account + CLI wrapper derived from the owning config dir.
    pub account: PastSessionAccount,
    /// Working directory the process was launched in. Forward slashes, so the
    /// value drops straight into a shell command on Windows.
    pub working_dir: String,
    /// Claude Code's self-reported status (`idle` / `busy` / `waiting` / …).
    pub status: String,
    /// `interactive` for real windows.
    pub kind: String,
    pub started_at: i64,
    pub updated_at: i64,
    /// Ready-to-run: `cd '<dir>' && <wrapper> --resume <id>`.
    ///
    /// The `cd` is **not** cosmetic — Claude Code scopes sessions by project
    /// directory, so resuming from the wrong cwd does not find the session.
    pub resume_command: String,
}

/// Parse one registry file's bytes. Returns `None` when the JSON is malformed
/// or carries no `name` — a nameless row cannot serve this module's purpose.
fn parse_registry_file(bytes: &str, config_dir: &Path) -> Option<LiveClaudeSession> {
    let raw: RegistryFile = serde_json::from_str(bytes).ok()?;
    let name = raw.name?;
    if raw.session_id.is_empty() {
        return None;
    }
    let account = account_from_config_dir(config_dir.to_str());
    // Normalize separators so the emitted `cd` works verbatim in the POSIX-ish
    // shells the operator pastes into (Git Bash, the runner's own PTY).
    let working_dir = raw.cwd.unwrap_or_default().replace('\\', "/");
    let resume_command = if working_dir.is_empty() {
        format!("{} --resume {}", account.wrapper, raw.session_id)
    } else {
        format!(
            "cd '{}' && {} --resume {}",
            working_dir, account.wrapper, raw.session_id
        )
    };
    Some(LiveClaudeSession {
        session_id: raw.session_id,
        name,
        pid: raw.pid,
        account,
        working_dir,
        status: raw.status.unwrap_or_else(|| "unknown".to_string()),
        kind: raw.kind.unwrap_or_else(|| "interactive".to_string()),
        started_at: raw.started_at.unwrap_or(0),
        updated_at: raw.updated_at.unwrap_or(0),
        resume_command,
    })
}

/// Read every `<config_dir>/sessions/*.json`, keeping only entries whose PID is
/// in `live_pids`.
///
/// Fail-open per file: an unreadable or malformed entry is skipped rather than
/// failing the whole listing — one corrupt file must not hide 118 good ones.
/// Results are sorted by account then name so the output is stable between
/// calls (PID iteration order is not).
pub fn read_live_sessions(
    config_dirs: &[std::path::PathBuf],
    live_pids: &HashSet<u32>,
) -> Vec<LiveClaudeSession> {
    let mut out = Vec::new();
    for dir in config_dirs {
        let sessions_dir = dir.join("sessions");
        let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
            continue; // no sessions dir on this account — normal
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(session) = parse_registry_file(&bytes, dir) else {
                continue;
            };
            if !live_pids.contains(&session.pid) {
                continue; // stale file left by a crashed process
            }
            out.push(session);
        }
    }
    out.sort_by(|a, b| {
        a.account
            .label
            .cmp(&b.account.label)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("claude-reg-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(".claude-paktis").join("sessions")).unwrap();
        d
    }

    const SAMPLE: &str = r#"{"pid":2804,"sessionId":"b770ae37-1ffa-4888-a5d1-89d058307adf","cwd":"D:\\qontinui-root","startedAt":1784712055852,"version":"2.1.217","peerProtocol":1,"kind":"interactive","entrypoint":"cli","name":"per-agent coord-mcp proxy","status":"idle","updatedAt":1784770342016,"statusUpdatedAt":1784770342016}"#;

    #[test]
    fn parses_the_operator_ground_truth_row() {
        // Forward slashes deliberately: `Path::file_name` treats `\` as an
        // ordinary character on Linux, so a Windows-style `C:\claude\...`
        // literal yields the WHOLE string as the file name there, the
        // `.claude-` prefix never matches, and the account silently degrades
        // to `unknown`/`claude`. Forward slashes parse identically on both
        // platforms, so this asserts the real mapping in CI as well as locally.
        let s = parse_registry_file(SAMPLE, Path::new("C:/claude/.claude-paktis")).unwrap();
        assert_eq!(s.name, "per-agent coord-mcp proxy");
        assert_eq!(s.session_id, "b770ae37-1ffa-4888-a5d1-89d058307adf");
        assert_eq!(s.pid, 2804);
        assert_eq!(s.account.label, "paktis");
        assert_eq!(s.account.wrapper, "clp");
        assert_eq!(s.working_dir, "D:/qontinui-root");
        assert_eq!(s.status, "idle");
        assert_eq!(
            s.resume_command,
            "cd 'D:/qontinui-root' && clp --resume b770ae37-1ffa-4888-a5d1-89d058307adf"
        );
    }

    #[test]
    fn unknown_future_keys_do_not_break_parsing() {
        let json = r#"{"pid":1,"sessionId":"s","name":"n","cwd":"/x","brandNewKey":{"a":[1,2]}}"#;
        assert!(parse_registry_file(json, Path::new(".claude-gmail")).is_some());
    }

    #[test]
    fn rejects_nameless_and_malformed_rows() {
        let nameless = r#"{"pid":1,"sessionId":"s","cwd":"/x"}"#;
        assert!(parse_registry_file(nameless, Path::new(".claude-gmail")).is_none());
        assert!(parse_registry_file("{not json", Path::new(".claude-gmail")).is_none());
        let empty_id = r#"{"pid":1,"sessionId":"","name":"n"}"#;
        assert!(parse_registry_file(empty_id, Path::new(".claude-gmail")).is_none());
    }

    #[test]
    fn omits_cd_when_cwd_is_absent() {
        let json = r#"{"pid":7,"sessionId":"abc","name":"n"}"#;
        let s = parse_registry_file(json, Path::new(".claude-gmail")).unwrap();
        assert_eq!(s.resume_command, "clg --resume abc");
    }

    #[test]
    fn drops_entries_whose_pid_is_not_live() {
        let root = tmpdir("stale");
        let acct = root.join(".claude-paktis");
        fs::write(acct.join("sessions").join("2804.json"), SAMPLE).unwrap();

        let none: HashSet<u32> = HashSet::new();
        assert!(read_live_sessions(&[acct.clone()], &none).is_empty());

        let live: HashSet<u32> = [2804].into_iter().collect();
        assert_eq!(read_live_sessions(&[acct], &live).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn one_corrupt_file_does_not_hide_the_others() {
        let root = tmpdir("corrupt");
        let acct = root.join(".claude-paktis");
        let sdir = acct.join("sessions");
        fs::write(sdir.join("2804.json"), SAMPLE).unwrap();
        fs::write(sdir.join("9999.json"), "{{{ truncated").unwrap();
        fs::write(sdir.join("notes.txt"), SAMPLE).unwrap(); // non-json ignored

        let live: HashSet<u32> = [2804, 9999].into_iter().collect();
        let got = read_live_sessions(&[acct], &live);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "per-agent coord-mcp proxy");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_sessions_dir_is_not_an_error() {
        let root = tmpdir("nosessions");
        let bare = root.join(".claude-hotmail");
        fs::create_dir_all(&bare).unwrap();
        let live: HashSet<u32> = [1].into_iter().collect();
        assert!(read_live_sessions(&[bare], &live).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn several_live_processes_may_share_one_session_id() {
        // 22 of 80 session ids did at time of writing. Each process keeps its
        // own auto-generated name, so the reader must return BOTH rows rather
        // than collapsing them — the caller decides how to present it.
        let root = tmpdir("shared");
        let acct = root.join(".claude-gmail");
        fs::create_dir_all(acct.join("sessions")).unwrap();
        let mk = |pid: u32, name: &str| {
            format!(
                r#"{{"pid":{pid},"sessionId":"same-id","name":"{name}","cwd":"/w","status":"idle"}}"#
            )
        };
        fs::write(
            acct.join("sessions").join("10.json"),
            mk(10, "qontinui-web-0c"),
        )
        .unwrap();
        fs::write(
            acct.join("sessions").join("11.json"),
            mk(11, "qontinui-web-07"),
        )
        .unwrap();

        let live: HashSet<u32> = [10, 11].into_iter().collect();
        let got = read_live_sessions(&[acct], &live);
        assert_eq!(got.len(), 2);
        assert_eq!(got.iter().filter(|s| s.session_id == "same-id").count(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
