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
//! (last `/rename` custom-title → `ai-title` → auto-summary →
//! first-user-message preview → registry title). Measured against this
//! registry on 2026-07-23 that
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
//! - **A session id may have several rows.** "22 of 80" session ids did when
//!   this was first written (2026-07-23) — but that count filtered rows by PID
//!   membership only, with NO `procStart` check, so it counted stale files
//!   whose pid Windows had since handed to another process. The 2026-10-21
//!   census (plan
//!   `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
//!   Phase 1 findings) found 3 duplicate groups over 40 raw rows and **1** over
//!   the 19 VALID rows — and that one was a true fork. The registry is keyed by
//!   PID, so this reader returns one entry per *file* and lets the caller
//!   collapse by `session_id` if it wants; a caller that needs to know whether
//!   a row is evidence uses [`registry_row_validity`] / [`read_registry_rows`].
//! - **Liveness is re-checked here.** A crashed process cannot clean up after
//!   itself, so an entry whose PID is absent from the live process table is
//!   dropped rather than reported as open. PID membership alone does not prove
//!   the row is the process's own (pid reuse) — that is what
//!   [`registry_row_validity`] adds.

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
    /// The writing process's creation time in the [`Self::pid_domain`]'s clock
    /// — on Windows a FILETIME (100-ns ticks since 1601 UTC) as a decimal
    /// STRING (`"procStart":"134369923919058648"`). Absent on older CLI builds,
    /// which makes the row's validity UNKNOWN (see [`registry_row_validity`]).
    #[serde(default)]
    proc_start: Option<String>,
    /// `"win32:<host>"` on Windows. Names the clock `proc_start` is in.
    #[serde(default)]
    pid_domain: Option<String>,
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

// ---------------------------------------------------------------------------
// Registry-row VALIDITY — the one answer to "is this row evidence?"
// ---------------------------------------------------------------------------

/// Tolerance, in FILETIME ticks (100 ns), between a row's `procStart` and the
/// snapshot's creation time for the same pid. CIM truncates `CreationDate` to
/// the microsecond (10 ticks) while Claude Code writes the full-resolution
/// value, so a VALID row differs by 0-9 ticks; a reused pid differs by days.
/// `< 10` is "the same microsecond", which also absorbs a rounding (rather
/// than truncating) source.
pub const PROC_START_TOLERANCE_TICKS: u64 = 10;

/// Why a row is positively NOT evidence of a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleRow {
    /// The pid is not a countable live claude in the snapshot (dead, reused by
    /// a non-claude image, or a qontinui shim).
    PidNotLiveClaude,
    /// The pid IS a live claude, but its creation time is not the row's
    /// `procStart` — the pid was reused by a later claude (2026-10-21: pid
    /// 38920 was a live claude for paktis `f2a062c0` while a stale tiohorst
    /// row still named it with `b4c069ec`).
    ProcStartMismatch,
}

/// Why a row's validity could not be established either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownRow {
    /// The process table read failed (empty snapshot).
    ProcessTableUnreadable,
    /// The row carries no `procStart` (older CLI) — the pid is a live claude,
    /// but nothing proves the row is that process's own.
    NoProcStart,
    /// `procStart` is not a number.
    MalformedProcStart,
    /// `pidDomain` names a clock this runner does not compare against.
    UnsupportedPidDomain,
    /// The snapshot has no comparable creation time for the pid (non-Windows,
    /// or CIM reported none).
    CreationTimeUnavailable,
}

impl UnknownRow {
    pub fn as_str(self) -> &'static str {
        match self {
            UnknownRow::ProcessTableUnreadable => "process table unreadable",
            UnknownRow::NoProcStart => "registry row carries no procStart",
            UnknownRow::MalformedProcStart => "registry row procStart is malformed",
            UnknownRow::UnsupportedPidDomain => "registry row pidDomain is not comparable",
            UnknownRow::CreationTimeUnavailable => "process creation time unavailable",
        }
    }
}

/// Three-valued verdict on one `<config_dir>/sessions/<pid>.json` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowValidity {
    /// The row is the live claude process's own — evidence.
    Valid,
    /// Positively not evidence — ignore the row.
    Stale(StaleRow),
    /// Cannot tell. Never evidence FOR an action.
    Unknown(UnknownRow),
}

impl RowValidity {
    pub fn is_valid(self) -> bool {
        matches!(self, RowValidity::Valid)
    }
}

/// THE registry-row validity rule (plan
/// `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
/// Phase 3 "Registry-row validity"; shared by the migration identity check,
/// the binder's provider rung and the duplicate-host detector).
///
/// A row is VALID iff (a) its pid is a countable claude in `snapshot`
/// ([`is_countable_claude`](crate::process_capture::process_tree::is_countable_claude))
/// AND (b) its `procStart` is that pid's creation time to the microsecond
/// ([`PROC_START_TOLERANCE_TICKS`]). Registry `status`/`updatedAt` are NOT
/// liveness signals and are not read (a working pid read `busy` for 4 h).
///
/// Order matters: an unreadable table is UNKNOWN before anything else; a pid
/// that is not a live claude is STALE even when the row has no `procStart`
/// (no procStart could make a dead pid live).
pub fn registry_row_validity(
    pid: u32,
    proc_start: Option<&str>,
    pid_domain: Option<&str>,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> RowValidity {
    if snapshot.names.is_empty() {
        return RowValidity::Unknown(UnknownRow::ProcessTableUnreadable);
    }
    if !crate::process_capture::process_tree::is_countable_claude(pid, snapshot) {
        return RowValidity::Stale(StaleRow::PidNotLiveClaude);
    }
    let Some(raw) = proc_start.map(str::trim).filter(|s| !s.is_empty()) else {
        return RowValidity::Unknown(UnknownRow::NoProcStart);
    };
    // Only the Windows FILETIME clock is compared today. An absent domain is
    // accepted: the value is compared against the FILETIME map, which is empty
    // off Windows, so it degrades to UNKNOWN there rather than mismatching.
    if let Some(domain) = pid_domain {
        if !domain.starts_with("win32:") {
            return RowValidity::Unknown(UnknownRow::UnsupportedPidDomain);
        }
    }
    let Ok(row_ticks) = raw.parse::<u64>() else {
        return RowValidity::Unknown(UnknownRow::MalformedProcStart);
    };
    let Some(&snap_ticks) = snapshot.creation_filetimes.get(&pid) else {
        return RowValidity::Unknown(UnknownRow::CreationTimeUnavailable);
    };
    if row_ticks.abs_diff(snap_ticks) < PROC_START_TOLERANCE_TICKS {
        RowValidity::Valid
    } else {
        RowValidity::Stale(StaleRow::ProcStartMismatch)
    }
}

/// One registry file with its validity against ONE process-table snapshot —
/// the input the migration identity check and the duplicate-host detector
/// decide over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRow {
    pub pid: u32,
    pub session_id: String,
    /// The owning account's config dir (the dir the `sessions/` lives in).
    pub config_dir: std::path::PathBuf,
    /// `kind` verbatim (`interactive` for a real window); `None` when absent.
    pub kind: Option<String>,
    pub validity: RowValidity,
}

impl RegistryRow {
    /// `kind == "interactive"` — the class the 2026-10-21 census showed forks
    /// live in. An ABSENT kind counts as interactive, matching
    /// [`parse_registry_file`]'s default: dropping it would hide a live window.
    pub fn is_interactive(&self) -> bool {
        self.kind.as_deref().is_none_or(|k| k == "interactive")
    }
}

/// Parse one registry file into a [`RegistryRow`], validated against
/// `snapshot`. `None` for malformed JSON or an empty `sessionId`.
pub fn parse_registry_row(
    bytes: &str,
    config_dir: &Path,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> Option<RegistryRow> {
    let raw: RegistryFile = serde_json::from_str(bytes).ok()?;
    if raw.session_id.trim().is_empty() {
        return None;
    }
    let validity = registry_row_validity(
        raw.pid,
        raw.proc_start.as_deref(),
        raw.pid_domain.as_deref(),
        snapshot,
    );
    Some(RegistryRow {
        pid: raw.pid,
        session_id: raw.session_id,
        config_dir: config_dir.to_path_buf(),
        kind: raw.kind,
        validity,
    })
}

/// Read EVERY `<config_dir>/sessions/*.json` (no pid filter) and validate each
/// against `snapshot`. Fail-open per file like [`read_live_sessions`]. An
/// unreadable process table is NOT fail-open: every row then reads
/// [`UnknownRow::ProcessTableUnreadable`], which is how a consumer tells "no
/// evidence" from "no duplicates". Sorted by `(config_dir, pid)`.
pub fn read_registry_rows(
    config_dirs: &[std::path::PathBuf],
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> Vec<RegistryRow> {
    let mut out = Vec::new();
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
            if let Some(row) = parse_registry_row(&bytes, dir, snapshot) {
                out.push(row);
            }
        }
    }
    out.sort_by(|a, b| a.config_dir.cmp(&b.config_dir).then(a.pid.cmp(&b.pid)));
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
/// When several files name the same id (a "22 of 80" count once said so, measured without a procStart check — see the module doc),
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
        // Same session id reported by two live processes — the shape the uncorrected "22 of 80" count described
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
        // The unchecked "22 of 80" count (module doc) — at least one true fork per the 2026-10-21 census. Each process keeps its
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

    // ── Registry-row validity (plan 2026-10-21 Phase 3) ──────────────────

    use crate::process_capture::process_tree::ProcessSnapshot;

    /// A snapshot holding `claude.exe` pids with the given CIM (µs-truncated)
    /// FILETIME creation times, plus one non-claude process so the table is
    /// never empty.
    fn snap_with(claudes: &[(u32, u64)]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.names.insert(1, "System".to_string());
        for (pid, ft) in claudes {
            s.names.insert(*pid, "claude.exe".to_string());
            s.creation_filetimes.insert(*pid, *ft);
        }
        s
    }

    const W: Option<&str> = Some("win32:spaceship");

    /// Measured 2026-10-21: registry `…058648`, CIM `…058640` — the same
    /// process, 8 ticks apart. VALID. A 10-tick (1 µs) gap is not.
    ///
    /// Mutation check: changing `< PROC_START_TOLERANCE_TICKS` to `== 0`
    /// fails the first assertion; to `<= 10` fails the boundary one.
    #[test]
    fn validity_matches_at_microsecond_granularity() {
        let snap = snap_with(&[(28448, 134_369_923_919_058_640)]);
        assert_eq!(
            registry_row_validity(28448, Some("134369923919058648"), W, &snap),
            RowValidity::Valid
        );
        assert_eq!(
            registry_row_validity(28448, Some("134369923919058650"), W, &snap),
            RowValidity::Stale(StaleRow::ProcStartMismatch),
            "a 10-tick gap is a different microsecond"
        );
    }

    /// The pid-38920 fixture: a live claude now holding paktis `f2a062c0`
    /// under a valid row, while a stale tiohorst row still names it with
    /// `b4c069ec` and a procStart 0.8 days older. Only procStart tells them
    /// apart — the pid IS a countable claude.
    #[test]
    fn a_reused_pid_row_is_stale_even_though_the_pid_is_a_live_claude() {
        let snap = snap_with(&[(38920, 134_369_938_253_255_450)]);
        assert_eq!(
            registry_row_validity(38920, Some("134369938253255456"), W, &snap),
            RowValidity::Valid,
            "paktis f2a062c0 — the pid's own row"
        );
        assert_eq!(
            registry_row_validity(38920, Some("134369219930444954"), W, &snap),
            RowValidity::Stale(StaleRow::ProcStartMismatch),
            "tiohorst b4c069ec — a dead session's file on a reused pid"
        );
    }

    #[test]
    fn validity_dead_pid_is_stale_and_unknowns_are_typed() {
        let snap = snap_with(&[(7, 1_000)]);
        // Dead pid: stale even with no procStart.
        assert_eq!(
            registry_row_validity(99, None, W, &snap),
            RowValidity::Stale(StaleRow::PidNotLiveClaude)
        );
        // Pid alive but not a claude image (reused by another program).
        assert_eq!(
            registry_row_validity(1, Some("1000"), W, &snap),
            RowValidity::Stale(StaleRow::PidNotLiveClaude)
        );
        // Older CLI: no procStart → UNKNOWN, never valid.
        assert_eq!(
            registry_row_validity(7, None, W, &snap),
            RowValidity::Unknown(UnknownRow::NoProcStart)
        );
        assert_eq!(
            registry_row_validity(7, Some("abc"), W, &snap),
            RowValidity::Unknown(UnknownRow::MalformedProcStart)
        );
        assert_eq!(
            registry_row_validity(7, Some("1000"), Some("linux:box"), &snap),
            RowValidity::Unknown(UnknownRow::UnsupportedPidDomain)
        );
        // A claude whose creation time the snapshot does not carry.
        let mut no_ft = snap_with(&[]);
        no_ft.names.insert(7, "claude.exe".to_string());
        assert_eq!(
            registry_row_validity(7, Some("1000"), W, &no_ft),
            RowValidity::Unknown(UnknownRow::CreationTimeUnavailable)
        );
        // Unreadable table outranks everything.
        assert_eq!(
            registry_row_validity(7, Some("1000"), W, &ProcessSnapshot::default()),
            RowValidity::Unknown(UnknownRow::ProcessTableUnreadable)
        );
    }

    /// `read_registry_rows` keeps every file (no pid filter) and stamps each
    /// with its validity — including the incident's real file shape, which
    /// carries `procStart`/`pidDomain` as strings.
    #[test]
    fn read_registry_rows_validates_every_file() {
        let root = tmpdir("validated");
        let acct = root.join(".claude-paktis");
        let sdir = acct.join("sessions");
        fs::write(
            sdir.join("46416.json"),
            r#"{"pid":46416,"sessionId":"0c4e3025","cwd":"D:\\qontinui-root","procStart":"134369000000000005","pidDomain":"win32:spaceship","kind":"interactive","entrypoint":"cli","status":"busy"}"#,
        )
        .unwrap();
        fs::write(sdir.join("51.json"), r#"{"pid":51,"sessionId":"dead"}"#).unwrap();
        fs::write(sdir.join("52.json"), "{garbage").unwrap();
        let snap = snap_with(&[(46416, 134_369_000_000_000_000)]);

        let rows = read_registry_rows(&[acct.clone()], &snap);
        assert_eq!(
            rows.len(),
            2,
            "the malformed file is skipped, the dead one kept"
        );
        let live = rows.iter().find(|r| r.pid == 46416).unwrap();
        assert_eq!(live.validity, RowValidity::Valid);
        assert_eq!(live.session_id, "0c4e3025");
        assert_eq!(live.config_dir, acct);
        assert!(live.is_interactive());
        let dead = rows.iter().find(|r| r.pid == 51).unwrap();
        assert_eq!(
            dead.validity,
            RowValidity::Stale(StaleRow::PidNotLiveClaude)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn registry_row_kind_absent_counts_as_interactive_but_sdk_does_not() {
        let snap = snap_with(&[]);
        let r = |kind: Option<&str>| RegistryRow {
            pid: 1,
            session_id: "s".into(),
            config_dir: PathBuf::from("c"),
            kind: kind.map(str::to_string),
            validity: registry_row_validity(1, None, None, &snap),
        };
        assert!(r(None).is_interactive());
        assert!(r(Some("interactive")).is_interactive());
        assert!(!r(Some("sdk")).is_interactive());
    }
}
