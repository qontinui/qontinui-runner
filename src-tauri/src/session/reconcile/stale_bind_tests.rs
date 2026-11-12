//! Plan `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
//! Phase 1: every bind [`run_reconcile`] makes goes through the store's
//! write-time liveness guard ([`SessionLifecycleStore::bind_inferred`]), and a
//! bind keeps the session's own zone while its row is on the pane's page.
//!
//! The shape these pin was measured on 2026-11-11 (coord finding `f0a0c310`):
//! the boot pass read the live panes and the source panes' `claude --resume
//! <id>` command lines, the account migration then moved three sessions to new
//! panes and closed the source panes, the pass's transcript prescan timed out
//! 30 s later, and rung 2 (which never consults that index) bound all three
//! back onto the closed source panes, resetting their zones to -1.

use super::*;

const SESSION: &str = "1d221281-0000-4000-8000-000000000001";

/// A captured pane whose claude anchor (`anchor_pid`) is already resolved.
fn pane(terminal_id: &str, anchor_pid: u32) -> LivePty {
    LivePty {
        terminal_id: terminal_id.to_string(),
        pid: Some(anchor_pid + 10_000),
        working_dir: "C:/repo".to_string(),
        page_id: "default".to_string(),
        title: "claude".to_string(),
        ai_pid: Some(anchor_pid),
        ai_start_unix: Some(1_000),
    }
}

/// An open row for `id` on `terminal_id` in zone `zone_index`.
fn row(id: &str, terminal_id: &str, zone_index: i32, confirmed: bool) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: Some("C:/claude/.claude-ben".to_string()),
        working_dir: Some("C:/repo".to_string()),
        page_id: "default".to_string(),
        zone_index,
        title: Some("claude".to_string()),
        terminal_id: terminal_id.to_string(),
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: DEFAULT_PROVIDER.to_string(),
        origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
        restore_pending_at: None,
        awaiting_account_since: None,
        confirmed_at: confirmed.then_some(1),
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        wind_down_outcome: None,
        wind_down_at: None,
        wind_down_arm: None,
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
        adopted_from: None,
        last_session_start_at: None,
        last_alive_at: None,
        gate_id: None,
        gate_consuming_device_id: None,
        gate_bound_boot_ms: None,
    }
}

fn store() -> (tempfile::TempDir, SessionLifecycleStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    (dir, store)
}

/// The panes alive at WRITE time, as the drivers' `live_panes_of` reads
/// them — independent of the list the pass captured at its start.
fn live_now(panes: &[&str]) -> impl Fn() -> HashSet<String> {
    let panes: HashSet<String> = panes.iter().map(|p| p.to_string()).collect();
    move || panes.clone()
}

/// Every anchor pid in `pids` names `SESSION` on its command line (`--resume`).
fn resumes(pids: &[u32]) -> HashMap<u32, String> {
    pids.iter().map(|pid| (*pid, SESSION.to_string())).collect()
}

/// Reading 2, end to end. The migration's own spawn write put the row on its
/// new pane; the pass captured the source pane and its `--resume` command line
/// before the source pane exited, and its prescan timed out (an empty index).
/// The bind onto the source pane is refused, the row stays on its live pane,
/// and no tab is stamped for the refused bind.
#[test]
fn a_stale_rung_two_bind_onto_an_exited_source_pane_is_refused_and_the_row_stays_on_its_live_pane()
{
    let (_dir, store) = store();
    store.record_open(row(SESSION, "new-pane", 2, true));
    let captured = vec![pane("source-pane", 701), pane("new-pane", 702)];

    let actions = run_reconcile(
        &store,
        &captured,
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[701]),
        &live_now(&["new-pane"]),
    );

    let rec = store.get(SESSION).unwrap();
    assert_eq!(
        rec.terminal_id, "new-pane",
        "the row stays on the pane the session runs in, got {actions:?}"
    );
    assert_eq!(rec.state, "open");
    assert_eq!(rec.zone_index, 2);
    assert!(
        actions.contains(&ReconcileAction::BindNotWritten {
            terminal_id: "source-pane".to_string(),
            session_id: SESSION.to_string(),
            reason: InferredBind::TargetNotLive {
                terminal_id: "source-pane".to_string()
            },
        }),
        "the stale bind is reported refused because its pane exited, got {actions:?}"
    );
    assert!(
        session_bound_payloads(&actions).is_empty(),
        "a refused bind stamps no tab, got {actions:?}"
    );
}

/// Two live panes run one session (a hand-typed `claude --resume` beside the
/// running one). Each pass, the pane without the row is unbound and rung 2
/// names the session for it; moving the open row there would flip it between
/// the panes on every pass. The guard keeps it on the pane it is on.
#[test]
fn a_rung_two_bind_never_moves_an_open_row_off_its_live_pane() {
    let (_dir, store) = store();
    store.record_open(row(SESSION, "pane-a", 0, true));
    let both = vec![pane("pane-a", 801), pane("pane-b", 802)];

    for pass in 1..=2 {
        let actions = run_reconcile(
            &store,
            &both,
            &ProcessSnapshot::default(),
            &PrescannedTranscriptIndex::default(),
            &resumes(&[801, 802]),
            &live_now(&["pane-a", "pane-b"]),
        );
        assert_eq!(
            store.get(SESSION).unwrap().terminal_id,
            "pane-a",
            "pass {pass}: the row stays on the pane it is on, got {actions:?}"
        );
        assert!(
            actions.contains(&ReconcileAction::BindNotWritten {
                terminal_id: "pane-b".to_string(),
                session_id: SESSION.to_string(),
                reason: InferredBind::RowOnLivePane {
                    terminal_id: "pane-a".to_string()
                },
            }),
            "pass {pass}: got {actions:?}"
        );
    }
}

/// The dead-to-live heal is unchanged: a row left on a pane that is gone binds
/// to the live pane whose claude names it, and that bind stamps the tab.
#[test]
fn the_dead_to_live_heal_still_binds_a_row_on_a_gone_pane() {
    let (_dir, store) = store();
    store.record_open(row(SESSION, "gone-pane", 2, true));

    let actions = run_reconcile(
        &store,
        &[pane("new-pane", 901)],
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[901]),
        &live_now(&["new-pane"]),
    );

    assert_eq!(store.get(SESSION).unwrap().terminal_id, "new-pane");
    assert!(
        actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Bind { terminal_id, session_id, .. }
                if terminal_id == "new-pane" && session_id == SESSION
        )),
        "got {actions:?}"
    );
    assert_eq!(session_bound_payloads(&actions).len(), 1);
}

/// The zone is the session's (finding `f0a0c310`: both binds reset it to -1).
/// The pane the session moves to has no row of its own yet, so a zone looked
/// up by TERMINAL found nothing and wrote the sentinel over the session's real
/// zone. A respawn puts the pane in the zone its session's row names
/// (`account_migration.rs` respawns "on the same grid page/zone").
#[test]
fn a_reconcile_bind_keeps_the_sessions_zone() {
    let (_dir, store) = store();
    store.record_open(row(SESSION, "gone-pane", 4, true));

    run_reconcile(
        &store,
        &[pane("new-pane", 901)],
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[901]),
        &live_now(&["new-pane"]),
    );

    let rec = store.get(SESSION).unwrap();
    assert_eq!(rec.terminal_id, "new-pane");
    assert_eq!(rec.zone_index, 4, "the bind keeps the session's zone");
}

/// The precedence's other half: a session whose own zone is unknown takes the
/// zone the pane's own row knows (here the identity seam's provisional row,
/// which the bind then supersedes), rather than keeping the sentinel.
#[test]
fn a_session_with_no_known_zone_takes_the_panes_known_zone() {
    let (_dir, store) = store();
    store.record_open(row(SESSION, "gone-pane", UNZONED_ZONE_INDEX, true));
    store.record_open(row("seam-pin", "new-pane", 6, false));

    run_reconcile(
        &store,
        &[pane("new-pane", 901)],
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[901]),
        &live_now(&["new-pane"]),
    );

    let rec = store.get(SESSION).unwrap();
    assert_eq!(rec.terminal_id, "new-pane");
    assert_eq!(rec.zone_index, 6);
    assert_eq!(store.get("seam-pin").unwrap().state, "closed");
}

/// The pane's own row as production writes it: the identity seam's spawn row
/// (`record_pinned_session_open` with a literal zone 0, unconfirmed), which
/// nothing revises for an id no tab carries. Its placeholder never overwrites
/// the session's known zone on the same page.
///
/// Mutation: the pane's own row looked up before the session's zone.
#[test]
fn a_seam_spawn_rows_placeholder_zone_never_overwrites_the_sessions_same_page_zone() {
    let (_dir, store) = store();
    store.record_open(row(SESSION, "gone-pane", 4, true));
    seam_spawn_row(&store, "new-pane", "default");

    run_reconcile(
        &store,
        &[pane("new-pane", 901)],
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[901]),
        &live_now(&["new-pane"]),
    );

    let rec = store.get(SESSION).unwrap();
    assert_eq!(rec.terminal_id, "new-pane");
    assert_eq!(rec.zone_index, 4, "the session's zone, not the seam's 0");
}

/// A session resumed by hand into a pane on ANOTHER page does not carry its old
/// page's zone onto the new page: the bind takes the page from the pane, so the
/// old zone would place it in a zone of a page it was never in. The pane's own
/// row (the seam's spawn row, as production writes it) is the fallback; its
/// zone is the seam's placeholder 0, a known gap in the seam rather than in
/// this pass.
///
/// Mutation: the session-zone lookup's same-page filter removed.
#[test]
fn a_session_resumed_onto_another_page_does_not_carry_its_old_pages_zone() {
    let (_dir, store) = store();
    store.record_open(TerminalSessionRecord {
        page_id: "page-1".to_string(),
        ..row(SESSION, "gone-pane", 1, true)
    });
    seam_spawn_row(&store, "new-pane", "page-2");
    let other_page = LivePty {
        page_id: "page-2".to_string(),
        ..pane("new-pane", 901)
    };

    run_reconcile(
        &store,
        &[other_page],
        &ProcessSnapshot::default(),
        &PrescannedTranscriptIndex::default(),
        &resumes(&[901]),
        &live_now(&["new-pane"]),
    );

    let rec = store.get(SESSION).unwrap();
    assert_eq!(rec.terminal_id, "new-pane");
    assert_eq!(rec.page_id, "page-2");
    assert_ne!(
        rec.zone_index, 1,
        "the old page's zone never lands on page-2"
    );
    assert_eq!(rec.zone_index, 0, "the pane's own row's zone");
}

/// The identity seam's spawn write for a pane (`terminal/session.rs`): an
/// unconfirmed, seam-keyed row with the literal zone 0.
fn seam_spawn_row(store: &SessionLifecycleStore, terminal_id: &str, page_id: &str) {
    crate::commands::terminal::record_pinned_session_open(
        store,
        format!("seam-{terminal_id}"),
        terminal_id.to_string(),
        None,
        "C:/repo".to_string(),
        "claude".to_string(),
        page_id.to_string(),
        0,
        DEFAULT_PROVIDER.to_string(),
        None,
    );
}
