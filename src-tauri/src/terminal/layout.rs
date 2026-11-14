//! The terminal page layout: which terminals sit on which page (tab), each
//! joined to the session running in it, with the tab labels the webview
//! registered.
//!
//! Two readers share this module, so the join cannot drift between them:
//!
//! - the `terminal_list` Tauri command (`commands::terminal`), which hands a
//!   reconnecting webview each terminal's confirmed `claudeSessionId` and its
//!   `spawnName`; and
//! - `GET /terminal-layout` (`mcp::terminal_layout`), the per-tab session
//!   snapshot qontinui-web saves as a tab list on its `/sessions` page (plan
//!   `2026-11-08-save-per-tab-runner-session-lists-on-sessions-page-kept-14-days`).
//!
//! `GET /terminal-pages` (`mcp::terminals`) reads the same label registry and
//! the same page order.
//!
//! # The join: CONFIRMED records only
//!
//! [`join_terminals`] pairs each live terminal with
//! [`SessionLifecycleStore::find_confirmed_open_by_terminal`]. The spawn-time
//! identity seam writes a PROVISIONAL `open` record for EVERY terminal, plain
//! shells included, so a join on any open record would hand a shell a session
//! id it never ran and the account read at spawn. A terminal with no confirmed
//! record is still listed, with every session field `None`.
//!
//! # Tab labels: registered, never guessed
//!
//! Tab labels and their left-to-right order are webview state
//! (`src/components/terminal/useTerminalPages.ts`). The webview registers its
//! whole page list through the `terminal_pages_register` Tauri command
//! (`commands::terminal_pages`) whenever that list changes, and
//! [`page_labels`] keeps the last registration for this process. A page the
//! webview never registered is served with no label and no order: there is no
//! "Page N" fallback, because it would name a tab the operator never saw.
//! [`PageLabels::registered`] is `false` until the first registration of this
//! process, so a reader can tell "not registered yet" from "unnamed".
//!
//! # Unknown is `None`
//!
//! The builder never serves a stand-in for a value it does not know (served
//! policy `verification-and-evidence` `silent-empty-is-unknown`): the unzoned
//! sentinel
//! ([`UNZONED_ZONE_INDEX`](crate::session::session_lifecycle_store::UNZONED_ZONE_INDEX),
//! `-1`) is served as `None`, the runner's own `"unknown"` account label is
//! `None`, and a terminal with no confirmed record has `finished: None`, never
//! `false`.

use std::collections::BTreeMap;
use std::sync::RwLock;

use chrono::{DateTime, SecondsFormat, Utc};
use qontinui_types::terminal::{
    TerminalLayout, TerminalLayoutPage, TerminalLayoutRunner, TerminalLayoutSession,
};

use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use crate::terminal::types::TerminalInfo;

// ============================================================================
// Tab labels
// ============================================================================

/// One page (tab) as the webview registered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPage {
    /// The page id, the same value as [`TerminalInfo::page_id`].
    pub page_id: String,
    /// The tab label the operator sees; `None` when the webview sent none.
    pub label: Option<String>,
    /// The tab's left-to-right position (0 = leftmost), as registered.
    pub order: u32,
}

/// One consistent read of the registered tab labels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageLabels {
    /// The registered pages, in registered (left-to-right) order.
    pages: Vec<RegisteredPage>,
    /// When the webview last registered; `None` = not yet in this process.
    registered_at: Option<DateTime<Utc>>,
}

impl PageLabels {
    /// A registration made at `at`. `pages` must already be in registered
    /// order with no page id listed twice; `commands::terminal_pages`
    /// normalizes the webview's list into that shape.
    pub fn new(pages: Vec<RegisteredPage>, at: DateTime<Utc>) -> Self {
        Self {
            pages,
            registered_at: Some(at),
        }
    }

    /// Has the webview registered its tab list since this process started?
    pub fn registered(&self) -> bool {
        self.registered_at.is_some()
    }

    /// When the webview last registered; `None` before the first time.
    pub fn registered_at(&self) -> Option<DateTime<Utc>> {
        self.registered_at
    }

    /// The registered pages, in registered order.
    pub fn pages(&self) -> &[RegisteredPage] {
        &self.pages
    }
}

/// Where the last tab-label registration of this process is kept: see
/// [`page_labels`].
#[derive(Debug, Default)]
pub struct PageLabelRegistry {
    current: RwLock<PageLabels>,
}

impl PageLabelRegistry {
    /// An empty registry: nothing registered yet.
    pub const fn new() -> Self {
        Self {
            current: RwLock::new(PageLabels {
                pages: Vec::new(),
                registered_at: None,
            }),
        }
    }

    /// Replace the whole registration. The webview always sends its full page
    /// list, so a page missing from `labels` is no longer registered.
    pub fn replace(&self, labels: PageLabels) {
        // A poisoned lock only means a writer panicked mid-assignment of a
        // plain value; the labels are still a whole value, so take it over.
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = labels;
    }

    /// The current registration.
    pub fn snapshot(&self) -> PageLabels {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// This process's tab-label registry. Instance-scoped by construction: every
/// runner instance is its own process, with its own webview registering into
/// it.
pub fn page_labels() -> &'static PageLabelRegistry {
    static REGISTRY: PageLabelRegistry = PageLabelRegistry::new();
    &REGISTRY
}

/// One page in served order, with its registration (if any) and its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrangedPage<T> {
    /// The page id.
    pub page_id: String,
    /// The registered label; `None` when the page has none registered.
    pub label: Option<String>,
    /// The registered position; `None` when the page was never registered.
    pub order: Option<u32>,
    /// What sits on the page (terminals, session rows).
    pub items: Vec<T>,
}

/// Arrange per-page groups in served order: the registered pages first, in
/// registered order, then every other page id lexically (the order
/// `GET /terminal-pages` served before labels were registered).
///
/// With `include_empty_registered`, a registered page with nothing on it is
/// listed too: the tab exists although no terminal sits on it. Without it,
/// only pages that have items are listed.
pub fn arrange_pages<T>(
    mut groups: BTreeMap<String, Vec<T>>,
    labels: &PageLabels,
    include_empty_registered: bool,
) -> Vec<ArrangedPage<T>> {
    let mut out = Vec::with_capacity(groups.len() + labels.pages.len());
    for registered in &labels.pages {
        let items = groups.remove(&registered.page_id);
        if items.is_none() && !include_empty_registered {
            continue;
        }
        out.push(ArrangedPage {
            page_id: registered.page_id.clone(),
            label: registered.label.clone(),
            order: Some(registered.order),
            items: items.unwrap_or_default(),
        });
    }
    out.extend(groups.into_iter().map(|(page_id, items)| ArrangedPage {
        page_id,
        label: None,
        order: None,
        items,
    }));
    out
}

// ============================================================================
// The join
// ============================================================================

/// One live terminal with what the runner knows about the session in it.
#[derive(Debug, Clone)]
pub struct JoinedTerminal {
    /// The terminal as `TerminalManager::list` reports it.
    pub info: TerminalInfo,
    /// The `claude --name` the runner spawned it under, where it has one.
    pub spawn_name: Option<String>,
    /// The terminal's CONFIRMED open session record; `None` when it has none
    /// (a plain shell, a session whose start no hook has confirmed yet, or no
    /// lifecycle store in this process).
    pub session: Option<TerminalSessionRecord>,
}

/// Join every terminal to its CONFIRMED open session record (see the module
/// docs for why never a provisional one), and to its spawn name through
/// `spawn_name`. Keeps `terminals`' order and drops none of them. `store` is
/// `None` when no lifecycle store is managed: every terminal then has no
/// session.
pub fn join_terminals(
    terminals: Vec<TerminalInfo>,
    store: Option<&SessionLifecycleStore>,
    spawn_name: impl Fn(&str) -> Option<String>,
) -> Vec<JoinedTerminal> {
    terminals
        .into_iter()
        .map(|info| JoinedTerminal {
            spawn_name: spawn_name(&info.id),
            session: store.and_then(|s| s.find_confirmed_open_by_terminal(&info.id)),
            info,
        })
        .collect()
}

// ============================================================================
// The layout
// ============================================================================

/// The runner's own word for an account it could not name
/// (`session_archive::discovery::account_label_for` with no account suffix).
const UNKNOWN_ACCOUNT_LABEL: &str = "unknown";

/// Build the `GET /terminal-layout` body: every page in served order (see
/// [`arrange_pages`]; registered pages are listed even when empty), each with
/// its terminals in `terminals`' order.
pub fn build_layout(
    runner: TerminalLayoutRunner,
    captured_at: String,
    terminals: Vec<JoinedTerminal>,
    labels: &PageLabels,
) -> TerminalLayout {
    let mut groups: BTreeMap<String, Vec<TerminalLayoutSession>> = BTreeMap::new();
    for terminal in terminals {
        groups
            .entry(terminal.info.page_id.clone())
            .or_default()
            .push(session_row(terminal));
    }
    let pages = arrange_pages(groups, labels, true)
        .into_iter()
        .map(|page| TerminalLayoutPage {
            page_id: page.page_id,
            label: page.label,
            order: page.order,
            sessions: page.items,
        })
        .collect();
    TerminalLayout {
        runner,
        labels_registered: labels.registered(),
        captured_at,
        pages,
    }
}

/// One terminal's session row. The session fields come from its confirmed
/// record only; without one they are all `None`.
fn session_row(terminal: JoinedTerminal) -> TerminalLayoutSession {
    let JoinedTerminal {
        info,
        spawn_name,
        session,
    } = terminal;
    let record = session.as_ref();
    let finished_at = record.and_then(|r| r.finished_at);
    TerminalLayoutSession {
        terminal_id: info.id,
        title: non_blank(Some(info.title)),
        // Withholding is the relay egress gate's decision, not the builder's
        // (`mcp::backend_relay::redact_relay_content`).
        title_withheld: false,
        is_alive: info.is_alive,
        spawn_name: non_blank(spawn_name),
        claude_session_id: record.and_then(|r| non_blank(Some(r.claude_session_id.clone()))),
        account_label: record.and_then(|r| known_account(r.account_label.as_deref())),
        session_name: record.and_then(|r| non_blank(r.session_name.clone())),
        zone_index: record.and_then(|r| served_zone(r.zone_index)),
        finished: record.map(|r| r.finished_at.is_some()),
        finished_at: finished_at.and_then(rfc3339_from_millis),
        // A reason is served only beside the finish it was recorded with.
        finish_reason: record
            .filter(|r| r.finished_at.is_some())
            .and_then(|r| non_blank(r.finish_reason.clone())),
    }
}

/// `None` for an absent, empty or whitespace-only string.
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// The account label as served: `None` when absent, blank, or the runner's own
/// `"unknown"`.
fn known_account(label: Option<&str>) -> Option<String> {
    let label = label?.trim();
    if label.is_empty() || label.eq_ignore_ascii_case(UNKNOWN_ACCOUNT_LABEL) {
        return None;
    }
    Some(label.to_string())
}

/// The zone as served: `None` for every negative value — the unzoned sentinel
/// ([`UNZONED_ZONE_INDEX`](crate::session::session_lifecycle_store::UNZONED_ZONE_INDEX),
/// `-1`, which means deliberately unzoned OR unknown) and any other, which
/// names no zone either.
fn served_zone(zone_index: i32) -> Option<u32> {
    u32::try_from(zone_index).ok()
}

/// An instant as RFC 3339 with millisecond precision and a `Z` offset.
pub fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Unix-epoch milliseconds as [`rfc3339`]; `None` when out of range.
pub fn rfc3339_from_millis(millis: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(millis).map(rfc3339)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::UNZONED_ZONE_INDEX;

    const CONFIRMED_AT: i64 = 1_700_000_000_000;
    /// 2026-11-08T09:00:00.000Z.
    const FINISHED_AT: i64 = 1_794_128_400_000;

    fn info(id: &str, page_id: &str, title: &str) -> TerminalInfo {
        TerminalInfo {
            id: id.to_string(),
            title: title.to_string(),
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

    /// A minimal open record of session `id` on terminal `terminal_id`.
    fn record(id: &str, terminal_id: &str) -> TerminalSessionRecord {
        serde_json::from_value(serde_json::json!({
            "claudeSessionId": id,
            "pageId": "p",
            "zoneIndex": 0,
            "terminalId": terminal_id,
            "openedAt": 1,
            "lastSeenAt": 1,
            "state": "open",
        }))
        .expect("minimal record parses")
    }

    fn store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        (dir, store)
    }

    fn runner() -> TerminalLayoutRunner {
        TerminalLayoutRunner {
            instance_name: Some("primary".to_string()),
            port: Some(9876),
            build_id: Some("b-1".to_string()),
            device_id: None,
        }
    }

    fn registered(pages: &[(&str, Option<&str>, u32)]) -> PageLabels {
        PageLabels::new(
            pages
                .iter()
                .map(|(id, label, order)| RegisteredPage {
                    page_id: id.to_string(),
                    label: label.map(str::to_string),
                    order: *order,
                })
                .collect(),
            DateTime::<Utc>::from_timestamp_millis(FINISHED_AT).unwrap(),
        )
    }

    fn layout_of(
        terminals: Vec<TerminalInfo>,
        store: &SessionLifecycleStore,
        labels: &PageLabels,
    ) -> TerminalLayout {
        let joined = join_terminals(terminals, Some(store), |_| None);
        build_layout(runner(), "2026-11-08T09:00:00.000Z".into(), joined, labels)
    }

    fn row<'a>(layout: &'a TerminalLayout, terminal_id: &str) -> &'a TerminalLayoutSession {
        layout
            .pages
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| s.terminal_id == terminal_id)
            .unwrap_or_else(|| panic!("{terminal_id} is not in the layout"))
    }

    /// The plan's first fixture: a plain shell owns only the PROVISIONAL
    /// record the spawn seam writes for every terminal. It is listed, and
    /// every session field is null — never the provisional record's id or
    /// spawn-time account.
    #[test]
    fn a_plain_shell_with_only_a_provisional_record_serves_null_session_fields() {
        let (_dir, store) = store();
        let mut provisional = record("phantom-sess", "t-shell");
        provisional.account_label = Some("gmail".to_string());
        provisional.session_name = Some("seam-name".to_string());
        provisional.zone_index = 3;
        store.record_open(provisional);
        assert!(
            store.find_open_by_terminal("t-shell").is_some(),
            "the fixture's provisional record is open"
        );

        let joined = join_terminals(vec![info("t-shell", "p", "bash")], Some(&store), |id| {
            (id == "t-shell").then(|| "spawned-name".to_string())
        });
        let layout = build_layout(runner(), "now".into(), joined, &PageLabels::default());
        let shell = row(&layout, "t-shell");
        assert_eq!(shell.claude_session_id, None);
        assert_eq!(shell.account_label, None);
        assert_eq!(shell.session_name, None);
        assert_eq!(shell.zone_index, None);
        assert_eq!(shell.finished, None, "unknown, never false");
        assert_eq!(shell.finished_at, None);
        assert_eq!(shell.finish_reason, None);
        assert_eq!(shell.title.as_deref(), Some("bash"));
        assert_eq!(
            shell.spawn_name.as_deref(),
            Some("spawned-name"),
            "the runner's spawn name is served without a confirmed record"
        );
    }

    /// A confirmed record's fields pass, and its finished marker decides
    /// `finished` both ways: `true` with the time and reason, `false` with
    /// neither.
    #[test]
    fn a_confirmed_session_serves_its_identity_and_finished_marker() {
        let (_dir, store) = store();
        let mut done = record("sess-done", "t-done");
        done.confirmed_at = Some(CONFIRMED_AT);
        done.account_label = Some("hotmail".to_string());
        done.session_name = Some("1234 fix the thing".to_string());
        done.zone_index = 4;
        store.record_open(done);
        store
            .set_finished("sess-done", true, Some("unattended: 3 units".to_string()))
            .expect("finish the fixture");

        let mut working = record("sess-work", "t-work");
        working.confirmed_at = Some(CONFIRMED_AT);
        working.account_label = Some("gmail".to_string());
        store.record_open(working);

        let layout = layout_of(
            vec![info("t-done", "p", "a"), info("t-work", "p", "b")],
            &store,
            &PageLabels::default(),
        );
        let done = row(&layout, "t-done");
        assert_eq!(done.claude_session_id.as_deref(), Some("sess-done"));
        assert_eq!(done.account_label.as_deref(), Some("hotmail"));
        assert_eq!(done.session_name.as_deref(), Some("1234 fix the thing"));
        assert_eq!(done.zone_index, Some(4));
        assert_eq!(done.finished, Some(true));
        let at = done.finished_at.as_deref().expect("a finished time");
        assert!(DateTime::parse_from_rfc3339(at).is_ok(), "{at} is RFC 3339");
        assert_eq!(done.finish_reason.as_deref(), Some("unattended: 3 units"));

        let working = row(&layout, "t-work");
        assert_eq!(working.claude_session_id.as_deref(), Some("sess-work"));
        assert_eq!(
            working.finished,
            Some(false),
            "a confirmed record with no mark"
        );
        assert_eq!(working.finished_at, None);
        assert_eq!(working.finish_reason, None);
    }

    /// The runner's `"unknown"` account (any case) and the unzoned sentinel
    /// are served as null, never as the sentinel.
    #[test]
    fn an_unknown_account_and_an_unzoned_tile_serve_null() {
        let (_dir, store) = store();
        for (id, terminal, account) in [
            ("s1", "t1", Some("unknown")),
            ("s2", "t2", Some("UNKNOWN")),
            ("s3", "t3", Some("  ")),
            ("s4", "t4", None),
        ] {
            let mut rec = record(id, terminal);
            rec.confirmed_at = Some(CONFIRMED_AT);
            rec.account_label = account.map(str::to_string);
            rec.zone_index = UNZONED_ZONE_INDEX;
            store.record_open(rec);
        }
        let layout = layout_of(
            vec![
                info("t1", "p", "a"),
                info("t2", "p", "b"),
                info("t3", "p", "c"),
                info("t4", "p", "d"),
            ],
            &store,
            &PageLabels::default(),
        );
        for terminal in ["t1", "t2", "t3", "t4"] {
            let session = row(&layout, terminal);
            assert!(session.claude_session_id.is_some(), "{terminal} is joined");
            assert_eq!(session.account_label, None, "{terminal}");
            assert_eq!(session.zone_index, None, "{terminal}: -1 is not a zone");
        }
        assert_eq!(served_zone(0), Some(0));
        assert_eq!(served_zone(8), Some(8));
        assert_eq!(served_zone(UNZONED_ZONE_INDEX), None);
        assert_eq!(served_zone(-7), None);
        assert_eq!(known_account(Some(" clp ")).as_deref(), Some("clp"));
    }

    /// A tile at the unzoned sentinel (`-1`) reaches the wire as an explicit
    /// `"zoneIndex": null` — never `-1`, and never a missing key — while a real
    /// zone reaches it as its number.
    #[test]
    fn an_unzoned_tile_serializes_zone_index_as_null() {
        let (_dir, store) = store();
        for (id, terminal, zone) in [
            ("s-unzoned", "t-unzoned", UNZONED_ZONE_INDEX),
            ("s-zoned", "t-zoned", 3),
        ] {
            let mut rec = record(id, terminal);
            rec.confirmed_at = Some(CONFIRMED_AT);
            rec.zone_index = zone;
            store.record_open(rec);
        }
        let layout = layout_of(
            vec![info("t-unzoned", "p", "a"), info("t-zoned", "p", "b")],
            &store,
            &PageLabels::default(),
        );
        let json = serde_json::to_value(&layout).unwrap();
        let sessions = json["pages"][0]["sessions"].as_array().unwrap();
        let zone_of = |terminal: &str| {
            let session = sessions
                .iter()
                .find(|s| s["terminalId"] == terminal)
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("{terminal} row"));
            session
                .get("zoneIndex")
                .cloned()
                .unwrap_or_else(|| panic!("{terminal}: zoneIndex must be present"))
        };
        assert_eq!(zone_of("t-unzoned"), serde_json::Value::Null);
        assert_eq!(zone_of("t-zoned"), serde_json::json!(3));
    }

    /// Registered pages come first, in registered order — empty ones
    /// included — with their labels; a page the webview never registered
    /// follows with no label and no order; sessions keep terminal order.
    #[test]
    fn pages_follow_the_registered_order_and_an_unregistered_page_has_no_label() {
        let (_dir, store) = store();
        let labels = registered(&[
            ("p-right", Some("Right"), 0),
            ("p-empty", Some("Empty"), 1),
            ("p-left", Some("Left"), 2),
        ]);
        let layout = layout_of(
            vec![
                info("t1", "p-left", "one"),
                info("t2", "p-stray", "two"),
                info("t3", "p-right", "three"),
                info("t4", "p-left", "four"),
            ],
            &store,
            &labels,
        );
        assert!(layout.labels_registered);
        let pages: Vec<(&str, Option<&str>, Option<u32>, Vec<&str>)> = layout
            .pages
            .iter()
            .map(|p| {
                (
                    p.page_id.as_str(),
                    p.label.as_deref(),
                    p.order,
                    p.sessions.iter().map(|s| s.terminal_id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            pages,
            vec![
                ("p-right", Some("Right"), Some(0), vec!["t3"]),
                ("p-empty", Some("Empty"), Some(1), vec![]),
                ("p-left", Some("Left"), Some(2), vec!["t1", "t4"]),
                ("p-stray", None, None, vec!["t2"]),
            ]
        );
    }

    /// Before the first registration every page is served with no label and
    /// no order, lexically, and `labelsRegistered` says why.
    #[test]
    fn before_any_registration_no_page_has_a_label() {
        let (_dir, store) = store();
        let layout = layout_of(
            vec![info("t1", "p-b", "x"), info("t2", "p-a", "y")],
            &store,
            &PageLabels::default(),
        );
        assert!(!layout.labels_registered);
        let pages: Vec<(&str, Option<&str>, Option<u32>)> = layout
            .pages
            .iter()
            .map(|p| (p.page_id.as_str(), p.label.as_deref(), p.order))
            .collect();
        assert_eq!(pages, vec![("p-a", None, None), ("p-b", None, None)]);
    }

    /// Without `include_empty_registered` (the `GET /terminal-pages` shape) a
    /// registered page with nothing on it is left out, and the order rule is
    /// the same.
    #[test]
    fn arranging_without_empty_pages_drops_only_the_empty_registered_ones() {
        let labels = registered(&[("b", Some("B"), 0), ("empty", None, 1), ("a", None, 2)]);
        let groups: BTreeMap<String, Vec<u8>> = [
            ("a".to_string(), vec![1]),
            ("b".to_string(), vec![2]),
            ("z".to_string(), vec![3]),
        ]
        .into();
        let arranged: Vec<(String, Option<String>, Option<u32>)> =
            arrange_pages(groups, &labels, false)
                .into_iter()
                .map(|p| (p.page_id, p.label, p.order))
                .collect();
        assert_eq!(
            arranged,
            vec![
                ("b".to_string(), Some("B".to_string()), Some(0)),
                ("a".to_string(), None, Some(2)),
                ("z".to_string(), None, None),
            ]
        );
    }

    /// The registry holds one whole registration at a time, replaced whole.
    #[test]
    fn the_registry_replaces_the_whole_registration() {
        let registry = PageLabelRegistry::new();
        assert!(!registry.snapshot().registered());
        registry.replace(registered(&[
            ("p1", Some("One"), 0),
            ("p2", Some("Two"), 1),
        ]));
        registry.replace(registered(&[("p2", Some("Deux"), 0)]));
        let now = registry.snapshot();
        assert!(now.registered());
        assert_eq!(
            now.pages(),
            &[RegisteredPage {
                page_id: "p2".to_string(),
                label: Some("Deux".to_string()),
                order: 0,
            }]
        );
    }

    /// The served wire shape: camelCase keys, and every unknown serialized as
    /// an explicit `null` rather than left out.
    #[test]
    fn the_layout_serializes_camel_case_with_explicit_nulls() {
        let (_dir, store) = store();
        let layout = layout_of(
            vec![info("t-shell", "p", "bash")],
            &store,
            &PageLabels::default(),
        );
        let json = serde_json::to_value(&layout).unwrap();
        assert_eq!(json["labelsRegistered"], false);
        assert_eq!(json["runner"]["instanceName"], "primary");
        assert!(json["runner"]
            .as_object()
            .unwrap()
            .get("deviceId")
            .is_some_and(serde_json::Value::is_null));
        let page = &json["pages"][0];
        assert!(page.as_object().unwrap()["label"].is_null());
        assert!(page.as_object().unwrap()["order"].is_null());
        let session = page["sessions"][0].as_object().unwrap();
        for key in [
            "claudeSessionId",
            "accountLabel",
            "sessionName",
            "zoneIndex",
            "finished",
            "finishedAt",
            "finishReason",
            "spawnName",
        ] {
            assert!(
                session.get(key).is_some_and(serde_json::Value::is_null),
                "{key} must be an explicit null: {session:?}"
            );
        }
        assert_eq!(session["titleWithheld"], false);
        assert_eq!(session["isAlive"], true);
    }

    #[test]
    fn timestamps_are_rfc3339_millis_utc() {
        assert_eq!(
            rfc3339_from_millis(FINISHED_AT).as_deref(),
            Some("2026-11-08T09:00:00.000Z")
        );
        assert_eq!(rfc3339_from_millis(i64::MAX), None);
    }
}
