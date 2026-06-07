//! Runner-side **fs_observer backstop** (Ξ_FS Phase 5) — a defense-in-depth
//! DETECTOR (not a prevent mechanism) for edits that leaked OUTSIDE any
//! session worktree.
//!
//! ## What it does
//!
//! A machine-wide periodic scanner. Each tick it enumerates the SHARED
//! canonical checkouts (`<root>/<repo>/`, the same governed set the census
//! walks) and, for each one that is **dirty** (uncommitted working-tree
//! changes) AND **not covered by any live `kind=worktree` coord claim**,
//! POSTs the drift to coord's existing `POST /coord/fs/observations` ingest
//! tagged `source: "canonical_drift"`.
//!
//! "Dirty AND unclaimed" is the leak signature: Phases 1+2 register a
//! `kind=worktree` claim for every checkout a session legitimately
//! materializes / shared-branches in (keyed on the canonical path via
//! [`super::worktree_resource_key`]). So a checkout that is dirty with NO
//! holder is an edit the runner never provisioned — e.g. a VS Code
//! native-panel agent operating directly in the canonical tree.
//!
//! ## Why reuse `/coord/fs/observations` (not a new endpoint)
//!
//! Self-contained + immediately functional: coord already accepts that body
//! and ignores the extra `source`/`device_id`/… fields forward-compatibly.
//! A coord-side dashboard alarm keyed on `source=canonical_drift` is a
//! separate follow-up; the coord POST + the runner `warn!` line ARE the
//! alarm here.
//!
//! ## Tier-1 only — surface/attribute, never mutate
//!
//! This reconcile is detection only: NO auto-stash, no working-tree
//! mutation. It reads (`git status`, a libgit2 diff) and POSTs. Tier-2
//! (auto-stash) is deferred.
//!
//! ## Suppression contract (never false-alarm)
//!
//! An alarm fires STRICTLY on dirty AND a `null` `by-resource` holder. Any
//! other shape suppresses:
//!   - a non-null holder (claimed — including a legitimate `SharedBranch`
//!     self-claim) ⇒ suppress;
//!   - a lookup ERROR or coord-unreachable ⇒ suppress (we can't prove the
//!     checkout is unclaimed, so we never alarm);
//!   - a clean checkout, or zero reportable observations ⇒ suppress.
//!
//! ## Gating
//!
//! Behind [`FS_BACKSTOP_FLAG_ENV`] (`QONTINUI_FS_BACKSTOP_ENABLED`),
//! **default off**, best-effort, NEVER blocking. Cadence from
//! [`FS_BACKSTOP_INTERVAL_ENV`] (default 300s, floor 30s). Runs on the
//! non-Tauri fleet-publishers runtime (no `AppHandle`) — there is no banner.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};

use super::fs_observer::FsObservation;

/// Env flag that turns the fs_observer backstop on. Default off. Accepts the
/// usual truthy values; anything else (including unset) is false. Matches the
/// truthy-set used by [`super::fs_observer::fs_observer_enabled`].
pub const FS_BACKSTOP_FLAG_ENV: &str = "QONTINUI_FS_BACKSTOP_ENABLED";

/// Cadence env var for the backstop poller. Default 300s, floored at 30s
/// (mirrors the census / reclaim cadence knobs).
pub const FS_BACKSTOP_INTERVAL_ENV: &str = "QONTINUI_FS_BACKSTOP_INTERVAL_SECS";

/// Default backstop cadence in seconds — an order of magnitude slower than
/// the heartbeat, matching the census/reclaim pollers.
const DEFAULT_FS_BACKSTOP_INTERVAL_SECS: u64 = 300;

/// Floor for the cadence — never tighter than 30s regardless of env.
const FS_BACKSTOP_INTERVAL_FLOOR_SECS: u64 = 30;

/// Returns true iff the fs_observer backstop is enabled. Default off.
pub fn fs_backstop_enabled() -> bool {
    matches!(
        std::env::var(FS_BACKSTOP_FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Resolve the poller cadence from [`FS_BACKSTOP_INTERVAL_ENV`], defaulting to
/// [`DEFAULT_FS_BACKSTOP_INTERVAL_SECS`] and flooring at
/// [`FS_BACKSTOP_INTERVAL_FLOOR_SECS`]. (Copy of the census interval helper.)
fn interval_secs() -> u64 {
    std::env::var(FS_BACKSTOP_INTERVAL_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FS_BACKSTOP_INTERVAL_SECS)
        .max(FS_BACKSTOP_INTERVAL_FLOOR_SECS)
}

/// A live `kind=worktree` claim holder, as parsed from coord's
/// `GET /coord/claims/by-resource`. Only the fields we log are captured; an
/// object response (any holder) is enough to SUPPRESS the alarm.
#[derive(Debug, Clone)]
pub struct Holder {
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

/// GET the live `kind=worktree` claim holder for `resource_key`.
///
/// Returns `Ok(None)` when coord answers `null` (no holder — the leak
/// signature), `Ok(Some(holder))` when an object is returned (claimed —
/// suppress), and `Err` on any transport / non-2xx / parse failure (the
/// caller treats an `Err` as "can't prove unclaimed" and suppresses).
///
/// Near-verbatim from `terminal::coord_warn::check_and_warn`'s by-resource
/// lookup, minus the peer/self disambiguation: for the backstop ANY holder
/// (peer OR our own SharedBranch self-claim) is a legitimate cover that
/// suppresses the alarm.
pub async fn canonical_checkout_claim_holder(
    coord_base: &str,
    resource_key: &str,
) -> Result<Option<Holder>, String> {
    let url = format!(
        "{}/coord/claims/by-resource?kind=worktree&key={}",
        coord_base.trim_end_matches('/'),
        urlencoding::encode(resource_key),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET by-resource {}", resp.status().as_u16()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("parse body: {e}"))?;
    Ok(parse_by_resource_holder(&body))
}

/// Pure parse of a `by-resource` body into an optional [`Holder`]: `null`
/// ⇒ `None`, an object ⇒ `Some`. Extracted so the null-vs-object decision is
/// unit-testable without a live coord.
fn parse_by_resource_holder(body: &serde_json::Value) -> Option<Holder> {
    if body.is_null() {
        return None;
    }
    Some(Holder {
        machine_id: body
            .get("machine_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        session_id: body
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// Body of the backstop's `POST /coord/fs/observations`. A SUPERSET of
/// `fs_observer::FsObservationsRequest` — coord deserializes the
/// `observations` it knows and ignores the extra `source`/`device_id`/…
/// fields forward-compatibly. `source: "canonical_drift"` is the
/// distinguishing tag a coord-side dashboard alarm keys on (follow-up).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CanonicalDriftRequest {
    /// Distinguishes a backstop drift from a normal worktree observation.
    /// Always `"canonical_drift"`.
    pub source: &'static str,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub canonical_path: String,
    pub head_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<uuid::Uuid>,
    pub observations: Vec<FsObservation>,
}

/// The `source` tag value for a backstop drift POST.
const CANONICAL_DRIFT_SOURCE: &str = "canonical_drift";

/// PURE alarm decision: alarm iff the checkout is dirty AND has reportable
/// observations AND no holder covers it. A non-null `holder` (claimed,
/// including a legitimate SharedBranch self-claim) suppresses; a clean
/// checkout suppresses; zero observations suppress. Callers map a lookup
/// ERROR / coord-unreachable to "can't prove unclaimed" by NOT calling this
/// (they suppress directly), so this function only ever sees a proven holder
/// state.
pub fn should_alarm(is_dirty: bool, holder: Option<&Holder>, obs_len: usize) -> bool {
    is_dirty && holder.is_none() && obs_len > 0
}

/// Enumerate the governed canonical checkouts under `root`: every top-level
/// `qontinui-*` dir that is a CANONICAL repo root (not a `-wt-`/`-wt` worktree
/// dir) and has a `.git`. Reuses the census enumeration predicate
/// [`super::census::is_canonical_repo_dir`] so the backstop governs exactly
/// the set the census reports — they never drift.
///
/// Returns `(repo_name, canonical_path)` pairs.
fn governed_canonical_checkouts(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !super::census::is_canonical_repo_dir(&name) {
            continue;
        }
        if path.join(".git").exists() {
            out.push((name, path));
        }
    }
    out
}

/// Read `active_tenant_id` from `~/.qontinui/machine.json` (advisory — coord
/// DB-resolves the real tenant). `None` for single-tenant operators, which is
/// fine. Mirrors `census::resolve_tenant_id` (which is private; the read is
/// trivial and tenant attribution is best-effort).
fn resolve_tenant_id() -> Option<uuid::Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    uuid::Uuid::parse_str(raw.trim()).ok()
}

/// One scanned checkout's drift, assembled on the blocking pool: the dirty
/// gate already passed and observations were collected. The async tick then
/// performs the (async) claim lookup + POST per item.
struct ScannedDrift {
    repo: String,
    canonical_path: PathBuf,
    resource_key: String,
    head_sha: String,
    current_branch: Option<String>,
    observations: Vec<FsObservation>,
}

/// Synchronous disk-walk + git half of a tick: enumerate governed checkouts,
/// apply the cheap dirty gate, and for each dirty checkout collect its
/// uncommitted observations + branch + HEAD. Returns the candidates that are
/// dirty with ≥1 reportable observation (the async half does the claim
/// lookup + POST). Runs under `spawn_blocking`.
fn scan_drift_candidates(root: &Path) -> Vec<ScannedDrift> {
    let mut out: Vec<ScannedDrift> = Vec::new();
    for (repo, canonical) in governed_canonical_checkouts(root) {
        // Cheap dirty gate first — reuse the census `git status --porcelain`
        // probe. `None` (git failure) ⇒ treat as not-dirty (degrade honestly,
        // never alarm on a checkout we can't read).
        let is_dirty = super::census::compute_canonical_is_dirty(&canonical).unwrap_or(false);
        if !is_dirty {
            continue;
        }
        // Resolve the uncommitted observations against the checkout's own HEAD
        // (reuses the gitignore/denylist/blob-sha/hunk/cap logic).
        let observations = match super::fs_observer::collect_uncommitted_observations(&canonical) {
            Ok(o) => o,
            Err(e) => {
                debug!(
                    repo = %repo,
                    path = %canonical.display(),
                    error = %e,
                    "fs_backstop: collect failed — skipping checkout"
                );
                continue;
            }
        };
        if observations.is_empty() {
            // Dirty only in denylisted/ignored noise — nothing reportable.
            continue;
        }
        // HEAD sha + current branch for the request (best-effort; HEAD must
        // resolve since collect_uncommitted_observations already opened it).
        let head_sha = match resolve_head_sha(&canonical) {
            Some(sha) => sha,
            None => {
                debug!(
                    repo = %repo,
                    path = %canonical.display(),
                    "fs_backstop: HEAD unresolved — skipping checkout"
                );
                continue;
            }
        };
        let current_branch = super::census::compute_canonical_branch(&canonical);
        let resource_key = super::worktree_resource_key(&canonical);
        out.push(ScannedDrift {
            repo,
            canonical_path: canonical,
            resource_key,
            head_sha,
            current_branch,
            observations,
        });
    }
    out
}

/// Resolve a checkout's current HEAD sha via libgit2 (same pattern as
/// `edit_effect_loop::worktree_head_sha`). `None` on any error.
fn resolve_head_sha(checkout: &Path) -> Option<String> {
    let repo = git2::Repository::open(checkout).ok()?;
    let head = repo.head().ok()?;
    Some(head.target()?.to_string())
}

/// POST a built [`CanonicalDriftRequest`] to coord's existing fs-observations
/// ingest. Best-effort: any transport error / non-2xx logs and returns — the
/// backstop never surfaces a coord failure.
async fn post_canonical_drift(coord_base: &str, body: &CanonicalDriftRequest) {
    let url = format!("{}/coord/fs/observations", coord_base.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("fs_backstop: build http client failed: {e}");
            return;
        }
    };
    // Attach the device-JWT bearer when available (same write-path attach the
    // fs_observer producer uses; collapses to an anonymous send when unpaired).
    let req = crate::auth::attach_device_auth(client.post(&url));
    match req.json(body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                info!(
                    repo = %body.repo.as_deref().unwrap_or("?"),
                    path = %body.canonical_path,
                    n = body.observations.len(),
                    "fs_backstop: ALARM — canonical checkout dirty + unclaimed, posted canonical_drift"
                );
            } else {
                let txt = resp.text().await.unwrap_or_default();
                warn!(
                    url = %url,
                    status = status.as_u16(),
                    body = %txt,
                    "fs_backstop: coord returned non-2xx (soft failure)"
                );
            }
        }
        Err(e) => {
            debug!(url = %url, error = %e, "fs_backstop: post failed (coord unreachable)");
        }
    }
}

/// One backstop cycle: scan governed canonical checkouts, and for each that is
/// dirty + unclaimed with reportable observations, POST a `canonical_drift`
/// alarm. Gated on [`fs_backstop_enabled`] — a no-op (returns `Ok`) when off.
///
/// Returns `Ok(())` on a clean skip OR after best-effort POSTs (a coord
/// failure is swallowed inside [`post_canonical_drift`], never surfaced). The
/// disk-walk + git runs under `spawn_blocking`; the claim lookups + POSTs run
/// async.
pub async fn tick_once() -> Result<(), String> {
    if !fs_backstop_enabled() {
        return Ok(());
    }
    let device_id = match super::census::load_device_id_pub() {
        Some(id) => id.to_string(),
        None => {
            debug!("fs_backstop: no device_id — skipping");
            return Ok(());
        }
    };
    let coord_base = match super::census::coord_http_base_pub() {
        Some(b) => b,
        None => {
            debug!("fs_backstop: no coord_url configured — skipping");
            return Ok(());
        }
    };
    let root = match super::census::qontinui_root() {
        Some(r) => r,
        None => {
            debug!("fs_backstop: no qontinui-root dir resolved — skipping");
            return Ok(());
        }
    };

    // Disk-walk + git (status probe + libgit2 diff per dirty checkout) is a
    // synchronous multi-second walk — run it off the async worker, matching
    // the census/reclaim pattern.
    let candidates = tokio::task::spawn_blocking(move || scan_drift_candidates(&root))
        .await
        .map_err(|e| format!("fs_backstop scan panicked: {e}"))?;

    let tenant_id = resolve_tenant_id();

    for c in candidates {
        // Claim lookup — an ERROR / coord-unreachable SUPPRESSES (can't prove
        // unclaimed). Only a proven `null` holder lets the alarm through.
        let holder = match canonical_checkout_claim_holder(&coord_base, &c.resource_key).await {
            Ok(h) => h,
            Err(e) => {
                debug!(
                    repo = %c.repo,
                    key = %c.resource_key,
                    error = %e,
                    "fs_backstop: claim lookup failed — suppressing (can't prove unclaimed)"
                );
                continue;
            }
        };
        if !should_alarm(true, holder.as_ref(), c.observations.len()) {
            debug!(
                repo = %c.repo,
                key = %c.resource_key,
                claimed = holder.is_some(),
                "fs_backstop: suppressed (covered by a live worktree claim)"
            );
            continue;
        }
        let body = CanonicalDriftRequest {
            source: CANONICAL_DRIFT_SOURCE,
            device_id: device_id.clone(),
            repo: Some(c.repo.clone()),
            canonical_path: super::worktree_resource_key(&c.canonical_path),
            head_sha: c.head_sha,
            current_branch: c.current_branch,
            tenant_id,
            observations: c.observations,
        };
        post_canonical_drift(&coord_base, &body).await;
    }
    Ok(())
}

/// Spawn the periodic backstop poller on the ambient tokio runtime. Interval
/// from [`FS_BACKSTOP_INTERVAL_ENV`] (default 300s, floor 30s).
/// `MissedTickBehavior::Skip` + warn-and-retry, like
/// [`super::census::spawn_census`] / [`super::reclaim::spawn_reclaim`]. The
/// loop never panics; the gate is re-checked every tick inside
/// [`tick_once`] so flipping the flag at runtime takes effect.
pub fn spawn_backstop() {
    let secs = interval_secs();
    info!(
        "fs_backstop: starting periodic backstop poller, interval={}s (flag {} gates each tick)",
        secs, FS_BACKSTOP_FLAG_ENV
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = tick_once().await {
                warn!("fs_backstop: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_worktree::canonical_paths::default_canonical_path;
    use crate::agent_worktree::worktree_resource_key;

    #[test]
    fn enabled_is_false_by_default() {
        // Holds as long as no concurrent test set the flag (env is global).
        if std::env::var(FS_BACKSTOP_FLAG_ENV).is_err() {
            assert!(!fs_backstop_enabled());
        }
    }

    #[test]
    fn interval_defaults_and_floors() {
        // With the env unset, the default applies.
        if std::env::var(FS_BACKSTOP_INTERVAL_ENV).is_err() {
            assert_eq!(interval_secs(), DEFAULT_FS_BACKSTOP_INTERVAL_SECS);
        }
        // Floor is below the default — sanity on the constants (const block so
        // clippy doesn't flag the constant-value assertion).
        const { assert!(FS_BACKSTOP_INTERVAL_FLOOR_SECS <= DEFAULT_FS_BACKSTOP_INTERVAL_SECS) };
    }

    #[test]
    fn parse_holder_null_is_none() {
        assert!(parse_by_resource_holder(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn parse_holder_object_is_some() {
        let v = serde_json::json!({
            "machine_id": "11111111-1111-1111-1111-111111111111",
            "session_id": "22222222-2222-2222-2222-222222222222",
        });
        let h = parse_by_resource_holder(&v).expect("object => Some");
        assert_eq!(
            h.machine_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            h.session_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
    }

    #[test]
    fn parse_holder_object_missing_fields_still_some() {
        // Any object => a holder => SUPPRESS, even if it omits ids.
        let v = serde_json::json!({});
        let h = parse_by_resource_holder(&v);
        assert!(h.is_some(), "an empty object is still a holder");
        let h = h.unwrap();
        assert!(h.machine_id.is_none());
        assert!(h.session_id.is_none());
    }

    #[test]
    fn should_alarm_truth_table() {
        let holder = Holder {
            machine_id: Some("m".into()),
            session_id: None,
        };
        // dirty + null holder + obs > 0 => ALARM.
        assert!(should_alarm(true, None, 1));
        // dirty + a holder (claimed, incl. SharedBranch self-claim) => suppress.
        assert!(!should_alarm(true, Some(&holder), 1));
        // clean => suppress regardless of holder/obs.
        assert!(!should_alarm(false, None, 1));
        assert!(!should_alarm(false, Some(&holder), 1));
        // dirty + null holder but zero observations => suppress.
        assert!(!should_alarm(true, None, 0));
    }

    #[test]
    fn request_serializes_to_coord_contract() {
        // Build the canonical path portably (no hardcoded d:/qontinui-root
        // literal) so the test runs on every platform.
        let canonical = default_canonical_path("qontinui-runner").unwrap();
        let key = worktree_resource_key(&canonical);
        let req = CanonicalDriftRequest {
            source: CANONICAL_DRIFT_SOURCE,
            device_id: "dev-1".into(),
            repo: Some("qontinui-runner".into()),
            canonical_path: key.clone(),
            head_sha: "abc123".into(),
            current_branch: Some("main".into()),
            tenant_id: None,
            observations: vec![FsObservation {
                path: "src/leaked.rs".into(),
                pre_sha: Some("aaa".into()),
                post_sha: "bbb".into(),
                hunks: serde_json::json!([]),
            }],
        };
        let v = serde_json::to_value(&req).expect("serialize");
        // The distinguishing tag coord (eventually) alarms on.
        assert_eq!(v["source"], "canonical_drift");
        assert_eq!(v["device_id"], "dev-1");
        assert_eq!(v["repo"], "qontinui-runner");
        assert_eq!(v["canonical_path"], key);
        assert_eq!(v["head_sha"], "abc123");
        assert_eq!(v["current_branch"], "main");
        // tenant_id omitted (skip-if-none) — absent, not null.
        assert!(v.get("tenant_id").is_none());
        // observations reuse the fs_observer FsObservation shape.
        let obs = v["observations"].as_array().unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0]["path"], "src/leaked.rs");
        assert_eq!(obs[0]["pre_sha"], "aaa");
        assert_eq!(obs[0]["post_sha"], "bbb");
    }

    #[tokio::test]
    async fn tick_once_is_ok_noop_when_flag_off() {
        // With the flag off (default), tick_once short-circuits to Ok without
        // touching disk / coord. Only assert when the flag isn't set by a
        // concurrent test (env is global).
        if std::env::var(FS_BACKSTOP_FLAG_ENV).is_err() {
            assert!(tick_once().await.is_ok());
        }
    }
}
