//! Cost Efficiency Optimizer
//!
//! Analyzes token/cost data from `pipeline_agent_traces` and generates
//! actionable cost-optimization recommendations. Runs as a data-driven
//! pass (no AI session needed) and appends results to the recommendation
//! table alongside other optimizer outputs.

use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::recommendations;
use super::types::Recommendation;
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

    // Query per-phase cache stats for the last 30 days
    let phase_cache_stats = match query_phase_cache_stats(pg_db) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Cache efficiency: failed to query phase cache stats: {}", e);
            return results;
        }
    };

    if phase_cache_stats.is_empty() {
        debug!("Cache efficiency: no phase_token_usage data with cache metrics");
        return results;
    }

    info!(
        "Cache efficiency: analyzing {} phases",
        phase_cache_stats.len()
    );

    for stat in &phase_cache_stats {
        if stat.run_count < 5 {
            continue;
        }

        let total_input_plus_cache = stat.total_input + stat.total_cache_creation + stat.total_cache_read;
        let cache_hit_rate = if total_input_plus_cache > 0 {
            stat.total_cache_read as f64 / total_input_plus_cache as f64
        } else {
            0.0
        };

        // Check 1: Low cache hit rate (< 20%)
        if cache_hit_rate < 0.20 && total_input_plus_cache > 0 {
            let title = format!("Low cache efficiency in {} phase", stat.phase);
            if !has_recent(&title) {
                let confidence = compute_confidence(stat.run_count, 5, 30);
                let description = format!(
                    "**{}** has a cache hit rate of only {:.1}% over {} runs.\n\n\
                     - Input tokens: {}\n\
                     - Cache creation tokens: {}\n\
                     - Cache read tokens: {}\n\n\
                     Consider using stable prompt prefixes and moving variable \
                     content to the end of prompts to improve cache reuse.",
                    stat.phase,
                    cache_hit_rate * 100.0,
                    stat.run_count,
                    stat.total_input,
                    stat.total_cache_creation,
                    stat.total_cache_read,
                );
                let evidence = serde_json::json!({
                    "phase": stat.phase,
                    "cache_hit_rate": cache_hit_rate,
                    "total_input_tokens": stat.total_input,
                    "total_cache_creation_tokens": stat.total_cache_creation,
                    "total_cache_read_tokens": stat.total_cache_read,
                    "run_count": stat.run_count,
                })
                .to_string();

                match recommendations::create_recommendation(
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    Some(&stat.phase),
                    &title,
                    &description,
                    None,
                    None,
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => {
                        info!("Cache efficiency recommendation: {}", title);
                        results.push(rec);
                    }
                    Err(e) => warn!("Failed to create cache recommendation: {}", e),
                }
            }
        }

        // Check 2: Cache creation >> cache read (prompts being recreated without reuse)
        if stat.total_cache_creation > 0
            && stat.total_cache_creation > stat.total_cache_read * 3
        {
            let title = format!("Excessive cache recreation in {} phase", stat.phase);
            if !has_recent(&title) {
                let ratio = if stat.total_cache_read > 0 {
                    stat.total_cache_creation as f64 / stat.total_cache_read as f64
                } else {
                    f64::INFINITY
                };
                let confidence = compute_confidence(stat.run_count, 5, 30);
                let description = format!(
                    "**{}** creates {:.1}x more cache tokens than it reads back ({} creation vs {} read).\n\n\
                     This suggests prompts change frequently, invalidating the cache. \
                     Stabilize the prompt prefix across runs to maximize cache hits.",
                    stat.phase,
                    ratio,
                    stat.total_cache_creation,
                    stat.total_cache_read,
                );
                let evidence = serde_json::json!({
                    "phase": stat.phase,
                    "cache_creation_tokens": stat.total_cache_creation,
                    "cache_read_tokens": stat.total_cache_read,
                    "creation_to_read_ratio": ratio,
                    "run_count": stat.run_count,
                })
                .to_string();

                match recommendations::create_recommendation(
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    Some(&stat.phase),
                    &title,
                    &description,
                    None,
                    None,
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => {
                        info!("Cache efficiency recommendation: {}", title);
                        results.push(rec);
                    }
                    Err(e) => warn!("Failed to create cache recommendation: {}", e),
                }
            }
        }
    }

    info!(
        "Cache efficiency: generated {} recommendations",
        results.len()
    );
    results
}

/// Detect duplicate or near-duplicate prompts across pipeline agents.
///
/// Queries `phase_token_usage` to find phases that run with identical
/// (phase, model_used) pairs many times with high token counts, suggesting
/// repeated identical prompts that could benefit from caching or dedup.
pub fn detect_duplicate_prompts(
    pg_db: &Arc<PgDb>,
) -> Vec<Recommendation> {
    let mut results: Vec<Recommendation> = Vec::new();

    // Load existing recommendations for deduplication
    let existing = super::recommendations::list_recommendations(pg_db, Some("pipeline_prompt"), None)
        .unwrap_or_default();
    let recent_titles: Vec<String> = existing
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

    // Query per-(phase, model) token stats for last 30 days
    let group_stats = match query_phase_model_token_stats(pg_db) {
        Ok(stats) => stats,
        Err(e) => {
            warn!("Duplicate prompt detection: failed to query stats: {}", e);
            return results;
        }
    };

    for stat in &group_stats {
        if stat.run_count < 10 {
            continue;
        }

        let mean = stat.mean_input_tokens;
        let std_dev = stat.std_dev_input_tokens;

        // Skip if mean is too small (not worth optimizing)
        if mean < 5000.0 {
            continue;
        }

        // Coefficient of variation: very low CV means near-identical prompts
        let cv = if mean > 0.0 { std_dev / mean } else { 1.0 };
        if cv < 0.05 {
            let model_display = stat.model_used.as_deref().unwrap_or("unknown");
            let title = format!(
                "Potential duplicate prompts: {} ({})",
                stat.phase, model_display
            );
            if has_recent(&title) {
                continue;
            }

            let confidence = compute_confidence(stat.run_count, 10, 50);
            let description = format!(
                "**{} / {}** has {} runs with nearly identical input token counts \
                 (mean: {:.0}, CV: {:.4}).\n\n\
                 This suggests the same prompt is being sent repeatedly without \
                 variation. Consider:\n\
                 - Enabling prompt caching to avoid redundant processing\n\
                 - Caching the AI response if the input is truly identical\n\
                 - Deduplicating calls at the orchestration layer",
                stat.phase,
                model_display,
                stat.run_count,
                mean,
                cv,
            );
            let evidence = serde_json::json!({
                "phase": stat.phase,
                "model_used": model_display,
                "run_count": stat.run_count,
                "mean_input_tokens": mean,
                "std_dev_input_tokens": std_dev,
                "coefficient_of_variation": cv,
            })
            .to_string();

            match recommendations::create_recommendation(
                pg_db,
                "pipeline_prompt",
                "cost_optimization",
                Some(&stat.phase),
                &title,
                &description,
                None,
                None,
                Some(&evidence),
                confidence,
                None,
            ) {
                Ok(rec) => {
                    info!("Duplicate prompt recommendation: {}", title);
                    results.push(rec);
                }
                Err(e) => warn!("Failed to create duplicate prompt recommendation: {}", e),
            }
        }
    }

    info!(
        "Duplicate prompt detection: generated {} recommendations",
        results.len()
    );
    results
}

/// Identify phases where a cheaper model achieves similar pass rates.
///
/// Queries `phase_model_routing` Q-values to find phases where an expensive
/// model tier (opus) is dominant but a cheaper tier (sonnet) has comparable
/// Q-values, suggesting the cheaper model would work just as well.
pub fn detect_model_downgrade_opportunities(
    pg_db: &Arc<PgDb>,
) -> Vec<Recommendation> {
    let mut results: Vec<Recommendation> = Vec::new();

    // Load existing recommendations for deduplication
    let existing = super::recommendations::list_recommendations(pg_db, Some("pipeline_prompt"), None)
        .unwrap_or_default();
    let recent_titles: Vec<String> = existing
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

    // Load Q-routing table entries: (state_key, action, q_value, visit_count)
    let q_entries = match tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_all_q_entries())
    }) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Model downgrade detection: failed to load Q-table: {}", e);
            return results;
        }
    };

    if q_entries.is_empty() {
        debug!("Model downgrade detection: Q-table is empty");
        return results;
    }

    // Group by state_key
    let mut by_state: HashMap<String, Vec<(String, f64, u32)>> = HashMap::new();
    for (state_key, action, q_value, visits) in &q_entries {
        by_state
            .entry(state_key.clone())
            .or_default()
            .push((action.clone(), *q_value, *visits));
    }

    // Model tier classification
    let is_expensive = |action: &str| -> bool {
        let lower = action.to_lowercase();
        lower.contains("opus") || lower.contains("gpt-4o") || lower.contains("gpt-4-")
    };
    let is_cheap = |action: &str| -> bool {
        let lower = action.to_lowercase();
        lower.contains("sonnet") || lower.contains("haiku") || lower.contains("gpt-4o-mini")
            || lower.contains("gpt-3.5") || lower.contains("flash")
    };

    for (state_key, actions) in &by_state {
        // Find best expensive model and best cheap model
        let mut best_expensive: Option<(&str, f64, u32)> = None;
        let mut best_cheap: Option<(&str, f64, u32)> = None;

        for (action, q_value, visits) in actions {
            if *visits < 10 {
                continue;
            }
            if is_expensive(action) {
                if best_expensive.is_none() || *q_value > best_expensive.unwrap().1 {
                    best_expensive = Some((action.as_str(), *q_value, *visits));
                }
            }
            if is_cheap(action) {
                if best_cheap.is_none() || *q_value > best_cheap.unwrap().1 {
                    best_cheap = Some((action.as_str(), *q_value, *visits));
                }
            }
        }

        // If cheap model's Q-value >= 90% of expensive model's Q-value, suggest downgrade
        if let (Some((exp_model, exp_q, exp_visits)), Some((cheap_model, cheap_q, cheap_visits))) =
            (best_expensive, best_cheap)
        {
            if exp_q > 0.0 && cheap_q >= exp_q * 0.90 {
                let title = format!(
                    "Model downgrade opportunity: {} ({}→{})",
                    state_key, exp_model, cheap_model
                );
                if has_recent(&title) {
                    continue;
                }

                let q_ratio = cheap_q / exp_q;
                let total_visits = (exp_visits + cheap_visits) as i64;
                let confidence = compute_confidence(total_visits, 20, 60);
                // Rough savings estimate: expensive models cost ~5x more
                let estimated_savings_pct = (1.0 - 1.0 / 5.0) * 100.0; // ~80%
                let description = format!(
                    "For context **{}**, the cheaper model **{}** achieves {:.1}% of \
                     the Q-value of **{}** ({:.3} vs {:.3}).\n\n\
                     - Expensive model visits: {}\n\
                     - Cheaper model visits: {}\n\
                     - Estimated cost reduction: ~{:.0}%\n\n\
                     Consider routing this context to the cheaper model to reduce cost \
                     with minimal quality impact.",
                    state_key,
                    cheap_model,
                    q_ratio * 100.0,
                    exp_model,
                    cheap_q,
                    exp_q,
                    exp_visits,
                    cheap_visits,
                    estimated_savings_pct,
                );
                let evidence = serde_json::json!({
                    "state_key": state_key,
                    "expensive_model": exp_model,
                    "expensive_q_value": exp_q,
                    "expensive_visits": exp_visits,
                    "cheap_model": cheap_model,
                    "cheap_q_value": cheap_q,
                    "cheap_visits": cheap_visits,
                    "q_ratio": q_ratio,
                })
                .to_string();

                match recommendations::create_recommendation(
                    pg_db,
                    "pipeline_prompt",
                    "cost_optimization",
                    None,
                    &title,
                    &description,
                    Some(exp_model),
                    Some(cheap_model),
                    Some(&evidence),
                    confidence,
                    None,
                ) {
                    Ok(rec) => {
                        info!("Model downgrade recommendation: {}", title);
                        results.push(rec);
                    }
                    Err(e) => warn!("Failed to create model downgrade recommendation: {}", e),
                }
            }
        }
    }

    info!(
        "Model downgrade detection: generated {} recommendations",
        results.len()
    );
    results
}

// ---------------------------------------------------------------------------
// Helper structs and query functions for Phase 4 recommendations
// ---------------------------------------------------------------------------

/// Per-phase cache statistics.
#[derive(Debug, Clone)]
struct PhaseCacheStats {
    phase: String,
    total_input: i64,
    total_cache_creation: i64,
    total_cache_read: i64,
    run_count: i64,
}

/// Query per-phase cache token stats from phase_token_usage (last 30 days).
fn query_phase_cache_stats(pg_db: &Arc<PgDb>) -> Result<Vec<PhaseCacheStats>, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let conn = pg_db
                .pool()
                .get()
                .await
                .map_err(|e| format!("PG pool error: {}", e))?;
            let rows = conn
                .query(
                    r#"SELECT
                        phase,
                        COALESCE(SUM(input_tokens), 0)::bigint as total_input,
                        COALESCE(SUM(cache_creation_tokens), 0)::bigint as total_cache_creation,
                        COALESCE(SUM(cache_read_tokens), 0)::bigint as total_cache_read,
                        COUNT(*)::bigint as run_count
                    FROM phase_token_usage
                    WHERE created_at > $1::timestamptz
                    GROUP BY phase
                    HAVING COUNT(*) >= 5
                    ORDER BY total_input DESC"#,
                    &[&since],
                )
                .await
                .map_err(|e| format!("PG query phase cache stats: {}", e))?;

            Ok(rows
                .iter()
                .map(|r| PhaseCacheStats {
                    phase: r.get(0),
                    total_input: r.get(1),
                    total_cache_creation: r.get(2),
                    total_cache_read: r.get(3),
                    run_count: r.get(4),
                })
                .collect())
        })
    })
}

/// Per-(phase, model) token statistics for duplicate detection.
#[derive(Debug, Clone)]
struct PhaseModelTokenStats {
    phase: String,
    model_used: Option<String>,
    run_count: i64,
    mean_input_tokens: f64,
    std_dev_input_tokens: f64,
}

/// Query per-(phase, model_used) input token stats for duplicate prompt detection.
fn query_phase_model_token_stats(pg_db: &Arc<PgDb>) -> Result<Vec<PhaseModelTokenStats>, String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let conn = pg_db
                .pool()
                .get()
                .await
                .map_err(|e| format!("PG pool error: {}", e))?;
            let rows = conn
                .query(
                    r#"SELECT
                        phase,
                        model_used,
                        COUNT(*)::bigint as run_count,
                        AVG(input_tokens)::float8 as mean_input,
                        COALESCE(STDDEV_POP(input_tokens), 0)::float8 as stddev_input
                    FROM phase_token_usage
                    WHERE created_at > $1::timestamptz
                    GROUP BY phase, model_used
                    HAVING COUNT(*) >= 10
                    ORDER BY mean_input DESC"#,
                    &[&since],
                )
                .await
                .map_err(|e| format!("PG query phase model token stats: {}", e))?;

            Ok(rows
                .iter()
                .map(|r| PhaseModelTokenStats {
                    phase: r.get(0),
                    model_used: r.get(1),
                    run_count: r.get(2),
                    mean_input_tokens: r.get(3),
                    std_dev_input_tokens: r.get(4),
                })
                .collect())
        })
    })
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
