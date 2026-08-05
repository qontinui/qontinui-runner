//! Coord I/O for the Tier-0 looping-agent supervisor — the impure half of
//! Phase 8 (plan `2026-07-17-session-autonomy-fabric.md`).
//!
//! Everything here is a thin, fail-safe HTTP shim over coord; every DECISION it
//! feeds lives in the pure cores (`looping_agent::{desired_state, lease}`).
//! Three concerns:
//!
//! 1. **Desired state** — a poll of `GET /coord/agent-desired-state` behind a
//!    process-global cache, mirroring `mcp/fleet_policy_poller.rs`'s
//!    poll-into-process-global-cache shape (45s; NOT ETag — coord serves no
//!    validators on this route).
//! 2. **Slot leases** — `POST /claims/{acquire,heartbeat,release}`, the
//!    claim-first primitive (D3).
//! 3. **Playbook** — `GET /coord/agent-playbook/:name`, so coord's versioned
//!    document is the shepherd's source of truth and the runner-bundled copy is
//!    only the offline fallback.
//!
//! ## Why the cache is refreshed from the tick, not a supervised task
//!
//! `fleet_policy_poller` needs its own task because its cache is read
//! SYNCHRONOUSLY from a non-async call site with no app state. This one has no
//! such constraint: its only reader is the supervisor's own async tick. Folding
//! the refresh into that tick keeps the cadence in one place and avoids a
//! second supervised task whose death would silently freeze desired state at a
//! stale value — the exact failure the poller's own supervisor rationale warns
//! about. The tick still enforces the 45s cadence, so coord sees the same poll
//! volume either way.

use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use tracing::{debug, warn};

use qontinui_runner_lib::looping_agent::desired_state::CoordDesiredState;
use qontinui_runner_lib::looping_agent::lease::{
    parse_acquire_result, parse_heartbeat_result, AcquireOutcome, HeartbeatOutcome,
    LEASE_CLAIM_KIND, LEASE_TTL_SECS,
};

/// How often the desired state is refreshed. 45s matches
/// `fleet_policy_poller::POLL_INTERVAL` — long enough not to hammer coord,
/// short enough that an operator arming the fleet sees it inside a minute.
const DESIRED_STATE_TTL: Duration = Duration::from_secs(45);

/// Per-request timeout. Bounded well under the 5s supervisor tick so a hung
/// coord can never stall the whole supervision loop (an unreachable coord must
/// degrade the supervisor, not freeze it).
const HTTP_TIMEOUT: Duration = Duration::from_secs(4);

/// The cached desired state + when it was fetched.
///
/// `rules: None` means "we do not currently know" — either we have never
/// polled, or the last poll FAILED. Both collapse to the same fail-safe answer
/// (the pure resolver falls back to the local registry posture), which is why
/// they need not be distinguished: an unreachable coord is never permission.
#[derive(Debug, Clone, Default)]
struct DesiredCache {
    rules: Option<Vec<CoordDesiredState>>,
    fetched_at: Option<std::time::Instant>,
}

static DESIRED_CACHE: OnceLock<RwLock<DesiredCache>> = OnceLock::new();

fn cache() -> &'static RwLock<DesiredCache> {
    DESIRED_CACHE.get_or_init(|| RwLock::new(DesiredCache::default()))
}

/// The module's coord client, built once.
///
/// Every call site here shares one uniform [`HTTP_TIMEOUT`], so the deadline
/// stays baked into the client and callers are unchanged. What must NOT happen
/// is rebuilding it per request: a `reqwest::Client` owns a connection pool and
/// a DNS resolver, so per-call construction means a TLS handshake and a
/// blocking-pool DNS slot every time — the churn that starved coord requests
/// into spurious timeouts. See [`crate::coord_http::coord_client`].
fn http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| "build http client".to_string())
}

fn coord_base() -> Result<String, String> {
    crate::mcp::agent_worktrees::coord_http_base().map(|b| b.trim_end_matches('/').to_string())
}

// ===========================================================================
// Desired state
// ===========================================================================

/// Return the tenant's desired-state rules, refreshing the cache when stale.
///
/// `None` = unknown (never polled, or the last poll failed) → the pure resolver
/// falls back to the local registry. Never returns a partial/guessed answer.
pub async fn desired_state_rules() -> Option<Vec<CoordDesiredState>> {
    let stale = {
        let guard = cache().read().ok()?;
        match guard.fetched_at {
            Some(at) => at.elapsed() >= DESIRED_STATE_TTL,
            None => true,
        }
    };

    if stale {
        let fetched = fetch_desired_state().await;
        if let Ok(mut guard) = cache().write() {
            match fetched {
                Ok(rules) => {
                    guard.rules = Some(rules);
                }
                Err(e) => {
                    // Fail-safe: forget what we knew rather than act on a stale
                    // ARM. Keeping a last-good `armed=true` across an outage
                    // would let a runner spawn agents against a declaration the
                    // operator may have since revoked — and we cannot tell the
                    // difference from here.
                    debug!("looping_agent_coord: desired-state poll failed ({e}) — falling back to the local registry posture");
                    guard.rules = None;
                }
            }
            guard.fetched_at = Some(std::time::Instant::now());
        }
    }

    cache().read().ok()?.rules.clone()
}

/// One `GET /coord/agent-desired-state`.
async fn fetch_desired_state() -> Result<Vec<CoordDesiredState>, String> {
    let base = coord_base()?;
    let url = format!("{base}/coord/agent-desired-state");
    let client = http_client()?;

    let resp = crate::auth::attach_device_auth(client.get(&url))
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("coord status {}", status.as_u16()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    let rules = body
        .get("rules")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "response has no `rules` array".to_string())?;

    Ok(rules
        .iter()
        .filter_map(|r| {
            Some(CoordDesiredState {
                role: r.get("role")?.as_str()?.to_string(),
                // A malformed count/armed degrades to the fail-safe (0 /
                // disarmed) rather than dropping the rule silently.
                count: r.get("count").and_then(|c| c.as_u64()).unwrap_or(0) as u32,
                armed: r.get("armed").and_then(|a| a.as_bool()).unwrap_or(false),
            })
        })
        .collect())
}

/// Drop the cached desired state — forces the next tick to re-poll. Used by the
/// tests and available to a future "arm now" push path.
#[cfg(test)]
pub fn invalidate_desired_state_cache() {
    if let Ok(mut g) = cache().write() {
        *g = DesiredCache::default();
    }
}

// ===========================================================================
// Slot leases (claim-first — D3)
// ===========================================================================

/// Body shared by `/claims/{acquire,heartbeat,release}`.
///
/// `agent_session_id` is deliberately `null` (a bare-machine owner token).
///
/// This is a REAL design decision, not an omission. Coord folds
/// `agent_session_id` into the claim's owner token (`machine:session`), so
/// passing the shepherd's session id would make the lease belong to THAT
/// SESSION. The supervisor relaunches its shepherd with a fresh `--session-id`
/// on every context-low / K-cycle recycle — so on the very next relaunch the
/// supervisor's own re-acquire would come back `held` (by its own dead
/// session's token) and the slot would deadlock behind a ghost until TTL.
///
/// The lease belongs to the SUPERVISOR (one per runner, machine-scoped), not to
/// the session it supervises — exactly the "machine-level acquirers (daemons,
/// the merge scheduler) pass None" case the claim client documents. The
/// shepherd's session id still travels in `metadata` for the audit trail.
fn claim_body(
    resource_key: &str,
    machine_id: &uuid::Uuid,
    agent_id: &str,
    claude_session_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": LEASE_CLAIM_KIND,
        "resource_key": resource_key,
        "machine_id": machine_id.to_string(),
        "agent_session_id": serde_json::Value::Null,
        "ttl_seconds": LEASE_TTL_SECS,
        "metadata": {
            "agent_id": agent_id,
            "claude_session_id": claude_session_id,
            "source": "looping_agent_supervisor",
            "plan": "2026-07-17-session-autonomy-fabric Phase 8",
        },
    })
}

/// `POST /claims/acquire` for one slot. Any failure is
/// [`AcquireOutcome::Unavailable`] — NEVER permission.
pub async fn acquire_slot(
    resource_key: &str,
    machine_id: &uuid::Uuid,
    agent_id: &str,
    claude_session_id: Option<&str>,
) -> AcquireOutcome {
    let base = match coord_base() {
        Ok(b) => b,
        Err(e) => return AcquireOutcome::Unavailable(format!("coord base unresolved: {e}")),
    };
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return AcquireOutcome::Unavailable(e),
    };
    let url = format!("{base}/claims/acquire");
    let body = claim_body(resource_key, machine_id, agent_id, claude_session_id);

    let resp = match crate::auth::attach_device_auth(client.post(&url).json(&body))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return AcquireOutcome::Unavailable(format!("POST {url}: {e}")),
    };

    let status = resp.status();
    // 409 CONFLICT carries a real `result` body (topic conflicts); coord returns
    // 200 for a plain `held`. Anything else is a transport-class failure.
    if !status.is_success() && status != reqwest::StatusCode::CONFLICT {
        return AcquireOutcome::Unavailable(format!("coord status {}", status.as_u16()));
    }
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return AcquireOutcome::Unavailable(format!("decode: {e}")),
    };
    match v.get("result").and_then(|r| r.as_str()) {
        Some(result) => parse_acquire_result(
            result,
            v.get("current_holder")
                .and_then(|s| s.as_str())
                .map(str::to_string),
        ),
        None => AcquireOutcome::Unavailable("body missing `result` discriminator".to_string()),
    }
}

/// `POST /claims/heartbeat` on a lease we believe we hold.
pub async fn heartbeat_slot(
    resource_key: &str,
    machine_id: &uuid::Uuid,
    agent_id: &str,
    claude_session_id: Option<&str>,
) -> HeartbeatOutcome {
    let base = match coord_base() {
        Ok(b) => b,
        Err(e) => return HeartbeatOutcome::Unavailable(format!("coord base unresolved: {e}")),
    };
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return HeartbeatOutcome::Unavailable(e),
    };
    let url = format!("{base}/claims/heartbeat");
    let body = claim_body(resource_key, machine_id, agent_id, claude_session_id);

    let resp = match crate::auth::attach_device_auth(client.post(&url).json(&body))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return HeartbeatOutcome::Unavailable(format!("POST {url}: {e}")),
    };
    if !resp.status().is_success() {
        return HeartbeatOutcome::Unavailable(format!("coord status {}", resp.status().as_u16()));
    }
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return HeartbeatOutcome::Unavailable(format!("decode: {e}")),
    };
    match v.get("result").and_then(|r| r.as_str()) {
        Some(result) => parse_heartbeat_result(result),
        None => HeartbeatOutcome::Unavailable("body missing `result` discriminator".to_string()),
    }
}

/// `POST /claims/release` — best-effort. Frees the slot IMMEDIATELY on a clean
/// stop so a peer can fill it without waiting out the TTL. A failure is
/// harmless: the TTL is the backstop.
pub async fn release_slot(resource_key: &str, machine_id: &uuid::Uuid, agent_id: &str) {
    let (Ok(base), Ok(client)) = (coord_base(), http_client()) else {
        return;
    };
    let url = format!("{base}/claims/release");
    let body = claim_body(resource_key, machine_id, agent_id, None);
    match crate::auth::attach_device_auth(client.post(&url).json(&body))
        .send()
        .await
    {
        Ok(r) => debug!(
            "looping_agent_coord: released {resource_key} (coord status {})",
            r.status().as_u16()
        ),
        Err(e) => warn!("looping_agent_coord: release {resource_key} failed (best-effort): {e}"),
    }
}

// ===========================================================================
// Playbook
// ===========================================================================

/// `GET /coord/agent-playbook/:name` — the coord-served playbook body.
///
/// `None` on ANY failure (unreachable / 404 / auth / decode), which the pure
/// [`qontinui_runner_lib::looping_agent::playbook::resolve_playbook`] turns
/// into the bundled fallback. A shepherd must always spawn with real
/// instructions, even offline.
pub async fn fetch_playbook(name: &str) -> Option<String> {
    let base = coord_base().ok()?;
    let client = http_client().ok()?;
    let url = format!("{base}/coord/agent-playbook/{name}");

    let resp = crate::auth::attach_device_auth(client.get(&url))
        .send()
        .await
        .map_err(|e| debug!("looping_agent_coord: GET {url}: {e}"))
        .ok()?;
    if !resp.status().is_success() {
        debug!(
            "looping_agent_coord: playbook {name} unavailable (coord status {}) — \
             using the bundled fallback",
            resp.status().as_u16()
        );
        return None;
    }
    let doc: serde_json::Value = resp.json().await.ok()?;
    doc.get("body").and_then(|b| b.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_body_uses_a_machine_scoped_owner_token() {
        let machine = uuid::Uuid::nil();
        let body = claim_body(
            "role:t:merge_shepherd:0",
            &machine,
            "merge-shepherd",
            Some("sess-1"),
        );

        // THE assertion: agent_session_id MUST be null. Folding the shepherd's
        // session into the owner token would deadlock the slot behind the
        // supervisor's own ghost on the next fresh-session relaunch.
        assert_eq!(
            body["agent_session_id"],
            serde_json::Value::Null,
            "the lease belongs to the SUPERVISOR (machine-scoped), not to the session it \
             supervises — a session-scoped token would deadlock every relaunch"
        );
        assert_eq!(body["kind"], LEASE_CLAIM_KIND);
        assert_eq!(body["resource_key"], "role:t:merge_shepherd:0");
        assert_eq!(body["machine_id"], machine.to_string());

        // The TTL is explicit, not semantic_resource's 1800s default (which
        // would strand a dead runner's slot for 30 minutes).
        assert_eq!(body["ttl_seconds"], LEASE_TTL_SECS);
        assert_eq!(body["ttl_seconds"], 120);

        // The session id still rides along for the audit trail.
        assert_eq!(body["metadata"]["claude_session_id"], "sess-1");
        assert_eq!(body["metadata"]["agent_id"], "merge-shepherd");
    }

    #[test]
    fn claim_body_tolerates_a_not_yet_spawned_session() {
        let body = claim_body(
            "role:t:merge_shepherd:0",
            &uuid::Uuid::nil(),
            "merge-shepherd",
            None,
        );
        // Claim-first means we acquire BEFORE a session exists — the body must
        // be well-formed with no session id at all.
        assert_eq!(
            body["metadata"]["claude_session_id"],
            serde_json::Value::Null
        );
        assert_eq!(body["agent_session_id"], serde_json::Value::Null);
    }

    // NOTE: there is deliberately NO test here that points `COORD_HTTP_URL` at
    // a dead port to exercise the unreachable path. `env::set_var` races the
    // parallel test harness (a known flaky-red source in this workspace —
    // runner PR #753), and the behavior it would assert is already pinned
    // where it actually lives: the pure `lease::spawn_permitted` /
    // `desired_state::resolve` / `playbook::resolve_playbook` tests prove that
    // Unavailable-and-None are never permission. The shim's only job is to map
    // every failure onto those values, which the `parse_*` tests cover.
}
