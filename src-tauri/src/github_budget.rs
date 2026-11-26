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
//! every 30s (`session_pr_reconciler::POLL_INTERVAL`). Re-measured 2026-09-01:
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

use std::collections::{BTreeMap, HashMap};
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
///
/// Sized to hold one whole session-PR reconciler cycle. A full tick on a box
/// with ~120 open sessions visits ~670 distinct URLs in a fixed order, so a
/// 512-entry LRU evicted every entry before the next tick reached it and the
/// conditional GETs went out unconditional (plan
/// `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`,
/// D5). 4,096 leaves the byte budget below as the binding bound: at the
/// measured ~12.8 KB per entry it holds ~2,600 entries, above one mean-sized
/// cycle. The cache is process-global (the ticket and CI-sibling clients store
/// into it too), so a burst of MB-scale bodies can still evict part of a cycle;
/// `etagCache.approxBytes` near the budget on `/github-budget` is the tell.
const ETAG_CACHE_MAX_ENTRIES: usize = 4_096;

/// Heap budget for the ETag cache. The entry ceiling alone is not a bound: a
/// `/compare/{a}...{b}` body is MB-scale, so 4,096 of them is not "small". Both
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
/// `GET /github-budget` freely. A caller that passes the token itself by mistake would turn
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

/// THE rate-limit classifier: did GitHub refuse this for RATE rather than for
/// permission? Pure, and asked by every door that meters, retries or reports a
/// refusal (`trigger_system::github_api::retry_backoff_secs`, the ticket
/// provider, `ci_node::sibling`), so they cannot disagree.
///
/// - `429` — always a rate refusal (secondary limit).
/// - `403` with `x-ratelimit-remaining: 0` — the PRIMARY bucket is empty.
/// - `403` with `Retry-After` — a SECONDARY limit, budget left.
///
/// A `403` with neither is a permission/SSO refusal.
pub fn is_rate_limited_refusal(
    status: u16,
    retry_after: Option<u64>,
    remaining: Option<i64>,
) -> bool {
    match status {
        429 => true,
        403 => remaining == Some(0) || retry_after.is_some(),
        _ => false,
    }
}

/// `Retry-After` in whole seconds, when GitHub sent one. The HTTP-date form is
/// not used by GitHub's REST API and reads as absent.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
    parse_fraction(std::env::var(LOW_FRACTION_ENV).ok(), DEFAULT_LOW_FRACTION)
}

/// Pure parse of a fraction knob: the value must parse and lie strictly inside
/// `(0, 1)`, else `default`. Used by [`low_fraction`] and for the background
/// reserve, where `0` and `1` are both meaningless thresholds.
pub fn parse_fraction(raw: Option<String>, default: f64) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
        .unwrap_or(default)
}

/// Pure parse of the background GUARD knob, matching coord's
/// `parse_background_guard_fraction`: the CLOSED `[0, 1]` is accepted (`0`
/// leaves only the reserve as the margin), anything else is `default`.
pub fn parse_guard_fraction(raw: Option<String>, default: f64) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Credential key + background headroom (plan
// `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`,
// Phase 3, D4)
// ---------------------------------------------------------------------------

/// Names ONE credential's rate-limit bucket without carrying the credential:
/// the first 12 hex of the token's sha256. Safe to log and to serve.
///
/// The bucket is per credential, and this process spends more than one (the
/// reconciler's resolved token, a ticket provider's configured token, an
/// anonymous sibling probe). A gate that read one shared "last reading" judged
/// whichever credential answered last, so one credential's exhausted bucket
/// could defer — or wave through — another's request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CredentialKey(String);

impl CredentialKey {
    /// Key a token by digest. The token never leaves this function.
    pub fn of_token(token: &str) -> Self {
        use sha2::{Digest as _, Sha256};
        let digest = Sha256::digest(token.trim().as_bytes());
        let hex = hex::encode(digest);
        Self(hex.get(..12).unwrap_or(&hex).to_string())
    }

    /// The unauthenticated (per-IP) bucket.
    pub fn anonymous() -> Self {
        Self("anonymous".to_string())
    }

    /// Key an optional token: `None` is the anonymous bucket.
    pub fn of_optional_token(token: Option<&str>) -> Self {
        token.map_or_else(Self::anonymous, Self::of_token)
    }
}

impl std::fmt::Display for CredentialKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How many distinct credentials the registry keeps a reading (and a login)
/// for. A runner spends a handful; the cap only stops an unbounded map if some
/// caller keyed per request by mistake. Over it, the stalest reading is evicted.
const CREDENTIAL_KEY_CAP: usize = 32;

/// After a TERMINAL `GET /user` refusal (`401`/`403`), how long before the
/// login is asked again. A credential that cannot answer `/user` would
/// otherwise be re-asked every tick for the life of the process.
const LOGIN_RETRY_AFTER_SECS: i64 = 6 * 3600;

/// GitHub's rate-limit window, seconds.
const RATELIMIT_WINDOW_SECS: f64 = 3600.0;

/// The oldest reading [`github_headroom`] will judge from. Older than this and
/// the reading is UNKNOWN, which does not defer — so a deferring background
/// client re-probes at most once a minute with its next conditional GET (free
/// on `304`, and it still carries the headers), and its reading never freezes
/// near the reserve line. Same bound as coord's `HEADROOM_SNAPSHOT_MAX_AGE_SECS`.
const HEADROOM_READING_MAX_AGE_SECS: i64 = 60;

/// Floor on "seconds elapsed in this window" when projecting its burn, so the
/// first seconds after a reset cannot project an absurd rate. Same as coord.
const HEADROOM_MIN_ELAPSED_SECS: f64 = 60.0;

/// Default reserve, as a fraction of the limit, that background spend leaves
/// for the identity's interactive sessions at the end of the window.
const DEFAULT_BACKGROUND_MIN_RESERVE_FRACTION: f64 = 0.20;

/// Default safety margin on top of the projection, pro-rata to the seconds
/// left (coord's `DEFAULT_BACKGROUND_GUARD_FRACTION`).
const DEFAULT_BACKGROUND_GUARD_FRACTION: f64 = 0.10;

/// Env override for [`DEFAULT_BACKGROUND_MIN_RESERVE_FRACTION`].
const BACKGROUND_MIN_RESERVE_FRACTION_ENV: &str = "QONTINUI_GITHUB_BACKGROUND_MIN_RESERVE_FRACTION";

/// Env override for [`DEFAULT_BACKGROUND_GUARD_FRACTION`].
const BACKGROUND_GUARD_FRACTION_ENV: &str = "QONTINUI_GITHUB_BACKGROUND_GUARD_FRACTION";

/// Who a request is spent for.
///
/// `Background` is a poller nobody is waiting on (the session-PR reconciler,
/// ticket sync): under the headroom line it is DEFERRED — never sent, never
/// slept on. `Interactive` is everything else and keeps the old floor (sleep
/// one chunk near empty, retry one rate-limited refusal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpendClass {
    #[default]
    Interactive,
    Background,
}

/// The two knobs of [`github_headroom`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadroomKnobs {
    /// Fraction of the limit the window must still hold at reset.
    pub min_reserve_fraction: f64,
    /// Safety margin on top, as a fraction of the limit, pro-rata to the
    /// seconds left — which is what relaxes the rule near reset.
    pub guard_fraction: f64,
}

impl HeadroomKnobs {
    /// The env-configured knobs. The reserve is parsed like
    /// `QONTINUI_GITHUB_BUDGET_LOW_FRACTION` ([`parse_fraction`], open
    /// `(0, 1)`); the guard like coord's ([`parse_guard_fraction`], closed
    /// `[0, 1]`).
    pub fn from_env() -> Self {
        Self {
            min_reserve_fraction: parse_fraction(
                std::env::var(BACKGROUND_MIN_RESERVE_FRACTION_ENV).ok(),
                DEFAULT_BACKGROUND_MIN_RESERVE_FRACTION,
            ),
            guard_fraction: parse_guard_fraction(
                std::env::var(BACKGROUND_GUARD_FRACTION_ENV).ok(),
                DEFAULT_BACKGROUND_GUARD_FRACTION,
            ),
        }
    }
}

impl Default for HeadroomKnobs {
    fn default() -> Self {
        Self {
            min_reserve_fraction: DEFAULT_BACKGROUND_MIN_RESERVE_FRACTION,
            guard_fraction: DEFAULT_BACKGROUND_GUARD_FRACTION,
        }
    }
}

/// The three answers [`github_headroom`] can give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Headroom {
    /// GitHub's counters project the window to end above the margin.
    Holds,
    /// GitHub's counters project the window to end below the margin.
    Short,
    /// No usable, fresh, current-window reading — nothing established. Never
    /// a reason to defer.
    Unknown,
}

/// PURE — will this credential's window end with room to spare, projected from
/// GitHub's OWN counters? A port of qontinui-coord's `github_headroom`
/// (`crates/coord/src/github_app.rs`, #2635), with the floor expressed as a
/// fraction of the limit:
///
/// ```text
/// secs_to_reset = min(reset − now, 3600)
/// elapsed       = max(3600 − (reset − now), 60)
/// projected     = remaining − (limit − remaining) / elapsed × secs_to_reset
/// Holds  iff  projected ≥ min_reserve × limit + guard × limit × secs_to_reset / 3600
/// ```
///
/// `remaining` is GitHub's count for the whole identity, so the burn it implies
/// covers every box and every session on it — the headroom holds fleet-wide
/// without coordination. There is no "N minutes to reset" special case: the
/// margin's `secs_to_reset` term already relaxes the rule near reset, and a flat
/// percentage would ignore the burn rate.
///
/// UNKNOWN on an unknown `remaining`/`limit`/`reset`, a non-positive `limit`, a
/// window that has reset (`reset ≤ now`), and a reading older than
/// [`HEADROOM_READING_MAX_AGE_SECS`] (or stamped in the future).
pub fn github_headroom(
    remaining: Option<i64>,
    limit: Option<i64>,
    reset_at_unix: Option<i64>,
    reading_age_secs: i64,
    now_unix: i64,
    knobs: HeadroomKnobs,
) -> Headroom {
    let (Some(remaining), Some(limit), Some(reset_at_unix)) = (remaining, limit, reset_at_unix)
    else {
        return Headroom::Unknown;
    };
    if limit <= 0
        || reset_at_unix <= now_unix
        || !(0..=HEADROOM_READING_MAX_AGE_SECS).contains(&reading_age_secs)
    {
        return Headroom::Unknown;
    }
    let raw_secs_to_reset = (reset_at_unix - now_unix) as f64;
    let secs_to_reset = raw_secs_to_reset.min(RATELIMIT_WINDOW_SECS);
    let elapsed = (RATELIMIT_WINDOW_SECS - raw_secs_to_reset).max(HEADROOM_MIN_ELAPSED_SECS);
    let used = (limit - remaining).max(0) as f64;
    let projected = remaining as f64 - used / elapsed * secs_to_reset;
    let limit = limit as f64;
    let margin = knobs.min_reserve_fraction * limit
        + knobs.guard_fraction * limit * secs_to_reset / RATELIMIT_WINDOW_SECS;
    if projected >= margin {
        Headroom::Holds
    } else {
        Headroom::Short
    }
}

/// A background request the headroom gate decided against. It was NOT sent:
/// a caller must read it as "not now", never as a GitHub failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetDeferral {
    pub credential: CredentialKey,
    pub reason: DeferralReason,
}

/// Why the gate decided against a background request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferralReason {
    /// The credential's own fresh reading projects the window short.
    ProjectedShort {
        remaining: i64,
        limit: i64,
        reset_at_unix: i64,
    },
    /// GitHub refused this credential for rate and said when to come back
    /// (`Retry-After`, or the primary window's reset); that time has not come.
    RetryAfter { not_before_unix: i64 },
}

impl std::fmt::Display for BudgetDeferral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            DeferralReason::ProjectedShort {
                remaining,
                limit,
                reset_at_unix,
            } => write!(
                f,
                "GitHub background request deferred (not sent): credential {} is projected \
                 short of its reserve ({remaining}/{limit} remaining, window resets at \
                 {reset_at_unix})",
                self.credential
            ),
            DeferralReason::RetryAfter { not_before_unix } => write!(
                f,
                "GitHub background request deferred (not sent): credential {} was refused for \
                 rate and may not be asked again before {not_before_unix}",
                self.credential
            ),
        }
    }
}

/// One credential's latest `X-RateLimit-*` reading and when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialReading {
    obs: RateLimitObservation,
    observed_at_unix: i64,
}

// ---------------------------------------------------------------------------
// The heartbeat's user-bucket report (plan
// `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`,
// Phase 4)
// ---------------------------------------------------------------------------

/// The consumer whose credential the heartbeat reports: the session-PR
/// reconciler, the runner's standing spender on the operator's USER token.
pub const USER_BUDGET_REPORT_CONSUMER: &str = "session_pr_reconciler";

/// The oldest reading the heartbeat will report. coord stamps the row with ITS
/// ingest clock and counts it fresh for 120 s, so a reading older than that
/// would arrive already stale yet read as current — omitted instead.
const USER_BUDGET_REPORT_MAX_AGE_SECS: i64 = 120;

/// The furthest-ahead reset the heartbeat will report. A GitHub window is at
/// most an hour; coord refuses a reset more than ~61 min out. Kept inside
/// coord's bound (an hour plus 30 s of clock skew) so a block the runner sends
/// is a block coord records.
const USER_BUDGET_REPORT_MAX_RESET_AHEAD_SECS: i64 = 3600 + 30;

/// coord refuses a `limit` above this.
const USER_BUDGET_REPORT_MAX_LIMIT: i64 = 1_000_000;

/// coord keeps at most this many consumer labels (the busiest); the runner
/// sends no more than it keeps.
const USER_BUDGET_REPORT_MAX_CONSUMERS: usize = 64;

/// A consumer label coord keeps: `[a-z0-9_:.-]{1,64}`. Every runner consumer
/// is a `&'static str` like `session_pr_reconciler`; one that is not is
/// dropped here rather than sent to be dropped there.
fn is_reportable_consumer_label(label: &str) -> bool {
    (1..=64).contains(&label.len())
        && label.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b':' | b'.' | b'-')
        })
}

/// The `github_user_budget` block on the device-register heartbeat — the
/// reconciler's credential's own `X-RateLimit-*` reading, so a burn of the
/// shared user bucket on one box is visible from every other.
///
/// **snake_case on the wire, by coord's contract** (`GithubUserBudgetReport`
/// in qontinui-coord `routes_phase3.rs`). Deliberately NOT the camelCase serde
/// of [`GithubBudgetSnapshot`]: a renamed key here is silently ignored by
/// coord and the block never lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserBudgetReport {
    /// The authenticated login of the reported credential. coord stores only
    /// its digest. Never empty — see [`build_user_budget_report`].
    pub login: String,
    pub limit: i64,
    pub remaining: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<i64>,
    /// `X-RateLimit-Reset`, unix seconds.
    pub reset_at_unix: i64,
    /// `X-RateLimit-Resource`, e.g. `core`.
    pub resource: String,
    /// Cumulative CHARGED calls per consumer since `process_started_at_unix`
    /// (every credential this process spent, not only the reported one).
    pub charged_by_consumer: BTreeMap<String, u64>,
    /// When this process's meter started counting — the origin of
    /// `charged_by_consumer`, so a reader can tell a restart from a reset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_started_at_unix: Option<i64>,
}

/// PURE — the heartbeat's user-bucket block from one credential's reading, or
/// `None` when there is nothing honest to send:
///
/// - no login, or a blank one — coord would file it under
///   `user:unidentified`, and an unidentified bucket is exactly the gap this
///   block closes, so an absent login is never sent as a guess;
/// - an unknown `limit`/`remaining`/`reset`/`resource`, a limit outside
///   `1..=1_000_000`, a negative or over-limit `remaining`, or a
///   negative or over-limit `used`;
/// - a resource that is not `[a-z0-9_]{1,32}`;
/// - a window that has already reset, or a reset implausibly far ahead;
/// - a reading older than [`USER_BUDGET_REPORT_MAX_AGE_SECS`] or stamped in
///   the future.
///
/// `charged_by_consumer` keeps only labels coord accepts, at most the 64
/// busiest (ties by label).
pub fn build_user_budget_report(
    obs: &RateLimitObservation,
    observed_at_unix: i64,
    login: Option<&str>,
    charged_by_consumer: BTreeMap<String, u64>,
    process_started_at_unix: Option<i64>,
    now_unix: i64,
) -> Option<UserBudgetReport> {
    let login = login.map(str::trim).filter(|l| !l.is_empty())?;
    let limit = obs
        .limit
        .filter(|l| (1..=USER_BUDGET_REPORT_MAX_LIMIT).contains(l))?;
    let remaining = obs.remaining.filter(|r| (0..=limit).contains(r))?;
    let used = match obs.used {
        Some(u) if !(0..=limit).contains(&u) => return None,
        u => u,
    };
    let reset_at_unix = obs.reset_at_unix?;
    if reset_at_unix <= now_unix
        || reset_at_unix - now_unix > USER_BUDGET_REPORT_MAX_RESET_AHEAD_SECS
    {
        return None;
    }
    if !(0..=USER_BUDGET_REPORT_MAX_AGE_SECS).contains(&(now_unix - observed_at_unix)) {
        return None;
    }
    let resource = obs.resource.as_deref().map(str::trim).filter(|r| {
        (1..=32).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })?;
    Some(UserBudgetReport {
        login: login.to_string(),
        limit,
        remaining,
        used,
        reset_at_unix,
        resource: resource.to_string(),
        charged_by_consumer: reportable_consumers(charged_by_consumer),
        process_started_at_unix: process_started_at_unix.filter(|t| *t > 0),
    })
}

/// The consumer counts coord keeps: valid labels only, the
/// [`USER_BUDGET_REPORT_MAX_CONSUMERS`] busiest (ties broken by label).
fn reportable_consumers(all: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut ranked: Vec<(String, u64)> = all
        .into_iter()
        .filter(|(label, _)| is_reportable_consumer_label(label))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(USER_BUDGET_REPORT_MAX_CONSUMERS);
    ranked.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

/// One `(consumer, template, cache_mode)` row of the spend histogram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerRow {
    /// Which subsystem issued the call — e.g. `session_pr_reconciler`,
    /// `ci_node`. Free-form on purpose: a new caller should show
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
    /// Rate refusals ([`is_rate_limited_refusal`]): a `429`, or a `403` with
    /// `x-ratelimit-remaining: 0` or `Retry-After`.
    pub rate_limited: u64,
    /// Requests that never produced a response (DNS, TLS, timeout). Counted
    /// apart from `charged` because they say nothing about the budget.
    pub transport_error: u64,
    /// Background requests the headroom gate decided against — NEVER sent, so
    /// counted apart from every outcome above (see [`github_headroom`]).
    pub deferred: u64,
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
    /// Background requests deferred by the headroom gate, never sent.
    pub deferred: u64,
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
    deferred: u64,
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
    /// The latest reading from ANY credential — what the snapshot shows. Never
    /// a gate input: a gate reads [`Self::credentials`] for the bucket it is
    /// about to spend.
    last: Option<RateLimitObservation>,
    observed_at_unix: Option<i64>,
    /// The latest reading per credential, keyed by token digest.
    credentials: HashMap<CredentialKey, CredentialReading>,
    /// The authenticated login per credential, once resolved.
    logins: HashMap<CredentialKey, String>,
    /// The credential each consumer last SPENT (sent a request on) — so a
    /// reader can ask "which bucket is the reconciler billing?" without
    /// re-resolving its token (plan 2026-10-30-…, Phase 4).
    consumer_credentials: HashMap<String, CredentialKey>,
    /// When a login lookup last FAILED, per credential (unix seconds).
    login_failures: HashMap<CredentialKey, i64>,
    /// Per credential, the time before which background spend must not ask
    /// again — set by a rate refusal's `Retry-After` (unix seconds).
    not_before: HashMap<CredentialKey, i64>,
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
        self.record_with_retry_after(consumer, url, cache_mode, status, obs, None, now_unix);
    }

    /// [`Self::record`], also given the response's `Retry-After` — which is
    /// what marks a SECONDARY-limit `403` (budget left) as a rate refusal
    /// rather than a charged permission error ([`is_rate_limited_refusal`]).
    #[allow(clippy::too_many_arguments)]
    pub fn record_with_retry_after(
        &mut self,
        consumer: &str,
        url: &str,
        cache_mode: CacheMode,
        status: u16,
        obs: RateLimitObservation,
        retry_after: Option<u64>,
        now_unix: i64,
    ) {
        // A response with no `X-RateLimit-*` family says nothing about the
        // budget. Keeping the previous reading beats overwriting a real one with
        // a row of `None`s.
        if !obs.is_empty() {
            self.last = Some(obs.clone());
            self.observed_at_unix = Some(now_unix);
        }

        let limited = is_rate_limited_refusal(status, retry_after, obs.remaining);
        let mut delta = RowCounts::default();
        if limited {
            // Refused for rate (an empty bucket, or a secondary limit) — it
            // bought nothing, so it is neither charged nor free.
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

    /// Record a background request the headroom gate deferred. It was never
    /// sent, so it is neither charged, free, refused nor a transport error.
    pub fn record_deferred(&mut self, consumer: &str, url: &str, cache_mode: CacheMode) {
        self.apply(
            consumer,
            url,
            cache_mode,
            RowCounts {
                deferred: 1,
                ..RowCounts::default()
            },
        );
    }

    /// Keep `obs` as `credential`'s latest reading. A response with no
    /// `X-RateLimit-*` family keeps the previous reading, as for [`Self::last`].
    pub fn record_reading(
        &mut self,
        credential: &CredentialKey,
        obs: &RateLimitObservation,
        now_unix: i64,
    ) {
        if obs.is_empty() {
            return;
        }
        if !self.credentials.contains_key(credential)
            && self.credentials.len() >= CREDENTIAL_KEY_CAP
        {
            let stalest = self
                .credentials
                .iter()
                .min_by_key(|(_, r)| r.observed_at_unix)
                .map(|(k, _)| k.clone());
            if let Some(k) = stalest {
                self.credentials.remove(&k);
            }
        }
        self.credentials.insert(
            credential.clone(),
            CredentialReading {
                obs: obs.clone(),
                observed_at_unix: now_unix,
            },
        );
    }

    /// `credential`'s latest reading, or `None` (UNKNOWN) when it has none.
    pub fn rate_limit_for(&self, credential: &CredentialKey) -> Option<RateLimitObservation> {
        self.credentials.get(credential).map(|r| r.obs.clone())
    }

    /// The background gate's verdict for `credential` at `now_unix`: `Err`
    /// only when its OWN fresh reading projects [`Headroom::Short`]. No
    /// reading, or an UNKNOWN one, does not defer.
    pub fn background_verdict(
        &self,
        credential: &CredentialKey,
        now_unix: i64,
        knobs: HeadroomKnobs,
    ) -> Result<(), BudgetDeferral> {
        if let Some(&not_before_unix) = self.not_before.get(credential) {
            if now_unix < not_before_unix {
                return Err(BudgetDeferral {
                    credential: credential.clone(),
                    reason: DeferralReason::RetryAfter { not_before_unix },
                });
            }
        }
        let Some(reading) = self.credentials.get(credential) else {
            return Ok(());
        };
        let obs = &reading.obs;
        let verdict = github_headroom(
            obs.remaining,
            obs.limit,
            obs.reset_at_unix,
            now_unix - reading.observed_at_unix,
            now_unix,
            knobs,
        );
        match (verdict, obs.remaining, obs.limit, obs.reset_at_unix) {
            (Headroom::Short, Some(remaining), Some(limit), Some(reset_at_unix)) => {
                Err(BudgetDeferral {
                    credential: credential.clone(),
                    reason: DeferralReason::ProjectedShort {
                        remaining,
                        limit,
                        reset_at_unix,
                    },
                })
            }
            _ => Ok(()),
        }
    }

    /// GitHub refused `credential` for rate: background spend on it is
    /// deferred until `until_unix` (the later of any two such answers wins).
    /// Interactive callers are not affected.
    pub fn record_not_before(&mut self, credential: &CredentialKey, until_unix: i64) {
        if !self.not_before.contains_key(credential) && self.not_before.len() >= CREDENTIAL_KEY_CAP
        {
            // Full: drop the hold that ends soonest (the one most likely spent).
            let soonest = self
                .not_before
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(k, _)| k.clone());
            if let Some(k) = soonest {
                self.not_before.remove(&k);
            }
        }
        let slot = self
            .not_before
            .entry(credential.clone())
            .or_insert(until_unix);
        *slot = (*slot).max(until_unix);
    }

    /// Remember `credential`'s authenticated login.
    pub fn record_login(&mut self, credential: &CredentialKey, login: &str) {
        let login = login.trim();
        if login.is_empty() {
            return;
        }
        if !self.logins.contains_key(credential) && self.logins.len() >= CREDENTIAL_KEY_CAP {
            return;
        }
        self.logins.insert(credential.clone(), login.to_string());
    }

    /// `credential`'s login, once resolved.
    pub fn login_for(&self, credential: &CredentialKey) -> Option<String> {
        self.logins.get(credential).cloned()
    }

    /// Remember that `consumer` just spent `credential`. Capped like the other
    /// per-credential maps: a consumer label is a `&'static str` per client,
    /// so the cap only guards a caller that minted labels per request.
    pub fn record_consumer_credential(&mut self, consumer: &str, credential: &CredentialKey) {
        if !self.consumer_credentials.contains_key(consumer)
            && self.consumer_credentials.len() >= CREDENTIAL_KEY_CAP
        {
            return;
        }
        self.consumer_credentials
            .insert(consumer.to_string(), credential.clone());
    }

    /// The credential `consumer` last spent, or `None` (UNKNOWN) before its
    /// first request.
    pub fn credential_for_consumer(&self, consumer: &str) -> Option<CredentialKey> {
        self.consumer_credentials.get(consumer).cloned()
    }

    /// Cumulative CHARGED (non-`304`) calls per consumer, summed across that
    /// consumer's histogram rows. A LOWER BOUND once the histogram has dropped
    /// rows over its key cap (`templates_dropped`), never an over-count.
    pub fn charged_by_consumer(&self) -> BTreeMap<String, u64> {
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        for (key, counts) in &self.rows {
            let slot = out.entry(key.consumer.clone()).or_insert(0);
            *slot = slot.saturating_add(counts.charged);
        }
        out
    }

    /// The `github_user_budget` heartbeat block for the bucket `consumer`
    /// spends — see [`build_user_budget_report`] for when it is `None`.
    pub fn user_budget_report(
        &self,
        consumer: &str,
        meter_started_at_unix: Option<i64>,
        now_unix: i64,
    ) -> Option<UserBudgetReport> {
        let credential = self.credential_for_consumer(consumer)?;
        let reading = self.credentials.get(&credential)?;
        build_user_budget_report(
            &reading.obs,
            reading.observed_at_unix,
            self.logins.get(&credential).map(String::as_str),
            self.charged_by_consumer(),
            meter_started_at_unix,
            now_unix,
        )
    }

    /// Record that a login lookup for `credential` failed at `now_unix`. A
    /// DEFERRED lookup is not a failure — it was never sent — so callers do
    /// not record one.
    pub fn record_login_failure(&mut self, credential: &CredentialKey, now_unix: i64) {
        if !self.login_failures.contains_key(credential)
            && self.login_failures.len() >= CREDENTIAL_KEY_CAP
        {
            return;
        }
        self.login_failures.insert(credential.clone(), now_unix);
    }

    /// Should `credential`'s login be looked up now? No once it is known, and
    /// not within [`LOGIN_RETRY_AFTER_SECS`] of a failed lookup.
    pub fn login_lookup_due(&self, credential: &CredentialKey, now_unix: i64) -> bool {
        if self.logins.contains_key(credential) {
            return false;
        }
        match self.login_failures.get(credential) {
            Some(failed_at) => now_unix - failed_at >= LOGIN_RETRY_AFTER_SECS,
            None => true,
        }
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
        self.totals.deferred = self.totals.deferred.saturating_add(delta.deferred);

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
            row.deferred = row.deferred.saturating_add(delta.deferred);
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
                deferred: counts.deferred,
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
    /// Entries dropped to honour the entry ceiling (not same-key replaces).
    evicted_for_entries: u64,
    /// Entries dropped to honour the byte budget.
    evicted_for_bytes: u64,
}

/// What the ETag cache holds, what it is bounded by, and how often each bound
/// has evicted since process start — served as `etagCache` on
/// `GET /github-budget`.
///
/// The ceilings are served beside the counts because "entries 512 (full)" and
/// "approxBytes near the budget" are only readable against them, and the
/// eviction counters because an LRU that evicts inside one reconciler cycle
/// is exactly the thrash plan
/// `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`
/// D5 measured: a rising `evictedForEntries` between two cycles means the cap
/// binds again, a rising `evictedForBytes` means MB-scale bodies are crowding
/// the cycle out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EtagCacheGauge {
    pub entries: usize,
    /// Approximate by construction — it drives an eviction budget, not an
    /// accounting ledger.
    pub approx_bytes: usize,
    pub max_entries: usize,
    pub max_bytes: usize,
    pub evicted_for_entries: u64,
    pub evicted_for_bytes: u64,
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
            evicted_for_entries: 0,
            evicted_for_bytes: 0,
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
        // `push` returns the displaced pair both when it replaces this key and
        // when it evicts the LRU entry to honour the entry ceiling, so the byte
        // counter stays exact without re-summing the cache on every store.
        if let Some((displaced, old)) = self.entries.push(url.to_string(), entry) {
            self.bytes = self.bytes.saturating_sub(old.heap_bytes());
            if displaced != url {
                self.evicted_for_entries = self.evicted_for_entries.saturating_add(1);
            }
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

    /// The gauge that did not exist when coord's equivalent cache OOMed a
    /// fleet: counts, both ceilings, and per-bound eviction totals.
    pub fn gauge(&self) -> EtagCacheGauge {
        EtagCacheGauge {
            entries: self.entries.len(),
            approx_bytes: self.bytes,
            max_entries: self.entries.cap().get(),
            max_bytes: self.max_bytes,
            evicted_for_entries: self.evicted_for_entries,
            evicted_for_bytes: self.evicted_for_bytes,
        }
    }

    /// Evict least-recently-used entries until the byte budget holds. The entry
    /// ceiling is enforced by [`LruCache::push`] in [`Self::store`], which hands
    /// back what it evicted so the byte counter is already exact here.
    fn evict_to_budget(&mut self) {
        while self.bytes > self.max_bytes && self.entries.len() > 1 {
            match self.entries.pop_lru() {
                Some((_, old)) => {
                    self.bytes = self.bytes.saturating_sub(old.heap_bytes());
                    self.evicted_for_bytes = self.evicted_for_bytes.saturating_add(1);
                }
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

/// When the process-global meter started counting (unix seconds): the origin of
/// every cumulative count it holds. Set once, with the registry.
static METER_STARTED_AT_UNIX: OnceLock<i64> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| {
        METER_STARTED_AT_UNIX.get_or_init(now_unix);
        Mutex::new(Registry::new())
    })
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
///
/// `credential` names the bucket the request was billed to; its reading is
/// kept apart from every other credential's so a gate reads the bucket it is
/// about to spend (see [`CredentialKey`]).
pub fn record_response(
    credential: &CredentialKey,
    consumer: &str,
    url: &str,
    cache_mode: CacheMode,
    status: u16,
    headers: &HeaderMap,
) {
    let obs = parse_rate_limit(headers);
    let retry_after = parse_retry_after(headers);
    let now = now_unix();
    if let Ok(mut reg) = registry().lock() {
        reg.record_reading(credential, &obs, now);
        reg.record_consumer_credential(consumer, credential);
        reg.record_with_retry_after(consumer, url, cache_mode, status, obs, retry_after, now);
    }
}

/// Upper bound, in seconds, of the random delay added past every wait GitHub
/// names on a response — the interactive floor sleep, the single retry sleep,
/// and the background hold a rate refusal sets — before this process sends
/// again. A `ProjectedShort` headroom deferral is not a wait: it is re-judged on
/// each reconciler tick, whose phase already differs per runner.
///
/// Every runner on the fleet shares one user REST bucket, and every one of them
/// computes the same wait from the same `x-ratelimit-reset` (or `Retry-After`).
/// Resuming exactly there makes them all send in the same second the window
/// opens — the synchronized burst recorded in coord finding `e4c384e7`
/// (dossier `github-rate-limit-exhaustion`). Spreading the resume over half a
/// minute costs a waiting caller at most that, and GitHub's wait is a floor, so
/// a later resume is never a protocol violation.
pub(crate) const RESUME_JITTER_SECS: u64 = 30;

/// The wait a caller actually honours after GitHub said `wait_secs`: the same
/// wait, plus this process's own random share of [`RESUME_JITTER_SECS`], so
/// runners sharing a bucket do not all resume at the reset second.
pub(crate) fn resume_after(wait_secs: u64) -> u64 {
    use rand::Rng;
    wait_secs.saturating_add(rand::rng().random_range(0..=RESUME_JITTER_SECS))
}

/// A BACKGROUND request was refused for rate and told to wait `wait_secs`:
/// defer that credential's background spend until then, so the next tick or
/// nudge does not re-send inside the window ([`Registry::record_not_before`]).
pub fn record_rate_limited_refusal(credential: &CredentialKey, wait_secs: u64) {
    if let Ok(mut reg) = registry().lock() {
        let until = now_unix().saturating_add(i64::try_from(wait_secs).unwrap_or(i64::MAX));
        reg.record_not_before(credential, until);
    }
}

/// The heartbeat's `github_user_budget` block: the reading of the credential
/// the session-PR reconciler last spent, with its login, the per-consumer
/// charged counts and the meter's start. `None` before the reconciler's first
/// request, while its login is unresolved, on a stale or unusable reading, or
/// on a poisoned meter — see [`build_user_budget_report`].
pub fn user_budget_report() -> Option<UserBudgetReport> {
    let reg = registry().lock().ok()?;
    reg.user_budget_report(
        USER_BUDGET_REPORT_CONSUMER,
        METER_STARTED_AT_UNIX.get().copied(),
        now_unix(),
    )
}

/// The ONE background gate. `Ok` sends; `Err` is a deferral — the request must
/// not be sent and must not be slept on — and is counted on the meter as
/// `deferred` here, so no caller can defer without it showing.
///
/// Reads only `credential`'s own reading, judged by [`github_headroom`] with
/// the env [`HeadroomKnobs`]. No reading, a stale one, or a poisoned meter is
/// UNKNOWN and sends.
pub fn background_gate(
    credential: &CredentialKey,
    consumer: &str,
    url: &str,
    cache_mode: CacheMode,
) -> Result<(), BudgetDeferral> {
    let knobs = HeadroomKnobs::from_env();
    let Ok(mut reg) = registry().lock() else {
        return Ok(());
    };
    let verdict = reg.background_verdict(credential, now_unix(), knobs);
    if verdict.is_err() {
        reg.record_deferred(consumer, url, cache_mode);
    }
    verdict
}

/// `credential`'s latest `X-RateLimit-*` reading — the input an INTERACTIVE
/// caller's floor backs off against. `None` is UNKNOWN, never "full".
pub fn rate_limit_for(credential: &CredentialKey) -> Option<RateLimitObservation> {
    registry().lock().ok()?.rate_limit_for(credential)
}

/// Remember `credential`'s authenticated login (resolved once per credential
/// per process).
pub fn record_login(credential: &CredentialKey, login: &str) {
    if let Ok(mut reg) = registry().lock() {
        reg.record_login(credential, login);
    }
}

/// `credential`'s login, once resolved; `None` is UNKNOWN.
pub fn login_for(credential: &CredentialKey) -> Option<String> {
    registry().lock().ok()?.login_for(credential)
}

/// Should `credential`'s login be looked up now? See
/// [`Registry::login_lookup_due`]. A poisoned meter answers `false`: no
/// lookup is better than one every tick.
pub fn login_lookup_due(credential: &CredentialKey) -> bool {
    registry()
        .lock()
        .map(|reg| reg.login_lookup_due(credential, now_unix()))
        .unwrap_or(false)
}

/// Record a failed login lookup for `credential`, starting its back-off.
pub fn record_login_failure(credential: &CredentialKey) {
    if let Ok(mut reg) = registry().lock() {
        reg.record_login_failure(credential, now_unix());
    }
}

/// Seed ONE credential's reading without touching the snapshot's global
/// reading — so a test can put a bucket under the headroom line without racing
/// tests that assert on the process-global snapshot.
/// The background gate's verdict for `credential` right now, WITHOUT metering
/// a deferral — so a test can wait for a hold to become observable.
#[cfg(test)]
pub(crate) fn background_verdict_for_test(
    credential: &CredentialKey,
) -> Result<(), BudgetDeferral> {
    match registry().lock() {
        Ok(reg) => reg.background_verdict(credential, now_unix(), HeadroomKnobs::from_env()),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
pub(crate) fn seed_reading_for_test(credential: &CredentialKey, obs: &RateLimitObservation) {
    if let Ok(mut reg) = registry().lock() {
        reg.record_reading(credential, obs, now_unix());
    }
}

/// Serializes the tests that write or assert the process-global `last`
/// reading, so one test's deliberately exhausted global reading cannot leak
/// into another's assertions.
///
/// A CHILD of `env_lock` in the test-lock hierarchy: its holders read env
/// knobs (`low_fraction`, `HeadroomKnobs::from_env`) that other tests write
/// under `env_lock`, and it is held from three files, so `hierarchy_lock`
/// takes the env lock first and either call order nests.
#[cfg(test)]
pub(crate) fn global_last_lock() -> crate::test_env::TestLockGuard {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    crate::test_env::hierarchy_lock(&LOCK)
}

/// Set the process-global `last` reading (the snapshot's slot) to `obs` as if
/// `credential` had answered with it — the shape a regression to reading the
/// shared slot would act on. Hold [`global_last_lock`] around it.
#[cfg(test)]
pub(crate) fn seed_global_last_for_test(credential: &CredentialKey, obs: &RateLimitObservation) {
    if let Ok(mut reg) = registry().lock() {
        let now = now_unix();
        reg.record_reading(credential, obs, now);
        reg.last = Some(obs.clone());
        reg.observed_at_unix = Some(now);
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

/// The process-global ETag cache's gauge, or `None` when its mutex is
/// poisoned — UNKNOWN, which must not be served as an empty cache.
pub fn etag_cache_stats() -> Option<EtagCacheGauge> {
    gauge_of(etag_cache())
}

fn gauge_of(cache: &Mutex<EtagCache>) -> Option<EtagCacheGauge> {
    cache.lock().ok().map(|cache| cache.gauge())
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
        // The env is process-global, so hold the ONE shared lock every
        // env-touching test in this binary takes — owning the variable
        // excludes nobody on its own. The restore is declared AFTER the lock so
        // it runs while the lock is still held, and it survives the panic path,
        // which the manual restore at the end of this body did not.
        // Plan `2026-08-25-runner-test-suite-env-isolation`.
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[LOW_FRACTION_ENV]);

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

        // `_restore` drops here, including on the panic path above.
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
        let EtagCacheGauge {
            entries,
            approx_bytes: bytes,
            ..
        } = cache.gauge();
        assert_eq!(entries, 1);
        assert_eq!(bytes, 2 + "W/\"def\"".len());
    }

    #[test]
    fn etag_store_ignores_an_empty_etag() {
        let mut cache = EtagCache::new(8, 1024);
        cache.store("/u", "   ", Bytes::from_static(b"body"));
        assert_eq!(cache.gauge().entries, 0, "no ETag means nothing to replay");
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

        assert_eq!(cache.gauge().entries, 2);
        assert!(cache.replay("/b").is_none(), "LRU entry evicted");
        assert!(cache.replay("/a").is_some());
        assert!(cache.replay("/c").is_some());
    }

    /// The byte counter is maintained incrementally, so an entry the LRU evicts
    /// to honour the entry ceiling — and a same-key replacement — must leave it
    /// exact, not inflated by the departed entry.
    #[test]
    fn etag_cache_byte_counter_stays_exact_across_cap_eviction_and_replace() {
        let mut cache = EtagCache::new(2, 1024 * 1024);
        cache.store("/a", "1", Bytes::from_static(b"aaaa")); // 5 bytes
        cache.store("/b", "2", Bytes::from_static(b"bb")); // 3 bytes
        cache.store("/c", "3", Bytes::from_static(b"c")); // evicts /a; 2 bytes
        assert_eq!(
            (cache.gauge().entries, cache.gauge().approx_bytes),
            (2, 5),
            "evicted /a's bytes subtracted"
        );
        cache.store("/b", "22", Bytes::from_static(b"bbbbbb")); // replace: 8 bytes
        assert_eq!(
            (cache.gauge().entries, cache.gauge().approx_bytes),
            (2, 10),
            "replaced /b's old bytes subtracted"
        );
        let g = cache.gauge();
        assert_eq!(g.evicted_for_entries, 1, "the replace is not an eviction");
        assert_eq!(g.evicted_for_bytes, 0);
        assert_eq!((g.max_entries, g.max_bytes), (2, 1024 * 1024));
    }

    #[test]
    fn etag_cache_gauge_counts_byte_budget_evictions_separately() {
        let mut cache = EtagCache::new(64, 300);
        for i in 0..6 {
            cache.store(&format!("/big/{i}"), "e", Bytes::from(vec![0u8; 100]));
        }
        let g = cache.gauge();
        assert_eq!(g.evicted_for_entries, 0, "the entry ceiling never bound");
        assert_eq!(g.evicted_for_bytes as usize, 6 - g.entries);
        assert!(g.evicted_for_bytes > 0);
    }

    /// A poisoned cache lock is UNKNOWN and must be served as `null`, never as
    /// an empty cache (which is what the `(0, 0)` fallback used to claim).
    #[test]
    fn etag_cache_gauge_is_none_when_the_lock_is_poisoned() {
        let cache = Mutex::new(EtagCache::new(8, 1024));
        assert!(gauge_of(&cache).is_some());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache.lock().expect("fresh lock");
            panic!("poison the cache lock");
        }));
        assert!(cache.is_poisoned());
        assert_eq!(gauge_of(&cache), None);
    }

    #[test]
    fn etag_cache_default_gauge_serves_the_production_ceilings() {
        let g = EtagCache::default().gauge();
        assert_eq!(g.max_entries, ETAG_CACHE_MAX_ENTRIES);
        assert_eq!(g.max_bytes, ETAG_CACHE_MAX_BYTES);
    }

    #[test]
    fn etag_cache_evicts_on_the_byte_budget() {
        // Entry ceiling is generous; the byte budget is what must bind — a
        // handful of MB-scale `/compare/{a}...{b}` bodies is not "small".
        let mut cache = EtagCache::new(64, 300);
        for i in 0..6 {
            cache.store(&format!("/big/{i}"), "e", Bytes::from(vec![0u8; 100]));
        }
        let EtagCacheGauge {
            entries,
            approx_bytes: bytes,
            ..
        } = cache.gauge();
        assert!(bytes <= 300, "byte budget held, got {bytes}");
        assert!(entries < 6, "eviction happened, {entries} entries left");
        assert!(cache.replay("/big/5").is_some(), "newest survives");
        assert!(cache.replay("/big/0").is_none(), "oldest evicted");
    }

    /// The default cache must hold a whole reconciler cycle: one tick visits
    /// every URL once, in the same order, and the next tick revisits them. A cap
    /// below the cycle length evicts each entry before it is reached again, so
    /// every second-pass lookup misses (the measured 512-entry thrash).
    #[test]
    fn etag_cache_default_holds_a_full_reconcile_cycle() {
        // Comfortably above one ~670-URL cycle, and far above the old 512 cap.
        const URLS: usize = 1_000;
        const BODY_BYTES: usize = 12_800; // the measured mean entry size
        let mut cache = EtagCache::default();
        let url = |i: usize| format!("/repos/o/r{i}/pulls?state=all&head=o:b{i}");
        for i in 0..URLS {
            cache.store(&url(i), "\"e\"", Bytes::from(vec![0u8; BODY_BYTES]));
        }
        for i in 0..URLS {
            assert!(
                cache.replay(&url(i)).is_some(),
                "second-pass replay missed at entry {i} of {URLS}: the cache does not hold one cycle"
            );
        }
        assert_eq!(cache.gauge().entries, URLS);
    }

    #[test]
    fn etag_cache_invalidate_drops_the_entry_and_its_bytes() {
        let mut cache = EtagCache::new(8, 1024);
        cache.store("/u", "e", Bytes::from_static(b"body"));
        cache.invalidate("/u");
        assert_eq!((cache.gauge().entries, cache.gauge().approx_bytes), (0, 0));
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

    // -- Background headroom (plan 2026-10-30-…, Phase 3, D4) ---------------

    fn knobs(min_reserve_fraction: f64, guard_fraction: f64) -> HeadroomKnobs {
        HeadroomKnobs {
            min_reserve_fraction,
            guard_fraction,
        }
    }

    /// Mirrors coord's `github_headroom_holds_exactly_down_to_the_projected_margin`.
    #[test]
    fn github_headroom_holds_exactly_down_to_the_projected_margin() {
        // 1800 s to reset ⇒ 1800 s elapsed. limit 3600, remaining 2700 ⇒ used
        // 900 at exactly 0.5/s ⇒ projected 2700 − 900 = 1800. Margin with a
        // reserve of 0.375 (1350) and guard 0.25: 1350 + 0.25 × 3600 × 0.5 = 1800.
        let now = 1_000_000;
        let reset = Some(now + 1800);
        let k = knobs(0.375, 0.25);
        let at = |remaining| github_headroom(Some(remaining), Some(3600), reset, 0, now, k);
        assert_eq!(at(2700), Headroom::Holds, "projected == margin holds");
        // One point less: used 901 ⇒ projected ≈ 1798 < 1800.
        assert_eq!(at(2699), Headroom::Short, "just under the margin");
        // A smaller reserve gives the same state room to spare.
        assert_eq!(
            github_headroom(Some(2699), Some(3600), reset, 0, now, knobs(0.02, 0.25)),
            Headroom::Holds
        );
    }

    #[test]
    fn github_headroom_clamps_elapsed_early_in_a_window() {
        // 10 s after a reset (3590 s to go): elapsed clamps to 60 s, not 10.
        // limit 5000, remaining 4940 ⇒ used 60 ⇒ 1/s ⇒ projected 4940 − 3590 =
        // 1350; margin 0.20 × 5000 + 0.10 × 5000 × 3590/3600 ≈ 1498.6 ⇒ Short,
        // so use a smaller reserve to isolate the clamp:
        // 0.05 × 5000 + 498.6 ≈ 748.6 ⇒ Holds.
        let now = 1_000_000;
        let reset = Some(now + 3590);
        assert_eq!(
            github_headroom(Some(4940), Some(5000), reset, 0, now, knobs(0.05, 0.10)),
            Headroom::Holds,
            "clamped: 1/s projects headroom (unclamped 6/s would project < 0)"
        );
        // Double the early burn: 120 used ⇒ 2/s ⇒ projected 4880 − 7180 < 0.
        assert_eq!(
            github_headroom(Some(4880), Some(5000), reset, 0, now, knobs(0.05, 0.10)),
            Headroom::Short
        );
    }

    #[test]
    fn github_headroom_refuses_a_burn_projected_to_exhaust_the_window() {
        // Half the window gone, half the limit used ⇒ the burn alone exhausts
        // the window exactly at reset: projected 0 < the margin.
        let now = 1_000_000;
        let reset = Some(now + 1800);
        assert_eq!(
            github_headroom(
                Some(2500),
                Some(5000),
                reset,
                0,
                now,
                HeadroomKnobs::default()
            ),
            Headroom::Short
        );
        // A lighter burn: 1000 used over half the window ⇒ projected 3000 at
        // reset; margin 0.20 × 5000 + 0.10 × 5000 × 0.5 = 1250 ⇒ Holds.
        assert_eq!(
            github_headroom(
                Some(4000),
                Some(5000),
                reset,
                0,
                now,
                HeadroomKnobs::default()
            ),
            Headroom::Holds
        );
    }

    #[test]
    fn github_headroom_is_unknown_on_no_usable_fresh_current_window_reading() {
        let now = 1_000_000;
        let k = HeadroomKnobs::default();
        // Each case would be Short if judged (1% remaining, half the window).
        let cases = [
            (None, Some(5000), Some(now + 1800), 0, "unknown remaining"),
            (Some(50), None, Some(now + 1800), 0, "unknown limit"),
            (Some(50), Some(0), Some(now + 1800), 0, "a zero limit"),
            (Some(50), Some(-1), Some(now + 1800), 0, "a negative limit"),
            (Some(50), Some(5000), None, 0, "unknown reset"),
            (
                Some(50),
                Some(5000),
                Some(now),
                0,
                "a window that resets now",
            ),
            (
                Some(50),
                Some(5000),
                Some(now - 5),
                0,
                "a window that has reset",
            ),
            (
                Some(50),
                Some(5000),
                Some(now + 1800),
                61,
                "a reading older than 60 s",
            ),
            (
                Some(50),
                Some(5000),
                Some(now + 1800),
                -1,
                "a reading from the future",
            ),
        ];
        for (remaining, limit, reset, age, why) in cases {
            assert_eq!(
                github_headroom(remaining, limit, reset, age, now, k),
                Headroom::Unknown,
                "{why}"
            );
        }
        assert_eq!(
            github_headroom(Some(50), Some(5000), Some(now + 1800), 60, now, k),
            Headroom::Short,
            "exactly at the age bound still judges"
        );
    }

    fn reading(limit: i64, remaining: i64, reset_at_unix: i64) -> RateLimitObservation {
        RateLimitObservation {
            limit: Some(limit),
            remaining: Some(remaining),
            used: Some(limit - remaining),
            reset_at_unix: Some(reset_at_unix),
            resource: Some("core".to_string()),
        }
    }

    #[test]
    fn background_verdict_defers_only_on_the_credentials_own_short_reading() {
        let now = 1_000_000;
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        let b = CredentialKey::of_token("token-b");
        let k = HeadroomKnobs::default();

        // No reading at all is UNKNOWN: sends.
        assert_eq!(reg.background_verdict(&a, now, k), Ok(()));

        // A's bucket is exhausted. A defers; B — with no reading — still sends.
        reg.record_reading(&a, &reading(5000, 0, now + 1800), now);
        let d = reg.background_verdict(&a, now, k).expect_err("A is short");
        assert_eq!(d.credential, a);
        assert!(
            matches!(
                d.reason,
                DeferralReason::ProjectedShort { remaining: 0, .. }
            ),
            "{d}"
        );
        assert_eq!(reg.background_verdict(&b, now, k), Ok(()));

        // B's own healthy reading still sends, A's exhaustion notwithstanding.
        reg.record_reading(&b, &reading(5000, 4900, now + 1800), now);
        assert_eq!(reg.background_verdict(&b, now, k), Ok(()));
        assert!(reg.background_verdict(&a, now, k).is_err());

        // A's reading ages past 60 s ⇒ UNKNOWN ⇒ sends (the re-probe).
        assert_eq!(reg.background_verdict(&a, now + 61, k), Ok(()));
    }

    #[test]
    fn a_headerless_response_keeps_the_credentials_reading() {
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        reg.record_reading(&a, &reading(5000, 7, 2_000), 10);
        reg.record_reading(&a, &RateLimitObservation::default(), 11);
        assert_eq!(reg.rate_limit_for(&a).and_then(|o| o.remaining), Some(7));
    }

    #[test]
    fn credential_key_is_a_12_hex_digest_never_the_token() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let key = CredentialKey::of_token(token);
        assert_eq!(key.to_string().len(), 12);
        assert!(key.to_string().bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!token.contains(&key.to_string()));
        assert_eq!(key, CredentialKey::of_token(token), "stable");
        assert_ne!(key, CredentialKey::of_token("ghp_other"));
        assert_eq!(
            CredentialKey::of_optional_token(None),
            CredentialKey::anonymous()
        );
    }

    #[test]
    fn deferred_is_counted_apart_from_every_sent_outcome() {
        let mut reg = Registry::new();
        reg.record_deferred(
            "session_pr_reconciler",
            "/repos/o/r/pulls/1",
            CacheMode::Cached,
        );
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 1);
        assert_eq!(snap.totals.deferred, 1);
        assert_eq!(snap.totals.sent(), 0, "a deferred request was never sent");
        assert_eq!(snap.totals.transport_error, 0);
        assert_eq!(snap.consumers[0].deferred, 1);
        let v = serde_json::to_value(&snap).expect("serializes");
        assert_eq!(v["totals"]["deferred"], 1);
    }

    /// A rate refusal's `Retry-After` holds the credential's background spend
    /// until it passes — even with a healthy reading — and only that
    /// credential's.
    #[test]
    fn a_retry_after_defers_that_credentials_background_spend_until_it_passes() {
        let now = 1_000_000;
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        let b = CredentialKey::of_token("token-b");
        let k = HeadroomKnobs::default();
        reg.record_reading(&a, &reading(5000, 4900, now + 1800), now);

        reg.record_not_before(&a, now + 60);
        let d = reg
            .background_verdict(&a, now, k)
            .expect_err("inside Retry-After");
        assert_eq!(
            d.reason,
            DeferralReason::RetryAfter {
                not_before_unix: now + 60
            }
        );
        assert!(reg.background_verdict(&a, now + 59, k).is_err());
        assert_eq!(reg.background_verdict(&b, now, k), Ok(()), "per credential");
        // A shorter later answer never shortens the hold.
        reg.record_not_before(&a, now + 10);
        assert!(reg.background_verdict(&a, now + 30, k).is_err());
        // Past it, the (fresh, healthy) reading decides again.
        assert_eq!(reg.background_verdict(&a, now + 60, k), Ok(()));
    }

    /// Secondary-limit refusals are metered as `rate_limited`, not `charged`;
    /// a permission `403` (neither signal) is charged.
    #[test]
    fn secondary_limit_refusals_are_rate_limited_not_charged() {
        let mut reg = Registry::new();
        let url = "/repos/o/r/pulls/1";
        let budget_left = obs(5000, 4000);
        reg.record_with_retry_after(
            "c",
            url,
            CacheMode::Cached,
            429,
            budget_left.clone(),
            None,
            1,
        );
        reg.record_with_retry_after(
            "c",
            url,
            CacheMode::Cached,
            403,
            budget_left.clone(),
            Some(60),
            2,
        );
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 3);
        assert_eq!(snap.totals.rate_limited, 2);
        assert_eq!(snap.totals.charged, 0);

        reg.record_with_retry_after("c", url, CacheMode::Cached, 403, budget_left, None, 4);
        let snap = reg.snapshot(TOP_CONSUMERS_N, 0.20, 5);
        assert_eq!(snap.totals.charged, 1, "a permission 403 is charged");
        assert_eq!(snap.totals.rate_limited, 2);
    }

    #[test]
    fn the_rate_limit_classifier_separates_rate_from_permission() {
        assert!(is_rate_limited_refusal(429, None, None));
        assert!(is_rate_limited_refusal(429, None, Some(4000)));
        assert!(is_rate_limited_refusal(403, None, Some(0)));
        assert!(is_rate_limited_refusal(403, Some(30), Some(4000)));
        assert!(!is_rate_limited_refusal(403, None, Some(4000)));
        assert!(!is_rate_limited_refusal(403, None, None));
        assert!(!is_rate_limited_refusal(404, Some(30), Some(0)));
    }

    #[test]
    fn the_guard_knob_accepts_the_closed_unit_interval_like_coord() {
        assert_eq!(parse_guard_fraction(Some("0".into()), 0.10), 0.0);
        assert_eq!(parse_guard_fraction(Some("1".into()), 0.10), 1.0);
        assert_eq!(parse_guard_fraction(Some(" 0.25 ".into()), 0.10), 0.25);
        for bad in ["-0.1", "1.01", "NaN", "inf", "x", ""] {
            assert_eq!(
                parse_guard_fraction(Some(bad.into()), 0.10),
                0.10,
                "{bad:?}"
            );
        }
        assert_eq!(parse_guard_fraction(None, 0.10), 0.10);
    }

    /// A credential that cannot answer `GET /user` is not re-asked every tick:
    /// a failure backs off for [`LOGIN_RETRY_AFTER_SECS`], and a known login is
    /// never looked up again.
    #[test]
    fn a_failed_login_lookup_backs_off_and_a_known_login_is_never_re_asked() {
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        let now = 1_000_000;
        assert!(reg.login_lookup_due(&a, now), "never asked: due");

        reg.record_login_failure(&a, now);
        assert!(
            !reg.login_lookup_due(&a, now + 30),
            "next tick: not re-asked"
        );
        assert!(!reg.login_lookup_due(&a, now + LOGIN_RETRY_AFTER_SECS - 1));
        assert!(reg.login_lookup_due(&a, now + LOGIN_RETRY_AFTER_SECS));

        let b = CredentialKey::of_token("token-b");
        assert!(reg.login_lookup_due(&b, now + 30), "per credential");

        reg.record_login(&a, "jspinak");
        assert!(!reg.login_lookup_due(&a, now + 10 * LOGIN_RETRY_AFTER_SECS));
    }

    /// The verdict reads the credential's OWN reading, never the shared slot:
    /// with the global `last` exhausted by another credential, B still sends.
    #[test]
    fn the_shared_last_reading_never_defers_another_credential() {
        let now = 1_000_000;
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        let b = CredentialKey::of_token("token-b");
        let empty = reading(5000, 0, now + 1800);
        reg.record_reading(&a, &empty, now);
        reg.record(
            "c",
            "/repos/o/r/pulls/1",
            CacheMode::Cached,
            200,
            empty,
            now,
        );
        assert_eq!(
            reg.snapshot(1, 0.2, now).remaining,
            Some(0),
            "shared slot exhausted"
        );
        assert_eq!(
            reg.background_verdict(&b, now, HeadroomKnobs::default()),
            Ok(())
        );
    }

    #[test]
    fn fraction_knobs_parse_like_the_low_fraction() {
        assert_eq!(parse_fraction(None, 0.2), 0.2);
        assert_eq!(parse_fraction(Some(" 0.35 ".into()), 0.2), 0.35);
        for bad in ["0", "1", "-0.1", "1.5", "NaN", "inf", "x", ""] {
            assert_eq!(parse_fraction(Some(bad.into()), 0.2), 0.2, "{bad:?}");
        }
        assert_eq!(HeadroomKnobs::default().min_reserve_fraction, 0.20);
        assert_eq!(HeadroomKnobs::default().guard_fraction, 0.10);
    }

    #[test]
    fn login_is_kept_per_credential() {
        let mut reg = Registry::new();
        let a = CredentialKey::of_token("token-a");
        assert_eq!(reg.login_for(&a), None);
        reg.record_login(&a, " jspinak ");
        assert_eq!(reg.login_for(&a).as_deref(), Some("jspinak"));
        assert_eq!(reg.login_for(&CredentialKey::of_token("token-b")), None);
    }

    // -- Heartbeat user-bucket report (plan 2026-10-30-…, Phase 4) ----------

    fn consumers(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn user_budget_report_serializes_coords_exact_snake_case_shape() {
        let now = 1_763_559_000;
        let report = build_user_budget_report(
            &RateLimitObservation {
                limit: Some(5000),
                remaining: Some(4100),
                used: Some(900),
                reset_at_unix: Some(1_763_560_000),
                resource: Some("core".to_string()),
            },
            now - 5,
            Some("jspinak"),
            consumers(&[("session_pr_reconciler", 4350)]),
            Some(1_763_500_000),
            now,
        )
        .expect("a fresh, complete reading with a login is reported");
        let v = serde_json::to_value(&report).expect("serializes");
        assert_eq!(
            v,
            serde_json::json!({
                "login": "jspinak",
                "limit": 5000,
                "remaining": 4100,
                "used": 900,
                "reset_at_unix": 1_763_560_000_i64,
                "resource": "core",
                "charged_by_consumer": {"session_pr_reconciler": 4350},
                "process_started_at_unix": 1_763_500_000_i64,
            }),
            "the EXACT keys coord's GithubUserBudgetReport reads — never camelCase"
        );
    }

    #[test]
    fn user_budget_report_sends_only_consumer_labels_coord_keeps() {
        let now = 1_000_000;
        let good = reading(5000, 4000, now + 3600 + 30);
        let mut all = consumers(&[
            ("session_pr_reconciler", 10),
            ("ci_node:sibling.probe-1", 3),
            ("Upper", 99),
            ("has space", 99),
            ("", 99),
        ]);
        all.insert("x".repeat(65), 99);
        let r = build_user_budget_report(&good, now, Some("jspinak"), all, None, now)
            .expect("a reset exactly an hour + 30 s ahead is still reported");
        assert_eq!(
            r.charged_by_consumer,
            consumers(&[
                ("session_pr_reconciler", 10),
                ("ci_node:sibling.probe-1", 3)
            ])
        );

        // Over 64 valid labels: the 64 busiest are kept.
        let many: BTreeMap<String, u64> = (0..70_u64).map(|i| (format!("c{i:02}"), i)).collect();
        let r = build_user_budget_report(&good, now, Some("jspinak"), many, None, now)
            .expect("reported");
        assert_eq!(r.charged_by_consumer.len(), 64);
        assert!(r.charged_by_consumer.contains_key("c69"));
        assert!(
            !r.charged_by_consumer.contains_key("c05"),
            "the quietest are dropped"
        );
        assert!(r.charged_by_consumer.contains_key("c06"));
    }

    #[test]
    fn user_budget_report_omits_optional_keys_it_does_not_have() {
        let now = 1_000_000;
        let mut obs = reading(5000, 4000, now + 600);
        obs.used = None;
        let report =
            build_user_budget_report(&obs, now, Some("jspinak"), BTreeMap::new(), None, now)
                .expect("reported");
        let v = serde_json::to_value(&report).expect("serializes");
        let obj = v.as_object().expect("object");
        assert!(!obj.contains_key("used"));
        assert!(!obj.contains_key("process_started_at_unix"));
        assert_eq!(v["charged_by_consumer"], serde_json::json!({}));
    }

    #[test]
    fn user_budget_report_is_absent_when_nothing_honest_can_be_sent() {
        let now = 1_000_000;
        let good = reading(5000, 4000, now + 600);
        let build = |obs: &RateLimitObservation, observed_at: i64, login: Option<&str>| {
            build_user_budget_report(obs, observed_at, login, BTreeMap::new(), Some(1), now)
        };
        assert!(build(&good, now, Some("jspinak")).is_some(), "baseline");

        assert_eq!(build(&good, now, None), None, "no login is never a guess");
        assert_eq!(build(&good, now, Some("  ")), None, "a blank login");

        let with = |f: &dyn Fn(&mut RateLimitObservation)| {
            let mut o = good.clone();
            f(&mut o);
            o
        };
        let cases: Vec<(RateLimitObservation, &str)> = vec![
            (with(&|o| o.limit = None), "unknown limit"),
            (with(&|o| o.limit = Some(0)), "zero limit"),
            (
                with(&|o| o.limit = Some(1_000_001)),
                "limit over coord's ceiling",
            ),
            (with(&|o| o.remaining = None), "unknown remaining"),
            (with(&|o| o.remaining = Some(-1)), "negative remaining"),
            (with(&|o| o.remaining = Some(5001)), "remaining over limit"),
            (with(&|o| o.used = Some(5001)), "used over limit"),
            (with(&|o| o.used = Some(-1)), "negative used"),
            (with(&|o| o.reset_at_unix = None), "unknown reset"),
            (with(&|o| o.reset_at_unix = Some(now)), "window resets now"),
            (
                with(&|o| o.reset_at_unix = Some(now - 1)),
                "window has reset",
            ),
            (
                with(&|o| o.reset_at_unix = Some(now + 3600 + 31)),
                "reset implausibly far ahead",
            ),
            (with(&|o| o.resource = None), "unknown resource"),
            (
                with(&|o| o.resource = Some("Core".into())),
                "uppercase resource",
            ),
            (
                with(&|o| o.resource = Some("a".repeat(33))),
                "33-byte resource",
            ),
        ];
        for (obs, why) in cases {
            assert_eq!(build(&obs, now, Some("jspinak")), None, "{why}");
        }

        assert!(
            build(&good, now - 120, Some("jspinak")).is_some(),
            "120 s old"
        );
        assert_eq!(build(&good, now - 121, Some("jspinak")), None, "121 s old");
        assert_eq!(
            build(&good, now + 1, Some("jspinak")),
            None,
            "from the future"
        );
    }

    #[test]
    fn registry_reports_the_reconcilers_own_credential_with_summed_consumers() {
        let mut reg = Registry::new();
        let now = now_unix();
        let rec = CredentialKey::of_token("reconciler-token");
        let other = CredentialKey::of_token("ticket-token");
        let rl = |remaining: i64| {
            let mut h = HeaderMap::new();
            for (k, v) in [
                ("x-ratelimit-limit", "5000".to_string()),
                ("x-ratelimit-remaining", remaining.to_string()),
                ("x-ratelimit-used", (5000 - remaining).to_string()),
                ("x-ratelimit-reset", (now + 900).to_string()),
                ("x-ratelimit-resource", "core".to_string()),
            ] {
                h.insert(
                    HeaderName::from_static(k),
                    HeaderValue::from_str(&v).expect("header"),
                );
            }
            parse_rate_limit(&h)
        };
        let spend = |reg: &mut Registry,
                     cred: &CredentialKey,
                     consumer: &str,
                     url: &str,
                     status: u16,
                     remaining: i64| {
            let obs = rl(remaining);
            reg.record_reading(cred, &obs, now);
            reg.record_consumer_credential(consumer, cred);
            reg.record(consumer, url, CacheMode::Cached, status, obs, now);
        };

        // Before the reconciler has spent anything: UNKNOWN, omitted.
        assert_eq!(
            reg.user_budget_report(USER_BUDGET_REPORT_CONSUMER, Some(1), now),
            None
        );

        // Two templates on the reconciler (charged 2 + 1, one 304), one on a
        // ticket consumer on ANOTHER credential, which answered last.
        spend(
            &mut reg,
            &rec,
            USER_BUDGET_REPORT_CONSUMER,
            "https://api.github.com/repos/o/r/pulls/1",
            200,
            4200,
        );
        spend(
            &mut reg,
            &rec,
            USER_BUDGET_REPORT_CONSUMER,
            "https://api.github.com/repos/o/r/pulls/2",
            200,
            4199,
        );
        spend(
            &mut reg,
            &rec,
            USER_BUDGET_REPORT_CONSUMER,
            "https://api.github.com/repos/o/r/pulls/2",
            304,
            4199,
        );
        spend(
            &mut reg,
            &rec,
            USER_BUDGET_REPORT_CONSUMER,
            "https://api.github.com/user",
            200,
            4198,
        );
        spend(
            &mut reg,
            &other,
            "ticket_sync",
            "https://api.github.com/repos/o/r/issues",
            200,
            12,
        );

        // Login unresolved: omitted, never guessed.
        assert_eq!(
            reg.user_budget_report(USER_BUDGET_REPORT_CONSUMER, Some(1), now),
            None
        );

        reg.record_login(&rec, "jspinak");
        reg.record_login(&other, "someone-else");
        let r = reg
            .user_budget_report(USER_BUDGET_REPORT_CONSUMER, Some(1_700_000_000), now)
            .expect("reported");
        assert_eq!(
            r.login, "jspinak",
            "the reconciler's credential, not the last to answer"
        );
        assert_eq!(
            r.remaining, 4198,
            "the reconciler's credential's own latest reading"
        );
        assert_eq!(r.limit, 5000);
        assert_eq!(r.used, Some(802));
        assert_eq!(r.reset_at_unix, now + 900);
        assert_eq!(r.resource, "core");
        assert_eq!(r.process_started_at_unix, Some(1_700_000_000));
        assert_eq!(
            r.charged_by_consumer,
            consumers(&[(USER_BUDGET_REPORT_CONSUMER, 3), ("ticket_sync", 1)]),
            "charged summed per consumer across its templates; a 304 is not charged"
        );
    }

    // -- Global wrappers ----------------------------------------------------

    #[test]
    fn global_wrappers_do_not_panic_and_report_honestly() {
        // The process-global registry is shared with any other test that touches
        // it, so this asserts only shape-and-liveness invariants, never counts.
        // The tests that seed an exhausted global reading hold the same lock.
        let _global = global_last_lock();
        record_identity("QONTINUI_RUNNER_GITHUB_TOKEN", None);
        record_response(
            &CredentialKey::of_token("global-wrappers-probe-token"),
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
        assert!(etag_cache_stats().expect("cache lock").entries >= 1);
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

    // -- Resume jitter --------------------------------------------------------

    #[test]
    fn resume_after_never_shortens_the_wait_and_saturates() {
        assert_eq!(resume_after(u64::MAX), u64::MAX, "no overflow at the top");
        for wait in [0u64, 1, 300] {
            for _ in 0..50 {
                let got = resume_after(wait);
                assert!(
                    (wait..=wait + RESUME_JITTER_SECS).contains(&got),
                    "wait {wait}: {got} is outside [wait, wait + jitter]"
                );
            }
        }
    }

    #[test]
    fn resume_after_stays_in_bounds_and_actually_spreads() {
        let samples: Vec<u64> = (0..200).map(|_| resume_after(60)).collect();
        assert!(samples
            .iter()
            .all(|&s| (60..=60 + RESUME_JITTER_SECS).contains(&s)));
        let distinct: std::collections::HashSet<_> = samples.iter().collect();
        assert!(
            distinct.len() > 1,
            "200 draws all landed on one second — the resume is not jittered"
        );
    }
}
