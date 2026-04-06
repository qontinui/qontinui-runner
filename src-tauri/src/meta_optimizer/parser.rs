//! Parse structured recommendation markers from meta-optimizer AI output.
//!
//! Each optimizer type emits markers in its output:
//! - `[PROMPT_RECOMMENDATION]...[/PROMPT_RECOMMENDATION]` — pipeline_prompt_optimizer
//! - `[ARCH_RECOMMENDATION]...[/ARCH_RECOMMENDATION]` — architecture_optimizer
//! - `[CONFIG_RECOMMENDATION]...[/CONFIG_RECOMMENDATION]` — architecture_optimizer
//! - `[RULE_RECOMMENDATION]...[/RULE_RECOMMENDATION]` — generation_template_optimizer
//!
//! This module extracts these markers, parses key-value pairs inside, and saves
//! them as `meta_optimizer_recommendations` records via `recommendations::create_recommendation`.

use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

use super::recommendations;
use crate::database::pg::PgDb;

// ── Parsed types ────────────────────────────────────────────────────────

/// A parsed prompt recommendation from pipeline_prompt_optimizer output.
#[derive(Debug, Clone)]
pub struct ParsedPromptRecommendation {
    pub agent_type: String,
    pub variant_name: String,
    pub confidence: f64,
    pub rationale: String,
    pub prompt_content: String,
}

/// A parsed architecture recommendation from architecture_optimizer output.
#[derive(Debug, Clone)]
pub struct ParsedArchRecommendation {
    pub workflow_category: String,
    pub recommended_architecture: String,
    pub current_architecture: String,
    pub confidence: f64,
    pub rationale: String,
    pub evidence: String,
}

/// A parsed config recommendation from architecture_optimizer output.
#[derive(Debug, Clone)]
pub struct ParsedConfigRecommendation {
    pub architecture: String,
    pub parameter: String,
    pub current_value: String,
    pub recommended_value: String,
    pub confidence: f64,
    pub rationale: String,
    pub expected_impact: String,
}

/// A parsed advisory finding from architecture_optimizer output.
/// These represent insights that require code changes, not config changes.
#[derive(Debug, Clone)]
pub struct ParsedArchFinding {
    pub category: String,
    pub title: String,
    pub severity: String,
    pub finding: String,
    pub evidence: String,
    pub suggested_action: String,
}

/// A parsed rule recommendation from generation_template_optimizer output.
#[derive(Debug, Clone)]
pub struct ParsedRuleRecommendation {
    pub action: String,
    pub agent: String,
    pub section: String,
    pub rule_id: Option<String>,
    pub title: String,
    pub content: String,
    pub confidence: f64,
    pub rationale: String,
}

/// A parsed rule example from generation_template_optimizer output.
/// These attach positive/negative examples to existing rules (PromptWizard-inspired).
#[derive(Debug, Clone)]
pub struct ParsedRuleExample {
    pub rule_id: String,
    pub positive_example: String,
    pub negative_example: String,
    pub positive_explanation: String,
    pub negative_explanation: String,
}

/// A parsed meta-prompt rewrite from meta_prompt_optimizer output.
#[derive(Debug, Clone)]
pub struct ParsedMetaPromptRewrite {
    pub target_phase: String,
    pub target_agent: String,
    pub critique: String,
    pub weaknesses: String,
    pub strengths_to_preserve: String,
    pub changes_summary: String,
    pub expected_improvement: String,
    pub confidence: f64,
    pub rewritten_prompt: String,
}

// ── Marker extraction ───────────────────────────────────────────────────

/// Extract all blocks between `[TAG]` and `[/TAG]` from `output`.
fn extract_marker_blocks(output: &str, tag: &str) -> Vec<String> {
    let open = format!("[{}]", tag);
    let close = format!("[/{}]", tag);
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(start) = output[search_from..].find(&open) {
        let abs_start = search_from + start + open.len();
        if let Some(end) = output[abs_start..].find(&close) {
            let abs_end = abs_start + end;
            blocks.push(output[abs_start..abs_end].to_string());
            search_from = abs_end + close.len();
        } else {
            break;
        }
    }

    blocks
}

/// Parse key-value pairs from a marker block.
///
/// Supports:
/// - `key: value` (single line)
/// - `key: |` followed by indented multiline content
fn parse_key_value_pairs(block: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if let Some(colon_pos) = trimmed.find(": ") {
            let key = trimmed[..colon_pos].trim().to_string();
            let value_part = trimmed[colon_pos + 2..].trim();

            if value_part == "|" {
                // Multiline value: collect indented lines
                let mut multiline = Vec::new();
                i += 1;
                while i < lines.len() {
                    let next_line = lines[i];
                    let next_trimmed = next_line.trim();
                    // Stop when we hit another key-value pair at root indentation level
                    // or an empty line followed by a key-value pair
                    if !next_trimmed.is_empty()
                        && !next_line.starts_with(' ')
                        && !next_line.starts_with('\t')
                        && next_trimmed.contains(": ")
                    {
                        break;
                    }
                    // Dedent: remove up to 2 leading spaces
                    let dedented = next_line.strip_prefix("  ").unwrap_or(next_line);
                    multiline.push(dedented);
                    i += 1;
                }
                let value = multiline.join("\n").trim().to_string();
                map.insert(key, value);
            } else {
                map.insert(key, value_part.to_string());
                i += 1;
            }
        } else if let Some(colon_pos) = trimmed.find(':') {
            // Handle `key:` with no value (treat as empty string)
            let key = trimmed[..colon_pos].trim().to_string();
            let value_part = trimmed[colon_pos + 1..].trim();
            if value_part.is_empty() {
                map.insert(key, String::new());
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    map
}

fn get_str(map: &std::collections::HashMap<String, String>, key: &str) -> String {
    map.get(key).cloned().unwrap_or_default()
}

fn get_f64(map: &std::collections::HashMap<String, String>, key: &str) -> f64 {
    map.get(key)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.5)
}

fn get_opt(map: &std::collections::HashMap<String, String>, key: &str) -> Option<String> {
    map.get(key).filter(|v| !v.is_empty()).cloned()
}

// ── Public parsers ──────────────────────────────────────────────────────

/// Parse `[PROMPT_RECOMMENDATION]` blocks from pipeline_prompt_optimizer output.
pub fn parse_prompt_recommendations(output: &str) -> Vec<ParsedPromptRecommendation> {
    extract_marker_blocks(output, "PROMPT_RECOMMENDATION")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let agent_type = get_str(&kv, "agent_type");
            let variant_name = get_str(&kv, "variant_name");
            if agent_type.is_empty() || variant_name.is_empty() {
                warn!("Skipping PROMPT_RECOMMENDATION with missing agent_type or variant_name");
                return None;
            }
            Some(ParsedPromptRecommendation {
                agent_type,
                variant_name,
                confidence: get_f64(&kv, "confidence"),
                rationale: get_str(&kv, "rationale"),
                prompt_content: get_str(&kv, "prompt_content"),
            })
        })
        .collect()
}

/// Parse `[ARCH_RECOMMENDATION]` blocks from architecture_optimizer output.
pub fn parse_arch_recommendations(output: &str) -> Vec<ParsedArchRecommendation> {
    extract_marker_blocks(output, "ARCH_RECOMMENDATION")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let recommended = get_str(&kv, "recommended_architecture");
            if recommended.is_empty() {
                warn!("Skipping ARCH_RECOMMENDATION with missing recommended_architecture");
                return None;
            }
            Some(ParsedArchRecommendation {
                workflow_category: get_str(&kv, "workflow_category"),
                recommended_architecture: recommended,
                current_architecture: get_str(&kv, "current_architecture"),
                confidence: get_f64(&kv, "confidence"),
                rationale: get_str(&kv, "rationale"),
                evidence: get_str(&kv, "evidence"),
            })
        })
        .collect()
}

/// Parse `[CONFIG_RECOMMENDATION]` blocks from architecture_optimizer output.
pub fn parse_config_recommendations(output: &str) -> Vec<ParsedConfigRecommendation> {
    extract_marker_blocks(output, "CONFIG_RECOMMENDATION")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let parameter = get_str(&kv, "parameter");
            if parameter.is_empty() {
                warn!("Skipping CONFIG_RECOMMENDATION with missing parameter");
                return None;
            }
            Some(ParsedConfigRecommendation {
                architecture: get_str(&kv, "architecture"),
                parameter,
                current_value: get_str(&kv, "current_value"),
                recommended_value: get_str(&kv, "recommended_value"),
                confidence: get_f64(&kv, "confidence"),
                rationale: get_str(&kv, "rationale"),
                expected_impact: get_str(&kv, "expected_impact"),
            })
        })
        .collect()
}

/// Parse `[RULE_RECOMMENDATION]` blocks from generation_template_optimizer output.
pub fn parse_rule_recommendations(output: &str) -> Vec<ParsedRuleRecommendation> {
    extract_marker_blocks(output, "RULE_RECOMMENDATION")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let action = get_str(&kv, "action");
            let agent = get_str(&kv, "agent");
            if action.is_empty() || agent.is_empty() {
                warn!("Skipping RULE_RECOMMENDATION with missing action or agent");
                return None;
            }
            Some(ParsedRuleRecommendation {
                action,
                agent,
                section: get_str(&kv, "section"),
                rule_id: get_opt(&kv, "rule_id"),
                title: get_str(&kv, "title"),
                content: get_str(&kv, "content"),
                confidence: get_f64(&kv, "confidence"),
                rationale: get_str(&kv, "rationale"),
            })
        })
        .collect()
}

/// Parse `[ARCH_FINDING]` blocks from architecture_optimizer output.
pub fn parse_arch_findings(output: &str) -> Vec<ParsedArchFinding> {
    extract_marker_blocks(output, "ARCH_FINDING")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let title = get_str(&kv, "title");
            let finding = get_str(&kv, "finding");
            if title.is_empty() || finding.is_empty() {
                warn!("Skipping ARCH_FINDING with missing title or finding");
                return None;
            }
            Some(ParsedArchFinding {
                category: get_str(&kv, "category"),
                title,
                severity: get_str(&kv, "severity"),
                finding,
                evidence: get_str(&kv, "evidence"),
                suggested_action: get_str(&kv, "suggested_action"),
            })
        })
        .collect()
}

/// Parse `[RULE_EXAMPLE]` blocks from generation_template_optimizer output.
///
/// These attach positive/negative synthetic examples to existing rules,
/// inspired by PromptWizard's tandem instruction+examples optimization.
pub fn parse_rule_examples(output: &str) -> Vec<ParsedRuleExample> {
    extract_marker_blocks(output, "RULE_EXAMPLE")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let rule_id = get_str(&kv, "rule_id");
            if rule_id.is_empty() {
                warn!("Skipping RULE_EXAMPLE with missing rule_id");
                return None;
            }
            let positive = get_str(&kv, "positive_example");
            let negative = get_str(&kv, "negative_example");
            if positive.is_empty() && negative.is_empty() {
                warn!(
                    "Skipping RULE_EXAMPLE with no examples for rule {}",
                    rule_id
                );
                return None;
            }
            Some(ParsedRuleExample {
                rule_id,
                positive_example: positive,
                negative_example: negative,
                positive_explanation: get_str(&kv, "positive_explanation"),
                negative_explanation: get_str(&kv, "negative_explanation"),
            })
        })
        .collect()
}

/// Parse `[META_PROMPT_REWRITE]` blocks from meta_prompt_optimizer output.
pub fn parse_meta_prompt_rewrites(output: &str) -> Vec<ParsedMetaPromptRewrite> {
    extract_marker_blocks(output, "META_PROMPT_REWRITE")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let target_agent = get_str(&kv, "target_agent");
            let rewritten_prompt = get_str(&kv, "rewritten_prompt");
            if target_agent.is_empty() || rewritten_prompt.is_empty() {
                warn!("Skipping META_PROMPT_REWRITE with missing target_agent or rewritten_prompt");
                return None;
            }
            Some(ParsedMetaPromptRewrite {
                target_phase: get_str(&kv, "target_phase"),
                target_agent,
                critique: get_str(&kv, "critique"),
                weaknesses: get_str(&kv, "weaknesses"),
                strengths_to_preserve: get_str(&kv, "strengths_to_preserve"),
                changes_summary: get_str(&kv, "changes_summary"),
                expected_improvement: get_str(&kv, "expected_improvement"),
                confidence: get_f64(&kv, "confidence"),
                rewritten_prompt,
            })
        })
        .collect()
}

/// A parsed beam search rewrite candidate from beam_search output.
#[derive(Debug, Clone)]
pub struct ParsedBeamCandidate {
    pub critique: String,
    pub changes_summary: String,
    pub rewritten_prompt: String,
}

/// Parse `[BEAM_REWRITE]` blocks from beam search LLM output.
///
/// Returns all valid blocks found (there may be multiple per response
/// when branch_factor > 1 and the LLM outputs multiple rewrites).
pub fn parse_beam_candidates(output: &str) -> Vec<ParsedBeamCandidate> {
    extract_marker_blocks(output, "BEAM_REWRITE")
        .into_iter()
        .filter_map(|block| {
            let kv = parse_key_value_pairs(&block);
            let rewritten_prompt = get_str(&kv, "rewritten_prompt");
            if rewritten_prompt.is_empty() {
                warn!("Skipping BEAM_REWRITE with missing rewritten_prompt");
                return None;
            }
            Some(ParsedBeamCandidate {
                critique: get_str(&kv, "critique"),
                changes_summary: get_str(&kv, "changes_summary"),
                rewritten_prompt,
            })
        })
        .collect()
}

// ── Deduplication helper ─────────────────────────────────────────────────

/// Check if a recommendation with the same content already exists in a non-terminal state.
///
/// Uses content-hash based dedup (optimizer_type + recommendation_type + target_agent +
/// recommended_value) when the content_hash column is populated, falling back to exact
/// title match for older rows without hashes.
fn is_duplicate_recommendation(
    pg_db: &Arc<PgDb>,
    title: &str,
    optimizer_type: &str,
    recommendation_type: &str,
    target_agent: Option<&str>,
    recommended_value: Option<&str>,
) -> bool {
    let hash = super::recommendations::compute_content_hash(
        optimizer_type,
        recommendation_type,
        target_agent,
        recommended_value,
    );
    // Check content hash first (covers pending, canary, applied)
    if super::recommendations::is_content_duplicate(pg_db, &hash) {
        return true;
    }
    // Fallback: title match for rejected recs (rejected can't be re-created with same title)
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.count_rejected_by_title(title))
    })
    .map(|c| c > 0)
    .unwrap_or(false)
}

// ── Save orchestrator ───────────────────────────────────────────────────

/// Parse recommendations from AI output and save them to the database.
///
/// `optimizer_type` should be one of: `"pipeline_prompt"`, `"architecture"`, `"generation_template"`, `"meta_prompt"`.
///
/// Returns the number of recommendations created.
pub fn save_parsed_recommendations(
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    optimizer_run_id: Option<&str>,
    output: &str,
) -> Result<usize, String> {
    let mut count = 0;

    match optimizer_type {
        "pipeline_prompt" => {
            let recs = parse_prompt_recommendations(output);
            info!("Parsed {} PROMPT_RECOMMENDATION(s) from output", recs.len());
            for rec in &recs {
                let title = format!(
                    "Prompt variant '{}' for {}",
                    rec.variant_name, rec.agent_type
                );
                let description = if rec.rationale.is_empty() {
                    format!(
                        "New prompt variant '{}' for agent '{}'",
                        rec.variant_name, rec.agent_type
                    )
                } else {
                    rec.rationale.clone()
                };

                if rec.confidence < 0.45 {
                    debug!(
                        "Skipping low-confidence recommendation ({:.0}%): {}",
                        rec.confidence * 100.0,
                        title
                    );
                    continue;
                }

                // Serialize as JSON payload matching PromptRewritePayload
                let recommended_value = serde_json::json!({
                    "agent_type": rec.agent_type,
                    "variant_name": rec.variant_name,
                    "prompt_content": rec.prompt_content,
                })
                .to_string();

                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    "prompt_rewrite",
                    Some(&rec.agent_type),
                    Some(&recommended_value),
                ) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

                recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    "prompt_rewrite",
                    Some(&rec.agent_type),
                    &title,
                    &description,
                    None,
                    Some(&recommended_value),
                    None,
                    rec.confidence,
                    optimizer_run_id,
                )?;
                count += 1;
            }
        }

        "architecture" => {
            // Architecture recommendations
            let arch_recs = parse_arch_recommendations(output);
            info!(
                "Parsed {} ARCH_RECOMMENDATION(s) from output",
                arch_recs.len()
            );
            for rec in &arch_recs {
                let title = format!(
                    "Switch to {} for {} workflows",
                    rec.recommended_architecture, rec.workflow_category
                );

                if rec.confidence < 0.45 {
                    debug!(
                        "Skipping low-confidence recommendation ({:.0}%): {}",
                        rec.confidence * 100.0,
                        title
                    );
                    continue;
                }

                let description = if rec.rationale.is_empty() {
                    format!(
                        "Recommend {} architecture (currently {})",
                        rec.recommended_architecture, rec.current_architecture
                    )
                } else {
                    rec.rationale.clone()
                };

                // Serialize as JSON payload matching ConfigChangePayload
                let current_value = serde_json::json!({
                    "key": format!("architecture.{}", rec.workflow_category),
                    "value": rec.current_architecture,
                })
                .to_string();
                let recommended_value = serde_json::json!({
                    "key": format!("architecture.{}", rec.workflow_category),
                    "value": rec.recommended_architecture,
                })
                .to_string();

                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    "config_change",
                    None,
                    Some(&recommended_value),
                ) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

                recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    "config_change",
                    None,
                    &title,
                    &description,
                    Some(&current_value),
                    Some(&recommended_value),
                    if rec.evidence.is_empty() {
                        None
                    } else {
                        Some(&rec.evidence)
                    },
                    rec.confidence,
                    optimizer_run_id,
                )?;
                count += 1;
            }

            // Config recommendations (also from architecture optimizer)
            let config_recs = parse_config_recommendations(output);
            info!(
                "Parsed {} CONFIG_RECOMMENDATION(s) from output",
                config_recs.len()
            );
            for rec in &config_recs {
                let title = format!("Tune {} for {}", rec.parameter, rec.architecture);

                if rec.confidence < 0.45 {
                    debug!(
                        "Skipping low-confidence recommendation ({:.0}%): {}",
                        rec.confidence * 100.0,
                        title
                    );
                    continue;
                }

                let description = if rec.rationale.is_empty() {
                    format!(
                        "Change {} from {} to {}",
                        rec.parameter, rec.current_value, rec.recommended_value
                    )
                } else {
                    rec.rationale.clone()
                };

                // Serialize as JSON payload matching ConfigChangePayload
                let config_key = format!("{}.{}", rec.architecture, rec.parameter);
                let current_value = serde_json::json!({
                    "key": config_key,
                    "value": rec.current_value,
                })
                .to_string();
                let recommended_value = serde_json::json!({
                    "key": config_key,
                    "value": rec.recommended_value,
                })
                .to_string();

                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    "config_change",
                    None,
                    Some(&recommended_value),
                ) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

                recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    "config_change",
                    None,
                    &title,
                    &description,
                    Some(&current_value),
                    Some(&recommended_value),
                    if rec.expected_impact.is_empty() {
                        None
                    } else {
                        Some(&rec.expected_impact)
                    },
                    rec.confidence,
                    optimizer_run_id,
                )?;
                count += 1;
            }

            // Parse advisory findings
            let findings = parse_arch_findings(output);
            info!("Parsed {} ARCH_FINDING(s) from output", findings.len());
            for finding in &findings {
                let title = finding.title.clone();
                let metadata = serde_json::json!({
                    "category": finding.category,
                    "severity": finding.severity,
                    "suggested_action": finding.suggested_action,
                })
                .to_string();
                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    "finding",
                    None,
                    Some(&metadata),
                ) {
                    debug!("Skipping duplicate finding: {}", title);
                    continue;
                }
                recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    "finding", // Not actionable as config_change
                    None,
                    &title,
                    &finding.finding,
                    None,
                    Some(&metadata),
                    if finding.evidence.is_empty() {
                        None
                    } else {
                        Some(&finding.evidence)
                    },
                    0.0, // Findings don't have confidence — they're observations
                    optimizer_run_id,
                )?;
                count += 1;
            }
        }

        "generation_template" => {
            let recs = parse_rule_recommendations(output);
            info!("Parsed {} RULE_RECOMMENDATION(s) from output", recs.len());
            for rec in &recs {
                let title = if rec.title.is_empty() {
                    format!("{} rule for {} ({})", rec.action, rec.agent, rec.section)
                } else {
                    rec.title.clone()
                };

                if rec.confidence < 0.45 {
                    debug!(
                        "Skipping low-confidence recommendation ({:.0}%): {}",
                        rec.confidence * 100.0,
                        title
                    );
                    continue;
                }

                let description = if rec.rationale.is_empty() {
                    format!(
                        "{} rule in {}.{}: {}",
                        rec.action, rec.agent, rec.section, rec.content
                    )
                } else {
                    rec.rationale.clone()
                };

                // Serialize as JSON payload matching RulePayload
                let status_override = if rec.action == "disable" {
                    Some("disabled")
                } else {
                    None
                };
                let recommended_value = serde_json::json!({
                    "agent": rec.agent,
                    "section": rec.section,
                    "title": rec.title,
                    "content": rec.content,
                    "rule_id": rec.rule_id,
                    "status": status_override,
                })
                .to_string();

                let rec_type = match rec.action.as_str() {
                    "create" => "rule_create",
                    "update" => "rule_update",
                    "disable" => "rule_update",
                    _ => "rule_create",
                };

                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    rec_type,
                    Some(&rec.agent),
                    Some(&recommended_value),
                ) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

                recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    rec_type,
                    Some(&rec.agent),
                    &title,
                    &description,
                    rec.rule_id.as_deref(),
                    Some(&recommended_value),
                    None,
                    rec.confidence,
                    optimizer_run_id,
                )?;
                count += 1;
            }

            // Parse and save synthetic examples for existing rules (PromptWizard-inspired)
            let examples = parse_rule_examples(output);
            if !examples.is_empty() {
                info!(
                    "Parsed {} RULE_EXAMPLE(s) from output — attaching to existing rules",
                    examples.len()
                );
                save_rule_examples(pg_db, &examples);
            }
        }

        "meta_prompt" => {
            let rewrites = parse_meta_prompt_rewrites(output);
            info!(
                "Parsed {} META_PROMPT_REWRITE(s) from output",
                rewrites.len()
            );
            for rec in &rewrites {
                if rec.confidence < 0.45 {
                    debug!(
                        "Skipping low-confidence meta-prompt rewrite ({:.0}%): {}",
                        rec.confidence * 100.0,
                        rec.target_agent
                    );
                    continue;
                }

                let title = format!(
                    "Meta-prompt rewrite for {}/{}",
                    rec.target_phase, rec.target_agent
                );
                let description = if rec.changes_summary.is_empty() {
                    rec.critique.clone()
                } else {
                    rec.changes_summary.clone()
                };

                // Store the rewritten prompt as a prompt variant
                let variant_name = format!("meta_rewrite_v{}", chrono::Utc::now().timestamp());

                let recommended_value = serde_json::json!({
                    "agent_type": rec.target_agent,
                    "variant_name": variant_name,
                    "prompt_content": rec.rewritten_prompt,
                })
                .to_string();

                if is_duplicate_recommendation(
                    pg_db,
                    &title,
                    optimizer_type,
                    "prompt_rewrite",
                    Some(&rec.target_agent),
                    Some(&recommended_value),
                ) {
                    debug!("Skipping duplicate meta-prompt rewrite: {}", title);
                    continue;
                }

                // Semantic similarity guard: reject near-duplicate rewrites
                let rejected_prompts =
                    super::prompt_evolution::get_rejected_prompt_contents(pg_db, &rec.target_agent)
                        .unwrap_or_default();

                let mut too_similar = false;
                for (rejected_variant_id, rejected_content) in &rejected_prompts {
                    let similarity = crate::reflection::fuzzy_matching::trigram_similarity(
                        &rec.rewritten_prompt,
                        rejected_content,
                    );
                    if similarity > 0.90 {
                        warn!(
                            "Meta-prompt rewrite for {} is too similar to previously rejected variant {} (similarity={:.2}). Skipping.",
                            rec.target_agent, rejected_variant_id, similarity
                        );
                        too_similar = true;
                        break;
                    }
                }
                if too_similar {
                    continue;
                }

                // Robustness validation gate: reject prompts that fail basic checks
                let robustness_warnings =
                    crate::workflow_generation::hardener::validate_prompt_robustness(
                        &rec.rewritten_prompt,
                    );
                if !robustness_warnings.is_empty() {
                    warn!(
                        "Meta-prompt rewrite for {} failed robustness validation ({} warnings): {:?}",
                        rec.target_agent,
                        robustness_warnings.len(),
                        robustness_warnings
                    );
                    // Allow prompts with only minor warnings (1-2), block those with many
                    if robustness_warnings.len() > 2 {
                        warn!(
                            "Skipping meta-prompt rewrite for {} — too many robustness warnings",
                            rec.target_agent
                        );
                        continue;
                    }
                }

                let evidence = serde_json::json!({
                    "critique": rec.critique,
                    "weaknesses": rec.weaknesses,
                    "strengths_to_preserve": rec.strengths_to_preserve,
                    "expected_improvement": rec.expected_improvement,
                    "robustness_warnings": robustness_warnings,
                })
                .to_string();

                // Create the recommendation
                let recommendation = recommendations::create_recommendation(
                    pg_db,
                    optimizer_type,
                    "prompt_rewrite",
                    Some(&rec.target_agent),
                    &title,
                    &description,
                    None,
                    Some(&recommended_value),
                    Some(&evidence),
                    rec.confidence,
                    optimizer_run_id,
                )?;

                // Create prompt variant immediately (inactive)
                let variant = super::prompt_registry::create_variant(
                    pg_db,
                    &rec.target_agent,
                    &variant_name,
                    &rec.rewritten_prompt,
                    Some(&recommendation.id),
                )?;

                // Get the current active variant as parent
                let parent_id = super::prompt_registry::get_active_prompt(pg_db, &rec.target_agent)
                    .ok()
                    .flatten()
                    .map(|v| v.id);

                // Get current mean score for this group
                let samples = super::prompt_extractor::extract_prompt_samples_pg(pg_db, 500)
                    .unwrap_or_default();
                let groups = super::prompt_extractor::compute_group_metrics(&samples);
                let score_before = groups
                    .iter()
                    .find(|g| g.agent_type == rec.target_agent)
                    .map(|g| g.mean_score);

                // Compute baseline prompt hash for drift detection
                let baseline_prompt =
                    super::prompt_extractor::get_default_agent_prompt(&rec.target_agent);
                let baseline_hash = if baseline_prompt.is_empty() {
                    None
                } else {
                    Some(super::prompt_evolution::compute_prompt_hash(
                        &baseline_prompt,
                    ))
                };

                // Record evolution entry with baseline hash
                let _ = super::prompt_evolution::record_evolution_full(
                    pg_db,
                    &rec.target_agent,
                    parent_id.as_deref(),
                    &variant.id,
                    Some(&recommendation.id),
                    Some(&rec.critique),
                    Some(&rec.changes_summary),
                    score_before,
                    baseline_hash.as_deref(),
                );

                // Auto-start canary for the new variant
                if rec.confidence >= 0.60 {
                    if let Err(e) = super::canary::start_canary(pg_db, &recommendation.id, 20) {
                        warn!(
                            "Failed to auto-start canary for meta-prompt rewrite {}: {}",
                            recommendation.id, e
                        );
                    } else {
                        info!(
                            "Auto-started canary at 20% for meta-prompt rewrite {}",
                            recommendation.id
                        );
                    }
                }

                count += 1;
            }
        }

        other => {
            debug!(
                "Unknown optimizer type '{}' — no recommendations parsed",
                other
            );
        }
    }

    if count > 0 {
        info!(
            "Saved {} recommendation(s) for optimizer type '{}'",
            count, optimizer_type
        );

        // Auto-apply high-confidence rule recommendations
        auto_apply_high_confidence(pg_db, optimizer_run_id);
    }

    Ok(count)
}

/// Auto-apply high-confidence rule recommendations and auto-canary prompt/config changes.
///
/// - `rule_create` and `rule_update` with confidence >= 0.85 are applied directly
///   (safest, most reversible).
/// - `prompt_rewrite` with confidence >= 0.75 are started as canary rollouts at 20%
///   so they can be evaluated before full promotion.
/// - `config_change` with confidence >= 0.75 are started as canary rollouts at 10%
///   (more conservative since config changes affect global behavior).
pub fn auto_apply_high_confidence(pg_db: &Arc<PgDb>, optimizer_run_id: Option<&str>) {
    // Auto-reject findings — they are diagnostic observations, not actionable changes
    let findings_rejected: i64 = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(pg_db.reject_finding_recommendations(optimizer_run_id))
    })
    .unwrap_or(0);

    if findings_rejected > 0 {
        info!(
            "Auto-rejected {} finding recommendation(s) (observations, not actionable)",
            findings_rejected
        );
    }

    // Auto-apply rule changes (safest, most reversible)
    let rule_candidates: Vec<(String, f64)> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_pending_recommendations_by_type(
            &["rule_create", "rule_update"],
            0.85,
            optimizer_run_id,
        ))
    })
    .unwrap_or_default();

    for (rec_id, confidence) in &rule_candidates {
        match super::recommendations::apply_recommendation_with_side_effects(pg_db, rec_id) {
            Ok(()) => {
                info!(
                    "Auto-applied high-confidence recommendation {} (confidence: {:.0}%)",
                    rec_id,
                    confidence * 100.0
                );
            }
            Err(e) => {
                warn!("Failed to auto-apply recommendation {}: {}", rec_id, e);
            }
        }
    }

    if !rule_candidates.is_empty() {
        info!(
            "Auto-applied {} high-confidence rule recommendation(s)",
            rule_candidates.len()
        );
    }

    // Auto-canary prompt rewrites with high confidence (>= 0.85)
    // These are started as canary rollouts at 20% rather than applied directly,
    // since prompt changes have broader impact and benefit from A/B evaluation.
    let prompt_candidates: Vec<(String, f64)> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_pending_recommendations_by_type(
            &["prompt_rewrite"],
            0.75,
            optimizer_run_id,
        ))
    })
    .unwrap_or_default();

    for (rec_id, confidence) in &prompt_candidates {
        match super::canary::start_canary(pg_db, rec_id, 20) {
            Ok(canary_id) => {
                info!(
                    "Auto-started canary {} for prompt recommendation {} (confidence: {:.0}%, rollout: 20%)",
                    canary_id, rec_id, confidence * 100.0
                );
            }
            Err(e) => {
                warn!(
                    "Failed to auto-start canary for prompt recommendation {}: {}",
                    rec_id, e
                );
            }
        }
    }

    if !prompt_candidates.is_empty() {
        info!(
            "Auto-started {} canary rollout(s) for high-confidence prompt recommendation(s)",
            prompt_candidates.len()
        );
    }

    // Auto-canary config changes with high confidence (>= 0.85) at conservative 10% rollout.
    // Config changes affect global behavior, so use lower rollout than prompt rewrites.
    let config_candidates: Vec<(String, f64)> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.query_pending_recommendations_by_type(
            &["config_change"],
            0.75,
            optimizer_run_id,
        ))
    })
    .unwrap_or_default();

    for (rec_id, confidence) in &config_candidates {
        match super::canary::start_canary(pg_db, rec_id, 10) {
            Ok(canary_id) => {
                info!(
                    "Auto-started canary {} for config recommendation {} (confidence: {:.0}%, rollout: 10%)",
                    canary_id, rec_id, confidence * 100.0
                );
            }
            Err(e) => {
                warn!(
                    "Failed to auto-start canary for config recommendation {}: {}",
                    rec_id, e
                );
            }
        }
    }

    if !config_candidates.is_empty() {
        info!(
            "Auto-started {} canary rollout(s) for high-confidence config recommendation(s)",
            config_candidates.len()
        );
    }
}

/// Save parsed rule examples to existing rules in the database.
///
/// Each `ParsedRuleExample` is converted to a JSON array entry and merged
/// into the target rule's `examples_json` column. Examples are additive and
/// low-risk, so they bypass the recommendation lifecycle.
fn save_rule_examples(pg_db: &Arc<PgDb>, examples: &[ParsedRuleExample]) {
    for ex in examples {
        let rule_id = &ex.rule_id;

        let result: Result<(), String> = (|| {
            // Check rule exists
            let exists = tokio::task::block_in_place(|| {
                Handle::current().block_on(pg_db.rule_exists(rule_id))
            })?;

            if !exists {
                warn!("RULE_EXAMPLE targets non-existent rule: {}", rule_id);
                return Ok(());
            }

            // Build examples array
            let mut new_examples = Vec::new();
            if !ex.positive_example.is_empty() {
                new_examples.push(serde_json::json!({
                    "type": "positive",
                    "output": ex.positive_example,
                    "explanation": ex.positive_explanation,
                }));
            }
            if !ex.negative_example.is_empty() {
                new_examples.push(serde_json::json!({
                    "type": "negative",
                    "output": ex.negative_example,
                    "explanation": ex.negative_explanation,
                }));
            }

            // Merge with existing examples (if any), capping at 4 total
            let existing_json = tokio::task::block_in_place(|| {
                Handle::current().block_on(pg_db.get_rule_examples_json(rule_id))
            })?;

            let mut all_examples: Vec<serde_json::Value> = existing_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();

            all_examples.extend(new_examples);
            all_examples.truncate(4);

            let merged_json = serde_json::to_string(&all_examples)
                .map_err(|e| format!("JSON serialization error: {}", e))?;

            tokio::task::block_in_place(|| {
                Handle::current().block_on(pg_db.update_rule_examples(rule_id, &merged_json))
            })?;

            info!(
                "Attached {} example(s) to rule {}",
                all_examples.len(),
                rule_id
            );
            Ok(())
        })();

        if let Err(e) = result {
            warn!("Failed to save rule example for {}: {}", ex.rule_id, e);
        }
    }
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Save parsed recommendations with PG dual-write (fire-and-forget).
#[deprecated(note = "Use save_parsed_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn save_parsed_recommendations_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    optimizer_type: &str,
    optimizer_run_id: Option<&str>,
    output: &str,
) -> Result<usize, String> {
    save_parsed_recommendations(pg_db, optimizer_type, optimizer_run_id, output)
}

/// Auto-apply high-confidence recommendations with PG dual-write.
#[deprecated(note = "Use auto_apply_high_confidence directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn auto_apply_high_confidence_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    optimizer_run_id: Option<&str>,
) {
    auto_apply_high_confidence(pg_db, optimizer_run_id);
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_marker_blocks_basic() {
        let output = r#"Some preamble text.
[PROMPT_RECOMMENDATION]
agent_type: spec_analyst
variant_name: clarity_v2
confidence: 0.8
rationale: Improves clarity
prompt_content: |
  You are a spec analyst.
  Be clear and precise.
[/PROMPT_RECOMMENDATION]
Some trailing text."#;

        let blocks = extract_marker_blocks(output, "PROMPT_RECOMMENDATION");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("agent_type: spec_analyst"));
    }

    #[test]
    fn test_extract_multiple_blocks() {
        let output = r#"
[ARCH_RECOMMENDATION]
recommended_architecture: agentic
[/ARCH_RECOMMENDATION]
Middle text.
[ARCH_RECOMMENDATION]
recommended_architecture: pipeline
[/ARCH_RECOMMENDATION]
"#;
        let blocks = extract_marker_blocks(output, "ARCH_RECOMMENDATION");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_parse_key_value_simple() {
        let block = r#"
agent_type: spec_analyst
variant_name: clarity_v2
confidence: 0.85
"#;
        let kv = parse_key_value_pairs(block);
        assert_eq!(kv.get("agent_type").unwrap(), "spec_analyst");
        assert_eq!(kv.get("variant_name").unwrap(), "clarity_v2");
        assert_eq!(kv.get("confidence").unwrap(), "0.85");
    }

    #[test]
    fn test_parse_key_value_multiline() {
        let block = r#"
agent_type: implementer
prompt_content: |
  Line one of prompt.
  Line two of prompt.
confidence: 0.9
"#;
        let kv = parse_key_value_pairs(block);
        assert_eq!(kv.get("agent_type").unwrap(), "implementer");
        assert_eq!(kv.get("confidence").unwrap(), "0.9");
        let content = kv.get("prompt_content").unwrap();
        assert!(content.contains("Line one of prompt."));
        assert!(content.contains("Line two of prompt."));
    }

    #[test]
    fn test_parse_prompt_recommendations() {
        let output = r#"Analysis complete.
[PROMPT_RECOMMENDATION]
agent_type: spec_analyst
variant_name: clarity_focused_v2
confidence: 0.8
rationale: Historical data shows improved spec quality
prompt_content: |
  You are a specification analyst.
  Focus on clarity and completeness.
[/PROMPT_RECOMMENDATION]
"#;
        let recs = parse_prompt_recommendations(output);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].agent_type, "spec_analyst");
        assert_eq!(recs[0].variant_name, "clarity_focused_v2");
        assert!((recs[0].confidence - 0.8).abs() < 0.01);
        assert!(recs[0].prompt_content.contains("specification analyst"));
    }

    #[test]
    fn test_parse_arch_recommendations() {
        let output = r#"
[ARCH_RECOMMENDATION]
workflow_category: all
recommended_architecture: agentic_verification
current_architecture: traditional
confidence: 0.7
rationale: Agentic approach yields better results
evidence: 80% of runs with agentic verification passed
[/ARCH_RECOMMENDATION]
"#;
        let recs = parse_arch_recommendations(output);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_architecture, "agentic_verification");
        assert_eq!(recs[0].current_architecture, "traditional");
    }

    #[test]
    fn test_parse_config_recommendations() {
        let output = r#"
[CONFIG_RECOMMENDATION]
architecture: multi_agent_pipeline
parameter: max_total_iterations
current_value: 10
recommended_value: 15
confidence: 0.6
rationale: More iterations improve completion rate
expected_impact: +12% completion rate
[/CONFIG_RECOMMENDATION]
"#;
        let recs = parse_config_recommendations(output);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].parameter, "max_total_iterations");
        assert_eq!(recs[0].current_value, "10");
        assert_eq!(recs[0].recommended_value, "15");
    }

    #[test]
    fn test_parse_rule_recommendations() {
        let output = r#"
[RULE_RECOMMENDATION]
action: create
agent: hardener
section: important_rules
title: Always validate inputs
content: |
  All inputs must be validated before processing.
  Use strict type checking.
confidence: 0.75
rationale: Reduces runtime errors by 30%
[/RULE_RECOMMENDATION]
"#;
        let recs = parse_rule_recommendations(output);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].action, "create");
        assert_eq!(recs[0].agent, "hardener");
        assert_eq!(recs[0].title, "Always validate inputs");
        assert!(recs[0].content.contains("validated before processing"));
        assert!(recs[0].rule_id.is_none());
    }

    #[test]
    fn test_parse_rule_with_rule_id() {
        let output = r#"
[RULE_RECOMMENDATION]
action: update
agent: verifier
section: important_rules
rule_id: rule-123
title: Updated check rule
content: New content
confidence: 0.6
rationale: Needs updating
[/RULE_RECOMMENDATION]
"#;
        let recs = parse_rule_recommendations(output);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].rule_id.as_deref(), Some("rule-123"));
    }

    #[test]
    fn test_skip_incomplete_recommendations() {
        // Missing required fields
        let output = r#"
[PROMPT_RECOMMENDATION]
confidence: 0.8
rationale: Missing agent_type and variant_name
[/PROMPT_RECOMMENDATION]
"#;
        let recs = parse_prompt_recommendations(output);
        assert_eq!(recs.len(), 0);
    }

    #[test]
    fn test_parse_arch_findings() {
        let output = r#"
[ARCH_FINDING]
category: iteration_tuning
title: Most failures have max_iterations=0
severity: critical
finding: 90% of failed runs used max_iterations=0, suggesting the iteration budget is exhausted
evidence: 45/50 failures had max_iterations=0; 5/50 had max_iterations>=3
suggested_action: Increase default max_iterations in LoopConfig from 5 to 8
[/ARCH_FINDING]
"#;
        let findings = parse_arch_findings(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "iteration_tuning");
        assert_eq!(findings[0].title, "Most failures have max_iterations=0");
        assert_eq!(findings[0].severity, "critical");
        assert!(findings[0].finding.contains("90%"));
        assert!(findings[0].evidence.contains("45/50"));
        assert!(findings[0].suggested_action.contains("LoopConfig"));
    }

    #[test]
    fn test_parse_arch_findings_missing_fields() {
        let output = r#"
[ARCH_FINDING]
category: data_gap
severity: informational
[/ARCH_FINDING]
"#;
        let findings = parse_arch_findings(output);
        assert_eq!(findings.len(), 0); // Missing title and finding
    }

    #[test]
    fn test_no_markers_returns_empty() {
        let output = "Just some regular text with no markers.";
        assert!(parse_prompt_recommendations(output).is_empty());
        assert!(parse_arch_recommendations(output).is_empty());
        assert!(parse_config_recommendations(output).is_empty());
        assert!(parse_rule_recommendations(output).is_empty());
        assert!(parse_arch_findings(output).is_empty());
        assert!(parse_rule_examples(output).is_empty());
    }

    #[test]
    fn test_parse_rule_examples() {
        let output = r#"
[RULE_EXAMPLE]
rule_id: rule-abc123
positive_example: |
  curl -s $URL | jq '.status'
negative_example: |
  curl -f $URL | python -c 'import sys; print(sys.stdin.read())'
positive_explanation: Uses jq for lightweight JSON parsing
negative_explanation: Pipes to python subprocess — fragile and slow
[/RULE_EXAMPLE]
"#;
        let examples = parse_rule_examples(output);
        assert_eq!(examples.len(), 1);
        assert_eq!(examples[0].rule_id, "rule-abc123");
        assert!(examples[0].positive_example.contains("jq"));
        assert!(examples[0].negative_example.contains("python"));
        assert!(examples[0].positive_explanation.contains("jq"));
        assert!(examples[0].negative_explanation.contains("fragile"));
    }

    #[test]
    fn test_parse_rule_examples_missing_rule_id() {
        let output = r#"
[RULE_EXAMPLE]
positive_example: some example
negative_example: bad example
[/RULE_EXAMPLE]
"#;
        let examples = parse_rule_examples(output);
        assert_eq!(examples.len(), 0); // Missing rule_id
    }

    // ── Meta-prompt rewrite parser tests ──────────────────────────────

    #[test]
    fn test_parse_meta_prompt_rewrite() {
        let output = r#"
Based on the analysis, the verifier prompt needs improvement.

[META_PROMPT_REWRITE]
target_phase: verification
target_agent: verifier
critique: |
  The current prompt lacks specific output format requirements.
  The agent often produces unstructured responses.
weaknesses: |
  - No JSON output format specification
  - Missing role definition
strengths_to_preserve: |
  - Good task decomposition instructions
  - Clear boundary constraints
changes_summary: |
  Added JSON output format requirement and explicit role definition.
expected_improvement: |
  Expect 15-20% reduction in failure rate from format compliance.
confidence: 0.72
rewritten_prompt: |
  You are a code verification expert. Your role is to verify code changes.
  Always respond in JSON format with keys: status, issues, suggestions.
  You must never execute arbitrary commands.
[/META_PROMPT_REWRITE]
"#;
        let rewrites = parse_meta_prompt_rewrites(output);
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].target_phase, "verification");
        assert_eq!(rewrites[0].target_agent, "verifier");
        assert!(rewrites[0].critique.contains("output format"));
        assert!(rewrites[0].weaknesses.contains("JSON"));
        assert!(rewrites[0].strengths_to_preserve.contains("decomposition"));
        assert!(rewrites[0].changes_summary.contains("JSON output format"));
        assert!((rewrites[0].confidence - 0.72).abs() < 0.01);
        assert!(rewrites[0].rewritten_prompt.contains("verification expert"));
    }

    #[test]
    fn test_parse_meta_prompt_rewrite_missing_required_fields() {
        // Missing target_agent
        let output = r#"
[META_PROMPT_REWRITE]
target_phase: generation
rewritten_prompt: |
  Some prompt text
[/META_PROMPT_REWRITE]
"#;
        let rewrites = parse_meta_prompt_rewrites(output);
        assert_eq!(rewrites.len(), 0);

        // Missing rewritten_prompt
        let output2 = r#"
[META_PROMPT_REWRITE]
target_agent: implementer
critique: The prompt is bad
[/META_PROMPT_REWRITE]
"#;
        let rewrites2 = parse_meta_prompt_rewrites(output2);
        assert_eq!(rewrites2.len(), 0);
    }

    #[test]
    fn test_parse_meta_prompt_rewrite_multiple() {
        let output = r#"
[META_PROMPT_REWRITE]
target_phase: generation
target_agent: implementer
confidence: 0.65
rewritten_prompt: |
  First rewrite
[/META_PROMPT_REWRITE]

[META_PROMPT_REWRITE]
target_phase: verification
target_agent: verifier
confidence: 0.80
rewritten_prompt: |
  Second rewrite
[/META_PROMPT_REWRITE]
"#;
        let rewrites = parse_meta_prompt_rewrites(output);
        assert_eq!(rewrites.len(), 2);
        assert_eq!(rewrites[0].target_agent, "implementer");
        assert_eq!(rewrites[1].target_agent, "verifier");
    }
}
