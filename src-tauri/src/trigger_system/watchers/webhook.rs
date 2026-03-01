//! Webhook trigger utilities: HMAC verification and payload extraction.

use sha2::Sha256;
use std::collections::HashMap;
use tracing::debug;

/// Verify HMAC-SHA256 signature for webhook payloads.
///
/// Compares the `X-Hub-Signature-256` header against the computed HMAC
/// of the request body using the configured secret.
pub fn verify_hmac_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    use sha2::Digest;

    // GitHub sends: sha256=<hex>
    let expected_hex = signature_header
        .strip_prefix("sha256=")
        .unwrap_or(signature_header);

    // Compute HMAC-SHA256 manually using sha2 (we don't have hmac crate)
    // Use the standard HMAC construction: H((K ^ opad) || H((K ^ ipad) || message))
    let key = secret.as_bytes();
    let block_size = 64; // SHA-256 block size

    // If key is longer than block size, hash it first
    let key_padded = if key.len() > block_size {
        let mut hasher = Sha256::new();
        hasher.update(key);
        let hash = hasher.finalize();
        let mut padded = vec![0u8; block_size];
        padded[..hash.len()].copy_from_slice(&hash);
        padded
    } else {
        let mut padded = vec![0u8; block_size];
        padded[..key.len()].copy_from_slice(key);
        padded
    };

    // Inner hash: H((K ^ ipad) || message)
    let ipad: Vec<u8> = key_padded.iter().map(|b| b ^ 0x36).collect();
    let mut inner_hasher = Sha256::new();
    inner_hasher.update(&ipad);
    inner_hasher.update(body);
    let inner_hash = inner_hasher.finalize();

    // Outer hash: H((K ^ opad) || inner_hash)
    let opad: Vec<u8> = key_padded.iter().map(|b| b ^ 0x5c).collect();
    let mut outer_hasher = Sha256::new();
    outer_hasher.update(&opad);
    outer_hasher.update(inner_hash);
    let result = outer_hasher.finalize();

    let computed_hex = hex::encode(result);

    // Constant-time comparison
    if computed_hex.len() != expected_hex.len() {
        return false;
    }

    let mut diff = 0u8;
    for (a, b) in computed_hex.bytes().zip(expected_hex.bytes()) {
        diff |= a ^ b;
    }

    diff == 0
}

/// Extract variables from a JSON payload using simple JSONPath-style expressions.
///
/// Supports:
/// - `$.field` - top-level field
/// - `$.field.nested` - nested field access
/// - `$.array[0]` - array index access
/// - `$.array[0].field` - combined
pub fn extract_variables(
    payload: &serde_json::Value,
    mapping: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();

    for (var_name, jsonpath) in mapping {
        if let Some(value) = resolve_jsonpath(payload, jsonpath) {
            let str_value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "null".to_string(),
                other => other.to_string(),
            };
            debug!("Extracted variable '{}' = '{}'", var_name, str_value);
            result.insert(var_name.clone(), str_value);
        } else {
            debug!("JSONPath '{}' did not match in payload", jsonpath);
        }
    }

    result
}

/// Resolve a simple JSONPath expression against a JSON value.
fn resolve_jsonpath<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    // Strip leading $. if present
    let path = path
        .strip_prefix("$.")
        .unwrap_or(path.strip_prefix("$").unwrap_or(path));

    if path.is_empty() {
        return Some(value);
    }

    let mut current = value;

    for segment in split_path_segments(path) {
        // Check for array index: field[0]
        if let Some((field, index)) = parse_array_access(&segment) {
            if !field.is_empty() {
                current = current.get(&field)?;
            }
            current = current.get(index)?;
        } else {
            current = current.get(&segment)?;
        }
    }

    Some(current)
}

/// Split a dotted path into segments, respecting array brackets.
fn split_path_segments(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_bracket = false;

    for ch in path.chars() {
        match ch {
            '[' => {
                in_bracket = true;
                current.push(ch);
            }
            ']' => {
                in_bracket = false;
                current.push(ch);
            }
            '.' if !in_bracket => {
                if !current.is_empty() {
                    segments.push(current.clone());
                    current.clear();
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}

/// Parse array access notation: "field[0]" -> ("field", 0)
fn parse_array_access(segment: &str) -> Option<(String, usize)> {
    if let Some(bracket_start) = segment.find('[') {
        if let Some(bracket_end) = segment.find(']') {
            let field = segment[..bracket_start].to_string();
            let index_str = &segment[bracket_start + 1..bracket_end];
            if let Ok(index) = index_str.parse::<usize>() {
                return Some((field, index));
            }
        }
    }
    None
}

/// Check if a payload matches a JSONPath filter expression.
///
/// Simple filter: if the expression resolves to a truthy value, it matches.
pub fn matches_payload_filter(payload: &serde_json::Value, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }

    match resolve_jsonpath(payload, filter) {
        Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Number(_)) => true,
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(_)) => true,
        None => false,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_hmac_verification() {
        let secret = "mysecret";
        let body = b"hello world";

        // Compute expected HMAC manually
        let sig = compute_test_hmac(secret, body);
        let header = format!("sha256={}", sig);

        assert!(verify_hmac_signature(secret, body, &header));
        assert!(!verify_hmac_signature("wrong", body, &header));
        assert!(!verify_hmac_signature(secret, b"tampered", &header));
    }

    fn compute_test_hmac(secret: &str, body: &[u8]) -> String {
        use sha2::Digest;
        let key = secret.as_bytes();
        let block_size = 64;
        let mut key_padded = vec![0u8; block_size];
        key_padded[..key.len()].copy_from_slice(key);

        let ipad: Vec<u8> = key_padded.iter().map(|b| b ^ 0x36).collect();
        let mut inner = Sha256::new();
        inner.update(&ipad);
        inner.update(body);
        let inner_hash = inner.finalize();

        let opad: Vec<u8> = key_padded.iter().map(|b| b ^ 0x5c).collect();
        let mut outer = Sha256::new();
        outer.update(&opad);
        outer.update(&inner_hash);
        hex::encode(outer.finalize())
    }

    #[test]
    fn test_extract_variables_simple() {
        let payload = json!({
            "ref": "refs/heads/main",
            "head_commit": {
                "message": "fix: typo"
            }
        });

        let mut mapping = HashMap::new();
        mapping.insert("branch".to_string(), "$.ref".to_string());
        mapping.insert("message".to_string(), "$.head_commit.message".to_string());

        let vars = extract_variables(&payload, &mapping);
        assert_eq!(vars.get("branch").unwrap(), "refs/heads/main");
        assert_eq!(vars.get("message").unwrap(), "fix: typo");
    }

    #[test]
    fn test_extract_variables_array() {
        let payload = json!({
            "commits": [
                {"message": "first"},
                {"message": "second"}
            ]
        });

        let mut mapping = HashMap::new();
        mapping.insert("first_msg".to_string(), "$.commits[0].message".to_string());

        let vars = extract_variables(&payload, &mapping);
        assert_eq!(vars.get("first_msg").unwrap(), "first");
    }

    #[test]
    fn test_payload_filter() {
        let payload = json!({"action": "completed", "check_suite": {"conclusion": "failure"}});

        assert!(matches_payload_filter(&payload, "$.action"));
        assert!(matches_payload_filter(&payload, "$.check_suite.conclusion"));
        assert!(!matches_payload_filter(&payload, "$.nonexistent"));
        assert!(matches_payload_filter(&payload, "")); // empty filter always matches
    }
}
