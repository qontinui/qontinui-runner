//! Stable element fingerprinting for co-occurrence observations.
//!
//! A fingerprint must be identical across re-renders of the same physical
//! element. It is derived purely from semantic attributes (role,
//! accessibleName, tagName) plus a *normalized* structural hint that ignores
//! auto-generated ID suffixes. DOM-path hashes and random testid suffixes are
//! intentionally omitted — they flap across renders and would explode the
//! pair-frequency matrix with false negatives.
//!
//! The output is a 16-char SHA-256 hex prefix. Collisions at 64 bits are
//! vanishingly rare at the scales we care about (observations per spec × days
//! per window), and the shorter prefix keeps the JSONB column compact.

use sha2::{Digest, Sha256};

/// Maximum length of the accessibleName / text_content fragment folded into
/// the fingerprint. Longer text is truncated to keep fingerprints stable
/// against trailing whitespace or dynamic suffixes.
const NAME_TRUNC: usize = 60;

/// Compute a stable fingerprint for a single UI Bridge element JSON value.
///
/// Inputs consulted (first-match wins within each group):
/// - `role`      : `element.role` or `element.accessibility.role`
/// - `name`      : `element.accessibleName` or `element.accessibility.accessibleName`
///   or `element.label` or `element.text` (truncated to NAME_TRUNC)
/// - `tag`       : `element.tagName` (lowercased)
/// - `structure` : normalized structural hint — `element.parent` with digits
///   stripped when present, else `element.id` with digits stripped.
///
/// Volatile bits explicitly ignored: `bounds`, `selector`, `xpath`,
/// `dataAttributes.*` with numeric tails, `children` array contents.
pub fn stable_element_fingerprint(element_json: &serde_json::Value) -> String {
    let role = extract_role(element_json).unwrap_or_default();
    let name = extract_name(element_json).unwrap_or_default();
    let tag = extract_tag(element_json).unwrap_or_default();
    let structure = extract_structural_hint(element_json).unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(b"role=");
    hasher.update(role.as_bytes());
    hasher.update(b"|name=");
    hasher.update(name.as_bytes());
    hasher.update(b"|tag=");
    hasher.update(tag.as_bytes());
    hasher.update(b"|struct=");
    hasher.update(structure.as_bytes());

    let digest = hasher.finalize();
    // 16 hex chars = 64 bits of fingerprint entropy.
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn extract_role(el: &serde_json::Value) -> Option<String> {
    el.get("role")
        .and_then(|v| v.as_str())
        .or_else(|| {
            el.get("accessibility")
                .and_then(|a| a.get("role"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            el.get("attributes")
                .and_then(|a| a.get("role"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn extract_name(el: &serde_json::Value) -> Option<String> {
    let raw = el
        .get("accessibleName")
        .and_then(|v| v.as_str())
        .or_else(|| {
            el.get("accessibility")
                .and_then(|a| a.get("accessibleName"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| el.get("label").and_then(|v| v.as_str()))
        .or_else(|| el.get("text").and_then(|v| v.as_str()))
        .or_else(|| el.get("text_content").and_then(|v| v.as_str()))?
        .trim();

    if raw.is_empty() {
        return None;
    }

    let truncated: String = raw.chars().take(NAME_TRUNC).collect();
    Some(truncated)
}

fn extract_tag(el: &serde_json::Value) -> Option<String> {
    el.get("tagName")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
}

/// Produce a structural hint that is stable across re-renders. React
/// frequently auto-generates numeric suffixes in IDs (e.g. `button-:r3:`,
/// `input-42`); stripping digits collapses those back to the semantic stem.
fn extract_structural_hint(el: &serde_json::Value) -> Option<String> {
    let raw = el
        .get("parent")
        .and_then(|v| v.as_str())
        .or_else(|| el.get("id").and_then(|v| v.as_str()))?;

    // Strip ASCII digits and common auto-id markers.
    let mut out = String::with_capacity(raw.len());
    let mut last_was_separator = false;
    for ch in raw.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        // Collapse repeated colons/hyphens that appear once digits are gone.
        if matches!(ch, ':' | '-' | '_') {
            if last_was_separator {
                continue;
            }
            last_was_separator = true;
        } else {
            last_was_separator = false;
        }
        out.push(ch);
    }

    let trimmed = out
        .trim_matches(|c: char| matches!(c, ':' | '-' | '_'))
        .to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Same element passed twice must fingerprint identically.
    #[test]
    fn test_same_element_same_fingerprint() {
        let el = json!({
            "id": "submit-button",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Submit",
            "parent": "form-login",
        });
        let fp1 = stable_element_fingerprint(&el);
        let fp2 = stable_element_fingerprint(&el);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    /// Auto-generated ID suffixes (numeric or React :rX:) must NOT perturb
    /// the fingerprint — the structural hint strips digits.
    #[test]
    fn test_auto_id_suffixes_collapse() {
        let el1 = json!({
            "id": "button-42",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Save",
            "parent": "panel-17",
        });
        let el2 = json!({
            "id": "button-99",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Save",
            "parent": "panel-3",
        });
        assert_eq!(
            stable_element_fingerprint(&el1),
            stable_element_fingerprint(&el2),
            "digits in id/parent should not affect fingerprint"
        );
    }

    /// Different roles with otherwise identical semantic attrs must produce
    /// different fingerprints.
    #[test]
    fn test_different_role_different_fingerprint() {
        let button = json!({
            "id": "el-1",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Submit",
        });
        let link = json!({
            "id": "el-1",
            "tagName": "button",
            "role": "link",
            "accessibleName": "Submit",
        });
        assert_ne!(
            stable_element_fingerprint(&button),
            stable_element_fingerprint(&link)
        );
    }

    /// Accessible name read from nested `accessibility` object when top-level
    /// field is absent — matches the ExternalElement schema's dual shape.
    #[test]
    fn test_name_reads_from_nested_accessibility() {
        let flat = json!({
            "id": "x",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Close",
        });
        let nested = json!({
            "id": "x",
            "tagName": "button",
            "accessibility": {"role": "button", "accessibleName": "Close"},
        });
        assert_eq!(
            stable_element_fingerprint(&flat),
            stable_element_fingerprint(&nested),
            "top-level and nested accessibleName should be equivalent"
        );
    }

    /// Elements with different accessibleName but same role/tag produce
    /// different fingerprints — names are load-bearing.
    #[test]
    fn test_different_name_different_fingerprint() {
        let save = json!({
            "id": "el",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Save",
        });
        let cancel = json!({
            "id": "el",
            "tagName": "button",
            "role": "button",
            "accessibleName": "Cancel",
        });
        assert_ne!(
            stable_element_fingerprint(&save),
            stable_element_fingerprint(&cancel)
        );
    }
}
