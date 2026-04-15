//! Live-updatable configuration for the World State Verifier.
//!
//! The config is a process-global `RwLock<WsvConfig>` behind a `OnceLock`,
//! bootstrapped on first access from:
//!   1. Persisted settings (qontinui settings file via `config_facade`)
//!   2. Env-var fallback if the settings file hasn't been initialized yet
//!
//! Updates go through [`set_global`] so changes from the settings UI
//! take effect on the next verify call without a runner restart.
//!
//! Env vars (fallback + bootstrap):
//! - `QONTINUI_WORLD_STATE_VERIFIER`         — mode (disabled | enabled | shadow)
//! - `QONTINUI_WORLD_STATE_VERIFIER_ENDPOINT` — llama-swap base URL
//! - `QONTINUI_WORLD_STATE_VERIFIER_MODEL`    — model name/alias

use std::env;
use std::sync::{OnceLock, RwLock};

pub use crate::settings::WsvMode;

impl WsvMode {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Enabled | Self::Shadow)
    }
}

pub const ENV_MODE: &str = "QONTINUI_WORLD_STATE_VERIFIER";
pub const ENV_ENDPOINT: &str = "QONTINUI_WORLD_STATE_VERIFIER_ENDPOINT";
pub const ENV_MODEL: &str = "QONTINUI_WORLD_STATE_VERIFIER_MODEL";

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8100";
pub const DEFAULT_MODEL: &str = "cua-wsm";

/// Live config for the World State Verifier. Cloned out on read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsvConfig {
    pub mode: WsvMode,
    pub endpoint: String,
    pub model: String,
    pub show_screenshot_evidence: bool,
}

impl Default for WsvConfig {
    fn default() -> Self {
        Self {
            mode: WsvMode::Disabled,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            show_screenshot_evidence: true,
        }
    }
}

impl WsvConfig {
    /// Build a WsvConfig from env vars. Missing/unset vars take defaults.
    /// This is the boot-time fallback — callers that want the *persisted*
    /// settings should call [`WsvConfig::global`] instead.
    pub fn from_env() -> Self {
        let mode = match env::var(ENV_MODE)
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str()
        {
            "enabled" | "1" | "true" | "on" | "yes" => WsvMode::Enabled,
            "shadow" => WsvMode::Shadow,
            _ => WsvMode::Disabled,
        };
        Self {
            mode,
            endpoint: env::var(ENV_ENDPOINT).unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string()),
            model: env::var(ENV_MODEL).unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            show_screenshot_evidence: true,
        }
    }

    /// Merge persisted settings into a config. Used at startup to
    /// materialize persisted settings from the settings file into the
    /// live global.
    pub fn from_settings(settings: &crate::settings::WorldStateVerifierSettings) -> Self {
        Self {
            mode: settings.mode,
            endpoint: settings.endpoint.clone(),
            model: settings.model.clone(),
            show_screenshot_evidence: settings.show_screenshot_evidence,
        }
    }

    /// Returns a clone of the current global config. Initializes from
    /// env vars on first access if [`set_global`] hasn't been called.
    pub fn global() -> Self {
        let lock = WSV_CONFIG.get_or_init(|| RwLock::new(Self::from_env()));
        lock.read()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Replace the global config. Called from the Tauri save command
    /// and from startup once persisted settings have been loaded.
    pub fn set_global(new: Self) {
        let lock = WSV_CONFIG.get_or_init(|| RwLock::new(Self::from_env()));
        match lock.write() {
            Ok(mut guard) => *guard = new,
            Err(poisoned) => *poisoned.into_inner() = new,
        }
    }

    /// Initialize the global from persisted settings at startup. Call
    /// exactly once from `main.rs` before the agentic loop runs. Safe
    /// to call even if the settings file doesn't yet exist — falls
    /// back to defaults + env vars.
    ///
    /// Precedence rule:
    /// - If the user has ever saved WSV settings via the UI
    ///   (`ever_saved == true`), the persisted values win unconditionally.
    ///   This ensures a user who explicitly sets Disabled through the UI
    ///   is not silently overridden by a stale env var.
    /// - Otherwise (first-run, UI never touched), env vars are honored so
    ///   operators who configure WSV purely via environment still get it.
    pub fn init_from_persisted() {
        let persisted = crate::settings::get_world_state_verifier_settings();
        let cfg = if persisted.ever_saved {
            Self::from_settings(&persisted)
        } else {
            // No explicit UI save — use env vars as the authoritative
            // source, but keep the persisted endpoint/model values if
            // they happen to be non-default (they aren't, on a fresh
            // install, but be conservative).
            let mut c = Self::from_env();
            if persisted.endpoint != DEFAULT_ENDPOINT {
                c.endpoint = persisted.endpoint.clone();
            }
            if persisted.model != DEFAULT_MODEL {
                c.model = persisted.model.clone();
            }
            c
        };
        Self::set_global(cfg);
    }
}

static WSV_CONFIG: OnceLock<RwLock<WsvConfig>> = OnceLock::new();

// ── Back-compat shims used by existing call sites ────────────────────────

/// Returns the current WSV mode. Reads from the live global config.
pub fn parse_mode() -> WsvMode {
    WsvConfig::global().mode
}

/// Returns the current WSV endpoint.
pub fn endpoint() -> String {
    WsvConfig::global().endpoint
}

/// Returns the current WSV model alias.
pub fn model_name() -> String {
    WsvConfig::global().model
}

/// Returns whether to show screenshot evidence in canvas panels.
pub fn show_screenshot_evidence() -> bool {
    WsvConfig::global().show_screenshot_evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env vars are process-global, so tests that manipulate them would
    /// race if run in parallel. Serialize through this mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var(key).ok();
        match value {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
    }

    #[test]
    fn from_env_defaults_to_disabled() {
        with_env(ENV_MODE, None, || {
            let cfg = WsvConfig::from_env();
            assert_eq!(cfg.mode, WsvMode::Disabled);
        });
    }

    #[test]
    fn from_env_parses_enabled_variants() {
        for v in [
            "enabled",
            "ENABLED",
            "1",
            "true",
            "on",
            "yes",
            "  Enabled  ",
        ] {
            with_env(ENV_MODE, Some(v), || {
                assert_eq!(WsvConfig::from_env().mode, WsvMode::Enabled, "value={v}");
            });
        }
    }

    #[test]
    fn from_env_parses_shadow() {
        with_env(ENV_MODE, Some("shadow"), || {
            assert_eq!(WsvConfig::from_env().mode, WsvMode::Shadow);
        });
        with_env(ENV_MODE, Some("SHADOW"), || {
            assert_eq!(WsvConfig::from_env().mode, WsvMode::Shadow);
        });
    }

    #[test]
    fn from_env_garbage_values_fall_back_to_disabled() {
        for v in ["", "nope", "off", "0", "false", "shadowy"] {
            with_env(ENV_MODE, Some(v), || {
                assert_eq!(WsvConfig::from_env().mode, WsvMode::Disabled, "value={v}");
            });
        }
    }

    #[test]
    fn is_active_correctness() {
        assert!(!WsvMode::Disabled.is_active());
        assert!(WsvMode::Enabled.is_active());
        assert!(WsvMode::Shadow.is_active());
    }

    #[test]
    fn set_global_is_visible_on_next_read() {
        // Save/restore so we don't trample other tests that may read WSV_CONFIG.
        let prev = WsvConfig::global();
        WsvConfig::set_global(WsvConfig {
            mode: WsvMode::Shadow,
            endpoint: "http://example.test:9999".to_string(),
            model: "test-model".to_string(),
            show_screenshot_evidence: false,
        });
        let read = WsvConfig::global();
        assert_eq!(read.mode, WsvMode::Shadow);
        assert_eq!(read.endpoint, "http://example.test:9999");
        assert_eq!(read.model, "test-model");
        assert!(!read.show_screenshot_evidence);

        assert_eq!(parse_mode(), WsvMode::Shadow);
        assert_eq!(endpoint(), "http://example.test:9999");
        assert_eq!(model_name(), "test-model");
        assert!(!show_screenshot_evidence());

        WsvConfig::set_global(prev);
    }

    #[test]
    fn from_settings_roundtrip() {
        let s = crate::settings::WorldStateVerifierSettings {
            mode: WsvMode::Enabled,
            endpoint: "http://x:1".to_string(),
            model: "m".to_string(),
            show_screenshot_evidence: false,
            ever_saved: true,
        };
        let cfg = WsvConfig::from_settings(&s);
        assert_eq!(cfg.mode, WsvMode::Enabled);
        assert_eq!(cfg.endpoint, "http://x:1");
        assert_eq!(cfg.model, "m");
        assert!(!cfg.show_screenshot_evidence);
    }

    #[test]
    fn default_config_is_disabled_and_defaults() {
        let cfg = WsvConfig::default();
        assert_eq!(cfg.mode, WsvMode::Disabled);
        assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert!(cfg.show_screenshot_evidence);
    }
}
