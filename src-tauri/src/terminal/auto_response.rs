//! Fleet-configured auto-response engine.
//!
//! When a terminal Claude session's PTY output matches a fleet-configured
//! regex rule, the runner submits that rule's prompt back into the SAME live
//! session after a per-rule exponential backoff. This recovers a session
//! stranded by the transient "API Error: Server is temporarily limiting
//! requests (not your usage limit) · Rate limited" message — distinct from a
//! usage limit (handled by [`super::account_migration`], NOT this feature).
//!
//! ## Pieces
//!
//! - [`reload_rules`] compiles the fleet rule set (only `enabled` rules; bad
//!   regexes are warned-and-skipped, never fatal) into [`COMPILED_RULES`].
//! - [`AutoResponseWatchHook`] is an [`super::interceptor::OutputHook`]
//!   installed on every terminal's interceptor pipeline. It ANSI-strips +
//!   normalizes a bounded rolling window per terminal (sharing
//!   [`super::output_scan`] with the usage-limit hook) and, for each rule
//!   whose regex matches, hands the match to the [`scheduler`].
//! - [`scheduler`] tracks per `(terminal, rule)` backoff state and, after the
//!   computed delay, submits the rule's prompt into the live session via
//!   [`super::session::TerminalSession::submit_prompt`]. No max-retry cap —
//!   the backoff grows unbounded by default so a persistently-rate-limited
//!   session is nudged ever-more-gently rather than abandoned.
//!
//! Hot-path discipline: `process` runs on the PTY reader thread for every
//! chunk. It early-outs to a single atomic-ish [`RwLock`] read when no rules
//! are loaded, and never holds the per-terminal state lock across the
//! scheduler call.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::interceptor::OutputHook;
use super::output_scan::{normalize, AnsiStripper, WINDOW_KEEP};
use crate::settings::BackoffConfig;

/// A fleet rule with its `pattern` pre-compiled to a [`regex::Regex`].
pub struct CompiledRule {
    pub id: String,
    pub regex: regex::Regex,
    pub prompt: String,
    pub backoff: BackoffConfig,
}

/// The live, compiled rule set. Swapped wholesale by [`reload_rules`]; read on
/// the PTY hot path via [`rules_active`] and the per-chunk match scan.
static COMPILED_RULES: RwLock<Vec<CompiledRule>> = RwLock::new(Vec::new());

/// Recompile the rule set from a freshly-fetched/cached fleet rule list.
///
/// Only `enabled` rules are compiled. A rule whose `pattern` fails to compile
/// is warned-and-skipped — one bad regex never wedges the whole feature. The
/// whole `Vec` is replaced atomically so the hot path never sees a partial set.
pub fn reload_rules(rules: Vec<crate::settings::AutoResponseRule>) {
    let mut compiled = Vec::new();
    for rule in rules {
        if !rule.enabled {
            continue;
        }
        match regex::Regex::new(&rule.pattern) {
            Ok(regex) => compiled.push(CompiledRule {
                id: rule.id,
                regex,
                prompt: rule.prompt,
                backoff: rule.backoff,
            }),
            Err(e) => warn!(
                rule_id = %rule.id,
                pattern = %rule.pattern,
                error = %e,
                "auto_response: skipping rule with invalid regex"
            ),
        }
    }
    let count = compiled.len();
    if let Ok(mut guard) = COMPILED_RULES.write() {
        *guard = compiled;
    }
    info!(count, "auto_response: rules reloaded");
}

/// True iff at least one rule is loaded — the PTY hot-path early-out.
fn rules_active() -> bool {
    COMPILED_RULES
        .read()
        .map(|g| !g.is_empty())
        .unwrap_or(false)
}

// ── The hook ────────────────────────────────────────────────────────────────

struct TermScanState {
    stripper: AnsiStripper,
    window: String,
}

impl Default for TermScanState {
    fn default() -> Self {
        Self {
            stripper: AnsiStripper::default(),
            window: String::with_capacity(WINDOW_KEEP * 2),
        }
    }
}

/// Output hook that fires the auto-response scheduler whenever a fleet rule's
/// regex matches a PTY's recent (ANSI-stripped, normalized) output. Pure
/// pass-through for the data itself — it never modifies the stream.
pub struct AutoResponseWatchHook {
    states: Mutex<HashMap<String, TermScanState>>,
}

impl AutoResponseWatchHook {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for AutoResponseWatchHook {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputHook for AutoResponseWatchHook {
    fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8> {
        // Hot-path early-out: nothing to do when no rules are loaded.
        if !rules_active() {
            return data.to_vec();
        }

        // Feed + normalize the per-terminal window, then collect the matches.
        // Unlike the usage-limit hook this does NOT clear the window on a match:
        // backoff/no-stack live in the scheduler, and clearing would drop the
        // tail of a chunk that contained the match.
        let matches: Vec<(String, String, BackoffConfig)> = {
            let Ok(mut states) = self.states.lock() else {
                return data.to_vec();
            };
            let st = states.entry(terminal_id.to_string()).or_default();
            st.stripper.feed(data, &mut st.window);

            // Bound the rolling window without splitting a UTF-8 char.
            if st.window.len() > WINDOW_KEEP {
                let cut = st.window.len() - WINDOW_KEEP;
                let cut = (cut..st.window.len())
                    .find(|i| st.window.is_char_boundary(*i))
                    .unwrap_or(0);
                st.window.drain(..cut);
            }

            let normalized = normalize(&st.window);
            let Ok(rules) = COMPILED_RULES.read() else {
                return data.to_vec();
            };
            rules
                .iter()
                .filter(|r| r.regex.is_match(&normalized))
                .map(|r| (r.id.clone(), r.prompt.clone(), r.backoff.clone()))
                .collect()
        }; // states + rules locks released here

        // Dispatch to the scheduler with NO hook locks held.
        for (rule_id, prompt, backoff) in matches {
            scheduler::on_match(terminal_id, &rule_id, &prompt, &backoff);
        }

        data.to_vec()
    }
}

// ── Scheduler ────────────────────────────────────────────────────────────────

mod scheduler {
    use super::*;

    /// Per `(terminal_id, rule_id)` backoff bookkeeping.
    struct PairState {
        attempts: u32,
        last_fired: Instant,
        pending: bool,
    }

    /// `(terminal_id, rule_id)` -> backoff state. Option-wrapped so the static
    /// is const-initializable (mirrors `account_migration::MIGRATION_HISTORY`).
    static STATE: Mutex<Option<HashMap<(String, String), PairState>>> = Mutex::new(None);

    /// After this idle gap, a fresh match restarts the backoff at attempt 0 —
    /// the previous burst is considered resolved.
    pub(super) const RESET_WINDOW: Duration = Duration::from_secs(15 * 60);

    /// Pure backoff math: `initial * multiplier^attempts`, capped at
    /// `max_delay_secs` when set, else unbounded. Saturates (never overflows or
    /// panics) when the f64 result is non-finite or exceeds `u64::MAX`.
    pub(super) fn compute_delay(cfg: &BackoffConfig, attempts: u32) -> Duration {
        let raw = cfg.initial_delay_secs as f64 * cfg.multiplier.powi(attempts as i32);
        let mut secs = if !raw.is_finite() || raw >= u64::MAX as f64 {
            u64::MAX
        } else if raw < 0.0 {
            0
        } else {
            raw as u64
        };
        if let Some(cap) = cfg.max_delay_secs {
            secs = secs.min(cap);
        }
        Duration::from_secs(secs)
    }

    /// Register a match for `(tid, rid)`. Returns `None` when a fire is already
    /// pending (no-stack — one in-flight response per pair). Otherwise sets
    /// pending, resets attempts to 0 if the last fire was outside
    /// [`RESET_WINDOW`], and returns the delay to wait before submitting.
    /// `now` is a parameter for deterministic tests.
    pub(super) fn register_match(
        tid: &str,
        rid: &str,
        cfg: &BackoffConfig,
        now: Instant,
    ) -> Option<Duration> {
        let mut guard = STATE.lock().ok()?;
        let map = guard.get_or_insert_with(HashMap::new);
        let key = (tid.to_string(), rid.to_string());
        let entry = map.entry(key).or_insert_with(|| PairState {
            attempts: 0,
            last_fired: now,
            pending: false,
        });

        if entry.pending {
            return None; // already a response in flight for this pair
        }
        if now.duration_since(entry.last_fired) > RESET_WINDOW {
            entry.attempts = 0;
        }
        let delay = compute_delay(cfg, entry.attempts);
        entry.pending = true;
        Some(delay)
    }

    /// Mark a `(tid, rid)` fire complete: bump attempts, stamp `last_fired`,
    /// clear `pending` so the next match can schedule again.
    pub(super) fn record_fired(tid: &str, rid: &str, now: Instant) {
        let Ok(mut guard) = STATE.lock() else {
            return;
        };
        let map = guard.get_or_insert_with(HashMap::new);
        let entry = map
            .entry((tid.to_string(), rid.to_string()))
            .or_insert_with(|| PairState {
                attempts: 0,
                last_fired: now,
                pending: false,
            });
        entry.attempts = entry.attempts.saturating_add(1);
        entry.last_fired = now;
        entry.pending = false;
    }

    /// Entry point from the hook: schedule a backed-off prompt submission for a
    /// matched rule. No-op when a response is already pending for this pair.
    pub(super) fn on_match(tid: &str, rid: &str, prompt: &str, cfg: &BackoffConfig) {
        let Some(delay) = register_match(tid, rid, cfg, Instant::now()) else {
            return;
        };
        let tid = tid.to_string();
        let rid = rid.to_string();
        let prompt = prompt.to_string();
        info!(
            terminal_id = %tid,
            rule_id = %rid,
            delay_secs = delay.as_secs(),
            "auto_response: scheduling prompt submission"
        );
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            submit_to_session(&tid, &prompt).await;
            record_fired(&tid, &rid, Instant::now());
        });
    }

    /// Resolve the live session for `tid` and submit `prompt` into it. If the
    /// session is gone (closed during the backoff), log and return — the
    /// scheduler state is reaped lazily by the next reload / process anyway.
    async fn submit_to_session(tid: &str, prompt: &str) {
        use std::sync::Arc;

        use tauri::Manager;

        let Some(app) = crate::tauri_app_handle::current() else {
            return;
        };
        let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
            return;
        };
        let Some(session) = tm.get(tid) else {
            info!(
                terminal_id = %tid,
                "auto_response: session gone before scheduled submission — skipping"
            );
            return;
        };
        if let Err(e) = session.submit_prompt(prompt) {
            warn!(
                terminal_id = %tid,
                error = %e,
                "auto_response: failed to submit scheduled prompt"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, pattern: &str) -> crate::settings::AutoResponseRule {
        crate::settings::AutoResponseRule {
            id: id.to_string(),
            name: String::new(),
            pattern: pattern.to_string(),
            prompt: "please continue".to_string(),
            enabled: true,
            backoff: BackoffConfig::default(),
        }
    }

    #[test]
    fn reload_drops_disabled_and_invalid_rules() {
        let mut bad = rule("bad", "(unclosed");
        let mut disabled = rule("disabled", "ok");
        disabled.enabled = false;
        bad.enabled = true;
        reload_rules(vec![rule("good", "rate limited"), bad, disabled]);
        let guard = COMPILED_RULES.read().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].id, "good");
        drop(guard);
        // Leave the static empty for other tests.
        reload_rules(vec![]);
    }

    #[test]
    fn hook_matches_regex_over_ansi_window_split_across_calls() {
        reload_rules(vec![rule("rl", "rate limited")]);
        let hook = AutoResponseWatchHook::new();
        // A normalized window must collect the match even when the phrase and
        // an ANSI color code are split across two process() calls. We assert on
        // the normalized window match directly (the scheduler spawn needs a
        // tauri runtime, which a unit test lacks) by re-deriving it the same
        // way the hook does.
        let _ = OutputHook::process(&hook, "t1", b"API Error: Server is rate \x1b[31m");
        let _ = OutputHook::process(&hook, "t1", b"limited now");
        let states = hook.states.lock().unwrap();
        let window = &states.get("t1").unwrap().window;
        assert!(
            normalize(window).contains("rate limited"),
            "window did not contain the phrase: {:?}",
            normalize(window)
        );
        drop(states);
        reload_rules(vec![]);
    }

    #[test]
    fn compute_delay_unbounded() {
        let cfg = BackoffConfig {
            initial_delay_secs: 60,
            multiplier: 2.0,
            max_delay_secs: None,
        };
        let got: Vec<u64> = (0..5)
            .map(|a| scheduler::compute_delay(&cfg, a).as_secs())
            .collect();
        assert_eq!(got, vec![60, 120, 240, 480, 960]);
    }

    #[test]
    fn compute_delay_capped() {
        let cfg = BackoffConfig {
            initial_delay_secs: 60,
            multiplier: 2.0,
            max_delay_secs: Some(300),
        };
        let got: Vec<u64> = (0..5)
            .map(|a| scheduler::compute_delay(&cfg, a).as_secs())
            .collect();
        assert_eq!(got, vec![60, 120, 240, 300, 300]);
    }

    #[test]
    fn compute_delay_saturates_without_panic() {
        let cfg = BackoffConfig {
            initial_delay_secs: 60,
            multiplier: 2.0,
            max_delay_secs: None,
        };
        // 2^10000 overflows f64 to +inf — must saturate, not panic.
        let d = scheduler::compute_delay(&cfg, 10_000);
        assert_eq!(d.as_secs(), u64::MAX);
    }

    #[test]
    fn register_match_no_stack_while_pending() {
        let cfg = BackoffConfig::default();
        let now = Instant::now();
        assert!(scheduler::register_match("term-A", "r1", &cfg, now).is_some());
        // Second match before the first fires must NOT schedule again.
        assert!(scheduler::register_match("term-A", "r1", &cfg, now).is_none());
    }

    #[test]
    fn reset_window_restarts_attempts() {
        let cfg = BackoffConfig {
            initial_delay_secs: 60,
            multiplier: 2.0,
            max_delay_secs: None,
        };
        let t0 = Instant::now();
        // attempt 0 -> 60s
        assert_eq!(
            scheduler::register_match("term-B", "r1", &cfg, t0)
                .unwrap()
                .as_secs(),
            60
        );
        scheduler::record_fired("term-B", "r1", t0);
        // Soon after: attempt 1 -> 120s
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(
            scheduler::register_match("term-B", "r1", &cfg, t1)
                .unwrap()
                .as_secs(),
            120
        );
        scheduler::record_fired("term-B", "r1", t1);
        // After the reset window, attempts restart at 0 -> 60s again.
        let t2 = t1 + scheduler::RESET_WINDOW + Duration::from_secs(1);
        assert_eq!(
            scheduler::register_match("term-B", "r1", &cfg, t2)
                .unwrap()
                .as_secs(),
            60
        );
    }
}
