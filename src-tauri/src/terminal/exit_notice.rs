//! Where a pane's `terminal-exit` notice goes, and who is allowed to send one.
//!
//! Plan `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 1.
//!
//! # What was broken
//!
//! `terminal-exit` had exactly one emitter: the waiter thread, fired the
//! instant [`crate::terminal::pane_io::PaneIo::wait`] returned. That covers the
//! event's namesake — the child process ending — and it is also the frontend's
//! only tear-down signal: after boot, the tab list re-syncs against
//! `terminal_list` on `terminal-exit` and on nothing else
//! (`useTerminalManager.ts`).
//!
//! But a session leaves `TerminalManager` by a SECOND, independent act:
//! `terminal_close`, reachable from the Tauri command, the MCP/HTTP
//! `tauri_proxy` door and the backend relay. Those are two different facts
//! about a pane, and the frontend reacts to them differently — a pane whose
//! child exited keeps its tab (the operator still wants to read the output; the
//! backend still lists it), while a pane whose session is gone must lose its
//! tab.
//!
//! For a terminal whose child had ALREADY exited, the waiter thread was long
//! dead by the time the close arrived, so the removal emitted nothing at all.
//! The backend session was torn down, `terminal_list` stopped listing it, and
//! the webview never re-read the list: the tab stayed rendered forever against
//! a session that no longer existed. On a headless runner that is the whole
//! story, because the HTTP/MCP door is the ONLY close door there and the
//! consumer of the tab list is a remote webview whose sole tear-down signal is
//! this event's WS re-broadcast.
//!
//! # The fix
//!
//! [`crate::terminal::session::TerminalSession::close_with_deadline`] — the one
//! path every close door funnels through — sends its own notice. It is
//! deliberately NOT suppressed when the waiter already sent one: the two say
//! different things, and the one the webview needs is the LATER one, because
//! only then does `terminal_list` no longer list the terminal. The receiver is
//! idempotent (`TerminalInstance` records the first exit it sees and ignores
//! repeats), so a live pane's ordinary close costing two notices is harmless.
//!
//! Splitting the transport out behind [`ExitNoticeSink`] is what makes the
//! emission testable: production wires the Tauri event plus the WS re-broadcast
//! that reaches a remote webview, and a test hands in a recorder.

/// Where an exit notice goes.
///
/// Deliberately narrow: the waiter thread's OTHER exit-time duties — the mobile
/// push, closing the coord session mirror, auto-closing an emptied pop-out
/// window — mean "the child process ended", which a session tear-down does not,
/// so they stay where they are.
pub trait ExitNoticeSink {
    /// Deliver one exit notice for `terminal_id`.
    fn emit_exit(&self, terminal_id: &str, exit_code: Option<i32>);
}

/// Blanket forward through a reference, so a caller can hand out a borrowed
/// sink (a test's recorder, say) without giving it away.
impl<T: ExitNoticeSink + ?Sized> ExitNoticeSink for &T {
    fn emit_exit(&self, terminal_id: &str, exit_code: Option<i32>) {
        (**self).emit_exit(terminal_id, exit_code)
    }
}

/// Production sink: the Tauri `terminal-exit` event plus the WS relay
/// re-broadcast that carries it to a remote webview — on a headless runner, the
/// only consumer there is.
pub struct TauriExitSink<'a> {
    /// The app handle to emit on.
    pub app: &'a tauri::AppHandle,
}

impl ExitNoticeSink for TauriExitSink<'_> {
    fn emit_exit(&self, terminal_id: &str, exit_code: Option<i32>) {
        use tauri::Emitter;

        let event = crate::terminal::types::TerminalExitEvent {
            terminal_id: terminal_id.to_string(),
            exit_code,
        };
        if let Err(e) = self.app.emit("terminal-exit", &event) {
            tracing::warn!(
                terminal_id = %terminal_id,
                error = %e,
                "Failed to emit terminal exit event"
            );
        }
        // Re-broadcast for the backend relay: remote/mobile consumers, and the
        // SOURCE runner of a remote-attached tab.
        crate::event_system::broadcast_ws_notification(
            self.app,
            "terminal-exit",
            &serde_json::json!({
                "terminal_id": terminal_id,
                "exit_code": exit_code,
            }),
        );
    }
}

/// Records every notice it is handed, for assertions.
///
/// Lives outside `mod tests` so `terminal::session`'s tests can drive the
/// tear-down emission without a Tauri `AppHandle` — same arrangement as
/// `remote_pane_io::tests::RecordingSink`.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingExitSink {
    notices: std::sync::Mutex<Vec<(String, Option<i32>)>>,
}

#[cfg(test)]
impl RecordingExitSink {
    /// Every `(terminal_id, exit_code)` this sink was handed, in order.
    pub(crate) fn notices(&self) -> Vec<(String, Option<i32>)> {
        self.notices.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl ExitNoticeSink for RecordingExitSink {
    fn emit_exit(&self, terminal_id: &str, exit_code: Option<i32>) {
        self.notices
            .lock()
            .unwrap()
            .push((terminal_id.to_string(), exit_code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_sink_captures_id_and_code() {
        let sink = RecordingExitSink::default();
        sink.emit_exit("term-1", Some(3));
        assert_eq!(sink.notices(), vec![("term-1".to_string(), Some(3))]);
    }

    /// An unknown exit code (the `wait()` itself failed, or the pane never ran
    /// a child) must still produce a notice — the webview's re-sync is what
    /// matters, and it does not read the code.
    #[test]
    fn an_unknown_exit_code_still_produces_a_notice() {
        let sink = RecordingExitSink::default();
        sink.emit_exit("term-1", None);
        assert_eq!(sink.notices(), vec![("term-1".to_string(), None)]);
    }
}
