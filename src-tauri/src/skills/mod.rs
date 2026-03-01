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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameterOption {
    pub label: String,
    pub value: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SkillTemplate {
    #[serde(rename = "single_step")]
    SingleStep { step: HashMap<String, Value> },
    #[serde(rename = "multi_step")]
    MultiStep { steps: Vec<HashMap<String, Value>> },
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
    pub source: String, // "builtin" | "user"
}

/// Tracks that a step was created from a skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillOrigin {
    pub skill_id: String,
    pub skill_slug: String,
    pub parameter_values: HashMap<String, Value>,
}

// =============================================================================
// Built-in Skills
// =============================================================================

const BUILTIN_SKILLS_JSON: &str = include_str!("builtin.json");

fn load_builtin_skills() -> Vec<SkillDefinition> {
    serde_json::from_str(BUILTIN_SKILLS_JSON).expect("Failed to parse built-in skills JSON")
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

    /// Search skills by text query. Matches against name, description, slug, and tags.
    pub fn search(&self, query: &str) -> Vec<&SkillDefinition> {
        let query_lower = query.trim().to_lowercase();
        if query_lower.is_empty() {
            return self.all();
        }

        let words: Vec<&str> = query_lower.split_whitespace().collect();

        self.all()
            .into_iter()
            .filter(|skill| {
                let haystack = format!(
                    "{} {} {} {}",
                    skill.name,
                    skill.description,
                    skill.slug,
                    skill.tags.join(" ")
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect()
    }

    /// Get all built-in skill definitions as JSON (for AI generator context).
    pub fn builtin_as_json(&self) -> Value {
        serde_json::to_value(&self.builtin).unwrap_or(Value::Array(vec![]))
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

    let origin = SkillOrigin {
        skill_id: skill.id.clone(),
        skill_slug: skill.slug.clone(),
        parameter_values: effective_params.clone(),
    };

    let template_steps = match &skill.template {
        SkillTemplate::SingleStep { step } => vec![step.clone()],
        SkillTemplate::MultiStep { steps } => steps.clone(),
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
        };

        registry.set_user_skills(vec![user_skill]);
        assert_eq!(registry.all().len(), 16);
        assert!(registry.get("user:my-skill").is_some());
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
}
