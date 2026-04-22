//! Warm Claude API provider for the scripted-output emitter.
//!
//! This is Phase A of the plan `plans/warm-emitter-provider-2026-04-21.md`. The
//! emitter needs an always-fast, low-latency Haiku path regardless of which
//! provider the user configured for chat. Claude CLI (per-call subprocess) is
//! too slow; this module gives the emitter a direct HTTP path with:
//!
//! - A long-lived `reqwest::blocking::Client` so TLS + keepalive amortize
//!   across calls (first call ~150ms handshake, subsequent <10ms pool reuse).
//! - Credential resolution that prefers an explicit API key, then falls
//!   through to the Claude CLI's OAuth `accessToken` read from
//!   `.credentials.json`. Both credentials are sent as `x-api-key` — Anthropic
//!   accepts either through that header.
//! - The `anthropic-beta: prompt-caching-2024-07-31` header so `cache_control`
//!   markers on the system prompt actually fire once Phase B fattens the
//!   prompt past the ~1024-token Haiku minimum.
//!
//! # Spike result (Phase 0 — prompt caching under OAuth)
//!
//! Spike result (2026-04-21): caching works under OAuth via x-api-key header;
//! ≥2s write→read propagation; sub-threshold blocks (<~2048 tokens on Haiku)
//! silently no-op.

#![allow(dead_code)]

use super::cache_aware_builder::{
    parse_cache_tokens, MIN_CACHEABLE_CHARS, PROMPT_CACHING_BETA_HEADER,
};
use super::retry::retry_with_backoff;
use super::types::AiResponse;
use crate::config_facade::ai_keychain;
use crate::doctor::DoctorHandle;
use crate::settings;
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// A credential usable for authenticating to `api.anthropic.com`.
///
/// Both variants are sent as `x-api-key: <token>`. The enum exists so call
/// sites can log/telemetry which path served the request, not because the
/// wire transport differs.
#[derive(Debug, Clone)]
pub(super) enum WarmCredential {
    /// Explicit Anthropic API key from the runner's keychain.
    ApiKey(String),
    /// OAuth access token lifted from the Claude CLI's `.credentials.json`.
    OAuth(String),
}

impl WarmCredential {
    /// The raw bearer string to send in the `x-api-key` header.
    pub(super) fn token(&self) -> &str {
        match self {
            WarmCredential::ApiKey(s) => s,
            WarmCredential::OAuth(s) => s,
        }
    }

    /// A stable label for logs / telemetry.
    pub(super) fn kind_label(&self) -> &'static str {
        match self {
            WarmCredential::ApiKey(_) => "api_key",
            WarmCredential::OAuth(_) => "oauth",
        }
    }
}

/// Resolve the best available warm credential.
///
/// Order:
/// 1. Explicit API key via `ai_keychain().get("claude_api")`. Preferred when
///    present so production runners with a deliberately provisioned key bill
///    that key rather than silently falling through to the user's OAuth quota.
/// 2. OAuth access token from the Claude CLI's `.credentials.json`, skipped
///    if `expiresAt` is already in the past.
/// 3. `None` — the warm provider is unavailable; the `auto` path should fall
///    through to `Disabled`.
pub(super) fn resolve_warm_credential() -> Option<WarmCredential> {
    // 1. Keychain API key wins when present.
    match ai_keychain().get("claude_api") {
        Ok(Some(key)) => {
            if !key.trim().is_empty() {
                debug!("Warm credential: resolved from keychain (api_key)");
                return Some(WarmCredential::ApiKey(key));
            }
        }
        Ok(None) => {}
        Err(e) => {
            warn!(
                "Warm credential: keychain lookup failed ({}); falling through to OAuth",
                e
            );
        }
    }

    // 2. OAuth fallback via the Claude CLI credentials file.
    let creds_path = crate::commands::ai_settings::find_claude_credentials_path()?;
    resolve_oauth_from_path(&creds_path).map(WarmCredential::OAuth)
}

/// Read and validate an OAuth credential from a specific credentials file path.
///
/// Returns `None` if the file cannot be read, the JSON is malformed, the token
/// is missing, or `expiresAt` is in the past. Kept as a standalone helper so
/// unit tests can exercise it without touching the live keychain.
fn resolve_oauth_from_path(creds_path: &Path) -> Option<String> {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(e) => {
            debug!(
                "Warm credential: cannot read OAuth credentials at {}: {}",
                creds_path.display(),
                e
            );
            return None;
        }
    };

    let json: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            debug!("Warm credential: OAuth credentials JSON invalid: {}", e);
            return None;
        }
    };

    let oauth = &json["claudeAiOauth"];
    let token = oauth["accessToken"].as_str()?.to_string();
    if token.is_empty() {
        debug!("Warm credential: OAuth accessToken is empty");
        return None;
    }

    // Reject expired tokens — the CLI refreshes on its own invocations, but if
    // we hand an expired token to Anthropic we get a 401 loop. Fall through
    // so the emitter returns `Disabled` cleanly.
    let expires_at_ms = oauth["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms > 0 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        if now_ms >= expires_at_ms {
            debug!(
                "Warm credential: OAuth token expired (expires_at={}, now={})",
                expires_at_ms, now_ms
            );
            return None;
        }
    }

    debug!("Warm credential: resolved from OAuth credentials file");
    Some(token)
}

/// Long-lived blocking HTTP client, built once and reused across emit calls.
///
/// Keeps connections pooled with a 5-minute idle timeout and a 60-second TCP
/// keepalive so repeat calls in the same session skip TLS handshake costs.
fn warm_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .pool_idle_timeout(Duration::from_secs(300))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|e| {
                warn!(
                    "Warm client: reqwest builder failed ({}); falling back to default",
                    e
                );
                reqwest::blocking::Client::new()
            })
    })
}

/// Stable marker string embedded in the "no credentials" error message.
///
/// Callers (notably the emitter) match on this prefix to distinguish a
/// credential-absent condition from a network/HTTP failure, so they can emit
/// `fallback{reason=disabled}` instead of `emitter_error`. Keep the string
/// stable; intentional wording changes need a matching update at the call
/// site.
pub(crate) const WARM_NO_CREDENTIAL_MARKER: &str = "warm-credentials-unavailable";

/// Determine whether an [`AiResponse`] failure came from the
/// "no credentials could be resolved" path vs a live HTTP/network error.
pub(crate) fn is_no_credential_error(resp: &AiResponse) -> bool {
    if resp.success {
        return false;
    }
    matches!(&resp.error, Some(msg) if msg.contains(WARM_NO_CREDENTIAL_MARKER))
}

/// Sanitize a schema name to Claude's tool-name alphabet (alphanumerics + `_`).
fn sanitize_tool_name(schema_name: &str) -> String {
    schema_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the `system` array for the request body.
///
/// Only attaches `cache_control: ephemeral` when `system_prefix.len() >=
/// MIN_CACHEABLE_CHARS`. Below that threshold, Anthropic silently ignores the
/// marker and sending it anyway would be misleading telemetry. If the prefix
/// is empty we return an empty array — the caller will typically send the
/// message-only body.
fn build_system_array(system_prefix: &str) -> Value {
    if system_prefix.is_empty() {
        return serde_json::json!([]);
    }
    if system_prefix.len() >= MIN_CACHEABLE_CHARS {
        serde_json::json!([{
            "type": "text",
            "text": system_prefix,
            "cache_control": {"type": "ephemeral"}
        }])
    } else {
        serde_json::json!([{
            "type": "text",
            "text": system_prefix,
        }])
    }
}

/// Run a prompt against the warm Claude API path with a split
/// (system_prefix, user_message) shape.
///
/// Thin analog of `run_claude_api` but using the warm client + warm credential
/// resolution. The `system_prefix` is stable across emit calls and gets
/// `cache_control: ephemeral` *only* when it crosses Anthropic's Haiku
/// caching minimum; the `user_message` carries the per-call dynamic payload
/// and is never cached.
pub(crate) fn run_claude_api_warm(
    system_prefix: &str,
    user_message: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    let credential = match resolve_warm_credential() {
        Some(c) => c,
        None => {
            return AiResponse::error(format!(
                "{}: no Claude credential available for warm path (neither keychain API key nor OAuth credentials file).",
                WARM_NO_CREDENTIAL_MARKER
            ));
        }
    };

    info!(
        "Running Claude API (warm) model={} auth={} system_len={} user_len={} cache_marker={}",
        model,
        credential.kind_label(),
        system_prefix.len(),
        user_message.len(),
        system_prefix.len() >= MIN_CACHEABLE_CHARS,
    );

    let mut request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": user_message}],
    });
    if !system_prefix.is_empty() {
        request_body["system"] = build_system_array(system_prefix);
    }

    if let Some(temp) = temperature_override {
        request_body["temperature"] = serde_json::json!(temp);
    }

    let client = warm_client();

    retry_with_backoff("Claude API (warm)", || {
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", credential.token())
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", PROMPT_CACHING_BETA_HEADER)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Claude API (warm) error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<Value>() {
                    Ok(json) => {
                        let content = json["content"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or("")
                            .to_string();

                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();
                        let (cache_creation, cache_read) = parse_cache_tokens(&json);

                        if cache_creation > 0 || cache_read > 0 {
                            debug!(
                                "Claude API (warm) cache metrics — creation: {}, read: {}",
                                cache_creation, cache_read
                            );
                        }

                        match (input_tokens, output_tokens) {
                            (Some(input), Some(output)) => AiResponse::success_with_cache_tokens(
                                content,
                                input,
                                output,
                                cache_creation,
                                cache_read,
                            ),
                            _ => AiResponse::success(content),
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Claude API (warm) response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!("Claude API (warm) request failed: {}", e)),
        }
    })
}

/// Run a prompt against the warm Claude API path with server-side
/// structured-output enforcement via tool_use.
///
/// This is the primary entry point for the emitter. It mirrors
/// `run_claude_api_with_structured_output`'s shape but:
/// - Uses the long-lived warm `reqwest::blocking::Client`.
/// - Authenticates with `WarmCredential` (API key or OAuth).
/// - Takes a split `(system_prefix, user_message)` prompt. The stable
///   `system_prefix` is marked with `cache_control: ephemeral` *only when it
///   crosses [`MIN_CACHEABLE_CHARS`]* — sub-threshold blocks skip the marker
///   so telemetry doesn't lie about cache participation. The `user_message`
///   carries per-call dynamic payload (goal, schemaHint, outputPreview) and
///   is never cached.
pub(crate) fn run_claude_api_warm_with_structured_output(
    system_prefix: &str,
    user_message: &str,
    settings: &settings::ClaudeApiSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    schema: &Value,
    schema_name: &str,
) -> AiResponse {
    let model = model_override.unwrap_or(&settings.model);
    let max_tokens = max_tokens_override.unwrap_or(settings.max_tokens);

    let credential = match resolve_warm_credential() {
        Some(c) => c,
        None => {
            return AiResponse::error(format!(
                "{}: no Claude credential available for warm path (neither keychain API key nor OAuth credentials file).",
                WARM_NO_CREDENTIAL_MARKER
            ));
        }
    };

    info!(
        "Running Claude API (warm, structured) model={} auth={} schema={} system_len={} user_len={} cache_marker={}",
        model,
        credential.kind_label(),
        schema_name,
        system_prefix.len(),
        user_message.len(),
        system_prefix.len() >= MIN_CACHEABLE_CHARS,
    );

    let tool_name = sanitize_tool_name(schema_name);
    let tool_def = serde_json::json!({
        "name": tool_name,
        "description": format!("Generate output conforming to the {} schema", schema_name),
        "input_schema": schema
    });

    let mut request_body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{
            "role": "user",
            "content": user_message,
        }],
        "tools": [tool_def],
        "tool_choice": {"type": "tool", "name": tool_name}
    });
    if !system_prefix.is_empty() {
        request_body["system"] = build_system_array(system_prefix);
    }

    if let Some(temp) = temperature_override {
        request_body["temperature"] = serde_json::json!(temp);
    }

    let client = warm_client();

    retry_with_backoff("Claude API (warm, structured)", || {
        let response = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", credential.token())
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", PROMPT_CACHING_BETA_HEADER)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Claude API (warm, structured) error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<Value>() {
                    Ok(json) => {
                        let content = extract_tool_use_input(&json, &tool_name);

                        let input_tokens = json["usage"]["input_tokens"].as_u64();
                        let output_tokens = json["usage"]["output_tokens"].as_u64();
                        let (cache_creation, cache_read) = parse_cache_tokens(&json);

                        if cache_creation > 0 || cache_read > 0 {
                            debug!(
                                "Claude API (warm, structured) cache metrics — creation: {}, read: {}",
                                cache_creation, cache_read
                            );
                        }

                        match (input_tokens, output_tokens) {
                            (Some(input), Some(output)) => AiResponse::success_with_cache_tokens(
                                content,
                                input,
                                output,
                                cache_creation,
                                cache_read,
                            ),
                            _ => AiResponse::success(content),
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Claude API (warm, structured) response: {}",
                        e
                    )),
                }
            }
            Err(e) => AiResponse::error(format!(
                "Claude API (warm, structured) request failed: {}",
                e
            )),
        }
    })
}

/// Extract the tool input JSON from a Claude tool_use response.
///
/// When `tool_choice` forces a specific tool, the response's first content
/// block is a `tool_use` block whose `input` field is the schema-conformant
/// JSON. We serialize that back to a string so callers can parse it through
/// whatever schema-aware deserialization they already use.
fn extract_tool_use_input(response_json: &Value, tool_name: &str) -> String {
    if let Some(content_array) = response_json["content"].as_array() {
        for block in content_array {
            if block["type"].as_str() == Some("tool_use")
                && block["name"].as_str() == Some(tool_name)
            {
                if let Some(input) = block.get("input") {
                    return serde_json::to_string(input).unwrap_or_default();
                }
            }
        }
        // Tool name mismatch — fall back to the first tool_use block.
        for block in content_array {
            if block["type"].as_str() == Some("tool_use") {
                if let Some(input) = block.get("input") {
                    debug!(
                        "Warm structured output: tool name mismatch (expected '{}', got '{:?}'), using first tool_use block",
                        tool_name,
                        block["name"].as_str()
                    );
                    return serde_json::to_string(input).unwrap_or_default();
                }
            }
        }
        // Final fallback: raw text block (shouldn't happen with tool_choice forced).
        for block in content_array {
            if let Some(text) = block["text"].as_str() {
                debug!(
                    "Warm structured output: no tool_use block found, falling back to text content"
                );
                return text.to_string();
            }
        }
    }
    String::new()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Write a synthetic `.credentials.json` with the given access token and
    /// `expiresAt` millisecond value; return the owning `NamedTempFile` so the
    /// path stays live while the test inspects it.
    fn write_creds(token: &str, expires_at_ms: i64) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": token,
                "expiresAt": expires_at_ms,
                "subscriptionType": "pro"
            }
        });
        write!(f, "{}", body).expect("write");
        f
    }

    #[test]
    fn oauth_reader_accepts_fresh_token() {
        let future_ms = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            + 3_600_000;
        let file = write_creds("sk-oauth-fresh", future_ms);
        let token = resolve_oauth_from_path(file.path()).expect("fresh token accepted");
        assert_eq!(token, "sk-oauth-fresh");
    }

    #[test]
    fn oauth_reader_rejects_expired_token() {
        let past_ms = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            - 60_000;
        let file = write_creds("sk-oauth-expired", past_ms);
        assert!(
            resolve_oauth_from_path(file.path()).is_none(),
            "expired token should be rejected"
        );
    }

    #[test]
    fn oauth_reader_rejects_missing_file() {
        let missing = std::env::temp_dir().join("claude_api_warm_nonexistent.json");
        // Ensure it really doesn't exist.
        let _ = std::fs::remove_file(&missing);
        assert!(resolve_oauth_from_path(&missing).is_none());
    }

    #[test]
    fn oauth_reader_rejects_empty_token() {
        let future_ms = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            + 3_600_000;
        let file = write_creds("", future_ms);
        assert!(resolve_oauth_from_path(file.path()).is_none());
    }

    #[test]
    fn oauth_reader_rejects_malformed_json() {
        let mut f = NamedTempFile::new().expect("tempfile");
        write!(f, "not json at all").expect("write");
        assert!(resolve_oauth_from_path(f.path()).is_none());
    }

    #[test]
    fn oauth_reader_rejects_missing_oauth_block() {
        let mut f = NamedTempFile::new().expect("tempfile");
        write!(f, "{{}}").expect("write");
        assert!(resolve_oauth_from_path(f.path()).is_none());
    }

    #[test]
    fn warm_credential_labels_are_stable() {
        let api = WarmCredential::ApiKey("k".to_string());
        let oauth = WarmCredential::OAuth("t".to_string());
        assert_eq!(api.kind_label(), "api_key");
        assert_eq!(oauth.kind_label(), "oauth");
        assert_eq!(api.token(), "k");
        assert_eq!(oauth.token(), "t");
    }

    #[test]
    fn tool_name_sanitization_is_strict() {
        assert_eq!(sanitize_tool_name("FooBar_1"), "FooBar_1");
        assert_eq!(sanitize_tool_name("foo.bar-baz"), "foo_bar_baz");
        assert_eq!(sanitize_tool_name("has spaces"), "has_spaces");
    }

    #[test]
    fn extract_tool_use_input_finds_named_tool() {
        let body = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "my_tool", "input": {"k": "v"}}
            ]
        });
        let got = extract_tool_use_input(&body, "my_tool");
        let parsed: Value = serde_json::from_str(&got).unwrap();
        assert_eq!(parsed["k"], "v");
    }

    #[test]
    fn extract_tool_use_input_falls_back_to_first_tool() {
        let body = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "other_tool", "input": {"a": 1}}
            ]
        });
        let got = extract_tool_use_input(&body, "expected_tool");
        let parsed: Value = serde_json::from_str(&got).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn extract_tool_use_input_returns_empty_for_no_content() {
        let body = serde_json::json!({"content": []});
        assert_eq!(extract_tool_use_input(&body, "tool"), "");
    }

    // NOTE: the full `resolve_warm_credential()` function is not unit-tested
    // directly because it reads from the process-level OS keychain
    // (`ai_keychain().get(...)`) and the live Claude config directory — both
    // are global, shared-across-tests state. Injecting a fake keychain would
    // require plumbing a trait boundary that doesn't exist yet, and we'd be
    // testing the mock more than the code. The two moving parts worth testing
    // in isolation are (a) the OAuth file parser (covered above) and (b) the
    // keychain-first precedence, which is a single `match` on an `Ok(Some(_))`
    // return value — covered by inspection rather than a live-keychain test.
}
