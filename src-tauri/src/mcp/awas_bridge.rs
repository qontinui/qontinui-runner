//! AWAS to UI Bridge Adapter
//!
//! This module provides utilities to convert AWAS-discovered elements
//! to ui-bridge format, enabling unified element discovery and control.
//!
//! # Architecture
//!
//! AWAS and ui-bridge serve complementary purposes:
//! - **AWAS**: API-based discovery from /.well-known/ai-actions.json manifests
//! - **ui-bridge**: DOM-based element registry and real-time state tracking
//!
//! This adapter bridges the two systems by:
//! 1. Converting AWAS actions to UIBridgeElement format
//! 2. Including AWAS elements in unified discovery responses
//! 3. Routing AWAS action execution through ui-bridge infrastructure

use serde::{Deserialize, Serialize};

use crate::commands::ui_bridge::{ElementIdentifier, ElementRect, ElementState, UIBridgeElement};

/// AWAS action as returned from manifest discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwasAction {
    pub id: String,
    pub name: Option<String>,
    pub method: String, // GET, POST, etc.
    pub endpoint: String,
    pub intent: Option<String>,
    pub side_effect: Option<bool>,
    #[serde(default)]
    pub parameters: Vec<AwasParameter>,
}

/// AWAS action parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwasParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: Option<String>,
    pub required: Option<bool>,
    pub description: Option<String>,
}

/// AWAS manifest structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwasManifest {
    pub app_name: Option<String>,
    pub conformance_level: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub actions: Vec<AwasAction>,
}

/// Convert an AWAS action to a UIBridgeElement
///
/// This creates a ui-bridge compatible element from an AWAS action,
/// allowing it to be included in unified element discovery and
/// action execution flows.
pub fn awas_action_to_ui_bridge_element(
    action: &AwasAction,
    _base_url: Option<&str>,
) -> UIBridgeElement {
    // Infer element type from HTTP method
    let element_type = match action.method.to_uppercase().as_str() {
        "GET" => "link",                      // Navigation action
        "POST" | "PUT" | "PATCH" => "button", // Mutation action
        "DELETE" => "button",
        _ => "custom",
    }
    .to_string();

    // Build selector targeting AWAS-annotated elements
    let selector = format!("[data-awas-action=\"{}\"]", action.id);
    let xpath = format!("//*[@data-awas-action='{}']", action.id);

    // Determine available actions based on method
    let actions = match action.method.to_uppercase().as_str() {
        "GET" => vec!["click".to_string(), "navigate".to_string()],
        _ => vec!["click".to_string(), "execute".to_string()],
    };

    UIBridgeElement {
        id: format!("awas_{}", action.id),
        element_type,
        label: action.name.clone().or_else(|| Some(action.id.clone())),
        actions,
        custom_actions: Some(vec!["awas_execute".to_string()]),
        identifier: ElementIdentifier {
            ui_id: None, // Not a UI Bridge registered element
            test_id: None,
            awas_id: Some(action.id.clone()),
            html_id: None,
            xpath,
            selector,
        },
        state: ElementState {
            // AWAS elements don't have real-time state
            visible: true,  // Assumed available if in manifest
            enabled: true,  // Assumed enabled
            focused: false, // Not tracked
            rect: ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
            value: None,
            checked: None,
            selected_options: None,
            text_content: action.intent.clone(),
        },
        registered_at: chrono::Utc::now().timestamp_millis(),
        mounted: true, // Manifest-declared elements are always "mounted"
    }
}

/// Convert an entire AWAS manifest to UIBridgeElements
pub fn awas_manifest_to_ui_bridge_elements(manifest: &AwasManifest) -> Vec<UIBridgeElement> {
    manifest
        .actions
        .iter()
        .map(|action| awas_action_to_ui_bridge_element(action, manifest.base_url.as_deref()))
        .collect()
}

/// Check if an element ID refers to an AWAS element
pub fn is_awas_element(element_id: &str) -> bool {
    element_id.starts_with("awas_")
}

/// Extract the AWAS action ID from a ui-bridge element ID
pub fn extract_awas_action_id(element_id: &str) -> Option<String> {
    if is_awas_element(element_id) {
        Some(element_id.trim_start_matches("awas_").to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_awas_action_to_ui_bridge_element() {
        let action = AwasAction {
            id: "login".to_string(),
            name: Some("Login Action".to_string()),
            method: "POST".to_string(),
            endpoint: "/api/login".to_string(),
            intent: Some("Authenticate user".to_string()),
            side_effect: Some(true),
            parameters: vec![],
        };

        let element = awas_action_to_ui_bridge_element(&action, Some("https://example.com"));

        assert_eq!(element.id, "awas_login");
        assert_eq!(element.element_type, "button");
        assert_eq!(element.label, Some("Login Action".to_string()));
        assert!(element.actions.contains(&"execute".to_string()));
        assert_eq!(element.identifier.awas_id, Some("login".to_string()));
        assert_eq!(element.identifier.selector, "[data-awas-action=\"login\"]");
    }

    #[test]
    fn test_is_awas_element() {
        assert!(is_awas_element("awas_login"));
        assert!(is_awas_element("awas_submit_form"));
        assert!(!is_awas_element("button_submit"));
        assert!(!is_awas_element("ui_element_1"));
    }

    #[test]
    fn test_extract_awas_action_id() {
        assert_eq!(
            extract_awas_action_id("awas_login"),
            Some("login".to_string())
        );
        assert_eq!(
            extract_awas_action_id("awas_submit_form"),
            Some("submit_form".to_string())
        );
        assert_eq!(extract_awas_action_id("button_submit"), None);
    }
}
