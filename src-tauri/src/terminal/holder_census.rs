//! The PTY-holder census the restart verdict consumes.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 5 (D9 under D13 and D15). A holder pane's `claude` can leave the
//! runner's process subtree — a holder spawned through WMI, and every holder a
//! restarted runner adopts, descends from something else — and the
//! `/restart-readiness` census walks that subtree. Without this the restart
//! verdict would read "safe" BECAUSE the sessions became invisible (Risk 2).
//!
//! The census is NOT a second classifier. It is
//! `qontinui_pty_holder::client::census` — the same walk and the same
//! two-observable verdict (an answered handshake, then the lock) the Phase 3
//! boot sweep (`super::holder_boot_sweep`, `qontinui_pty_holder::sweep`) runs —
//! minus the sweep's reaping: this read is taken on every `/restart-readiness`
//! request and every background tracking pass, and it changes nothing. What
//! this module adds is only the runner's half: which pane directory, which
//! panes not to probe, and the translation of each row into the
//! [`HolderPane`] the partition consumes (`session::tracking_health`).
//!
//! - **One answered handshake per holder, concurrently, under ONE deadline**
//!   ([`CENSUS_TIMEOUT`]) — D13's census. A pane that has not answered by the
//!   deadline is Unknown, never dropped.
//! - **Fresh, never cached.** [`take`] runs inside
//!   `tracking_health::compute`, which every caller (`/restart-readiness`, the
//!   wind-down tick, the background pass) runs fresh.
//! - **Unknown and incompatible are counted** ([`HolderPaneState`]). A held
//!   lock whose holder does not answer, and a holder with no common protocol
//!   version (D15), are reported as such; the verdict blocks on each.
//! - **A pane this runner is spawning right now is not probed** (a probe can
//!   steal a starting holder's lock — `client::census`'s own warning). It is
//!   reported Unknown ("being spawned by the caller"), and blocks like one.
//! - **An unreadable pane directory is an error, not an empty census**: the
//!   readiness verdict fails closed on it. A pane directory that does not exist
//!   is an empty census — no holder was ever started there.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use qontinui_pty_holder::client::{census_with_versions, CensusEntry, Probe};
use qontinui_pty_holder::lock::{read_record, LockRecord, ENDED_BY_EXIT};
use qontinui_pty_holder::pane::{lock_path, PaneId};
use qontinui_pty_holder::proc_identity::is_same_process;
use qontinui_pty_holder::protocol::PROTOCOL_VERSIONS;

use super::TerminalManager;
use crate::session::tracking_health::{HolderCensus, HolderPane, HolderPaneState};

/// The one deadline every holder's handshake shares. A healthy holder answers
/// in milliseconds; a holder that misses this is reported Unknown, which
/// BLOCKS — so a short deadline can only move a pane toward "unsafe", never
/// toward a false "safe". `/restart-readiness` is polled under short client
/// timeouts, so this is kept well below them.
pub const CENSUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Take this runner instance's holder census, fresh. Never fails: every
/// failure is carried in the returned [`HolderCensus`] (an unreadable pane
/// directory as `error`, an unanswered pane as Unknown).
pub async fn take(manager: &Arc<TerminalManager>) -> HolderCensus {
    // Panes being built right now: probing one can steal its starting
    // holder's lock. Read BEFORE the walk, so a pane that starts building
    // after this read is one whose spec is not yet written — and whose lock
    // does not exist yet either, so the walk cannot see it.
    let skip: Vec<PaneId> = manager
        .in_flight_ids()
        .iter()
        .filter_map(|id| PaneId::new(id).ok())
        .collect();
    let pane_dir = super::daemon_pane_io::runner_pane_dir();
    match tokio::task::spawn_blocking(move || {
        census_in(pane_dir, &skip, CENSUS_TIMEOUT, PROTOCOL_VERSIONS)
    })
    .await
    {
        Ok(census) => census,
        Err(e) => HolderCensus {
            panes: Vec::new(),
            error: Some(format!("the holder census task did not complete: {e}")),
        },
    }
}

/// [`take`]'s body over an explicit pane directory, skip list, deadline and
/// version offer — the seam the tests drive.
///
/// `pane_dir: Err` means this process has no runner data directory at all.
/// Then no holder can have been spawned by it either (`DaemonPaneIo`'s spawn
/// resolves the same directory and falls back to an in-process PTY without
/// one), so the census is empty — not unknown.
pub fn census_in(
    pane_dir: Result<PathBuf, String>,
    skip: &[PaneId],
    timeout: Duration,
    versions: &[u32],
) -> HolderCensus {
    let pane_dir = match pane_dir {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(
                error = %e,
                "holder census: no pane directory resolves — no holder can have been spawned"
            );
            return HolderCensus::default();
        }
    };
    match census_with_versions(&pane_dir, timeout, skip, versions) {
        Ok(rows) => HolderCensus {
            panes: rows
                .into_iter()
                .map(|row| {
                    let lock_file = lock_file_for(&pane_dir, &row.pane_id);
                    let record = move || read_record(&lock_file);
                    pane_from_row(row, record, is_same_process)
                })
                .collect(),
            error: None,
        },
        Err(e) => HolderCensus {
            panes: Vec::new(),
            error: Some(format!(
                "the PTY holder pane directory {} could not be enumerated: {e}",
                pane_dir.display()
            )),
        },
    }
}

fn lock_file_for(pane_dir: &Path, pane_id: &str) -> PathBuf {
    match PaneId::new(pane_id) {
        Ok(p) => lock_path(pane_dir, &p),
        // A foreign stem: the census already named it Unknown; its record (if
        // any) is at the same `<stem>.lock`.
        Err(_) => pane_dir.join(format!("{pane_id}.lock")),
    }
}

/// A recorded pid, kept unless its start token PROVES it is now another
/// process. An absent token (an earlier holder's record) is unverifiable and
/// is kept — fail-closed toward counting, never toward dropping a pane's tree.
fn verified(
    pid: Option<u32>,
    token: Option<&str>,
    same: &impl Fn(u32, Option<&str>) -> bool,
) -> Option<u32> {
    let pid = pid?;
    match token {
        Some(_) if !same(pid, token) => None,
        _ => Some(pid),
    }
}

/// One census row as the partition consumes it. `record` reads the pane's
/// lock record (called only when the row does not carry one); `same` is
/// `proc_identity::is_same_process` in production.
pub fn pane_from_row(
    row: CensusEntry,
    record: impl FnOnce() -> Option<LockRecord>,
    same: impl Fn(u32, Option<&str>) -> bool,
) -> HolderPane {
    let blank = |state: HolderPaneState, detail: Option<String>| HolderPane {
        pane_id: row.pane_id.clone(),
        state,
        holder_pid: None,
        child_pid: None,
        holder_build: None,
        negotiated_version: None,
        holder_versions: Vec::new(),
        detail,
        claude_pids: Vec::new(),
    };
    // The pids an unanswered or incompatible holder's record names, each kept
    // unless its own start token proves it recycled.
    let from_record = |mut pane: HolderPane, rec: Option<LockRecord>| {
        if let Some(r) = rec {
            pane.holder_pid = verified(Some(r.holder_pid), r.holder_start.as_deref(), &same);
            pane.child_pid = verified(r.child_pid, r.child_start.as_deref(), &same);
            pane.holder_build = Some(r.holder_build);
            if pane.holder_versions.is_empty() {
                pane.holder_versions = r.versions;
            }
        }
        pane
    };
    match row.probe.clone() {
        Probe::Healthy { hello_ack } => HolderPane {
            holder_pid: Some(hello_ack.holder_pid),
            child_pid: hello_ack.child_pid,
            holder_build: Some(hello_ack.holder_build),
            negotiated_version: Some(hello_ack.version),
            ..blank(HolderPaneState::Live, None)
        },
        Probe::Incompatible { holder_versions } => {
            let mut pane = blank(
                HolderPaneState::Incompatible,
                Some(format!(
                    "no common protocol version: this runner speaks {PROTOCOL_VERSIONS:?}, the \
                     holder speaks {holder_versions:?} — left running, never killed (D15)"
                )),
            );
            pane.holder_versions = holder_versions;
            from_record(pane, record())
        }
        Probe::Unknown {
            reason,
            record: rec,
        } => {
            let rec = rec.or_else(record);
            from_record(blank(HolderPaneState::Unknown, Some(reason)), rec)
        }
        Probe::Dead { record: rec } => {
            let rec = rec.or_else(record);
            let mut pane = blank(
                HolderPaneState::Dead,
                Some("the pane's lock is free: its holder is dead".to_string()),
            );
            if let Some(r) = rec {
                // The holder is dead; only its child can still be running.
                // One that exited on its own (the holder recorded so under its
                // lock) is gone, and its pid may already be someone else's.
                if r.ended.as_deref() != Some(ENDED_BY_EXIT) {
                    pane.child_pid = verified(r.child_pid, r.child_start.as_deref(), &same);
                }
                pane.holder_build = Some(r.holder_build);
                pane.holder_versions = r.versions;
            }
            pane
        }
        // `census` walks lock files, so it never reports Absent. Should it
        // ever, the pane is still a row: counted, as unknown.
        Probe::Absent => blank(
            HolderPaneState::Unknown,
            Some("the census reported no lock file for a lock file it walked".to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_pty_holder::protocol::HelloAck;

    fn rec(holder_pid: u32, child_pid: Option<u32>) -> LockRecord {
        LockRecord {
            holder_pid,
            child_pid,
            versions: vec![1, 2, 3],
            started_at_unix_ms: 1,
            holder_build: "b".into(),
            holder_start: Some(format!("h{holder_pid}")),
            ended: None,
            child_start: child_pid.map(|c| format!("c{c}")),
            scope_unit: None,
        }
    }

    /// The tokens `rec` writes are "h<pid>" / "c<pid>": a process is the same
    /// one iff its token matches.
    fn same_as_recorded(pid: u32, token: Option<&str>) -> bool {
        token.is_some_and(|t| t.get(1..) == Some(pid.to_string().as_str()))
    }

    fn row(pane_id: &str, probe: Probe) -> CensusEntry {
        CensusEntry {
            pane_id: pane_id.into(),
            probe,
        }
    }

    #[test]
    fn pty_holder_census_maps_every_probe_verdict_to_a_counted_pane() {
        let live = pane_from_row(
            row(
                "a",
                Probe::Healthy {
                    hello_ack: HelloAck {
                        version: 3,
                        holder_build: "b".into(),
                        holder_pid: 10,
                        child_pid: Some(11),
                    },
                },
            ),
            || panic!("a live holder's record is not read"),
            same_as_recorded,
        );
        assert_eq!(live.state, HolderPaneState::Live);
        assert_eq!((live.holder_pid, live.child_pid), (Some(10), Some(11)));
        assert_eq!(live.negotiated_version, Some(3));
        assert!(!live.is_unresolved());

        let unknown = pane_from_row(
            row(
                "b",
                Probe::Unknown {
                    reason: "lock held but handshake unanswered".into(),
                    record: Some(rec(20, Some(21))),
                },
            ),
            || None,
            same_as_recorded,
        );
        assert_eq!(unknown.state, HolderPaneState::Unknown);
        assert_eq!(
            (unknown.holder_pid, unknown.child_pid),
            (Some(20), Some(21))
        );
        assert!(unknown.is_unresolved());

        // Incompatible carries no record in its probe: the record is read.
        let incompatible = pane_from_row(
            row(
                "c",
                Probe::Incompatible {
                    holder_versions: vec![9],
                },
            ),
            || Some(rec(30, Some(31))),
            same_as_recorded,
        );
        assert_eq!(incompatible.state, HolderPaneState::Incompatible);
        assert_eq!(incompatible.holder_versions, vec![9]);
        assert_eq!(incompatible.holder_pid, Some(30));
        assert!(incompatible.is_unresolved());
        assert!(incompatible.detail.unwrap().contains("[9]"));

        // An unknown with NO record at all is still a pane, still unresolved.
        let bare = pane_from_row(
            row(
                "d",
                Probe::Unknown {
                    reason: "census deadline elapsed".into(),
                    record: None,
                },
            ),
            || None,
            same_as_recorded,
        );
        assert_eq!(bare.state, HolderPaneState::Unknown);
        assert_eq!((bare.holder_pid, bare.child_pid), (None, None));
        assert!(bare.is_unresolved());
    }

    #[test]
    fn pty_holder_census_drops_only_a_pid_its_token_proves_recycled() {
        // Recorded tokens that no longer match: both pids are someone else's.
        let recycled = pane_from_row(
            row(
                "a",
                Probe::Unknown {
                    reason: "x".into(),
                    record: Some(rec(20, Some(21))),
                },
            ),
            || None,
            |_, _| false,
        );
        assert_eq!((recycled.holder_pid, recycled.child_pid), (None, None));
        // ...but the pane itself is still counted.
        assert!(recycled.is_unresolved());

        // No token at all (an earlier holder's record): kept, fail-closed.
        let mut old = rec(20, Some(21));
        old.holder_start = None;
        old.child_start = None;
        let kept = pane_from_row(
            row(
                "b",
                Probe::Unknown {
                    reason: "x".into(),
                    record: Some(old),
                },
            ),
            || None,
            |_, _| false,
        );
        assert_eq!((kept.holder_pid, kept.child_pid), (Some(20), Some(21)));
    }

    #[test]
    fn pty_holder_census_dead_pane_keeps_only_a_child_that_may_still_run() {
        let dead = |r: LockRecord, same: fn(u32, Option<&str>) -> bool| {
            pane_from_row(row("a", Probe::Dead { record: Some(r) }), || None, same)
        };
        let running = dead(rec(20, Some(21)), same_as_recorded);
        assert_eq!(running.state, HolderPaneState::Dead);
        assert_eq!(running.holder_pid, None, "the holder is dead");
        assert_eq!(running.child_pid, Some(21));
        assert!(!running.is_unresolved());

        let mut exited = rec(20, Some(21));
        exited.ended = Some(ENDED_BY_EXIT.to_string());
        assert_eq!(dead(exited, same_as_recorded).child_pid, None);

        assert_eq!(dead(rec(20, Some(21)), |_, _| false).child_pid, None);
    }

    #[test]
    fn pty_holder_census_no_pane_dir_is_empty_but_an_unreadable_one_is_an_error() {
        let none = census_in(
            Err("no home".into()),
            &[],
            CENSUS_TIMEOUT,
            PROTOCOL_VERSIONS,
        );
        assert_eq!(none, HolderCensus::default());

        let missing = std::env::temp_dir().join(format!(
            "ptyc-missing-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let empty = census_in(Ok(missing), &[], CENSUS_TIMEOUT, PROTOCOL_VERSIONS);
        assert!(empty.panes.is_empty() && empty.error.is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ptyc-open-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            let refused = census_in(Ok(dir.clone()), &[], CENSUS_TIMEOUT, PROTOCOL_VERSIONS);
            let _ = std::fs::remove_dir_all(&dir);
            assert!(refused.panes.is_empty());
            assert!(
                refused.error.is_some(),
                "a pane dir others can write to is UNKNOWN, never an empty census"
            );
        }
    }

    /// End to end against the real `qontinui-pty-holder` (Linux; the Windows
    /// named-pipe arm is UNRUN here): one live holder, one held lock with no
    /// endpoint, and the same live holder offered a version it does not speak
    /// — live, unknown and incompatible, each a counted pane.
    #[cfg(target_os = "linux")]
    mod holder_e2e {
        use super::*;
        use crate::terminal::daemon_pane_io::DaemonPaneIo;
        use crate::terminal::pane_io::{PaneIo, ScrubbedCommand};
        use qontinui_pty_holder::lock::{PaneLock, TryLock};
        use qontinui_pty_holder::pane::prepare_private_dir;

        fn holder() -> PathBuf {
            let exe = std::env::current_exe().unwrap();
            let profile = exe.parent().and_then(Path::parent).unwrap();
            let p = profile.join("qontinui-pty-holder");
            assert!(
                p.is_file(),
                "{} is missing — build it first: `cargo build -p qontinui-pty-holder`",
                p.display()
            );
            p
        }

        #[test]
        fn pty_holder_census_counts_live_unknown_and_incompatible_holders() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            let dir = PathBuf::from("/tmp").join(format!("ptyc-{}-{nanos:x}", std::process::id()));
            prepare_private_dir(&dir).unwrap();

            let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
            cmd.args(["-c", "exec sleep 30"]);
            cmd.cwd("/tmp");
            let live_id = PaneId::new("census-live").unwrap();
            let pane = DaemonPaneIo::spawn(
                &holder(),
                &dir,
                &live_id,
                &ScrubbedCommand::seal(cmd),
                80,
                24,
            )
            .expect("spawn a real holder");

            // A held lock whose "holder" never binds an endpoint: Unknown.
            let wedged = PaneId::new("census-wedged").unwrap();
            let held = match PaneLock::try_acquire(&lock_path(&dir, &wedged)).unwrap() {
                TryLock::Acquired(l) => l,
                TryLock::Held => panic!("fresh lock held"),
            };

            let census = census_in(
                Ok(dir.clone()),
                &[],
                Duration::from_secs(5),
                PROTOCOL_VERSIONS,
            );
            assert_eq!(census.error, None);
            let state = |id: &str| {
                census
                    .panes
                    .iter()
                    .find(|p| p.pane_id == id)
                    .map(|p| p.state)
            };
            assert_eq!(state("census-live"), Some(HolderPaneState::Live));
            assert_eq!(state("census-wedged"), Some(HolderPaneState::Unknown));
            let live = census
                .panes
                .iter()
                .find(|p| p.pane_id == "census-live")
                .unwrap();
            assert_eq!(live.child_pid, pane.pid());
            assert!(live.holder_pid.is_some());

            // The same live holder, offered only a version it does not speak.
            let foreign = census_in(Ok(dir.clone()), &[], Duration::from_secs(5), &[999]);
            let foreign_live = foreign
                .panes
                .iter()
                .find(|p| p.pane_id == "census-live")
                .unwrap();
            assert_eq!(foreign_live.state, HolderPaneState::Incompatible);
            assert!(foreign_live.is_unresolved());
            assert_eq!(
                foreign_live.holder_pid, live.holder_pid,
                "an incompatible holder's pids come from its lock record"
            );

            // A pane the caller is spawning is not probed — and still counted.
            let skipped = census_in(
                Ok(dir.clone()),
                std::slice::from_ref(&live_id),
                Duration::from_secs(5),
                PROTOCOL_VERSIONS,
            );
            let s = skipped
                .panes
                .iter()
                .find(|p| p.pane_id == "census-live")
                .unwrap();
            assert_eq!(s.state, HolderPaneState::Unknown);

            drop(held);
            pane.kill(Duration::from_secs(5)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
