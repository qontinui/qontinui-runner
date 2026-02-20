//! Relevance Filter for Step Type Metadata
//!
//! Filters step types based on keywords in the workflow description
//! to reduce token usage by only including relevant step types in the AI prompt.

use super::step_type_metadata::{StepCategory, StepTypeMetadata};

/// Filter step types to only those relevant to the given description.
///
/// With only 4 core types, all types are almost always included since
/// the token overhead is minimal. The Automation category (ui_bridge) is
/// conditionally included based on web/UI keywords.
pub fn filter_relevant_step_types(
    description: &str,
    all_types: &'static [StepTypeMetadata],
) -> Vec<&'static StepTypeMetadata> {
    let desc_lower = description.to_lowercase();
    let include_automation = has_web_keywords(&desc_lower);

    all_types
        .iter()
        .filter(|meta| {
            match meta.category {
                // Always include Core, Verification, and Utility types
                StepCategory::Core | StepCategory::Verification | StepCategory::Utility => true,
                // Conditional inclusion based on keywords
                StepCategory::Automation => include_automation,
            }
        })
        .collect()
}

fn has_web_keywords(desc: &str) -> bool {
    const WEB_KEYWORDS: &[&str] = &[
        "web",
        "frontend",
        "ui",
        "localhost",
        "browser",
        "page",
        "react",
        "next",
        "playwright",
        "html",
        "css",
        "dom",
        "website",
        "webapp",
        "web app",
        "sidebar",
        "navigation",
        "component",
        "button",
        "form",
        "modal",
        "dialog",
        "ui bridge",
        "sdk",
    ];
    WEB_KEYWORDS.iter().any(|kw| desc.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::step_type_metadata::get_all_step_type_metadata;

    #[test]
    fn test_core_types_always_included() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run pytest and fix errors", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"prompt"));
        assert!(names.contains(&"command"));
    }

    #[test]
    fn test_verification_types_always_included() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run pytest and fix errors", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        // "test" was merged into "command" — command is always included
        assert!(names.contains(&"command"));
    }

    #[test]
    fn test_python_workflow_excludes_automation() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run pytest and fix errors", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(!names.contains(&"ui_bridge"));
    }

    #[test]
    fn test_web_keywords_include_automation_types() {
        let all = get_all_step_type_metadata();
        let filtered =
            filter_relevant_step_types("check the web frontend has correct UI elements", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"ui_bridge"));
    }
}
