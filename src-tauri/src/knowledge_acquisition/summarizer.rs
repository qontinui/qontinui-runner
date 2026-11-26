use crate::config_facade::ai_keychain;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

const DEFAULT_MAX_LENGTH: usize = 2000;
const SUMMARIZATION_THRESHOLD: usize = 3000;
/// Model used for summarization — uses Haiku for speed and cost efficiency.
/// Update this constant when model IDs change.
const SUMMARIZATION_MODEL: &str = "claude-haiku-4-5-20251001";

/// Summarize large content for storage in task_knowledge.
///
/// If LLM is available (Claude API key configured), uses Claude to produce a focused summary.
/// Otherwise falls back to simple truncation.
pub async fn summarize_for_storage(
    content: &str,
    query: &str,
    max_length: Option<usize>,
) -> Result<String, String> {
    let max_len = max_length.unwrap_or(DEFAULT_MAX_LENGTH);

    // Don't summarize if already short enough
    if content.len() <= SUMMARIZATION_THRESHOLD {
        return Ok(content.to_string());
    }

    // Try LLM summarization
    match llm_summarize(content, query, max_len).await {
        Ok(summary) => Ok(summary),
        Err(e) => {
            eprintln!("[summarizer] LLM summarization failed, using truncation fallback: {e}");
            Ok(truncate_smart(content, max_len))
        }
    }
}

/// Whether content needs summarization before embedding
pub fn needs_summarization(content: &str) -> bool {
    content.len() > SUMMARIZATION_THRESHOLD
}

/// LLM-based summarization using Claude API
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
async fn llm_summarize(content: &str, query: &str, max_length: usize) -> Result<String, String> {
    // Cap input to avoid sending too much to the API (safe UTF-8 boundary)
    let input = if content.len() > 50_000 {
        let mut end = 50_000;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        &content[..end]
    } else {
        content
    };

    let prompt = format!(
        "Summarize the following web content in relation to this query: \"{query}\"\n\
         Preserve: critical facts, code examples, version numbers, error messages, URLs.\n\
         Max length: {max_length} characters.\n\n{input}"
    );

    let request_body = serde_json::json!({
        "model": SUMMARIZATION_MODEL,
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": prompt
        }]
    });

    // Blocking: the route resolution may run the gateway's api-key-helper, and
    // the HTTP call uses the blocking client.
    let summary = spawn_blocking_tracked(move || {
        // Vendor host + keychain key, or the declared model gateway
        // (`crate::model_gateway`).
        let call = crate::model_gateway::ModelCall::resolve(|| {
            ai_keychain()
                .get("claude_api")
                .map_err(|e| format!("Keychain error: {e}"))?
                .ok_or_else(|| "No Claude API key configured".to_string())
        })?;
        summarize_via(&call, &request_body)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))??;

    Ok(summary)
}

/// Send one summarization request over `call` and return the first text block.
fn summarize_via(
    call: &crate::model_gateway::ModelCall,
    request_body: &serde_json::Value,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = call
        .post_blocking(&client, crate::model_gateway::MESSAGES_PATH)
        // Per request, not only per client: a gateway call uses the runner's
        // own no-redirect client, which carries no timeout of its own.
        .timeout(std::time::Duration::from_secs(30))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(request_body)
        .send()
        .map_err(|e| format!("Claude API request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("Claude API returned HTTP {status}: {body}"));
    }

    let response: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Failed to parse Claude response: {e}"))?;

    // Extract text from Claude Messages API response
    let text = response["content"]
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or("")
        .to_string();

    if text.is_empty() {
        return Err("Empty response from Claude API".to_string());
    }

    Ok(text)
}

/// Smart truncation: try to break at paragraph/sentence boundaries
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn truncate_smart(content: &str, max_length: usize) -> String {
    if content.len() <= max_length {
        return content.to_string();
    }

    // Try to find a paragraph break near the limit
    let search_range = &content[..max_length];

    // Prefer paragraph break
    if let Some(pos) = search_range.rfind("\n\n") {
        if pos > max_length / 2 {
            return format!("{}\n\n[... truncated]", &content[..pos]);
        }
    }

    // Prefer sentence break
    if let Some(pos) = search_range.rfind(". ") {
        if pos > max_length / 2 {
            return format!("{}.\n\n[... truncated]", &content[..pos]);
        }
    }

    // Hard truncate at char boundary
    let mut end = max_length;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[... truncated]", &content[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_smart_short() {
        assert_eq!(truncate_smart("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_smart_paragraph() {
        let content = format!(
            "First paragraph.\n\nSecond paragraph.\n\n{}",
            "x".repeat(3000)
        );
        let result = truncate_smart(&content, 80);
        assert!(result.contains("First paragraph."));
        assert!(result.contains("Second paragraph."));
        assert!(result.contains("[... truncated]"));
        assert!(result.len() < 100);
    }

    #[test]
    fn test_truncate_smart_sentence() {
        let content = format!("First sentence. Second sentence. {}", "x".repeat(3000));
        let result = truncate_smart(&content, 60);
        assert!(result.contains("[... truncated]"));
    }

    #[test]
    fn test_needs_summarization() {
        assert!(!needs_summarization("short"));
        assert!(needs_summarization(&"x".repeat(4000)));
    }

    #[tokio::test]
    async fn test_summarize_short_content_passthrough() {
        let result = summarize_for_storage("short content", "query", None)
            .await
            .unwrap();
        assert_eq!(result, "short content");
    }
}

#[cfg(test)]
mod gateway_routing_tests {
    use super::*;
    use crate::model_gateway::test_support::one_shot_messages_server;
    use crate::model_gateway::{ModelCall, ModelGatewaySettings};

    /// The summarizer's request reaches a declared gateway (path under its base
    /// URL, its routing header) and never the vendor host, and the reply is
    /// parsed from the gateway's response.
    #[test]
    fn summarizer_request_goes_to_the_declared_gateway() {
        let (base, server) = one_shot_messages_server("a summary");
        let mut decl = ModelGatewaySettings {
            base_url: Some(format!("{base}/llm")),
            network_auth: true,
            ..Default::default()
        };
        decl.headers.insert("X-Route".into(), "kb".into());
        let call = ModelCall::resolve_with(&decl, || panic!("vendor key must not be read"))
            .expect("gateway resolves");
        let body =
            serde_json::json!({"model": SUMMARIZATION_MODEL, "max_tokens": 8, "messages": []});
        assert_eq!(summarize_via(&call, &body).unwrap(), "a summary");
        let lines = server.join().unwrap();
        assert_eq!(lines[0], "POST /llm/v1/messages HTTP/1.1");
        assert!(
            lines.iter().any(|l| l.eq_ignore_ascii_case("x-route: kb")),
            "{lines:?}"
        );
    }
}
