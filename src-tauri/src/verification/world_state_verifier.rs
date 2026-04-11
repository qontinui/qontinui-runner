//! World State Verifier: VLM-based judge for whether a GUI action
//! achieved its declared intent.
//!
//! Wraps Zery/CUA_World_State_Model (Qwen2.5-VL-7B-Instruct fine-tune,
//! SEAgent judge) served via llama-swap's OpenAI-compatible endpoint.
//! Sends pre- and post-action screenshots plus a text intent, gets back
//! a structured verdict with a calibrated confidence and a possible
//! "refused" outcome (action was a no-op on the intended target).

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tracing::{debug, warn};

use crate::agentic_verification::{
    VerificationIssue, VerificationStatus, VerificationVerdict,
};

/// Errors returned by the WorldStateVerifier. Callers should fall back
/// to the existing text-based verifier agent on any error.
#[derive(Debug, Error)]
pub enum VerifierError {
    #[error("HTTP error calling WSM endpoint: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WSM endpoint returned non-success status: {0}")]
    Status(u16),
    #[error("failed to parse WSM response: {0}")]
    Parse(String),
    #[error("WSM response contained no usable verdict")]
    EmptyVerdict,
}

/// Raw JSON schema we ask the WSM to emit. Separate from
/// `VerificationVerdict` so the prompt contract and the internal type
/// can evolve independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WsmJudgement {
    /// One of: "pass", "fail", "refused", "partial".
    status: String,
    /// Model-calibrated confidence in [0.0, 1.0].
    confidence: f64,
    /// Short explanation of what changed between pre and post.
    observations: String,
    /// When `status == "refused"`: why the action was judged a no-op.
    #[serde(default)]
    refused_reason: Option<String>,
    /// Optional: what the worker should try next.
    #[serde(default)]
    next_priority: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorldStateVerifier {
    client: Client,
    endpoint: String,
    model: String,
}

impl WorldStateVerifier {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            // llama-swap may cold-start the model on first request (weight
            // download on the very first call, ~14GB). 5 minutes is enough
            // for cached starts; cold starts will exceed this and fall back
            // to the text verifier, which is the desired behavior — we
            // don't want to block the agentic loop on a one-time download.
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }

    /// Build a verifier configured from QONTINUI_WORLD_STATE_VERIFIER_*
    /// env vars. Pairs with `verification::mode::parse_mode`.
    pub fn from_env() -> Self {
        Self::new(super::mode::endpoint(), super::mode::model_name())
    }

    /// Judge whether the action described by `intent` transformed the UI
    /// from `pre_screenshot_b64` to `post_screenshot_b64` in a way that
    /// achieved the intent.
    ///
    /// Both screenshots are raw base64-encoded PNG bytes (no data-URL
    /// prefix) — the verifier adds the `data:image/png;base64,` prefix
    /// when building the OpenAI message.
    pub async fn verify(
        &self,
        pre_screenshot_b64: &str,
        post_screenshot_b64: &str,
        intent: &str,
        goal_context: Option<&str>,
    ) -> Result<VerificationVerdict, VerifierError> {
        let payload = self.build_payload(
            pre_screenshot_b64,
            post_screenshot_b64,
            intent,
            goal_context,
        );

        let url = format!("{}/v1/chat/completions", self.endpoint.trim_end_matches('/'));
        debug!("WSM: POST {} (model={})", url, self.model);

        let resp = self.client.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VerifierError::Status(status.as_u16()));
        }
        let body: serde_json::Value = resp.json().await?;

        let content = extract_content(&body)
            .ok_or_else(|| VerifierError::Parse("no choices[0].message.content".into()))?;

        let judgement = parse_judgement(&content)?;
        Ok(judgement_to_verdict(judgement))
    }

    fn build_payload(
        &self,
        pre_b64: &str,
        post_b64: &str,
        intent: &str,
        goal_context: Option<&str>,
    ) -> serde_json::Value {
        let system_prompt = WSM_SYSTEM_PROMPT;
        let user_text = build_user_text(intent, goal_context);

        json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": user_text},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/png;base64,{}", pre_b64)
                            }
                        },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/png;base64,{}", post_b64)
                            }
                        },
                    ],
                }
            ],
            "temperature": 0.0,
            "max_tokens": 512,
        })
    }
}

const WSM_SYSTEM_PROMPT: &str = "You are a GUI world-state verifier. You receive two screenshots \
(PRE and POST) of an application and a text intent describing what action the agent attempted. \
Your job is to judge, by comparing the two screenshots, whether the POST state reflects that the \
intended action was actually performed on the correct target.\n\n\
Output STRICT JSON matching this schema — no prose before or after:\n\
```json\n\
{\n\
  \"status\": \"pass\" | \"fail\" | \"refused\" | \"partial\",\n\
  \"confidence\": <float 0.0-1.0>,\n\
  \"observations\": \"<short explanation of what changed>\",\n\
  \"refused_reason\": \"<present only if status=refused>\",\n\
  \"next_priority\": \"<optional: what the worker should try next>\"\n\
}\n\
```\n\n\
Status semantics:\n\
- pass: The intended action was performed on the correct target AND the resulting state \
matches the intent.\n\
- partial: Some visible progress toward the intent, but not complete.\n\
- fail: No visible progress, or the state regressed.\n\
- refused: The action did NOT interact with the intended target (wrong button clicked, \
no-op, click missed, click landed on the wrong element). This is distinct from fail — \
use refused when the agent appears to have acted on the wrong element.\n\n\
Confidence must be calibrated: use high values (>0.8) only when you can clearly see the \
intended change or the clear absence of it. Use middle values (0.5-0.7) when you are unsure.";

fn build_user_text(intent: &str, goal_context: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(goal) = goal_context {
        if !goal.is_empty() {
            s.push_str("## Overall Goal\n");
            s.push_str(goal);
            s.push_str("\n\n");
        }
    }
    s.push_str("## Action Intent (what the worker just attempted)\n");
    s.push_str(intent);
    s.push_str("\n\nThe first image is PRE (before the action). The second image is POST (after the action). Compare them and return the JSON verdict.");
    s
}

fn extract_content(body: &serde_json::Value) -> Option<String> {
    body.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

fn parse_judgement(content: &str) -> Result<WsmJudgement, VerifierError> {
    // Find a JSON object in the content — strip markdown fences if present.
    let trimmed = content.trim();
    let json_str = if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        let end = after
            .find("```")
            .ok_or_else(|| VerifierError::Parse("unterminated ```json fence".into()))?;
        after[..end].trim()
    } else if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        // Skip optional language tag line.
        let line_end = after
            .find('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let body = &after[line_end..];
        let end = body
            .find("```")
            .ok_or_else(|| VerifierError::Parse("unterminated ``` fence".into()))?;
        body[..end].trim()
    } else if let Some(start) = trimmed.find('{') {
        // Raw JSON — find the matching closing brace.
        let tail = &trimmed[start..];
        let mut depth = 0i32;
        let mut end = 0usize;
        for (i, ch) in tail.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 || end == 0 {
            return Err(VerifierError::Parse("no balanced JSON object".into()));
        }
        &tail[..end]
    } else {
        return Err(VerifierError::Parse("no JSON found in WSM output".into()));
    };

    serde_json::from_str::<WsmJudgement>(json_str)
        .map_err(|e| VerifierError::Parse(format!("{e} — raw: {}", json_str.chars().take(200).collect::<String>())))
}

fn judgement_to_verdict(j: WsmJudgement) -> VerificationVerdict {
    let status = match j.status.to_lowercase().as_str() {
        "pass" => VerificationStatus::Pass,
        "partial" => VerificationStatus::Partial,
        "refused" => VerificationStatus::Refused,
        "fail" => VerificationStatus::Fail,
        other => {
            warn!("WSM: unknown status '{}', defaulting to Fail", other);
            VerificationStatus::Fail
        }
    };

    // Clamp confidence into [0,1] but do NOT re-scale or cap — use the
    // model's own calibrated value directly. No 0.6 heuristic ceiling.
    let confidence = j.confidence.clamp(0.0, 1.0);

    let issues = if status == VerificationStatus::Refused {
        vec![VerificationIssue {
            description: j
                .refused_reason
                .clone()
                .unwrap_or_else(|| "WSM refused the action as a no-op".to_string()),
            severity: "warning".to_string(),
            suggestion: Some(
                "Try a different candidate target or action — the current one was a no-op"
                    .to_string(),
            ),
        }]
    } else {
        vec![]
    };

    VerificationVerdict {
        status,
        confidence,
        observations: j.observations,
        next_priority: j.next_priority,
        issues,
        unreachable: false,
        unreachable_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_pass() {
        let raw = "```json\n{\"status\":\"pass\",\"confidence\":0.92,\"observations\":\"File menu opened\"}\n```";
        let j = parse_judgement(raw).unwrap();
        assert_eq!(j.status, "pass");
        assert!((j.confidence - 0.92).abs() < 1e-6);
    }

    #[test]
    fn parses_raw_json_refused() {
        let raw = r#"{"status":"refused","confidence":0.85,"observations":"Clicked wrong swatch","refused_reason":"Green clicked instead of red"}"#;
        let j = parse_judgement(raw).unwrap();
        let v = judgement_to_verdict(j);
        assert_eq!(v.status, VerificationStatus::Refused);
        assert_eq!(v.issues.len(), 1);
        assert!(v.issues[0].description.contains("Green"));
    }

    #[test]
    fn parses_unfenced_preamble() {
        let raw = "Here is my verdict:\n{\"status\":\"fail\",\"confidence\":0.7,\"observations\":\"no change\"}\nend";
        let j = parse_judgement(raw).unwrap();
        assert_eq!(j.status, "fail");
    }

    #[test]
    fn unknown_status_falls_back_to_fail() {
        let j = WsmJudgement {
            status: "weird".into(),
            confidence: 0.5,
            observations: "".into(),
            refused_reason: None,
            next_priority: None,
        };
        let v = judgement_to_verdict(j);
        assert_eq!(v.status, VerificationStatus::Fail);
    }

    #[test]
    fn confidence_is_clamped_not_capped() {
        let j = WsmJudgement {
            status: "pass".into(),
            confidence: 0.97,
            observations: "".into(),
            refused_reason: None,
            next_priority: None,
        };
        let v = judgement_to_verdict(j);
        // Deliberately well above the old 0.6 / 0.9 heuristic caps.
        assert!((v.confidence - 0.97).abs() < 1e-6);

        let j2 = WsmJudgement {
            status: "pass".into(),
            confidence: 1.5,
            observations: "".into(),
            refused_reason: None,
            next_priority: None,
        };
        assert_eq!(judgement_to_verdict(j2).confidence, 1.0);
    }

    #[test]
    fn refused_has_suggestion_to_try_different_candidate() {
        let j = WsmJudgement {
            status: "refused".into(),
            confidence: 0.8,
            observations: "".into(),
            refused_reason: Some("missed target".into()),
            next_priority: None,
        };
        let v = judgement_to_verdict(j);
        assert!(v.issues[0]
            .suggestion
            .as_ref()
            .unwrap()
            .contains("different candidate"));
    }

    #[test]
    fn extract_content_handles_missing_fields() {
        let body = json!({"choices": []});
        assert!(extract_content(&body).is_none());
        let body = json!({"nothing": 1});
        assert!(extract_content(&body).is_none());
    }

    #[test]
    fn extract_content_happy_path() {
        let body = json!({
            "choices": [
                {"message": {"content": "hello"}}
            ]
        });
        assert_eq!(extract_content(&body).as_deref(), Some("hello"));
    }

    #[test]
    fn parse_judgement_rejects_empty() {
        assert!(parse_judgement("").is_err());
        assert!(parse_judgement("no json here at all").is_err());
    }
}
