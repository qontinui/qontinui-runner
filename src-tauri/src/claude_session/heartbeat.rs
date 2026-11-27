//! Stop rule for the "⏳ AI is working..." heartbeat threads in
//! [`super::runner`] and [`super::session`].
//!
//! Both threads poll a stop channel. The original check,
//! `stop_rx.try_recv().is_ok()`, exited only on an explicit `send(())`. A path
//! that leaves between spawning the thread and sending the stop DROPS the
//! sender instead, and `try_recv` then answers `Err(Disconnected)` forever, so
//! the thread keeps emitting `ai-output` heartbeats for a session that is long
//! gone. In `session.rs` those paths are the initialization-failure
//! `return Err(..)`s that run before `stop_tx` is moved into the
//! `ClaudeSession`; in `runner.rs` it is a panic before the cleanup sends.
//! Observed 2026-11-06 in the runner's `dev-logs/ai-output.jsonl`: two
//! task-runs that ended `failed` on 2026-11-04 were still emitting
//! "AI is working... (3914m 0s)" every 30 s.
//!
//! A dropped sender means no one can ever stop the thread, so it must be read
//! as "stop", exactly like a sent message.

use std::sync::mpsc::{Receiver, TryRecvError};

/// `true` when the heartbeat thread owning `stop_rx` must exit: a stop was
/// sent, or every sender has been dropped.
pub(crate) fn heartbeat_should_stop(stop_rx: &Receiver<()>) -> bool {
    !matches!(stop_rx.try_recv(), Err(TryRecvError::Empty))
}

#[cfg(test)]
mod tests {
    use super::heartbeat_should_stop;
    use std::sync::mpsc;

    #[test]
    fn keeps_running_while_sender_alive_and_silent() {
        let (tx, rx) = mpsc::channel::<()>();
        assert!(!heartbeat_should_stop(&rx));
        drop(tx);
    }

    #[test]
    fn stops_on_explicit_stop() {
        let (tx, rx) = mpsc::channel::<()>();
        tx.send(()).unwrap();
        assert!(heartbeat_should_stop(&rx));
    }

    /// The leak: an early return drops the sender without sending.
    #[test]
    fn stops_when_sender_dropped_without_sending() {
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        assert!(heartbeat_should_stop(&rx));
    }
}
