//! Admission control for CI dispatches — defer-not-reject (the
//! `ContinuationGuard::AtCap` idiom from `agent_runtime`):
//!
//! - HARD reject (POST `cancelled` + reason): ci_node disabled, repo not in
//!   the local allowlist, unsafe identifiers, missing repo checkout, disk
//!   below the floor. These can't succeed by waiting.
//! - DEFER (hold in a FIFO queue, re-admit when a slot frees): at the
//!   concurrency cap. The queue is drained by the build-finished hook —
//!   the capacity-freed re-poll pattern.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{reporting, CiDispatchPayload};
use crate::settings::CiNodeSettings;

/// Pure admission verdict. `Reject` carries the operator-readable reason
/// that lands in the result summary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Proceed,
    Defer,
    Reject(String),
}

/// Pure admission decision over injected inputs (no globals — unit-tested
/// without settings files or live state).
pub(crate) fn admission_decision(
    settings: &CiNodeSettings,
    repo: &str,
    running_count: usize,
) -> Admission {
    if !settings.enabled {
        return Admission::Reject("ci_node is disabled on this device".to_string());
    }
    if !repo_allowed(&settings.repo_allowlist, repo) {
        return Admission::Reject(format!(
            "repo {repo:?} is not in this device's ci_node.repo_allowlist"
        ));
    }
    if running_count >= settings.max_concurrent_builds.max(1) as usize {
        return Admission::Defer;
    }
    Admission::Proceed
}

/// Allowlist match: an entry equals the full slug (`owner/name`) or the
/// bare basename.
pub(crate) fn repo_allowed(allowlist: &[String], repo: &str) -> bool {
    let basename = crate::agent_runtime::local_repo_name(repo);
    allowlist
        .iter()
        .any(|entry| entry == repo || entry == basename)
}

/// Pick the free space (GiB) of the volume holding `root` from a
/// `(mount_point, available_bytes)` list — longest matching mount wins.
/// Pure for tests; the live caller feeds it `sysinfo::Disks`.
pub(crate) fn pick_volume_free_gb(mounts: &[(PathBuf, u64)], root: &Path) -> Option<u64> {
    mounts
        .iter()
        .filter(|(mount, _)| root.starts_with(mount))
        .max_by_key(|(mount, _)| mount.as_os_str().len())
        .map(|(_, avail)| avail / (1024 * 1024 * 1024))
}

/// Live free-disk probe for the QONTINUI_ROOT volume. `None` when the
/// volume can't be resolved — the caller fails OPEN with a warning (a
/// telemetry gap must not brick the lane; the 20 GiB floor is a guard, not
/// a security boundary).
fn free_disk_gb_for(root: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mounts: Vec<(PathBuf, u64)> = disks
        .list()
        .iter()
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
        .collect();
    pick_volume_free_gb(&mounts, root)
}

/// Minimum available RAM (GiB) to START a build (plan §4.6: "minimum free
/// RAM" alongside the disk floor). Fixed rather than a setting — 4 GiB is
/// well under any machine that can build these workspaces at all, so the
/// gate only trips when the box is genuinely starved and one more rustc
/// would push it into OOM/thrash territory.
pub(crate) const MIN_FREE_RAM_GB: u64 = 4;

/// `true` when available RAM is below the floor. Pure over injected bytes.
pub(crate) fn ram_below_floor(available_bytes: u64, floor_gb: u64) -> bool {
    available_bytes / (1024 * 1024 * 1024) < floor_gb
}

/// Live available-RAM probe. `None` degrades to fail-open like the disk
/// probe (a telemetry gap must not brick the lane).
fn available_ram_bytes() -> Option<u64> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let avail = sys.available_memory();
    (avail > 0).then_some(avail)
}

struct CiState {
    /// dispatch_id → cancel token for the running build.
    running: HashMap<String, CancellationToken>,
    /// Deferred (at-cap) dispatches, FIFO.
    queued: VecDeque<CiDispatchPayload>,
}

fn ci_state() -> &'static Mutex<CiState> {
    static STATE: OnceLock<Mutex<CiState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CiState {
            running: HashMap::new(),
            queued: VecDeque::new(),
        })
    })
}

/// Coord base for reporting on a payload (payload-pinned URL first, profile
/// fallback).
fn report_base(payload: &CiDispatchPayload) -> Option<String> {
    let pinned = payload.coord_http_url.trim();
    if !pinned.is_empty() {
        return Some(pinned.trim_end_matches('/').to_string());
    }
    super::coord_http_base()
}

fn reject(payload: &CiDispatchPayload, reason: String) {
    warn!(
        "ci_node: rejecting dispatch {} for {}: {reason}",
        payload.dispatch_id, payload.repo
    );
    if let Some(base) = report_base(payload) {
        reporting::post_cancelled_result_detached(base, payload.dispatch_id.clone(), reason);
    }
}

/// Entry point from the WS subscription for `build_requested`.
pub(crate) fn submit(payload: CiDispatchPayload) {
    // Identifier safety FIRST: an unsafe dispatch_id can't even be reported
    // (it rides the result URL path), so it is dropped with a log only.
    if !super::dispatch_id_is_safe(&payload.dispatch_id) {
        warn!(
            "ci_node: dropping dispatch with unsafe dispatch_id (len={})",
            payload.dispatch_id.len()
        );
        return;
    }
    if !super::repo_slug_is_safe(&payload.repo) {
        reject(&payload, format!("unsafe repo slug {:?}", payload.repo));
        return;
    }

    let settings = crate::settings::get_ci_node_settings();
    let decision = {
        let state = ci_state().lock().unwrap();
        // Dedup: a dispatch already running or queued here is a duplicate
        // delivery (WS replay) — ignore it rather than double-building.
        if state.running.contains_key(&payload.dispatch_id)
            || state
                .queued
                .iter()
                .any(|p| p.dispatch_id == payload.dispatch_id)
        {
            info!(
                "ci_node: duplicate dispatch {} ignored (already running/queued)",
                payload.dispatch_id
            );
            return;
        }
        admission_decision(&settings, &payload.repo, state.running.len())
    };

    match decision {
        Admission::Reject(reason) => reject(&payload, reason),
        Admission::Defer => {
            info!(
                "ci_node: at capacity — deferring dispatch {} for {} (queue depth {})",
                payload.dispatch_id,
                payload.repo,
                ci_state().lock().unwrap().queued.len() + 1
            );
            ci_state().lock().unwrap().queued.push_back(payload);
        }
        Admission::Proceed => start_build(payload, settings),
    }
}

/// Start one admitted build: disk floor gate, then spawn the executor task
/// with a fresh cancel token registered under the dispatch_id.
fn start_build(payload: CiDispatchPayload, settings: CiNodeSettings) {
    let Some(root) = crate::agent_runtime::qontinui_root_dir() else {
        reject(
            &payload,
            "QONTINUI_ROOT not resolvable on this device".to_string(),
        );
        return;
    };

    match free_disk_gb_for(&root) {
        Some(free) if free < settings.min_free_disk_gb => {
            reject(
                &payload,
                format!(
                    "free disk {free} GiB on the {} volume is below the ci_node.min_free_disk_gb \
                     floor ({} GiB)",
                    root.display(),
                    settings.min_free_disk_gb
                ),
            );
            return;
        }
        Some(free) => info!(
            "ci_node: disk gate ok ({free} GiB free ≥ {} GiB floor)",
            settings.min_free_disk_gb
        ),
        None => warn!(
            "ci_node: could not resolve free disk for {} — proceeding (fail-open guard)",
            root.display()
        ),
    }

    // RAM floor (plan §4.6): a build admitted onto a starved box would OOM
    // the developer's own work before it OOMs itself.
    match available_ram_bytes() {
        Some(avail) if ram_below_floor(avail, MIN_FREE_RAM_GB) => {
            reject(
                &payload,
                format!(
                    "available RAM {} GiB is below the {MIN_FREE_RAM_GB} GiB floor",
                    avail / (1024 * 1024 * 1024)
                ),
            );
            return;
        }
        Some(_) => {}
        None => warn!("ci_node: could not resolve available RAM — proceeding (fail-open guard)"),
    }

    let token = CancellationToken::new();
    {
        let mut state = ci_state().lock().unwrap();
        // Re-check the cap under the lock (submit's read was unlocked
        // in-between for the disk probe).
        if state.running.len() >= settings.max_concurrent_builds.max(1) as usize {
            drop(state);
            info!(
                "ci_node: slot taken while gating — deferring dispatch {}",
                payload.dispatch_id
            );
            ci_state().lock().unwrap().queued.push_back(payload);
            return;
        }
        state
            .running
            .insert(payload.dispatch_id.clone(), token.clone());
    }

    let dispatch_id = payload.dispatch_id.clone();
    info!(
        "ci_node: starting dispatch {} repo={} sha={} check_name={:?}",
        dispatch_id, payload.repo, payload.head_sha, payload.check_name
    );
    tokio::spawn(async move {
        super::executor::run_dispatch(payload, root, token).await;
        on_build_finished(&dispatch_id);
    });
}

/// Build-finished hook: free the slot, then re-admit the oldest deferred
/// dispatch (fresh settings + disk gate — conditions may have changed while
/// it waited).
fn on_build_finished(dispatch_id: &str) {
    let next = {
        let mut state = ci_state().lock().unwrap();
        state.running.remove(dispatch_id);
        state.queued.pop_front()
    };
    if let Some(payload) = next {
        info!(
            "ci_node: slot freed by {} — re-admitting deferred dispatch {}",
            dispatch_id, payload.dispatch_id
        );
        submit(payload);
    }
}

/// Entry point from the WS subscription for `build_cancelled`.
pub(crate) fn cancel(dispatch_id: &str) {
    let (was_running, dequeued) = {
        let mut state = ci_state().lock().unwrap();
        if let Some(token) = state.running.get(dispatch_id) {
            token.cancel();
            (true, None)
        } else {
            let before = state.queued.len();
            let mut removed: Option<CiDispatchPayload> = None;
            state.queued.retain(|p| {
                if p.dispatch_id == dispatch_id {
                    removed = Some(p.clone());
                    false
                } else {
                    true
                }
            });
            (before != state.queued.len(), removed)
        }
    };
    if was_running && dequeued.is_none() {
        info!("ci_node: cancel requested for running dispatch {dispatch_id}");
    } else if let Some(payload) = dequeued {
        info!("ci_node: cancelled queued dispatch {dispatch_id} before start");
        if let Some(base) = report_base(&payload) {
            reporting::post_cancelled_result_detached(
                base,
                payload.dispatch_id,
                "cancelled by coord while queued on this device".to_string(),
            );
        }
    } else {
        info!("ci_node: cancel for unknown dispatch {dispatch_id} (not running/queued here)");
    }
}

/// App-shutdown seam: cancel every running build's token and drop the
/// queue. Called from the `main.rs` window-close handler; the Windows Job
/// Object (kill-on-close) is the hard backstop for the process trees.
pub(crate) fn cancel_all_for_shutdown() {
    let mut state = ci_state().lock().unwrap();
    for (id, token) in state.running.iter() {
        info!("ci_node: shutdown — cancelling dispatch {id}");
        token.cancel();
    }
    state.queued.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(enabled: bool, allow: &[&str], cap: u32) -> CiNodeSettings {
        CiNodeSettings {
            enabled,
            max_concurrent_builds: cap,
            repo_allowlist: allow.iter().map(|s| s.to_string()).collect(),
            min_free_disk_gb: 20,
        }
    }

    #[test]
    fn disabled_is_a_hard_reject() {
        let s = settings(false, &["qontinui-runner"], 1);
        assert!(matches!(
            admission_decision(&s, "qontinui/qontinui-runner", 0),
            Admission::Reject(r) if r.contains("disabled")
        ));
    }

    #[test]
    fn unlisted_repo_is_a_hard_reject_and_empty_allowlist_runs_nothing() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert!(matches!(
            admission_decision(&s, "qontinui/qontinui-coord", 0),
            Admission::Reject(r) if r.contains("repo_allowlist")
        ));
        // Empty allowlist = nothing runnable, even enabled.
        let s = settings(true, &[], 1);
        assert!(matches!(
            admission_decision(&s, "qontinui/qontinui-runner", 0),
            Admission::Reject(_)
        ));
    }

    #[test]
    fn at_cap_defers_never_rejects() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 1),
            Admission::Defer
        );
        // Below cap proceeds.
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 0),
            Admission::Proceed
        );
        // cap=0 is treated as 1 (a nonsensical hand-edit must not brick
        // admission into permanent deferral at running=0).
        let s0 = settings(true, &["qontinui-runner"], 0);
        assert_eq!(
            admission_decision(&s0, "qontinui/qontinui-runner", 0),
            Admission::Proceed
        );
    }

    #[test]
    fn allowlist_matches_slug_or_basename() {
        assert!(repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui/qontinui-runner"
        ));
        assert!(repo_allowed(
            &["qontinui/qontinui-runner".to_string()],
            "qontinui/qontinui-runner"
        ));
        assert!(repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui-runner"
        ));
        assert!(!repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui/qontinui-web"
        ));
    }

    #[test]
    fn ram_floor_threshold() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert!(ram_below_floor(3 * GIB, MIN_FREE_RAM_GB));
        assert!(ram_below_floor(4 * GIB - 1, MIN_FREE_RAM_GB));
        assert!(!ram_below_floor(4 * GIB, MIN_FREE_RAM_GB));
        assert!(!ram_below_floor(64 * GIB, MIN_FREE_RAM_GB));
    }

    #[test]
    fn volume_pick_prefers_longest_mount_prefix() {
        let mounts = vec![
            (PathBuf::from("C:\\"), 5 * 1024 * 1024 * 1024_u64),
            (PathBuf::from("D:\\"), 100 * 1024 * 1024 * 1024_u64),
        ];
        // Windows-shaped paths only resolve on Windows path semantics;
        // use starts_with-compatible shapes for a portable test instead.
        let mounts_portable = vec![
            (PathBuf::from("/"), 5 * 1024 * 1024 * 1024_u64),
            (PathBuf::from("/data"), 100 * 1024 * 1024 * 1024_u64),
        ];
        assert_eq!(
            pick_volume_free_gb(&mounts_portable, Path::new("/data/qontinui-root")),
            Some(100)
        );
        assert_eq!(
            pick_volume_free_gb(&mounts_portable, Path::new("/home/x")),
            Some(5)
        );
        assert_eq!(
            pick_volume_free_gb(&mounts, Path::new("/nowhere")),
            None,
            "no matching mount → None (caller fails open)"
        );
    }
}
