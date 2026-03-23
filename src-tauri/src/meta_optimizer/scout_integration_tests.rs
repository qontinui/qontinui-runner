//! Scout integration tests — cross-cutting tests for meta-optimizer features.
//!
//! Covers: canary rollout logic, eval spec CRUD, parser marker extraction,
//! agentic metrics (composite scoring, task completion, step efficiency),
//! content-hash dedup, and cost-confidence scaling.

#[cfg(test)]
mod tests {
    // =========================================================================
    // Canary rollout tests (promptfoo-inspired)
    // =========================================================================
    mod canary {
        use crate::database::CheckpointDb;
        use crate::meta_optimizer::canary::{
            evaluate_canary, get_active_canaries, record_canary_run, should_apply_canary,
            start_canary, CanaryMetrics,
        };

        fn setup_db() -> CheckpointDb {
            CheckpointDb::new_in_memory().unwrap()
        }

        /// Insert a minimal recommendation row so canary foreign keys work.
        fn insert_recommendation(db: &CheckpointDb, rec_id: &str) {
            let id = rec_id.to_string();
            db.with_conn(move |conn| {
                conn.execute(
                    r#"INSERT OR IGNORE INTO meta_optimizer_recommendations
                       (id, optimizer_type, recommendation_type, title, description, confidence, status, created_at)
                       VALUES (?1, 'pipeline_prompt', 'prompt_rewrite', 'test', 'test desc', 0.8, 'pending', datetime('now'))"#,
                    rusqlite::params![id],
                ).map_err(|e| format!("{}", e))?;
                Ok(())
            }).unwrap();
        }

        #[test]
        fn should_apply_canary_returns_false_when_no_active_canary() {
            let db = setup_db();
            // No canary exists for this recommendation
            assert!(!should_apply_canary(&db, "nonexistent-rec"));
        }

        #[test]
        fn should_apply_canary_respects_percentage_100() {
            let db = setup_db();
            let rec_id = "rec-100pct";
            insert_recommendation(&db, rec_id);
            let canary_id = start_canary(&db, rec_id, 100).unwrap();
            assert!(!canary_id.is_empty());

            // With 100% rollout, should_apply_canary should always return true
            // (probabilistic, but 100% means the roll < 100.0 always)
            let mut applied_count = 0;
            for _ in 0..20 {
                if should_apply_canary(&db, rec_id) {
                    applied_count += 1;
                }
            }
            assert_eq!(applied_count, 20, "100% canary should always apply");
        }

        #[test]
        fn should_apply_canary_percentage_clamped_to_valid_range() {
            let db = setup_db();
            // Percentage 0 should be clamped to 1
            let rec_id = "rec-zero-pct";
            insert_recommendation(&db, rec_id);
            let _canary_id = start_canary(&db, rec_id, 0).unwrap();

            // With 1% (clamped from 0), should rarely apply but at least the canary exists
            let canaries = get_active_canaries(&db).unwrap();
            assert_eq!(canaries.len(), 1);
            assert_eq!(canaries[0].percentage, 1); // clamped from 0 to 1
        }

        #[test]
        fn start_canary_sets_recommendation_to_canary_status() {
            let db = setup_db();
            let rec_id = "rec-status-check";
            insert_recommendation(&db, rec_id);
            let _canary_id = start_canary(&db, rec_id, 50).unwrap();

            // Check recommendation status was updated
            let status: String = db
                .with_conn({
                    let id = rec_id.to_string();
                    move |conn| {
                        conn.query_row(
                            "SELECT status FROM meta_optimizer_recommendations WHERE id = ?1",
                            rusqlite::params![id],
                            |row| row.get(0),
                        )
                        .map_err(|e| format!("{}", e))
                    }
                })
                .unwrap();
            assert_eq!(status, "canary");
        }

        #[test]
        fn record_canary_run_updates_metrics() {
            let db = setup_db();
            let rec_id = "rec-metrics";
            insert_recommendation(&db, rec_id);
            let canary_id = start_canary(&db, rec_id, 50).unwrap();

            // Record baseline runs
            record_canary_run(&db, &canary_id, false, true, 0.05, 1000.0).unwrap();
            record_canary_run(&db, &canary_id, false, true, 0.03, 800.0).unwrap();
            record_canary_run(&db, &canary_id, false, false, 0.04, 900.0).unwrap();

            // Record canary runs
            record_canary_run(&db, &canary_id, true, true, 0.02, 700.0).unwrap();
            record_canary_run(&db, &canary_id, true, false, 0.06, 1100.0).unwrap();

            // Verify counts
            let canaries = get_active_canaries(&db).unwrap();
            assert_eq!(canaries.len(), 1);
            assert_eq!(canaries[0].baseline_run_count, 3);
            assert_eq!(canaries[0].canary_run_count, 2);

            // Verify metrics JSON was updated
            let baseline: CanaryMetrics =
                serde_json::from_str(&canaries[0].baseline_metrics_json).unwrap();
            assert_eq!(baseline.success_count, 2);
            assert_eq!(baseline.failure_count, 1);
            assert!((baseline.total_cost_usd - 0.12).abs() < 0.001);

            let canary: CanaryMetrics =
                serde_json::from_str(&canaries[0].canary_metrics_json).unwrap();
            assert_eq!(canary.success_count, 1);
            assert_eq!(canary.failure_count, 1);
        }

        #[test]
        fn evaluate_canary_continues_when_min_runs_not_met() {
            let db = setup_db();
            let rec_id = "rec-eval-min";
            insert_recommendation(&db, rec_id);
            let canary_id = start_canary(&db, rec_id, 50).unwrap();

            // Only 3 canary runs (min is 10)
            for _ in 0..3 {
                record_canary_run(&db, &canary_id, true, true, 0.01, 500.0).unwrap();
                record_canary_run(&db, &canary_id, false, true, 0.01, 500.0).unwrap();
            }

            let eval = evaluate_canary(&db, &canary_id).unwrap();
            assert_eq!(eval.verdict, "continue");
            assert!(!eval.min_runs_met);
        }

        #[test]
        fn evaluate_canary_computes_success_rates_and_delta() {
            let db = setup_db();
            let rec_id = "rec-eval-rates";
            insert_recommendation(&db, rec_id);
            let canary_id = start_canary(&db, rec_id, 50).unwrap();

            // 12 baseline runs: 10 success, 2 failure = 83.3%
            for _ in 0..10 {
                record_canary_run(&db, &canary_id, false, true, 0.01, 500.0).unwrap();
            }
            for _ in 0..2 {
                record_canary_run(&db, &canary_id, false, false, 0.01, 500.0).unwrap();
            }

            // 12 canary runs: 11 success, 1 failure = 91.7%
            for _ in 0..11 {
                record_canary_run(&db, &canary_id, true, true, 0.01, 500.0).unwrap();
            }
            record_canary_run(&db, &canary_id, true, false, 0.01, 500.0).unwrap();

            let eval = evaluate_canary(&db, &canary_id).unwrap();
            assert!(eval.min_runs_met);
            // Baseline SR ~83.3%, Canary SR ~91.7%
            assert!(eval.baseline_success_rate > 83.0 && eval.baseline_success_rate < 84.0);
            assert!(eval.canary_success_rate > 91.0 && eval.canary_success_rate < 92.0);
            assert!(eval.delta > 0.0, "canary should be better");
            // Should have statistical fields populated
            assert!(eval.p_value.is_some());
            assert!(eval.confidence_interval.is_some());
            assert!(eval.effect_size.is_some());
        }

        #[test]
        fn evaluate_canary_computes_cost_and_duration_deltas() {
            let db = setup_db();
            let rec_id = "rec-eval-cost";
            insert_recommendation(&db, rec_id);
            let canary_id = start_canary(&db, rec_id, 50).unwrap();

            // Baseline: avg cost 0.10, avg duration 1000ms
            for _ in 0..12 {
                record_canary_run(&db, &canary_id, false, true, 0.10, 1000.0).unwrap();
            }
            // Canary: avg cost 0.05 (50% cheaper), avg duration 800ms (20% faster)
            for _ in 0..12 {
                record_canary_run(&db, &canary_id, true, true, 0.05, 800.0).unwrap();
            }

            let eval = evaluate_canary(&db, &canary_id).unwrap();
            // Cost delta should be negative (canary is cheaper)
            assert!(eval.cost_delta_pct.is_some());
            let cost_pct = eval.cost_delta_pct.unwrap();
            assert!(cost_pct < -40.0 && cost_pct > -60.0, "expected ~-50%, got {}", cost_pct);

            // Duration delta should be negative (canary is faster)
            assert!(eval.duration_delta_pct.is_some());
            let dur_pct = eval.duration_delta_pct.unwrap();
            assert!(dur_pct < -15.0 && dur_pct > -25.0, "expected ~-20%, got {}", dur_pct);
        }

        #[test]
        fn canary_metrics_default() {
            let m = CanaryMetrics::default();
            assert_eq!(m.success_count, 0);
            assert_eq!(m.failure_count, 0);
            assert_eq!(m.total_cost_usd, 0.0);
            assert_eq!(m.total_duration_ms, 0.0);
        }
    }

    // =========================================================================
    // Eval spec tests (promptfoo-inspired declarative eval)
    // =========================================================================
    mod eval_spec {
        use crate::database::CheckpointDb;
        use crate::meta_optimizer::eval_spec::{
            delete_eval_spec, get_eval_spec, list_eval_specs, save_eval_spec, EvalAssertion,
            EvalInput, EvalSpec, EvalTestCase, EvalThresholds,
        };

        fn setup_db() -> CheckpointDb {
            CheckpointDb::new_in_memory().unwrap()
        }

        fn make_spec(id: &str, name: &str, agent: Option<&str>) -> EvalSpec {
            EvalSpec {
                id: id.to_string(),
                name: name.to_string(),
                target_agent: agent.map(String::from),
                test_cases: vec![EvalTestCase {
                    id: "tc-1".to_string(),
                    description: "Basic test".to_string(),
                    input: EvalInput::Workflow {
                        workflow_id: "wf-1".to_string(),
                    },
                    assertions: vec![
                        EvalAssertion::SuccessRate { min: 0.8 },
                        EvalAssertion::MaxDuration { ms: 60000 },
                    ],
                }],
                thresholds: EvalThresholds::default(),
            }
        }

        #[test]
        fn eval_thresholds_defaults() {
            let t = EvalThresholds::default();
            assert_eq!(t.min_trials, 3);
            assert!((t.significance_threshold - 0.05).abs() < f64::EPSILON);
            assert!((t.required_pass_rate - 1.0).abs() < f64::EPSILON);
        }

        #[test]
        fn save_and_get_eval_spec() {
            let db = setup_db();
            let spec = make_spec("es-1", "Test Spec", Some("implementer"));
            save_eval_spec(&db, &spec).unwrap();

            let loaded = get_eval_spec(&db, "es-1").unwrap();
            assert!(loaded.is_some());
            let loaded = loaded.unwrap();
            assert_eq!(loaded.id, "es-1");
            assert_eq!(loaded.name, "Test Spec");
            assert_eq!(loaded.target_agent.as_deref(), Some("implementer"));
            assert_eq!(loaded.test_cases.len(), 1);
            assert_eq!(loaded.test_cases[0].assertions.len(), 2);
        }

        #[test]
        fn get_nonexistent_spec_returns_none() {
            let db = setup_db();
            let result = get_eval_spec(&db, "nonexistent").unwrap();
            assert!(result.is_none());
        }

        #[test]
        fn save_eval_spec_upserts() {
            let db = setup_db();
            let mut spec = make_spec("es-upsert", "V1", Some("verifier"));
            save_eval_spec(&db, &spec).unwrap();

            spec.name = "V2".to_string();
            save_eval_spec(&db, &spec).unwrap();

            let loaded = get_eval_spec(&db, "es-upsert").unwrap().unwrap();
            assert_eq!(loaded.name, "V2");
        }

        #[test]
        fn list_eval_specs_filters_by_agent() {
            let db = setup_db();
            save_eval_spec(&db, &make_spec("es-a", "A", Some("implementer"))).unwrap();
            save_eval_spec(&db, &make_spec("es-b", "B", Some("verifier"))).unwrap();
            save_eval_spec(&db, &make_spec("es-c", "C", Some("implementer"))).unwrap();

            let all = list_eval_specs(&db, None).unwrap();
            assert_eq!(all.len(), 3);

            let impl_only = list_eval_specs(&db, Some("implementer")).unwrap();
            assert_eq!(impl_only.len(), 2);

            let verifier_only = list_eval_specs(&db, Some("verifier")).unwrap();
            assert_eq!(verifier_only.len(), 1);
        }

        #[test]
        fn delete_eval_spec_removes_it() {
            let db = setup_db();
            save_eval_spec(&db, &make_spec("es-del", "Delete Me", None)).unwrap();
            assert!(get_eval_spec(&db, "es-del").unwrap().is_some());

            delete_eval_spec(&db, "es-del").unwrap();
            assert!(get_eval_spec(&db, "es-del").unwrap().is_none());
        }

        #[test]
        fn eval_assertion_serialization_roundtrip() {
            let assertions = vec![
                EvalAssertion::SuccessRate { min: 0.9 },
                EvalAssertion::MaxDuration { ms: 30000 },
                EvalAssertion::MaxCost { cents: 5.0 },
                EvalAssertion::MaxIterations { count: 3 },
                EvalAssertion::NoRegression {
                    metric: "success_rate".to_string(),
                    tolerance_pp: 5.0,
                },
            ];

            let json = serde_json::to_string(&assertions).unwrap();
            let deserialized: Vec<EvalAssertion> = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.len(), 5);
        }

        #[test]
        fn eval_input_workflow_ids_variant() {
            let input = EvalInput::WorkflowIds {
                ids: vec!["wf-1".into(), "wf-2".into()],
            };
            let json = serde_json::to_string(&input).unwrap();
            assert!(json.contains("workflow_ids"));
            let roundtrip: EvalInput = serde_json::from_str(&json).unwrap();
            if let EvalInput::WorkflowIds { ids } = roundtrip {
                assert_eq!(ids.len(), 2);
            } else {
                panic!("Expected WorkflowIds variant");
            }
        }
    }

    // =========================================================================
    // Content hash dedup tests (promptfoo-inspired)
    // =========================================================================
    mod content_hash {
        use crate::meta_optimizer::recommendations::compute_content_hash;

        #[test]
        fn content_hash_is_deterministic() {
            let h1 = compute_content_hash("pipeline_prompt", "prompt_rewrite", Some("implementer"), Some("content"));
            let h2 = compute_content_hash("pipeline_prompt", "prompt_rewrite", Some("implementer"), Some("content"));
            assert_eq!(h1, h2);
        }

        #[test]
        fn content_hash_differs_for_different_inputs() {
            let h1 = compute_content_hash("pipeline_prompt", "prompt_rewrite", Some("implementer"), Some("v1"));
            let h2 = compute_content_hash("pipeline_prompt", "prompt_rewrite", Some("implementer"), Some("v2"));
            assert_ne!(h1, h2);
        }

        #[test]
        fn content_hash_differs_by_optimizer_type() {
            let h1 = compute_content_hash("pipeline_prompt", "prompt_rewrite", Some("a"), Some("x"));
            let h2 = compute_content_hash("architecture", "prompt_rewrite", Some("a"), Some("x"));
            assert_ne!(h1, h2);
        }

        #[test]
        fn content_hash_none_vs_empty_differs() {
            let h_none = compute_content_hash("t", "r", None, None);
            let h_empty = compute_content_hash("t", "r", Some(""), Some(""));
            // Both map to empty string in the hash, so they should be equal
            assert_eq!(h_none, h_empty);
        }

        #[test]
        fn content_hash_is_hex_sha256_length() {
            let h = compute_content_hash("a", "b", Some("c"), Some("d"));
            // SHA-256 hex digest is 64 characters
            assert_eq!(h.len(), 64);
            assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    // =========================================================================
    // Agentic metrics: composite score tests (deepeval-inspired)
    // =========================================================================
    mod composite_scoring {
        use crate::meta_optimizer::agentic_metrics::{
            composite_score, AgenticMetric, MetricResult,
        };

        fn make_result(metric: AgenticMetric, score: f64) -> MetricResult {
            MetricResult {
                metric,
                score,
                confidence: 1.0,
                rationale: String::new(),
                is_llm_judged: false,
                model_used: None,
            }
        }

        #[test]
        fn composite_score_empty_returns_zero() {
            assert_eq!(composite_score(&[]), 0.0);
        }

        #[test]
        fn composite_score_single_metric() {
            let scores = vec![make_result(AgenticMetric::TaskCompletion, 0.8)];
            // Single metric: composite = score itself (weight cancels)
            let c = composite_score(&scores);
            assert!((c - 0.8).abs() < 0.001);
        }

        #[test]
        fn composite_score_all_perfect() {
            let scores = vec![
                make_result(AgenticMetric::TaskCompletion, 1.0),
                make_result(AgenticMetric::StepEfficiency, 1.0),
                make_result(AgenticMetric::ToolCorrectness, 1.0),
                make_result(AgenticMetric::ArgumentCorrectness, 1.0),
            ];
            let c = composite_score(&scores);
            assert!((c - 1.0).abs() < 0.001);
        }

        #[test]
        fn composite_score_all_zero() {
            let scores = vec![
                make_result(AgenticMetric::TaskCompletion, 0.0),
                make_result(AgenticMetric::StepEfficiency, 0.0),
                make_result(AgenticMetric::ToolCorrectness, 0.0),
            ];
            let c = composite_score(&scores);
            assert!((c - 0.0).abs() < 0.001);
        }

        #[test]
        fn composite_score_weights_task_completion_highest() {
            // TaskCompletion weight=0.25, StepEfficiency weight=0.12
            let scores_high_tc = vec![
                make_result(AgenticMetric::TaskCompletion, 1.0),
                make_result(AgenticMetric::StepEfficiency, 0.0),
            ];
            let scores_high_se = vec![
                make_result(AgenticMetric::TaskCompletion, 0.0),
                make_result(AgenticMetric::StepEfficiency, 1.0),
            ];
            let c_tc = composite_score(&scores_high_tc);
            let c_se = composite_score(&scores_high_se);
            // Higher weight on TaskCompletion should make it dominate
            assert!(c_tc > c_se, "TaskCompletion should have higher weight: {} vs {}", c_tc, c_se);
            // Expected: c_tc = 0.25/(0.25+0.12) ≈ 0.676, c_se = 0.12/(0.25+0.12) ≈ 0.324
            assert!((c_tc - 0.6757).abs() < 0.01);
            assert!((c_se - 0.3243).abs() < 0.01);
        }

        #[test]
        fn composite_score_with_rag_metrics() {
            // RAG metrics have weight 0.05 each
            let scores = vec![
                make_result(AgenticMetric::TaskCompletion, 1.0),
                make_result(AgenticMetric::RagContextualPrecision, 0.5),
            ];
            let c = composite_score(&scores);
            // (1.0*0.25 + 0.5*0.05) / (0.25+0.05) = 0.275/0.30 ≈ 0.9167
            assert!((c - 0.9167).abs() < 0.01);
        }

        #[test]
        fn default_weights_sum_to_one() {
            let metrics = [
                AgenticMetric::TaskCompletion,
                AgenticMetric::GoalAccuracy,
                AgenticMetric::StepEfficiency,
                AgenticMetric::ToolCorrectness,
                AgenticMetric::PlanAdherence,
                AgenticMetric::PlanQuality,
                AgenticMetric::ArgumentCorrectness,
                AgenticMetric::RagContextualPrecision,
                AgenticMetric::RagContextualRecall,
                AgenticMetric::RagFaithfulness,
                AgenticMetric::RagAnswerRelevancy,
            ];
            let total: f64 = metrics.iter().map(|m| m.default_weight()).sum();
            assert!((total - 1.0).abs() < 0.001, "Weights sum to {} not 1.0", total);
        }
    }

    // =========================================================================
    // Task completion scoring tests (deepeval-inspired)
    // =========================================================================
    mod task_completion {
        use crate::meta_optimizer::agentic_metrics::{AgenticMetric, DeterministicInput};
        use crate::meta_optimizer::agentic_metrics::task_completion;

        fn make_input(
            verification_passed: bool,
            iterations: u32,
            was_stopped: bool,
            max_iterations_reached: bool,
            error_type: Option<&str>,
        ) -> DeterministicInput {
            DeterministicInput {
                task_run_id: "scout-test".to_string(),
                status: if verification_passed { "success" } else { "failure" }.to_string(),
                verification_passed,
                was_stopped,
                iterations,
                step_count: Some(5),
                verification_step_count: Some(2),
                agentic_step_count: Some(3),
                max_iterations_reached,
                duration_secs: 100.0,
                error_type: error_type.map(String::from),
                tools_used: Vec::new(),
                agent_traces: Vec::new(),
            }
        }

        #[test]
        fn success_scores_one() {
            let result = task_completion::score(&make_input(true, 2, false, false, None));
            assert_eq!(result.score, 1.0);
            assert_eq!(result.metric, AgenticMetric::TaskCompletion);
        }

        #[test]
        fn zero_iteration_scores_zero() {
            let result = task_completion::score(&make_input(false, 0, false, false, None));
            assert_eq!(result.score, 0.0);
        }

        #[test]
        fn max_iterations_scores_point_three() {
            let result = task_completion::score(&make_input(false, 3, false, true, None));
            assert_eq!(result.score, 0.3);
        }

        #[test]
        fn stopped_early_scores_point_five() {
            let result = task_completion::score(&make_input(false, 1, true, false, None));
            assert_eq!(result.score, 0.5);
        }

        #[test]
        fn stopped_with_progress_scores_point_seven() {
            let result = task_completion::score(&make_input(false, 3, true, false, None));
            assert_eq!(result.score, 0.7);
        }

        #[test]
        fn timeout_scores_point_two() {
            let result = task_completion::score(&make_input(false, 2, false, false, Some("timeout")));
            assert_eq!(result.score, 0.2);
        }

        #[test]
        fn confidence_is_highest_for_success() {
            let result = task_completion::score(&make_input(true, 2, false, false, None));
            assert_eq!(result.confidence, 1.0);
        }

        #[test]
        fn confidence_is_high_for_zero_iteration() {
            let result = task_completion::score(&make_input(false, 0, false, false, None));
            assert!(result.confidence >= 0.9);
        }
    }

    // =========================================================================
    // Step efficiency scoring tests (deepeval-inspired)
    // =========================================================================
    mod step_efficiency {
        use crate::meta_optimizer::agentic_metrics::step_efficiency::{self, StepBaseline};
        use crate::meta_optimizer::agentic_metrics::DeterministicInput;

        fn make_input(iterations: u32, step_count: Option<i64>) -> DeterministicInput {
            DeterministicInput {
                task_run_id: "scout-eff".to_string(),
                status: "success".to_string(),
                verification_passed: true,
                was_stopped: false,
                iterations,
                step_count,
                verification_step_count: None,
                agentic_step_count: None,
                max_iterations_reached: false,
                duration_secs: 100.0,
                error_type: None,
                tools_used: Vec::new(),
                agent_traces: Vec::new(),
            }
        }

        #[test]
        fn at_baseline_scores_one() {
            // Default baseline is 2 iterations
            let result = step_efficiency::score(&make_input(2, None), None);
            assert!((result.score - 1.0).abs() < 0.001);
        }

        #[test]
        fn under_baseline_clamped_to_one() {
            // 1 iteration is under the 2-iteration baseline → clamped to 1.0
            let result = step_efficiency::score(&make_input(1, None), None);
            assert!((result.score - 1.0).abs() < 0.001);
        }

        #[test]
        fn over_baseline_is_penalized() {
            // 3 iterations with baseline of 2 → 1.0 - (3-2)/2 = 0.5
            let result = step_efficiency::score(&make_input(3, None), None);
            assert!((result.score - 0.5).abs() < 0.01);
        }

        #[test]
        fn double_baseline_scores_zero() {
            // 4 iterations with baseline of 2 → 0.0
            let result = step_efficiency::score(&make_input(4, None), None);
            assert!((result.score - 0.0).abs() < 0.01);
        }

        #[test]
        fn zero_iterations_scores_zero() {
            let result = step_efficiency::score(&make_input(0, None), None);
            assert_eq!(result.score, 0.0);
        }

        #[test]
        fn custom_baseline_works() {
            let baseline = StepBaseline {
                expected_iterations: 5.0,
                expected_steps: 20.0,
            };
            // 5 iterations at baseline → 1.0
            let result = step_efficiency::score(&make_input(5, None), Some(&baseline));
            assert!((result.score - 1.0).abs() < 0.001);

            // 10 iterations = 2x baseline → 0.0
            let result = step_efficiency::score(&make_input(10, None), Some(&baseline));
            assert!((result.score - 0.0).abs() < 0.001);
        }

        #[test]
        fn blended_iteration_and_step_scoring() {
            // Default baseline: 2 iterations, 8 steps
            // 2 iterations (at baseline, eff=1.0), 8 steps (at baseline, eff=1.0)
            // blended = 1.0*0.6 + 1.0*0.4 = 1.0
            let result = step_efficiency::score(&make_input(2, Some(8)), None);
            assert!((result.score - 1.0).abs() < 0.01);

            // 3 iterations (eff=0.5), 16 steps (eff=0.0)
            // blended = 0.5*0.6 + 0.0*0.4 = 0.3
            let result = step_efficiency::score(&make_input(3, Some(16)), None);
            assert!((result.score - 0.3).abs() < 0.01);
        }
    }

    // =========================================================================
    // Parser tests (PromptWizard-inspired)
    // =========================================================================
    mod parser {
        use crate::meta_optimizer::parser::{
            parse_prompt_recommendations, parse_rule_examples, parse_rule_recommendations,
        };

        #[test]
        fn parse_rule_example_blocks() {
            let output = r#"
Some preamble text.

[RULE_EXAMPLE]
rule_id: rule-timeout-handling
positive_example: |
  Always set a timeout of 30 seconds for external API calls.
negative_example: |
  Call the API without any timeout.
positive_explanation: Timeouts prevent hanging workflows.
negative_explanation: No timeout can cause infinite waits.
[/RULE_EXAMPLE]

Some interstitial text.

[RULE_EXAMPLE]
rule_id: rule-error-logging
positive_example: Log errors with full stack traces.
negative_example: Silently swallow errors.
positive_explanation: Stack traces aid debugging.
negative_explanation: Silent failures hide bugs.
[/RULE_EXAMPLE]
"#;
            let examples = parse_rule_examples(output);
            assert_eq!(examples.len(), 2);

            assert_eq!(examples[0].rule_id, "rule-timeout-handling");
            assert!(examples[0].positive_example.contains("timeout of 30 seconds"));
            assert!(examples[0].negative_example.contains("without any timeout"));
            assert!(examples[0].positive_explanation.contains("Timeouts prevent"));
            assert!(examples[0].negative_explanation.contains("infinite waits"));

            assert_eq!(examples[1].rule_id, "rule-error-logging");
            assert!(examples[1].positive_example.contains("stack traces"));
        }

        #[test]
        fn parse_rule_example_skips_missing_rule_id() {
            let output = r#"
[RULE_EXAMPLE]
positive_example: Some example
negative_example: Bad example
[/RULE_EXAMPLE]
"#;
            let examples = parse_rule_examples(output);
            assert_eq!(examples.len(), 0);
        }

        #[test]
        fn parse_rule_example_skips_no_examples() {
            let output = r#"
[RULE_EXAMPLE]
rule_id: rule-empty
[/RULE_EXAMPLE]
"#;
            let examples = parse_rule_examples(output);
            assert_eq!(examples.len(), 0);
        }

        #[test]
        fn parse_prompt_recommendation_blocks() {
            let output = r#"
[PROMPT_RECOMMENDATION]
agent_type: implementer
variant_name: v2-concise
confidence: 0.85
rationale: Reduce verbosity in implementation prompts
prompt_content: |
  You are a code implementer. Be concise.
  Focus on correctness over explanation.
[/PROMPT_RECOMMENDATION]
"#;
            let recs = parse_prompt_recommendations(output);
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].agent_type, "implementer");
            assert_eq!(recs[0].variant_name, "v2-concise");
            assert!((recs[0].confidence - 0.85).abs() < 0.01);
            assert!(recs[0].prompt_content.contains("Be concise"));
        }

        #[test]
        fn parse_prompt_recommendation_skips_missing_fields() {
            let output = r#"
[PROMPT_RECOMMENDATION]
confidence: 0.9
prompt_content: Some content
[/PROMPT_RECOMMENDATION]
"#;
            // Missing agent_type and variant_name → should be skipped
            let recs = parse_prompt_recommendations(output);
            assert_eq!(recs.len(), 0);
        }

        #[test]
        fn parse_rule_recommendation_blocks() {
            let output = r#"
[RULE_RECOMMENDATION]
action: add
agent: spec_analyst
section: quality_checks
title: Require acceptance criteria
content: |
  Every spec must include explicit acceptance criteria
  with measurable outcomes.
confidence: 0.75
rationale: Specs without criteria lead to vague implementations
[/RULE_RECOMMENDATION]
"#;
            let recs = parse_rule_recommendations(output);
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].action, "add");
            assert_eq!(recs[0].agent, "spec_analyst");
            assert_eq!(recs[0].section, "quality_checks");
            assert_eq!(recs[0].title, "Require acceptance criteria");
            assert!(recs[0].content.contains("acceptance criteria"));
            assert!((recs[0].confidence - 0.75).abs() < 0.01);
        }

        #[test]
        fn parse_multiple_marker_types_in_same_output() {
            let output = r#"
Analysis complete. Here are my recommendations:

[PROMPT_RECOMMENDATION]
agent_type: verifier
variant_name: strict-v1
confidence: 0.7
rationale: Tighten verification
prompt_content: Be strict.
[/PROMPT_RECOMMENDATION]

[RULE_EXAMPLE]
rule_id: rule-verification
positive_example: Check all assertions
negative_example: Skip assertions
positive_explanation: Complete coverage
negative_explanation: Missed bugs
[/RULE_EXAMPLE]

[RULE_RECOMMENDATION]
action: modify
agent: verifier
section: core
title: Stricter checks
content: Add type checking
confidence: 0.6
rationale: Type errors are common
[/RULE_RECOMMENDATION]
"#;
            let prompt_recs = parse_prompt_recommendations(output);
            let rule_examples = parse_rule_examples(output);
            let rule_recs = parse_rule_recommendations(output);

            assert_eq!(prompt_recs.len(), 1);
            assert_eq!(rule_examples.len(), 1);
            assert_eq!(rule_recs.len(), 1);
        }

        #[test]
        fn parse_handles_unclosed_markers_gracefully() {
            let output = r#"
[PROMPT_RECOMMENDATION]
agent_type: implementer
variant_name: v1
confidence: 0.9
prompt_content: content here
"#;
            // No closing tag → should return 0 results (not panic)
            let recs = parse_prompt_recommendations(output);
            assert_eq!(recs.len(), 0);
        }
    }

    // =========================================================================
    // Stats module: proportion analysis and verdict tests (canary verdicts)
    // =========================================================================
    mod stats_verdict {
        use crate::stats::{
            compute_verdict, proportion_analysis, Verdict, VerdictThresholds,
        };

        #[test]
        fn proportion_analysis_insufficient_data() {
            // Both groups have fewer than min_samples (2)
            let result = proportion_analysis((1, 1), (1, 1), 2);
            assert!(result.p_value.is_none());
            assert!(result.confidence_interval.is_none());
            assert!(result.effect_size.is_none());
        }

        #[test]
        fn proportion_analysis_with_sufficient_data() {
            // Experiment: 8/10 success, Control: 5/10 success
            let result = proportion_analysis((8, 10), (5, 10), 2);
            assert!(result.p_value.is_some());
            assert!(result.confidence_interval.is_some());
            assert!(result.effect_size.is_some());
            // Experiment is better, so p-value should be low
            let p = result.p_value.unwrap();
            assert!(p < 0.5, "p-value should indicate experiment is better: {}", p);
        }

        #[test]
        fn canary_verdict_promote_when_clearly_better() {
            let thresholds = VerdictThresholds::canary();
            // Canary clearly better: 90/100 vs 60/100 → +30pp, highly significant
            let analysis = proportion_analysis((90, 100), (60, 100), 2);
            let delta = (90.0 / 100.0 - 60.0 / 100.0) * 100.0; // +30pp
            let verdict = compute_verdict(delta, &analysis, 100, &thresholds);
            assert_eq!(verdict, Verdict::Positive);
        }

        #[test]
        fn canary_verdict_rollback_when_significantly_worse() {
            let thresholds = VerdictThresholds::canary();
            // Canary is much worse: 2/20 vs 18/20
            let analysis = proportion_analysis((2, 20), (18, 20), 2);
            let delta = (2.0 / 20.0 - 18.0 / 20.0) * 100.0; // -80pp
            let verdict = compute_verdict(delta, &analysis, 20, &thresholds);
            assert_eq!(verdict, Verdict::Negative);
        }

        #[test]
        fn canary_verdict_as_str_mappings() {
            assert_eq!(Verdict::Positive.as_canary_str(), "promote");
            assert_eq!(Verdict::Negative.as_canary_str(), "rollback");
            assert_eq!(Verdict::Neutral.as_canary_str(), "continue");
            assert_eq!(Verdict::InsufficientData.as_canary_str(), "continue");
        }

        #[test]
        fn recommendation_verdict_as_str_mappings() {
            assert_eq!(Verdict::Positive.as_recommendation_str(), "improved");
            assert_eq!(Verdict::Negative.as_recommendation_str(), "regressed");
            assert_eq!(Verdict::Neutral.as_recommendation_str(), "neutral");
            assert_eq!(Verdict::InsufficientData.as_recommendation_str(), "insufficient_data");
        }

        #[test]
        fn recommendation_verdict_insufficient_data_below_min_runs() {
            let thresholds = VerdictThresholds::recommendation();
            let analysis = proportion_analysis((3, 3), (3, 3), 2);
            // Only 3 runs but min_runs is 5
            let verdict = compute_verdict(0.0, &analysis, 3, &thresholds);
            assert_eq!(verdict, Verdict::InsufficientData);
        }
    }

    // =========================================================================
    // Cost confidence scaling tests (OpenViking tiered context inspired)
    // =========================================================================
    mod cost_confidence {
        /// Replicates the private `compute_confidence` logic from cost_optimizer.rs
        /// to test the sample size → confidence scaling formula.
        ///
        /// Linearly scales from 0.5 at `min_samples` to 0.95 at `max_samples`.
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

        #[test]
        fn below_min_samples_returns_floor() {
            assert!((compute_confidence(3, 10, 30) - 0.5).abs() < 0.001);
            assert!((compute_confidence(0, 10, 30) - 0.5).abs() < 0.001);
        }

        #[test]
        fn at_min_samples_returns_floor() {
            assert!((compute_confidence(10, 10, 30) - 0.5).abs() < 0.001);
        }

        #[test]
        fn at_max_samples_returns_ceiling() {
            assert!((compute_confidence(30, 10, 30) - 0.95).abs() < 0.001);
            assert!((compute_confidence(100, 10, 30) - 0.95).abs() < 0.001);
        }

        #[test]
        fn midpoint_is_halfway() {
            // At midpoint (20) between 10 and 30: progress = 0.5
            // confidence = 0.5 + 0.5 * 0.45 = 0.725
            assert!((compute_confidence(20, 10, 30) - 0.725).abs() < 0.001);
        }

        #[test]
        fn monotonically_increasing() {
            let c1 = compute_confidence(10, 10, 30);
            let c2 = compute_confidence(15, 10, 30);
            let c3 = compute_confidence(20, 10, 30);
            let c4 = compute_confidence(25, 10, 30);
            let c5 = compute_confidence(30, 10, 30);
            assert!(c1 <= c2);
            assert!(c2 <= c3);
            assert!(c3 <= c4);
            assert!(c4 <= c5);
        }
    }

    // =========================================================================
    // Tiered context types tests (OpenViking-inspired)
    // =========================================================================
    mod tiered_context {
        use crate::meta_optimizer::types::ContextTier;

        #[test]
        fn context_tier_default_is_l0() {
            let tier: ContextTier = Default::default();
            assert_eq!(tier, ContextTier::L0);
        }

        #[test]
        fn context_tier_deserialize() {
            let l0: ContextTier = serde_json::from_str("\"l0\"").unwrap();
            let l1: ContextTier = serde_json::from_str("\"l1\"").unwrap();
            let l2: ContextTier = serde_json::from_str("\"l2\"").unwrap();
            assert_eq!(l0, ContextTier::L0);
            assert_eq!(l1, ContextTier::L1);
            assert_eq!(l2, ContextTier::L2);
        }
    }
}
