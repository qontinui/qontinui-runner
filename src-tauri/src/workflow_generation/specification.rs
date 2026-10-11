//! Specification Agent — Pre-Generation Acceptance Criteria
//!
//! Runs before the Builder Agent to define structured acceptance criteria that
//! guide verification step generation. Forces the AI to think about observable
//! success conditions before any implementation steps are created.
//!
//! The pipeline becomes:
//!   Discovery → Investigation → **Specification** → Builder → Autofix → [Verify↔Fix] → Hardener → Validate

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::str_utils::truncate_str;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// Types
// ============================================================================

/// How a criterion should be verified.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Shell command (exit code, stdout check)
    #[default]
    Command,
    /// UI Bridge SDK assertion
    UiBridge,
    /// Test runner (Playwright, pytest, etc.)
    Test,
    /// Cannot be automated — human review needed
    Manual,
}

/// Priority level for an acceptance criterion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CriterionPriority {
    /// Must pass for the workflow to succeed
    #[default]
    Critical,
    /// Should pass, but workflow can proceed without it
    Important,
    /// Nice-to-have verification
    Optional,
}

/// EARS requirement category (Easy Approach to Requirements Syntax).
/// Categorizes criteria by their trigger/condition pattern for better verification step generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EarsCategory {
    /// Always true: "The system shall [action]"
    Ubiquitous,
    /// Triggered by event: "When [event], the system shall [action]"
    EventDriven,
    /// Depends on state: "While [state], the system shall [action]"
    StateDriven,
    /// Feature toggle: "Where [feature], the system shall [action]"
    Optional,
    /// Combined conditions: "While [state], when [event], the system shall [action]"
    Complex,
    /// Unwanted behaviour: "If [trigger], then the system shall [response]" —
    /// how the system must respond to an undesired situation (a failure, an
    /// invalid input, a lost connection). Serialized `unwanted_behavior`; the
    /// British spelling EARS itself uses is accepted on input.
    #[serde(alias = "unwanted_behaviour")]
    UnwantedBehavior,
    /// Not applicable (legacy criteria without EARS classification)
    #[default]
    #[serde(other)]
    NotApplicable,
}

impl EarsCategory {
    /// Every classified category, in the order the prompts list them.
    /// `NotApplicable` is the absence of a classification, not a category.
    pub const CLASSIFIED: [EarsCategory; 6] = [
        EarsCategory::Ubiquitous,
        EarsCategory::EventDriven,
        EarsCategory::StateDriven,
        EarsCategory::Optional,
        EarsCategory::Complex,
        EarsCategory::UnwantedBehavior,
    ];

    /// The wire name — identical to the serde serialization — or `"n/a"` for
    /// an unclassified criterion.
    pub fn as_str(&self) -> &'static str {
        match self {
            EarsCategory::Ubiquitous => "ubiquitous",
            EarsCategory::EventDriven => "event_driven",
            EarsCategory::StateDriven => "state_driven",
            EarsCategory::Optional => "optional",
            EarsCategory::Complex => "complex",
            EarsCategory::UnwantedBehavior => "unwanted_behavior",
            EarsCategory::NotApplicable => "n/a",
        }
    }

    /// Whether a criterion of this category names a trigger (an event, a
    /// state, a feature or an unwanted condition) that verification has to
    /// set up before it can assert anything.
    pub fn requires_trigger(&self) -> bool {
        !matches!(self, EarsCategory::Ubiquitous | EarsCategory::NotApplicable)
    }

    /// Render a criterion as its EARS sentence:
    /// `If <trigger>, then the <system> shall <action>` and so on.
    ///
    /// A missing trigger on a category that needs one renders as
    /// `(trigger not specified)` rather than collapsing into the ubiquitous
    /// form, so an incomplete criterion reads as incomplete. `Complex` takes
    /// its trigger verbatim — it already carries its own "While …, when …".
    /// `NotApplicable` renders the action alone.
    pub fn render(&self, system: &str, trigger: Option<&str>, action: &str) -> String {
        let trigger = trigger
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("(trigger not specified)");
        match self {
            EarsCategory::Ubiquitous => format!("The {system} shall {action}"),
            EarsCategory::EventDriven => format!("When {trigger}, the {system} shall {action}"),
            EarsCategory::StateDriven => format!("While {trigger}, the {system} shall {action}"),
            EarsCategory::Optional => format!("Where {trigger}, the {system} shall {action}"),
            EarsCategory::Complex => format!("{trigger}, the {system} shall {action}"),
            EarsCategory::UnwantedBehavior => {
                format!("If {trigger}, then the {system} shall {action}")
            }
            EarsCategory::NotApplicable => action.to_string(),
        }
    }

    /// Classify a requirement sentence by its EARS template, ignoring case,
    /// leading whitespace and a list prefix (`1.`, `2)`, `-`, `*`, `•`).
    ///
    /// Every pattern requires `shall` — a sentence with no `shall` is not an
    /// EARS requirement. The sentence is split into comma-separated clauses;
    /// the opening keyword is the first word of the first clause, and it must
    /// be followed by at least one trigger word.
    ///
    /// * **If** → [`EarsCategory::UnwantedBehavior`] when a `then` completes
    ///   the template, in one of two places:
    ///   - **with commas:** some clause after the first, up to and including
    ///     the clause holding `shall`, OPENS with `then` —
    ///     `If <trigger>, then the <system> shall …`, and also
    ///     `If <trigger>, then <adverbial>, the <system> shall …`;
    ///   - **without commas** (the whole sentence is one clause): a `then` at
    ///     the third word or later, not directly after `and`/`or`/`but`, with
    ///     at least one word between it and `shall`. Nothing checks that what
    ///     follows the `then` is a subject, so a comma-less
    ///     "If the user logs in then logs out the session shall end" still
    ///     reads as unwanted behaviour.
    ///
    ///   Otherwise an `If` sentence is [`EarsCategory::NotApplicable`] — e.g.
    ///   "If the user logs in and then logs out, the session shall end",
    ///   whose `then` sits inside the trigger clause instead of opening one.
    /// * **While** → [`EarsCategory::Complex`] when a `when` comes before
    ///   `shall`: opening a later clause (`While <state>, when <event>, …`),
    ///   or inside the first clause at the third word or later, not directly
    ///   after a conjunction, with a word after it
    ///   (`While <state> when <event>, the <system> shall …`, with or
    ///   without the second comma). A `when` after `shall` belongs to the
    ///   response, so "While uploading, the system shall show progress when
    ///   asked" stays [`EarsCategory::StateDriven`], as does any other
    ///   `While` sentence.
    /// * **When** → [`EarsCategory::EventDriven`]; **Where** →
    ///   [`EarsCategory::Optional`]; **The** (with `shall` in the same
    ///   clause) → [`EarsCategory::Ubiquitous`];
    /// * anything else → [`EarsCategory::NotApplicable`].
    ///
    /// This is a keyword reading of the template, not a parser: it is meant to
    /// suggest a category, never to overrule one an author set.
    pub fn classify(sentence: &str) -> EarsCategory {
        const CONJUNCTIONS: [&str; 3] = ["and", "or", "but"];
        let lower = strip_list_prefix(sentence).to_lowercase();
        let clauses: Vec<Vec<&str>> = lower
            .split(',')
            .map(|clause| {
                clause
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .collect::<Vec<&str>>()
            })
            .filter(|words| !words.is_empty())
            .collect();
        let Some(shall_at) = clauses.iter().position(|c| c.contains(&"shall")) else {
            return EarsCategory::NotApplicable;
        };
        let first = &clauses[0];
        // A trigger needs at least one word after its keyword.
        let has_trigger = first.len() >= 2 && first[1] != "shall";
        // `word` inside the FIRST clause: at the third word or later, not
        // directly after a conjunction, with at least one word after it and
        // before `shall` (or before the clause's end when `shall` is in a
        // later clause).
        let stop = first
            .iter()
            .position(|w| *w == "shall")
            .unwrap_or(first.len());
        let inline_in_first = |word: &str| -> bool {
            (2..stop)
                .any(|i| first[i] == word && !CONJUNCTIONS.contains(&first[i - 1]) && i + 1 < stop)
        };
        // A later clause, up to and including the `shall` clause, opened by `word`.
        let opens_later_clause = |word: &str| clauses[1..=shall_at].iter().any(|c| c[0] == word);

        match first[0] {
            "if" if has_trigger => {
                let then_opens_clause = opens_later_clause("then");
                let then_inline = shall_at == 0 && inline_in_first("then");
                if then_opens_clause || then_inline {
                    EarsCategory::UnwantedBehavior
                } else {
                    EarsCategory::NotApplicable
                }
            }
            "while" if has_trigger => {
                let when_before_shall = inline_in_first("when") || opens_later_clause("when");
                if when_before_shall {
                    EarsCategory::Complex
                } else {
                    EarsCategory::StateDriven
                }
            }
            "when" if has_trigger => EarsCategory::EventDriven,
            "where" if has_trigger => EarsCategory::Optional,
            "the" if shall_at == 0 => EarsCategory::Ubiquitous,
            _ => EarsCategory::NotApplicable,
        }
    }
}

/// Drop a leading list marker — `1.`, `12)`, `-`, `*` or `•` followed by
/// whitespace — and surrounding whitespace, so a numbered requirement list
/// classifies line by line.
fn strip_list_prefix(sentence: &str) -> &str {
    let trimmed = sentence.trim();
    let rest = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
    let had_digits = rest.len() != trimmed.len();
    let after_marker = if had_digits {
        rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))
    } else {
        rest.strip_prefix('-')
            .or_else(|| rest.strip_prefix('*'))
            .or_else(|| rest.strip_prefix('•'))
    };
    match after_marker {
        Some(tail) if tail.starts_with(char::is_whitespace) => tail.trim_start(),
        _ => trimmed,
    }
}

/// A single acceptance criterion describing an observable success condition.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptanceCriterion {
    /// Unique kebab-case identifier, e.g. "typecheck-passes"
    pub id: String,
    /// Human-readable description, e.g. "TypeScript compilation succeeds with no errors"
    pub description: String,
    /// How this criterion should be verified
    pub method: VerificationMethod,
    /// Priority level
    pub priority: CriterionPriority,
    /// Concrete hint for the builder, e.g. "Run `npx tsc --noEmit` in frontend/"
    pub verification_hint: String,
    /// Category grouping, e.g. "compilation", "ui-content", "behavior"
    pub category: String,
    /// EARS requirement category
    #[serde(default)]
    pub ears_category: EarsCategory,
    /// The trigger event or state condition (for event_driven/state_driven/complex)
    #[serde(default)]
    pub trigger: Option<String>,
    /// The expected system action/behavior
    #[serde(default)]
    pub action: Option<String>,
}

/// Bugfix-specific criteria template (Kiro-inspired).
/// Used when the Specification agent detects bugfix intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugfixCriteria {
    /// What currently happens (broken behavior)
    pub current_behavior: String,
    /// What should happen (correct behavior)
    pub expected_behavior: String,
    /// What must NOT change (regression protection)
    #[serde(default)]
    pub unchanged_behavior: Vec<String>,
    /// Steps to reproduce the bug
    #[serde(default)]
    pub reproduction_steps: Vec<String>,
}

/// Collection of acceptance criteria produced by the specification agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptanceCriteria {
    /// One-line summary of what success looks like
    pub goal_summary: String,
    /// Ordered list of acceptance criteria
    pub criteria: Vec<AcceptanceCriterion>,
    /// Assumptions made by the specification agent
    pub assumptions: Vec<String>,
    /// Bugfix-specific context (populated when the task is a bug fix)
    #[serde(default)]
    pub bugfix_context: Option<BugfixCriteria>,
}

/// Result of the specification phase.
#[derive(Debug, Clone)]
pub struct SpecificationResult {
    /// The parsed acceptance criteria (may be empty on failure)
    pub criteria: AcceptanceCriteria,
    /// Whether the specification phase succeeded
    pub success: bool,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Raw AI output for debugging
    pub raw_output: String,
    /// The full prompt sent to the AI (for training data capture)
    pub prompt: String,
}

// ============================================================================
// Specification Agent
// ============================================================================

/// Run the specification agent to produce acceptance criteria for a task.
///
/// Follows the same pattern as `investigator::run_investigation()`:
/// - Builds a prompt from task context
/// - Makes a single AI call
/// - Parses structured output
/// - Returns gracefully on any failure (empty criteria, `success: false`)
pub fn run_specification_agent(
    description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    insights_section: Option<&str>,
    verification_depth: &str,
    relevant_issues: &[crate::known_issues::KnownIssue],
    constitution: Option<&str>,
) -> SpecificationResult {
    let start = Instant::now();

    let prompt = build_specification_prompt(
        description,
        discovery_context,
        resolved_contexts,
        insights_section,
        verification_depth,
        relevant_issues,
        constitution,
    );
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    let duration_ms = start.elapsed().as_millis() as u64;

    if !ai_result.success {
        warn!(
            "Specification agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        );
        return SpecificationResult {
            criteria: empty_criteria(),
            success: false,
            duration_ms,
            raw_output: ai_result.error.unwrap_or_default(),
            prompt,
        };
    }

    let output = ai_result.output.trim().to_string();

    if output.is_empty() || output.len() < 20 {
        warn!(
            "Specification agent produced too-short output ({} chars), falling back",
            output.len()
        );
        return SpecificationResult {
            criteria: empty_criteria(),
            success: false,
            duration_ms,
            raw_output: output,
            prompt,
        };
    }

    // Extract JSON from the response (may be wrapped in markdown code blocks)
    let json_text = crate::workflow_generation::generator::extract_json_from_response(&output);

    match serde_json::from_str::<AcceptanceCriteria>(&json_text) {
        Ok(criteria) => {
            info!(
                "Specification agent produced {} criteria in {}ms (goal: {})",
                criteria.criteria.len(),
                duration_ms,
                truncate_str(&criteria.goal_summary, 80)
            );
            debug!(
                "Criteria IDs: {:?}",
                criteria.criteria.iter().map(|c| &c.id).collect::<Vec<_>>()
            );

            if criteria.criteria.is_empty() {
                warn!("Specification agent returned zero criteria");
                return SpecificationResult {
                    criteria,
                    success: false,
                    duration_ms,
                    raw_output: output,
                    prompt,
                };
            }

            SpecificationResult {
                criteria,
                success: true,
                duration_ms,
                raw_output: output,
                prompt,
            }
        }
        Err(e) => {
            warn!("Failed to parse specification output as JSON: {}", e);
            debug!("Specification raw output: {}", truncate_str(&output, 500));
            SpecificationResult {
                criteria: empty_criteria(),
                success: false,
                duration_ms,
                raw_output: output,
                prompt,
            }
        }
    }
}

// ============================================================================
// Prompt Builder
// ============================================================================

/// Build the prompt for the specification agent.
fn build_specification_prompt(
    description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
    insights_section: Option<&str>,
    verification_depth: &str,
    relevant_issues: &[crate::known_issues::KnownIssue],
    constitution: Option<&str>,
) -> String {
    let mut prompt = format!(
        r#"You are a specification agent for an automation platform. Your job is to define **acceptance criteria** — concrete, observable conditions that prove a task was completed successfully.

You do NOT implement anything. You only define what "done" looks like.

## Task Description

{description}

## Project Context

{discovery}
"#,
        description = description,
        discovery = if discovery_context.is_empty() {
            "(No project discovery data available)"
        } else {
            discovery_context
        },
    );

    if !resolved_contexts.is_empty() {
        prompt.push_str(&format!(
            "\n## Additional Context\n\n{}\n",
            resolved_contexts
        ));
    }

    // Inject project constitution so criteria respect project-wide principles
    if let Some(constitution_text) = constitution {
        prompt.push('\n');
        prompt.push_str(&super::constitution::format_constitution_for_prompt(
            constitution_text,
        ));
        prompt.push_str("\nGenerated criteria MUST respect these principles. Any criterion that violates the constitution is invalid.\n");
    }

    if let Some(insights) = insights_section {
        if !insights.is_empty() {
            prompt.push('\n');
            prompt.push_str(insights);
        }
    }

    // Inject verification depth instructions
    match verification_depth {
        "smoke" => {
            prompt.push_str("\n## Verification Scope: SMOKE\n\n");
            prompt.push_str(
                "Generate only 1-3 minimal criteria: the app builds, starts, and doesn't crash.\n",
            );
            prompt.push_str("Do NOT generate detailed behavioral or content criteria.\n");
        }
        "thorough" => {
            prompt.push_str("\n## Verification Scope: THOROUGH\n\n");
            prompt.push_str("In addition to standard criteria, add 1-2 exploratory criteria that ask the AI to look for anomalies, unexpected behavior, or visual defects in the UI.\n");
        }
        "regression" => {
            prompt.push_str("\n## Verification Scope: REGRESSION\n\n");
            prompt.push_str("Include standard criteria plus regression checks for all known issues listed below.\n");
            prompt.push_str("Each known issue should produce at least one dedicated criterion.\n");
        }
        _ => {} // "standard" — no extra instructions
    }

    // Inject known issues as additional criteria hints
    if !relevant_issues.is_empty() {
        prompt.push_str("\n## Known Issues (Generate Regression Criteria)\n\n");
        prompt.push_str("The following issues have been previously observed. Generate a criterion for each to verify it does not recur:\n\n");
        for issue in relevant_issues {
            let severity = issue.severity.as_str();
            let hint = issue.verification_hint.as_deref().unwrap_or("(no hint)");
            prompt.push_str(&format!(
                "- **{}** [{}]: {}\n  Hint: {}\n\n",
                issue.title, severity, issue.description, hint,
            ));
        }
    }

    prompt.push_str(
        r#"
## Instructions

Think about what observable success looks like for this task. Focus on outcomes, not implementation.

For each criterion, consider:
- **What can be checked automatically?** Prefer command-based checks (exit codes, grep, test runners) and UI Bridge assertions over manual review.
- **What is the minimum bar?** Critical criteria are necessary conditions. Optional criteria are nice-to-have.
- **What would a reviewer check?** If a human reviewed this work, what would they verify?

Produce 3–8 structured criteria. Each criterion must have:
- `id`: kebab-case identifier (e.g., "typecheck-passes", "button-renders-correctly")
- `description`: one-sentence description of the success condition
- `method`: how to verify it — one of "command", "ui_bridge", "test", "manual"
- `priority`: one of "critical", "important", "optional"
- `verification_hint`: a concrete suggestion for how to check it (e.g., "Run `npx tsc --noEmit`", "Assert element with id 'button-save' is visible via UI Bridge")
- `category`: grouping label (e.g., "compilation", "ui-content", "behavior", "data-integrity", "style")

## EARS Format (Easy Approach to Requirements Syntax)

Classify each criterion using EARS categories:
- **ubiquitous**: Always true — "The system shall [action]"
  Example: { "ears_category": "ubiquitous", "action": "compile without TypeScript errors" }
- **event_driven**: Triggered by event — "When [trigger], the system shall [action]"
  Example: { "ears_category": "event_driven", "trigger": "user submits the login form", "action": "display the dashboard" }
- **state_driven**: Depends on state — "While [trigger], the system shall [action]"
  Example: { "ears_category": "state_driven", "trigger": "the user is authenticated", "action": "show the user's profile data" }
- **optional**: Feature toggle — set priority to "optional"
  Example: { "ears_category": "optional", "trigger": "dark mode is enabled", "action": "render with dark color scheme" }
- **complex**: Combined state + event
  Example: { "ears_category": "complex", "trigger": "While authenticated, when clicking logout", "action": "clear session and redirect to login" }
- **unwanted_behavior**: Response to an undesired situation — "If [trigger], then the system shall [action]"
  Example: { "ears_category": "unwanted_behavior", "trigger": "the API returns a 500 error", "action": "show an error banner and keep the form contents" }
  Use it for failure handling, invalid input and lost connections — anything the system must survive rather than do.

Each criterion in the JSON output must include:
- "ears_category": one of "ubiquitous", "event_driven", "state_driven", "optional", "complex", "unwanted_behavior"
- "trigger": the event, state or unwanted condition (required for event_driven, state_driven, complex, unwanted_behavior; null for ubiquitous)
- "action": the expected system behavior (required for all)

## Bugfix Detection

If the task description indicates a bug fix (keywords: fix, bug, broken, error, regression, doesn't work, should but doesn't),
include a "bugfix_context" field in your response:
{
  "bugfix_context": {
    "current_behavior": "What currently happens (the bug)",
    "expected_behavior": "What should happen after the fix",
    "unchanged_behavior": ["Behavior that must NOT change (regression protection)"],
    "reproduction_steps": ["Step 1", "Step 2", "..."]
  }
}
For non-bugfix tasks, omit this field.

Also provide:
- `goal_summary`: one sentence summarizing what overall success looks like
- `assumptions`: list of assumptions you're making (e.g., "Project uses TypeScript", "Frontend runs on localhost:3001")

## Output Format

Return ONLY valid JSON matching this structure. No markdown code blocks, no explanations.

{
  "goal_summary": "...",
  "criteria": [
    {
      "id": "typecheck-passes",
      "description": "TypeScript compilation completes without errors",
      "method": "command|ui_bridge|test|manual",
      "priority": "critical|important|optional",
      "verification_hint": "Run npx tsc --noEmit",
      "category": "compilation",
      "ears_category": "ubiquitous",
      "trigger": null,
      "action": "compile without TypeScript errors"
    }
  ],
  "assumptions": ["...", "..."],
  "bugfix_context": null
}
"#,
    );

    prompt
}

// ============================================================================
// Criteria Formatter (for Builder Prompt Injection)
// ============================================================================

/// Format acceptance criteria as a section to inject into the builder prompt.
///
/// Produces a markdown section with:
/// - A requirements table
/// - Instructions for the builder to tag verification steps with `criterion_id`
/// - A minimum verification step count
pub fn format_criteria_for_builder(criteria: &AcceptanceCriteria) -> String {
    let automatable: Vec<&AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .collect();

    let mut section = String::from("## Acceptance Criteria\n\n");
    section.push_str(&format!("**Goal:** {}\n\n", criteria.goal_summary));

    // Requirements table with EARS classification
    section.push_str("| ID | Priority | Method | EARS | Description | Verification Hint |\n");
    section.push_str("|---|---|---|---|---|---|\n");
    for c in &criteria.criteria {
        let method_str = match c.method {
            VerificationMethod::Command => "command",
            VerificationMethod::UiBridge => "ui_bridge",
            VerificationMethod::Test => "test",
            VerificationMethod::Manual => "manual",
        };
        let priority_str = match c.priority {
            CriterionPriority::Critical => "CRITICAL",
            CriterionPriority::Important => "important",
            CriterionPriority::Optional => "optional",
        };
        let ears_str = c.ears_category.as_str();
        section.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            c.id, priority_str, method_str, ears_str, c.description, c.verification_hint
        ));
    }

    // EARS-informed verification step generation instructions
    section.push_str("\n### EARS-Informed Verification Step Structure\n\n");
    section.push_str(
        "Use the EARS category of each criterion to determine the verification step structure:\n\n",
    );
    section.push_str(
        "- **ubiquitous** criteria: Generate a single check step that runs unconditionally (e.g., lint, typecheck).\n",
    );
    section.push_str(
        "- **event_driven** criteria: Generate a **setup step** that triggers the event, then a **check step** that verifies the expected action occurred.\n",
    );
    section.push_str(
        "- **state_driven** criteria: Generate a **setup step** that establishes the required state, then a **check step** that verifies the expected behavior in that state.\n",
    );
    section.push_str(
        "- **optional** criteria: Generate the check step with `\"required\": false` so it won't fail the workflow if the feature isn't enabled.\n",
    );
    section.push_str(
        "- **complex** criteria: Generate a **multi-step sequence**: (1) establish state, (2) trigger event, (3) verify the expected action.\n",
    );
    section.push_str(
        "- **unwanted_behavior** criteria: Generate a **fault-injection step** that induces the unwanted condition (a failing dependency, an invalid input, a dropped connection), then a **check step** that verifies the system's specified response — and that it did not crash or corrupt state.\n",
    );

    // Emit structured EARS detail for every category that names a trigger
    // except `optional` (its check is a single non-required step).
    let ears_criteria: Vec<&AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| {
            matches!(
                c.ears_category,
                EarsCategory::EventDriven
                    | EarsCategory::StateDriven
                    | EarsCategory::Complex
                    | EarsCategory::UnwantedBehavior
            )
        })
        .collect();
    if !ears_criteria.is_empty() {
        section.push_str("\n#### Trigger/Action Details\n\n");
        for c in &ears_criteria {
            let action_str = c.action.as_deref().unwrap_or("(not specified)");
            let steps = match c.ears_category {
                EarsCategory::EventDriven => "generate setup+trigger+assert steps",
                EarsCategory::StateDriven => "generate state-setup+assert steps",
                EarsCategory::Complex => "generate state-setup+trigger+assert steps",
                EarsCategory::UnwantedBehavior => "generate fault-injection+assert steps",
                _ => continue,
            };
            let pattern = format!(
                "- `{}`: {} -- {}\n",
                c.id,
                c.ears_category
                    .render("system", c.trigger.as_deref(), action_str),
                steps
            );
            section.push_str(&pattern);
        }
    }

    section.push_str("\n### Requirements for Verification Steps\n\n");
    section.push_str(
        "- **CRITICAL:** Your `verification_steps` MUST include a step for each automatable criterion (method != manual).\n",
    );
    section.push_str(
        "- Each verification step MUST include a `\"criterion_id\"` field matching the criterion's `id`.\n",
    );
    section.push_str(
        "- The verification step's type should match the criterion's method: `command` -> command step, `ui_bridge` -> ui_bridge step, `test` -> command step with test_type.\n",
    );

    if !automatable.is_empty() {
        section.push_str(&format!(
            "- Minimum verification step count: **{}** (one per automatable criterion).\n",
            automatable.len()
        ));
    }

    // Bugfix context for the builder
    if let Some(ref bugfix) = criteria.bugfix_context {
        section.push_str("\n### Bugfix Context\n\n");
        section.push_str("This is a **bug fix** task. Structure verification to confirm the fix and prevent regression.\n\n");
        section.push_str(&format!(
            "- **Current (broken) behavior:** {}\n",
            bugfix.current_behavior
        ));
        section.push_str(&format!(
            "- **Expected (fixed) behavior:** {}\n",
            bugfix.expected_behavior
        ));
        if !bugfix.unchanged_behavior.is_empty() {
            section.push_str("- **Must NOT change (regression protection):**\n");
            for item in &bugfix.unchanged_behavior {
                section.push_str(&format!("  - {}\n", item));
            }
        }
        if !bugfix.reproduction_steps.is_empty() {
            section.push_str("- **Reproduction steps:**\n");
            for (i, step) in bugfix.reproduction_steps.iter().enumerate() {
                section.push_str(&format!("  {}. {}\n", i + 1, step));
            }
        }
        section.push_str("\nVerification steps should include:\n");
        section.push_str("1. A step that reproduces the original bug scenario and confirms the fix resolves it.\n");
        section.push_str("2. Regression steps for each \"unchanged behavior\" item.\n");
    }

    if !criteria.assumptions.is_empty() {
        section.push_str("\n### Assumptions\n\n");
        for assumption in &criteria.assumptions {
            section.push_str(&format!("- {}\n", assumption));
        }
    }

    section
}

/// Format acceptance criteria as a cross-validation section for the verification prompt.
///
/// Instructs the verification agent to check that each criterion has a matching step.
pub fn format_criteria_for_verifier(criteria: &AcceptanceCriteria) -> String {
    let automatable: Vec<&AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .collect();

    if automatable.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## Acceptance Criteria Cross-Validation\n\n");
    section
        .push_str("The workflow was generated from these acceptance criteria. Check coverage:\n\n");

    for c in &automatable {
        let priority_str = match c.priority {
            CriterionPriority::Critical => "CRITICAL",
            CriterionPriority::Important => "important",
            CriterionPriority::Optional => "optional",
        };
        section.push_str(&format!(
            "- `{}` ({}) — {}: {}\n",
            c.id, priority_str, c.category, c.description
        ));
    }

    section.push_str("\n### Cross-validation checks:\n");
    section.push_str(
        "- Flag if any **CRITICAL** criterion lacks a verification step with a matching `criterion_id`.\n",
    );
    section.push_str(
        "- Flag verification steps with `criterion_id` values that don't match any criterion above.\n",
    );
    section.push_str(
        "- Flag if an important criterion has no verification step (warning, not blocking).\n",
    );

    // EARS structure checks
    let has_event_driven = automatable
        .iter()
        .any(|c| matches!(c.ears_category, EarsCategory::EventDriven));
    let has_state_driven = automatable
        .iter()
        .any(|c| matches!(c.ears_category, EarsCategory::StateDriven));
    let has_complex = automatable
        .iter()
        .any(|c| matches!(c.ears_category, EarsCategory::Complex));
    let has_unwanted = automatable
        .iter()
        .any(|c| matches!(c.ears_category, EarsCategory::UnwantedBehavior));

    if has_event_driven || has_state_driven || has_complex || has_unwanted {
        section.push_str("\n### EARS Structure Checks:\n");
        if has_event_driven {
            section.push_str(
                "- Flag if an **event_driven** criterion has only a single verification step (should have setup+trigger+assert).\n",
            );
        }
        if has_state_driven {
            section.push_str(
                "- Flag if a **state_driven** criterion has no state-setup step preceding its assertion step.\n",
            );
        }
        if has_complex {
            section.push_str(
                "- Flag if a **complex** criterion lacks the full sequence: state-setup, event-trigger, assertion.\n",
            );
        }
        if has_unwanted {
            section.push_str(
                "- Flag if an **unwanted_behavior** criterion has no step that induces the unwanted condition before asserting the response (a happy-path check does not verify failure handling).\n",
            );
        }
    }

    section
}

// ============================================================================
// Helpers
// ============================================================================

fn empty_criteria() -> AcceptanceCriteria {
    AcceptanceCriteria {
        goal_summary: String::new(),
        criteria: vec![],
        assumptions: vec![],
        bugfix_context: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Dark mode toggle works correctly".to_string(),
            criteria: vec![
                AcceptanceCriterion {
                    id: "typecheck-passes".to_string(),
                    description: "TypeScript compilation succeeds with no errors".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Run `npx tsc --noEmit` in frontend/".to_string(),
                    category: "compilation".to_string(),
                    ears_category: EarsCategory::Ubiquitous,
                    trigger: None,
                    action: Some("compile without TypeScript errors".to_string()),
                },
                AcceptanceCriterion {
                    id: "toggle-renders".to_string(),
                    description: "Dark mode toggle button is visible on settings page".to_string(),
                    method: VerificationMethod::UiBridge,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Assert element 'toggle-dark-mode' exists via UI Bridge"
                        .to_string(),
                    category: "ui-content".to_string(),
                    ears_category: EarsCategory::EventDriven,
                    trigger: Some("user navigates to settings page".to_string()),
                    action: Some("display the dark mode toggle button".to_string()),
                },
                AcceptanceCriterion {
                    id: "manual-visual-review".to_string(),
                    description: "Colors look correct in dark mode".to_string(),
                    method: VerificationMethod::Manual,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Visual inspection".to_string(),
                    category: "style".to_string(),
                    ears_category: EarsCategory::StateDriven,
                    trigger: Some("dark mode is enabled".to_string()),
                    action: Some("render with dark color scheme".to_string()),
                },
            ],
            assumptions: vec![
                "Project uses TypeScript".to_string(),
                "Frontend runs on localhost:3001".to_string(),
            ],
            bugfix_context: None,
        }
    }

    #[test]
    fn test_format_criteria_for_builder() {
        let criteria = sample_criteria();
        let output = format_criteria_for_builder(&criteria);

        assert!(output.contains("## Acceptance Criteria"));
        assert!(output.contains("typecheck-passes"));
        assert!(output.contains("toggle-renders"));
        assert!(output.contains("manual-visual-review"));
        assert!(output.contains("criterion_id"));
        // 2 automatable criteria (not manual)
        assert!(output.contains("Minimum verification step count: **2**"));
    }

    #[test]
    fn test_format_criteria_for_builder_ears_instructions() {
        let criteria = sample_criteria();
        let output = format_criteria_for_builder(&criteria);

        // EARS column in the table
        assert!(output.contains("| EARS |"));
        assert!(output.contains("ubiquitous"));
        assert!(output.contains("event_driven"));

        // EARS-informed step structure instructions
        assert!(output.contains("EARS-Informed Verification Step Structure"));
        assert!(output.contains("setup step"));
        assert!(output.contains("trigger"));

        // Trigger/Action details for event_driven criterion
        assert!(output.contains("Trigger/Action Details"));
        assert!(output.contains("user navigates to settings page"));
        assert!(output.contains("display the dark mode toggle button"));
    }

    #[test]
    fn test_format_criteria_for_builder_bugfix_context() {
        let mut criteria = sample_criteria();
        criteria.bugfix_context = Some(BugfixCriteria {
            current_behavior: "Button does not respond to clicks".to_string(),
            expected_behavior: "Button toggles dark mode on click".to_string(),
            unchanged_behavior: vec!["Light mode styling remains intact".to_string()],
            reproduction_steps: vec![
                "Navigate to settings".to_string(),
                "Click the dark mode toggle".to_string(),
            ],
        });
        let output = format_criteria_for_builder(&criteria);

        assert!(output.contains("Bugfix Context"));
        assert!(output.contains("Button does not respond to clicks"));
        assert!(output.contains("Button toggles dark mode on click"));
        assert!(output.contains("Light mode styling remains intact"));
        assert!(output.contains("Navigate to settings"));
        assert!(output.contains("regression"));
    }

    #[test]
    fn test_format_criteria_for_verifier() {
        let criteria = sample_criteria();
        let output = format_criteria_for_verifier(&criteria);

        assert!(output.contains("Cross-Validation"));
        assert!(output.contains("typecheck-passes"));
        assert!(output.contains("toggle-renders"));
        // Manual criterion should NOT appear in verifier section
        assert!(!output.contains("manual-visual-review"));
    }

    #[test]
    fn test_format_criteria_for_verifier_ears_checks() {
        let criteria = sample_criteria();
        let output = format_criteria_for_verifier(&criteria);

        // Should include EARS structure checks for event_driven criterion
        assert!(output.contains("EARS Structure Checks"));
        assert!(output.contains("event_driven"));
        assert!(output.contains("setup+trigger+assert"));
    }

    #[test]
    fn test_empty_criteria() {
        let criteria = empty_criteria();
        assert!(criteria.criteria.is_empty());
        assert!(criteria.goal_summary.is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let criteria = sample_criteria();
        let json = serde_json::to_string(&criteria).unwrap();
        let parsed: AcceptanceCriteria = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.criteria.len(), 3);
        assert_eq!(parsed.criteria[0].id, "typecheck-passes");
        assert_eq!(parsed.criteria[0].method, VerificationMethod::Command);
        assert_eq!(parsed.criteria[1].priority, CriterionPriority::Critical);
    }

    #[test]
    fn test_serde_backward_compat_no_ears_fields() {
        // Old-format JSON without EARS fields should deserialize with defaults
        let json = r#"{
            "goal_summary": "App builds",
            "criteria": [{
                "id": "build-passes",
                "description": "Build succeeds",
                "method": "command",
                "priority": "critical",
                "verification_hint": "Run build",
                "category": "compilation"
            }],
            "assumptions": []
        }"#;
        let parsed: AcceptanceCriteria = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.criteria.len(), 1);
        assert_eq!(
            parsed.criteria[0].ears_category,
            EarsCategory::NotApplicable
        );
        assert!(parsed.criteria[0].trigger.is_none());
        assert!(parsed.criteria[0].action.is_none());
        assert!(parsed.bugfix_context.is_none());
    }

    #[test]
    fn test_serde_ears_category_unknown_variant() {
        // Unknown ears_category value should fall back to NotApplicable via serde(other)
        let json = r#"{
            "goal_summary": "Test",
            "criteria": [{
                "id": "test",
                "description": "Test",
                "method": "command",
                "priority": "critical",
                "verification_hint": "",
                "category": "test",
                "ears_category": "some_future_variant",
                "trigger": null,
                "action": null
            }],
            "assumptions": []
        }"#;
        let parsed: AcceptanceCriteria = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.criteria[0].ears_category,
            EarsCategory::NotApplicable
        );
    }

    #[test]
    fn test_ears_category_default() {
        let criterion = AcceptanceCriterion::default();
        assert_eq!(criterion.ears_category, EarsCategory::NotApplicable);
    }

    fn unwanted_criterion() -> AcceptanceCriterion {
        AcceptanceCriterion {
            id: "save-survives-500".to_string(),
            description: "A failed save keeps the form contents".to_string(),
            method: VerificationMethod::UiBridge,
            priority: CriterionPriority::Critical,
            verification_hint: "Stub the save endpoint to 500, submit, assert banner".to_string(),
            category: "behavior".to_string(),
            ears_category: EarsCategory::UnwantedBehavior,
            trigger: Some("the save request returns a 500 error".to_string()),
            action: Some("show an error banner and keep the form contents".to_string()),
        }
    }

    #[test]
    fn test_unwanted_behavior_serde_name_and_alias() {
        let json = serde_json::to_string(&EarsCategory::UnwantedBehavior).unwrap();
        assert_eq!(json, "\"unwanted_behavior\"");
        for spelling in ["\"unwanted_behavior\"", "\"unwanted_behaviour\""] {
            let parsed: EarsCategory = serde_json::from_str(spelling).unwrap();
            assert_eq!(parsed, EarsCategory::UnwantedBehavior, "{spelling}");
        }
    }

    #[test]
    fn test_as_str_matches_serde_for_every_classified_category() {
        for category in EarsCategory::CLASSIFIED {
            let json = serde_json::to_string(&category).unwrap();
            assert_eq!(json, format!("\"{}\"", category.as_str()));
            let back: EarsCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, category, "{json} must round-trip");
        }
        assert_eq!(EarsCategory::NotApplicable.as_str(), "n/a");
    }

    #[test]
    fn test_unwanted_behavior_parses_inside_a_criterion() {
        let json = r#"{
            "goal_summary": "Saves fail safely",
            "criteria": [{
                "id": "save-survives-500",
                "description": "A failed save keeps the form contents",
                "method": "ui_bridge",
                "priority": "critical",
                "verification_hint": "",
                "category": "behavior",
                "ears_category": "unwanted_behavior",
                "trigger": "the save request returns a 500 error",
                "action": "show an error banner"
            }],
            "assumptions": []
        }"#;
        let parsed: AcceptanceCriteria = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.criteria[0].ears_category,
            EarsCategory::UnwantedBehavior
        );
    }

    #[test]
    fn test_render_every_category() {
        let sys = "system";
        let cases = [
            (EarsCategory::Ubiquitous, None, "The system shall log out"),
            (
                EarsCategory::EventDriven,
                Some("the user clicks logout"),
                "When the user clicks logout, the system shall log out",
            ),
            (
                EarsCategory::StateDriven,
                Some("the session is active"),
                "While the session is active, the system shall log out",
            ),
            (
                EarsCategory::Optional,
                Some("single sign-on is enabled"),
                "Where single sign-on is enabled, the system shall log out",
            ),
            (
                EarsCategory::Complex,
                Some("While authenticated, when the token expires"),
                "While authenticated, when the token expires, the system shall log out",
            ),
            (
                EarsCategory::UnwantedBehavior,
                Some("the identity provider is unreachable"),
                "If the identity provider is unreachable, then the system shall log out",
            ),
            (EarsCategory::NotApplicable, None, "log out"),
        ];
        for (category, trigger, expected) in cases {
            assert_eq!(category.render(sys, trigger, "log out"), expected);
        }
    }

    #[test]
    fn test_render_marks_a_missing_trigger_instead_of_dropping_it() {
        for trigger in [None, Some(""), Some("   ")] {
            assert_eq!(
                EarsCategory::UnwantedBehavior.render("service", trigger, "retry"),
                "If (trigger not specified), then the service shall retry"
            );
        }
    }

    #[test]
    fn test_classify_every_pattern() {
        let cases = [
            (
                "If the payment gateway times out, then the system shall queue the order",
                EarsCategory::UnwantedBehavior,
            ),
            (
                "  IF a duplicate id is submitted THEN the API shall return 409",
                EarsCategory::UnwantedBehavior,
            ),
            (
                "While logged in, when the token expires, the app shall refresh it",
                EarsCategory::Complex,
            ),
            (
                "When the user submits, the system shall save",
                EarsCategory::EventDriven,
            ),
            (
                "While offline, the app shall queue writes",
                EarsCategory::StateDriven,
            ),
            (
                "Where dark mode is enabled, the UI shall use the dark palette",
                EarsCategory::Optional,
            ),
            (
                "The system shall encrypt data at rest",
                EarsCategory::Ubiquitous,
            ),
            // A bare "If" with no "then" is not EARS.
            ("If possible, make it fast", EarsCategory::NotApplicable),
            // "then" must follow the opening word, not be it.
            ("Then the system shall stop", EarsCategory::NotApplicable),
            // Prefix of a longer word is not the keyword.
            ("Whenever it rains, carry on", EarsCategory::NotApplicable),
            ("", EarsCategory::NotApplicable),
        ];
        for (sentence, expected) in cases {
            assert_eq!(EarsCategory::classify(sentence), expected, "{sentence:?}");
        }
    }

    #[test]
    fn test_classify_rejects_keywords_in_the_wrong_place() {
        let negatives = [
            // `then` inside the trigger, not opening the response clause.
            "If the user logs in and then logs out, the session shall end",
            // No trigger between `If` and `then`.
            "If-then rules shall apply",
            // No `shall` at all.
            "If, then",
            "If the cache is cold, then warm it",
            "When the user submits the form",
            "The system encrypts data at rest",
            // `shall` present but the opening word is no EARS keyword.
            "Users shall be notified",
            // `then` after a conjunction, comma-less form.
            "If the job fails or then retries the worker shall alert",
        ];
        for sentence in negatives {
            assert_eq!(
                EarsCategory::classify(sentence),
                EarsCategory::NotApplicable,
                "{sentence:?}"
            );
        }
    }

    #[test]
    fn test_classify_a_when_after_shall_is_not_complex() {
        assert_eq!(
            EarsCategory::classify("While uploading, the system shall show progress when asked"),
            EarsCategory::StateDriven
        );
        assert_eq!(
            EarsCategory::classify("While uploading the system shall show progress when asked"),
            EarsCategory::StateDriven
        );
    }

    #[test]
    fn test_classify_complex_without_a_comma_before_when() {
        for sentence in [
            "While logged in when the token expires, the app shall refresh it",
            "While logged in when the token expires the app shall refresh it",
        ] {
            assert_eq!(
                EarsCategory::classify(sentence),
                EarsCategory::Complex,
                "{sentence:?}"
            );
        }
        // A `when` right after a conjunction is part of the state, not the event.
        assert_eq!(
            EarsCategory::classify("While idle and when paused, the app shall sleep"),
            EarsCategory::StateDriven
        );
    }

    #[test]
    fn test_classify_if_then_adverbial_clause() {
        // Pinned: a `then` opening ANY clause up to the `shall` clause
        // completes the If template, so an adverbial between `then` and the
        // subject is still unwanted behaviour.
        assert_eq!(
            EarsCategory::classify(
                "If the disk is full, then immediately, the system shall reject the upload"
            ),
            EarsCategory::UnwantedBehavior
        );
        // …but a `then` after `shall` completes nothing.
        assert_eq!(
            EarsCategory::classify("If the disk is full, the system shall alert, then retry"),
            EarsCategory::NotApplicable
        );
    }

    #[test]
    fn test_classify_comma_less_then_limitation_is_pinned() {
        // Documented limitation: without commas nothing checks that a subject
        // follows `then`, so this reads as unwanted behaviour. With the comma
        // the same words are refused (see the negatives test).
        assert_eq!(
            EarsCategory::classify("If the user logs in then logs out the session shall end"),
            EarsCategory::UnwantedBehavior
        );
    }

    #[test]
    fn test_classify_strips_a_list_prefix() {
        let cases = [
            (
                "1. If the disk is full, then the system shall reject the upload",
                EarsCategory::UnwantedBehavior,
            ),
            (
                "12) When a file is dropped, the uploader shall start",
                EarsCategory::EventDriven,
            ),
            (
                "- The system shall log every request",
                EarsCategory::Ubiquitous,
            ),
            (
                "* Where SSO is enabled, the app shall hide the password field",
                EarsCategory::Optional,
            ),
            (
                "• While offline, the app shall queue writes",
                EarsCategory::StateDriven,
            ),
            // A number that is not a list marker is part of the sentence.
            ("3 users shall be admins", EarsCategory::NotApplicable),
        ];
        for (sentence, expected) in cases {
            assert_eq!(EarsCategory::classify(sentence), expected, "{sentence:?}");
        }
    }

    #[test]
    fn test_classify_inverts_render_for_the_comma_less_and_adverbial_forms() {
        // Complex whose trigger carries no comma between state and event.
        let complex = EarsCategory::Complex.render(
            "app",
            Some("While logged in when the token expires"),
            "refresh it",
        );
        assert_eq!(
            complex,
            "While logged in when the token expires, the app shall refresh it"
        );
        assert_eq!(EarsCategory::classify(&complex), EarsCategory::Complex);

        // Unwanted behaviour whose response starts with an adverbial clause.
        let unwanted = EarsCategory::UnwantedBehavior.render(
            "system",
            Some("the disk is full"),
            "reject the upload",
        );
        let with_adverbial = unwanted.replacen("then", "then immediately,", 1);
        assert_eq!(
            with_adverbial,
            "If the disk is full, then immediately, the system shall reject the upload"
        );
        assert_eq!(
            EarsCategory::classify(&with_adverbial),
            EarsCategory::UnwantedBehavior
        );
    }

    #[test]
    fn test_classify_inverts_render() {
        for category in EarsCategory::CLASSIFIED {
            let trigger = match category {
                EarsCategory::Complex => Some("While idle, when a job arrives"),
                _ => Some("a job arrives"),
            };
            let sentence = category.render("scheduler", trigger, "start it");
            assert_eq!(EarsCategory::classify(&sentence), category, "{sentence}");
        }
    }

    #[test]
    fn test_requires_trigger() {
        assert!(!EarsCategory::Ubiquitous.requires_trigger());
        assert!(!EarsCategory::NotApplicable.requires_trigger());
        for category in [
            EarsCategory::EventDriven,
            EarsCategory::StateDriven,
            EarsCategory::Optional,
            EarsCategory::Complex,
            EarsCategory::UnwantedBehavior,
        ] {
            assert!(category.requires_trigger(), "{}", category.as_str());
        }
    }

    #[test]
    fn test_builder_renders_unwanted_behavior() {
        let mut criteria = sample_criteria();
        criteria.criteria.push(unwanted_criterion());
        let output = format_criteria_for_builder(&criteria);

        assert!(output.contains("| save-survives-500 | CRITICAL | ui_bridge | unwanted_behavior |"));
        assert!(
            output.contains("**unwanted_behavior** criteria: Generate a **fault-injection step**")
        );
        assert!(output.contains(
            "- `save-survives-500`: If the save request returns a 500 error, then the system shall show an error banner and keep the form contents -- generate fault-injection+assert steps"
        ));
    }

    #[test]
    fn test_verifier_checks_unwanted_behavior_only_when_present() {
        let mut criteria = sample_criteria();
        let without = format_criteria_for_verifier(&criteria);
        assert!(!without.contains("unwanted_behavior"));

        criteria.criteria.push(unwanted_criterion());
        let with = format_criteria_for_verifier(&criteria);
        assert!(with.contains(
            "Flag if an **unwanted_behavior** criterion has no step that induces the unwanted condition"
        ));
    }

    #[test]
    fn test_verifier_emits_ears_checks_for_unwanted_behavior_alone() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Fails safely".to_string(),
            criteria: vec![unwanted_criterion()],
            assumptions: vec![],
            bugfix_context: None,
        };
        let output = format_criteria_for_verifier(&criteria);
        assert!(output.contains("EARS Structure Checks"));
        assert!(!output.contains("event_driven"));
    }

    #[test]
    fn test_prompt_lists_unwanted_behavior() {
        let prompt = build_specification_prompt(
            "Add retry to the uploader",
            "",
            "",
            None,
            "standard",
            &[],
            None,
        );
        assert!(prompt.contains("**unwanted_behavior**"));
        assert!(prompt.contains("If [trigger], then the system shall [action]"));
        assert!(prompt.contains(
            "one of \"ubiquitous\", \"event_driven\", \"state_driven\", \"optional\", \"complex\", \"unwanted_behavior\""
        ));
    }
}
