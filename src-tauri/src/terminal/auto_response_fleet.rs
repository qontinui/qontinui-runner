//! Fleet auto-response rule fetch loop + on-disk cache.
//!
//! Polls coord (`GET /coord/policies/runner-rules`, device-JWT auth) for the
//! operator-configured auto-response rule set, caches the result on disk, and
//! hands it to [`super::auto_response::reload_rules`]. The on-disk cache lets
//! the runner re-arm the rules immediately at boot (before the first successful
//! fetch) so a session that hit the transient rate-limit message during a
//! runner restart is still recovered.
//!
//! Phase 4 (unified-automation): the rule source moved from the qontinui-web
//! backend to coord, and rules now carry a tagged `action` — either a fixed
//! `submit_prompt` (the offline-capable auto-continue) or a `resolve_by_scoring`
//! that defers the response text to coord's LLM-judge resolve endpoint. The
//! built-in rate-limit rule is now SERVER-seeded in coord (system-tenant
//! default) and arrives over the wire — it is no longer hardcoded here.
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

use crate::settings::BackoffConfig;

/// The action a fleet rule takes when its regex matches.
///
/// Tagged on `kind` to match the coord projection. `submit_prompt` is the
/// fully-offline auto-continue (a fixed text submitted into the session);
/// `resolve_by_scoring` defers the response text to coord's resolve endpoint
/// (an LLM-judge over scored policy options) and injects nothing if coord can't
/// confidently resolve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    /// Submit a fixed text into the matched session. Offline-capable.
    SubmitPrompt { text: String },
    /// Resolve the response via coord's scoring/judge endpoint for `policy_id`.
    ResolveByScoring { policy_id: String },
}

/// A fleet rule as projected by `GET /coord/policies/runner-rules`.
///
/// `case_insensitive` and `backoff` may be omitted (→ `false` / default). The
/// `action` is normally present, but a pre-upgrade on-disk cache (written by the
/// PR-#571 shape) has no `action` and a bare `prompt` field instead; the custom
/// [`Deserialize`] below maps that stale shape to `SubmitPrompt { text: prompt }`
/// so a version-bump cache load still arms the offline auto-continue rather than
/// failing the whole cache.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetRule {
    pub id: String,
    pub pattern: String,
    pub case_insensitive: bool,
    pub backoff: BackoffConfig,
    pub action: RuleAction,
}

impl<'de> Deserialize<'de> for FleetRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Permissive raw shape: accepts the coord projection (`action`) AND a
        /// stale PR-#571 cache row (bare `prompt`, no `action`).
        #[derive(Deserialize)]
        struct Raw {
            id: String,
            pattern: String,
            #[serde(default)]
            case_insensitive: bool,
            #[serde(default)]
            backoff: BackoffConfig,
            #[serde(default)]
            action: Option<RuleAction>,
            /// Legacy field from the pre-Phase-4 on-disk cache.
            #[serde(default)]
            prompt: Option<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let action = match raw.action {
            Some(a) => a,
            // Stale cache: synthesize the offline auto-continue from `prompt`.
            None => RuleAction::SubmitPrompt {
                text: raw.prompt.unwrap_or_default(),
            },
        };
        Ok(FleetRule {
            id: raw.id,
            pattern: raw.pattern,
            case_insensitive: raw.case_insensitive,
            backoff: raw.backoff,
            action,
        })
    }
}

/// Wire shape of `GET /coord/policies/runner-rules`. Coord returns only the
/// applicable rules; the `ETag` is delivered as an HTTP response header (read
/// from the header, not the body).
#[derive(Deserialize)]
struct FleetRulesResponse {
    rules: Vec<FleetRule>,
}

/// On-disk cache of the last successful fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CachedFleetRules {
    etag: Option<String>,
    fetched_at: String,
    rules: Vec<FleetRule>,
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

/// Rules endpoint on coord (`{coord_base}/coord/policies/runner-rules`).
///
/// Resolves the coord base via the shared resolver (`COORD_HTTP_URL` env →
/// active profile `coord_url`, ws→http) — the SAME path every other coord
/// reader uses. Returns `None` when nothing is configured, so the fetch tick
/// cleanly skips (no localhost fallback, no web URL).
fn rules_endpoint_url() -> Option<String> {
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(format!(
            "{}/coord/policies/runner-rules",
            base.trim_end_matches('/')
        )),
        _ => None,
    }
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
    let Some(url) = rules_endpoint_url() else {
        debug!("auto_response_fleet: no coord base configured — skipping fetch");
        return Ok(());
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;

    // Device-JWT bearer attached the same way every coord reader does.
    let mut req = crate::coord_http::coord_get(&client, &url);
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
    use crate::settings::BackoffConfig;

    fn sample_rule() -> FleetRule {
        FleetRule {
            id: "r1".to_string(),
            pattern: "rate limited".to_string(),
            case_insensitive: false,
            backoff: BackoffConfig::default(),
            action: RuleAction::SubmitPrompt {
                text: "please continue".to_string(),
            },
        }
    }

    #[test]
    fn coord_projection_deserializes_both_action_kinds() {
        let body = r#"{
            "rules": [
                {
                    "id": "rate-limit",
                    "pattern": "(?i)rate limited",
                    "case_insensitive": true,
                    "backoff": {"initial_delay_secs": 60, "multiplier": 2.0, "max_delay_secs": null},
                    "action": {"kind": "submit_prompt", "text": "Please continue."}
                },
                {
                    "id": "needs-judge",
                    "pattern": "which option",
                    "action": {"kind": "resolve_by_scoring", "policy_id": "11111111-1111-1111-1111-111111111111"}
                }
            ]
        }"#;
        let parsed: FleetRulesResponse = serde_json::from_str(body).expect("coord body parses");
        assert_eq!(parsed.rules.len(), 2);

        assert_eq!(parsed.rules[0].id, "rate-limit");
        assert!(parsed.rules[0].case_insensitive);
        assert_eq!(
            parsed.rules[0].action,
            RuleAction::SubmitPrompt {
                text: "Please continue.".to_string()
            }
        );

        assert_eq!(parsed.rules[1].id, "needs-judge");
        // Omitted case_insensitive / backoff default cleanly.
        assert!(!parsed.rules[1].case_insensitive);
        assert_eq!(parsed.rules[1].backoff, BackoffConfig::default());
        assert_eq!(
            parsed.rules[1].action,
            RuleAction::ResolveByScoring {
                policy_id: "11111111-1111-1111-1111-111111111111".to_string()
            }
        );
    }

    #[test]
    fn stale_cache_without_action_maps_prompt_to_submit_prompt() {
        // A pre-Phase-4 (PR #571) cache row: bare `prompt`, no `action`, plus
        // dropped legacy fields (`name`, `enabled`) that must be tolerated.
        let stale = r#"{
            "etag": "W/\"old\"",
            "fetched_at": "2026-06-13T00:00:00Z",
            "rules": [
                {
                    "id": "rate-limit",
                    "name": "rate-limit-recover",
                    "pattern": "rate limited",
                    "prompt": "please continue",
                    "enabled": true,
                    "backoff": {"initial_delay_secs": 60, "multiplier": 2.0, "max_delay_secs": null}
                }
            ]
        }"#;
        let cached: CachedFleetRules =
            serde_json::from_str(stale).expect("stale cache still loads");
        assert_eq!(cached.rules.len(), 1);
        assert_eq!(
            cached.rules[0].action,
            RuleAction::SubmitPrompt {
                text: "please continue".to_string()
            },
            "absent action should synthesize SubmitPrompt from the legacy prompt"
        );
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
