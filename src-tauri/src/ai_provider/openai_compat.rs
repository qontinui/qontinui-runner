#![allow(dead_code)]

//! Generic OpenAI-compatible `/chat/completions` client.
//!
//! Works against any endpoint that speaks the OpenAI chat-completions wire
//! format: DeepSeek (the default, see `OpenAiCompatibleSettings::default()`),
//! vLLM, LM Studio, Together, etc.
//!
//! API key resolution order:
//! 1. OS keychain slot `openai_compatible` (the same slot the Settings UI
//!    writes via `store_ai_api_key("openai_compatible", ...)`)
//! 2. `DEEPSEEK_API_KEY` env var
//! 3. `OPENAI_COMPATIBLE_API_KEY` env var
//!
//! A missing key is NOT an immediate error — local endpoints (vLLM, LM
//! Studio) often require no auth, so the request is sent without an
//! `Authorization` header and only a subsequent 401/403 produces a clear
//! "API key" error.

use super::types::AiResponse;
use crate::config_facade::{ai_keychain, keychain_keys};
use crate::doctor::DoctorHandle;
use crate::settings;
use tracing::{debug, info, warn};

/// Run a prompt via an OpenAI-compatible `/chat/completions` endpoint.
///
/// Blocking HTTP POST to `{base_url}/chat/completions` with the standard
/// OpenAI chat body. Honors `settings.timeout_seconds`. Parses
/// `choices[0].message.content` and, when present, `usage.prompt_tokens` /
/// `usage.completion_tokens` for token accounting.
///
/// Retry/backoff and circuit-breaker tracking are provided by the callers in
/// `routing.rs` (`retry_with_backoff_tracked` with the `openai_compatible`
/// breaker key), so this function performs exactly one attempt.
pub(super) fn run_openai_compat(
    prompt: &str,
    settings: &settings::OpenAiCompatibleSettings,
    model_override: Option<&str>,
    _doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let api_key = resolve_api_key();
    execute_chat_completion(prompt, settings, model_override, api_key.as_deref())
}

/// Resolve the API key: keychain first, then env fallbacks.
///
/// Returns `None` when no key is configured anywhere — callers proceed
/// without an `Authorization` header (valid for unauthenticated local
/// endpoints).
fn resolve_api_key() -> Option<String> {
    match ai_keychain().get(keychain_keys::OPENAI_COMPATIBLE_API_KEY) {
        Ok(Some(key)) if !key.trim().is_empty() => return Some(key),
        Ok(_) => {}
        Err(e) => {
            // Keychain trouble should not hard-fail the request path — the
            // env fallbacks (and keyless local endpoints) must still work.
            warn!(
                "Failed to read openai_compatible key from keychain (falling back to env): {}",
                e
            );
        }
    }

    for var in ["DEEPSEEK_API_KEY", "OPENAI_COMPATIBLE_API_KEY"] {
        if let Ok(val) = std::env::var(var) {
            if !val.trim().is_empty() {
                debug!("Using OpenAI-compatible API key from {} env var", var);
                return Some(val);
            }
        }
    }

    None
}

/// Single-attempt chat-completion call with an explicit (already-resolved)
/// API key. Split out from `run_openai_compat` so unit tests can inject the
/// key without touching the OS keychain or process env.
fn execute_chat_completion(
    prompt: &str,
    settings: &settings::OpenAiCompatibleSettings,
    model_override: Option<&str>,
    api_key: Option<&str>,
) -> AiResponse {
    let base_url = settings.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return AiResponse::error(
            "OpenAI-compatible base_url is empty. Configure it in Settings (e.g. \
             https://api.deepseek.com)."
                .to_string(),
        );
    }

    let model = model_override.unwrap_or(&settings.model);
    info!(
        "Running OpenAI-compatible API (base_url: {}, model: {}, override: {}, has_key: {})",
        base_url,
        model,
        model_override.is_some(),
        api_key.is_some()
    );

    let mut builder = reqwest::blocking::Client::builder();
    if settings.timeout_seconds > 0 {
        builder = builder.timeout(std::time::Duration::from_secs(settings.timeout_seconds));
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return AiResponse::error(format!("Failed to create HTTP client: {}", e)),
    };

    let url = format!("{}/chat/completions", base_url);
    let request_body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}]
    });

    let mut request = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&request_body);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }

    match request.send() {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                if api_key.is_none()
                    && (status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN)
                {
                    return AiResponse::error(format!(
                        "OpenAI-compatible endpoint rejected the request ({}) and no API key \
                         is configured. Set one in Settings (keychain slot \
                         'openai_compatible') or via the DEEPSEEK_API_KEY / \
                         OPENAI_COMPATIBLE_API_KEY env var. Response: {}",
                        status, body
                    ));
                }
                return AiResponse::error(format!(
                    "OpenAI-compatible API error ({}): {}",
                    status, body
                ));
            }

            match resp.json::<serde_json::Value>() {
                Ok(json) => {
                    let content = json["choices"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|c| c["message"]["content"].as_str())
                        .unwrap_or("")
                        .to_string();

                    // OpenAI usage format:
                    // {"usage": {"prompt_tokens": N, "completion_tokens": N, "total_tokens": N}}
                    let input_tokens = json["usage"]["prompt_tokens"].as_u64();
                    let output_tokens = json["usage"]["completion_tokens"].as_u64();

                    if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                        debug!(
                            "OpenAI-compatible API tokens - input: {}, output: {}, total: {}",
                            input,
                            output,
                            input + output
                        );
                        AiResponse::success_with_tokens(content, input, output)
                    } else {
                        debug!(
                            "OpenAI-compatible API response missing token counts: usage={:?}",
                            json["usage"]
                        );
                        AiResponse::success(content)
                    }
                }
                Err(e) => AiResponse::error(format!(
                    "Failed to parse OpenAI-compatible API response: {}",
                    e
                )),
            }
        }
        Err(e) => AiResponse::error(format!("OpenAI-compatible API request failed: {}", e)),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// Read a full HTTP request (headers + Content-Length body) from a stream.
    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(header_end) = find_subsequence(&buf, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let content_length = headers
                    .lines()
                    .find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        if name.trim().eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if buf.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Spawn a one-shot mock HTTP server that replies with `status_line` +
    /// `body`, and sends the raw captured request back over a channel.
    fn spawn_mock_server(status_line: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let (tx, rx) = mpsc::channel();
        let status_line = status_line.to_string();
        let body = body.to_string();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let request = read_http_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = tx.send(request);
            }
        });
        (format!("http://{}", addr), rx)
    }

    fn test_settings(base_url: String, timeout_seconds: u64) -> settings::OpenAiCompatibleSettings {
        settings::OpenAiCompatibleSettings {
            base_url,
            model: "test-model".to_string(),
            timeout_seconds,
        }
    }

    #[test]
    fn success_parses_content_and_usage() {
        let (base_url, rx) = spawn_mock_server(
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"hello from mock"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        );
        // Trailing slash must be trimmed before appending /chat/completions.
        let settings = test_settings(format!("{}/", base_url), 5);

        let response = execute_chat_completion("say hello", &settings, None, Some("sk-test"));

        assert!(response.success, "expected success: {:?}", response.error);
        assert_eq!(response.output, "hello from mock");
        assert_eq!(response.input_tokens, Some(10));
        assert_eq!(response.output_tokens, Some(5));

        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("POST /chat/completions "),
            "unexpected request line: {}",
            request.lines().next().unwrap_or("")
        );
        assert!(
            request.contains("authorization: Bearer sk-test")
                || request.contains("Authorization: Bearer sk-test")
        );
        assert!(request.contains(r#""model":"test-model""#));
        assert!(request.contains(r#""content":"say hello""#));
    }

    #[test]
    fn success_without_usage_still_succeeds() {
        let (base_url, _rx) = spawn_mock_server(
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"no usage"}}]}"#,
        );
        let settings = test_settings(base_url, 5);

        let response = execute_chat_completion("hi", &settings, None, None);

        assert!(response.success, "expected success: {:?}", response.error);
        assert_eq!(response.output, "no usage");
        assert_eq!(response.input_tokens, None);
        assert_eq!(response.output_tokens, None);
    }

    #[test]
    fn model_override_wins_over_settings_model() {
        let (base_url, rx) =
            spawn_mock_server("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#);
        let settings = test_settings(base_url, 5);

        let response =
            execute_chat_completion("hi", &settings, Some("deepseek-reasoner"), Some("sk-x"));

        assert!(response.success);
        let request = rx.recv().expect("captured request");
        assert!(request.contains(r#""model":"deepseek-reasoner""#));
    }

    #[test]
    fn http_4xx_returns_error_with_status() {
        let (base_url, _rx) = spawn_mock_server("400 Bad Request", r#"{"error":"bad model name"}"#);
        let settings = test_settings(base_url, 5);

        let response = execute_chat_completion("hi", &settings, None, Some("sk-test"));

        assert!(!response.success);
        let err = response.error.expect("error message");
        assert!(err.contains("400"), "error should carry status: {}", err);
        assert!(err.contains("bad model name"));
    }

    #[test]
    fn timeout_returns_error() {
        // Handler accepts the connection, reads the request, and never replies.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request(&mut stream);
                thread::sleep(std::time::Duration::from_secs(5));
            }
        });
        let settings = test_settings(format!("http://{}", addr), 1);

        let response = execute_chat_completion("hi", &settings, None, None);

        assert!(!response.success);
        let err = response.error.expect("error message");
        assert!(
            err.contains("request failed"),
            "expected request-failed error, got: {}",
            err
        );
    }

    #[test]
    fn missing_key_plus_401_names_the_api_key() {
        let (base_url, _rx) = spawn_mock_server("401 Unauthorized", r#"{"error":"auth required"}"#);
        let settings = test_settings(base_url, 5);

        let response = execute_chat_completion("hi", &settings, None, None);

        assert!(!response.success);
        let err = response.error.expect("error message");
        assert!(
            err.contains("no API key"),
            "401-without-key must clearly name the missing API key: {}",
            err
        );
        assert!(err.contains("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn empty_base_url_is_a_clear_config_error() {
        let settings = test_settings(String::new(), 5);

        let response = execute_chat_completion("hi", &settings, None, None);

        assert!(!response.success);
        assert!(response.error.expect("error message").contains("base_url"));
    }
}
