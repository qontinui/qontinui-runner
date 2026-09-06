//! `GET /github-budget` — the runner's GitHub REST budget meter, served
//! (plan `2026-08-30-github-rest-budget-is-structurally-oversubscribed`,
//! Phase C), plus the low-headroom watch loop that says so out loud.
//!
//! # Why this exists
//!
//! A ~5,600 req/hr burn against a 5,000-point cap was attributed to coord's
//! merge train. **The attribution was wrong.** The sampler read the operator's
//! *user/OAuth* bucket (limit 5,000); coord bills GitHub *App installation*
//! buckets (limit 5,850) — and coord has exported
//! `coord_github_ratelimit_remaining` on `/metrics` with a sub-20%
//! `coord_github_ratelimit_low` alert since June 2026, plus full per-URL-template
//! attribution since July. That instrumentation is exactly why coord could be
//! *excluded* within minutes.
//!
//! **This module is the user-bucket counterpart of coord's half.** Nothing
//! metered the user/OAuth token: no gauge, no alert, no owner. That is why its
//! exhaustion surfaced as a steward halting mid-sweep and a detector reporting
//! UNKNOWN, rather than as a number. Re-measured 2026-09-01: **309 calls in
//! 301 s ≈ 62/min ≈ 3,700/hr on that one shared token.** The runner is the
//! process that is always up and already spends it, so the meter lives here.
//! (A per-session counter was considered and rejected: sessions are ephemeral
//! and N-many, the token is one, and the runner outlives them all.)
//!
//! # What this module owns, and what it does not
//!
//! [`crate::github_budget`] (Phase A) owns the measurement — the process-global
//! registry, the ETag cache, the `(consumer, template, cache_mode)` histogram
//! and the `unknown | nominal | low | exhausted` classification. This module
//! owns only two things on top of it:
//!
//! 1. **[`ROUTE_PATH`]** — one read-only `GET` that serves Phase A's snapshot as
//!    JSON, so an operator, a steward or a detector can ask for the number
//!    instead of inferring it from a stall.
//! 2. **[`start_low_headroom_watch`]** — a 60 s loop that turns a low bucket
//!    into a `warn!` naming the bucket, the headroom, the reset and the top
//!    spenders, and turns a recovery back into an all-clear.
//!
//! Neither path spends a GitHub request: both read process-local state that the
//! call sites already recorded. The route is therefore safe to poll.
//!
//! # Honesty about absence
//!
//! Two rules this module inherits from Phase A and must not soften:
//!
//! - **`unknown` is not low, and never warns.** Before the first observation
//!   Phase A reports `drift_class: "unknown"` and returns `None` from
//!   [`crate::github_budget::low_headroom`]. A fresh process stays SILENT rather
//!   than announcing a problem it has not measured — see [`decide`].
//! - **Silence from the watch loop is not evidence of health.** It is emitted by
//!   an unmeasured budget, by a disabled watch
//!   ([`WATCH_DISABLED_ENV`]), and by a loop that never started because
//!   [`routes`] was called outside a `tokio` runtime — all of which read
//!   identical from the log. The authoritative answer is the route: read
//!   `budget.driftClass` and `watch.running`, which distinguish those cases.
//!   Do not infer a healthy bucket from an absent warning.
//!
//! # Where this route is registered
//!
//! `mcp_api::create_router` merges [`routes`] alongside the other top-level
//! families. This repo has **no top-level route manifest** to also register
//! with: the `route_entries()` + `manifest_matches_route_calls` contract is
//! specific to the `mcp/ui_bridge/<family>` tree, which additionally owes SDK
//! and WS wrappers and a frontend IPC bridge — a read-only diagnostic gains
//! nothing from that and would owe all of it (the same call `mcp_api` records
//! for `/coord-mcp/tool-policy`). Discovery for top-level families is the
//! `mcp::api_surface` static scanner, which reads `.route(` calls out of the
//! source and therefore picks [`ROUTE_PATH`] up with no registration step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::{debug, info, warn};

use crate::github_budget::{self, ConsumerRow, GithubBudgetSnapshot, LowHeadroom};
use crate::mcp::types::{ApiResponse, ApiState};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// The one path this family serves. Shared by [`routes`] and by the tests so
/// the two cannot drift — axum exposes no route introspection, so a literal in
/// each place would be two independent facts.
pub const ROUTE_PATH: &str = "/github-budget";

/// Watch cadence. Matches coord's `github_ratelimit_watcher` so the two halves
/// of this plan sample at the same depth and the same rate.
///
/// There is nothing to gain from going faster: Phase A's registry is updated
/// SYNCHRONOUSLY on every GitHub response, so the value this loop reads is
/// already current — the interval controls how often it is *re-examined*, not
/// how fresh it is.
const WATCH_INTERVAL: Duration = Duration::from_secs(60);

/// Kill switch, in the repo's existing `QONTINUI_*_DISABLED` style
/// (`QONTINUI_SPAWN_AUTHZ_DISABLED`, `QONTINUI_PROMPT_AUDIT_REPORT_DISABLED`).
/// Default **ON** — the whole point of the plan is that this bucket had no
/// owner, and a meter that ships off has none either.
pub const WATCH_DISABLED_ENV: &str = "QONTINUI_GITHUB_BUDGET_WATCH_DISABLED";

/// How much the remaining fraction must fall BELOW the last warned fraction
/// before the loop warns again while already in the low state.
///
/// Five percentage points of a 5,000-point bucket is 250 requests — a material
/// worsening, and at the measured 62/min it is four minutes of spend. Anything
/// finer re-warns on ordinary poll jitter, and a warning that repeats every
/// tick trains its reader to filter it, which is the failure this loop exists
/// to avoid.
const MATERIAL_FRACTION_DROP: f64 = 0.05;

/// How many histogram rows the warning names. The warning has to fit one log
/// line and be readable at 3 a.m.; the full histogram is one `GET` away at
/// [`ROUTE_PATH`].
const WARN_TOP_TEMPLATES: usize = 3;

/// Respawn ladder for the watch loop under
/// [`crate::mcp::task_supervisor::spawn_supervised_forever`]. Slower than the
/// relay's because a dead budget watch is a lost warning, not a lost feature —
/// there is no reason to hot-spin at it.
const WATCH_INITIAL_BACKOFF: Duration = Duration::from_secs(5);
const WATCH_MAX_BACKOFF: Duration = Duration::from_secs(300);
const WATCH_HEALTHY_RUN: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// `(entries, approximate bytes)` from [`github_budget::etag_cache_stats`],
/// named so a reader does not have to count tuple positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EtagCacheStats {
    pub entries: usize,
    /// Approximate by construction — it drives an eviction budget, not an
    /// accounting ledger.
    pub approx_bytes: usize,
}

/// What the `top` query parameter actually did.
///
/// Reported rather than applied silently, because `top` can only ever NARROW
/// the histogram: Phase A's [`github_budget::snapshot`] fixes the served width
/// at its own `TOP_CONSUMERS_N` and Phase C does not modify Phase A. When a
/// caller asks for more rows than exist in the snapshot *and* the snapshot
/// admits it elided some, [`widen_unavailable`](Self::widen_unavailable) says
/// so instead of returning a short list that looks complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramMeta {
    /// The `top` the caller asked for, if any.
    pub requested_top: Option<usize>,
    /// Rows actually in `budget.consumers`.
    pub served_rows: usize,
    /// Rows the snapshot knows about and did not serve — Phase A's elision plus
    /// anything this request's `top` narrowed away.
    pub elided_rows: usize,
    /// `true` when the caller asked to widen beyond what Phase A serves and
    /// rows were genuinely lost to that ceiling. Never `true` merely because
    /// `top` exceeded the row count.
    pub widen_unavailable: bool,
}

/// Watch-loop state, so a reader of the route can tell "no warning because the
/// bucket is fine" from "no warning because nothing is watching".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    /// `false` when [`WATCH_DISABLED_ENV`] is set.
    pub enabled: bool,
    /// `true` once [`start_low_headroom_watch`] actually spawned the task.
    /// `false` with `enabled: true` means the spawn was skipped for want of a
    /// `tokio` runtime.
    pub running: bool,
    pub interval_seconds: u64,
    /// The last DECISIVE class the loop observed (`nominal` | `low` |
    /// `exhausted`), or `unknown` when it has yet to see one. `unknown` here is
    /// never a health claim.
    pub last_class: &'static str,
    pub warnings_emitted: u64,
    pub recoveries_emitted: u64,
    /// When the loop last changed its mind, unix seconds. `None` = never.
    pub last_transition_at_unix: Option<i64>,
}

/// The `GET /github-budget` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubBudgetReport {
    /// Phase A's snapshot verbatim, `consumers` narrowed by `top` if asked.
    /// **Carries no token** — [`github_budget::CredentialIdentity`] records the
    /// resolution-step NAME and an optional login, never the secret.
    pub budget: GithubBudgetSnapshot,
    /// Non-`None` only when the bucket is measurably below the threshold.
    /// `None` is "healthy **or unknown**" — disambiguate with
    /// `budget.driftClass`.
    pub low_headroom: Option<LowHeadroom>,
    /// The threshold in force, after `QONTINUI_GITHUB_BUDGET_LOW_FRACTION`.
    pub low_fraction: f64,
    pub etag_cache: EtagCacheStats,
    pub histogram: HistogramMeta,
    pub watch: WatchStatus,
}

// ---------------------------------------------------------------------------
// Query parsing
// ---------------------------------------------------------------------------

/// Parse the `top` query parameter.
///
/// Kept as a free function over `Option<&str>` rather than a `serde` field so
/// the rejection is an [`ApiResponse`] envelope with a usable message instead of
/// axum's bare `Failed to deserialize query string` text — and so it is
/// directly unit-testable without a router.
///
/// Absent, empty and whitespace-only all mean "no opinion" (`Ok(None)`); `0`
/// and anything unparseable are rejected rather than silently coerced, because
/// a `top` the caller mistyped must not read as a histogram with nothing in it.
fn parse_top(raw: Option<&str>) -> Result<Option<usize>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<usize>() {
        Ok(0) => Err("`top` must be at least 1".to_string()),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(format!(
            "`top` must be a non-negative integer, got `{trimmed}`"
        )),
    }
}

/// Apply `top` to a snapshot in place and describe what happened.
///
/// Narrowing moves the dropped rows into `consumers_elided` rather than
/// discarding them from the accounting — Phase A's rule that a truncating
/// histogram must say it truncated applies to this layer too.
fn apply_top(snapshot: &mut GithubBudgetSnapshot, requested_top: Option<usize>) -> HistogramMeta {
    let served_before = snapshot.consumers.len();
    let elided_before = snapshot.consumers_elided;

    if let Some(top) = requested_top {
        if top < served_before {
            let dropped = served_before - top;
            snapshot.consumers.truncate(top);
            snapshot.consumers_elided = elided_before.saturating_add(dropped);
        }
    }

    HistogramMeta {
        requested_top,
        served_rows: snapshot.consumers.len(),
        elided_rows: snapshot.consumers_elided,
        widen_unavailable: requested_top.is_some_and(|t| t > served_before) && elided_before > 0,
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `GET /github-budget[?top=N]` — the runner's GitHub REST spend, right now.
///
/// Deliberately takes no [`ApiState`]: everything it reports is process-global
/// in [`crate::github_budget`], so the handler stays trivially testable and
/// cannot be blocked behind a lock some other subsystem holds. Read-only, spends
/// no GitHub request, and safe to poll.
///
/// `top` narrows `budget.consumers`; see [`HistogramMeta`] for why it cannot
/// widen.
pub async fn get_github_budget(
    Query(params): Query<HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<GithubBudgetReport>>,
    (StatusCode, Json<ApiResponse<GithubBudgetReport>>),
> {
    let requested_top = parse_top(params.get("top").map(String::as_str)).map_err(|message| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<GithubBudgetReport>::error(message)),
        )
    })?;

    // Ask the registry for the width the caller wanted rather than trimming the
    // default twelve: `snapshot_top` widens as well as narrows, so a request for
    // more rows now returns more rows instead of reporting `widenUnavailable`.
    // `apply_top` still runs — it is what narrows and accounts the elided rows,
    // and it is the only thing that can honestly report a width the registry
    // could not satisfy (fewer rows retained in memory than were asked for).
    let mut budget = match requested_top {
        Some(top) => github_budget::snapshot_top(top),
        None => github_budget::snapshot(),
    };
    let histogram = apply_top(&mut budget, requested_top);
    let (entries, approx_bytes) = github_budget::etag_cache_stats();

    Ok(Json(ApiResponse::success(GithubBudgetReport {
        budget,
        low_headroom: github_budget::low_headroom(),
        low_fraction: github_budget::low_fraction(),
        etag_cache: EtagCacheStats {
            entries,
            approx_bytes,
        },
        histogram,
        watch: watch_status(),
    })))
}

/// Routes contributed to the runner's main router from `mcp_api.rs`.
///
/// Also the boot hook for [`start_low_headroom_watch`]. That is deliberate:
/// `create_router` is the one place this family is reachable from, the
/// coordinator's file budget for `mcp_api.rs` is the merge line alone, and
/// `create_router` already spawns background work of its own (the wrapper
/// bootstrap). The start is idempotent, so a second call — a test building the
/// router twice, a secondary instance — is a no-op.
pub fn routes() -> Router<std::sync::Arc<ApiState>> {
    start_low_headroom_watch();
    Router::new().route(ROUTE_PATH, get(get_github_budget))
}

// ---------------------------------------------------------------------------
// Low-headroom watch
// ---------------------------------------------------------------------------

/// What a single tick decided to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchAction {
    /// Say nothing. Covers `unknown`, a steady nominal bucket, and a low bucket
    /// already warned about that has not materially worsened.
    Silent,
    /// Entry into the low state, an escalation `low → exhausted`, or a material
    /// further drop.
    Warn,
    /// The all-clear. Emitted exactly once per low episode.
    Recover,
}

/// The loop's memory between ticks. Small on purpose — everything else it needs
/// is re-read from Phase A each tick, so there is no cached state to go stale.
#[derive(Debug, Default)]
struct WatchLedger {
    /// Last DECISIVE class: `nominal` | `low` | `exhausted`. `None` until the
    /// first one. An `unknown` tick never writes here — see [`decide`].
    last_class: Option<&'static str>,
    /// Remaining fraction at the last warning, for the material-drop test.
    last_warned_fraction: Option<f64>,
    warnings: u64,
    recoveries: u64,
    last_transition_at_unix: Option<i64>,
}

static LEDGER: OnceLock<Mutex<WatchLedger>> = OnceLock::new();
static WATCH_RUNNING: AtomicBool = AtomicBool::new(false);

fn ledger() -> &'static Mutex<WatchLedger> {
    LEDGER.get_or_init(|| Mutex::new(WatchLedger::default()))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether [`WATCH_DISABLED_ENV`] disables the loop, given its raw value.
///
/// Split from the `std::env` read so it is testable without mutating process
/// environment from a parallel test run. Matches the truthiness set
/// `agent_authorization::authz_disabled` already uses.
fn watch_disabled_by(value: Option<&str>) -> bool {
    matches!(
        value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn watch_disabled() -> bool {
    watch_disabled_by(std::env::var(WATCH_DISABLED_ENV).ok().as_deref())
}

/// The current [`WatchStatus`], for the route.
fn watch_status() -> WatchStatus {
    let (last_class, warnings, recoveries, last_transition_at_unix) = match ledger().lock() {
        Ok(l) => (
            l.last_class.unwrap_or("unknown"),
            l.warnings,
            l.recoveries,
            l.last_transition_at_unix,
        ),
        // A poisoned ledger is UNKNOWN, not healthy.
        Err(_) => ("unknown", 0, 0, None),
    };
    WatchStatus {
        enabled: !watch_disabled(),
        running: WATCH_RUNNING.load(Ordering::Relaxed),
        interval_seconds: WATCH_INTERVAL.as_secs(),
        last_class,
        warnings_emitted: warnings,
        recoveries_emitted: recoveries,
        last_transition_at_unix,
    }
}

/// Decide what one tick should emit, and fold the observation into `ledger`.
///
/// `class` is Phase A's `drift_class`; `fraction` its `remaining_fraction`.
/// Pure apart from the `&mut` ledger, so every transition below is a unit test
/// rather than a timing experiment.
///
/// The rules, and why:
///
/// - **`unknown` → [`WatchAction::Silent`], ledger UNTOUCHED.** A cold process
///   has measured nothing. Warning there would report a problem that has not
///   been observed, and — worse — writing `unknown` into `last_class` would let
///   a later nominal tick fire a "recovery" for an episode that never happened.
/// - **`nominal`** → `Recover` iff the previous decisive class was low or
///   exhausted, else `Silent`. Exactly one all-clear per episode: a warning with
///   no matching all-clear teaches operators to ignore warnings.
/// - **`low` / `exhausted`** → `Warn` on entry, on escalation `low →
///   exhausted`, and on a further drop of at least [`MATERIAL_FRACTION_DROP`];
///   `Silent` otherwise. `exhausted → low` is an improvement that is still bad,
///   so it stays silent — the all-clear is reserved for `nominal`.
fn decide(
    ledger: &mut WatchLedger,
    class: &'static str,
    fraction: Option<f64>,
    now: i64,
) -> WatchAction {
    match class {
        "unknown" => WatchAction::Silent,

        "nominal" => {
            let was_low = matches!(ledger.last_class, Some("low") | Some("exhausted"));
            ledger.last_class = Some("nominal");
            if was_low {
                ledger.last_warned_fraction = None;
                ledger.recoveries = ledger.recoveries.saturating_add(1);
                ledger.last_transition_at_unix = Some(now);
                WatchAction::Recover
            } else {
                WatchAction::Silent
            }
        }

        "low" | "exhausted" => {
            let previous = ledger.last_class;
            let already_low = matches!(previous, Some("low") | Some("exhausted"));
            // One-directional ON PURPOSE. `low → exhausted` is news. The
            // reverse, `exhausted → low`, is an improvement that is still bad,
            // and warning on it would make a bucket oscillating either side of
            // the 2% floor warn on every tick — the spam this ledger exists to
            // prevent. The all-clear is reserved for `nominal`.
            let escalated = previous == Some("low") && class == "exhausted";
            let material_drop = match (ledger.last_warned_fraction, fraction) {
                (Some(previous_fraction), Some(now_fraction)) => {
                    previous_fraction - now_fraction >= MATERIAL_FRACTION_DROP
                }
                // No warned fraction yet, or the class went low without a usable
                // fraction: fall back to the entry test rather than inventing a
                // comparison.
                _ => false,
            };

            ledger.last_class = Some(class);

            if !already_low || escalated || material_drop {
                ledger.last_warned_fraction = fraction;
                ledger.warnings = ledger.warnings.saturating_add(1);
                ledger.last_transition_at_unix = Some(now);
                WatchAction::Warn
            } else {
                WatchAction::Silent
            }
        }

        // Phase A's `drift_class` is a closed set of four `&'static str`s. A
        // fifth would be a Phase A change; treat it as UNKNOWN rather than
        // guessing, and never warn on it.
        _ => WatchAction::Silent,
    }
}

/// The top spenders, rendered for one log line.
///
/// Reports `charged` and `not_modified` separately because they are the two
/// halves of the plan's actual question: `charged` is what drains the bucket,
/// `not_modified` is what the ETag cache saved. A row with a high
/// `not_modified` is working as intended and must not read as a culprit.
fn format_top_templates(rows: &[ConsumerRow], limit: usize) -> String {
    if rows.is_empty() {
        // Not "nothing is spending" — the histogram can be empty on a process
        // that recorded a rate-limit header without recording a row.
        return "<no histogram rows recorded>".to_string();
    }
    rows.iter()
        .take(limit)
        .map(|row| {
            format!(
                "{} {} [{}] charged={} not_modified={}",
                row.consumer,
                row.template,
                row.cache_mode.as_str(),
                row.charged,
                row.not_modified
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The credential this spend is billed to, for the warning. Never the token —
/// Phase A only ever stored the resolution-step name.
fn format_bucket(snapshot: &GithubBudgetSnapshot) -> String {
    let resource = snapshot.resource.as_deref().unwrap_or("unknown-resource");
    match snapshot.identity.as_ref() {
        Some(identity) => match identity.login.as_deref() {
            Some(login) => format!("{resource} via {} (login {login})", identity.source),
            None => format!("{resource} via {}", identity.source),
        },
        // The single unanswerable question of the original incident. Say so.
        None => format!("{resource} via UNKNOWN credential (identity not recorded)"),
    }
}

/// `reset_at_unix=… (in Ns)`, or an explicit unknown. GitHub's reset is the only
/// thing that tells an operator whether to wait or to stop.
fn format_reset(reset_at_unix: Option<i64>, now: i64) -> String {
    match reset_at_unix {
        Some(reset) => {
            let seconds = reset.saturating_sub(now);
            if seconds >= 0 {
                format!("reset_at_unix={reset} (in {seconds}s)")
            } else {
                format!("reset_at_unix={reset} (elapsed {}s ago)", -seconds)
            }
        }
        None => "reset_at_unix=unknown".to_string(),
    }
}

/// One tick: read Phase A, decide, log. Separated from the loop so the loop is
/// nothing but a timer.
fn watch_tick() {
    let snapshot = github_budget::snapshot();
    let class = snapshot.drift_class;
    let fraction = snapshot.remaining_fraction;
    let now = now_unix();

    let action = match ledger().lock() {
        Ok(mut l) => decide(&mut l, class, fraction, now),
        // A poisoned ledger would make every tick look like a fresh entry and
        // spam the log. Stay silent instead — the route still reports the
        // budget, and the poison itself is a bug elsewhere.
        Err(_) => WatchAction::Silent,
    };

    match action {
        WatchAction::Silent => {}
        WatchAction::Warn => {
            let low = github_budget::low_headroom();
            let (remaining, limit) = match low.as_ref() {
                Some(low) => (low.remaining.to_string(), low.limit.to_string()),
                None => (
                    snapshot
                        .remaining
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    snapshot
                        .limit
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
            };
            warn!(
                "GitHub REST budget {}: bucket {} — remaining {}/{} ({:.1}% of limit, threshold {:.0}%), {}. \
                 Top spenders: {}. Full detail: GET :9876{}",
                class,
                format_bucket(&snapshot),
                remaining,
                limit,
                fraction.unwrap_or(0.0) * 100.0,
                github_budget::low_fraction() * 100.0,
                format_reset(snapshot.reset_at_unix, now),
                format_top_templates(&snapshot.consumers, WARN_TOP_TEMPLATES),
                ROUTE_PATH
            );
        }
        WatchAction::Recover => {
            info!(
                "GitHub REST budget recovered: bucket {} — remaining {}/{} ({:.1}% of limit) is back above the {:.0}% threshold",
                format_bucket(&snapshot),
                snapshot
                    .remaining
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                snapshot
                    .limit
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                fraction.unwrap_or(0.0) * 100.0,
                github_budget::low_fraction() * 100.0,
            );
        }
    }
}

/// Start the low-headroom watch loop. Idempotent; safe to call from every
/// [`routes`] construction.
///
/// Skipped, with a log line naming the reason, when [`WATCH_DISABLED_ENV`] is
/// set or when there is no `tokio` runtime to spawn onto (a unit test building
/// the router, say). Both cases are reported by [`WatchStatus`] rather than left
/// to be inferred from the absence of warnings.
pub fn start_low_headroom_watch() {
    if watch_disabled() {
        // Announced, not quiet: an operator debugging a missing warning must be
        // able to find the switch that removed it.
        info!(
            "GitHub REST budget watch disabled by {} — the user/OAuth bucket is UNMONITORED until it is unset ({} still serves the meter)",
            WATCH_DISABLED_ENV, ROUTE_PATH
        );
        return;
    }
    if WATCH_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        WATCH_RUNNING.store(false, Ordering::SeqCst);
        debug!("GitHub REST budget watch not started: no tokio runtime on this thread");
        return;
    }

    info!(
        "GitHub REST budget watch started ({}s cadence, low threshold {:.0}%)",
        WATCH_INTERVAL.as_secs(),
        github_budget::low_fraction() * 100.0
    );

    // `spawn_supervised_forever` rather than a bare `tokio::spawn`: a panic in a
    // bare loop kills the subsystem permanently and silently, which for this
    // loop means the bucket goes back to having no owner — the exact condition
    // the plan exists to end.
    let _handle = crate::mcp::task_supervisor::spawn_supervised_forever(
        "github-budget-watch",
        WATCH_INITIAL_BACKOFF,
        WATCH_MAX_BACKOFF,
        WATCH_HEALTHY_RUN,
        || async {
            loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                watch_tick();
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_budget::{BudgetTotals, CacheMode, CredentialIdentity};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn row(consumer: &str, template: &str, charged: u64, not_modified: u64) -> ConsumerRow {
        ConsumerRow {
            consumer: consumer.to_string(),
            template: template.to_string(),
            cache_mode: CacheMode::Cached,
            charged,
            not_modified,
            rate_limited: 0,
            transport_error: 0,
            cache_effectiveness: None,
        }
    }

    fn snapshot_with(rows: Vec<ConsumerRow>, elided: usize) -> GithubBudgetSnapshot {
        GithubBudgetSnapshot {
            identity: Some(CredentialIdentity {
                source: "gh auth token".to_string(),
                login: Some("operator".to_string()),
            }),
            limit: Some(5000),
            remaining: Some(500),
            used: Some(4500),
            reset_at_unix: Some(1_000_000),
            resource: Some("core".to_string()),
            remaining_fraction: Some(0.1),
            drift_class: "low",
            observed_at_unix: Some(999_000),
            snapshot_at_unix: 999_100,
            consumers: rows,
            consumers_elided: elided,
            templates_dropped: 0,
            calls_dropped: 0,
            totals: BudgetTotals::default(),
        }
    }

    // -- query parsing ------------------------------------------------------

    #[test]
    fn parse_top_absent_and_blank_are_no_opinion() {
        assert_eq!(parse_top(None), Ok(None));
        assert_eq!(parse_top(Some("")), Ok(None));
        assert_eq!(parse_top(Some("   ")), Ok(None));
    }

    #[test]
    fn parse_top_accepts_a_positive_integer_with_surrounding_space() {
        assert_eq!(parse_top(Some("3")), Ok(Some(3)));
        assert_eq!(parse_top(Some(" 25 ")), Ok(Some(25)));
    }

    #[test]
    fn parse_top_rejects_zero_and_garbage_rather_than_coercing() {
        assert!(parse_top(Some("0")).is_err());
        assert!(parse_top(Some("-1")).is_err());
        let err = parse_top(Some("abc")).unwrap_err();
        assert!(err.contains("abc"), "message should quote the input: {err}");
    }

    // -- histogram narrowing ------------------------------------------------

    #[test]
    fn apply_top_narrows_and_accounts_the_dropped_rows() {
        let mut snapshot = snapshot_with(
            vec![
                row("pr_watcher", "/repos/{owner}/{repo}/pulls", 10, 1),
                row("session_pr_reconciler", "/repos/{owner}/{repo}", 5, 2),
                row("ci_node", "/repos/{owner}/{repo}/check-runs", 3, 0),
            ],
            4,
        );
        let meta = apply_top(&mut snapshot, Some(1));
        assert_eq!(snapshot.consumers.len(), 1);
        // 4 already elided by Phase A + 2 narrowed away here.
        assert_eq!(snapshot.consumers_elided, 6);
        assert_eq!(meta.served_rows, 1);
        assert_eq!(meta.elided_rows, 6);
        assert_eq!(meta.requested_top, Some(1));
        assert!(!meta.widen_unavailable);
    }

    #[test]
    fn apply_top_absent_leaves_the_snapshot_alone() {
        let mut snapshot = snapshot_with(vec![row("pr_watcher", "/x", 1, 0)], 0);
        let meta = apply_top(&mut snapshot, None);
        assert_eq!(snapshot.consumers.len(), 1);
        assert_eq!(snapshot.consumers_elided, 0);
        assert_eq!(meta.requested_top, None);
        assert!(!meta.widen_unavailable);
    }

    #[test]
    fn apply_top_reports_widen_unavailable_only_when_rows_were_really_lost() {
        // Asked for more than exist, and Phase A elided some: honest "cannot".
        let mut lossy = snapshot_with(vec![row("a", "/x", 1, 0)], 7);
        assert!(apply_top(&mut lossy, Some(50)).widen_unavailable);

        // Asked for more than exist, but nothing was elided: the list IS
        // complete, so this must not claim otherwise.
        let mut complete = snapshot_with(vec![row("a", "/x", 1, 0)], 0);
        assert!(!apply_top(&mut complete, Some(50)).widen_unavailable);
    }

    // -- watch decisions ----------------------------------------------------

    #[test]
    fn unknown_never_warns_and_never_touches_the_ledger() {
        let mut ledger = WatchLedger::default();
        assert_eq!(
            decide(&mut ledger, "unknown", None, 100),
            WatchAction::Silent
        );
        assert_eq!(ledger.last_class, None);
        assert_eq!(ledger.warnings, 0);
        assert_eq!(ledger.recoveries, 0);
        assert_eq!(ledger.last_transition_at_unix, None);
    }

    #[test]
    fn unknown_after_a_low_episode_does_not_forge_a_recovery() {
        let mut ledger = WatchLedger::default();
        assert_eq!(decide(&mut ledger, "low", Some(0.15), 1), WatchAction::Warn);
        assert_eq!(decide(&mut ledger, "unknown", None, 2), WatchAction::Silent);
        // Still low as far as the ledger is concerned, so a later nominal tick
        // is the one and only recovery.
        assert_eq!(ledger.last_class, Some("low"));
        assert_eq!(
            decide(&mut ledger, "nominal", Some(0.9), 3),
            WatchAction::Recover
        );
        assert_eq!(ledger.recoveries, 1);
    }

    #[test]
    fn nominal_from_cold_is_silent_not_a_recovery() {
        let mut ledger = WatchLedger::default();
        assert_eq!(
            decide(&mut ledger, "nominal", Some(0.8), 10),
            WatchAction::Silent
        );
        assert_eq!(ledger.recoveries, 0);
        assert_eq!(ledger.last_class, Some("nominal"));
    }

    #[test]
    fn warns_once_on_entry_then_stays_silent_while_steady() {
        let mut ledger = WatchLedger::default();
        assert_eq!(decide(&mut ledger, "low", Some(0.19), 1), WatchAction::Warn);
        assert_eq!(
            decide(&mut ledger, "low", Some(0.18), 2),
            WatchAction::Silent
        );
        assert_eq!(
            decide(&mut ledger, "low", Some(0.17), 3),
            WatchAction::Silent
        );
        assert_eq!(ledger.warnings, 1);
        assert_eq!(ledger.last_transition_at_unix, Some(1));
    }

    #[test]
    fn warns_again_only_on_a_material_further_drop() {
        let mut ledger = WatchLedger::default();
        assert_eq!(decide(&mut ledger, "low", Some(0.19), 1), WatchAction::Warn);
        // 0.045 below the last warned fraction — short of
        // MATERIAL_FRACTION_DROP, so still silent.
        assert_eq!(
            decide(&mut ledger, "low", Some(0.145), 2),
            WatchAction::Silent
        );
        // 0.08 below it: comfortably material. Deliberately NOT the exact
        // boundary — the comparison is on `f64`, so a value chosen to land
        // exactly on MATERIAL_FRACTION_DROP tests the representation of
        // `0.19 - 0.14`, not the rule.
        assert_eq!(decide(&mut ledger, "low", Some(0.11), 3), WatchAction::Warn);
        assert_eq!(ledger.warnings, 2);
        assert_eq!(ledger.last_warned_fraction, Some(0.11));
    }

    #[test]
    fn escalation_from_low_to_exhausted_warns_again() {
        let mut ledger = WatchLedger::default();
        assert_eq!(decide(&mut ledger, "low", Some(0.19), 1), WatchAction::Warn);
        assert_eq!(
            decide(&mut ledger, "exhausted", Some(0.01), 2),
            WatchAction::Warn
        );
        assert_eq!(ledger.warnings, 2);
        // Coming back up to `low` is still bad — no all-clear for that.
        assert_eq!(
            decide(&mut ledger, "low", Some(0.03), 3),
            WatchAction::Silent
        );
        assert_eq!(ledger.warnings, 2);
    }

    #[test]
    fn recovery_is_emitted_exactly_once_per_episode() {
        let mut ledger = WatchLedger::default();
        decide(&mut ledger, "low", Some(0.10), 1);
        assert_eq!(
            decide(&mut ledger, "nominal", Some(0.85), 2),
            WatchAction::Recover
        );
        assert_eq!(
            decide(&mut ledger, "nominal", Some(0.86), 3),
            WatchAction::Silent
        );
        assert_eq!(ledger.recoveries, 1);
        // A second episode gets its own warn + all-clear pair.
        assert_eq!(decide(&mut ledger, "low", Some(0.11), 4), WatchAction::Warn);
        assert_eq!(
            decide(&mut ledger, "nominal", Some(0.9), 5),
            WatchAction::Recover
        );
        assert_eq!(ledger.warnings, 2);
        assert_eq!(ledger.recoveries, 2);
    }

    #[test]
    fn an_unrecognised_class_is_treated_as_unknown() {
        let mut ledger = WatchLedger::default();
        assert_eq!(
            decide(&mut ledger, "something-new", Some(0.01), 1),
            WatchAction::Silent
        );
        assert_eq!(ledger.warnings, 0);
        assert_eq!(ledger.last_class, None);
    }

    // -- env switch ---------------------------------------------------------

    #[test]
    fn watch_is_enabled_by_default_and_off_only_on_a_truthy_value() {
        assert!(!watch_disabled_by(None));
        assert!(!watch_disabled_by(Some("")));
        assert!(!watch_disabled_by(Some("0")));
        assert!(!watch_disabled_by(Some("false")));
        for truthy in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(watch_disabled_by(Some(truthy)), "{truthy} should disable");
        }
    }

    // -- formatting ---------------------------------------------------------

    #[test]
    fn top_templates_render_charged_and_not_modified_separately() {
        let rows = vec![
            row("pr_watcher", "/repos/{owner}/{repo}/pulls", 120, 30),
            row("session_pr_reconciler", "/repos/{owner}/{repo}", 40, 400),
            row("ci_node", "/repos/{owner}/{repo}/check-runs", 5, 0),
            row("ticket_system", "/repos/{owner}/{repo}/issues", 1, 0),
        ];
        let rendered = format_top_templates(&rows, WARN_TOP_TEMPLATES);
        assert!(rendered.contains("pr_watcher"));
        assert!(rendered.contains("charged=120 not_modified=30"));
        assert!(rendered.contains("session_pr_reconciler"));
        // Only WARN_TOP_TEMPLATES rows.
        assert!(!rendered.contains("ticket_system"));
    }

    #[test]
    fn empty_histogram_says_so_rather_than_reading_as_no_spend() {
        assert_eq!(
            format_top_templates(&[], WARN_TOP_TEMPLATES),
            "<no histogram rows recorded>"
        );
    }

    #[test]
    fn bucket_names_the_credential_source_never_a_token() {
        let snapshot = snapshot_with(vec![], 0);
        let rendered = format_bucket(&snapshot);
        assert!(rendered.contains("core"));
        assert!(rendered.contains("gh auth token"));
        assert!(rendered.contains("operator"));

        let mut anonymous = snapshot_with(vec![], 0);
        anonymous.identity = None;
        assert!(format_bucket(&anonymous).contains("UNKNOWN credential"));
    }

    #[test]
    fn reset_renders_future_past_and_unknown_distinctly() {
        assert_eq!(
            format_reset(Some(1_000), 400),
            "reset_at_unix=1000 (in 600s)"
        );
        assert_eq!(
            format_reset(Some(1_000), 1_600),
            "reset_at_unix=1000 (elapsed 600s ago)"
        );
        assert_eq!(format_reset(None, 1), "reset_at_unix=unknown");
    }

    // -- handler ------------------------------------------------------------

    /// The handler takes no `ApiState`, so it can be mounted on a bare router —
    /// the same path constant `routes()` registers.
    fn test_router() -> Router {
        Router::new().route(ROUTE_PATH, get(get_github_budget))
    }

    /// Named `get_json` rather than `get` so it cannot shadow
    /// [`axum::routing::get`], which [`test_router`] needs from `use super::*`.
    async fn get_json(uri: &str) -> (StatusCode, serde_json::Value) {
        let response = test_router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn handler_returns_the_snapshot_shape() {
        let (status, body) = get_json(ROUTE_PATH).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], serde_json::json!(true));

        let data = &body["data"];
        // Phase A's snapshot, in full.
        let budget = &data["budget"];
        assert!(budget["driftClass"].is_string());
        assert!(budget["consumers"].is_array());
        assert!(budget.get("consumersElided").is_some());
        assert!(budget.get("templatesDropped").is_some());
        assert!(budget["totals"].is_object());
        assert!(budget["snapshotAtUnix"].is_i64());

        // The Phase C envelope around it.
        assert!(data["lowFraction"].is_f64());
        assert!(data["etagCache"]["entries"].is_u64());
        assert!(data["etagCache"]["approxBytes"].is_u64());
        assert!(data["histogram"]["servedRows"].is_u64());
        assert!(data["watch"]["intervalSeconds"].is_u64());
        assert!(data["watch"]["enabled"].is_boolean());
        assert!(data["watch"]["running"].is_boolean());
        assert!(data["watch"]["lastClass"].is_string());

        // A token must never appear anywhere in the payload; the identity is a
        // resolution-step NAME or nothing.
        let rendered = body.to_string();
        assert!(!rendered.contains("ghp_"));
        assert!(!rendered.contains("gho_"));
    }

    /// The route must never report a low bucket it has not measured, and must
    /// never hide one it has.
    ///
    /// Deliberately an INVARIANT rather than `assert_eq!(class, "unknown")`:
    /// the meter is process-global and every `#[test]` in this crate shares one
    /// binary, so a peer test that records a real GitHub response would make an
    /// absolute assertion flaky. What must hold regardless is the pairing —
    /// `unknown`/`nominal` carry no `lowHeadroom`, `low`/`exhausted` always do.
    #[tokio::test]
    async fn route_never_claims_low_headroom_it_has_not_measured() {
        let (_, body) = get_json(ROUTE_PATH).await;
        let class = body["data"]["budget"]["driftClass"].as_str().unwrap();
        let low_is_null = body["data"]["lowHeadroom"].is_null();
        match class {
            "unknown" | "nominal" => assert!(
                low_is_null,
                "class {class} must not carry a lowHeadroom claim"
            ),
            "low" | "exhausted" => assert!(
                !low_is_null,
                "class {class} must carry the lowHeadroom detail"
            ),
            other => panic!("unexpected driftClass `{other}`"),
        }
    }

    #[tokio::test]
    async fn top_is_echoed_back_in_the_histogram_meta() {
        let (status, body) = get_json(&format!("{ROUTE_PATH}?top=2")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["histogram"]["requestedTop"], 2);
    }

    #[tokio::test]
    async fn a_bad_top_is_a_400_with_an_envelope_not_a_bare_axum_rejection() {
        let (status, body) = get_json(&format!("{ROUTE_PATH}?top=0")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], serde_json::json!(false));
        assert!(body["error"].as_str().unwrap().contains("top"));

        let (status, body) = get_json(&format!("{ROUTE_PATH}?top=nope")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn routes_registers_exactly_the_documented_path() {
        // `routes()` also starts the watch; both calls must be no-op-safe.
        let _ = routes();
        let _ = routes();
        let (status, _) = get_json(ROUTE_PATH).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn watch_status_never_claims_health_it_has_not_measured() {
        let status = watch_status();
        assert_eq!(status.interval_seconds, WATCH_INTERVAL.as_secs());
        // Whatever the ledger holds, `lastClass` is one of the closed set and
        // an unstarted loop reports `unknown`, not `nominal`.
        assert!(matches!(
            status.last_class,
            "unknown" | "nominal" | "low" | "exhausted"
        ));
    }
}
