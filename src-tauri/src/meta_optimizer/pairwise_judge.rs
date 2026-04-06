//! Pairwise LLM judging for the Prompt Duel Optimizer.
//!
//! Builds prompts that ask an LLM "which output is better?" and parses the
//! response.  Position bias is mitigated by swapping presentation order on
//! alternating duels.  The module never calls the LLM itself — the caller
//! sends the built prompt through the existing AI provider system and hands
//! the raw response back here for parsing.

use serde::{Deserialize, Serialize};
use tracing::warn;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Input to a single pairwise comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseJudgeInput {
    /// What the task / prompt is trying to accomplish.
    pub task_description: String,
    /// Output produced with candidate A's prompt.
    pub output_a: String,
    /// Output produced with candidate B's prompt.
    pub output_b: String,
    /// Judging criteria, e.g. `["correctness", "completeness", "clarity"]`.
    pub criteria: Vec<String>,
}

/// Who won the pairwise comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairwiseWinner {
    A,
    B,
    Tie,
}

/// Parsed judge response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseJudgeResult {
    pub winner: PairwiseWinner,
    pub rationale: String,
    pub confidence: f64,
    /// Per-criterion scores for candidate A (criterion name, score 0–10).
    pub scores_a: Option<Vec<(String, f64)>>,
    /// Per-criterion scores for candidate B (criterion name, score 0–10).
    pub scores_b: Option<Vec<(String, f64)>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a prompt asking the LLM to compare two outputs.
///
/// When `swap_positions` is true the outputs are presented in reverse order
/// (output_b as "Output 1", output_a as "Output 2") to mitigate position bias.
pub fn build_pairwise_prompt(input: &PairwiseJudgeInput, swap_positions: bool) -> String {
    let (first, second) = if swap_positions {
        (&input.output_b, &input.output_a)
    } else {
        (&input.output_a, &input.output_b)
    };

    let criteria_list = input
        .criteria
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are an impartial judge evaluating two outputs for the same task.

## Task Description
{task}

## Output 1
{first}

## Output 2
{second}

## Judging Criteria
{criteria_list}

## Instructions
Compare the two outputs above against every listed criterion. Be objective — the order in which the outputs are presented must not influence your judgement.

Respond with a single JSON object (no extra text) using exactly this schema:
{{
  "winner": <1 | 2 | "tie">,
  "rationale": "<brief explanation>",
  "confidence": <float 0.0–1.0>,
  "scores_1": {{ "<criterion>": <score 0–10>, ... }},
  "scores_2": {{ "<criterion>": <score 0–10>, ... }}
}}"#,
        task = input.task_description,
        first = first,
        second = second,
        criteria_list = criteria_list,
    )
}

/// Parse the LLM's JSON response into a [`PairwiseJudgeResult`].
///
/// Handles JSON in markdown code blocks, bare JSON, and various winner
/// representations. If `was_swapped` is true the positional winner is
/// reversed (1→B, 2→A instead of 1→A, 2→B).
///
/// Returns a `Tie` with confidence 0.0 on any parse failure.
pub fn parse_pairwise_response(response: &str, was_swapped: bool) -> PairwiseJudgeResult {
    let fallback = PairwiseJudgeResult {
        winner: PairwiseWinner::Tie,
        rationale: String::from("Failed to parse judge response"),
        confidence: 0.0,
        scores_a: None,
        scores_b: None,
    };

    let json_str = extract_json(response);
    let json_str = match json_str {
        Some(s) => s,
        None => {
            warn!("pairwise_judge: no JSON found in response");
            return fallback;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("pairwise_judge: JSON parse error: {e}");
            return fallback;
        }
    };

    // -- winner --
    let positional_winner = match &parsed["winner"] {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_u64() {
                match i {
                    1 => PositionalWinner::One,
                    2 => PositionalWinner::Two,
                    _ => PositionalWinner::Tie,
                }
            } else {
                PositionalWinner::Tie
            }
        }
        serde_json::Value::String(s) => match s.trim().to_lowercase().as_str() {
            "1" => PositionalWinner::One,
            "2" => PositionalWinner::Two,
            "tie" => PositionalWinner::Tie,
            _ => PositionalWinner::Tie,
        },
        _ => PositionalWinner::Tie,
    };

    let winner = resolve_winner(positional_winner, was_swapped);

    // -- rationale --
    let rationale = parsed["rationale"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // -- confidence (normalized and clamped) --
    let raw_confidence = parsed["confidence"].as_f64().unwrap_or(0.0);
    let confidence = if raw_confidence > 1.0 && raw_confidence <= 100.0 {
        // LLM returned 0-100 scale instead of 0-1; normalize
        (raw_confidence / 100.0).clamp(0.0, 1.0)
    } else {
        raw_confidence.clamp(0.0, 1.0)
    };

    // -- per-criterion scores --
    // scores_1 / scores_2 are positional; map them to A/B respecting swap.
    let scores_1 = parse_scores_object(&parsed["scores_1"]);
    let scores_2 = parse_scores_object(&parsed["scores_2"]);

    let (scores_a, scores_b) = if was_swapped {
        (scores_2, scores_1)
    } else {
        (scores_1, scores_2)
    };

    PairwiseJudgeResult {
        winner,
        rationale,
        confidence,
        scores_a,
        scores_b,
    }
}

/// Deterministic position-bias mitigation: swap on odd duel indices.
pub fn should_swap_positions(duel_index: usize) -> bool {
    duel_index % 2 == 1
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum PositionalWinner {
    One,
    Two,
    Tie,
}

fn resolve_winner(positional: PositionalWinner, was_swapped: bool) -> PairwiseWinner {
    match positional {
        PositionalWinner::Tie => PairwiseWinner::Tie,
        PositionalWinner::One => {
            if was_swapped {
                PairwiseWinner::B
            } else {
                PairwiseWinner::A
            }
        }
        PositionalWinner::Two => {
            if was_swapped {
                PairwiseWinner::A
            } else {
                PairwiseWinner::B
            }
        }
    }
}

/// Extract the JSON substring from a response that may wrap it in a markdown
/// code block.
fn extract_json(response: &str) -> Option<&str> {
    // Try ```json ... ``` first
    if let Some(start) = response.find("```json") {
        let after_tag = start + "```json".len();
        if let Some(end) = response[after_tag..].find("```") {
            return Some(response[after_tag..after_tag + end].trim());
        }
    }
    // Try ``` ... ```
    if let Some(start) = response.find("```") {
        let after_tag = start + "```".len();
        if let Some(end) = response[after_tag..].find("```") {
            let inner = response[after_tag..after_tag + end].trim();
            if inner.starts_with('{') {
                return Some(inner);
            }
        }
    }
    // Try bare JSON
    let trimmed = response.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    None
}

/// Parse a `{ "criterion": score }` object into a `Vec<(String, f64)>`.
fn parse_scores_object(value: &serde_json::Value) -> Option<Vec<(String, f64)>> {
    let obj = value.as_object()?;
    let mut out = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        if let Some(score) = v.as_f64() {
            out.push((k.clone(), score));
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> PairwiseJudgeInput {
        PairwiseJudgeInput {
            task_description: "Summarize an article about climate change.".into(),
            output_a: "Output from candidate A.".into(),
            output_b: "Output from candidate B.".into(),
            criteria: vec![
                "correctness".into(),
                "completeness".into(),
                "clarity".into(),
            ],
        }
    }

    #[test]
    fn test_build_prompt_no_swap() {
        let input = sample_input();
        let prompt = build_pairwise_prompt(&input, false);
        // output_a should appear as Output 1
        let pos_a = prompt.find("## Output 1").unwrap();
        let pos_b = prompt.find("## Output 2").unwrap();
        let section_1 = &prompt[pos_a..pos_b];
        assert!(
            section_1.contains("Output from candidate A"),
            "Output 1 should contain output_a"
        );
        let section_2 = &prompt[pos_b..];
        assert!(
            section_2.contains("Output from candidate B"),
            "Output 2 should contain output_b"
        );
    }

    #[test]
    fn test_build_prompt_with_swap() {
        let input = sample_input();
        let prompt = build_pairwise_prompt(&input, true);
        let pos_a = prompt.find("## Output 1").unwrap();
        let pos_b = prompt.find("## Output 2").unwrap();
        let section_1 = &prompt[pos_a..pos_b];
        // When swapped, output_b is presented as Output 1
        assert!(
            section_1.contains("Output from candidate B"),
            "Output 1 should contain output_b when swapped"
        );
        let section_2 = &prompt[pos_b..];
        assert!(
            section_2.contains("Output from candidate A"),
            "Output 2 should contain output_a when swapped"
        );
    }

    #[test]
    fn test_parse_valid_response() {
        let json = r#"{
            "winner": 1,
            "rationale": "Output 1 is more accurate.",
            "confidence": 0.85,
            "scores_1": {"correctness": 9, "completeness": 8, "clarity": 7},
            "scores_2": {"correctness": 6, "completeness": 7, "clarity": 8}
        }"#;

        let result = parse_pairwise_response(json, false);
        assert_eq!(result.winner, PairwiseWinner::A);
        assert_eq!(result.rationale, "Output 1 is more accurate.");
        assert!((result.confidence - 0.85).abs() < f64::EPSILON);
        let scores_a = result.scores_a.unwrap();
        assert!(scores_a.iter().any(|(k, v)| k == "correctness" && (*v - 9.0).abs() < f64::EPSILON));
        let scores_b = result.scores_b.unwrap();
        assert!(scores_b.iter().any(|(k, v)| k == "correctness" && (*v - 6.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_parse_swapped_response() {
        // LLM says winner is 1, but positions were swapped so real winner is B
        let json = r#"{
            "winner": 1,
            "rationale": "First output was better.",
            "confidence": 0.9,
            "scores_1": {"correctness": 9},
            "scores_2": {"correctness": 5}
        }"#;

        let result = parse_pairwise_response(json, true);
        assert_eq!(result.winner, PairwiseWinner::B);
        // scores_1 (positional) maps to B when swapped, scores_2 maps to A
        let scores_a = result.scores_a.unwrap();
        assert!(scores_a.iter().any(|(k, v)| k == "correctness" && (*v - 5.0).abs() < f64::EPSILON));
        let scores_b = result.scores_b.unwrap();
        assert!(scores_b.iter().any(|(k, v)| k == "correctness" && (*v - 9.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_parse_response_in_code_block() {
        let response = r#"Here is my evaluation:

```json
{
    "winner": 2,
    "rationale": "Second output is clearer.",
    "confidence": 0.75,
    "scores_1": {"clarity": 5},
    "scores_2": {"clarity": 9}
}
```

That's my assessment."#;

        let result = parse_pairwise_response(response, false);
        assert_eq!(result.winner, PairwiseWinner::B);
        assert!((result.confidence - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_invalid_response() {
        let result = parse_pairwise_response("this is not json at all", false);
        assert_eq!(result.winner, PairwiseWinner::Tie);
        assert!((result.confidence - 0.0).abs() < f64::EPSILON);
        assert!(result.scores_a.is_none());
        assert!(result.scores_b.is_none());
    }

    #[test]
    fn test_parse_tie() {
        let json = r#"{
            "winner": "tie",
            "rationale": "Both are equally good.",
            "confidence": 0.5,
            "scores_1": {"correctness": 7},
            "scores_2": {"correctness": 7}
        }"#;

        let result = parse_pairwise_response(json, false);
        assert_eq!(result.winner, PairwiseWinner::Tie);
        assert_eq!(result.rationale, "Both are equally good.");
    }

    #[test]
    fn test_should_swap() {
        assert!(!should_swap_positions(0));
        assert!(should_swap_positions(1));
        assert!(!should_swap_positions(2));
        assert!(should_swap_positions(3));
        assert!(!should_swap_positions(100));
        assert!(should_swap_positions(101));
    }
}
