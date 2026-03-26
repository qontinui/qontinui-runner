//! Agent Role Definitions Module
//!
//! Implements the role/goal/backstory pattern from CrewAI for specialized
//! agent behavior. Each agent role has a defined purpose, personality,
//! available tools, and constraints.
//!
//! ## Key Concepts
//!
//! - **Role**: The agent's job title/specialty (e.g., "Senior Debugger")
//! - **Goal**: What the agent is trying to achieve
//! - **Backstory**: Personality and approach context for the LLM
//! - **Tools**: What capabilities the role has access to
//! - **Constraints**: Boundaries the agent must respect
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::agent_roles::*;
//!
//! let debugger = AgentRole::debugger();
//! let prompt = debugger.build_system_prompt();
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Tool Specifications
// ============================================================================

/// Specification of a tool available to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique identifier for the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON schema for input parameters.
    pub input_schema: Option<serde_json::Value>,
    /// Whether this tool can modify state.
    pub is_mutating: bool,
    /// Risk level of the tool.
    pub risk_level: ToolRiskLevel,
}

/// Risk classification for tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolRiskLevel {
    /// Safe, read-only operations.
    #[default]
    Low,
    /// Potentially modifying but reversible.
    Medium,
    /// Significant changes, may be hard to reverse.
    High,
    /// Critical operations requiring explicit approval.
    Critical,
}

impl ToolSpec {
    /// Create a new tool specification.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: None,
            is_mutating: false,
            risk_level: ToolRiskLevel::Low,
        }
    }

    /// Mark this tool as mutating.
    pub fn mutating(mut self) -> Self {
        self.is_mutating = true;
        self
    }

    /// Set the risk level.
    pub fn with_risk(mut self, level: ToolRiskLevel) -> Self {
        self.risk_level = level;
        self
    }

    /// Set the input schema.
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }
}

// ============================================================================
// Model Tier
// ============================================================================

/// Model capability tier for role-based model selection.
///
/// Maps to the routing config's simple/medium/complex models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Fast, cheap model (e.g., Haiku) — for read-only analysis, simple tasks.
    Simple,
    /// Balanced model (e.g., Sonnet) — default for most roles.
    #[default]
    Medium,
    /// Powerful model (e.g., Opus) — for complex planning, architecture.
    Complex,
}

// ============================================================================
// Agent Role Definition
// ============================================================================

/// Complete definition of an agent's role, capabilities, and behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRole {
    /// Unique identifier for this role.
    pub id: String,
    /// The agent's job title/specialty.
    pub role: String,
    /// What the agent is trying to achieve.
    pub goal: String,
    /// Personality and approach context for the LLM.
    pub backstory: String,
    /// Tools this role has access to.
    pub tools: Vec<ToolSpec>,
    /// Boundaries the agent must respect.
    pub constraints: Vec<String>,
    /// Additional context or instructions.
    pub additional_context: Option<String>,
    /// Preferred communication style.
    pub communication_style: CommunicationStyle,
    /// How autonomous this role is.
    pub autonomy_level: AutonomyLevel,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Custom metadata.
    pub metadata: HashMap<String, serde_json::Value>,
    /// Tool whitelist — when `Some`, only these tools may be invoked.
    /// `None` means unrestricted (legacy roles use `tools` field instead).
    pub allowed_tools: Option<Vec<String>>,
    /// Preferred model capability tier for this role.
    pub preferred_model_tier: ModelTier,
    /// Maximum token budget for responses from this role.
    pub max_tokens_budget: Option<u32>,
}

/// Communication style preferences for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationStyle {
    /// Brief, to-the-point responses.
    Concise,
    /// Balanced detail level.
    #[default]
    Balanced,
    /// Detailed explanations and reasoning.
    Detailed,
    /// Technical with precise terminology.
    Technical,
    /// Educational, explaining concepts.
    Educational,
}

/// How much autonomy the agent has in decision-making.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    /// Must confirm every action.
    Supervised,
    /// Confirm significant actions only.
    #[default]
    Guided,
    /// Can act independently within constraints.
    Autonomous,
    /// Full autonomy, minimal oversight.
    FullAutonomy,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            role: "Assistant".to_string(),
            goal: "Help complete the assigned task".to_string(),
            backstory: "You are a helpful assistant.".to_string(),
            tools: Vec::new(),
            constraints: Vec::new(),
            additional_context: None,
            communication_style: CommunicationStyle::default(),
            autonomy_level: AutonomyLevel::default(),
            tags: Vec::new(),
            metadata: HashMap::new(),
            allowed_tools: None,
            preferred_model_tier: ModelTier::default(),
            max_tokens_budget: None,
        }
    }
}

impl AgentRole {
    /// Create a new agent role with basic information.
    pub fn new(id: impl Into<String>, role: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: role.into(),
            goal: goal.into(),
            ..Default::default()
        }
    }

    /// Set the backstory.
    pub fn with_backstory(mut self, backstory: impl Into<String>) -> Self {
        self.backstory = backstory.into();
        self
    }

    /// Add a tool to the role.
    pub fn with_tool(mut self, tool: ToolSpec) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add multiple tools.
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// Add a constraint.
    pub fn with_constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraints.push(constraint.into());
        self
    }

    /// Add multiple constraints.
    pub fn with_constraints(mut self, constraints: Vec<String>) -> Self {
        self.constraints.extend(constraints);
        self
    }

    /// Set additional context.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.additional_context = Some(context.into());
        self
    }

    /// Set communication style.
    pub fn with_style(mut self, style: CommunicationStyle) -> Self {
        self.communication_style = style;
        self
    }

    /// Set autonomy level.
    pub fn with_autonomy(mut self, level: AutonomyLevel) -> Self {
        self.autonomy_level = level;
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Set the tool whitelist (for role specializations).
    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = Some(tools);
        self
    }

    /// Set the preferred model tier.
    pub fn with_model_tier(mut self, tier: ModelTier) -> Self {
        self.preferred_model_tier = tier;
        self
    }

    /// Set the maximum token budget for responses.
    pub fn with_max_tokens_budget(mut self, budget: u32) -> Self {
        self.max_tokens_budget = Some(budget);
        self
    }

    /// Build a system prompt from this role definition.
    pub fn build_system_prompt(&self) -> String {
        let mut prompt = format!(
            "# Role: {}\n\n## Goal\n{}\n\n## Background\n{}\n",
            self.role, self.goal, self.backstory
        );

        if !self.constraints.is_empty() {
            prompt.push_str("\n## Constraints\n");
            for constraint in &self.constraints {
                prompt.push_str(&format!("- {}\n", constraint));
            }
        }

        if !self.tools.is_empty() {
            prompt.push_str("\n## Available Tools\n");
            for tool in &self.tools {
                prompt.push_str(&format!("- **{}**: {}\n", tool.name, tool.description));
            }
        }

        if let Some(context) = &self.additional_context {
            prompt.push_str(&format!("\n## Additional Context\n{}\n", context));
        }

        // Add communication style guidance
        prompt.push_str(&format!(
            "\n## Communication Style\n{}\n",
            self.communication_style.description()
        ));

        // Add autonomy guidance
        prompt.push_str(&format!(
            "\n## Decision Authority\n{}\n",
            self.autonomy_level.description()
        ));

        prompt
    }

    /// Check if this role can use a specific tool.
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        self.tools.iter().any(|t| t.name == tool_name)
    }

    /// Get all high-risk tools for this role.
    pub fn high_risk_tools(&self) -> Vec<&ToolSpec> {
        self.tools
            .iter()
            .filter(|t| matches!(t.risk_level, ToolRiskLevel::High | ToolRiskLevel::Critical))
            .collect()
    }
}

impl CommunicationStyle {
    /// Get a description of this communication style for prompts.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Concise => "Be brief and to the point. Avoid unnecessary elaboration.",
            Self::Balanced => "Provide appropriate detail without over-explaining.",
            Self::Detailed => "Explain your reasoning thoroughly. Show your work.",
            Self::Technical => "Use precise technical terminology. Be exact and specific.",
            Self::Educational => {
                "Explain concepts as you go. Help the reader understand the 'why'."
            }
        }
    }
}

impl AutonomyLevel {
    /// Get a description of this autonomy level for prompts.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Supervised => {
                "Confirm every action before proceeding. Do not make changes without explicit approval."
            }
            Self::Guided => {
                "Proceed with routine actions but confirm significant changes or decisions."
            }
            Self::Autonomous => {
                "Act independently within your constraints. Report results rather than asking for permission."
            }
            Self::FullAutonomy => {
                "You have full authority to complete the task. Use your judgment."
            }
        }
    }
}

// ============================================================================
// Predefined Roles
// ============================================================================

impl AgentRole {
    /// Create a Senior Debugger role.
    pub fn debugger() -> Self {
        Self::new(
            "debugger",
            "Senior Debugger",
            "Identify root causes of failures systematically",
        )
        .with_backstory(
            "You approach problems methodically, always checking logs first, then stack traces, \
             then code. You never guess - you verify. You document your findings as you go \
             so others can follow your reasoning.",
        )
        .with_tools(vec![
            ToolSpec::new("read_logs", "Read log files to find error patterns"),
            ToolSpec::new("analyze_stacktrace", "Parse and analyze stack traces"),
            ToolSpec::new("grep", "Search code for patterns"),
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("git_blame", "Check who changed code and when"),
        ])
        .with_constraints(vec![
            "Never modify code without identifying root cause first".to_string(),
            "Document findings before suggesting fixes".to_string(),
            "Check for recent changes that might have introduced the issue".to_string(),
        ])
        .with_style(CommunicationStyle::Technical)
        .with_autonomy(AutonomyLevel::Guided)
        .with_tag("debugging")
        .with_tag("analysis")
    }

    /// Create a Code Reviewer role.
    pub fn code_reviewer() -> Self {
        Self::new(
            "code_reviewer",
            "Senior Code Reviewer",
            "Review code changes for quality, correctness, and best practices",
        )
        .with_backstory(
            "You have years of experience reviewing code. You focus on what matters: \
             correctness, maintainability, and security. You give constructive feedback \
             that helps developers improve. You praise good patterns and explain why \
             certain approaches are problematic.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("git_diff", "View changes between versions"),
            ToolSpec::new("run_tests", "Execute test suites"),
            ToolSpec::new("run_linter", "Run static analysis tools"),
        ])
        .with_constraints(vec![
            "Focus on substance over style".to_string(),
            "Explain the 'why' behind suggestions".to_string(),
            "Acknowledge good patterns, not just problems".to_string(),
            "Consider the broader codebase context".to_string(),
        ])
        .with_style(CommunicationStyle::Educational)
        .with_autonomy(AutonomyLevel::Supervised)
        .with_tag("review")
        .with_tag("quality")
    }

    /// Create a Test Engineer role.
    pub fn test_engineer() -> Self {
        Self::new(
            "test_engineer",
            "QA Test Engineer",
            "Ensure comprehensive test coverage and identify edge cases",
        )
        .with_backstory(
            "You think like a breaker, not a maker. Your job is to find what can go wrong. \
             You consider edge cases, boundary conditions, error paths, and unexpected inputs. \
             You write tests that will catch regressions before they reach production.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("write_file", "Write test files")
                .mutating()
                .with_risk(ToolRiskLevel::Medium),
            ToolSpec::new("run_tests", "Execute test suites"),
            ToolSpec::new("coverage_report", "Generate coverage reports"),
        ])
        .with_constraints(vec![
            "Test behavior, not implementation".to_string(),
            "Each test should test one thing".to_string(),
            "Tests must be deterministic".to_string(),
            "Consider both happy path and error cases".to_string(),
        ])
        .with_style(CommunicationStyle::Technical)
        .with_autonomy(AutonomyLevel::Autonomous)
        .with_tag("testing")
        .with_tag("qa")
    }

    /// Create an Architect role.
    pub fn architect() -> Self {
        Self::new(
            "architect",
            "Software Architect",
            "Design robust, scalable, and maintainable system architectures",
        )
        .with_backstory(
            "You see the big picture. You understand how components interact, where \
             bottlenecks will emerge, and how requirements will evolve. You balance \
             immediate needs against future flexibility. You communicate tradeoffs clearly.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("analyze_dependencies", "Map module dependencies"),
            ToolSpec::new("diagram", "Generate architecture diagrams"),
            ToolSpec::new("grep", "Search code for patterns"),
        ])
        .with_constraints(vec![
            "Consider scalability implications".to_string(),
            "Prefer composition over inheritance".to_string(),
            "Minimize coupling between components".to_string(),
            "Document architectural decisions and rationale".to_string(),
        ])
        .with_style(CommunicationStyle::Detailed)
        .with_autonomy(AutonomyLevel::Supervised)
        .with_tag("architecture")
        .with_tag("design")
    }

    /// Create a Refactoring Specialist role.
    pub fn refactorer() -> Self {
        Self::new(
            "refactorer",
            "Refactoring Specialist",
            "Improve code structure without changing behavior",
        )
        .with_backstory(
            "You are a code surgeon. You make precise, incremental changes that improve \
             the codebase. You never refactor without tests to verify behavior is preserved. \
             You follow established refactoring patterns and commit frequently.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("write_file", "Write modified files")
                .mutating()
                .with_risk(ToolRiskLevel::Medium),
            ToolSpec::new("run_tests", "Execute test suites"),
            ToolSpec::new("rename_symbol", "Rename across codebase")
                .mutating()
                .with_risk(ToolRiskLevel::Medium),
            ToolSpec::new("extract_function", "Extract code to new function")
                .mutating()
                .with_risk(ToolRiskLevel::Medium),
        ])
        .with_constraints(vec![
            "Never refactor without passing tests first".to_string(),
            "Make small, incremental changes".to_string(),
            "Commit after each successful refactoring step".to_string(),
            "Preserve existing behavior exactly".to_string(),
        ])
        .with_style(CommunicationStyle::Concise)
        .with_autonomy(AutonomyLevel::Autonomous)
        .with_tag("refactoring")
        .with_tag("cleanup")
    }

    /// Create a Security Analyst role.
    pub fn security_analyst() -> Self {
        Self::new(
            "security_analyst",
            "Security Analyst",
            "Identify and remediate security vulnerabilities",
        )
        .with_backstory(
            "You think like an attacker to defend like a pro. You know the OWASP Top 10 \
             and common vulnerability patterns. You verify findings before reporting them \
             and suggest concrete mitigations. You prioritize by exploitability and impact.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("grep", "Search code for patterns"),
            ToolSpec::new("security_scan", "Run security scanners"),
            ToolSpec::new("dependency_audit", "Check for vulnerable dependencies"),
        ])
        .with_constraints(vec![
            "Never expose actual credentials or secrets in reports".to_string(),
            "Verify vulnerabilities before reporting".to_string(),
            "Include remediation steps with each finding".to_string(),
            "Classify severity accurately (CVSS if applicable)".to_string(),
        ])
        .with_style(CommunicationStyle::Technical)
        .with_autonomy(AutonomyLevel::Guided)
        .with_tag("security")
        .with_tag("audit")
    }

    /// Create a Documentation Writer role.
    pub fn documentation_writer() -> Self {
        Self::new(
            "documentation_writer",
            "Technical Writer",
            "Create clear, accurate, and helpful documentation",
        )
        .with_backstory(
            "You make complex things understandable. You write for your audience, not \
             yourself. You use examples liberally. You keep docs in sync with code and \
             remove outdated information. You structure content for discoverability.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("write_file", "Write documentation files")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
            ToolSpec::new("run_docs", "Build and preview documentation"),
        ])
        .with_constraints(vec![
            "Examples must be tested and working".to_string(),
            "Use consistent terminology".to_string(),
            "Link to related documentation".to_string(),
            "Keep explanations concise but complete".to_string(),
        ])
        .with_style(CommunicationStyle::Educational)
        .with_autonomy(AutonomyLevel::Autonomous)
        .with_tag("documentation")
        .with_tag("writing")
    }

    /// Create a Performance Engineer role.
    pub fn performance_engineer() -> Self {
        Self::new(
            "performance_engineer",
            "Performance Engineer",
            "Identify and resolve performance bottlenecks",
        )
        .with_backstory(
            "You measure before you optimize. You understand that premature optimization \
             is the root of all evil, but you also know when performance matters. You use \
             profilers and benchmarks to find real bottlenecks, not imagined ones.",
        )
        .with_tools(vec![
            ToolSpec::new("read_file", "Read source files"),
            ToolSpec::new("profile", "Run performance profiler"),
            ToolSpec::new("benchmark", "Run benchmarks"),
            ToolSpec::new("analyze_complexity", "Analyze algorithmic complexity"),
            ToolSpec::new("write_file", "Write optimized code")
                .mutating()
                .with_risk(ToolRiskLevel::Medium),
        ])
        .with_constraints(vec![
            "Measure before and after any optimization".to_string(),
            "Document performance characteristics".to_string(),
            "Consider memory usage, not just CPU time".to_string(),
            "Prioritize hot paths over cold paths".to_string(),
        ])
        .with_style(CommunicationStyle::Technical)
        .with_autonomy(AutonomyLevel::Guided)
        .with_tag("performance")
        .with_tag("optimization")
    }

    // ========================================================================
    // Vision Agent Roles (TuriX-CUA inspired Brain/Actor pattern)
    // ========================================================================

    /// Create a Brain role for visual UI analysis.
    ///
    /// The Brain is a vision-capable model that receives annotated screenshots
    /// and element indices. It analyzes the current screen state, assesses
    /// progress, and produces an action plan for the Actor.
    ///
    /// Typically uses ModelTier::Complex (Opus, GPT-4V, Gemini Pro Vision).
    pub fn brain() -> Self {
        Self::new(
            "brain",
            "Visual Analysis Brain",
            "Analyze current screen state, identify interactive elements, assess progress, and produce an action plan",
        )
        .with_backstory(
            "You are a visual UI analysis specialist. You receive annotated screenshots with \
             numbered element markers and a text index of all interactive elements. Your job \
             is to understand the current state of the application, assess whether previous \
             actions succeeded, and produce a clear action plan for the next steps. Reference \
             elements by their index number (e.g., 'click element [3]'). Be precise about \
             what you observe and what should happen next.",
        )
        .with_tools(vec![
            ToolSpec::new("analyze_screenshot", "Analyze an annotated screenshot with numbered elements"),
            ToolSpec::new("read_element_index", "Read the text index of screen elements"),
            ToolSpec::new("record_info", "Save a finding for later retrieval"),
        ])
        .with_constraints(vec![
            "Always reference elements by their index number".to_string(),
            "Describe what you observe before suggesting actions".to_string(),
            "Assess whether the previous action succeeded or failed".to_string(),
            "Produce a clear, ordered action plan".to_string(),
        ])
        .with_style(CommunicationStyle::Detailed)
        .with_autonomy(AutonomyLevel::Autonomous)
        .with_model_tier(ModelTier::Complex)
        .with_tag("vision")
        .with_tag("analysis")
        .with_tag("brain")
    }

    /// Create an Actor role for UI action execution.
    ///
    /// The Actor receives the Brain's action plan (text only, no images) and
    /// translates it into concrete UI actions. It's a lightweight model
    /// optimized for fast, accurate action translation.
    ///
    /// Typically uses ModelTier::Simple (Haiku, Sonnet, Gemini Flash).
    pub fn actor() -> Self {
        Self::new(
            "actor",
            "UI Action Actor",
            "Execute the action plan by translating goals into precise UI actions",
        )
        .with_backstory(
            "You are a UI action execution specialist. You receive an action plan from the \
             Brain agent that describes what needs to happen, along with an element index \
             listing all interactive elements by number. Your job is to translate each plan \
             step into concrete UI actions: click, type, scroll, select, etc. Be precise \
             with element references and action parameters. If you are confused or the plan \
             seems wrong, signal RequestContext so the Brain can re-analyze.",
        )
        .with_tools(vec![
            ToolSpec::new("click", "Click on a UI element by index number")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
            ToolSpec::new("type_text", "Type text into an input element")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
            ToolSpec::new("scroll", "Scroll in a direction")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
            ToolSpec::new("select", "Select an option from a dropdown")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
            ToolSpec::new("wait", "Wait for a condition before proceeding"),
            ToolSpec::new("record_info", "Save intermediate information for later retrieval")
                .mutating()
                .with_risk(ToolRiskLevel::Low),
        ])
        .with_constraints(vec![
            "Follow the Brain's action plan in order".to_string(),
            "Reference elements by their index number from the element index".to_string(),
            "Signal RequestContext if the plan seems wrong or the screen has changed".to_string(),
            "Do not guess — if uncertain, request re-analysis".to_string(),
        ])
        .with_style(CommunicationStyle::Concise)
        .with_autonomy(AutonomyLevel::Guided)
        .with_model_tier(ModelTier::Simple)
        .with_tag("action")
        .with_tag("execution")
        .with_tag("actor")
    }
}

// ============================================================================
// Role Registry
// ============================================================================

/// Registry of available agent roles.
#[derive(Debug, Default)]
pub struct RoleRegistry {
    roles: HashMap<String, AgentRole>,
}

impl RoleRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with all predefined roles.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(AgentRole::debugger());
        registry.register(AgentRole::code_reviewer());
        registry.register(AgentRole::test_engineer());
        registry.register(AgentRole::architect());
        registry.register(AgentRole::refactorer());
        registry.register(AgentRole::security_analyst());
        registry.register(AgentRole::documentation_writer());
        registry.register(AgentRole::performance_engineer());
        registry.register(AgentRole::brain());
        registry.register(AgentRole::actor());
        registry
    }

    /// Register a role.
    pub fn register(&mut self, role: AgentRole) {
        self.roles.insert(role.id.clone(), role);
    }

    /// Get a role by ID.
    pub fn get(&self, id: &str) -> Option<&AgentRole> {
        self.roles.get(id)
    }

    /// List all registered role IDs.
    pub fn list_ids(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }

    /// Find roles by tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<&AgentRole> {
        self.roles
            .values()
            .filter(|r| r.tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Get all roles.
    pub fn all(&self) -> Vec<&AgentRole> {
        self.roles.values().collect()
    }
}

// ============================================================================
// Role Selection
// ============================================================================

/// Suggest appropriate roles for a given task.
pub fn suggest_roles_for_task(task_description: &str) -> Vec<&'static str> {
    let lower = task_description.to_lowercase();
    let mut suggestions = Vec::new();

    // Keyword matching for role suggestions
    if lower.contains("debug")
        || lower.contains("error")
        || lower.contains("bug")
        || lower.contains("fix")
    {
        suggestions.push("debugger");
    }

    if lower.contains("review") || lower.contains("pr") || lower.contains("pull request") {
        suggestions.push("code_reviewer");
    }

    if lower.contains("test") || lower.contains("coverage") || lower.contains("qa") {
        suggestions.push("test_engineer");
    }

    if lower.contains("architect")
        || lower.contains("design")
        || lower.contains("structure")
        || lower.contains("scale")
    {
        suggestions.push("architect");
    }

    if lower.contains("refactor") || lower.contains("cleanup") || lower.contains("reorganize") {
        suggestions.push("refactorer");
    }

    if lower.contains("security")
        || lower.contains("vulnerab")
        || lower.contains("owasp")
        || lower.contains("audit")
    {
        suggestions.push("security_analyst");
    }

    if lower.contains("doc") || lower.contains("readme") || lower.contains("explain") {
        suggestions.push("documentation_writer");
    }

    if lower.contains("perf") || lower.contains("slow") || lower.contains("optim") {
        suggestions.push("performance_engineer");
    }

    suggestions
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_creation() {
        let role = AgentRole::new("test", "Test Role", "Do testing things");
        assert_eq!(role.id, "test");
        assert_eq!(role.role, "Test Role");
        assert_eq!(role.goal, "Do testing things");
    }

    #[test]
    fn test_role_builder() {
        let role = AgentRole::new("custom", "Custom Role", "Custom goal")
            .with_backstory("Custom backstory")
            .with_tool(ToolSpec::new("tool1", "First tool"))
            .with_constraint("Must do X".to_string())
            .with_style(CommunicationStyle::Technical)
            .with_autonomy(AutonomyLevel::Autonomous);

        assert_eq!(role.backstory, "Custom backstory");
        assert_eq!(role.tools.len(), 1);
        assert_eq!(role.constraints.len(), 1);
        assert_eq!(role.communication_style, CommunicationStyle::Technical);
        assert_eq!(role.autonomy_level, AutonomyLevel::Autonomous);
    }

    #[test]
    fn test_predefined_roles() {
        let debugger = AgentRole::debugger();
        assert_eq!(debugger.id, "debugger");
        assert!(!debugger.tools.is_empty());
        assert!(!debugger.constraints.is_empty());

        let reviewer = AgentRole::code_reviewer();
        assert_eq!(reviewer.id, "code_reviewer");
    }

    #[test]
    fn test_system_prompt_generation() {
        let role = AgentRole::debugger();
        let prompt = role.build_system_prompt();

        assert!(prompt.contains("Senior Debugger"));
        assert!(prompt.contains("root cause"));
        assert!(prompt.contains("Constraints"));
        assert!(prompt.contains("Available Tools"));
    }

    #[test]
    fn test_role_registry() {
        let registry = RoleRegistry::with_defaults();

        assert!(registry.get("debugger").is_some());
        assert!(registry.get("code_reviewer").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_find_by_tag() {
        let registry = RoleRegistry::with_defaults();
        let debugging_roles = registry.find_by_tag("debugging");

        assert!(!debugging_roles.is_empty());
        assert!(debugging_roles.iter().any(|r| r.id == "debugger"));
    }

    #[test]
    fn test_suggest_roles() {
        let suggestions = suggest_roles_for_task("Fix the bug in the login module");
        assert!(suggestions.contains(&"debugger"));

        let suggestions = suggest_roles_for_task("Review the PR for the new feature");
        assert!(suggestions.contains(&"code_reviewer"));

        let suggestions = suggest_roles_for_task("Write tests for the API");
        assert!(suggestions.contains(&"test_engineer"));
    }

    #[test]
    fn test_can_use_tool() {
        let role = AgentRole::debugger();
        assert!(role.can_use_tool("read_logs"));
        assert!(!role.can_use_tool("deploy_to_production"));
    }

    #[test]
    fn test_high_risk_tools() {
        let role = AgentRole::new("risky", "Risky Role", "Do risky things")
            .with_tool(ToolSpec::new("safe", "Safe tool"))
            .with_tool(
                ToolSpec::new("dangerous", "Dangerous tool").with_risk(ToolRiskLevel::Critical),
            );

        let high_risk = role.high_risk_tools();
        assert_eq!(high_risk.len(), 1);
        assert_eq!(high_risk[0].name, "dangerous");
    }
}
