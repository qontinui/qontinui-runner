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

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::commands::ui_bridge::{
    ElementActionInfo, ElementIdentifier, ElementRect, ElementState, UIBridgeElement,
};
use qontinui_types::ir::IrEffect;

/// Classify the AWAS action behind an `awas_execute` custom action.
///
/// **This reads a declaration the AWAS app author already made; it does not
/// infer one.** `AwasAction::side_effect` is part of the manifest format, and
/// served policy `testing` `an-actions-safety-class-is-declared-not-re-derived`
/// is explicit that an author's declaration is always preferred to a session's
/// inference. Discarding it and emitting `None` for every action — as the
/// pre-`ElementActionInfo` shape forced — throws away the only judgement anyone
/// has made about these endpoints.
///
/// | `side_effect` | result |
/// |---|---|
/// | `Some(false)` | `Read` — the author declared no side effect |
/// | `Some(true)` + `DELETE` | `Destructive` — the author chose DELETE, and removal is that method's own semantics |
/// | `Some(true)` | `Write` — a declared mutation |
/// | `None` | `None` — **UNCLASSIFIED, not safe** |
///
/// **The `None` arm is load-bearing and must not be "improved" into a
/// method-derived default.** A method gives idempotency semantics, not blast
/// radius: an arbitrary third-party `POST` is equally "add to cart" and "wire
/// $10,000". Synthesising a class there would manufacture a confident answer
/// nobody judged — the same fail-open shape `core/action-effect.ts` refuses for
/// the SDK verb map, where a default rendered as a declaration is a lie on
/// exactly the surface the annotation exists to protect. The existing
/// `_ => "custom"` element-type arm proves unknown methods reach this function.
///
/// Known bound, recorded rather than silently resolved: `Some(true)` + `POST`
/// maps to `Write`, not `Destructive`. Dimensions 2 (the state is an external
/// party's) and 3 (a remote mutation is invisible in the local GUI) of
/// `operating-rules` `what-makes-an-action-destructive` both argue for the
/// stricter reading, but classifying every AWAS mutation `destructive` excludes
/// the whole surface from automatic walks. Plan
/// `2026-09-04-effect-calculus-joins-the-component-action-registry`,
/// Design decision 4, flags this as a product call on the tenant's AWAS surface.
fn awas_execute_effect(action: &AwasAction) -> Option<IrEffect> {
    match action.side_effect {
        Some(false) => Some(IrEffect::Read),
        Some(true) if action.method.eq_ignore_ascii_case("DELETE") => Some(IrEffect::Destructive),
        Some(true) => Some(IrEffect::Write),
        None => None,
    }
}

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
        // One custom action per AWAS element, now carrying the safety class the
        // manifest already declared. See `awas_execute_effect` — an absent
        // `side_effect` yields `effect: None`, which the schema defines as
        // UNCLASSIFIED rather than safe.
        custom_actions: Some(vec![ElementActionInfo {
            id: "awas_execute".to_string(),
            label: action.name.clone(),
            description: action.intent.clone(),
            param_schema: None,
            effect: awas_execute_effect(action),
        }]),
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
            visible: true, // Assumed available if in manifest
            enabled: true, // Assumed enabled
            // No live DOM ref behind a manifest-declared action, so neither
            // the native `disabled` property nor `aria-disabled` is
            // observable. Both stay `false`, which is the fold consistent
            // with the assumed-enabled signal above (`enabled ==
            // !(disabled || aria_disabled)`).
            disabled: false,
            aria_disabled: false,
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
        // AWAS actions are manifest-declared, not live DOM refs — no bbox
        // is available. The bbox-first click provider will miss on these
        // and the caller falls through to the structured criteria matcher.
        bbox: None,
        visible: None,
        // ARIA / DOM walker fields — not populated for manifest-declared
        // AWAS elements (no live DOM ref to read from). Downstream criteria
        // matchers must tolerate `None` for these.
        role: None,
        tag_name: None,
        aria_label: None,
        accessible_name: None,
        text: None,
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

    fn awas(method: &str, side_effect: Option<bool>) -> AwasAction {
        AwasAction {
            id: "act".to_string(),
            name: None,
            method: method.to_string(),
            endpoint: "/api/act".to_string(),
            intent: None,
            side_effect,
            parameters: vec![],
        }
    }

    /// The manifest's own `side_effect` declaration is READ, not re-derived.
    #[test]
    fn awas_execute_effect_reads_the_authors_declaration() {
        assert_eq!(
            awas_execute_effect(&awas("GET", Some(false))),
            Some(IrEffect::Read)
        );
        assert_eq!(
            awas_execute_effect(&awas("POST", Some(true))),
            Some(IrEffect::Write)
        );
        assert_eq!(
            awas_execute_effect(&awas("PUT", Some(true))),
            Some(IrEffect::Write)
        );
        assert_eq!(
            awas_execute_effect(&awas("DELETE", Some(true))),
            Some(IrEffect::Destructive)
        );
        // Method casing is the manifest author's choice, not a contract.
        assert_eq!(
            awas_execute_effect(&awas("delete", Some(true))),
            Some(IrEffect::Destructive)
        );
    }

    /// **The load-bearing arm.** An absent `side_effect` is UNCLASSIFIED, and
    /// must NEVER become a method-derived default: a method gives idempotency
    /// semantics, not blast radius, so an arbitrary third-party POST is equally
    /// "add to cart" and "wire $10,000". If someone later "improves" this into
    /// a `match method` fallback, this test is what stops it.
    #[test]
    fn an_undeclared_side_effect_stays_unclassified_for_every_method() {
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "WEIRD", ""] {
            assert_eq!(
                awas_execute_effect(&awas(method, None)),
                None,
                "method {method:?} must not manufacture a class the manifest never declared"
            );
        }
    }

    /// A declared `read` must survive onto the element, because the whole point
    /// of widening `custom_actions` was to make the class REACHABLE.
    #[test]
    fn the_declared_class_reaches_the_element_custom_action() {
        let mut action = awas("GET", Some(false));
        action.intent = Some("List invoices".to_string());
        action.name = Some("List".to_string());

        let element = awas_action_to_ui_bridge_element(&action, None);
        let custom = element.custom_actions.expect("custom actions present");
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].id, "awas_execute");
        assert_eq!(custom[0].effect, Some(IrEffect::Read));
        assert_eq!(custom[0].label, Some("List".to_string()));
        assert_eq!(custom[0].description, Some("List invoices".to_string()));
    }

    /// And an undeclared one reaches it ABSENT rather than defaulted.
    #[test]
    fn an_undeclared_class_reaches_the_element_absent() {
        let element = awas_action_to_ui_bridge_element(&awas("POST", None), None);
        let custom = element
            .custom_actions
            .as_ref()
            .expect("custom actions present");
        assert_eq!(custom[0].effect, None);

        // Absent on the WIRE too — `skip_serializing_if` must keep it out, so a
        // consumer reads "unclassified" rather than a class nobody chose.
        let json = serde_json::to_string(&element).expect("serializes");
        assert!(
            !json.contains("\"effect\""),
            "an unclassified action must not emit an `effect` key: {json}"
        );
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
