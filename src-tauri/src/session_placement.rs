//! Where a runner-hosted Claude session sits — its terminal, page (tab), tab
//! label and window — carried on the session census, and the early census post
//! that keeps it current (plan
//! `2026-11-16-runner-page-name-reaches-the-coord-session-identity-card`,
//! Phase 4, D2/D7/D8).
//!
//! # Placement
//!
//! [`placements_by_session`] reuses the layout join
//! ([`crate::terminal::layout::join_terminals`] + [`arrange_pages`] over the
//! registered tab labels), never a hand join of the terminal list to the
//! lifecycle store. That join pairs a terminal only with its CONFIRMED session
//! record, so a plain shell — which carries a provisional record and never a
//! session id — gets no placement. The label and window are exactly what the
//! webview registered: an unregistered page (the internal `default` one
//! included) has no label and no window, and nothing here invents a name.
//!
//! [`apply_placements`] puts the placement on the census rows this runner
//! hosts. A row hosted by ANOTHER runner on the device (found by ancestry) has
//! no terminal here and gets none.
//!
//! coord's census ingest (qontinui-coord#4369, `CensusSessionIn`) takes the
//! four fields under the names [`Placement`] serializes with and computes
//! `page_name_withheld` itself, so the runner never sends it. Page names are
//! tab labels: the egress class runner#2750's relay audit already passes
//! ungated (plan D9) — no new egress flow.
//!
//! # The early post
//!
//! The census posts every [`crate::session::census::CENSUS_INTERVAL`]. A
//! rename, a reorder, a page added or removed, a page popped out or back
//! (`terminal_pages_register`), a terminal moved to another page
//! (`TerminalManager::set_page`), a session handle bound (`spawn_register`)
//! and a terminal closed each call [`request_census_post`], so coord's card
//! follows within seconds instead of a minute (D7).
//!
//! Requests are DEBOUNCED by [`EARLY_POST_DEBOUNCE`] from the first one:
//! every request inside that window rides the same post. They are SINGLE-FLIGHT
//! because only the census publisher loop consumes them, one post at a time; a
//! request made while a post is in flight is kept and answered by the next one.
//! The regular interval remains the heal-without-a-trigger path.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Notify;

use crate::session::census::CensusRow;
use crate::terminal::layout::{arrange_pages, JoinedTerminal, PageLabels};

/// How long an early census post waits after the first request, so a burst
/// (a rename that also reorders, a move followed by a bind) costs one post.
pub const EARLY_POST_DEBOUNCE: Duration = Duration::from_secs(2);

/// Where one runner-hosted session sits. Field names are coord's census ingest
/// contract (`CensusSessionIn` in qontinui-coord `session_census.rs`) — do not
/// rename. Unknowns serialize as explicit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Placement {
    /// The runner terminal the session runs in.
    pub terminal_id: String,
    /// The page (tab) the terminal sits on.
    pub page_id: String,
    /// The tab label the webview registered; `null` when it registered none.
    pub page_name: Option<String>,
    /// The window the page is shown in (`"main"` or a pop-out's label);
    /// `null` when the webview registered none.
    pub page_window: Option<String>,
}

/// Every confirmed session's placement, keyed by its Claude session id.
/// `joined` is [`crate::terminal::layout::join_terminals`]' output; a terminal
/// with no confirmed record (a plain shell) contributes nothing. Should one
/// session id be joined to two terminals, the first in served page order wins.
pub fn placements_by_session(
    joined: Vec<JoinedTerminal>,
    labels: &PageLabels,
) -> HashMap<String, Placement> {
    let mut groups: BTreeMap<String, Vec<JoinedTerminal>> = BTreeMap::new();
    for terminal in joined {
        groups
            .entry(terminal.info.page_id.clone())
            .or_default()
            .push(terminal);
    }
    let mut out = HashMap::new();
    for page in arrange_pages(groups, labels, false) {
        for terminal in page.items {
            let Some(record) = terminal.session else {
                continue;
            };
            let session_id = record.claude_session_id.trim();
            if session_id.is_empty() {
                continue;
            }
            out.entry(session_id.to_string()).or_insert_with(|| Placement {
                terminal_id: terminal.info.id.clone(),
                page_id: page.page_id.clone(),
                page_name: page.label.clone(),
                page_window: page.window.clone(),
            });
        }
    }
    out
}

/// Put each runner-hosted row's placement on it; every other row gets none.
pub fn apply_placements(rows: &mut [CensusRow], placements: &HashMap<String, Placement>) {
    for row in rows {
        row.placement = if row.runner_hosted {
            placements.get(&row.session_id).cloned()
        } else {
            None
        };
    }
}

// ===========================================================================
// The early post
// ===========================================================================

/// A debounced "post the census now" request. See the module docs.
#[derive(Debug)]
pub struct EarlyPost {
    requested: AtomicBool,
    notify: Notify,
    debounce: Duration,
}

impl EarlyPost {
    pub fn new(debounce: Duration) -> Self {
        Self {
            requested: AtomicBool::new(false),
            notify: Notify::new(),
            debounce,
        }
    }

    /// Ask for an early post. Never blocks; callable from any thread.
    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Resolve once a post is due: after the first request, plus the debounce.
    /// Every request made before the debounce ends is consumed with it. A
    /// request made after (while the caller posts) keeps the next `wait` due.
    pub async fn wait(&self) {
        while !self.requested.load(Ordering::SeqCst) {
            // A request between the load and this await left a permit, so
            // `notified` returns at once rather than missing it.
            self.notify.notified().await;
        }
        tokio::time::sleep(self.debounce).await;
        self.requested.store(false, Ordering::SeqCst);
    }
}

/// This process's early-post request, consumed by the census publisher.
pub fn early_post() -> &'static EarlyPost {
    static CELL: OnceLock<EarlyPost> = OnceLock::new();
    CELL.get_or_init(|| EarlyPost::new(EARLY_POST_DEBOUNCE))
}

/// Ask the census publisher to post soon (D7). A no-op in effect on a runner
/// whose publisher is not running (a secondary).
pub fn request_census_post() {
    early_post().request();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
    use crate::terminal::layout::{join_terminals, RegisteredPage};
    use crate::terminal::types::TerminalInfo;
    use chrono::{DateTime, Utc};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    const SID_WORK: &str = "11111111-1111-4111-8111-111111111111";
    const SID_POP: &str = "22222222-2222-4222-8222-222222222222";
    const SID_DEFAULT: &str = "33333333-3333-4333-8333-333333333333";
    const SID_SHELL: &str = "44444444-4444-4444-8444-444444444444";
    const SID_ELSEWHERE: &str = "55555555-5555-4555-8555-555555555555";

    fn info(id: &str, page_id: &str) -> TerminalInfo {
        TerminalInfo {
            id: id.to_string(),
            title: "t".to_string(),
            pid: Some(42),
            cols: 80,
            rows: 24,
            working_dir: "/w".to_string(),
            is_alive: true,
            exit_code: None,
            created_at: 0,
            total_bytes_produced: 0,
            page_id: page_id.to_string(),
        }
    }

    fn record(sid: &str, terminal_id: &str, confirmed: bool) -> TerminalSessionRecord {
        let mut r: TerminalSessionRecord = serde_json::from_value(serde_json::json!({
            "claudeSessionId": sid,
            "pageId": "p",
            "zoneIndex": 0,
            "terminalId": terminal_id,
            "openedAt": 1,
            "lastSeenAt": 1,
            "state": "open",
        }))
        .expect("minimal record parses");
        if confirmed {
            r.confirmed_at = Some(1_700_000_000_000);
        }
        r
    }

    fn page(id: &str, label: Option<&str>, order: u32, window: Option<&str>) -> RegisteredPage {
        RegisteredPage {
            page_id: id.to_string(),
            label: label.map(str::to_string),
            order,
            window: window.map(str::to_string),
        }
    }

    fn row(sid: &str, runner_hosted: bool) -> CensusRow {
        CensusRow {
            session_id: sid.to_string(),
            pid: 7,
            pid_alive: true,
            proc_start: None,
            started_at: None,
            account: None,
            config_dir: None,
            cwd: None,
            entrypoint: None,
            kind: None,
            tmux_pane: None,
            window_name: None,
            window_name_source: None,
            registry_status: None,
            registry_status_updated_at: None,
            last_acted_at: None,
            runner_hosted,
            runner_ancestor: None,
            placement: None,
        }
    }

    /// The layout fixture: a named page in the main window, a page popped out
    /// into a pop-out, the internal `default` page the webview never
    /// registered, and a plain shell that holds only a provisional record.
    fn fixture_rows() -> Vec<CensusRow> {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        store.record_open(record(SID_WORK, "t-work", true));
        store.record_open(record(SID_POP, "t-pop", true));
        store.record_open(record(SID_DEFAULT, "t-default", true));
        store.record_open(record(SID_SHELL, "t-shell", false));
        let labels = PageLabels::new(
            vec![
                page("p-work", Some("bauhuas"), 0, Some("main")),
                page("p-pop", Some("Popped"), 1, Some("terminal-popout-2")),
            ],
            DateTime::<Utc>::from_timestamp_millis(1_794_128_400_000).unwrap(),
        );
        let joined = join_terminals(
            vec![
                info("t-work", "p-work"),
                info("t-pop", "p-pop"),
                info("t-default", "default"),
                info("t-shell", "p-work"),
            ],
            Some(&store),
            |_| None,
        );
        let placements = placements_by_session(joined, &labels);
        let mut rows = vec![
            row(SID_WORK, true),
            row(SID_POP, true),
            row(SID_DEFAULT, true),
            row(SID_SHELL, true),
            row(SID_ELSEWHERE, true),
        ];
        apply_placements(&mut rows, &placements);
        rows
    }

    fn placement_json(rows: &[CensusRow], sid: &str) -> serde_json::Value {
        let row = rows.iter().find(|r| r.session_id == sid).unwrap();
        let v = serde_json::to_value(row).unwrap();
        let obj = v.as_object().unwrap();
        let keys = ["terminal_id", "page_id", "page_name", "page_window"];
        let present: Vec<&str> = keys.into_iter().filter(|k| obj.contains_key(*k)).collect();
        if present.is_empty() {
            return serde_json::Value::Null;
        }
        assert_eq!(present.len(), 4, "all four or none: {obj:?}");
        assert!(!obj.contains_key("page_name_withheld"), "coord computes it");
        serde_json::json!({
            "terminal_id": obj["terminal_id"],
            "page_id": obj["page_id"],
            "page_name": obj["page_name"],
            "page_window": obj["page_window"],
        })
    }

    /// The wire shape coord's ingest reads, per row, as literal JSON.
    #[test]
    fn runner_hosted_rows_carry_their_terminal_page_label_and_window() {
        let rows = fixture_rows();
        assert_eq!(
            placement_json(&rows, SID_WORK),
            serde_json::json!({
                "terminal_id": "t-work",
                "page_id": "p-work",
                "page_name": "bauhuas",
                "page_window": "main",
            })
        );
        assert_eq!(
            placement_json(&rows, SID_POP),
            serde_json::json!({
                "terminal_id": "t-pop",
                "page_id": "p-pop",
                "page_name": "Popped",
                "page_window": "terminal-popout-2",
            })
        );
        // Unregistered `default` page: the id is known, the name and window
        // are not, and nothing invents them.
        assert_eq!(
            placement_json(&rows, SID_DEFAULT),
            serde_json::json!({
                "terminal_id": "t-default",
                "page_id": "default",
                "page_name": null,
                "page_window": null,
            })
        );
        // Hosted by another runner on the device: no terminal here.
        assert_eq!(placement_json(&rows, SID_ELSEWHERE), serde_json::Value::Null);
    }

    /// A plain shell holds only the provisional record the spawn seam writes;
    /// it never gets a placement, even for a row claiming its session id.
    #[test]
    fn a_plain_shell_carries_no_placement() {
        let rows = fixture_rows();
        assert_eq!(placement_json(&rows, SID_SHELL), serde_json::Value::Null);
    }

    /// A row this runner does not host gets no placement even when a
    /// terminal here carries its session id.
    #[test]
    fn a_row_not_runner_hosted_carries_no_placement() {
        let mut rows = vec![row(SID_WORK, false)];
        let placements = HashMap::from([(
            SID_WORK.to_string(),
            Placement {
                terminal_id: "t".into(),
                page_id: "p".into(),
                page_name: None,
                page_window: None,
            },
        )]);
        apply_placements(&mut rows, &placements);
        assert_eq!(placement_json(&rows, SID_WORK), serde_json::Value::Null);
    }

    /// Three triggers within the debounce window cost ONE post; a later
    /// trigger costs one more.
    #[tokio::test(start_paused = true)]
    async fn three_triggers_within_two_seconds_post_once() {
        let early = Arc::new(EarlyPost::new(EARLY_POST_DEBOUNCE));
        let posts = Arc::new(AtomicUsize::new(0));
        let (e, p) = (early.clone(), posts.clone());
        tokio::spawn(async move {
            loop {
                e.wait().await;
                p.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(posts.load(Ordering::SeqCst), 0, "nothing requested yet");

        early.request();
        tokio::time::sleep(Duration::from_millis(500)).await;
        early.request();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        early.request();
        tokio::time::sleep(Duration::from_millis(1000)).await; // t = 2.5 s
        assert_eq!(posts.load(Ordering::SeqCst), 1, "one post for the burst");

        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(posts.load(Ordering::SeqCst), 1, "no post without a request");

        early.request();
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert_eq!(posts.load(Ordering::SeqCst), 2);
    }
}
