//! Cost Efficiency Optimizer
//!
//! Analyzes token/cost data from `pipeline_agent_traces` and generates
//! actionable cost-optimization recommendations. Runs as a data-driven
//! pass (no AI session needed) and appends results to the recommendation
//! table alongside other optimizer outputs.

use std::sync::Arc;
use tracing::{debug, info, warn};

use super::recommendations;
use super::types::Recommendation;
use crate::database::CheckpointDb;
use crate::database::pg::PgDb;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Analyze token efficiency across pipeline agents and generate cost
/// optimization recommendations.
///
/// Called after meta-optimizer workflow completion (or periodically) to
/// surface cost hotspots that don't require AI analysis.
///
/// Avoids duplicating any recommendation whose `title` already exists in the
/// last 7 days.
pub fn generate_cost_recommendations(
    pg_db: &Arc<PgDb>,
    existing_recommendations: &[Recommendation],
) -> Vec<Recommendation> {
    let mut results: Vec<Recommendation> = Vec::new();

    // Collect recent titles (last 7 days) to avoid duplicates
    let recent_titles: Vec<String> = existing_recommendations
        .iter()
        .filter(|r| {
            if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&r.created_at) {
                let age = chrono::Utc::now().signed_duration_since(created);
                age.num_days() < 7
            } else {
                false
            }
        })
        .map(|r| r.title.clone())
        .collect();

    let has_recent = |title: &str| recent_titles.iter().any(|t| t == title);

    // ── 1. Query per-agent token/cost averages (last 30 days) ────────────
    let agent_stats = match query_agent_cost_stats(pg_db) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Cost optimizer: failed to query agent stats: {}", e);
            return results;
        }
    };

    if agent_stats.is_empty() {
        debug!("Cost optimizer: no agent trace data with non-zero tokens");
        return results;
    }

    info!(
        "Cost optimizer: analyzing {} agent types",
        agent_stats.len()
    );

    // ── 2. Compute aggregate metrics ─────────────────────────────────────
    let total_cost: f64 = agent_stats.iter().map(|a| a.total_cost).sum();
    let mut avg_tokens: Vec<f64> = agent_stats.iter().map(|a| a.avg_total_tokens()).collect();
    avg_tokens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let median_tokens = if avg_tokens.is_empty() {
        0.0
    } else if avg_tokens.len().is_multiple_of(2) {
        let mid = avg_tokens.len() / 2;
        (avg_tokens[mid - 1] + avg_tokens[mid]) / 2.0
    } else {
        avg_tokens[avg_tokens.len() / 2]
    };

    // ── 3. Check: High token agents (avg > 2x median) ───────────────────
    if median_tokens > 0.0 {
        for agent in &agent_stats {
            let avg_total = agent.avg_total_tokens();
            if avg_total > median_tokens * 2.0 && agent.run_count >= 5 {
                let title = format!("High token usage: {} agent", agent.agent_type);
                if has_recent(&title) {
                    continue;
                }

                let ratio = avg_total / median_tokens;
                let confidence = compute_confidence(agent.run_count, 10, 30);
                let description = format!(
                    "**{}** averages {:.0} tokens per run ({:.1}x the median of {:.0}).\n\n\
                     - Avg input tokens: {:.0}\n\
                     - Avg output tokens: {:.0}\n\
                     - Avg cost: ${:.4}\n\
                     - Runs analyzed: {}\n\n\
                     Consider reducing prompt size or splitting into smaller steps \
                     to lower per-invocation token usage.",
                    agent.agent_type,
                    avg_total,
                    ratio,
                    median_tokens,
                    agent.avg_tokens_in,
                    agent.avg_tokens_out,
                    agent.avg_cost,
                    agent.run_count,
                );
                let evidence = serde_json::json!({
                    "agent_type": agent.agent_type,
                    "avg_total_tokens": avg_total,
                    "median_tokens": median_tokens,
                    "ratio": ratio,
                    "run_count": agent.run_count,
                })
                .to_string();

                match recommendations::create_recommendation(
                    
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    Some(&agent.agent_type),
                    &title,
                    &description,
                    None,
                    None,
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => {
                        info!("Cost recommendation created: {}", title);
                        results.push(rec);
                    }
                    Err(e) => warn!("Failed to create cost recommendation: {}", e),
                }
            }
        }
    }

    // ── 4. Check: Cost concentration (one agent > 70% of total cost) ────
    if total_cost > 0.0 {
        for agent in &agent_stats {
            let pct = agent.total_cost / total_cost;
            if pct > 0.70 && agent.run_count >= 5 {
                let title = format!(
                    "Cost concentration: {} agent ({:.0}%)",
                    agent.agent_type,
                    pct * 100.0
                );
                if has_recent(&title) {
                    continue;
                }

                let confidence = compute_confidence(agent.run_count, 10, 30);
                let description = format!(
                    "**{}** accounts for {:.1}% of total pipeline cost (${:.2} of ${:.2}).\n\n\
                     - Avg cost per run: ${:.4}\n\
                     - Total runs: {}\n\n\
                     This single agent dominates cost. Investigate whether its prompt \
                     can be made more concise or if cheaper model tiers are viable.",
                    agent.agent_type,
                    pct * 100.0,
                    agent.total_cost,
                    total_cost,
                    agent.avg_cost,
                    agent.run_count,
                );
                let evidence = serde_json::json!({
                    "agent_type": agent.agent_type,
                    "agent_cost": agent.total_cost,
                    "total_cost": total_cost,
                    "cost_pct": pct,
                    "run_count": agent.run_count,
                })
                .to_string();

                match recommendations::create_recommendation(
                    
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    Some(&agent.agent_type),
                    &title,
                    &description,
                    None,
                    None,
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => {
                        info!("Cost recommendation created: {}", title);
                        results.push(rec);
                    }
                    Err(e) => warn!("Failed to create cost recommendation: {}", e),
                }
            }
        }
    }

    // ── 5. Check: Zero-token agents with AI calls ────────────────────────
    let zero_token_agents = match query_zero_token_agents(pg_db) {
        Ok(agents) => agents,
        Err(e) => {
            warn!("Cost optimizer: failed to query zero-token agents: {}", e);
            Vec::new()
        }
    };

    for (agent_type, run_count) in &zero_token_agents {
        if *run_count >= 5 {
            let title = format!("Instrumentation gap: {} agent has 0 tokens", agent_type);
            if has_recent(&title) {
                continue;
            }

            let confidence = compute_confidence(*run_count, 5, 20);
            let description = format!(
                "**{}** has {} traces with zero tokens (both input and output).\n\n\
                 This likely means token tracking is not wired for this agent, \
                 making cost analysis incomplete. Wire token counters from the \
                 AI provider response into the trace record.",
                agent_type, run_count,
            );
            let evidence = serde_json::json!({
                "agent_type": agent_type,
                "zero_token_run_count": run_count,
            })
            .to_string();

            match recommendations::create_recommendation(
                
                pg_db,
                "pipeline_prompt",
                "cost_optimization",
                Some(agent_type),
                &title,
                &description,
                None,
                None,
                Some(&evidence),
                confidence,
                None,
            ) {
                Ok(rec) => {
                    info!("Cost recommendation created: {}", title);
                    results.push(rec);
                }
                Err(e) => warn!("Failed to create cost recommendation: {}", e),
            }
        }
    }

    // ── 6. Check: CLI sessions where API would suffice ───────────────────
    // Join pipeline_agent_traces → task_runs → iteration_logs to detect
    // high-cost CLI provider usage.
    let cli_agents = match query_cli_heavy_agents(pg_db) {
        Ok(agents) => agents,
        Err(e) => {
            debug!("Cost optimizer: CLI agent query skipped: {}", e);
            Vec::new()
        }
    };

    for (agent_type, cli_runs, total_runs, avg_cli_cost, avg_api_cost) in &cli_agents {
        let cli_pct = if *total_runs > 0 {
            *cli_runs as f64 / *total_runs as f64
        } else {
            0.0
        };
        // Only flag if CLI is dominant (>60%) and meaningfully more expensive
        if cli_pct > 0.60 && *avg_cli_cost > *avg_api_cost * 1.5 && *total_runs >= 5 {
            let title = format!(
                "Model selection: {} agent uses expensive CLI sessions",
                agent_type
            );
            if has_recent(&title) {
                continue;
            }

            let potential_savings = (avg_cli_cost - avg_api_cost) * *cli_runs as f64;
            let confidence = compute_confidence(*total_runs, 10, 30);
            let description = format!(
                "**{}** runs {:.0}% of invocations via CLI sessions (avg ${:.4}/run) \
                 vs API (avg ${:.4}/run).\n\n\
                 - CLI runs: {} of {}\n\
                 - Estimated savings if switched to API: ${:.2}\n\n\
                 Consider using API provider for this agent if CLI-specific \
                 features (tool use, file access) are not required.",
                agent_type,
                cli_pct * 100.0,
                avg_cli_cost,
                avg_api_cost,
                cli_runs,
                total_runs,
                potential_savings,
            );
            let evidence = serde_json::json!({
                "agent_type": agent_type,
                "cli_runs": cli_runs,
                "total_runs": total_runs,
                "avg_cli_cost": avg_cli_cost,
                "avg_api_cost": avg_api_cost,
                "potential_savings": potential_savings,
            })
            .to_string();

            match recommendations::create_recommendation(
                
                pg_db,
                "pipeline_prompt",
                "cost_optimization",
                Some(agent_type),
                &title,
                &description,
                None,
                None,
                Some(&evidence),
                confidence,
                None,
            ) {
                Ok(rec) => {
                    info!("Cost recommendation created: {}", title);
                    results.push(rec);
                }
                Err(e) => warn!("Failed to create cost recommendation: {}", e),
            }
        }
    }

    info!(
        "Cost optimizer: generated {} recommendations",
        results.len()
    );
    results
}

// ---------------------------------------------------------------------------
// Cost analysis data (consumed by the API endpoint)
// ---------------------------------------------------------------------------

/// Per-agent cost breakdown returned by the cost-analysis endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentCostBreakdown {
    pub agent_type: String,
    pub run_count: i64,
    pub avg_tokens_in: f64,
    pub avg_tokens_out: f64,
    pub avg_cost: f64,
    pub total_cost: f64,
    pub cost_pct: f64,
}

/// Summary returned by `GET /meta-optimizer/cost-analysis`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CostAnalysisSummary {
    pub agents: Vec<AgentCostBreakdown>,
    pub total_cost: f64,
    pub period_days: i64,
    pub cost_trend: String,
    pub active_cost_recommendations: Vec<Recommendation>,
}

/// Build a full cost-analysis summary for the API endpoint.
pub fn build_cost_analysis(pg_db: &Arc<PgDb>) -> Result<CostAnalysisSummary, String> {
    let agent_stats = query_agent_cost_stats(pg_db)?;

    let total_cost: f64 = agent_stats.iter().map(|a| a.total_cost).sum();

    let agents: Vec<AgentCostBreakdown> = agent_stats
        .iter()
        .map(|a| AgentCostBreakdown {
            agent_type: a.agent_type.clone(),
            run_count: a.run_count,
            avg_tokens_in: a.avg_tokens_in,
            avg_tokens_out: a.avg_tokens_out,
            avg_cost: a.avg_cost,
            total_cost: a.total_cost,
            cost_pct: if total_cost > 0.0 {
                a.total_cost / total_cost
            } else {
                0.0
            },
        })
        .collect();

    // Cost trend: compare last 15 days vs previous 15 days
    let cost_trend = compute_cost_trend(pg_db).unwrap_or_else(|_| "unknown".to_string());

    // Active cost recommendations
    let active_cost_recs = query_active_cost_recommendations(pg_db).unwrap_or_default();

    Ok(CostAnalysisSummary {
        agents,
        total_cost,
        period_days: 30,
        cost_trend,
        active_cost_recommendations: active_cost_recs,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Raw per-agent statistics from the traces table.
#[derive(Debug, Clone)]
struct AgentCostStats {
    agent_type: String,
    run_count: i64,
    avg_tokens_in: f64,
    avg_tokens_out: f64,
    avg_cost: f64,
    total_cost: f64,
}

impl AgentCostStats {
    fn avg_total_tokens(&self) -> f64 {
        self.avg_tokens_in + self.avg_tokens_out
    }
}

/// Query per-agent token/cost averages over the last 30 days (non-zero tokens only).
fn query_agent_cost_stats(pg_db: &Arc<PgDb>) -> Result<Vec<AgentCostStats>, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let tuples = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_agent_cost_stats(&since))
    })?;
    Ok(tuples
        .into_iter()
        .map(|(agent_type, run_count, avg_tokens_in, avg_tokens_out, avg_cost, total_cost)| {
            AgentCostStats {
                agent_type,
                run_count,
                avg_tokens_in,
                avg_tokens_out,
                avg_cost,
                total_cost,
            }
        })
        .collect())
}

/// Query agents that have traces but zero tokens (instrumentation gap).
fn query_zero_token_agents(pg_db: &Arc<PgDb>) -> Result<Vec<(String, i64)>, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_zero_token_agents(&since))
    })
}

/// Detect agents that primarily use CLI sessions (expensive) vs API.
///
/// Joins pipeline_agent_traces → iteration_logs to check if the run used CLI
/// provider. Returns (agent_type, cli_runs, total_runs, avg_cli_cost, avg_api_cost).
fn query_cli_heavy_agents(pg_db: &Arc<PgDb>) -> Result<Vec<(String, i64, i64, f64, f64)>, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_cli_heavy_agents(&since))
    })
}

/// Compare cost over recent vs previous 15-day window.
fn compute_cost_trend(pg_db: &Arc<PgDb>) -> Result<String, String> {
    let now = chrono::Utc::now();
    let mid = (now - chrono::Duration::days(15)).to_rfc3339();
    let start = (now - chrono::Duration::days(30)).to_rfc3339();

    let (recent_cost, previous_cost) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.compute_cost_trend(&mid, &start))
    })?;

    if previous_cost == 0.0 && recent_cost == 0.0 {
        Ok("no_data".to_string())
    } else if previous_cost == 0.0 {
        Ok("increasing".to_string())
    } else {
        let change_pct = (recent_cost - previous_cost) / previous_cost;
        if change_pct > 0.10 {
            Ok("increasing".to_string())
        } else if change_pct < -0.10 {
            Ok("decreasing".to_string())
        } else {
            Ok("stable".to_string())
        }
    }
}

/// Fetch active (pending) cost_optimization recommendations.
fn query_active_cost_recommendations(pg_db: &Arc<PgDb>) -> Result<Vec<Recommendation>, String> {
    super::recommendations::list_recommendations(pg_db, Some("pipeline_prompt"), Some("pending")).map(
        |recs| {
            recs.into_iter()
                .filter(|r| r.recommendation_type == "cost_optimization")
                .collect()
        },
    )
}

/// Compute confidence based on sample size. Linearly scales from 0.5 at
/// `min_samples` to 0.95 at `max_samples`.
fn compute_confidence(run_count: i64, min_samples: i64, max_samples: i64) -> f64 {
    if run_count < min_samples {
        0.5
    } else if run_count >= max_samples {
        0.95
    } else {
        let range = (max_samples - min_samples) as f64;
        let progress = (run_count - min_samples) as f64 / range;
        0.5 + progress * 0.45
    }
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Build cost analysis summary with PG fallback (currently delegates to SQLite).
pub fn build_cost_analysis_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
) -> Result<CostAnalysisSummary, String> {
    build_cost_analysis(pg_db)
}

/// Generate cost recommendations with PG dual-write (fire-and-forget).
#[deprecated(note = "Use generate_cost_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn generate_cost_recommendations_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    existing_recommendations: &[Recommendation],
) -> Vec<Recommendation> {
    generate_cost_recommendations(pg_db, existing_recommendations)
}

// ---------------------------------------------------------------------------
// Phase 4: Cache-aware cost recommendations
// ---------------------------------------------------------------------------

/// Generate recommendations based on prompt cache efficiency.
///
/// Analyzes phase_token_usage for cache hit rates and identifies
/// optimization opportunities.
pub fn generate_cache_efficiency_recommendations(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    existing_recommendations: &[Recommendation],
) -> Vec<Recommendation> {
    let mut results: Vec<Recommendation> = Vec::new();

    // Collect recent titles (last 7 days) to avoid duplicates
    let recent_titles: Vec<String> = existing_recommendations
        .iter()
        .filter(|r| {
            if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&r.created_at) {
                let age = chrono::Utc::now().signed_duration_since(created);
                age.num_days() < 7
            } else {
                false
            }
        })
        .map(|r| r.title.clone())
        .collect();

    let has_recent = |title: &str| recent_titles.iter().any(|t| t == title);

    // Query cache stats from SQLite phase_token_usage (last 30 days)
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let cache_stats = match query_cache_stats(db, &since) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Cache efficiency: failed to query cache stats: {}", e);
            return results;
        }
    };

    let (total_input, total_cache_creation, total_cache_read, run_count) = cache_stats;

    if run_count < 10 {
        debug!(
            "Cache efficiency: insufficient data ({} runs, need 10+)",
            run_count
        );
        return results;
    }

    let denominator = total_cache_read as f64 + total_cache_creation as f64 + total_input as f64;
    if denominator <= 0.0 {
        debug!("Cache efficiency: no token data to analyze");
        return results;
    }

    let cache_hit_rate = total_cache_read as f64 / denominator;

    info!(
        "Cache efficiency: hit_rate={:.2}%, creation={}, read={}, input={}, runs={}",
        cache_hit_rate * 100.0,
        total_cache_creation,
        total_cache_read,
        total_input,
        run_count,
    );

    if cache_hit_rate < 0.5 {
        let title = "Low prompt cache hit rate".to_string();
        if has_recent(&title) {
            return results;
        }

        let confidence = compute_confidence(run_count, 10, 30);
        let description = format!(
            "Prompt cache hit rate is **{:.1}%** (target: ≥50%).\n\n\
             - Cache read tokens: {}\n\
             - Cache creation tokens: {}\n\
             - Regular input tokens: {}\n\
             - Runs analyzed: {}\n\n\
             Low cache hit rates mean the model re-processes prompts that could be cached. \
             Consider structuring system prompts as stable prefixes so they remain in cache \
             across calls, or increasing the cache TTL window.",
            cache_hit_rate * 100.0,
            total_cache_read,
            total_cache_creation,
            total_input,
            run_count,
        );
        let evidence = serde_json::json!({
            "cache_hit_rate": cache_hit_rate,
            "total_cache_read": total_cache_read,
            "total_cache_creation": total_cache_creation,
            "total_input_tokens": total_input,
            "run_count": run_count,
        })
        .to_string();

        match recommendations::create_recommendation(
            pg_db,
            "pipeline_prompt",
            "cost_optimization",
            None,
            &title,
            &description,
            None,
            None,
            Some(&evidence),
            confidence,
            None,
        ) {
            Ok(rec) => {
                info!("Cache efficiency recommendation created: {}", title);
                results.push(rec);
            }
            Err(e) => warn!("Failed to create cache recommendation: {}", e),
        }
    }

    results
}

/// Detect duplicate or near-duplicate prompts across pipeline agents.
///
/// Queries `phase_token_usage` to find phases that run with identical
/// (phase, model_used) pairs many times with high token counts, suggesting
/// repeated identical prompts that could benefit from caching or dedup.
pub fn detect_duplicate_prompts(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> Vec<Recommendation> {
    let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();

    // Query phase_token_usage for (phase, model_used) pairs with high repetition
    let conn = match db.get_conn() {
        Ok(c) => c,
        Err(e) => {
            debug!("Duplicate prompt detection: failed to get DB conn: {}", e);
            return Vec::new();
        }
    };

    let mut stmt = match conn.prepare(
        r#"SELECT phase, model_used,
                  COUNT(*) as run_count,
                  AVG(input_tokens) as avg_input,
                  MIN(input_tokens) as min_input,
                  MAX(input_tokens) as max_input
           FROM phase_token_usage
           WHERE created_at > ?1
             AND model_used IS NOT NULL
             AND input_tokens > 0
           GROUP BY phase, model_used
           HAVING COUNT(*) >= 10
           ORDER BY run_count DESC"#,
    ) {
        Ok(s) => s,
        Err(e) => {
            debug!("Duplicate prompt detection: query prep failed: {}", e);
            return Vec::new();
        }
    };

    struct PhaseRepetition {
        phase: String,
        model_used: String,
        run_count: i64,
        avg_input: f64,
        min_input: i64,
        max_input: i64,
    }

    let rows: Vec<PhaseRepetition> = match stmt.query_map(rusqlite::params![since], |row| {
        Ok(PhaseRepetition {
            phase: row.get(0)?,
            model_used: row.get(1)?,
            run_count: row.get(2)?,
            avg_input: row.get(3)?,
            min_input: row.get(4)?,
            max_input: row.get(5)?,
        })
    }) {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            debug!("Duplicate prompt detection: query failed: {}", e);
            return Vec::new();
        }
    };

    if rows.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    for row in &rows {
        // If min and max input tokens are within 10% of each other,
        // the prompts are likely near-identical (same template, same content).
        let range = row.max_input - row.min_input;
        let avg = row.avg_input;
        if avg <= 0.0 {
            continue;
        }
        let variation = range as f64 / avg;

        // Low variation (<10%) across many runs suggests duplicate prompts
        if variation < 0.10 && row.run_count >= 10 {
            let title = format!(
                "Duplicate prompts: {} phase ({} identical runs)",
                row.phase, row.run_count
            );

            let confidence = compute_confidence(row.run_count, 10, 30);
            let description = format!(
                "**{}** phase sent ~{:.0} input tokens per call across {} runs with <10% variation \
                 (min={}, max={}, model={}).\n\n\
                 This strongly suggests the same prompt is being sent repeatedly. \
                 Consider enabling prompt caching or deduplicating redundant calls.",
                row.phase,
                avg,
                row.run_count,
                row.min_input,
                row.max_input,
                row.model_used,
            );
            let evidence = serde_json::json!({
                "phase": row.phase,
                "model_used": row.model_used,
                "run_count": row.run_count,
                "avg_input_tokens": avg,
                "min_input_tokens": row.min_input,
                "max_input_tokens": row.max_input,
                "variation_pct": variation * 100.0,
            })
            .to_string();

            info!(
                "Duplicate prompt detected: phase={}, runs={}, variation={:.1}%",
                row.phase,
                row.run_count,
                variation * 100.0,
            );

            match recommendations::create_recommendation(
                pg_db,
                "pipeline_prompt",
                "cost_optimization",
                None,
                &title,
                &description,
                None,
                None,
                Some(&evidence),
                confidence,
                None,
            ) {
                Ok(rec) => results.push(rec),
                Err(e) => warn!("Failed to create duplicate prompt recommendation: {}", e),
            }

            // One recommendation per run is enough
            break;
        }
    }

    results
}

/// Identify phases where a cheaper model achieves similar pass rates.
///
/// Queries `phase_model_routing` Q-values to find phases where an expensive
/// model tier (opus) is dominant but a cheaper tier (sonnet) has comparable
/// Q-values, suggesting the cheaper model would work just as well.
pub fn detect_model_downgrade_opportunities(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> Vec<Recommendation> {
    let conn = match db.get_conn() {
        Ok(c) => c,
        Err(e) => {
            debug!("Model downgrade detection: failed to get DB conn: {}", e);
            return Vec::new();
        }
    };

    // Query phase_model_routing for phases with multiple model tiers
    let mut stmt = match conn.prepare(
        r#"SELECT phase, model_tier, q_value, visit_count
           FROM phase_model_routing
           WHERE visit_count >= 3
           ORDER BY phase, q_value DESC"#,
    ) {
        Ok(s) => s,
        Err(e) => {
            debug!("Model downgrade detection: query prep failed: {}", e);
            return Vec::new();
        }
    };

    struct RoutingRow {
        phase: String,
        model_tier: String,
        q_value: f64,
        visit_count: i64,
    }

    let rows: Vec<RoutingRow> = match stmt.query_map([], |row| {
        Ok(RoutingRow {
            phase: row.get(0)?,
            model_tier: row.get(1)?,
            q_value: row.get(2)?,
            visit_count: row.get(3)?,
        })
    }) {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            debug!("Model downgrade detection: query failed: {}", e);
            return Vec::new();
        }
    };

    if rows.is_empty() {
        return Vec::new();
    }

    // Group by phase
    use std::collections::HashMap;
    let mut by_phase: HashMap<String, Vec<&RoutingRow>> = HashMap::new();
    for row in &rows {
        by_phase.entry(row.phase.clone()).or_default().push(row);
    }

    // Model tier cost ranking (higher index = more expensive).
    // Tiers in phase_model_routing are stored as "simple"/"medium"/"complex"
    // (from model_q_router.rs tier_to_str()), not model family names.
    let tier_cost_rank = |tier: &str| -> u32 {
        match tier.to_lowercase().as_str() {
            "simple" => 0,
            "medium" => 1,
            "complex" => 2,
            _ => 1, // default to medium
        }
    };

    let mut results = Vec::new();

    for (phase, tiers) in &by_phase {
        if tiers.len() < 2 {
            continue;
        }

        // Find the most-used (highest visit_count) tier
        let dominant = tiers.iter().max_by_key(|t| t.visit_count).unwrap();
        let dominant_rank = tier_cost_rank(&dominant.model_tier);

        // Look for cheaper tiers with comparable Q-values (within 15%)
        for tier in tiers {
            let tier_rank = tier_cost_rank(&tier.model_tier);
            if tier_rank >= dominant_rank {
                continue; // Not cheaper
            }
            if tier.visit_count < 3 {
                continue; // Insufficient data
            }

            // Q-value comparison: cheaper tier within 15% of dominant
            let q_gap = if dominant.q_value.abs() > 0.001 {
                (dominant.q_value - tier.q_value).abs() / dominant.q_value.abs()
            } else {
                // Both near zero — comparable
                0.0
            };

            if q_gap <= 0.15 {
                let title = format!(
                    "Model downgrade opportunity: {} phase ({}→{})",
                    phase, dominant.model_tier, tier.model_tier
                );

                let total_visits: i64 = tiers.iter().map(|t| t.visit_count).sum();
                let confidence = compute_confidence(total_visits, 10, 30);
                let description = format!(
                    "**{}** phase primarily uses **{}** (Q={:.3}, {} visits) but **{}** \
                     has comparable quality (Q={:.3}, {} visits, {:.1}% gap).\n\n\
                     Switching to the cheaper tier could reduce cost without \
                     meaningful quality loss. Consider adjusting model routing \
                     to prefer the cheaper tier for this phase.",
                    phase,
                    dominant.model_tier,
                    dominant.q_value,
                    dominant.visit_count,
                    tier.model_tier,
                    tier.q_value,
                    tier.visit_count,
                    q_gap * 100.0,
                );
                let evidence = serde_json::json!({
                    "phase": phase,
                    "dominant_tier": dominant.model_tier,
                    "dominant_q_value": dominant.q_value,
                    "dominant_visits": dominant.visit_count,
                    "cheaper_tier": tier.model_tier,
                    "cheaper_q_value": tier.q_value,
                    "cheaper_visits": tier.visit_count,
                    "q_gap_pct": q_gap * 100.0,
                })
                .to_string();

                info!(
                    "Model downgrade opportunity: phase={}, {}→{}, q_gap={:.1}%",
                    phase, dominant.model_tier, tier.model_tier, q_gap * 100.0,
                );

                match recommendations::create_recommendation(
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    None,
                    &title,
                    &description,
                    Some(&dominant.model_tier),
                    Some(&tier.model_tier),
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => results.push(rec),
                    Err(e) => warn!("Failed to create model downgrade recommendation: {}", e),
                }

                break; // One recommendation per phase
            }
        }

        // Limit total recommendations
        if results.len() >= 3 {
            break;
        }
    }

    results
}

/// Query aggregate cache token stats from SQLite phase_token_usage.
///
/// Returns (total_input, total_cache_creation, total_cache_read, run_count).
fn query_cache_stats(
    db: &CheckpointDb,
    since: &str,
) -> Result<(i64, i64, i64, i64), String> {
    let conn = db.get_conn()?;
    let mut stmt = conn
        .prepare(
            r#"SELECT
                COALESCE(SUM(input_tokens), 0) as total_input,
                COALESCE(SUM(cache_creation_tokens), 0) as total_creation,
                COALESCE(SUM(cache_read_tokens), 0) as total_read,
                COUNT(DISTINCT task_run_id) as run_count
            FROM phase_token_usage
            WHERE created_at > ?1"#,
        )
        .map_err(|e| format!("Failed to prepare cache stats query: {}", e))?;

    let row = stmt
        .query_row(rusqlite::params![since], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| format!("Failed to query cache stats: {}", e))?;

    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_confidence() {
        assert!((compute_confidence(3, 10, 30) - 0.5).abs() < 0.001);
        assert!((compute_confidence(10, 10, 30) - 0.5).abs() < 0.001);
        assert!((compute_confidence(20, 10, 30) - 0.725).abs() < 0.001);
        assert!((compute_confidence(30, 10, 30) - 0.95).abs() < 0.001);
        assert!((compute_confidence(50, 10, 30) - 0.95).abs() < 0.001);
    }
}
