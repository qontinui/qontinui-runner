//! Key-order-independent JSON canonicalisation.
//!
//! `serde_json::Map` is a `BTreeMap` (sorted keys) only while the
//! `preserve_order` feature is off. Cargo unifies features across the whole
//! build, and `agent-client-protocol` enables `preserve_order`
//! non-optionally, so in this crate a `Map` is an `IndexMap` that iterates and
//! serializes in insertion (or input-text) order. Code that needs a
//! deterministic key order — a content hash, a header row, a byte comparison —
//! must not rely on `Map` iteration order; it sorts through this module.

use serde_json::{Map, Value};

/// Recursively sort every object's keys lexicographically. Arrays keep their
/// element order. The result serializes identically whether or not
/// `preserve_order` is enabled.
pub fn sort_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = Map::new();
            for (k, v) in entries {
                out.insert(k, sort_keys(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

/// Serialize `value` with every object's keys sorted, recursively.
pub fn to_canonical_string(value: &Value) -> serde_json::Result<String> {
    serde_json::to_string(&sort_keys(value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_keys_serialize_sorted_regardless_of_insertion_order() {
        let v: Value =
            serde_json::from_str(r#"{"b":1,"a":{"z":true,"m":[{"y":1,"x":2}]}}"#).unwrap();
        assert_eq!(
            to_canonical_string(&v).unwrap(),
            r#"{"a":{"m":[{"x":2,"y":1}],"z":true},"b":1}"#
        );
    }

    #[test]
    fn arrays_keep_element_order() {
        let v: Value = serde_json::from_str(r#"[3,1,2]"#).unwrap();
        assert_eq!(to_canonical_string(&v).unwrap(), "[3,1,2]");
    }
}
