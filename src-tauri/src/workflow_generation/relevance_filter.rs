//! Relevance Filter for Step Type Metadata
//!
//! Filters step types based on keywords in the workflow description
//! to reduce token usage by only including relevant step types in the AI prompt.

use super::step_type_metadata::{StepCategory, StepTypeMetadata};

/// Filter step types to only those relevant to the given description.
///
/// Always includes Core and Verification types. Conditionally includes
/// WebApp, GUI, AWAS, and Utility types based on keyword matching.
///
/// Expected token reduction: 40-60% for non-web/non-GUI workflows.
pub fn filter_relevant_step_types(
    description: &str,
    all_types: &'static [StepTypeMetadata],
) -> Vec<&'static StepTypeMetadata> {
    let desc_lower = description.to_lowercase();

    let include_web = has_web_keywords(&desc_lower);
    let include_gui = has_gui_keywords(&desc_lower);
    let include_awas = has_awas_keywords(&desc_lower);

    all_types
        .iter()
        .filter(|meta| {
            match meta.category {
                // Always include Core and Verification types
                StepCategory::Core | StepCategory::Verification => true,
                // Always include Utility types (mcp_call, macro)
                StepCategory::Utility => true,
                // Conditional inclusion based on keywords
                StepCategory::WebApp => include_web,
                StepCategory::Gui => include_gui,
                StepCategory::Awas => include_awas,
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
        "script",
        "screenshot",
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
    ];
    WEB_KEYWORDS.iter().any(|kw| desc.contains(kw))
}

fn has_gui_keywords(desc: &str) -> bool {
    const GUI_KEYWORDS: &[&str] = &[
        "gui",
        "click",
        "mouse",
        "screen",
        "desktop",
        "keyboard",
        "hotkey",
        "scroll",
        "window",
        "application state",
        "visual automation",
        "image recognition",
    ];
    GUI_KEYWORDS.iter().any(|kw| desc.contains(kw))
}

fn has_awas_keywords(desc: &str) -> bool {
    const AWAS_KEYWORDS: &[&str] = &[
        "awas",
        "web automation",
        "manifest",
        "discover actions",
        "extract elements",
    ];
    AWAS_KEYWORDS.iter().any(|kw| desc.contains(kw))
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
        assert!(names.contains(&"shell_command"));
        assert!(names.contains(&"api_request"));
    }

    #[test]
    fn test_verification_types_always_included() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run pytest and fix errors", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"test"));
        assert!(names.contains(&"check"));
        assert!(names.contains(&"gate"));
    }

    #[test]
    fn test_python_workflow_excludes_awas_and_gui() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run pytest and fix errors", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(!names.contains(&"awas_discover"));
        assert!(!names.contains(&"gui_action"));
        assert!(!names.contains(&"state"));
    }

    #[test]
    fn test_web_keywords_include_webapp_types() {
        let all = get_all_step_type_metadata();
        let filtered =
            filter_relevant_step_types("check the web frontend has correct UI elements", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"script"));
        assert!(names.contains(&"screenshot"));
    }

    #[test]
    fn test_gui_keywords_include_gui_types() {
        let all = get_all_step_type_metadata();
        let filtered =
            filter_relevant_step_types("click the button on the desktop application", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"gui_action"));
        assert!(names.contains(&"state"));
    }

    #[test]
    fn test_awas_keywords_include_awas_types() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("discover awas actions on the page", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"awas_discover"));
        assert!(names.contains(&"awas_execute"));
    }

    #[test]
    fn test_utility_types_always_included() {
        let all = get_all_step_type_metadata();
        let filtered = filter_relevant_step_types("run a simple check", all);
        let names: Vec<&str> = filtered.iter().map(|m| m.step_type).collect();
        assert!(names.contains(&"mcp_call"));
        assert!(names.contains(&"macro"));
    }
}
