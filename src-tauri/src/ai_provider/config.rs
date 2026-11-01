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
/// [`usage_rank`] — or, in `HighestExpectedUsage` mode, by
/// [`usage_rank_highest_expected`].
///
/// `usage_delta` is the account's actual 7-day utilization minus its
/// *expected* linear utilization at this point in the billing window
/// (negative = under projected pace); `None` when the probe couldn't compute
/// it (e.g. the 7d-reset header was absent). Against the floors of
/// [`under_pace_shortfall`] (less any handoff charge) it is what selects the
/// pace tier — it is no longer a ranking key in its own right. The handoff
/// charge is deliberately NOT a field here: [`record_account_usage`] replaces
/// the whole sample on every probe, which would wipe it (`handoff_ledger`). `expected` is
/// the key the under-pace tier actually ranks on, and the denominator of the
/// over-pace ratio; it is also the value [`usage_rank_highest_expected`]
/// returns directly for a qualifying under-pace account. `utilization` is the
/// raw 0.0–1.0 weekly fraction: the ranking key for the `Unknown` tier, and
/// the numerator of the over-pace ratio.
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

/// Minimum shortfall `expected - effective_utilization` for an account to count
/// as UNDER its pace (see [`under_pace_shortfall`]).
///
/// A bare `delta < 0` admitted an account by a hair: on 2026-10-18 pakqon
/// qualified on a 0.4-point margin, won the handoffs, and ran out with ten
/// sessions on it. The probe also reports whole percents, so a sub-point delta
/// is below the measurement's own resolution. Override:
/// `claude_cli.selection_min_shortfall`, clamped to [`MIN_FLOOR_SETTING`].
pub const MIN_SHORTFALL: f64 = 0.02;

/// Minimum capacity left before the reset, `1 - effective_utilization`, for an
/// account to count as under its pace — and the line below which an account
/// ranks after every other usable one ([`UsageRank::low_headroom`]). Guards a
/// thin account that is still well under pace late in its window. Override:
/// `claude_cli.selection_min_headroom`, clamped to [`MIN_FLOOR_SETTING`].
pub const MIN_HEADROOM: f64 = 0.10;

/// The load a handoff charges its target, in utilization units, until it
/// decays (`handoff_ledger`). With [`MIN_SHORTFALL`] an account with shortfall
/// `s` admits about `(s - MIN_SHORTFALL) / HANDOFF_LOAD_ESTIMATE + 1`
/// handoffs in one window before it drops out of the under-pace tier.
///
/// Calibration (2026-10-28, from the incident's runner logs on spaceship,
/// `qontinui-runner.log.2026-10-18`/`-19`): pakqon's `(oauth_usage): 7d=`
/// probe read 82 % at 22:03 and 100 % at 07:32 — 18 points in 9.5 h — while
/// it carried at least 13 handed-over sessions (10 from merytshost per the
/// plan, 3 landed on spaceship at 21:07–21:17), i.e. 0.014–0.018 per session.
/// The probe resolves whole percents, so 0.02 is the nearest step at or above
/// the measurement, erring toward spreading load. Override: `claude_cli.handoff_load_estimate`, clamped to `0.0..=1.0`
/// (`0.0` turns the charge off).
pub const HANDOFF_LOAD_ESTIMATE: f64 = 0.02;

/// The lowest value either floor may be set to. Utilization is reported in
/// whole percents, so a floor below one point is below the measurement's
/// resolution.
pub const MIN_FLOOR_SETTING: f64 = 0.01;

/// Slack for the floor comparisons, so a shortfall or headroom that equals its
/// floor in decimal (`0.84 - 0.82`) is not refused for a binary rounding error.
const FLOOR_EPSILON: f64 = 1e-9;

/// The two headroom floors every pace reader applies (D1 of plan
/// `2026-10-19-usage-limit-handoff-lands-sessions-on-accounts-that-cannot-absorb-them`).
/// Resolved from settings once per pick, BEFORE any selection lock is taken,
/// and passed to the readers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadroomFloors {
    pub min_shortfall: f64,
    pub min_headroom: f64,
}

impl Default for HeadroomFloors {
    fn default() -> Self {
        Self {
            min_shortfall: MIN_SHORTFALL,
            min_headroom: MIN_HEADROOM,
        }
    }
}

impl HeadroomFloors {
    /// The floors a `ClaudeCliSettings` asks for: each override clamped to
    /// `MIN_FLOOR_SETTING..=1.0`, an absent or non-finite one the constant.
    pub fn from_cli(cli: &settings::ClaudeCliSettings) -> Self {
        let floor = |v: Option<f64>, default: f64| {
            v.filter(|v| v.is_finite())
                .map_or(default, |v| v.clamp(MIN_FLOOR_SETTING, 1.0))
        };
        Self {
            min_shortfall: floor(cli.selection_min_shortfall, MIN_SHORTFALL),
            min_headroom: floor(cli.selection_min_headroom, MIN_HEADROOM),
        }
    }

    /// The floors from the stored settings.
    pub fn effective() -> Self {
        Self::from_cli(&settings::get_ai_settings().claude_cli)
    }
}

/// The handoff load charge a `ClaudeCliSettings` asks for: the override
/// clamped to `0.0..=1.0`, an absent or non-finite one [`HANDOFF_LOAD_ESTIMATE`].
pub fn handoff_load_estimate_from(cli: &settings::ClaudeCliSettings) -> f64 {
    cli.handoff_load_estimate
        .filter(|v| v.is_finite())
        .map_or(HANDOFF_LOAD_ESTIMATE, |v| v.clamp(0.0, 1.0))
}

/// [`handoff_load_estimate_from`] the stored settings.
pub fn effective_handoff_load_estimate() -> f64 {
    handoff_load_estimate_from(&settings::get_ai_settings().claude_cli)
}

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

/// Test-only RAII guard: captures `RESOLVED_CONFIG_DIR` when built and writes
/// it back on drop, so whatever a test (or a production path it drives, such as
/// the retry loop's rate-limit rotation) publishes there cannot outlive the
/// test — including when an assertion panics. It restores; it does not
/// serialize — hold `pin_account_selection_for_test` for that.
#[cfg(test)]
pub(crate) struct ResolvedConfigDirRestore(Option<String>);

#[cfg(test)]
impl ResolvedConfigDirRestore {
    pub(crate) fn capture() -> Self {
        Self(get_resolved_config_dir())
    }
}

#[cfg(test)]
impl Drop for ResolvedConfigDirRestore {
    fn drop(&mut self) {
        set_resolved_config_dir(self.0.take());
    }
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
    /// A model gateway is declared: every session runs under the runner-owned
    /// gateway config dir, and subscription accounts are off
    /// ([`crate::model_gateway`]). Wins over a request override too.
    ModelGateway,
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
            ClaudeConfigDirSource::ModelGateway => "model_gateway",
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
    if let Some(dir) = crate::model_gateway::session_config_dir_override() {
        return (Some(dir), ClaudeConfigDirSource::ModelGateway);
    }
    // The EFFECTIVE mode, not the raw field: a locally-`manual`, UNPINNED
    // machine under a fleet auto mode has the picker rotate
    // (`pick_best_account` reads the same resolver), and matching the raw
    // field here would discard that pick and spawn on the manual `config_dir`
    // — the fleet value would half-apply. Resolved off `cli_settings` itself —
    // callers hand in a roster-overlaid settings document (normally
    // `get_ai_settings().claude_cli`, the same source `effective_selection_mode`
    // reads) — so the mode and its pin come from the document this function
    // was given.
    let mode = crate::claude_accounts::resolve_for_cli_settings(cli_settings);
    let (candidate, source) = match mode {
        // Every auto mode pins its choice via the same `RESOLVED_CONFIG_DIR`
        // set by `pick_best_account` — the source names predate the other
        // auto modes but still apply: they describe "the auto-picker resolved
        // / fell back", not which ranking the picker used.
        AccountSelectionMode::LeastUsage
        | AccountSelectionMode::HighestExpectedUsage
        | AccountSelectionMode::WeightedRandom
        | AccountSelectionMode::AlternatingHandoff => {
            match get_resolved_config_dir() {
                // The resolved dir (set by the credential-aware picker) wins.
                Some(dir) => (Some(dir), ClaudeConfigDirSource::LeastUsageResolved),
                // else fall back to the manual config_dir.
                None => (
                    cli_settings.config_dir.clone(),
                    ClaudeConfigDirSource::LeastUsageConfigDirFallback,
                ),
            }
        }
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
    // A per-request account is a subscription account; with a gateway declared
    // those are off, so the gateway dir wins over the override as well.
    if let Some(dir) = crate::model_gateway::session_config_dir_override() {
        return (Some(dir), ClaudeConfigDirSource::ModelGateway);
    }
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
/// get the over-pace ratio; [`usage_rank_highest_expected`] returns it
/// directly as its ranking key. A feeder that passes `None` for it drops the
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
    /// Under pace with real headroom ([`under_pace_shortfall`]'s floors):
    /// burn the capacity that expires soonest. Ordered by `expected`
    /// **DESCENDING** — the account whose 7-day window is furthest along
    /// wins, because unused weekly capacity does not roll over past the reset
    /// (use-it-or-lose-it).
    UnderPace { expected: f64 },
    /// No usable pace signal — `usage_delta` and/or `expected` is absent, so
    /// the account cannot be classified under- or over-pace at all. Ordered
    /// by raw `utilization` ASCENDING, which is exactly how this population
    /// has always been ranked.
    Unknown { utilization: f64 },
    /// At or over pace, OR under it by less than the floors allow (demoted,
    /// not excluded): least-over **relative to its own pace**. Ordered by
    /// `ratio = effective_utilization / expected` ASCENDING. A difference is
    /// not comparable across accounts at different points in their windows; a
    /// ratio is.
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

/// One account's full selection rank: the dominating `exhausted` tier, the
/// `low_headroom` tier below it, and its [`PaceRank`]. Compare two of these
/// with [`cmp_rank`] — never field by field, and never by flattening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageRank {
    pub exhausted: bool,
    /// Less than [`HeadroomFloors::min_headroom`] left before the reset
    /// (`1 - effective_utilization`). Such an account ranks after every other
    /// usable account, whatever either's pace tier, but before any exhausted
    /// one — so it is picked only when nothing with real headroom is usable.
    pub low_headroom: bool,
    pub pace: PaceRank,
}

/// Selection rank for an account from the latest snapshot, if a fresh sample
/// exists: a [`UsageRank`] carrying the dominating `exhausted` flag, the
/// `low_headroom` flag, and the account's [`PaceRank`] — the pace tier it sits
/// in AND that tier's own key.
///
/// `charge` is the account's decayed handoff load
/// (`handoff_ledger::HandoffLedger::charge_sum`), added to the probed
/// utilization to give `effective_utilization`; the caller reads it from the
/// ledger it already holds, so this reader never locks the ledger.
///
/// `exhausted` is the dominating tier: an exhausted account (out of tokens /
/// rejected) always sorts after a usable one, no matter how favourable its
/// pace key. Below it, `low_headroom` (`1 - effective_utilization <
/// floors.min_headroom`) sorts after every account with headroom. Below that,
/// the sample is classified into exactly one pace tier:
///
/// | condition | tier | that tier's key |
/// |---|---|---|
/// | under pace past both floors ([`under_pace_shortfall`]'s predicate) | [`PaceRank::UnderPace`] | `expected` **DESCENDING** |
/// | `usage_delta` and/or `expected` absent | [`PaceRank::Unknown`] | `effective_utilization` ascending |
/// | `usage_delta` and `expected` present, either floor failed | [`PaceRank::OverPace`] | `effective_utilization / expected` **ASCENDING** |
///
/// A floor-failing account is DEMOTED to the over-pace tier, not excluded: it
/// drops out of the handoff strategies' candidates but stays reachable through
/// the ranked fallback when no account qualifies.
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
pub fn usage_rank(config_dir: &str, charge: f64, floors: &HeadroomFloors) -> Option<UsageRank> {
    let snap = USAGE_SNAPSHOT.lock().ok()?;
    let map = snap.as_ref()?;
    let sample = map.get(config_dir)?;
    if sample.captured_at.elapsed() > USAGE_SNAPSHOT_TTL {
        return None;
    }
    Some(rank_sample(sample, charge, floors))
}

/// The pure half of [`usage_rank`]: one fresh sample's rank.
fn rank_sample(sample: &UsageSample, charge: f64, floors: &HeadroomFloors) -> UsageRank {
    let effective = effective_utilization(sample, charge);
    let pace = match (sample.usage_delta, sample.expected) {
        // Measured under its own projected pace with real headroom →
        // use-it-or-lose-it tier. The SAME predicate the handoff strategies
        // and `usage_rank_highest_expected` read.
        (Some(_), Some(expected)) if floored_shortfall(sample, charge, floors).is_some() => {
            PaceRank::UnderPace { expected }
        }
        // Measured at-or-over pace, or under it by less than the floors allow
        // (demoted, not excluded) → ranked by overrun relative to its pace.
        (Some(_), Some(expected)) => PaceRank::OverPace {
            ratio: over_pace_ratio(effective, expected),
        },
        // Either half of the pace signal missing → no pace classification is
        // measurable, so it gets its own tier rather than a manufactured one.
        _ => PaceRank::Unknown {
            utilization: effective,
        },
    };
    UsageRank {
        exhausted: sample.exhausted,
        low_headroom: !has_min_headroom(effective, floors),
        pace,
    }
}

/// Probed utilization plus the handoff charge (D2).
fn effective_utilization(sample: &UsageSample, charge: f64) -> f64 {
    sample.utilization + charge.max(0.0)
}

/// `1 - effective_utilization >= floors.min_headroom`.
fn has_min_headroom(effective: f64, floors: &HeadroomFloors) -> bool {
    1.0 - effective + FLOOR_EPSILON >= floors.min_headroom
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
/// Four levels, in order:
/// 1. `exhausted` — `false` before `true`. An out-of-tokens account sorts last
///    no matter how favourable its pace key.
/// 2. `low_headroom` — `false` before `true`. An account with less than
///    [`HeadroomFloors::min_headroom`] left ranks after every other usable
///    account in every tier, but still before every exhausted one.
/// 3. The pace **tier** ([`PaceRank::tier_index`]): under-pace before unknown
///    before over-pace.
/// 4. Only for two accounts in the *same* tier, that tier's own key in that
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
        .then_with(|| a.low_headroom.cmp(&b.low_headroom))
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

/// Selection rank for the `HighestExpectedUsage` mode: `Some(expected_utilization)`
/// only for an account that is BOTH usable (not exhausted) AND currently under
/// its projected weekly pace with real headroom ([`under_pace_shortfall`]).
/// Every other case — exhausted, at-or-over projected pace, under it by less
/// than a floor, or no fresh/computable sample — returns `None`, exactly like
/// [`usage_rank`] returning `None` for a stale/missing sample: the caller
/// (`account_usage::pick_from_highest_expected`) treats `None` as "not a
/// candidate for the primary ranking" and falls through to the same
/// cold-start / cooldown-ordering fallback [`usage_rank`]'s callers use.
///
/// Among the `Some` survivors the caller picks the MAXIMUM
/// `expected_utilization`: of the accounts with headroom, prefer the one
/// furthest along its billing window (soonest to reset), so an account that
/// still has a full week of headroom ahead of it is saved for later rather
/// than spent down first.
pub fn usage_rank_highest_expected(
    config_dir: &str,
    charge: f64,
    floors: &HeadroomFloors,
) -> Option<f64> {
    under_pace_shortfall(config_dir, charge, floors).map(|(expected, _)| expected)
}

/// `Some((expected, shortfall))` for an account that is usable (not exhausted)
/// and under its projected weekly pace WITH REAL HEADROOM, from a fresh sample:
/// `expected` is the expected utilization at this point in the window and
/// `shortfall` is `expected - effective_utilization`. `None` in every case
/// [`usage_rank_highest_expected`] excludes. The input of the
/// `WeightedRandom` and `AlternatingHandoff` selection modes.
///
/// `effective_utilization` is the probed utilization plus `charge`, the
/// account's decayed handoff load, which the caller reads from the ledger it
/// already holds (`handoff_ledger`, "Lock order"). The account qualifies only
/// when BOTH floors hold:
/// - shortfall `>= floors.min_shortfall` ([`MIN_SHORTFALL`]);
/// - headroom `1 - effective_utilization >= floors.min_headroom`
///   ([`MIN_HEADROOM`]).
///
/// This is the ONE under-pace predicate: [`usage_rank`]'s under-pace tier
/// reads it too (through [`floored_shortfall`]), so every mode, the spawn-time
/// picker and the handoff picker agree.
pub fn under_pace_shortfall(
    config_dir: &str,
    charge: f64,
    floors: &HeadroomFloors,
) -> Option<(f64, f64)> {
    let snap = USAGE_SNAPSHOT.lock().ok()?;
    let map = snap.as_ref()?;
    let sample = map.get(config_dir)?;
    if sample.captured_at.elapsed() > USAGE_SNAPSHOT_TTL {
        return None;
    }
    if sample.exhausted {
        return None;
    }
    floored_shortfall(sample, charge, floors)
}

/// The floors of [`under_pace_shortfall`] over one sample, without the
/// freshness and exhaustion checks (which [`usage_rank`] keeps as their own
/// tiers). `shortfall = -usage_delta - charge`: the measured delta stays the
/// authority on where the account sits against its pace, and the charge moves
/// it as if that load had already been burned.
fn floored_shortfall(
    sample: &UsageSample,
    charge: f64,
    floors: &HeadroomFloors,
) -> Option<(f64, f64)> {
    let expected = sample.expected?;
    let delta = sample.usage_delta?;
    let shortfall = -delta - charge.max(0.0);
    if shortfall + FLOOR_EPSILON < floors.min_shortfall {
        return None;
    }
    if !has_min_headroom(effective_utilization(sample, charge), floors) {
        return None;
    }
    Some((expected, shortfall))
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
    if crate::model_gateway::gateway_declared() {
        crate::model_gateway::note_subscription_path_off("rotate_account_on_rate_limit");
        return false;
    }
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
    /// `RESOLVED_CONFIG_DIR`. What IS serialized: every test holding
    /// `pin_account_selection_for_test` (directly or through
    /// `isolated_ambient_with_fleet_pin`) — this one,
    /// `effective_config_dir_follows_the_fleet_mode_unless_pinned` (the one test
    /// that publishes a dir there), the `config_report_cmd` tests that reach
    /// `get_effective_config_dir` through the report, and
    /// `retry::tests::test_retry_succeeds_on_second_attempt` (whose 429 reaches
    /// `rotate_account_on_rate_limit`. On Linux under the fixture/cargo-guard
    /// its roster is empty and rotation writes nothing; elsewhere it may read
    /// the real roster and rotate, and its restore guard puts the value back).
    /// A test touching `RESOLVED_CONFIG_DIR` WITHOUT that pin is not
    /// serialized against them.
    #[test]
    fn effective_config_dir_distinguishes_unconfigured_from_dead_credentials() {
        // The mode is resolved against the process-global fleet cache; pin it
        // to "no fleet opinion" so a concurrently running poller test cannot
        // turn these `Manual` fixtures into an auto mode.
        let _fleet = crate::mcp::fleet_policy_poller::pin_account_selection_for_test(None);
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

    /// A config dir with live credentials: a `.credentials.json` expiring a
    /// day out and carrying no refresh token, so the check is pure (no
    /// refresh is ever requested) and passes on expiry alone.
    fn dir_with_live_credentials() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let expires_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 86_400_000;
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-oauth-test",
                "refreshToken": "",
                "expiresAt": expires_at_ms,
                "scopes": ["user:inference"],
            }
        });
        std::fs::write(dir.path().join(".credentials.json"), body.to_string()).expect("write");
        let path = dir.path().to_string_lossy().to_string();
        (dir, path)
    }

    /// **The half-apply guarantee, pinned at the decision site.** Under a fleet
    /// auto mode an UNPINNED machine whose local mode is `Manual` must take an
    /// auto arm (the pick `pick_best_account` resolved, else the `config_dir`
    /// fallback) and never the `Manual` arm. A PINNED machine keeps `Manual`.
    ///
    /// Held under the fleet pin for the whole test. That serializes it against
    /// every test that also takes `pin_account_selection_for_test` — and ONLY
    /// those: the other tests in this module, the `config_report_cmd` tests
    /// that reach `get_effective_config_dir`, and the retry test whose 429
    /// reaches `rotate_account_on_rate_limit`. `RESOLVED_CONFIG_DIR` is set
    /// explicitly per arm and restored on drop, and the restore guard is
    /// declared AFTER the temp dirs so it drops FIRST: a deleted path is never
    /// left published. The retry test also reaches a writer (the rate-limit
    /// rotation): on Linux under the fixture/cargo-guard its roster is empty
    /// and nothing is written; elsewhere it may read the real roster and
    /// rotate, which is why it too restores on drop — and the shared fleet pin
    /// keeps it from running while this test is mid-assertion.
    #[test]
    fn effective_config_dir_follows_the_fleet_mode_unless_pinned() {
        let _fleet = crate::mcp::fleet_policy_poller::pin_account_selection_for_test(Some(
            AccountSelectionMode::LeastUsage,
        ));
        // Temp dirs FIRST, restore guard AFTER: locals drop in reverse order,
        // so the published pick is restored before its directory is deleted.
        let (_manual_guard, manual_dir) = dir_with_live_credentials();
        let (_picked_guard, picked_dir) = dir_with_live_credentials();
        let _restore = ResolvedConfigDirRestore::capture();

        let unpinned_manual = settings::ClaudeCliSettings {
            account_selection_mode: AccountSelectionMode::Manual,
            account_selection_pinned: false,
            config_dir: Some(manual_dir.clone()),
            ..Default::default()
        };
        let pinned_manual = settings::ClaudeCliSettings {
            account_selection_pinned: true,
            ..unpinned_manual.clone()
        };

        // (a) Unpinned, with a picker result: the PICK wins, not the manual dir.
        set_resolved_config_dir(Some(picked_dir.clone()));
        assert_eq!(
            get_effective_config_dir(&unpinned_manual),
            (
                Some(picked_dir.clone()),
                ClaudeConfigDirSource::LeastUsageResolved
            )
        );
        // (a') Unpinned, no picker result: the auto arm's fallback — still not
        // the `Manual` arm, even though it lands on the same dir.
        set_resolved_config_dir(None);
        assert_eq!(
            get_effective_config_dir(&unpinned_manual),
            (
                Some(manual_dir.clone()),
                ClaudeConfigDirSource::LeastUsageConfigDirFallback
            )
        );

        // (b) Pinned: the fleet is ignored, the `Manual` arm decides — and a
        // picker result is ignored with it.
        set_resolved_config_dir(Some(picked_dir));
        assert_eq!(
            get_effective_config_dir(&pinned_manual),
            (Some(manual_dir), ClaudeConfigDirSource::Manual)
        );
    }

    /// A per-request override is returned VERBATIM and names itself — it is the
    /// one path that skips the credential check here, so a report that showed
    /// it as `manual_config_dir` would be claiming a validation that did not
    /// happen.
    #[test]
    fn effective_config_dir_override_is_verbatim_and_names_itself() {
        let _fleet = crate::mcp::fleet_policy_poller::pin_account_selection_for_test(None);
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
        usage_rank(dir, 0.0, &HeadroomFloors::default())
            .expect("a sample was just recorded")
            .pace
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

    /// REGRESSION — the 2026-10-18 incident (plan
    /// `2026-10-19-usage-limit-handoff-lands-sessions-on-accounts-that-cannot-absorb-them`,
    /// Finding 1): pakqon's probe samples on merytshost, read from the runner
    /// log `qontinui-runner.log.2026-10-18`.
    ///
    /// | time (CEST) | utilization | expected | delta | picked then |
    /// |---|---|---|---|---|
    /// | 20:41 | 0.81 | 0.8136 | −0.004 | pakqon |
    /// | 21:34 | 0.82 | 0.8189 | +0.001 | ben |
    /// | 22:03 | 0.82 | 0.8217 | −0.002 | pakqon |
    /// | 22:29:56 | 0.82 | 0.8243 | −0.004 | pakqon |
    /// | 23:31 | 0.84 | 0.8305 | +0.010 | ben |
    ///
    /// pakqon is under pace at NO row. On the under-pace rows the floor that
    /// excludes it is `MIN_SHORTFALL`: with that floor relaxed the rows
    /// qualify, because pakqon had 16–19 % left, above `MIN_HEADROOM`.
    #[test]
    fn incident_2026_10_18_pakqon_is_never_under_pace_and_min_shortfall_is_why() {
        let rows = [
            ("20_41", 0.81, 0.8136),
            ("21_34", 0.82, 0.8189),
            ("22_03", 0.82, 0.8217),
            ("22_29_56", 0.82, 0.8243),
            ("23_31", 0.84, 0.8305),
        ];
        let no_shortfall_floor = HeadroomFloors {
            min_shortfall: f64::MIN_POSITIVE,
            min_headroom: MIN_HEADROOM,
        };
        for (at, utilization, expected) in rows {
            let dir = format!("/test/config/incident_2026_10_18/pakqon_{at}");
            let delta: f64 = utilization - expected;
            record_account_usage(&[(dir.clone(), utilization, Some(delta), Some(expected), false)]);
            assert_eq!(
                under_pace_shortfall(&dir, 0.0, &HeadroomFloors::default()),
                None,
                "{at}: pakqon must not be under pace"
            );
            assert!(
                matches!(pace_of(&dir), PaceRank::OverPace { .. }),
                "{at}: demoted to the over-pace tier, not excluded"
            );
            assert!(
                !usage_rank(&dir, 0.0, &HeadroomFloors::default())
                    .unwrap()
                    .low_headroom
            );
            if delta < 0.0 {
                assert!(
                    under_pace_shortfall(&dir, 0.0, &no_shortfall_floor).is_some(),
                    "{at}: MIN_SHORTFALL is the floor that excluded it, not MIN_HEADROOM"
                );
            }
        }
    }

    #[test]
    fn floors_are_read_from_settings_and_clamped() {
        let cli = |s: Option<f64>, h: Option<f64>, e: Option<f64>| settings::ClaudeCliSettings {
            selection_min_shortfall: s,
            selection_min_headroom: h,
            handoff_load_estimate: e,
            ..Default::default()
        };
        let none = cli(None, None, None);
        assert_eq!(HeadroomFloors::from_cli(&none), HeadroomFloors::default());
        assert_eq!(handoff_load_estimate_from(&none), HANDOFF_LOAD_ESTIMATE);

        let set = cli(Some(0.05), Some(0.2), Some(0.03));
        assert_eq!(
            HeadroomFloors::from_cli(&set),
            HeadroomFloors {
                min_shortfall: 0.05,
                min_headroom: 0.2
            }
        );
        assert_eq!(handoff_load_estimate_from(&set), 0.03);

        // Neither floor may go below one whole percent.
        let low = cli(Some(0.0), Some(-1.0), Some(-0.5));
        assert_eq!(
            HeadroomFloors::from_cli(&low),
            HeadroomFloors {
                min_shortfall: MIN_FLOOR_SETTING,
                min_headroom: MIN_FLOOR_SETTING
            }
        );
        assert_eq!(
            handoff_load_estimate_from(&low),
            0.0,
            "0 turns the charge off"
        );

        let high = cli(Some(7.0), Some(f64::NAN), Some(f64::INFINITY));
        assert_eq!(
            HeadroomFloors::from_cli(&high),
            HeadroomFloors {
                min_shortfall: 1.0,
                min_headroom: MIN_HEADROOM
            }
        );
        assert_eq!(handoff_load_estimate_from(&high), HANDOFF_LOAD_ESTIMATE);
    }

    #[test]
    fn under_pace_needs_both_floors_and_reads_the_charge() {
        let floors = HeadroomFloors::default();
        // Shortfall 0.05, headroom 0.30: under pace.
        let dir = "/test/config/floors_both";
        record_account_usage(&[(dir.to_string(), 0.70, Some(-0.05), Some(0.75), false)]);
        let (expected, shortfall) = under_pace_shortfall(dir, 0.0, &floors).unwrap();
        assert_eq!(expected, 0.75);
        assert!((shortfall - 0.05).abs() < 1e-12);
        // A charge reduces the shortfall one for one…
        let (_, charged) = under_pace_shortfall(dir, 0.02, &floors).unwrap();
        assert!((charged - 0.03).abs() < 1e-12);
        // …and exactly at the floor still qualifies (decimal, not binary).
        assert!(under_pace_shortfall(dir, 0.03, &floors).is_some());
        assert_eq!(under_pace_shortfall(dir, 0.031, &floors), None);
        assert!(matches!(
            usage_rank(dir, 0.031, &floors).unwrap().pace,
            PaceRank::OverPace { .. }
        ));

        // Plenty of shortfall, too little headroom: not under pace.
        let thin = "/test/config/floors_thin";
        record_account_usage(&[(thin.to_string(), 0.91, Some(-0.08), Some(0.99), false)]);
        assert_eq!(under_pace_shortfall(thin, 0.0, &floors), None);
        let r = usage_rank(thin, 0.0, &floors).unwrap();
        assert!(r.low_headroom);
        assert!(matches!(r.pace, PaceRank::OverPace { .. }));
        // The charge counts against headroom too.
        let edge = "/test/config/floors_headroom_charge";
        record_account_usage(&[(edge.to_string(), 0.85, Some(-0.10), Some(0.95), false)]);
        assert!(!usage_rank(edge, 0.0, &floors).unwrap().low_headroom);
        assert!(usage_rank(edge, 0.06, &floors).unwrap().low_headroom);
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
        record_account_usage(&[(dir.to_string(), 0.55, Some(-0.35), Some(0.90), true)]);
        assert_eq!(
            usage_rank(dir, 0.0, &HeadroomFloors::default()),
            Some(UsageRank {
                exhausted: true,
                low_headroom: false,
                pace: PaceRank::UnderPace { expected: 0.90 },
            })
        );
    }

    #[test]
    fn usage_rank_none_when_unrecorded() {
        assert_eq!(
            usage_rank("/test/config/rank_never", 0.0, &HeadroomFloors::default()),
            None
        );
    }

    // --- cmp_rank: the one shared comparator --------------------------------

    fn rank(exhausted: bool, pace: PaceRank) -> UsageRank {
        UsageRank {
            exhausted,
            low_headroom: false,
            pace,
        }
    }

    fn thin_rank(exhausted: bool, pace: PaceRank) -> UsageRank {
        UsageRank {
            exhausted,
            low_headroom: true,
            pace,
        }
    }

    #[test]
    fn cmp_rank_low_headroom_sorts_after_every_usable_tier_but_before_exhausted() {
        // A thin account with the best possible under-pace key still sorts
        // after the WORST member of every usable tier…
        let thin_under = thin_rank(false, PaceRank::UnderPace { expected: 0.99 });
        let worst_under = rank(false, PaceRank::UnderPace { expected: 0.0 });
        let worst_unknown = rank(false, PaceRank::Unknown { utilization: 0.89 });
        let worst_over = rank(false, PaceRank::OverPace { ratio: 50.0 });
        for usable in [worst_under, worst_unknown, worst_over] {
            assert_eq!(cmp_rank(&usable, &thin_under), Ordering::Less);
            assert_eq!(cmp_rank(&thin_under, &usable), Ordering::Greater);
        }
        // …and before every exhausted one, thin or not.
        for dead in [
            rank(true, PaceRank::UnderPace { expected: 0.99 }),
            thin_rank(true, PaceRank::UnderPace { expected: 0.99 }),
        ] {
            assert_eq!(cmp_rank(&thin_under, &dead), Ordering::Less);
        }
        // Between two thin accounts the tier and its key still decide.
        let thin_over = thin_rank(false, PaceRank::OverPace { ratio: 1.1 });
        assert_eq!(cmp_rank(&thin_under, &thin_over), Ordering::Less);
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
    fn usage_rank_highest_expected_returns_expected_when_under_pace() {
        let dir = "/test/config/hexp_under_pace";
        // utilization 0.30, expected 0.50 → delta -0.20 (under pace).
        record_account_usage(&[(dir.to_string(), 0.30, Some(-0.20), Some(0.50), false)]);
        assert_eq!(
            usage_rank_highest_expected(dir, 0.0, &HeadroomFloors::default()),
            Some(0.50)
        );
    }

    #[test]
    fn usage_rank_highest_expected_excludes_at_or_over_pace() {
        let dir_over = "/test/config/hexp_over_pace";
        let dir_at = "/test/config/hexp_at_pace";
        // utilization 0.60, expected 0.50 → delta +0.10 (over pace) → excluded.
        record_account_usage(&[(dir_over.to_string(), 0.60, Some(0.10), Some(0.50), false)]);
        assert_eq!(
            usage_rank_highest_expected(dir_over, 0.0, &HeadroomFloors::default()),
            None
        );
        // utilization exactly at expected pace (delta 0.0) → excluded too.
        record_account_usage(&[(dir_at.to_string(), 0.50, Some(0.0), Some(0.50), false)]);
        assert_eq!(
            usage_rank_highest_expected(dir_at, 0.0, &HeadroomFloors::default()),
            None
        );
    }

    #[test]
    fn usage_rank_highest_expected_excludes_exhausted() {
        let dir = "/test/config/hexp_exhausted";
        // Under pace on paper, but exhausted → still excluded.
        record_account_usage(&[(dir.to_string(), 1.0, Some(-0.05), Some(1.05), true)]);
        assert_eq!(
            usage_rank_highest_expected(dir, 0.0, &HeadroomFloors::default()),
            None
        );
    }

    #[test]
    fn usage_rank_highest_expected_excludes_unknown_expected_or_delta() {
        let dir = "/test/config/hexp_unknown";
        // No usage_delta / expected_utilization (probe couldn't compute it).
        record_account_usage(&[(dir.to_string(), 0.30, None, None, false)]);
        assert_eq!(
            usage_rank_highest_expected(dir, 0.0, &HeadroomFloors::default()),
            None
        );
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
