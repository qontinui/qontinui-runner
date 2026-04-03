//! Multimodal Prompt Types
//!
//! Types for constructing prompts that include both text and image content.
//! Used by the multimodal variants of the AI provider functions to send
//! annotated screenshots alongside text prompts to vision-capable models.
//!
//! These types serialize to the native content block format expected by
//! each provider's API (Claude, Gemini). The routing layer handles the
//! translation.

use serde::Serialize;

// =============================================================================
// Content Block Types
// =============================================================================

/// A single content block in a multimodal prompt.
///
/// Serializes to Claude's content block format:
/// ```json
/// {"type": "text", "text": "..."}
/// {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "..."}}
/// ```
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// Image source for a content block (base64-encoded).
#[derive(Debug, Clone, Serialize)]
pub struct ImageSource {
    /// Always "base64" for inline images.
    #[serde(rename = "type")]
    pub source_type: String,
    /// MIME type, e.g. "image/png" or "image/jpeg".
    pub media_type: String,
    /// Base64-encoded image data (no data URL prefix).
    pub data: String,
}

// =============================================================================
// Multimodal Prompt
// =============================================================================

/// A prompt that can contain both text and image content blocks.
///
/// Use this instead of `&str` when you need to send screenshots or other
/// images to the AI model alongside text instructions.
#[derive(Debug, Clone)]
pub struct MultimodalPrompt {
    /// Optional system prompt (used by Claude API's top-level `system` field).
    pub system: Option<String>,
    /// Content blocks in order — typically text first, then image(s), then more text.
    pub content: Vec<ContentBlock>,
}

impl MultimodalPrompt {
    /// Create a text-only multimodal prompt (for backward compatibility).
    pub fn text_only(text: &str) -> Self {
        Self {
            system: None,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// Create a prompt with text and a single screenshot.
    ///
    /// The screenshot should be base64-encoded PNG data (without the `data:image/png;base64,` prefix).
    pub fn with_screenshot(text: &str, screenshot_base64: &str) -> Self {
        Self {
            system: None,
            content: vec![
                ContentBlock::Text {
                    text: text.to_string(),
                },
                ContentBlock::Image {
                    source: ImageSource {
                        source_type: "base64".to_string(),
                        media_type: "image/png".to_string(),
                        data: screenshot_base64.to_string(),
                    },
                },
            ],
        }
    }

    /// Create a prompt with a system message, text, and a screenshot.
    pub fn with_system_and_screenshot(system: &str, text: &str, screenshot_base64: &str) -> Self {
        Self {
            system: Some(system.to_string()),
            content: vec![
                ContentBlock::Text {
                    text: text.to_string(),
                },
                ContentBlock::Image {
                    source: ImageSource {
                        source_type: "base64".to_string(),
                        media_type: "image/png".to_string(),
                        data: screenshot_base64.to_string(),
                    },
                },
            ],
        }
    }

    /// Extract all text content from the prompt, ignoring images.
    ///
    /// Used as a fallback when the provider doesn't support multimodal (e.g. CLI).
    pub fn text_content(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref system) = self.system {
            parts.push(system.clone());
        }
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                parts.push(text.clone());
            }
        }
        parts.join("\n\n")
    }

    /// Check if this prompt contains any image content blocks.
    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    }

    /// Build the Claude API `messages` array as a JSON value.
    ///
    /// Claude format:
    /// ```json
    /// [{"role": "user", "content": [{"type": "text", ...}, {"type": "image", ...}]}]
    /// ```
    pub fn to_claude_messages(&self) -> serde_json::Value {
        serde_json::json!([{
            "role": "user",
            "content": self.content
        }])
    }

    /// Build the Gemini API `contents` array as a JSON value.
    ///
    /// Gemini format:
    /// ```json
    /// [{"parts": [{"text": "..."}, {"inline_data": {"mime_type": "...", "data": "..."}}]}]
    /// ```
    pub fn to_gemini_contents(&self) -> serde_json::Value {
        let parts: Vec<serde_json::Value> = self
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => serde_json::json!({"text": text}),
                ContentBlock::Image { source } => serde_json::json!({
                    "inline_data": {
                        "mime_type": source.media_type,
                        "data": source.data
                    }
                }),
            })
            .collect();

        serde_json::json!([{"parts": parts}])
    }

    /// Build the Gemini API `systemInstruction` field as a JSON value.
    ///
    /// Returns `None` if no system prompt is set. Gemini supports system
    /// instructions via a top-level `systemInstruction` field alongside
    /// `contents`.
    pub fn to_gemini_system_instruction(&self) -> Option<serde_json::Value> {
        self.system.as_ref().map(|sys| {
            serde_json::json!({
                "parts": [{"text": sys}]
            })
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_only_prompt() {
        let prompt = MultimodalPrompt::text_only("Hello, world!");
        assert!(!prompt.has_images());
        assert_eq!(prompt.text_content(), "Hello, world!");
    }

    #[test]
    fn test_with_screenshot() {
        let prompt = MultimodalPrompt::with_screenshot("Analyze this", "iVBORw0KGgo=");
        assert!(prompt.has_images());
        assert_eq!(prompt.text_content(), "Analyze this");
        assert_eq!(prompt.content.len(), 2);
    }

    #[test]
    fn test_with_system_and_screenshot() {
        let prompt = MultimodalPrompt::with_system_and_screenshot(
            "You are a UI verifier",
            "Check this screenshot",
            "iVBORw0KGgo=",
        );
        assert!(prompt.has_images());
        assert_eq!(prompt.system.as_deref(), Some("You are a UI verifier"));
        assert_eq!(
            prompt.text_content(),
            "You are a UI verifier\n\nCheck this screenshot"
        );
    }

    #[test]
    fn test_claude_messages_text_only() {
        let prompt = MultimodalPrompt::text_only("Hello");
        let messages = prompt.to_claude_messages();
        let content = &messages[0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello");
    }

    #[test]
    fn test_claude_messages_with_image() {
        let prompt = MultimodalPrompt::with_screenshot("Analyze", "AAAA");
        let messages = prompt.to_claude_messages();
        let content = &messages[0]["content"];

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Analyze");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAAA");
    }

    #[test]
    fn test_gemini_contents_with_image() {
        let prompt = MultimodalPrompt::with_screenshot("Analyze", "AAAA");
        let contents = prompt.to_gemini_contents();
        let parts = &contents[0]["parts"];

        assert_eq!(parts[0]["text"], "Analyze");
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "AAAA");
    }

    #[test]
    fn test_gemini_contents_text_only() {
        let prompt = MultimodalPrompt::text_only("Hello");
        let contents = prompt.to_gemini_contents();
        let parts = &contents[0]["parts"];

        assert_eq!(parts[0]["text"], "Hello");
        assert!(parts.get(1).is_none() || parts[1].is_null());
    }
}
