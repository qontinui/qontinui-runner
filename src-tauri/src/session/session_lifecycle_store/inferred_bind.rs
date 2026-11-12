//! The write-time liveness guard on the reconcile binder's writes (plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`, Phase 1).
//!
//! The binder ([`crate::session::reconcile::run_reconcile`]) infers which pane a
//! session runs in from evidence it gathered BEFORE it writes: the pass reads
//! the live PTYs and their claude command lines first, then waits on a
//! transcript prescan bounded at 30 s. On 2026-11-11 that gap re-pointed three
//! account-migrated sessions at their source panes, which had closed 30 s
//! earlier, and the ledger read them `missing` for ~42 s, until the next
//! periodic pass bound them back (coord finding `f0a0c310`). An inferred bind is
//! therefore judged against the panes alive at WRITE time, under the store
//! lock: [`super::SessionLifecycleStore::bind_inferred`] asks [`refusal`] before
//! it merges.

use super::TerminalSessionRecord;

/// What [`super::SessionLifecycleStore::bind_inferred`] did with one inferred
/// bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferredBind {
    /// Written, through the same merge as
    /// [`super::SessionLifecycleStore::record_open`].
    Bound,
    /// Refused: the pane the evidence named is not a live PTY of this process
    /// at write time. Nothing was written.
    TargetNotLive { terminal_id: String },
    /// Refused: the session's row is OPEN on another pane that is live at write
    /// time. The session runs there, and an inference does not move a running
    /// session. Nothing was written.
    RowOnLivePane { terminal_id: String },
    /// Not written: the store's lock was poisoned.
    StoreUnavailable,
}

/// Why an inferred bind of `target_terminal` must not be written over
/// `existing` (the session's row now, if it has one), or `None` when it may be.
///
/// - The target must be a live PTY of this process now.
/// - An OPEN row on ANOTHER live pane stays where it is: the session runs there.
///   Only an authoritative writer moves a running session — the account
///   migration's own spawn write, or a SessionStart hook, both through
///   [`super::SessionLifecycleStore::record_open`].
/// - Anything else binds: a session with no row, a bind onto the row's own pane
///   (a confirmation or a refresh), a CLOSED row (re-opened on the pane its
///   claude now runs in), and a row on a pane that is gone (the dead-to-live
///   heal).
pub(crate) fn refusal(
    existing: Option<&TerminalSessionRecord>,
    target_terminal: &str,
    terminal_is_live: &dyn Fn(&str) -> bool,
) -> Option<InferredBind> {
    let target = target_terminal.trim();
    if target.is_empty() || !terminal_is_live(target) {
        return Some(InferredBind::TargetNotLive {
            terminal_id: target.to_string(),
        });
    }
    let row = existing?;
    let current = row.terminal_id.trim();
    if row.state == "open" && !current.is_empty() && current != target && terminal_is_live(current)
    {
        return Some(InferredBind::RowOnLivePane {
            terminal_id: current.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::InferredBind;
    use crate::session::session_lifecycle_store::{
        SessionLifecycleStore, TerminalSessionRecord, DEFAULT_PROVIDER, ORIGIN_AUTHORITATIVE,
    };

    /// An open row for `id` on `terminal_id`, in zone `zone_index`.
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

    /// A liveness oracle over a fixed set of panes.
    fn live(panes: &[&str]) -> impl Fn(&str) -> bool {
        let panes: HashSet<String> = panes.iter().map(|p| p.to_string()).collect();
        move |t: &str| panes.contains(t)
    }

    /// Reading 2 (finding `f0a0c310`): the migration moved the session's row to
    /// its new pane, the source pane exited, and a bind from evidence read
    /// before that (the source pane's `claude --resume` command line) arrives.
    /// It is refused, writes nothing, and the row stays on its live pane.
    #[test]
    fn a_stale_bind_onto_a_pane_no_longer_live_is_refused_and_the_row_stays_on_its_live_pane() {
        let (_dir, store) = store();
        store.record_open(row("migrated", "new-pane", 2, true));
        let before = store.get("migrated").unwrap();

        let outcome = store.bind_inferred(
            row("migrated", "source-pane", -1, false),
            &live(&["new-pane"]),
        );

        assert_eq!(
            outcome,
            InferredBind::TargetNotLive {
                terminal_id: "source-pane".to_string()
            }
        );
        let after = store.get("migrated").unwrap();
        assert_eq!(
            after.terminal_id, "new-pane",
            "the row stays on its live pane"
        );
        assert_eq!(after.zone_index, 2);
        assert_eq!(after.state, "open");
        assert_eq!(
            after.last_seen_at, before.last_seen_at,
            "a refused bind writes nothing, not even a liveness stamp"
        );
    }

    /// The target-liveness arm on its own: the row's pane is gone too, so only
    /// the target's death refuses the bind.
    #[test]
    fn a_bind_onto_a_pane_not_live_is_refused_even_when_the_rows_own_pane_is_gone() {
        let (_dir, store) = store();
        store.record_open(row("s", "gone-pane", 1, true));

        let outcome = store.bind_inferred(row("s", "also-gone", -1, false), &live(&["elsewhere"]));

        assert_eq!(
            outcome,
            InferredBind::TargetNotLive {
                terminal_id: "also-gone".to_string()
            }
        );
        assert_eq!(store.get("s").unwrap().terminal_id, "gone-pane");
    }

    /// A blank target names no pane, so it is never live.
    #[test]
    fn a_bind_naming_no_pane_is_refused() {
        let (_dir, store) = store();
        let outcome = store.bind_inferred(row("s", "  ", -1, false), &|_| true);
        assert_eq!(
            outcome,
            InferredBind::TargetNotLive {
                terminal_id: String::new()
            }
        );
        assert!(store.get("s").is_none(), "nothing written");
    }

    /// Two live panes run one session (a hand-typed `claude --resume` beside the
    /// running one; 22 of 80 sessions had several live processes when this was
    /// measured). An inference must not move the open row off the pane it is
    /// on, or every pass would bind it to whichever pane is unbound and flip it
    /// back the next. Refusing also evicts no sibling on the target pane.
    #[test]
    fn a_bind_that_would_move_an_open_row_off_a_live_pane_is_refused() {
        let (_dir, store) = store();
        store.record_open(row("s", "pane-a", 0, true));
        // The target pane's own provisional seam row.
        store.record_open(row("seam", "pane-b", 3, false));

        let outcome =
            store.bind_inferred(row("s", "pane-b", 3, true), &live(&["pane-a", "pane-b"]));

        assert_eq!(
            outcome,
            InferredBind::RowOnLivePane {
                terminal_id: "pane-a".to_string()
            }
        );
        assert_eq!(store.get("s").unwrap().terminal_id, "pane-a");
        assert_eq!(
            store.get("seam").unwrap().state,
            "open",
            "a refused bind supersedes no sibling on the target pane"
        );
    }

    /// The dead-to-live heal: the row is on a pane that is gone, and the live
    /// pane holding its id binds it. The merge is `record_open`'s, so the
    /// incoming confirmation lands too.
    #[test]
    fn the_dead_to_live_heal_still_binds() {
        let (_dir, store) = store();
        store.record_open(row("s", "gone-pane", 4, false));

        let outcome = store.bind_inferred(row("s", "live-pane", 4, true), &live(&["live-pane"]));

        assert_eq!(outcome, InferredBind::Bound);
        let after = store.get("s").unwrap();
        assert_eq!(after.terminal_id, "live-pane");
        assert_eq!(after.state, "open");
        assert!(after.confirmed_at.is_some());
    }

    /// A bind onto the row's own live pane is a refresh (here, the confirmation
    /// a rung-3 bind brings), not a move.
    #[test]
    fn a_bind_onto_the_rows_own_live_pane_binds() {
        let (_dir, store) = store();
        store.record_open(row("s", "pane", 0, false));

        let outcome = store.bind_inferred(row("s", "pane", 0, true), &live(&["pane"]));

        assert_eq!(outcome, InferredBind::Bound);
        assert!(store.get("s").unwrap().confirmed_at.is_some());
    }

    /// A session with no row yet, and a CLOSED row whose old pane still lives
    /// (a bare shell left after `/exit`), both bind onto the live pane its
    /// claude runs in. Only an OPEN row is held on its pane.
    #[test]
    fn a_new_session_and_a_closed_row_bind_onto_a_live_pane() {
        let (_dir, store) = store();
        assert_eq!(
            store.bind_inferred(row("new", "pane-n", -1, true), &live(&["pane-n"])),
            InferredBind::Bound
        );
        assert_eq!(store.get("new").unwrap().terminal_id, "pane-n");

        store.record_open(row("was-closed", "old-pane", 0, true));
        store.record_close(
            "was-closed",
            crate::session::close_reason::CloseReason::PollDead,
        );
        assert_eq!(
            store.bind_inferred(
                row("was-closed", "pane-t", 0, true),
                &live(&["old-pane", "pane-t"])
            ),
            InferredBind::Bound
        );
        let reopened = store.get("was-closed").unwrap();
        assert_eq!(reopened.state, "open");
        assert_eq!(reopened.terminal_id, "pane-t");
    }
}
