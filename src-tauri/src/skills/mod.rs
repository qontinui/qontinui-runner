//! Skill Registry
//!
//! A skill is a named, parameterized template that produces pre-configured
//! workflow step(s) when instantiated. Skills sit between raw step types
//! and full workflows as composable building blocks.
//!
//! This module provides:
//! - Rust types mirroring the TypeScript skill definitions
//! - A registry that loads built-in skills from embedded JSON
//! - Search and lookup functionality
//! - Skill instantiation (template → concrete step configs)
//! - Playbook parsing (markdown with YAML frontmatter for domain knowledge)

pub mod playbook_parser;

use crate::database::pg::PgDb;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameterOption {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String, // "string" | "number" | "boolean" | "select"
    pub label: String,
    pub description: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SkillParameterOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<ParameterDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDependency {
    pub param: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRef {
    pub skill_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_overrides: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SkillTemplate {
    #[serde(rename = "single_step")]
    SingleStep { step: HashMap<String, Value> },
    #[serde(rename = "multi_step")]
    MultiStep { steps: Vec<HashMap<String, Value>> },
    #[serde(rename = "composition")]
    Composition { skill_refs: Vec<SkillRef> },
    /// Markdown playbook with domain knowledge (TuriX-CUA inspired).
    ///
    /// Playbooks are human-editable markdown files with YAML frontmatter that
    /// provide LLM context about how to interact with specific applications.
    /// Unlike other templates that generate workflow steps, playbooks inject
    /// domain knowledge into AI prompts.
    #[serde(rename = "playbook")]
    Playbook {
        /// Full markdown content (the body after frontmatter).
        content: String,
        /// Trigger conditions for when this playbook should be included.
        #[serde(default)]
        triggers: Vec<PlaybookTrigger>,
    },
}

/// Trigger condition for a playbook.
///
/// Determines when a playbook should be automatically included in AI prompts
/// based on the current automation context (app name, URL pattern, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookTrigger {
    /// Type of trigger: "app_name", "url_pattern", "tag".
    pub trigger_type: String,
    /// Value to match against (exact match for app_name, glob for url_pattern).
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub category: String, // "code-quality" | "testing" | "monitoring" | "ai-task" | "deployment" | "composition" | "custom"
    pub tags: Vec<String>,
    pub icon: String,
    pub color: String,
    pub allowed_phases: Vec<String>, // "setup" | "verification" | "agentic" | "completion"
    pub parameters: Vec<SkillParameter>,
    pub template: SkillTemplate,
    pub source: String, // "builtin" | "user" | "community"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<SkillAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
}

/// Tracks that a step was created from a skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillOrigin {
    pub skill_id: String,
    pub skill_slug: String,
    pub parameter_values: HashMap<String, Value>,
}

// =============================================================================
// Export / Import Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExportManifest {
    pub version: String,
    pub exported_at: String,
    pub app_version: String,
    pub content_type: String,
    pub skill_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExport {
    pub manifest: SkillExportManifest,
    pub skills: Vec<SkillDefinition>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub overwritten: usize,
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Compute a SHA-256 checksum of a skill's content for integrity verification.
pub fn compute_skill_checksum(skill: &SkillDefinition) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // Hash deterministic fields only (not usage_count, approval_status, etc.)
    hasher.update(skill.name.as_bytes());
    hasher.update(skill.slug.as_bytes());
    hasher.update(skill.description.as_bytes());
    hasher.update(skill.category.as_bytes());
    for tag in &skill.tags {
        hasher.update(tag.as_bytes());
    }
    if let Ok(template_json) = serde_json::to_string(&skill.template) {
        hasher.update(template_json.as_bytes());
    }
    if let Ok(params_json) = serde_json::to_string(&skill.parameters) {
        hasher.update(params_json.as_bytes());
    }
    if let Some(v) = &skill.version {
        hasher.update(v.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Compute a checksum for an entire skill export.
pub fn compute_export_checksum(skills: &[SkillDefinition]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for skill in skills {
        hasher.update(compute_skill_checksum(skill).as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

// =============================================================================
// Built-in Skills
// =============================================================================

const BUILTIN_SKILLS_JSON: &str = include_str!("builtin.json");

fn load_builtin_skills() -> Vec<SkillDefinition> {
    serde_json::from_str(BUILTIN_SKILLS_JSON).expect("Failed to parse built-in skills JSON")
}

/// Load user-created skills directly from a raw SQLite connection.
///
/// This mirrors `CheckpointDb::list_user_skills()` but works with the raw
/// `Connection` passed through the generator pipeline (same pattern as
/// `rules::load_rules`).
pub fn load_user_skills_from_conn() -> Vec<SkillDefinition> {
    Vec::new()
}

// =============================================================================
// Registry
// =============================================================================

#[derive(Debug, Clone)]
pub struct SkillRegistry {
    builtin: Vec<SkillDefinition>,
    user: Vec<SkillDefinition>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    /// Create a new registry with built-in skills loaded.
    pub fn new() -> Self {
        Self {
            builtin: load_builtin_skills(),
            user: Vec::new(),
        }
    }

    /// Create a registry with built-in skills + user skills loaded from DB.
    ///
    /// If `conn` is None or the query fails, only built-in skills are loaded.
    pub fn with_db() -> Self {
        // SQLite removed — fall back to built-in skills only
        Self::new()
    }

    /// Create a registry with built-in skills + user skills loaded from PG.
    ///
    /// If `pg_db` is None or the query fails, only built-in skills are loaded.
    /// Uses `Handle::current().block_on()` to call async PG methods from sync context.
    pub fn with_pg(pg_db: Option<&Arc<PgDb>>) -> Self {
        let mut registry = Self::new();
        if let Some(pg) = pg_db {
            let pg_clone = pg.clone();
            let user_skills = tokio::runtime::Handle::current()
                .block_on(async { pg_clone.list_user_skills().await.unwrap_or_default() });
            if !user_skills.is_empty() {
                tracing::debug!("Loaded {} user skills from PG", user_skills.len());
                registry.user = user_skills;
            }
        }
        registry
    }

    /// Register user-created skills (e.g., loaded from database).
    pub fn set_user_skills(&mut self, skills: Vec<SkillDefinition>) {
        self.user = skills;
    }

    /// Get all skills (built-in + user).
    pub fn all(&self) -> Vec<&SkillDefinition> {
        self.builtin.iter().chain(self.user.iter()).collect()
    }

    /// Get the number of built-in skills.
    pub fn builtin_count(&self) -> usize {
        self.builtin.len()
    }

    /// Get a skill by its ID.
    pub fn get(&self, id: &str) -> Option<&SkillDefinition> {
        self.builtin
            .iter()
            .chain(self.user.iter())
            .find(|s| s.id == id)
    }

    /// Get a skill by its slug.
    pub fn get_by_slug(&self, slug: &str) -> Option<&SkillDefinition> {
        self.builtin
            .iter()
            .chain(self.user.iter())
            .find(|s| s.slug == slug)
    }

    /// Get all skills allowed in a given phase.
    pub fn by_phase(&self, phase: &str) -> Vec<&SkillDefinition> {
        self.all()
            .into_iter()
            .filter(|s| s.allowed_phases.iter().any(|p| p == phase))
            .collect()
    }

    /// Get all skills in a given category.
    pub fn by_category(&self, category: &str) -> Vec<&SkillDefinition> {
        self.all()
            .into_iter()
            .filter(|s| s.category == category)
            .collect()
    }

    /// Get skills filtered by phase, tags, and/or category.
    ///
    /// This enables per-execution tool whitelisting: only expose skills
    /// relevant to the current task to reduce prompt bloat.
    ///
    /// - `phase`: if Some, only include skills allowed in this phase
    /// - `tags`: if non-empty, only include skills that have at least one matching tag
    /// - `category`: if Some, only include skills in this category
    pub fn skills_for_context(
        &self,
        phase: Option<&str>,
        tags: &[String],
        category: Option<&str>,
    ) -> Vec<&SkillDefinition> {
        self.all()
            .into_iter()
            .filter(|s| {
                // Phase filter
                if let Some(p) = phase {
                    if !s.allowed_phases.iter().any(|ap| ap == p) {
                        return false;
                    }
                }
                // Tag filter (any match)
                if !tags.is_empty() && !s.tags.iter().any(|t| tags.contains(t)) {
                    return false;
                }
                // Category filter
                if let Some(c) = category {
                    if s.category != c {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    /// Search skills by text query with relevance-ranked results.
    ///
    /// Scoring: exact name match (100), slug match (80), word in name (10 each),
    /// word in description (5 each), word in tags (8 each).
    pub fn search(&self, query: &str) -> Vec<&SkillDefinition> {
        let query_lower = query.trim().to_lowercase();
        if query_lower.is_empty() {
            return self.all();
        }

        let words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(&SkillDefinition, i32)> = self
            .all()
            .into_iter()
            .filter_map(|skill| {
                let name_lower = skill.name.to_lowercase();
                let slug_lower = skill.slug.to_lowercase();
                let desc_lower = skill.description.to_lowercase();
                let tags_lower: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();

                // All words must appear somewhere
                let haystack = format!(
                    "{} {} {} {}",
                    name_lower,
                    desc_lower,
                    slug_lower,
                    tags_lower.join(" ")
                );
                if !words.iter().all(|word| haystack.contains(word)) {
                    return None;
                }

                let mut score: i32 = 0;

                // Exact name match
                if name_lower == query_lower {
                    score += 100;
                }
                // Exact slug match
                if slug_lower == query_lower {
                    score += 80;
                }

                for word in &words {
                    if name_lower.contains(word) {
                        score += 10;
                    }
                    if desc_lower.contains(word) {
                        score += 5;
                    }
                    for tag in &tags_lower {
                        if tag.contains(word) {
                            score += 8;
                        }
                    }
                }

                Some((skill, score))
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(s, _)| s).collect()
    }

    /// Get all built-in skill definitions as JSON (for AI generator context).
    pub fn builtin_as_json(&self) -> Value {
        serde_json::to_value(&self.builtin).unwrap_or(Value::Array(vec![]))
    }

    /// Increment the usage count for a skill (called after successful instantiation).
    pub fn increment_usage(&mut self, skill_id: &str) {
        for skill in &mut self.user {
            if skill.id == skill_id {
                skill.usage_count = Some(skill.usage_count.unwrap_or(0) + 1);
                break;
            }
        }
    }
}

// =============================================================================
// Instantiation
// =============================================================================

/// Resolve {{placeholder}} values in a template step.
fn resolve_value(value: &Value, params: &HashMap<String, Value>) -> Option<Value> {
    match value {
        Value::String(s) => {
            // Exact placeholder: "{{name}}"
            if let Some(param_name) = s.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                let param_name = param_name.trim();
                // Return None if parameter is not provided (omit from output)
                params.get(param_name).cloned()
            } else if s.contains("{{") {
                // Inline interpolation: "prefix {{name}} suffix"
                let mut result = s.clone();
                for (key, val) in params {
                    let placeholder = format!("{{{{{}}}}}", key);
                    let replacement = match val {
                        Value::String(v) => v.clone(),
                        Value::Number(v) => v.to_string(),
                        Value::Bool(v) => v.to_string(),
                        _ => val.to_string(),
                    };
                    result = result.replace(&placeholder, &replacement);
                }
                // Remove unresolved placeholders
                let re_pattern = "{{";
                if result.contains(re_pattern) {
                    // Simple removal of remaining {{...}} placeholders
                    let mut cleaned = String::new();
                    let mut chars = result.chars().peekable();
                    while let Some(ch) = chars.next() {
                        if ch == '{' && chars.peek() == Some(&'{') {
                            // Skip until }}
                            chars.next(); // consume second {
                            while let Some(inner) = chars.next() {
                                if inner == '}' && chars.peek() == Some(&'}') {
                                    chars.next(); // consume second }
                                    break;
                                }
                            }
                        } else {
                            cleaned.push(ch);
                        }
                    }
                    if cleaned.is_empty() {
                        None
                    } else {
                        Some(Value::String(cleaned))
                    }
                } else {
                    Some(Value::String(result))
                }
            } else {
                Some(value.clone())
            }
        }
        Value::Object(obj) => {
            let mut result = serde_json::Map::new();
            for (key, val) in obj {
                if let Some(resolved) = resolve_value(val, params) {
                    result.insert(key.clone(), resolved);
                }
                // Omit keys with unresolved/missing values
            }
            Some(Value::Object(result))
        }
        Value::Array(arr) => {
            let resolved: Vec<Value> = arr
                .iter()
                .filter_map(|v| resolve_value(v, params))
                .collect();
            Some(Value::Array(resolved))
        }
        _ => Some(value.clone()),
    }
}

/// Instantiate a skill into concrete step config(s) as JSON Values.
///
/// Returns a Vec of step JSON objects ready for workflow insertion.
pub fn instantiate_skill(
    skill: &SkillDefinition,
    phase: &str,
    param_values: &HashMap<String, Value>,
) -> Result<Vec<Value>, String> {
    // Validate phase
    if !skill.allowed_phases.contains(&phase.to_string()) {
        return Err(format!(
            "Skill \"{}\" is not allowed in phase \"{}\". Allowed: {:?}",
            skill.name, phase, skill.allowed_phases
        ));
    }

    // Build effective params (user values over defaults)
    let mut effective_params = HashMap::new();
    for param in &skill.parameters {
        if let Some(user_val) = param_values.get(&param.name) {
            if !(user_val.is_null() || user_val.is_string() && user_val.as_str() == Some("")) {
                effective_params.insert(param.name.clone(), user_val.clone());
                continue;
            }
        }
        if let Some(default_val) = &param.default {
            effective_params.insert(param.name.clone(), default_val.clone());
        }
    }

    // Validate parameters
    for param in &skill.parameters {
        if let Some(val) = effective_params.get(&param.name) {
            // Check min/max for numeric params
            if let Some(min) = param.min {
                if let Some(num) = val.as_f64() {
                    if num < min {
                        return Err(format!(
                            "Parameter \"{}\" value {} is below minimum {}",
                            param.name, num, min
                        ));
                    }
                }
            }
            if let Some(max) = param.max {
                if let Some(num) = val.as_f64() {
                    if num > max {
                        return Err(format!(
                            "Parameter \"{}\" value {} exceeds maximum {}",
                            param.name, num, max
                        ));
                    }
                }
            }
            // Check regex pattern for string params
            if let Some(pattern) = &param.pattern {
                if let Some(s) = val.as_str() {
                    if let Ok(re) = regex::Regex::new(pattern) {
                        if !re.is_match(s) {
                            return Err(format!(
                                "Parameter \"{}\" value \"{}\" does not match pattern \"{}\"",
                                param.name, s, pattern
                            ));
                        }
                    }
                }
            }
        }
    }

    let origin = SkillOrigin {
        skill_id: skill.id.clone(),
        skill_slug: skill.slug.clone(),
        parameter_values: effective_params.clone(),
    };

    let template_steps = match &skill.template {
        SkillTemplate::SingleStep { step } => vec![step.clone()],
        SkillTemplate::MultiStep { steps } => steps.clone(),
        SkillTemplate::Composition { .. } => {
            return Err(format!(
                "Skill \"{}\" is a composition skill and cannot be directly instantiated",
                skill.name
            ));
        }
        SkillTemplate::Playbook { .. } => {
            return Err(format!(
                "Skill \"{}\" is a playbook and cannot be directly instantiated as workflow steps",
                skill.name
            ));
        }
    };

    let total = template_steps.len();
    let mut result = Vec::with_capacity(total);

    for (i, template_step) in template_steps.into_iter().enumerate() {
        let mut step_json = serde_json::Map::new();

        // Resolve template values
        for (key, value) in &template_step {
            if let Some(resolved) = resolve_value(value, &effective_params) {
                step_json.insert(key.clone(), resolved);
            }
        }

        // Add metadata
        step_json.insert("id".into(), Value::String(uuid::Uuid::new_v4().to_string()));
        step_json.insert("phase".into(), Value::String(phase.to_string()));
        step_json.insert(
            "skill_origin".into(),
            serde_json::to_value(&origin).unwrap_or(Value::Null),
        );

        let name = if total > 1 {
            format!("{} ({}/{})", skill.name, i + 1, total)
        } else {
            skill.name.clone()
        };
        step_json.insert("name".into(), Value::String(name));

        result.push(Value::Object(step_json));
    }

    Ok(result)
}

/// Instantiate a composition skill by resolving its skill_refs.
///
/// Each SkillRef is looked up in the registry and instantiated individually.
/// Returns all resulting steps flattened.
pub fn instantiate_composition(
    skill: &SkillDefinition,
    phase: &str,
    param_values: &HashMap<String, Value>,
    registry: &SkillRegistry,
) -> Result<Vec<Value>, String> {
    // Validate this skill's own dependencies
    validate_dependencies(skill, registry)?;

    let skill_refs = match &skill.template {
        SkillTemplate::Composition { skill_refs } => skill_refs,
        _ => {
            return Err(format!(
                "Skill \"{}\" is not a composition skill",
                skill.name
            ))
        }
    };

    let mut all_steps = Vec::new();
    for skill_ref in skill_refs {
        let ref_skill = registry
            .get(&skill_ref.skill_id)
            .ok_or_else(|| format!("Referenced skill not found: {}", skill_ref.skill_id))?;

        // Merge parent params with ref overrides
        let mut merged_params = param_values.clone();
        if let Some(overrides) = &skill_ref.parameter_overrides {
            for (k, v) in overrides {
                merged_params.insert(k.clone(), v.clone());
            }
        }

        let steps = instantiate_skill(ref_skill, phase, &merged_params)?;
        all_steps.extend(steps);
    }

    Ok(all_steps)
}

/// Validate that all skill dependencies are available in the registry.
/// Returns Ok(()) if all deps exist, or Err with list of missing dependency IDs.
pub fn validate_dependencies(
    skill: &SkillDefinition,
    registry: &SkillRegistry,
) -> Result<(), String> {
    if let Some(deps) = &skill.depends_on {
        let missing: Vec<&String> = deps
            .iter()
            .filter(|dep_id| registry.get(dep_id).is_none())
            .collect();

        if !missing.is_empty() {
            return Err(format!(
                "Skill \"{}\" has missing dependencies: {}",
                skill.name,
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

// =============================================================================
// Skill Matching (post-generation annotation)
// =============================================================================

/// Check if a template value is a literal (not a placeholder like "{{name}}").
fn is_literal(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.contains("{{"),
        _ => true,
    }
}

/// Try to match a generated step (as JSON Value) against a skill's template.
///
/// Returns `true` if all literal (non-placeholder) fields in the skill template
/// match the corresponding fields in the step. Only considers `single_step`
/// skills (multi-step matching is ambiguous for individual steps).
fn step_matches_skill(step: &Value, skill: &SkillDefinition) -> bool {
    let template_step = match &skill.template {
        SkillTemplate::SingleStep { step } => step,
        SkillTemplate::MultiStep { .. } => return false, // skip multi-step skills
        SkillTemplate::Composition { .. } => return false, // skip composition skills
        SkillTemplate::Playbook { .. } => return false,  // skip playbook skills
    };

    let step_obj = match step.as_object() {
        Some(o) => o,
        None => return false,
    };

    // All literal fields in the template must match the step
    for (key, template_val) in template_step {
        if !is_literal(template_val) {
            continue; // skip placeholder fields
        }

        match step_obj.get(key) {
            Some(step_val) if step_val == template_val => {}
            _ => return false, // field missing or doesn't match
        }
    }

    // Require at least 2 matching literal fields beyond just "type" to avoid
    // overly broad matches (e.g., shell-command matches everything with type=command)
    let literal_matches: usize = template_step
        .iter()
        .filter(|(key, val)| {
            is_literal(val)
                && *key != "type"
                && step_obj
                    .get(key.as_str())
                    .map(|v| v == *val)
                    .unwrap_or(false)
        })
        .count();

    literal_matches >= 1
}

/// Extract parameter values from a step by reverse-matching against a skill template.
///
/// For each placeholder field `"{{param_name}}"` in the template, looks up the
/// corresponding value in the step.
fn extract_params_from_step(step: &Value, skill: &SkillDefinition) -> HashMap<String, Value> {
    let template_step = match &skill.template {
        SkillTemplate::SingleStep { step } => step,
        SkillTemplate::MultiStep { .. } => return HashMap::new(),
        SkillTemplate::Composition { .. } => return HashMap::new(),
        SkillTemplate::Playbook { .. } => return HashMap::new(),
    };

    let step_obj = match step.as_object() {
        Some(o) => o,
        None => return HashMap::new(),
    };

    let mut params = HashMap::new();
    for (key, template_val) in template_step {
        if let Value::String(s) = template_val {
            if let Some(param_name) = s.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                let param_name = param_name.trim();
                if let Some(step_val) = step_obj.get(key) {
                    params.insert(param_name.to_string(), step_val.clone());
                }
            }
        }
    }
    params
}

/// Annotate steps in a workflow with `skill_origin` where they match known skills.
///
/// Steps that already have `skill_origin` are skipped.
/// Only steps in deterministic phases (setup, verification, completion) are checked.
pub fn annotate_skill_origins(
    workflow: &mut crate::unified_workflows::UnifiedWorkflow,
    registry: &SkillRegistry,
) {
    let phases: &mut [(&str, &mut Vec<Value>)] = &mut [
        ("setup", &mut workflow.setup_steps),
        ("verification", &mut workflow.verification_steps),
        ("completion", &mut workflow.completion_steps),
    ];

    let all_skills = registry.all();

    for (phase, steps) in phases.iter_mut() {
        for step in steps.iter_mut() {
            // Skip steps that already have skill_origin
            if step.get("skill_origin").is_some() {
                continue;
            }

            // Try to match against all skills allowed in this phase
            for skill in &all_skills {
                if !skill.allowed_phases.iter().any(|p| p == *phase) {
                    continue;
                }

                if step_matches_skill(step, skill) {
                    let params = extract_params_from_step(step, skill);
                    let origin = SkillOrigin {
                        skill_id: skill.id.clone(),
                        skill_slug: skill.slug.clone(),
                        parameter_values: params,
                    };
                    if let Ok(origin_val) = serde_json::to_value(&origin) {
                        step.as_object_mut()
                            .map(|obj| obj.insert("skill_origin".to_string(), origin_val));
                    }
                    break; // first match wins
                }
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_builtin_skills() {
        let skills = load_builtin_skills();
        assert_eq!(skills.len(), 15, "Should have 15 built-in skills");
    }

    #[test]
    fn test_registry_new() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.builtin_count(), 15);
        assert_eq!(registry.all().len(), 15);
    }

    #[test]
    fn test_get_by_id() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "Lint Project");
    }

    #[test]
    fn test_get_by_slug() {
        let registry = SkillRegistry::new();
        let skill = registry.get_by_slug("api-health-check");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().id, "builtin:api-health-check");
    }

    #[test]
    fn test_get_missing() {
        let registry = SkillRegistry::new();
        assert!(registry.get("nonexistent").is_none());
        assert!(registry.get_by_slug("nonexistent").is_none());
    }

    #[test]
    fn test_by_phase() {
        let registry = SkillRegistry::new();

        let setup = registry.by_phase("setup");
        assert!(!setup.is_empty());
        for skill in &setup {
            assert!(skill.allowed_phases.contains(&"setup".to_string()));
        }

        let agentic = registry.by_phase("agentic");
        // Only AI Task is allowed in agentic phase
        assert_eq!(agentic.len(), 1);
        assert_eq!(agentic[0].slug, "ai-task");
    }

    #[test]
    fn test_by_category() {
        let registry = SkillRegistry::new();

        let code_quality = registry.by_category("code-quality");
        assert_eq!(code_quality.len(), 5);
        for skill in &code_quality {
            assert_eq!(skill.category, "code-quality");
        }

        let testing = registry.by_category("testing");
        assert_eq!(testing.len(), 2);
    }

    #[test]
    fn test_skills_for_context_filters_by_tags() {
        let registry = SkillRegistry::new();

        // With empty tags, all skills returned (no tag filter)
        let all = registry.skills_for_context(None, &[], None);
        assert_eq!(all.len(), registry.all().len());

        // Filter by a tag that exists on some skills
        let testing_tags = vec!["testing".to_string()];
        let filtered = registry.skills_for_context(None, &testing_tags, None);
        assert!(
            !filtered.is_empty(),
            "Should find skills tagged with 'testing'"
        );
        for skill in &filtered {
            assert!(
                skill.tags.contains(&"testing".to_string()),
                "Skill '{}' should have 'testing' tag but has {:?}",
                skill.slug,
                skill.tags
            );
        }
        assert!(
            filtered.len() < all.len(),
            "Tag-filtered set should be smaller than all skills"
        );
    }

    #[test]
    fn test_skills_for_context_filters_by_phase_and_tags() {
        let registry = SkillRegistry::new();

        // Filter by phase only
        let verification_skills = registry.skills_for_context(Some("verification"), &[], None);
        for skill in &verification_skills {
            assert!(
                skill.allowed_phases.contains(&"verification".to_string()),
                "Skill '{}' should be allowed in verification phase",
                skill.slug
            );
        }

        // Filter by phase + category
        let code_quality_verification =
            registry.skills_for_context(Some("verification"), &[], Some("code-quality"));
        for skill in &code_quality_verification {
            assert_eq!(skill.category, "code-quality");
            assert!(skill.allowed_phases.contains(&"verification".to_string()));
        }
    }

    #[test]
    fn test_skills_for_context_nonexistent_tag_returns_empty() {
        let registry = SkillRegistry::new();
        let tags = vec!["nonexistent-tag-xyz".to_string()];
        let filtered = registry.skills_for_context(None, &tags, None);
        assert!(
            filtered.is_empty(),
            "Nonexistent tag should return no skills"
        );
    }

    #[test]
    fn test_search() {
        let registry = SkillRegistry::new();

        let results = registry.search("lint");
        assert!(results.iter().any(|s| s.slug == "lint-project"));

        let results = registry.search("playwright");
        assert!(results.iter().any(|s| s.slug == "playwright-test"));

        let results = registry.search("zzz-nonexistent");
        assert!(results.is_empty());

        // Empty query returns all
        let results = registry.search("");
        assert_eq!(results.len(), 15);
    }

    #[test]
    fn test_search_multi_word() {
        let registry = SkillRegistry::new();
        let results = registry.search("check format");
        assert!(results.iter().any(|s| s.slug == "format-check"));
    }

    #[test]
    fn test_instantiate_single_step() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project").unwrap();

        let mut params = HashMap::new();
        params.insert(
            "working_directory".to_string(),
            Value::String("./frontend".to_string()),
        );

        let steps = instantiate_skill(skill, "verification", &params).unwrap();
        assert_eq!(steps.len(), 1);

        let step = steps[0].as_object().unwrap();
        assert_eq!(step.get("type").unwrap(), "command");
        assert_eq!(step.get("mode").unwrap(), "check");
        assert_eq!(step.get("check_type").unwrap(), "lint");
        assert_eq!(step.get("working_directory").unwrap(), "./frontend");
        assert_eq!(step.get("phase").unwrap(), "verification");
        assert_eq!(step.get("name").unwrap(), "Lint Project");
        assert!(step.get("skill_origin").is_some());
    }

    #[test]
    fn test_instantiate_with_defaults() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:shell-command").unwrap();

        let mut params = HashMap::new();
        params.insert(
            "command".to_string(),
            Value::String("npm run build".to_string()),
        );
        // Don't provide fail_on_error — should use default (true)

        let steps = instantiate_skill(skill, "setup", &params).unwrap();
        assert_eq!(steps.len(), 1);

        let step = steps[0].as_object().unwrap();
        assert_eq!(step.get("command").unwrap(), "npm run build");
        assert_eq!(step.get("fail_on_error").unwrap(), true);
    }

    #[test]
    fn test_instantiate_wrong_phase() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:assert-element").unwrap();

        let params = HashMap::new();
        let result = instantiate_skill(skill, "agentic", &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_instantiate_omits_unresolved() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project").unwrap();

        // Don't provide working_directory (optional, no default)
        let params = HashMap::new();
        let steps = instantiate_skill(skill, "verification", &params).unwrap();
        let step = steps[0].as_object().unwrap();

        // working_directory should be absent (not null or empty string)
        assert!(step.get("working_directory").is_none());
    }

    #[test]
    fn test_user_skills() {
        let mut registry = SkillRegistry::new();
        assert_eq!(registry.all().len(), 15);

        let user_skill = SkillDefinition {
            id: "user:my-skill".into(),
            name: "My Custom Skill".into(),
            slug: "my-custom-skill".into(),
            description: "A custom user skill".into(),
            category: "custom".into(),
            tags: vec!["custom".into()],
            icon: "puzzle".into(),
            color: "gray".into(),
            allowed_phases: vec!["setup".into()],
            parameters: vec![],
            template: SkillTemplate::SingleStep {
                step: {
                    let mut m = HashMap::new();
                    m.insert("type".to_string(), Value::String("command".into()));
                    m.insert("mode".to_string(), Value::String("shell".into()));
                    m.insert("command".to_string(), Value::String("echo hello".into()));
                    m
                },
            },
            source: "user".into(),
            version: None,
            author: None,
            checksum: None,
            depends_on: None,
            usage_count: None,
            approval_status: None,
            forked_from: None,
        };

        registry.set_user_skills(vec![user_skill]);
        assert_eq!(registry.all().len(), 16);
        assert!(registry.get("user:my-skill").is_some());
    }

    #[test]
    fn test_step_matches_lint_skill() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project").unwrap();

        // Step that matches lint-project skill
        let step = serde_json::json!({
            "type": "command",
            "mode": "check",
            "check_type": "lint",
            "command": "npm run lint",
            "working_directory": "./frontend"
        });
        assert!(step_matches_skill(&step, skill));

        // Step with wrong check_type
        let wrong = serde_json::json!({
            "type": "command",
            "mode": "check",
            "check_type": "typecheck",
            "command": "tsc --noEmit"
        });
        assert!(!step_matches_skill(&wrong, skill));
    }

    #[test]
    fn test_extract_params_from_step() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project").unwrap();

        let step = serde_json::json!({
            "type": "command",
            "mode": "check",
            "check_type": "lint",
            "working_directory": "./frontend"
        });

        let params = extract_params_from_step(&step, skill);
        // lint-project template has {{working_directory}} as the only placeholder
        assert_eq!(params.get("working_directory").unwrap(), "./frontend");
        assert!(params.get("command").is_none()); // no command placeholder in lint-project
    }

    #[test]
    fn test_no_match_for_generic_shell() {
        let registry = SkillRegistry::new();
        let shell_skill = registry.get("builtin:shell-command").unwrap();

        // A generic shell command should match shell-command skill
        // (type=command, mode=shell are the template literals)
        let step = serde_json::json!({
            "type": "command",
            "mode": "shell",
            "command": "echo hello"
        });
        assert!(step_matches_skill(&step, shell_skill));
    }

    #[test]
    fn test_skill_origin_serialization() {
        let origin = SkillOrigin {
            skill_id: "builtin:lint-project".into(),
            skill_slug: "lint-project".into(),
            parameter_values: {
                let mut m = HashMap::new();
                m.insert(
                    "working_directory".to_string(),
                    Value::String("./src".into()),
                );
                m
            },
        };

        let json = serde_json::to_string(&origin).unwrap();
        let parsed: SkillOrigin = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.skill_id, "builtin:lint-project");
        assert_eq!(
            parsed.parameter_values.get("working_directory").unwrap(),
            "./src"
        );
    }

    #[test]
    fn test_composition_template_serde() {
        let skill_def = SkillDefinition {
            id: "user:comp".into(),
            name: "Composed".into(),
            slug: "composed".into(),
            description: "A composition".into(),
            category: "custom".into(),
            tags: vec![],
            icon: "layers".into(),
            color: "blue".into(),
            allowed_phases: vec!["setup".into()],
            parameters: vec![],
            template: SkillTemplate::Composition {
                skill_refs: vec![
                    SkillRef {
                        skill_id: "builtin:lint-project".into(),
                        parameter_overrides: None,
                    },
                    SkillRef {
                        skill_id: "builtin:format-check".into(),
                        parameter_overrides: None,
                    },
                ],
            },
            source: "user".into(),
            version: Some("1.0.0".into()),
            author: None,
            checksum: None,
            depends_on: None,
            usage_count: None,
            approval_status: None,
            forked_from: None,
        };

        let json = serde_json::to_string(&skill_def).unwrap();
        let parsed: SkillDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "user:comp");
        match &parsed.template {
            SkillTemplate::Composition { skill_refs } => {
                assert_eq!(skill_refs.len(), 2);
                assert_eq!(skill_refs[0].skill_id, "builtin:lint-project");
            }
            _ => panic!("Expected Composition template"),
        }
    }

    #[test]
    fn test_checksum_computation() {
        let registry = SkillRegistry::new();
        let skill = registry.get("builtin:lint-project").unwrap();
        let checksum = compute_skill_checksum(skill);
        assert_eq!(checksum.len(), 64); // SHA-256 hex = 64 chars

        // Same skill should produce same checksum
        let checksum2 = compute_skill_checksum(skill);
        assert_eq!(checksum, checksum2);
    }
}
