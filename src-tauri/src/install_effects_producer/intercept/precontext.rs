//! TTL'd `correlation_id → PreContext` stash for the two-call intercept
//! straddle (plan §3 A3 + §6 "TTL map liveness").
//!
//! The intercept pre-call (`POST /install-effects/run {mode:intercept}`) runs
//! declare + predict-and-check + pre-sha capture, then stashes everything the
//! post-call (`POST /install-effects/observe-verify`) needs to run the shared
//! `observe_and_verify` body — keyed by the minted `correlation_id`. The shim
//! runs the REAL install BETWEEN the two calls, so the context must survive
//! across an arbitrary (bounded) gap.
//!
//! Liveness: entries TTL after [`PRECONTEXT_TTL`] (10 min, §6). An abandoned
//! install (agent Ctrl-C'd between calls) simply expires; a post-call for an
//! expired/missing id degrades to an honest best-effort verify with no
//! pre-shas (the caller handles that — see [`take`]). Expiry is swept lazily on
//! every access — no background task.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use uuid::Uuid;

use crate::install_effects_producer::types::PackageManager;

/// How long a stashed pre-context lives before the post-call can no longer
/// find it. 10 minutes (§6) — generous for a normal install, bounded so an
/// abandoned straddle cannot leak memory.
pub const PRECONTEXT_TTL: Duration = Duration::from_secs(600);

/// Everything the post-call's `observe_and_verify` needs that was computed at
/// pre-call time. A trimmed mirror of [`crate::install_effects_producer::RunContext`]
/// plus the resolved `repo_path` (the post-call re-reads the worktree there).
#[derive(Debug, Clone)]
pub struct PreContext {
    pub repo: String,
    pub repo_path: String,
    pub package_manager: PackageManager,
    pub escalation_overridden: bool,
    pub affected_files: Vec<String>,
    pub predicted_pinned: BTreeMap<String, String>,
    pub predicted_cves: Vec<crate::install_effects_producer::types::Cve>,
    /// Pre-install manifest/lockfile blob shas captured BEFORE the shim ran the
    /// real install, so the FS-observation push pairs pre↔post.
    pub pre_shas: BTreeMap<String, Option<String>>,
}

struct Entry {
    ctx: PreContext,
    stored_at: Instant,
}

static STASH: Lazy<Mutex<HashMap<Uuid, Entry>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Stash `ctx` under `id`. Sweeps expired entries on the way in (lazy GC).
pub fn put(id: Uuid, ctx: PreContext) {
    let mut map = STASH.lock().unwrap_or_else(|e| e.into_inner());
    sweep(&mut map);
    map.insert(
        id,
        Entry {
            ctx,
            stored_at: Instant::now(),
        },
    );
}

/// Remove + return the context for `id`, or `None` if it was never stored or
/// has expired (TTL). Sweeps expired entries on the way through. A `None` here
/// means the post-call must degrade to a best-effort verify with no pre-shas.
pub fn take(id: &Uuid) -> Option<PreContext> {
    let mut map = STASH.lock().unwrap_or_else(|e| e.into_inner());
    sweep(&mut map);
    map.remove(id).and_then(|e| {
        if e.stored_at.elapsed() <= PRECONTEXT_TTL {
            Some(e.ctx)
        } else {
            None
        }
    })
}

/// Current number of live (un-swept) entries — for tests/diagnostics.
#[cfg(test)]
pub fn len() -> usize {
    let mut map = STASH.lock().unwrap_or_else(|e| e.into_inner());
    sweep(&mut map);
    map.len()
}

/// Drop every entry older than the TTL.
fn sweep(map: &mut HashMap<Uuid, Entry>) {
    map.retain(|_, e| e.stored_at.elapsed() <= PRECONTEXT_TTL);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(repo: &str) -> PreContext {
        PreContext {
            repo: repo.to_string(),
            repo_path: format!("/tmp/{repo}"),
            package_manager: PackageManager::Npm,
            escalation_overridden: false,
            affected_files: vec![],
            predicted_pinned: BTreeMap::new(),
            predicted_cves: vec![],
            pre_shas: BTreeMap::new(),
        }
    }

    #[test]
    fn put_then_take_roundtrips_and_removes() {
        let id = Uuid::new_v4();
        put(id, ctx("widget"));
        let got = take(&id).expect("stashed ctx present");
        assert_eq!(got.repo, "widget");
        // Second take is None (take removed it).
        assert!(take(&id).is_none(), "take must remove the entry");
    }

    #[test]
    fn missing_id_is_none() {
        assert!(take(&Uuid::new_v4()).is_none());
    }

    #[test]
    fn expired_entry_is_swept_and_returns_none() {
        let id = Uuid::new_v4();
        {
            let mut map = STASH.lock().unwrap();
            map.insert(
                id,
                Entry {
                    ctx: ctx("stale"),
                    // Force an expired timestamp.
                    stored_at: Instant::now() - PRECONTEXT_TTL - Duration::from_secs(1),
                },
            );
        }
        assert!(take(&id).is_none(), "expired entry must not be returned");
        // And it was removed from the map (no leak).
        assert!(STASH.lock().unwrap().get(&id).is_none());
    }
}
