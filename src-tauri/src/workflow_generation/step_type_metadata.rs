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
    WebApp,
    Gui,
    Awas,
    Utility,
}

impl StepCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepCategory::Core => "Core",
            StepCategory::Verification => "Verification",
            StepCategory::WebApp => "WebApp",
            StepCategory::Gui => "GUI",
            StepCategory::Awas => "AWAS",
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

static SCRIPT_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "code",
        field_type: FieldType::String,
        required: true,
        description: "Playwright TypeScript code",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "target_url",
        field_type: FieldType::String,
        required: false,
        description: "Starting URL (optional)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "refinement_enabled",
        field_type: FieldType::Boolean,
        required: false,
        description: "Enable script refinement",
        enum_values: &[],
        default: "true",
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

static CHECK_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "check_type",
        field_type: FieldType::String,
        required: true,
        description: "Type of check to run",
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
        name: "command",
        field_type: FieldType::String,
        required: false,
        description: "Command to run (for lint/format/typecheck/analyze/security/custom_command)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "working_directory",
        field_type: FieldType::String,
        required: false,
        description: "Working directory for the command, or git repo root for ci_cd auto-detection",
        enum_values: &[],
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
        description: "Wait for in-progress CI runs to complete instead of failing immediately (for ci_cd)",
        enum_values: &[],
        default: "false",
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

static SHELL_COMMAND_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "command",
        field_type: FieldType::String,
        required: true,
        description: "Shell command to execute",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "working_directory",
        field_type: FieldType::String,
        required: false,
        description: "Working directory for the command",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "timeout_seconds",
        field_type: FieldType::Number,
        required: false,
        description: "Timeout in seconds",
        enum_values: &[],
        default: "60",
    },
    StepTypeFieldDef {
        name: "fail_on_error",
        field_type: FieldType::Boolean,
        required: false,
        description: "Fail workflow if command exits with non-zero",
        enum_values: &[],
        default: "true",
    },
];

static API_REQUEST_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "method",
        field_type: FieldType::String,
        required: true,
        description: "HTTP method",
        enum_values: &["GET", "POST", "PUT", "PATCH", "DELETE"],
        default: "",
    },
    StepTypeFieldDef {
        name: "url",
        field_type: FieldType::String,
        required: true,
        description: "Request URL",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "headers",
        field_type: FieldType::Object,
        required: false,
        description: "Request headers as key-value pairs",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "body",
        field_type: FieldType::String,
        required: false,
        description: "Request body",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "content_type",
        field_type: FieldType::String,
        required: false,
        description: "Content type for body",
        enum_values: &["application/json", "text/plain", "none"],
        default: "",
    },
    StepTypeFieldDef {
        name: "extractions",
        field_type: FieldType::Array,
        required: false,
        description: "Variable extractions from response: [{\"variable_name\": \"...\", \"json_path\": \"$...\"}]",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "assertions",
        field_type: FieldType::Array,
        required: false,
        description: "Response assertions: [{\"type\": \"<assertion_type>\", \"expected\": <value>}]. Valid assertion types: \"status_code\" (compare HTTP status), \"body_contains\" (check body contains string), \"json_path\" (extract and compare via JSON path, requires \"json_path\" field), \"header\" (check response header, requires \"header_name\" field), \"response_time\" (verify response time in ms). ONLY these 5 types exist — do NOT use \"body_not_contains\" or any other unlisted type. Supported operators (optional \"operator\" field, default \"equals\"): \"equals\", \"contains\", \"matches\" (regex), \"greater_than\", \"less_than\".",
        enum_values: &[],
        default: "",
    },
];

static SCREENSHOT_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "delay_ms",
        field_type: FieldType::Number,
        required: false,
        description: "Delay before capture in milliseconds",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "monitor",
        field_type: FieldType::String,
        required: false,
        description: "Monitor to capture",
        enum_values: &["all", "primary", "left", "right"],
        default: "",
    },
];

static GUI_ACTION_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "action",
        field_type: FieldType::String,
        required: true,
        description: "Action to perform",
        enum_values: &["click", "double_click", "right_click", "type", "hotkey", "scroll"],
        default: "",
    },
    StepTypeFieldDef {
        name: "target_image_ids",
        field_type: FieldType::StringArray,
        required: false,
        description: "Target image UUIDs for visual matching",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "text_input",
        field_type: FieldType::String,
        required: false,
        description: "Text to type (for type action)",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "hotkey",
        field_type: FieldType::String,
        required: false,
        description: "Hotkey to press (e.g., 'ctrl+s')",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "scroll_direction",
        field_type: FieldType::String,
        required: false,
        description: "Scroll direction",
        enum_values: &["up", "down"],
        default: "",
    },
];

static STATE_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "state_id",
        field_type: FieldType::String,
        required: true,
        description: "Stored application state UUID",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "state_name",
        field_type: FieldType::String,
        required: false,
        description: "Display name for the state",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "timeout_seconds",
        field_type: FieldType::Number,
        required: false,
        description: "Timeout in seconds",
        enum_values: &[],
        default: "",
    },
];

static WORKFLOW_REF_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "workflow_id",
        field_type: FieldType::String,
        required: true,
        description: "Referenced workflow UUID",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "workflow_name",
        field_type: FieldType::String,
        required: false,
        description: "Display name for the workflow",
        enum_values: &[],
        default: "",
    },
];

static MCP_CALL_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "server_id",
        field_type: FieldType::String,
        required: true,
        description: "MCP server identifier",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "tool_name",
        field_type: FieldType::String,
        required: true,
        description: "Tool name on the MCP server",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "arguments",
        field_type: FieldType::Object,
        required: false,
        description: "Tool arguments as key-value pairs",
        enum_values: &[],
        default: "",
    },
];

static SPEC_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "spec_group",
        field_type: FieldType::Object,
        required: true,
        description: "Spec group with name and specs array: {\"name\": \"...\", \"specs\": [{\"element_id\": \"...\", \"assertions\": [...]}]}",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "element_source",
        field_type: FieldType::String,
        required: false,
        description: "Element source",
        enum_values: &["control", "external"],
        default: "",
    },
];

static GATE_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "required_steps",
    field_type: FieldType::StringArray,
    required: true,
    description: "IDs of verification steps that must pass",
    enum_values: &[],
    default: "",
}];

static CHECK_GROUP_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "check_group_id",
    field_type: FieldType::String,
    required: true,
    description: "Saved check group UUID",
    enum_values: &[],
    default: "",
}];

static MACRO_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "macro_id",
    field_type: FieldType::String,
    required: true,
    description: "Saved action macro UUID",
    enum_values: &[],
    default: "",
}];

static AWAS_DISCOVER_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "url",
        field_type: FieldType::String,
        required: true,
        description: "URL of webpage to discover actions on",
        enum_values: &[],
        default: "",
    },
];

static AWAS_EXECUTE_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "action_id",
        field_type: FieldType::String,
        required: true,
        description: "ID of AWAS action to execute",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "parameters",
        field_type: FieldType::Object,
        required: false,
        description: "Action parameters",
        enum_values: &[],
        default: "",
    },
];

static AWAS_CHECK_SUPPORT_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "url",
    field_type: FieldType::String,
    required: true,
    description: "URL to check AWAS support for",
    enum_values: &[],
    default: "",
}];

static AWAS_LIST_ACTIONS_FIELDS: &[StepTypeFieldDef] = &[StepTypeFieldDef {
    name: "url",
    field_type: FieldType::String,
    required: false,
    description: "URL to list AWAS actions for (uses last discovery if omitted)",
    enum_values: &[],
    default: "",
}];

static AWAS_EXTRACT_ELEMENTS_FIELDS: &[StepTypeFieldDef] = &[
    StepTypeFieldDef {
        name: "url",
        field_type: FieldType::String,
        required: true,
        description: "URL to extract elements from",
        enum_values: &[],
        default: "",
    },
    StepTypeFieldDef {
        name: "selector",
        field_type: FieldType::String,
        required: false,
        description: "CSS selector to filter elements",
        enum_values: &[],
        default: "",
    },
];

// ============================================================================
// All step type metadata entries
// ============================================================================

static ALL_METADATA: Lazy<Vec<StepTypeMetadata>> = Lazy::new(|| {
    vec![
        // Core step types
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
        StepTypeMetadata {
            step_type: "shell_command",
            display_name: "Shell Command",
            description: "Execute a shell command",
            category: StepCategory::Core,
            allowed_phases: &["setup", "completion"],
            icon: "Terminal",
            color: "gray",
            fields: SHELL_COMMAND_FIELDS,
        },
        StepTypeMetadata {
            step_type: "api_request",
            display_name: "API Request",
            description: "HTTP API call with variable extraction and assertions",
            category: StepCategory::Core,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "Globe",
            color: "blue",
            fields: API_REQUEST_FIELDS,
        },
        // Verification step types
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
            step_type: "check",
            display_name: "Check",
            description: "Code quality check (lint, format, typecheck, analyze, security, custom_command) or CI/CD pipeline status check (ci_cd) via GitHub Actions",
            category: StepCategory::Verification,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "CheckCircle",
            color: "green",
            fields: CHECK_FIELDS,
        },
        StepTypeMetadata {
            step_type: "gate",
            display_name: "Gate",
            description: "Aggregates verification results; blocks agentic loop if all pass",
            category: StepCategory::Verification,
            allowed_phases: &["verification"],
            icon: "ShieldCheck",
            color: "amber",
            fields: GATE_FIELDS,
        },
        StepTypeMetadata {
            step_type: "check_group",
            display_name: "Check Group",
            description: "Run a saved check group by ID",
            category: StepCategory::Verification,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "ListChecks",
            color: "green",
            fields: CHECK_GROUP_FIELDS,
        },
        StepTypeMetadata {
            step_type: "spec",
            display_name: "Spec",
            description: "UI Bridge spec assertions against live elements",
            category: StepCategory::Verification,
            allowed_phases: &["verification"],
            icon: "FileCheck",
            color: "teal",
            fields: SPEC_FIELDS,
        },
        // Web app step types
        StepTypeMetadata {
            step_type: "script",
            display_name: "Script",
            description: "Playwright browser automation script",
            category: StepCategory::WebApp,
            allowed_phases: &["setup", "completion"],
            icon: "Code",
            color: "orange",
            fields: SCRIPT_FIELDS,
        },
        StepTypeMetadata {
            step_type: "screenshot",
            display_name: "Screenshot",
            description: "Capture screen state for AI analysis",
            category: StepCategory::WebApp,
            allowed_phases: &["verification"],
            icon: "Camera",
            color: "pink",
            fields: SCREENSHOT_FIELDS,
        },
        // GUI step types
        StepTypeMetadata {
            step_type: "gui_action",
            display_name: "GUI Action",
            description: "Mouse and keyboard automation",
            category: StepCategory::Gui,
            allowed_phases: &["setup", "verification"],
            icon: "MousePointer",
            color: "indigo",
            fields: GUI_ACTION_FIELDS,
        },
        StepTypeMetadata {
            step_type: "state",
            display_name: "State",
            description: "Navigate to stored application state",
            category: StepCategory::Gui,
            allowed_phases: &["setup", "verification"],
            icon: "Layers",
            color: "cyan",
            fields: STATE_FIELDS,
        },
        StepTypeMetadata {
            step_type: "workflow_ref",
            display_name: "Workflow Reference",
            description: "Execute another workflow",
            category: StepCategory::Gui,
            allowed_phases: &["setup", "verification"],
            icon: "GitBranch",
            color: "violet",
            fields: WORKFLOW_REF_FIELDS,
        },
        // AWAS step types
        StepTypeMetadata {
            step_type: "awas_discover",
            display_name: "AWAS Discover",
            description: "Discover available actions on a webpage",
            category: StepCategory::Awas,
            allowed_phases: &["setup"],
            icon: "Search",
            color: "emerald",
            fields: AWAS_DISCOVER_FIELDS,
        },
        StepTypeMetadata {
            step_type: "awas_execute",
            display_name: "AWAS Execute",
            description: "Execute an AWAS action",
            category: StepCategory::Awas,
            allowed_phases: &["setup", "verification"],
            icon: "Play",
            color: "emerald",
            fields: AWAS_EXECUTE_FIELDS,
        },
        StepTypeMetadata {
            step_type: "awas_check_support",
            display_name: "AWAS Check Support",
            description: "Check if AWAS is supported for a URL",
            category: StepCategory::Awas,
            allowed_phases: &["setup"],
            icon: "HelpCircle",
            color: "emerald",
            fields: AWAS_CHECK_SUPPORT_FIELDS,
        },
        StepTypeMetadata {
            step_type: "awas_list_actions",
            display_name: "AWAS List Actions",
            description: "List available AWAS actions",
            category: StepCategory::Awas,
            allowed_phases: &["setup", "verification"],
            icon: "List",
            color: "emerald",
            fields: AWAS_LIST_ACTIONS_FIELDS,
        },
        StepTypeMetadata {
            step_type: "awas_extract_elements",
            display_name: "AWAS Extract Elements",
            description: "Extract elements from HTML",
            category: StepCategory::Awas,
            allowed_phases: &["verification"],
            icon: "Code2",
            color: "emerald",
            fields: AWAS_EXTRACT_ELEMENTS_FIELDS,
        },
        // Utility step types
        StepTypeMetadata {
            step_type: "mcp_call",
            display_name: "MCP Call",
            description: "Call MCP server tool",
            category: StepCategory::Utility,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "Plug",
            color: "slate",
            fields: MCP_CALL_FIELDS,
        },
        StepTypeMetadata {
            step_type: "macro",
            display_name: "Macro",
            description: "Run a saved action macro by ID",
            category: StepCategory::Utility,
            allowed_phases: &["setup", "verification", "completion"],
            icon: "Repeat",
            color: "zinc",
            fields: MACRO_FIELDS,
        },
    ]
});

/// Returns metadata for all step types that AI can generate.
///
/// This excludes internal/system step types like `log_watch`, `error_resolved`,
/// and `save_workflow_artifact` which are system-generated, not user-created.
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
        // Should have all 20 AI-generatable step types
        assert!(
            metadata.len() >= 18,
            "Expected at least 18 step types, got {}",
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
                // Every type allowed in validation should have metadata
                // (except internal types not exposed to AI)
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
        assert!(get_step_type_metadata("prompt").is_some());
        assert!(get_step_type_metadata("shell_command").is_some());
        assert!(get_step_type_metadata("api_request").is_some());
    }

    #[test]
    fn test_verification_types_present() {
        assert!(get_step_type_metadata("test").is_some());
        assert!(get_step_type_metadata("check").is_some());
        assert!(get_step_type_metadata("gate").is_some());
        assert!(get_step_type_metadata("spec").is_some());
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
