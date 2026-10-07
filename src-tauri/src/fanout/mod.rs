//! Prompt-matrix fan-out: N previewed prompts, each a fresh `claude` session,
//! admitted under a concurrency cap (plan
//! `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
//! Phase 6 and Decision 4).
//!
//! Deliberately thin — a queue, a cap and a spawn — and NOT a second task
//! engine: there is no design step, no subtask graph and no completion. A
//! member is an operator-driven PTY tab that lives on after admission; its slot
//! is released by PTY-plane signals the Conductor never reads (`terminal-exit`,
//! the runner-local finished marker) or by an explicit release.
//!
//! * [`model`] — the ledger's value types and pure rules.
//! * [`dispatcher`] — admission under one lock, with the store / host / events
//!   seams as traits.
//! * [`host`] — the production host: the drain-gated spawn through
//!   `commands::terminal::create_tracked_terminal_session_backend`, and the
//!   `fanout-changed` event.
//! * The ledger is `database::pg::fanout`; the HTTP door is `mcp::fanout`.
//!
//! Recovery lives inside the runner process: [`start`] reloads this instance's
//! active runs and reconciles them at boot. A run with no write of any kind for
//! longer than `dispatcher::STALE_RUN_AGE` (24 h) when it is loaded — typically a
//! torn-down temp runner's run, found by a later runner reusing its instance
//! name — is not resumed: its waiting members are cancelled with
//! `stale_after_restart`, and the operator re-creates the run if it is still
//! wanted. A run with a live member, or one paused by coord's device drain, is
//! never treated as stale. Nothing here touches the supervisor.

pub(crate) mod dispatcher;
pub(crate) mod host;
pub(crate) mod model;

use std::sync::Arc;
use std::time::Duration;

use tauri::{Listener, Manager};
use tracing::{info, warn};

/// How often the admission loop ticks with no wake. A wake (a create, PATCH,
/// release, or any `terminal-exit`) ticks immediately.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// Let session restore rebind surviving sessions to their terminals before
/// the restart reconcile judges which admitted members are still alive — the
/// looping-agent supervisor's settle, for the same reason.
const BOOT_SETTLE_DELAY: Duration = Duration::from_secs(45);

/// Build the dispatcher, manage it for the HTTP routes, and start the
/// supervised admission loop.
pub(crate) fn start(app: &tauri::AppHandle, pg: Arc<crate::database::pg::PgDb>) {
    let dispatcher = Arc::new(dispatcher::FanoutDispatcher::new(
        crate::orchestration_loop::loop_engine::run_owner_instance(),
        pg,
        Arc::new(host::TauriFanoutHost { app: app.clone() }),
        Arc::new(host::TauriFanoutEvents { app: app.clone() }),
    ));
    app.manage(dispatcher.clone());

    // A member's terminal exiting frees a slot: tick now rather than at the
    // next interval. The tick itself re-reads liveness, so a missed event costs
    // at most one interval.
    let on_exit = dispatcher.clone();
    app.listen("terminal-exit", move |_event| on_exit.wake());

    // Process-lifetime loop (no shutdown signal), self-healing: a panicking tick
    // respawns the loop with backoff instead of ending admission until the next
    // runner start. `tauri::async_runtime::spawn` enters a runtime context —
    // setup runs on the main thread, outside any reactor.
    tauri::async_runtime::spawn(async move {
        crate::mcp::task_supervisor::spawn_supervised_forever(
            "fanout-dispatcher",
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(60),
            move || run_loop(dispatcher.clone()),
        );
    });
    info!("fanout: dispatcher started");
}

/// Boot-settle, load + reconcile, then tick forever. Runs under
/// `spawn_supervised`, so a panic respawns it — and a respawn re-enters
/// `boot`, which is idempotent.
async fn run_loop(d: Arc<dispatcher::FanoutDispatcher>) {
    tokio::time::sleep(BOOT_SETTLE_DELAY).await;
    loop {
        if let Err(e) = d.boot().await {
            warn!(error = %e, "fanout: could not load active runs — retrying next tick");
        }
        d.tick().await;
        tokio::select! {
            _ = tokio::time::sleep(TICK_INTERVAL) => {}
            _ = d.woken() => {}
        }
    }
}
