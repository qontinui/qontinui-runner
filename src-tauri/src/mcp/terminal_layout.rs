//! `GET /terminal-layout`: this runner's terminal tabs and the sessions open
//! in each, read at one instant (plan
//! `2026-11-08-save-per-tab-runner-session-lists-on-sessions-page-kept-14-days`,
//! Phase 1).
//!
//! The body is `qontinui_types::terminal::TerminalLayout`, declared once in
//! qontinui-schemas and validated by qontinui-web with the generated model. It
//! is built by `terminal::layout::build_layout` over the same join
//! `terminal_list` uses: the live terminals, each terminal's CONFIRMED session
//! record, and the tab labels the webview registered. Every value the runner
//! does not know is `null`.
//!
//! # Why a module of its own
//!
//! Not `mcp::terminals`. Every route there is listed in its `route_entries()`,
//! and `terminals::tests::no_terminal_route_is_reachable_over_the_http_relay`
//! and `relay_path_policy::tests::no_terminal_route_is_allowlisted` refuse each
//! of them over the HTTP relay. This route IS relayed, as a GET only
//! (`relay_path_policy::RELAY_ALLOWED`), so it lives outside that table rather
//! than weakening those tests.
//!
//! # Who may read it
//!
//! It is a credential door in the origin guard (`origin_guard::CREDENTIAL_DOORS`):
//! no browser origin reaches it, the Trusted dev frontend included. qontinui-web's
//! saved-session-lists capture reads it through the `http_request` relay arm,
//! which arrives with no `Origin` and no `Sec-Fetch-*` and so classifies as a
//! non-browser caller. A terminal title is PTY-derived session content (OSC
//! 0/2), so the relay withholds each session row's `title` (`null`, with
//! `titleWithheld: true`) when that session's tenant has terminal streaming
//! off (`backend_relay::redact_relay_content`). Ids, accounts, names, labels
//! and finished markers pass. A local caller gets the titles as they are.

use std::path::Path;
use std::sync::Arc;

use axum::{extract::State, response::Json};
use chrono::{DateTime, Utc};
use qontinui_types::terminal::{TerminalLayout, TerminalLayoutRunner};
use tauri::Manager;

use crate::mcp::types::ApiState;
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::terminal::layout::{self, PageLabels};
use crate::terminal::types::TerminalInfo;

/// `GET /terminal-layout` — see the module docs. Reads its inputs off the
/// running process and hands them to [`compose_layout`].
pub async fn terminal_layout_handler(State(state): State<Arc<ApiState>>) -> Json<TerminalLayout> {
    let terminal_manager = crate::mcp::terminals::get_terminal_manager(&state);
    let store = state.app_handle.try_state::<Arc<SessionLifecycleStore>>();
    let readings = RunnerReadings {
        instance_name: crate::instance::instance_name(),
        port: crate::mcp::types::runner_api_port(&state.app_state),
        build_id: env!("RUNNER_BUILD_ID").to_string(),
        device_id: read_device_id(crate::machine_identity::machine_file_path().as_deref()),
    };
    Json(compose_layout(
        terminal_manager.list(),
        store.as_deref().map(Arc::as_ref),
        |id| terminal_manager.get(id).and_then(|s| s.spawn_name()),
        &layout::page_labels().snapshot(),
        readings,
        Utc::now(),
    ))
}

/// What the handler reads about this runner instance, raw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunnerReadings {
    /// `QONTINUI_INSTANCE_NAME`, when set (`crate::instance::instance_name`).
    pub(crate) instance_name: Option<String>,
    /// The port this instance's API is bound to; 0 before it binds.
    pub(crate) port: u16,
    /// The build id `/health` serves as `buildId`.
    pub(crate) build_id: String,
    /// The machine's canonical device id; `None` when it cannot be read.
    pub(crate) device_id: Option<String>,
}

/// The `runner` block, as the runner reports itself:
///
/// - `instanceName`: the name this instance registers under on its relay
///   socket (`backend_relay::reported_instance_name`), so a capture names the
///   same runner the web already lists;
/// - `port`: the bound port, `null` before binding (0);
/// - `buildId`: the same value as `/health` `buildId`, `null` if blank;
/// - `deviceId`: the canonical device id, `null` when unreadable or blank.
pub(crate) fn runner_identity(readings: RunnerReadings) -> TerminalLayoutRunner {
    let non_blank = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    TerminalLayoutRunner {
        instance_name: Some(crate::mcp::backend_relay::reported_instance_name(
            readings.instance_name,
        )),
        port: (readings.port != 0).then_some(readings.port),
        build_id: non_blank(Some(readings.build_id)),
        device_id: non_blank(readings.device_id),
    }
}

/// The device id stored in the `machine.json` at `path`; `None` when there is
/// no path, the file is missing or unreadable, or it names no device id.
/// Never mints one (`machine_identity::read_device_id_at`).
pub(crate) fn read_device_id(path: Option<&Path>) -> Option<String> {
    crate::machine_identity::read_device_id_at(path?).ok()
}

/// The whole `GET /terminal-layout` body from its inputs: everything the
/// handler does except read them off the running process. Joins `terminals`
/// to their CONFIRMED records in `store`, names each through `spawn_name`, and
/// builds the layout in `labels`' order, stamped `captured_at`.
pub(crate) fn compose_layout(
    terminals: Vec<TerminalInfo>,
    store: Option<&SessionLifecycleStore>,
    spawn_name: impl Fn(&str) -> Option<String>,
    labels: &PageLabels,
    readings: RunnerReadings,
    captured_at: DateTime<Utc>,
) -> TerminalLayout {
    layout::build_layout(
        runner_identity(readings),
        layout::rfc3339(captured_at),
        layout::join_terminals(terminals, store, spawn_name),
        labels,
    )
}

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;

    axum::Router::new().route("/terminal-layout", get(terminal_layout_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::TerminalSessionRecord;
    use crate::terminal::layout::RegisteredPage;

    fn info(id: &str, page_id: &str, title: &str) -> TerminalInfo {
        TerminalInfo {
            id: id.to_string(),
            title: title.to_string(),
            pid: None,
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

    fn record(id: &str, terminal_id: &str, confirmed: bool) -> TerminalSessionRecord {
        let mut rec: TerminalSessionRecord = serde_json::from_value(serde_json::json!({
            "claudeSessionId": id,
            "pageId": "p-work",
            "zoneIndex": 1,
            "terminalId": terminal_id,
            "openedAt": 1,
            "lastSeenAt": 1,
            "state": "open",
            "accountLabel": "gmail",
            "sessionName": "1234 work",
        }))
        .expect("minimal record parses");
        rec.confirmed_at = confirmed.then_some(1_700_000_000_000);
        rec
    }

    fn machine_json(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("machine.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The handler's whole composition, from the inputs it reads: the store
    /// lookup joins only the CONFIRMED record (the provisional one on the
    /// shell is ignored), the spawn-name closure is asked per terminal, the
    /// registered labels order the pages, the clock stamps `capturedAt`, and
    /// the runner block carries the readings — `deviceId` read from a
    /// `machine.json`, port 0 served as null, an unset instance name as the
    /// relay's `"primary"`.
    #[test]
    fn the_handler_composition_joins_names_orders_and_identifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        store.record_open(record("sess-claude", "t-claude", true));
        store.record_open(record("sess-phantom", "t-shell", false));
        let device = machine_json(dir.path(), r#"{"device_id":"dev-123","hostname":"h"}"#);
        let labels = PageLabels::new(
            vec![
                RegisteredPage {
                    page_id: "p-shells".to_string(),
                    label: Some("Shells".to_string()),
                    order: 0,
                },
                RegisteredPage {
                    page_id: "p-work".to_string(),
                    label: Some("Work".to_string()),
                    order: 1,
                },
            ],
            DateTime::<Utc>::from_timestamp_millis(1_794_128_400_000).unwrap(),
        );
        let asked = std::cell::RefCell::new(Vec::new());
        let layout = compose_layout(
            vec![
                info("t-claude", "p-work", "claude"),
                info("t-shell", "p-shells", "bash"),
            ],
            Some(&store),
            |id| {
                asked.borrow_mut().push(id.to_string());
                (id == "t-claude").then(|| "1234 work".to_string())
            },
            &labels,
            RunnerReadings {
                instance_name: None,
                port: 0,
                build_id: "b-77".to_string(),
                device_id: read_device_id(Some(&device)),
            },
            DateTime::<Utc>::from_timestamp_millis(1_794_128_460_000).unwrap(),
        );

        assert_eq!(layout.captured_at, "2026-11-08T09:01:00.000Z");
        assert_eq!(*asked.borrow(), vec!["t-claude", "t-shell"]);
        assert_eq!(
            layout.runner,
            TerminalLayoutRunner {
                instance_name: Some("primary".to_string()),
                port: None,
                build_id: Some("b-77".to_string()),
                device_id: Some("dev-123".to_string()),
            }
        );
        let pages: Vec<(&str, Option<&str>)> = layout
            .pages
            .iter()
            .map(|p| (p.page_id.as_str(), p.label.as_deref()))
            .collect();
        assert_eq!(
            pages,
            vec![("p-shells", Some("Shells")), ("p-work", Some("Work"))]
        );
        let shell = &layout.pages[0].sessions[0];
        assert_eq!(shell.terminal_id, "t-shell");
        assert_eq!(
            shell.claude_session_id, None,
            "the provisional record is not joined"
        );
        assert_eq!(shell.account_label, None);
        assert_eq!(shell.spawn_name, None);
        let claude = &layout.pages[1].sessions[0];
        assert_eq!(claude.claude_session_id.as_deref(), Some("sess-claude"));
        assert_eq!(claude.account_label.as_deref(), Some("gmail"));
        assert_eq!(claude.spawn_name.as_deref(), Some("1234 work"));
        assert_eq!(claude.zone_index, Some(1));
        assert_eq!(claude.finished, Some(false));
    }

    /// With no lifecycle store managed, every terminal is still listed, with
    /// no session joined; a bound port and a set instance name are served as
    /// they are.
    #[test]
    fn without_a_store_every_terminal_is_listed_unjoined() {
        let layout = compose_layout(
            vec![info("t-1", "p", "x")],
            None,
            |_| None,
            &PageLabels::default(),
            RunnerReadings {
                instance_name: Some("test-runner-7".to_string()),
                port: 9877,
                build_id: "b".to_string(),
                device_id: None,
            },
            Utc::now(),
        );
        assert!(!layout.labels_registered);
        assert_eq!(layout.pages[0].sessions[0].claude_session_id, None);
        assert_eq!(layout.pages[0].sessions[0].finished, None);
        assert_eq!(
            layout.runner.instance_name.as_deref(),
            Some("test-runner-7")
        );
        assert_eq!(layout.runner.port, Some(9877));
        assert_eq!(layout.runner.device_id, None);
    }

    /// The device id is read, never minted: a missing file, no path, a file
    /// naming none, and an unparseable one are all `None`.
    #[test]
    fn the_device_id_read_is_none_when_it_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_device_id(None), None);
        assert_eq!(read_device_id(Some(&dir.path().join("absent.json"))), None);
        let blank = machine_json(dir.path(), r#"{"device_id":"","hostname":"h"}"#);
        assert_eq!(read_device_id(Some(&blank)), None);
        let garbage = machine_json(dir.path(), "not json");
        assert_eq!(read_device_id(Some(&garbage)), None);
        let legacy = machine_json(dir.path(), r#"{"machine_id":"dev-legacy","hostname":"h"}"#);
        assert_eq!(read_device_id(Some(&legacy)).as_deref(), Some("dev-legacy"));
        assert!(
            !dir.path().join("absent.json").exists(),
            "nothing was minted"
        );
    }

    /// A blank build id or device id reading is served as null, never `""`.
    #[test]
    fn blank_readings_are_served_as_null() {
        let runner = runner_identity(RunnerReadings {
            instance_name: Some("n".to_string()),
            port: 1,
            build_id: "  ".to_string(),
            device_id: Some(String::new()),
        });
        assert_eq!(runner.build_id, None);
        assert_eq!(runner.device_id, None);
    }
}
