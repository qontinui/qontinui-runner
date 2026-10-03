//! Per-terminal session metrics — context usage and account headroom (plan
//! `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phase 6, the **no-statusline variant**).
//!
//! Phase 1 measured that a registered `statusLine` removes `esc to interrupt`
//! and `? for shortcuts` on CLI 2.1.285, so the runner never registers one and
//! nothing here reads statusline JSON. Every figure comes from a source the
//! runner already has:
//!
//! | Field group | Source | Where |
//! |---|---|---|
//! | context used | `transcript` — `input + cache_creation + cache_read` of the LAST assistant record's `message.usage`, reset at a `compact_boundary` | [`ContextTracker`], fed by `transcript_watcher` |
//! | context used (fallback) | `grid` — the `… until auto-compact: N%` countdown the context watcher already scans | `context_watcher::scan_terminals_once` |
//! | 5-hour / 7-day headroom | `oauth_probe` — the runner's existing account usage probe | [`record_oauth_results`], called from `record_usage_snapshot` |
//! | 5-hour / 7-day headroom | `cached_usage` — `<CLAUDE_CONFIG_DIR>/.claude.json` `cachedUsageUtilization`, read read-only | [`evaluate_cached_usage`] |
//! | session cost | none — the statusline was the only source | always `null` |
//!
//! ## Absence is not zero
//!
//! Every field is `Option` and serializes as `null` when unknown. Nothing is
//! ever coerced to 0; a numeric range check drops the FIELD, never the whole
//! reading. The headroom freshness rules are the ones plan
//! `2026-09-03-provider-limit-kills-destroy-subagent-context-and-nothing-resumes`
//! Phase 4 measured (`qontinui-claude-config/scripts/account-budget.sh`): a
//! reading older than four hours is `stale`, a stamp more than 300 s in the
//! future is a wrong clock, a `resets_at` in the past is `expired_reset`, and
//! a missing key is `absent` — each is UNKNOWN, never "full" and never
//! "exhausted".
//!
//! ## Publication
//!
//! A change is published as the Tauri event [`EVENT_NAME`]
//! (`{ terminalId, metrics }`) plus the same WS re-broadcast
//! `terminal-agent-state` uses; `get_terminal_agent_metrics` and
//! `GET /terminals/agent-state` (`metrics` on each row) are the initial reads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::Value;

use crate::commands::ai_settings::AccountUsageInfo;

/// The Tauri event (and WS channel) a metrics change is published on.
pub const EVENT_NAME: &str = "terminal-agent-metrics";

/// A headroom reading older than this is `stale` ⇒ UNKNOWN. Four hours, the
/// bound `account-budget.sh` settled on: inside the five-hour window, so one
/// reading never spans two windows, and loose enough that an idle roster is
/// not rendered wholly unreadable.
pub const HEADROOM_MAX_AGE_MS: u64 = 4 * 60 * 60 * 1000;

/// A stamp further ahead of the clock than this is a wrong clock, not a fresh
/// reading.
pub const FUTURE_SKEW_MS: u64 = 300_000;

/// A `resets_at` further ahead than this is not a real window boundary (the
/// longest window is seven days) — the field is dropped.
pub const RESETS_AT_MAX_AHEAD_MS: u64 = 8 * 24 * 60 * 60 * 1000;

/// Context-token sums above this are not a real context — the field is
/// dropped.
const MAX_CONTEXT_TOKENS: u64 = 10_000_000;

/// The standard Claude context window.
const WINDOW_200K: u64 = 200_000;
/// The extended (`[1m]`) Claude context window.
const WINDOW_1M: u64 = 1_000_000;

/// How often a terminal's account (`config_dir`) is re-resolved from the
/// session-lifecycle store.
const ACCOUNT_RECHECK: Duration = Duration::from_secs(30);

/// How often a config dir's `.claude.json` is re-stat'ed.
const CACHED_USAGE_RECHECK: Duration = Duration::from_secs(30);

/// Largest `.claude.json` the cached-usage reader will parse.
const MAX_CLAUDE_JSON_BYTES: u64 = 32 * 1024 * 1024;

/// Five-hour samples kept per account for the burn-rate projection.
const MAX_HISTORY: usize = 64;

/// Samples older than this are pruned from the history.
const HISTORY_RETENTION_MS: u64 = 6 * 60 * 60 * 1000;

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The wire shape
// ---------------------------------------------------------------------------

/// Where a context reading came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Transcript,
    Grid,
}

/// Where a headroom reading came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadroomSource {
    OauthProbe,
    CachedUsage,
}

/// One terminal's metrics. Serializes camelCase EXACTLY as the frontend codes
/// against:
///
/// ```text
/// { contextUsedPct, contextTokens, contextWindow, contextSource,
///   contextObservedAtMs, costUsd, fiveHourPct, fiveHourResetsAt,
///   sevenDayPct, headroomSource, headroomObservedAtMs }
/// ```
///
/// Every unknown field is `null`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetrics {
    pub context_used_pct: Option<f64>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub context_source: Option<ContextSource>,
    pub context_observed_at_ms: Option<u64>,
    /// Always `null`: the statusline was the only cost source and it is not
    /// registered (Phase 1 Q1). A unit type, so no code path can invent one.
    pub cost_usd: (),
    pub five_hour_pct: Option<f64>,
    /// RFC 3339.
    pub five_hour_resets_at: Option<String>,
    pub seven_day_pct: Option<f64>,
    pub headroom_source: Option<HeadroomSource>,
    pub headroom_observed_at_ms: Option<u64>,
}

/// The `terminal-agent-metrics` payload and one `get_terminal_agent_metrics`
/// row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentMetrics {
    pub terminal_id: String,
    pub metrics: SessionMetrics,
}

/// A percentage inside `0..=100`, else `None` (the range drop).
fn pct_in_range(p: f64) -> Option<f64> {
    (p.is_finite() && (0.0..=100.0).contains(&p)).then_some(p)
}

/// Round to one decimal — the display grain; keeps change detection from
/// firing on float noise.
fn round1(p: f64) -> f64 {
    (p * 10.0).round() / 10.0
}

// ---------------------------------------------------------------------------
// Context usage
// ---------------------------------------------------------------------------

/// One context reading.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextReading {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    pub used_pct: Option<f64>,
    pub source: ContextSource,
    pub observed_at_ms: u64,
}

impl ContextReading {
    /// A grid reading from the `… until auto-compact: N%` countdown: used is
    /// `100 − N`, measured against the auto-compact point rather than the full
    /// window (the `grid` source label says so). `N > 100` is dropped.
    pub fn from_grid_until_autocompact(left_pct: u32, observed_at_ms: u64) -> Option<Self> {
        let left = f64::from(left_pct);
        let used = pct_in_range(100.0 - left)?;
        Some(Self {
            tokens: None,
            window: None,
            used_pct: Some(used),
            source: ContextSource::Grid,
            observed_at_ms,
        })
    }
}

/// The context window a model id implies, when that is unambiguous.
///
/// Transcripts record the API model id (`claude-opus-5-5`), which does NOT
/// say whether the 1M window is on — so a bare id is ambiguous and yields
/// `None`. Unambiguous cases: an id carrying the `[1m]` marker, or a token
/// count a 200k window could not hold.
pub fn context_window_for(model: Option<&str>, tokens: u64) -> Option<u64> {
    if model.is_some_and(|m| m.to_ascii_lowercase().contains("[1m]")) || tokens > WINDOW_200K {
        return Some(WINDOW_1M);
    }
    None
}

/// What one transcript line says about context.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LineContext {
    /// `{"type":"system","subtype":"compact_boundary"}` — the context was
    /// compacted; the previous sum no longer describes it.
    Compacted,
    /// A top-level assistant record's usage.
    Usage { tokens: u64, model: Option<String> },
}

/// A `u64` field that is either absent (counts as 0 — the CLI omits a cache
/// field that did not apply) or a non-negative integer. `Err` when present
/// but not a count (the record is then ignored).
fn count_field(usage: &serde_json::Map<String, Value>, key: &str) -> Result<u64, ()> {
    match usage.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_u64().ok_or(()),
    }
}

/// Classify one parsed transcript record. Pure.
fn line_context(v: &Value) -> Option<LineContext> {
    let ty = v.get("type").and_then(Value::as_str)?;
    if ty == "system" {
        return (v.get("subtype").and_then(Value::as_str) == Some("compact_boundary"))
            .then_some(LineContext::Compacted);
    }
    if ty != "assistant" || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let msg = v.get("message")?;
    let model = msg.get("model").and_then(Value::as_str);
    // Synthetic turns (API-error placeholders) carry an all-zero usage that
    // describes no context.
    if model == Some("<synthetic>") {
        return None;
    }
    let usage = msg.get("usage")?.as_object()?;
    // `input_tokens` is required; the two cache counts may be absent.
    let input = usage.get("input_tokens")?.as_u64()?;
    let cc = count_field(usage, "cache_creation_input_tokens").ok()?;
    let cr = count_field(usage, "cache_read_input_tokens").ok()?;
    let tokens = input.checked_add(cc)?.checked_add(cr)?;
    Some(LineContext::Usage {
        tokens,
        model: model.map(str::to_string),
    })
}

/// Per-transcript context state: the last assistant usage, reset at a
/// compaction. One per transcript tail.
#[derive(Debug, Default, Clone)]
pub struct ContextTracker {
    tokens: Option<u64>,
    model: Option<String>,
    observed_at_ms: Option<u64>,
}

impl ContextTracker {
    /// Offer one transcript line. `true` when the reading changed. A cheap
    /// substring prefilter runs before any JSON parse: only lines naming
    /// `"usage"` or `compact_boundary` are parsed.
    pub fn observe_line(&mut self, line: &str, now_ms: u64) -> bool {
        if !line.contains("\"usage\"") && !line.contains("compact_boundary") {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        match line_context(&v) {
            None => false,
            Some(LineContext::Compacted) => {
                let changed = self.tokens.is_some();
                self.tokens = None;
                self.observed_at_ms = Some(now_ms);
                changed
            }
            Some(LineContext::Usage { tokens, model }) => {
                // Range check: an absurd sum is dropped, not clamped.
                let tokens = (tokens <= MAX_CONTEXT_TOKENS).then_some(tokens);
                if model.is_some() {
                    self.model = model;
                }
                let changed = self.tokens != tokens;
                self.tokens = tokens;
                self.observed_at_ms = Some(now_ms);
                changed
            }
        }
    }

    /// The current reading. `None` before any usage record, and after a
    /// compaction until the next one.
    pub fn reading(&self) -> Option<ContextReading> {
        let tokens = self.tokens?;
        let window = context_window_for(self.model.as_deref(), tokens);
        let used_pct = window.and_then(|w| {
            // Token counts are range-checked to ≤ 10M: exactly representable.
            let p = tokens as f64 * 100.0 / w as f64;
            pct_in_range(p).map(round1)
        });
        Some(ContextReading {
            tokens: Some(tokens),
            window,
            used_pct,
            source: ContextSource::Transcript,
            observed_at_ms: self.observed_at_ms?,
        })
    }
}

// ---------------------------------------------------------------------------
// Headroom
// ---------------------------------------------------------------------------

/// One account's headroom as a source reported it (raw — freshness is applied
/// at READ time by [`apply_freshness`], so a stored reading ages out).
#[derive(Debug, Clone, PartialEq)]
pub struct HeadroomReading {
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at_ms: Option<u64>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at_ms: Option<u64>,
    pub source: HeadroomSource,
    pub observed_at_ms: u64,
}

/// Why a headroom reading is UNKNOWN. The reasons `account-budget.sh` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadroomUnknown {
    /// No `.claude.json` for the account.
    NoFile,
    /// Unreadable, oversize or not a JSON object.
    Unreadable,
    /// No `cachedUsageUtilization` key — the account was never used here.
    Absent,
    /// No usable `fetchedAtMs`.
    Unstamped,
    /// Stamped more than [`FUTURE_SKEW_MS`] ahead of the clock.
    FutureStamp,
    /// Older than [`HEADROOM_MAX_AGE_MS`].
    Stale,
    /// Every limit's `resets_at` is in the past.
    ExpiredReset,
    /// No limit survived (none present, or each failed its range check).
    NoLimits,
}

/// What a limit's (pct, resets_at) pair is worth after the rules.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LimitCheck {
    Kept(f64, Option<u64>),
    Absent,
    Expired,
    Invalid,
}

/// One limit through the range and reset rules. Pure.
fn check_limit(pct: Option<f64>, resets_at_ms: Option<u64>, now_ms: u64) -> LimitCheck {
    let Some(pct) = pct else {
        return LimitCheck::Absent;
    };
    let Some(pct) = pct_in_range(pct) else {
        return LimitCheck::Invalid;
    };
    match resets_at_ms {
        Some(r) if r <= now_ms => LimitCheck::Expired,
        Some(r) if r > now_ms.saturating_add(RESETS_AT_MAX_AHEAD_MS) => LimitCheck::Invalid,
        Some(r) => LimitCheck::Kept(round1(pct), Some(r)),
        // A used window with no reset is unknowable; an unused one is a known
        // idle (nothing used, nothing to wait for).
        None if pct <= 0.0 => LimitCheck::Kept(0.0, None),
        None => LimitCheck::Invalid,
    }
}

/// The freshness + per-field rules, applied at read time. Pure.
pub fn apply_freshness(
    r: &HeadroomReading,
    now_ms: u64,
) -> Result<HeadroomReading, HeadroomUnknown> {
    if r.observed_at_ms > now_ms.saturating_add(FUTURE_SKEW_MS) {
        return Err(HeadroomUnknown::FutureStamp);
    }
    if now_ms.saturating_sub(r.observed_at_ms) > HEADROOM_MAX_AGE_MS {
        return Err(HeadroomUnknown::Stale);
    }
    let five = check_limit(r.five_hour_pct, r.five_hour_resets_at_ms, now_ms);
    let seven = check_limit(r.seven_day_pct, r.seven_day_resets_at_ms, now_ms);
    let split = |c: LimitCheck| match c {
        LimitCheck::Kept(p, reset) => (Some(p), reset),
        _ => (None, None),
    };
    let (five_hour_pct, five_hour_resets_at_ms) = split(five);
    let (seven_day_pct, seven_day_resets_at_ms) = split(seven);
    if five_hour_pct.is_none() && seven_day_pct.is_none() {
        let expired = matches!(five, LimitCheck::Expired) || matches!(seven, LimitCheck::Expired);
        return Err(if expired {
            HeadroomUnknown::ExpiredReset
        } else {
            HeadroomUnknown::NoLimits
        });
    }
    Ok(HeadroomReading {
        five_hour_pct,
        five_hour_resets_at_ms,
        seven_day_pct,
        seven_day_resets_at_ms,
        source: r.source,
        observed_at_ms: r.observed_at_ms,
    })
}

/// An RFC 3339 timestamp as unix millis.
fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(s.trim()).ok()?;
    u64::try_from(dt.timestamp_millis()).ok()
}

/// A limit object's `(utilization, resets_at)` from `cachedUsageUtilization`.
fn cached_limit(util: &Value, key: &str) -> (Option<f64>, Option<u64>) {
    let Some(obj) = util.get(key).filter(|v| v.is_object()) else {
        return (None, None);
    };
    let pct = obj.get("utilization").and_then(Value::as_f64);
    let reset = obj
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_ms);
    (pct, reset)
}

/// Parse `cachedUsageUtilization` out of a `.claude.json` document (raw — no
/// freshness applied). Pure.
pub fn parse_cached_usage(doc: &Value) -> Result<HeadroomReading, HeadroomUnknown> {
    if !doc.is_object() {
        return Err(HeadroomUnknown::Unreadable);
    }
    let cached = doc
        .get("cachedUsageUtilization")
        .filter(|v| v.is_object())
        .ok_or(HeadroomUnknown::Absent)?;
    let stamp = cached
        .get("fetchedAtMs")
        .filter(|v| v.is_number())
        .and_then(Value::as_f64)
        .filter(|f| f.is_finite() && *f >= 0.0)
        .ok_or(HeadroomUnknown::Unstamped)?;
    // Finite and non-negative (checked above); epoch millis fit u64.
    let observed_at_ms = stamp as u64;
    let util = cached.get("utilization").cloned().unwrap_or(Value::Null);
    let (five_hour_pct, five_hour_resets_at_ms) = cached_limit(&util, "five_hour");
    let (seven_day_pct, seven_day_resets_at_ms) = cached_limit(&util, "seven_day");
    Ok(HeadroomReading {
        five_hour_pct,
        five_hour_resets_at_ms,
        seven_day_pct,
        seven_day_resets_at_ms,
        source: HeadroomSource::CachedUsage,
        observed_at_ms,
    })
}

/// [`parse_cached_usage`] then [`apply_freshness`]. Pure.
pub fn evaluate_cached_usage(
    doc: &Value,
    now_ms: u64,
) -> Result<HeadroomReading, HeadroomUnknown> {
    parse_cached_usage(doc).and_then(|r| apply_freshness(&r, now_ms))
}

/// The runner's own usage-probe result as a headroom reading. `None` for a
/// failed probe (its `utilization: 1.0` is an error placeholder, not a
/// measurement). The weekly figure is taken only with its reset time: the
/// probe coerces an absent weekly window to 0.0, which must not render as 0%.
pub fn reading_from_usage_info(info: &AccountUsageInfo, observed_at_ms: u64) -> Option<HeadroomReading> {
    if info.error.is_some() {
        return None;
    }
    let secs_to_ms = |s: u64| s.saturating_mul(1000);
    Some(HeadroomReading {
        five_hour_pct: info.session_utilization.map(|u| u * 100.0),
        five_hour_resets_at_ms: info.session_resets_at.map(secs_to_ms),
        seven_day_pct: info.resets_at.map(|_| info.utilization * 100.0),
        seven_day_resets_at_ms: info.resets_at.map(secs_to_ms),
        source: HeadroomSource::OauthProbe,
        observed_at_ms,
    })
}

/// Of two usable readings, the one observed most recently.
fn fresher(a: Option<HeadroomReading>, b: Option<HeadroomReading>) -> Option<HeadroomReading> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.observed_at_ms > a.observed_at_ms { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// One five-hour utilization sample, for the burn-rate projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FiveHourSample {
    pub at_ms: u64,
    pub pct: f64,
    pub resets_at_ms: u64,
}

#[derive(Debug, Default)]
struct AccountHeadroom {
    oauth: Option<HeadroomReading>,
    history: Vec<FiveHourSample>,
}

impl AccountHeadroom {
    fn push_sample(&mut self, r: &HeadroomReading) {
        let (Some(pct), Some(resets_at_ms)) = (r.five_hour_pct, r.five_hour_resets_at_ms) else {
            return;
        };
        if self.history.iter().any(|s| s.at_ms == r.observed_at_ms) {
            return;
        }
        self.history.push(FiveHourSample {
            at_ms: r.observed_at_ms,
            pct,
            resets_at_ms,
        });
        self.history.sort_by_key(|s| s.at_ms);
        let newest = self.history.last().map_or(0, |s| s.at_ms);
        self.history
            .retain(|s| newest.saturating_sub(s.at_ms) <= HISTORY_RETENTION_MS);
        if self.history.len() > MAX_HISTORY {
            let excess = self.history.len() - MAX_HISTORY;
            self.history.drain(..excess);
        }
    }
}

/// Per-account headroom, keyed by `config_dir` exactly as the usage snapshot
/// keys it.
static ACCOUNTS: Mutex<Option<HashMap<String, AccountHeadroom>>> = Mutex::new(None);

/// Record the runner's probe results (called from
/// `commands::ai_settings::record_usage_snapshot`, so every probe caller —
/// the 10-minute loop, the migration confirm, the settings UI — feeds it).
pub fn record_oauth_results(results: &[AccountUsageInfo]) {
    let now = now_ms();
    let mut guard = ACCOUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    for info in results {
        let Some(reading) = reading_from_usage_info(info, now) else {
            continue;
        };
        let entry = map.entry(info.config_dir.clone()).or_default();
        entry.push_sample(&reading);
        entry.oauth = Some(reading);
    }
}

/// The account's five-hour samples, oldest first.
pub fn five_hour_history(config_dir: &str) -> Vec<FiveHourSample> {
    let guard = ACCOUNTS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .and_then(|m| m.get(config_dir))
        .map(|a| a.history.clone())
        .unwrap_or_default()
}

// ── the read-only `.claude.json` reader ─────────────────────────────────────

#[derive(Debug, Clone)]
struct CachedFileEntry {
    checked_at: Instant,
    mtime: Option<SystemTime>,
    parsed: Result<HeadroomReading, HeadroomUnknown>,
}

static CACHED_FILES: Mutex<Option<HashMap<String, CachedFileEntry>>> = Mutex::new(None);

/// The `.claude.json` paths for a config dir, in lookup order. A
/// `CLAUDE_CONFIG_DIR` holds its own `.claude.json`; the default `~/.claude`
/// account's lives beside it at `~/.claude.json`.
fn claude_json_candidates(config_dir: &str) -> Vec<PathBuf> {
    let dir = PathBuf::from(config_dir);
    let mut out = vec![dir.join(".claude.json")];
    if let Some(home) = dirs::home_dir() {
        if dir == home.join(".claude") {
            out.push(home.join(".claude.json"));
        }
    }
    out
}

/// Read and parse one account's `cachedUsageUtilization`, re-reading the file
/// only when its mtime moved. READ-ONLY — never writes any `~/.claude*` file.
fn cached_usage_raw(config_dir: &str) -> Result<HeadroomReading, HeadroomUnknown> {
    {
        let guard = CACHED_FILES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = guard.as_ref().and_then(|m| m.get(config_dir)) {
            if e.checked_at.elapsed() < CACHED_USAGE_RECHECK {
                return e.parsed.clone();
            }
        }
    }
    let path = claude_json_candidates(config_dir)
        .into_iter()
        .find(|p| p.is_file());
    let meta = path.as_ref().and_then(|p| std::fs::metadata(p).ok());
    let mtime = meta.as_ref().and_then(|m| m.modified().ok());
    let unchanged = {
        let guard = CACHED_FILES.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(config_dir))
            .filter(|e| mtime.is_some() && e.mtime == mtime)
            .map(|e| e.parsed.clone())
    };
    let parsed = match (unchanged, path, meta) {
        (Some(prev), _, _) => prev,
        (None, None, _) | (None, Some(_), None) => Err(HeadroomUnknown::NoFile),
        (None, Some(_), Some(m)) if m.len() > MAX_CLAUDE_JSON_BYTES => {
            Err(HeadroomUnknown::Unreadable)
        }
        (None, Some(p), Some(_)) => std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .ok_or(HeadroomUnknown::Unreadable)
            .and_then(|doc| parse_cached_usage(&doc)),
    };
    let mut guard = CACHED_FILES.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(
        config_dir.to_string(),
        CachedFileEntry {
            checked_at: Instant::now(),
            mtime,
            parsed: parsed.clone(),
        },
    );
    parsed
}

/// The freshest usable headroom reading for an account, from either source.
/// `None` when both are UNKNOWN. A usable reading's five-hour figure also
/// joins the burn-rate history.
pub fn account_headroom(config_dir: &str, now_ms: u64) -> Option<HeadroomReading> {
    let cached = cached_usage_raw(config_dir)
        .and_then(|r| apply_freshness(&r, now_ms))
        .ok();
    let mut guard = ACCOUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = guard
        .get_or_insert_with(HashMap::new)
        .entry(config_dir.to_string())
        .or_default();
    let oauth = entry
        .oauth
        .as_ref()
        .and_then(|r| apply_freshness(r, now_ms).ok());
    let best = fresher(oauth, cached);
    if let Some(r) = &best {
        entry.push_sample(r);
    }
    best
}

// ---------------------------------------------------------------------------
// The per-terminal slot
// ---------------------------------------------------------------------------

/// Everything the runner holds about one pane's metrics. Lives inside the
/// pane's `AgentStateSlot`.
#[derive(Debug, Default)]
pub struct MetricsSlot {
    transcript: Option<ContextReading>,
    grid: Option<ContextReading>,
    /// `(resolved_at, config_dir)`.
    account: Option<(Instant, Option<String>)>,
    published: Option<SessionMetrics>,
}

impl MetricsSlot {
    /// The transcript's reading (`None` after a compaction reset).
    pub fn set_transcript(&mut self, r: Option<ContextReading>) {
        self.transcript = r;
    }

    pub fn set_grid(&mut self, r: Option<ContextReading>) {
        self.grid = r;
    }

    /// The context reading to show. Ranked: a transcript reading with a
    /// percentage, then the grid, then a token-only transcript reading.
    pub fn context(&self) -> Option<&ContextReading> {
        let t = self.transcript.as_ref();
        t.filter(|r| r.used_pct.is_some())
            .or(self.grid.as_ref())
            .or(t)
    }

    /// Remaining context % from the transcript, when its window is known —
    /// the signal `context_watcher` ranks above the grid scan.
    pub fn transcript_remaining_pct(&self) -> Option<f64> {
        self.transcript
            .as_ref()
            .and_then(|r| r.used_pct)
            .map(|u| 100.0 - u)
    }

    fn account_due(&self) -> bool {
        self.account
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= ACCOUNT_RECHECK)
    }

    fn account(&self) -> Option<String> {
        self.account.as_ref().and_then(|(_, a)| a.clone())
    }

    /// True (and remembered) when `m` differs from what was last published.
    pub fn take_if_changed(&mut self, m: &SessionMetrics) -> bool {
        if self.published.as_ref() == Some(m) {
            return false;
        }
        self.published = Some(m.clone());
        true
    }
}

/// Build the wire shape from the ranked context and the headroom. Pure.
pub fn assemble(
    context: Option<&ContextReading>,
    headroom: Option<&HeadroomReading>,
) -> SessionMetrics {
    let ms_to_rfc3339 = |ms: u64| {
        i64::try_from(ms)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    };
    SessionMetrics {
        context_used_pct: context.and_then(|c| c.used_pct).and_then(pct_in_range),
        context_tokens: context.and_then(|c| c.tokens),
        context_window: context.and_then(|c| c.window),
        context_source: context.map(|c| c.source),
        context_observed_at_ms: context.map(|c| c.observed_at_ms),
        cost_usd: (),
        five_hour_pct: headroom.and_then(|h| h.five_hour_pct),
        five_hour_resets_at: headroom
            .and_then(|h| h.five_hour_resets_at_ms)
            .and_then(ms_to_rfc3339),
        seven_day_pct: headroom.and_then(|h| h.seven_day_pct),
        headroom_source: headroom.map(|h| h.source),
        headroom_observed_at_ms: headroom.map(|h| h.observed_at_ms),
    }
}

/// The account a pane runs as: the session-lifecycle store's sticky
/// `config_dir`, else the runner's resolved global dir (the same fallback
/// `account_migration` uses). `None` for a pane with no registered session.
fn resolve_terminal_account(terminal_id: &str) -> Option<String> {
    use tauri::Manager;
    let app = crate::tauri_app_handle::current()?;
    let store =
        app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()?;
    let record = store.find_open_by_terminal(terminal_id)?;
    record
        .config_dir
        .filter(|d| !d.trim().is_empty())
        .or_else(crate::ai_provider::get_resolved_config_dir)
}

/// One pane's computed metrics.
#[derive(Debug, Clone)]
pub struct ComputedMetrics {
    pub metrics: SessionMetrics,
    pub account: Option<String>,
    pub headroom: Option<HeadroomReading>,
}

/// Compute one pane's metrics. With `consume`, also report whether they
/// changed since the last publish (and remember them as published). All IO
/// (lifecycle lookup, `.claude.json` stat) happens outside the slot lock.
pub fn compute(
    session: &crate::terminal::session::TerminalSession,
    terminal_id: &str,
    consume: bool,
) -> (ComputedMetrics, bool) {
    let now = now_ms();
    let slot = session.agent_state_slot();
    let (due, cached_account) = {
        let g = slot.lock().unwrap_or_else(|e| e.into_inner());
        (g.metrics().account_due(), g.metrics().account())
    };
    let account = if due {
        resolve_terminal_account(terminal_id)
    } else {
        cached_account
    };
    let headroom = account.as_deref().and_then(|a| account_headroom(a, now));

    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.metrics_mut();
    if due {
        m.account = Some((Instant::now(), account.clone()));
    }
    let metrics = assemble(m.context(), headroom.as_ref());
    let changed = consume && m.take_if_changed(&metrics);
    (
        ComputedMetrics {
            metrics,
            account,
            headroom,
        },
        changed,
    )
}

/// Publish a metrics change.
pub fn emit(app: &tauri::AppHandle, event: &TerminalAgentMetrics) {
    use tauri::Emitter;
    if let Err(e) = app.emit(EVENT_NAME, event) {
        tracing::warn!(terminal_id = %event.terminal_id, error = %e, "agent-metrics: emit failed");
    }
    if let Ok(payload) = serde_json::to_value(event) {
        crate::event_system::broadcast_ws_notification(app, EVENT_NAME, &payload);
    }
}

/// Every live pane's metrics, for `get_terminal_agent_metrics`.
pub fn read_all(tm: &crate::terminal::TerminalManager) -> Vec<TerminalAgentMetrics> {
    tm.sessions_snapshot()
        .into_iter()
        .map(|(id, session)| {
            let (c, _) = compute(&session, &id, false);
            TerminalAgentMetrics {
                terminal_id: id,
                metrics: c.metrics,
            }
        })
        .collect()
}

/// A transcript tail's context reading changed: store it on the pane that
/// runs Claude session `session_id` (if any) and publish.
pub fn record_transcript_context(session_id: &str, reading: Option<ContextReading>) {
    use crate::terminal::agent_state::SlotDirectory;
    use tauri::Manager;
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let Some((terminal_id, slot)) = tm.by_session_id(session_id) else {
        return;
    };
    {
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        g.metrics_mut().set_transcript(reading);
    }
    crate::terminal::agent_state::publish_terminal(&terminal_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_900_000_000_000;
    const HOUR: u64 = 60 * 60 * 1000;

    fn assistant(input: u64, cc: u64, cr: u64, model: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "model": model,
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": cc,
                    "cache_read_input_tokens": cr,
                    "output_tokens": 999_999,
                }
            }
        })
        .to_string()
    }

    fn compact() -> String {
        json!({"type": "system", "subtype": "compact_boundary", "content": "Conversation compacted"})
            .to_string()
    }

    #[test]
    fn agent_metrics_transcript_formula_is_the_last_assistant_three_field_sum() {
        let mut t = ContextTracker::default();
        assert!(t.reading().is_none(), "nothing seen ⇒ no reading");
        assert!(t.observe_line(&assistant(2, 98_598, 24_754, "claude-opus-5-5"), NOW));
        let r = t.reading().unwrap();
        // output_tokens is NOT part of the context sum.
        assert_eq!(r.tokens, Some(2 + 98_598 + 24_754));
        assert_eq!(r.source, ContextSource::Transcript);
        assert_eq!(r.observed_at_ms, NOW);
        // A bare model id is ambiguous about the window ⇒ no percentage.
        assert_eq!(r.window, None);
        assert_eq!(r.used_pct, None);

        // The LAST assistant record wins.
        assert!(t.observe_line(&assistant(10, 0, 50_000, "claude-opus-5-5[1m]"), NOW + 1));
        let r = t.reading().unwrap();
        assert_eq!(r.tokens, Some(50_010));
        assert_eq!(r.window, Some(1_000_000));
        assert_eq!(r.used_pct, Some(5.0));

        // A sum a 200k window cannot hold implies the 1M window.
        assert!(t.observe_line(&assistant(1, 0, 400_000, "claude-opus-5-5"), NOW + 2));
        let r = t.reading().unwrap();
        assert_eq!(r.window, Some(1_000_000));
        assert_eq!(r.used_pct, Some(40.0));
    }

    #[test]
    fn agent_metrics_transcript_compact_boundary_resets() {
        let mut t = ContextTracker::default();
        t.observe_line(&assistant(1, 0, 900_000, "claude-opus-5-5"), NOW);
        assert!(t.reading().is_some());
        assert!(t.observe_line(&compact(), NOW + 1), "a reset is a change");
        assert!(t.reading().is_none(), "compacted ⇒ unknown until next usage");
        assert!(!t.observe_line(&compact(), NOW + 2), "a second reset changes nothing");
        t.observe_line(&assistant(5, 30_000, 0, "claude-opus-5-5"), NOW + 3);
        assert_eq!(t.reading().unwrap().tokens, Some(30_005));
    }

    #[test]
    fn agent_metrics_transcript_prefilter_and_ignored_records() {
        let mut t = ContextTracker::default();
        // No "usage" / compact_boundary substring ⇒ never parsed.
        assert!(!t.observe_line(r#"{"type":"user","message":{"content":"hi"}}"#, NOW));
        // Not JSON at all.
        assert!(!t.observe_line(r#"{"usage": oops"#, NOW));
        // Synthetic error turn.
        assert!(!t.observe_line(&assistant(0, 0, 0, "<synthetic>"), NOW));
        // Sidechain (subagent) record.
        let side = json!({"type":"assistant","isSidechain":true,
            "message":{"model":"claude-x","usage":{"input_tokens":5}}})
        .to_string();
        assert!(!t.observe_line(&side, NOW));
        // Negative / non-integer count ⇒ record ignored, never coerced.
        let bad = json!({"type":"assistant","message":{"model":"claude-x",
            "usage":{"input_tokens":5,"cache_read_input_tokens":-3}}})
        .to_string();
        assert!(!t.observe_line(&bad, NOW));
        // Missing input_tokens ⇒ ignored.
        let missing = json!({"type":"assistant","message":{"usage":{"cache_read_input_tokens":3}}})
            .to_string();
        assert!(!t.observe_line(&missing, NOW));
        assert!(t.reading().is_none());
        // Absent cache fields count as zero cache, not as unknown input.
        let plain = json!({"type":"assistant","message":{"model":"claude-x","usage":{"input_tokens":7}}})
            .to_string();
        assert!(t.observe_line(&plain, NOW));
        assert_eq!(t.reading().unwrap().tokens, Some(7));
    }

    #[test]
    fn agent_metrics_range_check_drops_the_field_not_the_payload() {
        let mut t = ContextTracker::default();
        t.observe_line(&assistant(1, 0, 20_000_000, "claude-x"), NOW);
        assert!(t.reading().is_none(), "absurd token sum dropped");

        // Headroom: an out-of-range 5h pct drops only that field.
        let r = HeadroomReading {
            five_hour_pct: Some(250.0),
            five_hour_resets_at_ms: Some(NOW + HOUR),
            seven_day_pct: Some(36.0),
            seven_day_resets_at_ms: Some(NOW + 48 * HOUR),
            source: HeadroomSource::OauthProbe,
            observed_at_ms: NOW,
        };
        let ok = apply_freshness(&r, NOW).unwrap();
        assert_eq!(ok.five_hour_pct, None);
        assert_eq!(ok.five_hour_resets_at_ms, None);
        assert_eq!(ok.seven_day_pct, Some(36.0));
        // A reset more than 8 days out drops that limit too.
        let far = HeadroomReading {
            seven_day_resets_at_ms: Some(NOW + 9 * 24 * HOUR),
            ..r.clone()
        };
        assert_eq!(apply_freshness(&far, NOW), Err(HeadroomUnknown::NoLimits));
        // Grid: a countdown over 100 is dropped.
        assert!(ContextReading::from_grid_until_autocompact(140, NOW).is_none());
        assert_eq!(
            ContextReading::from_grid_until_autocompact(8, NOW)
                .unwrap()
                .used_pct,
            Some(92.0)
        );
    }

    #[test]
    fn agent_metrics_null_never_zero() {
        let m = assemble(None, None);
        let v = serde_json::to_value(&m).unwrap();
        for key in [
            "contextUsedPct",
            "contextTokens",
            "contextWindow",
            "contextSource",
            "contextObservedAtMs",
            "costUsd",
            "fiveHourPct",
            "fiveHourResetsAt",
            "sevenDayPct",
            "headroomSource",
            "headroomObservedAtMs",
        ] {
            assert!(v.get(key).is_some(), "{key} is present");
            assert!(v[key].is_null(), "{key} is null, never 0: {v}");
        }
        assert_eq!(v.as_object().unwrap().len(), 11, "exactly the contract keys");

        // A failed probe (its utilization 1.0 is a placeholder) is no reading.
        let failed = AccountUsageInfo {
            utilization: 1.0,
            error: Some("Network error".into()),
            ..Default::default()
        };
        assert!(reading_from_usage_info(&failed, NOW).is_none());
        // A weekly figure with no reset time is the probe's 0.0 coercion.
        let partial = AccountUsageInfo {
            utilization: 0.0,
            session_utilization: Some(0.42),
            session_resets_at: Some((NOW + HOUR) / 1000),
            ..Default::default()
        };
        let r = reading_from_usage_info(&partial, NOW).unwrap();
        assert_eq!(r.seven_day_pct, None);
        assert_eq!(r.five_hour_pct, Some(42.0));
    }

    #[test]
    fn agent_metrics_wire_shape_is_the_frontend_contract() {
        let ctx = ContextReading {
            tokens: Some(50_010),
            window: Some(1_000_000),
            used_pct: Some(5.0),
            source: ContextSource::Transcript,
            observed_at_ms: NOW,
        };
        let head = HeadroomReading {
            five_hour_pct: Some(42.0),
            five_hour_resets_at_ms: Some(1_900_003_600_000),
            seven_day_pct: Some(36.0),
            seven_day_resets_at_ms: Some(NOW + 48 * HOUR),
            source: HeadroomSource::CachedUsage,
            observed_at_ms: NOW - 1000,
        };
        let ev = TerminalAgentMetrics {
            terminal_id: "t1".into(),
            metrics: assemble(Some(&ctx), Some(&head)),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["terminalId"], "t1");
        let m = &v["metrics"];
        assert_eq!(m["contextUsedPct"], 5.0);
        assert_eq!(m["contextTokens"], 50_010);
        assert_eq!(m["contextWindow"], 1_000_000);
        assert_eq!(m["contextSource"], "transcript");
        assert_eq!(m["contextObservedAtMs"], NOW);
        assert!(m["costUsd"].is_null());
        assert_eq!(m["fiveHourPct"], 42.0);
        assert_eq!(m["fiveHourResetsAt"], "2030-03-17T18:46:40Z");
        assert_eq!(m["sevenDayPct"], 36.0);
        assert_eq!(m["headroomSource"], "cached_usage");
        assert_eq!(m["headroomObservedAtMs"], NOW - 1000);
        let grid = serde_json::to_value(ContextSource::Grid).unwrap();
        assert_eq!(grid, "grid");
        assert_eq!(serde_json::to_value(HeadroomSource::OauthProbe).unwrap(), "oauth_probe");
    }

    fn cached_doc(fetched: u64, five: f64, five_reset: &str, seven: f64, seven_reset: &str) -> Value {
        json!({
            "numStartups": 3,
            "cachedUsageUtilization": {
                "accountUuid": "5ec1dbcc-0000",
                "fetchedAtMs": fetched,
                "utilization": {
                    "five_hour": {"utilization": five, "resets_at": five_reset,
                                  "limit_dollars": null},
                    "seven_day": {"utilization": seven, "resets_at": seven_reset},
                }
            }
        })
    }

    fn iso(ms: u64) -> String {
        chrono::DateTime::from_timestamp_millis(i64::try_from(ms).unwrap())
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn agent_metrics_cached_usage_fresh_reading_is_used() {
        let doc = cached_doc(NOW - 60_000, 55.0, &iso(NOW + HOUR), 36.0, &iso(NOW + 72 * HOUR));
        let r = evaluate_cached_usage(&doc, NOW).unwrap();
        assert_eq!(r.five_hour_pct, Some(55.0));
        assert_eq!(r.seven_day_pct, Some(36.0));
        assert_eq!(r.source, HeadroomSource::CachedUsage);
        assert_eq!(r.observed_at_ms, NOW - 60_000);
    }

    #[test]
    fn agent_metrics_cached_usage_stale_is_unknown() {
        // The vet's measured inversion case: a 15-hour-old 100 with a reset
        // 13 hours in the past is NOT an exhausted account.
        let doc = cached_doc(NOW - 15 * HOUR, 100.0, &iso(NOW - 13 * HOUR), 36.0, &iso(NOW + 72 * HOUR));
        assert_eq!(evaluate_cached_usage(&doc, NOW), Err(HeadroomUnknown::Stale));
        // Just past the four-hour bound.
        let doc = cached_doc(NOW - 4 * HOUR - 1, 10.0, &iso(NOW + HOUR), 1.0, &iso(NOW + HOUR));
        assert_eq!(evaluate_cached_usage(&doc, NOW), Err(HeadroomUnknown::Stale));
    }

    #[test]
    fn agent_metrics_cached_usage_absent_expired_future_unstamped() {
        assert_eq!(
            evaluate_cached_usage(&json!({"numStartups": 1}), NOW),
            Err(HeadroomUnknown::Absent)
        );
        assert_eq!(
            evaluate_cached_usage(&json!({"cachedUsageUtilization": {"utilization": {}}}), NOW),
            Err(HeadroomUnknown::Unstamped)
        );
        assert_eq!(
            evaluate_cached_usage(
                &json!({"cachedUsageUtilization": {"fetchedAtMs": true}}),
                NOW
            ),
            Err(HeadroomUnknown::Unstamped)
        );
        let future = cached_doc(NOW + 10 * 60_000, 5.0, &iso(NOW + HOUR), 5.0, &iso(NOW + HOUR));
        assert_eq!(evaluate_cached_usage(&future, NOW), Err(HeadroomUnknown::FutureStamp));
        // Every reset in the past ⇒ expired_reset, never "still exhausted".
        let expired = cached_doc(NOW - HOUR, 100.0, &iso(NOW - 60_000), 90.0, &iso(NOW - 1));
        assert_eq!(evaluate_cached_usage(&expired, NOW), Err(HeadroomUnknown::ExpiredReset));
        // One expired, one live ⇒ the live one survives alone.
        let half = cached_doc(NOW - HOUR, 100.0, &iso(NOW - 60_000), 30.0, &iso(NOW + HOUR));
        let r = evaluate_cached_usage(&half, NOW).unwrap();
        assert_eq!(r.five_hour_pct, None);
        assert_eq!(r.five_hour_resets_at_ms, None);
        assert_eq!(r.seven_day_pct, Some(30.0));
        // A used window with an unparsable reset is unknowable, not 0.
        let bad_reset = cached_doc(NOW - HOUR, 40.0, "tomorrow-ish", 30.0, &iso(NOW + HOUR));
        assert_eq!(evaluate_cached_usage(&bad_reset, NOW).unwrap().five_hour_pct, None);
        assert_eq!(
            evaluate_cached_usage(&json!([1, 2]), NOW),
            Err(HeadroomUnknown::Unreadable)
        );
    }

    #[test]
    fn agent_metrics_oauth_reading_ages_out_at_read_time() {
        let r = HeadroomReading {
            five_hour_pct: Some(10.0),
            five_hour_resets_at_ms: Some(NOW + 5 * HOUR),
            seven_day_pct: None,
            seven_day_resets_at_ms: None,
            source: HeadroomSource::OauthProbe,
            observed_at_ms: NOW,
        };
        assert!(apply_freshness(&r, NOW + HOUR).is_ok());
        assert_eq!(apply_freshness(&r, NOW + 4 * HOUR + 1), Err(HeadroomUnknown::Stale));
    }

    #[test]
    fn agent_metrics_fresher_source_wins() {
        let base = HeadroomReading {
            five_hour_pct: Some(10.0),
            five_hour_resets_at_ms: Some(NOW + HOUR),
            seven_day_pct: None,
            seven_day_resets_at_ms: None,
            source: HeadroomSource::OauthProbe,
            observed_at_ms: NOW - 600_000,
        };
        let newer = HeadroomReading {
            source: HeadroomSource::CachedUsage,
            observed_at_ms: NOW - 1000,
            ..base.clone()
        };
        assert_eq!(
            fresher(Some(base.clone()), Some(newer.clone())).unwrap().source,
            HeadroomSource::CachedUsage
        );
        assert_eq!(fresher(Some(base.clone()), None).unwrap().source, HeadroomSource::OauthProbe);
        assert_eq!(fresher(None, None), None);
    }

    #[test]
    fn agent_metrics_slot_ranks_transcript_above_grid() {
        let mut s = MetricsSlot::default();
        assert!(s.context().is_none());
        let grid = ContextReading::from_grid_until_autocompact(30, NOW).unwrap();
        s.set_grid(Some(grid.clone()));
        assert_eq!(s.context().unwrap().source, ContextSource::Grid);
        // A token-only transcript reading does not displace a grid percentage.
        let tokens_only = ContextReading {
            tokens: Some(90_000),
            window: None,
            used_pct: None,
            source: ContextSource::Transcript,
            observed_at_ms: NOW,
        };
        s.set_transcript(Some(tokens_only));
        assert_eq!(s.context().unwrap().source, ContextSource::Grid);
        assert_eq!(s.transcript_remaining_pct(), None);
        // A transcript percentage outranks the grid.
        let pct = ContextReading {
            tokens: Some(900_000),
            window: Some(1_000_000),
            used_pct: Some(90.0),
            source: ContextSource::Transcript,
            observed_at_ms: NOW,
        };
        s.set_transcript(Some(pct));
        assert_eq!(s.context().unwrap().source, ContextSource::Transcript);
        assert_eq!(s.transcript_remaining_pct(), Some(10.0));
        // Change detection.
        let m = assemble(s.context(), None);
        assert!(s.take_if_changed(&m));
        assert!(!s.take_if_changed(&m));
    }

    #[test]
    fn agent_metrics_history_dedupes_and_bounds() {
        let mut a = AccountHeadroom::default();
        let mut r = HeadroomReading {
            five_hour_pct: Some(10.0),
            five_hour_resets_at_ms: Some(NOW + HOUR),
            seven_day_pct: None,
            seven_day_resets_at_ms: None,
            source: HeadroomSource::OauthProbe,
            observed_at_ms: NOW,
        };
        a.push_sample(&r);
        a.push_sample(&r);
        assert_eq!(a.history.len(), 1, "same observation counted once");
        for i in 1..200 {
            r.observed_at_ms = NOW + i * 1000;
            a.push_sample(&r);
        }
        assert_eq!(a.history.len(), MAX_HISTORY);
        assert!(a.history.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
        // A reading with no five-hour reset is not a burn-rate sample.
        let mut b = AccountHeadroom::default();
        b.push_sample(&HeadroomReading {
            five_hour_resets_at_ms: None,
            ..r
        });
        assert!(b.history.is_empty());
    }
}
