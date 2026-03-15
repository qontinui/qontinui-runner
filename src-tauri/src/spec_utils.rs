//! Shared utilities for working with architecture specs.
//!
//! Architecture specs have `techStack` + `features` fields.
//! Page specs have `groups` + `assertions` fields.

use crate::str_utils::truncate_str;

/// Check if a parsed spec JSON value represents an architecture spec.
pub fn is_architecture_spec(spec: &serde_json::Value) -> bool {
    spec.get("techStack").is_some() && spec.get("features").is_some()
}

/// Parse a `spec_json` string from cached_app_specs and check if it's an architecture spec.
pub fn is_architecture_spec_str(spec_json: &str) -> bool {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(spec_json) {
        is_architecture_spec(&parsed)
    } else {
        false
    }
}

/// Format an architecture spec as concise markdown context for AI consumption.
pub fn format_architecture_markdown(spec: &serde_json::Value, project_name: &str) -> String {
    let mut sections: Vec<String> = Vec::new();

    let description = spec
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut header = format!("#### {}\n", project_name);
    if !description.is_empty() {
        let desc = if description.len() > 300 {
            format!("{}...", truncate_str(description, 297))
        } else {
            description.to_string()
        };
        header.push_str(&format!("{}\n", desc));
    }
    sections.push(header);

    // Tech stack
    if let Some(tech_stack) = spec.get("techStack").and_then(|v| v.as_array()) {
        if !tech_stack.is_empty() {
            let mut lines: Vec<String> = vec!["**Tech Stack:**".to_string()];
            for tech in tech_stack.iter().take(15) {
                let name = tech.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let purpose = tech.get("purpose").and_then(|v| v.as_str()).unwrap_or("");
                let version = tech
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|v| format!(" v{}", v))
                    .unwrap_or_default();
                lines.push(format!("- **{}{}** — {}", name, version, purpose));
            }
            if tech_stack.len() > 15 {
                lines.push(format!("- ... and {} more", tech_stack.len() - 15));
            }
            sections.push(lines.join("\n"));
        }
    }

    // Features
    if let Some(features) = spec.get("features").and_then(|v| v.as_array()) {
        if !features.is_empty() {
            let mut lines: Vec<String> = vec!["**Features:**".to_string()];
            for feature in features.iter().take(20) {
                let name = feature.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let priority = feature
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let desc = feature
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let short_desc = if desc.len() > 120 {
                    format!("{}...", truncate_str(desc, 117))
                } else {
                    desc.to_string()
                };
                let mut line = format!("- **{}**", name);
                if !priority.is_empty() {
                    line.push_str(&format!(" [{}]", priority));
                }
                if !short_desc.is_empty() {
                    line.push_str(&format!(": {}", short_desc));
                }
                lines.push(line);
            }
            if features.len() > 20 {
                lines.push(format!("- ... and {} more features", features.len() - 20));
            }
            sections.push(lines.join("\n"));
        }
    }

    // Patterns
    if let Some(patterns) = spec.get("patterns").and_then(|v| v.as_array()) {
        if !patterns.is_empty() {
            let mut lines: Vec<String> = vec!["**Architecture Patterns:**".to_string()];
            for pattern in patterns.iter().take(10) {
                let name = pattern.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = pattern
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let short_desc = if desc.len() > 100 {
                    format!("{}...", truncate_str(desc, 97))
                } else {
                    desc.to_string()
                };
                let mut line = format!("- **{}**", name);
                if !short_desc.is_empty() {
                    line.push_str(&format!(": {}", short_desc));
                }
                lines.push(line);
            }
            if patterns.len() > 10 {
                lines.push(format!("- ... and {} more patterns", patterns.len() - 10));
            }
            sections.push(lines.join("\n"));
        }
    }

    // Constraints (brief)
    if let Some(constraints) = spec.get("constraints").and_then(|v| v.as_array()) {
        if !constraints.is_empty() {
            let names: Vec<&str> = constraints
                .iter()
                .take(10)
                .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
                .collect();
            if !names.is_empty() {
                sections.push(format!("**Constraints:** {}", names.join(", ")));
            }
        }
    }

    // Key directories (brief)
    if let Some(directories) = spec.get("directories").and_then(|v| v.as_array()) {
        if !directories.is_empty() {
            let mut lines: Vec<String> = vec!["**Key Directories:**".to_string()];
            for dir in directories.iter().take(8) {
                let path = dir.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let purpose = dir.get("purpose").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("- `{}` — {}", path, purpose));
            }
            if directories.len() > 8 {
                lines.push(format!("- ... and {} more", directories.len() - 8));
            }
            sections.push(lines.join("\n"));
        }
    }

    // Agentic Structure
    if let Some(agentic) = spec.get("agenticStructure") {
        if let Some(agents) = agentic.get("agents").and_then(|v| v.as_array()) {
            if !agents.is_empty() {
                let mut lines: Vec<String> = vec!["**Agentic Structure:**".to_string()];
                for agent in agents.iter().take(10) {
                    let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let role = agent.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = agent
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let short_desc = if desc.len() > 100 {
                        format!("{}...", truncate_str(desc, 97))
                    } else {
                        desc.to_string()
                    };
                    lines.push(format!("- **{}** [{}]: {}", name, role, short_desc));
                }
                sections.push(lines.join("\n"));
            }
        }
        if let Some(loops) = agentic.get("feedbackLoops").and_then(|v| v.as_array()) {
            if !loops.is_empty() {
                let mut lines: Vec<String> = vec!["**Feedback Loops:**".to_string()];
                for loop_item in loops.iter().take(5) {
                    let name = loop_item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let exit = loop_item
                        .get("exitCondition")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let loop_type = loop_item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    lines.push(format!(
                        "- **{}** [{}]: exit when {}",
                        name, loop_type, exit
                    ));
                }
                sections.push(lines.join("\n"));
            }
        }
        if let Some(chains) = agentic.get("delegationChains").and_then(|v| v.as_array()) {
            if !chains.is_empty() {
                let mut lines: Vec<String> = vec!["**Delegation Chains:**".to_string()];
                for chain in chains.iter().take(8) {
                    let from = chain.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                    let to = chain.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                    let via = chain.get("via").and_then(|v| v.as_str()).unwrap_or("");
                    lines.push(format!("- {} -> {} via {}", from, to, via));
                }
                sections.push(lines.join("\n"));
            }
        }
    }

    sections.join("\n\n")
}

/// Format a brief architecture summary (for snapshots — compact).
pub fn format_architecture_summary(
    spec: &serde_json::Value,
    project_name: &str,
) -> serde_json::Value {
    let tech_stack: Vec<String> = spec
        .get("techStack")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(10)
                .filter_map(|t| {
                    let name = t.get("name").and_then(|v| v.as_str())?;
                    Some(name.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let feature_count = spec
        .get("features")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let pattern_count = spec
        .get("patterns")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let constraint_count = spec
        .get("constraints")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let agent_count = spec
        .get("agenticStructure")
        .and_then(|v| v.get("agents"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let feedback_loop_count = spec
        .get("agenticStructure")
        .and_then(|v| v.get("feedbackLoops"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let description = spec
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let short_desc = if description.len() > 200 {
        format!("{}...", truncate_str(description, 197))
    } else {
        description.to_string()
    };

    serde_json::json!({
        "name": project_name,
        "description": short_desc,
        "techStack": tech_stack,
        "featureCount": feature_count,
        "patternCount": pattern_count,
        "constraintCount": constraint_count,
        "agentCount": agent_count,
        "feedbackLoopCount": feedback_loop_count,
    })
}
