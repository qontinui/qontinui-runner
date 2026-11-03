//! The reconcile pass, entered where the runner enters it.
//!
//! Plan `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`. The
//! liveness poll (`main.rs`) and the boot driver hand
//! [`run_reconcile_pass`](crate::session::reconcile::run_reconcile_pass) a
//! lifecycle store loaded from its snapshot file, the live terminals, and a
//! process snapshot; the pass anchors each pane on its `claude`, queries argv,
//! scans transcripts on disk and reads the panes' Claude Code records itself.
//! This test does exactly that and nothing more: the store is a fixture FILE
//! the runner opens as it does at boot, the transcripts are real files under an
//! isolated config dir, and the only call into the session modules is the pass.
//!
//! Kept in its own file, using only what the pass's callers use, so it compiles
//! with or without the change it observes — the arming proof runs it both ways
//! (`qontinui-claude-config/scripts/arming-record.sh`, which lives in the
//! workspace's config repo, not in this one).
//!
//! What it does NOT do: production also attaches a
//! `pane_binding::LivePaneRegistry` to the store (`main.rs`, beside the
//! transcript probe). This test cannot, because that API is the change's own
//! — naming it would enter on the producer side, and the file would no longer
//! compile without the change. The pass reads the panes' records itself either
//! way; without a registry the store's binding guard has no evidence and never
//! refuses, so what this test observes is the reconcile pass's own choice.

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::reconcile::{run_reconcile_pass, LivePty};
use crate::session::session_lifecycle_store::SessionLifecycleStore;

const WORKDIR: &str = "/work/two-panes";
const S_A: &str = "aaaaaaaa-0000-4000-8000-00000000000a";
const S_B: &str = "bbbbbbbb-0000-4000-8000-00000000000b";
// Above Linux's PID_MAX_LIMIT (2^22): no live process can hold them, so the
// pass's argv query finds nothing — as for a `claude` started without an id.
const SHELL_A: u32 = 4_200_001;
const CLAUDE_A: u32 = 4_200_002;
const SHELL_B: u32 = 4_200_003;
const CLAUDE_B: u32 = 4_200_004;
/// Pane A's `claude` started at 2023-11-14T22:13:20Z, pane B's 100 s earlier.
const CLAUDE_A_STARTED: i64 = 1_700_000_000;
const CLAUDE_B_STARTED: i64 = 1_699_999_900;

/// The lifecycle snapshot as a runner persists it: both panes' rows open on
/// their own terminal, pinned at spawn, neither ever hook-confirmed.
fn snapshot_file() -> String {
    let row = |id: &str, terminal: &str| {
        serde_json::json!({
            "claudeSessionId": id,
            "configDir": null,
            "workingDir": WORKDIR,
            "pageId": "default",
            "zoneIndex": 0,
            "title": terminal,
            "terminalId": terminal,
            "openedAt": 1,
            "lastSeenAt": 1,
            "state": "open",
            "closedAt": null,
            "closeReason": null,
            "provider": "claude",
            "origin": "authoritative"
        })
    };
    serde_json::json!({ S_A: row(S_A, "term-a"), S_B: row(S_B, "term-b") }).to_string()
}

fn pane(terminal: &str, shell: u32) -> LivePty {
    LivePty {
        terminal_id: terminal.to_string(),
        pid: Some(shell),
        working_dir: WORKDIR.to_string(),
        page_id: "default".to_string(),
        title: terminal.to_string(),
        // Resolved by the pass from the snapshot, as main.rs leaves it.
        ai_pid: None,
        ai_start_unix: None,
    }
}

/// Each pane's shell with its `claude` under it.
fn process_table() -> ProcessSnapshot {
    let mut snapshot = ProcessSnapshot::default();
    for (shell, claude, started) in [
        (SHELL_A, CLAUDE_A, CLAUDE_A_STARTED),
        (SHELL_B, CLAUDE_B, CLAUDE_B_STARTED),
    ] {
        snapshot.parent_map.insert(shell, vec![claude]);
        snapshot.names.insert(shell, "bash".to_string());
        snapshot.names.insert(claude, "claude".to_string());
        snapshot.creation_times.insert(shell, started - 10);
        snapshot.creation_times.insert(claude, started);
    }
    snapshot
}

/// The 2026-10-25 shape through the pass: two live panes in one workdir, each
/// holding its own UNCONFIRMED row, and neither pane's Claude Code record
/// readable. S_A's transcript carries no timestamp in its first lines, so no
/// start anchor admits it; S_B's started after BOTH panes' `claude`, so for
/// pane A it is the one transcript rung 3 would correlate. Pane A must not
/// take it, pane B's session must stay on pane B — and pane B, whose own
/// transcript is the one found, ends CONFIRMED there (which also proves the
/// fixture transcripts were discovered at all).
///
/// The terminal manager lists panes in no order this test may rely on, and
/// the pass binds them in list order, so both orders are run, each on a fresh
/// store.
#[tokio::test]
async fn a_live_peers_unconfirmed_session_stays_on_its_pane_through_the_reconcile_pass() {
    let ambient = crate::test_env::isolated_ambient();
    let config = ambient.dir().join(".claude");
    for (id, transcript) in [
        (
            S_A,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"pane a\"}}\n",
        ),
        (
            S_B,
            "{\"type\":\"user\",\"timestamp\":\"2023-11-14T22:21:40Z\",\"message\":{\"role\":\"user\",\"content\":\"pane b\"}}\n",
        ),
    ] {
        let path = crate::terminal::transcript::session_transcript_path(&config, WORKDIR, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, transcript).unwrap();
    }

    let a_then_b = vec![pane("term-a", SHELL_A), pane("term-b", SHELL_B)];
    let b_then_a = vec![pane("term-b", SHELL_B), pane("term-a", SHELL_A)];
    for (order, live) in [("A then B", a_then_b), ("B then A", b_then_a)] {
        let store_path = ambient
            .dir()
            .join(format!("terminal-sessions-{}.json", live[0].terminal_id));
        std::fs::write(&store_path, snapshot_file()).unwrap();
        let store = SessionLifecycleStore::open(&store_path).unwrap();

        let actions = run_reconcile_pass(&store, &live, &process_table(), "test").await;

        let where_is = |id: &str| {
            store
                .get(id)
                .map(|row| (row.state, row.terminal_id, row.confirmed_at.is_some()))
                .unwrap_or_default()
        };
        let (a_state, a_terminal, _) = where_is(S_A);
        assert_eq!(
            (a_state.as_str(), a_terminal.as_str()),
            ("open", "term-a"),
            "{order}: pane A keeps its own session (actions: {actions:?})"
        );
        assert_eq!(
            where_is(S_B),
            ("open".to_string(), "term-b".to_string(), true),
            "{order}: pane B's session stays on pane B, confirmed from its own transcript \
             (actions: {actions:?})"
        );
    }
}
