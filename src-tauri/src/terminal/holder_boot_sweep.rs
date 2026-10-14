//! The runner's half of the PTY-holder boot sweep: at start-up, adopt every
//! pane a holder kept alive across the last runner's exit, and reap the dead.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3 (D8 under D13). The walk itself — classify each pane by handshake
//! and lock, reap a dead holder's PROVEN tree remainder, clean its files — is
//! `qontinui_pty_holder::sweep::boot_sweep`, over this instance's pane
//! directory ([`super::daemon_pane_io::runner_pane_dir`]). What only the
//! runner can do is here: turn each LIVE pane back into a terminal tab, through
//! [`TerminalManager::adopt_holder_pane`] (Phase 2b) — the first production
//! caller of that door.
//!
//! # Identity
//!
//! A holder carries no harness session id (that is Phase 6's OSC-reported
//! `SessionStart` id). The pane id IS the terminal id, and the session
//! lifecycle registry keys its open record by terminal id, so the adoption
//! takes the record's `claude_session_id` as the pinned session id and its
//! `page_id` as the tab's page ([`plan_adoptions`]). That is the id the pane
//! was spawned under: the identity seam records it at spawn. A pane with no
//! open record (a plain shell, or a record lost with the registry) is still
//! adopted — the process is alive and must be visible — under an empty pinned
//! id and the default page, and the log says so.
//!
//! # Ordering against the webview's restore
//!
//! The sweep starts in setup, before the webview loads, and setup waits for
//! it up to [`SETUP_WAIT`]. An adopted pane is then an ordinary live terminal in
//! `terminal_list`, which the frontend's restore reconnects to FIRST and binds
//! to its registry record — so it never also cold-resumes that session (two
//! `claude` processes on one session id). A pane whose attach runs past the
//! wait is still adopted when it lands (a `terminal-created` event); for it
//! the frontend's live-process check (`fetchLiveClaudeSessionIds`, PID
//! verified) is what refuses a duplicate resume.
//!
//! # Gate
//!
//! `terminal.pty_holder` governs whether NEW panes get a holder. The sweep runs
//! whenever the pane directory exists, setting on or off: a holder that is
//! alive is a session that is alive, and turning the setting off must not make
//! it invisible (the census-invisibility failure Risk 2 names). With the
//! setting off and no pane directory — the default — it costs one `stat`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::sweep::{boot_sweep, SweepOutcome, SweepReport};
use tauri::AppHandle;
use tracing::{info, warn};

use super::manager::TerminalManager;
use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// One deadline for classifying every pane (the census's handshakes run
/// concurrently under it).
pub const CENSUS_TIMEOUT: Duration = Duration::from_secs(5);

/// How long setup waits for the whole sweep (census, reaping the dead, and
/// every adoption's attach, concurrently) before it carries on. An attach is
/// bounded by `daemon_pane_io::ATTACH_DEADLINE`; this covers the census plus
/// one of them.
pub const SETUP_WAIT: Duration = Duration::from_secs(16);

/// One pane to adopt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub pane_id: String,
    /// The registry's page for the pane; `None` → the default page.
    pub page_id: Option<String>,
    /// The registry's session id for the pane; `""` when it has no open
    /// record.
    pub pinned_session_id: String,
}

/// The live panes of `report` that `already_open` does not name, each with
/// the identity `open_record` (terminal id → `(session id, page id)`) gives
/// it. Pure, so the identity rule is tested without a sweep.
pub fn plan_adoptions(
    report: &SweepReport,
    already_open: &[String],
    open_record: impl Fn(&str) -> Option<(String, String)>,
) -> Vec<Adoption> {
    report
        .live()
        .filter(|(pane_id, _)| !already_open.iter().any(|t| t == pane_id))
        .map(|(pane_id, _)| {
            let (pinned_session_id, page_id) = match open_record(pane_id) {
                Some((session, page)) => (session, Some(page)),
                None => (String::new(), None),
            };
            Adoption {
                pane_id: pane_id.to_string(),
                page_id,
                pinned_session_id,
            }
        })
        .collect()
}

/// One log line per non-live pane, so a reaped orphan or an unanswering
/// holder is never silent.
fn log_report(pane_dir: &Path, report: &SweepReport) {
    for entry in &report.entries {
        match &entry.outcome {
            SweepOutcome::Live { hello_ack, .. } => info!(
                pane = %entry.pane_id,
                holder_pid = hello_ack.holder_pid,
                child_pid = ?hello_ack.child_pid,
                "pty holder boot sweep: live holder found — adopting"
            ),
            SweepOutcome::Incompatible { holder_versions } => warn!(
                pane = %entry.pane_id,
                ?holder_versions,
                "pty holder boot sweep: a live holder speaks no protocol version this runner \
                 drives — left running, not adopted (plan D15)"
            ),
            SweepOutcome::Unknown { reason, record } => warn!(
                pane = %entry.pane_id,
                holder_pid = ?record.as_ref().map(|r| r.holder_pid),
                reason = %reason,
                "pty holder boot sweep: pane is UNKNOWN (lock held, no answer) — left untouched"
            ),
            SweepOutcome::Reaped {
                record,
                reap,
                lock_removed,
            } => info!(
                pane = %entry.pane_id,
                holder_pid = ?record.as_ref().map(|r| r.holder_pid),
                lock_removed,
                reap = %reap.summary(),
                "pty holder boot sweep: dead holder reaped"
            ),
        }
    }
    for path in &report.strays_removed {
        info!(path = %path.display(), "pty holder boot sweep: stray file removed");
    }
    for err in &report.errors {
        warn!(pane_dir = %pane_dir.display(), error = %err, "pty holder boot sweep: removal failed");
    }
}

/// Sweep this instance's pane directory and adopt every live pane into
/// `manager`, on a thread of its own. Blocks the caller for at most
/// [`SETUP_WAIT`] — the census, the reaping of dead panes and the adoptions
/// all count against it; whatever is still running then finishes on its own.
pub fn run(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    store: Option<Arc<SessionLifecycleStore>>,
) {
    let pane_dir = match super::daemon_pane_io::runner_pane_dir() {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "pty holder boot sweep: no pane directory — skipped");
            return;
        }
    };
    if !pane_dir.exists() {
        return;
    }
    let started = Instant::now();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let (manager, app_handle) = (manager.clone(), app_handle.clone());
    let spawned = std::thread::Builder::new()
        .name("pty-holder-boot-sweep".into())
        .spawn(move || {
            sweep_and_adopt(&manager, &app_handle, store.as_deref(), &pane_dir);
            let _ = done_tx.send(());
        });
    if let Err(e) = spawned {
        warn!(error = %e, "pty holder boot sweep: thread did not start — nothing adopted");
        return;
    }
    if done_rx.recv_timeout(SETUP_WAIT).is_err() {
        warn!(
            waited_ms = started.elapsed().as_millis() as u64,
            "pty holder boot sweep: setup stopped waiting; the sweep finishes in the \
             background and its adoptions land when their attach completes"
        );
    }
}

/// The sweep proper: classify, reap, adopt (concurrently), log.
fn sweep_and_adopt(
    manager: &Arc<TerminalManager>,
    app_handle: &AppHandle,
    store: Option<&SessionLifecycleStore>,
    pane_dir: &Path,
) {
    let started = Instant::now();
    let already_open: Vec<String> = manager.list().into_iter().map(|t| t.id).collect();
    // Never probe a pane this runner already serves (or is spawning into the
    // manager): a probe can steal a starting holder's lock.
    let skip: Vec<PaneId> = already_open
        .iter()
        .filter_map(|id| PaneId::new(id).ok())
        .collect();
    let report = match boot_sweep(pane_dir, CENSUS_TIMEOUT, &skip) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                pane_dir = %pane_dir.display(),
                error = %e,
                "pty holder boot sweep: the pane directory could not be swept — live holders \
                 in it are NOT adopted this run"
            );
            return;
        }
    };
    log_report(pane_dir, &report);
    let adoptions = plan_adoptions(&report, &already_open, |terminal_id| {
        store?
            .find_open_by_terminal(terminal_id)
            .map(|r| (r.claude_session_id, r.page_id))
    });
    let mut threads = Vec::new();
    for adoption in adoptions {
        if adoption.pinned_session_id.is_empty() {
            warn!(
                pane = %adoption.pane_id,
                "pty holder boot sweep: no open registry record for this pane — adopting it \
                 with no pinned session id, on the default page"
            );
        }
        let (manager, app_handle) = (manager.clone(), app_handle.clone());
        // Each adoption is bounded by its attach (`daemon_pane_io::ATTACH_DEADLINE`).
        let thread = std::thread::Builder::new()
            .name(format!("pty-holder-adopt-{}", adoption.pane_id))
            .spawn(move || {
                match manager.adopt_holder_pane(
                    &adoption.pane_id,
                    None,
                    adoption.page_id,
                    None,
                    app_handle,
                    adoption.pinned_session_id,
                ) {
                    Ok(info) => {
                        info!(pane = %info.id, "pty holder boot sweep: pane adopted into a terminal tab");
                        true
                    }
                    Err(e) => {
                        warn!(
                            pane = %adoption.pane_id,
                            error = %e,
                            "pty holder boot sweep: adoption FAILED — the holder keeps running \
                             and is retried by the next start's sweep"
                        );
                        false
                    }
                }
            });
        match thread {
            Ok(t) => threads.push(t),
            Err(e) => warn!(error = %e, "pty holder boot sweep: adoption thread did not start"),
        }
    }
    let adopted = threads
        .into_iter()
        .map(|t| t.join().unwrap_or(false))
        .filter(|ok| *ok)
        .count();
    info!(
        swept = report.entries.len(),
        adopted,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "pty holder boot sweep: done"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_pty_holder::protocol::HelloAck;
    use qontinui_pty_holder::sweep::SweepEntry;

    fn live(id: &str) -> SweepEntry {
        SweepEntry {
            pane_id: id.into(),
            outcome: SweepOutcome::Live {
                hello_ack: HelloAck {
                    version: 3,
                    holder_build: "b".into(),
                    holder_pid: 10,
                    child_pid: Some(11),
                },
                record: None,
            },
        }
    }

    #[test]
    fn pty_holder_boot_sweep_adopts_live_panes_under_their_registry_identity() {
        let report = SweepReport {
            entries: vec![
                live("t-known"),
                live("t-unrecorded"),
                live("t-open-already"),
                SweepEntry {
                    pane_id: "t-dead".into(),
                    outcome: SweepOutcome::Reaped {
                        record: None,
                        reap: Default::default(),
                        lock_removed: true,
                    },
                },
                SweepEntry {
                    pane_id: "t-wedged".into(),
                    outcome: SweepOutcome::Unknown {
                        reason: "x".into(),
                        record: None,
                    },
                },
            ],
            ..Default::default()
        };
        let plans = plan_adoptions(&report, &["t-open-already".to_string()], |id| {
            (id == "t-known").then(|| ("sess-1".to_string(), "page-2".to_string()))
        });
        assert_eq!(
            plans,
            vec![
                Adoption {
                    pane_id: "t-known".into(),
                    page_id: Some("page-2".into()),
                    pinned_session_id: "sess-1".into(),
                },
                Adoption {
                    pane_id: "t-unrecorded".into(),
                    page_id: None,
                    pinned_session_id: String::new(),
                },
            ],
            "only live panes not already open are adopted; dead, unknown and open ones are not"
        );
    }
}
