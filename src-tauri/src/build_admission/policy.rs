//! The broker's level and parameters, by the plan's local precedence (D8),
//! highest first:
//!
//! 1. `QONTINUI_BUILD_ADMISSION=0` in the runner's environment — broker OFF
//!    (the wrappers then use their degraded arm, so the bound never goes away);
//! 2. `~/.qontinui/build-admission/overrides.toml` — read every tick;
//! 3. coord's last desired payload, cached in `policy.json` (written by Phase
//!    7; read here when present);
//! 4. the compiled defaults in `qontinui_types::build_admission::Policy`.
//!
//! A failed or unparseable read never resolves to `off`: an unreadable layer is
//! skipped (and named in `notes`), never treated as "disabled"
//! [`capability-ships-enabled`]. **This build serves `observe` at most:** a
//! requested `enforce` is reported, and served as `observe` until the governor
//! (Phase 4) ships.

use qontinui_types::build_admission::Policy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Off,
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelSource {
    Env,
    Override,
    Coord,
    Default,
}

/// A partial [`Policy`]: each present field overrides the layer below.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    pub psi_admit_max: Option<f64>,
    pub psi_pause: Option<f64>,
    pub pause_sustain_s: Option<u64>,
    pub psi_resume: Option<f64>,
    pub reserve_floor_fraction: Option<f64>,
    pub reserve_floor_min_bytes: Option<u64>,
    pub promote_after_s: Option<u64>,
    pub paused_alert_after_s: Option<u64>,
    pub spawn_defer_wait_s: Option<u64>,
    pub settle_s: Option<u64>,
    pub history_window: Option<u32>,
    pub min_measurements: Option<u32>,
}

impl PolicyPatch {
    fn apply(&self, p: &mut Policy) {
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = self.$f { p.$f = v; } )* };
        }
        set!(
            psi_admit_max,
            psi_pause,
            pause_sustain_s,
            psi_resume,
            reserve_floor_fraction,
            reserve_floor_min_bytes,
            promote_after_s,
            paused_alert_after_s,
            spawn_defer_wait_s,
            settle_s,
            history_window,
            min_measurements
        );
    }
}

/// One layer's file shape (`overrides.toml` and `policy.json` alike).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub level: Option<Level>,
    #[serde(default)]
    pub policy: PolicyPatch,
}

/// The resolved policy.
#[derive(Debug, Clone, Serialize)]
pub struct Resolved {
    pub level_requested: Level,
    /// What this build serves: never above `observe`.
    pub level: Level,
    pub level_source: LevelSource,
    pub policy: Policy,
    pub notes: Vec<String>,
}

/// Resolve the layers. `overrides_toml` / `cached_json` are the files' text
/// (`None` = absent).
pub fn resolve(
    env: Option<&str>,
    overrides_toml: Option<&str>,
    cached_json: Option<&str>,
) -> Resolved {
    let mut notes = Vec::new();
    let mut policy = Policy::default();
    let mut level = Level::Observe;
    let mut source = LevelSource::Default;

    let cached = cached_json.and_then(|t| match serde_json::from_str::<Layer>(t) {
        Ok(l) => Some(l),
        Err(e) => {
            notes.push(format!("policy.json unreadable, skipped: {e}"));
            None
        }
    });
    let overrides = overrides_toml.and_then(|t| match toml::from_str::<Layer>(t) {
        Ok(l) => Some(l),
        Err(e) => {
            notes.push(format!("overrides.toml unreadable, skipped: {e}"));
            None
        }
    });
    for (layer, src) in [
        (cached, LevelSource::Coord),
        (overrides, LevelSource::Override),
    ] {
        let Some(layer) = layer else { continue };
        let mut candidate = policy;
        layer.policy.apply(&mut candidate);
        match candidate.validate() {
            Ok(()) => policy = candidate,
            Err(e) => notes.push(format!(
                "{src:?} policy refused ({e:?}); kept the layer below"
            )),
        }
        if let Some(l) = layer.level {
            level = l;
            source = src;
        }
    }
    if env.map(str::trim) == Some("0") {
        level = Level::Off;
        source = LevelSource::Env;
    }
    let served = match level {
        Level::Enforce => {
            notes.push(
                "enforce requested; this runner build serves observe until the governor ships"
                    .into(),
            );
            Level::Observe
        }
        l => l,
    };
    Resolved {
        level_requested: level,
        level: served,
        level_source: source,
        policy,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_observe() {
        let r = resolve(None, None, None);
        assert_eq!(
            (r.level, r.level_source),
            (Level::Observe, LevelSource::Default)
        );
        assert_eq!(r.policy, Policy::default());
    }

    #[test]
    fn precedence_env_over_override_over_coord() {
        let coord = r#"{"level":"off","policy":{"psi_admit_max":3.0}}"#;
        let over = "level = \"observe\"\n[policy]\nsettle_s = 40\n";
        let r = resolve(None, Some(over), Some(coord));
        assert_eq!(
            (r.level, r.level_source),
            (Level::Observe, LevelSource::Override)
        );
        assert_eq!(r.policy.psi_admit_max, 3.0);
        assert_eq!(r.policy.settle_s, 40);
        let r = resolve(Some("0"), Some(over), Some(coord));
        assert_eq!((r.level, r.level_source), (Level::Off, LevelSource::Env));
        let r = resolve(None, None, Some(coord));
        assert_eq!((r.level, r.level_source), (Level::Off, LevelSource::Coord));
    }

    #[test]
    fn an_unreadable_layer_is_skipped_never_off() {
        let r = resolve(None, Some("level = ["), Some("{nope"));
        assert_eq!(r.level, Level::Observe);
        assert_eq!(r.notes.len(), 2);
        // An unknown key is refused too, not silently dropped.
        let r = resolve(None, Some("levle = \"off\""), None);
        assert_eq!(r.level, Level::Observe);
        assert_eq!(r.notes.len(), 1);
    }

    #[test]
    fn an_invalid_parameter_set_keeps_the_layer_below() {
        let r = resolve(None, Some("[policy]\npsi_resume = 50.0\n"), None);
        assert_eq!(r.policy, Policy::default());
        assert!(r.notes[0].contains("ResumeNotBelowPause"), "{:?}", r.notes);
    }

    #[test]
    fn enforce_is_served_as_observe_in_this_build() {
        let r = resolve(None, Some("level = \"enforce\""), None);
        assert_eq!(
            (r.level_requested, r.level),
            (Level::Enforce, Level::Observe)
        );
        assert!(!r.notes.is_empty());
    }

    #[test]
    fn env_other_than_zero_does_not_disable() {
        assert_eq!(resolve(Some("1"), None, None).level, Level::Observe);
        assert_eq!(resolve(Some(""), None, None).level, Level::Observe);
    }
}
