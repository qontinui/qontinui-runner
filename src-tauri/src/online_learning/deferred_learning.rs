//! Deferred question confidence threshold learning.
//!
//! After each workflow run, this module analyzes the approval/rejection history
//! of deferred questions to adjust confidence thresholds per workflow. Over time,
//! workflows that consistently have their auto-decisions approved will see higher
//! confidence baselines (fewer questions), while workflows with frequent rejections
//! will see lower baselines (more questions).
//!
//! ## Algorithm
//!
//! 1. Query all reviewed deferred questions for the workflow (last 30 days)
//! 2. Bucket by confidence score at time of creation
//! 3. Compute approval rate per bucket
//! 4. Find the confidence level where approval rate crosses the target (default 0.80)
//! 5. Store the adjusted threshold in `agentic_metric_baselines`

use std::sync::Arc;
use tracing::{debug, info};

/// Minimum reviewed questions needed before adjusting thresholds.
const MIN_SAMPLES: usize = 5;

/// Target approval rate — the threshold is set where this fraction of
/// auto-decisions are approved by humans.
const TARGET_APPROVAL_RATE: f64 = 0.80;

/// Confidence bucket boundaries for analysis.
const BUCKET_BOUNDARIES: &[f64] = &[0.0, 0.2, 0.4, 0.6, 0.7, 0.8, 0.9, 1.0];

/// Statistics for a single confidence bucket.
#[derive(Debug)]
struct BucketStats {
    min: f64,
    max: f64,
    total: usize,
    approved: usize,
    approval_rate: f64,
}

/// Run the deferred question learning loop for a completed workflow.
///
/// Queries reviewed deferred questions for the workflow, computes approval
/// rates by confidence bucket, and adjusts the stored confidence threshold.
pub async fn update_confidence_threshold(pg: &Arc<crate::database::pg::PgDb>, workflow_id: &str) {
    // 1. Get all reviewed (non-pending) deferred questions for this workflow
    let questions = match get_reviewed_questions_for_workflow(pg, workflow_id).await {
        Ok(qs) => qs,
        Err(e) => {
            debug!(
                "Deferred learning: failed to query questions for {}: {}",
                workflow_id, e
            );
            return;
        }
    };

    if questions.len() < MIN_SAMPLES {
        debug!(
            "Deferred learning: only {} reviewed questions for {} (need {}), skipping",
            questions.len(),
            workflow_id,
            MIN_SAMPLES,
        );
        return;
    }

    // 2. Compute approval rates per confidence bucket
    let buckets = compute_bucket_stats(&questions);

    // 3. Find the optimal threshold
    let new_threshold = find_optimal_threshold(&buckets);

    // 4. Persist to DB
    let evidence = format_evidence(&buckets);
    if let Err(e) = pg
        .upsert_deferred_confidence_threshold(workflow_id, new_threshold, &evidence)
        .await
    {
        debug!("Deferred learning: failed to persist threshold: {}", e);
        return;
    }

    info!(
        "Deferred learning: adjusted threshold for {} to {:.2} (from {} reviewed questions)",
        workflow_id,
        new_threshold,
        questions.len(),
    );
}

/// A reviewed deferred question with the fields we need.
struct ReviewedQuestion {
    confidence: f64,
    approved: bool,
}

/// Query reviewed deferred questions for a workflow (via PgDb method).
async fn get_reviewed_questions_for_workflow(
    pg: &Arc<crate::database::pg::PgDb>,
    workflow_id: &str,
) -> Result<Vec<ReviewedQuestion>, String> {
    let pairs = pg
        .get_reviewed_deferred_questions_for_workflow(workflow_id)
        .await?;

    Ok(pairs
        .into_iter()
        .map(|(confidence, approved)| ReviewedQuestion {
            confidence,
            approved,
        })
        .collect())
}

/// Compute approval rate statistics per confidence bucket.
fn compute_bucket_stats(questions: &[ReviewedQuestion]) -> Vec<BucketStats> {
    let mut buckets: Vec<BucketStats> = BUCKET_BOUNDARIES
        .windows(2)
        .map(|w| BucketStats {
            min: w[0],
            max: w[1],
            total: 0,
            approved: 0,
            approval_rate: 0.0,
        })
        .collect();

    for q in questions {
        for bucket in &mut buckets {
            if q.confidence >= bucket.min && q.confidence < bucket.max {
                bucket.total += 1;
                if q.approved {
                    bucket.approved += 1;
                }
                break;
            }
        }
    }

    // Compute rates
    for bucket in &mut buckets {
        bucket.approval_rate = if bucket.total > 0 {
            bucket.approved as f64 / bucket.total as f64
        } else {
            1.0 // No data = assume safe
        };
    }

    buckets
}

/// Find the confidence threshold where the approval rate crosses the target.
///
/// Walks from high confidence downward. The threshold is set at the lowest
/// bucket boundary where the cumulative approval rate still exceeds
/// TARGET_APPROVAL_RATE. Empty buckets (no data) are skipped — they don't
/// contribute to lowering or raising the threshold.
///
/// If all buckets have high approval rates, returns a low threshold (few questions).
/// If all have low rates, returns a high threshold (more questions).
fn find_optimal_threshold(buckets: &[BucketStats]) -> f64 {
    let mut threshold = 0.85; // Sensible default

    // Accumulate from high confidence downward, skipping empty buckets
    let mut cumulative_total = 0;
    let mut cumulative_approved = 0;

    for bucket in buckets.iter().rev() {
        // Skip empty buckets — no data means no evidence
        if bucket.total == 0 {
            continue;
        }

        cumulative_total += bucket.total;
        cumulative_approved += bucket.approved;

        if cumulative_total >= 2 {
            let cumulative_rate = cumulative_approved as f64 / cumulative_total as f64;
            if cumulative_rate >= TARGET_APPROVAL_RATE {
                // This confidence level and above are safe to auto-approve
                threshold = bucket.min;
            } else {
                // Below target — stop lowering threshold
                break;
            }
        }
    }

    // Clamp to reasonable range
    threshold.clamp(0.3, 0.95)
}

/// Format evidence string for database storage.
fn format_evidence(buckets: &[BucketStats]) -> String {
    let mut lines = Vec::new();
    for b in buckets {
        if b.total > 0 {
            lines.push(format!(
                "[{:.1}-{:.1}): {}/{} approved ({:.0}%)",
                b.min,
                b.max,
                b.approved,
                b.total,
                b.approval_rate * 100.0,
            ));
        }
    }
    lines.join("; ")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_stats_all_approved() {
        let questions: Vec<ReviewedQuestion> = (0..10)
            .map(|i| ReviewedQuestion {
                confidence: 0.1 * i as f64,
                approved: true,
            })
            .collect();
        let buckets = compute_bucket_stats(&questions);
        for b in &buckets {
            if b.total > 0 {
                assert_eq!(b.approval_rate, 1.0);
            }
        }
    }

    #[test]
    fn test_bucket_stats_mixed() {
        let questions = vec![
            ReviewedQuestion {
                confidence: 0.3,
                approved: false,
            },
            ReviewedQuestion {
                confidence: 0.35,
                approved: false,
            },
            ReviewedQuestion {
                confidence: 0.5,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.7,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.8,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.9,
                approved: true,
            },
        ];
        let buckets = compute_bucket_stats(&questions);

        // [0.2-0.4) bucket should have 2 questions, 0 approved
        let low_bucket = buckets.iter().find(|b| b.min == 0.2).unwrap();
        assert_eq!(low_bucket.total, 2);
        assert_eq!(low_bucket.approved, 0);

        // [0.8-0.9) bucket should have 1 question, 1 approved
        let high_bucket = buckets.iter().find(|b| b.min == 0.8).unwrap();
        assert_eq!(high_bucket.total, 1);
        assert_eq!(high_bucket.approved, 1);
    }

    #[test]
    fn test_find_threshold_all_approved() {
        let questions: Vec<ReviewedQuestion> = (0..20)
            .map(|i| ReviewedQuestion {
                confidence: 0.05 * i as f64,
                approved: true,
            })
            .collect();
        let buckets = compute_bucket_stats(&questions);
        let threshold = find_optimal_threshold(&buckets);
        // All approved → threshold should be low (auto-approve most things)
        assert!(threshold < 0.5, "threshold={}", threshold);
    }

    #[test]
    fn test_find_threshold_low_confidence_rejected() {
        let questions = vec![
            ReviewedQuestion {
                confidence: 0.1,
                approved: false,
            },
            ReviewedQuestion {
                confidence: 0.2,
                approved: false,
            },
            ReviewedQuestion {
                confidence: 0.3,
                approved: false,
            },
            ReviewedQuestion {
                confidence: 0.5,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.6,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.7,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.8,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.9,
                approved: true,
            },
        ];
        let buckets = compute_bucket_stats(&questions);
        let threshold = find_optimal_threshold(&buckets);
        // Low confidence rejected, high approved → threshold around 0.4-0.6
        assert!(threshold >= 0.3, "threshold={}", threshold);
        assert!(threshold <= 0.7, "threshold={}", threshold);
    }

    #[test]
    fn test_threshold_clamped() {
        // Edge case: all buckets have high approval rates
        let questions = vec![
            ReviewedQuestion {
                confidence: 0.05,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.05,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.15,
                approved: true,
            },
            ReviewedQuestion {
                confidence: 0.15,
                approved: true,
            },
        ];
        let buckets = compute_bucket_stats(&questions);
        let threshold = find_optimal_threshold(&buckets);
        assert!(threshold >= 0.3, "threshold should be >= 0.3 floor");
        assert!(threshold <= 0.95, "threshold should be <= 0.95 ceiling");
    }

    #[test]
    fn test_format_evidence() {
        let buckets = vec![
            BucketStats {
                min: 0.0,
                max: 0.2,
                total: 0,
                approved: 0,
                approval_rate: 1.0,
            },
            BucketStats {
                min: 0.2,
                max: 0.4,
                total: 3,
                approved: 1,
                approval_rate: 0.333,
            },
            BucketStats {
                min: 0.4,
                max: 0.6,
                total: 5,
                approved: 4,
                approval_rate: 0.8,
            },
        ];
        let evidence = format_evidence(&buckets);
        assert!(evidence.contains("[0.2-0.4): 1/3"));
        assert!(evidence.contains("[0.4-0.6): 4/5"));
        assert!(!evidence.contains("[0.0-0.2)"));
    }
}
