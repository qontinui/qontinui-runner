//! The launch decision coord stamps on a spawn frame, and how this runner
//! applies it.
//!
//! Plan `2026-11-15-operator-sets-spawn-model-and-effort-per-spawn-type`
//! (D6, vet corrections V5/V7/V8/V14). coord resolves, once per tenant, which
//! model family and reasoning effort a spawn launches at — the operator's
//! per-spawn-type setting, else the continuation's own ask, else the
//! originating session's last reported values — and stamps three keys on the
//! frame: `launch_model`, `launch_effort` and `launch_settings`
//! (`{spawn_type, model_source, effort_source}`). Each key is omitted when
//! nothing resolved, so a frame coord decided nothing for is byte-identical to
//! the pre-field wire.
//!
//! This runner does NO classification and NO lookup: it reads the three keys,
//! drops (and logs) any value outside the two closed vocabularies — never
//! guessing a replacement — and binds what survives into the launch. The wire
//! owner is coord's `spawn_launch_settings::LaunchDecision`; the vocabularies
//! below mirror its `MODEL_FAMILIES` and `continuation_effort::EFFORT_LEVELS`
//! and are kept in lockstep by hand, like the rest of this module's wire
//! shapes.
//!
//! Where each field reaches the `claude` argv:
//!
//! - **Headless seams** (`run_continuation_headless`, `run_agent_subprocess`):
//!   both `--model` and `--effort`, appended in `headless_agent_args`.
//! - **Terminal seams** (`run_continuation_terminal`,
//!   `run_condition_check_terminal`): the model through `LaunchSpec.model` and
//!   the effort through `LaunchSpec.effort`; each beats the same template flag
//!   (`--model` / `--effort`, either spelling).

use serde::Deserialize;

/// Model families a frame may name. Mirror of coord
/// `spawn_launch_settings::MODEL_FAMILIES`. A family alias, resolved by Claude
/// Code to that family's latest model, so no model version is pinned here.
pub(crate) const MODEL_FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Reasoning-effort levels a frame may name. Mirror of coord
/// `gates::continuation_effort::EFFORT_LEVELS` (Claude Code `--effort`).
pub(crate) const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The `launch_settings` frame object: which spawn type coord classified the
/// dispatch as, and where each resolved field came from. Informational only —
/// it is logged, never acted on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct LaunchSettingsRecord {
    #[serde(default, deserialize_with = "lenient_string")]
    pub spawn_type: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub model_source: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub effort_source: Option<String>,
}

/// The three launch keys as they arrived on the frame, before validation.
///
/// `#[serde(flatten)]`-ed onto every spawn payload this runner parses
/// (`LaunchPayload`, `GateContinuationPayload`, `ConditionCheckPayload`).
/// Every field is lenient: a key of the wrong JSON type is kept as its raw
/// JSON text so [`LaunchFrame::resolve`] can drop it with a warning naming it,
/// and it never fails the frame — a launch hint the runner cannot read is not
/// a reason to drop the whole spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct LaunchFrame {
    #[serde(default, deserialize_with = "lenient_string")]
    pub launch_model: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub launch_effort: Option<String>,
    #[serde(default, deserialize_with = "lenient_record")]
    pub launch_settings: Option<LaunchSettingsRecord>,
}

/// A string-valued key, read leniently: a JSON string is kept as-is, `null` is
/// absent, and any other JSON value is kept as its raw text (which no
/// vocabulary matches, so validation drops it loudly).
fn lenient_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    })
}

fn lenient_record<'de, D>(d: D) -> Result<Option<LaunchSettingsRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    if value.is_null() {
        return Ok(None);
    }
    match serde_json::from_value::<LaunchSettingsRecord>(value) {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            tracing::warn!("launch: `launch_settings` key is unreadable ({e}); ignoring it");
            Ok(None)
        }
    }
}

/// Which kind of launch seam is applying the decision. Decides whether the
/// effort reaches the argv (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchSeam {
    /// A headless child: `--model` and `--effort` both appended.
    Headless,
    /// A `LaunchSpec`-rendered terminal argv: model and effort both bound.
    Terminal,
}

impl LaunchSeam {
    fn as_str(self) -> &'static str {
        match self {
            LaunchSeam::Headless => "headless",
            LaunchSeam::Terminal => "terminal",
        }
    }
}

/// The validated decision: only values inside the closed vocabularies survive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AppliedLaunch {
    /// A model family from [`MODEL_FAMILIES`], or `None` (start at the launch
    /// template / account default, exactly as before this field existed).
    pub model: Option<&'static str>,
    /// An effort level from [`EFFORT_LEVELS`], or `None` (account default).
    pub effort: Option<&'static str>,
    /// coord's classified spawn type, sanitised for logging.
    pub spawn_type: Option<String>,
    /// Where `model` came from, sanitised for logging.
    pub model_source: Option<String>,
    /// Where `effort` came from, sanitised for logging.
    pub effort_source: Option<String>,
    /// Every key that arrived with a value outside its vocabulary, as
    /// `key=<raw>` — dropped, never replaced with a guess.
    pub dropped: Vec<String>,
}

/// Match `raw` EXACTLY against a closed vocabulary — the same exact-match test
/// coord's own validators apply, so the runner accepts nothing its wire owner
/// would refuse (no trimming, no case folding: either would be a guess).
fn canonical(raw: &str, vocab: &[&'static str]) -> Option<&'static str> {
    vocab.iter().copied().find(|v| *v == raw)
}

/// A log-safe token: coord's spawn types and sources are `[a-z0-9_]`, so
/// anything else is replaced rather than echoed (no log-line injection).
fn log_token(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let ok = raw.len() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    Some(if ok {
        raw.to_string()
    } else {
        "invalid".to_string()
    })
}

/// A bounded, quoted rendering of a dropped raw value for the warning line.
fn quoted_raw(raw: &str) -> String {
    let clipped: String = raw.chars().take(64).collect();
    format!("{clipped:?}")
}

impl LaunchFrame {
    /// Validate the frame's keys into the decision this runner will apply.
    pub(crate) fn resolve(&self) -> AppliedLaunch {
        let mut dropped = Vec::new();
        let model = self.launch_model.as_deref().and_then(|raw| {
            let hit = canonical(raw, &MODEL_FAMILIES);
            if hit.is_none() {
                dropped.push(format!("launch_model={}", quoted_raw(raw)));
            }
            hit
        });
        let effort = self.launch_effort.as_deref().and_then(|raw| {
            let hit = canonical(raw, &EFFORT_LEVELS);
            if hit.is_none() {
                dropped.push(format!("launch_effort={}", quoted_raw(raw)));
            }
            hit
        });
        let rec = self.launch_settings.as_ref();
        AppliedLaunch {
            model,
            effort,
            spawn_type: log_token(rec.and_then(|r| r.spawn_type.as_deref())),
            model_source: model.and(log_token(rec.and_then(|r| r.model_source.as_deref()))),
            effort_source: effort.and(log_token(rec.and_then(|r| r.effort_source.as_deref()))),
            dropped,
        }
    }
}

impl AppliedLaunch {
    /// The `claude` flags a HEADLESS child gets: `--model <family>` and
    /// `--effort <level>`, each only when resolved. Empty when coord decided
    /// nothing, so such a launch's argv is unchanged.
    pub(crate) fn headless_args(&self) -> Vec<String> {
        let mut args = Vec::with_capacity(4);
        if let Some(m) = self.model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }
        if let Some(e) = self.effort {
            args.push("--effort".to_string());
            args.push(e.to_string());
        }
        args
    }

    /// The model a `LaunchSpec`-rendered (terminal) argv binds into
    /// `LaunchSpec.model`.
    pub(crate) fn spec_model(&self) -> Option<String> {
        self.model.map(str::to_string)
    }

    /// The effort a `LaunchSpec`-rendered (terminal) argv binds into
    /// `LaunchSpec.effort`.
    pub(crate) fn spec_effort(&self) -> Option<String> {
        self.effort.map(str::to_string)
    }

    /// The one per-spawn line (D6), in the plan's grammar
    /// `launch: spawn_type=<t> model=<m|none>(<source>) effort=<e|none>(<source>)`
    /// — an unresolved field reads `none(unresolved)`, a resolved one whose
    /// frame named no source `<v>(unknown)` — followed by ` seam=<s>`.
    pub(crate) fn log_line(&self, seam: LaunchSeam) -> String {
        let field = |v: Option<&str>, src: Option<&str>| match v {
            Some(v) => format!("{v}({})", src.unwrap_or("unknown")),
            None => "none(unresolved)".to_string(),
        };
        let line = format!(
            "launch: spawn_type={} model={} effort={} seam={}",
            self.spawn_type.as_deref().unwrap_or("none"),
            field(self.model, self.model_source.as_deref()),
            field(self.effort, self.effort_source.as_deref()),
            seam.as_str(),
        );
        line
    }

    /// Emit the per-spawn line, and one warning per dropped key.
    pub(crate) fn log(&self, seam: LaunchSeam, context: &str) {
        for d in &self.dropped {
            tracing::warn!(
                "launch: {context}: dropped {d} — not in the runner's vocabulary; the field \
                 starts at its default rather than a guess"
            );
        }
        tracing::info!("{} ({context})", self.log_line(seam));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(v: serde_json::Value) -> LaunchFrame {
        serde_json::from_value(v).expect("lenient frame never fails")
    }

    #[test]
    fn absent_keys_resolve_to_nothing_and_add_no_flags() {
        let applied = frame(json!({})).resolve();
        assert_eq!(applied, AppliedLaunch::default());
        assert!(applied.headless_args().is_empty());
        assert_eq!(applied.spec_model(), None);
        assert_eq!(
            applied.log_line(LaunchSeam::Headless),
            "launch: spawn_type=none model=none(unresolved) effort=none(unresolved) seam=headless"
        );
    }

    #[test]
    fn valid_keys_bind_with_their_sources() {
        let applied = frame(json!({
            "launch_model": "sonnet",
            "launch_effort": "xhigh",
            "launch_settings": {
                "spawn_type": "post_merge",
                "model_source": "operator_setting",
                "effort_source": "originating_session"
            }
        }))
        .resolve();
        assert_eq!(applied.model, Some("sonnet"));
        assert_eq!(applied.effort, Some("xhigh"));
        assert!(applied.dropped.is_empty());
        assert_eq!(
            applied.headless_args(),
            vec!["--model", "sonnet", "--effort", "xhigh"]
        );
        assert_eq!(
            applied.log_line(LaunchSeam::Headless),
            "launch: spawn_type=post_merge model=sonnet(operator_setting) \
             effort=xhigh(originating_session) seam=headless"
        );
    }

    #[test]
    fn every_vocabulary_value_is_accepted() {
        for m in MODEL_FAMILIES {
            assert_eq!(frame(json!({"launch_model": m})).resolve().model, Some(m));
        }
        for e in EFFORT_LEVELS {
            assert_eq!(frame(json!({"launch_effort": e})).resolve().effort, Some(e));
        }
    }

    #[test]
    fn a_near_miss_spelling_is_dropped_like_coord_would_refuse_it() {
        let applied = frame(json!({"launch_model": " Opus ", "launch_effort": "HIGH"})).resolve();
        assert_eq!(applied.model, None);
        assert_eq!(applied.effort, None);
        assert_eq!(applied.dropped.len(), 2);
    }

    #[test]
    fn invalid_values_are_dropped_never_guessed() {
        let applied = frame(json!({
            "launch_model": "claude-sonnet-5-5",
            "launch_effort": "ultracode",
        }))
        .resolve();
        assert_eq!(applied.model, None);
        assert_eq!(applied.effort, None);
        assert!(applied.headless_args().is_empty());
        assert_eq!(
            applied.dropped,
            vec![
                "launch_model=\"claude-sonnet-5-5\"".to_string(),
                "launch_effort=\"ultracode\"".to_string()
            ]
        );
    }

    #[test]
    fn a_wrong_json_type_is_dropped_without_failing_the_frame() {
        let applied = frame(json!({
            "launch_model": 7,
            "launch_effort": {"level": "high"},
            "launch_settings": "post_merge",
        }))
        .resolve();
        assert_eq!(applied.model, None);
        assert_eq!(applied.effort, None);
        assert_eq!(applied.spawn_type, None);
        assert_eq!(applied.dropped.len(), 2);
    }

    #[test]
    fn a_source_is_logged_only_for_a_field_that_survived() {
        let applied = frame(json!({
            "launch_model": "gpt",
            "launch_settings": {"spawn_type": "post_merge", "model_source": "operator_setting"}
        }))
        .resolve();
        assert_eq!(applied.model_source, None);
        assert_eq!(applied.spawn_type.as_deref(), Some("post_merge"));
    }

    #[test]
    fn log_tokens_never_echo_unsafe_text() {
        let applied = frame(json!({
            "launch_model": "opus",
            "launch_settings": {"spawn_type": "x\nlaunch: forged", "model_source": "a b"}
        }))
        .resolve();
        assert_eq!(applied.spawn_type.as_deref(), Some("invalid"));
        assert_eq!(applied.model_source.as_deref(), Some("invalid"));
    }

    #[test]
    fn a_terminal_seam_binds_model_and_effort_into_the_spec() {
        let applied = frame(json!({"launch_model": "haiku", "launch_effort": "low"})).resolve();
        assert_eq!(applied.spec_model().as_deref(), Some("haiku"));
        assert_eq!(applied.spec_effort().as_deref(), Some("low"));
        assert_eq!(
            applied.log_line(LaunchSeam::Terminal),
            "launch: spawn_type=none model=haiku(unknown) effort=low(unknown) seam=terminal"
        );
        assert_eq!(frame(json!({})).resolve().spec_effort(), None);
    }
}
