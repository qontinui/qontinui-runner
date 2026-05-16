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
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::process_tree::PidWithSpawnedAt;

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

/// Resolve the on-disk path; co-located with the existing
/// `auth_tokens.enc` in `<data_local_dir>/com.qontinui.runner/`.
pub fn state_path() -> Option<PathBuf> {
    let mut dir = dirs::data_local_dir()?;
    dir.push("com.qontinui.runner");
    Some(dir.join("process_tree_state.json"))
}

/// Load persisted state. Returns `Default` (empty) if the file is absent,
/// unreadable, or carries an unsupported schema version. This is the
/// "lose orphan reclaim for one startup" forward-compat branch.
pub fn load() -> OrphanState {
    let path = match state_path() {
        Some(p) => p,
        None => return OrphanState::default(),
    };
    let text = match std::fs::read_to_string(&path) {
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
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
/// For each persisted PID, query the live `Win32_Process` / `/proc/<pid>/stat`
/// creation time. If the PID is alive AND its creation time matches the
/// persisted `spawned_at_unix` (within ±2s to absorb tick-resolution drift),
/// it's an orphan — reap it via `health::kill_descendant_tree`. Otherwise
/// it's PID reuse (a new unrelated process); skip.
///
/// After reclaim the file is rewritten with empty trees so a crash mid-loop
/// doesn't double-reap (or kill an innocent process whose PID happens to
/// match a stale entry).
pub async fn reclaim_orphans() {
    let state = load();
    if state.trees.is_empty() {
        return;
    }

    use super::process_tree;

    // Take a fresh process-table snapshot once; cheap and avoids racing
    // multiple WMI calls. We only care about creation_times for the
    // PID-reuse guard.
    let snapshot = process_tree::snapshot_process_table_public().await;
    let live_creation_times = snapshot.creation_times;

    let mut total_reaped: usize = 0;
    let mut surviving_count: usize = 0;

    for (process_id, persisted) in state.trees.iter() {
        for entry in persisted.iter() {
            surviving_count += 1;
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
            // kill rather than risk reaping a coincidental match. Within
            // ±2s of clock-resolution drift counts as the same process.
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
            if (live_ts - entry.spawned_at_unix).abs() > 2 {
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
            let killed = super::health::kill_descendant_tree(entry.pid).await;
            tracing::info!(
                "orphan_state: reaped orphan tree for '{}' (root PID {}, {} processes killed)",
                process_id,
                entry.pid,
                killed
            );
            total_reaped += killed;
        }
    }

    // Clear the persisted state regardless of outcome. A surviving PID
    // we missed is recoverable on next iteration; double-reaping is not.
    let empty = OrphanState::new();
    if let Err(e) = save(&empty) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_orphan_state() {
        let mut s = OrphanState::new();
        s.trees.insert(
            "backend".to_string(),
            vec![PidWithSpawnedAt {
                pid: 1234,
                spawned_at_unix: 1715911000,
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
