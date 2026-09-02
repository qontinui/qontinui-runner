//! Runner-local GitHub REST budget meter + ETag cache (plan
//! `2026-08-30-github-rest-budget-is-structurally-oversubscribed`, Phase A).
//!
//! # Why this exists
//!
//! The fleet's GitHub REST spend was sampled at ~5,600 req/hr against a 5,000
//! cap and the incident was attributed to coord's merge train. **That
//! attribution was wrong.** The sampler read the operator's *user/OAuth* bucket
//! (`gh` authenticated as a human, limit 5,000); coord bills every merge-train
//! read to a GitHub *App installation* bucket (limit 5,850) and has had
//! per-URL-template spend attribution, a remaining-budget gauge and a sub-20%
//! alert since June/July 2026. coord was never spending the bucket that
//! exhausted.
//!
//! The runner is. It resolves its credential from `GITHUB_TOKEN` / `GH_TOKEN`
//! or — the common case on an operator box — `gh auth token`
//! (`session_pr_reconciler::resolve_github_token`, `ci_node::sibling`), which
//! is the *same user token the sampler measured*. It then polls on that token
//! every 30s (`session_pr_reconciler::POLL_INTERVAL`) and as often as every 10s
//! (`trigger_system::watchers::pr_watcher`). Re-measured 2026-09-01:
//! **309 calls in 301s ≈ 62/min ≈ 3,700/hr on that one shared token**, and the
//! runner exported *nothing* about it — no counter, no gauge, no ETag cache,
//! and no statement of which bucket it was billing.
//!
//! That last gap is why [`CredentialIdentity`] exists and why
//! [`GithubBudgetSnapshot`] leads with it: "which bucket am I spending?" was
//! the single unanswerable question in the whole incident, and answering it
//! costs one string.
//!
//! # Shape
//!
//! A process-global registry (`OnceLock<Mutex<Registry>>`) cheap enough to call
//! on **every** GitHub response. Modelled on coord's proven
//! `outbound_budget_observer` / `github_ratelimit_watcher` pair — same URL-template
//! collapse, same four-value cache-effectiveness classification, same 0.20
//! low-headroom default — but written for this process: a different credential,
//! no leader election, no database.
//!
//! All derivation lives on [`Registry`] and on free functions
//! ([`normalize_url_template`], [`derive_cache_effectiveness`],
//! [`parse_rate_limit`]) that take their inputs explicitly, so every unit test
//! below drives a *local* registry and none of them race the global.
//!
//! # What is honest about absence
//!
//! **A fresh process has recorded nothing, and that is `unknown` — never
//! `nominal`, never zero.** [`drift_class`](GithubBudgetSnapshot::drift_class)
//! reports `unknown` before the first observation and [`low_headroom`] returns
//! `None` there: an *unmeasured* budget is not a healthy one and it is not a low
//! one either. Reporting a cold start as healthy is precisely the failure this
//! plan exists to fix.
//!
//! Phase B wires the call sites (conditional requests, identity recording,
//! credential precedence). Phase C serves this on `GET /github-budget` and runs
//! the sub-20% headroom watcher. This module deliberately edits neither.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use lru::LruCache;
use reqwest::header::HeaderMap;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// How many `(consumer, template, cache_mode)` rows [`snapshot`] reports.
/// Matches coord's `TOP_CONSUMERS_N` so the two histograms read alike side by
/// side. Rows beyond this are counted in `consumersElided`, never silently
/// dropped.
const TOP_CONSUMERS_N: usize = 12;

/// How many distinct `(consumer, template, cache_mode)` rows the registry keeps
/// in memory. Larger than [`TOP_CONSUMERS_N`] on purpose: the interesting row is
/// often not in the top 12 at the moment a key is first seen, and admitting it
/// costs a few dozen bytes.
///
/// Once full, a *new* key is refused and counted in
/// [`GithubBudgetSnapshot::templates_dropped`]. coord learned the hard way that
/// a silently-truncating histogram lies, so the totals
/// ([`GithubBudgetSnapshot::totals`]) count every call including the refused
/// ones — the histogram may be incomplete, the totals never are.
const TEMPLATE_KEY_CAP: usize = 128;

/// Minimum requests SENT (`charged + not_modified`) on ONE `cached` row before
/// its 304 ratio is allowed to mean anything. Below this the row is
/// `insufficient_data` — a 0-of-3 row is noise, not a broken cache, and a
/// classifier that cries "ineffective" at every cold start teaches its reader
/// to ignore it.
const CACHE_EFFECTIVENESS_MIN_SENT: u64 = 20;

/// At or below this 304 fraction a `cached` row is `ineffective`: it is paying a
/// conditional round trip and getting essentially nothing back.
const CACHE_EFFECTIVENESS_INEFFECTIVE_MAX: f64 = 0.05;

/// Above [`CACHE_EFFECTIVENESS_INEFFECTIVE_MAX`] and below this a `cached` row
/// is `degraded` — the cache does something, but the endpoint re-downloads the
/// majority of its reads.
const CACHE_EFFECTIVENESS_EFFECTIVE_MIN: f64 = 0.50;

/// Fraction of limit at or below which the bucket is `exhausted` rather than
/// merely `low`. GitHub's 5,000-point user bucket → 100 points. Same floor as
/// coord's `EXHAUSTED_FLOOR_FRACTION`.
const EXHAUSTED_FLOOR_FRACTION: f64 = 0.02;

/// Default low-headroom threshold — matches coord's
/// `github_ratelimit_watcher::DEFAULT_LOW_FRACTION` so a runner and a coord
/// replica sound the same alarm at the same depth.
const DEFAULT_LOW_FRACTION: f64 = 0.20;

/// Env override for the low-headroom threshold. Must parse and lie strictly
/// inside `(0, 1)`; anything else falls back to [`DEFAULT_LOW_FRACTION`] rather
/// than arming a threshold that can never (or always) fire.
const LOW_FRACTION_ENV: &str = "QONTINUI_GITHUB_BUDGET_LOW_FRACTION";

/// Hard entry ceiling for the ETag cache.
const ETAG_CACHE_MAX_ENTRIES: usize = 512;

/// Heap budget for the ETag cache. The entry ceiling alone is not a bound: a
/// `/compare/{a}...{b}` body is MB-scale, so 512 of them is not "small". Both
/// limits are enforced; whichever binds first evicts.
const ETAG_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Cache mode
// ---------------------------------------------------------------------------

/// What the caller *decided* about caching before issuing the request.
///
/// This is the dimension that makes the histogram diagnosable rather than
/// merely descriptive. **Only [`CacheMode::Cached`] rows may be judged for cache
/// effectiveness**: a `Fresh` row sends no `If-None-Match` and a write has no
/// cache decision at all, so their `not_modified: 0` is correct *by
/// construction*. Collapsing the three into one ratio re-creates the exact
/// misdiagnosis coord's `cache_mode` split was added to remove — an uncacheable
/// overdraft rendered as "a cache with a poor hit rate".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// An `If-None-Match` was sent (or would have been, had an ETag been held).
    Cached,
    /// A deliberately unconditional GET — the caller wants a fresh body.
    Fresh,
    /// A write, or any request with no cache decision to make.
    Uncacheable,
}

impl CacheMode {
    /// Wire name, matching coord's `cache_mode` column values.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheMode::Cached => "cached",
            CacheMode::Fresh => "fresh",
            CacheMode::Uncacheable => "uncacheable",
        }
    }
}

// ---------------------------------------------------------------------------
// Credential identity
// ---------------------------------------------------------------------------

/// Which credential this process is spending, named by its **resolution step**.
///
/// Never carries the token. `source` is one of the resolution-step names the
/// runner's own resolver walks (`QONTINUI_RUNNER_GITHUB_TOKEN`, `GITHUB_TOKEN`,
/// `GH_TOKEN`, `gh auth token`); `login` is filled only when it is already
/// cheaply known (e.g. a `/user` response the runner made anyway) — this module
/// never spends a request to discover it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialIdentity {
    /// The resolution step that supplied the token. Never the token.
    pub source: String,
    /// The authenticated login, when known for free. `None` is UNKNOWN.
    pub login: Option<String>,
}

/// Defensive redaction for [`record_identity`].
///
/// The whole value of the identity field is that it can be logged and served on
/// `/health` freely. A caller that passes the token itself by mistake would turn
/// that into a credential leak on a public-ish surface, so anything that smells
/// like a GitHub token — the documented `gh*_` prefixes, or any implausibly long
/// opaque blob — is replaced rather than stored.
fn sanitize_identity_source(source: &str) -> String {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    let looks_like_token = trimmed.len() > 48
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
            .iter()
            .any(|p| trimmed.starts_with(p));
    if looks_like_token {
        "redacted".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Rate-limit headers
// ---------------------------------------------------------------------------

/// One reading of GitHub's `X-RateLimit-*` family.
///
/// Every field is `Option` because GitHub omits the whole family on some
/// responses (and every field of it on a transport error). A missing field is
/// UNKNOWN — it is never coerced to zero, which would read as "budget
/// exhausted".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitObservation {
    /// `x-ratelimit-limit` — the bucket ceiling (5,000 user / 5,850 app).
    pub limit: Option<i64>,
    /// `x-ratelimit-remaining`.
    pub remaining: Option<i64>,
    /// `x-ratelimit-used`.
    pub used: Option<i64>,
    /// `x-ratelimit-reset`, unix seconds.
    pub reset_at_unix: Option<i64>,
    /// `x-ratelimit-resource` — e.g. `core`, `search`, `graphql`.
    pub resource: Option<String>,
}

impl RateLimitObservation {
    /// True when GitHub sent none of the family — i.e. this response says
    /// nothing about the budget and must not overwrite what we already knew.
    fn is_empty(&self) -> bool {
        self.limit.is_none()
            && self.remaining.is_none()
            && self.used.is_none()
            && self.reset_at_unix.is_none()
            && self.resource.is_none()
    }
}

/// Parse the `X-RateLimit-*` family out of a response's headers. Pure — no
/// globals, no clock — so the tests can hand it a synthetic [`HeaderMap`].
///
/// Unparseable values are dropped to `None` rather than defaulted: a header we
/// cannot read is UNKNOWN, and pretending otherwise is how a cold or malformed
/// reading becomes a false "exhausted".
pub fn parse_rate_limit(headers: &HeaderMap) -> RateLimitObservation {
    let num = |name: &str| -> Option<i64> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
    };
    RateLimitObservation {
        limit: num("x-ratelimit-limit"),
        remaining: num("x-ratelimit-remaining"),
        used: num("x-ratelimit-used"),
        reset_at_unix: num("x-ratelimit-reset"),
        resource: headers
            .get("x-ratelimit-resource")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
    }
}

/// A `403`/`429` that GitHub refused for an exhausted bucket, as opposed to a
/// permission `403` or a secondary-limit `429` with budget left. The discriminator
/// is `x-ratelimit-remaining: 0` on the refusal itself.
fn is_rate_limited(status: u16, obs: &RateLimitObservation) -> bool {
    matches!(status, 403 | 429) && obs.remaining == Some(0)
}

// ---------------------------------------------------------------------------
// URL template normalization
// ---------------------------------------------------------------------------

/// Collapse a request URL to a bounded template: drop scheme/host and query
/// string, replace all-numeric path segments with `N`, and replace 40-hex
/// segments with `{sha}`.
///
/// `https://api.github.com/repos/o/r/pulls/1234?per_page=100`
///   → `/repos/o/r/pulls/N`
/// `/repos/o/r/commits/<40-hex>/check-runs`
///   → `/repos/o/r/commits/{sha}/check-runs`
///
/// This is the same normalization coord's `top_consumers` uses, and it is what
/// makes the histogram readable: without it every PR number and every head SHA
/// is its own row and the top-12 is 12 samples of one endpoint.
///
/// Only a **full 40-hex** segment is a SHA. A 7-char abbreviation is left alone
/// deliberately — plenty of legitimate path words are hex-ish at that length,
/// and over-collapsing merges endpoints that are genuinely distinct. Owner,
/// repo and endpoint words survive untouched.
pub fn normalize_url_template(url: &str) -> String {
    // Strip scheme://host — the runner's call sites pass absolute URLs, coord's
    // pass paths, and both must land on the same template.
    let path = match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[i + 3 + j..],
            None => "/",
        },
        None => url,
    };
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);

    let mut out = String::with_capacity(path.len());
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push('N');
        } else if seg.len() == 40 && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push_str("{sha}");
        } else {
            out.push_str(seg);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cache effectiveness
// ---------------------------------------------------------------------------

/// Derive, ON READ, whether a `cached` row's ETag cache is actually buying
/// anything. Pure — no state, no clock.
///
/// Returns `None` for `fresh` and `uncacheable`; see [`CacheMode`] for why that
/// abstention is the point rather than a gap.
///
/// The four values:
///
/// - `insufficient_data` — fewer than [`CACHE_EFFECTIVENESS_MIN_SENT`] sent.
///   **UNKNOWN, never "0% effective".**
/// - `ineffective` — at or below [`CACHE_EFFECTIVENESS_INEFFECTIVE_MAX`] free.
/// - `degraded` — below [`CACHE_EFFECTIVENESS_EFFECTIVE_MIN`] free.
/// - `effective` — at or above it.
///
/// `ineffective` is not automatically a runner bug. GitHub embeds the mutable
/// `head.repo`/`base.repo` object inside every `/repos/{o}/{r}/pulls/{n}`
/// representation, so any push to any branch of an active repo invalidates the
/// ETag of every open PR in it at once. The classification's job is to make the
/// fact visible; what to do about it is a judgement it does not make.
pub fn derive_cache_effectiveness(
    cache_mode: CacheMode,
    charged: u64,
    not_modified: u64,
) -> Option<&'static str> {
    if cache_mode != CacheMode::Cached {
        return None;
    }
    let sent = charged.saturating_add(not_modified);
    if sent < CACHE_EFFECTIVENESS_MIN_SENT {
        return Some("insufficient_data");
    }
    let ratio = not_modified as f64 / sent as f64;
    if ratio <= CACHE_EFFECTIVENESS_INEFFECTIVE_MAX {
        Some("ineffective")
    } else if ratio < CACHE_EFFECTIVENESS_EFFECTIVE_MIN {
        Some("degraded")
    } else {
        Some("effective")
    }
}

/// Low-headroom threshold: env [`LOW_FRACTION_ENV`], else
/// [`DEFAULT_LOW_FRACTION`]. A value outside `(0, 1)` — or one that does not
/// parse — is ignored, because a threshold of `0` never fires and a threshold of
/// `1` always does, and both are worse than the default.
pub fn low_fraction() -> f64 {
    std::env::var(LOW_FRACTION_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
        .unwrap_or(DEFAULT_LOW_FRACTION)
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

/// One `(consumer, template, cache_mode)` row of the spend histogram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerRow {
    /// Which subsystem issued the call — e.g. `session_pr_reconciler`,
    /// `pr_watcher`, `ci_node`. Free-form on purpose: a new caller should show
    /// up in the histogram without a code change here.
    pub consumer: String,
    /// [`normalize_url_template`] of the request URL.
    pub template: String,
    /// The caller's cache decision.
    pub cache_mode: CacheMode,
    /// Responses GitHub billed against the bucket (everything except a 304 and
    /// except a refusal issued because the bucket was already empty).
    pub charged: u64,
    /// `304 Not Modified` — **free** against the budget. This is the number the
    /// whole plan is trying to grow.
    pub not_modified: u64,
    /// `403`/`429` refusals carrying `x-ratelimit-remaining: 0`.
    pub rate_limited: u64,
    /// Requests that never produced a response (DNS, TLS, timeout). Counted
    /// apart from `charged` because they say nothing about the budget.
    pub transport_error: u64,
    /// `insufficient_data | ineffective | degraded | effective`, or `None` for a
    /// `fresh`/`uncacheable` row. See [`derive_cache_effectiveness`].
    pub cache_effectiveness: Option<&'static str>,
}

/// Process-wide call totals. **These count every recorded call, including ones
/// whose histogram row was refused by [`TEMPLATE_KEY_CAP`]** — the histogram may
/// be truncated, the totals never are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetTotals {
    pub charged: u64,
    pub not_modified: u64,
    pub rate_limited: u64,
    pub transport_error: u64,
}

impl BudgetTotals {
    /// Requests actually put on the wire — `charged + not_modified`. Excludes
    /// transport errors (no response) and empty-bucket refusals (no spend).
    pub fn sent(&self) -> u64 {
        self.charged.saturating_add(self.not_modified)
    }
}

/// Everything this process knows about its GitHub budget, in one serializable
/// value. Phase C serves it on `GET /github-budget` — a route of its own, NOT
/// `/health`: this is spend attribution, not a liveness answer, and folding it
/// into the liveness probe would make every health check pay for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubBudgetSnapshot {
    /// Which credential this spend was billed to. `None` until Phase B's
    /// resolver calls [`record_identity`] — and `None` is UNKNOWN, which is
    /// exactly the state the 2026-09-01 misattribution was made in.
    pub identity: Option<CredentialIdentity>,
    /// Bucket ceiling from the last response that carried the headers.
    pub limit: Option<i64>,
    pub remaining: Option<i64>,
    pub used: Option<i64>,
    /// `x-ratelimit-reset`, unix seconds.
    pub reset_at_unix: Option<i64>,
    /// `x-ratelimit-resource` (`core`, `search`, `graphql`, …).
    pub resource: Option<String>,
    /// `remaining / limit`, or `None` when either is unknown or `limit <= 0`.
    pub remaining_fraction: Option<f64>,
    /// `unknown | nominal | low | exhausted`. **`unknown` before any response
    /// has been recorded** — a cold process is not a healthy one.
    pub drift_class: &'static str,
    /// Unix seconds at which the rate-limit fields above were observed. `None`
    /// when nothing has been observed.
    pub observed_at_unix: Option<i64>,
    /// Unix seconds at which this snapshot was taken.
    pub snapshot_at_unix: i64,
    /// Top [`TOP_CONSUMERS_N`] rows by requests sent, descending.
    pub consumers: Vec<ConsumerRow>,
    /// Retained rows *not* rendered in `consumers`. Non-zero means the
    /// histogram above is a top-N view, not the whole registry.
    pub consumers_elided: usize,
    /// Distinct `(consumer, template, cache_mode)` keys refused because the
    /// registry was at [`TEMPLATE_KEY_CAP`]. **Non-zero means the histogram is
    /// incomplete** — the totals still are not.
    pub templates_dropped: u64,
    /// Individual calls whose row was refused. Folded into `totals`, absent from
    /// `consumers`.
    pub calls_dropped: u64,
    pub totals: BudgetTotals,
}

/// A budget below the [`low_fraction`] threshold. Phase C's watcher turns this
/// into a warn-on-entry / all-clear-on-recovery log line and surfaces it as
/// `lowHeadroom` on `GET /github-budget`.
///
/// `None` from [`low_headroom`] is UNKNOWN **or** healthy, and the watcher must
/// keep those apart — see its ledger, which an `unknown` tick deliberately
/// leaves untouched so a later nominal tick cannot forge a recovery for an
/// episode nobody observed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LowHeadroom {
    pub remaining: i64,
    pub limit: i64,
    /// `remaining / limit` at observation time.
    pub fraction: f64,
    /// The threshold that fired, so a reader need not guess the env value.
    pub threshold: f64,
    pub resource: Option<String>,
    pub reset_at_unix: Option<i64>,
    /// Which credential is running out — the actionable half.
    pub identity: Option<CredentialIdentity>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Histogram key. `cache_mode` is part of the key rather than an attribute so
/// the same template polled both conditionally and unconditionally cannot have
/// its two behaviours averaged into one meaningless ratio.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RowKey {
    consumer: String,
    template: String,
    cache_mode: CacheMode,
}

#[derive(Debug, Clone, Copy, Default)]
struct RowCounts {
    charged: u64,
    not_modified: u64,
    rate_limited: u64,
    transport_error: u64,
}

impl RowCounts {
    fn sent(&self) -> u64 {
        self.charged.saturating_add(self.not_modified)
    }
}

/// The meter itself. Held behind one process-global mutex, but constructible
/// standalone so every test below drives its own instance and none of them race.
#[derive(Debug, Default)]
pub struct Registry {
    identity: Option<CredentialIdentity>,
    last: Option<RateLimitObservation>,
    observed_at_unix: Option<i64>,
    rows: HashMap<RowKey, RowCounts>,
    templates_dropped: u64,
    calls_dropped: u64,
    totals: BudgetTotals,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record which credential this process resolved. Idempotent; a later call
    /// with a different source replaces the earlier one (the runner can
    /// re-resolve, and the *current* answer is the useful one).
    pub fn record_identity(&mut self, source: &str, login: Option<String>) {
        self.identity = Some(CredentialIdentity {
            source: sanitize_identity_source(source),
            login: login
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
        });
    }

    /// Fold one GitHub response into the meter.
    ///
    /// `now_unix` is injected rather than read from the clock so the derivation
    /// is deterministic under test; the global wrapper passes the real clock.
    pub fn record(
        &mut self,
        consumer: &str,
        url: &str,
        cache_mode: CacheMode,
        status: u16,
        obs: RateLimitObservation,
        now_unix: i64,
    ) {
        // A response with no `X-RateLimit-*` family says nothing about the
        // budget. Keeping the previous reading beats overwriting a real one with
        // a row of `None`s.
        if !obs.is_empty() {
            self.last = Some(obs.clone());
            self.observed_at_unix = Some(now_unix);
        }

        let limited = is_rate_limited(status, &obs);
        let mut delta = RowCounts::default();
        if limited {
            // Refused because the bucket was already empty — it bought nothing
            // and it cost nothing further, so it is neither charged nor free.
            delta.rate_limited = 1;
        } else if status == 304 {
            delta.not_modified = 1;
        } else {
            // GitHub decrements the bucket for everything else, 4xx and 5xx
            // included. Getting this wrong in either direction corrupts the
            // whole histogram.
            delta.charged = 1;
        }
        self.apply(consumer, url, cache_mode, delta);
    }

    /// Record a request that never produced a response (DNS, TLS, timeout).
    /// Counted apart from `charged`: the budget may or may not have been
    /// decremented and we cannot know, so it is not asserted either way.
    pub fn record_transport_error(&mut self, consumer: &str, url: &str, cache_mode: CacheMode) {
        self.apply(
            consumer,
            url,
            cache_mode,
            RowCounts {
                transport_error: 1,
                ..RowCounts::default()
            },
        );
    }

    /// Fold a delta into the totals (always) and into its histogram row (if the
    /// key is admitted).
    fn apply(&mut self, consumer: &str, url: &str, cache_mode: CacheMode, delta: RowCounts) {
        self.totals.charged = self.totals.charged.saturating_add(delta.charged);
        self.totals.not_modified = self.totals.not_modified.saturating_add(delta.not_modified);
        self.totals.rate_limited = self.totals.rate_limited.saturating_add(delta.rate_limited);
        self.totals.transport_error = self
            .totals
            .transport_error
            .saturating_add(delta.transport_error);

        let key = RowKey {
            consumer: consumer.to_string(),
            template: normalize_url_template(url),
            cache_mode,
        };
        if let Some(row) = self.rows.get_mut(&key) {
            row.charged = row.charged.saturating_add(delta.charged);
            row.not_modified = row.not_modified.saturating_add(delta.not_modified);
            row.rate_limited = row.rate_limited.saturating_add(delta.rate_limited);
            row.transport_error = row.transport_error.saturating_add(delta.transport_error);
        } else if self.rows.len() < TEMPLATE_KEY_CAP {
            self.rows.insert(key, delta);
        } else {
            // Cap reached. Say so out loud rather than quietly under-reporting:
            // the totals above already counted this call.
            self.templates_dropped = self.templates_dropped.saturating_add(1);
            self.calls_dropped = self.calls_dropped.saturating_add(1);
        }
    }

    /// `remaining / limit`, or `None` when unknown. `limit <= 0` is UNKNOWN, not
    /// 0% headroom — it means GitHub did not send a usable ceiling.
    pub fn remaining_fraction(&self) -> Option<f64> {
        let last = self.last.as_ref()?;
        let limit = last.limit?;
        let remaining = last.remaining?;
        if limit > 0 {
            Some(remaining as f64 / limit as f64)
        } else {
            None
        }
    }

    /// The most recent `X-RateLimit-*` reading, or `None` when nothing has been
    /// observed yet.
    ///
    /// This is the back-off input every GitHub client in the process shares.
    /// Before Phase B each client carried its own `AtomicU32` pair, so four
    /// disjoint counters each held a partial view of ONE bucket and each backed
    /// off on a quarter of the evidence. `None` is UNKNOWN — a caller must not
    /// read it as "the bucket is full", it simply has nothing to act on yet.
    pub fn last_rate_limit(&self) -> Option<RateLimitObservation> {
        self.last.clone()
    }

    /// `unknown | nominal | low | exhausted`.
    ///
    /// `unknown` when nothing has been observed, or when the observation lacks a
    /// usable `limit`/`remaining` pair. **An absent observation is never
    /// `nominal`.**
    pub fn drift_class(&self, threshold: f64) -> &'static str {
        match self.remaining_fraction() {
            None => "unknown",
            Some(f) if f <= EXHAUSTED_FLOOR_FRACTION => "exhausted",
            Some(f) if f < threshold => "low",
            Some(_) => "nominal",
        }
    }

    /// `Some` when the observed headroom is strictly below `threshold`.
    /// `None` on `unknown` — an unmeasured budget is not a low one.
    pub fn low_headroom(&self, threshold: f64) -> Option<LowHeadroom> {
        let fraction = self.remaining_fraction()?;
        if fraction >= threshold {
            return None;
        }
        let last = self.last.as_ref()?;
        Some(LowHeadroom {
            remaining: last.remaining?,
            limit: last.limit?,
            fraction,
            threshold,
            resource: last.resource.clone(),
            reset_at_unix: last.reset_at_unix,
            identity: self.identity.clone(),
        })
    }

    /// Render the snapshot, reporting the `top_n` busiest rows.
    pub fn snapshot(&self, top_n: usize, threshold: f64, now_unix: i64) -> GithubBudgetSnapshot {
        let mut consumers: Vec<ConsumerRow> = self
            .rows
            .iter()
            .map(|(key, counts)| ConsumerRow {
                consumer: key.consumer.clone(),
                template: key.template.clone(),
                cache_mode: key.cache_mode,
                charged: counts.charged,
                not_modified: counts.not_modified,
                rate_limited: counts.rate_limited,
                transport_error: counts.transport_error,
                cache_effectiveness: derive_cache_effectiveness(
                    key.cache_mode,
                    counts.charged,
                    counts.not_modified,
                ),
            })
            .collect();

        // Busiest first; ties broken on the key so the order is stable across
        // snapshots (a histogram that reshuffles at random is unreadable).
        consumers.sort_by(|a, b| {
            let sent_a = a.charged.saturating_add(a.not_modified);
            let sent_b = b.charged.saturating_add(b.not_modified);
            sent_b
                .cmp(&sent_a)
                .then_with(|| a.consumer.cmp(&b.consumer))
                .then_with(|| a.template.cmp(&b.template))
                .then_with(|| a.cache_mode.as_str().cmp(b.cache_mode.as_str()))
        });

        let consumers_elided = consumers.len().saturating_sub(top_n);
        consumers.truncate(top_n);

        let last = self.last.clone().unwrap_or_default();
        GithubBudgetSnapshot {
            identity: self.identity.clone(),
            limit: last.limit,
            remaining: last.remaining,
            used: last.used,
            reset_at_unix: last.reset_at_unix,
            resource: last.resource,
            remaining_fraction: self.remaining_fraction(),
            drift_class: self.drift_class(threshold),
            observed_at_unix: self.observed_at_unix,
            snapshot_at_unix: now_unix,
            consumers,
            consumers_elided,
            templates_dropped: self.templates_dropped,
            calls_dropped: self.calls_dropped,
            totals: self.totals,
        }
    }

    /// Retained row count. Used by the tests and by the cap assertions.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

// ---------------------------------------------------------------------------
// ETag cache
// ---------------------------------------------------------------------------

/// One cached GET body, keyed by the FULL request URL.
#[derive(Debug, Clone)]
struct EtagEntry {
    etag: String,
    body: Bytes,
}

impl EtagEntry {
    /// Approximate heap footprint. Deliberately approximate — it drives an
    /// eviction budget, not an accounting ledger — but it must never
    /// UNDER-count the body, which is the term that actually grows.
    fn heap_bytes(&self) -> usize {
        self.body.len() + self.etag.len()
    }
}

/// Bounded ETag store. A 304 is **free** against the GitHub budget; holding the
/// body is the only way to turn one into a usable response, and that is the
/// entire point of this module's Phase B.
///
/// The key is the full URL *including query string* — deliberately NOT the
/// normalized template. Two different SHAs are genuinely different bodies, so
/// collapsing them here would replay one commit's check-runs for another. That
/// makes the key set unbounded (every head SHA, every page), which is why the
/// bound is explicit: an entry ceiling **and** a byte budget, LRU-evicted.
#[derive(Debug)]
pub struct EtagCache {
    entries: LruCache<String, EtagEntry>,
    bytes: usize,
    max_bytes: usize,
}

impl EtagCache {
    /// `max_entries` is clamped to at least 1 — a zero-capacity LRU would panic
    /// and a cache that holds nothing is a silent regression to Phase 0.
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        let cap = NonZeroUsize::new(max_entries.max(1)).expect("clamped to >= 1");
        Self {
            entries: LruCache::new(cap),
            bytes: 0,
            max_bytes,
        }
    }

    /// The ETag to echo back as `If-None-Match`, if one is held. Counts as a
    /// use for LRU purposes: a URL we are still polling must not be evicted
    /// under a URL we merely stored once.
    pub fn etag_for(&mut self, url: &str) -> Option<String> {
        self.entries.get(url).map(|e| e.etag.clone())
    }

    /// Store (or replace) the body and ETag for `url`, then evict LRU-first
    /// until both bounds hold.
    ///
    /// An empty ETag is not stored: sending `If-None-Match: ""` would make every
    /// subsequent request unconditional while *claiming* to be cached, which is
    /// the exact reporting lie [`CacheMode`] exists to prevent.
    pub fn store(&mut self, url: &str, etag: &str, body: Bytes) {
        let etag = etag.trim();
        if etag.is_empty() {
            return;
        }
        let entry = EtagEntry {
            etag: etag.to_string(),
            body,
        };
        let added = entry.heap_bytes();
        if let Some(old) = self.entries.put(url.to_string(), entry) {
            self.bytes = self.bytes.saturating_sub(old.heap_bytes());
        }
        self.bytes = self.bytes.saturating_add(added);
        self.evict_to_budget();
    }

    /// The cached body to serve when GitHub answers `304 Not Modified`.
    pub fn replay(&mut self, url: &str) -> Option<Bytes> {
        self.entries.get(url).map(|e| e.body.clone())
    }

    /// Drop the entry for `url` — for a caller that learns the body is no longer
    /// replayable (a 404, a repo rename).
    pub fn invalidate(&mut self, url: &str) {
        if let Some(old) = self.entries.pop(url) {
            self.bytes = self.bytes.saturating_sub(old.heap_bytes());
        }
    }

    /// `(entries, approximate bytes)` — the gauge that did not exist when
    /// coord's equivalent cache OOMed a fleet.
    pub fn stats(&self) -> (usize, usize) {
        (self.entries.len(), self.bytes)
    }

    /// Evict least-recently-used entries until the byte budget holds. The entry
    /// ceiling is enforced by [`LruCache`] itself on `put`, but that eviction
    /// bypasses our byte counter, so re-derive it whenever the two could drift.
    fn evict_to_budget(&mut self) {
        // `LruCache::put` may have silently evicted to honour the entry cap.
        // Re-deriving the total is O(entries) and only runs on a store, which is
        // orders of magnitude rarer than a lookup.
        self.bytes = self.entries.iter().map(|(_, e)| e.heap_bytes()).sum();
        while self.bytes > self.max_bytes && self.entries.len() > 1 {
            match self.entries.pop_lru() {
                Some((_, old)) => self.bytes = self.bytes.saturating_sub(old.heap_bytes()),
                None => break,
            }
        }
    }
}

impl Default for EtagCache {
    fn default() -> Self {
        Self::new(ETAG_CACHE_MAX_ENTRIES, ETAG_CACHE_MAX_BYTES)
    }
}

// ---------------------------------------------------------------------------
// Process globals
// ---------------------------------------------------------------------------

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static ETAG_CACHE: OnceLock<Mutex<EtagCache>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

fn etag_cache() -> &'static Mutex<EtagCache> {
    ETAG_CACHE.get_or_init(|| Mutex::new(EtagCache::default()))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fold one GitHub response into the process-global meter.
///
/// Cheap by construction — a hash lookup and a few adds under a short-held
/// mutex — so it is safe on **every** response, which is the only sampling
/// discipline that survives contact with a 62-calls-per-minute poller.
///
/// A poisoned mutex is ignored rather than propagated: losing the meter must
/// never fail the GitHub call it was measuring.
pub fn record_response(
    consumer: &str,
    url: &str,
    cache_mode: CacheMode,
    status: u16,
    headers: &HeaderMap,
) {
    let obs = parse_rate_limit(headers);
    if let Ok(mut reg) = registry().lock() {
        reg.record(consumer, url, cache_mode, status, obs, now_unix());
    }
}

/// Record a request that never produced a response. See
/// [`Registry::record_transport_error`].
pub fn record_transport_error(consumer: &str, url: &str, cache_mode: CacheMode) {
    if let Ok(mut reg) = registry().lock() {
        reg.record_transport_error(consumer, url, cache_mode);
    }
}

/// Record which credential this process resolved — the resolution-step NAME,
/// never the token itself. See [`CredentialIdentity`].
pub fn record_identity(source: &str, login: Option<String>) {
    if let Ok(mut reg) = registry().lock() {
        reg.record_identity(source, login);
    }
}

/// The current budget snapshot, reporting the top [`TOP_CONSUMERS_N`] rows.
///
/// On a poisoned mutex this returns an EMPTY snapshot, whose `drift_class` is
/// `unknown` — the honest answer, since nothing could be read.
pub fn snapshot() -> GithubBudgetSnapshot {
    snapshot_top(TOP_CONSUMERS_N)
}

/// The current budget snapshot with an explicit row budget.
///
/// Same data as [`snapshot`], but for a caller that needs to find ONE named row
/// rather than read the busiest twelve — a quiet row (a single transport error,
/// say) sorts below every busy one and would otherwise be elided into
/// `consumersElided` where it cannot be inspected.
pub fn snapshot_top(top_n: usize) -> GithubBudgetSnapshot {
    let threshold = low_fraction();
    match registry().lock() {
        Ok(reg) => reg.snapshot(top_n, threshold, now_unix()),
        Err(_) => Registry::new().snapshot(top_n, threshold, now_unix()),
    }
}

/// `Some` when the observed budget is below the [`low_fraction`] threshold;
/// `None` when it is healthy **or unknown**.
pub fn low_headroom() -> Option<LowHeadroom> {
    let threshold = low_fraction();
    registry().lock().ok()?.low_headroom(threshold)
}

/// The process-wide `X-RateLimit-*` reading every GitHub client backs off
/// against. `None` is UNKNOWN — see [`Registry::last_rate_limit`].
pub fn last_rate_limit() -> Option<RateLimitObservation> {
    registry().lock().ok()?.last_rate_limit()
}

/// The `If-None-Match` value to send for `url`, if one is held.
pub fn etag_for(url: &str) -> Option<String> {
    etag_cache().lock().ok()?.etag_for(url)
}

/// Cache a 2xx body against its `ETag`, for a later 304 to replay.
pub fn store(url: &str, etag: &str, body: Bytes) {
    if let Ok(mut cache) = etag_cache().lock() {
        cache.store(url, etag, body);
    }
}

/// The cached body for `url`, to serve in place of a `304 Not Modified`.
pub fn replay(url: &str) -> Option<Bytes> {
    etag_cache().lock().ok()?.replay(url)
}

/// Drop a cached body that is known to be unreplayable.
pub fn invalidate(url: &str) {
    if let Ok(mut cache) = etag_cache().lock() {
        cache.invalidate(url);
    }
}

/// `(entries, approximate bytes)` held by the process-global ETag cache.
pub fn etag_cache_stats() -> (usize, usize) {
    match etag_cache().lock() {
        Ok(cache) => cache.stats(),
        Err(_) => (0, 0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    /// A synthetic `X-RateLimit-*` header set.
    fn rl_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).expect("static header name"),
                HeaderValue::from_str(v).expect("static header value"),
            );
        }
        h
    }

    fn obs(limit: i64, remaining: i64) -> RateLimitObservation {
        RateLimitObservation {
            limit: Some(limit),
            remaining: Some(remaining),
            used: Some(limit - remaining),
            reset_at_unix: Some(1_800_000_000),
            resource: Some("core".to_string()),
        }
    }

    // -- URL template normalization -----------------------------------------

    #[test]
    fn normalize_collapses_numeric_pr_id() {
        assert_eq!(
            normalize_url_template("/repos/qontinui/qontinui-web/pulls/1234"),
            "/repos/qontinui/qontinui-web/pulls/N"
        );
    }

    #[test]
    fn normalize_strips_scheme_host_and_query() {
        assert_eq!(
            normalize_url_template(
                "https://api.github.com/repos/o/r/pulls/123?state=open&per_page=100"
            ),
            "/repos/o/r/pulls/N"
        );
    }

    #[test]
    fn normalize_collapses_40_hex_sha() {
        let sha = "a1b2c3d4e5f6070809101112131415161718191a";
        assert_eq!(
            normalize_url_template(&format!("/repos/o/r/commits/{sha}/check-runs")),
            "/repos/o/r/commits/{sha}/check-runs"
        );
    }

    #[test]
    fn normalize_preserves_non_numeric_segments() {
        // Owner, repo and endpoint words must survive — collapsing them would
        // merge endpoints that are genuinely distinct.
        assert_eq!(
            normalize_url_template("/repos/o/r/actions/runs"),
            "/repos/o/r/actions/runs"
        );
        assert_eq!(
            normalize_url_template("https://api.github.com/user"),
            "/user"
        );
    }

    #[test]
    fn normalize_short_hexish_segment_is_not_a_sha() {
        assert_eq!(
            normalize_url_template("/repos/o/r/commits/abc1234/status"),
            "/repos/o/r/commits/abc1234/status"
        );
    }

    // -- Header parsing -----------------------------------------------------

    #[test]
    fn parse_rate_limit_reads_the_whole_family() {
        let h = rl_headers(&[
            ("x-ratelimit-limit", "5000"),
            ("x-ratelimit-remaining", "4300"),
            ("x-ratelimit-used", "700"),
            ("x-ratelimit-reset", "1800000000"),
            ("x-ratelimit-resource", "core"),
        ]);
        let o = parse_rate_limit(&h);
        assert_eq!(o.limit, Some(5000));
        assert_eq!(o.remaining, Some(4300));
        assert_eq!(o.used, Some(700));
        assert_eq!(o.reset_at_unix, Some(1_800_000_000));
        assert_eq!(o.resource.as_deref(), Some("core"));
    }

    #[test]
    fn parse_rate_limit_treats_missing_and_garbage_as_unknown() {
        let o = parse_rate_limit(&rl_headers(&[("x-ratelimit-remaining", "not-a-number")]));
        assert!(o.is_empty(), "garbage must not become a zero reading");
    }

    // -- Charged vs 304 accounting ------------------------------------------

    #[test]
    fn charged_and_not_modified_are_accounted_separately() {
        let mut reg = Registry::new();
        for _ in 0..3 {
            reg.record(
                "pr_watcher",
                "https://api.github.com/repos/o/r/pulls/7",
                CacheMode::Cached,
                200,
                obs(5000, 4000),
                10,
            );
        }
        for _ in 0..2 {
            reg.record(
                "pr_watcher",
                "https://api.github.com/repos/o/r/pulls/8",
                CacheMode::Cached,
                304,
                obs(5000, 4000),
                11,
            );
        }
        // A 4xx still costs a point.
        reg.record(
            "pr_watcher",
            "https://api.github.com/repos/o/r/pulls/9",
            CacheMode::Cached,
            404,
            obs(5000, 4000),
            12,
        );

        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 20);
        assert_eq!(snap.totals.charged, 4);
        assert_eq!(snap.totals.not_modified, 2);
        assert_eq!(snap.totals.sent(), 6);
        // All six collapse to ONE template row.
        assert_eq!(snap.consumers.len(), 1);
        assert_eq!(snap.consumers[0].template, "/repos/o/r/pulls/N");
    }

    #[test]
    fn rate_limited_and_transport_error_are_counted_apart_from_charged() {
        let mut reg = Registry::new();
        // 403 WITH remaining:0 is an empty-bucket refusal.
        reg.record(
            "ci_node",
            "/repos/o/r/pulls/1",
            CacheMode::Fresh,
            403,
            obs(5000, 0),
            10,
        );
        // 403 WITHOUT remaining:0 is a permission error — GitHub charged it.
        reg.record(
            "ci_node",
            "/repos/o/r/pulls/2",
            CacheMode::Fresh,
            403,
            obs(5000, 4999),
            11,
        );
        reg.record_transport_error("ci_node", "/repos/o/r/pulls/3", CacheMode::Fresh);

        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 20);
        assert_eq!(snap.totals.rate_limited, 1);
        assert_eq!(snap.totals.charged, 1);
        assert_eq!(snap.totals.transport_error, 1);
        assert_eq!(snap.totals.not_modified, 0);
    }

    #[test]
    fn cache_modes_of_one_template_are_separate_rows() {
        let mut reg = Registry::new();
        reg.record(
            "r",
            "/repos/o/r/pulls/1",
            CacheMode::Cached,
            200,
            obs(5000, 4000),
            1,
        );
        reg.record(
            "r",
            "/repos/o/r/pulls/1",
            CacheMode::Fresh,
            200,
            obs(5000, 4000),
            1,
        );
        assert_eq!(reg.row_count(), 2);
    }

    // -- Cache effectiveness ------------------------------------------------

    #[test]
    fn cache_effectiveness_insufficient_data_is_unknown_not_zero_percent() {
        // 0 of 3 free — noise, not a broken cache.
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 3, 0),
            Some("insufficient_data")
        );
        // One below the floor is still UNKNOWN.
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 19, 0),
            Some("insufficient_data")
        );
    }

    #[test]
    fn cache_effectiveness_ineffective_degraded_effective() {
        // The 2026-08-31 shape: 1150 charged / 2 free = 0.0017.
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 1148, 2),
            Some("ineffective")
        );
        // Exactly at the 5% boundary is still ineffective (inclusive).
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 95, 5),
            Some("ineffective")
        );
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 75, 25),
            Some("degraded")
        );
        // Exactly at 50% is effective (inclusive).
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 50, 50),
            Some("effective")
        );
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Cached, 7, 93),
            Some("effective")
        );
    }

    #[test]
    fn fresh_and_uncacheable_rows_get_no_effectiveness_verdict() {
        // A `fresh` row's 0% is correct BY CONSTRUCTION — judging it is the
        // exact misdiagnosis the cache_mode split exists to remove.
        assert_eq!(derive_cache_effectiveness(CacheMode::Fresh, 1000, 0), None);
        assert_eq!(
            derive_cache_effectiveness(CacheMode::Uncacheable, 1000, 0),
            None
        );
    }

    #[test]
    fn snapshot_attaches_effectiveness_only_to_cached_rows() {
        let mut reg = Registry::new();
        for i in 0..30 {
            reg.record(
                "pr_watcher",
                "/repos/o/r/pulls/1",
                CacheMode::Cached,
                if i < 27 { 304 } else { 200 },
                obs(5000, 4000),
                1,
            );
            reg.record(
                "pr_watcher",
                "/repos/o/r/issues/1/comments",
                CacheMode::Fresh,
                200,
                obs(5000, 4000),
                1,
            );
        }
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 2);
        let cached = snap
            .consumers
            .iter()
            .find(|r| r.cache_mode == CacheMode::Cached)
            .expect("cached row present");
        assert_eq!(cached.cache_effectiveness, Some("effective"));
        let fresh = snap
            .consumers
            .iter()
            .find(|r| r.cache_mode == CacheMode::Fresh)
            .expect("fresh row present");
        assert_eq!(fresh.cache_effectiveness, None);
    }

    // -- Drift class / absence ----------------------------------------------

    #[test]
    fn drift_class_is_unknown_before_any_observation() {
        let reg = Registry::new();
        assert_eq!(reg.drift_class(0.20), "unknown");
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 42);
        assert_eq!(snap.drift_class, "unknown");
        assert_eq!(snap.remaining_fraction, None);
        assert_eq!(snap.observed_at_unix, None);
        assert_eq!(snap.identity, None);
        assert_eq!(snap.totals, BudgetTotals::default());
    }

    #[test]
    fn drift_class_nominal_low_exhausted() {
        let mut reg = Registry::new();
        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 4000), 1);
        assert_eq!(reg.drift_class(0.20), "nominal");

        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 500), 2);
        assert_eq!(reg.drift_class(0.20), "low");

        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 50), 3);
        assert_eq!(reg.drift_class(0.20), "exhausted");
    }

    #[test]
    fn a_response_without_ratelimit_headers_does_not_erase_the_last_reading() {
        let mut reg = Registry::new();
        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 4000), 1);
        reg.record(
            "c",
            "/user",
            CacheMode::Fresh,
            200,
            RateLimitObservation::default(),
            2,
        );
        assert_eq!(reg.remaining_fraction(), Some(0.8));
        assert_eq!(
            reg.snapshot(TOP_CONSUMERS_N, 0.20, 3).observed_at_unix,
            Some(1)
        );
    }

    #[test]
    fn a_zero_limit_is_unknown_not_zero_headroom() {
        let mut reg = Registry::new();
        reg.record(
            "c",
            "/user",
            CacheMode::Fresh,
            200,
            RateLimitObservation {
                limit: Some(0),
                remaining: Some(0),
                ..RateLimitObservation::default()
            },
            1,
        );
        assert_eq!(reg.drift_class(0.20), "unknown");
        assert_eq!(reg.low_headroom(0.20), None);
    }

    // -- Low headroom + threshold -------------------------------------------

    #[test]
    fn low_headroom_is_none_when_unmeasured_and_when_healthy() {
        let reg = Registry::new();
        assert_eq!(reg.low_headroom(0.20), None, "unmeasured is not low");

        let mut reg = Registry::new();
        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 4000), 1);
        assert_eq!(reg.low_headroom(0.20), None);
    }

    #[test]
    fn low_headroom_fires_below_the_threshold_and_names_the_credential() {
        let mut reg = Registry::new();
        reg.record_identity("gh auth token", Some("jspinak".to_string()));
        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 900), 1);
        let low = reg.low_headroom(0.20).expect("0.18 < 0.20 fires");
        assert_eq!(low.remaining, 900);
        assert_eq!(low.limit, 5000);
        assert!((low.fraction - 0.18).abs() < 1e-9);
        assert_eq!(low.threshold, 0.20);
        assert_eq!(low.resource.as_deref(), Some("core"));
        let id = low.identity.expect("identity recorded");
        assert_eq!(id.source, "gh auth token");
        assert_eq!(id.login.as_deref(), Some("jspinak"));
    }

    #[test]
    fn low_headroom_does_not_fire_exactly_at_the_threshold() {
        let mut reg = Registry::new();
        reg.record("c", "/user", CacheMode::Fresh, 200, obs(5000, 1000), 1);
        assert_eq!(reg.low_headroom(0.20), None, "strict <, matching coord");
    }

    #[test]
    fn low_fraction_defaults_and_honours_a_valid_env_override() {
        // The env is process-global; this test owns the variable and restores it.
        let prior = std::env::var(LOW_FRACTION_ENV).ok();

        std::env::remove_var(LOW_FRACTION_ENV);
        assert_eq!(low_fraction(), DEFAULT_LOW_FRACTION);

        std::env::set_var(LOW_FRACTION_ENV, "0.35");
        assert_eq!(low_fraction(), 0.35);

        std::env::set_var(LOW_FRACTION_ENV, " 0.05 ");
        assert_eq!(low_fraction(), 0.05, "surrounding whitespace is trimmed");

        // Out of range in both directions, and unparseable — all fall back
        // rather than arming a never-fires / always-fires threshold.
        for bad in ["0", "1", "1.5", "-0.2", "", "nan", "twenty percent"] {
            std::env::set_var(LOW_FRACTION_ENV, bad);
            assert_eq!(
                low_fraction(),
                DEFAULT_LOW_FRACTION,
                "{bad:?} must fall back to the default"
            );
        }

        match prior {
            Some(v) => std::env::set_var(LOW_FRACTION_ENV, v),
            None => std::env::remove_var(LOW_FRACTION_ENV),
        }
    }

    // -- Histogram cap ------------------------------------------------------

    #[test]
    fn histogram_caps_and_counts_what_it_drops() {
        let mut reg = Registry::new();
        // Distinct, non-collapsible templates so the cap is what binds.
        for i in 0..(TEMPLATE_KEY_CAP + 15) {
            reg.record(
                "sweeper",
                &format!("/repos/o/r/endpoint-{i}"),
                CacheMode::Fresh,
                200,
                obs(5000, 4000),
                1,
            );
        }
        assert_eq!(reg.row_count(), TEMPLATE_KEY_CAP);

        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 2);
        assert_eq!(snap.templates_dropped, 15);
        assert_eq!(snap.calls_dropped, 15);
        // The histogram truncates; the TOTALS do not.
        assert_eq!(snap.totals.charged, (TEMPLATE_KEY_CAP + 15) as u64);
        assert_eq!(snap.consumers.len(), TOP_CONSUMERS_N);
        assert_eq!(snap.consumers_elided, TEMPLATE_KEY_CAP - TOP_CONSUMERS_N);
    }

    #[test]
    fn snapshot_orders_by_requests_sent_descending() {
        let mut reg = Registry::new();
        for _ in 0..5 {
            reg.record("a", "/quiet", CacheMode::Fresh, 200, obs(5000, 4000), 1);
        }
        for _ in 0..50 {
            reg.record("a", "/loud", CacheMode::Fresh, 200, obs(5000, 4000), 1);
        }
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 2);
        assert_eq!(snap.consumers[0].template, "/loud");
        assert_eq!(snap.consumers[0].charged, 50);
        assert_eq!(snap.consumers[1].template, "/quiet");
    }

    // -- Identity -----------------------------------------------------------

    #[test]
    fn identity_records_the_source_name_and_never_a_token() {
        let mut reg = Registry::new();
        reg.record_identity("GITHUB_TOKEN", None);
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 1);
        let id = snap.identity.expect("identity present");
        assert_eq!(id.source, "GITHUB_TOKEN");
        assert_eq!(id.login, None);

        // A caller that fumbles the token in must not have it stored.
        reg.record_identity("ghp_0123456789abcdefghijklmnopqrstuvwx", None);
        assert_eq!(
            reg.snapshot(TOP_CONSUMERS_N, 0.20, 2)
                .identity
                .expect("identity present")
                .source,
            "redacted"
        );

        reg.record_identity(&"x".repeat(200), None);
        assert_eq!(
            reg.snapshot(TOP_CONSUMERS_N, 0.20, 3)
                .identity
                .expect("identity present")
                .source,
            "redacted"
        );
    }

    #[test]
    fn identity_blank_source_reads_as_unknown() {
        let mut reg = Registry::new();
        reg.record_identity("   ", Some("   ".to_string()));
        let id = reg
            .snapshot(TOP_CONSUMERS_N, 0.20, 1)
            .identity
            .expect("identity present");
        assert_eq!(id.source, "unknown");
        assert_eq!(id.login, None, "a blank login is UNKNOWN, not a login");
    }

    // -- ETag cache ---------------------------------------------------------

    #[test]
    fn etag_store_and_replay_round_trip() {
        let mut cache = EtagCache::new(8, 1024);
        assert_eq!(cache.etag_for("/repos/o/r/pulls/1"), None);
        assert_eq!(cache.replay("/repos/o/r/pulls/1"), None);

        cache.store(
            "/repos/o/r/pulls/1",
            "W/\"abc\"",
            Bytes::from_static(b"{\"number\":1}"),
        );
        assert_eq!(
            cache.etag_for("/repos/o/r/pulls/1").as_deref(),
            Some("W/\"abc\"")
        );
        assert_eq!(
            cache.replay("/repos/o/r/pulls/1"),
            Some(Bytes::from_static(b"{\"number\":1}"))
        );

        // A store for the same URL replaces rather than accumulating.
        cache.store("/repos/o/r/pulls/1", "W/\"def\"", Bytes::from_static(b"{}"));
        let (entries, bytes) = cache.stats();
        assert_eq!(entries, 1);
        assert_eq!(bytes, 2 + "W/\"def\"".len());
    }

    #[test]
    fn etag_store_ignores_an_empty_etag() {
        let mut cache = EtagCache::new(8, 1024);
        cache.store("/u", "   ", Bytes::from_static(b"body"));
        assert_eq!(cache.stats().0, 0, "no ETag means nothing to replay");
        assert_eq!(cache.replay("/u"), None);
    }

    #[test]
    fn etag_cache_evicts_on_the_entry_ceiling() {
        let mut cache = EtagCache::new(2, 1024 * 1024);
        cache.store("/a", "1", Bytes::from_static(b"a"));
        cache.store("/b", "2", Bytes::from_static(b"b"));
        // Touch /a so /b is the least-recently-used.
        assert!(cache.etag_for("/a").is_some());
        cache.store("/c", "3", Bytes::from_static(b"c"));

        assert_eq!(cache.stats().0, 2);
        assert!(cache.replay("/b").is_none(), "LRU entry evicted");
        assert!(cache.replay("/a").is_some());
        assert!(cache.replay("/c").is_some());
    }

    #[test]
    fn etag_cache_evicts_on_the_byte_budget() {
        // Entry ceiling is generous; the byte budget is what must bind — a
        // handful of MB-scale `/compare/{a}...{b}` bodies is not "small".
        let mut cache = EtagCache::new(64, 300);
        for i in 0..6 {
            cache.store(&format!("/big/{i}"), "e", Bytes::from(vec![0u8; 100]));
        }
        let (entries, bytes) = cache.stats();
        assert!(bytes <= 300, "byte budget held, got {bytes}");
        assert!(entries < 6, "eviction happened, {entries} entries left");
        assert!(cache.replay("/big/5").is_some(), "newest survives");
        assert!(cache.replay("/big/0").is_none(), "oldest evicted");
    }

    #[test]
    fn etag_cache_invalidate_drops_the_entry_and_its_bytes() {
        let mut cache = EtagCache::new(8, 1024);
        cache.store("/u", "e", Bytes::from_static(b"body"));
        cache.invalidate("/u");
        assert_eq!(cache.stats(), (0, 0));
        assert_eq!(cache.replay("/u"), None);
    }

    #[test]
    fn etag_cache_keys_on_the_full_url_not_the_template() {
        // Two PR numbers are genuinely different bodies; collapsing them would
        // replay one PR's JSON for another.
        let mut cache = EtagCache::new(8, 1024);
        cache.store("/repos/o/r/pulls/1", "e1", Bytes::from_static(b"one"));
        cache.store("/repos/o/r/pulls/2", "e2", Bytes::from_static(b"two"));
        assert_eq!(
            cache.replay("/repos/o/r/pulls/1").as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(
            cache.replay("/repos/o/r/pulls/2").as_deref(),
            Some(&b"two"[..])
        );
    }

    // -- Global wrappers ----------------------------------------------------

    #[test]
    fn global_wrappers_do_not_panic_and_report_honestly() {
        // The process-global registry is shared with any other test that touches
        // it, so this asserts only shape-and-liveness invariants, never counts.
        record_identity("QONTINUI_RUNNER_GITHUB_TOKEN", None);
        record_response(
            "session_pr_reconciler",
            "https://api.github.com/repos/o/r/pulls/1",
            CacheMode::Cached,
            304,
            &rl_headers(&[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "4321"),
                ("x-ratelimit-resource", "core"),
            ]),
        );
        record_transport_error(
            "session_pr_reconciler",
            "/repos/o/r/pulls/2",
            CacheMode::Fresh,
        );

        let snap = snapshot();
        assert_eq!(snap.limit, Some(5000));
        assert_eq!(snap.remaining, Some(4321));
        assert_eq!(snap.drift_class, "nominal");
        assert!(snap.totals.not_modified >= 1);
        assert!(snap.totals.transport_error >= 1);
        assert!(snap
            .consumers
            .iter()
            .any(|r| r.template == "/repos/o/r/pulls/N"));
        assert_eq!(low_headroom(), None);

        store("/global/etag/probe", "e", Bytes::from_static(b"payload"));
        assert_eq!(etag_for("/global/etag/probe").as_deref(), Some("e"));
        assert_eq!(
            replay("/global/etag/probe"),
            Some(Bytes::from_static(b"payload"))
        );
        assert!(etag_cache_stats().0 >= 1);
        invalidate("/global/etag/probe");
        assert_eq!(replay("/global/etag/probe"), None);
    }

    #[test]
    fn snapshot_serializes_to_camel_case_json() {
        let reg = Registry::new();
        let v = serde_json::to_value(reg.snapshot(TOP_CONSUMERS_N, 0.20, 7))
            .expect("snapshot serializes");
        assert_eq!(v["driftClass"], "unknown");
        assert_eq!(v["remainingFraction"], serde_json::Value::Null);
        assert_eq!(v["templatesDropped"], 0);
        assert_eq!(v["totals"]["notModified"], 0);
    }
}
