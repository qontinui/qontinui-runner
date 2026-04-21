//! UI Bridge invoke-proxy allowlist wire-contract probe.
//!
//! On runner startup we dry-run each allowlisted command with an empty
//! `{}` payload and classify the reply. This catches schema drift between
//! a command's declared `args_schema` string and the actual Tauri
//! parameter shape — exactly the bug fixed in commit `01ba1085b`, where
//! two commands took a single `args: StructArgs` Rust parameter, forcing
//! HTTP callers to wrap everything as `{"args":{"args":{...}}}` despite
//! the schema documenting flat args.
//!
//! The probe is purely diagnostic: it logs but never fails boot. The
//! intent is to emit a single ERROR-level log when schema drift is
//! detected so it shows up in CI / ops dashboards before a real HTTP
//! caller trips over it.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use crate::ui_bridge_invoke::{InvokeRequestStore, InvokeResponse, UI_BRIDGE_COMMANDS};

/// Outcome of a single command probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeStatus {
    /// `ok: true` — the command accepted `{}` (unusual but valid — e.g. no required args).
    Ok,
    /// Tauri reported a missing required key, and that key *is* declared in `args_schema`.
    /// This is the expected and healthy outcome for commands that have required args.
    SchemaMatch { missing_key: String },
    /// Tauri reported a missing required key that is **not** declared in `args_schema`.
    /// This is the bug we're catching — schema drift between the Rust signature and the
    /// hand-written string in the allowlist entry.
    SchemaMismatch {
        missing_key: String,
        declared_keys: Vec<String>,
    },
    /// Error shape we couldn't classify (non-missing-key error, or the command rejected
    /// the probe for an unrelated reason). Skipped without failing boot.
    Unparseable { detail: String },
    /// The frontend never responded within the probe timeout.
    Timeout,
}

/// Per-command probe result suitable for logging.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub command: String,
    pub status: ProbeStatus,
    pub detail: Option<String>,
}

/// Probe timeout — kept short because we run 7-ish of these back-to-back on boot and
/// the frontend has already been mounted by the time `tokio::spawn` gets to us.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Iterate `UI_BRIDGE_COMMANDS`, probing each one with an empty args payload,
/// and return a classified result per command.
///
/// Follows the same pattern as `ui_bridge_invoke_handler` — register a oneshot,
/// emit `ui-bridge:invoke-request`, await with timeout. Never propagates errors
/// up to the caller; schema drift is reported via the returned [`ProbeResult`]
/// vector and logged by [`log_probe_results`].
pub async fn probe_allowlist_wire_contracts(
    app_handle: &tauri::AppHandle,
    store: Arc<InvokeRequestStore>,
) -> Vec<ProbeResult> {
    use tauri::Emitter;

    let mut results = Vec::with_capacity(UI_BRIDGE_COMMANDS.len());

    for cmd in UI_BRIDGE_COMMANDS {
        // Destructive commands opt out — see `ProxyableCommand::probe_with_empty_args`.
        // Skipping these sacrifices schema-drift detection for this specific
        // command (there's nothing to detect anyway — no required keys), but
        // avoids side-effects on startup.
        if !cmd.probe_with_empty_args {
            debug!(
                command = cmd.name,
                "probe skipped — command opted out via probe_with_empty_args=false"
            );
            continue;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel::<InvokeResponse>();
        store.register(request_id.clone(), sender).await;

        let payload = serde_json::json!({
            "request_id": request_id,
            "command": cmd.name,
            "args": {},
        });

        // Emit; if emit itself fails we can't probe this one and move on.
        if let Err(e) = app_handle.emit("ui-bridge:invoke-request", &payload) {
            store.cancel(&request_id).await;
            results.push(ProbeResult {
                command: cmd.name.to_string(),
                status: ProbeStatus::Unparseable {
                    detail: format!("emit failed: {}", e),
                },
                detail: Some(format!("emit failed: {}", e)),
            });
            continue;
        }

        let waited = tokio::time::timeout(PROBE_TIMEOUT, receiver).await;
        match waited {
            Ok(Ok(resp)) => {
                let status = classify_probe_reply(cmd.args_schema, &resp);
                let detail = resp.error.clone();
                results.push(ProbeResult {
                    command: cmd.name.to_string(),
                    status,
                    detail,
                });
            }
            Ok(Err(_recv_err)) => {
                store.cancel(&request_id).await;
                results.push(ProbeResult {
                    command: cmd.name.to_string(),
                    status: ProbeStatus::Unparseable {
                        detail: "oneshot sender dropped without a response".to_string(),
                    },
                    detail: None,
                });
            }
            Err(_elapsed) => {
                store.cancel(&request_id).await;
                results.push(ProbeResult {
                    command: cmd.name.to_string(),
                    status: ProbeStatus::Timeout,
                    detail: None,
                });
            }
        }
    }

    results
}

/// Classify a probe reply without performing any I/O — pure function, unit-testable.
///
/// Decision tree:
/// 1. `ok: true` → [`ProbeStatus::Ok`].
/// 2. `ok: false` with error matching `missing required key <X>` → look up `<X>` in the
///    top-level keys of `args_schema`. Present → [`ProbeStatus::SchemaMatch`]. Absent →
///    [`ProbeStatus::SchemaMismatch`] with the declared keys for ERROR logging.
/// 3. Any other error → [`ProbeStatus::Unparseable`] with the raw error string.
pub fn classify_probe_reply(schema_str: &str, reply: &InvokeResponse) -> ProbeStatus {
    if reply.ok {
        return ProbeStatus::Ok;
    }

    let Some(error) = reply.error.as_deref() else {
        return ProbeStatus::Unparseable {
            detail: "ok=false with no error message".to_string(),
        };
    };

    match extract_missing_key(error) {
        Some(missing_key) => {
            let declared = extract_top_level_keys(schema_str);
            if declared.iter().any(|k| k == &missing_key) {
                ProbeStatus::SchemaMatch { missing_key }
            } else {
                ProbeStatus::SchemaMismatch {
                    missing_key,
                    declared_keys: declared,
                }
            }
        }
        None => ProbeStatus::Unparseable {
            detail: error.to_string(),
        },
    }
}

/// Extract the key name Tauri reports as missing, e.g.
/// `"invalid args 'args' for command 'X': missing required key args"` → `Some("args")`.
fn extract_missing_key(error: &str) -> Option<String> {
    // Tauri 2.x uses "missing required key <name>" inside the IPC error string. The
    // surrounding phrasing has varied across versions (serde_json vs. Tauri's own
    // deserialiser), so we just anchor on the stable `missing required key` fragment.
    let re = Regex::new(r"missing required key\s+([A-Za-z_][A-Za-z0-9_]*)").ok()?;
    re.captures(error)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

/// Extract top-level object keys from a lenient schema string.
///
/// `args_schema` isn't strict JSON — it's a human-readable signature like
/// `{ "name": string, "metadata"?: object }`. We parse it heuristically by
/// scanning quoted identifiers at the top-level of the outermost `{}`. A
/// trailing `?` on the key (e.g. `"metadata"?`) is treated as the optional
/// marker and stripped from the returned name.
pub fn extract_top_level_keys(schema_str: &str) -> Vec<String> {
    let trimmed = schema_str.trim();

    // Fast path: empty schema / `{}` — no keys declared.
    if trimmed.is_empty() || trimmed == "{}" {
        return Vec::new();
    }

    // Try strict JSON first — some schemas happen to be valid JSON objects.
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
        return map.into_iter().map(|(k, _)| k).collect();
    }

    // Lenient scan: collect quoted identifiers that appear at depth 1 inside the
    // outermost `{}` and are immediately followed by `:` (optionally with `?` in
    // between for optional keys).
    let mut keys = Vec::new();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut string_start = 0usize;

    while i < bytes.len() {
        let c = bytes[i];

        if in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                // End of string literal at bytes[string_start..i].
                let content = &trimmed[string_start + 1..i];
                in_string = false;

                // Only consider strings at depth 1 (direct children of the outer object).
                if depth == 1 {
                    // Peek past whitespace and an optional `?` to see if a `:` follows.
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b'?' {
                        j += 1;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                    if j < bytes.len() && bytes[j] == b':' {
                        keys.push(content.to_string());
                    }
                }
            }
        } else {
            match c {
                b'"' => {
                    in_string = true;
                    string_start = i;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }

        i += 1;
    }

    keys
}

/// Log a vec of probe results at severity-appropriate levels.
///
/// - [`ProbeStatus::SchemaMismatch`] → ERROR (the bug we're catching).
/// - [`ProbeStatus::SchemaMatch`] / [`ProbeStatus::Ok`] → INFO (audit trail).
/// - [`ProbeStatus::Unparseable`] / [`ProbeStatus::Timeout`] → DEBUG (not actionable).
pub fn log_probe_results(results: &[ProbeResult]) {
    let mut mismatches = 0usize;
    let mut ok = 0usize;
    let mut skipped = 0usize;

    for r in results {
        match &r.status {
            ProbeStatus::Ok => {
                info!(
                    command = %r.command,
                    "ui_bridge_invoke_probe: ok (command accepted empty args)"
                );
                ok += 1;
            }
            ProbeStatus::SchemaMatch { missing_key } => {
                info!(
                    command = %r.command,
                    missing_key = %missing_key,
                    "ui_bridge_invoke_probe: ok (schema documents the required key Tauri asked for)"
                );
                ok += 1;
            }
            ProbeStatus::SchemaMismatch {
                missing_key,
                declared_keys,
            } => {
                error!(
                    command = %r.command,
                    tauri_missing_key = %missing_key,
                    args_schema_keys = ?declared_keys,
                    "UI Bridge allowlist schema drift: command '{}' expected key '{}' based on Tauri signature, but args_schema documents keys {:?}. Fix either the Rust command's parameter names or the args_schema string.",
                    r.command, missing_key, declared_keys
                );
                mismatches += 1;
            }
            ProbeStatus::Unparseable { detail } => {
                debug!(
                    command = %r.command,
                    detail = %detail,
                    "ui_bridge_invoke_probe: unparseable probe result — skipped"
                );
                skipped += 1;
            }
            ProbeStatus::Timeout => {
                warn!(
                    command = %r.command,
                    "ui_bridge_invoke_probe: frontend did not reply within probe timeout — skipped"
                );
                skipped += 1;
            }
        }
    }

    if mismatches > 0 {
        error!(
            total = results.len(),
            ok,
            mismatches,
            skipped,
            "ui_bridge_invoke_probe: finished with {} schema-drift mismatch(es) — see ERROR logs above",
            mismatches
        );
    } else {
        debug!(
            total = results.len(),
            ok, skipped, "ui_bridge_invoke_probe: finished with no schema drift"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply_ok() -> InvokeResponse {
        InvokeResponse {
            ok: true,
            result: Some(Value::Null),
            error: None,
        }
    }

    fn reply_err(msg: &str) -> InvokeResponse {
        InvokeResponse {
            ok: false,
            result: None,
            error: Some(msg.to_string()),
        }
    }

    #[test]
    fn ok_reply_classifies_as_ok() {
        let status = classify_probe_reply("{}", &reply_ok());
        assert_eq!(status, ProbeStatus::Ok);
    }

    #[test]
    fn missing_key_present_in_schema_is_schema_match() {
        let schema = "{ \"name\": string, \"metadata\"?: object }";
        let reply = reply_err("invalid args 'name' for command 'X': missing required key name");
        let status = classify_probe_reply(schema, &reply);
        assert_eq!(
            status,
            ProbeStatus::SchemaMatch {
                missing_key: "name".to_string()
            }
        );
    }

    #[test]
    fn missing_key_absent_from_schema_is_schema_mismatch() {
        // This is the regression: Rust signature takes `args: StructArgs` so Tauri
        // demands a top-level `args` key, but the schema documents `name` instead.
        let schema = "{ \"name\": string }";
        let reply = reply_err("invalid args 'args' for command 'X': missing required key args");
        let status = classify_probe_reply(schema, &reply);
        assert_eq!(
            status,
            ProbeStatus::SchemaMismatch {
                missing_key: "args".to_string(),
                declared_keys: vec!["name".to_string()],
            }
        );
    }

    #[test]
    fn unrecognised_error_is_unparseable() {
        let schema = "{ \"name\": string }";
        let reply = reply_err("some other weird error");
        let status = classify_probe_reply(schema, &reply);
        match status {
            ProbeStatus::Unparseable { detail } => {
                assert_eq!(detail, "some other weird error");
            }
            other => panic!("expected Unparseable, got {:?}", other),
        }
    }

    #[test]
    fn ok_false_without_error_is_unparseable() {
        let reply = InvokeResponse {
            ok: false,
            result: None,
            error: None,
        };
        let status = classify_probe_reply("{}", &reply);
        match status {
            ProbeStatus::Unparseable { .. } => {}
            other => panic!("expected Unparseable, got {:?}", other),
        }
    }

    #[test]
    fn extract_top_level_keys_handles_optional_marker() {
        let schema = "{ \"name\": string, \"metadata\"?: object, \"taskRunId\"?: string | null }";
        let keys = extract_top_level_keys(schema);
        assert_eq!(keys, vec!["name", "metadata", "taskRunId"]);
    }

    #[test]
    fn extract_top_level_keys_empty_schema() {
        assert!(extract_top_level_keys("{}").is_empty());
        assert!(extract_top_level_keys("").is_empty());
    }

    #[test]
    fn extract_top_level_keys_ignores_nested_object_keys() {
        // `nested.foo` must not leak out as a top-level key.
        let schema = "{ \"outer\": { \"foo\": string, \"bar\"?: number } }";
        let keys = extract_top_level_keys(schema);
        assert_eq!(keys, vec!["outer"]);
    }

    #[test]
    fn extract_top_level_keys_strict_json_fallback() {
        // Pure strict JSON still works via the serde_json fast path.
        let schema = "{\"a\":1,\"b\":2}";
        let mut keys = extract_top_level_keys(schema);
        keys.sort();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn missing_key_regex_tolerates_extra_surrounding_text() {
        // Different Tauri versions phrase this slightly differently; the regex
        // only needs the `missing required key <name>` fragment to fire.
        assert_eq!(
            extract_missing_key("Error: missing required key foo at path .args"),
            Some("foo".to_string())
        );
        assert_eq!(extract_missing_key("completely unrelated"), None);
    }

    #[test]
    fn real_allowlist_schemas_parse_cleanly() {
        // Sanity check: every command in the real allowlist must either declare
        // zero keys (for `{}`) or yield at least one key from our lenient parser.
        // If this ever regresses we'd lose the ability to diagnose drift.
        for cmd in UI_BRIDGE_COMMANDS {
            let keys = extract_top_level_keys(cmd.args_schema);
            if cmd.args_schema.trim() != "{}" {
                assert!(
                    !keys.is_empty(),
                    "command '{}' has schema {:?} but no keys extracted",
                    cmd.name,
                    cmd.args_schema
                );
            }
        }
    }
}
