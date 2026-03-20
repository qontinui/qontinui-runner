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

use tracing::{debug, info, warn};

use super::recommendations;
use crate::database::CheckpointDb;

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

// ── Deduplication helper ─────────────────────────────────────────────────

/// Check if a pending or rejected recommendation with the same title already exists.
/// This prevents re-generating recommendations that were previously rejected.
fn is_duplicate_recommendation(db: &CheckpointDb, title: &str) -> bool {
    let title = title.to_string();
    db.with_conn(move |conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE title = ?1 AND status IN ('pending', 'rejected')",
                rusqlite::params![title],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    })
    .unwrap_or(false)
}

// ── Save orchestrator ───────────────────────────────────────────────────

/// Parse recommendations from AI output and save them to the database.
///
/// `optimizer_type` should be one of: `"pipeline_prompt"`, `"architecture"`, `"generation_template"`.
///
/// Returns the number of recommendations created.
pub fn save_parsed_recommendations(
    db: &CheckpointDb,
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

                if rec.confidence < 0.5 {
                    debug!(
                        "Skipping low-confidence recommendation ({:.0}%): {}",
                        rec.confidence * 100.0,
                        title
                    );
                    continue;
                }

                if is_duplicate_recommendation(db, &title) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

                // Serialize as JSON payload matching PromptRewritePayload
                let recommended_value = serde_json::json!({
                    "agent_type": rec.agent_type,
                    "variant_name": rec.variant_name,
                    "prompt_content": rec.prompt_content,
                })
                .to_string();

                recommendations::create_recommendation(
                    db,
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

                if rec.confidence < 0.5 {
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

                if is_duplicate_recommendation(db, &title) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

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

                recommendations::create_recommendation(
                    db,
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

                if rec.confidence < 0.5 {
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

                if is_duplicate_recommendation(db, &title) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

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

                recommendations::create_recommendation(
                    db,
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
                if is_duplicate_recommendation(db, &title) {
                    debug!("Skipping duplicate finding: {}", title);
                    continue;
                }
                let metadata = serde_json::json!({
                    "category": finding.category,
                    "severity": finding.severity,
                    "suggested_action": finding.suggested_action,
                })
                .to_string();
                recommendations::create_recommendation(
                    db,
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

                if rec.confidence < 0.5 {
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

                if is_duplicate_recommendation(db, &title) {
                    debug!("Skipping duplicate recommendation: {}", title);
                    continue;
                }

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

                recommendations::create_recommendation(
                    db,
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
        auto_apply_high_confidence(db, optimizer_run_id);
    }

    Ok(count)
}

/// Auto-apply high-confidence rule recommendations.
///
/// Only applies `rule_create` and `rule_update` types with confidence >= 0.85.
/// Prompt rewrites and config changes always require human review since they
/// are harder to reverse and have broader impact.
pub fn auto_apply_high_confidence(db: &CheckpointDb, optimizer_run_id: Option<&str>) {
    let run_id = optimizer_run_id.map(|s| s.to_string());

    // Only auto-apply rule changes (safest, most reversible)
    let candidates: Vec<(String, f64)> = db
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id, confidence FROM meta_optimizer_recommendations
                       WHERE status = 'pending'
                         AND recommendation_type IN ('rule_create', 'rule_update')
                         AND confidence >= 0.85
                         AND (?1 IS NULL OR optimizer_run_id = ?1)"#,
                )
                .map_err(|e| format!("Query error: {}", e))?;

            let rows = stmt
                .query_map(rusqlite::params![run_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                })
                .map_err(|e| format!("Query error: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(rows)
        })
        .unwrap_or_default();

    for (rec_id, confidence) in &candidates {
        match super::recommendations::apply_recommendation_with_side_effects(db, rec_id) {
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

    if !candidates.is_empty() {
        info!(
            "Auto-applied {} high-confidence rule recommendation(s)",
            candidates.len()
        );
    }
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
    }
}
