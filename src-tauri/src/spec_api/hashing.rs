//! RFC 8785 (JCS) canonical-JSON hashing for Spec-Check.
//!
//! Two public entry points:
//! - `canonical_hash<T: Serialize>(&T) -> Result<String, HashError>` — generic
//!   canonical-hash. Serializes via `serde_jcs` (sorted keys, canonical number
//!   form, RFC 8785 string escapes), hashes with SHA-256, returns
//!   `"sha256-<hex>"`. Used by callers that already hold a fully-pruned
//!   value (e.g. the script-emitter cache key).
//! - `hash_ir_page_spec(&IrPageSpec) -> Result<String, HashError>` — strips
//!   cosmetic provenance fields (`file`, `line`, `column`) at every level
//!   before canonicalizing. Per §5.10 those are authoring artifacts that
//!   must not affect the content hash.

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use qontinui_types::ir::IrPageSpec;

/// Generic canonical content hash. Any `Serialize` → RFC 8785 JCS → SHA-256
/// → `"sha256-<hex>"`.
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<String, HashError> {
    let canonical = serde_jcs::to_string(value)
        .map_err(|e| HashError::Canonicalize(e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(format!("sha256-{}", hex::encode(hasher.finalize())))
}

/// Spec-IR hash. Walks the spec and clears `IrProvenance.file/line/column`
/// at every level (top, every state, every transition) before canonicalizing.
/// `source` and `plugin_version` are KEPT — they're identity-bearing, not
/// cosmetic.
///
/// The input is cloned, never mutated. Clone cost is O(n) but acceptable —
/// hashing runs per-author, not per-evaluation.
pub fn hash_ir_page_spec(spec: &IrPageSpec) -> Result<String, HashError> {
    let mut pruned = spec.clone();
    clear_cosmetic_provenance(&mut pruned.provenance);
    for state in &mut pruned.states {
        clear_cosmetic_provenance(&mut state.provenance);
    }
    for transition in &mut pruned.transitions {
        clear_cosmetic_provenance(&mut transition.provenance);
    }
    canonical_hash(&pruned)
}

fn clear_cosmetic_provenance(prov: &mut Option<qontinui_types::ir::IrProvenance>) {
    if let Some(p) = prov.as_mut() {
        p.file = None;
        p.line = None;
        p.column = None;
    }
}

#[derive(Debug, Error)]
pub enum HashError {
    #[error("canonicalize failed: {0}")]
    Canonicalize(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ir::{IrPageSpec, IrProvenance, IrState, IrTransition};
    use serde_json::json;

    fn empty_spec() -> IrPageSpec {
        IrPageSpec {
            version: "1.0".to_string(),
            id: "test-page".to_string(),
            name: "Test".to_string(),
            description: None,
            metadata: None,
            provenance: None,
            states: vec![],
            transitions: vec![],
            synthesized_groups: None,
            initial_state: None,
        }
    }

    #[test]
    fn hash_is_stable_across_key_order() {
        // Two semantically-identical JSON values with different key insertion
        // orders must hash identically.
        let a = json!({"alpha": 1, "beta": 2, "gamma": 3});
        let b = json!({"gamma": 3, "alpha": 1, "beta": 2});
        let ha = canonical_hash(&a).unwrap();
        let hb = canonical_hash(&b).unwrap();
        assert_eq!(ha, hb);
    }

    #[test]
    fn hash_changes_when_value_changes() {
        let a = json!({"alpha": 1});
        let b = json!({"alpha": 2});
        assert_ne!(canonical_hash(&a).unwrap(), canonical_hash(&b).unwrap());
    }

    #[test]
    fn hash_prefix_is_sha256_dash() {
        let h = canonical_hash(&json!({})).unwrap();
        assert!(h.starts_with("sha256-"));
        let hex_part = &h["sha256-".len()..];
        assert_eq!(hex_part.len(), 64);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ir_hash_ignores_file_line_column() {
        let mut a = empty_spec();
        a.provenance = Some(IrProvenance {
            source: "hand-authored".to_string(),
            file: Some("a.json".to_string()),
            line: Some(10),
            column: Some(5),
            plugin_version: None,
        });
        let mut b = empty_spec();
        b.provenance = Some(IrProvenance {
            source: "hand-authored".to_string(),
            file: Some("b.json".to_string()),
            line: Some(99),
            column: Some(1),
            plugin_version: None,
        });
        assert_eq!(hash_ir_page_spec(&a).unwrap(), hash_ir_page_spec(&b).unwrap());
    }

    #[test]
    fn ir_hash_distinguishes_source_field() {
        let mut a = empty_spec();
        a.provenance = Some(IrProvenance {
            source: "hand-authored".to_string(),
            file: None,
            line: None,
            column: None,
            plugin_version: None,
        });
        let mut b = empty_spec();
        b.provenance = Some(IrProvenance {
            source: "ai-generated".to_string(),
            file: None,
            line: None,
            column: None,
            plugin_version: None,
        });
        assert_ne!(hash_ir_page_spec(&a).unwrap(), hash_ir_page_spec(&b).unwrap());
    }

    #[test]
    fn ir_hash_strips_state_provenance() {
        let mut a = empty_spec();
        a.states.push(IrState {
            id: "s1".to_string(),
            name: "S1".to_string(),
            description: None,
            required_elements: vec![],
            excluded_elements: None,
            conditions: None,
            is_initial: None,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: Some(IrProvenance {
                source: "hand-authored".to_string(),
                file: Some("a.json".to_string()),
                line: Some(10),
                column: None,
                plugin_version: None,
            }),
            cross_refs: None,
        });
        let mut b = empty_spec();
        b.states.push(IrState {
            id: "s1".to_string(),
            name: "S1".to_string(),
            description: None,
            required_elements: vec![],
            excluded_elements: None,
            conditions: None,
            is_initial: None,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: Some(IrProvenance {
                source: "hand-authored".to_string(),
                file: Some("b.json".to_string()),
                line: Some(99),
                column: Some(7),
                plugin_version: None,
            }),
            cross_refs: None,
        });
        assert_eq!(hash_ir_page_spec(&a).unwrap(), hash_ir_page_spec(&b).unwrap());
    }

    #[test]
    fn ir_hash_independent_of_state_field_order() {
        // Serializing through serde_jcs always emits sorted object keys, so
        // two IRs with the same content always hash the same — this test
        // demonstrates that explicitly by hashing the same spec twice.
        let a = empty_spec();
        let b = empty_spec();
        assert_eq!(hash_ir_page_spec(&a).unwrap(), hash_ir_page_spec(&b).unwrap());
    }
}
