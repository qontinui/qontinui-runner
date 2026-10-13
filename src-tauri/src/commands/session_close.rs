//! The Session Manager's two tab-level doors (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 3):
//!
//! - `terminal_graceful_close` — **Close**, the D7 path. It goes through
//!   [`TerminalManager::graceful_exit`] and nothing else: `/exit` is typed only
//!   at an empty prompt, the tab closes only once every `claude` it saw has left,
//!   and every other outcome comes back as the [`GracefulExitOutcome`] it is.
//!   There is NO fallback to `terminal_close` here — a refusal is reported to
//!   the operator as "not closed: <reason>", and the separately confirmed
//!   Force close is the frontend's own, distinct call to `terminal_close`.
//! - `terminal_set_page` — **Move to page**, the Tauri twin of
//!   `POST /terminals/{id}/move` over the same [`TerminalManager::set_page`].

use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::session::SessionRegistry;
use crate::terminal::graceful_exit::{GracefulExitOutcome, DEFAULT_DEADLINE};
use crate::terminal::TerminalManager;

/// Ceiling on a requested close deadline, so a typo cannot hold the call open
/// for an hour. Same bound as the debug door's.
pub const MAX_CLOSE_DEADLINE: Duration = Duration::from_secs(300);

/// The deadline a close waits for `claude` to leave: the primitive's own
/// default, or the requested seconds clamped to `1..=`[`MAX_CLOSE_DEADLINE`].
pub fn graceful_close_deadline(raw: Option<u64>) -> Duration {
    raw.map(|secs| Duration::from_secs(secs.clamp(1, MAX_CLOSE_DEADLINE.as_secs())))
        .unwrap_or(DEFAULT_DEADLINE)
}

/// Did this outcome close the tab? Only [`GracefulExitOutcome::Exited`] did;
/// every other arm left the pane open (or, for `CloseOutcomeUnknown`, asserts
/// nothing about it — so it is not "closed" either).
pub fn closed_the_tab(outcome: &GracefulExitOutcome) -> bool {
    matches!(outcome, GracefulExitOutcome::Exited { .. })
}

/// Close a terminal tab through `TerminalManager::graceful_exit` and answer the
/// outcome verbatim. `Err` only when the terminal is unknown to this runner.
#[tauri::command]
pub async fn terminal_graceful_close(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_registry: tauri::State<'_, Arc<SessionRegistry>>,
    terminal_id: String,
    deadline_s: Option<u64>,
) -> Result<GracefulExitOutcome, String> {
    // Captured BEFORE the exit: a closed tab no longer answers it.
    let coord_id = terminal_manager
        .get(&terminal_id)
        .and_then(|s| s.coord_session_id());
    let outcome = terminal_manager
        .inner()
        .graceful_exit(&terminal_id, graceful_close_deadline(deadline_s))
        .await?;
    // The coord mirror closes with the tab — exactly as `terminal_close` does —
    // and only when the tab actually closed.
    if closed_the_tab(&outcome) {
        if let Some(coord_id) = coord_id {
            if let Err(e) = session_registry.inner().close_by_id(coord_id) {
                warn!(
                    terminal_id = %terminal_id,
                    coord_session = %coord_id,
                    error = %e,
                    "terminal_graceful_close: coord session close failed"
                );
            }
        }
    }
    Ok(outcome)
}

/// A page id as `set_page` takes it: trimmed, and never empty.
pub fn normalize_page_id(raw: &str) -> Result<String, String> {
    let page_id = raw.trim();
    if page_id.is_empty() {
        return Err("pageId is required and must be non-empty".to_string());
    }
    Ok(page_id.to_string())
}

/// Move a terminal onto another Terminal page (`POST /terminals/{id}/move`'s
/// twin). The manager emits `terminal-page-changed`, which every page's tab
/// list already follows.
#[tauri::command]
pub fn terminal_set_page(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    terminal_id: String,
    page_id: String,
) -> Result<(), String> {
    let page_id = normalize_page_id(&page_id)?;
    terminal_manager.set_page(&terminal_id, page_id, &app_handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_defaults_and_clamps() {
        assert_eq!(graceful_close_deadline(None), DEFAULT_DEADLINE);
        assert_eq!(graceful_close_deadline(Some(0)), Duration::from_secs(1));
        assert_eq!(graceful_close_deadline(Some(45)), Duration::from_secs(45));
        assert_eq!(graceful_close_deadline(Some(86_400)), MAX_CLOSE_DEADLINE);
    }

    #[test]
    fn only_exited_counts_as_closed() {
        assert!(closed_the_tab(&GracefulExitOutcome::Exited {
            waited_ms: 10,
            claude_pids: vec![1],
        }));
        for open in [
            GracefulExitOutcome::NoLiveClaude,
            GracefulExitOutcome::ProbeUnavailable {
                detail: "x".into(),
            },
            GracefulExitOutcome::Refused {
                reason: "busy".into(),
                claude_pids: vec![1],
            },
            GracefulExitOutcome::ExitStuck {
                waited_ms: 1,
                claude_pids: vec![1],
                last_probe_unreadable: false,
            },
            GracefulExitOutcome::CloseRefused {
                waited_ms: 1,
                claude_pids: vec![1],
                reason: "relaunched".into(),
            },
            GracefulExitOutcome::CloseOutcomeUnknown {
                waited_ms: 1,
                claude_pids: vec![1],
                detail: "task died".into(),
            },
        ] {
            assert!(!closed_the_tab(&open), "{open:?} must not read as closed");
        }
    }

    #[test]
    fn page_id_is_trimmed_and_required() {
        assert_eq!(normalize_page_id("  p1 ").unwrap(), "p1");
        assert!(normalize_page_id("   ").is_err());
        assert!(normalize_page_id("").is_err());
    }

    /// D7: the close door reaches a pane ONLY through `graceful_exit`. A plain
    /// close (`.close(` / `terminal_close`) here would be the silent kill
    /// fallback the plan forbids, so the command body must not name one.
    #[test]
    fn graceful_close_never_falls_back_to_a_kill() {
        let src = include_str!("session_close.rs");
        let start = src
            .find("pub async fn terminal_graceful_close")
            .expect("the command is defined");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .expect("the command body ends");
        let body = &src[start..end];
        assert!(body.contains(".graceful_exit("), "closes through graceful_exit");
        assert!(!body.contains(".close("), "never a plain close");
        assert!(!body.contains("terminal_close("), "never the kill door");
    }

    /// The outcome crosses to the frontend as `{ "outcome": "<snake_case>", … }`
    /// — the shape `sessionClose.ts` parses.
    #[test]
    fn outcome_wire_shape_is_tagged_snake_case() {
        let json = serde_json::to_value(GracefulExitOutcome::CloseRefused {
            waited_ms: 5,
            claude_pids: vec![7],
            reason: "r".into(),
        })
        .unwrap();
        assert_eq!(json["outcome"], "close_refused");
        assert_eq!(json["reason"], "r");
        let json = serde_json::to_value(GracefulExitOutcome::NoLiveClaude).unwrap();
        assert_eq!(json["outcome"], "no_live_claude");
    }
}
