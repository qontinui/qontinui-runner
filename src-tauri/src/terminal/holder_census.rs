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
//!   steal a starting holder's lock — `client::census`'s own warning). The
//!   in-flight set is read inside the walk, after the directory is
//!   enumerated; such a pane is reported Unknown ("being spawned by the
//!   caller") and blocks like one. A pane that starts spawning after the walk
//!   is added by [`add_in_flight`], which the caller runs after its process
//!   snapshot (review F2).
//! - **Identity, not boot time, for an adopted pane** (review F1):
//!   [`verified_holder_children`] proves a live terminal pid is its holder's
//!   recorded child by start token, so the PID-reuse guard does not call a
//!   pane adopted after a restart "recycled".
//! - **An unreadable pane directory is an error, not an empty census**: the
//!   readiness verdict fails closed on it. A pane directory that does not exist
//!   is an empty census — no holder was ever started there.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use qontinui_pty_holder::client::{census_with_skip, CensusEntry, Probe};
use qontinui_pty_holder::lock::{read_record, LockRecord, ENDED_BY_EXIT};
use qontinui_pty_holder::pane::{lock_path, PaneId};
use qontinui_pty_holder::proc_identity::is_same_process;
use qontinui_pty_holder::protocol::PROTOCOL_VERSIONS;

use super::TerminalManager;
use crate::session::tracking_health::{
    HolderCensus, HolderPane, HolderPaneState, HolderProtection, PaneHandoff,
};

/// The one deadline every holder's handshake shares. A healthy holder answers
/// in milliseconds; a holder that misses this is reported Unknown, which
/// BLOCKS — so a short deadline can only move a pane toward "unsafe", never
/// toward a false "safe". `/restart-readiness` is polled under short client
/// timeouts, so this is kept well below them.
pub const CENSUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Take this runner instance's holder census, fresh. Never fails: every
/// failure is carried in the returned [`HolderCensus`] (an unreadable pane
/// directory as `error`, an unanswered pane as Unknown).
///
/// The caller must still run [`add_in_flight`] AFTER its process snapshot:
/// a pane that starts spawning after the walk is not in it.
pub async fn take(manager: &Arc<TerminalManager>) -> HolderCensus {
    take_in(
        super::daemon_pane_io::runner_pane_dir(),
        manager.clone(),
        CENSUS_TIMEOUT,
    )
    .await
}

/// [`take`] over an explicit pane directory and deadline — the seam the tests
/// drive with a real [`TerminalManager`].
pub async fn take_in(
    pane_dir: Result<PathBuf, String>,
    manager: Arc<TerminalManager>,
    timeout: Duration,
) -> HolderCensus {
    // The live terminals' pids, for the identity proof (review F1). Cloned
    // out; no manager lock is held across the census.
    let terminals: Vec<(String, u32)> = manager
        .list()
        .into_iter()
        .filter_map(|t| t.pid.filter(|&p| p > 0).map(|p| (t.id, p)))
        .collect();
    let joined = tokio::task::spawn_blocking(move || {
        // Review F2: the skip set (panes being built) is read INSIDE the
        // walk, after the pane directory is enumerated and before any pane is
        // probed (`census_with_skip`), so a pane whose lock the walk saw and
        // that is being spawned right now is never probed. A pane that began
        // building after that read had no lock file when the directory was
        // enumerated; [`add_in_flight`] accounts for it after the snapshot.
        let skip_manager = manager.clone();
        let mut census = census_in(
            pane_dir.clone(),
            move || in_flight_panes(&skip_manager),
            timeout,
            PROTOCOL_VERSIONS,
        );
        if let Ok(dir) = &pane_dir {
            census.verified_terminal_pids =
                verified_holder_children_in(dir, &terminals, is_same_process);
        }
        census
    })
    .await;
    match joined {
        Ok(census) => census,
        Err(e) => HolderCensus {
            error: Some(format!("the holder census task did not complete: {e}")),
            ..HolderCensus::default()
        },
    }
}

fn in_flight_panes(manager: &TerminalManager) -> Vec<PaneId> {
    manager
        .in_flight_ids()
        .iter()
        .filter_map(|id| PaneId::new(id).ok())
        .collect()
}

/// Add a counted, unprobed `Unknown` row for every terminal this runner is
/// building right now (`in_flight`, read AFTER the caller's process
/// snapshot) that the census has no row for. Review F2: a pane that began
/// spawning after the census walked the pane directory — on Windows possibly
/// under a WMI-spawned holder outside the runner's subtree, so the snapshot
/// cannot see its `claude` either — is otherwise in neither source.
///
/// Every in-flight id is added, holder-backed or not: which backend a pane
/// being built will get is not knowable from here, and the window is the
/// seconds a spawn takes. Over-counting a tab that is opening is the
/// fail-closed direction.
pub fn add_in_flight(census: &mut HolderCensus, in_flight: &[String]) {
    for id in in_flight {
        if census.panes.iter().any(|p| &p.pane_id == id) {
            continue;
        }
        census.panes.push(HolderPane {
            pane_id: id.clone(),
            state: HolderPaneState::Unknown,
            holder_pid: None,
            child_pid: None,
            holder_build: None,
            negotiated_version: None,
            holder_versions: Vec::new(),
            detail: Some(
                "being spawned by this runner (in flight) — not probed, counted".to_string(),
            ),
            claude_pids: Vec::new(),
            protection: HolderProtection::Unknown,
            handoff: PaneHandoff::Unknown,
            handoff_reason: None,
        });
        if let Some(p) = census.panes.last_mut() {
            p.refresh_handoff();
        }
    }
    census.panes.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
}

/// Review F1: the live terminal pids PROVEN to be their holder's recorded
/// child — the pane's lock record names that pid as `child_pid`, and the
/// record's `child_start` token matches the live process
/// (`proc_identity::is_same_process`, passed as `same`). A restarted runner's
/// adopted pane has a child that predates this boot; this proof is what lets
/// the PID-reuse guard tell it from a recycled pid
/// (`tracking_health::pid_reuse_reference`).
///
/// A record without a token (an older holder), a pid the record does not
/// name, or a token that does not match proves nothing, and the pid keeps the
/// boot-time guard. A pane dir that is not private is never trusted.
pub fn verified_holder_children_in(
    pane_dir: &Path,
    terminals: &[(String, u32)],
    same: impl Fn(u32, Option<&str>) -> bool,
) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    if qontinui_pty_holder::pane::check_private_dir(pane_dir).is_err() {
        return out;
    }
    for (id, pid) in terminals {
        let Ok(pane) = PaneId::new(id) else { continue };
        let Some(record) = read_record(&lock_path(pane_dir, &pane)) else {
            continue;
        };
        if record.child_pid == Some(*pid)
            && record.child_start.is_some()
            && same(*pid, record.child_start.as_deref())
        {
            out.insert(*pid);
        }
    }
    out
}

/// [`verified_holder_children_in`] over this runner instance's pane
/// directory. Empty when none resolves (no holder can exist then).
pub fn verified_holder_children(terminals: &[(String, u32)]) -> std::collections::HashSet<u32> {
    match super::daemon_pane_io::runner_pane_dir() {
        Ok(dir) => verified_holder_children_in(&dir, terminals, is_same_process),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// [`take`]'s walk over an explicit pane directory, skip source, deadline and
/// version offer — the seam the tests drive. `skip` is read after the
/// directory is enumerated (`census_with_skip`).
///
/// `pane_dir: Err` means this process has no runner data directory at all.
/// Then no holder can have been spawned by it either (`DaemonPaneIo`'s spawn
/// resolves the same directory and falls back to an in-process PTY without
/// one), so the census is empty — not unknown.
pub fn census_in(
    pane_dir: Result<PathBuf, String>,
    skip: impl FnOnce() -> Vec<PaneId>,
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
    match census_with_skip(&pane_dir, timeout, skip, versions) {
        Ok(rows) => HolderCensus {
            panes: rows
                .into_iter()
                .map(|row| {
                    let lock_file = lock_file_for(&pane_dir, &row.pane_id);
                    let record = move || read_record(&lock_file);
                    let mut pane = pane_from_row(row, record, is_same_process);
                    observe_protection(&mut pane, holder_protection);
                    pane
                })
                .collect(),
            ..HolderCensus::default()
        },
        Err(e) => HolderCensus {
            error: Some(format!(
                "the PTY holder pane directory {} could not be enumerated: {e}",
                pane_dir.display()
            )),
            ..HolderCensus::default()
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
        protection: HolderProtection::Unknown,
        handoff: PaneHandoff::Unknown,
        handoff_reason: None,
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
    let mut pane = match row.probe.clone() {
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
    };
    pane.refresh_handoff();
    pane
}

/// Fill a census row's [`HolderProtection`] from its LIVE holder (`observe`
/// is [`holder_protection`] in production) and re-derive its handoff verdict.
/// A dead holder has nothing to observe: its protection stays `unknown`, and
/// its verdict is decided by its child alone.
pub fn observe_protection(pane: &mut HolderPane, observe: impl FnOnce(u32) -> HolderProtection) {
    pane.protection = match (pane.state, pane.holder_pid) {
        (HolderPaneState::Dead, _) | (_, None) => HolderProtection::Unknown,
        (_, Some(pid)) => observe(pid),
    };
    pane.refresh_handoff();
}

/// Whether holder `pid` is protected from the runner's own stop — the
/// spawner's `Unprotected::Cgroup` (`qontinui_pty_holder::spawn`), OBSERVED
/// from the live holder rather than remembered from its spawn, so a pane this
/// runner adopted after a restart carries it too and nothing is persisted.
///
/// Linux: [`protection_from_cgroup`] over `/proc/<pid>/cgroup`; an unreadable
/// one is `unknown`. Elsewhere: `protected` by spawn-route construction — no
/// Windows or macOS route leaves a holder inside the runner's kill domain
/// (the spawner never reports `Unprotected` there).
pub fn holder_protection(pid: u32) -> HolderProtection {
    #[cfg(target_os = "linux")]
    {
        protection_from_cgroup(
            std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .ok()
                .as_deref(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        HolderProtection::Protected
    }
}

/// Classify a `/proc/<pid>/cgroup` body (`None`: unreadable). Fail-closed:
/// anything not parseable is `unknown`, never `protected`.
///
/// - the deepest unit is the holder's own `qontinui-pty-holder-*.scope` (the
///   scope route) → `protected`;
/// - otherwise inside a unit whose stop the runner can trigger
///   (`spawn::cgroup_is_in_systemd_unit`, the spawner's own test) →
///   `unprotected_cgroup`;
/// - otherwise (no unit, a login session, the user manager) → `protected`.
pub fn protection_from_cgroup(cgroup_file: Option<&str>) -> HolderProtection {
    use qontinui_pty_holder::spawn::{cgroup_is_in_systemd_unit, HOLDER_UNIT_PREFIX};
    let Some(body) = cgroup_file else {
        return HolderProtection::Unknown;
    };
    let paths: Vec<&str> = body
        .lines()
        .filter_map(|l| l.splitn(3, ':').nth(2))
        .collect();
    if paths.is_empty() {
        return HolderProtection::Unknown;
    }
    let own_scope = paths.iter().any(|path| {
        path.split('/')
            .rfind(|c| c.ends_with(".service") || c.ends_with(".scope"))
            .is_some_and(|u| u.starts_with(HOLDER_UNIT_PREFIX) && u.ends_with(".scope"))
    });
    if own_scope {
        HolderProtection::Protected
    } else if cgroup_is_in_systemd_unit(body) {
        HolderProtection::UnprotectedCgroup
    } else {
        HolderProtection::Protected
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
            Vec::new,
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
        let empty = census_in(Ok(missing), Vec::new, CENSUS_TIMEOUT, PROTOCOL_VERSIONS);
        assert!(empty.panes.is_empty() && empty.error.is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("ptyc-open-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            let refused = census_in(Ok(dir.clone()), Vec::new, CENSUS_TIMEOUT, PROTOCOL_VERSIONS);
            let _ = std::fs::remove_dir_all(&dir);
            assert!(refused.panes.is_empty());
            assert!(
                refused.error.is_some(),
                "a pane dir others can write to is UNKNOWN, never an empty census"
            );
        }
    }

    /// Review F1: a terminal pid is proven to be its holder's child only when
    /// the lock record names that pid AND its `child_start` token matches the
    /// live process. A mismatched token, a different pid, or no token proves
    /// nothing.
    #[cfg(unix)]
    #[test]
    fn pty_holder_census_identity_proof_needs_the_recorded_pid_and_a_matching_token() {
        use qontinui_pty_holder::lock::{PaneLock, TryLock};
        let dir = std::env::temp_dir().join(format!(
            "ptyv-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        qontinui_pty_holder::pane::prepare_private_dir(&dir).unwrap();
        let write = |id: &str, r: LockRecord| {
            let pane = PaneId::new(id).unwrap();
            match PaneLock::try_acquire(&lock_path(&dir, &pane)).unwrap() {
                TryLock::Acquired(mut l) => l.write_record(&r).unwrap(),
                TryLock::Held => panic!("held"),
            }
        };
        write("a", rec(20, Some(21)));
        let mut tokenless = rec(30, Some(31));
        tokenless.child_start = None;
        write("b", tokenless);
        write("c", rec(40, Some(41)));
        let terminals = vec![
            ("a".to_string(), 21),
            ("b".to_string(), 31),
            ("c".to_string(), 99), // the record names 41, not 99
            ("absent".to_string(), 51),
        ];
        let proven = verified_holder_children_in(&dir, &terminals, same_as_recorded);
        assert_eq!(proven, [21].into_iter().collect());
        // The same records, but every token mismatches: nothing is proven.
        let none = verified_holder_children_in(&dir, &terminals, |_, _| false);
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D4/D6: a holder's protection is read from its cgroup, fail-closed —
    /// its own holder scope is protected, a shared unit cgroup is
    /// `unprotected_cgroup`, and an unreadable or unparseable body is
    /// `unknown`, never protected.
    #[test]
    fn pty_holder_protection_reads_the_holders_cgroup_fail_closed() {
        let own_scope = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/\
                         qontinui-pty-holder-p1-1-2.scope\n";
        assert_eq!(
            protection_from_cgroup(Some(own_scope)),
            HolderProtection::Protected
        );
        let in_runner_unit = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/\
                              qontinui-runner.service\n";
        assert_eq!(
            protection_from_cgroup(Some(in_runner_unit)),
            HolderProtection::UnprotectedCgroup
        );
        let login_session = "0::/user.slice/user-1000.slice/session-3.scope\n";
        assert_eq!(
            protection_from_cgroup(Some(login_session)),
            HolderProtection::Protected
        );
        assert_eq!(protection_from_cgroup(None), HolderProtection::Unknown);
        assert_eq!(protection_from_cgroup(Some("")), HolderProtection::Unknown);
        assert_eq!(
            protection_from_cgroup(Some("garbage")),
            HolderProtection::Unknown
        );
    }

    /// The census fills protection only from a LIVE holder pid, and the
    /// handoff verdict follows it.
    #[test]
    fn pty_holder_census_observes_protection_and_derives_the_handoff() {
        use crate::session::tracking_health::HandoffReason;
        let live = || {
            pane_from_row(
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
                || None,
                same_as_recorded,
            )
        };
        let unobserved = live();
        assert_eq!(unobserved.protection, HolderProtection::Unknown);
        assert_eq!(unobserved.handoff, PaneHandoff::Unknown);

        let mut kept = live();
        observe_protection(&mut kept, |pid| {
            assert_eq!(pid, 10);
            HolderProtection::Protected
        });
        assert_eq!(
            (kept.handoff, kept.handoff_reason),
            (PaneHandoff::Survivable, None)
        );

        let mut doomed = live();
        observe_protection(&mut doomed, |_| HolderProtection::UnprotectedCgroup);
        assert_eq!(
            (doomed.handoff, doomed.handoff_reason),
            (PaneHandoff::Killed, Some(HandoffReason::UnprotectedCgroup))
        );

        let mut dead = pane_from_row(
            row(
                "d",
                Probe::Dead {
                    record: Some(rec(20, Some(21))),
                },
            ),
            || None,
            same_as_recorded,
        );
        observe_protection(&mut dead, |_| panic!("a dead holder is not observed"));
        assert_eq!(dead.protection, HolderProtection::Unknown);
        assert_eq!(dead.handoff, PaneHandoff::Killed);
    }

    /// Review F2: every terminal still being built after the snapshot is a
    /// counted Unknown row — added once, never duplicating a census row.
    #[test]
    fn pty_holder_census_in_flight_panes_are_counted_never_dropped() {
        let mut census = HolderCensus {
            panes: vec![pane_from_row(
                row(
                    "seen",
                    Probe::Unknown {
                        reason: "x".into(),
                        record: None,
                    },
                ),
                || None,
                same_as_recorded,
            )],
            ..HolderCensus::default()
        };
        add_in_flight(&mut census, &["seen".to_string(), "new-tab".to_string()]);
        assert_eq!(census.panes.len(), 2);
        let added = census
            .panes
            .iter()
            .find(|p| p.pane_id == "new-tab")
            .unwrap();
        assert_eq!(added.state, HolderPaneState::Unknown);
        assert!(added.is_unresolved());
        assert!(added.detail.as_deref().unwrap().contains("in flight"));
    }

    /// Review F5: [`take_in`] against a real [`TerminalManager`] with an
    /// in-flight id. The pane's lock exists (its holder is starting), so the
    /// walk sees it — and the skip set read inside the walk keeps it from
    /// being probed: it is reported "not probed: being spawned", counted.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pty_holder_census_take_skips_and_counts_an_in_flight_pane() {
        use qontinui_pty_holder::lock::{PaneLock, TryLock};
        let dir = std::env::temp_dir().join(format!(
            "ptyt-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        qontinui_pty_holder::pane::prepare_private_dir(&dir).unwrap();
        let starting = PaneId::new("starting").unwrap();
        let _held = match PaneLock::try_acquire(&lock_path(&dir, &starting)).unwrap() {
            TryLock::Acquired(l) => l,
            TryLock::Held => panic!("held"),
        };
        let manager = Arc::new(TerminalManager::new());
        manager.mark_in_flight_for_test("starting");
        manager.mark_in_flight_for_test("no-lock-yet");

        let mut census = take_in(Ok(dir.clone()), manager.clone(), Duration::from_secs(2)).await;
        assert_eq!(census.error, None);
        let starting_row = census
            .panes
            .iter()
            .find(|p| p.pane_id == "starting")
            .unwrap();
        assert_eq!(starting_row.state, HolderPaneState::Unknown);
        assert!(
            starting_row
                .detail
                .as_deref()
                .unwrap()
                .contains("not probed"),
            "{:?}",
            starting_row.detail
        );
        // The pane with no lock yet is invisible to the walk...
        assert!(census.panes.iter().all(|p| p.pane_id != "no-lock-yet"));
        // ...and is counted once the caller adds the in-flight set.
        add_in_flight(&mut census, &manager.in_flight_ids());
        assert!(census
            .panes
            .iter()
            .any(|p| p.pane_id == "no-lock-yet" && p.is_unresolved()));
        assert_eq!(census.panes.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
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
                Vec::new,
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
            let foreign = census_in(Ok(dir.clone()), Vec::new, Duration::from_secs(5), &[999]);
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
                || vec![live_id.clone()],
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
