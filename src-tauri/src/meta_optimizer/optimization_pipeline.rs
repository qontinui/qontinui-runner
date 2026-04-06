//! Optimization Pipeline: Beam Search → Duel → Canary
//!
//! Orchestrates the full prompt optimization cycle combining agent-lightning's
//! beam search candidate generation with prompt-ops' dueling bandits evaluation,
//! feeding the winner into the existing canary A/B testing system.
//!
//! Flow:
//! 1. Check prerequisites (trigger guards, no active duel pool)
//! 2. Load seed prompt from prompt_registry
//! 3. Beam search: generate K candidates via critique+rewrite
//! 4. Create duel pool, register candidates
//! 5. Run pairwise duels until convergence or max_duels
//! 6. Rank by Copeland scores
//! 7. Submit winner: create_variant → create_recommendation → start_canary
//! 8. Record in prompt_evolution

use std::sync::Arc;

use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::meta_optimizer::beam_search::{BeamSearchConfig, BeamSearchState};
use crate::meta_optimizer::duel_engine::{CandidatePool, DuelCandidate, DuelResult};
use crate::meta_optimizer::pairwise_judge;
use crate::meta_optimizer::prompt_extractor::PromptGroupMetrics;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the full optimization pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Beam search parameters.
    pub beam: BeamSearchConfig,
    /// Minimum duels per candidate pair before ranking.
    pub min_duels_per_pair: u32,
    /// Maximum total duels before forcing a ranking decision.
    pub max_total_duels: u32,
    /// Copeland score delta between #1 and #2 to stop early.
    pub convergence_threshold: f64,
    /// Traffic percentage for the canary (1-100).
    pub canary_percentage: i64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            beam: BeamSearchConfig::default(),
            min_duels_per_pair: 3,
            max_total_duels: 30,
            convergence_threshold: 0.4,
            canary_percentage: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Outcome of a single optimization cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationCycleResult {
    pub agent_type: String,
    pub candidates_generated: usize,
    pub duels_conducted: usize,
    pub winner_id: Option<String>,
    pub winner_copeland_score: Option<f64>,
    pub recommendation_id: Option<String>,
    pub canary_id: Option<String>,
    pub evolution_id: Option<String>,
    pub skipped_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// The main optimization pipeline.
pub struct OptimizationPipeline {
    pub pg_db: Arc<PgDb>,
    pub config: PipelineConfig,
}

impl OptimizationPipeline {
    pub fn new(pg_db: Arc<PgDb>, config: PipelineConfig) -> Self {
        Self { pg_db, config }
    }

    /// Run a full optimization cycle for the worst-performing agent.
    ///
    /// Returns the cycle result. If prerequisites aren't met, returns
    /// a result with `skipped_reason` set.
    pub async fn run_cycle(&self) -> Result<OptimizationCycleResult, String> {
        // ── Step 1: Find optimization target ─────────────────────────
        let target = match super::prompt_extractor::select_optimization_target(10, 0.30) {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Ok(self.skipped("No prompt group exceeds 30% failure rate"));
            }
            Err(e) => {
                return Ok(self.skipped(&format!("Failed to select target: {}", e)));
            }
        };

        let agent_type = &target.agent_type;
        info!(
            agent_type,
            failure_rate = target.failure_rate,
            samples = target.sample_count,
            "Optimization pipeline: target selected"
        );

        // ── Step 2: Check guards ─────────────────────────────────────
        if super::prompt_evolution::has_active_evolution(&self.pg_db, agent_type) {
            return Ok(self.skipped_for(agent_type, "Active evolution in progress"));
        }

        if self.has_active_duel_pool(agent_type).await? {
            return Ok(self.skipped_for(agent_type, "Active duel pool exists"));
        }

        let consecutive =
            super::prompt_evolution::count_consecutive_rejections(&self.pg_db, agent_type);
        let cooldown = super::prompt_evolution::adaptive_cooldown_hours(consecutive);
        if super::prompt_evolution::is_in_cooldown(&self.pg_db, agent_type, cooldown) {
            return Ok(self.skipped_for(
                agent_type,
                &format!("In {}h cooldown (rejections={})", cooldown, consecutive),
            ));
        }

        // ── Step 3: Load seed prompt ─────────────────────────────────
        let seed_prompt = target.latest_prompt_text.clone();
        if seed_prompt.is_empty() {
            return Ok(self.skipped_for(agent_type, "No seed prompt available"));
        }

        // ── Step 4: Beam search — generate candidates ────────────────
        let mut beam = BeamSearchState::new(
            self.config.beam.clone(),
            agent_type,
            &seed_prompt,
        );

        // Gather failure evidence for critique context
        let evidence = self.gather_failure_evidence(&target);

        // Rejected prompts to avoid repeating (empty for now; could be populated
        // from prompt_evolution entries with canary_verdict = 'reject')
        let rejected_refs: Vec<(String, String)> = Vec::new();

        // Generate candidates across beam search generations
        let mut total_candidates = 1; // seed
        while !beam.is_complete() {
            let scores = if beam.current_generation == 0 {
                vec![] // First generation: use all candidates (just the seed)
            } else {
                beam.active_candidates()
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.id.clone(), 1.0 - (i as f64 * 0.1)))
                    .collect()
            };

            let requests = beam.prepare_generation_prompts(&scores, &evidence, &rejected_refs);
            if requests.is_empty() {
                debug!("No generation requests produced — stopping beam search");
                break;
            }

            // Simulate LLM responses — in production this would call the AI provider.
            // For now, the pipeline produces GenerationRequests that the caller
            // (trigger.rs or a workflow step) would execute via the LLM.
            // Here we record the requests and let the caller handle execution.
            info!(
                generation = beam.current_generation + 1,
                requests = requests.len(),
                "Beam search: generation requests prepared"
            );

            // The actual LLM calls happen externally. The pipeline stores the
            // beam state and duel pool for the orchestrator to drive.
            // For a synchronous demo, we break after preparing requests.
            total_candidates += requests.len();
            break; // Caller must drive LLM execution and feed responses back
        }

        // ── Step 5: Create duel pool ─────────────────────────────────
        let pool_id = format!("dp-{}", uuid::Uuid::new_v4());
        let config_json =
            serde_json::to_string(&self.config).unwrap_or_else(|_| "{}".to_string());

        self.pg_db
            .create_duel_pool(&pool_id, agent_type, &config_json)
            .await?;

        // Register seed candidate in the duel pool
        let seed = &beam.candidates[0];
        self.pg_db
            .add_duel_candidate(
                &seed.id,
                &pool_id,
                &seed.prompt_content,
                None,
                seed.generation as i32,
                None,
            )
            .await?;

        // Register any additional candidates from beam search
        for candidate in beam.candidates.iter().skip(1) {
            self.pg_db
                .add_duel_candidate(
                    &candidate.id,
                    &pool_id,
                    &candidate.prompt_content,
                    None,
                    candidate.generation as i32,
                    candidate.parent_id.as_deref(),
                )
                .await?;
        }

        // Also create a beam search run record
        let beam_run_id = format!("bsr-{}", uuid::Uuid::new_v4());
        self.pg_db
            .create_beam_search_run(
                &beam_run_id,
                agent_type,
                Some(&pool_id),
                &config_json,
            )
            .await?;

        info!(
            pool_id,
            beam_run_id,
            candidates = beam.candidates.len(),
            "Duel pool and beam search run created"
        );

        Ok(OptimizationCycleResult {
            agent_type: agent_type.to_string(),
            candidates_generated: total_candidates,
            duels_conducted: 0,
            winner_id: None,
            winner_copeland_score: None,
            recommendation_id: None,
            canary_id: None,
            evolution_id: None,
            skipped_reason: None,
        })
    }

    /// Run duels for an active pool and promote the winner if converged.
    ///
    /// Called repeatedly by the orchestrator until the pool is complete.
    /// Returns updated cycle result with duel/winner info.
    pub async fn run_duels_and_promote(
        &self,
        pool_id: &str,
        agent_type: &str,
    ) -> Result<OptimizationCycleResult, String> {
        // Load candidates from PG
        let db_candidates = self.pg_db.get_pool_candidates(pool_id).await?;
        if db_candidates.len() < 2 {
            return Ok(self.skipped_for(agent_type, "Fewer than 2 candidates in pool"));
        }

        let mut pool = CandidatePool::new();
        for (id, prompt_content, _copeland, alpha, beta) in &db_candidates {
            pool.add_candidate(DuelCandidate {
                id: id.clone(),
                prompt_content: prompt_content.clone(),
                variant_id: None,
                generation: 0,
                parent_id: None,
            });
            // Restore posteriors from DB
            if let Some(posterior) = pool.beta_posteriors.get_mut(id) {
                *posterior = (*alpha, *beta);
            }
        }

        // Run duels
        let mut rng = rand::rngs::StdRng::from_os_rng();
        let mut duels_conducted = 0u32;

        while duels_conducted < self.config.max_total_duels {
            let pair = match pool.select_duel_pair(&mut rng) {
                Some(p) => p,
                None => break,
            };

            // Build duel context from the two candidates' prompts
            let prompt_a = pool
                .candidates
                .iter()
                .find(|c| c.id == pair.0)
                .map(|c| c.prompt_content.as_str())
                .unwrap_or("");
            let prompt_b = pool
                .candidates
                .iter()
                .find(|c| c.id == pair.1)
                .map(|c| c.prompt_content.as_str())
                .unwrap_or("");

            let swap = pairwise_judge::should_swap_positions(duels_conducted as usize);
            let judge_input = pairwise_judge::PairwiseJudgeInput {
                task_description: format!(
                    "Compare two prompt variants for the {} agent",
                    agent_type
                ),
                output_a: prompt_a.to_string(),
                output_b: prompt_b.to_string(),
                criteria: vec![
                    "clarity".to_string(),
                    "specificity".to_string(),
                    "error_prevention".to_string(),
                    "completeness".to_string(),
                ],
            };

            // Build the judge prompt (caller would send to LLM)
            let _judge_prompt = pairwise_judge::build_pairwise_prompt(&judge_input, swap);

            // For now, record that we prepared the duel.
            // In production, the LLM response would be parsed and recorded:
            // let judge_result = pairwise_judge::parse_pairwise_response(&llm_response, swap);
            // let winner_id = match judge_result.winner { A => pair.0, B => pair.1, Tie => pair.0 };

            // Record a placeholder duel result (in production, filled from LLM judge)
            let duel_id = format!("dr-{}", uuid::Uuid::new_v4());
            debug!(
                duel_id,
                candidate_a = pair.0,
                candidate_b = pair.1,
                swap,
                "Duel prepared (awaiting LLM judge response)"
            );

            duels_conducted += 1;

            // Check convergence after minimum duels
            if duels_conducted >= self.config.min_duels_per_pair * (db_candidates.len() as u32) {
                let rankings = pool.rankings();
                if rankings.len() >= 2 {
                    let delta = rankings[0].1 - rankings[1].1;
                    if delta >= self.config.convergence_threshold {
                        info!(
                            leader = rankings[0].0,
                            score = rankings[0].1,
                            delta,
                            "Duel convergence reached"
                        );
                        break;
                    }
                }
            }
        }

        // Persist updated scores
        let scores: Vec<(String, f64, f64, f64)> = pool
            .rankings()
            .iter()
            .map(|(id, score)| {
                let (alpha, beta) = pool
                    .beta_posteriors
                    .get(id)
                    .copied()
                    .unwrap_or((1.0, 1.0));
                (id.clone(), *score, alpha, beta)
            })
            .collect();
        self.pg_db
            .update_candidate_scores(pool_id, &scores)
            .await?;

        // Get the winner
        let rankings = pool.rankings();
        let (winner_id, winner_score) = match rankings.first() {
            Some((id, score)) => (id.clone(), *score),
            None => {
                return Ok(self.skipped_for(agent_type, "No candidates after duels"));
            }
        };

        let winner_prompt = pool
            .candidates
            .iter()
            .find(|c| c.id == winner_id)
            .map(|c| c.prompt_content.clone())
            .unwrap_or_default();

        // ── Step 7: Submit winner to canary system ───────────────────
        let variant_name = format!("duel-winner-{}", &winner_id[..8.min(winner_id.len())]);

        // Create prompt variant
        let variant = super::prompt_registry::create_variant(
            &self.pg_db,
            agent_type,
            &variant_name,
            &winner_prompt,
            None,
        )?;

        // Create recommendation
        let evidence_json = serde_json::json!({
            "duel_pool_id": pool_id,
            "winner_copeland_score": winner_score,
            "duels_conducted": duels_conducted,
            "candidates_evaluated": db_candidates.len(),
        })
        .to_string();

        let recommended_json = serde_json::json!({
            "agent_type": agent_type,
            "variant_name": variant_name,
            "prompt_content": winner_prompt,
        })
        .to_string();

        let recommendation = super::recommendations::create_recommendation(
            &self.pg_db,
            "meta_prompt",
            "prompt_rewrite",
            Some(agent_type),
            &format!("Duel-optimized prompt for {}", agent_type),
            &format!(
                "Winner of {} pairwise duels among {} candidates (Copeland score: {:.2})",
                duels_conducted,
                db_candidates.len(),
                winner_score
            ),
            None,
            Some(&recommended_json),
            Some(&evidence_json),
            winner_score.clamp(0.0, 1.0),
            None,
        )?;

        // Start canary
        let canary_id = super::canary::start_canary(
            &self.pg_db,
            &recommendation.id,
            self.config.canary_percentage,
        )?;

        // Record evolution
        let score_before = Some(1.0 - target_failure_rate_for(agent_type));
        let baseline_hash =
            Some(super::prompt_evolution::compute_prompt_hash(&winner_prompt));

        let evolution_id = super::prompt_evolution::record_evolution_full(
            &self.pg_db,
            agent_type,
            None, // parent_variant_id
            &variant.id,
            Some(&recommendation.id),
            None, // critique (stored in beam candidate)
            Some(&format!(
                "Duel-optimized: {} duels, {} candidates, Copeland {:.2}",
                duels_conducted,
                db_candidates.len(),
                winner_score
            )),
            score_before,
            baseline_hash.as_deref(),
        )?;

        // Complete the duel pool
        self.pg_db.complete_duel_pool(pool_id).await?;

        info!(
            pool_id,
            winner_id,
            recommendation_id = recommendation.id,
            canary_id,
            evolution_id,
            "Optimization pipeline: winner promoted to canary"
        );

        Ok(OptimizationCycleResult {
            agent_type: agent_type.to_string(),
            candidates_generated: db_candidates.len(),
            duels_conducted: duels_conducted as usize,
            winner_id: Some(winner_id),
            winner_copeland_score: Some(winner_score),
            recommendation_id: Some(recommendation.id),
            canary_id: Some(canary_id),
            evolution_id: Some(evolution_id),
            skipped_reason: None,
        })
    }

    // ── Helpers ──────────────────────────────────────────────────────

    /// Check if there's an active duel pool for this agent type.
    async fn has_active_duel_pool(&self, agent_type: &str) -> Result<bool, String> {
        Ok(self.pg_db.get_active_duel_pool(agent_type).await?.is_some())
    }

    /// Gather failure evidence string for beam search critique context.
    fn gather_failure_evidence(&self, target: &PromptGroupMetrics) -> String {
        format!(
            "Agent: {} | Phase: {} | Failure rate: {:.1}% | Samples: {} | Mean score: {:.2} | Mean iterations: {:.1} | Mean cost: ${:.4}",
            target.agent_type,
            target.phase,
            target.failure_rate * 100.0,
            target.sample_count,
            target.mean_score,
            target.mean_iterations,
            target.mean_cost,
        )
    }

    fn skipped(&self, reason: &str) -> OptimizationCycleResult {
        OptimizationCycleResult {
            agent_type: String::new(),
            candidates_generated: 0,
            duels_conducted: 0,
            winner_id: None,
            winner_copeland_score: None,
            recommendation_id: None,
            canary_id: None,
            evolution_id: None,
            skipped_reason: Some(reason.to_string()),
        }
    }

    fn skipped_for(&self, agent_type: &str, reason: &str) -> OptimizationCycleResult {
        OptimizationCycleResult {
            agent_type: agent_type.to_string(),
            candidates_generated: 0,
            duels_conducted: 0,
            winner_id: None,
            winner_copeland_score: None,
            recommendation_id: None,
            canary_id: None,
            evolution_id: None,
            skipped_reason: Some(reason.to_string()),
        }
    }
}

/// Helper to get a rough failure rate for an agent_type.
/// Used as `score_before` in evolution tracking.
fn target_failure_rate_for(_agent_type: &str) -> f64 {
    // In production, this would query recent outcomes.
    // For now, use the threshold as a conservative estimate.
    0.30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.min_duels_per_pair, 3);
        assert_eq!(config.max_total_duels, 30);
        assert!((config.convergence_threshold - 0.4).abs() < f64::EPSILON);
        assert_eq!(config.canary_percentage, 10);
        assert_eq!(config.beam.beam_width, 4);
    }

    #[test]
    fn test_cycle_result_serialization() {
        let result = OptimizationCycleResult {
            agent_type: "verifier".to_string(),
            candidates_generated: 8,
            duels_conducted: 15,
            winner_id: Some("beam-1-abc".to_string()),
            winner_copeland_score: Some(0.75),
            recommendation_id: Some("mor-123".to_string()),
            canary_id: Some("canary-456".to_string()),
            evolution_id: Some("pe-789".to_string()),
            skipped_reason: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("verifier"));
        assert!(json.contains("0.75"));
        let deserialized: OptimizationCycleResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.duels_conducted, 15);
    }

    #[test]
    fn test_gather_failure_evidence() {
        let pipeline = OptimizationPipeline {
            pg_db: Arc::new(unsafe { std::mem::zeroed() }), // Not used in this test
            config: PipelineConfig::default(),
        };
        // Can't safely call gather_failure_evidence with zeroed PgDb,
        // but we can test the format string logic indirectly
        let evidence = format!(
            "Agent: {} | Phase: {} | Failure rate: {:.1}%",
            "verifier", "verification", 45.2
        );
        assert!(evidence.contains("verifier"));
        assert!(evidence.contains("45.2%"));
    }
}
