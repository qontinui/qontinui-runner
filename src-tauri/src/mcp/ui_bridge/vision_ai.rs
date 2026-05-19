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
    "You are describing a UI screenshot for an automation agent. Respond with a SINGLE \
JSON object, no markdown fence, no prose outside it, with EXACTLY these two keys:\n\
\n\
1. \"description\": a concise one-paragraph human-readable caption — visible text, \
interactive elements (buttons, links, inputs), and their approximate layout. Do not \
invent text or elements that aren't visible. If a region is blank or all-whitespace, \
say so here.\n\
\n\
2. \"structured\": a machine-readable object with EXACTLY these keys:\n\
   - \"elements\": array of {\"role\": string (e.g. button|link|input|heading), \
\"text\"?: string, \"state\"?: array of (\"disabled\"|\"loading\"|\"selected\"|\"focused\"), \
\"color\"?: string, \"bbox\"?: {\"x\":int,\"y\":int,\"w\":int,\"h\":int}}\n\
   - \"modals\": array of {\"kind\": \"confirm\"|\"alert\"|\"form\", \"title\"?: string, \
\"ctas\"?: array of string (button labels)}\n\
   - \"overlays\": array of {\"kind\": \"tooltip\"|\"dropdown\"|\"menu\", \"text\"?: string}\n\
   - \"layout\": one of \"centered\"|\"split\"|\"list\"|\"grid\"|\"custom\"\n\
   - \"confidence\": float in [0,1]\n\
\n\
Use ONLY the enumerated values for state/kind/layout. Omit optional keys rather than \
emitting null. Empty arrays are fine. Do not add keys not listed above.";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    a.abs_diff(b)
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
    /// Closed-schema machine twin of `description` (plan §8 Phase 4 /
    /// goal #3 — prose-paired-with-structured). `None` when the model's
    /// reply was prose-only or failed strict validation; the endpoint
    /// still returns `description` in that case (graceful fallback) and a
    /// `UB-VLM-STRUCTURED-PARSE-FAIL` diagnostic is logged.
    pub structured: Option<VlmStructuredSummary>,
}

// ===========================================================================
// VLM structured-twin schema (plan §8 Phase 4)
//
// Closed schema. Every string set that the plan enumerates is a Rust enum so
// serde rejects out-of-vocabulary values at parse time (strict validation —
// an unknown `layout` / `state` / modal `kind` fails the whole parse and
// triggers the prose-only fallback rather than silently widening the type).
// `Bbox` is the existing vision-module bbox type (`OcrBbox`); reused so the
// describe twin and the extract blocks share one bbox shape.
// ===========================================================================

/// The vision module's canonical bounding-box type. Re-exported under the
/// plan's `Bbox` name for the structured-twin schema; identical wire shape
/// (`{x,y,w,h}` camelCase) as `vision/extract` `OcrBlock.bbox`.
pub type Bbox = OcrBbox;

/// Interactable / lifecycle states a VLM may report for an element. Closed
/// set — serde rejects anything else (strict validation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ElementState {
    Disabled,
    Loading,
    Selected,
    Focused,
}

/// One element the VLM identified in the screenshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VlmElement {
    /// Free-text role (`button`, `link`, `input`, `heading`, …). Not
    /// closed — UI role vocabulary is open-ended; the agent reads it as a
    /// hint, not a discriminator.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Vec<ElementState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Bbox>,
}

/// Modal-dialog kind. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModalKind {
    Confirm,
    Alert,
    Form,
}

/// A modal/dialog the VLM identified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VlmModal {
    pub kind: ModalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Call-to-action button labels in reading order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctas: Option<Vec<String>>,
}

/// Transient-overlay kind. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlayKind {
    Tooltip,
    Dropdown,
    Menu,
}

/// A transient overlay (tooltip / dropdown / menu) the VLM identified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VlmOverlay {
    pub kind: OverlayKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// High-level page-layout classification. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VlmLayout {
    Centered,
    Split,
    List,
    Grid,
    Custom,
}

/// Closed-schema machine twin of the VLM caption. Mirrors the TS shape in
/// plan §4 Phase 4. `deny_unknown_fields` + the closed enums above make
/// `serde_json::from_str::<VlmStructuredSummary>` the strict validator: any
/// extra key or out-of-vocabulary enum value fails the parse, which the
/// describe handler treats as "prose-only fallback".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VlmStructuredSummary {
    #[serde(default)]
    pub elements: Vec<VlmElement>,
    #[serde(default)]
    pub modals: Vec<VlmModal>,
    #[serde(default)]
    pub overlays: Vec<VlmOverlay>,
    pub layout: VlmLayout,
    pub confidence: f64,
}

/// The dual-audience envelope the VLM is prompted to emit in JSON mode:
/// `{ "description": "...", "structured": { ... } }`. Internal — we split
/// it into [`VlmDescription`]'s prose + structured fields, never surface
/// the envelope itself. `deny_unknown_fields` keeps the contract tight.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct VlmDualEnvelope {
    description: String,
    structured: VlmStructuredSummary,
}

/// Strictly parse the VLM reply as the dual-audience envelope.
///
/// Returns `Ok((description, Some(structured)))` only when the reply is the
/// well-formed `{description, structured}` JSON envelope AND `structured`
/// passes strict serde validation (closed enums + `deny_unknown_fields`).
///
/// On ANY failure — not JSON, missing keys, prose-only, out-of-vocabulary
/// enum, extra fields — returns `Err(reason)`. The caller (the describe
/// handler / [`VlmClient::describe`]) then falls back to prose-only with
/// `structured: None` and logs `UB-VLM-STRUCTURED-PARSE-FAIL`. This
/// function never panics and never returns a partially-validated twin.
fn parse_vlm_structured(content: &str) -> Result<(String, VlmStructuredSummary), String> {
    let stripped = strip_fence(content);
    let envelope: VlmDualEnvelope =
        serde_json::from_str(stripped).map_err(|e| format!("{e}: {}", trunc(stripped, 200)))?;
    Ok((envelope.description, envelope.structured))
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
            Some(p) if !p.trim().is_empty() => format!(
                "Describe this UI screenshot as the specified JSON object. Caller's focus: {}",
                p.trim()
            ),
            _ => "Describe this UI screenshot as the specified JSON object.".to_string(),
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
            // JSON mode — ask the endpoint to constrain output to a JSON
            // object. llama-swap / OpenAI-compatible servers honour this;
            // servers that ignore it just return text, which the
            // strict-parse + prose fallback below handles gracefully.
            "response_format": { "type": "json_object" },
        });

        debug!("VLM: POST {} (model={})", url, self.model);
        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AiError::Status(status.as_u16()));
        }
        let body: serde_json::Value = resp.json().await?;
        let raw_content = extract_chat_content(&body)
            .ok_or_else(|| AiError::Parse("no choices[0].message.content".into()))?;
        let tokens = body
            .get("usage")
            .and_then(|u| serde_json::from_value::<VlmTokens>(u.clone()).ok());

        // Strict-parse the dual-audience envelope. On success the prose
        // comes from `description` inside the JSON; on ANY failure we fall
        // back to prose-only with the raw content as the caption and a
        // logged `UB-VLM-STRUCTURED-PARSE-FAIL` diagnostic. The endpoint
        // never errors on a structured-parse failure (plan §8 Phase 4 —
        // "Never 500 on structured-parse failure").
        let (description, structured) = match parse_vlm_structured(&raw_content) {
            Ok((desc, summary)) => (desc.trim().to_string(), Some(summary)),
            Err(reason) => {
                // Canonical diagnostic code emitted as a literal string.
                // The typed `qontinui_schemas::ui_bridge_diagnostics`
                // enum is wired by Phase 5 — kept decoupled here on
                // purpose (no schemas-crate dep in P4).
                warn!(
                    "UB-VLM-STRUCTURED-PARSE-FAIL: VLM reply not a valid \
{{description, structured}} envelope, falling back to prose-only: {reason}"
                );
                (raw_content.trim().to_string(), None)
            }
        };
        if description.is_empty() {
            warn!("VLM: empty description returned");
        }
        Ok(VlmDescription {
            description,
            tokens,
            structured,
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

    // ===================================================================
    // VLM structured-twin schema + strict-validation + fallback (Phase 4)
    // ===================================================================

    /// A well-formed dual envelope (the happy path the prompt asks for)
    /// parses, and every closed enum / nested shape round-trips.
    #[test]
    fn parse_vlm_structured_accepts_well_formed_envelope() {
        let reply = r#"{
            "description": "Confirm dialog: Save (disabled), Cancel (blue).",
            "structured": {
                "elements": [
                    {"role":"button","text":"Save","state":["disabled"],"color":"gray",
                     "bbox":{"x":10,"y":20,"w":80,"h":30}},
                    {"role":"button","text":"Cancel","color":"blue"}
                ],
                "modals": [
                    {"kind":"confirm","title":"Unsaved changes","ctas":["Save","Cancel"]}
                ],
                "overlays": [{"kind":"tooltip","text":"Click to save"}],
                "layout": "centered",
                "confidence": 0.91
            }
        }"#;
        let (desc, s) = parse_vlm_structured(reply).expect("well-formed envelope must parse");
        assert_eq!(desc, "Confirm dialog: Save (disabled), Cancel (blue).");
        assert_eq!(s.layout, VlmLayout::Centered);
        assert_eq!(s.confidence, 0.91);
        assert_eq!(s.elements.len(), 2);
        assert_eq!(
            s.elements[0].state.as_deref(),
            Some(&[ElementState::Disabled][..])
        );
        assert_eq!(s.elements[0].bbox.unwrap().w, 80);
        assert_eq!(s.elements[1].text.as_deref(), Some("Cancel"));
        // Plan §8 Phase 4 assertion target: modals[0].ctas carries both labels.
        assert_eq!(s.modals.len(), 1);
        assert_eq!(s.modals[0].kind, ModalKind::Confirm);
        assert_eq!(
            s.modals[0].ctas.as_deref(),
            Some(&["Save".to_string(), "Cancel".to_string()][..])
        );
        assert_eq!(s.overlays[0].kind, OverlayKind::Tooltip);
    }

    /// A fenced envelope (model wrapped it in ```json despite the prompt)
    /// still parses — `strip_fence` is shared with the OCR path.
    #[test]
    fn parse_vlm_structured_handles_fence() {
        let reply = "```json\n{\"description\":\"empty\",\"structured\":\
{\"elements\":[],\"modals\":[],\"overlays\":[],\"layout\":\"custom\",\
\"confidence\":0.5}}\n```";
        let (desc, s) = parse_vlm_structured(reply).expect("fenced envelope must parse");
        assert_eq!(desc, "empty");
        assert_eq!(s.layout, VlmLayout::Custom);
        assert!(s.elements.is_empty());
    }

    /// Optional arrays default to empty when omitted (prompt says "omit
    /// rather than null"); required `layout`/`confidence` still enforced.
    #[test]
    fn parse_vlm_structured_defaults_optional_arrays() {
        let reply = r#"{"description":"d","structured":{"layout":"list","confidence":0.7}}"#;
        let (_d, s) = parse_vlm_structured(reply).expect("minimal structured must parse");
        assert!(s.elements.is_empty() && s.modals.is_empty() && s.overlays.is_empty());
        assert_eq!(s.layout, VlmLayout::List);
    }

    /// Strict validation: an out-of-vocabulary enum value (here a bogus
    /// `layout`) fails the parse — it is NOT silently coerced or widened.
    #[test]
    fn parse_vlm_structured_rejects_unknown_enum_value() {
        let reply = r#"{"description":"d","structured":{"elements":[],"modals":[],
            "overlays":[],"layout":"sidebar","confidence":0.8}}"#;
        assert!(
            parse_vlm_structured(reply).is_err(),
            "unknown layout 'sidebar' must fail strict validation"
        );
    }

    /// Strict validation: an unknown extra key inside `structured` fails
    /// the parse (`deny_unknown_fields`) — the schema is closed.
    #[test]
    fn parse_vlm_structured_rejects_extra_fields() {
        let reply = r#"{"description":"d","structured":{"elements":[],"modals":[],
            "overlays":[],"layout":"grid","confidence":0.8,"extra":"nope"}}"#;
        assert!(
            parse_vlm_structured(reply).is_err(),
            "extra key in structured must fail strict validation"
        );
    }

    /// Fallback trigger: a prose-only reply (the model ignored JSON mode)
    /// is NOT a valid envelope → Err, so the handler keeps the prose and
    /// emits `UB-VLM-STRUCTURED-PARSE-FAIL`.
    #[test]
    fn parse_vlm_structured_rejects_prose_only_reply() {
        let prose = "Modal: Save (disabled), Cancel (enabled, blue). Centered layout.";
        assert!(
            parse_vlm_structured(prose).is_err(),
            "prose-only reply must fail so the handler falls back"
        );
    }

    /// Fallback trigger: the envelope is present but `structured` is
    /// missing required keys → Err (no partially-validated twin).
    #[test]
    fn parse_vlm_structured_rejects_missing_required_keys() {
        let reply = r#"{"description":"d","structured":{"elements":[]}}"#;
        assert!(
            parse_vlm_structured(reply).is_err(),
            "missing layout/confidence must fail"
        );
    }

    /// The schema serializes back to the documented camelCase wire shape
    /// (what `DescribeResponse.structured` emits to the agent / skill).
    #[test]
    fn vlm_structured_summary_serializes_camel_case() {
        let s = VlmStructuredSummary {
            elements: vec![VlmElement {
                role: "input".into(),
                text: None,
                state: Some(vec![ElementState::Focused]),
                color: None,
                bbox: Some(OcrBbox {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
            }],
            modals: vec![],
            overlays: vec![],
            layout: VlmLayout::Split,
            confidence: 0.42,
        };
        let json = serde_json::to_value(&s).expect("serialize");
        assert_eq!(json["layout"], "split");
        assert_eq!(json["elements"][0]["state"][0], "focused");
        assert_eq!(json["elements"][0]["bbox"]["w"], 3);
        // Omitted optionals are not serialized (skip_serializing_if).
        assert!(json["elements"][0].get("text").is_none());
    }
}
