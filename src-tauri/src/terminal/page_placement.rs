//! The page-capacity rule: where an AUTOMATIC placement puts a new terminal.
//!
//! One rule, used by every placement the operator did not make: the gate
//! continuation, the condition check, the looping-agent spawn, and every
//! [`TerminalManager::create`](super::TerminalManager::create) called with no
//! page (`POST /terminals` without `pageId`, the web-relay create, stewards,
//! the tauri_proxy create, the SessionRegistry PTY and claude_cli transports).
//! An explicit page — an operator spawning on the page they are looking at —
//! and an adopted holder pane (`insert_with_io`) never come through here.
//!
//! **A page's load counts what the restore is about to bring back.** Right
//! after a restart the live terminal list is nearly empty while the boot
//! restore is still re-creating panes, so a live-only count let `default` take
//! new spawns before its restore re-added 58 rows (coord finding `8c9e1419`,
//! 2026-11-05). [`page_loads`] therefore adds, per page, the rows of the
//! restorable set whose terminal is not live yet, on the page they were
//! recorded on. A restored row bound to a live terminal counts once, through
//! the live terminal. The restorable set is read under the same boot
//! classification `terminal_session_list_open` uses
//! ([`crate::session::shutdown_marker::restore_admission_inputs`]), so the
//! picker and the restore cannot disagree about which rows are coming back.

use std::collections::{HashMap, HashSet};

use super::types::TerminalInfo;
use super::TerminalManager;
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

/// The page every unpaged placement prefers while it has room.
const DEFAULT_PAGE: &str = "default";

/// Terminals a page holds before an automatic placement treats it as full.
///
/// The frontend `useZoneLayout.ts` `full-grid` layout lays out 9 zones (a 3×3
/// grid) per page; past that, the page renders a scrolling flow-grid instead,
/// so nothing is hidden. What the ceiling buys is render cost and
/// findability: the picker spreads automatic placements across pages with
/// room instead of piling them onto one.
pub(crate) const PAGE_ZONE_CEILING: usize = 9;

/// Choose the page an automatic placement lands on, from per-page `loads`.
///
/// Prefers, in order: `"default"` while under `ceiling`; then any other page
/// under it (fewest first, ties broken by the smallest page id, for
/// determinism); else a page from `mint`. A page absent from `loads` is
/// treated as load 0 — in particular an absent `"default"` is treated as
/// empty. The picker decides only among the pages PRESENT in `loads` plus the
/// implicit `"default"`.
pub(crate) fn pick_page(
    loads: &[(String, usize)],
    ceiling: usize,
    mint: impl FnOnce() -> String,
) -> String {
    let default_load = loads
        .iter()
        .find(|(p, _)| p == DEFAULT_PAGE)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    if default_load < ceiling {
        return DEFAULT_PAGE.to_string();
    }

    let best = loads
        .iter()
        .filter(|(p, c)| p != DEFAULT_PAGE && *c < ceiling)
        .min_by(|(pa, ca), (pb, cb)| ca.cmp(cb).then_with(|| pa.cmp(pb)));
    if let Some((page, _)) = best {
        return page.clone();
    }

    mint()
}

/// A recorded page id as the webview reads it: blank means `"default"`
/// (`useTerminalPages.ts`, `t.pageId || t.page_id || "default"`).
fn normalized_page(page_id: &str) -> &str {
    if page_id.is_empty() {
        DEFAULT_PAGE
    } else {
        page_id
    }
}

/// Pure core of [`page_loads`]: live terminals per page, plus each restorable
/// row whose `terminal_id` is blank or names no live terminal, on its recorded
/// page. Sorted by page id so the output is deterministic.
fn loads_from(live: &[TerminalInfo], restorable: &[TerminalSessionRecord]) -> Vec<(String, usize)> {
    let mut per_page: HashMap<String, usize> = HashMap::new();
    let live_ids: HashSet<&str> = live.iter().map(|t| t.id.as_str()).collect();
    for info in live {
        *per_page
            .entry(normalized_page(&info.page_id).to_string())
            .or_insert(0) += 1;
    }
    for row in restorable {
        if row.terminal_id.is_empty() || !live_ids.contains(row.terminal_id.as_str()) {
            *per_page
                .entry(normalized_page(&row.page_id).to_string())
                .or_insert(0) += 1;
        }
    }
    let mut loads: Vec<(String, usize)> = per_page.into_iter().collect();
    loads.sort();
    loads
}

/// Per-page load: live terminals, plus the restorable rows not yet bound to a
/// live terminal (see the module doc).
///
/// `store` is `None` only where the lifecycle store is not managed (it always
/// is in a running runner); the loads are then the live terminals alone.
pub(crate) fn page_loads(
    tm: &TerminalManager,
    store: Option<&SessionLifecycleStore>,
) -> Vec<(String, usize)> {
    let restorable = store
        .map(|s| {
            let (prior_marker_at, boot_was_clean) =
                crate::session::shutdown_marker::restore_admission_inputs();
            s.restorable_records(prior_marker_at, boot_was_clean)
        })
        .unwrap_or_default();
    loads_from(&tm.list(), &restorable)
}

/// The page a new automatically placed session lands on: [`pick_page`] over
/// [`page_loads`] at [`PAGE_ZONE_CEILING`], minting a fresh uuid page when
/// every page is full.
pub(crate) fn pick_page_for_new_session(
    tm: &TerminalManager,
    store: Option<&SessionLifecycleStore>,
) -> String {
    pick_page(&page_loads(tm, store), PAGE_ZONE_CEILING, || {
        uuid::Uuid::new_v4().to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const MINTED: &str = "MINTED";

    fn page(id: &str, n: usize) -> (String, usize) {
        (id.to_string(), n)
    }

    // ---- pick_page (moved from agent_runtime.rs) ----

    #[test]
    fn pick_page_default_under_ceiling_picks_default() {
        let loads = [page("default", 3), page("p1", 9)];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "default");
    }

    #[test]
    fn pick_page_default_full_picks_nonfull_other() {
        let loads = [page("default", 9), page("p1", 4)];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "p1");
        let loads = [page("default", 12), page("p1", 4)];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "p1");
    }

    #[test]
    fn pick_page_default_full_picks_fewest_terminals() {
        let loads = [
            page("default", 9),
            page("p1", 7),
            page("p2", 2),
            page("p3", 5),
        ];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "p2");
    }

    #[test]
    fn pick_page_tie_breaks_lexicographically() {
        let loads = [page("default", 9), page("p_b", 3), page("p_a", 3)];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "p_a");
    }

    #[test]
    fn pick_page_everything_full_mints() {
        let loads = [page("default", 9), page("p1", 9), page("p2", 11)];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), MINTED);
    }

    #[test]
    fn pick_page_empty_loads_picks_default() {
        let loads: [(String, usize); 0] = [];
        assert_eq!(pick_page(&loads, 9, || MINTED.to_string()), "default");
    }

    // ---- record-aware loads ----

    /// A restorable row, as `record_open` would hold it.
    fn row(session: &str, page_id: &str, terminal_id: &str) -> TerminalSessionRecord {
        serde_json::from_value(serde_json::json!({
            "claudeSessionId": session,
            "configDir": null,
            "workingDir": "/repo",
            "pageId": page_id,
            "zoneIndex": 0,
            "title": null,
            "terminalId": terminal_id,
            "openedAt": 0,
            "lastSeenAt": 0,
            "state": "open",
            "closedAt": null,
            "closeReason": null,
        }))
        .expect("a minimal open record deserializes")
    }

    fn rows_on(page_id: &str, n: usize) -> Vec<TerminalSessionRecord> {
        (0..n)
            .map(|i| row(&format!("{page_id}-s{i}"), page_id, ""))
            .collect()
    }

    /// A live terminal on `page_id` held under `id` in a real manager.
    fn live(tm: &TerminalManager, id: &str, page_id: &str) {
        let session = Arc::new(crate::terminal::session::tests::make_test_session_with_id(
            id,
        ));
        session.set_page(page_id.to_string());
        tm.insert_for_test(id, session);
    }

    /// A store holding `rows` that the restorable read admits under the same
    /// inputs `page_loads` passes it.
    fn store_with(rows: Vec<TerminalSessionRecord>) -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        for r in rows {
            store.record_open(r);
        }
        let (prior, clean) = crate::session::shutdown_marker::restore_admission_inputs();
        let admitted = store.restorable_records(prior, clean).len();
        assert!(admitted > 0, "fixture rows must be restorable");
        (dir, store)
    }

    /// The incident: 58 rows awaiting restore on `default`, 0 live. A live-only
    /// count reads `default` as empty and hands it the new session; the
    /// record-aware count reads it as full.
    #[test]
    fn rows_awaiting_restore_fill_their_page_for_a_new_session() {
        let tm = TerminalManager::new();
        let (_dir, store) = store_with(rows_on("default", 58));
        assert_eq!(
            page_loads(&tm, Some(&store)),
            vec![page("default", 58)],
            "58 restorable rows count on default with nothing live"
        );
        let picked = pick_page_for_new_session(&tm, Some(&store));
        assert_ne!(
            picked, "default",
            "default is full of rows awaiting restore"
        );
        assert_eq!(
            picked.len(),
            36,
            "no other page exists, so a uuid page is minted"
        );
    }

    /// The incident case with a page that has room: the new session goes
    /// there, not to the `default` its restore is about to refill.
    #[test]
    fn rows_awaiting_restore_send_a_new_session_to_the_page_with_room() {
        let tm = TerminalManager::new();
        live(&tm, "t-p1", "p1");
        let (_dir, store) = store_with(rows_on("default", 58));
        assert_eq!(
            page_loads(&tm, Some(&store)),
            vec![page("default", 58), page("p1", 1)]
        );
        assert_eq!(pick_page_for_new_session(&tm, Some(&store)), "p1");
    }

    #[test]
    fn a_restored_row_bound_to_a_live_terminal_counts_once() {
        let tm = TerminalManager::new();
        live(&tm, "term-restored", "p1");
        let (_dir, store) = store_with(vec![row("s-restored", "p1", "term-restored")]);
        assert_eq!(page_loads(&tm, Some(&store)), vec![page("p1", 1)]);
    }

    /// A row naming a terminal that is not live (the pane died with the
    /// runner) still awaits restore, and counts on its recorded page.
    #[test]
    fn a_row_whose_terminal_is_gone_counts_on_its_recorded_page() {
        let tm = TerminalManager::new();
        live(&tm, "term-other", "p2");
        let (_dir, store) = store_with(vec![row("s-gone", "p1", "term-dead")]);
        assert_eq!(
            page_loads(&tm, Some(&store)),
            vec![page("p1", 1), page("p2", 1)]
        );
    }

    #[test]
    fn a_page_with_nine_restorable_rows_is_full() {
        let mut rows = rows_on("default", 9);
        rows.extend(rows_on("p1", 9));
        rows.extend(rows_on("p2", 8));
        let tm = TerminalManager::new();
        let (_dir, store) = store_with(rows);
        assert_eq!(
            page_loads(&tm, Some(&store)),
            vec![page("default", 9), page("p1", 9), page("p2", 8)]
        );
        assert_eq!(pick_page_for_new_session(&tm, Some(&store)), "p2");
    }

    #[test]
    fn every_page_full_mints_a_new_page() {
        let tm = TerminalManager::new();
        for i in 0..9 {
            live(&tm, &format!("t-p1-{i}"), "p1");
        }
        let mut rows = rows_on("default", 5);
        rows.extend((0..4).map(|i| row(&format!("d-extra-{i}"), "", "")));
        let (_dir, store) = store_with(rows);
        assert_eq!(
            page_loads(&tm, Some(&store)),
            vec![page("default", 9), page("p1", 9)],
            "a blank recorded page counts as default"
        );
        let picked = pick_page_for_new_session(&tm, Some(&store));
        assert!(
            picked != "default" && picked != "p1" && picked.len() == 36,
            "every page full → a fresh uuid page, got {picked}"
        );
    }

    /// `TerminalManager::create`'s page step, on a real manager: an unpaged
    /// create (`POST /terminals` without `pageId`, a steward, a transport)
    /// lands where the picker says, not on the `default` its restore refills.
    #[test]
    fn an_unpaged_create_lands_where_the_picker_says() {
        let tm = TerminalManager::new();
        live(&tm, "t-p1", "p1");
        let (_dir, store) = store_with(rows_on("default", 58));
        assert_eq!(tm.resolve_page_id(None, Some(&store)), "p1");
    }

    /// An explicit page is the caller's (an operator spawning on the page they
    /// are looking at), even onto a full one.
    #[test]
    fn an_explicit_page_is_honoured() {
        let tm = TerminalManager::new();
        let (_dir, store) = store_with(rows_on("default", 58));
        assert_eq!(
            tm.resolve_page_id(Some("default".to_string()), Some(&store)),
            "default"
        );
        assert_eq!(
            tm.resolve_page_id(Some("p-op".to_string()), Some(&store)),
            "p-op"
        );
    }

    #[test]
    fn no_store_counts_live_terminals_only() {
        let tm = TerminalManager::new();
        live(&tm, "t1", "default");
        assert_eq!(page_loads(&tm, None), vec![page("default", 1)]);
        assert_eq!(pick_page_for_new_session(&tm, None), "default");
    }
}
