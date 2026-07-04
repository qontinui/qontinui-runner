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
//! - [`reload_rules`] compiles the coord-projected fleet rule set (bad regexes
//!   are warned-and-skipped, never fatal) into [`COMPILED_RULES`], carrying each
//!   rule's [`CompiledAction`] (fixed text vs. coord scoring-resolve).
//! - [`scan_grids_once`] / [`spawn_grid_scan_loop`] poll every live terminal's
//!   *rendered* VT cell grid ([`super::session::TerminalSession::grid`]) on a
//!   debounced tick. Scanning the rendered grid — not the raw byte stream —
//!   is what lets the engine see text inside a full-screen TUI like Claude
//!   Code, which paints each whole frame as one synchronized-output update: a
//!   small rolling byte-window only ever retains the *bottom* of such a frame
//!   (input box / status line), so the error text rendered mid-screen is
//!   trimmed out before it can match. The grid always reflects what is on
//!   screen. The scan is EDGE-triggered (see [`GRID_EDGE`]): a rule fires once
//!   when its pattern first appears and not again while the same text stays
//!   painted, re-arming once it leaves the screen.
//! - [`scheduler`] tracks per `(terminal, rule)` backoff state and, after the
//!   computed delay, submits the rule's prompt into the live session via
//!   [`super::session::TerminalSession::submit_prompt`]. No max-retry cap —
//!   the backoff grows unbounded by default so a persistently-rate-limited
//!   session is nudged ever-more-gently rather than abandoned.
//!
//! Cost discipline: every scan early-outs to a single [`RwLock`] read when no
//! rules are loaded; otherwise it is one `text_snapshot` + regex pass per live
//! terminal per tick (~1.5s), and the scheduler is dispatched with no scan
//! locks held.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use super::auto_response_fleet::{FleetRule, RuleAction, ScoringOption};
use super::output_scan::normalize;
use crate::settings::BackoffConfig;

/// The compiled, hot-path form of a rule's action.
///
/// `Fixed` is the fully-offline auto-continue (submit a fixed text). `Resolve`
/// scores the policy's options on each dimension via the runner's own
/// authenticated Claude CLI and POSTs the scores to coord's pure-composition
/// resolve endpoint — injecting nothing when the scorer is unavailable or coord
/// can't confidently resolve.
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledAction {
    /// Submit this fixed text into the matched session (offline-capable).
    Fixed(String),
    /// Score this policy's options/dimensions via the local Claude CLI, then
    /// resolve the winning text via coord. Carries everything the scorer needs:
    /// the coord policy id, the scoreable options, and the scoring dimensions.
    Resolve {
        policy_id: String,
        options: Vec<ScoringOption>,
        dimensions: Vec<String>,
    },
}

/// A fleet rule with its `pattern` pre-compiled to a [`regex::Regex`].
pub struct CompiledRule {
    pub id: String,
    pub regex: regex::Regex,
    pub action: CompiledAction,
    pub backoff: BackoffConfig,
}

/// The live, compiled rule set. Swapped wholesale by [`reload_rules`]; read on
/// the PTY hot path via [`rules_active`] and the per-chunk match scan.
static COMPILED_RULES: RwLock<Vec<CompiledRule>> = RwLock::new(Vec::new());

/// Recompile the rule set from a freshly-fetched/cached fleet rule list.
///
/// Coord projects only the applicable rules (no `enabled` filter on the wire),
/// so every rule here is compiled. A rule whose `pattern` fails to compile is
/// warned-and-skipped — one bad regex never wedges the whole feature. The whole
/// `Vec` is replaced atomically so the hot path never sees a partial set.
///
/// `case_insensitive` is honored via [`regex::RegexBuilder`]; an inline `(?i)`
/// in the pattern also works (and the two compose — either turns on the flag).
pub fn reload_rules(rules: Vec<FleetRule>) {
    let mut compiled = Vec::new();
    for rule in rules {
        match regex::RegexBuilder::new(&rule.pattern)
            .case_insensitive(rule.case_insensitive)
            .build()
        {
            Ok(regex) => {
                let action = match rule.action {
                    RuleAction::SubmitPrompt { text } => CompiledAction::Fixed(text),
                    RuleAction::ResolveByScoring {
                        policy_id,
                        options,
                        dimensions,
                    } => CompiledAction::Resolve {
                        policy_id,
                        options,
                        dimensions,
                    },
                };
                compiled.push(CompiledRule {
                    id: rule.id,
                    regex,
                    action,
                    backoff: rule.backoff,
                });
            }
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

// ── Grid scanner ─────────────────────────────────────────────────────────────

/// Per `(terminal_id, rule_id)` "was matching on the previous scan" flag, so
/// the grid scanner is EDGE-triggered: a rule fires once when its pattern first
/// appears on screen and NOT again while the same text stays painted (a
/// full-screen TUI keeps the rate-limit error on screen until the next turn).
/// It re-arms once the text leaves the screen, so a genuinely fresh occurrence
/// fires again. Without this, a persistently-visible error would re-fire on
/// every backoff completion and spam the session.
///
/// Option-wrapped so the static is const-initializable (mirrors
/// [`scheduler::STATE`]). Outer key: terminal id; inner: rule id -> matching.
static GRID_EDGE: Mutex<Option<HashMap<String, HashMap<String, bool>>>> = Mutex::new(None);

/// Pure edge-detection core: given a terminal's normalized screen text, the
/// compiled rules, and that terminal's prior per-rule match flags, return the
/// indices of rules that just transitioned no-match -> match (rising edges),
/// updating the flags in place. Pure + total — the unit-test seam for the
/// fire-once-per-appearance behavior.
fn collect_rising_edges(
    normalized: &str,
    rules: &[CompiledRule],
    prev: &mut HashMap<String, bool>,
) -> Vec<usize> {
    let mut fired = Vec::new();
    for (i, rule) in rules.iter().enumerate() {
        let matching = rule.regex.is_match(normalized);
        if matching && !prev.get(&rule.id).copied().unwrap_or(false) {
            fired.push(i);
        }
        prev.insert(rule.id.clone(), matching);
    }
    fired
}

/// One grid-scan pass over every live terminal: for each rule whose pattern is
/// *freshly* visible on a terminal's rendered screen, schedule a backed-off
/// response via the shared [`scheduler`]. Cheap no-op when no rules are loaded.
pub fn scan_grids_once() {
    use tauri::Manager;

    if !rules_active() {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let sessions = tm.sessions_snapshot();

    // Collect rising-edge matches under the rule + edge locks, then dispatch to
    // the scheduler with NO locks held (the scheduler spawns + locks its own
    // state).
    let mut to_fire: Vec<(String, String, CompiledAction, BackoffConfig, String)> = Vec::new();
    {
        let Ok(rules) = COMPILED_RULES.read() else {
            return;
        };
        let mut edge = GRID_EDGE.lock().unwrap_or_else(|e| e.into_inner());
        let map = edge.get_or_insert_with(HashMap::new);
        for (tid, session) in &sessions {
            // Read the *rendered* screen text (rows joined by `\n`) — the grid
            // is the VT-parsed cell buffer, so this is immune to the
            // synchronized-output frame batching that hides text from a raw
            // byte-stream scan in a full-screen TUI.
            let text = {
                let grid = session.grid();
                let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
                guard.text_snapshot().text
            };
            let normalized = normalize(&text);
            let prev = map.entry(tid.clone()).or_default();
            for idx in collect_rising_edges(&normalized, &rules, prev) {
                let rule = &rules[idx];
                to_fire.push((
                    tid.clone(),
                    rule.id.clone(),
                    rule.action.clone(),
                    rule.backoff.clone(),
                    normalized.clone(),
                ));
            }
        }
        // Drop edge state for terminals that have gone away.
        let live: std::collections::HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        map.retain(|tid, _| live.contains(tid));
    } // rules + edge locks released

    for (tid, rid, action, backoff, context) in to_fire {
        scheduler::on_match(&tid, &rid, &action, &backoff, &context);
    }
}

/// Grid-scan interval. Overridable via
/// `QONTINUI_AUTO_RESPONSE_SCAN_INTERVAL_MS` (floored at 200ms). 1.5s is well
/// under the default 60s backoff, so the (idle, on-screen) rate-limit error is
/// caught promptly while costing a trivial regex pass per terminal per tick.
fn scan_interval() -> Duration {
    let ms = std::env::var("QONTINUI_AUTO_RESPONSE_SCAN_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 200)
        .unwrap_or(1500);
    Duration::from_millis(ms)
}

/// Spawn the periodic grid scanner for the process lifetime. Detached; each
/// tick is best-effort and the loop never exits.
pub fn spawn_grid_scan_loop() {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(scan_interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!("auto_response: grid-scan loop started");
        loop {
            ticker.tick().await;
            scan_grids_once();
        }
    });
}

/// The coord identity of a live terminal session, captured for the unified
/// prompt-injection audit log. `terminal_id` is always set; `coord_session_id`
/// / `title` are `None` when the session has gone away or has no coord mirror
/// wired yet.
#[derive(Clone, Debug, Default)]
pub(super) struct SessionIdentity {
    pub terminal_id: Option<String>,
    pub coord_session_id: Option<uuid::Uuid>,
    pub title: Option<String>,
}

/// Resolve the live session identity for `tid` (same TerminalManager lookup the
/// scheduler uses to submit). Best-effort: always records the `terminal_id`;
/// leaves the coord id / title `None` if the app handle, the manager, or the
/// session is unavailable.
fn resolve_session_identity(tid: &str) -> SessionIdentity {
    use tauri::Manager;

    let mut ident = SessionIdentity {
        terminal_id: Some(tid.to_string()),
        ..Default::default()
    };
    if let Some(app) = crate::tauri_app_handle::current() {
        if let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() {
            if let Some(session) = tm.get(tid) {
                ident.coord_session_id = session.coord_session_id();
                ident.title = Some(session.title());
            }
        }
    }
    ident
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

    /// Entry point from the hook: schedule a backed-off response for a matched
    /// rule. No-op when a response is already pending for this pair.
    ///
    /// `Fixed` actions submit the fixed text after the backoff (fully offline).
    /// `Resolve` actions, after the backoff, score the policy's options on each
    /// dimension via the runner's own authenticated Claude CLI, POST the scores
    /// to coord's pure-composition resolve endpoint with `context`, and submit
    /// ONLY the confidently-resolved `response_text`; on a scorer failure
    /// (`None` scores), `resolved:false`, a coord error, or no options they
    /// inject NOTHING and let the next match retry under backoff.
    pub(super) fn on_match(
        tid: &str,
        rid: &str,
        action: &CompiledAction,
        cfg: &BackoffConfig,
        context: &str,
    ) {
        let Some(delay) = register_match(tid, rid, cfg, Instant::now()) else {
            return;
        };
        let tid = tid.to_string();
        let rid = rid.to_string();
        let action = action.clone();
        let context = context.to_string();
        info!(
            terminal_id = %tid,
            rule_id = %rid,
            delay_secs = delay.as_secs(),
            "auto_response: scheduling response"
        );
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            match action {
                CompiledAction::Fixed(text) => {
                    // Report the regex `submit_prompt` injection to coord's
                    // unified audit log ONLY when the submission actually
                    // landed. Best-effort + fire-and-forget (never blocks/fails
                    // the scheduler).
                    if submit_to_session(&tid, &text).await {
                        super::report::report_submit_prompt_injection(
                            &tid, &rid, &context, &context, &text,
                        )
                        .await;
                    }
                }
                CompiledAction::Resolve {
                    policy_id,
                    options,
                    dimensions,
                } => {
                    // Resolve the session identity so coord's scoring-injection
                    // audit row is attributed to the same session.
                    let identity = super::resolve_session_identity(&tid);
                    match resolve_scored_response(
                        &policy_id,
                        &context,
                        &options,
                        &dimensions,
                        identity,
                    )
                    .await
                    {
                        Some(text) => {
                            submit_to_session(&tid, &text).await;
                        }
                        None => info!(
                            terminal_id = %tid,
                            rule_id = %rid,
                            "auto_response: not resolved (no scores / coord unresolved) — injecting nothing"
                        ),
                    }
                }
            }
            // Always bump backoff: a `resolved:false` / error still consumed an
            // attempt, so the next match waits longer (and the reset-window
            // restarts attempts once the burst is over).
            record_fired(&tid, &rid, Instant::now());
        });
    }

    /// Resolve the live session for `tid` and submit `prompt` into it. If the
    /// session is gone (closed during the backoff), log and return — the
    /// scheduler state is reaped lazily by the next reload / process anyway.
    ///
    /// Returns `true` only when the prompt was actually submitted into a live
    /// session (so the caller can gate audit reporting on a real injection).
    async fn submit_to_session(tid: &str, prompt: &str) -> bool {
        use tauri::Manager;

        let Some(app) = crate::tauri_app_handle::current() else {
            return false;
        };
        let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
            return false;
        };
        let Some(session) = tm.get(tid) else {
            info!(
                terminal_id = %tid,
                "auto_response: session gone before scheduled submission — skipping"
            );
            return false;
        };
        if let Err(e) = session.submit_prompt(prompt) {
            warn!(
                terminal_id = %tid,
                error = %e,
                "auto_response: failed to submit scheduled prompt"
            );
            return false;
        }
        true
    }

    /// Score the policy's options via the local Claude CLI, then resolve the
    /// winning text via coord. Returns `Some(response_text)` ONLY when scoring
    /// produced a score map AND coord returned `resolved:true` with non-empty
    /// text. Every other outcome (no options, scorer unavailable / timeout /
    /// unparseable, `resolved:false`, network/parse error, missing coord base)
    /// yields `None` — the caller injects nothing (NEVER a fallback/guess).
    pub(super) async fn resolve_scored_response(
        policy_id: &str,
        context: &str,
        options: &[ScoringOption],
        dimensions: &[String],
        identity: super::SessionIdentity,
    ) -> Option<String> {
        decide_resolution_with(
            policy_id,
            context,
            options,
            dimensions,
            // CLI scorer seam: build the prompt + run the one-shot CLI.
            |ctx, opts, dims| {
                let prompt = super::scoring::build_scoring_prompt(ctx, opts, dims);
                async move { super::scoring::run_cli_scorer(prompt).await }
            },
            // Coord resolve seam — carries the session identity so coord can
            // attribute the scoring-injection audit row.
            move |pid, ctx, scores| {
                let pid = pid.to_string();
                let ctx = ctx.to_string();
                async move {
                    super::resolve::resolve_via_coord(
                        &pid,
                        &ctx,
                        scores,
                        identity.terminal_id,
                        identity.coord_session_id,
                        identity.title,
                    )
                    .await
                }
            },
        )
        .await
    }

    /// Generic firing core for the `Resolve` branch with the CLI scorer and the
    /// coord POST injected as seams, so the full fail-safe decision logic is
    /// unit-testable without a live `claude` or coord. Fail-safe branches:
    ///   * no options                → `None` (nothing to score)
    ///   * scorer returns `None`     → `None` (CLI unavailable/timeout/garbage)
    ///   * coord returns `None`      → `None` (`resolved:false`/network/empty)
    /// Only a scorer `Some` AND a coord `Some(text)` yields `Some(text)`.
    pub(super) async fn decide_resolution_with<ScoreFut, PostFut, ScoreFn, PostFn>(
        policy_id: &str,
        context: &str,
        options: &[ScoringOption],
        dimensions: &[String],
        score: ScoreFn,
        post: PostFn,
    ) -> Option<String>
    where
        ScoreFut: std::future::Future<Output = Option<serde_json::Value>>,
        PostFut: std::future::Future<Output = Option<String>>,
        ScoreFn: FnOnce(&str, &[ScoringOption], &[String]) -> ScoreFut,
        PostFn: FnOnce(&str, &str, serde_json::Value) -> PostFut,
    {
        // Fail-safe no-op when coord projected no options (e.g. an older coord
        // that doesn't send them, or a misconfigured policy): nothing to score.
        if options.is_empty() {
            debug!(
                policy_id,
                "auto_response: resolve rule has no options — injecting nothing"
            );
            return None;
        }
        // Score via the seam; `None` on any CLI failure → inject nothing.
        let scores = score(context, options, dimensions).await?;
        // Resolve via coord; `None` on unresolved/error → inject nothing.
        post(policy_id, context, scores).await
    }
}

/// Local Claude-CLI option scorer for `resolve_by_scoring` rules. The prompt
/// builder and JSON parser are pure (unit-tested); [`run_cli_scorer`] is the
/// only impure piece (spawns the one-shot CLI on a blocking thread).
mod scoring {
    use super::ScoringOption;
    use std::time::Duration;
    use tracing::{debug, warn};

    /// Bounded wall-clock for the one-shot scoring CLI call. Overridable via
    /// `QONTINUI_AUTO_RESPONSE_SCORE_TIMEOUT_SECS` (floored at 5s). 60s default
    /// is generous for a single short scoring turn while still bounding a wedged
    /// CLI so the scheduler task can't hang forever.
    fn score_timeout() -> Duration {
        let secs = std::env::var("QONTINUI_AUTO_RESPONSE_SCORE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n >= 5)
            .unwrap_or(60);
        Duration::from_secs(secs)
    }

    /// Build the deterministic scoring prompt instructing the CLI to score each
    /// option on each dimension in `0.0..1.0` and emit STRICT JSON
    /// `{"<option_id>":{"<dimension>":0.0,...},...}` with no prose. Pure — the
    /// unit-test seam for prompt content.
    pub(super) fn build_scoring_prompt(
        context: &str,
        options: &[ScoringOption],
        dimensions: &[String],
    ) -> String {
        let mut s = String::new();
        s.push_str(
            "You are scoring response options for an automation policy. A terminal \
             session is waiting for input. Below is the on-screen terminal context, \
             a list of candidate response options, and the dimensions to score each \
             option on.\n\n",
        );
        s.push_str("=== TERMINAL CONTEXT ===\n");
        s.push_str(context);
        s.push_str("\n=== END TERMINAL CONTEXT ===\n\n");

        s.push_str("=== OPTIONS ===\n");
        for opt in options {
            s.push_str("- id=\"");
            s.push_str(&opt.id);
            s.push_str("\" label=\"");
            s.push_str(&opt.label);
            s.push_str("\"\n");
        }
        s.push('\n');

        s.push_str("=== DIMENSIONS ===\n");
        if dimensions.is_empty() {
            // No dimensions configured: still ask for a single overall score so
            // coord has something to compose; keeps the JSON shape consistent.
            s.push_str("- overall\n");
        } else {
            for dim in dimensions {
                s.push_str("- ");
                s.push_str(dim);
                s.push('\n');
            }
        }
        s.push('\n');

        s.push_str(
            "Score EVERY option on EVERY dimension. Each score is a number from 0.0 \
             (worst fit) to 1.0 (best fit) given the terminal context.\n\n\
             Respond with STRICT JSON ONLY — no prose, no markdown, no code fences. \
             The JSON must be an object keyed by option id, whose value is an object \
             keyed by dimension name with a number value. Example shape:\n\
             {\"<option_id>\":{\"<dimension>\":0.0}}\n",
        );
        s
    }

    /// Parse the CLI's stdout into a scores map. Tolerant: strips Markdown code
    /// fences (```json … ```), then parses the remainder as a JSON object.
    /// Returns `None` on anything that is not a JSON object (garbage / prose /
    /// array / scalar). Pure — the unit-test seam for parse robustness.
    pub(super) fn parse_scores(raw: &str) -> Option<serde_json::Value> {
        let cleaned = strip_code_fences(raw);
        let value: serde_json::Value = serde_json::from_str(cleaned.trim()).ok()?;
        if value.is_object() {
            Some(value)
        } else {
            None
        }
    }

    /// Strip a single leading/trailing Markdown code fence pair if present,
    /// tolerating an optional language tag (```json). Returns the inner content;
    /// if no fence is found, returns the input unchanged.
    fn strip_code_fences(raw: &str) -> String {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix("```") {
            // Drop the optional language tag up to the first newline.
            let after_tag = match rest.find('\n') {
                Some(nl) => &rest[nl + 1..],
                None => rest,
            };
            let inner = after_tag.strip_suffix("```").unwrap_or(after_tag);
            // Also handle a trailing fence on its own line.
            return inner.trim_end_matches('`').trim().to_string();
        }
        t.to_string()
    }

    /// Run the one-shot CLI scorer on a blocking thread (it spawns + waits on a
    /// subprocess) and parse its output. `None` on a missing binary, nonzero
    /// exit, timeout, or unparseable output — every failure fails closed.
    pub(super) async fn run_cli_scorer(prompt: String) -> Option<serde_json::Value> {
        let timeout = score_timeout();
        let raw = tokio::task::spawn_blocking(move || {
            let settings = crate::settings::load_settings();
            crate::ai_provider::score_options_via_cli(&prompt, &settings.ai.claude_cli, timeout)
        })
        .await
        .ok()
        .flatten()?;
        match parse_scores(&raw) {
            Some(v) => {
                debug!("auto_response scorer: parsed scores from CLI");
                Some(v)
            }
            None => {
                warn!("auto_response scorer: CLI output not parseable as score JSON — no scores");
                None
            }
        }
    }
}

/// Coord scoring-resolve call, factored out of the scheduler so it is
/// unit-testable at the parse boundary without a live server.
mod resolve {
    use std::time::Duration;

    use serde::{Deserialize, Serialize};
    use tracing::{debug, warn};

    /// POST body for `{coord_base}/coord/policies/resolve`.
    ///
    /// `scores` is the runner-computed `{option_id:{dimension:0.0..1.0}}` map
    /// from the local CLI scorer; coord does pure composition over it (no judge
    /// of its own) to pick a confident winner.
    #[derive(Serialize)]
    struct ResolveRequest<'a> {
        policy_id: &'a str,
        context: &'a str,
        scores: serde_json::Value,
        // Optional session identity, so coord's scoring-injection audit row is
        // attributed to the same terminal session (coord Phase 2 adds these same
        // optional fields — keep the serde names identical).
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        coord_session_id: Option<uuid::Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    }

    /// Response from `{coord_base}/coord/policies/resolve` (always HTTP 200).
    #[derive(Debug, Deserialize, PartialEq)]
    pub(super) struct ResolveResponse {
        pub resolved: bool,
        #[serde(default)]
        pub response_text: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        pub reason: Option<String>,
    }

    /// Coord resolve endpoint URL, or `None` when no coord base is configured.
    fn resolve_endpoint_url() -> Option<String> {
        match qontinui_runner_lib::profiles::resolve_coord_base() {
            qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(format!(
                "{}/coord/policies/resolve",
                base.trim_end_matches('/')
            )),
            _ => None,
        }
    }

    /// Map a parsed resolve response to the response text to inject.
    ///
    /// `Some` ONLY when `resolved` is true AND a non-empty `response_text` is
    /// present. `resolved:false`, a missing/empty text → `None` (inject
    /// nothing). Pure + total — the unit-test seam for the resolve branch.
    pub(super) fn response_text_to_inject(resp: &ResolveResponse) -> Option<String> {
        if !resp.resolved {
            return None;
        }
        match resp.response_text.as_deref() {
            Some(t) if !t.is_empty() => Some(t.to_string()),
            _ => None,
        }
    }

    /// POST the resolve request (with the runner-computed `scores`) and return
    /// the text to inject, or `None` on any unresolved / error condition.
    pub(super) async fn resolve_via_coord(
        policy_id: &str,
        context: &str,
        scores: serde_json::Value,
        terminal_id: Option<String>,
        coord_session_id: Option<uuid::Uuid>,
        title: Option<String>,
    ) -> Option<String> {
        let url = resolve_endpoint_url()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .ok()?;
        let body = ResolveRequest {
            policy_id,
            context,
            scores,
            terminal_id,
            coord_session_id,
            title,
        };
        // Device-JWT bearer on a POST — the same write-path attach the coord
        // producers use (collapses to anonymous when unpaired).
        let req = qontinui_runner_lib::auth::attach_device_auth(client.post(&url));
        let resp = match req.json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "auto_response: resolve POST failed — injecting nothing");
                return None;
            }
        };
        if !resp.status().is_success() {
            warn!(status = %resp.status(), "auto_response: resolve non-2xx — injecting nothing");
            return None;
        }
        let parsed: ResolveResponse = match resp.json().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "auto_response: resolve parse failed — injecting nothing");
                return None;
            }
        };
        debug!(
            resolved = parsed.resolved,
            "auto_response: coord resolve result"
        );
        response_text_to_inject(&parsed)
    }
}

/// Reports a regex `submit_prompt` injection to coord's unified prompt-injection
/// audit log. Best-effort and strictly fire-and-forget: gated off by an env
/// kill-switch, bounded by a short timeout, and NEVER retries, blocks, or
/// propagates — any failure is a `warn!` and a return. Coord returns
/// `200 {"recorded":true}` on success; the body is not parsed.
mod report {
    use std::time::Duration;

    use serde::Serialize;
    use tracing::warn;

    /// POST body for `{coord_base}/coord/prompt-injections/report`.
    ///
    /// SHARED CONTRACT with coord Phase 2 — field names and shape must match
    /// exactly. `coord_session_id` / `title` serialize to `null` when the
    /// session is gone; `policy_id` is always `null` for the regex source.
    #[derive(Serialize)]
    struct ReportBody<'a> {
        terminal_id: String,
        coord_session_id: Option<uuid::Uuid>,
        title: Option<String>,
        rule_id: &'a str,
        policy_id: Option<&'a str>,
        trigger_kind: &'a str,
        trigger_text: &'a str,
        injected_prompt: &'a str,
        source: &'a str,
        metadata: serde_json::Value,
    }

    /// Coord audit-report endpoint URL, or `None` when no coord base is
    /// configured (then the report is silently skipped).
    fn report_endpoint_url() -> Option<String> {
        match qontinui_runner_lib::profiles::resolve_coord_base() {
            qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(format!(
                "{}/coord/prompt-injections/report",
                base.trim_end_matches('/')
            )),
            _ => None,
        }
    }

    /// Fire-and-forget report of a `submit_prompt` regex injection. `raw_trigger`
    /// carries the un-normalized matched screen text when available (the caller
    /// currently passes the normalized text for both, as the raw grid text is
    /// not plumbed into the scheduler).
    pub(super) async fn report_submit_prompt_injection(
        tid: &str,
        rule_id: &str,
        trigger_text_normalized: &str,
        raw_trigger: &str,
        injected_prompt: &str,
    ) {
        if std::env::var_os("QONTINUI_PROMPT_AUDIT_REPORT_DISABLED").is_some() {
            return;
        }
        let Some(url) = report_endpoint_url() else {
            return;
        };
        // Resolve the terminal identity the same way the scheduler submits;
        // still report (with nulls) if the session has since gone away.
        let ident = super::resolve_session_identity(tid);
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "auto_response: prompt-audit client build failed — skipping report");
                return;
            }
        };
        let body = ReportBody {
            terminal_id: tid.to_string(),
            coord_session_id: ident.coord_session_id,
            title: ident.title,
            rule_id,
            policy_id: None,
            trigger_kind: "terminal_regex",
            trigger_text: trigger_text_normalized,
            injected_prompt,
            source: "regex_submit_prompt",
            metadata: serde_json::json!({ "raw_trigger": raw_trigger }),
        };
        // Device-JWT bearer on the write path (collapses to anonymous when
        // unpaired), mirroring `mod resolve`.
        let req = qontinui_runner_lib::auth::attach_device_auth(client.post(&url));
        match req.json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                warn!(status = %resp.status(), "auto_response: prompt-audit report non-2xx — dropping");
            }
            Err(e) => {
                warn!(error = %e, "auto_response: prompt-audit report failed — dropping");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate/read the shared [`COMPILED_RULES`] static.
    /// `cargo test` runs tests concurrently, so without this a sibling test's
    /// `reload_rules(vec![])` can empty the static mid-test (e.g. between the two
    /// `process` calls of the window-accumulation test, flipping `rules_active()`
    /// false so the second chunk is dropped). Poison-tolerant.
    static RULES_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn rule(id: &str, pattern: &str) -> FleetRule {
        FleetRule {
            id: id.to_string(),
            pattern: pattern.to_string(),
            case_insensitive: false,
            backoff: BackoffConfig::default(),
            action: RuleAction::SubmitPrompt {
                text: "please continue".to_string(),
            },
        }
    }

    #[test]
    fn reload_drops_invalid_rules() {
        let _guard = RULES_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bad = rule("bad", "(unclosed");
        reload_rules(vec![rule("good", "rate limited"), bad]);
        let guard = COMPILED_RULES.read().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].id, "good");
        assert_eq!(
            guard[0].action,
            CompiledAction::Fixed("please continue".to_string())
        );
        drop(guard);
        // Leave the static empty for other tests.
        reload_rules(vec![]);
    }

    #[test]
    fn reload_compiles_resolve_action_and_case_insensitive() {
        let _guard = RULES_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut r = rule("ci", "Rate Limited");
        r.case_insensitive = true;
        r.action = RuleAction::ResolveByScoring {
            policy_id: "pol-1".to_string(),
            options: vec![
                ScoringOption {
                    id: "a".to_string(),
                    label: "Approve".to_string(),
                },
                ScoringOption {
                    id: "b".to_string(),
                    label: "Reject".to_string(),
                },
            ],
            dimensions: vec!["safety".to_string()],
        };
        reload_rules(vec![r]);
        let guard = COMPILED_RULES.read().unwrap();
        assert_eq!(guard.len(), 1);
        // case_insensitive honored even though the pattern has no inline (?i).
        assert!(guard[0].regex.is_match("rate limited"));
        assert_eq!(
            guard[0].action,
            CompiledAction::Resolve {
                policy_id: "pol-1".to_string(),
                options: vec![
                    ScoringOption {
                        id: "a".to_string(),
                        label: "Approve".to_string(),
                    },
                    ScoringOption {
                        id: "b".to_string(),
                        label: "Reject".to_string(),
                    },
                ],
                dimensions: vec!["safety".to_string()],
            }
        );
        drop(guard);
        reload_rules(vec![]);
    }

    #[test]
    fn resolve_response_injects_only_on_resolved_true() {
        use super::resolve::{response_text_to_inject, ResolveResponse};

        // resolved:true with text → inject that text.
        let ok = ResolveResponse {
            resolved: true,
            response_text: Some("the winner".to_string()),
            reason: None,
        };
        assert_eq!(response_text_to_inject(&ok), Some("the winner".to_string()));

        // resolved:false → inject nothing (NEVER a fallback/guess).
        let no = ResolveResponse {
            resolved: false,
            response_text: None,
            reason: Some("no_confident_winner".to_string()),
        };
        assert_eq!(response_text_to_inject(&no), None);

        // judge-unavailable shape → inject nothing.
        let unavail = ResolveResponse {
            resolved: false,
            response_text: None,
            reason: Some("judge_unavailable".to_string()),
        };
        assert_eq!(response_text_to_inject(&unavail), None);

        // Defensive: resolved:true but empty/missing text → inject nothing.
        let empty = ResolveResponse {
            resolved: true,
            response_text: Some(String::new()),
            reason: None,
        };
        assert_eq!(response_text_to_inject(&empty), None);
    }

    #[test]
    fn resolve_response_parses_both_coord_shapes() {
        use super::resolve::ResolveResponse;

        let yes: ResolveResponse =
            serde_json::from_str(r#"{"resolved":true,"response_text":"go"}"#).unwrap();
        assert_eq!(
            yes,
            ResolveResponse {
                resolved: true,
                response_text: Some("go".to_string()),
                reason: None,
            }
        );

        let no: ResolveResponse =
            serde_json::from_str(r#"{"resolved":false,"reason":"judge_unavailable"}"#).unwrap();
        assert_eq!(
            no,
            ResolveResponse {
                resolved: false,
                response_text: None,
                reason: Some("judge_unavailable".to_string()),
            }
        );
    }

    // ── Scoring prompt builder + parser (pure) ────────────────────────────────

    fn opts() -> Vec<ScoringOption> {
        vec![
            ScoringOption {
                id: "opt-a".to_string(),
                label: "Yes, proceed with the deploy".to_string(),
            },
            ScoringOption {
                id: "opt-b".to_string(),
                label: "No, cancel".to_string(),
            },
        ]
    }

    #[test]
    fn scoring_prompt_includes_options_dimensions_context_and_demands_json() {
        let dims = vec!["safety".to_string(), "intent_fit".to_string()];
        let prompt = super::scoring::build_scoring_prompt(
            "Coordinator: should I deploy to prod? (y/n)",
            &opts(),
            &dims,
        );
        // Context is carried verbatim.
        assert!(prompt.contains("should I deploy to prod? (y/n)"));
        // Both option ids + labels present.
        assert!(prompt.contains("opt-a"));
        assert!(prompt.contains("Yes, proceed with the deploy"));
        assert!(prompt.contains("opt-b"));
        assert!(prompt.contains("No, cancel"));
        // Both dimensions present.
        assert!(prompt.contains("safety"));
        assert!(prompt.contains("intent_fit"));
        // Demands STRICT JSON, no prose.
        assert!(prompt.contains("STRICT JSON"));
        assert!(prompt.to_lowercase().contains("no prose"));
        // The 0.0..1.0 score range is specified.
        assert!(prompt.contains("0.0") && prompt.contains("1.0"));
    }

    #[test]
    fn scoring_prompt_with_no_dimensions_asks_for_overall() {
        let prompt = super::scoring::build_scoring_prompt("ctx", &opts(), &[]);
        assert!(prompt.contains("overall"));
    }

    #[test]
    fn parse_scores_clean_json() {
        let raw =
            r#"{"opt-a":{"safety":0.9,"intent_fit":0.8},"opt-b":{"safety":0.2,"intent_fit":0.1}}"#;
        let v = super::scoring::parse_scores(raw).expect("clean JSON parses");
        assert_eq!(v["opt-a"]["safety"], serde_json::json!(0.9));
        assert_eq!(v["opt-b"]["intent_fit"], serde_json::json!(0.1));
    }

    #[test]
    fn parse_scores_code_fenced_json() {
        let raw = "```json\n{\"opt-a\":{\"safety\":1.0}}\n```";
        let v = super::scoring::parse_scores(raw).expect("code-fenced JSON parses");
        assert_eq!(v["opt-a"]["safety"], serde_json::json!(1.0));

        // Bare fence with no language tag also works.
        let raw2 = "```\n{\"opt-b\":{\"x\":0.5}}\n```";
        let v2 = super::scoring::parse_scores(raw2).expect("bare-fenced JSON parses");
        assert_eq!(v2["opt-b"]["x"], serde_json::json!(0.5));
    }

    #[test]
    fn parse_scores_garbage_is_none() {
        // Prose.
        assert!(super::scoring::parse_scores("I think option A is best.").is_none());
        // Empty.
        assert!(super::scoring::parse_scores("").is_none());
        // JSON but not an object (array / scalar).
        assert!(super::scoring::parse_scores("[1, 2, 3]").is_none());
        assert!(super::scoring::parse_scores("42").is_none());
        // Truncated / invalid JSON.
        assert!(super::scoring::parse_scores("{\"opt-a\": {").is_none());
    }

    // ── Resolve firing flow (seams mocked — no live claude / coord) ───────────

    #[tokio::test]
    async fn resolve_fires_submits_when_scorer_and_coord_agree() {
        // Scorer favors A; mocked coord composes A's text.
        let options = opts();
        let got = scheduler::decide_resolution_with(
            "pol-1",
            "ctx",
            &options,
            &["safety".to_string()],
            |_ctx, _opts, _dims| async {
                Some(serde_json::json!({"opt-a": {"safety": 0.9}, "opt-b": {"safety": 0.1}}))
            },
            |_pid, _ctx, scores| async move {
                // Coord saw the runner-computed scores and returns A's text.
                assert_eq!(scores["opt-a"]["safety"], serde_json::json!(0.9));
                Some("Yes, proceed with the deploy".to_string())
            },
        )
        .await;
        assert_eq!(got, Some("Yes, proceed with the deploy".to_string()));
    }

    #[tokio::test]
    async fn resolve_no_op_when_scorer_returns_none() {
        let options = opts();
        let mut posted = false;
        let got = scheduler::decide_resolution_with(
            "pol-1",
            "ctx",
            &options,
            &["safety".to_string()],
            |_c, _o, _d| async { None }, // scorer unavailable
            |_p, _c, _s| {
                posted = true;
                async { Some("should not happen".to_string()) }
            },
        )
        .await;
        assert_eq!(got, None, "scorer None must inject nothing");
        assert!(!posted, "coord must not be called when scoring failed");
    }

    #[tokio::test]
    async fn resolve_no_op_when_coord_unresolved() {
        let options = opts();
        let got = scheduler::decide_resolution_with(
            "pol-1",
            "ctx",
            &options,
            &["safety".to_string()],
            |_c, _o, _d| async { Some(serde_json::json!({"opt-a": {"safety": 0.5}})) },
            |_p, _c, _s| async { None }, // coord resolved:false / error
        )
        .await;
        assert_eq!(got, None, "coord None must inject nothing");
    }

    #[tokio::test]
    async fn resolve_no_op_when_no_options() {
        let mut scored = false;
        let got = scheduler::decide_resolution_with(
            "pol-1",
            "ctx",
            &[], // no options projected → fail-safe no-op
            &["safety".to_string()],
            |_c, _o, _d| {
                scored = true;
                async { Some(serde_json::json!({})) }
            },
            |_p, _c, _s| async { Some("nope".to_string()) },
        )
        .await;
        assert_eq!(got, None, "no options must inject nothing");
        assert!(!scored, "scorer must not run when there are no options");
    }

    /// A compiled rule built directly (no fleet round-trip) for grid-scan tests.
    fn crule(id: &str, pattern: &str) -> CompiledRule {
        CompiledRule {
            id: id.to_string(),
            regex: regex::Regex::new(pattern).unwrap(),
            action: CompiledAction::Fixed("please continue".to_string()),
            backoff: BackoffConfig::default(),
        }
    }

    #[test]
    fn grid_scan_is_edge_triggered_fire_once_per_appearance() {
        // The seeded rate-limit rule, matched against rendered screen text.
        let rules = vec![crule("rl", "(?i)server is temporarily limiting requests")];
        let mut prev: HashMap<String, bool> = HashMap::new();

        // Not on screen -> no fire.
        assert!(collect_rising_edges("PS D:\\> echo hi   hi", &rules, &mut prev).is_empty());
        // Error appears -> fires once (rising edge).
        assert_eq!(
            collect_rising_edges(
                "API Error: Server is temporarily limiting requests   continue?",
                &rules,
                &mut prev
            ),
            vec![0]
        );
        // Still painted on the next tick -> does NOT re-fire (no spam while the
        // full-screen TUI keeps the error on screen).
        assert!(collect_rising_edges(
            "server is temporarily limiting requests   ❯",
            &rules,
            &mut prev
        )
        .is_empty());
        // Scrolled off / screen cleared -> no fire, and the edge re-arms.
        assert!(
            collect_rising_edges("I'm ready to help — what's next?", &rules, &mut prev).is_empty()
        );
        // A genuinely fresh occurrence later -> fires again.
        assert_eq!(
            collect_rising_edges(
                "again — server is temporarily limiting requests",
                &rules,
                &mut prev
            ),
            vec![0]
        );
    }

    #[test]
    fn grid_scan_multiple_rules_independent_edges() {
        let rules = vec![crule("a", "rate limited"), crule("b", "overloaded")];
        let mut prev: HashMap<String, bool> = HashMap::new();
        // Only rule b visible -> fires index 1 only.
        assert_eq!(
            collect_rising_edges("server overloaded", &rules, &mut prev),
            vec![1]
        );
        // Now both visible -> only the newly-appeared rule a (index 0) fires.
        assert_eq!(
            collect_rising_edges("rate limited and overloaded", &rules, &mut prev),
            vec![0]
        );
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
