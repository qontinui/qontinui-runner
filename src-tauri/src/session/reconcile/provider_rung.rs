//! The provider-registry rung of the session binder, and the re-bind of a
//! CONFIRMED record (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
//! Phase 4).
//!
//! Claude Code writes `<config>/sessions/<pid>.json` naming the session id the
//! process is running RIGHT NOW — it follows an in-pane `/clear` or `/resume`,
//! which neither the argv (rung 2) nor a start-anchored transcript correlation
//! (rung 3) can see. On 2026-10-20 rung 3 bound pane `314176b7` to `0c4e3025`
//! because that transcript's first event postdated every pane in the cwd;
//! the provider's registry named pid 46416 in pane `601a25ac` all along.
//!
//! A registry row is evidence only when it passes the ONE validity rule,
//! [`registry_row_validity`] (pid is a countable live claude AND `procStart`
//! is that pid's creation time). A stale/reused-pid row never binds; a row
//! with no `procStart` is UNKNOWN and the binder falls back to rung 3.
//!
//! Two pure decisions live here, composed by `reconcile.rs`:
//! - [`decide_provider_rung`] — the FIRST bind of an unconfirmed pane (called
//!   from `decide_bind_for_live_pty` between rungs 2 and 3);
//! - [`decide_registry_rebind`] — the re-bind of a CONFIRMED record whose
//!   pane's valid row names a different id. It runs in the UNGUARDED
//!   `refresh_titles_pass`, because the bind pass is skipped entirely once
//!   every live pane is confirmed.

use std::collections::{HashMap, HashSet};

use super::{
    bind_record, BindOrigin, LivePty, ProviderSessionIndex, ProviderSessionMeta, ReconcileAction,
    TranscriptCandidate,
};
use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{registry_row_validity, RowValidity};
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, ORIGIN_AUTHORITATIVE,
};

/// A provider-registry row for a live claude pid that passed
/// [`registry_row_validity`] against the pass's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidProviderRow {
    pub session_id: String,
    /// The account config dir the row was found under (empty when unknown).
    pub config_dir: String,
}

/// Pick the VALID row for `pid` among every registry file naming it (a reused
/// pid can have a stale file in one account and its own in another — the
/// 2026-10-21 pid-38920 case). `None` when no row is valid: no row, only
/// stale rows, or only UNKNOWN-validity rows (older CLI) — the binder then
/// falls back to rung 3, and a re-bind is not attempted.
pub fn valid_provider_row(
    pid: u32,
    metas: &[ProviderSessionMeta],
    snapshot: &ProcessSnapshot,
) -> Option<ValidProviderRow> {
    let mut valid = metas.iter().filter(|m| {
        registry_row_validity(
            pid,
            m.proc_start.as_deref(),
            m.pid_domain.as_deref(),
            snapshot,
        ) == RowValidity::Valid
    });
    let first = valid.next()?;
    // Two VALID rows naming different ids for one live process cannot both be
    // true; refuse to choose.
    if valid.any(|m| m.session_id != first.session_id) {
        return None;
    }
    Some(ValidProviderRow {
        session_id: first.session_id.clone(),
        config_dir: first.config_dir.clone().unwrap_or_default(),
    })
}

/// The valid provider row for each live PTY's claude anchor (`ai_pid`), keyed
/// by that pid. Only PTYs in `ptys` are read.
pub fn valid_rows_for_ptys<'a, N: ProviderSessionIndex>(
    ptys: impl IntoIterator<Item = &'a LivePty>,
    index: &N,
    snapshot: &ProcessSnapshot,
) -> HashMap<u32, ValidProviderRow> {
    ptys.into_iter()
        .filter_map(|p| p.ai_pid)
        .filter_map(|pid| {
            valid_provider_row(pid, &index.metas_for_pid(pid), snapshot).map(|r| (pid, r))
        })
        .collect()
}

/// RUNG 2½ — the provider's live registry names the id the pane's claude is
/// running under. Same evidence grade as a typed `--session-id`: bind
/// `Authoritative`, confirmed iff a transcript for that id is among the cwd's
/// candidates. The config dir is the candidate's when confirmed, else the
/// registry row's own account.
pub fn decide_provider_rung(
    pty: &LivePty,
    row: &ValidProviderRow,
    candidates: &[TranscriptCandidate],
) -> ReconcileAction {
    let matching = candidates.iter().find(|c| c.session_id == row.session_id);
    ReconcileAction::Bind {
        terminal_id: pty.terminal_id.clone(),
        session_id: row.session_id.clone(),
        config_dir: matching
            .map(|c| c.config_dir.clone())
            .unwrap_or_else(|| row.config_dir.clone()),
        origin: BindOrigin::Authoritative,
        confirmed: matching.is_some(),
    }
}

/// A confirmed record the provider says is now running a different id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRebind {
    pub terminal_id: String,
    pub from_session_id: String,
    pub to_session_id: String,
    pub config_dir: String,
}

/// Pure: the id-mismatch arm `decide_title_refresh` used to drop silently.
/// `Some` iff the record is CONFIRMED (an unconfirmed one is the bind pass's
/// to settle) and the pane's VALID provider row names a different id.
pub fn decide_registry_rebind(
    rec: &TerminalSessionRecord,
    row: Option<&ValidProviderRow>,
) -> Option<RegistryRebind> {
    let row = row?;
    if rec.confirmed_at.is_none() || row.session_id == rec.claude_session_id {
        return None;
    }
    Some(RegistryRebind {
        terminal_id: rec.terminal_id.clone(),
        from_session_id: rec.claude_session_id.clone(),
        to_session_id: row.session_id.clone(),
        config_dir: row.config_dir.clone(),
    })
}

/// (terminal, to-id) pairs whose deferred re-bind was already logged at info,
/// so a pane waiting for its new transcript is reported once, not every tick.
fn deferred_logged() -> &'static std::sync::Mutex<HashSet<(String, String)>> {
    static CELL: std::sync::OnceLock<std::sync::Mutex<HashSet<(String, String)>>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Apply [`decide_registry_rebind`] to every live PTY with a confirmed record.
///
/// The re-bind is written only when a transcript for the NEW id exists
/// (`transcript_exists(config_dir, working_dir, sid)`), so it lands CONFIRMED
/// and `record_open`'s single-tenant supersede retires the stale record on the
/// same terminal in the same write. Without a transcript yet (a `/clear`
/// before the first prompt) it is deferred to a later tick and logged once.
/// Returns the `Bind` actions written, for the `session-bound` event.
pub fn rebind_confirmed_from_registry<N, F>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    index: &N,
    snapshot: &ProcessSnapshot,
    transcript_exists: F,
) -> Vec<ReconcileAction>
where
    N: ProviderSessionIndex,
    F: Fn(&str, &str, &str) -> bool,
{
    let mut out = Vec::new();
    for pty in live {
        let Some(ai_pid) = pty.ai_pid else { continue };
        let Some(rec) = store.find_open_by_terminal(&pty.terminal_id) else {
            continue;
        };
        if rec.confirmed_at.is_none() {
            continue; // the bind pass owns unconfirmed records
        }
        let row = valid_provider_row(ai_pid, &index.metas_for_pid(ai_pid), snapshot);
        let Some(rb) = decide_registry_rebind(&rec, row.as_ref()) else {
            continue;
        };
        if !transcript_exists(&rb.config_dir, &pty.working_dir, &rb.to_session_id) {
            let first = deferred_logged()
                .lock()
                .map(|mut s| s.insert((rb.terminal_id.clone(), rb.to_session_id.clone())))
                .unwrap_or(false);
            if first {
                tracing::info!(
                    terminal_id = %rb.terminal_id,
                    record_session = %rb.from_session_id,
                    provider_session = %rb.to_session_id,
                    pid = ai_pid,
                    "session binder: the provider's live registry names a different session than the confirmed record — re-bind deferred until its transcript exists"
                );
            }
            continue;
        }
        store.record_open(bind_record(
            pty,
            &rb.to_session_id,
            &rb.config_dir,
            ORIGIN_AUTHORITATIVE,
            true,
            rec.zone_index,
        ));
        tracing::info!(
            terminal_id = %rb.terminal_id,
            from_session = %rb.from_session_id,
            to_session = %rb.to_session_id,
            pid = ai_pid,
            "session binder: re-bound a confirmed record from the provider's live registry"
        );
        out.push(ReconcileAction::Bind {
            terminal_id: rb.terminal_id,
            session_id: rb.to_session_id,
            config_dir: rb.config_dir,
            origin: BindOrigin::Authoritative,
            confirmed: true,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::run_reconcile_with_provider;
    use super::super::tests::*;
    use super::*;
    use tempfile::tempdir;

    /// FILETIME the fixtures' claudes were created at (CIM, µs-truncated).
    const FT: u64 = 134_369_000_000_000_000;

    fn snap(claudes: &[u32]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.names.insert(1, "System".into());
        for pid in claudes {
            s.names.insert(*pid, "claude.exe".into());
            s.creation_filetimes.insert(*pid, FT);
            s.creation_times.insert(*pid, 1_000);
        }
        s
    }

    fn meta_row(sid: &str, cfg: &str, proc_start: Option<u64>) -> ProviderSessionMeta {
        ProviderSessionMeta {
            session_id: sid.into(),
            name: None,
            proc_start: proc_start.map(|t| t.to_string()),
            pid_domain: Some("win32:spaceship".into()),
            config_dir: Some(cfg.into()),
        }
    }

    struct Rows(HashMap<u32, Vec<ProviderSessionMeta>>);
    impl ProviderSessionIndex for Rows {
        fn meta_for_pid(&self, pid: u32) -> Option<ProviderSessionMeta> {
            self.0.get(&pid).and_then(|v| v.first().cloned())
        }
        fn metas_for_pid(&self, pid: u32) -> Vec<ProviderSessionMeta> {
            self.0.get(&pid).cloned().unwrap_or_default()
        }
    }

    fn confirmed(id: &str, terminal: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            confirmed_at: Some(1),
            ..title_rec(id, terminal, Some("t"))
        }
    }

    /// THE incident (2026-10-20): panes P1 and P2 in one cwd, both claudes
    /// started BEFORE the transcript's first event, and the provider registry
    /// names P2's pid. The pass must bind P2 — not P1, whichever is visited
    /// first.
    ///
    /// Mutation check: deleting the claimed-id seed from the provider rows in
    /// `run_reconcile_with_provider` makes P1 (visited first) bind `0c4e3025`
    /// via rung 3 — the `p1` assertion fails. Deleting the rung call in
    /// `decide_bind_for_live_pty` fails the `p2` assertion.
    #[test]
    fn incident_two_panes_one_cwd_binds_the_pane_the_registry_names() {
        for p2_first in [false, true] {
            let dir = tempdir().unwrap();
            let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
            let p1 = pty_anchored("314176b7", 51000, 900, "D:/qontinui-root");
            let p2 = pty_anchored("601a25ac", 46416, 800, "D:/qontinui-root");
            let live = if p2_first { vec![p2, p1] } else { vec![p1, p2] };
            let snapshot = snap(&[51000, 46416]);
            let mut by_wd = HashMap::new();
            by_wd.insert(
                "D:/qontinui-root".to_string(),
                vec![cand("0c4e3025", "C:/claude/.claude-tiohorst", Some(1_000))],
            );
            let index = FakeIndex {
                by_wd,
                existing_ids: ["0c4e3025".to_string()].into_iter().collect(),
            };
            let rows = valid_rows_for_ptys(
                live.iter(),
                &Rows(
                    [(
                        46416,
                        vec![meta_row(
                            "0c4e3025",
                            "C:/claude/.claude-tiohorst",
                            Some(FT + 8),
                        )],
                    )]
                    .into_iter()
                    .collect(),
                ),
                &snapshot,
            );

            let actions = run_reconcile_with_provider(
                &store,
                &live,
                &snapshot,
                &index,
                &no_cmdlines(),
                &rows,
            );

            assert!(
                actions.contains(&ReconcileAction::Bind {
                    terminal_id: "601a25ac".into(),
                    session_id: "0c4e3025".into(),
                    config_dir: "C:/claude/.claude-tiohorst".into(),
                    origin: BindOrigin::Authoritative,
                    confirmed: true,
                }),
                "p2 binds from the registry (p2_first={p2_first}): {actions:?}"
            );
            assert!(
                !actions.iter().any(|a| matches!(a,
                    ReconcileAction::Bind { terminal_id, .. } if terminal_id == "314176b7")),
                "p1 must not be bound to the session P2 runs (p2_first={p2_first}): {actions:?}"
            );
            assert_eq!(
                store.get("0c4e3025").unwrap().terminal_id,
                "601a25ac",
                "the record lives on the pane the provider names"
            );
        }
    }

    /// A stale or reused-pid row never binds: the rung requires validity, and
    /// rung 3 then runs exactly as before.
    ///
    /// Mutation check: replacing the `== RowValidity::Valid` filter in
    /// `valid_provider_row` with `true` makes the reused-pid row bind
    /// `f2a062c0`/`b4c069ec` authoritatively and fails both assertions.
    #[test]
    fn a_stale_or_reused_pid_row_never_binds() {
        let snapshot = snap(&[38920]);
        // Reused pid: procStart 0.8 days off.
        assert_eq!(
            valid_provider_row(
                38920,
                &[meta_row("b4c069ec", "t", Some(FT - 700_000_000_000))],
                &snapshot
            ),
            None
        );
        // Dead pid.
        assert_eq!(
            valid_provider_row(4, &[meta_row("x", "t", Some(FT))], &snapshot),
            None
        );
        // No procStart (older CLI) — UNKNOWN, not evidence.
        assert_eq!(
            valid_provider_row(38920, &[meta_row("x", "t", None)], &snapshot),
            None
        );
        // Two files for one pid, one stale one valid: the valid one wins
        // whatever order the config dirs are walked in.
        let both = [
            meta_row("b4c069ec", ".claude-tiohorst", Some(FT - 700_000_000_000)),
            meta_row("f2a062c0", ".claude-paktis", Some(FT + 6)),
        ];
        assert_eq!(
            valid_provider_row(38920, &both, &snapshot),
            Some(ValidProviderRow {
                session_id: "f2a062c0".into(),
                config_dir: ".claude-paktis".into()
            })
        );
    }

    /// No registry row ⇒ the rung does not fire and rung 3 binds as before.
    #[test]
    fn no_row_falls_back_to_rung_three() {
        let pty = pty_anchored("t", 5, 1_000, "C:/repo");
        let cands = [cand("only", "C:/cfg", Some(1_500))];
        assert_eq!(
            super::super::decide_bind_for_live_pty(
                &pty,
                None,
                1_000,
                None,
                &cands,
                &no_claims(),
                None
            ),
            ReconcileAction::Bind {
                terminal_id: "t".into(),
                session_id: "only".into(),
                config_dir: "C:/cfg".into(),
                origin: BindOrigin::Observed,
                confirmed: true,
            }
        );
        // With a row: authoritative, from the registry; provisional without
        // a transcript, carrying the row's account.
        let row = ValidProviderRow {
            session_id: "reg".into(),
            config_dir: "C:/acct".into(),
        };
        assert_eq!(
            super::super::decide_bind_for_live_pty(
                &pty,
                None,
                1_000,
                None,
                &cands,
                &no_claims(),
                Some(&row)
            ),
            ReconcileAction::Bind {
                terminal_id: "t".into(),
                session_id: "reg".into(),
                config_dir: "C:/acct".into(),
                origin: BindOrigin::Authoritative,
                confirmed: false,
            }
        );
    }

    /// A fully-confirmed fleet with one in-pane `/clear`: one tick of the
    /// re-bind re-points the terminal at the new id, confirmed, and retires
    /// the stale record. A matching row, a missing transcript and an
    /// unconfirmed record do nothing.
    ///
    /// Mutation check: making `decide_registry_rebind` return `None` on a
    /// mismatch fails the first assertion; dropping the `transcript_exists`
    /// gate fails the deferred one.
    #[test]
    fn clear_in_a_confirmed_pane_rebinds_within_one_tick() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(confirmed("before-clear", "term-1"));
        store.record_open(confirmed("steady", "term-2"));
        let live = vec![
            pty_anchored("term-1", 100, 1_000, "C:/repo"),
            pty_anchored("term-2", 200, 1_000, "C:/repo"),
        ];
        let snapshot = snap(&[100, 200]);
        let index = Rows(
            [
                (100, vec![meta_row("after-clear", "C:/cfg", Some(FT))]),
                (200, vec![meta_row("steady", "C:/cfg", Some(FT))]),
            ]
            .into_iter()
            .collect(),
        );

        let actions =
            rebind_confirmed_from_registry(&store, &live, &index, &snapshot, |_, _, _| true);
        assert_eq!(
            actions,
            vec![ReconcileAction::Bind {
                terminal_id: "term-1".into(),
                session_id: "after-clear".into(),
                config_dir: "C:/cfg".into(),
                origin: BindOrigin::Authoritative,
                confirmed: true,
            }]
        );
        assert_eq!(
            store
                .find_open_by_terminal("term-1")
                .unwrap()
                .claude_session_id,
            "after-clear"
        );
        assert_eq!(store.get("before-clear").unwrap().state, "closed");
        assert_eq!(store.get("steady").unwrap().state, "open");

        // Converged: a second tick writes nothing.
        assert!(
            rebind_confirmed_from_registry(&store, &live, &index, &snapshot, |_, _, _| true)
                .is_empty()
        );

        // Deferred: no transcript for the new id yet.
        let dir2 = tempdir().unwrap();
        let store2 = SessionLifecycleStore::open(dir2.path().join("s.json")).unwrap();
        store2.record_open(confirmed("before-clear", "term-1"));
        assert!(
            rebind_confirmed_from_registry(&store2, &live, &index, &snapshot, |_, _, _| false)
                .is_empty()
        );
        assert_eq!(
            store2
                .find_open_by_terminal("term-1")
                .unwrap()
                .claude_session_id,
            "before-clear"
        );
    }

    #[test]
    fn decide_registry_rebind_arms() {
        let row = ValidProviderRow {
            session_id: "b".into(),
            config_dir: "c".into(),
        };
        let rec = confirmed("a", "t");
        assert_eq!(
            decide_registry_rebind(&rec, Some(&row)),
            Some(RegistryRebind {
                terminal_id: "t".into(),
                from_session_id: "a".into(),
                to_session_id: "b".into(),
                config_dir: "c".into(),
            })
        );
        assert_eq!(decide_registry_rebind(&rec, None), None, "no valid row");
        assert_eq!(
            decide_registry_rebind(&confirmed("b", "t"), Some(&row)),
            None,
            "same id"
        );
        let unconfirmed = TerminalSessionRecord {
            confirmed_at: None,
            ..confirmed("a", "t")
        };
        assert_eq!(
            decide_registry_rebind(&unconfirmed, Some(&row)),
            None,
            "bind pass owns it"
        );
    }
}
