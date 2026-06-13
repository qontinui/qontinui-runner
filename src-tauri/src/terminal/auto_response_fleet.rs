//! Fleet auto-response rule fetch loop + on-disk cache.
//!
//! Polls the qontinui-web backend (`GET /api/v1/runner/auto-response-rules`,
//! device-JWT auth) for the operator-configured auto-response rule set, caches
//! the result on disk, and hands it to [`super::auto_response::reload_rules`].
//! The on-disk cache lets the runner re-arm the rules immediately at boot
//! (before the first successful fetch) so a session that hit the transient
//! rate-limit message during a runner restart is still recovered.
//!
//! Best-effort throughout: every network/IO error is logged and swallowed —
//! the runner keeps whatever rules it already has and retries next tick. The
//! poll mirrors `fleet::spawn_heartbeat` (interval + `MissedTickBehavior::Skip`)
//! and the cache mirrors `fleet`'s `last_budget.json` (atomic tmp+rename).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::settings::AutoResponseRule;

/// Wire shape of `GET /api/v1/runner/auto-response-rules`. The backend returns
/// only ENABLED rules, plus an `updated_at` timestamp (the `ETag` is delivered
/// both here and as an HTTP response header; we read the header).
#[derive(Deserialize)]
struct FleetRulesResponse {
    rules: Vec<AutoResponseRule>,
    #[serde(default)]
    #[allow(dead_code)]
    updated_at: Option<String>,
}

/// On-disk cache of the last successful fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CachedFleetRules {
    etag: Option<String>,
    fetched_at: String,
    rules: Vec<AutoResponseRule>,
}

/// Last seen `ETag`, used to send `If-None-Match` and get cheap 304s.
static LAST_ETAG: Mutex<Option<String>> = Mutex::new(None);

/// Cache file location: `~/.qontinui/fleet-auto-response-rules.json`.
fn cache_path() -> Option<PathBuf> {
    cache_path_in(dirs::home_dir()?)
}

/// Cache path under an arbitrary base dir — injectable for tests.
fn cache_path_in(base: PathBuf) -> Option<PathBuf> {
    Some(
        base.join(".qontinui")
            .join("fleet-auto-response-rules.json"),
    )
}

/// Rules endpoint on the qontinui-web backend.
fn rules_endpoint_url() -> String {
    format!(
        "{}/api/v1/runner/auto-response-rules",
        crate::api_config::get_api_base_url()
    )
}

/// Read the on-disk cache from `path`, if present and parseable.
fn read_cache_at(path: &PathBuf) -> Option<CachedFleetRules> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<CachedFleetRules>(&bytes) {
        Ok(c) => Some(c),
        Err(e) => {
            debug!(error = %e, "auto_response_fleet: cache parse failed — ignoring");
            None
        }
    }
}

/// Write `cached` to `path` atomically (tmp + rename). All IO errors are
/// debug-logged and swallowed.
fn write_cache_at(path: &PathBuf, cached: &CachedFleetRules) {
    let Ok(pretty) = serde_json::to_vec_pretty(cached) else {
        debug!("auto_response_fleet: cache serialize failed");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            debug!(error = %e, "auto_response_fleet: cache mkdir failed");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &pretty) {
        debug!(error = %e, "auto_response_fleet: cache write failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        debug!(error = %e, "auto_response_fleet: cache rename failed");
    }
}

/// Fetch the rule set once and reload the engine on success. Best-effort:
/// network / non-2xx errors `warn!` and return `Ok(())` (keep current rules);
/// a 304 returns `Ok(())` unchanged.
pub async fn fetch_and_reload_once() -> Result<(), String> {
    let url = rules_endpoint_url();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;

    // Device-JWT bearer attached the same way coord_http does.
    let mut req = qontinui_runner_lib::auth::attach_device_auth(client.get(&url));
    if let Some(etag) = LAST_ETAG.lock().ok().and_then(|g| g.clone()) {
        req = req.header("If-None-Match", etag);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(url = %url, error = %e, "auto_response_fleet: fetch failed — keeping current rules");
            return Ok(());
        }
    };

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        debug!("auto_response_fleet: rules unchanged (304)");
        return Ok(());
    }
    if !status.is_success() {
        warn!(%status, url = %url, "auto_response_fleet: non-2xx — keeping current rules");
        return Ok(());
    }

    // Capture the ETag header before consuming the body.
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let parsed: FleetRulesResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "auto_response_fleet: response parse failed — keeping current rules");
            return Ok(());
        }
    };

    let cached = CachedFleetRules {
        etag: etag.clone(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        rules: parsed.rules.clone(),
    };
    if let Some(path) = cache_path() {
        write_cache_at(&path, &cached);
    }
    if let Ok(mut g) = LAST_ETAG.lock() {
        *g = etag;
    }
    info!(
        count = parsed.rules.len(),
        "auto_response_fleet: rules fetched"
    );
    crate::terminal::auto_response::reload_rules(parsed.rules);
    Ok(())
}

/// Seed the engine from the on-disk cache at boot — arms rules before the first
/// network fetch. With no cache the engine starts dormant (empty rule set).
pub fn reload_from_cache_at_boot() {
    let cached = cache_path().and_then(|p| read_cache_at(&p));
    match cached {
        Some(c) => {
            if let Ok(mut g) = LAST_ETAG.lock() {
                *g = c.etag.clone();
            }
            info!(
                count = c.rules.len(),
                "auto_response_fleet: seeding rules from cache at boot"
            );
            crate::terminal::auto_response::reload_rules(c.rules);
        }
        None => {
            crate::terminal::auto_response::reload_rules(vec![]);
        }
    }
}

/// Spawn the periodic fetch loop. Interval from
/// `QONTINUI_AUTO_RESPONSE_FETCH_INTERVAL_SECS` (default 300s, floored at 1s);
/// `MissedTickBehavior::Skip` mirrors `fleet::spawn_heartbeat`.
pub fn spawn_fetch_loop() {
    let secs: u64 = std::env::var("QONTINUI_AUTO_RESPONSE_FETCH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300)
        .max(1);

    info!(
        interval_secs = secs,
        "auto_response_fleet: starting periodic rule fetch loop"
    );

    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = fetch_and_reload_once().await {
                warn!(error = %e, "auto_response_fleet: fetch tick errored");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AutoResponseRule, BackoffConfig};

    fn sample_rule() -> AutoResponseRule {
        AutoResponseRule {
            id: "r1".to_string(),
            name: "rate-limit-recover".to_string(),
            pattern: "rate limited".to_string(),
            prompt: "please continue".to_string(),
            enabled: true,
            backoff: BackoffConfig::default(),
        }
    }

    #[test]
    fn cache_roundtrips() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-autoresp-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = cache_path_in(tmp.clone()).unwrap();
        let cached = CachedFleetRules {
            etag: Some("W/\"abc\"".to_string()),
            fetched_at: "2026-06-13T00:00:00Z".to_string(),
            rules: vec![sample_rule()],
        };
        write_cache_at(&path, &cached);
        let read = read_cache_at(&path).expect("cache should read back");
        assert_eq!(read, cached);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_cache_boot_is_dormant() {
        // Reloading an empty rule set must leave the engine with no rules.
        crate::terminal::auto_response::reload_rules(vec![]);
        let missing = cache_path_in(
            std::env::temp_dir().join(format!("qontinui-autoresp-none-{}", std::process::id())),
        )
        .unwrap();
        assert!(read_cache_at(&missing).is_none());
    }
}
