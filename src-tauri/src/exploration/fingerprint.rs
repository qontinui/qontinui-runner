//! Fingerprint generation from SDK element JSON.
//!
//! Ports the TypeScript `fingerprintGenerator.ts` logic to Rust.
//! Generates stable element fingerprints for co-occurrence analysis.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

use super::types::{ElementFingerprint, RelativePosition, RepeatPattern};

// =============================================================================
// Configuration
// =============================================================================

pub struct FingerprintConfig {
    pub viewport_width: f64,
    pub viewport_height: f64,
}

impl Default for FingerprintConfig {
    fn default() -> Self {
        Self {
            viewport_width: 1920.0,
            viewport_height: 1080.0,
        }
    }
}

// =============================================================================
// Constants
// =============================================================================

const LANDMARK_ROLES: &[&str] = &[
    "banner",
    "complementary",
    "contentinfo",
    "form",
    "main",
    "navigation",
    "region",
    "search",
];

fn implicit_landmark(tag: &str) -> Option<&'static str> {
    match tag {
        "header" => Some("banner"),
        "footer" => Some("contentinfo"),
        "nav" => Some("navigation"),
        "main" => Some("main"),
        "aside" => Some("complementary"),
        "form" => Some("form"),
        "search" => Some("search"),
        _ => None,
    }
}

static DYNAMIC_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"\b\d{1,2}[/:.\-]\d{1,2}(?:[/:.\-]\d{2,4})?\b").unwrap(),
        Regex::new(r"\b\d+\b").unwrap(),
        Regex::new(r"(?i)\b[a-f0-9]{8,}\b").unwrap(),
        Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
            .unwrap(),
    ]
});

// =============================================================================
// Element Accessors (work with serde_json::Value)
// =============================================================================

fn get_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn get_bounds(el: &Value) -> (f64, f64, f64, f64) {
    // Try "rect" first (SDK snapshot format), then "bounds"
    let b = el
        .get("state")
        .and_then(|s| s.get("rect"))
        .or_else(|| el.get("bounds"))
        .or_else(|| el.get("rect"));

    match b {
        Some(b) => (
            get_f64(b, "x"),
            get_f64(b, "y"),
            get_f64(b, "width"),
            get_f64(b, "height"),
        ),
        None => (0.0, 0.0, 0.0, 0.0),
    }
}

fn get_element_id(el: &Value) -> &str {
    el.get("id").and_then(|v| v.as_str()).unwrap_or("")
}

fn get_tag_name(el: &Value) -> String {
    let tag = el
        .get("tagName")
        .and_then(|v| v.as_str())
        .or_else(|| el.get("type").and_then(|v| v.as_str()))
        .unwrap_or("unknown");
    tag.to_lowercase()
}

fn get_role(el: &Value) -> String {
    // Check accessibility.role, then top-level role
    el.get("accessibility")
        .and_then(|a| a.get("role"))
        .and_then(|v| v.as_str())
        .or_else(|| el.get("role").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn get_accessible_name(el: &Value) -> Option<String> {
    el.get("accessibleName")
        .and_then(|v| v.as_str())
        .or_else(|| {
            el.get("accessibility")
                .and_then(|a| a.get("accessibleName"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| el.get("label").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn get_aria_label(el: &Value) -> Option<String> {
    el.get("accessibility")
        .and_then(|a| a.get("ariaLabel"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_parent_id(el: &Value) -> Option<&str> {
    el.get("parent").and_then(|v| v.as_str())
}

fn get_children_ids(el: &Value) -> Vec<&str> {
    el.get("children")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

fn is_visible(el: &Value) -> bool {
    // Check state.visible or top-level visible
    el.get("state")
        .and_then(|s| s.get("visible"))
        .and_then(|v| v.as_bool())
        .or_else(|| el.get("visible").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

// =============================================================================
// Structural Path
// =============================================================================

fn compute_structural_path(el: &Value, index: &HashMap<String, &Value>) -> String {
    let mut parts = Vec::new();
    let mut current = Some(el);
    let mut depth = 0;

    while let Some(cur) = current {
        if depth > 30 {
            break;
        }
        parts.push(get_tag_name(cur));

        current = get_parent_id(cur).and_then(|pid| index.get(pid).copied());
        depth += 1;
    }

    parts.reverse();
    parts.join(" > ")
}

// =============================================================================
// Position Zone
// =============================================================================

fn classify_position_zone(el: &Value, config: &FingerprintConfig) -> String {
    let role = get_role(el);
    if role == "dialog" || role == "alertdialog" {
        return "modal".to_string();
    }

    if let Some(label) = get_aria_label(el) {
        if label.to_lowercase().contains("modal") {
            return "modal".to_string();
        }
    }

    let (x, y, w, h) = get_bounds(el);
    let center_x = x + w / 2.0;
    let center_y = y + h / 2.0;
    let vw = config.viewport_width;
    let vh = config.viewport_height;

    if vh <= 0.0 || vw <= 0.0 {
        return "main".to_string();
    }

    let rel_y = center_y / vh;
    let rel_x = center_x / vw;

    if rel_y < 0.12 {
        "header".to_string()
    } else if rel_y > 0.88 {
        "footer".to_string()
    } else if rel_x < 0.18 {
        "sidebar-left".to_string()
    } else if rel_x > 0.82 {
        "sidebar-right".to_string()
    } else {
        "main".to_string()
    }
}

// =============================================================================
// Size Category
// =============================================================================

fn classify_size_category(bounds: (f64, f64, f64, f64), config: &FingerprintConfig) -> String {
    let (_, _, w, h) = bounds;
    let max_dim = w.max(h);

    if w > config.viewport_width * 0.8 {
        "fullwidth".to_string()
    } else if w > 400.0 && h > 400.0 {
        "panel".to_string()
    } else if max_dim < 32.0 {
        "icon".to_string()
    } else if max_dim < 64.0 || (w < 200.0 && h < 50.0) {
        "button".to_string()
    } else if max_dim < 150.0 {
        "small".to_string()
    } else if max_dim < 400.0 {
        "medium".to_string()
    } else {
        "large".to_string()
    }
}

// =============================================================================
// Landmark Context
// =============================================================================

fn determine_landmark_context(
    el: &Value,
    index: &HashMap<String, &Value>,
) -> (String, Option<String>) {
    let mut current = Some(el);
    let mut depth = 0;

    while let Some(cur) = current {
        if depth > 20 {
            break;
        }

        let role = get_role(cur);
        if LANDMARK_ROLES.contains(&role.as_str()) {
            return (role, get_aria_label(cur));
        }

        let tag = get_tag_name(cur);
        if let Some(implicit) = implicit_landmark(&tag) {
            return (implicit.to_string(), get_aria_label(cur));
        }

        current = get_parent_id(cur).and_then(|pid| index.get(pid).copied());
        depth += 1;
    }

    ("none".to_string(), None)
}

// =============================================================================
// Accessible Name Normalization
// =============================================================================

fn normalize_accessible_name(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    if name.is_empty() {
        return None;
    }

    let mut normalized = name.to_string();
    for pattern in DYNAMIC_PATTERNS.iter() {
        normalized = pattern.replace_all(&normalized, "N").to_string();
    }

    // Collapse multiple spaces
    static RE_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    normalized = RE_SPACES.replace_all(&normalized, " ").trim().to_string();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

// =============================================================================
// Repeat Pattern Detection
// =============================================================================

fn detect_repeat_pattern(
    el: &Value,
    index: &HashMap<String, &Value>,
) -> (bool, Option<RepeatPattern>) {
    let parent_id = match get_parent_id(el) {
        Some(pid) => pid,
        None => return (false, None),
    };

    let parent = match index.get(parent_id) {
        Some(p) => *p,
        None => return (false, None),
    };

    let children = get_children_ids(parent);
    if children.len() < 3 {
        return (false, None);
    }

    let my_tag = get_tag_name(el);
    let my_role = get_role(el);
    let my_id = get_element_id(el);

    let mut same_count = 0usize;
    let mut my_index = 0usize;

    for child_id in &children {
        if let Some(sib) = index.get(*child_id) {
            let sib_tag = get_tag_name(sib);
            let sib_role = get_role(sib);

            if sib_tag == my_tag && sib_role == my_role {
                if get_element_id(sib) == my_id {
                    my_index = same_count;
                }
                same_count += 1;
            }
        }
    }

    if same_count < 3 {
        return (false, None);
    }

    let parent_tag = get_tag_name(parent);
    let parent_role = get_role(parent);

    let container_type = if parent_tag == "table" || parent_tag == "tbody" {
        "table"
    } else if parent_role == "grid" || parent_role == "treegrid" {
        "grid"
    } else {
        "list"
    };

    (
        true,
        Some(RepeatPattern {
            r#type: container_type.to_string(),
            container_selector: parent_tag.clone(),
            container_role: parent_role,
            item_selector: my_tag,
            item_role: my_role,
            index: my_index,
            total_count: same_count,
            container_tag: parent_tag,
        }),
    )
}

// =============================================================================
// FNV-1a Hash (matches TypeScript implementation exactly)
// =============================================================================

fn compute_hash(
    structural_path: &str,
    position_zone: &str,
    landmark_context: &str,
    role: &str,
    accessible_name: Option<&str>,
    size_category: &str,
    tag_name: &str,
) -> String {
    let input = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        structural_path,
        position_zone,
        landmark_context,
        role,
        accessible_name.unwrap_or(""),
        size_category,
        tag_name
    );

    // FNV-1a forward pass
    let mut hash1: u32 = 0x811c9dc5;
    for b in input.bytes() {
        hash1 ^= b as u32;
        hash1 = hash1.wrapping_mul(0x01000193);
    }

    // FNV-1a reverse pass (different seed)
    let mut hash2: u32 = 0x6c62272e;
    for b in input.bytes().rev() {
        hash2 ^= b as u32;
        hash2 = hash2.wrapping_mul(0x01000193);
    }

    format!("{:08x}{:08x}", hash1, hash2)
}

// =============================================================================
// Public API
// =============================================================================

/// Generate fingerprints for all elements from a snapshot JSON response.
///
/// Returns a map of fingerprint hash → ElementFingerprint, plus a map
/// of element ID → fingerprint hash.
pub fn generate_fingerprints(
    elements: &[Value],
    config: &FingerprintConfig,
) -> (HashMap<String, ElementFingerprint>, HashMap<String, String>) {
    // Build element index: id → &Value
    let index: HashMap<String, &Value> = elements
        .iter()
        .filter_map(|el| {
            let id = get_element_id(el);
            if id.is_empty() {
                None
            } else {
                Some((id.to_string(), el))
            }
        })
        .collect();

    let mut catalog: HashMap<String, ElementFingerprint> = HashMap::new();
    let mut element_map: HashMap<String, String> = HashMap::new();

    for el in elements {
        let id = get_element_id(el);
        if id.is_empty() || !is_visible(el) {
            continue;
        }

        let structural_path = compute_structural_path(el, &index);
        let position_zone = classify_position_zone(el, config);
        let bounds = get_bounds(el);
        let size_category = classify_size_category(bounds, config);
        let (landmark_context, landmark_label) = determine_landmark_context(el, &index);
        let role = get_role(el);
        let tag_name = get_tag_name(el);
        let accessible_name = normalize_accessible_name(get_accessible_name(el).as_deref());
        let (is_repeating, repeat_pattern) = detect_repeat_pattern(el, &index);

        let (_, _, w, h) = bounds;
        let rel_x = if w > 0.0 {
            ((bounds.0 + w / 2.0) / config.viewport_width).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let rel_y = if h > 0.0 {
            ((bounds.1 + h / 2.0) / config.viewport_height).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let hash = compute_hash(
            &structural_path,
            &position_zone,
            &landmark_context,
            &role,
            accessible_name.as_deref(),
            &size_category,
            &tag_name,
        );

        let fp = ElementFingerprint {
            hash: hash.clone(),
            structural_path,
            position_zone,
            landmark_context,
            landmark_label,
            role,
            tag_name,
            accessible_name,
            size_category,
            relative_position: RelativePosition {
                top: rel_y,
                left: rel_x,
            },
            is_repeating,
            repeat_pattern,
        };

        catalog.insert(hash.clone(), fp);
        element_map.insert(id.to_string(), hash);
    }

    (catalog, element_map)
}

/// Extract unique fingerprint hashes from an element→hash map.
pub fn extract_hashes(element_map: &HashMap<String, String>) -> Vec<String> {
    let mut hashes: Vec<String> = element_map.values().cloned().collect();
    hashes.sort();
    hashes.dedup();
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_hash() {
        // Ensure the hash produces a 16-char hex string
        let h = compute_hash(
            "div > button",
            "main",
            "navigation",
            "button",
            Some("Click"),
            "button",
            "button",
        );
        assert_eq!(h.len(), 16);
        // All hex chars
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_normalize_accessible_name() {
        assert_eq!(
            normalize_accessible_name(Some("3 items")),
            Some("N items".to_string())
        );
        assert_eq!(
            normalize_accessible_name(Some("2024-01-15")),
            Some("N".to_string())
        );
        assert_eq!(normalize_accessible_name(Some("")), None);
        assert_eq!(normalize_accessible_name(None), None);
    }

    #[test]
    fn test_classify_size_category() {
        let cfg = FingerprintConfig::default();
        assert_eq!(classify_size_category((0.0, 0.0, 20.0, 20.0), &cfg), "icon");
        assert_eq!(
            classify_size_category((0.0, 0.0, 100.0, 30.0), &cfg),
            "button"
        );
        assert_eq!(
            classify_size_category((0.0, 0.0, 500.0, 500.0), &cfg),
            "panel"
        );
        assert_eq!(
            classify_size_category((0.0, 0.0, 1600.0, 50.0), &cfg),
            "fullwidth"
        );
    }
}
