//! Tolerant criteria projection from an [`IrAssertion`]'s loosely-typed
//! `target.criteria` (`serde_json::Value`).
//!
//! ## Why a generator-local parser (cross-crate finding)
//!
//! The matcher (`qontinui-spec-check`) parses `target.criteria` directly into the
//! strict, `camelCase`-only [`qontinui_types::ir::IrElementCriteria`]
//! (`evaluator.rs::parse_criteria`). `IrElementCriteria` is
//! `#[serde(rename_all = "camelCase")]` **without** `deny_unknown_fields`, so a
//! `snake_case` criteria key like `text_contains` is silently dropped — it deserializes
//! to `text_contains: None`, making the assertion vacuous.
//!
//! The connect-runner oracle fixture writes its needle criteria as
//! `{"text_contains": "authorize"}` (snake_case). The **generator** must invert what
//! comprehension actually authored, so it reads criteria tolerantly: it accepts both
//! the camelCase wire form and the snake_case form for every field. This keeps the
//! emitted screen faithful to the authored intent regardless of which casing
//! comprehension emitted.
//!
//! This is surfaced as a cross-crate reconciliation item: comprehension (#3) should
//! emit `textContains` (camelCase) so the matcher reads it too — until then the matcher
//! ignores snake_case needles even though the generator honors them.

/// The subset of element criteria the generator inverts into instrumentation.
/// Every field is optional (criteria may be partial). Parsed tolerantly from the
/// loose `serde_json::Value` so both `text_contains` and `textContains` are honored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCriteria {
    pub role: Option<String>,
    pub tag_name: Option<String>,
    pub text: Option<String>,
    pub text_contains: Option<String>,
    pub aria_label: Option<String>,
    pub accessible_name: Option<String>,
    pub id: Option<String>,
}

impl ParsedCriteria {
    /// Project a loosely-typed criteria `Value` into the generator's view, accepting
    /// both camelCase and snake_case keys. A non-object (or `null`) yields the empty
    /// criteria (which inverts to a bare, role-less labelled element).
    pub fn from_value(value: &serde_json::Value) -> Self {
        let obj = match value.as_object() {
            Some(o) => o,
            None => return ParsedCriteria::default(),
        };
        // Read the first present key among the accepted aliases, as a trimmed,
        // non-empty string.
        let read = |aliases: &[&str]| -> Option<String> {
            for k in aliases {
                if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
                    let t = s.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
            None
        };
        ParsedCriteria {
            role: read(&["role"]),
            tag_name: read(&["tagName", "tag_name"]),
            text: read(&["text"]),
            text_contains: read(&["textContains", "text_contains"]),
            aria_label: read(&["ariaLabel", "aria_label"]),
            accessible_name: read(&["accessibleName", "accessible_name"]),
            id: read(&["id"]),
        }
    }

    /// True when the criteria declares no element signal at all — the inversion would
    /// be vacuous (any element matches), which the generator treats as a degenerate
    /// assertion worth surfacing rather than silently instrumenting nothing.
    pub fn is_empty(&self) -> bool {
        self.role.is_none()
            && self.tag_name.is_none()
            && self.text.is_none()
            && self.text_contains.is_none()
            && self.aria_label.is_none()
            && self.accessible_name.is_none()
            && self.id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_camelcase_text_contains() {
        let c = ParsedCriteria::from_value(&json!({"textContains": "authorize"}));
        assert_eq!(c.text_contains.as_deref(), Some("authorize"));
    }

    #[test]
    fn reads_snakecase_text_contains_the_fixture_shape() {
        // The connect-runner fixture writes snake_case — the matcher drops it, the
        // generator must honor it. This is the cross-crate finding pinned as a test.
        let c = ParsedCriteria::from_value(&json!({"text_contains": "authorize"}));
        assert_eq!(c.text_contains.as_deref(), Some("authorize"));
    }

    #[test]
    fn reads_role_and_text_combo() {
        let c = ParsedCriteria::from_value(&json!({"role": "button", "text": "Connect"}));
        assert_eq!(c.role.as_deref(), Some("button"));
        assert_eq!(c.text.as_deref(), Some("Connect"));
        assert!(!c.is_empty());
    }

    #[test]
    fn null_or_non_object_is_empty() {
        assert!(ParsedCriteria::from_value(&json!(null)).is_empty());
        assert!(ParsedCriteria::from_value(&json!("nope")).is_empty());
        assert!(ParsedCriteria::from_value(&json!({})).is_empty());
    }

    #[test]
    fn blank_strings_are_treated_as_absent() {
        let c = ParsedCriteria::from_value(&json!({"role": "   ", "text": ""}));
        assert!(c.is_empty());
    }
}
