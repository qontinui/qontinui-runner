//! Step Type Metadata Registry
//!
//! Generates AI documentation from actual type definitions rather than
//! hardcoded static strings. Each step type has structured metadata
//! including field schemas, allowed phases, and display properties.

use once_cell::sync::Lazy;

/// Category of step type for grouping and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepCategory {
    Core,
    Verification,
    Automation,
    Utility,
}

impl StepCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepCategory::Core => "Core",
            StepCategory::Verification => "Verification",
            StepCategory::Automation => "Automation",
            StepCategory::Utility => "Utility",
        }
    }
}

/// Field type for step configuration fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    StringArray,
}

impl FieldType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
            FieldType::Object => "object",
            FieldType::Array => "array",
            FieldType::StringArray => "string[]",
        }
    }
}

/// Definition for a single field in a step type's configuration.
#[derive(Debug, Clone)]
pub struct StepTypeFieldDef {
    pub name: &'static str,
    pub field_type: FieldType,
    pub required: bool,
    pub description: &'static str,
    /// Allowed enum values (empty if not an enum field).
    pub enum_values: &'static [&'static str],
    /// Default value as a displayable string (empty if no default).
    pub default: &'static str,
}

/// Metadata for a single step type.
#[derive(Debug, Clone)]
pub struct StepTypeMetadata {
    /// Serde name (e.g. "shell_command")
    pub step_type: &'static str,
    /// Human-readable label (e.g. "Shell Command")
    pub display_name: &'static str,
    /// One-line explanation
    pub description: &'static str,
    /// Category for grouping/filtering
    pub category: StepCategory,
    /// Which phases this step type is allowed in
    pub allowed_phases: &'static [&'static str],
    /// Icon name (for UI rendering)
    pub icon: &'static str,
    /// Color name (for UI rendering)
    pub color: &'static str,
    /// Field definitions for JSON configuration
    pub fields: &'static [StepTypeFieldDef],
}

// ============================================================================
// Field definitions for each step type
// ============================================================================

/// Unified command fields (union of shell_command + check + check_group fields).
///
/// The `command` step type dispatches based on which fields are populated:
/// - `check_group_id` set -> check group execution
/// - `check_type` set -> check execution (lint, format, typecheck, etc.)
/// - Otherwise -> plain shell command execution
static COMMAND_FIELDS: &[StepTypeFieldDef] = &[
    // Shell command fields
    StepTypeFieldDef {
        name: "command",
        field_type: FieldType::String,
        required: false,
        description: "Shell command to execute. Required for plain commands, optional for checks (auto-detected from check_type + language).",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "working_directory",
        field_type: FieldType::String,
        required: false,
        description: "Working directory for command execution, or git repo root for ci_cd auto-detection",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "fail_on_error",
        field_type: FieldType::Boolean,
        required: false,
        description: "Fail workflow if command exits with non-zero",
        enum_values: &[],
        default: "true",
    },
    // Check fields (set check_type to activate check mode)
    StepTypeFieldDef {
        name: "check_type",
        field_type: FieldType::String,
        required: false,
        description: "Type of check to run. When set, activates check mode with auto-detection.",
        enum_values: &[
            "lint",
            "format",
            "typecheck",
            "analyze",
            "security",
            "custom_command",
            "ci_cd",
        ],
        default: "",
    },
    StepTypeFieldDef {
        name: "auto_fix",
        field_type: FieldType::Boolean,
        required: false,
        description: "Automatically fix issues (for lint/format checks)",
        enum_values: &[],
        default: "false",
    },
    StepTypeFieldDef {
        name: "repository",
        field_type: FieldType::String,
        required: false,
        description: "GitHub repository in owner/repo format (for ci_cd). Auto-detected from working_directory if omitted.",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "workflow_name",
        field_type: FieldType::String,
        required: false,
        description: "GitHub Actions workflow name filter (for ci_cd, e.g. 'CI')",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "branch",
        field_type: FieldType::String,
        required: false,
        description: "Branch filter (for ci_cd, e.g. 'main')",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "wait_for_completion",
        field_type: FieldType::Boolean,
        required: false,
        description: "Wait for in-progress CI runs to complete (for ci_cd)",
        enum_values: &[],
        default: "false",
    },
    // Check group fields (set check_group_id to activate check group mode)
    StepTypeFieldDef {
        name: "check_group_id",
        field_type: FieldType::String,
        required: false,
        description: "Saved check group UUID. When set, executes all checks in the group.",
        enum_values: &[],
        default: "",
    },
];

static TEST_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "test_type",
        field_type: FieldType::String,
        required: true,
        description: "Type of test to run",
        enum_values: &[
            "playwright",
            "qontinui_vision",
            "python",
            "repository",
            "custom_command",
        ],
        default: "",
    },
    StepTypeFieldDef {
        name: "command",
        field_type: FieldType::String,
        required: false,
        description: "Command to run (for repository/custom_command)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "code",
        field_type: FieldType::String,
        required: false,
        description: "Test code (for playwright/python)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "description",
        field_type: FieldType::String,
        required: false,
        description: "Test description",
        enum_values: &[],
        default: "",
    },
];

static PROMPT_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "content",
    field_type: FieldType::String,
    required: true,
    description: "The prompt instructions",
    enum_values: &[],
    default: "",
}];

static UI_BRIDGE_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "action",
        field_type: FieldType::String,
        required: true,
        description: "UI Bridge action to perform",
        enum_values: &["navigate", "execute", "assert", "snapshot"],
        default: "",
    },
    StepTypeFieldDef {
        name: "url",
        field_type: FieldType::String,
        required: false,
        description: "Target URL (for navigate action)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "instruction",
        field_type: FieldType::String,
        required: false,
        description: "Natural language instruction (for execute action)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "target",
        field_type: FieldType::String,
        required: false,
        description: "Element ID or selector to target",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "assert_type",
        field_type: FieldType::String,
        required: false,
        description: "Type of assertion (for assert action)",
        enum_values: &[
            "element_exists",
            "element_text",
            "element_visible",
            "page_title",
            "element_count",
        ],
        default: "",
    },
    StepTypeFieldDef {
        name: "expected",
        field_type: FieldType::String,
        required: false,
        description: "Expected value for assertion",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "timeout_ms",
        field_type: FieldType::Number,
        required: false,
        description: "Timeout in milliseconds",
        enum_values: &[],
        default: "5000",
    },
];

// ============================================================================
// All step type metadata entries
// ============================================================================

static ALL_METADATA: Lazy<Vec<StepTypeMetadata>> = Lazy::new(|| {
    vec![
        // Core step types
        StepTypeMetadata {
            step_type: "command",
            display_name: "Command",
            description: "Execute a shell command, code quality check, or check group. Set check_type for checks, check_group_id for check groups, or just command for plain shell execution.",
            category: StepCategory::Core,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "Terminal",
            color: "gray",
            fields: COMMAND_FIELDS,
        },
        StepTypeMetadata {
            step_type: "test",
            display_name: "Test",
            description: "Run verification tests (Playwright, pytest, custom)",
            category: StepCategory::Verification,
            allowed_phases: &["verification"],
            icon: "FlaskConical",
            color: "green",
            fields: TEST_FIELDS,
        },
        StepTypeMetadata {
            step_type: "ui_bridge",
            display_name: "UI Bridge",
            description: "Interact with web apps via UI Bridge SDK (navigate, execute actions, assert element state, take snapshots)",
            category: StepCategory::Automation,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "Monitor",
            color: "cyan",
            fields: UI_BRIDGE_FIELDS,
        },
        StepTypeMetadata {
            step_type: "prompt",
            display_name: "Prompt",
            description: "AI task instructions for any phase",
            category: StepCategory::Core,
            allowed_phases: &["setup", "verification", "agentic", "completion"],
            icon: "MessageSquare",
            color: "purple",
            fields: PROMPT_FIELDS,
        },
    ]
});

/// Returns metadata for all 4 core step types that AI can generate.
///
/// The core types are: command, test, ui_bridge, prompt.
pub fn get_all_step_type_metadata() -> &'static [StepTypeMetadata] {
    &ALL_METADATA
}

/// Find metadata for a specific step type by name.
pub fn get_step_type_metadata(step_type: &str) -> Option<&'static StepTypeMetadata> {
    ALL_METADATA.iter().find(|m| m.step_type == step_type)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::validation::allowed_types_for_phase;

    #[test]
    fn test_all_step_types_have_metadata() {
        let metadata = get_all_step_type_metadata();
        assert_eq!(
            metadata.len(),
            4,
            "Expected exactly 4 step types, got {}",
            metadata.len()
        );
    }

    #[test]
    fn test_no_duplicate_step_types() {
        let metadata = get_all_step_type_metadata();
        let mut seen = std::collections::HashSet::new();
        for m in metadata {
            assert!(
                seen.insert(m.step_type),
                "Duplicate step type: {}",
                m.step_type
            );
        }
    }

    #[test]
    fn test_allowed_phases_match_validation() {
        let _metadata = get_all_step_type_metadata();
        for phase in &["setup", "verification", "completion"] {
            let validation_types = allowed_types_for_phase(phase);
            for vtype in validation_types {
                if let Some(meta) = get_step_type_metadata(vtype) {
                    assert!(
                        meta.allowed_phases.contains(phase),
                        "Step type '{}' is allowed in '{}' phase in validation.rs but not in metadata",
                        vtype,
                        phase
                    );
                }
            }
        }
    }

    #[test]
    fn test_metadata_phases_match_validation() {
        let metadata = get_all_step_type_metadata();
        for meta in metadata {
            for phase in meta.allowed_phases {
                let validation_types = allowed_types_for_phase(phase);
                assert!(
                    validation_types.contains(&meta.step_type),
                    "Step type '{}' claims phase '{}' in metadata but validation.rs doesn't allow it",
                    meta.step_type,
                    phase
                );
            }
        }
    }

    #[test]
    fn test_core_types_present() {
        assert!(get_step_type_metadata("command").is_some());
        assert!(get_step_type_metadata("prompt").is_some());
        assert!(get_step_type_metadata("test").is_some());
        assert!(get_step_type_metadata("ui_bridge").is_some());
    }

    #[test]
    fn test_deleted_types_absent() {
        assert!(get_step_type_metadata("shell_command").is_none());
        assert!(get_step_type_metadata("api_request").is_none());
        assert!(get_step_type_metadata("mcp_call").is_none());
        assert!(get_step_type_metadata("check").is_none());
        assert!(get_step_type_metadata("check_group").is_none());
        assert!(get_step_type_metadata("gate").is_none());
        assert!(get_step_type_metadata("spec").is_none());
    }

    #[test]
    fn test_command_type_has_unified_fields() {
        let meta = get_step_type_metadata("command").expect("command metadata should exist");
        assert_eq!(meta.category, StepCategory::Core);
        let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name).collect();
        // Shell command fields
        assert!(field_names.contains(&"command"));
        assert!(field_names.contains(&"working_directory"));
        assert!(field_names.contains(&"fail_on_error"));
        // Check fields
        assert!(field_names.contains(&"check_type"));
        assert!(field_names.contains(&"auto_fix"));
        // Check group fields
        assert!(field_names.contains(&"check_group_id"));
    }

    #[test]
    fn test_ui_bridge_type_present() {
        let meta = get_step_type_metadata("ui_bridge").expect("ui_bridge metadata should exist");
        assert_eq!(meta.category, StepCategory::Automation);
        assert!(meta.allowed_phases.contains(&"setup"));
        assert!(meta.allowed_phases.contains(&"verification"));
        assert!(meta.allowed_phases.contains(&"completion"));
        let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name).collect();
        assert!(field_names.contains(&"action"));
        assert!(field_names.contains(&"url"));
        assert!(field_names.contains(&"instruction"));
        assert!(field_names.contains(&"target"));
        assert!(field_names.contains(&"assert_type"));
        assert!(field_names.contains(&"expected"));
        assert!(field_names.contains(&"timeout_ms"));
    }

    #[test]
    fn test_fields_have_descriptions() {
        let metadata = get_all_step_type_metadata();
        for meta in metadata {
            for field in meta.fields {
                assert!(
                    !field.description.is_empty(),
                    "Field '{}' in step type '{}' has empty description",
                    field.name,
                    meta.step_type
                );
            }
        }
    }
}
