//! The model and reasoning effort a coord spawn frame asks the session to
//! launch at (plan `2026-11-15-operator-sets-spawn-model-and-effort-per-spawn-type`,
//! Phase 4).
//!
//! Coord decides once per tenant — operator setting, else inheritance from the
//! originating session — and stamps `launch_model`, `launch_effort` and
//! `launch_settings` on the frame. The runner does no classification and no
//! lookup: it validates what the frame carries, applies it to the launch, and
//! logs one line. An invalid value is dropped and logged, never guessed and
//! never repaired, so the session then starts at the launch template / account
//! default exactly as it did before this field existed.

use serde::Deserialize;
use serde_json::Value;

/// The three keys coord stamps on a spawn frame, flattened into each payload
/// shape that can carry them. Absent on a coord that predates the plan, which
/// reads as "the frame said nothing".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FrameLaunchKeys {
    #[serde(default, deserialize_with = "string_or_none")]
    pub launch_model: Option<String>,
    #[serde(default, deserialize_with = "string_or_none")]
    pub launch_effort: Option<String>,
    #[serde(default)]
    pub launch_settings: Option<Value>,
}

/// A non-string value reads as absent (warned later by nothing: it carries no
/// usable choice) so a mistyped key can never fail the whole frame's parse.
fn string_or_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::String(s) => Ok(Some(s)),
        other => {
            if !other.is_null() {
                tracing::warn!(value = %other, "launch key is not a string; ignored");
            }
            Ok(None)
        }
    }
}

impl FrameLaunchKeys {
    /// Validate and label the keys; logs the per-spawn line.
    pub fn resolve(&self) -> FrameLaunch {
        let launch = FrameLaunch::from_frame(
            self.launch_model.as_deref(),
            self.launch_effort.as_deref(),
            self.launch_settings.as_ref(),
        );
        launch.log();
        launch
    }
}

/// The model families coord stores (`--model <family>` is an alias Claude Code
/// resolves to that family's latest model).
pub const MODEL_FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// The reasoning-effort levels `claude --effort` accepts.
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A validated launch choice read off a spawn frame. `Default` is "the frame
/// said nothing", which renders no flag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameLaunch {
    pub model: Option<String>,
    pub effort: Option<String>,
    spawn_type: Option<String>,
    model_source: Option<String>,
    effort_source: Option<String>,
}

impl FrameLaunch {
    /// Validate the three frame keys. `settings` is the `launch_settings`
    /// object (`{spawn_type, model_source, effort_source}`), used only to
    /// label the log line.
    pub fn from_frame(
        model: Option<&str>,
        effort: Option<&str>,
        settings: Option<&Value>,
    ) -> Self {
        let model = accept("launch_model", model, &MODEL_FAMILIES);
        let effort = accept("launch_effort", effort, &EFFORT_LEVELS);
        let label = |key: &str| {
            settings
                .and_then(|s| s.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Self {
            model,
            effort,
            spawn_type: label("spawn_type"),
            model_source: label("model_source"),
            effort_source: label("effort_source"),
        }
    }

    /// True when the frame decided nothing, so the launch is unchanged.
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }

    /// `--model <m> --effort <e>` for the launch shapes that bypass
    /// [`crate::claude_session::launch_spec::LaunchSpec`] (headless children).
    pub fn headless_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(m) = &self.model {
            args.push("--model".to_string());
            args.push(m.clone());
        }
        if let Some(e) = &self.effort {
            args.push("--effort".to_string());
            args.push(e.clone());
        }
        args
    }

    /// The one per-spawn log line. Silent when the frame decided nothing.
    pub fn log(&self) {
        if self.is_empty() {
            return;
        }
        let part = |value: &Option<String>, source: &Option<String>| match value {
            Some(v) => format!("{v}({})", source.as_deref().unwrap_or("unknown")),
            None => "none".to_string(),
        };
        tracing::info!(
            "launch: spawn_type={} model={} effort={}",
            self.spawn_type.as_deref().unwrap_or("unknown"),
            part(&self.model, &self.model_source),
            part(&self.effort, &self.effort_source),
        );
    }
}

fn accept(key: &str, value: Option<&str>, allowed: &[&str]) -> Option<String> {
    let value = value?;
    if allowed.contains(&value) {
        Some(value.to_string())
    } else {
        tracing::warn!(
            key,
            value,
            "spawn frame launch value dropped: not one of {allowed:?}"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_values_pass_and_invalid_ones_are_dropped() {
        let l = FrameLaunch::from_frame(Some("sonnet"), Some("turbo"), None);
        assert_eq!(l.model.as_deref(), Some("sonnet"));
        assert_eq!(l.effort, None);
        let l = FrameLaunch::from_frame(Some("claude-sonnet-5-5"), Some("xhigh"), None);
        assert_eq!(l.model, None);
        assert_eq!(l.effort.as_deref(), Some("xhigh"));
    }

    #[test]
    fn keys_parse_from_a_frame_and_a_bare_frame_is_empty() {
        let k: FrameLaunchKeys = serde_json::from_value(serde_json::json!({
            "launch_model": "sonnet",
            "launch_effort": "bogus",
            "launch_settings": {"spawn_type": "post_merge", "model_source": "operator_setting"}
        }))
        .unwrap();
        let l = k.resolve();
        assert_eq!(l.model.as_deref(), Some("sonnet"));
        assert_eq!(l.effort, None);
        let bare: FrameLaunchKeys = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(bare.resolve().is_empty());
    }

    #[test]
    fn a_non_string_key_does_not_fail_the_frame() {
        let k: FrameLaunchKeys = serde_json::from_value(serde_json::json!({
            "launch_model": 7, "launch_effort": {"x": 1}
        }))
        .unwrap();
        assert!(k.resolve().is_empty());
    }

    #[test]
    fn empty_frame_renders_nothing() {
        let l = FrameLaunch::from_frame(None, None, None);
        assert!(l.is_empty());
        assert!(l.headless_args().is_empty());
    }

    #[test]
    fn headless_args_render_each_flag_once() {
        let l = FrameLaunch::from_frame(Some("opus"), Some("high"), None);
        assert_eq!(l.headless_args(), ["--model", "opus", "--effort", "high"]);
    }
}
