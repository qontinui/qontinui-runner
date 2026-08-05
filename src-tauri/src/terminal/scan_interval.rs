//! Shared cadence resolution for the full-fleet grid scanners.
//!
//! Plan `2026-07-28-runner-many-sessions-performance` Phase 8 promoted the
//! two 1.5 s grid-scan loops (`auto_response`, `usage_limit`) from
//! env-var-only tuning to a real setting. Both scanners walk every live
//! session's grid on every tick, so their cadence is the single knob that
//! most directly trades detection latency for per-session background cost —
//! which is exactly why it belongs in `settings.json` rather than only in an
//! env var an operator has to remember at launch time.
//!
//! **Precedence, highest first:**
//!
//! 1. the scanner's own env var — unchanged from before this phase, and
//!    still the escape hatch that beats persisted config (it is what a
//!    harness run or a one-off debug launch sets, and a stored setting must
//!    not silently override the launch that was just typed);
//! 2. `settings.performance.grid_scan_interval_ms`;
//! 3. the historical hardcoded 1500 ms, which is that setting's default —
//!    so an untouched install resolves to exactly the old value.
//!
//! The 200 ms floor ([`crate::settings::MIN_GRID_SCAN_INTERVAL_MS`]) applies
//! to **both** sources. Before this phase the env var enforced the floor by
//! *discarding* an under-floor value (`.filter(|&n| n >= 200)`), silently
//! falling back to 1500 ms — i.e. asking for 50 ms got you 1500 ms, the
//! opposite of the intent. Clamping instead gives the operator the fastest
//! cadence the floor permits, which is what "floor" means. An unparseable
//! env var still falls through to the setting.

use std::time::Duration;

use tracing::warn;

use crate::settings::MIN_GRID_SCAN_INTERVAL_MS;

/// Resolve a grid scanner's tick interval from its env override and the
/// persisted setting. Pure — the env value is passed in so this is testable
/// without mutating process environment (which races across `cargo test`'s
/// threads).
///
/// Returns the resolved interval and whether the floor had to clamp it, so
/// the caller can say so out loud: an operator whose env var previously meant
/// "fall back to 1500 ms" (the old `filter`) now gets 200 ms instead, which
/// is a 7.5× busier sweep and must not be silent.
pub fn resolve_scan_interval_clamped(
    env_value: Option<&str>,
    configured_ms: u64,
) -> (Duration, bool) {
    let requested = env_value
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(configured_ms);
    let ms = requested.max(MIN_GRID_SCAN_INTERVAL_MS);
    (Duration::from_millis(ms), ms != requested)
}

/// Read `env_var` and the persisted cadence, then resolve. The thin
/// I/O-bearing wrapper each scanner calls once when its loop is spawned.
///
/// The setting arm is floored by the settings type
/// ([`crate::settings::PerformanceSettings::effective_grid_scan_interval_ms`]),
/// the env arm by [`resolve_scan_interval_clamped`] — so the floor holds for
/// both sources while each stays the authority over its own value.
pub fn scan_interval_from_env(env_var: &str) -> Duration {
    let env_value = std::env::var(env_var).ok();
    let configured = crate::settings::get_performance_settings().effective_grid_scan_interval_ms();
    let (interval, clamped) = resolve_scan_interval_clamped(env_value.as_deref(), configured);
    if clamped {
        warn!(
            "{env_var}/settings.performance.grid_scan_interval_ms asked for a cadence below the \
             {MIN_GRID_SCAN_INTERVAL_MS}ms floor; clamping to {}ms. (Before this became a \
             setting, an under-floor env value was DISCARDED and the scanner ran at 1500ms — if \
             you were relying on that, set the value you actually want.)",
            interval.as_millis()
        );
    }
    interval
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resolved interval without the clamp flag — the shape most of these
    /// assertions care about. Test-local so the production surface stays the
    /// single `resolve_scan_interval_clamped` entry point.
    fn resolve_scan_interval(env_value: Option<&str>, configured_ms: u64) -> Duration {
        resolve_scan_interval_clamped(env_value, configured_ms).0
    }

    /// The defaults reproduce the pre-Phase-8 behavior exactly: no env var,
    /// stock setting → the historical 1500 ms.
    #[test]
    fn defaults_match_historical_1500ms() {
        let stock = crate::settings::PerformanceSettings::default();
        assert_eq!(stock.grid_scan_interval_ms, 1500);
        assert_eq!(
            resolve_scan_interval(None, stock.grid_scan_interval_ms),
            Duration::from_millis(1500)
        );
    }

    /// The setting is honored when no env var is set.
    #[test]
    fn setting_is_honored() {
        assert_eq!(
            resolve_scan_interval(None, 3000),
            Duration::from_millis(3000)
        );
    }

    /// The env var is the higher-precedence escape hatch — it beats the
    /// setting in both directions (faster and slower).
    #[test]
    fn env_override_beats_the_setting() {
        assert_eq!(
            resolve_scan_interval(Some("400"), 3000),
            Duration::from_millis(400)
        );
        assert_eq!(
            resolve_scan_interval(Some("9000"), 500),
            Duration::from_millis(9000)
        );
    }

    /// The 200 ms floor holds for the env var and for the setting alike,
    /// including 0 (which would otherwise spin the scanner).
    #[test]
    fn floor_holds_for_both_sources() {
        assert_eq!(
            resolve_scan_interval(Some("50"), 3000),
            Duration::from_millis(MIN_GRID_SCAN_INTERVAL_MS)
        );
        assert_eq!(
            resolve_scan_interval(None, 10),
            Duration::from_millis(MIN_GRID_SCAN_INTERVAL_MS)
        );
        assert_eq!(
            resolve_scan_interval(Some("0"), 1500),
            Duration::from_millis(MIN_GRID_SCAN_INTERVAL_MS)
        );
        assert_eq!(
            resolve_scan_interval(None, 0),
            Duration::from_millis(MIN_GRID_SCAN_INTERVAL_MS)
        );
    }

    /// Junk in the env var falls through to the setting rather than to the
    /// compiled-in default — an unreadable override must not also discard
    /// the operator's persisted choice.
    #[test]
    fn unparseable_env_falls_through_to_the_setting() {
        assert_eq!(
            resolve_scan_interval(Some("not-a-number"), 2500),
            Duration::from_millis(2500)
        );
        assert_eq!(
            resolve_scan_interval(Some(""), 2500),
            Duration::from_millis(2500)
        );
    }

    /// The clamp flag is what the caller logs on. It must fire exactly when
    /// the floor moved the number, and never on a value at or above it —
    /// otherwise the upgrade warning becomes noise operators learn to ignore.
    #[test]
    fn clamp_flag_reports_exactly_when_the_floor_moved_the_value() {
        assert!(!resolve_scan_interval_clamped(None, 1500).1);
        assert!(!resolve_scan_interval_clamped(None, MIN_GRID_SCAN_INTERVAL_MS).1);
        assert!(!resolve_scan_interval_clamped(Some("200"), 1500).1);
        assert!(resolve_scan_interval_clamped(Some("0"), 1500).1);
        assert!(resolve_scan_interval_clamped(Some("199"), 1500).1);
        assert!(resolve_scan_interval_clamped(None, 5).1);
        // Junk falls through to the setting, so the flag reflects the
        // SETTING's relation to the floor, not the junk's.
        assert!(!resolve_scan_interval_clamped(Some("junk"), 1500).1);
        assert!(resolve_scan_interval_clamped(Some("junk"), 5).1);
    }

    /// Surrounding whitespace (a shell-quoting artifact) is tolerated.
    #[test]
    fn whitespace_in_env_is_tolerated() {
        assert_eq!(
            resolve_scan_interval(Some(" 750 "), 1500),
            Duration::from_millis(750)
        );
    }
}
