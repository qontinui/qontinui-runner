//! PostgreSQL tiered information queries.
//!
//! Provides PG-backed implementations for tiered info panel commands
//! including config statistics, recent runs, failed runs, flakiness data,
//! and AI session history.

use super::PgDb;
use serde_json::json;

// ============================================================================
// Automation health score — the arithmetic, split out so each definition can
// carry its own doc comment and be unit-tested without a database.
//
// Three places declare this payload and agree with each other:
// `ui_bridge_ops::AutomationHealthScore`, the uibridge spec state
// `health-score-data-display`, and `HealthScoreCard.tsx`. Where the
// declarations fix a field completely, the function below implements exactly
// that and says so. Where they name a field but leave its DENOMINATOR
// undefined, the function says **"Definition chosen, not declared"** and gives
// the reasoning, so an operator can find and correct it.
//
// EVERY RATE IS `Option<f64>`, AND A ZERO DENOMINATOR IS `None`. A ratio whose
// denominator is zero was not measured, and no number — not `0.0`, not the
// "conservative" `1.0` — may stand in for that. `overall_score` is `None`
// whenever any input term is, so the declared formula is never evaluated on an
// unknown treated as zero. The counts beside them are measured facts and stay
// bare integers, including a non-zero `total_stalls` beside a null
// `stall_frequency`.
//
// This is not a style preference. It is fleet policy
// `verification-and-evidence` `unknown-must-not-render-as-a-default`, which
// governs exactly this shape: "a NON-empty, well-formed value whose provenance
// cannot carry it. Emptiness at least looks like an absence; a confident
// default does not, so nothing downstream has any reason to doubt it."
// ============================================================================

/// `element_success_rate` — **declared**, not chosen.
///
/// Successful ÷ total `ui_bridge_events` rows with
/// `event_type = 'action_executed'` and a non-NULL `element_id`, inside the
/// window. Population and success test are identical to the shipped
/// `get_element_reliability` and `get_flaky_elements` queries, so this number
/// agrees with the other analytics routes rather than being a second opinion.
///
/// With zero interactions the ratio is 0/0 — **undefined, and reported as
/// `null`**. It is not `0.0`: a rate of zero is a measurement ("everything
/// failed"), and emitting it from an empty window is manufacturing a
/// measurement out of an absence. Fleet policy `verification-and-evidence`
/// `unknown-must-not-render-as-a-default`: "A read that cannot support its
/// answer must render UNKNOWN, not a confident-looking value."
///
/// This function used to return `0.0` there, on the reasoning that all three
/// declarations typed the field as a bare `f64`/`number` with no null arm.
/// That was the declarations being wrong together, not a constraint — so the
/// declarations moved to `Option<f64>` / `number | null` instead.
fn health_element_success_rate(successful: i64, total: i64) -> Option<f64> {
    (total > 0).then(|| successful as f64 / total as f64)
}

/// `regression_rate` — **definition chosen, not declared** (the denominator).
///
/// The declarations name the field and give it no denominator. The NUMERATOR
/// is settled: a regressed `(element_id, action)` pair is whatever the shipped
/// `get_automation_regressions` query says it is — an NTILE(2) split on time,
/// at least 4 samples, recent success rate more than 0.1 below the prior half.
///
/// The denominator chosen is the simplest defensible one: the pairs that same
/// query could have judged at all, i.e. those meeting its own
/// `COUNT(*) >= 4` eligibility gate.
///
/// ```text
/// regression_rate = regressed pairs ÷ pairs eligible for the regression test
/// ```
///
/// Reusing the existing gate means the rate cannot be diluted by pairs with
/// too little history to have had a prior at all — a corpus of one-shot
/// interactions would otherwise drive the reported regression rate toward zero
/// no matter how badly the measurable pairs degraded.
///
/// With no eligible pairs the ratio is 0/0 — undefined, and reported as
/// `null`, for the same reason as [`health_element_success_rate`]. Note that
/// this arm is reached far more often than an empty window: a corpus of
/// hundreds of interactions still has no eligible pair until some
/// `(element, action)` accumulates four samples, so `regression_rate` can be
/// unknown while the other two rates are measured. The
/// `regression_eligible_pairs` count travels in the payload so a caller can
/// see which it is.
///
/// An operator who wants a different denominator — over all pairs regardless
/// of sample count, over distinct elements, or a per-run rate — should change
/// it here.
fn health_regression_rate(regressed_pairs: i64, eligible_pairs: i64) -> Option<f64> {
    (eligible_pairs > 0).then(|| regressed_pairs as f64 / eligible_pairs as f64)
}

/// `stall_frequency` — **definition chosen, not declared** (the denominator).
///
/// The declarations name the field, render it as a percentage, and feed it to
/// `1 - stall_frequency`, so it has to land in `[0, 1]`. The NUMERATOR is
/// settled: `stall_events` rows in the window, the same population the shipped
/// `get_stall_frequency` query groups. The denominator is not declared.
///
/// The one chosen is the interaction count already being computed for this
/// payload:
///
/// ```text
/// stall_frequency = stalls ÷ interactions, clamped to at most 1.0
/// ```
///
/// — "how often automation stalled, per element interaction". The clamp is
/// load-bearing rather than cosmetic: nothing constrains a run to stall less
/// often than it interacts, and an unclamped ratio above 1 would push
/// `overall_score` below zero, out of the range the frontend's colour
/// thresholds assume.
///
/// **Zero interactions is `null`, whatever the stall count.** This function
/// used to return `1.0` — the worst value — when interactions were zero but
/// stalls were not, on the reasoning that stalling without completing an
/// interaction is not health. That reasoning is sound and the number is still
/// a fabrication: a rate whose denominator is zero was not measured, and
/// `1.0` is as confident-looking as `0.0`. A conservative guess is still a
/// guess; policy licenses no default, not merely no *flattering* one, and the
/// conservative arm belongs to the CONSUMER — a caller may treat unknown as
/// worst-case, but the producer must not decide that for every caller. The
/// facts that motivated the `1.0` are not lost: `total_stalls` reports the
/// real count beside the null, so "stalls with no interactions" is visible in
/// the payload and is exactly what a caller keys its own conservative arm on.
///
/// With zero of both it is likewise `null`, not `0.0`.
///
/// An operator who wants stalls per RUN, or per unit of wall-clock time,
/// should change it here.
fn health_stall_frequency(total_stalls: i64, total_interactions: i64) -> Option<f64> {
    (total_interactions > 0).then(|| (total_stalls as f64 / total_interactions as f64).min(1.0))
}

/// `overall_score` — **declared verbatim**, weights and all.
///
/// The uibridge spec state `health-score-data-display` states the formula in
/// its own description:
///
/// ```text
/// Score = 0.30 * element_success_rate
///       + 0.25 * (1 - regression_rate)
///       + 0.25 * (1 - stall_frequency)
///       + 0.20 base
/// ```
///
/// The weights sum to 1.00 with the base included, so a flawless window scores
/// exactly `1.0`. Nothing about the weights is a judgement call; the numbers
/// are copied from the declaration.
///
/// **The formula is not evaluated at all unless every input is known.** Any
/// `None` term makes the score `None`.
///
/// This is the fix's load-bearing arm. The formula has two "no bad news" terms
/// and a constant, so feeding it three unknowns-treated-as-zero yields
/// `0 + 0.25 + 0.25 + 0.20 = 0.70` — a score manufactured entirely out of
/// absent data, which the card rendered as a green-ish "Good". The `+ 0.20`
/// constant is what floors an empty window there. The correction is NOT to
/// re-tune the weights (they are declared, and they are right for a measured
/// window); it is to refuse to evaluate a declared formula on undeclared
/// inputs.
fn health_overall_score(
    element_success_rate: Option<f64>,
    regression_rate: Option<f64>,
    stall_frequency: Option<f64>,
) -> Option<f64> {
    Some(
        0.30 * element_success_rate?
            + 0.25 * (1.0 - regression_rate?)
            + 0.25 * (1.0 - stall_frequency?)
            + 0.20,
    )
}

/// The six counts the health-score statement returns, in one value so the
/// payload builder can be exercised without a database.
#[derive(Debug, Clone, Copy)]
struct HealthScoreCounts {
    total_interactions: i64,
    successful_interactions: i64,
    total_elements: i64,
    eligible_pairs: i64,
    regressed_pairs: i64,
    total_stalls: i64,
}

/// The window the counts were actually taken over, so the payload can state
/// its own coverage instead of leaving the caller to assume it.
#[derive(Debug, Clone, Copy)]
struct HealthScoreWindow {
    /// The `?days=` value actually applied, after the handler's default.
    days: u32,
    /// The cutoff instant the SQL filtered on, in epoch milliseconds.
    start_epoch_ms: i64,
}

/// Build the `/ui-bridge/analytics/health-score` payload from the counts and
/// the window they were taken over.
///
/// Kept separate from the query so the DECLARED contract — every field name,
/// every JSON type — is testable without a Postgres server. The route's
/// original failure was not a query failure at all: it was this payload not
/// matching `ui_bridge_ops::AutomationHealthScore`, which is a pure-data
/// property and should be caught by a test that needs nothing to be running.
///
/// # Coverage is part of the payload, not a caller's assumption
///
/// `window_days` / `window_start_epoch_ms` say what was queried;
/// `regression_eligible_pairs` exposes the one denominator that is otherwise
/// invisible in the payload (the other two are `total_interactions`); and
/// `unknown_fields` is the machine-actionable discriminator — the list of
/// field names that came back `null` and why the score did, if it did. A
/// consumer that only ever reads `overall_score` still sees `null`; a
/// consumer that wants to say *which* input was missing does not have to
/// re-derive it from the counts.
///
/// Audience profile `fleet-services`: "If it cannot state its own coverage,
/// its consumers will over-trust it", and "A service needs a typed error with
/// a code and a machine-actionable discriminator; the human rendering is a
/// projection of that, never the source."
fn health_score_payload(c: HealthScoreCounts, w: HealthScoreWindow) -> serde_json::Value {
    let element_success_rate =
        health_element_success_rate(c.successful_interactions, c.total_interactions);
    let regression_rate = health_regression_rate(c.regressed_pairs, c.eligible_pairs);
    let stall_frequency = health_stall_frequency(c.total_stalls, c.total_interactions);
    let overall_score =
        health_overall_score(element_success_rate, regression_rate, stall_frequency);

    let mut unknown_fields: Vec<&'static str> = Vec::new();
    if element_success_rate.is_none() {
        unknown_fields.push("element_success_rate");
    }
    if regression_rate.is_none() {
        unknown_fields.push("regression_rate");
    }
    if stall_frequency.is_none() {
        unknown_fields.push("stall_frequency");
    }
    if overall_score.is_none() {
        unknown_fields.push("overall_score");
    }

    json!({
        "overall_score": overall_score,
        "element_success_rate": element_success_rate,
        "regression_rate": regression_rate,
        "stall_frequency": stall_frequency,
        "total_interactions": c.total_interactions,
        "total_elements": c.total_elements,
        "total_stalls": c.total_stalls,
        "regression_eligible_pairs": c.eligible_pairs,
        "window_days": w.days,
        "window_start_epoch_ms": w.start_epoch_ms,
        "unknown_fields": unknown_fields,
    })
}

/// One `Recommendation` row — **definition chosen, not declared** (the
/// field mapping).
///
/// `ui_bridge_ops::Recommendation` declares four fields — `priority: u32`,
/// `category`, `message`, `impact` — while the SQL behind
/// `generate_recommendations` supplies two facts: an `error_type` and how
/// often it occurred. Nothing declares how one becomes the other, so:
///
/// - `priority` — 1-based rank within the query's own `ORDER BY cnt DESC`, so
///   the most frequent error type is priority 1. The declared type is `u32`
///   and no scale or direction is declared anywhere; ascending-is-more-urgent
///   is the ordinary reading of a numbered priority list.
/// - `category` — the constant `"reduce_errors"`, carried over unchanged from
///   the `"type"` key the previous mapping emitted. There is one generator, so
///   there is one category today.
/// - `message` — the human sentence the previous mapping put in `"title"`,
///   unchanged.
/// - `impact` — `"high"` above 5 occurrences, else `"medium"`. The threshold
///   is not new: it is the same `count > 5` the previous mapping already
///   used, moved off `priority` (now numeric) onto the field that actually
///   reads as a severity.
fn error_type_recommendation(rank: usize, error_type: &str, count: i64) -> serde_json::Value {
    json!({
        "priority": (rank as u32) + 1,
        "category": "reduce_errors",
        "message": format!("Address recurring '{}' errors ({} occurrences)", error_type, count),
        "impact": if count > 5 { "high" } else { "medium" },
    })
}

impl PgDb {
    /// Get recent runs, optionally filtered by config_id.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_recent_runs(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                       tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                       tra.success, tra.error_type, tra.error_message,
                       tra.actions_summary, tra.states_visited, tra.transitions_executed,
                       tra.template_matches, tra.anomalies
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = $1
                ORDER BY tra.started_at DESC
                LIMIT $2
                "#,
                &[&cid, &limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_recent_runs", &e))?
        } else {
            conn.query(
                r#"
                SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                       tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                       tra.success, tra.error_type, tra.error_message,
                       tra.actions_summary, tra.states_visited, tra.transitions_executed,
                       tra.template_matches, tra.anomalies
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                ORDER BY tra.started_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_recent_runs", &e))?
        };

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "config_id": row.get::<_, Option<String>>(1),
                    "workflow_name": row.get::<_, Option<String>>(2),
                    "started_at": row.get::<_, Option<String>>(3),
                    "ended_at": row.get::<_, Option<String>>(4),
                    "duration_ms": row.get::<_, Option<i64>>(5),
                    "automation_status": row.get::<_, String>(6),
                    "success": row.get::<_, Option<bool>>(7),
                    "error_type": row.get::<_, Option<String>>(8),
                    "error_message": row.get::<_, Option<String>>(9),
                })
            })
            .collect())
    }

    /// Get AI session history (task runs), optionally filtered by config_id.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_ai_session_history(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = if let Some(cid) = config_id {
            conn.query(
                r#"
                SELECT id, task_name, created_at::TEXT, completed_at::TEXT, status,
                       sessions_count, task_type
                FROM task_runs
                WHERE config_id = $1
                ORDER BY created_at DESC
                LIMIT $2
                "#,
                &[&cid, &limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_ai_session_history", &e))?
        } else {
            conn.query(
                r#"
                SELECT id, task_name, created_at::TEXT, completed_at::TEXT, status,
                       sessions_count, task_type
                FROM task_runs
                ORDER BY created_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_ai_session_history", &e))?
        };

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "task_name": row.get::<_, String>(1),
                    "created_at": row.get::<_, String>(2),
                    "completed_at": row.get::<_, Option<String>>(3),
                    "status": row.get::<_, String>(4),
                    "sessions_count": row.get::<_, i32>(5),
                    "task_type": row.get::<_, Option<String>>(6),
                })
            })
            .collect())
    }

    /// Cleanup old automation records for a config, keeping the most recent N.
    pub async fn cleanup_old_runs(&self, config_id: &str, keep_count: u32) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let keep_i64 = keep_count as i64;

        let count = conn
            .execute(
                r#"
                DELETE FROM task_run_automation
                WHERE id IN (
                    SELECT tra.id FROM task_run_automation tra
                    INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                    WHERE tr.config_id = $1
                    ORDER BY tra.started_at DESC
                    OFFSET $2
                )
                "#,
                &[&config_id, &keep_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG cleanup_old_runs", &e))?;

        Ok(count as u32)
    }

    /// Get recent task runs (for list_recent_task_runs in testing.rs).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn list_recent_task_runs_pg(
        &self,
        limit: i32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"
                SELECT
                    tr.id,
                    tr.task_name,
                    tr.workflow_name,
                    tr.status,
                    tr.created_at::TEXT,
                    tr.completed_at::TEXT,
                    tr.goal_achieved,
                    CASE
                        WHEN tr.completed_at IS NOT NULL
                        THEN EXTRACT(EPOCH FROM (tr.completed_at - tr.created_at))::BIGINT * 1000
                        ELSE NULL
                    END as duration_ms
                FROM task_runs tr
                ORDER BY tr.created_at DESC
                LIMIT $1
                "#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_recent_task_runs", &e))?;

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "task_name": row.get::<_, String>(1),
                    "workflow_name": row.get::<_, Option<String>>(2),
                    "status": row.get::<_, String>(3),
                    "created_at": row.get::<_, String>(4),
                    "completed_at": row.get::<_, Option<String>>(5),
                    "goal_achieved": row.get::<_, Option<bool>>(6),
                    "duration_ms": row.get::<_, Option<i64>>(7),
                })
            })
            .collect())
    }

    /// Get workflow run context for AI test generation (for get_workflow_run_context in testing.rs).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_workflow_run_context_pg(
        &self,
        task_run_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Get task run
        let task_run = conn
            .query_opt(
                r#"
                SELECT id, task_name, prompt, status, workflow_name,
                       summary, goal_achieved, remaining_work,
                       created_at::TEXT, completed_at::TEXT
                FROM task_runs WHERE id = $1
                "#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_workflow_run_context", &e))?;

        let task_run = match task_run {
            Some(row) => json!({
                "id": row.get::<_, String>(0),
                "task_name": row.get::<_, String>(1),
                "prompt": row.get::<_, Option<String>>(2),
                "status": row.get::<_, String>(3),
                "workflow_name": row.get::<_, Option<String>>(4),
                "summary": row.get::<_, Option<String>>(5),
                "goal_achieved": row.get::<_, Option<bool>>(6),
                "remaining_work": row.get::<_, Option<String>>(7),
                "created_at": row.get::<_, String>(8),
                "completed_at": row.get::<_, Option<String>>(9),
            }),
            None => return Ok(None),
        };

        // Get automation
        let automation = conn
            .query_opt(
                r#"
                SELECT workflow_name, automation_status, duration_ms,
                       actions_summary, states_visited, transitions_executed
                FROM task_run_automation WHERE task_run_id = $1
                ORDER BY started_at DESC LIMIT 1
                "#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_workflow_run_context automation", &e))?
            .map(|row| {
                json!({
                    "workflow_name": row.get::<_, Option<String>>(0),
                    "automation_status": row.get::<_, String>(1),
                    "duration_ms": row.get::<_, Option<i64>>(2),
                    "actions_summary": row.get::<_, Option<String>>(3).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "states_visited": row.get::<_, Option<String>>(4).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "transitions_executed": row.get::<_, Option<String>>(5).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                })
            });

        // Get events
        let events: Vec<serde_json::Value> = conn
            .query(
                r#"
                SELECT id, event_type, event_subtype, data, duration_ms, timestamp::TEXT
                FROM task_run_events WHERE task_run_id = $1
                ORDER BY timestamp DESC LIMIT 50
                "#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "event_type": row.get::<_, String>(1),
                    "event_subtype": row.get::<_, Option<String>>(2),
                    "data": row.get::<_, Option<String>>(3).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "duration_ms": row.get::<_, Option<i64>>(4),
                    "timestamp": row.get::<_, String>(5),
                })
            })
            .collect();

        // Get findings
        let findings: Vec<serde_json::Value> = conn
            .query(
                r#"
                SELECT id, category, title, description, severity, detected_at::TEXT
                FROM task_run_findings WHERE task_run_id = $1
                ORDER BY detected_at DESC LIMIT 20
                "#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "category": row.get::<_, String>(1),
                    "title": row.get::<_, String>(2),
                    "description": row.get::<_, Option<String>>(3),
                    "severity": row.get::<_, String>(4),
                    "detected_at": row.get::<_, String>(5),
                })
            })
            .collect();

        Ok(Some(json!({
            "task_run": task_run,
            "automation": automation,
            "events": events,
            "findings": findings,
        })))
    }

    /// List shell commands with optional filters.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn list_shell_commands_filtered(
        &self,
        enabled_only: bool,
        category: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut conditions: Vec<String> = vec!["1=1".to_string()];
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut param_idx = 1u32;

        if enabled_only {
            conditions.push("enabled = true".to_string());
        }

        if let Some(cat) = category {
            conditions.push(format!("category = ${}", param_idx));
            params.push(Box::new(cat.to_string()));
            param_idx += 1;
        }

        let _ = param_idx; // suppress unused warning

        let sql = format!(
            r#"
            SELECT id, name, description, command, working_directory,
                   timeout_seconds, fail_on_error, category, tags,
                   enabled, created_at::TEXT, updated_at::TEXT
            FROM shell_commands
            WHERE {}
            ORDER BY name ASC
            "#,
            conditions.join(" AND ")
        );

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_shell_commands", &e))?;

        Ok(rows
            .iter()
            .map(|row| {
                let tags_str: String = row.get(8);
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(json!([]));
                json!({
                    "id": row.get::<_, String>(0),
                    "name": row.get::<_, String>(1),
                    "description": row.get::<_, Option<String>>(2),
                    "command": row.get::<_, String>(3),
                    "working_directory": row.get::<_, Option<String>>(4),
                    "timeout_seconds": row.get::<_, i32>(5),
                    "fail_on_error": row.get::<_, bool>(6),
                    "category": row.get::<_, Option<String>>(7),
                    "tags": tags,
                    "enabled": row.get::<_, bool>(9),
                    "created_at": row.get::<_, String>(10),
                    "updated_at": row.get::<_, String>(11),
                })
            })
            .collect())
    }

    /// Get all distinct shell command categories.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_shell_command_categories(&self) -> Result<Vec<String>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT DISTINCT category
                FROM shell_commands
                WHERE category IS NOT NULL AND category != ''
                ORDER BY category ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_shell_command_categories", &e))?;

        Ok(rows.iter().map(|row| row.get::<_, String>(0)).collect())
    }

    /// Update a shell command.
    pub async fn update_shell_command_full(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        command: &str,
        working_directory: Option<&str>,
        timeout_seconds: i32,
        fail_on_error: bool,
        category: Option<&str>,
        tags: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                r#"
                UPDATE shell_commands SET
                    name = $1,
                    description = $2,
                    command = $3,
                    working_directory = $4,
                    timeout_seconds = $5,
                    fail_on_error = $6,
                    category = $7,
                    tags = $8,
                    enabled = $9,
                    updated_at = NOW()
                WHERE id = $10
                "#,
                &[
                    &name,
                    &description,
                    &command,
                    &working_directory,
                    &timeout_seconds,
                    &fail_on_error,
                    &category,
                    &tags,
                    &enabled,
                    &id,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG update_shell_command", &e))?;

        Ok(count > 0)
    }

    /// Set shell command enabled status.
    pub async fn set_shell_command_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let count = conn
            .execute(
                "UPDATE shell_commands SET enabled = $1, updated_at = NOW() WHERE id = $2",
                &[&enabled, &id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG set_shell_command_enabled", &e))?;

        Ok(count > 0)
    }

    /// Get prompt variant content by ID.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_prompt_variant_content(
        &self,
        variant_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT prompt_content FROM prompt_registry WHERE id = $1",
                &[&variant_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_prompt_variant_content", &e))?;

        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    /// Get pending discoveries.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_pending_discoveries(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, payload, attempt_count, error, created_at::TEXT, updated_at::TEXT
                FROM discovery_queue
                WHERE status = 'pending'
                ORDER BY created_at ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_pending_discoveries", &e))?;

        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.get::<_, String>(0),
                    "payload": row.get::<_, String>(1),
                    "attempt_count": row.get::<_, i32>(2),
                    "error": row.get::<_, Option<String>>(3),
                    "created_at": row.get::<_, String>(4),
                    "updated_at": row.get::<_, String>(5),
                })
            })
            .collect())
    }

    /// Get config statistics (PG equivalent of tiered_info::get_config_statistics).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_config_statistics(
        &self,
        config_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT config_id, total_runs, successful_runs, failed_runs,
                      avg_duration_ms, last_run_at, success_rate, streak_current, streak_best
               FROM config_statistics WHERE config_id = $1"#,
                &[&config_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_config_statistics", &e))?;
        Ok(row.map(|r| {
            json!({
                "config_id": r.get::<_, String>(0),
                "total_runs": r.get::<_, i64>(1),
                "successful_runs": r.get::<_, i64>(2),
                "failed_runs": r.get::<_, i64>(3),
                "avg_duration_ms": r.get::<_, Option<f64>>(4),
                "last_run_at": r.get::<_, Option<String>>(5),
                "success_rate": r.get::<_, Option<f64>>(6),
                "streak_current": r.get::<_, Option<i64>>(7),
                "streak_best": r.get::<_, Option<i64>>(8),
            })
        }))
    }

    /// Get flaky transitions for a config.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_flaky_transitions(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            r#"SELECT transition_id, total_attempts, failure_count,
                      failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT as failure_rate
               FROM transition_reliability
               WHERE config_id = $1 AND failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT >= $2
               ORDER BY failure_rate DESC"#,
            &[&config_id, &threshold],
        ).await.map_err(|e| crate::database::pg::pg_err("PG get_flaky_transitions", &e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "item_id": r.get::<_, String>(0),
                    "total_attempts": r.get::<_, i64>(1),
                    "failure_count": r.get::<_, i64>(2),
                    "failure_rate": r.get::<_, f64>(3),
                })
            })
            .collect())
    }

    /// Get flaky templates for a config.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_flaky_templates(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            r#"SELECT template_id, total_attempts, failure_count,
                      failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT as failure_rate
               FROM template_reliability
               WHERE config_id = $1 AND failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT >= $2
               ORDER BY failure_rate DESC"#,
            &[&config_id, &threshold],
        ).await.map_err(|e| crate::database::pg::pg_err("PG get_flaky_templates", &e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "item_id": r.get::<_, String>(0),
                    "total_attempts": r.get::<_, i64>(1),
                    "failure_count": r.get::<_, i64>(2),
                    "failure_rate": r.get::<_, f64>(3),
                })
            })
            .collect())
    }

    /// Get debugging context for a config.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_debugging_context(
        &self,
        config_id: &str,
        config_name: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Get config stats
        let stats = self
            .get_config_statistics(config_id)
            .await?
            .unwrap_or(json!({}));
        // Get recent failures
        let failures = conn
            .query(
                r#"SELECT tra.error_type, tra.error_message, tra.ended_at::TEXT
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.success = false
               ORDER BY tra.ended_at DESC LIMIT 5"#,
                &[&config_id],
            )
            .await
            .unwrap_or_default();
        let recent_errors: Vec<serde_json::Value> = failures
            .iter()
            .map(|r| {
                json!({
                    "error_type": r.get::<_, Option<String>>(0),
                    "error_message": r.get::<_, Option<String>>(1),
                    "ended_at": r.get::<_, Option<String>>(2),
                })
            })
            .collect();
        Ok(json!({
            "config_id": config_id,
            "config_name": config_name,
            "statistics": stats,
            "recent_errors": recent_errors,
        }))
    }

    /// Get failed runs for a config.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_failed_runs(
        &self,
        config_id: &str,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let limit_i64 = limit as i64;
        let rows = conn
            .query(
                r#"SELECT tra.id, tr.config_id, tra.workflow_name, tra.started_at::TEXT,
                      tra.ended_at::TEXT, tra.duration_ms, tra.automation_status,
                      tra.success, tra.error_type, tra.error_message
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.success = false
               ORDER BY tra.started_at DESC LIMIT $2"#,
                &[&config_id, &limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_failed_runs", &e))?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<_, String>(0),
                    "config_id": r.get::<_, Option<String>>(1),
                    "workflow_name": r.get::<_, Option<String>>(2),
                    "started_at": r.get::<_, Option<String>>(3),
                    "ended_at": r.get::<_, Option<String>>(4),
                    "duration_ms": r.get::<_, Option<i64>>(5),
                    "status": r.get::<_, String>(6),
                    "success": r.get::<_, Option<bool>>(7),
                    "error_type": r.get::<_, Option<String>>(8),
                    "error_message": r.get::<_, Option<String>>(9),
                })
            })
            .collect())
    }

    /// Get flakiness summary for a config.
    pub async fn get_flakiness_summary(
        &self,
        config_id: &str,
        threshold: f64,
    ) -> Result<serde_json::Value, String> {
        let flaky_transitions = self.get_flaky_transitions(config_id, threshold).await?;
        let flaky_templates = self.get_flaky_templates(config_id, threshold).await?;
        let transition_ids: Vec<String> = flaky_transitions
            .iter()
            .filter_map(|v| v["item_id"].as_str().map(String::from))
            .collect();
        let template_ids: Vec<String> = flaky_templates
            .iter()
            .filter_map(|v| v["item_id"].as_str().map(String::from))
            .collect();
        Ok(json!({
            "flaky_transition_count": flaky_transitions.len(),
            "flaky_template_count": flaky_templates.len(),
            "flaky_transition_ids": transition_ids,
            "flaky_template_ids": template_ids,
            "cache_is_stale": false,
        }))
    }

    /// Get execution options based on flakiness.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_execution_options(
        &self,
        config_id: &str,
        transition_id: Option<&str>,
        template_id: Option<&str>,
        threshold: f64,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Default execution options
        let mut options = json!({
            "retry_count": 1,
            "timeout_multiplier": 1.0,
            "confidence_threshold": 0.7,
        });
        if let Some(tid) = transition_id {
            let row = conn
                .query_opt(
                    r#"SELECT failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT
                   FROM transition_reliability WHERE config_id = $1 AND transition_id = $2"#,
                    &[&config_id, &tid],
                )
                .await
                .unwrap_or(None);
            if let Some(r) = row {
                let rate: f64 = r.get(0);
                if rate >= threshold {
                    options = json!({ "retry_count": 3, "timeout_multiplier": 1.5, "confidence_threshold": 0.5 });
                }
            }
        } else if let Some(tmpl) = template_id {
            let row = conn
                .query_opt(
                    r#"SELECT failure_count::FLOAT / NULLIF(total_attempts, 0)::FLOAT
                   FROM template_reliability WHERE config_id = $1 AND template_id = $2"#,
                    &[&config_id, &tmpl],
                )
                .await
                .unwrap_or(None);
            if let Some(r) = row {
                let rate: f64 = r.get(0);
                if rate >= threshold {
                    options = json!({ "retry_count": 3, "timeout_multiplier": 1.5, "confidence_threshold": 0.5 });
                }
            }
        }
        Ok(options)
    }

    /// Update statistics after a run (simplified PG version).
    pub async fn update_statistics_after_run(
        &self,
        config_id: &str,
        success: bool,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO config_statistics (config_id, total_runs, successful_runs, failed_runs, last_run_at)
               VALUES ($1, 1, CASE WHEN $2 THEN 1 ELSE 0 END, CASE WHEN $2 THEN 0 ELSE 1 END, $3)
               ON CONFLICT (config_id) DO UPDATE SET
                 total_runs = config_statistics.total_runs + 1,
                 successful_runs = config_statistics.successful_runs + CASE WHEN $2 THEN 1 ELSE 0 END,
                 failed_runs = config_statistics.failed_runs + CASE WHEN NOT $2 THEN 1 ELSE 0 END,
                 last_run_at = $3"#,
            &[&config_id, &success, &now],
        ).await.map_err(|e| crate::database::pg::pg_err("PG update_statistics", &e))?;
        Ok(())
    }

    // ========================================================================
    // Discovery Operations (PG equivalents)
    // ========================================================================

    /// Get discovery summary.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_discovery_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let pending_count: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let ready: i64 = conn.query_one(
            "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3", &[],
        ).await.map(|r| r.get(0)).unwrap_or(0);
        let recent = self.get_pending_discoveries().await.unwrap_or_default();
        let preview: Vec<serde_json::Value> = recent.into_iter().take(10).collect();
        Ok(json!({
            "pending_count": pending_count,
            "ready_for_sync": ready,
            "can_sync": true,
            "recent": preview,
        }))
    }

    /// Extract discoveries for sync (returns id + payload pairs).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn extract_discoveries_for_sync(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let rows = conn.query(
            "SELECT id, payload FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3 ORDER BY created_at ASC LIMIT 50",
            &[],
        ).await.map_err(|e| crate::database::pg::pg_err("PG extract_discoveries", &e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Mark discovery as synced.
    pub async fn mark_discovery_synced(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "UPDATE discovery_queue SET status = 'synced' WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG mark_discovery_synced", &e))?;
        Ok(())
    }

    /// Mark discovery as failed.
    pub async fn mark_discovery_failed(&self, id: &str, error: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "UPDATE discovery_queue SET status = 'failed', attempt_count = attempt_count + 1, error = $2 WHERE id = $1",
            &[&id, &error],
        ).await.map_err(|e| crate::database::pg::pg_err("PG mark_discovery_failed", &e))?;
        Ok(())
    }

    /// Delete a discovery.
    pub async fn delete_discovery(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let count = conn
            .execute("DELETE FROM discovery_queue WHERE id = $1", &[&id])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG delete_discovery", &e))?;
        Ok(count > 0)
    }

    /// Cleanup failed discoveries.
    pub async fn cleanup_failed_discoveries(&self) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let count = conn
            .execute(
                "DELETE FROM discovery_queue WHERE status = 'failed' OR attempt_count >= 3",
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG cleanup_failed", &e))?;
        Ok(count as u32)
    }

    /// Get sync status.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_sync_status(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let pending: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let failed: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM discovery_queue WHERE status = 'failed'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let ready: i64 = conn.query_one("SELECT COUNT(*) FROM discovery_queue WHERE status = 'pending' AND attempt_count < 3", &[])
            .await.map(|r| r.get(0)).unwrap_or(0);
        Ok(json!({
            "pending_count": pending,
            "failed_count": failed,
            "ready_for_retry": ready,
            "authenticated": true,
        }))
    }

    // ========================================================================
    // PRM Export (PG equivalent)
    // ========================================================================

    /// Export step-level PRM training data from pipeline artifacts + task runs.
    ///
    /// Produces one example per step per iteration, matching the format expected
    /// by qontinui-prm's Python extractor (PrmTrainingExample).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn export_prm_training_data(
        &self,
        min_runs: i64,
    ) -> Result<(Vec<serde_json::Value>, serde_json::Value), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {e}"))?;

        // Join task_runs with generation_pipeline_artifacts for step-level data
        let rows = conn
            .query(
                r#"SELECT
                tr.id AS run_id,
                tr.workflow_id,
                tr.task_name,
                tr.status AS run_status,
                COALESCE(tr.verification_passed, false) AS verification_passed,
                gpa.final_json,
                gpa.verification_iterations,
                gpa.fixer_snapshots,
                gpa.specification_criteria,
                gpa.category
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL
            ORDER BY tr.created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG prm export query", &e))?;

        let runs_processed = rows.len();
        if (runs_processed as i64) < min_runs {
            return Err(format!(
                "Only {runs_processed} runs found, need at least {min_runs}"
            ));
        }

        let mut examples = Vec::new();
        let mut passed_count: usize = 0;
        let mut failed_count: usize = 0;
        let mut fixed_count: usize = 0;
        let mut domains_set = std::collections::HashSet::new();

        for row in &rows {
            let run_id: String = row.get(0);
            let workflow_id: Option<String> = row.get(1);
            let task_name: Option<String> = row.get(2);
            let run_status: String = row.get(3);
            let verification_passed: bool = row.get(4);
            let final_json: Option<String> = row.get(5);
            let verification_iterations: Option<String> = row.get(6);
            let fixer_snapshots: Option<String> = row.get(7);
            let spec_criteria: Option<String> = row.get(8);
            let category: Option<String> = row.get(9);

            if let Some(ref d) = category {
                domains_set.insert(d.clone());
            }

            let workflow_passed = run_status == "complete" && verification_passed;
            let context = task_name.unwrap_or_default();

            // Parse steps from final workflow JSON
            let steps = match &final_json {
                Some(j) => match serde_json::from_str::<serde_json::Value>(j) {
                    Ok(v) => v
                        .get("steps")
                        .or_else(|| v.get("verification_steps"))
                        .and_then(|s| s.as_array().cloned())
                        .unwrap_or_default(),
                    Err(_) => continue,
                },
                None => continue,
            };

            // Parse verification iterations — handles both flat arrays and {results: [...]} format
            let iterations: Vec<Vec<serde_json::Value>> = verification_iterations
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
                .map(|arr| {
                    arr.into_iter()
                        .map(|entry| {
                            // If entry has a "results" or "steps" key, use that array
                            entry
                                .get("results")
                                .or_else(|| entry.get("steps"))
                                .and_then(|v| v.as_array().cloned())
                                // Otherwise treat entry itself as array of step results
                                .or_else(|| entry.as_array().cloned())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Parse fixer diffs
            let fixer_diffs: Vec<serde_json::Value> = fixer_snapshots
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();

            // Parse criteria
            let criteria: Vec<serde_json::Value> = spec_criteria
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.get("criteria").and_then(|c| c.as_array().cloned()))
                .unwrap_or_default();

            for (step_idx, step_val) in steps.iter().enumerate() {
                let step_type = step_val
                    .get("step_type")
                    .or_else(|| step_val.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let step_name = step_val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed");

                // Format step definition as text for the Python extractor
                let mut step_parts = vec![
                    format!("Step Type: {step_type}"),
                    format!("Name: {step_name}"),
                ];
                if let Some(cmd) = step_val.get("command").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Command: {cmd}"));
                }
                if let Some(prompt) = step_val.get("prompt").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Prompt: {prompt}"));
                }
                if let Some(check) = step_val.get("check_type").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Check Type: {check}"));
                }
                if let Some(expected) = step_val.get("expected").and_then(|v| v.as_str()) {
                    step_parts.push(format!("Expected: {expected}"));
                }
                let step_text = step_parts.join("\n");

                // Find matching criterion
                let criterion = criteria.iter().find(|c| {
                    let cid = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    step_name.to_lowercase().contains(&cid.to_lowercase())
                });

                let criterion_text = criterion
                    .map(|c| {
                        format!(
                            "Criterion: {}\nMethod: {}\nPriority: {}",
                            c.get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("N/A"),
                            c.get("method").and_then(|v| v.as_str()).unwrap_or("N/A"),
                            c.get("priority").and_then(|v| v.as_str()).unwrap_or("N/A"),
                        )
                    })
                    .unwrap_or_else(|| "No specific acceptance criterion.".to_string());

                // Determine execution result from iteration data
                let step_iters: Vec<&serde_json::Value> = iterations
                    .iter()
                    .filter_map(|iter| iter.get(step_idx))
                    .collect();

                if step_iters.is_empty() {
                    // No iteration data — infer from workflow outcome
                    let (exec_result, final_outcome) = if workflow_passed {
                        ("passed", "workflow_passed")
                    } else {
                        ("failed", "workflow_failed")
                    };

                    if exec_result == "passed" {
                        passed_count += 1;
                    } else {
                        failed_count += 1;
                    }

                    examples.push(json!({
                        "run_id": run_id,
                        "workflow_id": workflow_id,
                        "step_index": step_idx,
                        "step_definition": step_val,
                        "criterion": criterion,
                        "workflow_context": context,
                        "execution_result": exec_result,
                        "iteration": 0,
                        "fixer_diff": null,
                        "final_outcome": final_outcome,
                        "domain": category,
                    }));
                } else {
                    for (iter_idx, iter_result) in step_iters.iter().enumerate() {
                        let passed = iter_result
                            .get("passed")
                            .or_else(|| iter_result.get("success"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let error = iter_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        let any_passed = step_iters.iter().any(|r| {
                            r.get("passed")
                                .or_else(|| r.get("success"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        });

                        let exec_result = if passed { "passed" } else { "failed" };
                        let final_outcome = if passed && workflow_passed {
                            "workflow_passed"
                        } else if !passed && any_passed {
                            fixed_count += 1;
                            "step_fixed_later"
                        } else if workflow_passed {
                            "workflow_passed"
                        } else {
                            "workflow_failed"
                        };

                        if passed {
                            passed_count += 1;
                        } else {
                            failed_count += 1;
                        }

                        let fixer_diff = if !passed {
                            fixer_diffs
                                .get(iter_idx)
                                .and_then(|d| d.get("diff").or_else(|| d.get("changes")))
                                .and_then(|v| v.as_str())
                        } else {
                            None
                        };

                        examples.push(json!({
                            "run_id": run_id,
                            "workflow_id": workflow_id,
                            "step_index": step_idx,
                            "step_definition": step_val,
                            "criterion": criterion,
                            "workflow_context": context,
                            "execution_result": exec_result,
                            "iteration": iter_idx,
                            "fixer_diff": fixer_diff,
                            "final_outcome": final_outcome,
                            "domain": category,
                        }));
                    }
                }
            }
        }

        let stats = json!({
            "total_examples": examples.len(),
            "passed_count": passed_count,
            "failed_count": failed_count,
            "fixed_count": fixed_count,
            "runs_processed": runs_processed,
            "domains": domains_set.into_iter().collect::<Vec<_>>(),
        });
        Ok((examples, stats))
    }

    /// Lightweight PRM stats query — counts without materializing examples.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn export_prm_stats(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {e}"))?;
        // COALESCE on each SUM — query_one with no matching rows would
        // otherwise return NULL and panic on r.get::<_, i64> below.
        let row = conn.query_one(
            r#"SELECT
                COUNT(DISTINCT tr.id) AS runs_processed,
                COUNT(*) AS artifact_count,
                COALESCE(SUM(CASE WHEN tr.verification_passed = true AND tr.status = 'complete' THEN 1 ELSE 0 END), 0)::BIGINT AS passed_runs,
                COALESCE(SUM(CASE WHEN tr.status = 'failed' THEN 1 ELSE 0 END), 0)::BIGINT AS failed_runs,
                ARRAY_AGG(DISTINCT gpa.category) FILTER (WHERE gpa.category IS NOT NULL) AS domains
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL"#,
            &[],
        ).await.map_err(|e| crate::database::pg::pg_err("PG prm stats", &e))?;

        let runs_processed: i64 = row.get(0);
        let total_examples: i64 = row.get(1);
        let passed_count: i64 = row.get(2);
        let failed_count: i64 = row.get(3);
        let domains: Option<Vec<String>> = row.get(4);

        Ok(json!({
            "total_examples": total_examples,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "fixed_count": 0,
            "runs_processed": runs_processed,
            "domains": domains.unwrap_or_default(),
        }))
    }

    // ========================================================================
    // UI Bridge Analytics (PG equivalents)
    // ========================================================================

    /// Compute the composite automation health score.
    ///
    /// # The contract this satisfies
    ///
    /// Three places declare this route's payload and they agree with each
    /// other: `ui_bridge_ops::AutomationHealthScore`, the uibridge spec state
    /// `health-score-data-display` in
    /// `specs/pages/automation-health/spec.uibridge.json`, and the
    /// `AutomationHealthScore` interface in
    /// `src/components/ui-bridge/HealthScoreCard.tsx`. All three describe an
    /// ELEMENT-interaction contract over `ui_bridge_events` plus
    /// `stall_events`.
    ///
    /// This function used to compute something else entirely — a RUN success
    /// ratio over `task_run_automation`, emitting `total_runs` /
    /// `successful_runs` / `avg_duration_ms`, which shares **no field** with
    /// the declared shape. The route therefore 500'd on every call with
    /// `Deserialization error: missing field 'element_success_rate'` once the
    /// numeric-aggregate panic that had been masking it was fixed (PR #1238).
    /// The DB layer was the lone outlier against three agreeing declarations,
    /// so it is the DB layer that moved.
    ///
    /// # Where each field comes from
    ///
    /// - `element_success_rate` — fully determined by the declarations:
    ///   successful ÷ total `ui_bridge_events` rows with
    ///   `event_type = 'action_executed'` and a non-NULL `element_id`, inside
    ///   the window. Identical population and success test to the shipped
    ///   `get_element_reliability` / `get_flaky_elements` queries, so the
    ///   number agrees with the other analytics routes.
    /// - `total_interactions` — the denominator of the above.
    /// - `total_elements` — distinct `element_id` in that population; the
    ///   frontend labels it "Unique Elements".
    /// - `total_stalls` — `stall_events` rows in the window; the same
    ///   population the shipped `get_stall_frequency` query groups.
    /// - `overall_score` — declared verbatim by the spec:
    ///   `0.30 * element_success_rate + 0.25 * (1 - regression_rate)
    ///    + 0.25 * (1 - stall_frequency) + 0.20` base — **evaluated only when
    ///   all three inputs are known**, and `null` otherwise.
    /// - `regression_rate` and `stall_frequency` — numerators declared,
    ///   denominators NOT declared. See the `Definition chosen, not declared`
    ///   doc comments on `health_regression_rate` and
    ///   `health_stall_frequency` at the top of this module.
    /// - `regression_eligible_pairs`, `window_days`, `window_start_epoch_ms`,
    ///   `unknown_fields` — the coverage the payload states about itself, so a
    ///   caller can see what a `null` rests on rather than assume.
    ///
    /// # Unknown is a first-class answer here
    ///
    /// Every rate is `null` when its denominator is zero, and `overall_score`
    /// is `null` whenever any of them is. An empty window used to score
    /// `0.70` — `0.30*0 + 0.25*(1-0) + 0.25*(1-0) + 0.20`, i.e. two "no bad
    /// news" terms plus the declared base — and the card painted it a
    /// green-ish "Good". That number was manufactured entirely out of absent
    /// data. See `unknown-must-not-render-as-a-default` in the module header.
    ///
    /// # Window binding
    ///
    /// `ui_bridge_events.timestamp` is `BIGINT` epoch-ms, so `$1` binds as a
    /// plain `i64` and the lexical-comparison class that PR #1238 fixed
    /// cannot arise there. `stall_events.created_at` IS `TIMESTAMPTZ`, so
    /// that half keeps #1238's discipline: `created_at >= $2::TEXT::TIMESTAMPTZ`
    /// compares real timestamps while keeping the parameter typed as text so
    /// it still binds from a Rust `String`. Comparing
    /// `created_at::TEXT >= $2` instead would drop every stall landing on the
    /// cutoff's own calendar day, because a space (0x20) sorts below `T`
    /// (0x54) at the separator.
    ///
    /// `window_days` is the `?days=` value the handler resolved, carried down
    /// only so the payload can report the coverage it was computed over. It
    /// takes no part in the SQL — `since_epoch_ms` is the cutoff — so the two
    /// cannot disagree about what was queried, only about how it is described.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn compute_automation_health_score(
        &self,
        since_epoch_ms: i64,
        window_days: u32,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since_ts = chrono::DateTime::from_timestamp_millis(since_epoch_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        // Every aggregate is a COUNT, which is never NULL even over zero rows,
        // and every column is cast to BIGINT — so no COALESCE is needed and
        // nothing can come back as `numeric`. That is deliberate: those two
        // shapes are exactly what panicked the previous implementation
        // (PR #1238), and this statement is built so neither can recur.
        //
        // `pair_rates` reproduces the shipped `get_automation_regressions`
        // query's definition of a regressed (element, action) pair exactly —
        // NTILE(2) split on time, at least 4 samples, recent rate more than
        // 0.1 below prior. Reproduced rather than reused because that query is
        // clorinde-generated and returns the rows themselves; here only the
        // two counts are wanted. If that definition changes, change it here
        // too.
        let row = conn
            .query_one(
                r#"WITH interactions AS (
                    SELECT element_id, action, timestamp, success
                    FROM ui_bridge_events
                    WHERE event_type = 'action_executed'
                      AND element_id IS NOT NULL
                      AND timestamp >= $1
                ),
                totals AS (
                    SELECT COUNT(*)::BIGINT AS total_interactions,
                           COUNT(*) FILTER (WHERE success)::BIGINT AS successful_interactions,
                           COUNT(DISTINCT element_id)::BIGINT AS total_elements
                    FROM interactions
                ),
                pair_splits AS (
                    SELECT element_id, action, success,
                           NTILE(2) OVER (PARTITION BY element_id, action ORDER BY timestamp) AS half
                    FROM interactions
                    WHERE action IS NOT NULL
                ),
                pair_rates AS (
                    SELECT SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision
                             / GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) AS prior_rate,
                           SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision
                             / GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1) AS recent_rate
                    FROM pair_splits
                    GROUP BY element_id, action
                    HAVING COUNT(*) >= 4
                ),
                regressions AS (
                    SELECT COUNT(*)::BIGINT AS eligible_pairs,
                           COUNT(*) FILTER (WHERE recent_rate < prior_rate - 0.1)::BIGINT AS regressed_pairs
                    FROM pair_rates
                ),
                stalls AS (
                    SELECT COUNT(*)::BIGINT AS total_stalls
                    FROM stall_events
                    WHERE created_at >= $2::TEXT::TIMESTAMPTZ
                )
                SELECT totals.total_interactions,
                       totals.successful_interactions,
                       totals.total_elements,
                       regressions.eligible_pairs,
                       regressions.regressed_pairs,
                       stalls.total_stalls
                FROM totals, regressions, stalls"#,
                &[&since_epoch_ms, &since_ts],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG health_score", &e))?;
        Ok(health_score_payload(
            HealthScoreCounts {
                total_interactions: row.get(0),
                successful_interactions: row.get(1),
                total_elements: row.get(2),
                eligible_pairs: row.get(3),
                regressed_pairs: row.get(4),
                total_stalls: row.get(5),
            },
            HealthScoreWindow {
                days: window_days,
                start_epoch_ms: since_epoch_ms,
            },
        ))
    }

    /// Generate prioritized improvement recommendations.
    ///
    /// The shape is declared by `ui_bridge_ops::Recommendation`:
    /// `priority: u32`, `category`, `message`, `impact`. This function used to
    /// emit `type` / `title` / `priority: "high"|"medium"` instead — no
    /// overlapping REQUIRED field, and a `priority` of the wrong JSON type —
    /// so every row failed to deserialize in the handler and the route served
    /// `{"data":[]}` while the SQL was returning rows. The handler's
    /// `filter_map(...ok())` is what turned a shape error into a plausible
    /// empty list; it has been replaced with a mapping that surfaces the
    /// failure.
    ///
    /// The query itself is unchanged, including PR #1238's
    /// `started_at > $1::TEXT::TIMESTAMPTZ` window discipline. Only its
    /// error handling and the row→JSON mapping moved.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn generate_recommendations(
        &self,
        since_epoch_ms: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since_ts = chrono::DateTime::from_timestamp_millis(since_epoch_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        // Get frequent error types. Same lexical-window defect as
        // `compute_automation_health_score` above: compare real timestamps,
        // and keep the parameter typed as text with `::TEXT::TIMESTAMPTZ`.
        //
        // The `.unwrap_or_default()` this replaces turned ANY query failure —
        // a missing table, a dead connection — into an empty recommendation
        // list indistinguishable from "nothing to recommend". Same
        // silent-empty-is-not-zero class as the mapping defect above; a query
        // error is now an error.
        let rows = conn
            .query(
                r#"SELECT error_type, COUNT(*) as cnt
               FROM task_run_automation
               WHERE started_at > $1::TEXT::TIMESTAMPTZ AND error_type IS NOT NULL
               GROUP BY error_type ORDER BY cnt DESC LIMIT 5"#,
                &[&since_ts],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG recommendations", &e))?;

        let recommendations: Vec<serde_json::Value> = rows
            .iter()
            .enumerate()
            .map(|(rank, r)| {
                let error_type: String = r.get(0);
                let count: i64 = r.get(1);
                error_type_recommendation(rank, &error_type, count)
            })
            .collect();
        Ok(recommendations)
    }

    // ========================================================================
    // Error Monitor (PG equivalent)
    // ========================================================================

    /// Generate fix workflow for errors (returns workflow JSON).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn generate_error_fix_workflow(
        &self,
        task_run_id: &str,
        max_iterations: u32,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Get errors for this task run
        let errors = conn
            .query(
                r#"SELECT id, title, description, category, severity
               FROM task_run_findings
               WHERE task_run_id = $1 AND category = 'error'
               ORDER BY created_at DESC LIMIT 10"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get errors", &e))?;

        let error_list: Vec<serde_json::Value> = errors
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<_, String>(0),
                    "title": r.get::<_, String>(1),
                    "description": r.get::<_, Option<String>>(2),
                    "category": r.get::<_, String>(3),
                    "severity": r.get::<_, Option<String>>(4),
                })
            })
            .collect();

        Ok(json!({
            "task_run_id": task_run_id,
            "max_iterations": max_iterations,
            "errors_found": error_list.len(),
            "errors": error_list,
            "workflow": null,
        }))
    }

    // ========================================================================
    // Agentic Metrics (PG equivalent)
    // ========================================================================

    /// Recompute all agentic baselines.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn recompute_agentic_baselines(&self) -> Result<u32, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // Count existing metrics to report
        let count: i64 = conn
            .query_one("SELECT COUNT(*) FROM agentic_metric_scores", &[])
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        // Mark baselines as recomputed
        conn.execute("UPDATE agentic_baselines SET recomputed_at = NOW()", &[])
            .await
            .ok();
        Ok(count as u32)
    }

    // ========================================================================
    // Performance Metrics (PG equivalents)
    // ========================================================================

    /// Get performance dashboard data.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_performance_dashboard(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(range_days)).to_rfc3339();

        // Summary. Carries the identical trio of defects as
        // `compute_automation_health_score`, on a second route:
        // COALESCE the SUM (unguarded `query_one` — `successful` is read as
        // `i64` unconditionally below); `::float8` on `AVG(tra.duration_ms)`
        // because `duration_ms` is `integer` and `AVG(integer)` is **numeric**,
        // which panics on `Option<f64>` even with zero matching rows; and a
        // real timestamp comparison instead of the lexical `::TEXT` one, which
        // dropped every run on the cutoff's own calendar day.
        let summary_row = conn.query_one(
            r#"SELECT COUNT(*) as total,
                      COALESCE(SUM(CASE WHEN tra.success THEN 1 ELSE 0 END), 0)::BIGINT as successful,
                      AVG(tra.duration_ms)::float8 as avg_duration
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.started_at > $2::TEXT::TIMESTAMPTZ"#,
            &[&config_id, &since],
        ).await.map_err(|e| crate::database::pg::pg_err("PG perf summary", &e))?;
        let total: i64 = summary_row.get(0);
        let successful: i64 = summary_row.get(1);
        let avg_duration: Option<f64> = summary_row.get(2);

        Ok(json!({
            "summary": {
                "total_runs": total,
                "successful_runs": successful,
                "failed_runs": total - successful,
                "success_rate": if total > 0 { successful as f64 / total as f64 } else { 0.0 },
                "avg_duration_ms": avg_duration,
            },
            "action_metrics": [],
            "transition_metrics": [],
            "element_metrics": [],
            "success_rate_trend": [],
            "duration_trend": [],
        }))
    }

    /// Get action performance metrics.
    pub async fn get_action_performance(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        // Simplified — return empty for now since action_metrics is deeply tied to SQLite schema
        Ok(vec![])
    }

    /// Get transition metrics.
    pub async fn get_transition_metrics(
        &self,
        config_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // transition_reliability table does not exist in the schema — return empty
        tracing::debug!(
            config_id,
            "get_transition_metrics: transition_reliability table not in schema"
        );
        let _ = conn; // suppress unused warning
        Ok(vec![])
    }

    /// Get element resolution metrics.
    pub async fn get_element_metrics(
        &self,
        config_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        // element_resolution_metrics table does not exist in the schema — return empty
        tracing::debug!(
            config_id,
            "get_element_metrics: element_resolution_metrics table not in schema"
        );
        let _ = conn; // suppress unused warning
        Ok(vec![])
    }

    /// Get success rate trend.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_success_rate_trend(
        &self,
        config_id: &str,
        range_days: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(range_days)).to_rfc3339();
        let rows = conn
            .query(
                // `SUM(CASE ... THEN 1 ELSE 0 END)` is `bigint` and is read as
                // `i64`, so that column is already correct and is left alone.
                // The window predicate is not: same lexical `::TEXT` compare
                // as the two sites above, fixed the same way.
                r#"SELECT DATE_TRUNC('day', tra.started_at)::TEXT as day,
                      COUNT(*) as total,
                      SUM(CASE WHEN tra.success THEN 1 ELSE 0 END) as successful
               FROM task_run_automation tra
               INNER JOIN task_runs tr ON tra.task_run_id = tr.id
               WHERE tr.config_id = $1 AND tra.started_at > $2::TEXT::TIMESTAMPTZ
               GROUP BY day ORDER BY day ASC"#,
                &[&config_id, &since],
            )
            .await
            .unwrap_or_default();
        Ok(rows
            .iter()
            .map(|r| {
                let total: i64 = r.get(1);
                let successful: i64 = r.get(2);
                json!({
                    "timestamp": r.get::<_, String>(0),
                    "value": if total > 0 { successful as f64 / total as f64 } else { 0.0 },
                })
            })
            .collect())
    }

    // ========================================================================
    // Learning Recorder (PG equivalent)
    // ========================================================================

    /// Record workflow learning outcome.
    pub async fn record_workflow_learning(
        &self,
        outcome: &serde_json::Value,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let id = outcome["id"]
            .as_str()
            .unwrap_or(&uuid::Uuid::new_v4().to_string())
            .to_string();
        let workflow_name = outcome["workflow_name"].as_str().unwrap_or("");
        let status = outcome["status"].as_str().unwrap_or("unknown");
        let iterations = outcome["iterations"].as_i64();
        let duration = outcome["duration_secs"].as_f64();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            // `created_at` is TIMESTAMPTZ and `$6` carries no cast, so
            // Postgres infers the parameter itself as `timestamp with time
            // zone` from the target column while Rust binds a `String` — the
            // bare-`$n` half of the PR #1238 defect, one token earlier than a
            // bare `$n::TIMESTAMPTZ`. This site was missed by that sweep.
            r#"INSERT INTO learning_outcomes (id, workflow_name, status, iterations, duration_secs, created_at)
               VALUES ($1, $2, $3, $4, $5, $6::TEXT::TIMESTAMPTZ)
               ON CONFLICT (id) DO NOTHING"#,
            &[
                &id, &workflow_name, &status,
                &iterations.map(|i| i as i32) as &(dyn tokio_postgres::types::ToSql + Sync),
                &duration as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        ).await.map_err(|e| crate::database::pg::pg_err("PG record_learning", &e))?;
        Ok(())
    }
}

/// PG-backed regression tests for the **NUMERIC-aggregate deserialization**
/// defect on `GET /ui-bridge/analytics/health-score`, and for the lexical
/// time-window predicate that shipped alongside it.
///
/// ## What broke
///
/// `duration_ms` is `integer`, and PostgreSQL's `AVG(integer)` returns
/// **numeric** — `tokio_postgres` has no `FromSql` from numeric to `f64`, so
/// `row.get::<_, Option<f64>>(2)` panicked with
/// `error deserializing column 2`. Because `query_one` over bare aggregates
/// always returns exactly one row, the panic did not need any data: it fired
/// on **every** call, against an empty table as readily as a full one. Axum's
/// catch-panic layer turned it into
/// `500 {"success":false,"error":"handler panicked: error retrieving column 2:
/// error deserializing column 2"}`.
///
/// The comment two lines above the defect documented the same class being
/// fixed for column 1 (a `COALESCE(SUM(...))` NULL) and stopped there.
///
/// The second defect in the same statement is the window predicate.
/// `started_at::TEXT > $1` compared Postgres's own rendering of a timestamptz
/// (`2019-03-15 18:00:00+00`) against an RFC3339 string
/// (`2019-03-15T12:00:00+00:00`) **lexically**. They agree through the date and
/// then diverge at the separator, where a space (0x20) sorts below `T` (0x54) —
/// so every row landing on the cutoff's own calendar day was dropped whatever
/// its clock time, silently shortening every window by up to a day.
///
/// ## Why these tests must hit a real server
///
/// Both failures live in the server's type resolution and the client's decode,
/// so a test with no server proves nothing. Point `DATABASE_URL` at an
/// **isolated scratch cluster** (never the machine-shared one) and run with
/// `--ignored`.
#[cfg(test)]
mod numeric_aggregate_pg_tests {
    use super::PgDb;

    /// A fixed, far-past window. `compute_automation_health_score` filters on
    /// nothing but `started_at`, so the tests cannot rely on a config id to
    /// isolate them — they own a slice of the timeline instead, and clear it
    /// before each run.
    const WINDOW_LO: &str = "2019-03-01T00:00:00Z";
    const WINDOW_HI: &str = "2019-04-01T00:00:00Z";

    fn db_url() -> String {
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must point at an ISOLATED scratch cluster")
    }

    fn epoch_ms(rfc3339: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must be RFC3339")
            .timestamp_millis()
    }

    /// One runtime per test, owning the pool it builds.
    fn run<F, T>(f: F) -> T
    where
        F: for<'a> FnOnce(
            &'a std::sync::Arc<PgDb>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>,
    {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        rt.block_on(async {
            let db =
                std::sync::Arc::new(PgDb::new(&db_url()).await.expect("connect to scratch PG"));
            f(&db).await
        })
    }

    /// Empty the test's slice of the timeline, across every table these tests
    /// touch. The FK from `task_run_automation.task_run_id` is
    /// `ON DELETE CASCADE`, so clearing the automation rows and then the
    /// seeded task runs is safe either way round.
    ///
    /// `ui_bridge_events` and `stall_events` are cleared by the same window
    /// rather than by an id prefix, because `compute_automation_health_score`
    /// filters on nothing but time — any stray row inside the window would
    /// land in the assertions.
    async fn clear_window(db: &PgDb) {
        let conn = db.pool().get().await.expect("pool");
        conn.execute(
            "DELETE FROM task_run_automation \
             WHERE started_at >= $1::TEXT::TIMESTAMPTZ AND started_at < $2::TEXT::TIMESTAMPTZ",
            &[&WINDOW_LO, &WINDOW_HI],
        )
        .await
        .expect("clear automation rows");
        conn.execute("DELETE FROM task_runs WHERE id LIKE 'numagg-%'", &[])
            .await
            .expect("clear seeded task runs");
        conn.execute(
            "DELETE FROM ui_bridge_events WHERE timestamp >= $1 AND timestamp < $2",
            &[&epoch_ms(WINDOW_LO), &epoch_ms(WINDOW_HI)],
        )
        .await
        .expect("clear ui_bridge_events");
        conn.execute(
            "DELETE FROM stall_events \
             WHERE created_at >= $1::TEXT::TIMESTAMPTZ AND created_at < $2::TEXT::TIMESTAMPTZ",
            &[&WINDOW_LO, &WINDOW_HI],
        )
        .await
        .expect("clear stall_events");
    }

    /// Seed one `task_run_automation` row at an exact instant.
    ///
    /// `duration_ms` is bound as `i32` deliberately: that is the column's real
    /// type, and it is exactly why `AVG` over it comes back numeric.
    async fn seed(
        db: &PgDb,
        tag: &str,
        config_id: &str,
        started_at: &str,
        duration_ms: i32,
        success: bool,
    ) {
        let conn = db.pool().get().await.expect("pool");
        let run_id = format!("numagg-{tag}");
        // `task_name` is NOT NULL with no default, so it has to be supplied
        // even though nothing under test reads it.
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, config_id) \
             VALUES ($1, 'numagg-probe', 'numagg-probe', $2) \
             ON CONFLICT (id) DO NOTHING",
            &[&run_id, &config_id],
        )
        .await
        .expect("seed task_run");
        conn.execute(
            "INSERT INTO task_run_automation \
                 (id, task_run_id, workflow_name, started_at, duration_ms, automation_status, success) \
             VALUES ($1, $2, 'numagg-probe', $3::TEXT::TIMESTAMPTZ, $4, 'complete', $5)",
            &[
                &format!("numagg-a-{tag}") as &(dyn tokio_postgres::types::ToSql + Sync),
                &run_id,
                &started_at.to_string(),
                &duration_ms,
                &success,
            ],
        )
        .await
        .expect("seed task_run_automation");
    }

    /// Seed one `action_executed` interaction — the population
    /// `element_success_rate`, `total_interactions` and `total_elements` are
    /// all computed over.
    ///
    /// `timestamp` is BIGINT epoch-ms on this table, which is why the health
    /// score binds `$1` as a plain `i64` and the lexical-window class cannot
    /// arise on this half of the query.
    async fn seed_event(
        db: &PgDb,
        element_id: &str,
        action: &str,
        at_rfc3339: &str,
        sequence: i64,
        success: bool,
    ) {
        let conn = db.pool().get().await.expect("pool");
        conn.execute(
            "INSERT INTO ui_bridge_events \
                 (timestamp, sequence, event_type, element_id, action, success) \
             VALUES ($1, $2, 'action_executed', $3, $4, $5)",
            &[
                &epoch_ms(at_rfc3339) as &(dyn tokio_postgres::types::ToSql + Sync),
                &sequence,
                &element_id,
                &action,
                &success,
            ],
        )
        .await
        .expect("seed ui_bridge_event");
    }

    /// Seed one stall. `created_at` is TIMESTAMPTZ here, so this is the half
    /// of the health score that still depends on PR #1238's
    /// `::TEXT::TIMESTAMPTZ` binding.
    async fn seed_stall(db: &PgDb, tag: &str, pattern_type: &str, created_at: &str) {
        let conn = db.pool().get().await.expect("pool");
        conn.execute(
            "INSERT INTO stall_events \
                 (id, task_run_id, iteration, pattern_type, created_at) \
             VALUES ($1, 'numagg-stall-run', 1, $2, $3::TEXT::TIMESTAMPTZ)",
            &[
                &format!("numagg-s-{tag}") as &(dyn tokio_postgres::types::ToSql + Sync),
                &pattern_type,
                &created_at.to_string(),
            ],
        )
        .await
        .expect("seed stall_event");
    }

    /// Deserialize the payload through the type the ROUTE deserializes it
    /// through. Asserting on `out["field"]` alone would not have caught the
    /// original defect: the handler's `serde_json::from_value::<
    /// AutomationHealthScore>` is what 500'd, so every test goes through it.
    /// A rate is `Option<f64>` now, so "is it 0.7" has to answer NO for
    /// `None` rather than panicking on an unwrap — a test that unwrapped
    /// would report a null as a crash instead of as a wrong value.
    fn close(actual: Option<f64>, expected: f64) -> bool {
        actual.is_some_and(|a| (a - expected).abs() < 1e-9)
    }

    fn as_declared(
        out: &serde_json::Value,
    ) -> crate::database::ui_bridge_ops::AutomationHealthScore {
        serde_json::from_value(out.clone()).unwrap_or_else(|e| {
            panic!(
                "payload must satisfy the declared AutomationHealthScore: {e}; payload was {out}"
            )
        })
    }

    /// The empty-dataset case. The route has to answer 200 with a body that
    /// satisfies the declared type, not 500 and not a body missing fields —
    /// and every rate in that body has to be `null`, not a number.
    ///
    /// Every declared field is asserted, and the payload is put through
    /// `AutomationHealthScore` — the exact deserialization the handler does,
    /// and the one that used to fail with
    /// `missing field 'element_success_rate'`.
    ///
    /// `overall_score` used to be `0.70` here. Nothing measured it: with no
    /// interactions `element_success_rate` was `0`, both penalty terms were
    /// `0`, and the declared formula returned `0 + 0.25 + 0.25 + 0.20` — two
    /// "no bad news" terms plus a constant, which the card painted a green-ish
    /// "Good". The counts are still asserted at zero because they are the
    /// measured half of the same payload.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn health_score_over_an_empty_window_is_unknown_not_zero_seven() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;

                let out = db
                    .compute_automation_health_score(epoch_ms(WINDOW_LO), 3650)
                    .await
                    .expect("health score must not error");
                let typed = as_declared(&out);

                assert_eq!(typed.total_interactions, 0);
                assert_eq!(typed.total_elements, 0);
                assert_eq!(typed.total_stalls, 0);
                assert_eq!(typed.regression_eligible_pairs, 0);
                assert_eq!(
                    typed.element_success_rate, None,
                    "0 of 0 interactions is undefined, not 0%: {out}"
                );
                assert_eq!(typed.regression_rate, None, "payload: {out}");
                assert_eq!(typed.stall_frequency, None, "payload: {out}");
                assert_eq!(
                    typed.overall_score, None,
                    "an empty window must not score the formula's base + both \
                     unpenalised quarters (0.70); it must report unknown: {out}"
                );
                assert_eq!(
                    typed.unknown_fields,
                    vec![
                        "element_success_rate".to_string(),
                        "regression_rate".to_string(),
                        "stall_frequency".to_string(),
                        "overall_score".to_string(),
                    ],
                    "the discriminator must name every null field: {out}"
                );
                assert_eq!(typed.window_days, 3650, "coverage: the window queried");
                assert_eq!(typed.window_start_epoch_ms, epoch_ms(WINDOW_LO));
            })
        });
    }

    /// The case this fix's first attempt got wrong: **zero interactions with a
    /// non-zero stall count**.
    ///
    /// `stall_frequency` was `1.0` here — the "worst" value, chosen because
    /// stalling without completing an interaction is not health. The reasoning
    /// is sound and the number is still a fabrication: the denominator is
    /// zero, so nothing was measured, and `1.0` is exactly as confident-looking
    /// as `0.0`. The producer states unknown; a consumer that wants the
    /// conservative reading keys it on `total_stalls`, which is why the count
    /// is asserted REAL beside the null rate rather than clamped, zeroed or
    /// suppressed.
    ///
    /// Stalls carry no `element_id`, so seeding them alone genuinely leaves
    /// `ui_bridge_events` empty — this fixture is the real shape, not a
    /// contrived one.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn health_score_with_stalls_but_no_interactions_reports_unknown_and_the_real_count() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;
                seed_stall(db, "solo-a", "no_progress", "2019-03-10T09:00:00Z").await;
                seed_stall(db, "solo-b", "action_loop", "2019-03-10T09:30:00Z").await;
                seed_stall(db, "solo-c", "no_progress", "2019-03-10T10:00:00Z").await;

                let out = db
                    .compute_automation_health_score(epoch_ms(WINDOW_LO), 3650)
                    .await
                    .expect("health score must not error");
                let typed = as_declared(&out);

                assert_eq!(
                    typed.total_stalls, 3,
                    "the stall COUNT is a measured fact and must be reported \
                     unchanged: {out}"
                );
                assert_eq!(typed.total_interactions, 0);
                assert_eq!(
                    typed.stall_frequency, None,
                    "3 stalls over 0 interactions is undefined — not 1.0 \
                     'worst', which is the producer guessing on the consumer's \
                     behalf: {out}"
                );
                assert_eq!(
                    typed.overall_score, None,
                    "no term may be evaluated from a null: {out}"
                );
            })
        });
    }

    /// The populated case, asserting SPECIFIC computed values from known
    /// fixture rows rather than merely "it answered".
    ///
    /// The fixture is built so every declared field has a different, hand-
    /// checkable value:
    ///
    /// - `btn-ok`/`click` — 4 interactions, first half both successes, second
    ///   half both failures. 4 samples clears the `COUNT(*) >= 4` gate, and
    ///   `0.0 < 1.0 - 0.1`, so this pair IS a regression.
    /// - `btn-cancel`/`click` — 4 interactions, all successes. Eligible,
    ///   not regressed.
    /// - `btn-lonely`/`click` — 2 interactions, one of each. Too few samples
    ///   to be eligible for the regression test, but it still counts toward
    ///   the interaction and element totals.
    ///
    /// So: total_interactions = 10, successes = 2+2+4+1 = 7 →
    /// element_success_rate = 0.7. Eligible pairs = 2, regressed = 1 →
    /// regression_rate = 0.5. 2 stalls over 10 interactions →
    /// stall_frequency = 0.2. Score =
    /// 0.30*0.7 + 0.25*0.5 + 0.25*0.8 + 0.20 = 0.21 + 0.125 + 0.20 + 0.20
    /// = 0.735.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn health_score_over_populated_data_computes_the_declared_fields() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;

                // btn-ok: successes then failures — a regression.
                seed_event(db, "btn-ok", "click", "2019-03-10T09:00:00Z", 1, true).await;
                seed_event(db, "btn-ok", "click", "2019-03-10T09:01:00Z", 2, true).await;
                seed_event(db, "btn-ok", "click", "2019-03-10T09:02:00Z", 3, false).await;
                seed_event(db, "btn-ok", "click", "2019-03-10T09:03:00Z", 4, false).await;
                // btn-cancel: uniformly healthy — eligible, not regressed.
                seed_event(db, "btn-cancel", "click", "2019-03-10T10:00:00Z", 5, true).await;
                seed_event(db, "btn-cancel", "click", "2019-03-10T10:01:00Z", 6, true).await;
                seed_event(db, "btn-cancel", "click", "2019-03-10T10:02:00Z", 7, true).await;
                seed_event(db, "btn-cancel", "click", "2019-03-10T10:03:00Z", 8, true).await;
                // btn-lonely: below the 4-sample eligibility gate.
                seed_event(db, "btn-lonely", "click", "2019-03-10T11:00:00Z", 9, true).await;
                seed_event(db, "btn-lonely", "click", "2019-03-10T11:01:00Z", 10, false).await;

                seed_stall(db, "one", "no_progress", "2019-03-10T12:00:00Z").await;
                seed_stall(db, "two", "action_loop", "2019-03-10T13:00:00Z").await;

                let out = db
                    .compute_automation_health_score(epoch_ms(WINDOW_LO), 3650)
                    .await
                    .expect("health score must not error");
                let typed = as_declared(&out);

                assert_eq!(typed.total_interactions, 10, "payload: {out}");
                assert_eq!(typed.total_elements, 3, "three distinct element ids");
                assert_eq!(typed.total_stalls, 2);
                assert_eq!(typed.regression_eligible_pairs, 2, "payload: {out}");
                assert!(
                    close(typed.element_success_rate, 0.7),
                    "7 of 10 interactions succeeded, got {:?}",
                    typed.element_success_rate
                );
                assert!(
                    close(typed.regression_rate, 0.5),
                    "1 of the 2 eligible pairs regressed, got {:?}",
                    typed.regression_rate
                );
                assert!(
                    close(typed.stall_frequency, 0.2),
                    "2 stalls over 10 interactions, got {:?}",
                    typed.stall_frequency
                );
                assert!(
                    close(typed.overall_score, 0.735),
                    "0.30*0.7 + 0.25*0.5 + 0.25*0.8 + 0.20 = 0.735, got {:?}",
                    typed.overall_score
                );
                assert!(
                    typed.unknown_fields.is_empty(),
                    "everything was measured here, so nothing is unknown: {out}"
                );
            })
        });
    }

    /// The stall half of the window predicate — the surviving TIMESTAMPTZ
    /// comparison in this function, and so the surviving PR #1238 regression.
    ///
    /// The cutoff sits at midday; one stall is six hours LATER on the **same
    /// calendar day**, one is the next morning, one is six hours earlier. Two
    /// are inside the window.
    ///
    /// A lexical `created_at::TEXT >= $2` would count only the next-morning
    /// stall, because `2019-03-15 18:00:00+00` sorts below
    /// `2019-03-15T12:00:00+00:00` at the separator (' ' 0x20 < 'T' 0x54).
    ///
    /// The assertion is on `total_stalls` rather than on `stall_frequency`
    /// because no interaction is seeded, so the rate is correctly `null` — the
    /// count is the measured fact this test is about.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn health_score_stall_window_keeps_rows_from_the_cutoffs_own_calendar_day() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;
                seed_stall(db, "before", "no_progress", "2019-03-15T06:00:00Z").await;
                seed_stall(db, "sameday", "no_progress", "2019-03-15T18:00:00Z").await;
                seed_stall(db, "nextday", "no_progress", "2019-03-16T06:00:00Z").await;

                let out = db
                    .compute_automation_health_score(epoch_ms("2019-03-15T12:00:00Z"), 3650)
                    .await
                    .expect("health score must not error");
                let typed = as_declared(&out);

                assert_eq!(
                    typed.total_stalls, 2,
                    "the same-day stall after the cutoff must be inside the window: {out}"
                );
            })
        });
    }

    /// The interaction half of the window predicate. `ui_bridge_events.timestamp`
    /// is BIGINT epoch-ms, so this is an ordinary integer comparison — but the
    /// route's `?days=` boundary is worth pinning anyway: a row exactly ON the
    /// cutoff is inside (`>=`), one a millisecond before is not.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn health_score_interaction_window_is_inclusive_of_the_cutoff_instant() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;
                seed_event(db, "el-before", "click", "2019-03-15T11:59:59Z", 1, true).await;
                seed_event(db, "el-at", "click", "2019-03-15T12:00:00Z", 2, true).await;
                seed_event(db, "el-after", "click", "2019-03-15T12:00:01Z", 3, true).await;

                let out = db
                    .compute_automation_health_score(epoch_ms("2019-03-15T12:00:00Z"), 3650)
                    .await
                    .expect("health score must not error");
                let typed = as_declared(&out);

                assert_eq!(
                    typed.total_interactions, 2,
                    "the row exactly on the cutoff is inside a `>=` window: {out}"
                );
                assert_eq!(typed.total_elements, 2);
            })
        });
    }

    /// `/ui-bridge/analytics/recommendations` served `{"data":[]}` while its
    /// SQL was returning rows, because no field of the emitted JSON matched
    /// the declared `Recommendation` and the handler swallowed the mismatch
    /// in a `filter_map(...ok())`.
    ///
    /// Seeding one failing automation row with an `error_type` must therefore
    /// produce exactly one recommendation that deserializes cleanly.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn recommendations_reach_the_caller_as_the_declared_shape() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;
                seed(
                    db,
                    "rec1",
                    "numagg-rec-config",
                    "2019-03-10T09:00:00Z",
                    100,
                    false,
                )
                .await;
                {
                    let conn = db.pool().get().await.expect("pool");
                    conn.execute(
                        "UPDATE task_run_automation SET error_type = 'TimeoutError' \
                         WHERE id = 'numagg-a-rec1'",
                        &[],
                    )
                    .await
                    .expect("set error_type");
                }

                let out = db
                    .generate_recommendations(epoch_ms(WINDOW_LO))
                    .await
                    .expect("recommendations must not error");

                assert_eq!(out.len(), 1, "one error type was seeded, got {out:?}");
                let typed: crate::database::ui_bridge_ops::Recommendation =
                    serde_json::from_value(out[0].clone()).unwrap_or_else(|e| {
                        panic!(
                            "row must satisfy the declared Recommendation: {e}; row {:?}",
                            out[0]
                        )
                    });
                assert_eq!(typed.priority, 1, "most frequent error type ranks first");
                assert_eq!(typed.category, "reduce_errors");
                assert!(
                    typed.message.contains("TimeoutError"),
                    "message names the error type, got {:?}",
                    typed.message
                );
                assert_eq!(typed.impact, "medium", "1 occurrence is below the >5 bar");
            })
        });
    }

    /// The sibling site: `get_performance_dashboard` carried the identical
    /// `AVG(tra.duration_ms)` defect on a second route, and panicked the same
    /// way with no matching rows.
    #[test]
    #[ignore = "requires an ISOLATED PG via DATABASE_URL"]
    fn performance_dashboard_survives_a_window_with_no_matching_runs() {
        run(|db| {
            Box::pin(async move {
                clear_window(db).await;

                let out = db
                    .get_performance_dashboard("numagg-no-such-config", 3650)
                    .await
                    .expect("performance dashboard must not error");

                assert_eq!(out["summary"]["total_runs"], serde_json::json!(0));
                assert!(
                    out["summary"]["avg_duration_ms"].is_null(),
                    "AVG over zero rows must stay NULL, got {:?}",
                    out["summary"]["avg_duration_ms"]
                );
            })
        });
    }

    // NOTE on PR #1238's lexical-window regression against
    // `task_run_automation`. `compute_automation_health_score` used to be the
    // site that carried it and no longer reads that table at all, so the test
    // that pinned it there was rewritten onto `stall_events` — see
    // `health_score_stall_window_keeps_rows_from_the_cutoffs_own_calendar_day`
    // above, which exercises the identical `::TEXT::TIMESTAMPTZ` discipline
    // against a FIXED 2019 cutoff and is therefore deterministic.
    //
    // A same-shaped test was deliberately NOT added for
    // `get_performance_dashboard`, the other surviving TIMESTAMPTZ site. Its
    // window is expressed as `range_days` counted back from `Utc::now()`, so
    // the cutoff instant inherits the current time of day. The lexical defect
    // is a *calendar-day* boundary bug, so pinning it there means placing a
    // fixture on the cutoff's own date and after it — which is only reachable
    // when `now`'s time of day leaves room before midnight. Such a test passes
    // most of the day and fails near it. A test that is green by the clock is
    // worse than no test, so the gap is recorded here instead of papered over.
}

/// The health-score CONTRACT, tested without a database.
///
/// The route's failure was never a query failure: the query ran fine and the
/// payload it built did not satisfy
/// `ui_bridge_ops::AutomationHealthScore`, so the handler's
/// `serde_json::from_value` returned
/// `missing field 'element_success_rate'` and the route 500'd on every call.
/// That is a pure-data property, so it is pinned by tests that need nothing
/// running — they fail in plain `cargo test` the moment a field name, a JSON
/// type or one of the chosen definitions drifts from the declarations.
#[cfg(test)]
mod health_score_contract_tests {
    use super::{
        error_type_recommendation, health_element_success_rate, health_overall_score,
        health_regression_rate, health_score_payload, health_stall_frequency, HealthScoreCounts,
        HealthScoreWindow,
    };
    use crate::database::ui_bridge_ops::{AutomationHealthScore, Recommendation};

    /// A window value for the payload builder. Nothing in these tests depends
    /// on which window it is, only that the payload reports one.
    const WINDOW: HealthScoreWindow = HealthScoreWindow {
        days: 7,
        start_epoch_ms: 1_551_398_400_000,
    };

    fn close(actual: Option<f64>, expected: f64) -> bool {
        actual.is_some_and(|a| (a - expected).abs() < 1e-9)
    }

    fn counts(
        total_interactions: i64,
        successful_interactions: i64,
        total_elements: i64,
        eligible_pairs: i64,
        regressed_pairs: i64,
        total_stalls: i64,
    ) -> HealthScoreCounts {
        HealthScoreCounts {
            total_interactions,
            successful_interactions,
            total_elements,
            eligible_pairs,
            regressed_pairs,
            total_stalls,
        }
    }

    /// The defect itself: the payload must deserialize into the type the
    /// handler deserializes it into. Three declarations agree on these seven
    /// field names; this is the test that holds the DB layer to them.
    #[test]
    fn payload_satisfies_the_declared_automation_health_score() {
        let payload = health_score_payload(counts(10, 7, 3, 2, 1, 2), WINDOW);
        let typed: AutomationHealthScore = serde_json::from_value(payload.clone())
            .unwrap_or_else(|e| panic!("payload must satisfy the declaration: {e}; got {payload}"));

        assert_eq!(typed.total_interactions, 10);
        assert_eq!(typed.total_elements, 3);
        assert_eq!(typed.total_stalls, 2);
        assert!(close(typed.element_success_rate, 0.7));
        assert!(close(typed.regression_rate, 0.5));
        assert!(close(typed.stall_frequency, 0.2));
        assert!(close(typed.overall_score, 0.735));
    }

    /// The payload states the coverage it was computed over, so a consumer
    /// does not have to assume it. `fleet-services`: "If it cannot state its
    /// own coverage, its consumers will over-trust it."
    #[test]
    fn a_measured_payload_states_its_window_and_claims_nothing_unknown() {
        let payload = health_score_payload(counts(10, 7, 3, 2, 1, 2), WINDOW);
        let typed: AutomationHealthScore =
            serde_json::from_value(payload.clone()).expect("declared shape");

        assert_eq!(typed.window_days, 7);
        assert_eq!(typed.window_start_epoch_ms, 1_551_398_400_000);
        assert_eq!(
            typed.regression_eligible_pairs, 2,
            "the regression denominator is otherwise invisible in the payload"
        );
        assert!(
            typed.unknown_fields.is_empty(),
            "nothing was unknown here, got {:?}",
            typed.unknown_fields
        );
    }

    /// **The defect this change fixes.** The empty payload must satisfy the
    /// declaration AND report unknown — not the 0.70 the declared formula
    /// returns when its three unknown inputs are read as zero
    /// (`0 + 0.25 + 0.25 + 0.20`), which the card rendered as a green-ish
    /// "Good".
    #[test]
    fn the_empty_payload_is_all_null_and_never_the_formulas_bare_base() {
        let payload = health_score_payload(counts(0, 0, 0, 0, 0, 0), WINDOW);
        let typed: AutomationHealthScore =
            serde_json::from_value(payload.clone()).unwrap_or_else(|e| {
                panic!("empty payload must satisfy the declaration: {e}; got {payload}")
            });

        assert_eq!(typed.total_interactions, 0);
        assert_eq!(typed.element_success_rate, None);
        assert_eq!(typed.regression_rate, None);
        assert_eq!(typed.stall_frequency, None);
        assert_eq!(
            typed.overall_score, None,
            "an empty window must not score 0.70 — two 'no bad news' terms \
             plus the 0.20 base is not a measurement; got {payload}"
        );
        assert_eq!(
            typed.unknown_fields,
            vec![
                "element_success_rate".to_string(),
                "regression_rate".to_string(),
                "stall_frequency".to_string(),
                "overall_score".to_string(),
            ],
            "the discriminator must name every null field"
        );
        assert!(
            payload["overall_score"].is_null(),
            "the JSON must carry an explicit null, not omit the key: {payload}"
        );
    }

    /// The counts are measured facts and survive whatever the rates do. This
    /// is the zero-interactions-with-stalls case at the payload level: a real
    /// `total_stalls` beside a `null` `stall_frequency`.
    #[test]
    fn counts_stay_real_when_every_rate_is_unknown() {
        let payload = health_score_payload(counts(0, 0, 0, 0, 0, 4), WINDOW);
        let typed: AutomationHealthScore =
            serde_json::from_value(payload.clone()).expect("declared shape");

        assert_eq!(
            typed.total_stalls, 4,
            "the stall count is measured and must not be zeroed, clamped or \
             suppressed by the unknown rate beside it: {payload}"
        );
        assert_eq!(
            typed.stall_frequency, None,
            "4 stalls over 0 interactions is undefined, not 1.0 'worst': {payload}"
        );
        assert_eq!(typed.overall_score, None);
    }

    /// A partially-measured window: interactions exist, but no
    /// `(element, action)` pair has the four samples the regression test
    /// needs. Two rates are real, one is unknown — and one unknown input is
    /// enough to make the score unknown. This is the common case, not an edge
    /// one, so it is pinned separately.
    #[test]
    fn one_unknown_term_makes_the_whole_score_unknown() {
        let payload = health_score_payload(counts(10, 7, 3, 0, 0, 2), WINDOW);
        let typed: AutomationHealthScore =
            serde_json::from_value(payload.clone()).expect("declared shape");

        assert!(close(typed.element_success_rate, 0.7), "measured");
        assert!(close(typed.stall_frequency, 0.2), "measured");
        assert_eq!(typed.regression_rate, None, "no eligible pairs");
        assert_eq!(
            typed.overall_score, None,
            "the formula must not be evaluated with 1 - null read as 1: {payload}"
        );
        assert_eq!(
            typed.unknown_fields,
            vec!["regression_rate".to_string(), "overall_score".to_string()],
            "the discriminator names exactly what is missing"
        );
    }

    /// The formula is the spec's, verbatim, for a window where everything IS
    /// known. A flawless window is exactly 1.0 — if the weights ever stop
    /// summing to 1.0 with the base, this catches it.
    #[test]
    fn a_flawless_window_scores_exactly_one() {
        assert!(close(
            health_overall_score(Some(1.0), Some(0.0), Some(0.0)),
            1.0
        ));
    }

    /// The worst possible MEASURED window still scores the declared 0.20 base,
    /// and never below zero. That 0.20 floor is only defensible when the
    /// inputs are real; it is the same constant that manufactured 0.70 out of
    /// an empty window, which is why the None arm is pinned right beside it.
    #[test]
    fn the_worst_window_scores_the_declared_base_and_nothing_less() {
        let worst = health_overall_score(Some(0.0), Some(1.0), Some(1.0));
        assert!(
            close(worst, 0.20),
            "the spec declares a 0.20 base, got {worst:?}"
        );
    }

    /// Every arm of the score's null propagation, one unknown at a time. The
    /// declared formula is never evaluated on an unknown.
    #[test]
    fn the_score_is_none_if_any_single_term_is_none() {
        assert_eq!(health_overall_score(None, Some(0.0), Some(0.0)), None);
        assert_eq!(health_overall_score(Some(1.0), None, Some(0.0)), None);
        assert_eq!(health_overall_score(Some(1.0), Some(0.0), None), None);
        assert_eq!(health_overall_score(None, None, None), None);
    }

    #[test]
    fn element_success_rate_is_successes_over_interactions() {
        assert!(close(health_element_success_rate(7, 10), 0.7));
        assert_eq!(
            health_element_success_rate(0, 0),
            None,
            "0/0 is undefined and must be null, not 0.0 — a rate of zero is a \
             measurement, and there was none"
        );
        assert_eq!(
            health_element_success_rate(0, 5),
            Some(0.0),
            "a MEASURED zero is still 0.0 — null is for the absent denominator"
        );
    }

    /// Definition chosen, not declared: regressed pairs over pairs ELIGIBLE
    /// for the regression test, not over all pairs.
    #[test]
    fn regression_rate_divides_by_the_eligible_pairs() {
        assert!(close(health_regression_rate(1, 2), 0.5));
        assert!(close(health_regression_rate(3, 4), 0.75));
        assert_eq!(
            health_regression_rate(0, 0),
            None,
            "no eligible pair means the rate was not measured"
        );
        assert_eq!(
            health_regression_rate(0, 4),
            Some(0.0),
            "four eligible pairs and none regressed is a measured 0%"
        );
    }

    /// Definition chosen, not declared: stalls per interaction, clamped.
    /// The clamp is what keeps a MEASURED `overall_score` inside `[0, 1]`, so
    /// it is asserted rather than assumed — and it must not reach the raw
    /// counts, which the payload reports unclamped.
    #[test]
    fn stall_frequency_is_stalls_per_interaction_clamped_to_one() {
        assert!(close(health_stall_frequency(2, 10), 0.2));
        assert_eq!(
            health_stall_frequency(30, 10),
            Some(1.0),
            "more stalls than interactions must clamp, or the score goes negative"
        );
        assert!(
            health_overall_score(Some(0.0), Some(0.0), health_stall_frequency(30, 10))
                .is_some_and(|s| s >= 0.0),
            "the clamp must keep a measured score non-negative"
        );

        // The clamp does not reach the counts: 30 stalls over 10 interactions
        // reports 30, beside a rate of 1.0.
        let payload = health_score_payload(counts(10, 10, 1, 0, 0, 30), WINDOW);
        assert_eq!(
            payload["total_stalls"],
            serde_json::json!(30),
            "the clamp is a score input only; the count stays raw: {payload}"
        );
    }

    /// The zero-denominator arms of `stall_frequency`, including the one this
    /// fix's first attempt got wrong. Both are `None`; neither is a number.
    #[test]
    fn stall_frequency_with_no_interactions_is_unknown_even_with_stalls() {
        assert_eq!(
            health_stall_frequency(1, 0),
            None,
            "stalling with no completed interaction is UNKNOWN, not 1.0 — a \
             conservative guess is still the producer guessing, and the \
             conservative arm belongs to the consumer"
        );
        assert_eq!(health_stall_frequency(0, 0), None);
    }

    /// The recommendations defect: the emitted row must satisfy the declared
    /// `Recommendation`. The previous mapping emitted `type` / `title` /
    /// a STRING `priority`, which matched no required field and made every row
    /// vanish in the handler's `filter_map`.
    #[test]
    fn a_recommendation_row_satisfies_the_declared_shape() {
        let row = error_type_recommendation(0, "TimeoutError", 1);
        let typed: Recommendation = serde_json::from_value(row.clone())
            .unwrap_or_else(|e| panic!("row must satisfy the declaration: {e}; got {row}"));

        assert_eq!(typed.priority, 1, "rank 0 is priority 1");
        assert_eq!(typed.category, "reduce_errors");
        assert_eq!(
            typed.message,
            "Address recurring 'TimeoutError' errors (1 occurrences)"
        );
        assert_eq!(typed.impact, "medium");
    }

    /// The chosen `count > 5` impact threshold and the 1-based ranking, both
    /// pinned so a change to either is deliberate.
    #[test]
    fn recommendation_rank_is_one_based_and_impact_turns_high_above_five() {
        let second: Recommendation =
            serde_json::from_value(error_type_recommendation(1, "ElementNotFound", 6))
                .expect("declared shape");
        assert_eq!(second.priority, 2);
        assert_eq!(second.impact, "high", "6 occurrences is above the >5 bar");

        let boundary: Recommendation =
            serde_json::from_value(error_type_recommendation(0, "Flake", 5))
                .expect("declared shape");
        assert_eq!(boundary.impact, "medium", "5 is not above 5");
    }
}
