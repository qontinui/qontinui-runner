//! Transport layer for [`crate::session::Session`].
//!
//! A transport is the thing that actually runs the work — a PTY-backed
//! shell, a Claude CLI subprocess, or the workflow executor. The
//! [`Transport`] trait wraps the three so [`crate::session::Session`]
//! treats them uniformly: select by `intent.kind`, drive `start` → `close`,
//! pump input/resize while alive.
//!
//! Plan §Phase 2 (`session/transport/{pty,claude_cli,workflow}.rs`) — each
//! impl delegates to the existing module that owns the actual subprocess
//! lifecycle (`terminal::TerminalManager`, `claude_session::ClaudeSession`,
//! `unified_workflow_executor`). Phase 9 cleanup absorbs those callees;
//! during Phase 2 we keep them in place so the runner stays operable while
//! the new surface lands.

pub mod claude_cli;
pub mod pty;
pub mod workflow;

use std::sync::Arc;

use super::intent::Intent;

/// Concrete handle returned by [`Transport::start`]. Holds whatever each
/// transport needs to route subsequent input / resize / close calls back
/// to its underlying subprocess.
///
/// Variant is the only `pub` field; consumers should not pattern-match the
/// payload and instead route via the trait methods.
#[derive(Debug)]
pub enum TransportHandle {
    /// Tied to a [`crate::terminal::TerminalManager`] terminal id.
    Pty { terminal_id: String },
    /// Tied to a [`crate::claude_session::SessionManager`] CLI session id.
    ClaudeCli { cli_session_id: String },
    /// Workflow runs are managed end-to-end by the workflow executor; the
    /// transport just records the `task_run_id` for log/observation
    /// hookups.
    Workflow { task_run_id: String },
    /// Externally-owned session (plan §Phase 10 dual-write). The real
    /// process is owned by the legacy path; this handle carries the
    /// legacy id purely for cross-referencing in the dashboard. No
    /// transport op touches the underlying process.
    External,
}

/// Errors raised by transports. Boxed so the variant set is open and each
/// transport can carry its native error context without exploding the
/// shared enum.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport rejected intent: {0}")]
    InvalidIntent(String),
    #[error("transport not yet implemented for kind: {0}")]
    Unsupported(&'static str),
    #[error("transport runtime error: {0}")]
    Runtime(String),
}

/// What [`crate::session::Session`] needs from each transport.
///
/// The trait is intentionally narrow: the broad surface (scrollback, grid
/// snapshots, OSC title sync, etc.) lives on each transport's native
/// manager and is reached via the [`TransportHandle`] payload.
///
/// `start` takes `&self` so a single manager-style transport (one Arc held
/// by Tauri state, see [`pty::PtyTransport`]) can spawn many sessions.
pub trait Transport: Send + Sync + 'static {
    /// Spawn the underlying subprocess / runtime for `intent` and return a
    /// [`TransportHandle`] that subsequent ops route through.
    fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError>;

    /// Write input bytes. For [`TransportHandle::Pty`] this is PTY stdin;
    /// for [`TransportHandle::ClaudeCli`] this is a stream-json user
    /// message; workflows do not accept ad-hoc input today.
    fn write_input(&self, handle: &TransportHandle, bytes: &[u8]) -> Result<(), TransportError>;

    /// Resize the underlying terminal. No-op for transports that don't
    /// own a terminal.
    fn resize(&self, handle: &TransportHandle, cols: u16, rows: u16) -> Result<(), TransportError>;

    /// Tear down the transport. Idempotent — calling `close` on an
    /// already-closed handle returns `Ok(())`.
    fn close(&self, handle: &TransportHandle) -> Result<(), TransportError>;

    /// Phase 8 (plan §D10) — subscribe to the session's output stream for
    /// opt-in PTY-output streaming. Returns a broadcast receiver yielding
    /// base64-encoded output chunks (the same stream the runner frontend
    /// renders), or `None` when this transport doesn't expose a tappable
    /// output stream.
    ///
    /// Implemented by every transport whose handle names a real PTY: both
    /// [`pty::PtyTransport`] and [`claude_cli::ClaudeCliTransport`] route
    /// through [`tap_pty_output`], so a `TerminalShell` and a
    /// `TerminalClaude` session stream identically.
    ///
    /// `None` for the transports that own no byte stream at all —
    /// [`workflow::WorkflowTransport`], [`ExternalTransport`], and the
    /// `Agentic` arm of [`claude_cli::ClaudeCliTransport`]; each hands back a
    /// handle that names no tappable process (see each impl's `tap_output`
    /// doc for the evidence). The [`super::output_pipe`] is simply never
    /// spawned for those. The registry only calls this when
    /// `intent.share_output` is true, so a non-shared session never reaches
    /// here (zero overhead off the opt-in path).
    ///
    /// **This method deliberately has NO default body.** It used to default to
    /// `None`, and that default is what produced the gap this seam exists to
    /// close: [`claude_cli::ClaudeCliTransport`] never wrote an impl, silently
    /// inherited `None`, and every `TerminalClaude` session was excluded from
    /// coord's transcript tiers — with nothing in the type system, and nothing
    /// in a test, to notice. A required method converts that whole class of
    /// regression into a compile error: a new transport, or a future edit that
    /// deletes an impl, cannot reach `main` streaming nothing by accident. A
    /// transport that genuinely has no output must now SAY `None` at its own
    /// site, next to the reason.
    fn tap_output(
        &self,
        handle: &TransportHandle,
    ) -> Option<tokio::sync::broadcast::Receiver<String>>;
}

/// Route a [`Transport::tap_output`] call to the PTY-backed output broadcast
/// named by `handle`.
///
/// Shared by [`pty::PtyTransport`] and [`claude_cli::ClaudeCliTransport`]:
/// both spawn their terminal through [`crate::terminal::TerminalManager`] and
/// both hand back a [`TransportHandle::Pty`], so the tap is one lookup in both
/// cases. Keeping it in one place is what stops the two from drifting apart
/// again — `ClaudeCliTransport` shipped with no `tap_output` at all, so every
/// `TerminalClaude` session was silently excluded from the coord output pipe
/// even though its handle named a real, already-tappable terminal.
///
/// `lookup` resolves a terminal id to that terminal's output broadcast
/// ([`crate::terminal::TerminalSession::subscribe_output`]). It is only called
/// for a [`TransportHandle::Pty`]; every other handle short-circuits to `None`
/// without touching the manager.
///
/// Subscribing is non-destructive: `subscribe_output` hands back a fresh
/// receiver on the terminal's existing `broadcast::Sender`, so an added
/// subscriber costs no other consumer a chunk (the producer sends one clone per
/// chunk to the channel — `terminal/session.rs` reader thread — and every
/// receiver sees the same sequence in the same order).
pub(crate) fn tap_pty_output<F>(
    handle: &TransportHandle,
    lookup: F,
) -> Option<tokio::sync::broadcast::Receiver<String>>
where
    F: FnOnce(&str) -> Option<tokio::sync::broadcast::Receiver<String>>,
{
    match handle {
        TransportHandle::Pty { terminal_id } => lookup(terminal_id),
        // No PTY behind any of these: `ClaudeCli` and `Workflow` carry a
        // `pending-…` placeholder id that names no live process, and
        // `External` is a bookkeeping mirror whose real terminal is owned by
        // the legacy path (which attaches its own pipe via
        // `SessionRegistry::attach_output_pipe`).
        TransportHandle::ClaudeCli { .. }
        | TransportHandle::Workflow { .. }
        | TransportHandle::External => None,
    }
}

/// Convenience alias for the dyn-trait form used by the [`super::Session`]
/// lifecycle.
pub type DynTransport = Arc<dyn Transport>;

/// No-op transport for **externally-owned** sessions (plan §Phase 10
/// dual-write). The underlying process — a legacy `terminal_create` PTY
/// or `claude_session` subprocess — is owned and torn down by the legacy
/// path; the coord-native mirror created via
/// [`super::SessionRegistry::register_external`] is pure bookkeeping for
/// the dashboard. All [`Transport`] ops are no-ops: nothing to spawn,
/// nothing to write, nothing to tear down (close is idempotent and never
/// touches the real process — closing the mirror must not kill the
/// operator's actual terminal).
#[derive(Debug, Default)]
pub struct ExternalTransport;

impl Transport for ExternalTransport {
    fn start(&self, _intent: &Intent) -> Result<TransportHandle, TransportError> {
        // register_external never calls start (the real process is
        // already running). Defined for trait completeness; returns an
        // external handle so a misuse is observable rather than panicking.
        Ok(TransportHandle::External)
    }
    fn write_input(&self, _handle: &TransportHandle, _bytes: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
    fn resize(
        &self,
        _handle: &TransportHandle,
        _cols: u16,
        _rows: u16,
    ) -> Result<(), TransportError> {
        Ok(())
    }
    fn close(&self, _handle: &TransportHandle) -> Result<(), TransportError> {
        Ok(())
    }

    /// No tap: an external mirror owns no process. The real terminal belongs
    /// to the legacy path, which attaches its OWN pipe to it via
    /// [`super::SessionRegistry::attach_output_pipe`]
    /// (`commands::terminal::create_terminal_session_backend` and
    /// `commands::productivity::spawn_worker_session` both do this). Tapping
    /// again from here would attach a second pipe to the same terminal and
    /// double-publish every chunk to coord under the same session id.
    fn tap_output(
        &self,
        _handle: &TransportHandle,
    ) -> Option<tokio::sync::broadcast::Receiver<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    fn pty(id: &str) -> TransportHandle {
        TransportHandle::Pty {
            terminal_id: id.to_string(),
        }
    }

    /// The wired path: a PTY handle reaches the lookup with its terminal id
    /// and the resulting receiver is handed straight to
    /// [`crate::session::output_pipe::spawn`].
    #[test]
    fn tap_pty_output_routes_a_pty_handle_to_the_lookup() {
        let (tx, _keep) = broadcast::channel::<String>(8);
        let mut seen: Option<String> = None;
        let rx = tap_pty_output(&pty("term-42"), |id| {
            seen = Some(id.to_string());
            Some(tx.subscribe())
        });
        assert!(rx.is_some(), "a PTY handle must yield an output tap");
        assert_eq!(seen.as_deref(), Some("term-42"));
    }

    /// The pipe consumes chunks in the order the terminal produced them —
    /// the guarantee `terminal/session.rs` states for the SSE broadcast, the
    /// WS relay and (through them) the coord output pipe.
    #[tokio::test]
    async fn tap_pty_output_receiver_yields_chunks_in_order() {
        let (tx, _keep) = broadcast::channel::<String>(8);
        let mut rx = tap_pty_output(&pty("term-1"), |_| Some(tx.subscribe()))
            .expect("PTY handle must yield a tap");

        // Base64-shaped chunks, exactly what `subscribe_output` carries.
        for chunk in ["YQ==", "Yg==", "Yw=="] {
            tx.send(chunk.to_string()).unwrap();
        }

        assert_eq!(rx.recv().await.unwrap(), "YQ==");
        assert_eq!(rx.recv().await.unwrap(), "Yg==");
        assert_eq!(rx.recv().await.unwrap(), "Yw==");
    }

    /// A second subscriber does not cost the first one a chunk — which is why
    /// wiring another transport onto the same terminal broadcast cannot make
    /// an existing consumer lossy.
    #[tokio::test]
    async fn tap_pty_output_is_non_destructive_for_existing_subscribers() {
        let (tx, _keep) = broadcast::channel::<String>(8);
        let mut first = tx.subscribe();
        let mut second = tap_pty_output(&pty("term-1"), |_| Some(tx.subscribe()))
            .expect("PTY handle must yield a tap");

        tx.send("YQ==".to_string()).unwrap();
        tx.send("Yg==".to_string()).unwrap();

        assert_eq!(first.recv().await.unwrap(), "YQ==");
        assert_eq!(second.recv().await.unwrap(), "YQ==");
        assert_eq!(first.recv().await.unwrap(), "Yg==");
        assert_eq!(second.recv().await.unwrap(), "Yg==");
    }

    /// A terminal that has already closed resolves to `None` rather than
    /// panicking — `SessionRegistry::start_inner` then simply spawns no pipe.
    #[test]
    fn tap_pty_output_none_when_the_terminal_is_gone() {
        assert!(tap_pty_output(&pty("closed"), |_| None).is_none());
    }

    // ---- The real transports -------------------------------------------
    //
    // `PtyTransport::new` and `ClaudeCliTransport::new` both require a
    // `tauri::AppHandle` (`pty.rs`, `claude_cli.rs`), and this crate does not
    // enable tauri's `test` feature, so neither can be constructed in-process
    // — and even with a mock app, putting a terminal INTO the
    // `TerminalManager` means `TerminalManager::create`, which spawns a real
    // PTY child. So their positive (`Some`) arm is pinned at COMPILE time
    // instead of by assertion: `Transport::tap_output` has no default body
    // (see the trait), so neither transport can lose its impl without failing
    // to compile, and the impl each one has is a single delegation to
    // `tap_pty_output`, which the tests above cover exhaustively.
    //
    // The transports that need no app handle ARE constructed and asserted on
    // directly, below — their `None` is a decision, and these tests are what
    // make it visible if someone changes it.

    /// A workflow run has no byte stream. Pinned on the real type so the
    /// decision is not silently reversed into a synthesised stream.
    #[test]
    fn workflow_transport_never_taps() {
        let t = workflow::WorkflowTransport::new();
        let handles = [
            TransportHandle::Workflow {
                task_run_id: "pending-abc".to_string(),
            },
            // Even handed a PTY-shaped handle: this transport owns no
            // terminal manager to resolve it against.
            pty("term-1"),
        ];
        for handle in &handles {
            assert!(
                t.tap_output(handle).is_none(),
                "workflow transport must expose no output tap for {:?}",
                handle
            );
        }
    }

    /// An external mirror must not tap: the legacy path that owns the real
    /// terminal attaches its own pipe via `attach_output_pipe`, so a tap here
    /// would double-publish every chunk to coord under the same session id.
    #[test]
    fn external_transport_never_taps() {
        let t = ExternalTransport;
        assert!(t.tap_output(&TransportHandle::External).is_none());
        assert!(
            t.tap_output(&pty("term-1")).is_none(),
            "an external mirror must not tap even a PTY-shaped handle"
        );
    }

    /// The `Agentic` arm of the claude_cli transport: its handle carries a
    /// `pending-<uuid>` placeholder that no code path ever replaces
    /// (`SessionRegistry::link_task_run` does not exist, and
    /// `SessionRecord::transport_handle` is never mutated after
    /// `start_inner`), so there is no id to resolve and the manager must not
    /// even be consulted. This is the arm `ClaudeCliTransport::tap_output`
    /// reaches for a `SessionKind::Agentic` session.
    #[test]
    fn agentic_claude_cli_handle_never_reaches_the_terminal_lookup() {
        let handle = TransportHandle::ClaudeCli {
            cli_session_id: format!("pending-{}", uuid::Uuid::new_v4()),
        };
        let rx = tap_pty_output(&handle, |id| {
            panic!("agentic placeholder id {id} must never be looked up");
        });
        assert!(rx.is_none());
    }

    /// Every non-PTY handle short-circuits without consulting the lookup.
    #[test]
    fn tap_pty_output_none_for_non_pty_handles() {
        let cases = [
            TransportHandle::ClaudeCli {
                cli_session_id: "pending-1".to_string(),
            },
            TransportHandle::Workflow {
                task_run_id: "pending-2".to_string(),
            },
            TransportHandle::External,
        ];
        for handle in cases {
            let rx = tap_pty_output(&handle, |_| {
                panic!("lookup must not run for {:?}", handle);
            });
            assert!(rx.is_none(), "{:?} must not yield an output tap", handle);
        }
    }
}
