//! Configuration for the in-process Rust Coordinator scheduler.
//!
//! Phase 1b ports the cheap rules A–E from `qontinui-claude-config/.claude/
//! commands/coordinate.md` into Rust. The `rust_scheduler_enabled` flag
//! defaults to `false`: the existing `/coordinate` Claude session keeps
//! owning the lease in production until Phase 2 (LLM callout) lands.
//!
//! The flag is read from the `QONTINUI_COORDINATOR_RUST_SCHEDULER` env
//! variable (`1`/`true` to enable). No `settings.toml`-style settings
//! object exists for the Coordinator yet; the env-var path is the
//! "fall back" the Phase 1b plan calls out explicitly.

/// Knobs for the Rust Coordinator scheduler. All fields have sensible
/// defaults; callers that don't care can use `Default::default()`.
#[derive(Debug, Clone)]
pub struct CoordinatorSchedulerConfig {
    /// Tick interval. Default 5s — matches the wall-clock budget the
    /// `/coordinate` slash command targets.
    pub interval_secs: u64,
    /// Initial delay before the first tick. Default 10s — gives the
    /// runner time to finish bootstrap before the first lease attempt.
    pub initial_delay_secs: u64,
    /// Phase 2 budget. The LLM callout consumed by Phase 2 will increment
    /// against this counter; Phase 1b doesn't read it.
    pub max_llm_calls_per_hour: u32,
    /// Master switch. `false` (the default) leaves the existing
    /// `/coordinate` Claude session in charge.
    pub rust_scheduler_enabled: bool,
    /// Phase 4 auto-trigger: when `true`, the scheduler fires
    /// [`crate::productivity::review::auto_review_in_product`] for each
    /// task it observes in `review` status that doesn't already have a
    /// reviews row. Defaults to `true`; the runtime path additionally
    /// gates on whether [`crate::ai_provider::oneshot::oneshot_for_settings`]
    /// returns a non-disabled provider so the loop can't spam the
    /// dashboard with stub rows when no LLM is configured.
    pub auto_review_enabled: bool,
}

impl Default for CoordinatorSchedulerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 5,
            initial_delay_secs: 10,
            max_llm_calls_per_hour: 10,
            rust_scheduler_enabled: false,
            auto_review_enabled: true,
        }
    }
}

impl CoordinatorSchedulerConfig {
    /// Read configuration from environment variables, falling back to the
    /// `Default` values for anything unset. The only env var Phase 1b
    /// actually consults is `QONTINUI_COORDINATOR_RUST_SCHEDULER`; the
    /// other fields are settable from env to make staging easier.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("QONTINUI_COORDINATOR_RUST_SCHEDULER") {
            cfg.rust_scheduler_enabled = parse_bool_flag(&v);
        }
        if let Ok(v) = std::env::var("QONTINUI_COORDINATOR_INTERVAL_SECS") {
            if let Ok(n) = v.parse::<u64>() {
                if n > 0 {
                    cfg.interval_secs = n;
                }
            }
        }
        if let Ok(v) = std::env::var("QONTINUI_COORDINATOR_INITIAL_DELAY_SECS") {
            if let Ok(n) = v.parse::<u64>() {
                cfg.initial_delay_secs = n;
            }
        }
        if let Ok(v) = std::env::var("QONTINUI_COORDINATOR_MAX_LLM_CALLS_PER_HOUR") {
            if let Ok(n) = v.parse::<u32>() {
                cfg.max_llm_calls_per_hour = n;
            }
        }
        if let Ok(v) = std::env::var("QONTINUI_COORDINATOR_AUTO_REVIEW_ENABLED") {
            cfg.auto_review_enabled = parse_bool_flag(&v);
        }

        cfg
    }
}

fn parse_bool_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = CoordinatorSchedulerConfig::default();
        assert!(!cfg.rust_scheduler_enabled);
        assert_eq!(cfg.interval_secs, 5);
        assert_eq!(cfg.initial_delay_secs, 10);
        assert_eq!(cfg.max_llm_calls_per_hour, 10);
        // auto_review defaults on so completed tasks get a verdict; it
        // self-disables at runtime when the active LLM provider has no
        // credentials.
        assert!(cfg.auto_review_enabled);
    }

    #[test]
    fn parses_truthy_flag_values() {
        for v in ["1", "true", "TRUE", "yes", " on "] {
            assert!(parse_bool_flag(v), "expected {:?} to parse truthy", v);
        }
    }

    #[test]
    fn parses_falsy_flag_values() {
        for v in ["0", "false", "no", "off", "", "   "] {
            assert!(!parse_bool_flag(v), "expected {:?} to parse falsy", v);
        }
    }
}
