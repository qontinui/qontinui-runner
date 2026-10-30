//! Which runner pane holds each live `claude` process — the ONE pid → pane
//! attribution.
//!
//! Plan `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
//! Phase 2 (D1 option C). The click-resume gate reads liveness from Claude
//! Code's own registry (`terminal_claude_session_list_live`), and to offer
//! "go to the existing pane" each live row must name the runner pane whose
//! process tree contains the row's pid. That inversion — pane roots → claude
//! pids, read backwards — is computed HERE and nowhere else:
//!
//! - [`pane_claude_pids`] is the walk: every `claude` in the inclusive
//!   subtrees of a pane's roots, over one process snapshot. The tracking-health
//!   census (`session::tracking_health::evaluate` / `compute`) shares this one
//!   walk for its holder panes. Sharing the walk is not sharing the answer:
//!   the two feed it different ROOTS, so a pid the census counts in a holder
//!   pane can be unattributed here. What differs:
//!   - the census walks every holder-census pane (`[holder_pid, child_pid]`,
//!     unguarded), including a DEAD holder's surviving child and a pane the
//!     terminal manager has no tab for, because it counts every live `claude`;
//!   - this attribution walks only panes the terminal manager lists as ALIVE
//!     ([`pane_roots`]), and admits a child pid — the manager's or the
//!     census's — only through the PID-reuse guard ([`ReuseGuard`]);
//!   - a pid under two panes is counted by the census in both, and attributed
//!     here to none.
//! - [`root_is_recycled`] is the PID-reuse guard
//!   (`process_tree::claude_present_in_inclusive_subtree`'s first half, keyed
//!   by `tracking_health::pid_reuse_reference`), usable on its own so a caller
//!   that already walked a subtree does not walk it twice.
//! - [`attribute_holders`] is the attribution: pure over its inputs.
//!
//! **Every pane is rooted at every one of `{child_pid, holder_pid}`.** A pane
//! backed by a PTY holder (plan 2026-09-12) routinely has a `claude` that is no
//! descendant of the runner — every pane a restarted runner adopted, and every
//! holder spawned through WMI — so walking only the runner's subtree would
//! leave it unattributed.
//!
//! **Fail toward "unknown", never toward a wrong pane.** A pid under no pane
//! maps to `None` (a process outside the runner — or one this pass could not
//! place). A pid under two panes also maps to `None`, with a `warn!`: two panes
//! claiming one process is an inconsistency, and naming either would send the
//! operator to a pane that may not be the writer. `None` NEVER means "not
//! live" — the row is live by the registry's own pid check; only its pane is
//! unknown.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use tracing::warn;

use crate::process_capture::process_tree::{
    claude_pids_in_inclusive_subtree, ProcessSnapshot, PID_REUSE_SKEW_MS,
};
use crate::session::claude_session_registry::LiveClaudeSession;
use crate::session::tracking_health::{pid_reuse_reference, HolderCensus};
use crate::terminal::types::TerminalInfo;

/// One runner pane's process roots, as [`attribute_holders`] walks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRoots {
    /// Stable runner terminal id.
    pub terminal_id: String,
    /// Grid page the pane is on (from [`TerminalInfo::page_id`]). `None` when
    /// the page is not known — the attribution still names the pane.
    pub page_id: Option<String>,
    /// The pane's PTY child pid as the terminal manager reports it. Subject to
    /// the PID-reuse guard ([`ReuseGuard`]): a pid that predates this runner's
    /// boot and is not PROVEN to be a holder's recorded child is a recycled
    /// pid, and is not walked.
    pub child_pid: Option<u32>,
    /// The pane's PTY holder pid (holder-backed panes only), from the holder
    /// census — identity already established there (an answered handshake, or
    /// a lock record whose start token matches), so it is not re-guarded.
    pub holder_pid: Option<u32>,
    /// The child pid the holder census names, when it differs from
    /// [`Self::child_pid`]. Guarded like `child_pid` ([`ReuseGuard`]): a DEAD
    /// holder's census keeps its recorded child while that child "cannot be
    /// proven not to be the same process", so the pid may since have been
    /// recycled. Only a pid in [`ReuseGuard::verified`] skips the boot
    /// comparison.
    pub census_child_pid: Option<u32>,
}

/// The pane that holds one live row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingPane {
    pub terminal_id: String,
    pub page_id: Option<String>,
}

/// The PID-reuse guard's inputs: the same pair `tracking_health` keys its
/// guard on, so this attribution and the census judge a pane root alike.
#[derive(Debug, Clone, Copy)]
pub struct ReuseGuard<'a> {
    /// Terminal pids PROVEN to be a holder's recorded child
    /// ([`HolderCensus::verified_terminal_pids`]): exempt from the boot
    /// comparison, because an adopted pane legitimately predates this boot.
    pub verified: &'a HashSet<u32>,
    /// This runner primary's boot instant
    /// (`tracking_health::primary_boot_unix_millis`). `None` = not known yet:
    /// then no unverified child pid can be told from a recycled one, and none
    /// is walked — its rows read as "pane unknown", never as a wrong pane.
    pub boot_unix_millis: Option<i64>,
}

impl ReuseGuard<'_> {
    /// True iff `child_pid` may be walked as a pane root.
    fn admits(&self, child_pid: u32, snapshot: &ProcessSnapshot) -> bool {
        match self.boot_unix_millis {
            Some(boot) => !root_is_recycled(
                child_pid,
                snapshot,
                pid_reuse_reference(child_pid, self.verified, boot),
            ),
            None => self.verified.contains(&child_pid),
        }
    }
}

/// The PID-reuse guard on a tracked root: true iff the snapshot knows the
/// root's creation time and it predates `reference_unix_millis` beyond
/// [`PID_REUSE_SKEW_MS`] — i.e. the pid now names a process that cannot be
/// the one the runner tracked. An unknown (absent or `<= 0`) creation time is
/// never "recycled". Identical to the guard
/// `process_tree::claude_present_in_inclusive_subtree` applies.
pub fn root_is_recycled(
    root_pid: u32,
    snapshot: &ProcessSnapshot,
    reference_unix_millis: i64,
) -> bool {
    match snapshot.creation_times.get(&root_pid) {
        Some(&created_secs) => {
            created_secs > 0 && created_secs * 1000 + PID_REUSE_SKEW_MS < reference_unix_millis
        }
        None => false,
    }
}

/// Every `claude` pid in the inclusive subtrees of `roots`, deduplicated, in
/// first-seen order. A root of `0`, `1` or `runner_pid` itself is never walked:
/// it would claim the whole box, or the runner's own tree.
pub fn pane_claude_pids(
    roots: impl IntoIterator<Item = u32>,
    runner_pid: u32,
    snapshot: &ProcessSnapshot,
) -> Vec<u32> {
    let mut pids: Vec<u32> = Vec::new();
    for root in roots {
        if root <= 1 || root == runner_pid {
            continue;
        }
        for pid in claude_pids_in_inclusive_subtree(root, snapshot) {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    pids
}

/// The roots one pane is walked from, after the reuse guard. Holder-census
/// roots first, matching the census's own `[holder_pid, child_pid]` order.
/// The holder pid is trusted (its identity is the census's proof); every child
/// pid — the census's and the manager's — must pass the guard.
fn admitted_roots(pane: &PaneRoots, snapshot: &ProcessSnapshot, guard: &ReuseGuard) -> Vec<u32> {
    let mut roots: Vec<u32> = Vec::new();
    if let Some(holder) = pane.holder_pid {
        roots.push(holder);
    }
    for child in [pane.census_child_pid, pane.child_pid]
        .into_iter()
        .flatten()
    {
        if !roots.contains(&child) && guard.admits(child, snapshot) {
            roots.push(child);
        }
    }
    roots
}

/// Attribute each live row's pid to the runner pane that holds it.
///
/// Returns one `(row_index, holder)` per entry of `row_pids`, in order.
/// `holder` is `Some` only when exactly ONE pane's tree contains the pid; it
/// is `None` for a pid under no pane (outside the runner) and for a pid under
/// two or more panes (logged with `warn!`). Pure: no I/O beyond the log line.
pub fn attribute_holders(
    row_pids: &[u32],
    panes: &[PaneRoots],
    snapshot: &ProcessSnapshot,
    runner_pid: u32,
    guard: &ReuseGuard,
) -> Vec<(usize, Option<HoldingPane>)> {
    // pid -> the indices of every pane whose tree contains it.
    let mut owners: HashMap<u32, Vec<usize>> = HashMap::new();
    for (pane_idx, pane) in panes.iter().enumerate() {
        let roots = admitted_roots(pane, snapshot, guard);
        for pid in pane_claude_pids(roots, runner_pid, snapshot) {
            let list = owners.entry(pid).or_default();
            if !list.contains(&pane_idx) {
                list.push(pane_idx);
            }
        }
    }

    row_pids
        .iter()
        .enumerate()
        .map(|(row_idx, pid)| {
            let holder = match owners.get(pid).map(Vec::as_slice) {
                None | Some([]) => None,
                Some([only]) => Some(HoldingPane {
                    terminal_id: panes[*only].terminal_id.clone(),
                    page_id: panes[*only].page_id.clone(),
                }),
                Some(many) => {
                    let ids: Vec<&str> = many
                        .iter()
                        .map(|&i| panes[i].terminal_id.as_str())
                        .collect();
                    warn!(
                        pid = *pid,
                        panes = ?ids,
                        "live_holders: one live claude pid lies under {} runner panes — \
                         attributing it to none (pane unknown, still live)",
                        many.len()
                    );
                    None
                }
            };
            (row_idx, holder)
        })
        .collect()
}

/// Join the terminal manager's panes with the holder census into the roots
/// [`attribute_holders`] walks.
///
/// Only panes the terminal manager lists AND reports alive are returned:
/// - a pane the runner has no tab for cannot be "gone to";
/// - an EXITED pane holds no live conversation, and its `pid` still names the
///   dead child. That pid may since have been reused by an unrelated process
///   started after this runner booted, which the boot comparison in
///   [`ReuseGuard`] cannot tell from the pane's own child. Walking it would
///   name the exited tab as the holder of whatever `claude` now runs under the
///   recycled pid.
///
/// A census row for a returned pane contributes its holder as a trusted root
/// and, when it differs from the manager's, its child as a guarded one.
pub fn pane_roots(panes: &[TerminalInfo], census: &HolderCensus) -> Vec<PaneRoots> {
    panes
        .iter()
        .filter(|info| info.is_alive)
        .map(|info| {
            let row = census.panes.iter().find(|p| p.pane_id == info.id);
            let child_pid = info.pid.filter(|&p| p > 0);
            let census_child_pid = row
                .and_then(|r| r.child_pid)
                .filter(|&c| Some(c) != child_pid);
            PaneRoots {
                terminal_id: info.id.clone(),
                page_id: Some(info.page_id.clone()).filter(|p| !p.is_empty()),
                child_pid,
                holder_pid: row.and_then(|r| r.holder_pid),
                census_child_pid,
            }
        })
        .collect()
}

/// One `terminal_claude_session_list_live` row: the registry row verbatim,
/// plus the pane that holds it. `terminalId` / `pageId` are OMITTED when the
/// pane is unknown — absence means "held outside a runner pane, or pane
/// unknown", never "not live".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionRow {
    #[serde(flatten)]
    pub session: LiveClaudeSession,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
}

/// Attach each session's holding pane, preserving the input order.
pub fn with_holding_panes(
    sessions: Vec<LiveClaudeSession>,
    panes: &[PaneRoots],
    snapshot: &ProcessSnapshot,
    runner_pid: u32,
    guard: &ReuseGuard,
) -> Vec<LiveSessionRow> {
    let pids: Vec<u32> = sessions.iter().map(|s| s.pid).collect();
    let attributed = attribute_holders(&pids, panes, snapshot, runner_pid, guard);
    sessions
        .into_iter()
        .zip(attributed)
        .map(|(session, (_, holder))| {
            let (terminal_id, page_id) = match holder {
                Some(h) => (Some(h.terminal_id), h.page_id),
                None => (None, None),
            };
            LiveSessionRow {
                session,
                terminal_id,
                page_id,
            }
        })
        .collect()
}

/// The rows WITHOUT pane attribution: what `terminal_claude_session_list_live`
/// answers when the caller did not ask for holders (`withHolders` false). The
/// pane keys are omitted, which reads as "pane unknown" — never "not live".
pub fn without_holding_panes(sessions: Vec<LiveClaudeSession>) -> Vec<LiveSessionRow> {
    sessions
        .into_iter()
        .map(|session| LiveSessionRow {
            session,
            terminal_id: None,
            page_id: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::past_sessions::PastSessionAccount;
    use crate::session::tracking_health::{HolderPane, HolderPaneState};

    const RUNNER: u32 = 1000;
    /// A boot instant well after every "old" process below and well before
    /// every "new" one.
    const BOOT_MS: i64 = 2_000_000_000_000;
    const NEW_S: i64 = 2_000_000_100; // after boot
    const OLD_S: i64 = 1_900_000_000; // long before boot

    fn snap(parent_map: &[(u32, &[u32])], procs: &[(u32, &str, i64)]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        for (p, kids) in parent_map {
            s.parent_map.insert(*p, kids.to_vec());
        }
        for (pid, name, created) in procs {
            s.names.insert(*pid, name.to_string());
            s.creation_times.insert(*pid, *created);
        }
        s
    }

    fn pane(id: &str, page: &str, child: Option<u32>, holder: Option<u32>) -> PaneRoots {
        PaneRoots {
            terminal_id: id.to_string(),
            page_id: Some(page.to_string()),
            child_pid: child,
            holder_pid: holder,
            census_child_pid: None,
        }
    }

    fn held(id: &str, page: &str) -> Option<HoldingPane> {
        Some(HoldingPane {
            terminal_id: id.to_string(),
            page_id: Some(page.to_string()),
        })
    }

    fn guard(verified: &HashSet<u32>) -> ReuseGuard<'_> {
        ReuseGuard {
            verified,
            boot_unix_millis: Some(BOOT_MS),
        }
    }

    /// The runner (1000) hosts two shell panes: 10 → claude 11, and
    /// 20 → claude 21 → nested claude 22. Claude 30 runs outside the runner.
    fn runner_box() -> ProcessSnapshot {
        snap(
            &[(RUNNER, &[10, 20]), (10, &[11]), (20, &[21]), (21, &[22])],
            &[
                (RUNNER, "qontinui-runner.exe", NEW_S - 50),
                (10, "pwsh.exe", NEW_S),
                (11, "claude.exe", NEW_S),
                (20, "bash", NEW_S),
                (21, "claude", NEW_S),
                (22, "claude", NEW_S),
                (30, "claude.exe", NEW_S),
            ],
        )
    }

    fn two_panes() -> Vec<PaneRoots> {
        vec![
            pane("t-a", "page-1", Some(10), None),
            pane("t-b", "page-2", Some(20), None),
        ]
    }

    #[test]
    fn zero_rows_attribute_nothing() {
        let verified = HashSet::new();
        let out = attribute_holders(&[], &two_panes(), &runner_box(), RUNNER, &guard(&verified));
        assert!(out.is_empty());
    }

    #[test]
    fn one_row_in_a_pane_names_that_pane_and_its_page() {
        let verified = HashSet::new();
        let out = attribute_holders(
            &[11],
            &two_panes(),
            &runner_box(),
            RUNNER,
            &guard(&verified),
        );
        assert_eq!(out, vec![(0, held("t-a", "page-1"))]);
    }

    #[test]
    fn many_rows_keep_their_order_and_each_gets_its_own_pane() {
        let verified = HashSet::new();
        let out = attribute_holders(
            &[22, 30, 11, 21],
            &two_panes(),
            &runner_box(),
            RUNNER,
            &guard(&verified),
        );
        assert_eq!(
            out,
            vec![
                (0, held("t-b", "page-2")), // nested claude: still pane b
                (1, None),                  // outside the runner
                (2, held("t-a", "page-1")),
                (3, held("t-b", "page-2")),
            ]
        );
    }

    #[test]
    fn a_pid_under_no_pane_is_none() {
        let verified = HashSet::new();
        let out = attribute_holders(
            &[30],
            &two_panes(),
            &runner_box(),
            RUNNER,
            &guard(&verified),
        );
        assert_eq!(out, vec![(0, None)]);
        // No panes at all: every row is unattributed, none dropped.
        let out = attribute_holders(&[11, 30], &[], &runner_box(), RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None), (1, None)]);
    }

    #[test]
    fn a_pid_under_two_panes_is_none() {
        // Pane c is rooted at shell 20's claude 21, so claude 22 lies under
        // both pane b (root 20) and pane c (root 21).
        let mut panes = two_panes();
        panes.push(pane("t-c", "page-3", Some(21), None));
        let verified = HashSet::new();
        let out = attribute_holders(&[22, 11], &panes, &runner_box(), RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None), (1, held("t-a", "page-1"))]);
    }

    #[test]
    fn two_roots_of_one_pane_reaching_the_same_pid_are_not_two_panes() {
        // A holder pane whose holder (40) parents its child (41) which runs
        // claude 42: both roots reach 42, and that is ONE pane.
        let s = snap(
            &[(40, &[41]), (41, &[42])],
            &[
                (40, "qontinui-pty-holder", OLD_S),
                (41, "pwsh", OLD_S),
                (42, "claude", OLD_S),
            ],
        );
        let verified: HashSet<u32> = [41].into_iter().collect();
        let panes = vec![pane("t-h", "page-h", Some(41), Some(40))];
        let out = attribute_holders(&[42], &panes, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, held("t-h", "page-h"))]);
    }

    #[test]
    fn a_holder_backed_pane_whose_claude_is_not_a_runner_descendant_is_attributed() {
        // Adopted after a restart: holder 50 descends from nothing the runner
        // owns, and its child 51 predates this boot. The manager reports no
        // usable child pid; only the holder root reaches claude 52.
        let s = snap(
            &[(RUNNER, &[]), (50, &[51]), (51, &[52])],
            &[
                (RUNNER, "qontinui-runner.exe", NEW_S - 50),
                (50, "qontinui-pty-holder.exe", OLD_S),
                (51, "pwsh.exe", OLD_S),
                (52, "claude.exe", OLD_S),
            ],
        );
        let verified = HashSet::new();
        let panes = vec![pane("t-held", "page-h", None, Some(50))];
        let out = attribute_holders(&[52], &panes, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, held("t-held", "page-h"))]);

        // Without the holder root, the same pane cannot reach it: the boot
        // guard refuses the unverified pre-boot child.
        let child_only = vec![pane("t-held", "page-h", Some(51), None)];
        let out = attribute_holders(&[52], &child_only, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None)]);

        // ...and a child PROVEN to be the holder's recorded child is walked.
        let proven: HashSet<u32> = [51].into_iter().collect();
        let out = attribute_holders(&[52], &child_only, &s, RUNNER, &guard(&proven));
        assert_eq!(out, vec![(0, held("t-held", "page-h"))]);
    }

    #[test]
    fn a_reused_pid_is_not_a_pane_root() {
        // The manager still reports pane t-old's child as pid 60, but pid 60
        // is now an unrelated process started long before this boot, whose
        // tree happens to hold claude 61.
        let s = snap(
            &[(60, &[61])],
            &[(60, "explorer.exe", OLD_S), (61, "claude.exe", NEW_S)],
        );
        let verified = HashSet::new();
        let panes = vec![pane("t-old", "page-1", Some(60), None)];
        let out = attribute_holders(&[61], &panes, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None)], "a recycled pid names no pane");

        // Boot unknown: no unverified child can be told from a recycled one.
        let fresh = snap(
            &[(70, &[71])],
            &[(70, "pwsh", NEW_S), (71, "claude", NEW_S)],
        );
        let unknown_boot = ReuseGuard {
            verified: &verified,
            boot_unix_millis: None,
        };
        let panes = vec![pane("t-new", "page-1", Some(70), None)];
        let out = attribute_holders(&[71], &panes, &fresh, RUNNER, &unknown_boot);
        assert_eq!(out, vec![(0, None)]);
        // With the boot known, the same fresh pane is attributed.
        let out = attribute_holders(&[71], &panes, &fresh, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, held("t-new", "page-1"))]);
    }

    #[test]
    fn the_runner_and_init_are_never_pane_roots() {
        // A pane whose recorded root is the runner itself (or pid 1) would
        // claim every claude on the box; it is skipped.
        let verified = HashSet::new();
        let panes = vec![
            pane("t-runner", "page-1", Some(RUNNER), None),
            pane("t-init", "page-1", Some(1), Some(0)),
        ];
        let out = attribute_holders(&[11, 21], &panes, &runner_box(), RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None), (1, None)]);
    }

    #[test]
    fn root_is_recycled_matches_the_presence_guard() {
        let s = snap(&[], &[(5, "claude", 100), (6, "claude", 0)]);
        // Created at 100 s; reference within the skew: not recycled.
        assert!(!root_is_recycled(5, &s, 100_000 + PID_REUSE_SKEW_MS));
        // Reference past the skew: recycled.
        assert!(root_is_recycled(5, &s, 100_000 + PID_REUSE_SKEW_MS + 1));
        // Unknown creation time (0 / absent): never recycled.
        assert!(!root_is_recycled(6, &s, i64::MAX));
        assert!(!root_is_recycled(7, &s, i64::MAX));
        // Agrees with process_tree's own guard on every case.
        for (pid, reference) in [
            (5, 100_000 + PID_REUSE_SKEW_MS),
            (5, i64::MAX),
            (6, i64::MAX),
        ] {
            assert_eq!(
                !root_is_recycled(pid, &s, reference),
                crate::process_capture::process_tree::claude_present_in_inclusive_subtree(
                    pid, &s, reference
                ),
            );
        }
    }

    #[test]
    fn pane_claude_pids_dedups_in_first_seen_order() {
        let s = runner_box();
        assert_eq!(pane_claude_pids([20, 21, 10], RUNNER, &s), vec![21, 22, 11]);
        assert!(pane_claude_pids([RUNNER, 1, 0], RUNNER, &s).is_empty());
    }

    fn holder_pane(id: &str, holder: Option<u32>, child: Option<u32>) -> HolderPane {
        HolderPane {
            pane_id: id.to_string(),
            state: HolderPaneState::Live,
            holder_pid: holder,
            child_pid: child,
            holder_build: None,
            negotiated_version: None,
            holder_versions: vec![],
            detail: None,
            claude_pids: vec![],
        }
    }

    fn info(id: &str, pid: Option<u32>, page: &str, is_alive: bool) -> TerminalInfo {
        TerminalInfo {
            id: id.to_string(),
            title: String::new(),
            pid,
            cols: 80,
            rows: 24,
            working_dir: "D:/work".to_string(),
            is_alive,
            exit_code: (!is_alive).then_some(0),
            created_at: 0,
            total_bytes_produced: 0,
            page_id: page.to_string(),
        }
    }

    fn alive(id: &str, pid: Option<u32>, page: &str) -> TerminalInfo {
        info(id, pid, page, true)
    }

    #[test]
    fn pane_roots_joins_the_manager_list_with_the_holder_census() {
        let census = HolderCensus {
            panes: vec![
                holder_pane("t-held", Some(50), Some(51)),
                holder_pane("t-moved", Some(80), Some(82)),
                holder_pane("t-no-tab", Some(90), Some(91)),
                holder_pane("t-exited", Some(95), Some(96)),
            ],
            ..HolderCensus::default()
        };
        let live = vec![
            alive("t-plain", Some(10), "page-1"),
            alive("t-held", Some(51), "page-2"),
            alive("t-moved", Some(81), ""),
            alive("t-nopid", Some(0), "page-3"),
            info("t-exited", Some(96), "page-4", false),
        ];
        let roots = pane_roots(&live, &census);
        assert_eq!(
            roots,
            vec![
                PaneRoots {
                    terminal_id: "t-plain".into(),
                    page_id: Some("page-1".into()),
                    child_pid: Some(10),
                    holder_pid: None,
                    census_child_pid: None,
                },
                PaneRoots {
                    terminal_id: "t-held".into(),
                    page_id: Some("page-2".into()),
                    child_pid: Some(51),
                    holder_pid: Some(50),
                    census_child_pid: None, // same as the manager's child
                },
                PaneRoots {
                    terminal_id: "t-moved".into(),
                    page_id: None, // empty page id is unknown, not ""
                    child_pid: Some(81),
                    holder_pid: Some(80),
                    census_child_pid: Some(82),
                },
                PaneRoots {
                    terminal_id: "t-nopid".into(),
                    page_id: Some("page-3".into()),
                    child_pid: None,
                    holder_pid: None,
                    census_child_pid: None,
                },
            ],
            "a census row with no manager tab (t-no-tab) is not a pane to go to, and an \
             exited pane (t-exited) is no pane at all"
        );
    }

    /// Review M1: an EXITED pane keeps its dead child's pid in
    /// `TerminalInfo.pid`. When that pid is reused by a process started AFTER
    /// this runner booted, the boot comparison admits it — so the only thing
    /// that stops the exited tab from being named holder of the claude now
    /// running under the recycled pid is skipping the dead pane.
    #[test]
    fn an_exited_pane_whose_old_pid_now_roots_an_outside_claude_maps_to_none() {
        // Pane t-dead's shell (pid 60) exited; pid 60 now names a fresh
        // terminal (started after boot, so the reuse guard admits it) whose
        // claude 61 runs outside every runner pane.
        let s = snap(
            &[(60, &[61])],
            &[
                (60, "WindowsTerminal.exe", NEW_S),
                (61, "claude.exe", NEW_S),
            ],
        );
        let verified = HashSet::new();
        let census = HolderCensus::default();

        let exited = vec![info("t-dead", Some(60), "page-1", false)];
        let panes = pane_roots(&exited, &census);
        assert!(panes.is_empty(), "an exited pane is no root: {panes:?}");
        let out = attribute_holders(&[61], &panes, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, None)], "the outside claude names no pane");

        // The same pane reported ALIVE is walked — the skip is the dead
        // pane's, not the pid's.
        let running = vec![alive("t-dead", Some(60), "page-1")];
        let panes = pane_roots(&running, &census);
        let out = attribute_holders(&[61], &panes, &s, RUNNER, &guard(&verified));
        assert_eq!(out, vec![(0, held("t-dead", "page-1"))]);
    }

    /// Review M1: a DEAD holder's census row keeps its recorded child pid
    /// while the child "cannot be proven not to be the same process". That pid
    /// is guarded like the manager's: a pre-boot process under it that is not
    /// a PROVEN holder child is a recycled pid, and is not walked.
    #[test]
    fn a_dead_holder_census_child_is_rejected_by_the_guard() {
        // Holder 70 is dead (no holder pid); its recorded child 71 is now an
        // unrelated process started long before this boot, holding claude 72.
        let s = snap(
            &[(71, &[72])],
            &[(71, "explorer.exe", OLD_S), (72, "claude.exe", NEW_S)],
        );
        let mut dead = holder_pane("t-h", None, Some(71));
        dead.state = HolderPaneState::Dead;
        let census = HolderCensus {
            panes: vec![dead],
            ..HolderCensus::default()
        };
        // The manager reports no usable child pid for the pane.
        let live = vec![alive("t-h", None, "page-h")];
        let panes = pane_roots(&live, &census);
        assert_eq!(panes[0].census_child_pid, Some(71));

        let unverified = HashSet::new();
        let out = attribute_holders(&[72], &panes, &s, RUNNER, &guard(&unverified));
        assert_eq!(
            out,
            vec![(0, None)],
            "a recycled census child names no pane"
        );

        // Boot unknown: an unverified census child is not walked either.
        let unknown_boot = ReuseGuard {
            verified: &unverified,
            boot_unix_millis: None,
        };
        let out = attribute_holders(&[72], &panes, &s, RUNNER, &unknown_boot);
        assert_eq!(out, vec![(0, None)]);

        // A census child PROVEN to be the holder's recorded child is walked.
        let proven: HashSet<u32> = [71].into_iter().collect();
        let out = attribute_holders(&[72], &panes, &s, RUNNER, &guard(&proven));
        assert_eq!(out, vec![(0, held("t-h", "page-h"))]);
    }

    fn session(id: &str, pid: u32) -> LiveClaudeSession {
        LiveClaudeSession {
            session_id: id.to_string(),
            name: format!("name-{id}"),
            name_source: None,
            pid,
            account: PastSessionAccount {
                label: "acct".into(),
                wrapper: "cla".into(),
            },
            working_dir: "D:/work".into(),
            status: "idle".into(),
            kind: "interactive".into(),
            started_at: 1,
            updated_at: 2,
            resume_command: Some(format!("cd 'D:/work' && cla --resume {id}")),
        }
    }

    #[test]
    fn rows_serialize_terminal_and_page_only_when_known() {
        let verified = HashSet::new();
        let rows = with_holding_panes(
            vec![session("in-pane", 11), session("outside", 30)],
            &two_panes(),
            &runner_box(),
            RUNNER,
            &guard(&verified),
        );
        let json = serde_json::to_value(&rows).unwrap();
        let first = json[0].as_object().unwrap();
        assert_eq!(first["sessionId"], "in-pane");
        assert_eq!(first["pid"], 11);
        assert_eq!(
            first["resumeCommand"],
            "cd 'D:/work' && cla --resume in-pane"
        );
        assert_eq!(first["terminalId"], "t-a");
        assert_eq!(first["pageId"], "page-1");
        let second = json[1].as_object().unwrap();
        assert_eq!(second["sessionId"], "outside");
        assert!(!second.contains_key("terminalId"), "omitted, not null");
        assert!(!second.contains_key("pageId"), "omitted, not null");
    }

    /// Review W10: a read without holders keeps every row and its registry
    /// fields, and omits the pane keys — pane unknown, never "not live".
    #[test]
    fn rows_without_holders_keep_every_row_and_omit_the_pane_keys() {
        let rows = without_holding_panes(vec![session("in-pane", 11), session("outside", 30)]);
        let json = serde_json::to_value(&rows).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        for (row, id) in arr.iter().zip(["in-pane", "outside"]) {
            let obj = row.as_object().unwrap();
            assert_eq!(obj["sessionId"], id);
            assert!(!obj.contains_key("terminalId"), "omitted, not null");
            assert!(!obj.contains_key("pageId"), "omitted, not null");
        }
    }

    #[test]
    fn zero_sessions_serialize_as_an_empty_list() {
        let verified = HashSet::new();
        let rows = with_holding_panes(
            vec![],
            &two_panes(),
            &runner_box(),
            RUNNER,
            &guard(&verified),
        );
        assert_eq!(serde_json::to_value(&rows).unwrap(), serde_json::json!([]));
    }
}
