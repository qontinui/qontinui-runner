//! Merged "previous sessions" model — the DISPLAY-only listing that surfaces
//! the real `--resume` name for every session the runner has seen (plan
//! `previous-sessions-listing`, phases P1–P3).
//!
//! [`PastSession`] is keyed on `claude_session_id` and merged newest-wins
//! across three sources:
//!
//! 1. **Registry** ([`SessionLifecycleStore::all_records`]) — fresh
//!    identity/layout for sessions inside the ~24 h retention window.
//! 2. **Snapshot history**
//!    ([`snapshot_history::read_all_snapshot_sessions`]) — DISPLAY-only reader
//!    that fills in ids OLDER than the registry retention (the append-only
//!    history keeps 14 days). Registry rows win over snapshot rows for the same
//!    id.
//! 3. **On-disk transcript** — the through-line `resume_name` (last `/rename`
//!    custom-title, else auto summary, else first-message preview) plus a
//!    RE-COMPUTED `transcript_exists` / `restorable` probe. These two are never
//!    read from the snapshot (it does not persist them for pruned ids); they
//!    are re-derived from disk every call.
//!
//! This whole module is DISPLAY-only. It never drives restore — the snapshot
//! reader's invariant (restore must not read the history) is preserved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use crate::session::snapshot_history::{self, SnapshotSession};
use crate::terminal::transcript;

/// Single-linkage cohort gap: sessions whose `last_seen_at` are within this of
/// each other belong to the same cohort (a crash cohort renders as one group).
const COHORT_GAP_MS: i64 = 5 * 60 * 1000;

/// Which Claude account a session belongs to, derived from its `config_dir`
/// (`.claude-<x>` suffix). `wrapper` is the CLI launcher command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastSessionAccount {
    /// Account label (e.g. `"gmail"`), or `"unknown"` when the config dir has
    /// no `.claude-<x>` suffix.
    pub label: String,
    /// CLI wrapper command (`"clg"`/`"clh"`/…), or `"claude"` for an
    /// unrecognized/absent account.
    pub wrapper: String,
}

/// One row of the previous-sessions listing — the merged + enriched view of a
/// session across the registry, snapshot history, and on-disk transcript.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastSession {
    /// Stable provider session id (`claude --resume <id>`).
    pub claude_session_id: String,
    /// The real `--resume` name (last `/rename` custom-title → auto summary →
    /// first-message preview → registry title → `"Session <date>"`).
    pub resume_name: String,
    /// Ready-to-run resume command: `"<wrapper> --resume <id>"`.
    pub resume_command: String,
    /// Account derived from `config_dir`.
    pub account: PastSessionAccount,
    /// Grid page the session's tile belonged to.
    pub page_id: String,
    /// Grid zone index (-1 = unassigned).
    pub zone_index: i32,
    /// `"open"` | `"closed"`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    /// Runner terminal id that hosted the session (registry rows only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// AI-CLI provider (`"claude"`, `"gemini"`).
    pub provider: String,
    /// Unix millis of the most recent liveness/touch (snapshot rows use
    /// `opened_at` as a proxy — they carry no `last_seen_at`).
    pub last_seen_at: i64,
    /// Unix millis the session was first opened.
    pub opened_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// Unix millis a provider hook confirmed a real session started (registry
    /// rows only; snapshot rows carry only the derived `confirmed` bool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<i64>,
    /// Whether a provider hook confirmed a real session started here.
    pub confirmed: bool,
    /// Whether a transcript for this id exists on disk NOW — re-probed every
    /// call (never read from the snapshot).
    pub transcript_exists: bool,
    /// Whether this id is `--resume`-able NOW (`confirmed` AND
    /// `transcript_exists`) — re-computed from disk.
    pub restorable: bool,
    /// Synthetic id grouping sessions that share a dense `last_seen_at` band
    /// (single-linkage at [`COHORT_GAP_MS`]); the band's earliest timestamp.
    pub cohort_id: i64,
}

impl PastSession {
    /// A "shell" is a phantom terminal that never ran a provider: no transcript
    /// on disk AND never confirmed. These are filtered out by default (see
    /// [`PastSessionsOpts::include_shells`]). A real-but-pruned session (whose
    /// transcript aged out) stays `confirmed` and is NOT treated as a shell.
    fn is_shell(&self) -> bool {
        !self.transcript_exists && !self.confirmed
    }
}

/// Options for [`build_past_sessions`]. All filters are optional; `None` /
/// default means "no filter".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastSessionsOpts {
    /// Keep only sessions whose `last_seen_at >= since_ms`.
    pub since_ms: Option<i64>,
    /// Keep only sessions on this grid page.
    pub page_id: Option<String>,
    /// Keep only sessions under this account label.
    pub account: Option<String>,
    /// Include phantom shell sessions (no transcript, never confirmed).
    /// Default `false` — they are noise in a "previous sessions" list.
    #[serde(default)]
    pub include_shells: bool,
    /// Cap the returned list (after sorting newest-first).
    pub limit: Option<usize>,
}

/// Map a `config_dir` to its account label + CLI wrapper. The mapping mirrors
/// the fleet's account roster (`gmail→clg`, `hotmail→clh`, `paktis→clp`,
/// `qontinui→clq`, `tiohorst→clt`); an unrecognized `.claude-<x>` keeps `<x>`
/// as the label with the generic `claude` wrapper, and a config dir with no
/// `.claude-<x>` suffix (or none at all) is `unknown`/`claude`.
pub(crate) fn account_from_config_dir(config_dir: Option<&str>) -> PastSessionAccount {
    let acct = |label: &str, wrapper: &str| PastSessionAccount {
        label: label.to_string(),
        wrapper: wrapper.to_string(),
    };
    let suffix = config_dir.and_then(|cd| {
        Path::new(cd)
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|name| name.strip_prefix(".claude-"))
            .map(|s| s.to_string())
    });
    match suffix.as_deref() {
        Some("gmail") => acct("gmail", "clg"),
        Some("hotmail") => acct("hotmail", "clh"),
        Some("paktis") => acct("paktis", "clp"),
        Some("qontinui") => acct("qontinui", "clq"),
        Some("tiohorst") => acct("tiohorst", "clt"),
        Some(other) => acct(other, "claude"),
        None => acct("unknown", "claude"),
    }
}

/// Locate the on-disk transcript for a session, if any. Prefers the recorded
/// `config_dir`; falls back to scanning every known Claude config dir (a
/// session's account may have been re-encoded / moved). Requires a
/// `working_dir` to build the encoded project path.
fn resolve_transcript_path(
    config_dir: Option<&str>,
    working_dir: Option<&str>,
    session_id: &str,
) -> Option<PathBuf> {
    let wd = working_dir?;
    if let Some(cd) = config_dir {
        let p = transcript::session_transcript_path(Path::new(cd), wd, session_id);
        if p.exists() {
            return Some(p);
        }
    }
    for dir in transcript::find_claude_config_dirs() {
        let p = transcript::session_transcript_path(&dir, wd, session_id);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Format a unix-millis timestamp into a short human date for the `"Session
/// <date>"` fallback name.
fn short_date(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|dt| dt.format("%b %-d, %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Shared enrichment: build a [`PastSession`] from the identity/layout tuple
/// common to both sources, probing the transcript on disk for the resume name
/// and restorability. `cohort_id` is left `0` here and assigned later by
/// [`assign_cohorts`].
#[allow(clippy::too_many_arguments)]
fn build_enriched(
    claude_session_id: String,
    config_dir: Option<String>,
    working_dir: Option<String>,
    page_id: String,
    zone_index: i32,
    state: String,
    close_reason: Option<String>,
    terminal_id: Option<String>,
    provider: String,
    last_seen_at: i64,
    opened_at: i64,
    closed_at: Option<i64>,
    confirmed_at: Option<i64>,
    confirmed: bool,
    title: Option<String>,
) -> PastSession {
    let transcript_path = resolve_transcript_path(
        config_dir.as_deref(),
        working_dir.as_deref(),
        &claude_session_id,
    );
    let transcript_exists = transcript_path.is_some();
    let restorable = confirmed && transcript_exists;

    let account = account_from_config_dir(config_dir.as_deref());
    let resume_command = format!("{} --resume {}", account.wrapper, claude_session_id);

    // resume name: on-disk resume/preview name → registry title → date default.
    let resume_name = transcript_path
        .as_deref()
        .and_then(transcript::resume_or_preview_name_from_path)
        .or_else(|| title.clone().filter(|t| !t.trim().is_empty()))
        .unwrap_or_else(|| format!("Session {}", short_date(last_seen_at)));

    PastSession {
        claude_session_id,
        resume_name,
        resume_command,
        account,
        page_id,
        zone_index,
        state,
        close_reason,
        terminal_id,
        config_dir,
        working_dir,
        provider,
        last_seen_at,
        opened_at,
        closed_at,
        confirmed_at,
        confirmed,
        transcript_exists,
        restorable,
        cohort_id: 0,
    }
}

fn past_from_record(rec: &TerminalSessionRecord) -> PastSession {
    let confirmed = rec.confirmed_at.is_some();
    build_enriched(
        rec.claude_session_id.clone(),
        rec.config_dir.clone(),
        rec.working_dir.clone(),
        rec.page_id.clone(),
        rec.zone_index,
        rec.state.clone(),
        rec.close_reason.clone(),
        Some(rec.terminal_id.clone()),
        rec.provider.clone(),
        rec.last_seen_at,
        rec.opened_at,
        rec.closed_at,
        rec.confirmed_at,
        confirmed,
        rec.title.clone(),
    )
}

fn past_from_snapshot(s: &SnapshotSession) -> PastSession {
    build_enriched(
        s.claude_session_id.clone(),
        s.config_dir.clone(),
        s.working_dir.clone(),
        s.page_id.clone(),
        s.zone_index,
        s.state.clone(),
        s.close_reason.clone(),
        None, // snapshot carries no terminal_id
        s.provider.clone(),
        s.opened_at, // snapshot carries no last_seen_at — opened_at is the proxy
        s.opened_at,
        None, // no closed_at on the snapshot
        None, // no confirmed_at on the snapshot (only the derived bool below)
        s.confirmed,
        s.title.clone(),
    )
}

/// Assign cohort ids by single-linkage clustering on `last_seen_at`. Sorts the
/// slice ascending, starts a new cohort whenever the gap to the previous
/// session exceeds [`COHORT_GAP_MS`], and stamps each session with its band's
/// earliest timestamp. Deterministic.
fn assign_cohorts(sessions: &mut [PastSession]) {
    sessions.sort_by_key(|s| s.last_seen_at);
    let mut cohort_start: i64 = 0;
    let mut prev: Option<i64> = None;
    for s in sessions.iter_mut() {
        let new_cohort = match prev {
            Some(p) => s.last_seen_at - p > COHORT_GAP_MS,
            None => true,
        };
        if new_cohort {
            cohort_start = s.last_seen_at;
        }
        s.cohort_id = cohort_start;
        prev = Some(s.last_seen_at);
    }
}

/// Build the merged, enriched, filtered previous-sessions listing (newest
/// first). Merge rule: snapshot rows are laid down first, then registry rows
/// overwrite by id (the registry is fresher for recent sessions; the snapshot
/// fills in ids older than the registry retention). Cohorts are assigned across
/// the FULL merged set before filtering, so a crash cohort groups consistently
/// regardless of the `since_ms`/`page_id`/`account` filters.
pub fn build_past_sessions(
    store: &SessionLifecycleStore,
    snapshot_path: &Path,
    opts: &PastSessionsOpts,
) -> Vec<PastSession> {
    let mut by_id: HashMap<String, PastSession> = HashMap::new();

    // Snapshot-sourced rows first (ids the registry may have pruned).
    for s in snapshot_history::read_all_snapshot_sessions(snapshot_path) {
        let ps = past_from_snapshot(&s);
        by_id.insert(ps.claude_session_id.clone(), ps);
    }
    // Registry rows overwrite — fresher identity/layout for recent sessions.
    for rec in store.all_records() {
        let ps = past_from_record(&rec);
        by_id.insert(ps.claude_session_id.clone(), ps);
    }

    let mut out: Vec<PastSession> = by_id.into_values().collect();
    assign_cohorts(&mut out);

    out.retain(|s| {
        if !opts.include_shells && s.is_shell() {
            return false;
        }
        if let Some(since) = opts.since_ms {
            if s.last_seen_at < since {
                return false;
            }
        }
        if let Some(ref pid) = opts.page_id {
            if &s.page_id != pid {
                return false;
            }
        }
        if let Some(ref acc) = opts.account {
            if &s.account.label != acc {
                return false;
            }
        }
        true
    });

    out.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    if let Some(limit) = opts.limit {
        out.truncate(limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_mapping_covers_roster_and_fallbacks() {
        assert_eq!(
            account_from_config_dir(Some("C:/claude/.claude-gmail")).wrapper,
            "clg"
        );
        assert_eq!(
            account_from_config_dir(Some("C:/claude/.claude-tiohorst")).label,
            "tiohorst"
        );
        // Unknown suffix keeps its label but the generic wrapper.
        let other = account_from_config_dir(Some("C:/claude/.claude-weird"));
        assert_eq!(other.label, "weird");
        assert_eq!(other.wrapper, "claude");
        // No `.claude-<x>` suffix → unknown/claude.
        let none = account_from_config_dir(Some("C:/claude/.claude"));
        assert_eq!(none.label, "unknown");
        assert_eq!(account_from_config_dir(None).label, "unknown");
    }

    #[test]
    fn resume_command_uses_account_wrapper() {
        let ps = build_enriched(
            "sess-1".to_string(),
            Some("C:/claude/.claude-hotmail".to_string()),
            None,
            "default".to_string(),
            0,
            "closed".to_string(),
            None,
            None,
            "claude".to_string(),
            1_000,
            1_000,
            None,
            None,
            false,
            Some("Fix build".to_string()),
        );
        assert_eq!(ps.resume_command, "clh --resume sess-1");
        assert_eq!(ps.account.label, "hotmail");
        // No transcript on disk → not restorable, title used as the name.
        assert!(!ps.transcript_exists);
        assert!(!ps.restorable);
        assert_eq!(ps.resume_name, "Fix build");
    }

    #[test]
    fn cohort_grouping_splits_on_gap() {
        let mk = |id: &str, seen: i64| PastSession {
            claude_session_id: id.to_string(),
            resume_name: "n".to_string(),
            resume_command: "c".to_string(),
            account: PastSessionAccount {
                label: "gmail".to_string(),
                wrapper: "clg".to_string(),
            },
            page_id: "default".to_string(),
            zone_index: 0,
            state: "closed".to_string(),
            close_reason: None,
            terminal_id: None,
            config_dir: None,
            working_dir: None,
            provider: "claude".to_string(),
            last_seen_at: seen,
            opened_at: seen,
            closed_at: None,
            confirmed_at: None,
            confirmed: true,
            transcript_exists: true,
            restorable: true,
            cohort_id: 0,
        };
        // Two tight bands separated by a > 5 min gap.
        let base = 1_700_000_000_000i64;
        let mut sessions = vec![
            mk("a", base),
            mk("b", base + 60_000),
            mk("c", base + 10 * 60_000), // new cohort (10 min gap)
            mk("d", base + 10 * 60_000 + 30_000),
        ];
        assign_cohorts(&mut sessions);
        // sorted ascending by assign_cohorts
        assert_eq!(sessions[0].cohort_id, base);
        assert_eq!(sessions[1].cohort_id, base, "within gap → same cohort");
        assert_eq!(
            sessions[2].cohort_id,
            base + 10 * 60_000,
            "beyond gap → new cohort"
        );
        assert_eq!(sessions[3].cohort_id, base + 10 * 60_000);
        // Exactly two distinct cohorts.
        let distinct: std::collections::HashSet<_> = sessions.iter().map(|s| s.cohort_id).collect();
        assert_eq!(distinct.len(), 2);
    }
}
