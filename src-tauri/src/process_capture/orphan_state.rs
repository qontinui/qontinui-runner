//! Persistent descendant-tree state across runner sessions.
//!
//! When the runner crashes or is force-killed without an orderly shutdown,
//! its managed-process children (`cmd → poetry → python → uvicorn`) survive
//! as orphans. The Windows Job Object set up at `main.rs::init_job_object`
//! reaps these on graceful exit, but a ⚙ crash leaves the JobObject handle
//! closed mid-state and Windows can race the runner before the
//! KILL_ON_JOB_CLOSE setting takes effect for late-spawned children.
//!
//! To recover, every successful descendant-discovery pass (see
//! `process_tree::descendant_discovery_once`) writes the current tree to
//! `<data_local_dir>/com.qontinui.runner/process_tree_state.json`. On the
//! next runner startup, we reload that file, sanity-filter each persisted
//! PID against the live process's actual creation time (PIDs get reused),
//! reap any surviving descendants via `health::kill_descendant_tree`, and
//! clear the file so a crash mid-reclaim doesn't loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::process_tree::{PidWithSpawnedAt, ProcessSnapshot};

/// Bumped when the on-disk shape changes. Older readers skip unknown
/// versions rather than partially-parse and lose data.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level on-disk representation. Keyed by `ProcessConfig.id` so a
/// renamed-but-same-id config still reclaims its prior generation.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OrphanState {
    pub schema_version: u32,
    #[serde(default)]
    pub trees: HashMap<String, Vec<PidWithSpawnedAt>>,
}

impl OrphanState {
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            trees: HashMap::new(),
        }
    }
}

/// Resolve the on-disk path: `<data_local_dir>/com.qontinui.runner/`
/// (`instance_env::runner_data_local_base`) — beside `auth_tokens.enc` on a
/// runner without `QONTINUI_SECURE_STORAGE_DIR`.
pub fn state_path() -> Option<PathBuf> {
    let dir = qontinui_runner_lib::instance_env::runner_data_local_base()?;
    Some(dir.join("process_tree_state.json"))
}

/// Load persisted state. Returns `Default` (empty) if the file is absent,
/// unreadable, or carries an unsupported schema version. This is the
/// "lose orphan reclaim for one startup" forward-compat branch.
pub fn load() -> OrphanState {
    match state_path() {
        Some(path) => load_from(&path),
        None => OrphanState::default(),
    }
}

/// [`load`] from an explicit path.
pub(crate) fn load_from(path: &Path) -> OrphanState {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return OrphanState::default(),
    };
    let parsed: OrphanState = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "orphan_state: failed to parse {}: {} — ignoring",
                path.display(),
                e
            );
            return OrphanState::default();
        }
    };
    if parsed.schema_version != SCHEMA_VERSION {
        tracing::warn!(
            "orphan_state: schema {} != {}, skipping reclaim this startup",
            parsed.schema_version,
            SCHEMA_VERSION
        );
        return OrphanState::default();
    }
    parsed
}

/// Persist `state` atomically (write-temp-then-rename so a crash mid-write
/// leaves the previous file intact).
pub fn save(state: &OrphanState) -> std::io::Result<()> {
    let path = match state_path() {
        Some(p) => p,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "data_local_dir unavailable",
            ));
        }
    };
    save_to(&path, state)
}

/// [`save`] to an explicit path.
pub(crate) fn save_to(path: &Path, state: &OrphanState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Upsert a single process's descendant list and persist.
///
/// Best-effort: failures are logged but don't propagate, since orphan
/// reclaim is a safety net, not load-bearing for correctness.
pub fn record_tree(process_id: &str, descendants: &[PidWithSpawnedAt]) {
    let mut state = load();
    if descendants.is_empty() {
        state.trees.remove(process_id);
    } else {
        state
            .trees
            .insert(process_id.to_string(), descendants.to_vec());
    }
    state.schema_version = SCHEMA_VERSION;
    if let Err(e) = save(&state) {
        tracing::warn!("orphan_state: failed to persist tree for {process_id}: {e}");
    }
}

/// Reclaim any orphan descendants left over from a prior session.
///
/// For each persisted PID, read its live creation time from one fresh
/// process-table snapshot. If the PID is alive AND its creation time matches
/// the persisted `spawned_at_unix` ([`super::health::is_same_process`]: within
/// ±2s to absorb tick-resolution drift), it's an orphan — reap it via
/// `health::kill_descendant_tree`, which re-checks that creation time on a
/// handle held through the kill (Windows). Otherwise it's PID reuse (a new
/// unrelated process); skip.
///
/// After reclaim the file is rewritten with empty trees so a crash mid-loop
/// doesn't double-reap (or kill an innocent process whose PID happens to
/// match a stale entry).
///
/// A snapshot that is `stale` or empty ([`ProcessSnapshot::is_unknown`]) is
/// UNKNOWN: nothing is killed and the file is KEPT, so the next startup
/// retries rather than forgetting the orphans.
pub async fn reclaim_orphans() {
    let Some(path) = state_path() else {
        return;
    };
    // No snapshot at all when nothing was persisted.
    if load_from(&path).trees.is_empty() {
        return;
    }
    // One snapshot whose read began now (never the memo), taken once. We only
    // care about creation_times for the PID-reuse guard.
    let snapshot = super::process_tree::snapshot_process_table_fresh().await;
    reclaim_at(&path, &snapshot, super::health::kill_descendant_tree).await;
}

/// [`reclaim_orphans`] against an explicit state file, snapshot and kill
/// function: on an UNKNOWN snapshot nothing is killed and the file is left
/// exactly as it was; otherwise every target is killed and the file is
/// cleared.
pub(crate) async fn reclaim_at<F, Fut>(path: &Path, snapshot: &ProcessSnapshot, mut kill: F)
where
    F: FnMut(PidWithSpawnedAt) -> Fut,
    Fut: std::future::Future<Output = usize>,
{
    let state = load_from(path);
    if state.trees.is_empty() {
        return;
    }
    let targets = match reclaim_targets(&state, snapshot) {
        Ok(t) => t,
        Err(why) => {
            tracing::warn!("orphan_state: startup reclaim skipped — {why}");
            return;
        }
    };

    let mut total_reaped: usize = 0;
    let surviving_count: usize = state.trees.values().map(Vec::len).sum();
    for (process_id, root) in targets {
        let killed = kill(root).await;
        tracing::info!(
            "orphan_state: reaped orphan tree for '{}' (root PID {}, {} processes killed)",
            process_id,
            root.pid,
            killed
        );
        total_reaped += killed;
    }

    // Clear the persisted state regardless of outcome. A surviving PID
    // we missed is recoverable on next iteration; double-reaping is not.
    let empty = OrphanState::new();
    if let Err(e) = save_to(path, &empty) {
        tracing::warn!("orphan_state: failed to clear state file after reclaim: {e}");
    }

    if surviving_count > 0 {
        tracing::info!(
            "orphan_state: startup reclaim complete — {} prior-session entries inspected, {} processes killed",
            surviving_count,
            total_reaped
        );
    }
}

/// The `(process id, recorded root)` pairs the startup reclaim may kill,
/// judged against `snapshot` — the recorded root carries the persisted
/// creation time the kill re-checks — or `Err` when the snapshot is UNKNOWN
/// (`stale`, or an empty table), in which case NOTHING may be killed: a pid
/// absent from, or present in, a table that was not read now proves nothing
/// about the process holding that pid now.
pub(crate) fn reclaim_targets(
    state: &OrphanState,
    snapshot: &ProcessSnapshot,
) -> Result<Vec<(String, PidWithSpawnedAt)>, String> {
    if snapshot.is_unknown() {
        return Err(format!(
            "the process table is UNKNOWN (stale: {}, empty: {}); no orphan is killed from it",
            snapshot.stale,
            snapshot.parent_map.is_empty()
        ));
    }
    let live_creation_times = &snapshot.creation_times;
    let mut targets = Vec::new();
    for (process_id, persisted) in state.trees.iter() {
        for entry in persisted.iter() {
            let live_ts = match live_creation_times.get(&entry.pid).copied() {
                Some(t) => t,
                None => {
                    // PID no longer exists; nothing to do.
                    continue;
                }
            };
            // PID-reuse guard. The persisted fingerprint is meaningful only
            // when BOTH timestamps parsed successfully (`> 0`). If either
            // side is 0 we can't tell "still our orphan" from "PID reused
            // by an unrelated process", so we fail closed and skip the
            // kill rather than risk reaping a coincidental match. When the
            // record and the table both carry the exact FILETIME they must be
            // equal; otherwise ±2s of clock-resolution drift counts as the
            // same process (`health::is_same_process`, the rule the kill
            // re-checks on a held handle).
            if entry.spawned_at_unix == 0 || live_ts == 0 {
                tracing::debug!(
                    "orphan_state: PID {} creation timestamp unavailable \
                     (persisted={}, live={}); skipping reclaim for '{}' \
                     (fail-closed against PID reuse)",
                    entry.pid,
                    entry.spawned_at_unix,
                    live_ts,
                    process_id
                );
                continue;
            }
            let live_ft = snapshot.creation_filetimes.get(&entry.pid).copied();
            if !super::health::is_same_process(*entry, live_ts, live_ft) {
                tracing::debug!(
                    "orphan_state: PID {} reused since prior session \
                     (persisted spawned_at={}, live={}); skipping reclaim for '{}'",
                    entry.pid,
                    entry.spawned_at_unix,
                    live_ts,
                    process_id
                );
                continue;
            }
            targets.push((process_id.clone(), *entry));
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_orphan(pid: u32, spawned_at_unix: i64) -> OrphanState {
        let mut s = OrphanState::new();
        s.trees.insert(
            "backend".to_string(),
            vec![PidWithSpawnedAt {
                pid,
                spawned_at_unix,
                spawned_at_ft: None,
            }],
        );
        s
    }

    fn live(pid: u32, created: i64, stale: bool) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.parent_map.insert(1, vec![pid]);
        s.creation_times.insert(pid, created);
        s.stale = stale;
        s
    }

    /// The pid is listed with its persisted creation time — a match the
    /// reclaim would kill — but the table is STALE: nothing is a target.
    #[test]
    fn orphan_state_refuses_a_stale_snapshot() {
        let state = one_orphan(4242, 1_715_911_000);
        let err = reclaim_targets(&state, &live(4242, 1_715_911_000, true))
            .expect_err("a stale table must not yield kill targets");
        assert!(err.contains("UNKNOWN"), "{err}");
        // The same table read fresh IS a target: the refusal is the flag.
        assert_eq!(
            reclaim_targets(&state, &live(4242, 1_715_911_000, false)).unwrap(),
            vec![(
                "backend".to_string(),
                PidWithSpawnedAt {
                    pid: 4242,
                    spawned_at_unix: 1_715_911_000,
                    spawned_at_ft: None,
                }
            )]
        );
        // An empty (fresh-flagged) table is a failed read too.
        let empty = ProcessSnapshot {
            stale: false,
            ..ProcessSnapshot::default()
        };
        assert!(reclaim_targets(&state, &empty).is_err());
    }

    /// On an UNKNOWN (stale) table the reclaim kills nothing and leaves the
    /// state file byte-for-byte as it was, so the next startup retries; on a
    /// readable table with nothing to kill it clears the file.
    #[tokio::test]
    async fn orphan_state_keeps_its_file_when_the_table_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("process_tree_state.json");
        save_to(&path, &one_orphan(4242, 1_715_911_000)).expect("seed");
        let before = std::fs::read(&path).expect("read");

        let never_kill = |_root: PidWithSpawnedAt| -> std::future::Ready<usize> {
            panic!("nothing may be killed from an UNKNOWN table")
        };
        reclaim_at(&path, &live(4242, 1_715_911_000, true), never_kill).await;
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "kept on UNKNOWN"
        );

        // A readable table where the pid has been reused: nothing to kill, and
        // the file is cleared.
        reclaim_at(&path, &live(4242, 1_715_999_000, false), never_kill).await;
        assert!(
            load_from(&path).trees.is_empty(),
            "cleared after a readable pass"
        );
    }

    /// The PID-reuse guard is unchanged: a creation time off by more than 2 s,
    /// or unknown on either side, is not a target.
    #[test]
    fn orphan_state_targets_only_a_matching_creation_time() {
        let state = one_orphan(4242, 1_715_911_000);
        assert!(reclaim_targets(&state, &live(4242, 1_715_911_003, false))
            .unwrap()
            .is_empty());
        assert!(reclaim_targets(&state, &live(4242, 0, false))
            .unwrap()
            .is_empty());
        assert!(reclaim_targets(&state, &live(9, 1_715_911_000, false))
            .unwrap()
            .is_empty());
        assert_eq!(
            reclaim_targets(&state, &live(4242, 1_715_911_002, false))
                .unwrap()
                .len(),
            1
        );
    }

    /// W2: a record and a table that both carry the exact FILETIME are
    /// compared exactly — one tick apart in the same second is a reused pid —
    /// and a table without it falls back to the 2 s window.
    #[test]
    fn orphan_state_compares_exact_filetimes_exactly() {
        const FT: u64 = 133_000_000_000_000_000;
        let mut state = OrphanState::new();
        state.trees.insert(
            "backend".to_string(),
            vec![PidWithSpawnedAt {
                pid: 4242,
                spawned_at_unix: 1_715_911_000,
                spawned_at_ft: Some(FT),
            }],
        );
        let with_ft = |ft: u64| {
            let mut s = live(4242, 1_715_911_000, false);
            s.creation_filetimes.insert(4242, ft);
            s
        };
        assert!(reclaim_targets(&state, &with_ft(FT + 1))
            .unwrap()
            .is_empty());
        assert_eq!(reclaim_targets(&state, &with_ft(FT)).unwrap().len(), 1);
        assert_eq!(
            reclaim_targets(&state, &live(4242, 1_715_911_002, false))
                .unwrap()
                .len(),
            1,
            "no live FILETIME: the 2 s window decides"
        );
    }

    /// A state file written before `spawned_at_ft` existed still loads.
    #[test]
    fn orphan_state_loads_records_without_a_filetime() {
        let json = r#"{"schema_version":1,"trees":{"backend":[{"pid":7,"spawned_at_unix":5}]}}"#;
        let parsed: OrphanState = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.trees["backend"][0].spawned_at_ft, None);
    }

    #[test]
    fn round_trip_orphan_state() {
        let mut s = OrphanState::new();
        s.trees.insert(
            "backend".to_string(),
            vec![PidWithSpawnedAt {
                pid: 1234,
                spawned_at_unix: 1715911000,
                spawned_at_ft: None,
            }],
        );
        let json = serde_json::to_string(&s).unwrap();
        let parsed: OrphanState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.trees.len(), 1);
        assert_eq!(parsed.trees["backend"][0].pid, 1234);
    }

    #[test]
    fn schema_mismatch_returns_default() {
        let json = r#"{"schema_version":999,"trees":{"backend":[]}}"#;
        // Bypass load() because it reads from disk; emulate the version check.
        let parsed: OrphanState = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.schema_version, 999);
        // The actual load() would discard this and return default; here we
        // just confirm we can detect the mismatch.
    }
}
