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
//!
//! Both withhold a pane [`binder_exclusion`] names (2026-10-23 review fix 1):
//! one whose valid row names an id another live pane's valid row also names
//! (a fork — binding either pane would be a coin toss), and either side of an
//! account-migration hand-off ([`HandoffExclusions`], shared with the
//! duplicate-host detector). A valid row that DISAGREES with the launch argv
//! wins over it (fix 6, in `decide_bind_for_live_pty`).

use std::collections::{HashMap, HashSet};

use super::{
    bind_record, BindOrigin, LivePty, ProviderSessionIndex, ProviderSessionMeta, ReconcileAction,
    TranscriptCandidate,
};
use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{registry_row_validity, RowValidity};
use crate::session::duplicate_hosts::HandoffExclusions;
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

/// Session ids that the VALID provider rows of TWO OR MORE live panes name — a
/// forked conversation (the 2026-10-21 `0c4e3025` shape). The binder binds
/// neither pane to such an id and re-binds neither (2026-10-23 review fix 1):
/// whichever it picked, the other pane would be the one running it too.
pub fn forked_session_ids<'a>(
    live: impl IntoIterator<Item = &'a LivePty>,
    rows: &HashMap<u32, ValidProviderRow>,
) -> HashSet<String> {
    let mut panes: HashMap<&str, HashSet<&str>> = HashMap::new();
    for pty in live {
        if let Some(row) = pty.ai_pid.and_then(|pid| rows.get(&pid)) {
            panes
                .entry(row.session_id.as_str())
                .or_default()
                .insert(pty.terminal_id.as_str());
        }
    }
    panes
        .into_iter()
        .filter(|(_, terminals)| terminals.len() >= 2)
        .map(|(sid, _)| sid.to_string())
        .collect()
}

/// Pure: why the binder must leave `terminal_id` alone this pass, if it must.
///
/// - its valid row names a FORKED id ([`forked_session_ids`]);
/// - the terminal is one side of a migration hand-off — superseded by the
///   store or respawned by the migration within the hand-off window
///   ([`HandoffExclusions`]);
/// - its valid row names a session the migration just respawned elsewhere.
///
/// The migration retires the old pane itself; binding or re-binding either
/// side meanwhile would write a second open record for one conversation.
pub fn binder_exclusion(
    terminal_id: &str,
    row: Option<&ValidProviderRow>,
    forked: &HashSet<String>,
    exclusions: &HandoffExclusions,
) -> Option<&'static str> {
    if row.is_some_and(|r| forked.contains(&r.session_id)) {
        return Some("its provider row names a session another live pane also runs");
    }
    if exclusions.terminals.contains(terminal_id) {
        return Some("the pane is one side of an account-migration hand-off");
    }
    if row.is_some_and(|r| exclusions.sessions.contains(&r.session_id)) {
        return Some("its provider row names a session an account migration just respawned");
    }
    None
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

/// One re-bind [`plan_registry_rebinds`] decided on, still to be gated on
/// the new id's transcript existing.
#[derive(Debug, Clone)]
pub struct PlannedRebind {
    pub pty: LivePty,
    pub ai_pid: u32,
    pub zone_index: i32,
    pub rebind: RegistryRebind,
}

/// The decision half of [`rebind_confirmed_from_registry`]: every live PTY
/// with a confirmed record whose pane's valid row names a different id
/// ([`decide_registry_rebind`]) and that no [`binder_exclusion`] withholds.
/// Reads no files and writes nothing, so a caller on the async runtime can
/// probe the transcripts off it before [`apply_registry_rebinds`].
///
/// Also forgets deferred-re-bind log entries for terminals that are no
/// longer live, so that set does not grow for the runner's lifetime.
pub fn plan_registry_rebinds<N>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    index: &N,
    snapshot: &ProcessSnapshot,
    exclusions: &HandoffExclusions,
) -> Vec<PlannedRebind>
where
    N: ProviderSessionIndex,
{
    prune_deferred_logged(live);
    let rows = valid_rows_for_ptys(live.iter(), index, snapshot);
    let forked = forked_session_ids(live.iter(), &rows);
    let mut out = Vec::new();
    for pty in live {
        let Some(ai_pid) = pty.ai_pid else { continue };
        let Some(rec) = store.find_open_by_terminal(&pty.terminal_id) else {
            continue;
        };
        if rec.confirmed_at.is_none() {
            continue; // the bind pass owns unconfirmed records
        }
        let row = rows.get(&ai_pid);
        let Some(rb) = decide_registry_rebind(&rec, row) else {
            continue;
        };
        if let Some(why) = binder_exclusion(&pty.terminal_id, row, &forked, exclusions) {
            tracing::debug!(
                terminal_id = %rb.terminal_id,
                record_session = %rb.from_session_id,
                provider_session = %rb.to_session_id,
                why,
                "session binder: confirmed-record re-bind withheld"
            );
            continue;
        }
        out.push(PlannedRebind {
            pty: pty.clone(),
            ai_pid,
            zone_index: rec.zone_index,
            rebind: rb,
        });
    }
    out
}

/// Drop deferred-log entries whose terminal is not in `live`.
fn prune_deferred_logged(live: &[LivePty]) {
    if let Ok(mut set) = deferred_logged().lock() {
        retain_live_terminals(&mut set, live);
    }
}

/// Pure: keep only the `(terminal, sid)` entries whose terminal is in `live`.
fn retain_live_terminals(set: &mut HashSet<(String, String)>, live: &[LivePty]) {
    set.retain(|(terminal, _)| live.iter().any(|p| &p.terminal_id == terminal));
}

/// The write half of [`rebind_confirmed_from_registry`]: each planned re-bind
/// paired with whether a transcript for the NEW id exists. With one, the
/// re-bind is written CONFIRMED, so `record_open`'s single-tenant supersede
/// retires the stale record on the same terminal in the same write. Without
/// one yet (a `/clear` before the first prompt) it is deferred to a later
/// tick and logged once. Returns the `Bind` actions written, for the
/// `session-bound` event.
///
/// The plan was made before the caller's transcript probe, which awaits, so
/// each re-bind is RE-CHECKED against the store as of `now_ms` first
/// ([`rebind_still_applies`]): one whose record moved on, or whose pane or
/// session became part of a migration hand-off meanwhile, is skipped
/// (round-13 review S3).
pub fn apply_registry_rebinds(
    store: &SessionLifecycleStore,
    planned: Vec<(PlannedRebind, bool)>,
    now_ms: i64,
) -> Vec<ReconcileAction> {
    let exclusions = if planned.is_empty() {
        HandoffExclusions::default()
    } else {
        HandoffExclusions::current(store, now_ms)
    };
    let mut out = Vec::new();
    for (p, transcript_exists) in planned {
        let PlannedRebind {
            pty,
            ai_pid,
            zone_index,
            rebind: rb,
        } = p;
        if let Err(why) = rebind_still_applies(store, &rb, &exclusions) {
            tracing::debug!(
                terminal_id = %rb.terminal_id,
                record_session = %rb.from_session_id,
                provider_session = %rb.to_session_id,
                why,
                "session binder: planned re-bind dropped — the store changed while its transcript was probed"
            );
            continue;
        }
        if !transcript_exists {
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
            &pty,
            &rb.to_session_id,
            &rb.config_dir,
            ORIGIN_AUTHORITATIVE,
            true,
            zone_index,
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

/// Whether a planned re-bind still holds against the store NOW: the pane's open
/// record is still the confirmed `rb.from_session_id` the plan saw, and
/// [`binder_exclusion`] (over freshly read `exclusions`) still names nothing.
/// The fork test is the plan's: it reads the process table, which this
/// re-check does not re-read.
fn rebind_still_applies(
    store: &SessionLifecycleStore,
    rb: &RegistryRebind,
    exclusions: &HandoffExclusions,
) -> Result<(), &'static str> {
    let unchanged = store
        .find_open_by_terminal(&rb.terminal_id)
        .is_some_and(|r| r.claude_session_id == rb.from_session_id && r.confirmed_at.is_some());
    if !unchanged {
        return Err("the pane's record no longer names the session the plan re-binds from");
    }
    let row = ValidProviderRow {
        session_id: rb.to_session_id.clone(),
        config_dir: rb.config_dir.clone(),
    };
    match binder_exclusion(&rb.terminal_id, Some(&row), &HashSet::new(), exclusions) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// Apply [`decide_registry_rebind`] to every live PTY with a confirmed record:
/// [`plan_registry_rebinds`], then [`apply_registry_rebinds`] gated on
/// `transcript_exists(config_dir, working_dir, sid)` for the NEW id. The
/// synchronous composition, for callers already off the async runtime (and
/// the tests); `refresh_titles_pass` probes on the blocking pool instead.
///
/// A pane [`binder_exclusion`] names (forked id, migration hand-off) is never
/// re-bound.
pub fn rebind_confirmed_from_registry<N, F>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    index: &N,
    snapshot: &ProcessSnapshot,
    exclusions: &HandoffExclusions,
    transcript_exists: F,
) -> Vec<ReconcileAction>
where
    N: ProviderSessionIndex,
    F: Fn(&str, &str, &str) -> bool,
{
    let planned = plan_registry_rebinds(store, live, index, snapshot, exclusions)
        .into_iter()
        .map(|p| {
            let found = transcript_exists(
                &p.rebind.config_dir,
                &p.pty.working_dir,
                &p.rebind.to_session_id,
            );
            (p, found)
        })
        .collect();
    apply_registry_rebinds(store, planned, chrono::Utc::now().timestamp_millis())
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
                &none(),
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
            rebind_confirmed_from_registry(&store, &live, &index, &snapshot, &none(), |_, _, _| {
                true
            });
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
        assert!(rebind_confirmed_from_registry(
            &store,
            &live,
            &index,
            &snapshot,
            &none(),
            |_, _, _| true
        )
        .is_empty());

        // Deferred: no transcript for the new id yet.
        let dir2 = tempdir().unwrap();
        let store2 = SessionLifecycleStore::open(dir2.path().join("s.json")).unwrap();
        store2.record_open(confirmed("before-clear", "term-1"));
        assert!(rebind_confirmed_from_registry(
            &store2,
            &live,
            &index,
            &snapshot,
            &none(),
            |_, _, _| false
        )
        .is_empty());
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

    // ── 2026-10-23 review fixes ─────────────────────────────────────────────

    fn none() -> HandoffExclusions {
        HandoffExclusions::default()
    }

    fn binds(actions: &[ReconcileAction]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                ReconcileAction::Bind { terminal_id, .. } => Some(terminal_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Fix 1: two live panes whose VALID rows name ONE id (a forked
    /// conversation) — the bind pass binds neither, at any rung.
    ///
    /// Mutation check: deleting the `forked` arm of `binder_exclusion` binds
    /// whichever pane is visited first.
    #[test]
    fn a_forked_id_binds_neither_pane() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let live = vec![
            pty_anchored("p1", 51000, 900, "C:/repo"),
            pty_anchored("p2", 46416, 800, "C:/repo"),
        ];
        let snapshot = snap(&[51000, 46416]);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("0c4e3025", "C:/cfg", Some(1_000))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: HashSet::new(),
        };
        let rows = valid_rows_for_ptys(
            live.iter(),
            &Rows(
                [
                    (51000, vec![meta_row("0c4e3025", "C:/a", Some(FT))]),
                    (46416, vec![meta_row("0c4e3025", "C:/b", Some(FT))]),
                ]
                .into_iter()
                .collect(),
            ),
            &snapshot,
        );
        assert_eq!(
            forked_session_ids(live.iter(), &rows),
            ["0c4e3025".to_string()].into_iter().collect()
        );
        let actions = run_reconcile_with_provider(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &rows,
            &none(),
        );
        assert!(binds(&actions).is_empty(), "{actions:?}");
        assert!(store.get("0c4e3025").is_none());
    }

    /// Fix 1: a pane on either side of a migration hand-off — superseded /
    /// respawned terminal, or a row naming the respawned session — is left
    /// alone at every rung, even with a unique rung-3 candidate.
    ///
    /// Mutation checks: deleting the `exclusions.terminals` arm binds the
    /// superseded pane via rung 3; deleting the `exclusions.sessions` arm
    /// binds the respawned session via rung 2½.
    #[test]
    fn a_migration_handoff_pane_is_left_alone_at_every_rung() {
        let snapshot = snap(&[100, 200]);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("only", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: HashSet::new(),
        };
        // Superseded terminal, no provider row: rung 3 would bind "only".
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let old = vec![pty_anchored("old-pane", 100, 1_000, "C:/repo")];
        let by_terminal = HandoffExclusions {
            terminals: ["old-pane".to_string()].into_iter().collect(),
            sessions: HashSet::new(),
        };
        let actions = run_reconcile_with_provider(
            &store,
            &old,
            &snapshot,
            &index,
            &no_cmdlines(),
            &HashMap::new(),
            &by_terminal,
        );
        assert!(binds(&actions).is_empty(), "{actions:?}");
        // Sanity: without the exclusion the same pane binds.
        assert_eq!(
            binds(&run_reconcile_with_provider(
                &store,
                &old,
                &snapshot,
                &index,
                &no_cmdlines(),
                &HashMap::new(),
                &none(),
            )),
            vec!["old-pane"]
        );

        // A row naming the session a migration just respawned elsewhere.
        let dir2 = tempdir().unwrap();
        let store2 = SessionLifecycleStore::open(dir2.path().join("s.json")).unwrap();
        let other = vec![pty_anchored("other-pane", 200, 1_000, "C:/repo")];
        let rows: HashMap<u32, ValidProviderRow> = [(
            200,
            ValidProviderRow {
                session_id: "respawned".into(),
                config_dir: "C:/cfg".into(),
            },
        )]
        .into_iter()
        .collect();
        let by_session = HandoffExclusions {
            terminals: HashSet::new(),
            sessions: ["respawned".to_string()].into_iter().collect(),
        };
        let actions = run_reconcile_with_provider(
            &store2,
            &other,
            &snapshot,
            &index,
            &no_cmdlines(),
            &rows,
            &by_session,
        );
        assert!(binds(&actions).is_empty(), "{actions:?}");
    }

    /// Fix 1: the confirmed-record re-bind withholds the same panes — a
    /// re-bind to a forked id, and a re-bind of a superseded terminal.
    ///
    /// Mutation check: deleting the `binder_exclusion` call in
    /// `rebind_confirmed_from_registry` re-binds both.
    #[test]
    fn the_confirmed_rebind_withholds_forked_and_handoff_panes() {
        let snapshot = snap(&[100, 200, 300]);
        let live = vec![
            pty_anchored("term-1", 100, 1_000, "C:/repo"),
            pty_anchored("term-2", 200, 1_000, "C:/repo"),
            pty_anchored("term-3", 300, 1_000, "C:/repo"),
        ];
        let index = Rows(
            [
                // term-1 and term-2 both run "forked" now.
                (100, vec![meta_row("forked", "C:/cfg", Some(FT))]),
                (200, vec![meta_row("forked", "C:/cfg", Some(FT))]),
                // term-3 was /clear-ed — but it is being migrated away.
                (300, vec![meta_row("after-clear", "C:/cfg", Some(FT))]),
            ]
            .into_iter()
            .collect(),
        );
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(confirmed("a", "term-1"));
        store.record_open(confirmed("b", "term-2"));
        store.record_open(confirmed("c", "term-3"));
        let ex = HandoffExclusions {
            terminals: ["term-3".to_string()].into_iter().collect(),
            sessions: HashSet::new(),
        };
        let actions =
            rebind_confirmed_from_registry(&store, &live, &index, &snapshot, &ex, |_, _, _| true);
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(
            store
                .find_open_by_terminal("term-3")
                .unwrap()
                .claude_session_id,
            "c"
        );
        // Sanity: without the hand-off, term-3 re-binds (the forked pair still
        // does not).
        let actions =
            rebind_confirmed_from_registry(&store, &live, &index, &snapshot, &none(), |_, _, _| {
                true
            });
        assert_eq!(binds(&actions), vec!["term-3"]);
    }

    /// Fix 6: a VALID registry row that disagrees with the launch argv wins —
    /// the argv is the id the process STARTED with, the registry the one it
    /// runs after an in-pane `/clear`. When they agree, rung 2 binds as before.
    ///
    /// Mutation check: deleting the disagreement arm binds the stale
    /// `launched` id.
    #[test]
    fn a_valid_row_beats_a_disagreeing_stale_launch_id() {
        let pty = pty_anchored("t", 5, 1_000, "C:/repo");
        let cands = [cand("reg", "C:/cfg", Some(1_500))];
        let row = ValidProviderRow {
            session_id: "reg".into(),
            config_dir: "C:/acct".into(),
        };
        assert_eq!(
            super::super::decide_bind_for_live_pty(
                &pty,
                None,
                1_000,
                Some("launched"),
                &cands,
                &no_claims(),
                Some(&row)
            ),
            ReconcileAction::Bind {
                terminal_id: "t".into(),
                session_id: "reg".into(),
                config_dir: "C:/cfg".into(),
                origin: BindOrigin::Authoritative,
                confirmed: true,
            }
        );
        // Agreement: rung 2 (no row account fallback — the candidate's or
        // empty), exactly as before.
        let only_launch = [cand("other", "C:/cfg", Some(1_500))];
        assert_eq!(
            super::super::decide_bind_for_live_pty(
                &pty,
                None,
                1_000,
                Some("reg"),
                &only_launch,
                &no_claims(),
                Some(&row)
            ),
            ReconcileAction::Bind {
                terminal_id: "t".into(),
                session_id: "reg".into(),
                config_dir: String::new(),
                origin: BindOrigin::Authoritative,
                confirmed: false,
            }
        );
    }

    /// Round-13 S3: a planned re-bind is re-checked when applied — after the
    /// await — and skipped when the pane became one side of a hand-off, or
    /// its record moved on, in the meantime. Unchanged, it is applied.
    ///
    /// Mutation check: dropping the `rebind_still_applies` gate in
    /// `apply_registry_rebinds` writes both stale re-binds and fails the
    /// first two assertions.
    #[test]
    fn a_planned_rebind_the_store_moved_past_is_skipped() {
        let plan = |term: &str| PlannedRebind {
            pty: pty_anchored(term, 100, 1_000, "C:/repo"),
            ai_pid: 100,
            zone_index: 0,
            rebind: RegistryRebind {
                terminal_id: term.to_string(),
                from_session_id: "sid-old".to_string(),
                to_session_id: format!("sid-new-{term}"),
                config_dir: "C:/claude/.claude-r13s3".to_string(),
            },
        };
        let now = chrono::Utc::now().timestamp_millis();
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // Superseded (a migration hand-off) during the probe.
        store.record_open(confirmed("sid-old", "r13s3-handoff"));
        store.mark_terminal_superseded("r13s3-handoff", "r13s3-handoff-new");
        assert!(
            apply_registry_rebinds(&store, vec![(plan("r13s3-handoff"), true)], now).is_empty(),
            "a hand-off pane is not re-bound"
        );

        // The record was re-bound to another id during the probe.
        store.record_open(confirmed("sid-other", "r13s3-moved"));
        assert!(
            apply_registry_rebinds(&store, vec![(plan("r13s3-moved"), true)], now).is_empty(),
            "a record that moved on is not overwritten"
        );
        assert_eq!(
            store
                .find_open_by_terminal("r13s3-moved")
                .unwrap()
                .claude_session_id,
            "sid-other"
        );

        // Control: nothing changed — applied.
        store.record_open(confirmed("sid-old", "r13s3-same"));
        assert_eq!(
            apply_registry_rebinds(&store, vec![(plan("r13s3-same"), true)], now).len(),
            1
        );
        assert_eq!(
            store
                .find_open_by_terminal("r13s3-same")
                .unwrap()
                .claude_session_id,
            "sid-new-r13s3-same"
        );
    }

    /// Round-12 S4: the deferred-re-bind log forgets terminals that are no
    /// longer live, instead of growing for the runner's lifetime. Exercised on
    /// a local set: the process-global log is pruned by every parallel test
    /// that runs a re-bind pass, so asserting on it races.
    ///
    /// Mutation check: making `retain_live_terminals` a no-op keeps the
    /// closed terminal's entry and fails the first assertion.
    #[test]
    fn the_deferred_rebind_log_forgets_closed_terminals() {
        let key = |t: &str| (t.to_string(), "to-sid".to_string());
        let mut set: HashSet<(String, String)> = [key("closed"), key("live")].into();
        retain_live_terminals(&mut set, &[pty_anchored("live", 1, 1, "C:/repo")]);
        assert!(!set.contains(&key("closed")));
        assert!(set.contains(&key("live")));
    }

    /// Fix 8: the title / re-bind pass reads the tick's shared raw registry,
    /// never the disk — a registry built in memory renames the tab.
    ///
    /// Mutation check: making `refresh_titles_pass` build its index from a
    /// fresh disk read instead of `registry` leaves the title unchanged.
    #[tokio::test]
    async fn the_title_pass_reads_the_ticks_shared_registry() {
        use crate::session::claude_session_registry::{RawRegistry, RawRegistryFile};
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(confirmed("sid-1", "term-1"));
        let live = vec![pty_anchored("term-1", 100, 1_000, "C:/repo")];
        let raw = RawRegistry {
            files: vec![RawRegistryFile {
                config_dir: std::path::PathBuf::from("C:/no-such-account"),
                file_pid: Some(100),
                content: Ok(format!(
                    r#"{{"pid":100,"sessionId":"sid-1","name":"renamed","procStart":"{FT}","pidDomain":"win32:spaceship","kind":"interactive"}}"#
                )),
            }],
            dir_errors: vec![],
            complete: true,
        };
        let out = super::super::refresh_titles_pass(&store, &live, &snap(&[100]), &raw).await;
        assert_eq!(out.titles.len(), 1, "{:?}", out.titles);
        assert_eq!(
            store
                .find_open_by_terminal("term-1")
                .unwrap()
                .title
                .as_deref(),
            Some("renamed")
        );
    }
}
