//! The model and reasoning effort coord decided for a spawn, read off the frame.
//!
//! Plan `2026-11-15-operator-sets-spawn-model-and-effort-per-spawn-type`
//! (D6, V7, V8, V14). Coord resolves, once per tenant, which model family and
//! effort level a spawn of a given type launches at (the operator's per-type
//! setting, else the continuation's own key, else the originating session's) and
//! stamps the answer on the frame as three optional keys:
//!
//! - `launch_model` — a model family: `fable` | `opus` | `sonnet` | `haiku`,
//!   rendered as `claude --model <family>` (Claude Code resolves the alias to
//!   that family's latest model).
//! - `launch_effort` — `low` | `medium` | `high` | `xhigh` | `max`, rendered as
//!   `claude --effort <level>`.
//! - `launch_settings` — `{spawn_type, model_source, effort_source}`, provenance
//!   only, used for the one launch log line.
//!
//! The runner does NO classification and no lookup; it applies what the frame
//! says and nothing else. Each key is lenient: an absent or `null` key is
//! "unresolved" (the launch is exactly as before), and a value outside the
//! vocabulary is DROPPED with a `warn!` — never guessed, never mapped to a
//! neighbouring value, and never a reason to fail the frame.
//!
//! These keys are Claude Code launch flags. A spawn that runs under a non-Claude
//! harness (an ACP agent) must not receive them; see
//! [`LaunchChoice::for_harness`].

use serde::{Deserialize, Deserializer};
use tracing::{info, warn};

/// The model families coord may stamp. Mirrors coord's vocabulary verbatim.
pub(crate) const MODEL_FAMILIES: &[&str] = &["fable", "opus", "sonnet", "haiku"];

/// The effort levels coord may stamp. Mirrors coord's vocabulary verbatim.
pub(crate) const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Longest provenance token (`spawn_type`, `*_source`) carried into a log line.
const PROVENANCE_MAX_BYTES: usize = 64;

/// The `launch_settings` frame key: where coord's decision came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LaunchSettingsMeta {
    #[serde(default, deserialize_with = "deserialize_provenance_token")]
    pub spawn_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_provenance_token")]
    pub model_source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_provenance_token")]
    pub effort_source: Option<String>,
}

/// The validated launch decision for one spawn. `Default` = nothing resolved,
/// which renders no flag at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchChoice {
    /// A member of [`MODEL_FAMILIES`], or `None`.
    pub model: Option<String>,
    /// A member of [`EFFORT_LEVELS`], or `None`.
    pub effort: Option<String>,
    /// Provenance, for the log line only.
    pub settings: Option<LaunchSettingsMeta>,
}

impl LaunchChoice {
    /// Assemble from a frame's three (already validated) fields.
    pub fn from_frame(
        model: Option<&String>,
        effort: Option<&String>,
        settings: Option<&LaunchSettingsMeta>,
    ) -> Self {
        Self {
            model: model.cloned(),
            effort: effort.cloned(),
            settings: settings.cloned(),
        }
    }

    /// The choice as it applies to a spawn under the given harness.
    ///
    /// `claude_harness == false` (the spawn runs an ACP agent, not Claude Code)
    /// returns the empty choice: `--model <family>` and `--effort <level>` are
    /// Claude Code flags and mean nothing — or something else — to another
    /// agent. Coord does not stamp the keys on a harness continuation either
    /// (V14); this is the runner half of the same rule.
    ///
    /// This build has no non-Claude harness, so every call site passes `true`.
    /// The ACP lane (qontinui-runner#2958) adds `harness` to the frame; its
    /// Claude/ACP branch is where `payload.harness.is_none()` belongs.
    pub fn for_harness(self, claude_harness: bool) -> Self {
        if claude_harness {
            self
        } else {
            if self.model.is_some() || self.effort.is_some() {
                info!("launch: model/effort keys ignored — the spawn runs a non-Claude harness");
            }
            Self::default()
        }
    }

    /// The single per-spawn launch log line:
    /// `launch: spawn_type=<t> model=<m|none>(<source>) effort=<e|none>(<source>)`.
    pub fn log_line(&self, fallback_spawn_type: &str) -> String {
        let meta = self.settings.as_ref();
        let spawn_type = meta
            .and_then(|m| m.spawn_type.as_deref())
            .unwrap_or(fallback_spawn_type);
        let field = |value: Option<&str>, source: Option<&str>| match value {
            Some(v) => format!("{v}({})", source.unwrap_or("unspecified")),
            None => "none(unresolved)".to_string(),
        };
        format!(
            "launch: spawn_type={spawn_type} model={} effort={}",
            field(
                self.model.as_deref(),
                meta.and_then(|m| m.model_source.as_deref())
            ),
            field(
                self.effort.as_deref(),
                meta.and_then(|m| m.effort_source.as_deref())
            ),
        )
    }

    /// Emit [`Self::log_line`] at `info`.
    pub fn log(&self, fallback_spawn_type: &str) {
        info!("{}", self.log_line(fallback_spawn_type));
    }

    /// Apply the choice to a headless `claude` argv (`spawn_claude_child`'s
    /// `extra_args`): any existing `--model`/`--effort` (pair or attached
    /// `--flag=value` form) is REPLACED, never duplicated, by the frame's value.
    /// A field the frame does not set leaves the argv's own flag alone, so an
    /// empty choice returns `args` unchanged.
    pub fn apply_to_args(&self, args: Vec<String>) -> Vec<String> {
        let mut out = args;
        if let Some(model) = &self.model {
            out = replace_flag(out, "--model", model);
        }
        if let Some(effort) = &self.effort {
            out = replace_flag(out, "--effort", effort);
        }
        out
    }
}

/// Remove every `flag <v>` / `flag=<v>` from `args`, then append `flag value`.
fn replace_flag(args: Vec<String>, flag: &str, value: &str) -> Vec<String> {
    let attached = format!("{flag}=");
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == flag {
            // Drop the flag and its value (when one follows).
            let _ = it.next();
        } else if !a.starts_with(&attached) {
            out.push(a);
        }
    }
    out.push(flag.to_string());
    out.push(value.to_string());
    out
}

/// Validate one vocabulary-bound frame key. `null`/absent → `None` silently; a
/// value outside `allowed` → `None` with a `warn!` naming the key.
fn validated(key: &str, value: serde_json::Value, allowed: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if allowed.contains(&s.as_str()) => Some(s),
        other => {
            let shown: String = other.to_string().chars().take(64).collect();
            warn!(
                "launch: frame key `{key}` = {shown} is outside {allowed:?} — dropped, the \
                 launch keeps its template/account default (never a guessed value)"
            );
            None
        }
    }
}

/// `deserialize_with` for `launch_model`.
pub(crate) fn deserialize_launch_model<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(validated("launch_model", value, MODEL_FAMILIES))
}

/// `deserialize_with` for `launch_effort`.
pub(crate) fn deserialize_launch_effort<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(validated("launch_effort", value, EFFORT_LEVELS))
}

/// `deserialize_with` for `launch_settings`: an unreadable shape is `None` with a
/// `warn!`, never a failed frame.
pub(crate) fn deserialize_launch_settings<'de, D>(
    d: D,
) -> Result<Option<LaunchSettingsMeta>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    if value.is_null() {
        return Ok(None);
    }
    match serde_json::from_value::<LaunchSettingsMeta>(value) {
        Ok(m) => Ok(Some(m)),
        Err(e) => {
            warn!("launch: frame key `launch_settings` is unreadable ({e}) — provenance dropped");
            Ok(None)
        }
    }
}

/// A provenance token is logged, so it is bounded to `[a-z0-9_]{1,64}`; anything
/// else (including a non-string) reads as absent.
fn deserialize_provenance_token<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().and_then(|s| {
        let ok = !s.is_empty()
            && s.len() <= PROVENANCE_MAX_BYTES
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        ok.then(|| s.to_string())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Frame {
        #[serde(default, deserialize_with = "deserialize_launch_model")]
        launch_model: Option<String>,
        #[serde(default, deserialize_with = "deserialize_launch_effort")]
        launch_effort: Option<String>,
        #[serde(default, deserialize_with = "deserialize_launch_settings")]
        launch_settings: Option<LaunchSettingsMeta>,
    }

    fn choice(v: serde_json::Value) -> LaunchChoice {
        let f: Frame = serde_json::from_value(v).expect("a launch frame never fails to parse");
        LaunchChoice::from_frame(
            f.launch_model.as_ref(),
            f.launch_effort.as_ref(),
            f.launch_settings.as_ref(),
        )
    }

    #[test]
    fn every_vocabulary_value_is_accepted() {
        for m in MODEL_FAMILIES {
            assert_eq!(
                choice(serde_json::json!({"launch_model": m}))
                    .model
                    .as_deref(),
                Some(*m)
            );
        }
        for e in EFFORT_LEVELS {
            assert_eq!(
                choice(serde_json::json!({"launch_effort": e}))
                    .effort
                    .as_deref(),
                Some(*e)
            );
        }
    }

    #[test]
    fn invalid_values_are_dropped_not_guessed() {
        let c = choice(serde_json::json!({
            "launch_model": "claude-sonnet-5-5",
            "launch_effort": "HIGH",
            "launch_settings": 7,
        }));
        assert_eq!(c, LaunchChoice::default());
        let c = choice(serde_json::json!({"launch_model": 3, "launch_effort": ["max"]}));
        assert_eq!(c, LaunchChoice::default());
    }

    #[test]
    fn absent_and_null_are_unresolved() {
        assert_eq!(choice(serde_json::json!({})), LaunchChoice::default());
        assert_eq!(
            choice(
                serde_json::json!({"launch_model": null, "launch_effort": null, "launch_settings": null})
            ),
            LaunchChoice::default()
        );
    }

    #[test]
    fn provenance_tokens_are_bounded() {
        let c = choice(serde_json::json!({"launch_settings": {
            "spawn_type": "post_merge",
            "model_source": "operator_setting\nINJECTED",
            "effort_source": "originating_session",
        }}));
        let m = c.settings.expect("settings readable");
        assert_eq!(m.spawn_type.as_deref(), Some("post_merge"));
        assert_eq!(m.model_source, None);
        assert_eq!(m.effort_source.as_deref(), Some("originating_session"));
    }

    #[test]
    fn log_line_shape() {
        let c = choice(serde_json::json!({
            "launch_model": "sonnet",
            "launch_settings": {"spawn_type": "post_merge", "model_source": "operator_setting"},
        }));
        assert_eq!(
            c.log_line("gate_continuation"),
            "launch: spawn_type=post_merge model=sonnet(operator_setting) effort=none(unresolved)"
        );
        assert_eq!(
            LaunchChoice::default().log_line("agent_spawn"),
            "launch: spawn_type=agent_spawn model=none(unresolved) effort=none(unresolved)"
        );
    }

    #[test]
    fn apply_replaces_never_duplicates() {
        let c = choice(serde_json::json!({"launch_model": "sonnet", "launch_effort": "high"}));
        let args: Vec<String> = ["--model", "opus", "--effort=low", "--session-id", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = c.apply_to_args(args);
        assert_eq!(
            out,
            ["--session-id", "x", "--model", "sonnet", "--effort", "high"]
        );
    }

    #[test]
    fn empty_choice_leaves_args_identical() {
        let args: Vec<String> = ["--model", "opus", "--session-id", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(LaunchChoice::default().apply_to_args(args.clone()), args);
    }

    #[test]
    fn a_non_claude_harness_gets_nothing() {
        let c = choice(serde_json::json!({"launch_model": "haiku", "launch_effort": "max"}));
        assert_eq!(c.clone().for_harness(true), c);
        assert_eq!(c.for_harness(false), LaunchChoice::default());
    }
}
