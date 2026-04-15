//! APO-style beam search for meta-prompt optimization.
//!
//! Manages multiple prompt candidates across generations, generating rewrite
//! requests and pruning based on Copeland scores. Each generation expands the
//! top candidates via different thinking styles, then prunes back to beam width.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::debug;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for beam search prompt optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeamSearchConfig {
    /// How many candidates to keep per generation.
    pub beam_width: usize,
    /// Number of children generated per parent.
    pub branch_factor: usize,
    /// Total generations to run.
    pub max_generations: usize,
    /// Diversity mutation tips cycled across children.
    pub thinking_styles: Vec<String>,
}

impl Default for BeamSearchConfig {
    fn default() -> Self {
        Self {
            beam_width: 4,
            branch_factor: 2,
            max_generations: 3,
            thinking_styles: vec![
                "Evidence-driven: Ground changes in specific failure examples".into(),
                "Structural clarity: Restructure for clarity with step-by-step instructions".into(),
                "Constraint-tightening: Add explicit constraints and guardrails".into(),
                "Few-shot exemplar: Add concrete input/output examples".into(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Candidate
// ---------------------------------------------------------------------------

/// A single candidate prompt variant in the beam.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeamCandidate {
    pub id: String,
    pub prompt_content: String,
    pub critique: String,
    pub changes_summary: String,
    pub parent_id: Option<String>,
    pub generation: u32,
    pub thinking_style: String,
}

// ---------------------------------------------------------------------------
// Generation request
// ---------------------------------------------------------------------------

/// A request to generate a child candidate via LLM.
#[derive(Debug, Clone)]
pub struct GenerationRequest {
    pub parent_id: String,
    pub parent_prompt: String,
    pub thinking_style: String,
    pub critique_context: String,
    pub llm_prompt: String,
}

// ---------------------------------------------------------------------------
// Beam search state machine
// ---------------------------------------------------------------------------

/// Manages the beam search state across generations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeamSearchState {
    pub config: BeamSearchConfig,
    pub agent_type: String,
    pub current_generation: u32,
    pub candidates: Vec<BeamCandidate>,
    pub pruned_ids: HashSet<String>,
}

impl BeamSearchState {
    /// Create initial state with a single seed candidate at generation 0.
    pub fn new(config: BeamSearchConfig, agent_type: &str, seed_prompt: &str) -> Self {
        let seed = BeamCandidate {
            id: format!("beam-seed-{}", uuid::Uuid::new_v4()),
            prompt_content: seed_prompt.to_string(),
            critique: String::new(),
            changes_summary: String::new(),
            parent_id: None,
            generation: 0,
            thinking_style: "seed".to_string(),
        };
        debug!(
            agent_type,
            seed_id = %seed.id,
            "Created beam search seed candidate"
        );
        Self {
            config,
            agent_type: agent_type.to_string(),
            current_generation: 0,
            candidates: vec![seed],
            pruned_ids: HashSet::new(),
        }
    }

    /// Build LLM prompts for the next generation of children.
    ///
    /// `parent_scores` maps candidate IDs to their Copeland scores. If empty,
    /// all current-generation active candidates are used as parents.
    /// `failure_evidence` is raw failure text to include in the prompt.
    /// `rejected_prompts` are (id, prompt_content) pairs to avoid repeating.
    pub fn prepare_generation_prompts(
        &self,
        parent_scores: &[(String, f64)],
        failure_evidence: &str,
        rejected_prompts: &[(String, String)],
    ) -> Vec<GenerationRequest> {
        let active = self.active_candidates();

        // Select parents: top beam_width by score, or all active if no scores.
        let parents: Vec<&BeamCandidate> = if parent_scores.is_empty() {
            active.into_iter().take(self.config.beam_width).collect()
        } else {
            let mut scored: Vec<(&BeamCandidate, f64)> = active
                .into_iter()
                .filter_map(|c| {
                    parent_scores
                        .iter()
                        .find(|(id, _)| id == &c.id)
                        .map(|(_, s)| (c, *s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored
                .into_iter()
                .take(self.config.beam_width)
                .map(|(c, _)| c)
                .collect()
        };

        let styles = &self.config.thinking_styles;
        let mut requests = Vec::new();
        let mut style_idx = 0;

        for parent in &parents {
            for _ in 0..self.config.branch_factor {
                let style = if styles.is_empty() {
                    "General improvement".to_string()
                } else {
                    let s = styles[style_idx % styles.len()].clone();
                    style_idx += 1;
                    s
                };

                let llm_prompt = build_rewrite_prompt(
                    &parent.prompt_content,
                    failure_evidence,
                    rejected_prompts,
                    &style,
                );

                requests.push(GenerationRequest {
                    parent_id: parent.id.clone(),
                    parent_prompt: parent.prompt_content.clone(),
                    thinking_style: style,
                    critique_context: failure_evidence.to_string(),
                    llm_prompt,
                });
            }
        }

        debug!(
            count = requests.len(),
            generation = self.current_generation + 1,
            "Prepared generation requests"
        );
        requests
    }

    /// Process LLM responses and create new candidates.
    ///
    /// `responses` are (parent_id, raw_llm_response, thinking_style) triples.
    /// The thinking_style comes from the GenerationRequest, not the parent.
    /// Returns the newly created candidates.
    pub fn process_generation_responses(
        &mut self,
        responses: Vec<(String, String, String)>,
    ) -> Vec<BeamCandidate> {
        let next_gen = self.current_generation + 1;
        let mut new_candidates = Vec::new();

        for (parent_id, response, thinking_style) in responses {
            let blocks = extract_beam_rewrite_blocks(&response);
            if blocks.is_empty() {
                debug!(parent_id, "No BEAM_REWRITE block found in LLM response");
                continue;
            }

            for (critique, changes_summary, rewritten_prompt) in blocks {
                let candidate = BeamCandidate {
                    id: format!("beam-{}-{}", next_gen, uuid::Uuid::new_v4()),
                    prompt_content: rewritten_prompt,
                    critique,
                    changes_summary,
                    parent_id: Some(parent_id.clone()),
                    generation: next_gen,
                    thinking_style: thinking_style.clone(),
                };
                new_candidates.push(candidate);
            }
        }

        debug!(
            count = new_candidates.len(),
            generation = next_gen,
            "Processed generation responses"
        );

        self.candidates.extend(new_candidates.clone());
        self.current_generation = next_gen;
        new_candidates
    }

    /// Prune candidates from the current generation to beam width based on scores.
    ///
    /// Candidates from previous generations that are still parents of active
    /// candidates are kept. Pruned IDs are recorded in `self.pruned_ids`.
    pub fn prune_to_beam_width(&mut self, scores: &[(String, f64)]) {
        let gen = self.current_generation;

        // Separate current-gen candidates from older ones.
        let mut current_gen_ids: Vec<(String, f64)> = self
            .candidates
            .iter()
            .filter(|c| c.generation == gen && !self.pruned_ids.contains(&c.id))
            .filter_map(|c| {
                scores
                    .iter()
                    .find(|(id, _)| id == &c.id)
                    .map(|(_, s)| (c.id.clone(), *s))
            })
            .collect();

        current_gen_ids.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // IDs to keep from current generation.
        let keep_ids: Vec<String> = current_gen_ids
            .iter()
            .take(self.config.beam_width)
            .map(|(id, _)| id.clone())
            .collect();

        // Collect IDs of current-gen candidates that didn't make the cut.
        let pruned_now: Vec<String> = current_gen_ids
            .iter()
            .skip(self.config.beam_width)
            .map(|(id, _)| id.clone())
            .collect();

        // Also find current-gen candidates that had no score entry.
        let unscored_pruned: Vec<String> = self
            .candidates
            .iter()
            .filter(|c| {
                c.generation == gen
                    && !self.pruned_ids.contains(&c.id)
                    && !keep_ids.contains(&c.id)
                    && !pruned_now.contains(&c.id)
            })
            .map(|c| c.id.clone())
            .collect();

        // Collect all active parent IDs referenced by kept candidates.
        let active_parent_ids: Vec<String> = self
            .candidates
            .iter()
            .filter(|c| {
                !self.pruned_ids.contains(&c.id)
                    && !pruned_now.contains(&c.id)
                    && !unscored_pruned.contains(&c.id)
            })
            .filter_map(|c| c.parent_id.clone())
            .collect();

        // Don't prune older candidates that are parents of active ones.
        let mut all_pruned = Vec::new();
        all_pruned.extend(pruned_now);
        all_pruned.extend(unscored_pruned);

        // Filter out any that are parents of still-active candidates.
        let all_pruned: Vec<String> = all_pruned
            .into_iter()
            .filter(|id| !active_parent_ids.contains(id))
            .collect();

        debug!(
            pruned = all_pruned.len(),
            kept = keep_ids.len(),
            generation = gen,
            "Pruned beam candidates"
        );

        self.pruned_ids.extend(all_pruned);
    }

    /// Return all candidates not in the pruned set.
    pub fn active_candidates(&self) -> Vec<&BeamCandidate> {
        self.candidates
            .iter()
            .filter(|c| !self.pruned_ids.contains(&c.id))
            .collect()
    }

    /// Whether the beam search has completed all generations.
    pub fn is_complete(&self) -> bool {
        self.current_generation >= self.config.max_generations as u32
    }
}

// ---------------------------------------------------------------------------
// Prompt construction helper
// ---------------------------------------------------------------------------

fn build_rewrite_prompt(
    parent_prompt: &str,
    failure_evidence: &str,
    rejected_prompts: &[(String, String)],
    thinking_style: &str,
) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are rewriting a prompt to improve its performance.\n\n\
         ## Current prompt\n\n```\n",
    );
    prompt.push_str(parent_prompt);
    prompt.push_str("\n```\n\n");

    prompt.push_str("## Failure evidence\n\n");
    prompt.push_str(failure_evidence);
    prompt.push_str("\n\n");

    if !rejected_prompts.is_empty() {
        prompt.push_str("## Previously rejected prompts (DO NOT repeat these approaches)\n\n");
        for (id, content) in rejected_prompts {
            prompt.push_str(&format!("### Rejected: {}\n```\n{}\n```\n\n", id, content));
        }
    }

    prompt.push_str(&format!("## Thinking style\n\n{}\n\n", thinking_style));

    prompt.push_str(
        "## Output format\n\n\
         Respond with exactly one rewrite block:\n\n\
         ```\n\
         [BEAM_REWRITE]\n\
         critique: |\n\
         \x20 <what's wrong with the parent>\n\
         changes_summary: |\n\
         \x20 <what you changed and why>\n\
         rewritten_prompt: |\n\
         \x20 <the complete rewritten prompt>\n\
         [/BEAM_REWRITE]\n\
         ```\n",
    );

    prompt
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a single `[BEAM_REWRITE]...[/BEAM_REWRITE]` block.
///
/// Returns `(critique, changes_summary, rewritten_prompt)`.
pub fn parse_beam_rewrite(text: &str) -> Option<(String, String, String)> {
    let start_tag = "[BEAM_REWRITE]";
    let end_tag = "[/BEAM_REWRITE]";

    let start = text.find(start_tag)?;
    let end = text.find(end_tag)?;
    if end <= start {
        return None;
    }

    let inner = &text[start + start_tag.len()..end];
    parse_beam_fields(inner)
}

/// Extract ALL `[BEAM_REWRITE]` blocks from text.
pub fn extract_beam_rewrite_blocks(text: &str) -> Vec<(String, String, String)> {
    let start_tag = "[BEAM_REWRITE]";
    let end_tag = "[/BEAM_REWRITE]";
    let mut results = Vec::new();
    let mut search_from = 0;

    while search_from < text.len() {
        let start = match text[search_from..].find(start_tag) {
            Some(pos) => search_from + pos,
            None => break,
        };
        let after_start = start + start_tag.len();
        let end = match text[after_start..].find(end_tag) {
            Some(pos) => after_start + pos,
            None => break,
        };

        let inner = &text[after_start..end];
        if let Some(parsed) = parse_beam_fields(inner) {
            results.push(parsed);
        }

        search_from = end + end_tag.len();
    }

    results
}

/// Parse the three YAML-ish fields from inside a BEAM_REWRITE block.
fn parse_beam_fields(inner: &str) -> Option<(String, String, String)> {
    let critique = extract_field(inner, "critique:")?;
    let changes_summary = extract_field(inner, "changes_summary:")?;
    let rewritten_prompt = extract_field(inner, "rewritten_prompt:")?;
    Some((critique, changes_summary, rewritten_prompt))
}

/// Extract a YAML-like `key: |` multiline field value using line-anchored parsing.
///
/// Searches for the key at the start of a line (after optional whitespace) to avoid
/// matching keys that appear inside field values (e.g., a rewritten_prompt that
/// contains "critique:" in its text).
fn extract_field(text: &str, key: &str) -> Option<String> {
    // Find key anchored at the start of a line
    let key_pos = find_line_anchored(text, key)?;
    let after_key = &text[key_pos + key.len()..];

    // The next field boundary is the next line-anchored key occurrence
    let next_keys = ["critique:", "changes_summary:", "rewritten_prompt:"];
    let field_end = next_keys
        .iter()
        .filter(|&&k| k != key)
        .filter_map(|k| {
            find_line_anchored(&text[key_pos + key.len()..], k).map(|pos| key_pos + key.len() + pos)
        })
        .min();

    let value_region = match field_end {
        Some(end) => &text[key_pos + key.len()..end],
        None => after_key,
    };

    // Strip the optional `|` pipe character and trim.
    let value = value_region.trim();
    let value = if value.starts_with('|') {
        value[1..].trim()
    } else {
        value
    };

    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Find a key that appears at the start of a line (after optional whitespace).
/// Returns the byte offset of the key within `text`.
fn find_line_anchored(text: &str, key: &str) -> Option<usize> {
    for (line_start, line) in line_offsets(text) {
        let trimmed_offset = line.len() - line.trim_start().len();
        if line.trim_start().starts_with(key) {
            return Some(line_start + trimmed_offset);
        }
    }
    None
}

/// Yields (byte_offset, line_content) for each line in text.
fn line_offsets(text: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        result.push((offset, line));
        offset += line.len() + 1; // +1 for the \n
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beam_config_default() {
        let cfg = BeamSearchConfig::default();
        assert_eq!(cfg.beam_width, 4);
        assert_eq!(cfg.branch_factor, 2);
        assert_eq!(cfg.max_generations, 3);
        assert_eq!(cfg.thinking_styles.len(), 4);
        assert!(cfg.thinking_styles[0].contains("Evidence-driven"));
        assert!(cfg.thinking_styles[1].contains("Structural clarity"));
        assert!(cfg.thinking_styles[2].contains("Constraint-tightening"));
        assert!(cfg.thinking_styles[3].contains("Few-shot exemplar"));
    }

    #[test]
    fn test_beam_state_new_has_seed() {
        let state =
            BeamSearchState::new(BeamSearchConfig::default(), "planner", "You are a planner.");
        assert_eq!(state.candidates.len(), 1);
        assert_eq!(state.current_generation, 0);
        assert!(state.candidates[0].id.starts_with("beam-seed-"));
        assert_eq!(state.candidates[0].prompt_content, "You are a planner.");
        assert_eq!(state.candidates[0].thinking_style, "seed");
        assert!(state.candidates[0].parent_id.is_none());
        assert_eq!(state.candidates[0].generation, 0);
    }

    #[test]
    fn test_prepare_generation_prompts() {
        let config = BeamSearchConfig {
            beam_width: 2,
            branch_factor: 2,
            ..Default::default()
        };
        let mut state = BeamSearchState::new(config, "planner", "You are a planner.");

        // Add a second candidate so we have 2 parents.
        state.candidates.push(BeamCandidate {
            id: "beam-extra".to_string(),
            prompt_content: "You are a better planner.".to_string(),
            critique: String::new(),
            changes_summary: String::new(),
            parent_id: None,
            generation: 0,
            thinking_style: "seed".to_string(),
        });

        let scores = vec![
            (state.candidates[0].id.clone(), 0.8),
            ("beam-extra".to_string(), 0.6),
        ];

        let requests =
            state.prepare_generation_prompts(&scores, "Failed to produce JSON output", &[]);

        // 2 parents x 2 branch_factor = 4 requests
        assert_eq!(requests.len(), 4);
        // Each request should reference a valid parent.
        for req in &requests {
            assert!(req.parent_id == state.candidates[0].id || req.parent_id == "beam-extra");
            assert!(!req.llm_prompt.is_empty());
            assert!(req.llm_prompt.contains("BEAM_REWRITE"));
            assert!(req.llm_prompt.contains("Failed to produce JSON output"));
        }
    }

    #[test]
    fn test_process_generation_responses() {
        let config = BeamSearchConfig::default();
        let mut state = BeamSearchState::new(config, "planner", "You are a planner.");
        let parent_id = state.candidates[0].id.clone();

        let response = "\
Here is my rewrite:

[BEAM_REWRITE]
critique: |
  The prompt lacks structure and specific output format requirements.
changes_summary: |
  Added step-by-step instructions and JSON output format.
rewritten_prompt: |
  You are an expert planner. Always output valid JSON.
[/BEAM_REWRITE]
"
        .to_string();

        let new_candidates = state.process_generation_responses(vec![(
            parent_id.clone(),
            response,
            "Structural clarity".to_string(),
        )]);

        assert_eq!(new_candidates.len(), 1);
        assert_eq!(new_candidates[0].generation, 1);
        assert_eq!(new_candidates[0].parent_id, Some(parent_id));
        assert!(new_candidates[0].id.starts_with("beam-1-"));
        assert!(new_candidates[0].critique.contains("lacks structure"));
        assert!(new_candidates[0].changes_summary.contains("step-by-step"));
        assert!(new_candidates[0].prompt_content.contains("expert planner"));
        assert_eq!(state.current_generation, 1);
        // Total candidates: seed + 1 new
        assert_eq!(state.candidates.len(), 2);
    }

    #[test]
    fn test_prune_to_beam_width() {
        let config = BeamSearchConfig {
            beam_width: 3,
            ..Default::default()
        };
        let mut state = BeamSearchState::new(config, "planner", "seed");
        state.current_generation = 1;

        // Add 6 candidates at generation 1.
        for i in 0..6 {
            state.candidates.push(BeamCandidate {
                id: format!("cand-{}", i),
                prompt_content: format!("prompt {}", i),
                critique: String::new(),
                changes_summary: String::new(),
                parent_id: Some(state.candidates[0].id.clone()),
                generation: 1,
                thinking_style: "test".to_string(),
            });
        }

        let scores = vec![
            ("cand-0".to_string(), 0.9),
            ("cand-1".to_string(), 0.1),
            ("cand-2".to_string(), 0.7),
            ("cand-3".to_string(), 0.3),
            ("cand-4".to_string(), 0.8),
            ("cand-5".to_string(), 0.2),
        ];

        state.prune_to_beam_width(&scores);

        let active_ids: Vec<String> = state
            .active_candidates()
            .iter()
            .filter(|c| c.generation == 1)
            .map(|c| c.id.clone())
            .collect();

        // Top 3 by score: cand-0 (0.9), cand-4 (0.8), cand-2 (0.7)
        assert_eq!(active_ids.len(), 3);
        assert!(active_ids.contains(&"cand-0".to_string()));
        assert!(active_ids.contains(&"cand-4".to_string()));
        assert!(active_ids.contains(&"cand-2".to_string()));
        assert!(!active_ids.contains(&"cand-1".to_string()));
        assert!(!active_ids.contains(&"cand-3".to_string()));
        assert!(!active_ids.contains(&"cand-5".to_string()));

        // The seed (generation 0) should still be active as a parent.
        let seed_active = state.active_candidates().iter().any(|c| c.generation == 0);
        assert!(seed_active);
    }

    #[test]
    fn test_is_complete() {
        let config = BeamSearchConfig {
            max_generations: 3,
            ..Default::default()
        };
        let mut state = BeamSearchState::new(config, "planner", "seed");
        assert!(!state.is_complete());

        state.current_generation = 2;
        assert!(!state.is_complete());

        state.current_generation = 3;
        assert!(state.is_complete());

        state.current_generation = 5;
        assert!(state.is_complete());
    }

    #[test]
    fn test_parse_beam_rewrite() {
        let text = r#"
Some preamble text.
[BEAM_REWRITE]
critique: |
  The prompt is too vague.
changes_summary: |
  Made it more specific with constraints.
rewritten_prompt: |
  You are a precise planner. Always output JSON.
[/BEAM_REWRITE]
Trailing text.
"#;
        let result = parse_beam_rewrite(text);
        assert!(result.is_some());
        let (critique, changes, prompt) = result.unwrap();
        assert!(critique.contains("too vague"));
        assert!(changes.contains("more specific"));
        assert!(prompt.contains("precise planner"));
    }

    #[test]
    fn test_parse_beam_rewrite_invalid() {
        assert!(parse_beam_rewrite("no tags here").is_none());
        assert!(parse_beam_rewrite("[BEAM_REWRITE] no end tag").is_none());
        assert!(parse_beam_rewrite("[/BEAM_REWRITE][BEAM_REWRITE]").is_none());
        assert!(parse_beam_rewrite("[BEAM_REWRITE]\njunk\n[/BEAM_REWRITE]").is_none());
    }
}
