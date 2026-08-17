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
    /// Absent on very old CLI builds (or a registry write we caught before the
    /// field landed). Such a row is degraded for DISPLAY but its `sessionId` +
    /// `pid` still prove liveness, so [`parse_registry_file`] KEEPS it with an
    /// empty name rather than dropping it — silently removing a live id from
    /// the liveness oracle would let the restore path respawn (fork) it.
    name: Option<String>,
    /// How Claude Code arrived at `name`. `"derived"` means *it* invented the
    /// name from the cwd (`qontinui-root-ec`, `qontinui-coord-88`); an **absent**
    /// key means an operator `/rename` (or a launch-time name) supplied it.
    ///
    /// Kept `Option` + `serde(default)` because most rows omit it entirely (12
    /// of the 17 named rows on the operator's box on 2026-07-28), and because a
    /// future CLI may add further variants. Consumers must treat any value
    /// other than `"derived"` — including absent — as operator-chosen.
    #[serde(default)]
    name_source: Option<String>,
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
    /// Empty when the registry file carried no `name` (very old CLI builds) —
    /// the row is still a LIVE session and must count for liveness; display
    /// callers substitute a placeholder.
    pub name: String,
    /// Provenance of [`Self::name`], verbatim from the registry file
    /// (`nameSource`). `Some("derived")` marks Claude Code's own
    /// `<dir>-<2hex>` auto-derivation, which is *weaker* than the names the
    /// runner's other surfaces already show — callers must not let such a name
    /// preempt an existing one. `None` (the common case) and any other value
    /// mean the name is operator-chosen and should win.
    ///
    /// Deliberately **not** filtered here: [`read_live_sessions`] is also the
    /// backing read for the `/copy-names` listing, which wants every live
    /// session regardless of provenance. The precedence decision belongs at the
    /// display site.
    pub name_source: Option<String>,
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

impl LiveClaudeSession {
    /// Project this live row into the durable-record identity overlay (D1).
    ///
    /// The one place the live→durable field mapping lives, so the confirmation
    /// hook and the liveness poll cannot drift apart on it.
    ///
    /// A NAMELESS row (old CLI builds write no `name`) contributes neither
    /// `session_name` nor `name_source`: it is still a live session and still
    /// carries a real account, but claiming a provenance for a name that does
    /// not exist would be an invented value. The account is always contributed
    /// — it is derived from the owning config dir, which is always known here.
    pub fn identity_update(
        &self,
    ) -> crate::session::session_lifecycle_store::SessionIdentityUpdate {
        use crate::session::session_lifecycle_store::{durable_name_source, SessionIdentityUpdate};
        let named = !self.name.trim().is_empty();
        SessionIdentityUpdate {
            account_label: Some(self.account.label.clone()),
            account_wrapper: Some(self.account.wrapper.clone()),
            session_name: named.then(|| self.name.clone()),
            name_source: named.then(|| durable_name_source(self.name_source.as_deref())),
            ..Default::default()
        }
    }
}

/// Parse one registry file's bytes. Returns `None` only when the JSON is
/// malformed or carries no usable `sessionId` — nothing to be live *as*.
///
/// A row WITHOUT a `name` is kept (empty-string name): this reader doubles as
/// the restore path's liveness oracle, and for that purpose a sessionId-bearing
/// row with a live pid IS a live session regardless of what it is called.
/// Dropping it would silently remove a live id from the oracle and let the
/// restore path respawn — fork — that session. Display callers (`/copy-names`)
/// substitute a placeholder for the empty name.
fn parse_registry_file(bytes: &str, config_dir: &Path) -> Option<LiveClaudeSession> {
    let raw: RegistryFile = serde_json::from_str(bytes).ok()?;
    let name = raw.name.unwrap_or_default();
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
        name_source: raw.name_source,
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

/// Extract the live-PID set from a process-table snapshot, FAILING CLOSED on
/// an empty table.
///
/// The runner itself is always a live process, so on a healthy read the table
/// can never be empty — emptiness means the snapshot helper failed (the same
/// failure mode `main.rs`'s liveness poll guards with `tick_snapshot_ok`).
/// Returning an empty set here would make [`read_live_sessions`] filter EVERY
/// registry row out, and the command would report success + `[]` — which the
/// frontend restore oracle reads as "definitively no live sessions" and
/// respawns (forks) everything. An indeterminate read must surface as an
/// error so the frontend maps it to `null` → skip-unknown instead.
pub fn live_pids_from_snapshot(
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> Result<HashSet<u32>, String> {
    if snapshot.names.is_empty() {
        return Err(
            "process-table snapshot came back empty (the runner itself is always live, so an \
             empty table is a failed read) — session liveness is indeterminate"
                .to_string(),
        );
    }
    Ok(snapshot.names.keys().copied().collect())
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

/// Find the live-registry row for ONE session id, WITHOUT taking a
/// process-table snapshot.
///
/// [`read_live_sessions`] filters by live PID, which costs a full process-table
/// snapshot (a PowerShell / `/proc` sweep). That price is right for "list
/// everything open right now" but wrong for the identity stamp on
/// `POST /control/session-open`, which runs on the provider's STARTUP path and
/// must not add a system scan to it.
///
/// The trade is stated rather than hidden: without the PID filter a stale file
/// left by a crashed process can be returned. It cannot name a DIFFERENT
/// session — the correlation key is the session id itself — so the worst case
/// is a slightly outdated name for the right session, which the 45s liveness
/// poll then refreshes. That is strictly better than the alternative (no name
/// at all until the first poll tick).
///
/// Returns `None` when no file names the id — the common case at startup, since
/// Claude Code may not have written its registry file by the time the
/// SessionStart hook fires. `None` means UNKNOWN: callers must leave the stored
/// value alone (the sticky rule), never blank it.
///
/// When several files name the same id (22 of 80 did on the operator's box),
/// an OPERATOR-named row wins over a `nameSource: "derived"` one — a `/rename`
/// is the string the operator actually sees, and letting an auto-name preempt
/// it is the precedence bug this module's doc comment warns about.
pub fn find_live_session_by_id(
    config_dirs: &[std::path::PathBuf],
    session_id: &str,
) -> Option<LiveClaudeSession> {
    let mut best: Option<LiveClaudeSession> = None;
    for dir in config_dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("sessions")) else {
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
            if session.session_id != session_id {
                continue;
            }
            let operator_named = session.name_source.as_deref() != Some("derived");
            let wins = match &best {
                None => true,
                // Only an operator-named row displaces an already-held one, and
                // only when what it displaces is a derived auto-name.
                Some(prev) => operator_named && prev.name_source.as_deref() == Some("derived"),
            };
            if wins {
                best = Some(session);
            }
        }
    }
    best
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
    fn name_source_is_carried_through_verbatim() {
        // The operator's box on 2026-07-28: 5 of 17 named rows look like this —
        // Claude Code's own `<dir>-<2hex>` auto-derivation. The reader must
        // surface the provenance (and must NOT drop the row) so the display
        // sites can refuse to let it preempt a better name.
        let derived = r#"{"pid":198780,"sessionId":"6c71bd20","cwd":"D:\\qontinui-root","kind":"interactive","entrypoint":"sdk-cli","name":"qontinui-coord-c7","nameSource":"derived"}"#;
        let s = parse_registry_file(derived, Path::new("C:/claude/.claude-qontinui")).unwrap();
        assert_eq!(s.name, "qontinui-coord-c7");
        assert_eq!(s.name_source.as_deref(), Some("derived"));

        // The other 12 carry no `nameSource` key at all and hold real operator
        // names — absent means operator-chosen, not unknown.
        let operator_named = r#"{"pid":1,"sessionId":"s","name":"worktree prune","cwd":"/x"}"#;
        let s = parse_registry_file(operator_named, Path::new(".claude-gmail")).unwrap();
        assert_eq!(s.name, "worktree prune");
        assert_eq!(s.name_source, None);
    }

    /// The D1 projection: a named row contributes account + name + a RESOLVED
    /// provenance; a nameless one contributes only the account, never an
    /// invented name or a provenance for a name that does not exist.
    #[test]
    fn identity_update_projects_account_and_resolves_name_provenance() {
        let operator = parse_registry_file(SAMPLE, Path::new("C:/claude/.claude-paktis")).unwrap();
        let u = operator.identity_update();
        assert_eq!(u.account_label.as_deref(), Some("paktis"));
        assert_eq!(u.account_wrapper.as_deref(), Some("clp"));
        assert_eq!(u.session_name.as_deref(), Some("per-agent coord-mcp proxy"));
        // Absent `nameSource` resolves to an explicit operator marker, so the
        // durable record's `None` keeps its one meaning: "never observed".
        assert_eq!(u.name_source.as_deref(), Some("operator"));
        // Not this projection's business — the D1 fields it cannot know stay None.
        assert_eq!(u.tenant_id, None);
        assert_eq!(u.task_run_id, None);
        assert_eq!(u.bypass_permissions, None);

        let derived = parse_registry_file(
            r#"{"pid":1,"sessionId":"s","name":"qontinui-root-ec","nameSource":"derived"}"#,
            Path::new("C:/claude/.claude-gmail"),
        )
        .unwrap();
        let u = derived.identity_update();
        assert_eq!(u.name_source.as_deref(), Some("derived"));
        assert_eq!(u.account_label.as_deref(), Some("gmail"));

        // A nameless row (old CLI build) is still a live session with a real
        // account, but contributes no name and no provenance.
        let nameless = parse_registry_file(
            r#"{"pid":1,"sessionId":"s"}"#,
            Path::new("C:/x/.claude-gmail"),
        )
        .unwrap();
        let u = nameless.identity_update();
        assert_eq!(u.account_label.as_deref(), Some("gmail"));
        assert_eq!(u.session_name, None);
        assert_eq!(u.name_source, None);
    }

    /// `find_live_session_by_id` locates ONE session by id without a process
    /// snapshot, prefers an operator-named row over a derived one when several
    /// processes report the same id, and answers `None` (UNKNOWN) for an id no
    /// file names.
    #[test]
    fn find_live_session_by_id_prefers_the_operator_named_row() {
        let root = tmpdir("find-by-id");
        let acct = root.join(".claude-paktis");
        let sessions = acct.join("sessions");
        // Same session id reported by two live processes — the shape 22 of 80
        // rows had on the operator's box.
        fs::write(
            sessions.join("11.json"),
            r#"{"pid":11,"sessionId":"dup","name":"qontinui-root-ec","nameSource":"derived","cwd":"/x"}"#,
        )
        .unwrap();
        fs::write(
            sessions.join("12.json"),
            r#"{"pid":12,"sessionId":"dup","name":"merge train steward","cwd":"/x"}"#,
        )
        .unwrap();
        fs::write(
            sessions.join("13.json"),
            r#"{"pid":13,"sessionId":"other","name":"unrelated","cwd":"/x"}"#,
        )
        .unwrap();

        let dirs = vec![acct.clone()];
        let found = find_live_session_by_id(&dirs, "dup").expect("id is present");
        assert_eq!(
            found.name, "merge train steward",
            "an operator name must not be preempted by Claude's auto-name"
        );
        assert_eq!(found.account.label, "paktis");

        assert_eq!(
            find_live_session_by_id(&dirs, "other").unwrap().name,
            "unrelated"
        );
        assert!(
            find_live_session_by_id(&dirs, "absent").is_none(),
            "an id no file names is UNKNOWN, not an empty session"
        );
        // A config dir with no sessions dir is normal, not an error.
        assert!(find_live_session_by_id(&[root.join(".claude-nothing")], "dup").is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn name_source_survives_the_json_round_trip_as_camel_case() {
        // The frontend gate reads `nameSource`; a snake_case key there would
        // silently disable it (every row would look operator-named).
        let derived =
            r#"{"pid":1,"sessionId":"s","name":"qontinui-root-ec","nameSource":"derived"}"#;
        let s = parse_registry_file(derived, Path::new(".claude-gmail")).unwrap();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["nameSource"], "derived");

        let plain = r#"{"pid":1,"sessionId":"s","name":"worktree prune"}"#;
        let s = parse_registry_file(plain, Path::new(".claude-gmail")).unwrap();
        let json = serde_json::to_value(&s).unwrap();
        assert!(json["nameSource"].is_null());
    }

    #[test]
    fn derived_rows_are_still_listed() {
        // `/copy-names` wants every live session; only the display sites apply
        // the precedence gate.
        let root = tmpdir("derived");
        let acct = root.join(".claude-paktis");
        fs::write(
            acct.join("sessions").join("42.json"),
            r#"{"pid":42,"sessionId":"d","name":"qontinui-root-ec","nameSource":"derived"}"#,
        )
        .unwrap();
        let live: HashSet<u32> = [42].into_iter().collect();
        let got = read_live_sessions(&[acct], &live);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name_source.as_deref(), Some("derived"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_future_keys_do_not_break_parsing() {
        let json = r#"{"pid":1,"sessionId":"s","name":"n","cwd":"/x","brandNewKey":{"a":[1,2]}}"#;
        assert!(parse_registry_file(json, Path::new(".claude-gmail")).is_some());
    }

    #[test]
    fn rejects_malformed_and_idless_rows() {
        assert!(parse_registry_file("{not json", Path::new(".claude-gmail")).is_none());
        let empty_id = r#"{"pid":1,"sessionId":"","name":"n"}"#;
        assert!(parse_registry_file(empty_id, Path::new(".claude-gmail")).is_none());
    }

    #[test]
    fn keeps_nameless_rows_for_liveness() {
        // A sessionId-bearing row with a live pid IS a live session even when
        // `name` is absent (old CLI build). Dropping it fail-open would remove
        // a live id from the restore oracle → respawn-while-alive fork.
        let nameless = r#"{"pid":1,"sessionId":"s","cwd":"/x"}"#;
        let s = parse_registry_file(nameless, Path::new(".claude-gmail")).unwrap();
        assert_eq!(s.session_id, "s");
        assert_eq!(s.name, "");
        assert_eq!(s.resume_command, "cd '/x' && clg --resume s");
    }

    #[test]
    fn nameless_row_with_live_pid_appears_in_read_live_sessions() {
        let root = tmpdir("nameless");
        let acct = root.join(".claude-paktis");
        fs::write(
            acct.join("sessions").join("42.json"),
            r#"{"pid":42,"sessionId":"live-id","cwd":"/w","status":"idle"}"#,
        )
        .unwrap();

        let live: HashSet<u32> = [42].into_iter().collect();
        let got = read_live_sessions(&[acct], &live);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id, "live-id");
        assert_eq!(got[0].name, "");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_process_snapshot_is_a_failed_read_not_an_empty_pid_set() {
        // FAIL CLOSED: the runner itself is always live, so an empty table is
        // impossible on a healthy read. success + [] would tell the frontend
        // "definitively nothing alive" → respawn (fork) everything.
        use crate::process_capture::process_tree::ProcessSnapshot;
        let empty = ProcessSnapshot::default();
        assert!(live_pids_from_snapshot(&empty).is_err());

        let mut ok = ProcessSnapshot::default();
        ok.names.insert(1234, "claude.exe".to_string());
        let pids = live_pids_from_snapshot(&ok).unwrap();
        assert!(pids.contains(&1234));
        assert_eq!(pids.len(), 1);
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
