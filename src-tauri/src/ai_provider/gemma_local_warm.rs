//! Warm local-Gemma provider for the scripted-output emitter.
//!
//! Third lane in the emitter's provider pinning (after `claude_api_warm` and
//! `claude_api`). Calls a locally-hosted `llama.cpp` server (typically
//! `ghcr.io/ggml-org/llama.cpp:server-cuda` running Gemma 4 26B-A4B-it) over
//! HTTP. Designed to fill the gap on dev machines that either can't or won't
//! spend Anthropic quota on emit calls:
//!
//! - Zero dollars, zero quota, no Anthropic auth — local model.
//! - KV-cache reuse is automatic in llama.cpp (prompt-cache is a server-side
//!   feature, no beta header needed). The fattened system prefix already in
//!   `build_prompt_split` amortizes across emit calls the same way.
//! - On RTX 5090 with the 26B MoE (4B active params, Q6_K quant), the spike
//!   on 2026-04-22 measured 1.1–1.4 s wall-time per call — faster than the
//!   Claude Haiku 4.5 warm path.
//!
//! # Why the raw `/completion` endpoint, not `/v1/chat/completions`
//!
//! Gemma 4 emits a chain-of-thought in a `<|channel>thought ... <channel|>`
//! wrapper before the final answer. llama.cpp's built-in Jinja chat template
//! for Gemma 4 strips those wrapper tokens too aggressively and returns
//! empty content. Bypass it: build the Gemma-native prompt manually
//! (`<user|>...<turn|>\n<model|>\n`), call `/completion`, and post-filter
//! anything before the last `<channel|>` marker.
//!
//! # No credential resolution
//!
//! Unlike `claude_api_warm`, this lane has no credentials to resolve. The
//! only runtime gate is whether the configured HTTP endpoint responds; a
//! connection refusal surfaces as the emitter's `no-server` variant so the
//! `Auto` cascade can fall through cleanly.

#![allow(dead_code)]

use super::retry::retry_with_backoff;
use super::types::AiResponse;
use crate::doctor::DoctorHandle;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Stable marker string embedded in the "no local server" error message.
///
/// The emitter's `Auto` cascade matches on this prefix to distinguish a
/// server-unreachable condition from a model or parse failure, so it can
/// fall through to `Disabled` (or to a prior lane) instead of surfacing a
/// noisy `LlmError`. Keep the string stable; wording changes need a
/// matching update at the call site.
pub(crate) const LOCAL_NO_SERVER_MARKER: &str = "gemma-local-server-unreachable";

/// Default endpoint of the local llama.cpp server. `llama-swap` listens on
/// 8100 but doesn't proxy the raw `/completion` path, so we target a
/// dedicated standalone llama-server container on 8200 (see
/// `qontinui/docker/gemma-server/docker-compose.yml`). Configurable via
/// `ScriptedOutputSettings::gemma_local_endpoint`.
pub const DEFAULT_LOCAL_ENDPOINT: &str = "http://localhost:8200";

/// Default alias the llama.cpp server registers for the loaded GGUF. Used in
/// log lines only; llama.cpp's `/completion` endpoint doesn't require a
/// model id in the request body.
pub const DEFAULT_MODEL_ALIAS: &str = "gemma-4-26b-a4b";

/// Per-call request timeout. Tighter than `claude_api_warm`'s retry-with-
/// backoff ceiling because the hot-path emitter LLM timeout caps us at 5 s
/// upstream anyway — a stuck request in the client is wasted wall time.
const LOCAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Long-lived blocking HTTP client, built once and reused across emit calls.
///
/// No TLS (localhost HTTP), so the main win vs a fresh `Client` is connection
/// pooling for repeat calls within a session. TCP keepalive lets llama-swap
/// or a bare llama-server reuse the idle socket instead of paying a fresh
/// three-way handshake per call.
fn local_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .pool_idle_timeout(Duration::from_secs(300))
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(LOCAL_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                warn!(
                    "Gemma local client: reqwest builder failed ({}); falling back to default",
                    e
                );
                reqwest::blocking::Client::new()
            })
    })
}

/// Build the Gemma-4 native chat prompt for a split (system_prefix, user_message).
///
/// Gemma 4's tokenizer has dedicated single-token markers for role turns:
/// `<user|>` opens a user turn, `<turn|>` closes it, `<model|>` opens the
/// model turn. The stable system prefix is inlined into the user turn so
/// llama.cpp's prefix-cache can reuse it across calls (the KV cache is
/// keyed on the token prefix of the full prompt).
pub(super) fn build_gemma_prompt(system_prefix: &str, user_message: &str) -> String {
    // Keep the concatenation cheap: one allocation, no Vec<String>.
    let mut out = String::with_capacity(system_prefix.len() + user_message.len() + 32);
    out.push_str("<user|>");
    if !system_prefix.is_empty() {
        out.push_str(system_prefix);
        out.push_str("\n\n---\n\n");
    }
    out.push_str(user_message);
    out.push_str("<turn|>\n<model|>\n");
    out
}

/// Extract the final-answer portion of Gemma 4's output, dropping the
/// reasoning trace.
///
/// Gemma 4 emits `<|channel>thought\n...\n<channel|>{final_answer}`. Take
/// the text after the last `<channel|>` marker; if the marker is absent
/// (the model decided not to use the channel this call), return the whole
/// content unchanged.
pub(super) fn strip_channel_trace(content: &str) -> &str {
    const MARKER: &str = "<channel|>";
    match content.rsplit_once(MARKER) {
        Some((_, tail)) => tail,
        None => content,
    }
}

/// Strip leading/trailing ```json fences if present, returning a JSON-parseable slice.
///
/// Gemma 4 often wraps structured output in a fenced code block even when
/// instructed not to. Stripping here keeps the emitter's JSON parser from
/// choking.
pub(super) fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```JSON"))
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s)
        .trim_start();
    let s = s.strip_suffix("```").unwrap_or(s).trim_end();
    s
}

/// Extract exactly the first balanced JSON object (`{...}`) from a string,
/// ignoring any trailing content.
///
/// Gemma 4's freeform output frequently looks like:
/// ```text
/// {"expression": "..."}
/// (some stray natural-language tail, or a second fenced block, or a chat-turn token)
/// ```
/// `serde_json::from_str` is strict and rejects trailing characters, so we
/// hand it just the first balanced object. String-literal contents are
/// scanned for escapes so a `{` inside a JSON string doesn't bump depth.
///
/// Returns the input unchanged if no `{` is found or braces don't balance.
pub(super) fn extract_first_json_object(s: &str) -> &str {
    let bytes = s.as_bytes();
    let Some(start) = bytes.iter().position(|&b| b == b'{') else {
        return s;
    };
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[start..=i];
                }
            }
            _ => {}
        }
    }
    s
}

/// Run a prompt against the local Gemma server with a split
/// (system_prefix, user_message) shape.
///
/// Thin local-HTTP analog of `run_claude_api_warm_with_structured_output`.
/// Shares the same "split prompt" contract so the emitter dispatch table
/// stays symmetric across provider modes. The returned `AiResponse.content`
/// is the already-channel-filtered, fence-stripped final answer — callers
/// parse `{"expression": "..."}` out of it with the same
/// `extract_expression` helper they use for the Claude path.
pub(crate) fn run_gemma_local_warm_with_structured_output(
    system_prefix: &str,
    user_message: &str,
    endpoint: &str,
    model_alias: &str,
    _doctor_handle: Option<&DoctorHandle>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    _schema: &Value, // future: could be compiled to a GBNF grammar constraint
    schema_name: &str,
) -> AiResponse {
    let max_tokens = max_tokens_override.unwrap_or(512);
    let temperature = temperature_override.unwrap_or(0.0);

    let prompt = build_gemma_prompt(system_prefix, user_message);

    info!(
        "Running Gemma local (warm) endpoint={} model={} schema={} system_len={} user_len={} prompt_len={}",
        endpoint,
        model_alias,
        schema_name,
        system_prefix.len(),
        user_message.len(),
        prompt.len(),
    );

    let request_body = serde_json::json!({
        "prompt": prompt,
        "temperature": temperature,
        "n_predict": max_tokens,
        // Critical: without this, KV-cache reuse of the stable prefix is off.
        "cache_prompt": true,
        // Stop at end-of-turn. Gemma 4 also sometimes emits <user|> as a
        // run-on; stop on that too so we don't feed an open turn into the
        // parser.
        "stop": ["<turn|>", "<user|>"],
    });

    let url = format!("{}/completion", endpoint.trim_end_matches('/'));
    let client = local_client();

    retry_with_backoff("Gemma local (warm)", || {
        let response = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&request_body)
            .send();

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    return AiResponse::error(format!(
                        "Gemma local (warm) error ({}): {}",
                        status, body
                    ));
                }

                match resp.json::<Value>() {
                    Ok(json) => {
                        let raw = json["content"].as_str().unwrap_or("").to_string();
                        let after_channel = strip_channel_trace(&raw);
                        let unfenced = strip_code_fences(after_channel);
                        let final_content = extract_first_json_object(unfenced);

                        // llama.cpp usage fields come back under `tokens_*` or
                        // nested `timings`. The emitter cares about
                        // `input_tokens` and `output_tokens` for budget
                        // accounting; cache stats don't apply to local infer
                        // (cost is GPU time, not API tokens).
                        let input_tokens = json["tokens_evaluated"]
                            .as_u64()
                            .or_else(|| json["prompt_n"].as_u64())
                            .or_else(|| json["timings"]["prompt_n"].as_u64());
                        let output_tokens = json["tokens_predicted"]
                            .as_u64()
                            .or_else(|| json["timings"]["predicted_n"].as_u64());

                        if let (Some(pn), Some(pc)) = (
                            json["tokens_evaluated"].as_u64(),
                            json["tokens_cached"].as_u64(),
                        ) {
                            debug!(
                                "Gemma local (warm) KV reuse: new={} cached={} (prefix-cache ratio {:.1}%)",
                                pn,
                                pc,
                                if pn + pc == 0 {
                                    0.0
                                } else {
                                    100.0 * pc as f64 / (pn + pc) as f64
                                }
                            );
                        }

                        match (input_tokens, output_tokens) {
                            (Some(input), Some(output)) => AiResponse::success_with_tokens(
                                final_content.to_string(),
                                input,
                                output,
                            ),
                            _ => AiResponse::success(final_content.to_string()),
                        }
                    }
                    Err(e) => AiResponse::error(format!(
                        "Failed to parse Gemma local (warm) response: {}",
                        e
                    )),
                }
            }
            Err(e) => {
                // Distinguish "server not reachable" from other HTTP errors so
                // the Auto cascade can fall through cleanly on a missing
                // llama-server. reqwest's `connect` / `builder`-kind errors
                // are the wire-level signals we want.
                if e.is_connect() || e.is_timeout() {
                    AiResponse::error(format!(
                        "{}: Gemma local request to {} failed: {}",
                        LOCAL_NO_SERVER_MARKER, url, e
                    ))
                } else {
                    AiResponse::error(format!("Gemma local (warm) request failed: {}", e))
                }
            }
        }
    })
}

/// Determine whether an [`AiResponse`] failure came from the
/// "server unreachable" path vs a live HTTP/model error. Mirrors
/// `claude_api_warm::is_no_credential_error` so the emitter's Auto
/// cascade logic can treat both lanes symmetrically.
pub(crate) fn is_no_server_error(resp: &AiResponse) -> bool {
    if resp.success {
        return false;
    }
    matches!(&resp.error, Some(msg) if msg.contains(LOCAL_NO_SERVER_MARKER))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_embeds_system_and_user() {
        let p = build_gemma_prompt("STABLE_PREFIX_BLOCK", "per-call goal: X");
        assert!(p.starts_with("<user|>STABLE_PREFIX_BLOCK"));
        assert!(p.contains("\n\n---\n\n"));
        assert!(p.contains("per-call goal: X"));
        assert!(p.ends_with("<turn|>\n<model|>\n"));
    }

    #[test]
    fn build_prompt_omits_separator_when_system_empty() {
        let p = build_gemma_prompt("", "only-user");
        assert_eq!(p, "<user|>only-user<turn|>\n<model|>\n");
    }

    #[test]
    fn strip_channel_trace_takes_last_segment() {
        let c = "<|channel>thought\nI should extract...<channel|>{\"expression\": \"foo\"}";
        assert_eq!(strip_channel_trace(c), "{\"expression\": \"foo\"}");
    }

    #[test]
    fn strip_channel_trace_passthrough_when_no_marker() {
        let c = "{\"expression\": \"foo\"}";
        assert_eq!(strip_channel_trace(c), c);
    }

    #[test]
    fn strip_channel_trace_uses_last_marker_when_multiple() {
        // Model sometimes emits a nested channel before the final. The
        // final answer is always after the last marker.
        let c = "<channel|>intermediate<channel|>final";
        assert_eq!(strip_channel_trace(c), "final");
    }

    #[test]
    fn strip_code_fences_handles_json_fence() {
        let s = "```json\n{\"expression\": \"abc\"}\n```";
        assert_eq!(strip_code_fences(s), "{\"expression\": \"abc\"}");
    }

    #[test]
    fn strip_code_fences_handles_bare_fence() {
        let s = "```\n{\"k\": 1}\n```";
        assert_eq!(strip_code_fences(s), "{\"k\": 1}");
    }

    #[test]
    fn strip_code_fences_passthrough_when_no_fence() {
        let s = "{\"k\": 1}";
        assert_eq!(strip_code_fences(s), s);
    }

    #[test]
    fn extract_first_json_object_strips_trailing_text() {
        let s = "{\"expression\":\"foo\"}\n\nextra chat-turn garbage";
        assert_eq!(extract_first_json_object(s), "{\"expression\":\"foo\"}");
    }

    #[test]
    fn extract_first_json_object_handles_braces_in_strings() {
        // A `{` inside a JSON string must not bump the nesting counter.
        let s = r#"{"expression":"((x) => ({y: 1}))()"}trailing"#;
        assert_eq!(
            extract_first_json_object(s),
            r#"{"expression":"((x) => ({y: 1}))()"}"#
        );
    }

    #[test]
    fn extract_first_json_object_handles_escaped_quote_in_string() {
        let s = r#"{"expression":"a\"b","k":1}tail"#;
        assert_eq!(
            extract_first_json_object(s),
            r#"{"expression":"a\"b","k":1}"#
        );
    }

    #[test]
    fn extract_first_json_object_passthrough_when_unbalanced() {
        let s = "{unbalanced";
        assert_eq!(extract_first_json_object(s), s);
    }

    #[test]
    fn extract_first_json_object_passthrough_when_no_brace() {
        let s = "no json here";
        assert_eq!(extract_first_json_object(s), s);
    }

    #[test]
    fn is_no_server_error_matches_marker() {
        let r = AiResponse::error(format!("{}: connect refused", LOCAL_NO_SERVER_MARKER));
        assert!(is_no_server_error(&r));
    }

    #[test]
    fn is_no_server_error_rejects_unrelated_error() {
        let r = AiResponse::error("model OOM".to_string());
        assert!(!is_no_server_error(&r));
    }

    #[test]
    fn is_no_server_error_rejects_success() {
        let r = AiResponse::success("ok".to_string());
        assert!(!is_no_server_error(&r));
    }
}
