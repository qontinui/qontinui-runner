//! Element Collector — Gathers UI elements for screenshot annotation.
//!
//! Collects interactive elements from two sources:
//! - **UI Bridge** (white-box): Fetches semantic snapshot via HTTP, extracts
//!   elements with normalizedRect coordinates.
//! - **Step outputs** (black-box): Reads DetectedElement from step execution
//!   results, which already have bounding boxes from cascade detection.
//!
//! Produces a list of `AnnotatedElement` suitable for the annotation engine.

use crate::mcp::types::get_mcp_api_port;
use crate::vision::annotator::AnnotatedElement;
use crate::vision::types::NormalizedRect;
use tracing::{debug, warn};

// =============================================================================
// UI Bridge Element Collection
// =============================================================================

/// Collect interactive elements from a UI Bridge snapshot for annotation.
///
/// Fetches the SDK snapshot endpoint, extracts elements with position data,
/// and converts them to `AnnotatedElement` with sequential numbering.
///
/// Returns an empty vec if the UI Bridge is not connected or the fetch fails.
pub async fn collect_from_ui_bridge() -> Vec<AnnotatedElement> {
    let port = get_mcp_api_port();
    let url = format!(
        "http://127.0.0.1:{}/ui-bridge/sdk/control/snapshot",
        port
    );

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let snapshot: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    extract_elements_from_snapshot(&snapshot)
}

/// Extract annotated elements from a UI Bridge snapshot JSON value.
///
/// Looks for interactive elements (button, input, select, textarea, link,
/// checkbox, radio) that have position data (rect or normalizedRect).
pub fn extract_elements_from_snapshot(snapshot: &serde_json::Value) -> Vec<AnnotatedElement> {
    let elements = match snapshot.get("elements").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            // Try nested under "data"
            match snapshot
                .get("data")
                .and_then(|d| d.get("elements"))
                .and_then(|v| v.as_array())
            {
                Some(arr) => arr,
                None => return Vec::new(),
            }
        }
    };

    let interactive_types = [
        "button", "input", "select", "textarea", "link", "checkbox", "radio", "a",
    ];

    let mut result = Vec::new();
    let mut index = 1_u32;

    for el in elements {
        let el_type = el
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Filter to interactive elements only
        if !interactive_types.contains(&el_type) {
            continue;
        }

        // Skip hidden/disabled elements
        if let Some(state) = el.get("state") {
            let visible = state
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !visible {
                continue;
            }
        }

        // Extract normalized coordinates
        let normalized_rect = extract_normalized_rect(el);
        let Some(normalized_rect) = normalized_rect else {
            continue;
        };

        let label = el
            .get("label")
            .and_then(|v| v.as_str())
            .or_else(|| {
                el.get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                el.get("accessibleName")
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("(unlabeled)")
            .to_string();

        // Truncate long labels
        let label = if label.len() > 40 {
            format!("{}...", &label[..37])
        } else {
            label
        };

        result.push(AnnotatedElement {
            index,
            label,
            element_type: el_type.to_string(),
            normalized_rect,
        });

        index += 1;

        // Cap at 50 elements to keep annotation readable and token-efficient
        if index > 50 {
            debug!("Element collector: capped at 50 elements");
            break;
        }
    }

    debug!(
        "Collected {} interactive elements from UI Bridge snapshot",
        result.len()
    );
    result
}

/// Extract a NormalizedRect from a snapshot element JSON value.
///
/// Tries normalizedRect first (already 0-1), then falls back to rect
/// with viewport dimensions for normalization.
fn extract_normalized_rect(el: &serde_json::Value) -> Option<NormalizedRect> {
    // Try normalizedRect (UI Bridge provides this directly)
    if let Some(nr) = el.get("normalizedRect").or_else(|| {
        el.get("state").and_then(|s| s.get("normalizedRect"))
    }) {
        let x = nr.get("x").and_then(|v| v.as_f64())? as f32;
        let y = nr.get("y").and_then(|v| v.as_f64())? as f32;
        let width = nr.get("width").and_then(|v| v.as_f64())? as f32;
        let height = nr.get("height").and_then(|v| v.as_f64())? as f32;
        return Some(NormalizedRect { x, y, width, height });
    }

    // Fallback: use rect with absolute pixel coords
    // Need viewport dimensions to normalize
    let rect = el.get("rect").or_else(|| {
        el.get("state").and_then(|s| s.get("rect"))
    })?;

    let x = rect.get("x").and_then(|v| v.as_f64())? as f32;
    let y = rect.get("y").and_then(|v| v.as_f64())? as f32;
    let w = rect.get("width").and_then(|v| v.as_f64())? as f32;
    let h = rect.get("height").and_then(|v| v.as_f64())? as f32;

    // Without viewport info, we can't normalize pixel coords
    // Assume a reasonable default viewport (1920x1080) as a heuristic
    // This is imperfect but better than dropping the element entirely
    if w > 0.0 && h > 0.0 {
        // If coords look already normalized (all < 1.5), pass through
        if x < 1.5 && y < 1.5 && w < 1.5 && h < 1.5 {
            return Some(NormalizedRect {
                x,
                y,
                width: w,
                height: h,
            });
        }
        // Otherwise assume pixel coords with 1920x1080 default
        return Some(NormalizedRect {
            x: x / 1920.0,
            y: y / 1080.0,
            width: w / 1920.0,
            height: h / 1080.0,
        });
    }

    None
}

// =============================================================================
// Step Output Element Collection (black-box path)
// =============================================================================

/// Convert DetectedElement step outputs to AnnotatedElements.
///
/// Used for black-box automation where elements come from cascade detection
/// (template matching, OCR, UIA, etc.) rather than UI Bridge.
pub fn collect_from_detected_elements(
    elements: &[crate::commands::step_outputs::DetectedElement],
) -> Vec<AnnotatedElement> {
    elements
        .iter()
        .enumerate()
        .filter_map(|(i, el)| {
            let normalized_rect = el.normalized_bounding_box.as_ref().map(|nb| {
                NormalizedRect {
                    x: nb.x,
                    y: nb.y,
                    width: nb.width,
                    height: nb.height,
                }
            })?;

            Some(AnnotatedElement {
                index: (i + 1) as u32,
                label: el.label.clone(),
                element_type: el.element_type.clone(),
                normalized_rect,
            })
        })
        .collect()
}

// =============================================================================
// Combined Collection
// =============================================================================

/// Collect elements from the best available source.
///
/// Tries UI Bridge first (richer data), falls back to detected elements
/// from step outputs if UI Bridge is unavailable.
pub async fn collect_elements(
    detected_elements: Option<&[crate::commands::step_outputs::DetectedElement]>,
) -> Vec<AnnotatedElement> {
    // Try UI Bridge first (white-box path)
    let ui_bridge_elements = collect_from_ui_bridge().await;
    if !ui_bridge_elements.is_empty() {
        return ui_bridge_elements;
    }

    // Fall back to step output elements (black-box path)
    if let Some(detected) = detected_elements {
        let collected = collect_from_detected_elements(detected);
        if !collected.is_empty() {
            return collected;
        }
    }

    Vec::new()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_elements_from_snapshot_with_normalized_rect() {
        let snapshot = serde_json::json!({
            "elements": [
                {
                    "type": "button",
                    "label": "Submit",
                    "normalizedRect": { "x": 0.5, "y": 0.8, "width": 0.2, "height": 0.05 },
                    "state": { "visible": true, "enabled": true }
                },
                {
                    "type": "input",
                    "label": "Email",
                    "normalizedRect": { "x": 0.1, "y": 0.3, "width": 0.6, "height": 0.04 },
                    "state": { "visible": true }
                },
                {
                    "type": "div",
                    "label": "Container",
                    "normalizedRect": { "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0 }
                }
            ]
        });

        let elements = extract_elements_from_snapshot(&snapshot);
        // div is not interactive, should be filtered out
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].label, "Submit");
        assert_eq!(elements[0].index, 1);
        assert_eq!(elements[1].label, "Email");
        assert_eq!(elements[1].index, 2);
    }

    #[test]
    fn test_extract_hidden_elements_skipped() {
        let snapshot = serde_json::json!({
            "elements": [
                {
                    "type": "button",
                    "label": "Hidden",
                    "normalizedRect": { "x": 0.1, "y": 0.1, "width": 0.1, "height": 0.05 },
                    "state": { "visible": false }
                },
                {
                    "type": "button",
                    "label": "Visible",
                    "normalizedRect": { "x": 0.5, "y": 0.5, "width": 0.2, "height": 0.05 },
                    "state": { "visible": true }
                }
            ]
        });

        let elements = extract_elements_from_snapshot(&snapshot);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].label, "Visible");
    }

    #[test]
    fn test_extract_with_pixel_rect_fallback() {
        let snapshot = serde_json::json!({
            "elements": [
                {
                    "type": "button",
                    "label": "Login",
                    "rect": { "x": 960.0, "y": 540.0, "width": 192.0, "height": 54.0 }
                }
            ]
        });

        let elements = extract_elements_from_snapshot(&snapshot);
        assert_eq!(elements.len(), 1);
        // Should be normalized using 1920x1080 default
        assert!((elements[0].normalized_rect.x - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_collect_from_detected_elements() {
        use crate::commands::step_outputs::{BoundingBox, DetectedElement};

        let detected = vec![
            DetectedElement {
                id: "1".into(),
                label: "Button".into(),
                element_type: "button".into(),
                text_content: None,
                bounding_box: BoundingBox {
                    x: 100,
                    y: 200,
                    width: 50,
                    height: 30,
                },
                normalized_bounding_box: Some(NormalizedRect {
                    x: 0.05,
                    y: 0.18,
                    width: 0.026,
                    height: 0.028,
                }),
                confidence: 0.95,
            },
        ];

        let annotated = collect_from_detected_elements(&detected);
        assert_eq!(annotated.len(), 1);
        assert_eq!(annotated[0].label, "Button");
        assert_eq!(annotated[0].index, 1);
    }

    #[test]
    fn test_empty_snapshot() {
        let snapshot = serde_json::json!({});
        assert!(extract_elements_from_snapshot(&snapshot).is_empty());
    }
}
