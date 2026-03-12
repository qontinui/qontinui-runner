//! AI-discovered constraint proposals.
//!
//! During the agentic phase, the AI can propose new constraints it discovers
//! while analyzing the codebase. Proposals are extracted from the AI's output
//! using structured markers and can be:
//!
//! 1. Applied immediately for the remainder of the current workflow
//! 2. Logged for the user to review and add to `constraints.toml`
//!
//! ## Marker format
//!
//! The AI emits proposals using TOML blocks inside markers:
//!
//! ```text
//! [CONSTRAINT_PROPOSAL]
//! id = "project:no-raw-sql"
//! name = "No raw SQL queries"
//! description = "Use parameterized queries instead of string concatenation"
//! severity = "warn"
//!
//! [check]
//! type = "grep_forbidden"
//! pattern = "execute\\(f\""
//! file_glob = "*.py"
//! [/CONSTRAINT_PROPOSAL]
//! ```
//!
//! The AI can also propose enabling/disabling builtins:
//!
//! ```text
//! [CONSTRAINT_PROPOSAL]
//! builtin = "no-debug-statements"
//! enabled = true
//! reason = "Found console.log statements that should be removed"
//! [/CONSTRAINT_PROPOSAL]
//! ```

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::str_utils::truncate_str;

use super::types::{Constraint, ConstraintCheck, ConstraintSeverity};

/// A constraint proposal from the AI.
#[derive(Debug, Clone)]
pub enum ConstraintProposal {
    /// Propose a new custom constraint.
    NewConstraint(Constraint),
    /// Propose enabling or disabling a builtin.
    BuiltinOverride {
        builtin_suffix: String,
        enabled: bool,
        reason: String,
    },
}

/// Raw TOML structure for parsing a proposal block.
#[derive(Debug, Deserialize)]
struct RawProposal {
    // --- New constraint fields ---
    id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default = "default_severity")]
    severity: String,
    check: Option<CheckBlock>,

    // --- Builtin override fields ---
    builtin: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    reason: String,
}

fn default_severity() -> String {
    "warn".to_string()
}

/// Check block within a proposal.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CheckBlock {
    GrepForbidden {
        pattern: String,
        #[serde(default)]
        file_glob: Option<String>,
    },
    GrepRequired {
        pattern: String,
        #[serde(default)]
        file_glob: Option<String>,
    },
    FileScope {
        allowed_paths: Vec<String>,
    },
}

const MARKER_START: &str = "[CONSTRAINT_PROPOSAL]";
const MARKER_END: &str = "[/CONSTRAINT_PROPOSAL]";

/// Parse constraint proposals from AI output text.
///
/// Extracts all `[CONSTRAINT_PROPOSAL]...[/CONSTRAINT_PROPOSAL]` blocks
/// and converts them into typed proposals. Invalid blocks are logged and
/// skipped — we never fail the workflow due to a malformed proposal.
pub fn parse_proposals(output: &str) -> Vec<ConstraintProposal> {
    let mut proposals = Vec::new();
    let mut search_pos = 0;

    while let Some(start) = output[search_pos..].find(MARKER_START) {
        let abs_start = search_pos + start + MARKER_START.len();

        if let Some(end) = output[abs_start..].find(MARKER_END) {
            let abs_end = abs_start + end;
            let block = output[abs_start..abs_end].trim();

            if !block.is_empty() {
                match parse_single_proposal(block) {
                    Some(proposal) => {
                        info!(
                            "CONSTRAINT-PROPOSAL: Parsed proposal from AI output: {:?}",
                            proposal_summary(&proposal),
                        );
                        proposals.push(proposal);
                    }
                    None => {
                        debug!(
                            "CONSTRAINT-PROPOSAL: Failed to parse proposal block: {}",
                            truncate_str(block, 200),
                        );
                    }
                }
            }

            search_pos = abs_end + MARKER_END.len();
        } else {
            // No closing marker — skip
            warn!("CONSTRAINT-PROPOSAL: Found opening marker without closing marker");
            break;
        }
    }

    if !proposals.is_empty() {
        info!(
            "CONSTRAINT-PROPOSAL: Extracted {} proposal(s) from AI output",
            proposals.len(),
        );
    }

    proposals
}

/// Parse a single TOML proposal block.
fn parse_single_proposal(block: &str) -> Option<ConstraintProposal> {
    let raw: RawProposal = match toml::from_str(block) {
        Ok(r) => r,
        Err(e) => {
            warn!("CONSTRAINT-PROPOSAL: TOML parse error: {}", e);
            return None;
        }
    };

    // Decide which variant: builtin override or new constraint
    if let Some(builtin_suffix) = raw.builtin {
        // Builtin override proposal
        let enabled = raw.enabled.unwrap_or(true);
        return Some(ConstraintProposal::BuiltinOverride {
            builtin_suffix,
            enabled,
            reason: raw.reason,
        });
    }

    // New constraint proposal — needs id, name, and check
    let id = raw.id?;
    let name = raw.name?;
    let check_block = raw.check?;

    // Ensure the ID has a namespace prefix
    let id = if id.contains(':') {
        id
    } else {
        format!("ai:{}", id)
    };

    let severity = match raw.severity.as_str() {
        "block" => ConstraintSeverity::Block,
        "warn" => ConstraintSeverity::Warn,
        "log" => ConstraintSeverity::Log,
        _ => {
            warn!(
                "CONSTRAINT-PROPOSAL: Unknown severity '{}', defaulting to 'warn'",
                raw.severity,
            );
            ConstraintSeverity::Warn
        }
    };

    let check = match check_block {
        CheckBlock::GrepForbidden { pattern, file_glob } => {
            // Validate regex
            if let Err(e) = regex::Regex::new(&pattern) {
                warn!(
                    "CONSTRAINT-PROPOSAL: Invalid regex in proposal '{}': {}",
                    id, e,
                );
                return None;
            }
            ConstraintCheck::GrepForbidden { pattern, file_glob }
        }
        CheckBlock::GrepRequired { pattern, file_glob } => {
            if let Err(e) = regex::Regex::new(&pattern) {
                warn!(
                    "CONSTRAINT-PROPOSAL: Invalid regex in proposal '{}': {}",
                    id, e,
                );
                return None;
            }
            ConstraintCheck::GrepRequired { pattern, file_glob }
        }
        CheckBlock::FileScope { allowed_paths } => ConstraintCheck::FileScope { allowed_paths },
    };

    Some(ConstraintProposal::NewConstraint(Constraint {
        id,
        name,
        description: raw.description,
        check,
        severity,
        enabled: true,
    }))
}

/// Get a short summary for logging.
fn proposal_summary(proposal: &ConstraintProposal) -> String {
    match proposal {
        ConstraintProposal::NewConstraint(c) => {
            format!("new constraint '{}' ({:?})", c.id, c.severity)
        }
        ConstraintProposal::BuiltinOverride {
            builtin_suffix,
            enabled,
            ..
        } => {
            format!(
                "builtin '{}' → {}",
                builtin_suffix,
                if *enabled { "enable" } else { "disable" },
            )
        }
    }
}

/// Format proposals as a TOML snippet that can be appended to `constraints.toml`.
///
/// This is logged at the end of a workflow so the user can easily copy-paste
/// AI-discovered constraints into their config file.
pub fn proposals_to_toml(proposals: &[ConstraintProposal]) -> String {
    let mut output = String::new();

    let builtin_overrides: Vec<_> = proposals
        .iter()
        .filter_map(|p| match p {
            ConstraintProposal::BuiltinOverride {
                builtin_suffix,
                enabled,
                reason,
            } => Some((builtin_suffix, enabled, reason)),
            _ => None,
        })
        .collect();

    if !builtin_overrides.is_empty() {
        output.push_str("# AI-suggested builtin overrides\n[builtins]\n");
        for (suffix, enabled, reason) in &builtin_overrides {
            if !reason.is_empty() {
                output.push_str(&format!("# Reason: {}\n", reason));
            }
            output.push_str(&format!("{} = {}\n", suffix, enabled));
        }
        output.push('\n');
    }

    let new_constraints: Vec<_> = proposals
        .iter()
        .filter_map(|p| match p {
            ConstraintProposal::NewConstraint(c) => Some(c),
            _ => None,
        })
        .collect();

    for constraint in &new_constraints {
        output.push_str("# AI-discovered constraint\n");
        output.push_str("[[constraint]]\n");
        output.push_str(&format!("id = \"{}\"\n", constraint.id));
        output.push_str(&format!("name = \"{}\"\n", constraint.name));
        if !constraint.description.is_empty() {
            output.push_str(&format!("description = \"{}\"\n", constraint.description));
        }
        let severity_str = match constraint.severity {
            ConstraintSeverity::Block => "block",
            ConstraintSeverity::Warn => "warn",
            ConstraintSeverity::Log => "log",
        };
        output.push_str(&format!("severity = \"{}\"\n", severity_str));
        output.push('\n');

        match &constraint.check {
            ConstraintCheck::GrepForbidden { pattern, file_glob } => {
                output.push_str("[constraint.check]\n");
                output.push_str("type = \"grep_forbidden\"\n");
                output.push_str(&format!("pattern = \"{}\"\n", escape_toml_string(pattern)));
                if let Some(glob) = file_glob {
                    output.push_str(&format!("file_glob = \"{}\"\n", escape_toml_string(glob)));
                }
            }
            ConstraintCheck::GrepRequired { pattern, file_glob } => {
                output.push_str("[constraint.check]\n");
                output.push_str("type = \"grep_required\"\n");
                output.push_str(&format!("pattern = \"{}\"\n", escape_toml_string(pattern)));
                if let Some(glob) = file_glob {
                    output.push_str(&format!("file_glob = \"{}\"\n", escape_toml_string(glob)));
                }
            }
            ConstraintCheck::FileScope { allowed_paths } => {
                output.push_str("[constraint.check]\n");
                output.push_str("type = \"file_scope\"\n");
                let paths: Vec<String> = allowed_paths
                    .iter()
                    .map(|p| format!("\"{}\"", escape_toml_string(p)))
                    .collect();
                output.push_str(&format!("allowed_paths = [{}]\n", paths.join(", ")));
            }
            ConstraintCheck::Command {
                cmd,
                cwd,
                timeout_secs,
            } => {
                output.push_str("[constraint.check]\n");
                output.push_str("type = \"command\"\n");
                output.push_str(&format!("cmd = \"{}\"\n", escape_toml_string(cmd)));
                if let Some(cwd) = cwd {
                    output.push_str(&format!("cwd = \"{}\"\n", escape_toml_string(cwd)));
                }
                output.push_str(&format!("timeout_secs = {}\n", timeout_secs));
            }
        }
        output.push('\n');
    }

    output
}

/// Escape backslashes and quotes for TOML string values.
fn escape_toml_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_new_constraint_proposal() {
        let output = r#"
I noticed the codebase has raw SQL queries. Let me propose a constraint:

[CONSTRAINT_PROPOSAL]
id = "project:no-raw-sql"
name = "No raw SQL queries"
description = "Use parameterized queries instead of string concatenation"
severity = "warn"

[check]
type = "grep_forbidden"
pattern = "execute\\(f\""
file_glob = "*.py"
[/CONSTRAINT_PROPOSAL]

I've fixed the issue by using parameterized queries.
"#;

        let proposals = parse_proposals(output);
        assert_eq!(proposals.len(), 1);

        match &proposals[0] {
            ConstraintProposal::NewConstraint(c) => {
                assert_eq!(c.id, "project:no-raw-sql");
                assert_eq!(c.name, "No raw SQL queries");
                assert_eq!(c.severity, ConstraintSeverity::Warn);
                assert!(matches!(
                    &c.check,
                    ConstraintCheck::GrepForbidden { pattern, file_glob: Some(glob) }
                    if pattern == r#"execute\(f""# && glob == "*.py"
                ));
            }
            _ => panic!("Expected NewConstraint"),
        }
    }

    #[test]
    fn test_parse_builtin_override_proposal() {
        let output = r#"
Found console.log statements that should be cleaned up.

[CONSTRAINT_PROPOSAL]
builtin = "no-debug-statements"
enabled = true
reason = "Found console.log statements that should be removed before commit"
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert_eq!(proposals.len(), 1);

        match &proposals[0] {
            ConstraintProposal::BuiltinOverride {
                builtin_suffix,
                enabled,
                reason,
            } => {
                assert_eq!(builtin_suffix, "no-debug-statements");
                assert!(*enabled);
                assert!(reason.contains("console.log"));
            }
            _ => panic!("Expected BuiltinOverride"),
        }
    }

    #[test]
    fn test_parse_multiple_proposals() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
id = "project:no-todos"
name = "No TODOs"
severity = "log"

[check]
type = "grep_forbidden"
pattern = "(?i)\\bTODO\\b"
[/CONSTRAINT_PROPOSAL]

Some other text here.

[CONSTRAINT_PROPOSAL]
id = "project:src-scope"
name = "Source scope"
severity = "block"

[check]
type = "file_scope"
allowed_paths = ["src/", "tests/"]
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert_eq!(proposals.len(), 2);
        assert!(
            matches!(&proposals[0], ConstraintProposal::NewConstraint(c) if c.id == "project:no-todos")
        );
        assert!(
            matches!(&proposals[1], ConstraintProposal::NewConstraint(c) if c.id == "project:src-scope")
        );
    }

    #[test]
    fn test_auto_namespace_id() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
id = "no-hardcoded-urls"
name = "No hardcoded URLs"
severity = "warn"

[check]
type = "grep_forbidden"
pattern = "https?://[a-zA-Z0-9]"
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert_eq!(proposals.len(), 1);
        match &proposals[0] {
            ConstraintProposal::NewConstraint(c) => {
                assert_eq!(c.id, "ai:no-hardcoded-urls");
            }
            _ => panic!("Expected NewConstraint"),
        }
    }

    #[test]
    fn test_invalid_regex_skipped() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
id = "bad"
name = "Bad"
severity = "warn"

[check]
type = "grep_forbidden"
pattern = "[invalid("
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert!(proposals.is_empty(), "Invalid regex should skip proposal");
    }

    #[test]
    fn test_missing_closing_marker() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
id = "orphan"
name = "Orphan"
"#;

        let proposals = parse_proposals(output);
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_malformed_toml_skipped() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
this is not valid toml {{{
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_missing_required_fields_skipped() {
        let output = r#"
[CONSTRAINT_PROPOSAL]
id = "incomplete"
[/CONSTRAINT_PROPOSAL]
"#;

        let proposals = parse_proposals(output);
        assert!(proposals.is_empty(), "Missing name and check should skip");
    }

    #[test]
    fn test_proposals_to_toml() {
        let proposals = vec![
            ConstraintProposal::BuiltinOverride {
                builtin_suffix: "no-debug-statements".to_string(),
                enabled: true,
                reason: "Found debug statements".to_string(),
            },
            ConstraintProposal::NewConstraint(Constraint {
                id: "ai:no-raw-sql".to_string(),
                name: "No raw SQL".to_string(),
                description: "Use parameterized queries".to_string(),
                check: ConstraintCheck::GrepForbidden {
                    pattern: r#"execute\(f""#.to_string(),
                    file_glob: Some("*.py".to_string()),
                },
                severity: ConstraintSeverity::Warn,
                enabled: true,
            }),
        ];

        let toml_output = proposals_to_toml(&proposals);
        assert!(toml_output.contains("[builtins]"));
        assert!(toml_output.contains("no-debug-statements = true"));
        assert!(toml_output.contains("[[constraint]]"));
        assert!(toml_output.contains("ai:no-raw-sql"));
        assert!(toml_output.contains("grep_forbidden"));
    }

    #[test]
    fn test_no_proposals_in_normal_output() {
        let output = "I've fixed the bug by updating the database query. All tests pass now.";
        let proposals = parse_proposals(output);
        assert!(proposals.is_empty());
    }
}
