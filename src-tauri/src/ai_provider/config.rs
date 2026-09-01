use crate::settings::{self, AccountSelectionMode};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Cached resolved config dir for least-usage account selection.
/// Set by `set_resolved_config_dir` at startup or when usage is checked.
static RESOLVED_CONFIG_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Per-account cooldown tracking. When an account hits a rate limit, it is
/// marked with `(when_marked, cooldown_duration)`. Each account carries its
/// own duration so that API providers can honour the precise `Retry-After`
/// returned by the server (e.g. 600s for a daily-quota 429), while CLI
/// subprocess paths fall back to the default [`RATE_LIMIT_COOLDOWN_SECS`].
static ACCOUNT_COOLDOWNS: Mutex<Option<HashMap<String, (Instant, Duration)>>> = Mutex::new(None);

/// Default cooldown for rate-limited accounts when no precise `Retry-After`
/// is available (e.g. the Claude CLI subprocess prints a "rate limit" message
/// on stdout without a header). Five minutes.
const RATE_LIMIT_COOLDOWN_SECS: u64 = 300;

/// One account's weekly-usage sample, captured by the usage probe
/// (`commands::ai_settings::probe_account_usage`) and ranked by
/// [`usage_rank`].
///
/// `usage_delta` is the account's actual 7-day utilization minus its
/// *expected* linear utilization at this point in the billing window
/// (negative = under projected pace); `None` when the probe couldn't compute
/// it (e.g. the 7d-reset header was absent). Its **sign** is what selects the
/// pace tier — it is no longer a ranking key in its own right. `expected` is
/// the key the under-pace tier actually ranks on, and the denominator of the
/// over-pace ratio. `utilization` is the raw 0.0–1.0 weekly fraction: the
/// ranking key for the `Unknown` tier, and the numerator of the over-pace
/// ratio.
///
/// Ranking runs on **use-it-or-lose-it**: unused weekly capacity expires at
/// the reset and does not roll over, so among accounts under their pace the
/// one whose window is furthest along (highest `expected`) is the one to
/// burn. Mirrored in the frontend by `compareByUsageHeadroom`
/// (`src/components/settings/types.ts`).
///
/// `exhausted` marks an account that **won't serve a request right now** — at
/// or over its weekly cap, server-reported rejected, or the probe call itself
/// failed (the probe hits the same per-account quota the CLI uses, so this
/// also catches a spend-limited account whose weekly token utilization still
/// looks low). Exhausted accounts are deprioritized in selection regardless of
/// how favourable their pace key is: a fully-used account whose window is
/// nearly over still has no tokens left to burn, so a less-favourable but
/// *usable* account must win. Computed at probe time by
/// `commands::ai_settings::record_usage_snapshot`.
#[derive(Clone, Copy, Debug)]
struct UsageSample {
    captured_at: Instant,
    usage_delta: Option<f64>,
    utilization: f64,
    /// Expected utilization at probe time: the **linear elapsed fraction of
    /// the account's 7-day window** (0.0–1.0), NOT a budget or an allowance.
    /// `1.0` means the window is about to reset; `0.0` means it just did.
    /// Computed by `commands::ai_settings::compute_expected_usage`, which
    /// returns `None` when `resets_at` is missing or already past (the Haiku
    /// header-probe fallback), so this is `None` for accounts with no usable
    /// pace signal.
    expected: Option<f64>,
    exhausted: bool,
}

/// Latest weekly-usage snapshot per account, keyed by config dir. Populated
/// off the selection hot path by the usage-probe callers (the periodic
/// startup refresh, the Settings/Terminal `check_accounts_usage` command, and
/// the `/analytics/account-usage` route) so [`usage_rank`] can rank
/// accounts without making its own HTTP calls. The probe endpoint
/// self-rate-limits, so the hot path must never probe inline — it reads this
/// cache instead.
static USAGE_SNAPSHOT: Mutex<Option<HashMap<String, UsageSample>>> = Mutex::new(None);

/// Maximum age of a usage sample before [`usage_rank`] treats it as stale and
/// returns `None` (so selection falls back to cooldown ordering). Fifteen
/// minutes — comfortably longer than the startup refresh cadence.
const USAGE_SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);

/// Maximum age at which a sample's `exhausted` flag is still trusted by
/// [`account_known_exhausted`]. Longer than [`USAGE_SNAPSHOT_TTL`] because
/// exhaustion is a slow-changing signal — a weekly cap doesn't clear in
/// minutes — so the cold-start/stale fallback can still avoid an account we
/// saw maxed out a little while ago, without acting on hours-old data.
const EXHAUSTION_STALE_TTL: Duration = Duration::from_secs(60 * 60);

/// Set the resolved config directory (called after usage check).
pub fn set_resolved_config_dir(dir: Option<String>) {
    if let Ok(mut cached) = RESOLVED_CONFIG_DIR.lock() {
        info!("Setting resolved config dir: {:?}", dir);
        *cached = dir;
    }
}

/// Get the current resolved config directory (for display/status).
pub fn get_resolved_config_dir() -> Option<String> {
    RESOLVED_CONFIG_DIR
        .lock()
        .ok()
        .and_then(|cached| cached.clone())
}

/// Which arm of [`get_effective_config_dir`] decided the answer — including
/// the two arms that decide there is NO answer.
///
/// The house `(value, source)` shape (`profiles::CoordBaseSource`,
/// `api_config::ApiBaseUrlArm`), applied to the per-account
/// `CLAUDE_CONFIG_DIR` selection. It exists because this resolver's `None` is
/// the most expensive value it can return — it is what makes a session spawn
/// fail loud — and `None` alone does not say WHY:
///
/// - a `LeastUsage` runner that has never run the usage probe and has no
///   manual `config_dir` has nothing configured at all, and
/// - a runner whose selected account's `.credentials.json` expired past
///   refresh HAS an account and lost it,
///
/// which are different machines needing different fixes and were, until now,
/// the same `None`. The arm is also what distinguishes the two *successful*
/// paths that produce an identical string: a `LeastUsage` runner whose picker
/// resolved to the manual dir anyway reads exactly like `Manual` mode.
///
/// The names are stable wire strings: they appear verbatim in the config
/// report (layer 11) and are meant to be greppable across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeConfigDirSource {
    /// A per-request account override was supplied and returned verbatim. Only
    /// reachable through [`get_effective_config_dir_with_override`].
    RequestOverride,
    /// `LeastUsage` mode; the credential-aware picker's resolved dir won
    /// ([`get_resolved_config_dir`]).
    LeastUsageResolved,
    /// `LeastUsage` mode, but no resolved dir had been set (the usage probe has
    /// not run yet, or picked nothing) — the manual `config_dir` was used as
    /// the fallback. Deliberately NOT reported as `Manual`: the runner is in
    /// least-usage mode and is not doing least-usage selection.
    LeastUsageConfigDirFallback,
    /// `Manual` mode; the configured `config_dir`.
    Manual,
    /// A candidate dir existed but [`has_valid_credentials`]
    /// (super::oauth_refresh::has_valid_credentials) rejected it, so the
    /// resolver yields `None` rather than pinning `CLAUDE_CONFIG_DIR` to a dead
    /// account.
    RejectedNoCredentials,
    /// No candidate at all: neither a resolved dir nor a configured
    /// `config_dir`.
    Unconfigured,
}

impl ClaudeConfigDirSource {
    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeConfigDirSource::RequestOverride => "request_override",
            ClaudeConfigDirSource::LeastUsageResolved => "least_usage_resolved",
            ClaudeConfigDirSource::LeastUsageConfigDirFallback => "least_usage_config_dir_fallback",
            ClaudeConfigDirSource::Manual => "manual_config_dir",
            ClaudeConfigDirSource::RejectedNoCredentials => "rejected_no_credentials",
            ClaudeConfigDirSource::Unconfigured => "unconfigured",
        }
    }
}

impl std::fmt::Display for ClaudeConfigDirSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Get the effective config directory, considering account selection mode,
/// plus WHICH arm produced it.
///
/// Returns a config dir ONLY when it has live credentials (a `.credentials.json`
/// that is unexpired or refreshable — see
/// [`super::oauth_refresh::has_valid_credentials`]). A credential-less or
/// unrefreshable candidate yields `None` so the spawn path fails loud
/// (`agent_runtime::run_continuation_terminal` / `spawn_claude_child`) instead
/// of pinning `CLAUDE_CONFIG_DIR` to a dead account and 401-zombie-ing the
/// subprocess. In `LeastUsage` mode the resolved dir
/// ([`pick_best_account`](super::account_usage::pick_best_account)) is already
/// credential-filtered; the recheck here also covers `Manual` mode, whose
/// `config_dir` is never run through the picker.
///
/// The [`ClaudeConfigDirSource`] is produced by the SAME traversal that
/// produces the dir — one walk, both outputs — so nothing downstream (the
/// config report included) is ever entitled to re-derive the selection rule.
/// See that type for why the `None` arms are as load-bearing as the `Some`
/// ones.
pub fn get_effective_config_dir(
    cli_settings: &settings::ClaudeCliSettings,
) -> (Option<String>, ClaudeConfigDirSource) {
    let (candidate, source) = match cli_settings.account_selection_mode {
        AccountSelectionMode::LeastUsage => match get_resolved_config_dir() {
            // The resolved dir (set by the credential-aware picker) wins.
            Some(dir) => (Some(dir), ClaudeConfigDirSource::LeastUsageResolved),
            // else fall back to the manual config_dir.
            None => (
                cli_settings.config_dir.clone(),
                ClaudeConfigDirSource::LeastUsageConfigDirFallback,
            ),
        },
        AccountSelectionMode::Manual => (
            cli_settings.config_dir.clone(),
            ClaudeConfigDirSource::Manual,
        ),
    };

    match candidate {
        Some(dir) if super::oauth_refresh::has_valid_credentials(&dir) => (Some(dir), source),
        // A candidate that failed the credential check is a DIFFERENT failure
        // from having no candidate, and the arm says which.
        Some(_) => (None, ClaudeConfigDirSource::RejectedNoCredentials),
        None => (None, ClaudeConfigDirSource::Unconfigured),
    }
}

/// Like [`get_effective_config_dir`], but honours an explicit per-request
/// account override. Same `(value, source)` shape.
///
/// When `override_dir` is `Some`, it is returned verbatim with
/// [`ClaudeConfigDirSource::RequestOverride`] — the caller is responsible for
/// validating it first (per-request account selection resolves it via
/// [`super::account_usage::resolve_requested_account`], which restricts the
/// choice to credential-valid roster dirs), which is exactly why the arm must
/// name it: this is the one path that did NOT go through the credential check
/// here, and a reader of the report has to be able to tell.
/// When `None`, delegates to the global [`get_effective_config_dir`] resolution
/// (unchanged default behaviour).
pub fn get_effective_config_dir_with_override(
    cli_settings: &settings::ClaudeCliSettings,
    override_dir: Option<&str>,
) -> (Option<String>, ClaudeConfigDirSource) {
    match override_dir {
        Some(dir) => (
            Some(dir.to_string()),
            ClaudeConfigDirSource::RequestOverride,
        ),
        None => get_effective_config_dir(cli_settings),
    }
}

/// Mark an account as rate-limited with the default cooldown duration.
///
/// Used by paths that don't have access to a precise `Retry-After` value
/// (e.g. the Claude CLI subprocess). API providers should prefer
/// [`mark_account_rate_limited_with_duration`].
pub fn mark_account_rate_limited(config_dir: &str) {
    mark_account_rate_limited_with_duration(
        config_dir,
        Duration::from_secs(RATE_LIMIT_COOLDOWN_SECS),
    );
}

/// Mark an account as rate-limited with a specific cooldown duration.
///
/// API providers should call this with the parsed `Retry-After` header so
/// the cooldown matches the server's actual recovery window. If the duration
/// is unknown, callers should fall back to [`mark_account_rate_limited`].
pub fn mark_account_rate_limited_with_duration(config_dir: &str, cooldown: Duration) {
    if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);
        info!(
            "Marking account '{}' as rate-limited for {}s",
            short_label(config_dir),
            cooldown.as_secs()
        );
        map.insert(config_dir.to_string(), (Instant::now(), cooldown));
    }
}

/// Check if an account is currently in cooldown.
pub(super) fn is_account_cooled_down(config_dir: &str) -> bool {
    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_ref() {
            if let Some((marked_at, cooldown)) = map.get(config_dir) {
                return marked_at.elapsed() < *cooldown;
            }
        }
    }
    false
}

/// How long until this specific account's cooldown expires.
///
/// Returns `Some(remaining)` if the account is still cooled down, or `None`
/// if it is available (either never marked, or the cooldown has expired).
///
/// Companion to [`is_account_cooled_down`]; reads `ACCOUNT_COOLDOWNS` under
/// a single lock acquisition for consistency with other readers in this
/// module.
pub fn time_until_cooled_down(config_dir: &str) -> Option<Duration> {
    let cooldowns = ACCOUNT_COOLDOWNS.lock().ok()?;
    let map = cooldowns.as_ref()?;
    let (marked_at, cooldown) = map.get(config_dir)?;
    let remaining = cooldown.saturating_sub(marked_at.elapsed());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining)
    }
}

/// Record weekly-usage samples from a probe into the selection snapshot.
///
/// Called by the usage-probe paths (startup periodic refresh, the
/// `check_accounts_usage` command, the `/analytics/account-usage` route) so
/// the selection hot path has fresh pace data without issuing its own HTTP
/// calls. Each tuple is `(config_dir, utilization, usage_delta, expected,
/// exhausted)`.
///
/// `expected` is the linear elapsed fraction of the account's 7-day window
/// (see `UsageSample::expected`) and is **not optional detail**: [`usage_rank`]
/// ranks the under-pace tier on it directly (highest first — that account's
/// spare capacity expires soonest and does not roll over) and divides by it to
/// get the over-pace ratio. A feeder that passes `None` for it drops the
/// account into the `Unknown` tier, where it can never outrank a measured
/// under-pace account.
pub fn record_account_usage(samples: &[(String, f64, Option<f64>, Option<f64>, bool)]) {
    if let Ok(mut snap) = USAGE_SNAPSHOT.lock() {
        let map = snap.get_or_insert_with(HashMap::new);
        let now = Instant::now();
        for (dir, utilization, usage_delta, expected, exhausted) in samples {
            map.insert(
                dir.clone(),
                UsageSample {
                    captured_at: now,
                    usage_delta: *usage_delta,
                    utilization: *utilization,
                    expected: *expected,
                    exhausted: *exhausted,
                },
            );
        }
    }
}

/// Which pace tier an account sits in, **carrying that tier's own ranking
/// key** rather than one flattened scalar.
///
/// The three tiers rank on different fields in different directions, so the
/// key travels with the tier and [`cmp_rank`] only ever compares two keys of
/// the same variant. A flattened `(u8, f64)` would encode "descending" as an
/// invisible negation and would let a future edit compare two different
/// tiers' keys — a comparison with no meaning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaceRank {
    /// Under pace (`usage_delta < 0`): burn the capacity that expires
    /// soonest. Ordered by `expected` **DESCENDING** — the account whose
    /// 7-day window is furthest along wins, because unused weekly capacity
    /// does not roll over past the reset (use-it-or-lose-it).
    UnderPace { expected: f64 },
    /// No usable pace signal — `usage_delta` and/or `expected` is absent, so
    /// the account cannot be classified under- or over-pace at all. Ordered
    /// by raw `utilization` ASCENDING, which is exactly how this population
    /// has always been ranked.
    Unknown { utilization: f64 },
    /// At or over pace (`usage_delta >= 0`): least-over **relative to its own
    /// pace**. Ordered by `ratio = utilization / expected` ASCENDING. A
    /// difference is not comparable across accounts at different points in
    /// their windows; a ratio is.
    OverPace { ratio: f64 },
}

impl PaceRank {
    /// Tier order: under-pace (0) before unknown (1) before over-pace (2).
    /// Every under-pace account sorts ahead of every unknown one, and every
    /// unknown one ahead of every measured-over-pace one.
    fn tier_index(&self) -> u8 {
        match self {
            PaceRank::UnderPace { .. } => 0,
            PaceRank::Unknown { .. } => 1,
            PaceRank::OverPace { .. } => 2,
        }
    }
}

/// One account's full selection rank: the dominating `exhausted` tier plus its
/// [`PaceRank`]. Compare two of these with [`cmp_rank`] — never field by
/// field, and never by flattening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageRank {
    pub exhausted: bool,
    pub pace: PaceRank,
}

/// Selection rank for an account from the latest snapshot, if a fresh sample
/// exists: a [`UsageRank`] carrying the dominating `exhausted` flag plus the
/// account's [`PaceRank`] — the pace tier it sits in AND that tier's own key.
///
/// `exhausted` is the dominating tier: an exhausted account (out of tokens /
/// rejected) always sorts after a usable one, no matter how favourable its
/// pace key. Below it, the sample is classified into exactly one pace tier:
///
/// | condition | tier | that tier's key |
/// |---|---|---|
/// | `usage_delta < 0` and `expected` present | [`PaceRank::UnderPace`] | `expected` **DESCENDING** |
/// | `usage_delta` and/or `expected` absent | [`PaceRank::Unknown`] | `utilization` ascending |
/// | `usage_delta >= 0` and `expected` present | [`PaceRank::OverPace`] | `utilization / expected` **ASCENDING** |
///
/// The under-pace key is descending **because unused weekly capacity expires
/// at the reset and does not roll over**: the account worth burning is the one
/// whose window is furthest along, since its spare capacity is the capacity
/// about to be lost. (This reverses the earlier `usage_delta`-ascending rule,
/// which picked the account with the *most* runway — precisely the capacity in
/// no danger of expiring.)
///
/// The over-pace key is a **ratio, not a difference**, because a difference is
/// not comparable across accounts at different points in their windows: +5
/// points over at 10% expected is far more over-pace than +5 points over at
/// 80% expected, yet a difference scores the two identically. The ratio is
/// built by [`over_pace_ratio`], never by a bare division — see its zero
/// guard. Ranks are only ever compared through [`cmp_rank`]; mirrored in
/// TypeScript by `compareByUsageHeadroom`
/// (`src/components/settings/types.ts`).
///
/// Returns `None` when there is no sample or it is older than
/// [`USAGE_SNAPSHOT_TTL`], so callers fall back to cooldown-only ordering.
pub fn usage_rank(config_dir: &str) -> Option<UsageRank> {
    let snap = USAGE_SNAPSHOT.lock().ok()?;
    let map = snap.as_ref()?;
    let sample = map.get(config_dir)?;
    if sample.captured_at.elapsed() > USAGE_SNAPSHOT_TTL {
        return None;
    }
    let pace = match (sample.usage_delta, sample.expected) {
        // Measured under its own projected pace → use-it-or-lose-it tier.
        (Some(delta), Some(expected)) if delta < 0.0 => PaceRank::UnderPace { expected },
        // Measured at-or-over pace → ranked by overrun relative to its pace.
        (Some(_), Some(expected)) => PaceRank::OverPace {
            ratio: over_pace_ratio(sample.utilization, expected),
        },
        // Either half of the pace signal missing → no pace classification is
        // measurable, so it gets its own tier rather than a manufactured one.
        _ => PaceRank::Unknown {
            utilization: sample.utilization,
        },
    };
    Some(UsageRank {
        exhausted: sample.exhausted,
        pace,
    })
}

/// The over-pace ratio `utilization / expected`, computed through an explicit
/// guard so a NaN is **never constructed**.
///
/// `expected == 0.0` is a live case, not a theoretical one: `elapsed_fraction`
/// clamps to `0.0` on a just-reset window
/// (`commands::ai_settings::compute_expected_usage`). A bare division would
/// then yield `0.0 / 0.0 == NaN`, and `partial_cmp` degrades NaN to
/// [`Ordering::Equal`] — which would make the selection order depend on the
/// roster's input position instead of on the data.
///
/// | case | ratio | position within over-pace |
/// |---|---|---|
/// | `expected == 0.0`, `utilization == 0.0` | `1.0` | first — exactly on pace with nothing spent |
/// | `expected == 0.0`, `utilization > 0.0` | [`f64::INFINITY`] | last — a just-reset window whose capacity is in no danger of expiring |
fn over_pace_ratio(utilization: f64, expected: f64) -> f64 {
    if expected == 0.0 {
        if utilization == 0.0 {
            1.0
        } else {
            f64::INFINITY
        }
    } else {
        utilization / expected
    }
}

/// Total order over [`UsageRank`] — the ONE comparator both pickers use.
///
/// Three levels, in order:
/// 1. `exhausted` — `false` before `true`. An out-of-tokens account sorts last
///    no matter how favourable its pace key.
/// 2. The pace **tier** ([`PaceRank::tier_index`]): under-pace before unknown
///    before over-pace.
/// 3. Only for two accounts in the *same* tier, that tier's own key in that
///    tier's own direction. Keys from different tiers are never compared —
///    they measure different things and a comparison between them would be
///    meaningless.
///
/// `partial_cmp(...).unwrap_or(Ordering::Equal)` is the leaf comparison. It is
/// belt-and-braces behind [`over_pace_ratio`]'s zero guard, not a substitute
/// for it: the guard is what ensures no NaN ever reaches this point.
///
/// Duplicated in TypeScript by `compareByUsageHeadroom`
/// (`src/components/settings/types.ts`); the two must stay in sync.
pub fn cmp_rank(a: &UsageRank, b: &UsageRank) -> Ordering {
    a.exhausted
        .cmp(&b.exhausted)
        .then_with(|| a.pace.tier_index().cmp(&b.pace.tier_index()))
        .then_with(|| match (a.pace, b.pace) {
            // DESCENDING: the account whose window is furthest along wins,
            // because its unused capacity expires soonest.
            (PaceRank::UnderPace { expected: x }, PaceRank::UnderPace { expected: y }) => {
                y.partial_cmp(&x).unwrap_or(Ordering::Equal)
            }
            // Ascending: least-used first.
            (PaceRank::Unknown { utilization: x }, PaceRank::Unknown { utilization: y }) => {
                x.partial_cmp(&y).unwrap_or(Ordering::Equal)
            }
            // Ascending: least-over relative to its own pace first.
            (PaceRank::OverPace { ratio: x }, PaceRank::OverPace { ratio: y }) => {
                x.partial_cmp(&y).unwrap_or(Ordering::Equal)
            }
            // Unreachable: `tier_index` already separated different tiers.
            _ => Ordering::Equal,
        })
}

/// Whether an account was last seen **exhausted** (out of tokens / rejected),
/// tolerating a longer staleness than [`usage_rank`] (see
/// [`EXHAUSTION_STALE_TTL`]). Used only by the cold-start / stale-snapshot
/// fallback in `pick_best_account` to skip an account we recently saw maxed
/// out but that isn't in a rate-limit cooldown (the usage probe's 429 doesn't
/// set an inference cooldown). Returns `false` when there is no sample, the
/// sample is older than the exhaustion staleness window, or it was usable.
pub fn account_known_exhausted(config_dir: &str) -> bool {
    if let Ok(snap) = USAGE_SNAPSHOT.lock() {
        if let Some(map) = snap.as_ref() {
            if let Some(sample) = map.get(config_dir) {
                return sample.exhausted && sample.captured_at.elapsed() <= EXHAUSTION_STALE_TTL;
            }
        }
    }
    false
}

/// Rotate to the next available account after a rate-limit hit.
///
/// Marks the current account as rate-limited (with the default cooldown,
/// **but only if it isn't already marked** — so a precise `Retry-After`
/// duration set by [`mark_account_rate_limited_with_duration`] survives),
/// then picks the first non-cooled-down account from the configured
/// `claude_config_dirs`. Returns `true` if a switch happened, `false` if
/// no alternative is available.
pub fn rotate_account_on_rate_limit() -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return false;
    }

    let current = get_resolved_config_dir();
    let default_cooldown = Duration::from_secs(RATE_LIMIT_COOLDOWN_SECS);

    // Single lock acquisition: mark current as rate-limited + find best alternative
    let next_dir = if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.get_or_insert_with(HashMap::new);

        // Mark the current account as rate-limited — but only if it isn't
        // already in the map. API providers may have already inserted a
        // precise `Retry-After`-derived duration; don't clobber that with
        // the shorter default.
        if let Some(ref dir) = current {
            if !map.contains_key(dir) {
                info!(
                    "Marking account '{}' as rate-limited for {}s",
                    short_label(dir),
                    default_cooldown.as_secs()
                );
                map.insert(dir.clone(), (Instant::now(), default_cooldown));
            }
        }

        // Find the first account that is not in cooldown
        let available = config_dirs.iter().find(|d| {
            current.as_ref() != Some(*d)
                && map
                    .get(*d)
                    .is_none_or(|(marked_at, cooldown)| marked_at.elapsed() >= *cooldown)
        });

        if let Some(dir) = available {
            Some(dir.clone())
        } else {
            // All accounts are in cooldown — pick the one closest to expiry
            // (smallest remaining = `cooldown - elapsed`).
            let best = config_dirs
                .iter()
                .filter(|d| current.as_ref() != Some(*d))
                .min_by_key(|d| {
                    map.get(*d)
                        .map(|(marked_at, cooldown)| cooldown.saturating_sub(marked_at.elapsed()))
                        .unwrap_or(Duration::ZERO) // never rate-limited = best candidate
                });
            best.cloned()
        }
    } else {
        return false;
    };
    // Lock is dropped here before calling set_resolved_config_dir

    if let Some(dir) = next_dir {
        let was_all_limited = is_account_cooled_down(&dir);
        if was_all_limited {
            warn!(
                "All accounts rate-limited, switching to closest-to-expiry: '{}'",
                short_label(&dir)
            );
        } else {
            info!(
                "Rotating account: '{}' -> '{}'",
                current.as_deref().map(short_label).unwrap_or("none"),
                short_label(&dir)
            );
        }
        set_resolved_config_dir(Some(dir));
        true
    } else {
        false
    }
}

/// How long until the next account becomes available (cooldown expires).
///
/// Returns `None` if any account is already available, or if there are
/// fewer than 2 accounts configured. Returns `Some(duration)` with the
/// wait time until the earliest cooldown expires.
///
/// Uses a single lock acquisition to avoid TOCTOU races.
pub fn time_until_next_account_available() -> Option<Duration> {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.len() < 2 {
        return None;
    }

    if let Ok(cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        let map = cooldowns.as_ref()?;

        // Check all accounts under one lock: compute remaining cooldown per account
        let mut all_in_cooldown = true;
        let mut earliest_remaining: Option<Duration> = None;

        for dir in &config_dirs {
            if let Some((marked_at, cooldown)) = map.get(dir) {
                let elapsed = marked_at.elapsed();
                if elapsed >= *cooldown {
                    // This account's cooldown has expired — no waiting needed
                    all_in_cooldown = false;
                    break;
                }
                let remaining = *cooldown - elapsed;
                earliest_remaining =
                    Some(earliest_remaining.map_or(remaining, |prev| prev.min(remaining)));
            } else {
                // Account was never rate-limited — it's available
                all_in_cooldown = false;
                break;
            }
        }

        if all_in_cooldown {
            earliest_remaining
        } else {
            None
        }
    } else {
        None
    }
}

/// Clear the cooldown for the account that has been cooling the longest,
/// switch to it, and return true. Returns false if no accounts are configured.
pub fn force_unlock_earliest_account() -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if config_dirs.is_empty() {
        return false;
    }

    let best = if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_mut() {
            // Find account with the oldest cooldown (most elapsed time since marked).
            // Accounts not in the map count as "infinitely old" — i.e. never cooled,
            // so they're the best candidates to unlock-and-switch-to.
            let best = config_dirs
                .iter()
                .max_by_key(|d| {
                    map.get(*d)
                        .map(|(marked_at, _)| marked_at.elapsed())
                        .unwrap_or(Duration::MAX)
                })
                .cloned();
            // Clear its cooldown
            if let Some(ref dir) = best {
                map.remove(dir);
                info!(
                    "Force-unlocked account '{}' after cooldown wait",
                    short_label(dir)
                );
            }
            best
        } else {
            config_dirs.into_iter().next()
        }
    } else {
        return false;
    };

    if let Some(dir) = best {
        set_resolved_config_dir(Some(dir));
        true
    } else {
        false
    }
}

/// Get a short label for a config dir (last path component).
fn short_label(config_dir: &str) -> &str {
    std::path::Path::new(config_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(config_dir)
}

/// Get account status info for all configured accounts.
/// Returns (config_dir, label, is_active, is_cooled_down) for each.
pub fn get_account_statuses() -> Vec<(String, String, bool, bool)> {
    let config_dirs = settings::get_claude_config_dirs();
    let current = get_resolved_config_dir();

    config_dirs
        .into_iter()
        .map(|dir| {
            let label = short_label(&dir).to_string();
            let is_active = current.as_ref() == Some(&dir);
            let cooled = is_account_cooled_down(&dir);
            (dir, label, is_active, cooled)
        })
        .collect()
}

/// Manually switch to a specific account by config dir path.
/// Clears any cooldown on the target account.
/// Returns true if the switch was valid, false if the dir isn't in the configured list.
pub fn switch_to_account(config_dir: &str) -> bool {
    let config_dirs = settings::get_claude_config_dirs();
    if !config_dirs.contains(&config_dir.to_string()) {
        warn!(
            "Cannot switch to '{}': not in configured claude_config_dirs",
            config_dir
        );
        return false;
    }

    // Clear cooldown on the target account
    if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
        if let Some(map) = cooldowns.as_mut() {
            map.remove(config_dir);
        }
    }

    info!(
        "Manually switching to account '{}'",
        short_label(config_dir)
    );
    set_resolved_config_dir(Some(config_dir.to_string()));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    /// Remove a specific dir from the static `ACCOUNT_COOLDOWNS` map.
    ///
    /// Tests share this static, so each test uses a unique dir path and
    /// clears only its own entries. Wiping the entire map would race with
    /// concurrent tests in `account_usage::tests` that mark their own dirs.
    fn clear_dir(dir: &str) {
        if let Ok(mut cooldowns) = ACCOUNT_COOLDOWNS.lock() {
            if let Some(map) = cooldowns.as_mut() {
                map.remove(dir);
            }
        }
    }

    /// `Manual` mode with nothing configured, and `Manual` mode whose
    /// configured dir has no live credentials, both return `None` — and the
    /// ARM is the only thing that tells them apart. Before Phase 2 of
    /// `2026-08-20-effective-config-provenance-and-env-generation` they were
    /// indistinguishable, so "no account" was one message covering a runner
    /// that was never set up and a runner whose login expired.
    ///
    /// Deliberately `Manual`-only: the `LeastUsage` arms read the process-global
    /// `RESOLVED_CONFIG_DIR`, and a test that mutated it would race every other
    /// test in this crate. The arms exercised here touch no shared state.
    #[test]
    fn effective_config_dir_distinguishes_unconfigured_from_dead_credentials() {
        let unconfigured = settings::ClaudeCliSettings {
            account_selection_mode: AccountSelectionMode::Manual,
            config_dir: None,
            ..Default::default()
        };
        assert_eq!(
            get_effective_config_dir(&unconfigured),
            (None, ClaudeConfigDirSource::Unconfigured)
        );

        // A path that certainly holds no `.credentials.json`.
        let dead = settings::ClaudeCliSettings {
            account_selection_mode: AccountSelectionMode::Manual,
            config_dir: Some(
                "/qontinui-test/no-such-account-dir/effective_config_dir_arms".to_string(),
            ),
            ..Default::default()
        };
        assert_eq!(
            get_effective_config_dir(&dead),
            (None, ClaudeConfigDirSource::RejectedNoCredentials)
        );
    }

    /// A per-request override is returned VERBATIM and names itself — it is the
    /// one path that skips the credential check here, so a report that showed
    /// it as `manual_config_dir` would be claiming a validation that did not
    /// happen.
    #[test]
    fn effective_config_dir_override_is_verbatim_and_names_itself() {
        let cli = settings::ClaudeCliSettings {
            account_selection_mode: AccountSelectionMode::Manual,
            config_dir: Some("/configured/but/ignored".to_string()),
            ..Default::default()
        };
        assert_eq!(
            get_effective_config_dir_with_override(&cli, Some("/pinned/by/request")),
            (
                Some("/pinned/by/request".to_string()),
                ClaudeConfigDirSource::RequestOverride
            )
        );
    }

    /// The arm vocabulary is a WIRE contract (it is printed verbatim in
    /// `config report` layer 11 and compared across machines), so it is pinned
    /// to LITERALS rather than to the enum's own `as_str`.
    #[test]
    fn claude_config_dir_source_wire_strings_are_stable() {
        assert_eq!(
            ClaudeConfigDirSource::RequestOverride.as_str(),
            "request_override"
        );
        assert_eq!(
            ClaudeConfigDirSource::LeastUsageResolved.as_str(),
            "least_usage_resolved"
        );
        assert_eq!(
            ClaudeConfigDirSource::LeastUsageConfigDirFallback.as_str(),
            "least_usage_config_dir_fallback"
        );
        assert_eq!(ClaudeConfigDirSource::Manual.as_str(), "manual_config_dir");
        assert_eq!(
            ClaudeConfigDirSource::RejectedNoCredentials.as_str(),
            "rejected_no_credentials"
        );
        assert_eq!(ClaudeConfigDirSource::Unconfigured.as_str(), "unconfigured");
    }

    #[test]
    fn mark_with_duration_then_is_cooled_then_expires() {
        let dir = "/test/config/mark_with_duration";
        clear_dir(dir);
        mark_account_rate_limited_with_duration(dir, Duration::from_millis(50));
        assert!(
            is_account_cooled_down(dir),
            "account should be cooled immediately after marking"
        );
        sleep(Duration::from_millis(80));
        assert!(
            !is_account_cooled_down(dir),
            "account should no longer be cooled after the duration elapses"
        );
        clear_dir(dir);
    }

    #[test]
    fn time_until_cooled_down_returns_remaining() {
        let dir = "/test/config/time_remaining";
        clear_dir(dir);
        let total = Duration::from_secs(120);
        mark_account_rate_limited_with_duration(dir, total);
        let remaining = time_until_cooled_down(dir).expect("should be cooled");
        // Within a few seconds tolerance — the test exec time is the only delta.
        assert!(
            remaining <= total && remaining + Duration::from_secs(5) >= total,
            "expected ~{}s remaining, got {}s",
            total.as_secs(),
            remaining.as_secs()
        );
        clear_dir(dir);
    }

    #[test]
    fn time_until_cooled_down_returns_none_when_unmarked() {
        let dir = "/test/config/never_marked";
        clear_dir(dir);
        assert!(time_until_cooled_down(dir).is_none());
    }

    #[test]
    fn time_until_cooled_down_returns_none_after_expiry() {
        let dir = "/test/config/expired";
        clear_dir(dir);
        mark_account_rate_limited_with_duration(dir, Duration::from_millis(30));
        sleep(Duration::from_millis(60));
        assert!(
            time_until_cooled_down(dir).is_none(),
            "expired cooldown should report None"
        );
        clear_dir(dir);
    }

    /// Convenience: the `PaceRank` of a fresh sample, panicking if there is
    /// none (every caller below has just recorded one).
    fn pace_of(dir: &str) -> PaceRank {
        usage_rank(dir).expect("a sample was just recorded").pace
    }

    #[test]
    fn usage_rank_classifies_under_pace_by_expected() {
        let dir = "/test/config/pace_under";
        // 0.55 used against 0.80 expected → delta -0.25, measured UNDER pace.
        // The key is `expected` (the window is 80% elapsed), NOT the delta.
        record_account_usage(&[(dir.to_string(), 0.55, Some(-0.25), Some(0.80), false)]);
        assert_eq!(pace_of(dir), PaceRank::UnderPace { expected: 0.80 });
    }

    #[test]
    fn usage_rank_missing_expected_lands_in_unknown_ranked_by_utilization() {
        let dir_no_delta = "/test/config/pace_unknown_no_delta";
        let dir_no_expected = "/test/config/pace_unknown_no_expected";
        // No delta AND no expected — the Haiku header-probe fallback with no
        // reset header. Ranked on raw utilization, as it always has been.
        record_account_usage(&[(dir_no_delta.to_string(), 0.42, None, None, false)]);
        assert_eq!(
            pace_of(dir_no_delta),
            PaceRank::Unknown { utilization: 0.42 }
        );
        // A delta with NO expected cannot yield either tier's key either, so
        // it is Unknown too rather than a manufactured pace classification.
        record_account_usage(&[(dir_no_expected.to_string(), 0.31, Some(-0.10), None, false)]);
        assert_eq!(
            pace_of(dir_no_expected),
            PaceRank::Unknown { utilization: 0.31 }
        );
    }

    #[test]
    fn usage_rank_classifies_over_pace_by_ratio() {
        let dir = "/test/config/pace_over";
        // 0.90 used against 0.75 expected → delta +0.15, ratio 1.2.
        record_account_usage(&[(dir.to_string(), 0.90, Some(0.15), Some(0.75), false)]);
        match pace_of(dir) {
            PaceRank::OverPace { ratio } => {
                assert!(!ratio.is_nan(), "the ratio must never be NaN");
                assert!((ratio - 1.2).abs() < 1e-12, "ratio was {ratio}");
            }
            other => panic!("expected OverPace, got {other:?}"),
        }
    }

    #[test]
    fn usage_rank_delta_exactly_zero_is_over_pace_not_under() {
        let dir = "/test/config/pace_boundary_zero_delta";
        // The tier boundary is `usage_delta < 0`, so exactly-on-pace is the
        // over-pace tier — ratio 1.0, the least-over value there is.
        record_account_usage(&[(dir.to_string(), 0.50, Some(0.0), Some(0.50), false)]);
        assert_eq!(pace_of(dir), PaceRank::OverPace { ratio: 1.0 });
    }

    /// Row 1 of the `expected == 0.0` table: a just-reset window with nothing
    /// spent is defined as ratio `1.0` — first within over-pace. A bare
    /// `0.0 / 0.0` would be NaN here, and `partial_cmp` degrades NaN to
    /// `Ordering::Equal`, making the order depend on roster position.
    #[test]
    fn usage_rank_zero_expected_zero_utilization_is_ratio_one_not_nan() {
        let dir = "/test/config/pace_zero_expected_zero_util";
        record_account_usage(&[(dir.to_string(), 0.0, Some(0.0), Some(0.0), false)]);
        match pace_of(dir) {
            PaceRank::OverPace { ratio } => {
                assert!(!ratio.is_nan(), "0.0/0.0 must NOT reach the comparator");
                assert!(ratio.is_finite());
                assert_eq!(ratio, 1.0);
            }
            other => panic!("expected OverPace, got {other:?}"),
        }
    }

    /// Row 2 of the `expected == 0.0` table: tokens spent against a
    /// just-reset window is the arithmetic limit, `f64::INFINITY` — last
    /// within over-pace, which is also the right answer on the merits (that
    /// account's capacity is in no danger of expiring).
    #[test]
    fn usage_rank_zero_expected_positive_utilization_is_infinity_not_nan() {
        let dir = "/test/config/pace_zero_expected_pos_util";
        record_account_usage(&[(dir.to_string(), 0.07, Some(0.07), Some(0.0), false)]);
        match pace_of(dir) {
            PaceRank::OverPace { ratio } => {
                assert!(!ratio.is_nan(), "the ratio must never be NaN");
                assert!(ratio.is_infinite() && ratio.is_sign_positive());
            }
            other => panic!("expected OverPace, got {other:?}"),
        }
    }

    #[test]
    fn usage_rank_carries_exhausted_flag() {
        let dir = "/test/config/rank_exhausted";
        // Exhausted even though the pace key looks favourable (under pace with
        // a nearly-elapsed window).
        record_account_usage(&[(dir.to_string(), 0.95, Some(-0.05), Some(1.0), true)]);
        assert_eq!(
            usage_rank(dir),
            Some(UsageRank {
                exhausted: true,
                pace: PaceRank::UnderPace { expected: 1.0 },
            })
        );
    }

    #[test]
    fn usage_rank_none_when_unrecorded() {
        assert_eq!(usage_rank("/test/config/rank_never"), None);
    }

    // --- cmp_rank: the one shared comparator --------------------------------

    fn rank(exhausted: bool, pace: PaceRank) -> UsageRank {
        UsageRank { exhausted, pace }
    }

    #[test]
    fn cmp_rank_exhausted_is_the_dominating_tier() {
        // Best possible pace key, but exhausted → still sorts last.
        let dead = rank(true, PaceRank::UnderPace { expected: 0.99 });
        let alive = rank(false, PaceRank::OverPace { ratio: 9.0 });
        assert_eq!(cmp_rank(&dead, &alive), Ordering::Greater);
        assert_eq!(cmp_rank(&alive, &dead), Ordering::Less);
    }

    #[test]
    fn cmp_rank_tier_order_is_under_then_unknown_then_over() {
        // Each tier's WORST member still beats the next tier's best.
        let under = rank(false, PaceRank::UnderPace { expected: 0.0 });
        let unknown = rank(false, PaceRank::Unknown { utilization: 1.0 });
        let over = rank(false, PaceRank::OverPace { ratio: 1.0 });
        assert_eq!(cmp_rank(&under, &unknown), Ordering::Less);
        assert_eq!(cmp_rank(&unknown, &over), Ordering::Less);
        assert_eq!(cmp_rank(&under, &over), Ordering::Less);
    }

    #[test]
    fn cmp_rank_under_pace_orders_by_expected_descending() {
        // Use-it-or-lose-it: the window furthest along wins.
        let late = rank(false, PaceRank::UnderPace { expected: 0.80 });
        let early = rank(false, PaceRank::UnderPace { expected: 0.06 });
        assert_eq!(cmp_rank(&late, &early), Ordering::Less);
        assert_eq!(cmp_rank(&early, &late), Ordering::Greater);
        assert_eq!(cmp_rank(&late, &late), Ordering::Equal);
    }

    #[test]
    fn cmp_rank_unknown_orders_by_utilization_ascending() {
        let empty = rank(false, PaceRank::Unknown { utilization: 0.10 });
        let full = rank(false, PaceRank::Unknown { utilization: 0.90 });
        assert_eq!(cmp_rank(&empty, &full), Ordering::Less);
    }

    #[test]
    fn cmp_rank_over_pace_orders_by_ratio_ascending_with_infinity_last() {
        let least = rank(false, PaceRank::OverPace { ratio: 1.063 });
        let most = rank(false, PaceRank::OverPace { ratio: 1.400 });
        let just_reset = rank(
            false,
            PaceRank::OverPace {
                ratio: f64::INFINITY,
            },
        );
        assert_eq!(cmp_rank(&least, &most), Ordering::Less);
        assert_eq!(cmp_rank(&most, &just_reset), Ordering::Less);
        assert_eq!(cmp_rank(&just_reset, &least), Ordering::Greater);
    }

    #[test]
    fn account_known_exhausted_reflects_last_sample() {
        let dir_ex = "/test/config/known_exhausted";
        let dir_ok = "/test/config/known_usable";
        record_account_usage(&[(dir_ex.to_string(), 1.0, Some(0.1), Some(0.9), true)]);
        record_account_usage(&[(dir_ok.to_string(), 0.5, Some(-0.1), Some(0.6), false)]);
        assert!(account_known_exhausted(dir_ex), "exhausted sample → true");
        assert!(!account_known_exhausted(dir_ok), "usable sample → false");
        // Unrecorded account is treated as not-known-exhausted (eligible).
        assert!(!account_known_exhausted("/test/config/known_never"));
    }

    #[test]
    fn long_cooldown_survives_rotate_default_remark() {
        // Regression: if a 600s cooldown is set by an API provider via
        // `mark_account_rate_limited_with_duration`, calling
        // `rotate_account_on_rate_limit` must NOT overwrite it with the
        // shorter default (300s).
        //
        // This test only exercises the cooldown map itself — we can't invoke
        // `rotate_account_on_rate_limit` without setting up `settings`. The
        // protected invariant is the `map.contains_key` branch in
        // `rotate_account_on_rate_limit`; simulate that branch directly.
        let dir = "/test/config/long_cooldown";
        clear_dir(dir);
        let long = Duration::from_secs(600);
        mark_account_rate_limited_with_duration(dir, long);

        // Simulate the guarded re-mark logic from `rotate_account_on_rate_limit`.
        let already_marked = ACCOUNT_COOLDOWNS
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|m| m.contains_key(dir)))
            .unwrap_or(false);
        assert!(
            already_marked,
            "account should be in the cooldown map before the rotate guard fires"
        );

        // Verify the original long duration is still in effect (would be the
        // case after `rotate_account_on_rate_limit` no-ops the re-mark).
        let remaining = time_until_cooled_down(dir).expect("still cooled");
        assert!(
            remaining > Duration::from_secs(500),
            "long cooldown should still be in effect, got {}s",
            remaining.as_secs()
        );
        clear_dir(dir);
    }
}
