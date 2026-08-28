//! Tauri commands for the per-run AI cost cap (`settings.cost_budget`).
//!
//! Plan `2026-08-20-workflow-resume-reexecutes-and-rebills` Phase 5 replaced a
//! hardcoded `$5.00 / 500,000 token` constant — reachable only through a
//! `RunCostTrackers::with_budget` constructor that no production code called —
//! with a real settings section. It landed the storage half: the
//! [`crate::settings::Settings::cost_budget`] key, its per-field serde
//! defaults, [`TokenBudget::sanitized`], and a
//! [`crate::config_facade::SettingsField`] impl.
//!
//! What it did not land is a way to reach any of that. The `SettingsField`
//! impl had zero callers, and the only reader was
//! [`TokenBudget::from_settings`] going straight to `load_settings()`. So the
//! cap was configurable only by hand-editing `settings.json` — the same
//! "configurable-looking seam that nothing configures" the phase set out to
//! delete, moved one layer up rather than removed. These two commands are the
//! missing half, and they are what makes the `SettingsField` impl live.
//!
//! Thin wrappers, in the shape of
//! [`crate::commands::performance_settings`] — with one deliberate divergence.
//!
//! **Both ends sanitize, so `get` → `save` is a fixed point.** The getter
//! serves the value `register_cost_trackers` will actually enforce, so the UI
//! cannot show a cap a run will not honour; the setter stores the sanitized
//! value, so loading and saving without editing cannot change what is on disk.
//!
//! That differs from [`crate::commands::performance_settings`], which stores
//! verbatim and applies its floors at use — and the difference is forced by
//! what sanitizing *does* here. A performance floor clamps a scalar, so the
//! operator's intent survives. [`TokenBudget::sanitized`] also **`retain`s**
//! `phase_budgets`, dropping any fraction outside `(0.0, 1.0]`. Serving
//! sanitized while storing verbatim would therefore make a plain round-trip
//! silently delete a malformed phase entry from `settings.json` with nothing
//! shown to the operator — a save that loses data it never displayed. Storing
//! the sanitized value instead makes the drop happen at the moment of the
//! save, where `sanitized` logs each substitution at `warn`.
//!
//! [`TokenBudget::from_settings`] still sanitizes at use, so a hand-edited
//! `settings.json` that never passes through these commands is still safe.
//!
//! The budget is read once per task run at tracker registration, so a change
//! made here is live for the next run with no runner restart.

use crate::cost_management::budget::TokenBudget;

/// Return the per-run cost budget as it will actually be enforced.
#[tauri::command]
pub fn get_cost_budget_settings() -> Result<TokenBudget, String> {
    Ok(crate::settings::get_cost_budget_settings())
}

/// Persist a new per-run cost budget.
///
/// Stores — and echoes — the **sanitized** value, which is what the next run
/// will enforce. Not a re-read of disk, and not necessarily byte-for-byte what
/// was passed in; see the module docs for why this one stores sanitized where
/// the performance caps store verbatim.
#[tauri::command]
pub fn save_cost_budget_settings(settings: TokenBudget) -> Result<TokenBudget, String> {
    let sanitized = settings.sanitized();
    crate::settings::save_cost_budget_settings(sanitized.clone())?;
    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The echo is the enforced value, not the typed one: an operator who
    /// stores an unusable cap must see the fallback that will really apply.
    #[test]
    fn the_echo_is_sanitized_not_the_raw_input() {
        let typed = TokenBudget {
            max_cost_per_run_usd: 0.0,
            max_tokens_per_run: 0,
            ..TokenBudget::default()
        };
        let echoed = typed.clone().sanitized();

        assert_ne!(echoed.max_cost_per_run_usd, typed.max_cost_per_run_usd);
        assert_eq!(
            echoed.max_cost_per_run_usd,
            TokenBudget::default().max_cost_per_run_usd
        );
        assert_eq!(
            echoed.max_tokens_per_run,
            TokenBudget::default().max_tokens_per_run
        );
    }

    /// A usable budget must round-trip untouched — sanitizing is a fallback
    /// for unusable values, not a normalizer.
    #[test]
    fn a_usable_budget_round_trips_unchanged() {
        let typed = TokenBudget {
            max_cost_per_run_usd: 12.5,
            max_tokens_per_run: 1_000_000,
            ..TokenBudget::default()
        };
        assert_eq!(typed.clone().sanitized(), typed);
    }

    /// Both commands sanitize, so loading a budget and saving it back
    /// unedited cannot change what is stored. Without that, `sanitized`'s
    /// `retain` on `phase_budgets` would let a plain round-trip silently drop
    /// a malformed entry the operator was never shown.
    #[test]
    fn get_then_save_is_a_fixed_point_even_for_an_unusable_stored_value() {
        let on_disk = TokenBudget {
            max_cost_per_run_usd: -1.0,
            max_tokens_per_run: 0,
            phase_budgets: [
                ("agentic".to_string(), 0.6),
                ("bogus".to_string(), 4.0),
                ("nan".to_string(), f64::NAN),
            ]
            .into_iter()
            .collect(),
        };

        // What the getter serves.
        let served = on_disk.sanitized();
        assert!(!served.phase_budgets.contains_key("bogus"));
        assert!(!served.phase_budgets.contains_key("nan"));
        assert_eq!(served.phase_budgets.get("agentic"), Some(&0.6));

        // Saving that back unedited stores exactly the same thing, and doing
        // it again is still the same thing.
        assert_eq!(served.clone().sanitized(), served);
        assert_eq!(served.clone().sanitized().sanitized(), served);
    }
}
