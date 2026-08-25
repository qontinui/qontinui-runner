//! Prompt-optimization client for the `qontinui-prm` sidecar, with a held-out
//! validation gate.
//!
//! # Naming: this is **not** GEPA
//!
//! The types here are spelled `GEPA*` for historical reasons, and the name is
//! load-bearing on the persistence side — the `gepa_optimization_runs` table
//! (`database/pg/adaptive_learning.rs`) and two live Tauri commands
//! (`commands/adaptive_learning.rs`) read it. So the rename is deliberately
//! deferred rather than done here.
//!
//! What the sidecar actually runs is DSPy's
//! **`BootstrapFewShotWithRandomSearch`**: it bootstraps candidate few-shot
//! **demonstrations** and picks among them by random search. It is *not*
//! Greedy Evolutionary Prompt Adaptation, and — the practical consequence —
//! **it does not rewrite signature instructions**. Expect
//! [`GEPAResult::new_instructions`] to come back byte-identical to
//! [`GEPAResult::old_instructions`] on most runs, with the whole measured
//! difference coming from the selected demos. [`GEPAResult::instructions_changed`]
//! is the field that answers that question; the field name does not.
//!
//! # Flow
//!
//! 1. Training examples are extracted from historical learning outcomes
//!    ([`GepaIntegration::extract_training_examples_query`]).
//! 2. They are POSTed to the sidecar's `/gepa/optimize/domain`, which splits
//!    them train / val / **test**, optimizes on train, selects on val, and
//!    scores **both** arms on the untouched held-out `test` split.
//! 3. The sidecar returns **per-example** score vectors for the two arms —
//!    same order, same length — and this module runs the accept/reject
//!    decision locally through [`qontinui_runner_stats`].
//!
//! # The validation gate
//!
//! The decision is a **paired** one-sided test on per-example deltas
//! ([`qontinui_runner_stats::paired_analysis`] → [`qontinui_runner_stats::compute_verdict`] with
//! [`qontinui_runner_stats::VerdictThresholds::held_out_gate`]). Pairing is what makes a
//! small held-out set survivable: both arms are scored on the *same* examples,
//! so between-example variance drops out.
//!
//! Two properties are deliberate rather than incidental:
//!
//! * A position where **either** arm failed to evaluate (`null`) is
//!   **excluded**, never scored `0.0`. Imputing a zero fabricates an
//!   observation and reads as a real regression.
//! * "Could not decide" is a **distinct outcome** from "decided against" —
//!   see [`OptimizationOutcome`]. With zero paired examples the gate reports
//!   [`OptimizationOutcome::InsufficientData`], never a `0.0` improvement.
//!
//! `improvement`, `old_score` and `new_score` from the sidecar are **display
//! values only**. The decision is recomputed here from the paired vectors.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use qontinui_runner_stats::{self as stats, Verdict, VerdictThresholds};

// ============================================================================
// Domain Classification
// ============================================================================

/// Verification domain for domain-specific prompt optimization.
///
/// Each domain groups verification steps by the system layer they test,
/// enabling targeted prompt evolution per domain rather than one-size-fits-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDomain {
    Compilation,
    Api,
    Ui,
    Database,
    Security,
    General,
}

impl VerificationDomain {
    /// All known verification domains.
    pub fn all() -> &'static [VerificationDomain] {
        &[
            VerificationDomain::Compilation,
            VerificationDomain::Api,
            VerificationDomain::Ui,
            VerificationDomain::Database,
            VerificationDomain::Security,
            VerificationDomain::General,
        ]
    }

    /// String representation used in API calls and logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationDomain::Compilation => "compilation",
            VerificationDomain::Api => "api",
            VerificationDomain::Ui => "ui",
            VerificationDomain::Database => "database",
            VerificationDomain::Security => "security",
            VerificationDomain::General => "general",
        }
    }
}

impl std::fmt::Display for VerificationDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Knobs the sidecar accepts on an optimization request.
///
/// These are serialized **flat** onto the request body (see
/// [`DomainOptimizeRequest::config`]) because that is what the sidecar's
/// `OptimizeRequest` declares. Sending them under a nested `config` key — as
/// this client used to — is silently dropped by pydantic.
///
/// There is deliberately **no accept/reject threshold here**. The decision is a
/// paired statistical verdict computed in [`evaluate_held_out`]; a scalar
/// `min_improvement_threshold` on a small-sample mean is not a decision, and a
/// second copy of it in this struct would be a second decision surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GEPAConfig {
    /// Maximum optimization rounds per invocation.
    pub max_rounds: usize,
    /// Number of candidate programs to evaluate per round.
    pub num_candidates: usize,
    /// Cap on examples the sidecar draws per round, before the train/val/test
    /// split. The SQL extractor already returns up to 500 rows
    /// ([`GepaIntegration::extract_training_examples_query`]), so this is the
    /// lever that decides how much of that becomes statistical power.
    pub max_examples_per_round: usize,
}

impl Default for GEPAConfig {
    fn default() -> Self {
        Self {
            max_rounds: 5,
            num_candidates: 3,
            max_examples_per_round: 200,
        }
    }
}

// ============================================================================
// Result Types
// ============================================================================

/// Result of one optimization run, as returned by the sidecar.
///
/// The wire shape is **flat** — there is no `{success, result}` envelope. HTTP
/// status already carries success/failure and is checked before this is parsed,
/// so a second envelope would be a duplicate error channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GEPAResult {
    /// Instructions the run started from.
    pub old_instructions: String,
    /// Instructions the run ended with.
    ///
    /// **Usually identical to [`Self::old_instructions`].** The sidecar runs
    /// `BootstrapFewShotWithRandomSearch`, which selects few-shot *demos* and
    /// does not rewrite signature instructions (see the module header). Read
    /// [`Self::instructions_changed`] rather than assuming a rewrite happened.
    pub new_instructions: String,
    /// Whether [`Self::new_instructions`] actually differs from
    /// [`Self::old_instructions`]. Computed by the sidecar against its input.
    #[serde(default)]
    pub instructions_changed: bool,
    /// Mean held-out score of the original instructions (display only).
    pub old_score: f64,
    /// Mean held-out score of the optimized program (display only).
    pub new_score: f64,
    /// `new_score - old_score` (**display only**).
    ///
    /// Not used for the accept/reject decision — that is recomputed from the
    /// paired per-example vectors by [`evaluate_held_out`]. A mean-of-means
    /// delta discards the pairing that makes a small held-out set decidable.
    pub improvement: f64,
    /// Per-example scores for the **control** arm on the held-out set.
    ///
    /// Same order and same length as [`Self::new_scores`]; `None` means that
    /// example's evaluation failed. Index `i` in both vectors refers to the
    /// same held-out example.
    #[serde(default)]
    pub old_scores: Vec<Option<f64>>,
    /// Per-example scores for the **experimental** arm on the held-out set.
    /// See [`Self::old_scores`].
    #[serde(default)]
    pub new_scores: Vec<Option<f64>>,
    /// Size of the held-out (`test`) split the two arms were scored on.
    #[serde(default)]
    pub held_out_size: usize,
    /// Number of held-out examples the sidecar could not score (as reported by
    /// the sidecar; [`GateDecision::excluded`] recomputes it from the vectors).
    #[serde(default)]
    pub excluded_count: usize,
    /// Whether the sidecar fell back to cross-domain examples because too few
    /// matched the requested domain. When true, this was **not** a
    /// domain-specific optimization.
    #[serde(default)]
    pub domain_widened: bool,
    /// Few-shot examples selected during optimization.
    #[serde(default)]
    pub few_shot_examples: Vec<serde_json::Value>,
}

// ============================================================================
// HTTP API Types
// ============================================================================

/// Request body for the sidecar's `/gepa/optimize/domain` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainOptimizeRequest {
    pub domain: String,
    pub training_examples: Vec<serde_json::Value>,
    pub current_instructions: String,
    /// Base URL of the runner's evaluation API that the sidecar's metric calls
    /// back into. **Must be spelled `127.0.0.1`, not `localhost`** — on Windows
    /// `localhost` resolves to `::1` first and pays a doomed IPv6 connect
    /// before reaching the IPv4-only listener (see the workspace `CLAUDE.md`).
    pub evaluation_api_url: String,
    /// Flattened onto the request body, **not** nested under a `config` key —
    /// the sidecar declares these fields flat and silently discards a nested
    /// object.
    #[serde(flatten)]
    pub config: GEPAConfig,
}

// ============================================================================
// Validation gate
// ============================================================================

/// Scale factor from a `0.0..=1.0` quality score to **percentage points**.
///
/// [`qontinui_runner_stats::compute_verdict`]'s `sr_delta_pp` argument and every field of
/// [`VerdictThresholds`] are in percentage points — that is what they mean to
/// the five existing callers (`meta_optimizer/canary.rs`,
/// `meta_optimizer/eval_runner.rs`, `meta_optimizer/snapshots.rs` ×3,
/// `database/pg/canary.rs`), all of which pass a success-rate delta in pp.
///
/// The sidecar's scores are on a `0.0..=1.0` scale, so a raw mean delta is
/// **100× smaller** than the same effect expressed in pp. Passing it unscaled
/// would silently divide every threshold's meaning by 100. Scaling therefore
/// happens **here, at the call site**, before anything reaches `qontinui_runner_stats`:
/// each per-example delta is multiplied by this constant, so the p-value, the
/// confidence interval and the `sr_delta_pp` argument are all in one unit.
const SCORE_TO_PP: f64 = 100.0;

/// The gate's reading of one run's held-out evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct GateDecision {
    /// Statistical verdict from [`qontinui_runner_stats::compute_verdict`].
    pub verdict: Verdict,
    /// Number of held-out examples where **both** arms produced a score.
    pub paired: usize,
    /// Number of held-out positions dropped because at least one arm was
    /// `null`. Excluded, never imputed as `0.0`.
    pub excluded: usize,
    /// Mean paired delta (`new - old`) in **percentage points**.
    /// `0.0` when [`Self::paired`] is 0 — read the verdict, not this field.
    pub mean_delta_pp: f64,
    /// One-sided p-value for "the new program is better", when computable.
    pub p_value: Option<f64>,
    /// 95% CI of the mean paired delta, in percentage points.
    pub confidence_interval: Option<(f64, f64)>,
}

/// Run the held-out validation gate over a sidecar result.
///
/// Pairs the two arms **positionally**: index `i` of `old_scores` and
/// `new_scores` is the same held-out example. A position where either arm is
/// `None` is excluded from the comparison.
///
/// Returns [`Verdict::InsufficientData`] — never a `0.0` delta — when the two
/// vectors disagree in length (a contract violation, so nothing is pairable) or
/// when too few positions survive pairing.
pub fn evaluate_held_out(result: &GEPAResult) -> GateDecision {
    let thresholds = VerdictThresholds::held_out_gate();

    if result.old_scores.len() != result.new_scores.len() {
        warn!(
            old_len = result.old_scores.len(),
            new_len = result.new_scores.len(),
            "Held-out score vectors differ in length; nothing can be paired"
        );
        return GateDecision {
            verdict: Verdict::InsufficientData,
            paired: 0,
            excluded: result.old_scores.len().max(result.new_scores.len()),
            mean_delta_pp: 0.0,
            p_value: None,
            confidence_interval: None,
        };
    }

    let total = result.old_scores.len();
    let deltas_pp: Vec<f64> = result
        .old_scores
        .iter()
        .zip(result.new_scores.iter())
        .filter_map(|(old, new)| match (old, new) {
            (Some(o), Some(n)) => Some((n - o) * SCORE_TO_PP),
            // Unpaired: at least one arm failed to evaluate this example.
            // Excluded — scoring it 0.0 would fabricate a regression.
            _ => None,
        })
        .collect();

    let paired = deltas_pp.len();
    let excluded = total - paired;

    // Guard the all-excluded case explicitly rather than leaning on
    // `min_runs`: it is the *current* state of the world (the sidecar's metric
    // is wire-broken, so every example fails), and a mean over an empty vector
    // is NaN, not "no change".
    if paired == 0 {
        return GateDecision {
            verdict: Verdict::InsufficientData,
            paired: 0,
            excluded,
            mean_delta_pp: 0.0,
            p_value: None,
            confidence_interval: None,
        };
    }

    let mean_delta_pp = deltas_pp.iter().sum::<f64>() / paired as f64;
    let analysis = stats::paired_analysis(&deltas_pp, thresholds.min_runs as usize);
    let verdict = stats::compute_verdict(mean_delta_pp, &analysis, paired as u64, &thresholds);

    GateDecision {
        verdict,
        paired,
        excluded,
        mean_delta_pp,
        p_value: analysis.p_value,
        confidence_interval: analysis.confidence_interval,
    }
}

/// Outcome of [`GepaIntegration::optimize_domain`].
///
/// "Could not decide" is a **first-class** variant, distinct from "decided
/// against": a run that never reached a decidable comparison must not be
/// recorded as a run that rejected the new prompt. [`Self::status_str`] maps
/// each arm onto the `status` column
/// `database/pg/adaptive_learning.rs::insert_gepa_run` already takes, so the
/// distinction lands with no schema change.
#[derive(Debug, Clone)]
pub enum OptimizationOutcome {
    /// The gate never ran — disabled, cooldown not elapsed, or too few
    /// training examples to bother the sidecar with.
    Skipped { reason: &'static str },
    /// The held-out paired comparison **accepted** the optimized program.
    Accepted {
        result: Box<GEPAResult>,
        decision: GateDecision,
    },
    /// The held-out paired comparison **decided against** the optimized
    /// program (a regression, or no significant gain).
    Rejected {
        result: Box<GEPAResult>,
        decision: GateDecision,
    },
    /// The comparison could not be made — too few paired held-out examples.
    /// **Nothing was decided.**
    InsufficientData {
        result: Box<GEPAResult>,
        decision: GateDecision,
    },
}

impl OptimizationOutcome {
    /// Value for `insert_gepa_run`'s `status` column.
    pub fn status_str(&self) -> &'static str {
        match self {
            OptimizationOutcome::Skipped { .. } => "skipped",
            OptimizationOutcome::Accepted { .. } => "accepted",
            OptimizationOutcome::Rejected { .. } => "rejected",
            OptimizationOutcome::InsufficientData { .. } => "insufficient_data",
        }
    }

    /// The optimized result, only when the gate actually accepted it.
    ///
    /// Every other arm yields `None` — including `InsufficientData`, which is
    /// why callers must consult [`Self::status_str`] rather than treating a
    /// `None` here as a rejection.
    pub fn accepted_result(&self) -> Option<&GEPAResult> {
        match self {
            OptimizationOutcome::Accepted { result, .. } => Some(result),
            _ => None,
        }
    }
}

// ============================================================================
// Integration
// ============================================================================

/// Client for the prompt-optimization Python sidecar.
///
/// Manages connection state, cooldown tracking, and HTTP communication with the
/// optimization service.
pub struct GepaIntegration {
    /// Base URL of the sidecar service (e.g. "http://127.0.0.1:8200").
    gepa_url: String,
    /// Whether optimization is enabled.
    enabled: bool,
    /// Minimum duration between optimization runs.
    optimization_cooldown: std::time::Duration,
    /// Minimum number of training examples before an optimization is worth
    /// requesting — see [`Self::MIN_RUNS_BETWEEN_OPTIMIZATIONS`].
    min_runs_between_optimizations: usize,
    /// Timestamp of the last optimization run.
    last_optimization: Option<std::time::Instant>,
    /// HTTP client for communicating with the sidecar.
    client: reqwest::Client,
}

impl GepaIntegration {
    /// Minimum training examples before an optimization request is sent.
    ///
    /// Raised from 10, which could not work: the sidecar splits its examples
    /// 80 / 10 / 10 into train / val / **test**, so 10 examples yielded a
    /// *one*-example held-out set — below the `n >= 2` a paired t-test needs to
    /// estimate any variance at all, so the gate could only ever have returned
    /// `InsufficientData`.
    ///
    /// 100 is the smallest floor that makes the held-out split viable at the
    /// design point: 100 → 80 train / 10 val / **10 test**, matching
    /// [`VerdictThresholds::held_out_gate`]'s `min_runs` of 10, which is the
    /// paired-n the plan's power argument is written against. Sending a
    /// request the sidecar would refuse — or that could not produce a decidable
    /// comparison — is wasted work, so the floor is enforced before the call.
    ///
    /// Note this makes `InsufficientData` the *common* path until
    /// `learning_outcomes` volume grows. That is the intended behaviour: the
    /// gate declines to decide rather than deciding on noise.
    pub const MIN_RUNS_BETWEEN_OPTIMIZATIONS: usize = 100;

    /// Create a new integration pointed at the given service URL.
    pub fn new(gepa_url: &str) -> Self {
        Self {
            gepa_url: gepa_url.trim_end_matches('/').to_string(),
            enabled: true,
            optimization_cooldown: std::time::Duration::from_secs(30 * 60), // 30 minutes
            min_runs_between_optimizations: Self::MIN_RUNS_BETWEEN_OPTIMIZATIONS,
            last_optimization: None,
            client: reqwest::Client::new(),
        }
    }

    /// Run domain-specific prompt optimization, then gate the result on a
    /// paired held-out comparison.
    ///
    /// `evaluation_api_url` is the runner's own API, which the sidecar's metric
    /// calls back into. **Spell it `http://127.0.0.1:9876`, not `localhost`** —
    /// see [`DomainOptimizeRequest::evaluation_api_url`].
    ///
    /// Returns [`OptimizationOutcome`], which keeps "not run", "accepted",
    /// "rejected" and "could not decide" as four distinct answers. `Err` is
    /// reserved for communication and parse failures.
    pub async fn optimize_domain(
        &mut self,
        domain: VerificationDomain,
        training_examples: Vec<serde_json::Value>,
        current_instructions: &str,
        evaluation_api_url: &str,
    ) -> Result<OptimizationOutcome, String> {
        if !self.enabled {
            debug!("Prompt optimization disabled, skipping");
            return Ok(OptimizationOutcome::Skipped { reason: "disabled" });
        }

        if !self.cooldown_elapsed() {
            debug!("Prompt optimization cooldown not yet elapsed, skipping");
            return Ok(OptimizationOutcome::Skipped { reason: "cooldown" });
        }

        if training_examples.len() < self.min_runs_between_optimizations {
            debug!(
                count = training_examples.len(),
                min = self.min_runs_between_optimizations,
                "Not enough training examples for a viable held-out split"
            );
            return Ok(OptimizationOutcome::Skipped {
                reason: "insufficient_training_examples",
            });
        }

        if evaluation_api_url.contains("localhost") {
            warn!(
                url = %evaluation_api_url,
                "evaluation_api_url uses 'localhost'; on Windows this pays a doomed IPv6 \
                 connect before the IPv4 listener answers — use 127.0.0.1"
            );
        }

        let request = DomainOptimizeRequest {
            domain: domain.as_str().to_string(),
            training_examples,
            current_instructions: current_instructions.to_string(),
            evaluation_api_url: evaluation_api_url.to_string(),
            config: GEPAConfig::default(),
        };

        let url = format!("{}/gepa/optimize/domain", self.gepa_url);
        info!(domain = %domain, url = %url, "Sending domain optimization request");

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Optimizer HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            warn!(status = %status, body = %body, "Optimization request failed");
            return Err(format!("Optimizer returned {status}: {body}"));
        }

        let result: GEPAResult = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse optimizer response: {e}"))?;

        // Update cooldown timestamp — the round ran, whatever the gate decides.
        self.last_optimization = Some(std::time::Instant::now());

        if result.domain_widened {
            warn!(
                domain = %domain,
                "Sidecar widened past the requested domain; this was NOT a \
                 domain-specific optimization"
            );
        }

        let decision = evaluate_held_out(&result);

        info!(
            domain = %domain,
            verdict = ?decision.verdict,
            paired = decision.paired,
            excluded = decision.excluded,
            mean_delta_pp = decision.mean_delta_pp,
            p_value = ?decision.p_value,
            instructions_changed = result.instructions_changed,
            "Held-out validation gate evaluated"
        );

        let result = Box::new(result);
        Ok(match decision.verdict {
            Verdict::Positive => OptimizationOutcome::Accepted { result, decision },
            Verdict::Negative | Verdict::Neutral => {
                OptimizationOutcome::Rejected { result, decision }
            }
            Verdict::InsufficientData => OptimizationOutcome::InsufficientData { result, decision },
        })
    }

    /// Check whether enough time has passed since the last optimization.
    pub fn cooldown_elapsed(&self) -> bool {
        match self.last_optimization {
            None => true,
            Some(last) => last.elapsed() >= self.optimization_cooldown,
        }
    }

    /// SQL query to extract training examples from learning outcomes and task runs.
    ///
    /// Returns rows with: domain, instructions, outcome_json, score, created_at.
    /// These feed into the training pipeline as (input, output, label) triples.
    pub fn extract_training_examples_query() -> &'static str {
        r#"
        SELECT
            lo.category AS domain,
            tr.prompt AS instructions,
            lo.outcome AS outcome_json,
            COALESCE(lo.confidence, 0.5) AS score,
            lo.created_at
        FROM learning_outcomes lo
        JOIN task_runs tr ON tr.id = lo.task_run_id
        WHERE lo.outcome IS NOT NULL
          AND tr.prompt IS NOT NULL
          AND lo.created_at > datetime('now', '-30 days')
        ORDER BY lo.created_at DESC
        LIMIT 500
        "#
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a result with the given per-example vectors; everything else is
    /// filler so the gate tests read as gate tests.
    fn result_with(old: Vec<Option<f64>>, new: Vec<Option<f64>>) -> GEPAResult {
        let held_out_size = old.len();
        GEPAResult {
            old_instructions: "old".to_string(),
            new_instructions: "old".to_string(),
            instructions_changed: false,
            old_score: 0.0,
            new_score: 0.0,
            improvement: 0.0,
            old_scores: old,
            new_scores: new,
            held_out_size,
            excluded_count: 0,
            domain_widened: false,
            few_shot_examples: vec![],
        }
    }

    #[test]
    fn test_verification_domain_all() {
        let all = VerificationDomain::all();
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn test_verification_domain_as_str() {
        assert_eq!(VerificationDomain::Compilation.as_str(), "compilation");
        assert_eq!(VerificationDomain::Api.as_str(), "api");
        assert_eq!(VerificationDomain::Ui.as_str(), "ui");
        assert_eq!(VerificationDomain::Database.as_str(), "database");
        assert_eq!(VerificationDomain::Security.as_str(), "security");
        assert_eq!(VerificationDomain::General.as_str(), "general");
    }

    #[test]
    fn test_gepa_config_defaults() {
        let config = GEPAConfig::default();
        assert_eq!(config.max_rounds, 5);
        assert_eq!(config.num_candidates, 3);
        assert_eq!(config.max_examples_per_round, 200);
    }

    #[test]
    fn test_gepa_integration_new() {
        let integration = GepaIntegration::new("http://127.0.0.1:8200/");
        assert_eq!(integration.gepa_url, "http://127.0.0.1:8200");
        assert!(integration.enabled);
        assert!(integration.cooldown_elapsed());
        assert_eq!(integration.min_runs_between_optimizations, 100);
    }

    #[test]
    fn test_extract_training_examples_query() {
        let query = GepaIntegration::extract_training_examples_query();
        assert!(query.contains("learning_outcomes"));
        assert!(query.contains("task_runs"));
    }

    #[test]
    fn test_domain_serde_roundtrip() {
        let domain = VerificationDomain::Security;
        let json = serde_json::to_string(&domain).unwrap();
        assert_eq!(json, "\"security\"");
        let back: VerificationDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(back, domain);
    }

    // ------------------------------------------------------------------
    // Wire contract
    // ------------------------------------------------------------------

    /// The request must serialize FLAT — no nested `config` object, which the
    /// sidecar silently discards.
    #[test]
    fn test_request_serializes_flat_not_nested() {
        let request = DomainOptimizeRequest {
            domain: "compilation".to_string(),
            training_examples: vec![serde_json::json!({})],
            current_instructions: "instr".to_string(),
            evaluation_api_url: "http://127.0.0.1:9876".to_string(),
            config: GEPAConfig::default(),
        };
        let value = serde_json::to_value(&request).unwrap();
        let obj = value.as_object().unwrap();

        assert!(
            !obj.contains_key("config"),
            "config must be flattened, got {value}"
        );
        assert_eq!(obj["domain"], "compilation");
        assert_eq!(obj["current_instructions"], "instr");
        assert_eq!(obj["evaluation_api_url"], "http://127.0.0.1:9876");
        assert_eq!(obj["max_rounds"], 5);
        assert_eq!(obj["num_candidates"], 3);
        assert_eq!(obj["max_examples_per_round"], 200);
    }

    /// The response must deserialize from the sidecar's FLAT body — no
    /// `{success, result}` envelope, and every field present.
    #[test]
    fn test_response_deserializes_flat_body() {
        let body = r#"{
            "old_instructions": "before",
            "new_instructions": "before",
            "instructions_changed": false,
            "old_score": 0.7,
            "new_score": 0.75,
            "improvement": 0.05,
            "old_scores": [0.7, null, 0.8],
            "new_scores": [0.75, 0.6, null],
            "held_out_size": 3,
            "excluded_count": 2,
            "domain_widened": false,
            "few_shot_examples": []
        }"#;
        let result: GEPAResult = serde_json::from_str(body).unwrap();

        assert_eq!(result.old_instructions, "before");
        assert!(!result.instructions_changed);
        assert_eq!(result.old_scores, vec![Some(0.7), None, Some(0.8)]);
        assert_eq!(result.new_scores, vec![Some(0.75), Some(0.6), None]);
        assert_eq!(result.held_out_size, 3);
        assert_eq!(result.excluded_count, 2);
        assert!(!result.domain_widened);
    }

    /// A body missing the optional additions still parses — the required core
    /// is the four instruction/score fields, not an envelope.
    #[test]
    fn test_response_tolerates_missing_optional_fields() {
        let body = r#"{
            "old_instructions": "a",
            "new_instructions": "b",
            "old_score": 0.1,
            "new_score": 0.2,
            "improvement": 0.1
        }"#;
        let result: GEPAResult = serde_json::from_str(body).unwrap();
        assert!(result.old_scores.is_empty());
        assert!(result.new_scores.is_empty());
        assert_eq!(result.held_out_size, 0);
    }

    // ------------------------------------------------------------------
    // Held-out gate
    // ------------------------------------------------------------------

    /// The current state of the world: the sidecar's metric is wire-broken, so
    /// every example fails to evaluate. That must be `InsufficientData` — NOT a
    /// 0.0 delta that reads as "no change".
    #[test]
    fn test_all_excluded_is_insufficient_data_not_zero_improvement() {
        let result = result_with(vec![None; 12], vec![None; 12]);
        let decision = evaluate_held_out(&result);

        assert_eq!(decision.verdict, Verdict::InsufficientData);
        assert_eq!(decision.paired, 0);
        assert_eq!(decision.excluded, 12);
        assert!(decision.p_value.is_none());
        assert!(decision.mean_delta_pp.is_finite());
    }

    /// An empty held-out set is likewise undecidable, not neutral.
    #[test]
    fn test_empty_vectors_are_insufficient_data() {
        let decision = evaluate_held_out(&result_with(vec![], vec![]));
        assert_eq!(decision.verdict, Verdict::InsufficientData);
        assert_eq!(decision.paired, 0);
    }

    /// A position where EITHER arm is null is excluded, never scored 0.
    ///
    /// Every paired position here improves by +0.10 (10 pp). If the nulls were
    /// imputed as 0.0 the two of them would contribute -0.8 and +0.6, dragging
    /// the mean negative; excluding them leaves a clean +10 pp.
    #[test]
    fn test_null_positions_are_excluded_not_scored_zero() {
        let mut old: Vec<Option<f64>> = vec![Some(0.5); 12];
        let mut new: Vec<Option<f64>> = vec![Some(0.6); 12];
        old[3] = None; // new[3] = Some(0.6) — dropped, not scored as +0.6
        new[7] = None; // old[7] = Some(0.5) — dropped, not scored as -0.5

        let decision = evaluate_held_out(&result_with(old, new));

        assert_eq!(decision.paired, 10);
        assert_eq!(decision.excluded, 2);
        assert!(
            (decision.mean_delta_pp - 10.0).abs() < 1e-9,
            "mean delta must be +10 pp, got {}",
            decision.mean_delta_pp
        );
    }

    /// Deltas are scaled to percentage points before reaching `qontinui_runner_stats`.
    /// A raw 0..1 delta would be 100x smaller and read as ~0.1 pp.
    #[test]
    fn test_deltas_are_scaled_to_percentage_points() {
        let old = vec![Some(0.50); 10];
        let new = vec![Some(0.60); 10];
        let decision = evaluate_held_out(&result_with(old, new));

        assert!(
            (decision.mean_delta_pp - 10.0).abs() < 1e-9,
            "expected 10 pp for a 0.10 score delta, got {}",
            decision.mean_delta_pp
        );
        let (lo, hi) = decision.confidence_interval.expect("ci");
        assert!((lo - 10.0).abs() < 1e-9 && (hi - 10.0).abs() < 1e-9);
    }

    /// Below the held-out floor of 10 paired examples the gate declines to
    /// decide, even on a large apparent gain.
    #[test]
    fn test_below_held_out_floor_is_insufficient_data() {
        let old = vec![Some(0.1); 9];
        let new = vec![Some(0.9); 9];
        let decision = evaluate_held_out(&result_with(old, new));

        assert_eq!(decision.paired, 9);
        assert_eq!(decision.verdict, Verdict::InsufficientData);
    }

    /// Length mismatch is a contract violation — nothing is pairable, so the
    /// gate reports undecidable rather than zipping to the shorter vector and
    /// mis-pairing examples.
    #[test]
    fn test_length_mismatch_is_insufficient_data() {
        let decision = evaluate_held_out(&result_with(vec![Some(0.1); 10], vec![Some(0.9); 8]));
        assert_eq!(decision.verdict, Verdict::InsufficientData);
        assert_eq!(decision.paired, 0);
    }

    /// A consistent, significant per-example gain is accepted.
    #[test]
    fn test_consistent_improvement_is_positive() {
        let old: Vec<Option<f64>> = (0..12).map(|i| Some(0.40 + i as f64 * 0.01)).collect();
        let new: Vec<Option<f64>> = (0..12).map(|i| Some(0.50 + i as f64 * 0.01)).collect();
        let decision = evaluate_held_out(&result_with(old, new));

        assert_eq!(decision.paired, 12);
        assert_eq!(decision.verdict, Verdict::Positive);
        assert!((decision.mean_delta_pp - 10.0).abs() < 1e-9);
    }

    /// A consistent per-example loss is a regression, not "neutral" — the arm
    /// a two-sided p-value would have lost.
    #[test]
    fn test_consistent_regression_is_negative() {
        let old: Vec<Option<f64>> = (0..12).map(|i| Some(0.50 + i as f64 * 0.01)).collect();
        let new: Vec<Option<f64>> = (0..12).map(|i| Some(0.40 + i as f64 * 0.01)).collect();
        let decision = evaluate_held_out(&result_with(old, new));

        assert_eq!(decision.verdict, Verdict::Negative);
        assert!((decision.mean_delta_pp - -10.0).abs() < 1e-9);
    }

    /// A tiny, noisy delta decides nothing either way: half the examples move
    /// +5 pp and half move -4 pp, for a mean of +0.5 pp swamped by the spread.
    #[test]
    fn test_marginal_delta_is_neutral() {
        let old: Vec<Option<f64>> = vec![Some(0.50); 12];
        let new: Vec<Option<f64>> = (0..12)
            .map(|i| Some(if i % 2 == 0 { 0.55 } else { 0.46 }))
            .collect();
        let decision = evaluate_held_out(&result_with(old, new));

        assert_eq!(decision.paired, 12);
        assert!(
            (decision.mean_delta_pp - 0.5).abs() < 1e-9,
            "mean = {}",
            decision.mean_delta_pp
        );
        assert_eq!(decision.verdict, Verdict::Neutral);
    }

    // ------------------------------------------------------------------
    // Outcome mapping
    // ------------------------------------------------------------------

    /// "Could not decide" must never be recorded as "decided against".
    #[test]
    fn test_outcome_status_strings_are_distinct() {
        let filler = || Box::new(result_with(vec![], vec![]));
        let decision = evaluate_held_out(&result_with(vec![], vec![]));

        let skipped = OptimizationOutcome::Skipped { reason: "disabled" };
        let accepted = OptimizationOutcome::Accepted {
            result: filler(),
            decision: decision.clone(),
        };
        let rejected = OptimizationOutcome::Rejected {
            result: filler(),
            decision: decision.clone(),
        };
        let undecided = OptimizationOutcome::InsufficientData {
            result: filler(),
            decision,
        };

        assert_eq!(skipped.status_str(), "skipped");
        assert_eq!(accepted.status_str(), "accepted");
        assert_eq!(rejected.status_str(), "rejected");
        assert_eq!(undecided.status_str(), "insufficient_data");
        assert_ne!(undecided.status_str(), rejected.status_str());

        assert!(accepted.accepted_result().is_some());
        assert!(rejected.accepted_result().is_none());
        assert!(undecided.accepted_result().is_none());
        assert!(skipped.accepted_result().is_none());
    }
}
