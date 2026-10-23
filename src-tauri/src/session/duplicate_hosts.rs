//! Report a session found LIVE IN TWO PROCESSES (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
//! Phase 5) — report-only.
//!
//! On 2026-10-21 session `0c4e3025` ran as pid 46416 (tiohorst) AND pid 51860
//! (lukas, the migration's resume) for hours, both appending to diverging
//! transcripts, and nothing said so. Claude Code's own registry
//! (`<config>/sessions/<pid>.json`) shows it directly: two VALID interactive
//! rows naming one `sessionId`.
//!
//! Only rows that pass the ONE validity rule
//! ([`registry_row_validity`](crate::session::claude_session_registry::registry_row_validity))
//! and are `kind == interactive` count — the 2026-10-21 census found 3
//! duplicate groups over raw rows but only 1 over valid rows, and that one was
//! the real fork. A row of UNKNOWN validity never counts. An unreadable
//! process table is UNKNOWN, never "no duplicates".
//!
//! The migration's own hand-off (old pane still alive for ~12 s after the
//! resumed pane starts) is excluded: a group with a holder on a terminal the
//! store has marked superseded, or on a terminal / session the migration
//! respawned within 60 s ([`crate::terminal::migration_handoff`]).
//!
//! It NEVER closes, signals or modifies either process
//! [policy: exercising-a-live-surface-is-state-neutral case 2]: one `warn!`
//! per new occurrence and one best-effort coord finding.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{RegistryRow, RowValidity, UnknownRow};
use crate::session::reconcile::LivePty;
use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// One process holding a duplicated session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// The runner pane whose subtree hosts the pid; `None` for a claude the
    /// runner does not host (a plain terminal window).
    pub terminal_id: Option<String>,
    pub config_dir: PathBuf,
}

/// One session id held by ≥ 2 valid interactive processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateHost {
    pub session_id: String,
    /// Sorted by pid.
    pub holders: Vec<Holder>,
}

impl DuplicateHost {
    fn dedupe_key(&self) -> (String, Vec<u32>) {
        (
            self.session_id.clone(),
            self.holders.iter().map(|h| h.pid).collect(),
        )
    }
}

/// The detector's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateVerdict {
    /// The registry could not be validated — say nothing about duplicates.
    Unknown(String),
    /// Observed; empty means "no duplicates seen", which is only sayable
    /// because the table was read.
    Observed(Vec<DuplicateHost>),
}

/// What to exclude as the migration's own hand-off.
#[derive(Debug, Clone, Default)]
pub struct HandoffExclusions {
    /// Terminals marked by `store.mark_terminal_superseded` or respawned by
    /// `migrate_session` within the hand-off window.
    pub terminals: HashSet<String>,
    /// Sessions `migrate_session` respawned within the hand-off window.
    pub sessions: HashSet<String>,
}

/// Pure detector.
///
/// `table_readable` is the snapshot read's verdict (`false` ⇒ `Unknown`).
/// `pid_terminals` maps each claude pid hosted in a live pane to its terminal.
pub fn detect_duplicate_hosts(
    table_readable: bool,
    rows: &[RegistryRow],
    pid_terminals: &HashMap<u32, String>,
    exclusions: &HandoffExclusions,
) -> DuplicateVerdict {
    if !table_readable
        || rows
            .iter()
            .any(|r| r.validity == RowValidity::Unknown(UnknownRow::ProcessTableUnreadable))
    {
        return DuplicateVerdict::Unknown("process table unreadable".into());
    }
    let mut by_sid: BTreeMap<&str, Vec<&RegistryRow>> = BTreeMap::new();
    for r in rows
        .iter()
        .filter(|r| r.validity.is_valid() && r.is_interactive())
    {
        by_sid.entry(r.session_id.as_str()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (sid, mut group) in by_sid {
        group.sort_by_key(|r| r.pid);
        group.dedup_by_key(|r| r.pid);
        if group.len() < 2 || exclusions.sessions.contains(sid) {
            continue;
        }
        let holders: Vec<Holder> = group
            .iter()
            .map(|r| Holder {
                pid: r.pid,
                terminal_id: pid_terminals.get(&r.pid).cloned(),
                config_dir: r.config_dir.clone(),
            })
            .collect();
        let in_handoff = holders.iter().any(|h| {
            h.terminal_id
                .as_ref()
                .is_some_and(|t| exclusions.terminals.contains(t))
        });
        if in_handoff {
            continue;
        }
        out.push(DuplicateHost {
            session_id: sid.to_string(),
            holders,
        });
    }
    DuplicateVerdict::Observed(out)
}

/// Dedupe across ticks: one report per `(sid, pid set)` while it persists. A
/// group that disappears and later recurs is reported again. An `Unknown`
/// tick leaves the memory untouched.
#[derive(Debug, Default)]
pub struct DuplicateReporter {
    reported: HashSet<(String, Vec<u32>)>,
}

impl DuplicateReporter {
    /// The occurrences in `found` not reported before.
    pub fn new_occurrences(&mut self, found: &[DuplicateHost]) -> Vec<DuplicateHost> {
        let current: HashSet<(String, Vec<u32>)> = found.iter().map(|d| d.dedupe_key()).collect();
        let fresh = found
            .iter()
            .filter(|d| !self.reported.contains(&d.dedupe_key()))
            .cloned()
            .collect();
        self.reported = current;
        fresh
    }
}

fn reporter() -> &'static Mutex<DuplicateReporter> {
    static CELL: std::sync::OnceLock<Mutex<DuplicateReporter>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Mutex::new(DuplicateReporter::default()))
}

/// `pid → terminal_id` for every claude pid in a live pane's subtree.
pub fn pane_claude_pids(live: &[LivePty], snapshot: &ProcessSnapshot) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for pty in live {
        let Some(root) = pty.pid else { continue };
        for pid in
            crate::process_capture::process_tree::claude_pids_in_inclusive_subtree(root, snapshot)
        {
            out.insert(pid, pty.terminal_id.clone());
        }
    }
    out
}

fn describe(d: &DuplicateHost) -> String {
    d.holders
        .iter()
        .map(|h| {
            format!(
                "pid {} (pane {}, config {})",
                h.pid,
                h.terminal_id
                    .as_deref()
                    .unwrap_or("none — not a runner pane"),
                h.config_dir.display()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// I/O wrapper, called once per (unguarded) session-lifecycle poll tick with
/// that tick's snapshot. Reads the registry, detects, warns once per new
/// occurrence, and posts each new occurrence to coord in a detached task.
/// Returns the verdict (for tests/logging).
pub fn run_duplicate_hosts_pass(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
) -> DuplicateVerdict {
    let table_readable = !snapshot.names.is_empty();
    let rows = if table_readable {
        crate::session::claude_session_registry::read_registry_rows(
            &crate::terminal::transcript::find_claude_config_dirs(),
            snapshot,
        )
    } else {
        Vec::new()
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (mut terminals, sessions) =
        crate::terminal::migration_handoff::recent_migration_handoffs(now_ms);
    terminals.extend(store.superseded_terminal_ids());
    let exclusions = HandoffExclusions {
        terminals,
        sessions,
    };
    let verdict = detect_duplicate_hosts(
        table_readable,
        &rows,
        &pane_claude_pids(live, snapshot),
        &exclusions,
    );
    let DuplicateVerdict::Observed(found) = &verdict else {
        tracing::debug!(
            ?verdict,
            "duplicate hosts: registry not readable this tick — UNKNOWN"
        );
        return verdict;
    };
    let fresh = reporter()
        .lock()
        .map(|mut r| r.new_occurrences(found))
        .unwrap_or_default();
    for d in fresh {
        let holders = describe(&d);
        tracing::warn!(
            claude_session = %d.session_id,
            holders = %holders,
            "duplicate hosts: one session is live in more than one process — the conversation has forked; nothing was closed (close the unwanted pane by hand)"
        );
        tauri::async_runtime::spawn(post_duplicate_finding(d, holders));
    }
    verdict
}

/// One best-effort coord finding through the device-authed findings door —
/// the same route and body shape (`title`, `body`, `kind`, `topic`,
/// `resource_keys`; coord's `PostFindingBody` is `deny_unknown_fields`) as
/// `mcp_api::post_coord_mcp_drift_finding` / `coord_outside_observer::post_finding`.
/// One attempt, 10 s, warn-and-continue.
async fn post_duplicate_finding(d: DuplicateHost, holders: String) {
    post_session_finding(finding_body(&d, &holders), &d.session_id, "duplicate hosts").await;
}

/// The ONE door this module and the account migration's stall report
/// (`terminal::account_migration::followup`, plan
/// `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
/// Phase 2b) post a session-keyed coord finding through: `POST
/// <coord>/coord/agent-findings` under the device credential the coord-mcp
/// door resolves. One attempt, 10 s, warn-and-continue — never retried, never
/// surfaced to the caller. `who` prefixes the log lines.
pub(crate) async fn post_session_finding(
    body: serde_json::Value,
    claude_session_id: &str,
    who: &'static str,
) {
    let door = qontinui_runner_lib::coord_doctor::resolve_coord_mcp_door();
    let scope = {
        use crate::auth::TenantScope;
        use qontinui_runner_lib::tenant_pin::TenantPin;
        match door.pin {
            TenantPin::Pinned(t) => TenantScope::Owned(t),
            TenantPin::Unpinned => TenantScope::Device,
            TenantPin::Unresolvable => TenantScope::Unresolved,
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "{who}: reqwest client build failed; finding not posted");
            return;
        }
    };
    let url = format!("{}/coord/agent-findings", door.base.trim_end_matches('/'));
    match crate::auth::attach_device_auth_for(client.post(&url).json(&body), scope)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => tracing::info!(
            claude_session = %claude_session_id,
            "{who}: finding posted to coord"
        ),
        Ok(resp) => tracing::warn!(
            status = %resp.status(),
            claude_session = %claude_session_id,
            "{who}: coord refused the finding; not retried"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            claude_session = %claude_session_id,
            "{who}: posting the finding failed; not retried"
        ),
    }
}

/// The finding body — pure, so its shape is pinned by a test.
pub fn finding_body(d: &DuplicateHost, holders: &str) -> serde_json::Value {
    json!({
        "title": format!(
            "session {} is live in {} processes at once (forked conversation)",
            d.session_id,
            d.holders.len()
        ),
        "body": format!(
            "Claude Code's live registry (<config>/sessions/<pid>.json) has {} VALID interactive \
             rows (live claude pid + matching procStart) naming session {}: {}. Each process \
             appends to its own transcript, so the conversation has diverged. The runner closed \
             and signalled nothing; close the unwanted pane by hand. Reported once per \
             (session, pid set) by the runner's duplicate-host detector.",
            d.holders.len(),
            d.session_id,
            holders
        ),
        "kind": "investigation",
        "topic": "session-identity",
        "resource_keys": [format!("session:{}", d.session_id)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::claude_session_registry::StaleRow;

    fn row(pid: u32, sid: &str, cfg: &str, validity: RowValidity) -> RegistryRow {
        RegistryRow {
            pid,
            session_id: sid.into(),
            config_dir: PathBuf::from(cfg),
            kind: Some("interactive".into()),
            validity,
        }
    }
    const V: RowValidity = RowValidity::Valid;

    fn observed(v: DuplicateVerdict) -> Vec<DuplicateHost> {
        match v {
            DuplicateVerdict::Observed(f) => f,
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    fn terminals() -> HashMap<u32, String> {
        [
            (46416, "601a25ac".to_string()),
            (51860, "new-pane".to_string()),
        ]
        .into_iter()
        .collect()
    }

    /// Zero, one and two holders. Two valid interactive holders of one id —
    /// the 2026-10-21 `0c4e3025` shape — is a finding naming both pids, panes
    /// and config dirs.
    ///
    /// Mutation check: changing `group.len() < 2` to `< 3` drops the
    /// two-holder finding.
    #[test]
    fn zero_one_and_two_holders() {
        let none = HandoffExclusions::default();
        assert!(observed(detect_duplicate_hosts(true, &[], &terminals(), &none)).is_empty());
        let one = [row(46416, "0c4e3025", ".claude-tiohorst", V)];
        assert!(observed(detect_duplicate_hosts(true, &one, &terminals(), &none)).is_empty());
        let two = [
            row(46416, "0c4e3025", ".claude-tiohorst", V),
            row(51860, "0c4e3025", ".claude-lukas", V),
            row(7, "other", ".claude-lukas", V),
        ];
        assert_eq!(
            observed(detect_duplicate_hosts(true, &two, &terminals(), &none)),
            vec![DuplicateHost {
                session_id: "0c4e3025".into(),
                holders: vec![
                    Holder {
                        pid: 46416,
                        terminal_id: Some("601a25ac".into()),
                        config_dir: PathBuf::from(".claude-tiohorst"),
                    },
                    Holder {
                        pid: 51860,
                        terminal_id: Some("new-pane".into()),
                        config_dir: PathBuf::from(".claude-lukas"),
                    },
                ],
            }]
        );
    }

    /// A stale file whose pid is dead, and a reused pid (procStart mismatch —
    /// the pid-38920 case), are not holders. Neither is an UNKNOWN-validity
    /// row (no procStart), nor a non-interactive row (benign SDK class).
    ///
    /// Mutation check: dropping `r.validity.is_valid()` from the filter
    /// reports every one of these groups.
    #[test]
    fn stale_reused_unknown_and_non_interactive_rows_are_ignored() {
        let none = HandoffExclusions::default();
        let mut sdk = row(5, "a", "c", V);
        sdk.kind = Some("sdk".into());
        let rows = [
            row(1, "a", "c", V),
            row(2, "a", "c", RowValidity::Stale(StaleRow::PidNotLiveClaude)),
            row(
                38920,
                "a",
                "c",
                RowValidity::Stale(StaleRow::ProcStartMismatch),
            ),
            row(4, "a", "c", RowValidity::Unknown(UnknownRow::NoProcStart)),
            sdk,
        ];
        assert!(observed(detect_duplicate_hosts(true, &rows, &HashMap::new(), &none)).is_empty());
    }

    /// The migration's own hand-off: the old pane is superseded (or the new
    /// pane / session was just respawned) — no finding.
    ///
    /// Mutation check: deleting the `in_handoff` skip fails the first
    /// assertion; deleting the `exclusions.sessions` check fails the second.
    #[test]
    fn in_flight_migration_shape_is_not_a_finding() {
        let rows = [
            row(46416, "0c4e3025", ".claude-tiohorst", V),
            row(51860, "0c4e3025", ".claude-lukas", V),
        ];
        let by_terminal = HandoffExclusions {
            terminals: ["601a25ac".to_string()].into_iter().collect(),
            sessions: HashSet::new(),
        };
        assert!(observed(detect_duplicate_hosts(
            true,
            &rows,
            &terminals(),
            &by_terminal
        ))
        .is_empty());
        let by_session = HandoffExclusions {
            terminals: HashSet::new(),
            sessions: ["0c4e3025".to_string()].into_iter().collect(),
        };
        assert!(observed(detect_duplicate_hosts(
            true,
            &rows,
            &HashMap::new(),
            &by_session
        ))
        .is_empty());
    }

    /// An unreadable table is UNKNOWN — never "no duplicates".
    ///
    /// Mutation check: removing the `!table_readable` guard returns
    /// `Observed([])` and fails.
    #[test]
    fn unreadable_table_is_unknown() {
        let none = HandoffExclusions::default();
        assert!(matches!(
            detect_duplicate_hosts(false, &[], &HashMap::new(), &none),
            DuplicateVerdict::Unknown(_)
        ));
        let rows = [row(
            1,
            "a",
            "c",
            RowValidity::Unknown(UnknownRow::ProcessTableUnreadable),
        )];
        assert!(matches!(
            detect_duplicate_hosts(true, &rows, &HashMap::new(), &none),
            DuplicateVerdict::Unknown(_)
        ));
    }

    /// One report per (sid, pid set) while it persists; a recurrence after it
    /// cleared is reported again; a new pid set is new.
    ///
    /// Mutation check: never inserting into `reported` reports the same
    /// occurrence on the second tick.
    #[test]
    fn reporter_dedupes_per_session_and_pid_set() {
        let d = |pids: &[u32]| DuplicateHost {
            session_id: "s".into(),
            holders: pids
                .iter()
                .map(|p| Holder {
                    pid: *p,
                    terminal_id: None,
                    config_dir: PathBuf::from("c"),
                })
                .collect(),
        };
        let mut r = DuplicateReporter::default();
        assert_eq!(r.new_occurrences(&[d(&[1, 2])]).len(), 1);
        assert!(
            r.new_occurrences(&[d(&[1, 2])]).is_empty(),
            "same occurrence"
        );
        assert_eq!(r.new_occurrences(&[d(&[1, 3])]).len(), 1, "new pid set");
        assert!(r.new_occurrences(&[]).is_empty());
        assert_eq!(r.new_occurrences(&[d(&[1, 3])]).len(), 1, "recurrence");
    }

    /// The body carries exactly the keys coord's `deny_unknown_fields`
    /// `PostFindingBody` accepts, keyed on the session.
    #[test]
    fn finding_body_shape() {
        let d = DuplicateHost {
            session_id: "0c4e3025".into(),
            holders: vec![],
        };
        let b = finding_body(&d, "pid 1 (pane x, config c)");
        let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["body", "kind", "resource_keys", "title", "topic"]);
        assert_eq!(b["resource_keys"], json!(["session:0c4e3025"]));
    }
}
