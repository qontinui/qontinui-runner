//! AI-backed vision operations (Phase 4 of the UI Bridge Vision Pipeline).
//!
//! Two clients sit behind the OpenAI-compatible Chat Completions API
//! exposed by llama-swap:
//!
//! - [`OcrClient`] — sends an image and asks the model to return a JSON
//!   array of `{ bbox, text, confidence }` blocks. Post-processes for
//!   whitespace collapse + per-block dedup + confidence floor, ported
//!   verbatim from `qontinui/src/qontinui/semantic/processors/ocr_processor.py`
//!   plus the IOCREngine-shape contract in `find/backends/ocr_backend.py`.
//!
//! - [`VlmClient`] — sends an image plus a description prompt and
//!   returns the model's natural-language caption. Prompt templates are
//!   carried over from `qontinui/src/qontinui/find/backends/{grounding_vlm,vision_llm}_backend.py`
//!   — same UI-TARS family the Python side validated against.
//!
//! Configuration: env vars `QONTINUI_VISION_OCR_ENDPOINT`,
//! `QONTINUI_VISION_OCR_MODEL`, `QONTINUI_VISION_VLM_ENDPOINT`,
//! `QONTINUI_VISION_VLM_MODEL`. All default to `http://127.0.0.1:8100`
//! (the standard local llama-swap port) with model aliases
//! `paddleocr` and `qontinui-grounding-v1`. Override at process start
//! to point at a different llama-swap instance or remote model.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tracing::{debug, warn};

/// Default llama-swap host endpoint. Matches `verification::mode::DEFAULT_ENDPOINT`.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8100";
pub const DEFAULT_OCR_MODEL: &str = "paddleocr";
pub const DEFAULT_VLM_MODEL: &str = "qontinui-grounding-v1";

pub const ENV_OCR_ENDPOINT: &str = "QONTINUI_VISION_OCR_ENDPOINT";
pub const ENV_OCR_MODEL: &str = "QONTINUI_VISION_OCR_MODEL";
pub const ENV_VLM_ENDPOINT: &str = "QONTINUI_VISION_VLM_ENDPOINT";
pub const ENV_VLM_MODEL: &str = "QONTINUI_VISION_VLM_MODEL";

/// OCR system prompt. Asks the model to emit a JSON array directly so we
/// don't have to robust-parse free-text. Mirrors the structured-output
/// contract from `OCRProcessor._process_with_tesseract` (Python):
/// each block has bbox, text, confidence — no scene-graph or object-type
/// classification (that's Phase 6 analyzers).
pub const OCR_SYSTEM_PROMPT: &str =
    "You are an OCR engine. You will be shown a single image. Identify every block of \
visible text. For each block, emit one JSON object with: `bbox` (object with `x`, `y`, \
`w`, `h` in pixels relative to the image), `text` (the rendered text, exactly as it \
appears, with no transformation), and `confidence` (float in [0,1]). Respond with a \
single JSON array of these objects, no prose, no markdown fence. If you see no text, \
respond with `[]`.";

/// VLM description prompt. Ports the prompt shape from
/// `qontinui/src/qontinui/find/backends/vision_llm_backend.py` — a concise
/// structured-text request that surfaces the elements/regions an agent
/// would care about (text content + layout cues + interactable signals).
pub const VLM_SYSTEM_PROMPT: &str =
    "You are describing a UI screenshot for an automation agent. Focus on what the agent \
would need to act: visible text, interactive elements (buttons, links, inputs), and \
their approximate layout. Be concise — one paragraph. Do not invent text or elements \
that aren't visible. If a region is blank or all-whitespace, say so.";

#[derive(Debug, Error)]
pub enum AiError {
    #[error("HTTP error calling vision endpoint: {0}")]
    Http(#[from] reqwest::Error),
    #[error("vision endpoint returned non-success status: {0}")]
    Status(u16),
    #[error("failed to parse vision response: {0}")]
    Parse(String),
}

// ===========================================================================
// OCR client
// ===========================================================================

/// One detected text block. The contract for downstream consumers
/// (`vision/extract` response, `/visual-check` skill).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrBlock {
    pub bbox: OcrBbox,
    pub text: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrBbox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Raw shape the model is asked to emit. Internal; we transform into [`OcrBlock`]
/// after post-processing.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcrRawBlock {
    bbox: OcrBbox,
    text: String,
    confidence: f64,
}

#[derive(Debug, Clone)]
pub struct OcrClient {
    client: Client,
    endpoint: String,
    model: String,
}

impl OcrClient {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            // OCR is fast (≤300ms warm). Cold model-load through llama-swap can
            // take a minute on first invocation. Match WSM's 5-minute ceiling.
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }

    pub fn from_env() -> Self {
        let endpoint =
            std::env::var(ENV_OCR_ENDPOINT).unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
        let model = std::env::var(ENV_OCR_MODEL).unwrap_or_else(|_| DEFAULT_OCR_MODEL.to_string());
        Self::new(endpoint, model)
    }

    /// Send the image bytes (already encoded as PNG/JPEG/WebP — anything the
    /// model accepts) and return the parsed + post-processed blocks plus
    /// the aggregate text (one-per-line in scan-order, top-to-bottom).
    pub async fn extract(
        &self,
        image_bytes: &[u8],
        image_mime: &str,
        min_confidence: f64,
    ) -> Result<(Vec<OcrBlock>, String), AiError> {
        let b64 = B64.encode(image_bytes);
        let url = format!(
            "{}/v1/chat/completions",
            self.endpoint.trim_end_matches('/')
        );
        let payload = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": OCR_SYSTEM_PROMPT },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Extract all text from this image." },
                        { "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", image_mime, b64) } }
                    ]
                }
            ],
            "temperature": 0.0,
            "max_tokens": 4096,
        });

        debug!("OCR: POST {} (model={})", url, self.model);
        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AiError::Status(status.as_u16()));
        }
        let body: serde_json::Value = resp.json().await?;
        let content = extract_chat_content(&body)
            .ok_or_else(|| AiError::Parse("no choices[0].message.content".into()))?;

        let raw = parse_ocr_json(&content)?;
        let blocks = post_process_blocks(raw, min_confidence);
        let aggregate = aggregate_text(&blocks);
        Ok((blocks, aggregate))
    }
}

/// Parse the model's response. Handles bare JSON arrays and
/// fence-wrapped responses (` ```json [..] ``` `).
fn parse_ocr_json(content: &str) -> Result<Vec<OcrRawBlock>, AiError> {
    let stripped = strip_fence(content);
    serde_json::from_str(stripped)
        .map_err(|e| AiError::Parse(format!("OCR JSON: {e}: {}", trunc(stripped, 200))))
}

/// Drop ```` ```json ... ``` ```` fence if present. The OpenAI streaming API
/// often wraps structured output in a fence even when the system prompt
/// says no markdown.
fn strip_fence(s: &str) -> &str {
    let trimmed = s.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    body.strip_suffix("```").unwrap_or(body).trim()
}

/// OCR post-processing pipeline, ported from `OCRProcessor._process_*` (Python):
///
/// 1. Drop empty / whitespace-only text.
/// 2. Drop blocks below `min_confidence`.
/// 3. Collapse internal whitespace (multi-space, tabs → single space).
/// 4. Dedup identical (text, ≈bbox) tuples — some engines emit one block
///    per line AND a duplicate spanning the same lines.
/// 5. Sort top-to-bottom, left-to-right for stable scan order.
fn post_process_blocks(raw: Vec<OcrRawBlock>, min_confidence: f64) -> Vec<OcrBlock> {
    let mut out: Vec<OcrBlock> = raw
        .into_iter()
        .filter_map(|r| {
            let collapsed = collapse_whitespace(&r.text);
            if collapsed.is_empty() {
                return None;
            }
            if r.confidence < min_confidence {
                return None;
            }
            Some(OcrBlock {
                bbox: r.bbox,
                text: collapsed,
                confidence: r.confidence,
            })
        })
        .collect();

    // Dedup: same text + bbox within 4 px on each side.
    out.sort_by(|a, b| {
        a.bbox
            .y
            .cmp(&b.bbox.y)
            .then(a.bbox.x.cmp(&b.bbox.x))
            .then(a.text.cmp(&b.text))
    });
    let mut deduped: Vec<OcrBlock> = Vec::with_capacity(out.len());
    for block in out {
        let dup = deduped
            .iter()
            .any(|existing| existing.text == block.text && box_close(existing.bbox, block.bbox, 4));
        if !dup {
            deduped.push(block);
        }
    }
    deduped
}

fn box_close(a: OcrBbox, b: OcrBbox, tol: u32) -> bool {
    diff(a.x, b.x) <= tol && diff(a.y, b.y) <= tol && diff(a.w, b.w) <= tol && diff(a.h, b.h) <= tol
}

fn diff(a: u32, b: u32) -> u32 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

/// Collapse runs of `\t`, multi-space, and embedded newlines into single
/// spaces. Trim leading/trailing. Mirrors the whitespace-collapse logic
/// in the Python OCRProcessor's `text.strip()` + downstream
/// `' '.join(text.split())` patterns scattered through callers.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = true; // suppress leading whitespace
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Concatenate text in scan order, one block per line. The aggregate is
/// useful for "did the page contain string X" without needing to walk
/// the bbox list.
fn aggregate_text(blocks: &[OcrBlock]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ===========================================================================
// VLM describe client
// ===========================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VlmDescription {
    pub description: String,
    pub tokens: Option<VlmTokens>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VlmTokens {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct VlmClient {
    client: Client,
    endpoint: String,
    model: String,
}

impl VlmClient {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }

    pub fn from_env() -> Self {
        let endpoint =
            std::env::var(ENV_VLM_ENDPOINT).unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
        let model = std::env::var(ENV_VLM_MODEL).unwrap_or_else(|_| DEFAULT_VLM_MODEL.to_string());
        Self::new(endpoint, model)
    }

    /// Send the image + optional user prompt addendum and return the
    /// caption. `extra_prompt` is appended to the canonical VLM_SYSTEM_PROMPT
    /// scope (e.g., "Focus on the terminal area.").
    pub async fn describe(
        &self,
        image_bytes: &[u8],
        image_mime: &str,
        extra_prompt: Option<&str>,
        max_tokens: u32,
    ) -> Result<VlmDescription, AiError> {
        let b64 = B64.encode(image_bytes);
        let url = format!(
            "{}/v1/chat/completions",
            self.endpoint.trim_end_matches('/')
        );
        let user_text = match extra_prompt {
            Some(p) if !p.trim().is_empty() => {
                format!("Describe this UI screenshot. Caller's focus: {}", p.trim())
            }
            _ => "Describe this UI screenshot.".to_string(),
        };
        let payload = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": VLM_SYSTEM_PROMPT },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": user_text },
                        { "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", image_mime, b64) } }
                    ]
                }
            ],
            "temperature": 0.0,
            "max_tokens": max_tokens.max(64),
        });

        debug!("VLM: POST {} (model={})", url, self.model);
        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AiError::Status(status.as_u16()));
        }
        let body: serde_json::Value = resp.json().await?;
        let description = extract_chat_content(&body)
            .ok_or_else(|| AiError::Parse("no choices[0].message.content".into()))?
            .trim()
            .to_string();
        if description.is_empty() {
            warn!("VLM: empty description returned");
        }
        let tokens = body
            .get("usage")
            .and_then(|u| serde_json::from_value::<VlmTokens>(u.clone()).ok());
        Ok(VlmDescription {
            description,
            tokens,
        })
    }
}

// ===========================================================================
// Shared helpers
// ===========================================================================

fn extract_chat_content(body: &serde_json::Value) -> Option<String> {
    body.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

fn trunc(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        // Find the previous char boundary ≤ n.
        let mut end = n;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_whitespace_runs() {
        assert_eq!(collapse_whitespace("hello   world"), "hello world");
        assert_eq!(collapse_whitespace("\t a\n\nb \t"), "a b");
        assert_eq!(collapse_whitespace("   "), "");
    }

    #[test]
    fn post_process_filters_empty_and_low_conf() {
        let raw = vec![
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                text: "Click me".into(),
                confidence: 0.95,
            },
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 0,
                    y: 20,
                    w: 10,
                    h: 10,
                },
                text: "   ".into(),
                confidence: 0.99,
            },
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 0,
                    y: 40,
                    w: 10,
                    h: 10,
                },
                text: "Low conf".into(),
                confidence: 0.5,
            },
        ];
        let out = post_process_blocks(raw, 0.7);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Click me");
    }

    #[test]
    fn post_process_dedupes_close_boxes() {
        let raw = vec![
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 100,
                    y: 200,
                    w: 50,
                    h: 20,
                },
                text: "Submit".into(),
                confidence: 0.95,
            },
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 102,
                    y: 201,
                    w: 51,
                    h: 20,
                },
                text: "Submit".into(),
                confidence: 0.93,
            },
        ];
        let out = post_process_blocks(raw, 0.0);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn post_process_keeps_different_text_at_same_position() {
        let raw = vec![
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 100,
                    y: 200,
                    w: 50,
                    h: 20,
                },
                text: "Submit".into(),
                confidence: 0.95,
            },
            OcrRawBlock {
                bbox: OcrBbox {
                    x: 100,
                    y: 200,
                    w: 50,
                    h: 20,
                },
                text: "Cancel".into(),
                confidence: 0.93,
            },
        ];
        let out = post_process_blocks(raw, 0.0);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn aggregate_text_joins_scan_order() {
        let blocks = vec![
            OcrBlock {
                bbox: OcrBbox {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                text: "First".into(),
                confidence: 0.9,
            },
            OcrBlock {
                bbox: OcrBbox {
                    x: 0,
                    y: 20,
                    w: 10,
                    h: 10,
                },
                text: "Second".into(),
                confidence: 0.9,
            },
        ];
        assert_eq!(aggregate_text(&blocks), "First\nSecond");
    }

    #[test]
    fn parse_ocr_json_handles_fence() {
        let fenced = "```json\n[{\"bbox\":{\"x\":0,\"y\":0,\"w\":10,\"h\":10},\"text\":\"hi\",\"confidence\":0.9}]\n```";
        let parsed = parse_ocr_json(fenced).expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "hi");
    }

    #[test]
    fn parse_ocr_json_handles_bare_array() {
        let bare = r#"[{"bbox":{"x":0,"y":0,"w":10,"h":10},"text":"hi","confidence":0.9}]"#;
        let parsed = parse_ocr_json(bare).expect("parse");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_ocr_json_rejects_garbage() {
        assert!(parse_ocr_json("hello world").is_err());
    }
}
