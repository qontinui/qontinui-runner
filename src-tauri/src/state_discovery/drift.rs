//! Drift detector (Step 6 of the observation pipeline plan).
//!
//! Periodically scores how well the currently-loaded compiled state machine
//! matches recent co-occurrence observations:
//!
//! ```text
//! fit_score = |observations where >=1 state's element set was fully present|
//!             / |recent observations|
//! ```
//!
//! Persists one `state_discovery_drift_scores` row per tick. If `fit_score`
//! stays below 0.9 across three consecutive windows (same spec_id) a WARN is
//! emitted so the State Machine page / log stream can surface "model
//! disagrees with N% of recent observations — likely refactor".
//!
//! # Resolving compiled-element -> fingerprint
//!
//! Compiled SM states store their element descriptors as ElementQuery objects
//! keyed by role/name/tag, not by the Rust `stable_element_fingerprint` hash
//! that observations carry. `save_compiled_state_machine` stashes the full
//! `requiredElements` array inside each state's `extra_metadata.requiredElements`.
//! This detector reads that array back and calls
//! `crate::state_discovery::stable_element_fingerprint` on each element JSON
//! to derive the exact same hash form used in `co_occurrence_observations.fingerprints`.
//! Any state with an empty element set is skipped — an empty set is a subset of
//! everything and would trivially match every observation.
//!
//! # spec_id resolution
//!
//! `state_machine_configs` has no `spec_id` column today. Until compiled-SM
//! rows learn to carry one, we always insert with `spec_id = NULL` and look
//! back across all NULL-spec drift scores when checking the 3-consecutive-
//! windows condition. (Comment retained in case a future migration adds the
//! column — the filter plumbing here already handles the Some/None split.)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::state_discovery::stable_element_fingerprint;
use crate::state_machine_configs::types::SmConfigFull;

/// Interval between drift ticks. 10 min matches the plan's "every N snapshots
/// or every 10 min" cadence, low enough to catch fresh refactors and high
/// enough that we don't churn the DB with tautological scores.
const TICK_INTERVAL: Duration = Duration::from_secs(600);

/// Number of most-recent observations to score per tick. Matches the K=50
/// default called out in the plan.
const WINDOW_SIZE: i32 = 50;

/// Wall-clock budget for a single tick. If fingerprinting or PG queries hang
/// past this, drop the tick and keep the loop alive.
const TICK_BUDGET: Duration = Duration::from_secs(30);

/// fit_score threshold below which the run counts as "drifting" for the
/// consecutive-windows alert. Matches the plan's 0.9.
const DRIFT_THRESHOLD: f64 = 0.9;

/// Number of consecutive drifting windows that triggers a WARN.
const CONSECUTIVE_WINDOWS: usize = 3;

/// Top-level entry point — runs forever in a `tauri::async_runtime::spawn`.
///
/// Errors inside a tick log but never kill the loop. A per-tick `timeout`
/// guards against unexpected stalls (bridge pool starvation, pathologically
/// large element sets).
pub async fn run_drift_detector(app_state: Arc<crate::commands::AppState>) {
    info!("drift-detector: starting (interval={}s)", TICK_INTERVAL.as_secs());
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    // Skip the first immediate tick — give the rest of startup a moment to
    // settle (Python bridge warming, PG pool initialising, auto-load landing).
    interval.tick().await;

    loop {
        interval.tick().await;
        match timeout(TICK_BUDGET, run_tick(&app_state)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!("drift-detector: tick failed: {}", e);
            }
            Err(_) => {
                warn!(
                    "drift-detector: tick exceeded {}s budget; skipping",
                    TICK_BUDGET.as_secs()
                );
            }
        }
    }
}

/// Run one drift-detection tick: load SM, score observations, persist row,
/// emit consecutive-drift WARN if warranted.
async fn run_tick(app_state: &Arc<crate::commands::AppState>) -> Result<(), String> {
    let pg_db = app_state.pg_db.clone();

    // 1. Load currently-active SM (same selection policy as
    //    auto_load_default_state_machine: first non-empty config).
    let selected = match load_active_sm(&pg_db).await? {
        Some(s) => s,
        None => {
            debug!("drift-detector: no compiled SM yet, skipping tick");
            return Ok(());
        }
    };

    // 2. Build state -> fingerprint-set map.
    let state_fps = build_state_fingerprint_map(&selected);
    if state_fps.is_empty() {
        debug!(
            "drift-detector: SM '{}' has no states with fingerprintable elements, skipping tick",
            selected.config.name
        );
        return Ok(());
    }

    // 3. spec_id resolution: SmConfig carries no spec_id today, so always None.
    //    (Kept as Option<&str> so future schema additions can slot in.)
    let spec_id: Option<String> = None;

    // 4. Load last K non-invalidated observations, newest first.
    let observations = load_recent_observations(&pg_db, spec_id.as_deref(), WINDOW_SIZE).await?;

    if observations.is_empty() {
        debug!("drift-detector: no recent observations, skipping insertion");
        return Ok(());
    }

    let observations_considered = observations.len() as i32;

    // 5. Score.
    let mut states_matched: i32 = 0;
    for obs_fp in &observations {
        if state_fps
            .values()
            .any(|state_fp_set| !state_fp_set.is_empty() && state_fp_set.is_subset(obs_fp))
        {
            states_matched += 1;
        }
    }
    let fit_score = (states_matched as f64) / (observations_considered as f64);

    info!(
        "drift-detector: config='{}' window={} considered={} matched={} fit_score={:.3}",
        selected.config.name,
        WINDOW_SIZE,
        observations_considered,
        states_matched,
        fit_score
    );

    // 6. Persist.
    insert_drift_score(
        &pg_db,
        spec_id.as_deref(),
        WINDOW_SIZE,
        fit_score,
        observations_considered,
        states_matched,
    )
    .await?;

    // 7. Consecutive-drift check.
    if fit_score < DRIFT_THRESHOLD {
        let below = recent_below_threshold(&pg_db, spec_id.as_deref(), CONSECUTIVE_WINDOWS).await?;
        if below >= CONSECUTIVE_WINDOWS {
            warn!(
                "drift-detector: fit_score has been below {:.2} for {} consecutive windows \
                 (latest={:.3}, spec_id={:?}, config='{}') — likely refactor; \
                 consider invalidating stale observations or re-deriving states",
                DRIFT_THRESHOLD,
                below,
                fit_score,
                spec_id,
                selected.config.name
            );
        }
    }

    Ok(())
}

/// First non-empty SM config, matching `auto_load_default_state_machine`'s
/// selection. Returns None if there is no compiled SM (common on fresh runners).
async fn load_active_sm(pg_db: &PgDb) -> Result<Option<SmConfigFull>, String> {
    let configs = pg_db
        .list_sm_configs()
        .await
        .map_err(|e| format!("list_sm_configs: {}", e))?;

    for cfg in configs.iter() {
        match pg_db.get_sm_config_full(&cfg.id).await {
            Ok(Some(full)) => {
                if !full.states.is_empty() || !full.transitions.is_empty() {
                    return Ok(Some(full));
                }
            }
            Ok(None) => continue,
            Err(e) => {
                warn!(
                    "drift-detector: failed to load full config {}: {} — skipping",
                    cfg.id, e
                );
                continue;
            }
        }
    }
    Ok(None)
}

/// For each state in the compiled SM, extract every element from
/// `extra_metadata.requiredElements`, fingerprint it, and collect the set.
///
/// States whose required-element array is missing or empty are dropped —
/// an empty fingerprint set would be a subset of every observation and
/// trivially inflate fit_score.
fn build_state_fingerprint_map(sm: &SmConfigFull) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::with_capacity(sm.states.len());
    for state in &sm.states {
        let required = state
            .extra_metadata
            .get("requiredElements")
            .and_then(|v| v.as_array());
        let required = match required {
            Some(arr) if !arr.is_empty() => arr,
            _ => continue,
        };
        let mut fps: HashSet<String> = HashSet::with_capacity(required.len());
        for el in required {
            fps.insert(stable_element_fingerprint(el));
        }
        if !fps.is_empty() {
            out.insert(state.state_id.clone(), fps);
        }
    }
    out
}

/// Last `limit` non-invalidated observations (newest first), each returned as
/// a fingerprint `HashSet`. Dropped: rows with non-array fingerprints or empty
/// fingerprint arrays (can't contribute signal).
async fn load_recent_observations(
    pg_db: &PgDb,
    spec_id: Option<&str>,
    limit: i32,
) -> Result<Vec<HashSet<String>>, String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let rows = if let Some(sid) = spec_id {
        conn.query(
            r#"SELECT fingerprints
               FROM co_occurrence_observations
               WHERE invalidated_at IS NULL
                 AND spec_id = $1
               ORDER BY captured_at DESC
               LIMIT $2::bigint"#,
            &[&sid, &(limit as i64)],
        )
        .await
    } else {
        conn.query(
            r#"SELECT fingerprints
               FROM co_occurrence_observations
               WHERE invalidated_at IS NULL
               ORDER BY captured_at DESC
               LIMIT $1::bigint"#,
            &[&(limit as i64)],
        )
        .await
    }
    .map_err(|e| format!("PG query co_occurrence_observations: {}", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let fingerprints_json: serde_json::Value = row.get(0);
        let set: HashSet<String> = match fingerprints_json.as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => continue,
        };
        if set.is_empty() {
            continue;
        }
        out.push(set);
    }
    Ok(out)
}

async fn insert_drift_score(
    pg_db: &PgDb,
    spec_id: Option<&str>,
    window_size: i32,
    fit_score: f64,
    observations_considered: i32,
    states_matched: i32,
) -> Result<(), String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let spec_id_owned: Option<String> = spec_id.map(|s| s.to_string());

    conn.execute(
        r#"INSERT INTO state_discovery_drift_scores
           (spec_id, window_size, fit_score, observations_considered, states_matched)
           VALUES ($1, $2, $3, $4, $5)"#,
        &[
            &spec_id_owned as &(dyn tokio_postgres::types::ToSql + Sync),
            &window_size,
            &fit_score,
            &observations_considered,
            &states_matched,
        ],
    )
    .await
    .map_err(|e| format!("PG insert state_discovery_drift_scores: {}", e))?;
    Ok(())
}

/// Count, among the most recent `n` drift-score rows for the given spec_id,
/// how many are below the threshold. Returns min(n, rows_below_streak). Used
/// to decide whether to emit the consecutive-windows WARN.
///
/// We check the *streak* not the count: if the most recent row is >=
/// threshold, the streak is broken and we return 0 regardless of history.
async fn recent_below_threshold(
    pg_db: &PgDb,
    spec_id: Option<&str>,
    n: usize,
) -> Result<usize, String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let limit = n as i64;
    let rows = if let Some(sid) = spec_id {
        conn.query(
            r#"SELECT fit_score FROM state_discovery_drift_scores
               WHERE spec_id = $1
               ORDER BY computed_at DESC
               LIMIT $2::bigint"#,
            &[&sid, &limit],
        )
        .await
    } else {
        conn.query(
            r#"SELECT fit_score FROM state_discovery_drift_scores
               WHERE spec_id IS NULL
               ORDER BY computed_at DESC
               LIMIT $1::bigint"#,
            &[&limit],
        )
        .await
    }
    .map_err(|e| format!("PG query state_discovery_drift_scores: {}", e))?;

    let mut streak = 0usize;
    for row in rows {
        let score: f64 = row.get(0);
        if score < DRIFT_THRESHOLD {
            streak += 1;
        } else {
            break;
        }
    }
    Ok(streak)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine_configs::types::{SmConfig, SmState};
    use serde_json::json;

    fn mk_state(id: &str, elements: serde_json::Value) -> SmState {
        SmState {
            id: id.to_string(),
            config_id: "cfg-1".to_string(),
            state_id: id.to_string(),
            name: id.to_string(),
            description: None,
            element_ids: Vec::new(),
            render_ids: Vec::new(),
            confidence: 0.9,
            acceptance_criteria: Vec::new(),
            extra_metadata: json!({ "requiredElements": elements }),
            domain_knowledge: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn mk_sm(states: Vec<SmState>) -> SmConfigFull {
        SmConfigFull {
            config: SmConfig {
                id: "cfg-1".to_string(),
                name: "Test SM".to_string(),
                description: None,
                render_count: 0,
                element_count: 0,
                include_html_ids: false,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            states,
            transitions: Vec::new(),
        }
    }

    #[test]
    fn test_build_state_fingerprint_map_skips_empty() {
        let sm = mk_sm(vec![
            mk_state("s1", json!([])),
            mk_state(
                "s2",
                json!([{"id": "x", "tagName": "button", "role": "button", "accessibleName": "Go"}]),
            ),
        ]);
        let map = build_state_fingerprint_map(&sm);
        assert!(!map.contains_key("s1"), "empty-element state must be dropped");
        assert!(map.contains_key("s2"));
        assert_eq!(map.get("s2").unwrap().len(), 1);
    }

    #[test]
    fn test_build_state_fingerprint_map_missing_required_elements() {
        // No requiredElements key at all — state should be dropped.
        let st = SmState {
            id: "s1".to_string(),
            config_id: "c".to_string(),
            state_id: "s1".to_string(),
            name: "s1".to_string(),
            description: None,
            element_ids: Vec::new(),
            render_ids: Vec::new(),
            confidence: 0.9,
            acceptance_criteria: Vec::new(),
            extra_metadata: json!({}),
            domain_knowledge: Vec::new(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        };
        let sm = mk_sm(vec![st]);
        assert!(build_state_fingerprint_map(&sm).is_empty());
    }

    /// Every element in a state fingerprints to something deterministic, and
    /// that exact hash must show up in the map.
    #[test]
    fn test_state_fingerprints_match_direct_computation() {
        let el = json!({
            "id": "submit",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Submit",
        });
        let expected = stable_element_fingerprint(&el);
        let sm = mk_sm(vec![mk_state("s1", json!([el.clone()]))]);
        let map = build_state_fingerprint_map(&sm);
        assert!(map.get("s1").unwrap().contains(&expected));
    }
}
