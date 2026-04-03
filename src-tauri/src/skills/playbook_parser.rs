//! Playbook Parser
//!
//! Parses markdown files with YAML frontmatter into SkillDefinition objects
//! with the Playbook template variant. Playbooks are human-editable domain
//! knowledge files that provide LLM context about how to interact with
//! specific applications.
//!
//! ## File Format
//!
//! ```markdown
//! ---
//! name: Salesforce Login Flow
//! description: How to log into Salesforce and navigate to Setup
//! category: domain-knowledge
//! tags: [salesforce, authentication]
//! triggers:
//!   - type: app_name
//!     value: Salesforce
//!   - type: url_pattern
//!     value: "*.salesforce.com/*"
//! ---
//!
//! ## Salesforce Login
//!
//! 1. The login page has a username field with ID "username"...
//! ```

#![allow(dead_code)]

use super::{PlaybookTrigger, SkillDefinition, SkillTemplate};
use serde::Deserialize;
use std::path::Path;
use tracing::{debug, warn};

/// Intermediate YAML frontmatter structure.
#[derive(Debug, Deserialize)]
struct PlaybookFrontmatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    triggers: Vec<TriggerEntry>,
    #[serde(default)]
    context: Option<String>, // "ui-bridge", "black-box", or "both"
    /// Optional parameters for recorded/generated playbooks.
    #[serde(default)]
    parameters: Vec<ParameterEntry>,
}

/// A parameter entry in the playbook frontmatter.
#[derive(Debug, Deserialize)]
struct ParameterEntry {
    name: String,
    #[serde(rename = "type", default = "default_param_type")]
    param_type: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default: Option<String>,
}

fn default_param_type() -> String {
    "string".to_string()
}

fn default_category() -> String {
    "domain-knowledge".to_string()
}

#[derive(Debug, Deserialize)]
struct TriggerEntry {
    #[serde(rename = "type")]
    trigger_type: String,
    value: String,
}

/// Parse a markdown file with YAML frontmatter into a SkillDefinition.
///
/// The file must start with `---`, followed by YAML metadata, followed by
/// another `---`, followed by markdown content.
///
/// # Arguments
/// * `path` - Path to the `.md` file.
///
/// # Returns
/// A `SkillDefinition` with `SkillTemplate::Playbook`, or an error string.
pub fn parse_playbook(path: &Path) -> Result<SkillDefinition, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read playbook {}: {}", path.display(), e))?;

    parse_playbook_content(&content, path)
}

/// Parse playbook content (for testing without filesystem).
pub fn parse_playbook_content(
    content: &str,
    source_path: &Path,
) -> Result<SkillDefinition, String> {
    // Split frontmatter from body
    let (frontmatter_str, body) = split_frontmatter(content)
        .ok_or_else(|| format!("No YAML frontmatter found in {}", source_path.display()))?;

    // Parse YAML frontmatter
    let frontmatter: PlaybookFrontmatter = serde_yaml::from_str(frontmatter_str).map_err(|e| {
        format!(
            "Invalid YAML frontmatter in {}: {}",
            source_path.display(),
            e
        )
    })?;

    // Generate ID from filename
    let id = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let slug = id.replace('_', "-");

    // Convert triggers
    let triggers: Vec<PlaybookTrigger> = frontmatter
        .triggers
        .into_iter()
        .map(|t| PlaybookTrigger {
            trigger_type: t.trigger_type,
            value: t.value,
        })
        .collect();

    // Build tags (include context tag if specified)
    let mut tags = frontmatter.tags;
    if let Some(ctx) = frontmatter.context {
        tags.push(format!("context:{}", ctx));
    }

    // Convert parameters
    let parameters: Vec<super::SkillParameter> = frontmatter
        .parameters
        .into_iter()
        .map(|p| super::SkillParameter {
            name: p.name.clone(),
            param_type: p.param_type,
            label: p.name.clone(),
            description: p.description.unwrap_or_default(),
            required: false,
            default: p.default.map(|v| serde_json::Value::String(v)),
            options: None,
            placeholder: None,
            min: None,
            max: None,
            pattern: None,
            depends_on: None,
        })
        .collect();

    debug!(
        "Parsed playbook '{}' from {} ({} triggers, {} tags, {} parameters)",
        frontmatter.name,
        source_path.display(),
        triggers.len(),
        tags.len(),
        parameters.len()
    );

    Ok(SkillDefinition {
        id: format!("playbook-{}", id),
        name: frontmatter.name,
        slug,
        description: frontmatter.description.unwrap_or_default(),
        category: frontmatter.category,
        tags,
        icon: "book".to_string(),
        color: "#6366f1".to_string(),
        allowed_phases: vec!["setup".to_string(), "agentic".to_string()],
        parameters,
        template: SkillTemplate::Playbook {
            content: body.to_string(),
            triggers,
        },
        source: "user".to_string(),
        version: None,
        author: None,
        checksum: None,
        depends_on: None,
        usage_count: None,
        approval_status: None,
        forked_from: None,
    })
}

/// Split markdown content into YAML frontmatter and body.
///
/// Expects the content to start with `---\n`, followed by YAML, followed by
/// `---\n`, followed by the markdown body.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.trim_start();

    if !content.starts_with("---") {
        return None;
    }

    // Find the closing `---`
    let after_first = &content[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    let close_pos = after_first.find("\n---")?;
    let frontmatter = &after_first[..close_pos];
    let body = &after_first[close_pos + 4..]; // skip "\n---"
    let body = body.trim_start_matches(['\r', '\n']);

    Some((frontmatter, body))
}

/// Load all playbooks from a directory.
///
/// Scans for `.md` files, parses each, and returns the successfully parsed ones.
/// Files that fail to parse are logged as warnings but do not block other files.
pub fn load_playbooks_from_dir(dir: &Path) -> Vec<SkillDefinition> {
    if !dir.exists() || !dir.is_dir() {
        debug!(
            "Playbook directory {} does not exist, skipping",
            dir.display()
        );
        return vec![];
    }

    let mut playbooks = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Failed to read playbook directory {}: {}", dir.display(), e);
            return vec![];
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            match parse_playbook(&path) {
                Ok(skill) => {
                    debug!("Loaded playbook: {} ({})", skill.name, skill.id);
                    playbooks.push(skill);
                }
                Err(e) => {
                    warn!("Failed to parse playbook {}: {}", path.display(), e);
                }
            }
        }
    }

    debug!(
        "Loaded {} playbooks from {}",
        playbooks.len(),
        dir.display()
    );
    playbooks
}

/// Find playbooks whose triggers match the given context.
pub fn playbooks_for_context<'a>(
    playbooks: &'a [SkillDefinition],
    app_name: Option<&str>,
    url: Option<&str>,
    tags: &[String],
) -> Vec<&'a SkillDefinition> {
    playbooks
        .iter()
        .filter(|skill| {
            if let SkillTemplate::Playbook { triggers, .. } = &skill.template {
                if triggers.is_empty() {
                    return true; // No triggers means always include
                }
                triggers.iter().any(|trigger| {
                    match trigger.trigger_type.as_str() {
                        "app_name" => app_name.is_some_and(|name| {
                            name.to_lowercase().contains(&trigger.value.to_lowercase())
                        }),
                        "url_pattern" => {
                            url.is_some_and(|u| {
                                // Simple glob matching: split on * and check segments
                                // appear in order within the URL.
                                // e.g., "*.salesforce.com/*" splits to ["", ".salesforce.com/", ""]
                                let segments: Vec<&str> = trigger.value.split('*').collect();
                                let mut remaining = u;
                                for (i, seg) in segments.iter().enumerate() {
                                    if seg.is_empty() {
                                        continue;
                                    }
                                    if i == 0 {
                                        // First segment must be a prefix
                                        if !remaining.starts_with(seg) {
                                            return false;
                                        }
                                        remaining = &remaining[seg.len()..];
                                    } else if i == segments.len() - 1 {
                                        // Last segment must be a suffix
                                        if !remaining.ends_with(seg) {
                                            return false;
                                        }
                                    } else {
                                        // Middle segments must appear in order
                                        match remaining.find(seg) {
                                            Some(pos) => remaining = &remaining[pos + seg.len()..],
                                            None => return false,
                                        }
                                    }
                                }
                                true
                            })
                        }
                        "tag" => tags.iter().any(|t| t == &trigger.value),
                        _ => false,
                    }
                })
            } else {
                false
            }
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE_PLAYBOOK: &str = r#"---
name: Salesforce Login Flow
description: How to log into Salesforce and navigate to Setup
category: domain-knowledge
tags: [salesforce, authentication]
triggers:
  - type: app_name
    value: Salesforce
  - type: url_pattern
    value: "*.salesforce.com/*"
---

## Salesforce Login

1. The login page has a username field with ID "username" and a password field with ID "password".
2. After login, wait for the "Setup" icon to appear in the top-right corner.
3. If MFA is required, there will be a verification code input.
"#;

    #[test]
    fn test_split_frontmatter() {
        let (fm, body) = split_frontmatter(SAMPLE_PLAYBOOK).unwrap();
        assert!(fm.contains("name: Salesforce Login Flow"));
        assert!(body.contains("## Salesforce Login"));
    }

    #[test]
    fn test_parse_playbook_content() {
        let path = PathBuf::from("salesforce-login.md");
        let skill = parse_playbook_content(SAMPLE_PLAYBOOK, &path).unwrap();

        assert_eq!(skill.name, "Salesforce Login Flow");
        assert_eq!(
            skill.description,
            "How to log into Salesforce and navigate to Setup"
        );
        assert_eq!(skill.category, "domain-knowledge");
        assert!(skill.tags.contains(&"salesforce".to_string()));
        assert!(skill.tags.contains(&"authentication".to_string()));
        assert_eq!(skill.id, "playbook-salesforce-login");
        assert_eq!(skill.source, "user");

        if let SkillTemplate::Playbook { content, triggers } = &skill.template {
            assert!(content.contains("## Salesforce Login"));
            assert_eq!(triggers.len(), 2);
            assert_eq!(triggers[0].trigger_type, "app_name");
            assert_eq!(triggers[0].value, "Salesforce");
            assert_eq!(triggers[1].trigger_type, "url_pattern");
        } else {
            panic!("Expected Playbook template");
        }
    }

    #[test]
    fn test_parse_minimal_playbook() {
        let content = "---\nname: Minimal\n---\n\nJust some instructions.";
        let path = PathBuf::from("minimal.md");
        let skill = parse_playbook_content(content, &path).unwrap();
        assert_eq!(skill.name, "Minimal");
        assert_eq!(skill.category, "domain-knowledge"); // default
        assert!(skill.tags.is_empty());
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let content = "# Just markdown\n\nNo frontmatter here.";
        let path = PathBuf::from("bad.md");
        assert!(parse_playbook_content(content, &path).is_err());
    }

    #[test]
    fn test_playbooks_for_context_app_name() {
        let path = PathBuf::from("sf.md");
        let skill = parse_playbook_content(SAMPLE_PLAYBOOK, &path).unwrap();
        let playbooks = vec![skill];

        let matched = playbooks_for_context(&playbooks, Some("Salesforce Lightning"), None, &[]);
        assert_eq!(matched.len(), 1);

        let not_matched = playbooks_for_context(&playbooks, Some("Excel"), None, &[]);
        assert_eq!(not_matched.len(), 0);
    }

    #[test]
    fn test_playbooks_for_context_url() {
        let path = PathBuf::from("sf.md");
        let skill = parse_playbook_content(SAMPLE_PLAYBOOK, &path).unwrap();
        let playbooks = vec![skill];

        let matched = playbooks_for_context(
            &playbooks,
            None,
            Some("https://mycompany.salesforce.com/setup"),
            &[],
        );
        assert_eq!(matched.len(), 1);
    }
}
