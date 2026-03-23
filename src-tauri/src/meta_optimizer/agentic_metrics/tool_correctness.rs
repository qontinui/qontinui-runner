//! Tool Correctness metric — deterministic scorer.
//!
//! Evaluates whether the workflow used the expected tools for its task type.
//! Compares `tools_used` (from learning_outcomes) against a learned baseline
//! of expected tools per workflow category.
//!
//! Scoring approach:
//! - **Jaccard similarity** between actual and expected tool sets
//! - Bonus for using all expected tools (recall)
//! - Penalty for using many unexpected tools (precision)
//! - Zero tools used with non-zero baseline → 0.0
//! - No baseline available → 0.5 with low confidence (neutral)

use super::{AgenticMetric, MetricResult};

/// Learned tool baseline for a workflow category.
#[derive(Debug, Clone)]
pub struct ToolBaseline {
    /// Tools commonly used by successful runs of this workflow type.
    pub expected_tools: Vec<String>,
}

/// Input data specific to tool correctness scoring.
#[derive(Debug, Clone)]
pub struct ToolCorrectnessInput {
    /// Tools actually used during the workflow run.
    pub tools_used: Vec<String>,
    /// Whether the run had zero iterations (startup crash).
    pub zero_iterations: bool,
}

/// Compute the tool correctness score.
///
/// `baseline`: optional learned tool baseline; returns neutral score if None.
pub fn score(input: &ToolCorrectnessInput, baseline: Option<&ToolBaseline>) -> MetricResult {
    let (score_val, rationale, confidence) = compute(input, baseline);

    MetricResult {
        metric: AgenticMetric::ToolCorrectness,
        score: score_val,
        confidence,
        rationale,
        is_llm_judged: false,
        model_used: None,
    }
}

fn compute(
    input: &ToolCorrectnessInput,
    baseline: Option<&ToolBaseline>,
) -> (f64, String, f64) {
    // Zero-iteration crash — no tools could have been used
    if input.zero_iterations {
        return (
            0.0,
            "Zero iterations — no tools executed".into(),
            0.95,
        );
    }

    let baseline = match baseline {
        Some(b) if !b.expected_tools.is_empty() => b,
        _ => {
            // No baseline — can't judge tool correctness
            if input.tools_used.is_empty() {
                return (
                    0.5,
                    "No tools used and no baseline available — neutral score".into(),
                    0.3,
                );
            }
            return (
                0.5,
                format!(
                    "No tool baseline available — {} tools used but correctness unknown",
                    input.tools_used.len()
                ),
                0.3,
            );
        }
    };

    // No tools used but baseline expects some
    if input.tools_used.is_empty() {
        return (
            0.0,
            format!(
                "No tools used but {} expected: [{}]",
                baseline.expected_tools.len(),
                baseline.expected_tools.join(", ")
            ),
            0.85,
        );
    }

    // Compute set overlap metrics
    let actual: std::collections::HashSet<&str> =
        input.tools_used.iter().map(|s| s.as_str()).collect();
    let expected: std::collections::HashSet<&str> =
        baseline.expected_tools.iter().map(|s| s.as_str()).collect();

    let intersection = actual.intersection(&expected).count() as f64;
    let union = actual.union(&expected).count() as f64;

    // Jaccard similarity: |A ∩ B| / |A ∪ B|
    let jaccard = if union > 0.0 {
        intersection / union
    } else {
        1.0 // Both empty
    };

    // Recall: what fraction of expected tools were used?
    let recall = if !expected.is_empty() {
        intersection / expected.len() as f64
    } else {
        1.0
    };

    // Precision: what fraction of used tools were expected?
    let precision = if !actual.is_empty() {
        intersection / actual.len() as f64
    } else {
        1.0
    };

    // Blended score: 40% Jaccard + 35% recall + 25% precision
    // Recall weighted higher because missing expected tools is worse
    // than having extra tools (which may be legitimately novel).
    let blended = jaccard * 0.40 + recall * 0.35 + precision * 0.25;

    let unexpected: Vec<&&str> = actual.difference(&expected).collect();
    let missing: Vec<&&str> = expected.difference(&actual).collect();

    let mut rationale = format!(
        "Jaccard={:.2}, recall={:.2} ({}/{} expected), precision={:.2} ({}/{} used)",
        jaccard,
        recall,
        intersection as u32,
        expected.len(),
        precision,
        intersection as u32,
        actual.len(),
    );

    if !missing.is_empty() {
        rationale.push_str(&format!(
            ". Missing: [{}]",
            missing.iter().map(|s| **s).collect::<Vec<_>>().join(", ")
        ));
    }
    if !unexpected.is_empty() {
        rationale.push_str(&format!(
            ". Unexpected: [{}]",
            unexpected
                .iter()
                .map(|s| **s)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    (blended, rationale, 0.8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline(tools: &[&str]) -> ToolBaseline {
        ToolBaseline {
            expected_tools: tools.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn input(tools: &[&str], zero_iter: bool) -> ToolCorrectnessInput {
        ToolCorrectnessInput {
            tools_used: tools.iter().map(|s| s.to_string()).collect(),
            zero_iterations: zero_iter,
        }
    }

    #[test]
    fn test_zero_iterations() {
        let result = score(&input(&[], true), Some(&baseline(&["read_file"])));
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn test_perfect_match() {
        let b = baseline(&["read_file", "write_file"]);
        let result = score(&input(&["read_file", "write_file"], false), Some(&b));
        assert!((result.score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_partial_match() {
        let b = baseline(&["read_file", "write_file", "run_command"]);
        let result = score(&input(&["read_file", "write_file"], false), Some(&b));
        // Jaccard = 2/3, recall = 2/3, precision = 1.0
        // blended = 0.667*0.4 + 0.667*0.35 + 1.0*0.25 = 0.267 + 0.233 + 0.25 = 0.75
        assert!((result.score - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_no_overlap() {
        let b = baseline(&["read_file"]);
        let result = score(&input(&["run_command"], false), Some(&b));
        // Jaccard = 0/2, recall = 0/1, precision = 0/1 → blended = 0.0
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn test_no_baseline() {
        let result = score(&input(&["read_file"], false), None);
        assert_eq!(result.score, 0.5);
        assert!(result.confidence < 0.5);
    }

    #[test]
    fn test_extra_tools() {
        let b = baseline(&["read_file"]);
        let result = score(
            &input(&["read_file", "write_file", "run_command"], false),
            Some(&b),
        );
        // Jaccard = 1/3, recall = 1/1, precision = 1/3
        // blended = 0.333*0.4 + 1.0*0.35 + 0.333*0.25 = 0.133 + 0.35 + 0.083 = 0.567
        assert!((result.score - 0.567).abs() < 0.02);
    }

    #[test]
    fn test_no_tools_used_with_baseline() {
        let b = baseline(&["read_file"]);
        let result = score(&input(&[], false), Some(&b));
        assert_eq!(result.score, 0.0);
    }
}
