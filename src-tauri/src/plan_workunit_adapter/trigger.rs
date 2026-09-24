//! Wire-up + periodic trigger (Phase 3).
//!
//! Mounts the adapter as a periodic **reconcile scan** of the operator's
//! `plans/` directory, pushing each plan's parsed work-unit to coord via
//! [`super::push`]. The periodic-scan trigger (over a filesystem watch or an
//! on-edit hook) is deliberate: it is robust across runner downtime and closed
//! sessions — an edit made while the runner was down is picked up on the next
//! tick — and the edge-triggered push ([`super::push::decide_push`]) makes a
//! re-scan of an unchanged corpus free of phantom transitions. It mirrors the
//! model coord's own `plan_ingest_worker` used (~60s tick).
//!
//! ## Metrics
//!
//! The runner has no Prometheus surface, so observability is process-local
//! atomic counters named to mirror coord's `coord_plan_ingest_*` so the
//! operator reads the same signals: scanned / transitions / cycles /
//! conflicts. [`adapter_metrics`] exposes the shared instance and
//! [`AdapterMetrics::snapshot`] reads it.
//!
//! ## Opt-in
//!
//! The markdown-plan tier is armed by exactly one thing: the runner's
//! `PathSettings::plans_dir` setting — the **Paths** section of the settings
//! UI, or `paths.plans_dir` in `settings.json`. There is **no environment
//! override**: the one that used to exist was a backward-compatibility shim
//! that silently outranked the setting; the binary's `plans_dir_migration`
//! persists its value into the setting once at boot, and the env read itself
//! is gone. The markdown-plan carrier is the optional top coordination tier,
//! so a runner with nothing configured no-ops (it never scans, never pushes) —
//! claims/intent and coord-native work-units are unaffected.
//!
//! [`spawn_if_configured`] gates only on a resolvable coord base. The loop it
//! spawns re-reads the path settings **every tick** through a [`PathReader`]
//! closure, so a directory configured, changed or cleared while the runner is
//! running takes effect within one interval and never needs a restart (which
//! fleet policy forbids) — the same per-cycle posture as the `plan_capture`
//! dial ([`CaptureGate`]). The settings arrive through a closure rather than
//! being read here because this module lives in the lib crate and the settings
//! store lives in the runner binary's module tree; the binary supplies the
//! reader. [`resolve_plans_dir`] owns the resolution so every surface that
//! needs the active plans dir — the adapter here, the session-env injection
//! and the plan-library read door in the binary — resolves it identically.

use super::parser::{parse_work_unit, slug_from_filename, ParsedWorkUnit, PlanConvention};
use super::push::{
    push_archive_metadata, push_work_unit, push_work_unit_with_remote,
    push_work_unit_with_status_write, PushOutcomeKind, SetDepsOutcome, StatusWrite, WorkUnitSink,
};
use super::ref_scan::CycleRefPin;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Process-local adapter metrics, mirroring coord's `coord_plan_ingest_*`.
#[derive(Debug, Default)]
pub struct AdapterMetrics {
    /// Plans scanned in the last cycle (gauge-like; overwritten each cycle).
    pub scanned: AtomicU64,
    /// Total status transitions emitted (counter).
    pub transitions_total: AtomicU64,
    /// Total reconcile cycles run (counter).
    pub cycles_total: AtomicU64,
    /// Total remote-divergence conflicts surfaced (counter) — coord's
    /// `coord_plan_ingest_reverts_total` analogue.
    pub conflicts_total: AtomicU64,
    /// Total per-unit push errors (counter).
    pub errors_total: AtomicU64,
    /// Total transitions SUPPRESSED by the graduation-bootstrap deferral — a
    /// real (non-adapter) agent owns the unit, so the markdown proxy emitted
    /// nothing (counter). Counted separately from `transitions_total` because a
    /// deferral is a write that did NOT happen; folding it into the refresh
    /// count (the shipped behaviour) made the adapter's most consequential
    /// decision invisible in the cycle log.
    pub deferrals_total: AtomicU64,
    /// Total dependency-edge replace-sets applied to coord's edge table
    /// (`POST /coord/work-units/:slug/deps` 2xx) (counter).
    pub deps_set_total: AtomicU64,
    /// Total dep-set calls skipped because coord returned 503 (edge table not
    /// yet migrated — benign, JSONB fallback covers it) (counter).
    pub deps_skipped_unmigrated_total: AtomicU64,
    /// Total dep-set calls that hard-errored (counter). Best-effort: an error
    /// here does NOT fail the reconcile — the unit's upsert already succeeded
    /// and edges are additive.
    pub deps_errors_total: AtomicU64,
    /// Total `metadata.archive_path` stamps written by the archive scan
    /// (counter). Metadata-only — never a status transition (D4).
    pub archive_stamped_total: AtomicU64,
    /// Slugs coord refused with a `403` **naming no clearing condition**, and
    /// this process has therefore retired PRINCIPAL-wide (counter, monotonic —
    /// one increment per refused slug, not per cycle). A non-zero value here
    /// with a flat `errors_total` is the healthy shape: the adapter noticed a
    /// permission verdict and stopped re-asking.
    ///
    /// **This counter is one operator action only: *fix the principal's
    /// permission*.** It deliberately does NOT include the
    /// `terminality: permanent` retirements — those say *fix the plan file's
    /// status stamp*, which shares nothing with a missing grant, and are
    /// counted in [`AdapterMetrics::retired_permanent_total`].
    pub forbidden_total: AtomicU64,
    /// `(slug, status)` pairs coord answered `terminality: permanent` for, and
    /// this process has therefore retired for that PAIR (counter, monotonic —
    /// one increment per retired pair, not per cycle).
    ///
    /// The operator action is *edit the plan file's status stamp* — typically a
    /// coord-DERIVED word (`shipped`/`ready`) that coord computes and nobody
    /// may set. It self-clears: the next cycle whose parsed status differs is
    /// pushed normally, with no restart.
    pub retired_permanent_total: AtomicU64,
    /// Scan roots in effect after the loop's last path resolution (gauge):
    /// the distinct configured directories among `plans_dir`,
    /// `plans_archive_dir` and `prompts_dir`. `0` while the tier is off.
    pub scan_roots: AtomicU64,
    /// Times the loop (re)built its resolved path set (counter): `1` after the
    /// first tick, `+1` for every settings change it picked up. `0` means the
    /// loop has not ticked yet — or was never spawned — so the two gauges
    /// beside it are not yet answers.
    pub path_resolutions_total: AtomicU64,
    /// The active plans dir the loop resolved on its last tick (gauge);
    /// `None` while the tier is off or before the first tick.
    pub active_plans_dir: std::sync::Mutex<Option<String>>,
    /// The last cycle's measurement of the scanned working tree against the
    /// ref it is supposed to represent (gauge) — see [`ScanDivergence`].
    ///
    /// Written on EVERY tick including the idle one, so `None` here means
    /// exactly one thing: the loop has not ticked yet (or was never spawned).
    /// It never means "no divergence" — a tier-off machine records
    /// [`ScanDivergenceState::NotScanning`] rather than leaving this empty,
    /// because an empty slot and a healthy scan reading the same is the defect
    /// the detector exists to end.
    pub scan_divergence: std::sync::Mutex<Option<ScanDivergence>>,
    /// Slugs whose dep-edge `set_deps` call coord refused with a `403`, and
    /// whose edge push this process has therefore retired (counter, monotonic
    /// — one increment per refused slug, not per cycle). Tracked separately
    /// from `forbidden_total`: the unit's own upsert/transition route can be
    /// permitted while the edge-table route is not (coord evaluates them as
    /// separate authorization checks), so a deps-only refusal must not retire
    /// the whole unit — see the `forbidden_deps` set in [`reconcile_once`].
    pub deps_forbidden_total: AtomicU64,
    /// Total units whose `last_applied` was seeded from coord's current status
    /// because this process had no memory of them (counter). Non-zero almost
    /// exclusively on the first cycle after a runner start — but that is true
    /// only while the BULK prime is working. A DEFERRED unit deliberately never
    /// enters `last_applied` (see [`reconcile_once`] for why the memory must
    /// stay untouched), so in the per-slug fallback regime every persistently
    /// deferred unit re-seeds every cycle. The bulk prime inserts directly into
    /// `last_applied`, which is what keeps those units covered.
    pub seeded_total: AtomicU64,
    /// Total units SKIPPED because their seed read failed (counter). A unit
    /// counted here was not pushed at all this cycle — see [`reconcile_once`]
    /// for why an unreadable remote status must abstain rather than overwrite.
    pub seed_errors_total: AtomicU64,
    /// Cycles whose coord work-unit reads and writes were WITHHELD because
    /// this device is bound to more than one tenant and the plans' owning
    /// tenant is not resolvable from the adapter (counter) — see
    /// [`work_unit_write_posture`]. Non-zero on a multi-bound device is the
    /// designed state, not a fault: it is what stops the adapter filing a
    /// fleet's plans under whichever tenant the default credential names.
    pub work_unit_writes_withheld_total: AtomicU64,
    /// The subset of `work_unit_writes_withheld_total` withheld because the
    /// binding set was UNKNOWN rather than multi-bound (counter). Unlike the
    /// multi-bound case this is a FAULT to look at: no fresh coord binding
    /// record — a secondary/temp runner (it runs no register heartbeat), or a
    /// primary whose heartbeat is not succeeding.
    pub work_unit_writes_withheld_unknown_total: AtomicU64,
}

/// A point-in-time read of [`AdapterMetrics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub scanned: u64,
    pub transitions_total: u64,
    pub cycles_total: u64,
    pub conflicts_total: u64,
    pub errors_total: u64,
    pub deferrals_total: u64,
    pub deps_set_total: u64,
    pub deps_skipped_unmigrated_total: u64,
    pub deps_errors_total: u64,
    pub archive_stamped_total: u64,
    pub forbidden_total: u64,
    pub retired_permanent_total: u64,
    pub scan_roots: u64,
    pub path_resolutions_total: u64,
    pub active_plans_dir: Option<String>,
    /// The loop's last scan-divergence reading; `None` only before the first
    /// tick — see [`AdapterMetrics::scan_divergence`].
    pub scan_divergence: Option<ScanDivergence>,
    pub deps_forbidden_total: u64,
    pub seeded_total: u64,
    pub seed_errors_total: u64,
    pub work_unit_writes_withheld_total: u64,
    pub work_unit_writes_withheld_unknown_total: u64,
}

impl AdapterMetrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            scanned: self.scanned.load(Ordering::Relaxed),
            transitions_total: self.transitions_total.load(Ordering::Relaxed),
            cycles_total: self.cycles_total.load(Ordering::Relaxed),
            conflicts_total: self.conflicts_total.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
            deferrals_total: self.deferrals_total.load(Ordering::Relaxed),
            deps_set_total: self.deps_set_total.load(Ordering::Relaxed),
            deps_skipped_unmigrated_total: self
                .deps_skipped_unmigrated_total
                .load(Ordering::Relaxed),
            deps_errors_total: self.deps_errors_total.load(Ordering::Relaxed),
            archive_stamped_total: self.archive_stamped_total.load(Ordering::Relaxed),
            forbidden_total: self.forbidden_total.load(Ordering::Relaxed),
            retired_permanent_total: self.retired_permanent_total.load(Ordering::Relaxed),
            scan_roots: self.scan_roots.load(Ordering::Relaxed),
            path_resolutions_total: self.path_resolutions_total.load(Ordering::Relaxed),
            active_plans_dir: self
                .active_plans_dir
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            scan_divergence: self
                .scan_divergence
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            deps_forbidden_total: self.deps_forbidden_total.load(Ordering::Relaxed),
            seeded_total: self.seeded_total.load(Ordering::Relaxed),
            seed_errors_total: self.seed_errors_total.load(Ordering::Relaxed),
            work_unit_writes_withheld_total: self
                .work_unit_writes_withheld_total
                .load(Ordering::Relaxed),
            work_unit_writes_withheld_unknown_total: self
                .work_unit_writes_withheld_unknown_total
                .load(Ordering::Relaxed),
        }
    }
}

/// The shared process-wide adapter metrics.
pub fn adapter_metrics() -> &'static AdapterMetrics {
    static METRICS: OnceLock<AdapterMetrics> = OnceLock::new();
    METRICS.get_or_init(AdapterMetrics::default)
}

// ---------------------------------------------------------------------------
// Scan-source divergence — the DETECTOR half of plan
// `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`.
//
// [`read_plan_dir`] scans a WORKING TREE. Nothing about that tree is pinned:
// the directory the operator points `paths.plans_dir` at is an ordinary
// checkout, and a checkout can sit parked on a peer's branch for weeks.
// Measured on the operator box 2026-09-10, the configured dir was 2153 commits
// behind and 11 ahead of its own default branch, and the plan-library rows
// sourced from it agreed with that parked tree 96% of the time and with
// `origin/main` only 76%.
//
// The reason that went unnoticed for months is not that the number was bad —
// it is that NOBODY EVER COMPUTED IT. Every downstream surface (the work-unit
// rows, the plan library, this module's own cycle log) reports the scan as
// having succeeded, because by its own lights it did. This block computes the
// missing number once per cycle and publishes it beside the other gauges.
//
// It measures and it says. It does NOT change the scan source and it does NOT
// fetch: the reading is explicitly "as of this clone's last fetch", which is
// what makes landing the detector a behaviour-free change. Moving the scan onto
// a ref is Phase 2, deferred while four open PRs rewrite this seam.
// ---------------------------------------------------------------------------

/// Why a [`ScanDivergence`] reading says what it says.
///
/// Four states, deliberately not two. The whole failure this type exists to
/// end is that "in step with the ref", "nothing is being scanned at all" and
/// "the measurement did not work" were one indistinguishable silence — the
/// `silent-empty-is-unknown` shape. So each gets its own name, and no arm is
/// allowed to render as `0 behind / 0 ahead` unless the zeros were measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanDivergenceState {
    /// No `paths.plans_dir` is configured: the tier is OFF and NOTHING is
    /// scanned. Recorded — not skipped — precisely because an unrecorded idle
    /// cycle is indistinguishable from a healthy one to every reader.
    NotScanning,
    /// A plans dir is configured but is not inside a git work tree. A
    /// supported configuration (an operator may author into a plain
    /// directory), so it is not an error — but there is no ref to compare
    /// against, which is a different statement from "it matches the ref".
    NotAGitWorkTree,
    /// Measured against [`ScanDivergence::default_ref`]: `behind` and `ahead`
    /// are real counts, and zeros here mean zero.
    ///
    /// **Scoped to commits.** The comparison is `HEAD` against the ref, so
    /// `0/0` says the checked-out COMMIT is in step — it does NOT say the
    /// bytes being scanned match the ref. [`read_plan_dir`] reads the working
    /// tree, and a tree exactly on the default branch with uncommitted or
    /// untracked plan files still publishes content no ref carries. Comparing
    /// the tree itself belongs to the phase that moves the scan onto a ref.
    Measured,
    /// A plans dir is configured, it IS a work tree, and the measurement
    /// itself failed — no `origin/HEAD`, a git that would not run, an
    /// unreadable ref. UNKNOWN, never zero, and [`ScanDivergence::detail`]
    /// names which probe failed.
    Unknown,
}

impl ScanDivergenceState {
    /// The stable snake_case tag every read surface renders.
    ///
    /// Spelled here rather than derived through serde so this module stays
    /// serde-free: the projection that crosses the Tauri boundary lives in
    /// `commands::path_settings`, and this string is the contract between the
    /// two.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotScanning => "not_scanning",
            Self::NotAGitWorkTree => "not_a_git_work_tree",
            Self::Measured => "measured",
            Self::Unknown => "unknown",
        }
    }
}

/// One cycle's measurement of the scanned working tree against the ref it is
/// supposed to represent.
///
/// Every field beyond `state` is `Option` because each is only meaningful on
/// some arms — and an absent field is UNKNOWN, never a defaulted zero or an
/// empty string. `behind`/`ahead` are populated on
/// [`ScanDivergenceState::Measured`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanDivergence {
    pub state: ScanDivergenceState,
    /// The directory actually scanned, as configured.
    pub plans_dir: Option<String>,
    /// The git work-tree root containing `plans_dir`. Not the same thing —
    /// the plans dir is usually a subdirectory of the checkout.
    pub repo_root: Option<String>,
    /// The repo's OWN default branch, resolved at scan time from
    /// `origin/HEAD` (e.g. `origin/main`) — never a hardcoded guess. A clone
    /// whose default branch cannot be resolved is `Unknown`, because guessing
    /// `origin/main` on a repo whose default is something else would produce a
    /// confidently wrong divergence number, which is worse than none.
    pub default_ref: Option<String>,
    /// What `default_ref` points at in this clone, **as of its last fetch**.
    /// Phase 1 never fetches, so a long-unfetched clone reports a stale ref
    /// and a small divergence honestly rather than a fresh one it did not earn.
    /// How long ago that fetch was is [`Self::ref_age_secs`].
    pub ref_sha: Option<String>,
    /// What the scanned work tree's `HEAD` points at.
    pub head_sha: Option<String>,
    /// Commits on `default_ref` that the scanned tree's HEAD does NOT have —
    /// how far the scan source is stale. This is the number that read 2153.
    pub behind: Option<u64>,
    /// Commits on the scanned tree's HEAD that `default_ref` does NOT have —
    /// content the scan is publishing that no ref carries. This is the number
    /// that read 11.
    pub ahead: Option<u64>,
    /// Seconds since `default_ref` was last known to be refreshed in this
    /// clone — the fresher of a `FETCH_HEAD` that names the default branch at
    /// the sha the ref now holds, and the ref's newest reflog entry (see
    /// [`GitRefReader::ref_refresh_stamps`]).
    ///
    /// This is what turns `behind` from a number into a claim. `behind` counts
    /// against the ref AS THIS CLONE LAST SAW IT, so a clone unfetched for a
    /// week reads a confidently low `behind` — the counts are then LOWER
    /// BOUNDS, and [`Self::counts_are_floors`] says so. `None` is UNKNOWN (no
    /// readable source, or the probe failed) and is treated as a floor too,
    /// never as fresh. Read on the `Measured` arm only; `None` elsewhere.
    pub ref_age_secs: Option<u64>,
    /// One line naming why the state is `Unknown` or `NotAGitWorkTree`. Never
    /// empty on those two states: an unexplained UNKNOWN is the same dead end
    /// as the silence this type replaces. On `Measured` it is `None` unless
    /// `ref_age_secs` is absent, in which case it names why the age is unknown.
    pub detail: Option<String>,
    /// The plan-library `source_repo` key of `plans_dir` — the same
    /// `<repo>/<dir>` identity the artifacts this dir produces carry
    /// (`body_push::derive_source_repo`). Resolved beside the git probes, on
    /// the blocking pool, because it walks the filesystem for a `.git`; the
    /// scan-root report only copies it. `None` when nothing is scanned, or
    /// when the reading was not produced by the tick's measurement.
    pub source_repo: Option<String>,
    /// When this reading was taken, as unix seconds — the clock the tick read
    /// for the measurement itself, NOT the time anything later forwarded it.
    /// `None` only for a reading no tick stamped (a hand-built one).
    pub observed_at_unix: Option<i64>,
}

/// How recently `default_ref` must have been refreshed for a `Measured`
/// reading's counts to be taken as current rather than as lower bounds.
///
/// Six hours: on a runner box the worktree census refreshes each canonical
/// repo's trunk every 300 s (`agent_worktree::census::fetch_trunk`), so a
/// healthy ref is minutes old and six hours of silence means the refresh
/// itself is not happening. It is deliberately not tighter — a laptop that
/// slept over lunch is not a fault — and not looser, because at the measured
/// ~64 commits/day a six-hour-old ref can already hide a dozen plans.
pub const SCAN_REF_FRESH_WITHIN: Duration = Duration::from_secs(6 * 60 * 60);

impl ScanDivergence {
    /// All-`None` reading in `state`. Private: every public constructor below
    /// fills in whatever that state is obliged to carry.
    fn blank(state: ScanDivergenceState) -> Self {
        Self {
            state,
            plans_dir: None,
            repo_root: None,
            default_ref: None,
            ref_sha: None,
            head_sha: None,
            behind: None,
            ahead: None,
            ref_age_secs: None,
            detail: None,
            source_repo: None,
            observed_at_unix: None,
        }
    }

    /// This reading, stamped as taken at `unix` seconds — see
    /// [`Self::observed_at_unix`].
    pub fn observed_at(self, unix: i64) -> Self {
        Self {
            observed_at_unix: Some(unix),
            ..self
        }
    }

    /// The tier is off — no plans dir is configured, so nothing is scanned.
    /// Recorded on every idle cycle.
    pub fn not_scanning() -> Self {
        Self::blank(ScanDivergenceState::NotScanning)
    }

    /// UNKNOWN with a named reason. Used for the failures that happen OUTSIDE
    /// [`measure_scan_divergence`] — a probe task that could not be joined —
    /// so those never degrade into silence either.
    pub fn unknown(plans_dir: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            plans_dir,
            detail: Some(detail.into()),
            ..Self::blank(ScanDivergenceState::Unknown)
        }
    }

    /// `true` when the scan source measurably differs from the ref in EITHER
    /// direction — the condition the whole plan exists to surface.
    ///
    /// Ahead counts, not just behind. A checkout 0 behind and 11 ahead is
    /// publishing eleven commits' worth of plans that exist on no ref at all,
    /// which is the same class of invisible authority as a stale one; treating
    /// only `behind` as interesting would have let exactly that arm of the
    /// measured defect log at INFO.
    pub fn is_stale(&self) -> bool {
        self.state == ScanDivergenceState::Measured
            && (self.behind.unwrap_or(0) > 0 || self.ahead.unwrap_or(0) > 0)
    }

    /// Whether the ref the counts were taken against is known to be current
    /// — refreshed within [`SCAN_REF_FRESH_WITHIN`].
    ///
    /// `None` when there is nothing to judge: the reading is not `Measured`
    /// (no counts exist), or the ref's age is unknown. Deliberately three-way —
    /// an unknown age is not a stale one, but neither is it a fresh one, and
    /// [`Self::counts_are_floors`] is where the two unknowns meet.
    pub fn ref_is_fresh(&self) -> Option<bool> {
        if self.state != ScanDivergenceState::Measured {
            return None;
        }
        self.ref_age_secs
            .map(|age| age <= SCAN_REF_FRESH_WITHIN.as_secs())
    }

    /// `true` when the `Measured` counts are LOWER BOUNDS rather than current
    /// numbers: the ref is older than [`SCAN_REF_FRESH_WITHIN`] or of unknown
    /// age.
    ///
    /// The floor rule. `behind` compares HEAD against the ref as this clone
    /// last saw it, so every commit the remote gained since that refresh is
    /// missing from the count — the true divergence is at least the reported
    /// one. That makes a floor of `0/0` the dangerous reading: it LOOKS like
    /// agreement and proves nothing, so every read surface (the log line, the
    /// IPC view, the published scan-root row) carries this flag beside the
    /// counts. Only an age PROVEN fresh lifts it; an unknown age never does.
    ///
    /// Strictly it is `behind` that is the floor: the remote gaining commits
    /// can only raise it. `ahead` moves the other way — commits of HEAD's that
    /// have since landed on the remote stop counting once the ref catches up —
    /// so a stale `ahead` may OVERSTATE. Either way the pair is not current,
    /// which is the one thing the flag asserts.
    ///
    /// `false` on every non-`Measured` state, which has no counts to qualify.
    pub fn counts_are_floors(&self) -> bool {
        self.state == ScanDivergenceState::Measured && self.ref_is_fresh() != Some(true)
    }

    /// Whether `other` says the same thing as `self` — every field equal
    /// except the two that move with the clock: the raw `ref_age_secs`, which
    /// is compared only through its freshness verdict ([`Self::ref_is_fresh`]),
    /// and `observed_at_unix`, which is when, not what.
    ///
    /// Both grow every tick, so plain equality would make every reading
    /// "new": the edge-triggered log would fire every minute and the
    /// scan-root report would post every cycle. What a reader acts on is
    /// whether the counts are current or floors, so a crossing of the
    /// freshness window IS a change and a clock advancing inside it is not.
    /// (`detail` is compared verbatim, which is why every probe failure it
    /// quotes is worded without per-run noise such as a pid — see
    /// [`ProcessGit::describe`].)
    pub fn is_same_reading(&self, other: &ScanDivergence) -> bool {
        let strip = |d: &ScanDivergence| ScanDivergence {
            ref_age_secs: None,
            observed_at_unix: None,
            ..d.clone()
        };
        strip(self) == strip(other) && self.ref_is_fresh() == other.ref_is_fresh()
    }
}

/// The four git reads [`measure_scan_divergence`] needs, behind a trait so the
/// measurement is a pure function of its answers.
///
/// Injected rather than called directly because the interesting cases — no
/// `origin/HEAD`, a non-repo directory, a tree 2153 behind — are miserable to
/// build as real repos in a unit test and trivial to state as canned answers.
/// [`ProcessGit`] is the one production implementation.
pub trait GitRefReader: Send + Sync {
    /// The git work-tree root containing `dir`.
    ///
    /// Three outcomes, not two, and the split is the point: `Ok(Some(root))`
    /// located it, `Ok(None)` established that `dir` is definitively NOT
    /// inside a work tree (an ANSWER — `NotAGitWorkTree`), and `Err` means the
    /// question could not be ASKED at all (no `git` on PATH, a plans dir that
    /// does not exist, a probe that timed out on a stalled mount). Folding
    /// that third case into `Ok(None)` would report a broken measurement as
    /// the one benign state a reader is invited to shrug at — the exact
    /// conflation this type exists to end.
    fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String>;
    /// The repo's default branch as a remote-tracking ref name, e.g.
    /// `origin/main`. `Err` when it cannot be established — the caller turns
    /// that into `Unknown`, never into a default.
    fn default_ref(&self, repo_root: &Path) -> Result<String, String>;
    /// Resolve one rev to a full object id.
    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<String, String>;
    /// `(behind, ahead)` for `head` measured against `reference`: how many
    /// commits `reference` has that `head` lacks, then the mirror. The tuple
    /// order matches `git rev-list --left-right --count <reference>...<head>`
    /// so the wire and the type cannot drift — see [`parse_left_right_count`].
    fn count_behind_ahead(
        &self,
        repo_root: &Path,
        reference: &str,
        head: &str,
    ) -> Result<(u64, u64), String>;
    /// Every record of when `default_ref` (e.g. `origin/main`, currently at
    /// `ref_sha`) was refreshed in this clone — one answer PER SOURCE, as unix
    /// seconds. [`combine_refresh_stamps`] turns them into one verdict.
    ///
    /// Several sources, because each misses a case another catches: a fetch
    /// that finds the branch unchanged writes no reflog entry but does rewrite
    /// `FETCH_HEAD`, while a push updates the tracking ref (and its reflog)
    /// without any fetch at all.
    ///
    /// - `FETCH_HEAD`'s mtime — counted ONLY when it carries a line naming the
    ///   default branch (`branch '<name>' of …`) at exactly `ref_sha`. A fetch
    ///   of some other branch refreshes nothing this reading compares against,
    ///   and a `FETCH_HEAD` whose sha disagrees with the tracking ref (a
    ///   rejected non-fast-forward, a fetch by URL that updates no tracking
    ///   ref) proves nothing about it either. `FETCH_HEAD` is per-worktree,
    ///   so a checkout has up to two (its own and the common dir's).
    /// - The ENTRY time of the newest reflog record for
    ///   `refs/remotes/<default_ref>` — when the ref last moved. Not the tip
    ///   commit's committer time, which says when someone authored a commit,
    ///   not when this clone learned of it.
    ///
    /// Per source: `Ok(Some(t))` a record, `Ok(None)` no such record (an
    /// ANSWER), `Err` that source's probe failed. They are returned separately
    /// — never pre-combined — so one source's failure cannot discard another
    /// source's answer, and so a future-dated record can be dropped BEFORE the
    /// fresher is chosen rather than winning the comparison and then being
    /// thrown out along with the trustworthy ones.
    fn ref_refresh_stamps(
        &self,
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Vec<Result<Option<i64>, String>>;

    // ---- Phase 2 of `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`
    // The three reads that let the scan take its bytes from a REF instead of
    // the checked-out tree. Each returns `Err` for "could not ask", and the
    // caller turns that into a cycle that PUBLISHES NOTHING rather than one
    // that publishes a tree — a failed fetch must leave the previous corpus
    // standing [policy: `unknown-must-not-render-as-a-default`].

    /// Refresh `default_ref` from its remote before the scan reads it.
    ///
    /// Reading a ref is only ever as fresh as the last fetch, so the scan owns
    /// the fetch rather than inheriting whatever some other process last did.
    /// `Err` is NOT "scan the tree instead": it is this cycle declining to
    /// publish.
    fn fetch_default(&self, repo_root: &Path, default_ref: &str) -> Result<(), String>;

    /// The blob entries of `<ref>:<rel_dir>`, **depth 1 only**.
    ///
    /// Non-recursive on purpose, matching [`read_plan_dir`]'s documented flat
    /// contract and coord's `walk_root`. A recursive walk here would silently
    /// add every subdirectory plan to the corpus as a side effect of a
    /// scan-SOURCE change — two behaviour changes in one phase, and the wider
    /// one unannounced. If those plans should be scanned that is its own
    /// decision with its own blast radius.
    ///
    /// Trees and submodule links are skipped, not errors: a `plans/artifacts/`
    /// subdirectory is an ordinary, expected entry that this walk does not
    /// descend into.
    fn list_ref_dir(
        &self,
        repo_root: &Path,
        ref_name: &str,
        rel_dir: &str,
    ) -> Result<Vec<RefDirEntry>, String>;

    /// Read many blobs by object id in ONE `git` invocation.
    ///
    /// One process, not one per file. The scan's own call site records the
    /// active dir at ~1,100 files and the first cycle at five minutes; a
    /// `git show` per entry would add that many spawns to a loop that already
    /// dilates a single-worker runtime's time driver.
    ///
    /// Returns one entry per requested id, in the order requested, so a
    /// caller can zip it against [`Self::list_ref_dir`]'s names without a
    /// second lookup. A blob that cannot be read is `Err` in its own slot —
    /// one unreadable file does not discard the other 1,099.
    fn read_blobs(&self, repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>>;
}

/// One depth-1 blob in a ref's directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefDirEntry {
    /// The entry's own name, with no directory part — `2026-09-10-foo.md`.
    pub name: String,
    /// The blob's object id, which is what [`GitRefReader::read_blobs`] reads.
    /// Carried rather than re-deriving `<ref>:<dir>/<name>` per file so the
    /// batch read needs no second path round-trip.
    pub id: String,
}

/// What the refresh records say, taken together — see
/// [`combine_refresh_stamps`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshVerdict {
    /// The ref was last known refreshed at this unix time — the freshest
    /// TRUSTWORTHY record.
    At(i64),
    /// No source records a refresh at all.
    NoRecord,
    /// The only records there are sit more than
    /// [`SCAN_REF_FUTURE_TOLERANCE_SECS`] in the future, so none can be
    /// trusted.
    OnlyFutureDated,
    /// No trustworthy record, and at least one probe failed.
    ProbeFailed(String),
}

/// Combine every source's refresh record into one verdict.
///
/// 1. Records more than [`SCAN_REF_FUTURE_TOLERANCE_SECS`] ahead of `now_unix`
///    are DROPPED first. They prove nothing (a clock corrected backwards, a
///    restored mtime) — and taking the max before dropping them would let one
///    such record hide a trustworthy fresh one.
/// 2. The freshest remaining record wins. A failed probe beside it is ignored:
///    it could only have been fresher, so the result is at worst an
///    overstated age — a floor, never a false "fresh".
/// 3. With no trustworthy record: a future-dated one is named first (its
///    detail is stable), then a probe failure, then "no record".
pub fn combine_refresh_stamps(
    stamps: Vec<Result<Option<i64>, String>>,
    now_unix: i64,
) -> RefreshVerdict {
    let mut freshest: Option<i64> = None;
    let mut future_dated = false;
    let mut errors: Vec<String> = Vec::new();
    for stamp in stamps {
        match stamp {
            Ok(Some(t)) if t.saturating_sub(now_unix) > SCAN_REF_FUTURE_TOLERANCE_SECS => {
                future_dated = true;
            }
            Ok(Some(t)) => freshest = Some(freshest.map_or(t, |f| f.max(t))),
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    match freshest {
        Some(t) => RefreshVerdict::At(t),
        None if future_dated => RefreshVerdict::OnlyFutureDated,
        None if !errors.is_empty() => RefreshVerdict::ProbeFailed(errors.join("; ")),
        None => RefreshVerdict::NoRecord,
    }
}

/// Measure the scan source against the ref it should be reading.
///
/// Pure over `git` and the clock: every branch is reachable from a fake
/// reader, which is what makes the four states testable without a repo on
/// disk, and `now_unix` is a parameter so the ref's age is too.
pub fn measure_scan_divergence(
    plans_dir: Option<&Path>,
    git: &dyn GitRefReader,
    now_unix: i64,
) -> ScanDivergence {
    measure_unstamped(plans_dir, git, now_unix).observed_at(now_unix)
}

/// The tick's measurement: [`measure_scan_divergence`] plus the reading's
/// plan-library `source_repo` key.
///
/// Run on the blocking pool with the git probes because
/// [`super::body_push::derive_source_repo`] walks the filesystem for a `.git`
/// — work that does not belong on the single-worker runtime the scan-root
/// report is later posted from. Kept out of [`measure_scan_divergence`] so
/// that stays pure over its fake reader.
fn measure_scan_source(dir: &Path, git: &dyn GitRefReader, now_unix: i64) -> ScanDivergence {
    ScanDivergence {
        source_repo: super::body_push::derive_source_repo(dir),
        ..measure_scan_divergence(Some(dir), git, now_unix)
    }
}

/// The `detail` of a reading whose measurement task did not complete.
///
/// Stable by construction: a `JoinError`'s `Display` names the task's
/// per-run id, and `detail` is part of what [`ScanDivergence::is_same_reading`]
/// compares — so formatting the error itself would make a measurement that
/// panics every tick a NEW reading every tick (a WARN and a POST a minute).
fn probe_task_failure_detail(e: &tokio::task::JoinError) -> &'static str {
    if e.is_panic() {
        "the scan-divergence probe task panicked, so nothing about the scan source was measured"
    } else {
        "the scan-divergence probe task was cancelled, so nothing about the scan source was \
         measured"
    }
}

/// [`measure_scan_divergence`] without the `observed_at` stamp, so the stamp
/// is applied in ONE place and no arm can return a reading without it.
fn measure_unstamped(
    plans_dir: Option<&Path>,
    git: &dyn GitRefReader,
    now_unix: i64,
) -> ScanDivergence {
    let Some(dir) = plans_dir else {
        return ScanDivergence::not_scanning();
    };
    let dir_str = dir.display().to_string();
    let root = match git.work_tree_root(dir) {
        Ok(Some(root)) => root,
        Ok(None) => {
            return ScanDivergence {
                plans_dir: Some(dir_str.clone()),
                detail: Some(format!(
                    "the configured plans dir `{dir_str}` is not inside a git work tree, so \
                     there is no ref to compare the scanned files against (this is a supported \
                     configuration, not a fault — it is reported so it cannot be mistaken for \
                     agreement with a ref)"
                )),
                ..ScanDivergence::blank(ScanDivergenceState::NotAGitWorkTree)
            }
        }
        // NOT `NotAGitWorkTree`: we never established that it isn't one.
        Err(e) => {
            return ScanDivergence {
                plans_dir: Some(dir_str.clone()),
                detail: Some(format!(
                    "cannot tell whether the configured plans dir `{dir_str}` is inside a git \
                     work tree, so nothing about the scan source is established: {e}"
                )),
                ..ScanDivergence::blank(ScanDivergenceState::Unknown)
            }
        }
    };
    let root_str = root.display().to_string();
    let base = ScanDivergence {
        plans_dir: Some(dir_str),
        repo_root: Some(root_str.clone()),
        ..ScanDivergence::blank(ScanDivergenceState::Unknown)
    };

    let default_ref = match git.default_ref(&root) {
        Ok(r) => r,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!(
                    "cannot resolve the default branch of `{root_str}`, so there is nothing to \
                     measure against: {e}"
                )),
                ..base
            }
        }
    };
    let base = ScanDivergence {
        default_ref: Some(default_ref.clone()),
        ..base
    };

    let ref_sha = match git.rev_parse(&root, &default_ref) {
        Ok(s) => s,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!(
                    "cannot resolve `{default_ref}` in `{root_str}`: {e}"
                )),
                ..base
            }
        }
    };
    let head_sha = match git.rev_parse(&root, "HEAD") {
        Ok(s) => s,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!("cannot resolve `HEAD` in `{root_str}`: {e}")),
                ..base
            }
        }
    };
    let base = ScanDivergence {
        ref_sha: Some(ref_sha.clone()),
        head_sha: Some(head_sha),
        ..base
    };

    match git.count_behind_ahead(&root, &default_ref, "HEAD") {
        Ok((behind, ahead)) => {
            // The counts are real either way; the age decides only whether
            // they are current or floors. So an age that cannot be read keeps
            // `Measured` — dropping to `Unknown` would throw away two true
            // numbers — and says why in `detail`, while `ref_age_secs: None`
            // makes `counts_are_floors()` true.
            let verdict = combine_refresh_stamps(
                git.ref_refresh_stamps(&root, &default_ref, &ref_sha),
                now_unix,
            );
            // Every detail below is STABLE across ticks — no live difference,
            // no pid, no count of seconds — because `detail` is part of what
            // `is_same_reading` compares: a number that moves every tick would
            // make one unchanging fault a new reading (a WARN and a POST) every
            // minute.
            let (ref_age_secs, detail) = match verdict {
                RefreshVerdict::At(refreshed_at) => {
                    (Some(ref_age_from(now_unix, refreshed_at)), None)
                }
                RefreshVerdict::OnlyFutureDated => (
                    None,
                    Some(format!(
                        "every refresh record for `{default_ref}` in `{root_str}` is dated more \
                         than {SCAN_REF_FUTURE_TOLERANCE_SECS}s in the FUTURE (a clock \
                         correction, or a restored file's mtime), so none proves when the ref \
                         was last refreshed — its age is unknown and the counts are lower bounds"
                    )),
                ),
                RefreshVerdict::NoRecord => (
                    None,
                    Some(format!(
                        "neither a `FETCH_HEAD` naming `{default_ref}` at its current sha \
                         nor a reflog entry for `refs/remotes/{default_ref}` exists in \
                         `{root_str}`, so when the ref was last refreshed is unknown — the \
                         counts are lower bounds"
                    )),
                ),
                RefreshVerdict::ProbeFailed(e) => (
                    None,
                    Some(format!(
                        "cannot read when `{default_ref}` was last refreshed in \
                         `{root_str}`, so the counts are lower bounds: {e}"
                    )),
                ),
            };
            ScanDivergence {
                state: ScanDivergenceState::Measured,
                behind: Some(behind),
                ahead: Some(ahead),
                ref_age_secs,
                detail,
                ..base
            }
        }
        Err(e) => ScanDivergence {
            detail: Some(format!(
                "cannot count `{default_ref}`...`HEAD` in `{root_str}`: {e}"
            )),
            ..base
        },
    }
}

/// Build the symmetric-difference range for the ahead/behind count.
///
/// One named function purely so the ORDER can be pinned by test. The parse
/// below reads `<left>` as behind and `<right>` as ahead; that is only correct
/// while `reference` is interpolated LEFT of the `...` and `head` RIGHT of it.
/// Swap the two here and every reading inverts — a parked tree renders as a
/// lead — while [`parse_left_right_count`]'s own tests keep passing, because
/// the parser cannot see which rev produced which column.
fn left_right_range(reference: &str, head: &str) -> String {
    format!("{reference}...{head}")
}

/// Parse `git rev-list --left-right --count <reference>...<head>` output.
///
/// The two tab-separated numbers are `<left>\t<right>`, and the orientation is
/// the one thing here that can be silently wrong: LEFT counts commits
/// reachable from the LEFT rev (`<reference>`) and not from the right — the
/// scanned tree is BEHIND by that many — and RIGHT is the mirror, the scanned
/// tree's own commits no ref carries, AHEAD. Swap them and a 2153-behind park
/// renders as 2153 AHEAD, which reads like a busy authoring machine rather
/// than a stale one. Parsed in exactly one named place, and pinned by test.
fn parse_left_right_count(raw: &str) -> Result<(u64, u64), String> {
    let mut fields = raw.split_whitespace();
    let (Some(left), Some(right), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(format!(
            "expected two whitespace-separated counts from `rev-list --left-right --count`, got {raw:?}"
        ));
    };
    let behind = left
        .parse::<u64>()
        .map_err(|e| format!("left count {left:?} is not a number: {e}"))?;
    let ahead = right
        .parse::<u64>()
        .map_err(|e| format!("right count {right:?} is not a number: {e}"))?;
    Ok((behind, ahead))
}

/// How far in the future a refresh timestamp may sit and still be read as
/// "just now" (age 0).
///
/// Five minutes of skew between the file clock and the process clock is
/// ordinary (a network filesystem, an NTP step). Beyond it the timestamp is
/// not a small skew but a record that cannot be trusted — a clock corrected
/// backwards after a fetch, a checkout restored from a backup with future
/// mtimes — and reading it as age 0 would be a FALSE "fresh", the one outcome
/// the floor rule exists to prevent. So a further-future stamp is an UNKNOWN
/// age instead — and is dropped before any other record is compared with it
/// ([`combine_refresh_stamps`]).
const SCAN_REF_FUTURE_TOLERANCE_SECS: i64 = 300;

/// `now - refreshed_at` in whole seconds, saturating at zero.
///
/// A refresh stamped slightly in the future (within
/// [`SCAN_REF_FUTURE_TOLERANCE_SECS`]) reads as age 0 rather than wrapping to
/// an astronomically old ref; a stamp further out never reaches here.
fn ref_age_from(now_unix: i64, refreshed_at: i64) -> u64 {
    u64::try_from(now_unix.saturating_sub(refreshed_at)).unwrap_or(0)
}

/// The branch name a remote-tracking ref stands for: `origin/main` → `main`,
/// `origin/release/x` → `release/x`. This is the spelling `FETCH_HEAD` uses in
/// its `branch '<name>' of <url>` descriptions.
fn default_branch_name(default_ref: &str) -> &str {
    default_ref
        .strip_prefix("origin/")
        .or_else(|| default_ref.split_once('/').map(|(_, branch)| branch))
        .unwrap_or(default_ref)
}

/// Whether `FETCH_HEAD` `contents` record a fetch of `branch` at `ref_sha`.
///
/// Each line is `<sha>\t<"" | "not-for-merge">\t<description>`, and a branch
/// fetch's description is `branch '<name>' of <url>`. Both halves must match:
/// the NAME, because a fetch of some other branch refreshes nothing this
/// reading compares against; and the SHA, because a `FETCH_HEAD` line whose
/// sha is not what the tracking ref now holds did not land in it — a
/// non-fast-forward the refspec refused, or a fetch by URL that updates no
/// tracking ref — so its mtime says nothing about the ref's freshness.
fn fetch_head_names_ref(contents: &str, branch: &str, ref_sha: &str) -> bool {
    let wanted = format!("branch '{branch}' of ");
    contents.lines().any(|line| {
        let mut fields = line.split('\t');
        let (Some(sha), Some(_merge_marker), Some(description)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        sha.trim() == ref_sha && description.starts_with(&wanted)
    })
}

/// Parse `git reflog show -n1 --date=unix --format=%gd <ref>` output — e.g.
/// `origin/main@{1789129011}` — into the entry's unix time.
///
/// Empty output is `Ok(None)`: the ref exists (it was just resolved) but has no
/// reflog (`core.logAllRefUpdates` off, or expired) — an absent source, not a
/// failure. Anything else that is not a plausible unix time is an error rather
/// than a guess: in particular `@{0}`, which is what `%gd` prints if the date
/// format is not applied, must not parse as "refreshed in 1970".
fn parse_reflog_entry_time(raw: &str) -> Result<Option<i64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let inner = raw
        .rsplit_once("@{")
        .and_then(|(_, rest)| rest.strip_suffix('}'))
        .ok_or_else(|| format!("expected `<ref>@{{<unix time>}}` from the reflog, got {raw:?}"))?;
    let secs = inner
        .parse::<i64>()
        .map_err(|e| format!("reflog selector {inner:?} is not a unix time: {e}"))?;
    // 2001-09-09: anything earlier is a selector index, not a timestamp.
    if secs < 1_000_000_000 {
        return Err(format!(
            "reflog selector {raw:?} is not a unix time (was `--date=unix` ignored?)"
        ));
    }
    Ok(Some(secs))
}

/// One `FETCH_HEAD` file's contribution: its mtime when it records a fetch of
/// `branch` at `ref_sha` ([`fetch_head_names_ref`]), `Ok(None)` when it is
/// absent or names something else.
fn fetch_head_file_refreshed_at(
    path: &Path,
    branch: &str,
    ref_sha: &str,
) -> Result<Option<i64>, String> {
    // Stat BEFORE reading. A fetch landing between the two then pairs an
    // OLDER mtime with newer contents — an overstated age — where the
    // opposite order could pair a fresh mtime from a fetch of some other
    // branch with contents that still named this one: a false "fresh".
    let modified = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot stat `{}`: {e}", path.display())),
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read `{}`: {e}", path.display())),
    };
    if !fetch_head_names_ref(&contents, branch, ref_sha) {
        return Ok(None);
    }
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("`{}` has an mtime before the epoch: {e}", path.display()))?
        .as_secs();
    Ok(Some(i64::try_from(secs).unwrap_or(i64::MAX)))
}

/// Budget for every `git` invocation the detector makes.
///
/// All of them are LOCAL plumbing reads, so a healthy call is milliseconds; the
/// bound exists for a concurrent `index.lock` or a repo on a stalled mount.
/// They run on the blocking pool (see the reconcile loop's tick), but an unbounded
/// hang there still leaks a pool thread per cycle, so every one is capped —
/// the same posture as `git_status_subset`'s `GIT_TIMEOUT`.
const SCAN_DIVERGENCE_GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for the scan's `git fetch` — the one NETWORK call in this module,
/// and so the one that must not inherit the local-plumbing budget above. A
/// cold or slow fetch overrunning 20 s would report `TimedOut`, which the
/// caller reads as `Unavailable`, which publishes nothing: a slow network
/// would look exactly like a broken ref.
const SCAN_FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Budget for reading ONE plan blob — a local object read, like the probes
/// above, so it carries their budget rather than the network one.
const SCAN_BLOB_TIMEOUT: Duration = Duration::from_secs(20);

/// How many BYTES of blob bodies the cache holds before it evicts.
///
/// Bounded in BYTES rather than entries, because an entry is a whole plan body
/// and those vary by an order of magnitude — measured on the live corpus: 1863
/// plans, 28.4 KB average, 185.7 KB largest, 54.2 MB total. An entry cap sized
/// for "several roots" therefore states no memory bound at all: 6000 entries is
/// ~175 MB held in a global static in a process that runs for weeks, on a fleet
/// that keeps a knowledge-base page on exhaustion signatures. 96 MB holds the
/// measured corpus plus an archive root with headroom, and says what it costs.
const BLOB_CACHE_MAX_BYTES: usize = 96 * 1024 * 1024;

/// Blob bodies already read, keyed by OBJECT ID, with the read counter at
/// which each was last used.
///
/// A git object id is a content hash, so `id -> bytes` is a cache with no
/// invalidation problem: the same id is provably the same bytes, forever and
/// across repos. That is what makes the per-blob read affordable — the listing
/// changes rarely, so after the first cycle nearly every id is a hit.
///
/// ## Why it is NOT pruned to the caller's own id set
///
/// It was, and that was a defect the moment a SECOND caller appeared. Phase 2
/// had exactly one `read_blobs` call per cycle, so "retain only this call's
/// ids" was a no-op and the steady state really did spawn nothing. Phase 3
/// added the document layer, so there are now `1 + roots` calls per cycle —
/// and a prune to one caller's ids EVICTS every other root's corpus, turning
/// every cycle's every root into a full miss. Measured shape: ~1,863 plans, so
/// ~1,863 `git cat-file` spawns per minute, forever, each taken while holding
/// this mutex.
///
/// So eviction is by LEAST-RECENTLY-USED against a cap instead. Anything still
/// being read each cycle stays hot whichever caller reads it; a rewritten
/// plan's old id ages out on its own, because a rewrite yields a new id and
/// the old one stops being touched. That is the property the prune was
/// reaching for, and it is the one an id-set prune could not express.
fn blob_cache() -> &'static std::sync::Mutex<HashMap<String, (String, u64)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (String, u64)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Evict the coldest quarter of `cache` once its bodies exceed `max_bytes`, by
/// LRU stamp.
///
/// Extracted so the property that REGRESSED is testable. The previous policy
/// was `retain(|id, _| this_call's_ids.contains(id))`, which is correct for
/// exactly one caller per cycle and catastrophic for two: each call evicts the
/// other's corpus, so every cycle's every root becomes a full miss and spawns a
/// `git cat-file` per plan. Nothing pinned that, because with one caller the
/// prune was a no-op — the defect was invisible until a second caller existed.
///
/// A quarter at a time rather than one entry, so eviction is amortised instead
/// of paid on every insert once the cap is reached.
fn evict_cold_blobs(cache: &mut HashMap<String, (String, u64)>, max_bytes: usize) {
    let held: usize = cache.values().map(|(b, _)| b.len()).sum();
    if held <= max_bytes || cache.len() < 2 {
        return;
    }
    // On the `len < 2` guard: it is NOT an index guard, and an earlier comment
    // implied it was. `stamps[cache.len() / 4]` is in bounds for every
    // `len >= 1` (`len/4 < len`), and `len == 0` returns on the sum anyway.
    // What it prevents is EVICT-EVERYTHING at `len == 1`: the cutoff would be
    // the sole entry's own stamp, `retain(n > cutoff)` would empty the cache,
    // and a single body larger than the bound would then be re-read on every
    // call — permanent thrash on the largest plan. Exceeding the bound by at
    // most one body is the better trade.
    //
    // Two soft-bound facts, so the bound is not read as harder than it is: one
    // pass drops a quarter of ENTRIES against a BYTE bound, so a single pass
    // does not guarantee `held <= max_bytes` — it converges over calls. And the
    // check runs AFTER a call's inserts, so peak resident is the bound plus one
    // call's bytes.
    // OBSERVABLE, once per process. Crossing the bound puts the working set
    // into cyclic eviction — LRU's textbook worst case here, because the roots
    // are read in the same order every cycle, so the coldest quarter is always
    // the root read earliest and is evicted just before it is needed again.
    // That is the original defect's cost profile reached by another route, and
    // it must not be a silent cliff.
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            held_bytes = held,
            max_bytes,
            entries = cache.len(),
            "plan adapter: the blob cache crossed its byte bound and is now evicting; a \
             working set larger than the bound re-introduces a per-cycle re-read of the \
             evicted root (one `git cat-file` per plan). Raise the bound or reduce the \
             scan roots."
        );
    }
    let mut stamps: Vec<u64> = cache.values().map(|(_, n)| *n).collect();
    stamps.sort_unstable();
    // A quarter at a time rather than one entry, so eviction is amortised
    // instead of paid on every insert once the bound is reached.
    let cutoff = stamps[cache.len() / 4];
    cache.retain(|_, (_, n)| *n > cutoff);
}

/// Serve `id` from `cache` if present, RE-STAMPING it as most-recently-used.
///
/// Extracted from [`GitRefReader::read_blobs`]' hit arm so the re-stamp is
/// reachable from a test at all. Without it a body only one caller reads keeps
/// its INSERT stamp forever and ages out under eviction while being read every
/// cycle — the cyclic re-read this cache exists to prevent, by a third route.
fn touch_blob(cache: &mut HashMap<String, (String, u64)>, id: &str) -> Option<String> {
    let slot = cache.get_mut(id)?;
    slot.1 = blob_cache_tick();
    Some(slot.0.clone())
}

/// Monotonic read counter, for the cache's LRU stamp.
fn blob_cache_tick() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The production [`GitRefReader`]: shells out to `git`, always with an
/// explicit `-C <dir>` so the probe can never pick up the runner's own cwd.
///
/// Every probe goes through `run_probe_quiet`, whose non-zero arm is DEBUG:
/// three of the four reads answer negatively as a matter of routine (a plans
/// dir outside a repo, a clone with no `origin/HEAD`), and WARNing on those
/// would bury the timeout WARN that does matter.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessGit;

impl ProcessGit {
    /// Run one probe, keeping the [`crate::process_helpers::DegradeReason`]
    /// intact. Callers need it: a non-zero exit is a real ANSWER for three of
    /// the four reads ("not a work tree", "no `origin/HEAD`"), while a spawn
    /// failure or a timeout is the measurement itself breaking, and the two
    /// must not collapse.
    fn probe(
        dir: &Path,
        args: &[&str],
        label: &str,
    ) -> Result<String, crate::process_helpers::DegradeReason> {
        Self::probe_within(dir, args, label, SCAN_DIVERGENCE_GIT_TIMEOUT)
    }

    /// [`Self::probe`] on an explicit budget, for the one call that is not a
    /// local plumbing read — see [`SCAN_FETCH_TIMEOUT`].
    fn probe_within(
        dir: &Path,
        args: &[&str],
        label: &str,
        budget: Duration,
    ) -> Result<String, crate::process_helpers::DegradeReason> {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C").arg(dir).args(args);
        match crate::process_helpers::run_probe_quiet(cmd, budget, label) {
            crate::process_helpers::ProbeOutcome::Captured(out) => {
                Ok(String::from_utf8_lossy(&out).trim().to_string())
            }
            crate::process_helpers::ProbeOutcome::Degraded(reason) => Err(reason),
        }
    }

    /// [`Self::probe`] with every degrade flattened to a sentence — for the
    /// two reads whose non-zero exit carries no extra meaning.
    fn run(dir: &Path, args: &[&str], label: &str) -> Result<String, String> {
        Self::run_within(dir, args, label, SCAN_DIVERGENCE_GIT_TIMEOUT)
    }

    /// [`Self::run`] on an explicit budget. The budget reaches
    /// [`Self::describe`] too, so a timed-out fetch names the budget it
    /// actually overran rather than the probe budget it never had.
    fn run_within(
        dir: &Path,
        args: &[&str],
        label: &str,
        budget: Duration,
    ) -> Result<String, String> {
        Self::probe_within(dir, args, label, budget)
            .map_err(|reason| Self::describe_within(args, &reason, budget))
    }

    /// One sentence for a probe that did not answer — worded WITHOUT per-run
    /// noise. `DegradeReason`'s `Debug` form carries the killed child's pid
    /// (`TimedOut { pid: 41873, .. }`), and this text reaches
    /// [`ScanDivergence::detail`], which [`ScanDivergence::is_same_reading`]
    /// compares verbatim: a pid in it made a probe that times out every tick
    /// a NEW reading every tick — a WARN and a scan-root POST per minute for
    /// what is one unchanging fault.
    fn describe(args: &[&str], reason: &crate::process_helpers::DegradeReason) -> String {
        Self::describe_within(args, reason, SCAN_DIVERGENCE_GIT_TIMEOUT)
    }

    /// [`Self::describe`] for a probe that ran on a non-default budget.
    fn describe_within(
        args: &[&str],
        reason: &crate::process_helpers::DegradeReason,
        budget: Duration,
    ) -> String {
        use crate::process_helpers::DegradeReason;
        let why = match reason {
            DegradeReason::Status => "it exited non-zero".to_string(),
            DegradeReason::SpawnError => "it could not be spawned (SpawnError)".to_string(),
            DegradeReason::TimedOut { reaped, .. } => format!(
                "it overran its {}s budget and was killed (TimedOut{})",
                budget.as_secs(),
                if *reaped { "" } else { ", not reaped" }
            ),
            DegradeReason::Truncated(t) => format!("its output was truncated ({t:?})"),
        };
        format!("`git {}` did not answer: {why}", args.join(" "))
    }

    /// The `FETCH_HEAD` sources of [`GitRefReader::ref_refresh_stamps`]: the
    /// mtimes of the two `FETCH_HEAD` files a checkout can have — its own
    /// (`--git-path FETCH_HEAD`) and the common dir's — each counted only when
    /// it records a fetch of the default branch at `ref_sha`.
    ///
    /// Two files because `FETCH_HEAD` is PER-WORKTREE while the tracking ref
    /// and its reflog are shared. In a linked worktree `--git-path FETCH_HEAD`
    /// names that worktree's own file — which the worktree census's fetch,
    /// run in the PRIMARY checkout, never writes — so reading it alone would
    /// miss the refresh that actually updated the shared ref. The primary's
    /// file is `<git-common-dir>/FETCH_HEAD`. In a primary checkout both paths
    /// are the same file and reading it twice changes nothing.
    ///
    /// Returned as two SEPARATE answers: a `rev-parse --git-common-dir` that
    /// fails costs only the common-dir source, never the per-worktree answer
    /// already in hand (and vice versa).
    ///
    /// Both paths come from `rev-parse`, never a hardcoded `.git/…`: git
    /// prints them relative to the `-C` dir (or absolute), so each is joined
    /// onto `repo_root` — an absolute path replaces the base on join.
    fn fetch_head_stamps(
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Vec<Result<Option<i64>, String>> {
        let branch = default_branch_name(default_ref);
        let one = |args: &[&str], label: &str, file: &str| -> Result<Option<i64>, String> {
            let located = Self::run(repo_root, args, label)?;
            if located.is_empty() {
                return Err(format!("`git {}` returned nothing", args.join(" ")));
            }
            let path = repo_root.join(located);
            let path = if file.is_empty() {
                path
            } else {
                path.join(file)
            };
            fetch_head_file_refreshed_at(&path, branch, ref_sha)
        };
        vec![
            one(
                &["rev-parse", "--git-path", "FETCH_HEAD"],
                "plan adapter: scan-divergence FETCH_HEAD path probe",
                "",
            ),
            one(
                &["rev-parse", "--git-common-dir"],
                "plan adapter: scan-divergence git-common-dir probe",
                "FETCH_HEAD",
            ),
        ]
    }

    /// The reflog source of [`GitRefReader::ref_refresh_stamps`]: the ENTRY time
    /// of the newest reflog record for `refs/remotes/<default_ref>`.
    ///
    /// `%gd` with `--date=unix` renders the selector as `<ref>@{<entry time>}`
    /// — the time the ref was written. `%ct` would be the tip COMMIT's
    /// committer time, which says when a commit was authored upstream, not
    /// when this clone received it: a week-old commit fetched a minute ago
    /// would read a week stale, and a fresh commit on a clone unfetched since
    /// would read as refreshed at authoring time.
    fn reflog_refreshed_at(repo_root: &Path, default_ref: &str) -> Result<Option<i64>, String> {
        let tracking = format!("refs/remotes/{default_ref}");
        let out = Self::run(
            repo_root,
            &[
                "reflog",
                "show",
                "-n1",
                "--date=unix",
                "--format=%gd",
                &tracking,
            ],
            "plan adapter: scan-divergence reflog probe",
        )?;
        parse_reflog_entry_time(&out)
    }
}

impl ProcessGit {
    /// Read ONE blob by object id, on a hard clock.
    ///
    /// ## Why one process per blob rather than one `cat-file --batch`
    ///
    /// A single `--batch` over the whole dir is one spawn instead of ~1,100,
    /// and that is what this used to be. It cannot survive contact with the
    /// bounded-subprocess rule: every sanctioned wrapper in
    /// [`crate::process_helpers`] forces `stdin(null)` and caps captured
    /// stdout at `MAX_CAPTURED_BYTES` (4 MiB), so a `--batch` can neither be
    /// FED its ids through one nor return a plans dir's tens of megabytes
    /// through one. Hand-rolling the spawn to get around that is precisely the
    /// defect class `scripts/check_untimed_subprocess.py` exists to stop —
    /// an unbounded `Child::wait()` on a tokio blocking-pool thread, which on
    /// 2026-08-30 exhausted the 512-thread pool and took `/livez` dark.
    ///
    /// So the read is per blob, through `run_with_timeout_detailed`, and the
    /// SPAWN COUNT is bought back by the cache above rather than by owning a
    /// child. That trade also deletes the writer thread, the watchdog, the
    /// `Arc<Mutex<Child>>` and the manual reap this function used to need —
    /// none of which can be got wrong if none of them exists.
    ///
    /// A truncated read is an `Err`, never a short body: a plan body silently
    /// cut at the byte cap would reach the corpus as a real plan with its
    /// phases missing.
    fn cat_file_blob(repo_root: &Path, id: &str) -> Result<Vec<u8>, String> {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C").arg(repo_root).args(["cat-file", "blob", id]);
        let run = crate::process_helpers::run_with_timeout_detailed(cmd, SCAN_BLOB_TIMEOUT)
            .map_err(|e| format!("`git cat-file blob {id}` could not be spawned: {e}"))?;
        if let Some(t) = run.truncation {
            return Err(format!(
                "blob {id} was read INCOMPLETELY ({t:?}); a short plan body must not reach the \
                 corpus"
            ));
        }
        match run.outcome {
            crate::process_helpers::TimedOutput::Completed(o) if o.status.success() => Ok(o.stdout),
            // A non-zero exit is git's own answer: no such object here.
            crate::process_helpers::TimedOutput::Completed(_) => {
                Err(format!("blob {id} is missing from this repo"))
            }
            crate::process_helpers::TimedOutput::TimedOut { reaped, .. } => Err(format!(
                "blob {id} overran its {}s budget and was killed (reaped={reaped})",
                SCAN_BLOB_TIMEOUT.as_secs()
            )),
        }
    }
}

impl GitRefReader for ProcessGit {
    fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
        const ARGS: [&str; 2] = ["rev-parse", "--show-toplevel"];
        match Self::probe(dir, &ARGS, "plan adapter: scan-divergence work-tree probe") {
            Ok(out) if out.is_empty() => Ok(None),
            Ok(out) => Ok(Some(PathBuf::from(out))),
            // A non-zero exit here is git's own answer — "not a git repository"
            // — and the one degrade that means `dir` genuinely is not a work
            // tree. A missing binary, a nonexistent dir or a timeout tells us
            // nothing about `dir`, so it must not be reported as if it did.
            Err(crate::process_helpers::DegradeReason::Status) => Ok(None),
            Err(reason) => Err(Self::describe(&ARGS, &reason)),
        }
    }

    fn default_ref(&self, repo_root: &Path) -> Result<String, String> {
        const ARGS: [&str; 4] = [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ];
        // `--quiet` makes an unset `origin/HEAD` exit NON-ZERO with empty
        // stdout rather than printing nothing and exiting 0, so the actionable
        // sentence has to hang off the `Status` arm — hung off an empty `Ok`
        // it would be unreachable, and the operator would get `did not answer`
        // for the one failure that has a one-line fix.
        //
        // Deliberately an error either way, never a fallback: `origin/main` is
        // a guess, and a divergence measured against the wrong branch is a
        // confident wrong number, which is worse than UNKNOWN.
        const UNSET: &str = "`origin/HEAD` is not set in this clone (`git remote set-head \
                             origin -a` sets it); refusing to assume a default branch";
        match Self::probe(
            repo_root,
            &ARGS,
            "plan adapter: scan-divergence default-branch probe",
        ) {
            Ok(out) if out.is_empty() => Err(UNSET.to_string()),
            Ok(out) => Ok(out),
            Err(crate::process_helpers::DegradeReason::Status) => Err(UNSET.to_string()),
            Err(reason) => Err(Self::describe(&ARGS, &reason)),
        }
    }

    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<String, String> {
        let out = Self::run(
            repo_root,
            &["rev-parse", rev],
            "plan adapter: scan-divergence rev-parse probe",
        )?;
        if out.is_empty() {
            return Err(format!("`git rev-parse {rev}` returned nothing"));
        }
        Ok(out)
    }

    fn count_behind_ahead(
        &self,
        repo_root: &Path,
        reference: &str,
        head: &str,
    ) -> Result<(u64, u64), String> {
        let range = left_right_range(reference, head);
        let out = Self::run(
            repo_root,
            &["rev-list", "--left-right", "--count", &range],
            "plan adapter: scan-divergence ahead/behind probe",
        )?;
        parse_left_right_count(&out)
    }

    fn ref_refresh_stamps(
        &self,
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Vec<Result<Option<i64>, String>> {
        let mut stamps = Self::fetch_head_stamps(repo_root, default_ref, ref_sha);
        stamps.push(Self::reflog_refreshed_at(repo_root, default_ref));
        stamps
    }

    fn fetch_default(&self, repo_root: &Path, default_ref: &str) -> Result<(), String> {
        // `origin/main` -> remote `origin`, branch `main`. Split rather than
        // assuming "origin" so this stays correct if `default_ref` ever stops
        // resolving through `refs/remotes/origin/HEAD` — which is what it
        // reads today, so the split is defensive rather than load-bearing.
        let (remote, branch) = match default_ref.split_once('/') {
            Some((r, b)) if !r.is_empty() && !b.is_empty() => (r, b),
            _ => {
                return Err(format!(
                    "default ref `{default_ref}` is not `<remote>/<branch>`, so no fetch target can be derived"
                ))
            }
        };
        // Called at most once per `(repo_root, default_ref)` per cycle: the
        // cycle's [`CycleRefPin`] dedups every consumer and every root onto
        // its first fetch, so this reader holds no memo of its own.
        // `-c gc.auto=0`: this is a WRITE into a checkout other agents are
        // working in. git runs `gc --auto` after a fetch by default, and a
        // repack fired off by the scan loop is a side effect on a shared
        // resource the scan has no business causing.
        // `--no-tags`: the scan reads one branch; tag traffic is pure cost.
        Self::run_within(
            repo_root,
            &[
                "-c",
                "gc.auto=0",
                "fetch",
                "--quiet",
                "--no-tags",
                remote,
                branch,
            ],
            "plan adapter: scan fetch",
            SCAN_FETCH_TIMEOUT,
        )
        .map(|_| ())
    }

    fn list_ref_dir(
        &self,
        repo_root: &Path,
        ref_name: &str,
        rel_dir: &str,
    ) -> Result<Vec<RefDirEntry>, String> {
        // No `-r`: depth 1, which IS the contract. `<ref>:<dir>` addresses the
        // directory's own tree, so entries come back bare-named.
        let spec = format!("{ref_name}:{rel_dir}");
        let args = ["ls-tree", "-z", spec.as_str()];
        let out =
            Self::probe(repo_root, &args, "plan adapter: scan ref listing").map_err(|reason| {
                match reason {
                    // A non-zero exit here is git's own ANSWER — that path is not
                    // in that ref — and it is a real, permanent configuration
                    // rather than a fault to repair: a plans dir that is
                    // gitignored, or that exists only on a feature branch. Naming
                    // it separately is what stops an operator hunting a broken
                    // fetch that never happened.
                    crate::process_helpers::DegradeReason::Status => format!(
                        "`{spec}` does not exist in that ref (an unpushed or ignored plans dir is \
                     not discoverable work)"
                    ),
                    other => Self::describe(&args, &other),
                }
            })?;
        let mut entries = Vec::new();
        for rec in out.split('\0') {
            if rec.is_empty() {
                continue;
            }
            // `<mode> SP <type> SP <id> TAB <name>`
            let (meta, name) = match rec.split_once('\t') {
                Some(v) => v,
                None => continue,
            };
            let mut f = meta.split_whitespace();
            let (mode, kind, id) = match (f.next(), f.next(), f.next()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => continue,
            };
            // Trees and commit links (submodules) are skipped, not errors.
            //
            // Mode `120000` is a SYMLINK, which `ls-tree` also reports as a
            // blob — and whose blob content is the link TARGET STRING, not the
            // file. Reading it would put a one-line path into the corpus and
            // parse it as a plan. `read_plan_dir` follows the link and reads
            // the target; nothing here can, so the parity-preserving answer is
            // to skip it rather than to publish the wrong bytes.
            if kind != "blob" || mode == "120000" {
                continue;
            }
            entries.push(RefDirEntry {
                name: name.to_string(),
                id: id.to_string(),
            });
        }
        Ok(entries)
    }

    fn read_blobs(&self, repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>> {
        if ids.is_empty() {
            return Vec::new();
        }
        // A PURE DELEGATION, and it must stay one — the logic lives where it is
        // tested. `read_blobs_wrapper_is_a_pure_delegation` pins this spelling
        // for the same reason `wedge_diagnostics`' lane wrapper is pinned: a
        // behavioural test against the PROCESS-GLOBAL cache cannot drive it
        // past a 96 MB bound, so nothing here would be covered if the body grew
        // back into this function.
        let mut cache = blob_cache().lock().unwrap_or_else(|p| p.into_inner());
        Self::read_blobs_into(&mut cache, repo_root, ids, BLOB_CACHE_MAX_BYTES)
    }
}

impl ProcessGit {
    /// [`GitRefReader::read_blobs`] against an EXPLICIT cache and bound.
    ///
    /// Module-private on purpose: production must not be able to choose a cache
    /// or a bound. The parameters exist so a test can cover THIS function — the
    /// one that does the work — against a map the rest of the test binary
    /// cannot touch and a bound it can actually cross.
    ///
    /// This is the crate's existing pattern, not a new one: see
    /// [`crate::wedge_diagnostics::spawn_blocking_tracked`], whose doc says the
    /// parameter exists "so the lane tests can cover THIS function … against a
    /// table the rest of the test binary cannot saturate".
    ///
    /// An earlier draft of the eviction test's doc claimed the hit arm could
    /// not be covered because "reaching it under eviction needs a global bound
    /// override, and such an override would race". That was WRONG — the route
    /// is to PARAMETERISE, not to override, and the crate already did it here.
    /// Recorded because the false reason is what argued a closeable gap shut.
    fn read_blobs_into(
        cache: &mut HashMap<String, (String, u64)>,
        repo_root: &Path,
        ids: &[String],
        max_bytes: usize,
    ) -> Vec<Result<String, String>> {
        let out: Vec<Result<String, String>> = ids
            .iter()
            .map(|id| match touch_blob(cache, id) {
                // A git object id is a CONTENT HASH, so a hit is not a guess
                // that the bytes are unchanged — it is a proof of it. That is
                // the whole reason this cache needs no invalidation rule.
                // `touch_blob` also re-stamps it, so a body only this caller
                // reads stays hot.
                Some(hit) => Ok(hit),
                None => {
                    let body = Self::cat_file_blob(repo_root, id)?;
                    // STRICT UTF-8, not `from_utf8_lossy`: `read_plan_dir`'s
                    // `read_to_string` SKIPS a non-UTF-8 file, and a silently
                    // mangled body here would be a plan the two arms disagree
                    // about — the opposite of the parity this phase claims.
                    let text = String::from_utf8(body)
                        .map_err(|e| format!("blob {id} is not valid UTF-8: {e}"))?;
                    cache.insert(id.clone(), (text.clone(), blob_cache_tick()));
                    Ok(text)
                }
            })
            .collect();
        // LRU against the bound — NEVER a prune to this caller's own ids, which
        // would evict every other scan root's corpus on every call. See
        // [`blob_cache`].
        evict_cold_blobs(cache, max_bytes);
        out
    }
}

/// What [`record_scan_divergence`] says about a changed reading: `(at_warn,
/// message)`. A pure function so the wording — which is the whole point of
/// the floor rule — is pinned by test rather than asserted in a comment.
///
/// Three shapes:
/// - **floors** ([`ScanDivergence::counts_are_floors`]) — WARN, and the text
///   says the counts are LOWER BOUNDS and names the ref's age (or that it is
///   unknown). This covers a floor of `0/0`: it must never reach the log as
///   the benign "reading changed" line, because a reader skimming for WARNs
///   would take it for agreement with a ref it was never compared against.
/// - **stale against a fresh ref** — WARN, the parked-tree text, with the age.
/// - everything else — INFO, a plain transition note.
fn scan_divergence_message(d: &ScanDivergence) -> (bool, String) {
    let default_ref = d.default_ref.as_deref().unwrap_or("the default branch");
    let window_hours = SCAN_REF_FRESH_WITHIN.as_secs() / 3600;
    if d.counts_are_floors() {
        let as_of = match d.ref_age_secs {
            Some(age) => format!(
                "as of a refresh {age}s (~{}h) ago, older than the {window_hours}h freshness \
                 window",
                age / 3600
            ),
            None => format!(
                "as of a refresh of UNKNOWN age (nothing proves it happened within the \
                 {window_hours}h freshness window)"
            ),
        };
        let message = if d.is_stale() {
            format!(
                "plan adapter: the scanned plans dir is a WORKING TREE that has diverged from \
                 its own default branch, and the counts are LOWER BOUNDS: they were taken \
                 against `{default_ref}` {as_of}, so the true `behind` can only be larger (and \
                 `ahead` may overstate). This describes the CHECKOUT, not what was published: \
                 since Phase 3 both the work units and the plan bodies are read from the ref, so \
                 a divergent tree no longer implies a divergent corpus"
            )
        } else {
            format!(
                "plan adapter: the scanned plans dir's HEAD reads 0 behind / 0 ahead of \
                 `{default_ref}`, but that is a LOWER BOUND, not agreement: the ref was compared \
                 {as_of}, so how far the scan source has fallen behind is UNKNOWN until the ref \
                 is refreshed (this reading is taken WITHOUT fetching; the scan \
                 itself does fetch — see `fetch_default`)"
            )
        };
        return (true, message);
    }
    if d.is_stale() {
        let as_of = d
            .ref_age_secs
            .map(|age| format!("a refresh {age}s ago"))
            .unwrap_or_else(|| "this clone's last fetch".to_string());
        return (
            true,
            format!(
                "plan adapter: the scanned plans dir is a WORKING TREE that is behind its own \
                 default branch. Counts are as of {as_of}. This is a fact about the CHECKOUT \
                 only — since Phase 3 both layers publish from the ref, so this no longer means \
                 the corpus is stale; it means peers reading files in this tree get old bytes"
            ),
        );
    }
    (
        false,
        "plan adapter: scan-source divergence reading changed".to_string(),
    )
}

/// Whether [`record_scan_divergence`] logs `new` — i.e. whether it differs
/// from the reading it replaces by [`ScanDivergence::is_same_reading`], never
/// by plain equality: `observed_at_unix` is re-stamped every tick and
/// `ref_age_secs` grows every tick, so plain equality would log every minute.
/// The first reading after start (no previous) always logs.
fn scan_divergence_changed(previous: Option<&ScanDivergence>, new: &ScanDivergence) -> bool {
    !previous.is_some_and(|previous| previous.is_same_reading(new))
}

/// Publish this cycle's reading, and log it **only when it changed**.
///
/// Every cycle records; only a transition logs. A per-cycle line for a
/// steady-state reading is pure volume at a 60s tick, and volume is how the
/// one line that matters gets missed. "Changed" is
/// [`ScanDivergence::is_same_reading`], not plain equality: the ref's age
/// grows every tick, and only its crossing of the freshness window is news. A
/// stale scan source ([`ScanDivergence::is_stale`]) or a floor reading
/// ([`ScanDivergence::counts_are_floors`]) logs at WARN — see
/// [`scan_divergence_message`].
fn record_scan_divergence(divergence: ScanDivergence, metrics: &AdapterMetrics) {
    let mut slot = metrics
        .scan_divergence
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if scan_divergence_changed(slot.as_ref(), &divergence) {
        let (at_warn, message) = scan_divergence_message(&divergence);
        if at_warn {
            tracing::warn!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                ref_age_secs = ?divergence.ref_age_secs,
                counts_are_floors = divergence.counts_are_floors(),
                detail = ?divergence.detail,
                "{message}"
            );
        } else {
            tracing::info!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                ref_age_secs = ?divergence.ref_age_secs,
                counts_are_floors = divergence.counts_are_floors(),
                detail = ?divergence.detail,
                "{message}"
            );
        }
    }
    *slot = Some(divergence);
}

/// Outcome of one reconcile cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconcileSummary {
    pub scanned: u64,
    pub transitions: u64,
    pub conflicts: u64,
    pub errors: u64,
    /// Transitions suppressed because a real agent owns the unit
    /// ([`PushOutcomeKind::Deferred`]). Not an error and not a transition.
    pub deferred: u64,
    /// Dependency-edge replace-sets applied to coord's edge table this cycle.
    pub deps_set: u64,
    /// Dep-set calls skipped because coord's edge table isn't migrated yet.
    pub deps_skipped_unmigrated: u64,
    /// Dep-set calls that hard-errored (does not count toward `errors`, which
    /// is reserved for the unit upsert/transition path — a dep-edge failure is
    /// non-fatal and additive).
    pub deps_errors: u64,
    /// Units skipped or retired this cycle because coord answered `403` and
    /// named no clearing condition — a PERMISSION verdict, whose remedy is to
    /// fix the principal's grant. Deliberately NOT folded into `errors`:
    /// `errors` means "retryable, and we will retry", which is the one thing a
    /// permission verdict is not — and deliberately not folded together with
    /// `retired_permanent` either, which is a different operator action.
    pub forbidden: u64,
    /// Units whose STATUS WRITE was withheld this cycle because coord answered
    /// `terminality: permanent` for the status the file asks to apply — a
    /// PLAN-FILE verdict, whose remedy is to edit the stamp. Scoped to the
    /// `(slug, status)` pair, so it stops counting a unit the moment its file
    /// changes. The unit's status-less metadata upsert still runs.
    pub retired_permanent: u64,
    /// Dep-edge pushes skipped or retired this cycle because coord answered
    /// `403` on `POST /coord/work-units/:slug/deps` specifically. Not folded
    /// into `deps_errors` for the same reason `forbidden` is kept out of
    /// `errors` — and not folded into `forbidden`, since a deps refusal does
    /// not imply the unit's own upsert/transition route is refused too.
    ///
    /// Only the principal class (a `403` naming no clearing condition) retires
    /// this route. A `terminality: permanent` on a dep-edge set is left in the
    /// retry arm deliberately: the key it would have to be retired on is the
    /// attempted DEP SET, not a status, and coord is not known to emit one
    /// here — a retirement keyed too broadly is the defect this file closed on
    /// the unit route.
    pub deps_forbidden: u64,
    /// Units whose `last_applied` was seeded from coord's current status this
    /// cycle (no in-process memory of them).
    pub seeded: u64,
    /// Units SKIPPED this cycle because the seed read failed. These are NOT
    /// counted in `errors` (nothing was pushed) and NOT in `scanned`'s
    /// success sense — they are an explicit abstention.
    pub seed_errors: u64,
}

/// The provenance path RECORDED for one scanned plan file: the scan root's
/// repo-relative prefix, joined with the file's own name, `/`-separated.
///
/// **Never the absolute path.** This string is what the adapter ships to coord
/// as `metadata.source_path` (and, for an archived plan, `metadata.archive_path`),
/// and onward to the plan library as `agent.work_artifacts.source_path` — a
/// corpus every machine in the fleet reads. An authoring machine's own
/// filesystem path resolves nowhere else, and resolving nowhere is
/// indistinguishable from the plan not existing. Measured 2026-09-06: 1177 of
/// 2648 served work units were in exactly that state, split between
/// `D:\qontinui-root\...` and `/home/<user>/...`, because this function used
/// to be `entry.path().to_string_lossy()`.
///
/// `root` is [`super::body_push::derive_source_repo`] of the scan dir — the
/// same two-component `<repo>/<dir relative to the repo root>` form the plan
/// library already stores as `source_repo`, so the two layers join by
/// construction: `source_path == source_repo + "/" + file name`.
fn relative_source_path(root: Option<&str>, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        // A directory entry always has a file name; fall back to the whole
        // path rather than dropping the entry, and normalize separators so
        // even this arm cannot emit a backslash.
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));
    match root {
        Some(r) if !r.is_empty() => format!("{r}/{name}"),
        _ => name,
    }
}

/// The scan source for one cycle: the REF where there is one, the working tree
/// only where the plans dir is not in a repo at all.
///
/// `Err` means **publish nothing this cycle** — the ref could not be
/// established, refreshed or read, so the previous corpus stands. That is the
/// whole point of Phase 2: a scan that cannot read its ref must not substitute
/// a working tree, because the substitution IS the defect
/// [policy: `unknown-must-not-render-as-a-default`].
///
/// The reason is RETURNED rather than logged here. A permanently unreadable
/// ref (a clone with no `origin/HEAD`; a gitignored plans dir) is ONE
/// unchanging fault, and a WARN per minute for it is how the line that matters
/// gets missed — the same rule [`ProcessGit::describe`] strips pids for. Only
/// the caller holds enough state to dedup it.
///
/// The parsed shape is identical on both arms — same slug, same
/// `source_path` — so moving the source does not churn a single coord row.
/// `read_plans_for_cycle_arms_agree` pins that by construction.
///
/// `Ok` carries a [`CycleScan`], whose `complete` flag says whether EVERY
/// plan the source listed was read. Both arms can come back short without
/// failing — the tree walk skips an unreadable or unstattable file, the ref
/// walk skips a blob that will not read or is not UTF-8 — and a short `Ok`
/// must not be read as the absence of what it skipped.
///
/// `pin` is the cycle's [`CycleRefPin`]: the ref is fetched and resolved to an
/// object id at most once per cycle, and the document half
/// ([`scan_roots_at_source`]) reads through the SAME pin — so the census this
/// returns and the bodies that half publishes are listed at one commit.
pub fn read_plans_for_cycle(
    dir: &Path,
    conv: &PlanConvention,
    git: &dyn GitRefReader,
    pin: &CycleRefPin,
) -> Result<CycleScan, String> {
    use super::ref_scan::{read_ref_dir_at, ScanSource};
    match pin.resolve_source(git, dir) {
        ScanSource::WorkTree => {
            let scan = scan_plan_dir(dir, conv);
            Ok(CycleScan {
                units: scan.units,
                // The tree walk's own completeness, carried through unchanged:
                // a short `units` here still must not read as absence.
                complete: scan.complete,
                // No ref was listed, so the REF side is ABSENT — UNKNOWN, never
                // an empty set. A zero here would be the claim that the default
                // branch holds no plans, which nothing measured.
                ref_census: None,
                dangling_link_slugs: scan.dangling_link_slugs,
            })
        }
        ScanSource::Unavailable { reason } => Err(reason),
        ScanSource::Ref {
            repo_root,
            ref_name,
            rel_dir,
        } => match read_ref_dir_at(
            git,
            &repo_root,
            &ref_name,
            // `resolve_source` just resolved this pair, so this is the pinned
            // answer, never a second fetch.
            pin.resolve_ref(git, &repo_root, &ref_name)?,
            &rel_dir,
        ) {
            Ok(listing) => {
                // Resolved ONCE per scan, as in `read_plan_dir`: it walks the
                // ancestor chain for a `.git` and every entry shares the answer.
                let source_root = super::body_push::derive_source_repo(dir);
                let ref_census = ref_census_from_names(&listing.names, listing.ref_sha.clone());
                // The census comes off the LISTING and `complete` qualifies the
                // BLOBS, so a partial blob read shortens `units` without
                // shortening the census — which is exactly why both travel.
                let complete = listing.complete;
                Ok(CycleScan {
                    units: listing
                        .files
                        .into_iter()
                        .map(|f| {
                            // The path a unit RECORDS is the one the tree scan
                            // would have recorded — the ref is where the bytes
                            // came from, not a different plan corpus.
                            let path = dir.join(&f.name);
                            let source_path = relative_source_path(source_root.as_deref(), &path);
                            let slug = slug_from_filename(&path.to_string_lossy());
                            parse_work_unit(&slug, &source_path, &f.body, conv)
                        })
                        .collect(),
                    complete,
                    ref_census: Some(ref_census),
                    dangling_link_slugs: Vec::new(),
                })
            }
            Err(e) => Err(format!(
                "could not read `{ref_name}:{rel_dir}` in {}: {e}",
                repo_root.display()
            )),
        },
    }
}

/// What one plans-dir scan produced — the parsed units, and whether the scan
/// was **complete**.
///
/// The flag exists because an empty (or short) `units` is otherwise
/// indistinguishable between *"the directory holds nothing"* and *"the
/// directory could not be read"* — the `silent-empty-is-unknown` shape. Every
/// consumer that reasons about ABSENCE (today: the disappeared-slug detector,
/// [`newly_disappeared_slugs`]) must gate on `complete`; a consumer that only
/// reasons about what it FOUND (the reconcile, the archive stamp) may ignore it,
/// because a short scan pushes less rather than concluding more.
///
/// Produced by BOTH scan sources: [`scan_plan_dir`] for a working tree, and
/// [`read_plans_for_cycle`]'s ref arm, whose per-blob skip
/// ([`super::ref_scan::read_ref_dir_at`]) is the same partial-read shape.
#[derive(Debug)]
pub struct PlanDirScan {
    pub units: Vec<ParsedWorkUnit>,
    /// `true` **iff** every plan the source listed was resolved and read. On
    /// the tree arm: the directory listing succeeded, every entry it yielded
    /// was resolved, and every `*.md` among them was STATTED and read. On the
    /// ref arm: every `*.md` blob the listing named was read as UTF-8. Any
    /// single failure makes it `false` — the PARTIAL read is the nastier
    /// shape, because the vector still looks plausible.
    pub complete: bool,
    /// Stems whose `*.md` NAME is in the directory as a DANGLING symlink —
    /// resolved as not-a-plan, so absent from `units` without clearing
    /// `complete` (see [`is_dangling_symlink`]).
    ///
    /// Carried because "not a plan" and "not there" are different claims, and
    /// the disappeared-slug detector asks the second: a link whose target is
    /// briefly missing (a non-atomic rewrite of the target) has NOT left the
    /// active dir, and warning about it would burn the slug's warn-once for
    /// the life of the process. Read ONLY as presence, never as a plan.
    ///
    /// The price, accepted on purpose: a link that stays broken for good is
    /// SILENT to the detector too, because its name never leaves. It is not
    /// silent overall — the scan logs it every cycle at debug, and the
    /// body-sync dry-run names it as a `dangling_symlink` skip — and a missed
    /// warning is the recoverable direction, where a false one burns the slug.
    pub dangling_link_slugs: Vec<String>,
}

/// What one `*.md` directory entry turned out to be, once its metadata was
/// resolved.
///
/// This exists as a named classification — rather than the [`Path::is_file`]
/// one-liner it replaced — because that call is
/// `fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)`: it FOLDS the IO
/// error into `false`, so an entry whose `stat` refuses is indistinguishable
/// from a directory named `x.md` and was dropped with no log line while
/// [`PlanDirScan::complete`] stayed `true`.
///
/// The shape is not hypothetical. A POSIX plans dir at mode `0o644` (read, no
/// execute — a botched `chmod -R a-x`, a tarball with odd modes, a restrictive
/// Windows ACL) lists every name from `read_dir`, which needs only `r`, and
/// then fails EACCES on every `stat`, which needs `x`. EIO/ESTALE on a network
/// or 9p mount reaches the same arm.
#[derive(Debug)]
enum PlanEntry {
    /// A regular file: read it.
    Read,
    /// Resolved, and genuinely not a plan — a DIRECTORY named `*.md`. Skipping
    /// it is the guard's legitimate purpose and costs the scan nothing.
    NotAPlan,
    /// A symlink whose target is not there — see [`is_dangling_symlink`].
    /// RESOLVED, like [`PlanEntry::NotAPlan`], and so it does not clear
    /// [`PlanDirScan::complete`]; kept as its own arm so the skip is logged
    /// under its real name rather than as a directory.
    DanglingLink,
    /// The listing yielded the name and `stat` refused it. A GAP, not a skip.
    Unstattable(std::io::Error),
}

/// Classify one entry from its metadata result. Pure, so the `Err` arm is
/// testable on every platform — including a box where no unprivileged process
/// can manufacture a real `stat` failure.
///
/// `lstat` is consulted ONLY on a failed `stat`, to tell the one DECIDED
/// failure (a dangling symlink) from the uncertain ones — see
/// [`is_dangling_symlink`].
fn classify_plan_entry(
    meta: std::io::Result<std::fs::Metadata>,
    lstat: impl FnOnce() -> std::io::Result<std::fs::Metadata>,
) -> PlanEntry {
    match meta {
        Ok(m) if m.is_file() => PlanEntry::Read,
        Ok(_) => PlanEntry::NotAPlan,
        Err(e) if is_dangling_symlink(&e, lstat) => PlanEntry::DanglingLink,
        Err(e) => PlanEntry::Unstattable(e),
    }
}

/// Whether a failed, link-FOLLOWING `stat` failed because the entry is a
/// DANGLING SYMLINK — `stat` says `NotFound` while `lstat` finds a symlink.
///
/// That is a DECIDED answer, and the only metadata failure that is: the entry
/// is a link, and there is no plan behind it. Both plan-dir walks (this
/// module's [`scan_plan_dir`] and `body_push`'s listing, which the work-tree
/// slug census is taken from) resolve it as NOT A PLAN rather than as a gap.
/// Treated as a gap it was PERMANENT darkness from a knowable cause: the link
/// answers the same way every cycle, so the work-tree census reported ABSENT
/// and this scan reported PARTIAL — disarming the disappeared-slug detector —
/// for as long as nobody noticed the link.
///
/// Not-a-plan is also what the REF side already says about the same entry:
/// `ProcessGit::list_ref_dir` skips every mode-`120000` entry, so a link is
/// never a ref stem either.
///
/// Deliberately narrow. Every OTHER failure still reads as a gap:
///
///  * `stat` refused for any other reason (`PermissionDenied`, EIO, ESTALE,
///    ELOOP) — the entry's kind is exactly what was not established.
///  * `stat` says `NotFound` and `lstat` does too — a file removed between
///    `read_dir` and `stat`, or one being replaced by an unlink-then-create
///    (a `git checkout` does exactly that). The name may be back a moment
///    later, so shrinking the stem set on it would under-report a plan that
///    exists; the gap costs one cycle and recovers by itself.
///  * `lstat` failing at all — nothing was decided.
///
/// A link that WORKED last cycle and breaks this one therefore drops its stem
/// from a COMPLETE scan's plans and from the work-tree census — which may be
/// only the one cycle a non-atomic rewrite of the TARGET takes, the same race
/// as a vanished plain file, landing on the decided side. That is tolerable
/// for both because neither remembers: the census is re-taken every report.
/// The one consumer that DOES remember — the disappeared-slug detector, whose
/// warn-once set is never pruned — is kept off it by
/// [`PlanDirScan::dangling_link_slugs`], which counts the link's NAME as
/// present.
///
/// `lstat` is a thunk so it is only issued for a `NotFound` — the one kind that
/// can be decided — rather than on every failed `stat`.
pub(super) fn is_dangling_symlink(
    stat_err: &std::io::Error,
    lstat: impl FnOnce() -> std::io::Result<std::fs::Metadata>,
) -> bool {
    stat_err.kind() == std::io::ErrorKind::NotFound
        && lstat().is_ok_and(|m| m.file_type().is_symlink())
}

/// The REF census of an already-taken listing.
///
/// One constructor for both doors — the scanning arm of
/// [`read_plans_for_cycle`] and the listing-only [`ref_census_only`] — so the
/// two cannot drift on the source label, on how a name becomes a stem, or on
/// whether the sha travels with it.
fn ref_census_from_names(
    names: &[String],
    ref_sha: Option<String>,
) -> super::body_push::PlanSlugCensus {
    super::body_push::PlanSlugCensus::new(
        super::body_push::SLUG_CENSUS_SOURCE_REF,
        ref_sha,
        names.iter().map(|n| slug_from_filename(n)),
    )
}

/// The REF side's stem census ALONE: one `git ls-tree` at the ref this clone
/// already holds. No fetch, no blob read, no parse, and no coord call.
///
/// This is what a cycle that will publish NO corpus can still honestly
/// produce. The work-unit write posture
/// ([`work_unit_write_posture`]) withholds coord work-unit READS AND WRITES on
/// a multi-bound (or unknown-bound) device — a standing property of the
/// device, not a passing failure. Withholding the census along with them made
/// the ref side of the coverage question ABSENT on every cycle FOREVER on
/// exactly the devices that hold both sides of it, and the first such cycle
/// cleared the stems the web had already stored, because a report whose
/// `censuses` omits a source stores that source as NULL. The cost objection
/// that justifies skipping the SCAN does not reach the listing: the expensive
/// part is the ~1,100 blob reads, and this door takes none of them.
///
/// Three outcomes, kept apart on purpose:
///
/// * `Ok(Some(census))` — the ref was listed; this is the reading.
/// * `Ok(None)` — there is no ref side to read at all (the plans dir is not in
///   a work tree, which is a supported layout). ABSENT, and an ANSWER.
/// * `Err(reason)` — the listing could not be taken. Also ABSENT, but it is
///   a FAULT, so the caller logs it (deduped — a clone with no `origin/HEAD`
///   is one unchanging fault, and a WARN a minute for it is how the line that
///   matters gets missed) [policy: `unknown-must-not-render-as-a-default`].
pub fn ref_census_only(
    dir: &Path,
    git: &dyn GitRefReader,
) -> Result<Option<super::body_push::PlanSlugCensus>, String> {
    use super::ref_scan::{list_ref_plan_names, resolve_ref_listing_source, ScanSource};
    match resolve_ref_listing_source(git, dir) {
        // Not in a repo: the same `None` the WorkTree arm of
        // `read_plans_for_cycle` reports. There is no ref, so there is no ref
        // set — never an empty one, which would claim a default branch holds
        // no plans.
        ScanSource::WorkTree => Ok(None),
        ScanSource::Unavailable { reason } => Err(reason),
        ScanSource::Ref {
            repo_root,
            ref_name,
            rel_dir,
        } => match list_ref_plan_names(git, &repo_root, &ref_name, &rel_dir) {
            Ok(listing) => Ok(Some(ref_census_from_names(&listing.names, listing.ref_sha))),
            Err(e) => Err(format!(
                "could not list `{ref_name}:{rel_dir}` in {}: {e}",
                repo_root.display()
            )),
        },
    }
}

/// One cycle's scan: the parsed units, and what the cycle can say about the
/// REF side's stem set.
///
/// Both halves of this adapter now publish from the fetched REF (the body sync
/// since Phase 3, through the same [`CycleRefPin`] on a writing cycle — on a
/// withheld cycle the census comes from the no-fetch [`ref_census_only`] and is
/// deliberately not pinned), but the scan-root report
/// still compares that ref against the WORKING TREE the checkout is parked
/// on — and one device is the only thing in the fleet that holds both
/// answers. This carries the ref half out to the scan-root report, where the
/// web can difference it against the tree half instead of computing a ratio
/// off one of them.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleScan {
    pub units: Vec<ParsedWorkUnit>,
    /// `true` **iff** every plan the source listed was resolved and read —
    /// the same flag [`PlanDirScan::complete`] carries, forwarded by both
    /// arms. It qualifies `units` ONLY: `ref_census` is taken from the
    /// LISTING, before any blob is read, so it survives the partial read this
    /// flag reports.
    pub complete: bool,
    /// The `ref` census this cycle listed. `None` on the WORK-TREE arm: the
    /// plans dir is not in a repo at all, so there is no ref side to report —
    /// ABSENT (UNKNOWN), never an empty set.
    pub ref_census: Option<super::body_push::PlanSlugCensus>,
    /// [`PlanDirScan::dangling_link_slugs`], forwarded by the WORK-TREE arm.
    /// Always empty on the ref arm, which skips every symlink at the listing.
    pub dangling_link_slugs: Vec<String>,
}

/// Read + parse every `*.md` in `dir` (non-recursive — the plans dir is flat,
/// matching coord's `walk_root`), reporting whether the walk was COMPLETE.
/// IO errors on individual files — a refused `stat` as well as a refused read
/// — are logged and skipped; a missing dir yields an empty vec. All of those
/// clear [`PlanDirScan::complete`]. A DIRECTORY named `*.md` and a DANGLING
/// symlink named `*.md` are the two entries skipped WITHOUT clearing it: each
/// is resolved, and genuinely not a plan (see [`is_dangling_symlink`]).
///
/// The absolute path is still what is OPENED and what is logged on an IO
/// error; only the path RECORDED on the parsed unit is made relative — see
/// [`relative_source_path`].
pub fn scan_plan_dir(dir: &Path, conv: &PlanConvention) -> PlanDirScan {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "plan adapter: cannot read plans dir");
            return PlanDirScan {
                units: Vec::new(),
                complete: false,
                dangling_link_slugs: Vec::new(),
            };
        }
    };
    // Resolved ONCE per scan, not per file: it walks the ancestor chain
    // looking for `.git`, and every entry in this directory shares the answer.
    let source_root = super::body_push::derive_source_repo(dir);
    let mut out = Vec::new();
    let mut complete = true;
    let mut dangling_link_slugs = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // A per-ENTRY failure: the listing started, but this name could
                // not be resolved. `flatten()` used to drop it silently, which
                // is exactly how a short read passed for an empty directory.
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "plan adapter: cannot read a plans-dir entry; scan is PARTIAL"
                );
                complete = false;
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        match classify_plan_entry(path.metadata(), || path.symlink_metadata()) {
            PlanEntry::Read => {}
            PlanEntry::NotAPlan => continue,
            PlanEntry::DanglingLink => {
                tracing::debug!(
                    path = %path.display(),
                    "plan adapter: a plans-dir entry is a dangling symlink; resolved as NOT a \
                     plan, so the scan stays COMPLETE"
                );
                dangling_link_slugs.push(slug_from_filename(&path.to_string_lossy()));
                continue;
            }
            PlanEntry::Unstattable(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "plan adapter: cannot stat a plans-dir entry; scan is PARTIAL"
                );
                complete = false;
                continue;
            }
        }
        let path_str = path.to_string_lossy().to_string();
        let source_path = relative_source_path(source_root.as_deref(), &path);
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let slug = slug_from_filename(&path_str);
                out.push(parse_work_unit(&slug, &source_path, &body, conv));
            }
            Err(e) => {
                tracing::warn!(path = %path_str, error = %e, "plan adapter: cannot read plan file");
                complete = false;
            }
        }
    }
    PlanDirScan {
        units: out,
        complete,
        dangling_link_slugs,
    }
}

/// [`scan_plan_dir`], for a caller that only reasons about what was FOUND.
///
/// **Never call this from a consumer that reasons about ABSENCE** — the
/// discarded `complete` flag is the only thing separating "nothing is there"
/// from "nothing could be read".
pub fn read_plan_dir(dir: &Path, conv: &PlanConvention) -> Vec<ParsedWorkUnit> {
    scan_plan_dir(dir, conv).units
}

/// Why a push was retired for the life of this process — **and how broadly**.
///
/// The two are not one condition wearing two hats, which is why they are not
/// one counter either: *"the principal lacks permission — fix the grant"* and
/// *"this plan file carries a status coord derives — fix the stamp"* share
/// nothing an operator can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementReason {
    /// coord answered `403` and named no clearing condition. A permission
    /// verdict is a property of the PRINCIPAL and the slug, not of any
    /// particular status, so this retires **every** status for that slug.
    ForbiddenPrincipal,
    /// coord answered `terminality: permanent` for one attempted status. coord
    /// scopes that word to the `(slug, status)` pair:
    /// [`crate::http_disposition::DenialTerminality::Permanent`] is documented
    /// *"permanently unsatisfiable for this (slug, status)"* — so this retires
    /// **that pair's status write only**.
    PermanentForStatus,
}

/// The process-lifetime retirement store: which pushes this process has stopped
/// attempting, and on whose authority.
///
/// **The key is the scope coord actually asserted, never broader.** A plan file
/// is an INPUT to the request coord refused, so the moment its parsed status
/// changes the request is no longer identical and the permanence coord asserted
/// no longer covers it. Keying a `permanent` denial on the slug alone would
/// mean the ordinary path (a plan stamped `vetted`, then edited to the corpus's
/// normal terminal word `shipped`) retired the slug FOREVER on its first
/// cycle, silently dropping every later edit — with no recovery short of a
/// runner restart, which served policy `production-and-cost`
/// `runner-lifecycle` forbids.
///
/// So a `permanent` retirement is keyed on the PAIR and self-clears when the
/// file changes; only the 403/principal class is slug-wide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetiredSlugs {
    /// Slugs coord answered `403` for, with no terminality naming a clearing
    /// condition. Every status is retired.
    principal: HashSet<String>,
    /// Attempted statuses coord answered `permanent` for, keyed by slug.
    permanent: HashMap<String, HashSet<String>>,
}

impl RetiredSlugs {
    /// The reason this `(slug, attempted status)` push is retired, or `None` if
    /// it is still to be attempted.
    pub fn retirement_for(&self, slug: &str, attempted_status: &str) -> Option<RetirementReason> {
        if self.principal.contains(slug) {
            return Some(RetirementReason::ForbiddenPrincipal);
        }
        if self
            .permanent
            .get(slug)
            .is_some_and(|statuses| statuses.contains(attempted_status))
        {
            return Some(RetirementReason::PermanentForStatus);
        }
        None
    }

    /// Retire every status for `slug` (a `403`). `true` when this is new, so
    /// the caller counts and WARNs exactly once per slug per process.
    pub fn retire_principal(&mut self, slug: &str) -> bool {
        self.principal.insert(slug.to_string())
    }

    /// Retire the `(slug, status)` pair (a `terminality: permanent` denial).
    /// `true` when this pair is new.
    pub fn retire_status(&mut self, slug: &str, attempted_status: &str) -> bool {
        self.permanent
            .entry(slug.to_string())
            .or_default()
            .insert(attempted_status.to_string())
    }

    /// No retirement of either class is recorded.
    pub fn is_empty(&self) -> bool {
        self.principal.is_empty() && self.permanent.is_empty()
    }
}

/// One coord refusal, normalised across the TWO error shapes this adapter's
/// sink produces — the READ routes' [`super::push::ForbiddenByCoord`] and the
/// WRITE routes' [`super::push::CoordWriteError`] — so the retirement decision
/// is made ONCE rather than once per shape.
struct CoordDenial {
    /// The route that refused, as the log has always spelled it.
    route: String,
    /// coord's response body, verbatim.
    detail: String,
    /// The HTTP status coord answered with (`403` by construction for the read
    /// shape, which exists only for that status).
    status: Option<u16>,
    /// The shared classifier's reading of it — disposition, coord's denial code
    /// and its `terminality` hint.
    verdict: crate::http_disposition::Verdict,
}

impl CoordDenial {
    /// True when this denial retires the PRINCIPAL for the slug — every status,
    /// for the life of the process.
    ///
    /// **Only a `403` that names no clearing condition.** Every `403` coord
    /// emits on the work-unit write routes carries a NON-permanent terminality
    /// (`self_attestation_forbidden` and `attester_unresolved` are
    /// `actor_dependent`, `owner_unresolved` is `state_dependent`), and coord
    /// names concrete events that clear each of them with the caller changing
    /// nothing. Retiring on those would suppress a denial a later cycle could
    /// legitimately clear — see [`crate::http_disposition::DenialTerminality`].
    ///
    /// An ABSENT or unrecognised hint still retires: that is a coord predating
    /// the hint, whose `403`s reproduced the log-flood this retirement closed,
    /// and UNKNOWN here must not silently re-open it.
    fn retires_the_principal(&self) -> bool {
        self.status == Some(403) && self.verdict.terminality.is_none()
    }
}

/// Recover the [`CoordDenial`] behind a push failure, whichever shape carries
/// it. `None` for a transport blip or a parse error — neither is a verdict.
fn denial_of(err: &anyhow::Error) -> Option<CoordDenial> {
    if let Some(f) = err.downcast_ref::<crate::plan_workunit_adapter::push::ForbiddenByCoord>() {
        return Some(CoordDenial {
            route: f.route.to_string(),
            detail: f.detail.clone(),
            status: Some(403),
            // The read shape keeps coord's body verbatim, so its terminality —
            // when a coord sends one on a read route — is readable here too,
            // through the same one classifier.
            verdict: crate::http_disposition::classify(Some(403), &f.detail),
        });
    }
    let w = super::push::coord_write_error(err)?;
    Some(CoordDenial {
        route: w.op.to_string(),
        detail: w.body.clone(),
        status: w.status,
        verdict: w.verdict(),
    })
}

/// Read every scan root through its RESOLVED source — the ref where there is
/// one, the working tree only where the dir is not in a repo at all.
///
/// The document-layer counterpart of [`read_plans_for_cycle`], and deliberately
/// the same three arms, because the two layers publishing from different bytes
/// is the defect this exists to close: Phase 2 moved the work-unit half onto
/// the ref and left this half on the tree, so on a checkout 717 commits behind
/// its default branch a plan amended on `origin/main` changed no file here, the
/// digest memory saw no change, and no write was ever issued — the artifact's
/// `updated_at` FROZE rather than going stale (finding 61b51044).
///
/// ## Two consequences this deliberately accepts, stated because the docs were
/// silent on them
///
/// **An unpushed or gitignored plans dir stops being CAPTURED.** `list_ref_dir`
/// turns git's non-zero exit into "does not exist in that ref", which lands on
/// the `Err` arm and contributes nothing. That reasoning was written for the
/// work-unit layer, where an unpushed plan genuinely is not tracked work. Here
/// it means the document corpus stops capturing a plan that exists only in the
/// tree — and because the sync is upsert-only, an already-published body
/// FREEZES rather than being corrected, which is this plan's own symptom
/// relocated. Note the asymmetry: a tenant authoring into a plain non-repo
/// directory keeps capture (the `WorkTree` arm), a tenant authoring into a repo
/// does not. That is a product call and it is made HERE, visibly, rather than
/// falling out of a branch nobody documented.
///
/// **A plan that is a SYMLINK stops being published.** `list_ref_dir` skips
/// mode `120000` (its blob is the link target, not the file), while the tree
/// walk's `classify_entry` follows links via `fs::metadata`. So the two arms do
/// NOT scan an identical set, and `body_sync_arms_agree` pins the
/// CLASSIFICATION half only — the listings differ here and on subdirectory skip
/// records. A symlinked plan published before this change freezes after it.
///
/// One contract difference from the work-unit arm, and it is why this returns a
/// partial set rather than an all-or-nothing `Option`: the body sync is
/// UPSERT-ONLY. `backfill_once` writes the artifacts it is handed and deletes
/// nothing, so a root that contributes nothing costs a refresh, never a
/// deletion. The work-unit reconcile has a disappearance concept and must
/// therefore publish nothing at all rather than publish a short set; here the
/// per-root independence is safe and strictly better, because one unreadable
/// root must not stop the others refreshing. That independence holds across
/// REPOS: roots sharing a repo share the pin's single fetch, so a failed fetch
/// takes all of them dark together for that cycle.
///
/// `pin` is the cycle's [`CycleRefPin`]. The reconcile loop hands in the SAME
/// pin [`read_plans_for_cycle`] resolved through, so the bodies published here
/// are listed at the commit the work-unit half's census was listed at, and two
/// roots in one repo share one fetch. A caller with no work-unit half (the
/// catch-up CLI, a withheld cycle) passes a fresh one.
pub fn scan_roots_at_source(
    roots: &[super::body_push::ScanRoot],
    conv: &PlanConvention,
    git: &dyn GitRefReader,
    pin: &CycleRefPin,
) -> (
    Vec<super::body_push::ScannedArtifact>,
    Vec<super::body_push::SkippedFile>,
) {
    use super::ref_scan::{read_ref_dir_at, ScanSource};
    let mut artifacts = Vec::new();
    let mut skipped = Vec::new();
    for root in roots {
        match pin.resolve_source(git, &root.dir) {
            // Not in a repo at all — a SUPPORTED layout (a tenant may author
            // into a plain directory), so the tree is the only source there is
            // and reading it is correct rather than a degradation.
            ScanSource::WorkTree => {
                artifacts.extend(super::body_push::scan_one_root(root, conv, &mut skipped));
            }
            ScanSource::Ref {
                repo_root,
                ref_name,
                rel_dir,
            } => match pin
                .resolve_ref(git, &repo_root, &ref_name)
                .and_then(|sha| read_ref_dir_at(git, &repo_root, &ref_name, sha, &rel_dir))
            {
                Ok(listing) => {
                    record_ref_listing_gaps(root, &listing, &mut skipped);
                    artifacts.extend(super::body_push::scan_one_root_at_ref(
                        root,
                        &listing.files,
                        conv,
                        &mut skipped,
                    ));
                }
                Err(e) => {
                    // NOT a fallback to the tree: substituting the tree here is
                    // the very defect this arm removes, and it would republish
                    // the parked bytes under the same identity
                    // [policy: `unknown-must-not-render-as-a-default`].
                    skipped.push(super::body_push::SkippedFile {
                        path: root.dir.to_string_lossy().to_string(),
                        reason: "unreadable_ref",
                    });
                    tracing::warn!(
                        root = %root.label,
                        repo_root = %repo_root.display(),
                        ref_name = %ref_name,
                        rel_dir = %rel_dir,
                        error = %e,
                        "plan library: could not read this scan root at the ref; \
                         it contributes nothing this cycle (the corpus keeps its last bodies)"
                    );
                }
            },
            ScanSource::Unavailable { reason } => {
                skipped.push(super::body_push::SkippedFile {
                    path: root.dir.to_string_lossy().to_string(),
                    reason: "scan_source_unavailable",
                });
                tracing::warn!(
                    root = %root.label,
                    dir = %root.dir.display(),
                    reason = %reason,
                    "plan library: scan source unavailable for this root; \
                     it contributes nothing this cycle (the corpus keeps its last bodies)"
                );
            }
        }
    }
    (artifacts, skipped)
}

/// Record what a ref listing could NOT read as `SkippedFile`s — the ref-arm
/// twin of the work-tree arm's `unreadable_file` / `unreadable_entry` records
/// in `body_push::scan_listing`.
///
/// [`super::ref_scan::read_ref_dir_at`] logs each blob it could not read and
/// clears [`super::ref_scan::RefListing::complete`], but until this existed
/// nothing consumed that flag and no skip was recorded — so a catch-up dry run
/// over a ref-sourced root reported a PARTIAL scan as a whole one: the missing
/// plan was absent from both the artifact count and the skipped list. Absence
/// of a report is not a report of absence
/// [policy: `unknown-must-not-render-as-a-default`].
///
/// Each stem the listing NAMED but did not return is recorded individually, at
/// the path the work-tree walk would have recorded it at. A short `read_blobs`
/// answer lands here too, because `names` is taken before any blob is read. An
/// incomplete listing with no missing name (which `read_ref_dir_at` cannot
/// produce today) is still recorded, at the root, so the flag can never be
/// cleared without a trace.
fn record_ref_listing_gaps(
    root: &super::body_push::ScanRoot,
    listing: &super::ref_scan::RefListing,
    skipped: &mut Vec<super::body_push::SkippedFile>,
) {
    if listing.complete {
        return;
    }
    let read: HashSet<&str> = listing.files.iter().map(|f| f.name.as_str()).collect();
    let before = skipped.len();
    for name in listing.names.iter().filter(|n| !read.contains(n.as_str())) {
        skipped.push(super::body_push::SkippedFile {
            path: root.dir.join(name).to_string_lossy().to_string(),
            reason: "unreadable_file",
        });
    }
    if skipped.len() == before {
        skipped.push(super::body_push::SkippedFile {
            path: root.dir.to_string_lossy().to_string(),
            reason: "unreadable_entry",
        });
    }
}

/// Push every parsed unit through the edge-trigger + conflict logic, updating
/// the client-side `last_applied` memory and the shared metrics. Pure of IO
/// beyond the sink, so it is unit-tested with a fake sink.
pub async fn reconcile_once<S: WorkUnitSink + ?Sized>(
    parsed_units: &[ParsedWorkUnit],
    last_applied: &mut HashMap<String, String>,
    last_deps: &mut HashMap<String, Vec<String>>,
    forbidden: &mut RetiredSlugs,
    forbidden_deps: &mut HashSet<String>,
    sink: &S,
    metrics: &AdapterMetrics,
) -> ReconcileSummary {
    let mut summary = ReconcileSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        // A status-block `Area:` the parser rejected: warned here, once per
        // plan per reconcile pass, and nowhere else — the parse is pure, and
        // the plan-library body sync parses the same files. The push still
        // runs, with `metadata.area` omitted. Deliberately repeated every
        // pass: the declaration stays visible until the plan file is fixed.
        if let Some(why) = &u.area_rejected {
            tracing::warn!(
                slug = %u.slug,
                path = %u.source_path,
                "plan adapter: status-block `Area:` rejected — {why}; \
                 metadata.area omitted for this plan"
            );
        }
        // A push coord has already refused is not re-issued: the request would
        // be byte-identical, so the verdict would be too. Stopping here — rather
        // than merely muting the log — is what makes this a fix and not a mute:
        // it also stops the HTTP call.
        //
        // **How much is stopped depends on WHAT coord scoped its refusal to**,
        // and the two classes are not interchangeable:
        //
        // - `ForbiddenPrincipal` (a `403` naming no clearing condition): a
        //   verdict about the principal, so EVERY request for this slug would
        //   be refused — the status-less metadata upsert included. Skip the
        //   unit whole.
        //
        // - `PermanentForStatus` (a `terminality: "permanent"` denial): a
        //   verdict about the `(slug, status)` PAIR of a STATUS WRITE. The
        //   status-less metadata upsert the same cycle would emit is a
        //   different request, and coord accepts it — it is exactly what the
        //   derived-status filter already degrades an upsert to. So the push
        //   still runs, in the metadata-only shape
        //   ([`super::push::StatusWrite::RetiredPermanently`]): no status write,
        //   no transition, no `current_status` GET, no `last_actor` GET — but
        //   `title`, `phases`, `source_path` and `depends_on` keep reaching
        //   coord. `continue`ing here instead would freeze a unit's provenance
        //   for the life of the process the first time its stamp reached
        //   `shipped`.
        //
        // The lookup takes the PARSED STATUS as well as the slug, because that
        // is the scope coord's permanence covers ([`RetiredSlugs`]): a file
        // edited off the refused word is a DIFFERENT request and is attempted
        // again, with no restart and no operator action beyond the edit.
        let status_write = match forbidden.retirement_for(&u.slug, &u.status) {
            Some(RetirementReason::ForbiddenPrincipal) => {
                summary.forbidden += 1;
                continue;
            }
            Some(RetirementReason::PermanentForStatus) => {
                summary.retired_permanent += 1;
                StatusWrite::RetiredPermanently
            }
            None => StatusWrite::Allowed,
        };
        // `last_applied` is per-PROCESS memory, rebuilt empty by `run_loop` on
        // every runner start. Without seeding, the first cycle after a start
        // sees `None` for EVERY slug — including slugs coord already has rows
        // for — and `decide_push` returns `UpsertWithStatus`, which sends
        // `status: Some(<file status>)`. That path bypasses BOTH safety
        // mechanisms below it:
        //
        //   * the agent-ownership deferral in `push_work_unit` is gated on
        //     `PushAction::Transition`, on the reasoning that an
        //     `UpsertWithStatus` is a brand-new unit with no agent owner to
        //     defer to — true per-process, NOT true per-row; and
        //   * the remote-divergence conflict check is inside
        //     `if let Some(prev) = last_applied`, so it is skipped entirely and
        //     logs nothing.
        //
        // coord then applies `status = COALESCE($3, work_units.status)`, and a
        // non-NULL `$3` OVERWRITES. Measured on this fleet 2026-09-01 against
        // 488 plan-backed units: 233 file statuses diverged from coord and 88
        // would have demoted a TERMINAL status (48 shipped -> in_progress,
        // 11 shipped -> partial, 5 shipped -> draft, ...) — silently, on every
        // runner start, not just the first.
        //
        // Seeding from the remote row restores the intended semantics: a unit
        // coord already knows becomes `RefreshOnly` (status unchanged, sends
        // `None`, COALESCE preserves) or a `Transition` that DOES pass through
        // the ownership deferral and conflict check. A slug coord has never
        // seen still reads `None` and is still created, which is correct.
        let mut prev = last_applied.get(&u.slug).cloned();
        // The seed read, when this cycle paid for one. `Some(_)` is a
        // SUCCESSFUL read and is handed to the push as `known_remote` so the
        // deferral's convergence check and the conflict check below it do not
        // re-read the same value: without it every seeded unit costs TWO GETs,
        // not one, and the cold cycle is ~2,400 serialized reads rather than
        // ~1,200. There is no spelling of the hint that means "my read failed"
        // — a failed read abstains and never reaches the push, which is exactly
        // the contract `push_work_unit_with_remote` documents.
        let mut seed_read: Option<Option<String>> = None;
        // A retired pair is NOT seeded. The push is forced to the metadata-only
        // shape whatever `prev` says, it puts no status on the wire, and it
        // reports nothing applied — so a seed read here would buy nothing and
        // would be re-paid EVERY cycle (the memory it would prime is never
        // written for a retired pair). The seed's whole purpose — stopping an
        // `UpsertWithStatus` from overwriting coord's status — cannot arise on
        // a push that sends no status.
        if prev.is_none() && status_write == StatusWrite::Allowed {
            match sink.current_status(&u.slug).await {
                // Coord already has this unit. Treat its status as what we last
                // applied so the edge-trigger compares against reality.
                Ok(remote) => {
                    if let Some(remote) = &remote {
                        summary.seeded += 1;
                        metrics.seeded_total.fetch_add(1, Ordering::Relaxed);
                        prev = Some(remote.clone());
                    }
                    // `Ok(None)` is genuinely absent -> a real create, and
                    // `prev` stays `None`, which is correct.
                    seed_read = Some(remote);
                }
                // UNKNOWN. Falling through with `prev = None` would re-create
                // the row from the file and is exactly the overwrite this seed
                // exists to prevent, so ABSTAIN: skip the unit this cycle and
                // count it. The next cycle retries; nothing is lost, because a
                // plan file that still differs is still there to be pushed.
                Err(e) => {
                    summary.seed_errors += 1;
                    metrics.seed_errors_total.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        slug = %u.slug,
                        error = %format!("{e:#}"),
                        "plan adapter: cannot read remote status to seed last-applied; \
                         SKIPPING this unit rather than risk overwriting coord's status"
                    );
                    continue;
                }
            }
        }
        let known_remote = seed_read.as_ref().map(|s| s.as_deref());
        match push_work_unit_with_status_write(sink, u, prev.as_deref(), known_remote, status_write)
            .await
        {
            Ok(outcome) => {
                if outcome.conflict {
                    summary.conflicts += 1;
                    metrics.conflicts_total.fetch_add(1, Ordering::Relaxed);
                }
                if matches!(outcome.kind, PushOutcomeKind::Transitioned { .. }) {
                    summary.transitions += 1;
                    metrics.transitions_total.fetch_add(1, Ordering::Relaxed);
                }
                let deferred = matches!(outcome.kind, PushOutcomeKind::Deferred { .. });
                if deferred {
                    summary.deferred += 1;
                    metrics.deferrals_total.fetch_add(1, Ordering::Relaxed);
                }
                // Record what we just applied so the next cycle is edge-triggered
                // — but ONLY what was actually applied, which the push reports
                // as `applied_status`: the value that went ON THE WIRE, or one
                // read back off coord in the same push. Three shapes apply
                // NOTHING and must record nothing.
                //
                // A status the derived-status filter WITHDREW from the upsert
                // (`shipped`, `ready`) is one of them, and the upsert still
                // SUCCEEDS — so recording `u.status` here would write a memory
                // of a word coord was never sent. That would arm the conflict
                // check (a `current_status` GET per slug per cycle) and then
                // make it announce `file wins (loud override)` every cycle
                // forever, for a race this writer had withdrawn from. A
                // permanently-retired pair's metadata-only push is another. See
                // [`super::push::PushOutcome::applied_status`].
                //
                // A deferral is the third. It
                // wrote NOTHING, so recording it as applied would be a lie with
                // two consequences: the next cycle would answer `RefreshOnly`
                // and stop re-checking (so a PERSISTENT deferral would be
                // counted exactly once, in the first cycle after start, and
                // every later cycle would log `deferred=0` — indistinguishable
                // from "no divergence", the very defect this counter closes);
                // and once the file moved again the stale memory would make
                // `push_work_unit`'s conflict check warn "file wins (loud
                // override)" every cycle forever while the file demonstrably did
                // not win. Leaving the memory untouched makes the deferral
                // re-evaluated every cycle, so `deferred` reads as a live gauge
                // of "units an agent currently owns and the file disagrees
                // with".
                if let Some(applied) = &outcome.applied_status {
                    last_applied.insert(u.slug.clone(), applied.clone());
                }

                // After the unit's upsert/transition succeeded, ALSO push its
                // dependency set to coord's first-class edge table (additive to
                // the metadata.depends_on JSONB fallback the upsert already
                // wrote). Best-effort: a 503 (table not migrated) is benign and
                // a hard error does NOT fail the reconcile — the unit already
                // landed and edges are additive. Edge-triggered: only re-send
                // when the dep set changed since we last applied it (the
                // replace-set is idempotent, so this is purely an optimization).
                let deps_changed =
                    !u.depends_on.is_empty() && last_deps.get(&u.slug) != Some(&u.depends_on);
                if deps_changed && forbidden_deps.contains(&u.slug) {
                    // Mirrors the top-level `forbidden` skip above, scoped to the
                    // deps route alone: coord refused THIS route for THIS slug
                    // before, and the replace-set would be byte-identical, so
                    // re-asking cannot change the verdict.
                    summary.deps_forbidden += 1;
                } else if deps_changed {
                    match sink.set_deps(&u.slug, &u.depends_on).await {
                        Ok(SetDepsOutcome::Ok { edges_set }) => {
                            summary.deps_set += 1;
                            metrics.deps_set_total.fetch_add(1, Ordering::Relaxed);
                            last_deps.insert(u.slug.clone(), u.depends_on.clone());
                            tracing::debug!(
                                slug = %u.slug,
                                edges_set,
                                "plan adapter: dep edges set on coord edge table"
                            );
                        }
                        Ok(SetDepsOutcome::TableNotMigrated) => {
                            summary.deps_skipped_unmigrated += 1;
                            metrics
                                .deps_skipped_unmigrated_total
                                .fetch_add(1, Ordering::Relaxed);
                            // Do NOT cache last_deps: the table isn't there yet,
                            // so we want to retry the edge write next cycle once
                            // the migration lands.
                            tracing::debug!(
                                slug = %u.slug,
                                "plan adapter: dep edge table not yet migrated; \
                                 JSONB fallback covers deps, will retry"
                            );
                        }
                        Err(e) => {
                            // Same distinction as the main push above, scoped to
                            // this route: a 403 here is settled and retired, an
                            // ordinary failure is retried every cycle (best-effort,
                            // as before — it still does not fail the reconcile).
                            //
                            // It goes through the SAME `denial_of` normaliser,
                            // and that is the fix rather than a tidy-up: this
                            // arm used to downcast to `ForbiddenByCoord` alone,
                            // a shape `HttpWorkUnitSink::set_deps` **cannot
                            // produce** — it builds a `CoordWriteError` for
                            // every non-2xx, and only `classify_failure` (the
                            // two READ routes) builds the other one. So in
                            // production a `403` landed in `deps_errors` and
                            // the identical replace-set was re-issued every
                            // cycle forever.
                            //
                            // This arm and the unit arm below deliberately
                            // disagree about which terminalities are settled:
                            // coord's work-unit write routes emit a
                            // `terminality` hint, `POST /coord/work-units/:slug/deps`
                            // emits none, so here the `403`-with-no-terminality
                            // test is the only signal there is. If coord ever
                            // answers this route `terminality: "permanent"` on
                            // a non-403 status, this arm falls through to
                            // `deps_errors` and retries every cycle — wire the
                            // `is_permanently_denied()` test in here (scoped
                            // to the deps route's own retirement set) that day.
                            if let Some(d) = denial_of(&e).filter(|d| d.retires_the_principal()) {
                                forbidden_deps.insert(u.slug.clone());
                                summary.deps_forbidden += 1;
                                metrics.deps_forbidden_total.fetch_add(1, Ordering::Relaxed);
                                tracing::warn!(
                                    slug = %u.slug,
                                    route = %d.route,
                                    detail = %d.detail,
                                    "plan adapter: coord refused this unit's dep-edge set (403); \
                                     retiring the edge push for the life of this process (the \
                                     unit's own upsert/transition route is unaffected). Restart \
                                     the runner after fixing the principal's permission."
                                );
                            } else {
                                summary.deps_errors += 1;
                                metrics.deps_errors_total.fetch_add(1, Ordering::Relaxed);
                                tracing::warn!(
                                    slug = %u.slug,
                                    error = %format!("{e:#}"),
                                    "plan adapter: dep-edge set failed (non-fatal; \
                                     unit upsert succeeded, edges are additive)"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // A 403 is a settled permission verdict, not a transient
                // failure. Retire the slug and say so ONCE; every later cycle
                // takes the `forbidden.contains` skip above and logs nothing.
                //
                // TWO shapes carry that verdict, and both must retire. The READ
                // routes funnel through `push::classify_failure` into
                // `ForbiddenByCoord`; the WRITE routes carry status + body in
                // `CoordWriteError` (which is strictly richer — it keeps coord's
                // machine-readable denial code). Routing only one of them here
                // would leave half the retry storm running.
                let denial = denial_of(&e);
                let verdict = denial.as_ref().map(|d| &d.verdict);
                // `permanent` FIRST: it is the narrowest retirement (the
                // `(slug, status)` pair), and a denial carrying it is settled
                // whatever its status code.
                if let Some(d) = denial
                    .as_ref()
                    .filter(|d| d.verdict.is_permanently_denied())
                {
                    // coord SAID this can never succeed. `terminality:
                    // "permanent"` is the one value in its closed three-element
                    // vocabulary that is unconditionally terminal — nothing
                    // invalidates it — so the pair is retired: one WARN per
                    // pair per process, and no status write on any later cycle
                    // while the file still parses to that status.
                    //
                    // `actor_dependent`, `state_dependent`, an absent hint (an
                    // older coord) and a word this build does not recognise all
                    // fall through — UNKNOWN retries, which is the pre-change
                    // behaviour.
                    //
                    // **The key is `u.status` — the status the FILE parsed to —
                    // regardless of whether the request that failed actually
                    // carried it.** Today that is exact, because every push
                    // shape that can produce a `permanent` denial does carry it
                    // (`UpsertWithStatus`'s `status` field, a `Transition`'s
                    // `to_status`); coord's `permanent` on a work-unit write is
                    // `status_is_derived`, which only a status-carrying body can
                    // trigger. If coord ever denied a status-LESS upsert
                    // permanently, this would retire a pair whose status was
                    // never sent — carry the sent status out of the push and
                    // key on that instead, that day.
                    if forbidden.retire_status(&u.slug, &u.status) {
                        metrics
                            .retired_permanent_total
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            slug = %u.slug,
                            route = %d.route,
                            detail = %d.detail,
                            denial = ?verdict.and_then(|v| v.denial.as_ref().map(|t| t.as_code())),
                            terminality = "permanent",
                            attempted_status = %u.status,
                            retirement_scope = "slug+status",
                            "plan adapter: coord refused this work unit permanently for the \
                             status it was asked to apply; retiring that (slug, status) pair's \
                             STATUS WRITE — coord states no change of actor or state can make \
                             an identical request succeed. The unit's status-less metadata \
                             upsert keeps running. REMEDIATION: edit the plan file's status \
                             stamp (a coord-DERIVED word such as `shipped`/`ready` is computed \
                             by coord and settable by nobody). The next parsed status that \
                             differs is pushed on the very next cycle — no restart, and none \
                             is possible (served policy `production-and-cost` \
                             `runner-lifecycle`)."
                        );
                    }
                    // ONE count per unit per cycle. A unit whose pair was
                    // ALREADY retired was counted by the pre-check above and
                    // then pushed in the metadata-only shape; if that
                    // status-less upsert is itself denied `permanent` (a shape
                    // coord does not produce today), that is a FAILED
                    // provenance push, not a second retirement — so it is an
                    // error, counted and said, rather than a silent drop.
                    if status_write == StatusWrite::Allowed {
                        summary.retired_permanent += 1;
                    } else {
                        summary.errors += 1;
                        metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            slug = %u.slug,
                            route = %d.route,
                            detail = %d.detail,
                            "plan adapter: the metadata-only push of a permanently-retired \
                             (slug, status) pair was refused"
                        );
                    }
                } else if let Some(d) = denial.as_ref().filter(|d| d.retires_the_principal()) {
                    if forbidden.retire_principal(&u.slug) {
                        metrics.forbidden_total.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            slug = %u.slug,
                            route = %d.route,
                            detail = %d.detail,
                            denial = ?verdict.and_then(|v| v.denial.as_ref().map(|t| t.as_code())),
                            retirement_scope = "slug",
                            "plan adapter: coord refused this work unit (403); retiring the \
                             slug for the life of this process — an identical retry \
                             cannot change the verdict. Restart the runner after \
                             fixing the principal's permission."
                        );
                    }
                    summary.forbidden += 1;
                } else {
                    summary.errors += 1;
                    metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                    // **This arm has no rate limit: one WARN per slug per
                    // cycle, for as long as the denial stands.** That is
                    // correct by doctrine — a denial whose `terminality` names
                    // a clearing condition (or names none on a non-403) is NOT
                    // settled, so it must keep being retried and must keep
                    // being visible. If a population of `actor_dependent`
                    // denials (a separation-of-duties flip) ever floods here,
                    // the fix is a per-slug log DAMPER — never a retirement,
                    // which would suppress a denial a later cycle could
                    // legitimately clear.
                    //
                    // The status coord answered with used to be formatted into
                    // the error string and thrown away here, so a `422`
                    // structural refusal and a `502` transport blip read
                    // identically in the log and to any code downstream.
                    // `CoordWriteError` now carries it, and the ONE shared
                    // classifier turns it into a verdict — `disposition` (retry
                    // or not) plus coord's own `denial` code and `terminality`
                    // when it named them. This is the arm those do NOT retire:
                    // an UNKNOWN or out-of-band-clearing verdict, which retries.
                    // Logging all three makes the distinction VISIBLE, which is
                    // what 33 hours of byte-identical cycle summaries never
                    // were.
                    tracing::warn!(
                        slug = %u.slug,
                        error = %format!("{e:#}"),
                        disposition = ?verdict.map(|v| v.disposition),
                        denial = ?verdict.and_then(|v| v.denial.as_ref().map(|d| d.as_code())),
                        terminality = ?verdict.and_then(|v| v.terminality),
                        "plan adapter: push failed"
                    );
                }
            }
        }
    }
    metrics.scanned.store(summary.scanned, Ordering::Relaxed);
    summary
}

/// One unit the agent-owner deferral suppressed, carried out of the backfill so
/// the caller can NAME them. `deferred=N` with no names is a number an operator
/// cannot act on, and the `info!` inside [`push_work_unit`] is below the CLI's
/// default filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredUnit {
    pub slug: String,
    /// `by_actor` of the unit's newest status-history row.
    pub owner: String,
    /// The status the file wanted to apply, and did not.
    pub wanted: String,
}

/// Outcome of one [`backfill_work_units_once`] pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkUnitBackfillSummary {
    /// Plan files parsed into work units.
    pub scanned: u64,
    /// Units that did not exist in coord and were created WITH their status.
    pub created: u64,
    /// Units already carrying the file's status — title/metadata refreshed,
    /// no status write, no history row.
    pub refreshed: u64,
    /// Units whose coord status differed from the file's and were moved.
    pub transitioned: u64,
    /// Transitions the agent-owner deferral suppressed
    /// ([`PushOutcomeKind::Deferred`]).
    pub deferred: u64,
    /// Units whose read or push errored, excluding a permanent refusal of the
    /// status transition (see `refused_permanently`). The pass continues past
    /// each one.
    pub failed: u64,
    /// Units whose status TRANSITION coord refused with `terminality: "permanent"` — in
    /// practice a file stamped with a coord-DERIVED word (`shipped`/`ready`)
    /// that coord does not yet hold. Not a failure: the unit's metadata upsert
    /// runs before the refused status write, and no retry of the identical
    /// request could succeed. The operator action, if any, is the stamp.
    pub refused_permanently: u64,
    /// The deferred units, named. `deferred == deferred_units.len()`.
    pub deferred_units: Vec<DeferredUnit>,
}

/// One-shot **work-unit** backfill: the catch-up path for a machine whose
/// reconcile loop never ran.
///
/// Sibling of the plan-library body backfill
/// (`super::body_push::backfill_once`, driven by
/// `qontinui-pr plan-library-backfill`) — same scanner, same one-shot shape —
/// but it drives `coord.work_units` through [`push_work_unit`] instead of
/// pushing bodies to `agent.work_artifacts`. Neither one is a substitute for
/// the other: they fill different halves of the corpus, and until this existed
/// the work-unit half had **no** catch-up path at all, so an unconfigured
/// runner's ingestion gap could only be closed by arming the tier and waiting
/// for a future runner start.
///
/// ## Why it seeds `last_applied` from coord instead of starting empty
///
/// [`reconcile_once`] carries a client-side last-applied memory that a
/// long-lived loop accumulates. A one-shot has none — and starting from an
/// empty map would make [`super::push::decide_push`] answer `UpsertWithStatus` for **every**
/// unit, which writes a status unconditionally: it would clobber a status an
/// agent had set, and a second run would churn the whole corpus. So each unit's
/// seed is coord's CURRENT status, read from the sink. That makes the three
/// arms fall out correctly and makes the run idempotent by construction:
///
/// - absent in coord → seed `None` → `UpsertWithStatus` → **created**;
/// - present with the same status → `RefreshOnly` → metadata-only upsert;
/// - present with a different status → `Transition` → and therefore **through
///   the agent-owner deferral** ([`push_work_unit`]'s P2a gate), which is the
///   only arm that gate covers. A backfill that started from an empty memory
///   would route every unit down `UpsertWithStatus` and bypass the deferral
///   entirely — silently overwriting exactly the statuses it protects.
///
/// Dependency edges are deliberately NOT pushed here: `build_metadata` already
/// carries `depends_on` in the `metadata` JSONB (the documented fallback), and
/// the edge table is the reconcile loop's incremental business.
pub async fn backfill_work_units_once<S: WorkUnitSink + ?Sized>(
    parsed_units: &[ParsedWorkUnit],
    sink: &S,
) -> WorkUnitBackfillSummary {
    let mut summary = WorkUnitBackfillSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        let seed = match sink.current_status(&u.slug).await {
            Ok(s) => s,
            Err(e) => {
                summary.failed += 1;
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan backfill: cannot read current work-unit status; skipping this unit \
                     (a push with an unknown seed could clobber a status an agent set)"
                );
                continue;
            }
        };
        // Hand the already-read status through: `push_work_unit` would
        // otherwise re-read it to run a conflict check against a `prev` that IS
        // that read — an answer fixed by construction, bought with a second GET
        // per existing unit.
        match push_work_unit_with_remote(sink, u, seed.as_deref(), Some(seed.as_deref())).await {
            Ok(outcome) => match outcome.kind {
                PushOutcomeKind::Created => summary.created += 1,
                PushOutcomeKind::Refreshed => summary.refreshed += 1,
                PushOutcomeKind::Transitioned { .. } => summary.transitioned += 1,
                PushOutcomeKind::Deferred { owner, wanted } => {
                    summary.deferred += 1;
                    summary.deferred_units.push(DeferredUnit {
                        slug: u.slug.clone(),
                        owner,
                        wanted,
                    });
                }
            },
            Err(e) => {
                // Only a permanent refusal of the TRANSITION qualifies: the
                // `Transition` arm's status-less upsert must have succeeded
                // before the transition was sent, which is what makes
                // "metadata was refreshed" true. A permanent refusal of the
                // upsert itself, or of a read, means nothing landed — that is
                // a failure, whatever its terminality.
                let transition_refused_permanently = denial_of(&e)
                    .is_some_and(|d| d.verdict.is_permanently_denied())
                    && super::push::coord_write_error(&e).is_some_and(|w| w.op == "transition");
                if transition_refused_permanently {
                    summary.refused_permanently += 1;
                    tracing::info!(
                        slug = %u.slug,
                        attempted_status = %u.status,
                        "plan backfill: coord refused this unit's status permanently (metadata \
                         was refreshed); not a failure — edit the stamp if it is wrong"
                    );
                } else {
                    summary.failed += 1;
                    tracing::warn!(
                        slug = %u.slug,
                        error = %format!("{e:#}"),
                        "plan backfill: push failed"
                    );
                }
            }
        }
    }
    summary
}

/// Outcome of one metadata-only archive scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArchiveSummary {
    /// Archived plans scanned this cycle.
    pub scanned: u64,
    /// `metadata.archive_path` stamps written (metadata-only upserts).
    pub stamped: u64,
    /// Per-unit archive-upsert errors.
    pub errors: u64,
}

/// Metadata-only reconcile of the **archive** directory (D4). For every plan
/// found in the archive dir, stamp `metadata.archive_path` provenance via
/// [`push_archive_metadata`] — **never** a status transition. Pure of IO beyond
/// the sink, so it is unit-tested with a fake sink.
///
/// The archive scan carries no client-side edge-trigger memory: an archived
/// plan is terminal, its `archive_path` is stable, and the upsert is idempotent,
/// so re-stamping each cycle is harmless (and re-asserts provenance a coord
/// restart might have missed). It records nothing into `last_applied`, so it can
/// never influence the active-dir transition path.
pub async fn reconcile_archive_once<S: WorkUnitSink + ?Sized>(
    archived_units: &[ParsedWorkUnit],
    sink: &S,
    metrics: &AdapterMetrics,
) -> ArchiveSummary {
    let mut summary = ArchiveSummary {
        scanned: archived_units.len() as u64,
        ..Default::default()
    };
    for u in archived_units {
        match push_archive_metadata(sink, u).await {
            Ok(()) => {
                summary.stamped += 1;
                metrics
                    .archive_stamped_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                summary.errors += 1;
                metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan adapter: archive metadata stamp failed"
                );
            }
        }
    }
    summary
}

/// Pure disappeared-slug detection (D4). A slug we have previously SEEN
/// (present in `known`) that is now absent from BOTH the active scan
/// (`active_slugs`) and the archive scan (`archive_slugs`), and has not already
/// been warned about (`warned`), is "disappeared": the plan file left the
/// active dir without landing in the archive. Returns those newly-disappeared
/// slugs and records them in `warned` so each is surfaced **once per process**.
///
/// **`known` is the set of slugs the active scan has SEEN, never the set whose
/// status was APPLIED.** Those two look interchangeable and are not: the
/// apply-memory (`last_applied`) records only what went on the wire, so it
/// never holds a unit whose status this adapter withdrew (`shipped`, `ready`),
/// nor one the agent-owner deferral suppressed, nor one whose `(slug, status)`
/// pair coord permanently denied. Keyed on the apply-memory, the detector
/// would go blind to **exactly the plans most likely to be moved or deleted**
/// — a `shipped` plan being consolidated is the whole case it exists for.
///
/// The caller only *warns* on the result — the work unit is left untouched.
/// Terminal state is owned by coord's derive engine; the adapter must never
/// push `shipped`/`archived` to fill the gap (a second-writer race).
///
/// **The caller MUST establish that both scans were COMPLETE before calling
/// this** ([`PlanDirScan::complete`]). This function is pure set arithmetic: it
/// cannot tell a directory that holds nothing from one that could not be read,
/// and on the second it reports the entire corpus disappeared AND poisons
/// `warned`, which is warn-once per process — permanent blindness from one
/// transient IO fault. See the guard in `LoopState::tick`.
pub fn newly_disappeared_slugs(
    known: &HashSet<String>,
    active_slugs: &HashSet<String>,
    archive_slugs: &HashSet<String>,
    warned: &mut HashSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for slug in known {
        if !active_slugs.contains(slug) && !archive_slugs.contains(slug) && !warned.contains(slug) {
            warned.insert(slug.clone());
            out.push(slug.clone());
        }
    }
    out
}

/// The path settings the adapter resolves every tick — the three
/// `PathSettings` directories exactly as configured. Blank and unset are both
/// "unset"; the resolvers ([`resolve_plans_dir`] and siblings) normalise them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathInputs {
    pub plans_dir: Option<String>,
    pub plans_archive_dir: Option<String>,
    pub prompts_dir: Option<String>,
}

/// Per-tick reader of the path settings.
///
/// A closure rather than a value for the same reason as [`CaptureGate`]: the
/// settings store lives in the runner binary, this loop lives in the lib
/// crate, and the value must be re-read on every cycle so an edit made in the
/// settings UI takes effect within one interval — never at "the next runner
/// start", which fleet policy forbids anyway.
pub type PathReader = std::sync::Arc<dyn Fn() -> PathInputs + Send + Sync>;

/// One tick's resolution of [`PathInputs`] through the three resolvers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedDirs {
    plans: Option<String>,
    archive: Option<String>,
    prompts: Option<String>,
}

impl ResolvedDirs {
    /// The reconcile loop resolves the **device default** — no tenant, and
    /// therefore no per-tenant map — and that is a deliberate, named limitation
    /// rather than an oversight.
    ///
    /// Scanning N per-tenant roots requires attributing each root's pushes to
    /// *its own* tenant, and that attribution comes from the credential, never
    /// from which directory a file was found in (plan
    /// `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`
    /// §2 D1). Pushing tenant B's directory under a bearer whose organization is
    /// A's would file B's plans as A's while *claiming* to be declared — strictly
    /// worse than today's fusion. So the multi-root scan is gated on the corpus's
    /// tenant axis landing, and until then the empty map plus `None` tenant makes
    /// the fall-through to the scalar explicit at the call site: per-tenant
    /// *authoring* works from the session-launch side, per-tenant *capture*
    /// waits.
    fn resolve(inputs: PathInputs) -> Self {
        let no_tenant_overrides = BTreeMap::new();
        Self {
            plans: resolve_plans_dir(inputs.plans_dir, &no_tenant_overrides, None),
            archive: resolve_plans_archive_dir(
                inputs.plans_archive_dir,
                &no_tenant_overrides,
                None,
            ),
            prompts: resolve_prompts_dir(inputs.prompts_dir, &no_tenant_overrides, None),
        }
    }
}

/// Everything one reconcile loop carries from tick to tick.
///
/// Factored out of [`run_loop`] so a tick is a plain `async fn` a test can
/// drive without timers — the loop itself is nothing but
/// `interval.tick().await; state.tick(..).await`.
struct LoopState {
    conv: PlanConvention,
    paths: PathReader,
    /// The body-sync sink, when the sync is enabled and a backend resolved at
    /// spawn. The sync itself ([`BodySync`]) is rebuilt from it whenever the
    /// resolved root set changes.
    body_sync_sink: Option<super::body_push::HttpArtifactSink>,
    capture_gate: CaptureGate,
    body_sync: Option<BodySync>,
    /// The resolution the current `body_sync` and the `scan_roots` gauge were
    /// built from. `None` before the first tick, so the first tick always
    /// builds them and always logs the tier's state.
    resolved: Option<ResolvedDirs>,
    last_applied: HashMap<String, String>,
    last_deps: HashMap<String, Vec<String>>,
    /// Every slug the ACTIVE scan has parsed since the current plans dir was
    /// resolved — the disappeared-slug detector's `known` set.
    ///
    /// Deliberately NOT `last_applied.keys()`. That map holds only units whose
    /// status actually went on the wire, which silently excludes the three
    /// populations the detector most needs to cover: a coord-derived stamp
    /// (`shipped`/`ready`) the write-side filter withdrew, a unit the
    /// agent-owner deferral suppressed, and a `(slug, status)` pair coord
    /// permanently denied. See [`newly_disappeared_slugs`].
    seen_slugs: HashSet<String>,
    warned_disappeared: HashSet<String>,
    /// Whether the current streak of detector-skipping cycles has already
    /// been WARNed about. Cleared by any cycle whose scans were complete.
    detector_skip_warned: bool,
    /// Pushes coord has refused for good — a `403` on the principal (every
    /// status for that slug) or a `terminality: permanent` on one
    /// `(slug, status)` pair's status write. Owned by the loop (there is
    /// exactly one per process), so "retired" means "for this process's
    /// lifetime" — which is why the permanent class is keyed on the pair: a
    /// plan file edited off the refused word re-arms itself, and a runner
    /// restart (the only thing that would clear a slug-wide retirement) is
    /// forbidden by served policy `production-and-cost` `runner-lifecycle`.
    forbidden: RetiredSlugs,
    /// Same, scoped to the dep-edge route alone: coord evaluates the unit's own
    /// upsert/transition route and the edge-table route as separate authorization
    /// checks, so a deps-only 403 must not retire the whole unit. See
    /// [`AdapterMetrics::deps_forbidden_total`].
    forbidden_deps: HashSet<String>,
    /// The `Unavailable` reason last WARNed for, so one unchanging fault —
    /// a clone with no `origin/HEAD`, a plans dir absent from the ref — costs
    /// one line rather than one per minute. Cleared by any cycle that reads,
    /// so a recurrence after a recovery is news again.
    last_scan_unavailable: Option<String>,
    /// The git reader the per-cycle scan-divergence measurement uses.
    /// [`ProcessGit`] in production; injected in tests so a tick neither
    /// shells out to a real `git` nor needs a real repo on disk.
    git: std::sync::Arc<dyn GitRefReader>,
    /// Whether the cold-start BULK seed has been attempted for the current
    /// corpus. `false` means the next tick will try to prime `last_applied`
    /// from one paged read instead of paying `reconcile_once`'s per-slug seed
    /// on every plan. Re-armed at two sites, both of which clear `last_applied`
    /// and would otherwise leave the per-slug fallback to pay one round-trip
    /// per plan: [`LoopState::apply_resolution`] whenever the active plans dir
    /// moves, and [`LoopState::tick`] when work-unit writes RESUME after a
    /// withheld [`WorkUnitWritePosture`] (the memory is frozen at the last
    /// Write cycle, so the resumed cycle must re-prime from coord).
    bulk_seeded: bool,
    /// Test-only: the scan-root reporter every rebuilt [`BodySync`] gets
    /// instead of its sink, so a tick-level test can observe reports.
    #[cfg(test)]
    scan_reporter: Option<std::sync::Arc<dyn super::body_push::ScanRootReporter>>,
    /// Handed to every rebuilt [`BodySync`] — see [`ScanReportGate`]. Closed
    /// until [`Self::with_scan_report_gate`] supplies the binary's predicate.
    scan_report_gate: ScanReportGate,
    /// How many tenants this device is bound to, read once per cycle for
    /// [`work_unit_write_posture`]. [`read_device_binding_count`] in
    /// production; a constant in tests (see [`LoopState::new`]).
    binding_count: std::sync::Arc<dyn Fn() -> BindingCountReading + Send + Sync>,
    /// The posture last logged, so a standing state costs one line.
    last_write_posture: Option<WorkUnitWritePosture>,
    /// Consecutive cycles in [`WorkUnitWritePosture::WithheldBindingsUnknown`].
    /// The first is expected on a fresh boot (the adapter's first tick can beat
    /// the first register heartbeat) and logs at info; a second in a row warns.
    unknown_posture_streak: u32,
}

impl LoopState {
    fn new(
        paths: PathReader,
        body_sync_sink: Option<super::body_push::HttpArtifactSink>,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            conv: PlanConvention::operator_default(),
            paths,
            git: std::sync::Arc::new(ProcessGit),
            body_sync_sink,
            capture_gate,
            body_sync: None,
            resolved: None,
            last_applied: HashMap::new(),
            last_deps: HashMap::new(),
            seen_slugs: HashSet::new(),
            warned_disappeared: HashSet::new(),
            detector_skip_warned: false,
            last_scan_unavailable: None,
            forbidden: RetiredSlugs::default(),
            forbidden_deps: HashSet::new(),
            bulk_seeded: false,
            #[cfg(test)]
            scan_reporter: None,
            scan_report_gate: scan_report_gate_closed(),
            // Tests must never read the operator's REAL `paired_user.json`
            // (a two-tenant dev box would withhold every tick-level test's
            // pushes); they state a count through `with_binding_count`.
            #[cfg(not(test))]
            binding_count: std::sync::Arc::new(read_device_binding_count),
            #[cfg(test)]
            binding_count: std::sync::Arc::new(|| BindingCountReading {
                local: 1,
                coord: Some(1),
            }),
            last_write_posture: None,
            unknown_posture_streak: 0,
        }
    }

    /// State the device's binding count. Test-only, like [`Self::with_git`]:
    /// a production seam that could be swapped at runtime would be a way to
    /// quietly disarm the multi-bound gate.
    #[cfg(test)]
    fn with_binding_count(mut self, local: usize, coord: Option<usize>) -> Self {
        self.binding_count = std::sync::Arc::new(move || BindingCountReading { local, coord });
        self
    }

    /// Like [`Self::with_binding_count`], but the reading lives in a cell the
    /// test can move between ticks — how a posture FLIP is staged.
    #[cfg(test)]
    fn with_binding_count_cell(
        mut self,
        cell: std::sync::Arc<std::sync::Mutex<BindingCountReading>>,
    ) -> Self {
        self.binding_count =
            std::sync::Arc::new(move || *cell.lock().unwrap_or_else(|e| e.into_inner()));
        self
    }

    /// Supply the instance-ownership predicate every rebuilt [`BodySync`]
    /// gates its scan-root reports on — see [`ScanReportGate`].
    fn with_scan_report_gate(mut self, gate: ScanReportGate) -> Self {
        self.scan_report_gate = gate;
        self
    }

    /// Route every rebuilt [`BodySync`]'s scan-root reports to `reporter`.
    /// Test-only, like [`Self::with_git`].
    #[cfg(test)]
    fn with_scan_reporter(
        mut self,
        reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
    ) -> Self {
        self.scan_reporter = Some(reporter);
        self
    }

    /// Swap in a different [`GitRefReader`]. Test-only: production always
    /// wants [`ProcessGit`], and a seam that can be reconfigured at runtime
    /// would be a way for the detector to be quietly disarmed.
    #[cfg(test)]
    fn with_git(mut self, git: std::sync::Arc<dyn GitRefReader>) -> Self {
        self.git = git;
        self
    }

    /// The path settings changed (or this is the first tick): publish the new
    /// resolution to the metrics, rebuild the body sync's scan roots, re-seed
    /// the edge-detection memory when the active dir moved, and log the
    /// tier's state — **once per transition**, here, never per tick.
    fn apply_resolution(&mut self, resolved: ResolvedDirs, metrics: &AdapterMetrics) {
        let previous = self.resolved.replace(resolved.clone());
        let roots = super::body_push::scan_roots(
            resolved.plans.clone(),
            resolved.archive.clone(),
            resolved.prompts.clone(),
        );
        let root_count = roots.len();
        metrics
            .scan_roots
            .store(root_count as u64, Ordering::Relaxed);
        metrics
            .path_resolutions_total
            .fetch_add(1, Ordering::Relaxed);
        *metrics
            .active_plans_dir
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = resolved.plans.clone();
        // Rebuilding the body sync resets its digest memory — a re-seed, the
        // same posture as a supervised restart: nothing is replayed, the next
        // cycle re-reads what is on disk and pushes only what differs from
        // what the backend already holds.
        self.body_sync = self.body_sync_sink.as_ref().map(|sink| {
            BodySync::new(roots, sink.clone(), self.capture_gate.clone())
                // The loop's OWN reader, so the document layer, the work-unit
                // layer and the divergence probe cannot look at three different
                // gits — and so a tick-level test drives all three with one
                // injected fake.
                .with_git(std::sync::Arc::clone(&self.git))
                .with_scan_report_gate(self.scan_report_gate.clone())
        });
        #[cfg(test)]
        if let Some(reporter) = self.scan_reporter.clone() {
            self.body_sync = self.body_sync.take().map(|bs| bs.with_reporter(reporter));
        }

        let first_tick = previous.is_none();
        let previous_plans = previous.and_then(|p| p.plans);
        if !first_tick && previous_plans == resolved.plans {
            tracing::info!(
                archive_dir = ?resolved.archive,
                prompts_dir = ?resolved.prompts,
                scan_roots = root_count,
                "plan adapter: path settings changed; scan roots rebuilt"
            );
            return;
        }
        // A different active dir is a different corpus: the edge memory keyed
        // by slug would otherwise flag every old slug as disappeared and
        // suppress a legitimate first-seen transition in the new one.
        // `forbidden` stays — a 403 is coord's verdict on the slug, not on
        // where its file lives.
        self.last_applied.clear();
        self.last_deps.clear();
        self.seen_slugs.clear();
        self.warned_disappeared.clear();
        // `last_applied` is empty again, so the corpus is cold again: re-arm the
        // bulk seed rather than leave the per-slug fallback to pay one
        // round-trip per plan on the next cycle.
        self.bulk_seeded = false;
        match &resolved.plans {
            Some(dir) => tracing::info!(
                dir = %dir,
                archive_dir = ?resolved.archive,
                prompts_dir = ?resolved.prompts,
                scan_roots = root_count,
                "plan adapter: markdown-plan tier is ON — scanning the active plans dir every cycle"
            ),
            // Say it out loud, at `info`, and NAME what arms the tier so the
            // reader does not have to find this function to learn it. A
            // silent idle here is indistinguishable from a healthy scan — the
            // `silent-empty-is-unknown` shape — and it is exactly how a
            // fleet-wide work-unit ingestion gap went unreported for months.
            None => tracing::info!(
                setting = "paths.plans_dir",
                "plan adapter: markdown-plan tier is OFF on this machine — no active plans \
                 dir is configured, so NO plan file is scanned and NO work unit is pushed to \
                 coord from this runner. Arm it by setting `paths.plans_dir` in the Paths \
                 section of the runner's settings; it takes effect within one scan interval, \
                 no restart needed. Catch a machine up immediately with \
                 `qontinui-pr plan-workunit-backfill --plans-dir <dir>`"
            ),
        }
    }

    /// Cold-start BULK seed, attempted once per corpus.
    ///
    /// [`reconcile_once`] already seeds `last_applied` per slug when this
    /// process has no memory of it — that is the CORRECTNESS path and it
    /// abstains rather than overwriting. But it costs one `current_status`
    /// round-trip per plan on the first cycle after a runner start (~1,200
    /// serialized GETs on this fleet). One paged [`WorkUnitSink::list_statuses`]
    /// read collapses that.
    ///
    /// Failure here is deliberately NON-FATAL in every arm, and the two arms
    /// differ in whether they are TERMINAL. A sink with no bulk door returns
    /// `Ok(None)` and will not grow one mid-process, so that arm retires the
    /// attempt. An `Err` is logged and RETRIED on the next tick — a transient
    /// failure must not retire the seed for the life of the process, because
    /// the likeliest moment for one is the first cycle after a runner start.
    /// Either way the per-slug seed still runs; nothing about correctness
    /// depends on this method succeeding.
    ///
    /// Only slugs ACTUALLY IN the scanned dir are primed. The memory is this
    /// corpus's edge-trigger state; a coord-native unit that was never
    /// plan-backed has no business in it. (The disappeared-slug detector no
    /// longer reads this map at all — it keys on [`LoopState::seen_slugs`].)
    async fn bulk_seed<S: WorkUnitSink + ?Sized>(
        &mut self,
        units: &[ParsedWorkUnit],
        sink: &S,
        metrics: &AdapterMetrics,
    ) {
        // An EMPTY corpus must not consume the one attempt: `units` is empty
        // both when the plans dir has not been populated yet and when the
        // `spawn_blocking` walk failed with a JoinError. Burning the flag there
        // primes nothing and never retries until the dir path changes.
        if self.bulk_seeded || units.is_empty() {
            return;
        }
        match sink.list_statuses().await {
            Ok(Some(remote)) => {
                // Arm only on a COMPLETED read. Arming before the await would
                // let a transient failure retire the seed permanently — and the
                // likeliest moment for that failure is the first cycle after a
                // runner start, when coord may not be reachable yet, which is
                // precisely the cycle the bulk read exists to make cheap.
                self.bulk_seeded = true;
                let mut primed = 0u64;
                for u in units {
                    if let Some(status) = remote.get(&u.slug) {
                        self.last_applied.insert(u.slug.clone(), status.clone());
                        primed += 1;
                    }
                }
                metrics.seeded_total.fetch_add(primed, Ordering::Relaxed);
                tracing::info!(
                    primed,
                    remote_units = remote.len(),
                    scanned = units.len(),
                    "plan adapter: cold-start bulk seed applied"
                );
            }
            Ok(None) => {
                // A sink with no bulk door will not grow one mid-process, so
                // this arm IS terminal — unlike the error arm below it.
                self.bulk_seeded = true;
                tracing::debug!("plan adapter: sink has no bulk seed door; per-slug seed only");
            }
            Err(e) => {
                // Deliberately leaves `bulk_seeded` false: the next tick retries.
                // The per-slug seed carries correctness in the meantime, and it
                // abstains rather than overwriting, so a retry costs only reads.
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "plan adapter: bulk seed failed; retrying next cycle, per-slug seed meanwhile"
                );
            }
        }
    }

    /// Re-resolve the path settings, rebuild whatever depends on them if they
    /// moved, then run one reconcile cycle — or idle, when no active plans dir
    /// is configured.
    ///
    /// Each cycle: reconcile the active dir (edge-triggered status
    /// transitions), then — when an archive dir is configured — metadata-only
    /// stamp every archived plan's `archive_path` (never a transition, D4),
    /// then warn once about any slug that vanished from both dirs.
    async fn tick<S: WorkUnitSink + ?Sized>(&mut self, sink: &S, metrics: &AdapterMetrics) {
        let resolved = ResolvedDirs::resolve((self.paths)());
        if self.resolved.as_ref() != Some(&resolved) {
            self.apply_resolution(resolved.clone(), metrics);
        }

        // Measure the scan source against the ref it should be reading —
        // BEFORE the early return below, so the idle cycle records
        // `NotScanning` instead of leaving the last reading (or nothing at
        // all) standing. A tier-off machine that reports silence is
        // indistinguishable from one whose scan is in step, and that
        // indistinguishability is the whole defect.
        //
        // Same RT-P0 reasoning as the scan itself: these are `git` subprocess
        // reads, blocking, on a runtime built with `worker_threads(1)`. They
        // go to the blocking pool for the same reason `read_plan_dir` does —
        // parking that single worker also stops its time driver.
        let divergence = match resolved.plans.clone() {
            // Nothing configured: the answer is `NotScanning` and it needs no
            // git at all, so it is computed INLINE. Hopping an unarmed tick to
            // the blocking pool would buy nothing and cost every idle runner a
            // pool round-trip per minute.
            None => ScanDivergence::not_scanning().observed_at(chrono::Utc::now().timestamp()),
            Some(dir) => {
                let git = std::sync::Arc::clone(&self.git);
                match tokio::task::spawn_blocking(move || {
                    // The clock is read HERE, on the blocking thread, just as
                    // the measurement starts — at argument evaluation, so a
                    // moment BEFORE the git probes run, not between them. The
                    // probes are local reads (milliseconds; bounded at 20 s),
                    // far inside the freshness window's resolution, so the
                    // ref's age and the reading's `observed_at` are as of the
                    // measurement rather than of the tick's start, and the
                    // function itself stays pure over the clock.
                    measure_scan_source(
                        Path::new(&dir),
                        git.as_ref(),
                        chrono::Utc::now().timestamp(),
                    )
                })
                .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        // Logged with the full error (it carries a per-run task
                        // id); the READING gets only the stable half.
                        tracing::warn!(
                            error = %e,
                            "plan adapter: the scan-divergence probe task did not complete"
                        );
                        ScanDivergence::unknown(
                            resolved.plans.clone(),
                            probe_task_failure_detail(&e),
                        )
                        .observed_at(chrono::Utc::now().timestamp())
                    }
                }
            }
        };
        record_scan_divergence(divergence, metrics);

        let Some(dir) = resolved.plans.map(PathBuf::from) else {
            // Nothing is scanned — but a device whose plans dir was just
            // cleared must SAY so to the read side, or its last `measured` row
            // keeps being quoted until it ages out. The body sync's library
            // scan stays off here (unchanged); only its scan-root report runs.
            if let Some(bs) = self.body_sync.as_mut() {
                // BOTH stem sets ABSENT: no directory was enumerated and no
                // ref was listed, so there is no census to take. (`state` is
                // `not_scanning` here, which the web door forbids a census on
                // for exactly this reason.)
                bs.report_while_idle(metrics, ScanCensusInputs::absent())
                    .await;
            }
            return;
        };
        let archive_dir = resolved.archive.map(PathBuf::from);

        // Work-unit write posture — see [`work_unit_write_posture`]. Decided
        // BEFORE the scan: a withheld cycle makes no coord work-unit call at
        // all (bulk seed, per-slug seed, upsert, transition, deps, archive
        // stamp — every sink call below), so reading ~1,100 plan blobs off
        // the ref to then discard them would be pure cost. The body sync, which
        // scans its own roots and writes qontinui-web, still runs.
        //
        // The reading is two small file reads (`paired_user.json` and the
        // coord-bound sidecar); like every filesystem touch in this loop it
        // goes to the blocking pool, off the single `fleet-publishers` worker.
        let posture = {
            let read = std::sync::Arc::clone(&self.binding_count);
            match tokio::task::spawn_blocking(move || read()).await {
                Ok(reading) => work_unit_write_posture(reading),
                // Could not read the bindings at all: UNKNOWN, which withholds.
                Err(_) => WorkUnitWritePosture::WithheldBindingsUnknown,
            }
        };
        if posture == WorkUnitWritePosture::WithheldBindingsUnknown {
            self.unknown_posture_streak = self.unknown_posture_streak.saturating_add(1);
            metrics
                .work_unit_writes_withheld_unknown_total
                .fetch_add(1, Ordering::Relaxed);
            // First unknown cycle: info (a fresh boot races its own first
            // heartbeat). Still unknown next cycle: warn, once per streak.
            match self.unknown_posture_streak {
                1 => tracing::info!(
                    "plan adapter: coord's binding record is not available (fresh boot, or aged out) — withholding \
                     work-unit pushes this cycle and re-checking next cycle. The stem censuses are \
                     unaffected: this cycle still LISTS the ref and the work tree and reports both"
                ),
                2 => tracing::warn!("{}", unknown_bindings_message()),
                _ => {}
            }
        } else {
            self.unknown_posture_streak = 0;
            if let Some(line) = work_unit_write_posture_message(self.last_write_posture, posture) {
                match posture {
                    WorkUnitWritePosture::Write => tracing::info!("{line}"),
                    _ => tracing::warn!("{line}"),
                }
            }
        }
        // Writes RESUMING after a withheld posture: the edge memory is frozen
        // at the last Write cycle, and while writes were withheld sessions
        // kept upserting and transitioning units in coord and plan files kept
        // moving. Deciding transitions from that stale memory reads every
        // such move as a remote-divergence conflict — `push_work_unit` drops
        // the CAS `from_status` guard and warns "file wins (loud override)"
        // for a status the file never contradicted. Cold start has no such
        // hole because its memory is empty, so make the resumed cycle a cold
        // start: clear the memory and re-arm the bulk seed, exactly as
        // `apply_resolution` does for a corpus switch. The bulk `list_statuses`
        // read primes it in one round-trip; should that read fail, the
        // per-slug `current_status` seed still fires for every scanned slug
        // because the memory is empty. `forbidden` / `forbidden_deps` stay (a
        // 403 is coord's verdict on the slug regardless of posture),
        // `last_deps` stays (the edge replace-set is idempotent) and
        // `warned_disappeared` stays (those verdicts stand), and so does
        // `seen_slugs` — the detector's known set is what was SCANNED, not the
        // apply-memory cleared here, so a slug whose file vanished while writes
        // were withheld is still surfaced on the resumed cycle.
        if posture == WorkUnitWritePosture::Write
            && matches!(self.last_write_posture, Some(p) if p != WorkUnitWritePosture::Write)
        {
            self.last_applied.clear();
            self.bulk_seeded = false;
        }
        self.last_write_posture = Some(posture);
        if posture != WorkUnitWritePosture::Write {
            metrics
                .work_unit_writes_withheld_total
                .fetch_add(1, Ordering::Relaxed);
            metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
            // The posture withholds coord WORK-UNIT reads and writes. A stem
            // census is NEITHER — it is one `git ls-tree` at the ref this
            // clone already holds (see [`ref_census_only`]: no fetch, no blob
            // read, no coord call), so the cost that justifies skipping the
            // scan above does not reach it, and nothing here re-opens a
            // withheld coord door.
            //
            // Withholding it too made the census permanently dark: a posture
            // is a STANDING property of the device, so the ref side would have
            // been ABSENT on every cycle forever — and the first such cycle
            // would have cleared the stems the web already stored for this
            // device, because a report whose `censuses` omits a source stores
            // that source as NULL. The device that holds both sides of the
            // coverage question is exactly the one most likely to be
            // multi-bound.
            let ref_census = {
                let scan_dir = dir.clone();
                let git = std::sync::Arc::clone(&self.git);
                match tokio::task::spawn_blocking(move || ref_census_only(&scan_dir, git.as_ref()))
                    .await
                {
                    Ok(Ok(census)) => {
                        // Listed something, so the next unavailability is news
                        // again — the same clearing the scanning arm does.
                        self.last_scan_unavailable = None;
                        census
                    }
                    // Deduped on the reason, like both scan-failure arms
                    // below: one unchanging fault costs one line, not one a
                    // minute.
                    Ok(Err(reason)) => {
                        if self.last_scan_unavailable.as_deref() != Some(reason.as_str()) {
                            tracing::warn!(
                                dir = %dir.display(),
                                reason = %reason,
                                "plan adapter: could not list the ref for the stem census on a \
                                 withheld cycle; that side is reported ABSENT (never an empty set)"
                            );
                            self.last_scan_unavailable = Some(reason);
                        }
                        None
                    }
                    // The listing task did not complete, so the enumeration
                    // never happened at all: ABSENT, never a fabricated zero.
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "plan adapter: the ref stem-census task did not complete on a \
                             withheld cycle; that side is reported ABSENT"
                        );
                        None
                    }
                }
            };
            if let Some(bs) = self.body_sync.as_mut() {
                // Both sides travel: the ref census listed just above, and the
                // work-tree census `run_cycle` takes of the active plans root.
                //
                // NOT pinned to the census: that listing deliberately fetches
                // nothing, and pinning the body sync to it would stop a
                // standing withheld device ever refreshing its corpus. The
                // census carries its own `ref_sha`, so the two sides are each
                // self-describing; the body sync pins its own roots.
                bs.run_cycle(&self.conv, metrics, ref_census).await;
            }
            return;
        }

        // RT-P0: `read_plan_dir` is a SYNCHRONOUS walk — one `std::fs::read_dir`
        // plus a `read_to_string` of every `*.md` in the plans dir (~1,100
        // files; the loop's own tick comment measures the first cycle at
        // five minutes). This loop lives on the `fleet-publishers` runtime,
        // which is built with `worker_threads(1)` (`main.rs`), and it shares
        // that single worker with the census, reclaim, the orphan reaper, the
        // maintenance executor, the fs backstop and the agent runtime.
        //
        // Run inline, the walk parked that one worker for the whole scan, which
        // also stops the runtime's TIME DRIVER — so every timer on it dilates by
        // the scan duration. That is the mechanism behind a 20s keepalive firing
        // 264s late and an 8s backoff taking 25.5 minutes. See `off_runtime.rs`
        // for why a `tokio::time::timeout` cannot rescue this on its own.
        // ONE ref state for this whole cycle: the work-unit scan below resolves
        // through this pin, and the body sync further down reads through the
        // SAME one, so the census reported and the bodies published are listed
        // at one commit however long the reconcile between them runs and
        // whoever fetches the shared checkout meanwhile. Dropped with the
        // cycle, so the next cycle fetches afresh. See [`CycleRefPin`].
        let pin = std::sync::Arc::new(CycleRefPin::default());
        let active_scan = {
            let scan_dir = dir.clone();
            let conv = self.conv.clone();
            let pin = std::sync::Arc::clone(&pin);
            // The loop's OWN git reader, not a hardcoded `ProcessGit`: the
            // divergence probe two statements up already uses it, and a scan
            // that can look at a different reader than the probe measuring it
            // is a reading of nothing. It is also the only thing that makes
            // the publish-nothing arm reachable from a test.
            let git = std::sync::Arc::clone(&self.git);
            match tokio::task::spawn_blocking(move || {
                read_plans_for_cycle(&scan_dir, &conv, git.as_ref(), &pin)
            })
            .await
            {
                Ok(Ok(scan)) => scan,
                // Phase 2: `Err` is the ref saying it could not be read. It is
                // NOT an empty corpus — publishing `Vec::new()` here would let
                // reconcile treat every plan as disappeared.
                Ok(Err(reason)) => {
                    if self.last_scan_unavailable.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(
                            dir = %dir.display(),
                            reason = %reason,
                            "plan adapter: scan source unavailable; publishing nothing this \
                             cycle (the previous corpus stands)"
                        );
                        self.last_scan_unavailable = Some(reason);
                    }
                    // A cycle that publishes NOTHING is the one that most needs
                    // to say so to the read side, or its last `measured` row
                    // keeps being quoted until it ages out — the same reason
                    // the cleared-plans-dir arm above reports while idle, and
                    // the reason the scan-root report sits ahead of the
                    // breaker pause and the `artifacts.is_empty()` return.
                    if let Some(bs) = self.body_sync.as_mut() {
                        // The REF set is ABSENT: the scan source was
                        // unavailable, so nothing was listed — and a set that
                        // was never listed is UNKNOWN. Sending `count: 0` for
                        // it would say "this side holds no plans", which is
                        // the false zero this whole plan family exists to
                        // remove.
                        //
                        // The WORK-TREE set is still READ, and this is the
                        // arm where that distinction earns its keep. The
                        // causes that land here are mostly STANDING, not
                        // passing — `resolve_ref_listing_source` reports
                        // `Unavailable` for a clone with no `origin/HEAD`
                        // (`ProcessGit::default_ref` calls that "a real,
                        // permanent configuration"), for a plans dir outside
                        // its work-tree root, and for a plans dir that is
                        // unpushed or ignored. Withholding BOTH sides here
                        // would take this device's work-tree census dark on
                        // every cycle FOREVER while the report kept going out
                        // looking healthy — the same silent-permanent-absence
                        // shape as the withheld-posture defect, reached from
                        // a different arm.
                        //
                        // Reading it invents nothing: the work-tree census is
                        // one `read_dir` and needs no git at all, and
                        // `work_tree_census` returns ABSENT (never an empty
                        // set) when the dir will not enumerate or the listing
                        // is a floor. So this reports what was measured and
                        // stays silent about what was not.
                        bs.report_while_idle(
                            metrics,
                            ScanCensusInputs {
                                ref_census: None,
                                work_tree_dir: Some(dir.clone()),
                            },
                        )
                        .await;
                    }
                    metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(e) => {
                    // The SAME shape as the arm above, and for the same three
                    // reasons — this arm is a cycle that publishes nothing too.
                    //
                    // Deduped on the STABLE half: a `JoinError`'s `Display`
                    // carries a per-run task id (see
                    // [`probe_task_failure_detail`], which exists for exactly
                    // this), so a repeatedly panicking scan task would
                    // otherwise be a NEW warn string every 60 s — the
                    // per-minute volume the dedup is here to stop. The full
                    // error still reaches the line that does get logged.
                    let reason = format!(
                        "the plans-dir scan task did not complete: {}",
                        probe_task_failure_detail(&e)
                    );
                    if self.last_scan_unavailable.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(
                            error = %e,
                            "plan adapter: plans-dir scan task failed; publishing nothing this \
                             cycle (the previous corpus stands)"
                        );
                        self.last_scan_unavailable = Some(reason);
                    }
                    if let Some(bs) = self.body_sync.as_mut() {
                        // The REF set is ABSENT, and this is the arm where
                        // that matters most: the scan task did not COMPLETE,
                        // so its enumeration never happened at all. A zero
                        // census here would not even be stale — it would be
                        // FABRICATED, a set nothing on this device ever
                        // listed, and the web would store it as this device's
                        // answer for the side.
                        //
                        // The WORK-TREE set is read for the same reason as the
                        // arm above: it shares nothing with the failed task —
                        // no git, no blob reads, just one `read_dir` this
                        // cycle has not spent — and a panicking scan task can
                        // recur for as long as its cause stands. Its own
                        // failure modes stay ABSENT rather than empty
                        // (`work_tree_census` returns `None` when the dir will
                        // not enumerate or the listing is a floor), so nothing
                        // here can fabricate the zero the arm is guarding.
                        bs.report_while_idle(
                            metrics,
                            ScanCensusInputs {
                                ref_census: None,
                                work_tree_dir: Some(dir.clone()),
                            },
                        )
                        .await;
                    }
                    // Counted like every other cycle: a FROZEN `cycles_total`
                    // reads as "the loop is dead", which is a different and
                    // much louder claim than "the loop is failing".
                    metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        };
        // Read something, so the next unavailability is news again.
        self.last_scan_unavailable = None;
        let active_scan_complete = active_scan.complete;
        // Moved out before `units` is consumed by the reconcile below: the
        // ref census is the work-unit half's listing of the ref, and it
        // travels to the scan-root report on every cycle, including one whose
        // units the reconcile then takes ownership of. (The two are disjoint
        // fields, so the order of the two partial moves is not itself
        // load-bearing.)
        let ref_census = active_scan.ref_census;
        let active_dangling = active_scan.dangling_link_slugs;
        let units = active_scan.units;
        self.bulk_seed(&units, sink, metrics).await;
        let summary = reconcile_once(
            &units,
            &mut self.last_applied,
            &mut self.last_deps,
            &mut self.forbidden,
            &mut self.forbidden_deps,
            sink,
            metrics,
        )
        .await;
        metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            scanned = summary.scanned,
            transitions = summary.transitions,
            conflicts = summary.conflicts,
            errors = summary.errors,
            deferred = summary.deferred,
            deps_set = summary.deps_set,
            deps_skipped_unmigrated = summary.deps_skipped_unmigrated,
            deps_errors = summary.deps_errors,
            forbidden = summary.forbidden,
            retired_permanent = summary.retired_permanent,
            deps_forbidden = summary.deps_forbidden,
            seeded = summary.seeded,
            seed_errors = summary.seed_errors,
            "plan adapter: reconcile cycle complete"
        );

        // Archive scan (metadata-only) + disappeared-slug detection. When no
        // archive dir is configured, `read_plan_dir` on `None` is skipped and
        // the archive slug set is empty — a slug that vanishes from the active
        // dir with no archive configured is still surfaced as disappeared.
        // Same reasoning as the active scan above: off the single worker.
        let archive_scan = match archive_dir {
            Some(a) => {
                let conv = self.conv.clone();
                match tokio::task::spawn_blocking(move || scan_plan_dir(&a, &conv)).await {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "plan adapter: archive-dir scan task failed; this cycle scanned \
                             NOTHING from the archive"
                        );
                        // NOT "skipping this cycle" — the cycle continues. An
                        // empty-and-INCOMPLETE scan must not let any consumer
                        // read the emptiness as absence.
                        PlanDirScan {
                            units: Vec::new(),
                            complete: false,
                            dangling_link_slugs: Vec::new(),
                        }
                    }
                }
            }
            // No archive dir configured: there is nothing to read, so the empty
            // set is the WHOLE truth about the archive — a complete scan of
            // nothing, not a failed scan.
            None => PlanDirScan {
                units: Vec::new(),
                complete: true,
                dangling_link_slugs: Vec::new(),
            },
        };
        let archive_scan_complete = archive_scan.complete;
        let archive_dangling = archive_scan.dangling_link_slugs;
        let archived = archive_scan.units;
        if !archived.is_empty() {
            let asum = reconcile_archive_once(&archived, sink, metrics).await;
            tracing::info!(
                scanned = asum.scanned,
                stamped = asum.stamped,
                errors = asum.errors,
                "plan adapter: archive scan complete (metadata-only)"
            );
        }
        // Plan & prompt library body sync — opt-in, see `BodySync`.
        if let Some(bs) = self.body_sync.as_mut() {
            // The ref census this cycle's listing produced travels with the
            // body sync's own work-tree census, so ONE report carries both
            // sides of the set difference as of ONE cycle — and the pin makes
            // the bodies it publishes come from the commit that census names.
            bs.run_cycle_pinned(&self.conv, metrics, ref_census, pin)
                .await;
        }

        let active_slugs: HashSet<String> = units.iter().map(|u| u.slug.clone()).collect();
        let archive_slugs: HashSet<String> = archived.iter().map(|u| u.slug.clone()).collect();
        // Everything this cycle SAW joins the known set — whether or not its
        // status was pushable. See [`LoopState::seen_slugs`] for why the
        // apply-memory is the wrong input here.
        self.seen_slugs.extend(active_slugs.iter().cloned());
        // **The detector may only run on a COMPLETE read of both dirs.**
        //
        // Complete is not the same as CONSISTENT, and the gap is stated so it
        // is not mistaken for a guarantee: when the active dir is read from a
        // ref and the archive dir from a working tree that is behind it, a
        // plan moved into the archive on the ref is absent from both reads
        // while both are complete, and is warned about once (informational
        // only — coord's row is never touched). Reading the archive from the
        // same ref is the fix; it changes where archive stamps take their
        // bytes, so it is left to its own change.
        //
        // "Disappeared" is a claim about ABSENCE, and both scans report absence
        // and failure with the same short vector. The ref arm already refuses
        // the WHOLESALE failure (a failed listing or every blob failing is an
        // `Err` and the cycle returns above), but a PARTIAL one — one blob that
        // will not read or is not UTF-8, one unstattable tree entry, one
        // unreadable archived file — comes back `Ok` and short. Run unguarded,
        // that makes every slug it skipped look vanished, and
        // `newly_disappeared_slugs` INSERTS each into `warned_disappeared`,
        // which is warn-once **per process** — so the detector goes
        // permanently blind for those slugs, and the restart that would clear
        // it is forbidden by served policy `production-and-cost`
        // `runner-lifecycle`. Only the scan's own `complete` flag
        // distinguishes a short read from a short directory.
        if active_scan_complete && archive_scan_complete {
            // A dangling link's NAME is still in its dir: the plan has not
            // LEFT it, whatever its target is doing this cycle — so it counts
            // as present here, and only here (it was never read, so it is not
            // SEEN above). See `PlanDirScan::dangling_link_slugs`.
            let present_active: HashSet<String> = active_slugs
                .iter()
                .cloned()
                .chain(active_dangling)
                .collect();
            let present_archive: HashSet<String> = archive_slugs
                .iter()
                .cloned()
                .chain(archive_dangling)
                .collect();
            for slug in newly_disappeared_slugs(
                &self.seen_slugs,
                &present_active,
                &present_archive,
                &mut self.warned_disappeared,
            ) {
                tracing::warn!(
                    slug = %slug,
                    "plan adapter: work-unit slug disappeared from the active dir and is absent \
                     from the archive dir; leaving the unit untouched (terminal state is owned by \
                     coord's derive engine — the adapter never pushes shipped/archived)"
                );
            }
            self.detector_skip_warned = false;
        } else if !self.detector_skip_warned {
            // Once per STREAK of skipped cycles, not per cycle: a standing
            // fault (an unreadable plan file, a configured archive dir that
            // does not exist) would otherwise WARN every minute. The per-fault
            // detail is logged by the scan itself; a complete cycle re-arms
            // this, so a recurrence is news again.
            self.detector_skip_warned = true;
            tracing::warn!(
                active_scan_complete,
                archive_scan_complete,
                known_slugs = self.seen_slugs.len(),
                "plan adapter: disappeared-slug detection SKIPPED — a plan scan was \
                 incomplete, so an absent slug is UNKNOWN rather than gone; skipping \
                 until a complete scan (said once per streak)"
            );
        }
    }
}

/// Whether this cycle may read and write coord's work-unit store.
///
/// Plan `2026-09-17-plan-adapter-mints-work-units-under-the-default-binding-of-a-multi-bound-device`.
/// Every coord call the adapter's sink makes presents the DEFAULT binding's
/// credential, and coord stamps a new work unit with the tenant of the bearer
/// it verified. On a device bound to one tenant that is the right tenant by
/// construction. On a device bound to several it is a guess — and the adapter,
/// which reads each plan off `origin/main` within a cycle of the push, is the
/// FIRST writer of every new slug, so the guess becomes the row's owner and
/// the owning tenant's own sessions are then refused with
/// `slug_owned_by_another_tenant` (coord's global slug guard, correctly
/// refusing to fork the unit). Measured 2026-09-16/17: four plans in a row
/// minted under `meryts-2-0` by a device whose only credential was that
/// tenant's.
///
/// The owning tenant of a plans directory is not resolvable from here, so on a
/// multi-bound device the honest answer is UNKNOWN, and UNKNOWN withholds the
/// write (domain_spec `tenant-binding`: an unresolvable tenant is said, never
/// defaulted). Sessions keep full ability to register their own plan's unit in
/// their own tenant; this narrows a background writer only.
///
/// The binding count is a [`BindingCountReading`] from
/// [`read_device_binding_count`]: coord's authoritative figure where a fresh
/// record exists, and the locally-held slot count beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkUnitWritePosture {
    /// Coord reports (or the local slots prove) at most one binding: push
    /// exactly as before.
    Write,
    /// Bound to this many tenants: no coord work-unit call this cycle.
    WithheldMultiBound(usize),
    /// Coord's binding set is UNKNOWN (no fresh record — see
    /// `pair::coord_bound_tenant_count`) and the local slots cannot prove the
    /// device multi-bound. The local count alone under-reads exactly the
    /// hazardous device, so an unknown withholds rather than falling open.
    /// A healthy single-tenant runner leaves this on the first cycle after
    /// its register heartbeat succeeds (every 30 s).
    WithheldBindingsUnknown,
}

/// What this device knows about how many tenants it is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingCountReading {
    /// `auth::device_binding_count`: tenants this runner holds a credential
    /// for, per `paired_user.json` (every unreadable state counts as one).
    pub local: usize,
    /// `pair::coord_bound_tenant_count`: coord's own figure as last echoed on
    /// the register heartbeat; `None` when absent, unreadable or stale.
    pub coord: Option<usize>,
}

pub fn work_unit_write_posture(reading: BindingCountReading) -> WorkUnitWritePosture {
    match reading.coord {
        Some(coord) => {
            let n = reading.local.max(coord);
            if n > 1 {
                WorkUnitWritePosture::WithheldMultiBound(n)
            } else {
                WorkUnitWritePosture::Write
            }
        }
        None if reading.local > 1 => WorkUnitWritePosture::WithheldMultiBound(reading.local),
        None => WorkUnitWritePosture::WithheldBindingsUnknown,
    }
}

/// Read this device's [`BindingCountReading`] (blocking: two small file reads).
///
/// Deliberately NOT fed into the D2 bearer degrade
/// (`auth::select_scoped_bearer_lazy`): that rule governs every session-scoped
/// `Unresolved` write on the device, and widening its count is a separate
/// decision with a separate blast radius.
pub fn read_device_binding_count() -> BindingCountReading {
    BindingCountReading {
        local: crate::auth::device_binding_count(),
        coord: crate::pair::coord_bound_tenant_count(),
    }
}

/// Why an UNKNOWN binding set withholds, and what produces the record — named
/// causes, because "wait for a heartbeat" is false advice on an instance that
/// never runs one.
fn unknown_bindings_message() -> String {
    let path = crate::pair::coord_bound_tenants_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unresolvable data dir>".to_string());
    format!(
        "plan adapter: coord's binding set for this device is UNKNOWN — no fresh record at \
         {path} — so every coord work-unit read and write is WITHHELD rather than assuming this \
         device is bound to one tenant (the stem censuses still travel: a listing is not a \
         coord call). The record is written only by the PRIMARY runner's \
         register heartbeat: a supervisor-spawned secondary or temp runner never writes one \
         (and reads its own storage dir), so this posture is permanent there; on a primary, \
         check machine.json, the active profile's coord_url, and `fleet::heartbeat` warnings"
    )
}

/// The log line for a posture CHANGE, or `None` when nothing worth saying
/// changed. `previous == None` is the first cycle: a withheld posture is
/// announced, an ordinary one is not (it is today's behaviour).
fn work_unit_write_posture_message(
    previous: Option<WorkUnitWritePosture>,
    now: WorkUnitWritePosture,
) -> Option<String> {
    if previous == Some(now) {
        return None;
    }
    match now {
        WorkUnitWritePosture::WithheldMultiBound(n) => Some(format!(
            "plan adapter: this device is bound to {n} tenants and a plan's owning tenant is not \
             resolvable here — WITHHOLDING every coord work-unit read and write rather than filing \
             units under the default credential's tenant. The plan-library body sync is \
             unaffected, and so are the stem censuses: this cycle still LISTS the ref (one \
             `git ls-tree`, no fetch and no blob read) and the work tree, and reports both, so \
             this device's coverage reading does not go dark. A session registers its own plan's \
             unit with coord_work_unit_upsert in its own tenant"
        )),
        // `LoopState::tick` logs this posture through its own streak logic (info
        // first, warn once on a second consecutive cycle) and does not call here
        // for it; the arm keeps the function total for its other callers.
        WorkUnitWritePosture::WithheldBindingsUnknown => Some(unknown_bindings_message()),
        WorkUnitWritePosture::Write => previous.map(|_| {
            "plan adapter: coord reports this device bound to one tenant — work-unit pushes resume"
                .to_string()
        }),
    }
}

/// The periodic reconcile loop. Runs until the task is dropped.
///
/// The path settings are re-read on every tick ([`PathReader`]); what a tick
/// does with them is [`LoopState::tick`].
async fn run_loop<S: WorkUnitSink + ?Sized>(
    paths: PathReader,
    body_sync_sink: Option<super::body_push::HttpArtifactSink>,
    capture_gate: CaptureGate,
    scan_report_gate: ScanReportGate,
    sink: &S,
    interval_secs: u64,
) {
    let mut state =
        LoopState::new(paths, body_sync_sink, capture_gate).with_scan_report_gate(scan_report_gate);
    let metrics = adapter_metrics();
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    // A cycle can legitimately outrun the interval (the first one walks and
    // pushes ~1,100 files). The default `Burst` behaviour then fires every
    // missed tick back to back, so a 5-minute first cycle is followed by four
    // immediate no-gap cycles — the opposite of what a periodic reconcile
    // wants. `Delay` drops the missed ticks and simply restarts the interval
    // from now.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(
        interval_secs,
        "plan adapter: reconcile loop started (path settings are re-read every tick)"
    );
    loop {
        tick.tick().await;
        state.tick(sink, metrics).await;
    }
}

/// Per-machine **kill switch** for the plan & prompt **library body sync**
/// riding along with the reconcile loop (plan
/// `2026-08-10-plan-and-prompt-library-in-web` Phase 2). **On by default**;
/// disabled only by an exact `"0"` (plan
/// `2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
/// Phase 3 — the predecessor's decided-but-unshipped 4b).
///
/// **Why on by default now.** This shipped opt-in because the qontinui-web
/// plan-library routes were Cognito-only, and an on-by-default sync would have
/// emitted ~1,100 failed requests every 60s on every runner in the fleet. That
/// premise is gone — the routes accept the device bearer — and what remained
/// was a switch nobody flipped, on any measured device, so the corpus the
/// fleet declared authoritative for reads stayed frozen (served policy
/// `engineering-priorities` `capability-ships-enabled`). What bounds the sync is
/// not this flag: the backend-resolution guard (`HttpArtifactSink::from_env`
/// answers `None` with no resolvable backend, and a release build refuses a
/// machine-local one — see `main.rs`), the tenant's `plan_capture` dial
/// consulted every cycle ([`CaptureGate`]), the five-cycle failure breaker,
/// and — stricter than all three — the adapter loop ITSELF:
/// [`spawn_if_configured`] returns as soon as no coord base resolves
/// (`COORD_HTTP_URL` / `profiles.<active>.coord_url`), which is BEFORE the
/// body-sync sink is built, so a runner with none never syncs at all however
/// this flag reads.
/// The name is kept so an operator's existing `=1` keeps meaning "on".
pub const PLAN_LIBRARY_SYNC_ENV: &str = "QONTINUI_PLAN_LIBRARY_SYNC";

/// Whether the library body sync is enabled for this process — `true` unless
/// [`PLAN_LIBRARY_SYNC_ENV`] is exactly `"0"` (whitespace-trimmed). Read once
/// at spawn (unlike the write-door kill switch, which is read per request):
/// this one decides whether a long-lived loop *has* a sync at all, and a
/// mid-flight change of that shape has no meaning.
pub fn body_sync_enabled() -> bool {
    !matches!(std::env::var(PLAN_LIBRARY_SYNC_ENV), Ok(v) if v.trim() == "0")
}

/// The line [`body_sync_if_enabled`] logs when the body sync is killed, as a
/// pure function of the flag's observed value so a test can pin its content
/// without a log-capture dependency (the crate has none) — plan
/// `2026-08-27-plan-corpus-read-path-is-dark` Phase 1 (D6): a spawn-time flag
/// whose state is unobservable is worse than a flag that is off. The disabled
/// arm used to be a bare `None` — the only one of the three arms with no
/// signal at all — so "is the body sync on for this device?" had no answer in
/// any log.
///
/// `observed` is the raw env value — `None` when the variable is unset — so a
/// value that is not the killing `"0"` yet still landed here (which cannot
/// happen today, but the line must not lie if the predicate ever moves) is
/// printed back verbatim rather than collapsed into "off".
pub fn body_sync_disabled_message(observed: Option<&str>) -> String {
    format!(
        "plan library: body sync is KILLED on this machine — {PLAN_LIBRARY_SYNC_ENV} is {} — so \
         BodySync was NOT constructed and NO plan body reaches agent.work_artifacts from \
         this runner (the work-unit reconcile is unaffected). It is on by default: unset the \
         variable before the runner starts; it is read once at spawn",
        match observed {
            Some(v) => format!("set to {v:?} (the exact string \"0\" kills it)"),
            None => "unset".to_string(),
        }
    )
}

/// What [`BodySync::run_cycle`] says about the tenant's `plan_capture` dial
/// this cycle, given the verdict it recorded last cycle: the dial is announced
/// on the FIRST cycle unconditionally, on every later cycle only when it flips,
/// and otherwise not at all.
///
/// The first-cycle arm exists because a runner that boots with the dial OFF
/// used to say nothing recognisable about it — the previous "changed" wording
/// fired then too, but described a boot as a transition, and a reader grepping
/// for the dial's boot state found no line that named it as such.
pub fn capture_gate_message(previous: Option<bool>, gate_open: bool) -> Option<&'static str> {
    match (previous, gate_open) {
        (None, true) => Some(
            "plan library: tenant plan_capture dial is OPEN on the body sync's first cycle — \
             scanned plan bodies are pushed to agent.work_artifacts",
        ),
        (None, false) => Some(
            "plan library: tenant plan_capture dial is CLOSED on the body sync's first cycle — \
             plan bodies are NOT pushed to agent.work_artifacts until the dial opens (it is \
             re-read every cycle, no restart needed)",
        ),
        (Some(prev), now) if prev != now => {
            Some("plan library: tenant plan_capture level changed the body sync's authorization")
        }
        _ => None,
    }
}

/// Whether the tenant's fleet dial currently authorizes plan capture.
///
/// A callback rather than a direct read because the dial's cache lives in the
/// runner **binary** (`crate::mcp::fleet_policy_poller`) while this adapter
/// lives in the lib crate, which cannot see it. The binary supplies the reader
/// at spawn time; the lib stays free of the poller.
pub type CaptureGate = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// Whether THIS runner instance may publish the machine's scan-root reading.
///
/// Every runner instance on a machine reports under the same device token, and
/// the web keeps ONE row per device — so a supervisor-spawned temp runner with
/// a different (or no) plans dir would flip the machine's row back and forth
/// against the primary's. The reading is machine-scoped state, and only the
/// instance that owns shared root state may publish it: the binary supplies
/// `fleet::machine_state_publish_allowed(instance::owns_shared_root_state())`,
/// the same predicate every other device-keyed writer is gated on. A callback
/// for the same reason as [`CaptureGate`] — that predicate lives in the runner
/// binary, which this lib crate cannot see — and read per report, so it is
/// never a stale snapshot.
pub type ScanReportGate = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// The gate a loop or body sync has until one is supplied: CLOSED. A
/// machine-scoped writer that was never told it owns the machine must not
/// assume it does.
fn scan_report_gate_closed() -> ScanReportGate {
    std::sync::Arc::new(|| false)
}

/// How many **consecutive** entirely-failed cycles pause the sync.
///
/// The axis is consecutive cycles, not a sample-size floor on one cycle. A
/// floor cannot work here: in steady state the digest memory skips almost
/// everything, so a cycle in which the operator edited one plan legitimately
/// attempts exactly ONE push — a floor of, say, 10 would make the breaker
/// unreachable in precisely the state it has to protect, while a floor of 1
/// (the shipped behaviour) lets a single transient 500, a 30-second network
/// blip or a mid-rotation 401 latch the sync off. Requiring the failure to
/// persist across five cycles (~5 minutes at the default tick) distinguishes
/// "this backend is down" from "one request was unlucky" without reference to
/// how many files happened to change.
const TOTAL_FAILURE_CYCLES_BEFORE_PAUSE: u32 = 5;

/// How many cycles a tripped breaker sits out before trying again.
///
/// ~30 minutes at the default 60s tick. The breaker is a **pause that
/// re-arms**, never the one-way latch it started as: the latch's own error
/// message prescribed restarting the runner, which served policy
/// `production-and-cost` `runner-lifecycle` forbids outright — so a tripped
/// latch was unrecoverable for the process's whole life, which on this fleet
/// means indefinitely. A pause costs one cycle's worth of failed requests per
/// half hour while the backend is down, and resumes on its own the moment it
/// comes back.
const PAUSE_CYCLES: u32 = 30;

/// The body sync's failure breaker, as a pure state machine.
///
/// Factored out of [`BodySync`] so the property that matters — a single
/// transient failure must NOT disable the sync — is a unit test over the
/// shipping logic rather than a claim about it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FailureBreaker {
    consecutive_total_failures: u32,
    pause_cycles_remaining: u32,
}

impl FailureBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one cycle of the current pause, if any. Returns `true` when the
    /// caller should SKIP this cycle entirely.
    pub fn should_skip_cycle(&mut self) -> bool {
        if self.pause_cycles_remaining == 0 {
            return false;
        }
        self.pause_cycles_remaining -= 1;
        if self.pause_cycles_remaining == 0 {
            tracing::info!(
                "plan library: body-sync pause elapsed — retrying one cycle. If the backend \
                 or credential is still broken it will pause again; no restart is needed \
                 either way."
            );
        }
        true
    }

    /// Record a completed cycle. `attempted` counts only the pushes that
    /// actually reached the network (a locally-skipped unchanged file is not
    /// an attempt and cannot fail). Returns `true` when this call TRIPPED the
    /// breaker.
    pub fn record_cycle(&mut self, attempted: u64, errors: u64) -> bool {
        let totally_failed = attempted > 0 && errors == attempted;
        if !totally_failed {
            self.consecutive_total_failures = 0;
            return false;
        }
        self.consecutive_total_failures += 1;
        if self.consecutive_total_failures < TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            return false;
        }
        self.consecutive_total_failures = 0;
        self.pause_cycles_remaining = PAUSE_CYCLES;
        true
    }

    pub fn is_paused(&self) -> bool {
        self.pause_cycles_remaining > 0
    }

    pub fn consecutive_total_failures(&self) -> u32 {
        self.consecutive_total_failures
    }
}

/// How often an UNCHANGED scan-root reading is re-posted to the web read side.
///
/// A heartbeat, so the row's age stays honest: the web side ages a row by when
/// it last RECEIVED a report (its own clock, not this runner's `observed_at`)
/// and reads a row older than three of these (45 min) as `unknown` /
/// `observation_stale` — which is how a device that stopped reporting
/// (killed, offline, its sync switched off) stops being quoted as current.
/// Without it an in-step device would post once and then look dead. A row
/// whose latest report the web DECLINED (`applied: false`, a newer reading
/// already stored) reads `reading_superseded`; that case re-posts on the
/// shorter [`SCAN_REPORT_RETRY_AFTER_FAILURE`] cadence instead.
pub const SCAN_REPORT_HEARTBEAT: Duration = Duration::from_secs(15 * 60);

/// After a failed scan-root post, how long before the next attempt.
///
/// The report rides the reconcile tick (60 s), and a backend that does not
/// serve the route yet — the web half ships separately — would otherwise take
/// one doomed request a minute from every runner. Five minutes keeps a
/// recovered backend current well inside the 45-minute staleness horizon.
const SCAN_REPORT_RETRY_AFTER_FAILURE: Duration = Duration::from_secs(5 * 60);

/// Whether [`BodySync`] should post the current scan-root reading this cycle.
///
/// Pure over its inputs so the posting policy is a unit test:
/// - no reading yet (`current` is `None` — the reconcile loop has not ticked)
///   → nothing to post;
/// - inside the retry backoff after a failed post → wait;
/// - never posted → due;
/// - the reading CHANGED ([`ScanDivergence::is_same_reading`], so a ref age
///   advancing inside its freshness window is not a change) → due;
/// - unchanged, but [`SCAN_REPORT_HEARTBEAT`] has elapsed since the last
///   delivered post → due;
/// - unchanged, and the last delivered post came back `applied: false`
///   (`last_unapplied`) — the web kept a newer reading, almost always because
///   this machine's clock stepped back — then re-posted every
///   [`SCAN_REPORT_RETRY_AFTER_FAILURE`] instead, so the web picks up the
///   device's current reading soon after its clock catches up rather than a
///   whole heartbeat later;
/// - otherwise → not due.
pub fn scan_report_due(
    last_posted: Option<(&ScanDivergence, std::time::Instant)>,
    last_failed_at: Option<std::time::Instant>,
    current: Option<&ScanDivergence>,
    now: std::time::Instant,
    last_unapplied: bool,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    if last_failed_at.is_some_and(|failed| {
        now.saturating_duration_since(failed) < SCAN_REPORT_RETRY_AFTER_FAILURE
    }) {
        return false;
    }
    match last_posted {
        None => true,
        Some((previous, posted_at)) => {
            let repost_every = if last_unapplied {
                SCAN_REPORT_RETRY_AFTER_FAILURE
            } else {
                SCAN_REPORT_HEARTBEAT
            };
            !previous.is_same_reading(current)
                || now.saturating_duration_since(posted_at) >= repost_every
        }
    }
}

/// The library body-sync half of a reconcile cycle: re-scan the three roots and
/// push any artifact whose body digest moved.
///
/// Holds its own [`super::body_push::ArtifactSyncState`], so steady state costs
/// one directory walk and zero HTTP calls — the whole point of the digest
/// memory (pass 2's edge memo is what makes the "zero HTTP calls" half true;
/// without it the edge pass re-POSTed every `depends_on` edge every tick).
/// Kept in the same tick as the work-unit reconcile rather than on its own
/// timer so the two can never observe different filesystem states.
///
/// ## The fleet dial governs this, not just the briefing
///
/// `run_cycle` consults [`CaptureGate`] — the tenant's `plan_capture` level —
/// on every cycle, and does nothing at `off`. Without that the dial would be
/// advisory for everything except the system-prompt clause: a runner with
/// the body sync on would keep pushing the whole corpus at fleet level `off`.
/// Capture is two independent switches in the same direction — a per-machine
/// kill switch (`QONTINUI_PLAN_LIBRARY_SYNC`, on by default) AND a tenant-wide
/// dial — so the dial is a real fleet off-switch that does not require touching
/// env on every machine (and cannot, since restarting runners is forbidden).
#[derive(Clone)]
pub struct BodySync {
    roots: Vec<super::body_push::ScanRoot>,
    /// The git reader the body sync takes its BYTES through — the document
    /// layer's counterpart of [`LoopState::git`].
    ///
    /// Phase 2 of `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`
    /// moved the WORK-UNIT half onto `origin/<default-branch>` and left this
    /// half walking the filesystem, so on a checkout behind its own default
    /// branch the two halves published from different bytes and a plan amended
    /// only on the ref never reached the corpus at all (finding 61b51044:
    /// measured 717 commits behind, two stems frozen in the same cycle).
    git: std::sync::Arc<dyn GitRefReader>,
    sink: super::body_push::HttpArtifactSink,
    state: super::body_push::ArtifactSyncState,
    capture_gate: CaptureGate,
    breaker: FailureBreaker,
    /// Last gate verdict observed, so a flip is logged ONCE rather than every
    /// tick. `None` until the first cycle.
    last_gate_open: Option<bool>,
    /// The scan-root reading last ACCEPTED by the web read side, and when —
    /// see [`scan_report_due`]. Reset with the rest of this struct when the
    /// path settings move, so a new plans dir is reported on its first cycle.
    last_scan_report: Option<(ScanDivergence, std::time::Instant)>,
    /// The last failed scan-root post's failure KIND and time: the time
    /// drives the retry backoff, the kind makes the failure WARN
    /// edge-triggered (a repeat of the same kind drops to DEBUG — the message
    /// cannot key it, since a 422 body echoes input that differs per
    /// attempt). Cleared by a delivery.
    last_scan_report_failure: Option<(String, std::time::Instant)>,
    /// Whether the last DELIVERED report came back `applied: false` (the web
    /// kept a newer reading) — so that WARN fires once per episode, not per
    /// post.
    last_scan_report_unapplied: bool,
    /// Where scan-root reports go: the same web sink as the body pushes in
    /// production; a recording fake in tests (see
    /// [`super::body_push::ScanRootReporter`]).
    reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
    /// Whether this instance may publish the machine's reading at all — see
    /// [`ScanReportGate`]. Closed unless supplied.
    scan_report_gate: ScanReportGate,
    /// Whether the closed gate has been announced, so it is said once per
    /// body sync rather than every cycle.
    scan_report_gate_announced: bool,
    /// `source -> digest` of the stem set the web last DELIVERED-AND-STORED
    /// for that source, so an unchanged set travels as `slugs: null` plus its
    /// digest instead of ~100 KB of stems on every heartbeat — which is what
    /// keeps this report a heartbeat.
    ///
    /// Emptied whenever the web did NOT store the report (a failure, or a
    /// `applied: false` that kept a newer reading): withholding a set the web
    /// does not hold would have it clear that set to UNKNOWN on the digest
    /// mismatch, which is the integrity property working against us.
    last_census_digests: HashMap<String, String>,
}

/// What one cycle can say about the two stem sets, handed to the scan-root
/// report.
///
/// The work-tree side is a DIRECTORY rather than a census because the report
/// is deliberately sequenced ahead of the body sync's own walk (a paused or
/// empty cycle is the one that most needs to report), so the enumeration
/// happens inside the report — and only once the report is actually due.
#[derive(Debug, Clone, Default)]
pub struct ScanCensusInputs {
    /// The REF listing this cycle made. `None` = ABSENT (UNKNOWN), never an
    /// empty set.
    pub ref_census: Option<super::body_push::PlanSlugCensus>,
    /// The plans root to enumerate for the WORK-TREE census. `None` = this
    /// cycle enumerated nothing, so that side is ABSENT too.
    pub work_tree_dir: Option<PathBuf>,
}

impl ScanCensusInputs {
    /// Both sides ABSENT — what every cycle that enumerated nothing sends.
    ///
    /// Named rather than defaulted so each idle call site says out loud that
    /// it is reporting UNKNOWN, and so a reviewer can see at a glance that no
    /// idle arm fabricates a zero.
    pub fn absent() -> Self {
        Self::default()
    }
}

impl BodySync {
    pub fn new(
        roots: Vec<super::body_push::ScanRoot>,
        sink: super::body_push::HttpArtifactSink,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            roots,
            git: std::sync::Arc::new(ProcessGit),
            reporter: std::sync::Arc::new(sink.clone()),
            sink,
            state: super::body_push::ArtifactSyncState::new(),
            capture_gate,
            breaker: FailureBreaker::new(),
            last_gate_open: None,
            last_scan_report: None,
            last_scan_report_failure: None,
            last_scan_report_unapplied: false,
            scan_report_gate: scan_report_gate_closed(),
            scan_report_gate_announced: false,
            last_census_digests: HashMap::new(),
        }
    }

    /// Take bytes through `git` instead of the default [`ProcessGit`].
    ///
    /// Set by [`LoopState`] from its OWN reader, so the document layer, the
    /// work-unit layer and the divergence probe cannot look at three different
    /// gits — and so a test can drive this half with no repo on disk.
    pub fn with_git(mut self, git: std::sync::Arc<dyn GitRefReader>) -> Self {
        self.git = git;
        self
    }

    /// Supply the instance-ownership predicate scan-root reports are gated on
    /// — see [`ScanReportGate`]. Without it this body sync reports nothing.
    pub fn with_scan_report_gate(mut self, gate: ScanReportGate) -> Self {
        self.scan_report_gate = gate;
        self
    }

    /// Swap in a different scan-root reporter. Test-only, for the same reason
    /// as [`LoopState::with_git`]: production always reports through its own
    /// sink.
    #[cfg(test)]
    fn with_reporter(
        mut self,
        reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
    ) -> Self {
        self.reporter = reporter;
        self
    }

    /// The reading the reconcile tick just recorded in `metrics`.
    fn current_reading(metrics: &AdapterMetrics) -> Option<ScanDivergence> {
        metrics
            .scan_divergence
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The idle tick's share of a body-sync cycle: no plans dir is configured,
    /// so nothing is scanned or pushed — but the scan-root reading
    /// (`not_scanning`) is still reported, under the same capture gate and
    /// posting policy as an armed cycle. Without it a device whose plans dir
    /// was cleared goes silent and its last `measured` row is quoted until it
    /// ages out.
    ///
    /// `censuses` is [`ScanCensusInputs::absent`] at every idle call site, and
    /// is a PARAMETER rather than an assumption so each of those sites states
    /// it: an idle cycle enumerated nothing, and nothing enumerated is
    /// UNKNOWN, never zero.
    pub async fn report_while_idle(
        &mut self,
        metrics: &AdapterMetrics,
        censuses: ScanCensusInputs,
    ) {
        if !(self.capture_gate)() {
            return;
        }
        self.report_scan_root_if_due(
            Self::current_reading(metrics),
            std::time::Instant::now(),
            censuses,
        )
        .await;
    }

    /// Publish this device's scan-root reading to the web read side
    /// (`POST /api/v1/plan-library/scan-roots`) when [`scan_report_due`] says
    /// so — plan `2026-09-11-the-plan-corpus-scan-root-does-not-report-its-own-drift`
    /// Revised Phase 2.
    ///
    /// Without this, the reading that says "this corpus is fed from a tree N
    /// commits behind" exists only in this process and its local UI, while the
    /// corpus it qualifies is read fleet-wide. A failure is logged and dropped:
    /// it never counts toward the body-push breaker (a backend that does not
    /// serve this route yet must not pause plan-body capture) and never
    /// affects the rest of the cycle.
    async fn report_scan_root_if_due(
        &mut self,
        current: Option<ScanDivergence>,
        now: std::time::Instant,
        censuses: ScanCensusInputs,
    ) {
        // Machine-scoped state: only the instance that owns shared root state
        // publishes it (see `ScanReportGate`). Checked first, on BOTH paths
        // that reach here — the armed cycle and the idle tick.
        if !(self.scan_report_gate)() {
            if !self.scan_report_gate_announced {
                self.scan_report_gate_announced = true;
                tracing::info!(
                    "plan library: this runner instance does not own the machine's shared \
                     state (a secondary / temp runner), so it does NOT publish the scan-root \
                     reading — the primary instance reports for this device"
                );
            }
            return;
        }
        let due = scan_report_due(
            self.last_scan_report.as_ref().map(|(d, at)| (d, *at)),
            self.last_scan_report_failure.as_ref().map(|(_, at)| *at),
            current.as_ref(),
            now,
            self.last_scan_report_unapplied,
        );
        let Some(current) = current.filter(|_| due) else {
            return;
        };
        // Enumerated only once the report is DUE: the work-tree listing is a
        // `read_dir` this cycle would otherwise pay for and throw away.
        let mut resolved: Vec<super::body_push::PlanSlugCensus> = Vec::new();
        if let Some(ref_census) = censuses.ref_census {
            resolved.push(ref_census);
        }
        if let Some(dir) = censuses.work_tree_dir {
            // Small (one `read_dir` of ~1,800 entries) but still synchronous
            // filesystem work on a runtime built with `worker_threads(1)`, so
            // it goes to the blocking pool for the same reason the scan does.
            match spawn_blocking_tracked(move || super::body_push::work_tree_census(&dir)).await {
                Ok(census) => resolved.extend(census),
                Err(e) => tracing::warn!(
                    error = %e,
                    "plan library: the work-tree slug census task did not complete; that side \
                     is reported ABSENT (never an empty set)"
                ),
            }
        }
        // Withhold a set the web already holds: `slugs: null` plus the digest
        // that re-asserts it. The web KEEPS its stored set on a digest match
        // and clears it to UNKNOWN on a mismatch, so this is only ever done
        // against a digest a previous report is known to have landed.
        let resolved: Vec<_> = resolved
            .into_iter()
            .map(|census| {
                if self.last_census_digests.get(&census.source) == Some(&census.digest) {
                    census.withheld()
                } else {
                    census
                }
            })
            .collect();
        let report =
            super::body_push::ScanRootReport::from_divergence(&current, chrono::Utc::now())
                .with_censuses(resolved);
        match self.reporter.report_scan_root(&report).await {
            Ok(ack) => {
                // Delivered either way — scheduling treats any 2xx as sent.
                // `applied: false` means the web kept a NEWER reading for this
                // device, which is almost always this device's clock having
                // stepped back; said once per episode.
                match ack.applied {
                    Some(false) if !self.last_scan_report_unapplied => {
                        self.last_scan_report_unapplied = true;
                        tracing::warn!(
                            state = %report.state,
                            observed_at = %report.observed_at,
                            "plan library: the web read side kept a NEWER scan-root reading for \
                             this device than the one just sent (applied: false) — usually this \
                             machine's clock stepped back. The stored row stays until a reading \
                             newer than it arrives"
                        );
                    }
                    Some(false) => tracing::debug!(
                        observed_at = %report.observed_at,
                        "plan library: scan-root report still not applied (a newer reading is stored)"
                    ),
                    Some(true) if self.last_scan_report_unapplied => {
                        self.last_scan_report_unapplied = false;
                        tracing::info!("plan library: scan-root reports are being applied again");
                    }
                    _ => {}
                }
                // REPLACED, never merged: a source this report left out is
                // stored by the web as UNKNOWN, so a digest kept for it would
                // re-assert a set the web no longer holds.
                //
                // The memory is kept ONLY on a positive, updating store, and
                // each of the other three arms is a way the web does not hold
                // the set this digest names:
                //
                //  * `applied: Some(false)` — a NEWER reading is stored and
                //    this one changed nothing.
                //  * `applied: None` — the body did not say (an older web
                //    build, an unparseable answer). That is UNKNOWN, and
                //    UNKNOWN does not get to render as "stored": withholding
                //    against it bets a set the web may not hold. The
                //    conservative arm costs one extra full census on an
                //    ambiguous ack, which is rare by construction
                //    [policy: `unknown-must-not-render-as-a-default`].
                //  * `created` anything but `Some(false)` — `Some(true)` is
                //    the row being INSERTED, and the insert arm stores the
                //    census verbatim rather than carrying a withheld one
                //    forward, so a `slugs: null` in THIS report was stored as
                //    UNKNOWN. `None` is the SAME UNKNOWN as `applied`'s (an
                //    older web build, an unparseable body) and gets the same
                //    treatment — not-stored — which is why the spelling is
                //    `== Some(false)` and not `!= Some(true)`: only a positive
                //    "this UPDATED an existing row" licenses a withhold.
                //    Costs nothing in the normal case: a first report already
                //    has an empty memory.
                let stored_the_set = ack.applied == Some(true) && ack.created == Some(false);
                self.last_census_digests = if stored_the_set {
                    report.census_digests()
                } else {
                    HashMap::new()
                };
                if self.last_scan_report_failure.take().is_some() {
                    tracing::info!(
                        state = %report.state,
                        counts_are_floors = report.counts_are_floors,
                        "plan library: scan-root reading accepted again by the web read side"
                    );
                } else if self.last_scan_report.is_none() {
                    tracing::info!(
                        state = %report.state,
                        source_repo = ?report.source_repo,
                        behind = ?report.behind,
                        ahead = ?report.ahead,
                        ref_age_secs = ?report.ref_age_secs,
                        counts_are_floors = report.counts_are_floors,
                        "plan library: published this device's scan-root reading (re-posted on \
                         change, and at least every 15 min)"
                    );
                } else {
                    tracing::debug!(
                        state = %report.state,
                        "plan library: scan-root reading re-posted"
                    );
                }
                self.last_scan_report = Some((current, now));
            }
            Err(failure) => {
                let repeat = self
                    .last_scan_report_failure
                    .as_ref()
                    .is_some_and(|(previous, _)| *previous == failure.kind);
                if repeat {
                    tracing::debug!(
                        kind = %failure.kind,
                        error = %failure.message,
                        "plan library: scan-root report still failing (same kind)"
                    );
                } else {
                    tracing::warn!(
                        kind = %failure.kind,
                        error = %failure.message,
                        retry_after_secs = SCAN_REPORT_RETRY_AFTER_FAILURE.as_secs(),
                        "plan library: could not publish this device's scan-root reading to the \
                         web read side — readers of the corpus cannot see how far this device's \
                         plans dir has drifted. Plan-body capture is unaffected and this does \
                         not count toward its breaker; retrying after the backoff"
                    );
                }
                // Nothing landed, so nothing may be withheld next time.
                self.last_census_digests.clear();
                self.last_scan_report_failure = Some((failure.kind, now));
            }
        }
    }

    /// One body-sync cycle. `metrics` is the reconcile loop's own — the tick
    /// that calls this has just recorded its scan-divergence reading there,
    /// and that reading is what the scan-root report publishes.
    ///
    /// `ref_census` is what the work-unit half's ref listing saw this cycle —
    /// the OTHER side of the coverage question, which only this device holds.
    /// `None` means no ref was listed, which is ABSENT, never empty.
    pub async fn run_cycle(
        &mut self,
        conv: &PlanConvention,
        metrics: &AdapterMetrics,
        ref_census: Option<super::body_push::PlanSlugCensus>,
    ) {
        self.run_cycle_pinned(
            conv,
            metrics,
            ref_census,
            std::sync::Arc::new(CycleRefPin::default()),
        )
        .await;
    }

    /// [`Self::run_cycle`] reading through a [`CycleRefPin`] the caller
    /// already resolved the work-unit half through, so both halves of one
    /// reconcile cycle publish from one commit.
    pub async fn run_cycle_pinned(
        &mut self,
        conv: &PlanConvention,
        metrics: &AdapterMetrics,
        ref_census: Option<super::body_push::PlanSlugCensus>,
        pin: std::sync::Arc<CycleRefPin>,
    ) {
        let gate_open = (self.capture_gate)();
        if let Some(message) = capture_gate_message(self.last_gate_open, gate_open) {
            tracing::info!(capture_enabled = gate_open, "{message}");
        }
        self.last_gate_open = Some(gate_open);
        if !gate_open {
            return;
        }
        // The scan-root report goes AFTER the capture gate (a tenant at
        // `plan_capture = off` publishes nothing from its plans dir) and BEFORE
        // both the breaker's pause and the `artifacts.is_empty()` return below.
        // Those two are exactly the cycles whose corpus is NOT being refreshed
        // — a paused sync, an empty scan — so they are the last ones that
        // should go quiet about the scan source.
        //
        // The work-tree census is taken of the ACTIVE plans root alone — the
        // one the reading's `source_repo` names — so the set difference the
        // web computes stays scoped to a single `source_repo` rather than
        // mixing the archive and prompts roots into one denominator.
        let work_tree_dir = self
            .roots
            .iter()
            .find(|r| r.label == super::body_push::PLANS_ROOT_LABEL)
            .map(|r| r.dir.clone());
        self.report_scan_root_if_due(
            Self::current_reading(metrics),
            std::time::Instant::now(),
            ScanCensusInputs {
                ref_census,
                work_tree_dir,
            },
        )
        .await;
        if self.breaker.should_skip_cycle() {
            return;
        }

        // On the blocking pool, and now for a second reason as well as the
        // first. The scan reads a body per plan (~1,863 on the measured box)
        // either way, and doing that inline blocks a tokio worker for the whole
        // walk, starving every other task sharing it. Since Phase 3 it ALSO
        // resolves each root's source, which spawns `git` and — on the first
        // resolution of a `(repo, ref)` in the cycle — performs a network
        // fetch on a 120 s budget (`SCAN_FETCH_TIMEOUT`). The pin dedups that
        // across both halves and every root, so a cycle pays it once per repo;
        // a root in a different repo still pays its own, so a tick's worst
        // case scales with the number of distinct repos. Size it deliberately
        // against the reconcile interval rather than assuming the old walk's
        // cost.
        let roots = self.roots.clone();
        let conv = conv.clone();
        let git = std::sync::Arc::clone(&self.git);
        let scanned =
            spawn_blocking_tracked(move || scan_roots_at_source(&roots, &conv, git.as_ref(), &pin))
                .await;
        let (artifacts, skipped) = match scanned {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "plan library: scan task failed to join; skipping this cycle"
                );
                return;
            }
        };
        if artifacts.is_empty() {
            return;
        }
        let summary =
            super::body_push::backfill_once(&self.sink, &artifacts, &mut self.state).await;
        // Only pushes that reached the network count as attempts — a file the
        // digest memory skipped made no call and cannot have failed.
        let attempted =
            summary.created + summary.updated + summary.unchanged_remote + summary.errors;
        tracing::info!(
            scanned = artifacts.len(),
            skipped = skipped.len(),
            created = summary.created,
            updated = summary.updated,
            unchanged = summary.unchanged_remote,
            skipped_local = summary.skipped_local,
            kind_forks = summary.ambiguous_kind,
            errors = summary.errors,
            edges_set = summary.edges_set,
            edges_skipped = summary.edges_skipped_applied,
            edges_given_up = summary.edges_given_up,
            "plan library: body sync cycle complete"
        );
        if self.breaker.record_cycle(attempted, summary.errors) {
            tracing::error!(
                errors = summary.errors,
                consecutive_cycles = TOTAL_FAILURE_CYCLES_BEFORE_PAUSE,
                pause_cycles = PAUSE_CYCLES,
                env_var = PLAN_LIBRARY_SYNC_ENV,
                "plan library: every push failed for {TOTAL_FAILURE_CYCLES_BEFORE_PAUSE} \
                 consecutive cycles — pausing the body sync for {PAUSE_CYCLES} cycles rather \
                 than retrying the same failures every tick. It RE-ARMS on its own; no restart \
                 is needed (and restarting a runner is forbidden by fleet policy). Fix the \
                 backend/credential, or use `qontinui-pr plan-library-backfill` for a one-shot \
                 run."
            );
        }
    }
}

/// Blank is unset, everywhere: a path setting configured to `""` (or
/// whitespace) disables that directory rather than scanning a directory
/// named `""`.
///
/// **Trims what it returns**, so the scalar and the per-tenant arm
/// ([`tenant_override`]) answer in the same shape. They used to disagree: this
/// filtered on `trim()` and returned the value UNTRIMMED, so a hand-edited
/// `settings.json` carrying `"plans_dir": " /x "` exported
/// `QONTINUI_PLANS_DIR=" /x "` from the scalar and `/x` from a map entry — two
/// spellings of one directory, reached through one resolver. Both save doors
/// normalise on write, so only a hand edit produces it; the asymmetry is still
/// removed here rather than relied on not to matter.
fn non_blank(configured: Option<String>) -> Option<String> {
    configured
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The per-tenant arm shared by all three resolvers: a **non-blank** entry for
/// `tenant` in `by_tenant`, trimmed, or `None`.
///
/// Blank-is-unset applies **per entry**, not just to the device scalar: an
/// entry configured to `""` falls through to the scalar rather than naming a
/// directory called `""`. And a `None` tenant never reads the map at all —
/// three session-launch paths have no acting tenant by design, and "no tenant"
/// must mean "the device default", never "some entry".
///
/// Lookup is an **exact** match on the canonical tenant-id form the device
/// reports (`Uuid::to_string()` — lowercase, hyphenated), which is what
/// `commands::tenant::get_active_tenant`'s `candidates` and every spawn-tenant
/// admission carry. A key in any other shape is therefore inert here, and that
/// is deliberate: settings D2 says an unparseable or currently-unbound key is
/// PRESERVED (the operator may be re-pairing) and never resolves.
fn tenant_override(by_tenant: &BTreeMap<String, String>, tenant: Option<&str>) -> Option<String> {
    let value = by_tenant.get(tenant?)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Resolve the **active** plans directory for `tenant` from the runner's
/// `PathSettings`, or `None` when the markdown-plan tier is off for it.
///
/// Two rungs, in order: `plans_dir_by_tenant[tenant]` when `tenant` is named
/// and its entry is non-blank, then the device-wide `plans_dir` scalar. A
/// tenant with no entry, and a launch with no tenant at all, both land on the
/// scalar — the fall-back is what keeps the markdown-plan tier armed for the
/// launch paths that have no acting tenant by design (a coord-spawned gate
/// continuation, a relayed spawn, a steward) instead of failing them closed.
/// Plan `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`.
///
/// There is deliberately **no environment override** on either rung. The one
/// that used to sit above this setting was a backward-compatibility shim for a
/// pre-settings deployment, and it silently outranked the setting — the
/// settings UI could show a directory that was not the one in effect. It was
/// migrated into the setting once at boot (the binary's `plans_dir_migration`)
/// and then deleted: one precedence chain, one source of truth.
///
/// Kept as its own name rather than having callers spell the filter
/// themselves because it is the documented seam every surface that needs
/// "the plans dir" goes through — the adapter, the session-env injection, the
/// settings view, the `harness` capture door and the plan-library write door —
/// so they resolve it identically by construction. The three resolvers share
/// one body for the same reason they keep three names: each is the seam for one
/// directory.
///
/// **Pure on purpose**, so it stays the unit the per-tenant contract is
/// asserted on: everything it knows arrives as an argument, and every consumer
/// re-reads the settings itself on every call.
pub fn resolve_plans_dir(
    configured: Option<String>,
    by_tenant: &BTreeMap<String, String>,
    tenant: Option<&str>,
) -> Option<String> {
    tenant_override(by_tenant, tenant).or_else(|| non_blank(configured))
}

/// Resolve the plans **archive** directory (D4) for `tenant` from
/// `PathSettings::plans_archive_dir_by_tenant` then
/// `PathSettings::plans_archive_dir`. Deliberately not derivable from the
/// active dir (it commonly lives in a different repo), and blank counts as
/// unset per entry as well as on the scalar — see [`resolve_plans_dir`] for the
/// precedence and for why `None` tenant means the device default.
pub fn resolve_plans_archive_dir(
    configured: Option<String>,
    by_tenant: &BTreeMap<String, String>,
    tenant: Option<&str>,
) -> Option<String> {
    tenant_override(by_tenant, tenant).or_else(|| non_blank(configured))
}

/// Resolve the **prompts** directory (plan `2026-08-10-plan-and-prompt-library-in-web`
/// Phase 2) for `tenant`: the third scan root, and the value exported to agent
/// sessions as `QONTINUI_PROMPTS_DIR`. `prompts_dir_by_tenant` first, then the
/// `prompts_dir` scalar.
///
/// **Not derivable from the plans dir.** `/create-plan` currently *guesses*
/// `$QONTINUI_PLANS_DIR/../prompts/*.md`, which is exactly the guess this
/// setting exists to replace — the operator's prompts live in more than one
/// repo and the sibling-of-plans relationship does not hold in general. Blank
/// counts as unset — see [`resolve_plans_dir`].
pub fn resolve_prompts_dir(
    configured: Option<String>,
    by_tenant: &BTreeMap<String, String>,
    tenant: Option<&str>,
) -> Option<String> {
    tenant_override(by_tenant, tenant).or_else(|| non_blank(configured))
}

/// Spawn the reconcile loop iff a coord base resolves for this runner
/// (`COORD_HTTP_URL`, or the active profile's `coord_url`). Returns `None`
/// (no-op) otherwise.
///
/// Whether the loop actually **scans** is decided on every tick from `paths`
/// — see [`PathReader`]: a runner with no `paths.plans_dir` spawns the loop
/// and idles in it, logging the tier as OFF once, until the setting is filled
/// in; filling it in takes effect within one interval with no restart. The
/// archive dir gates only the metadata-only archive scan (D4) and the prompts
/// dir only the library scan; the loop reconciles the active dir whether or
/// not either is set. Interval overridable via
/// `QONTINUI_PLAN_ADAPTER_INTERVAL_SECS` (default 60s).
///
/// `configured_backend_url` must be the **persisted** web-integration URL (and
/// `None` when web integration is disabled or unset), NOT an already-defaulted
/// one: [`super::body_push::resolve_backend_base`] promises to answer `None`
/// rather than guess a host, and a caller that pre-substitutes a build default
/// turns that promise into "always configured, possibly at production".
///
/// `capture_gate` reads the tenant's `plan_capture` fleet dial — see
/// [`CaptureGate`]. It is consulted every cycle, so flipping the dial takes
/// effect without a restart.
///
/// `scan_report_gate` says whether this instance may publish the machine's
/// scan-root reading — see [`ScanReportGate`]. Required here, the one
/// production entry point, so the binary cannot forget it; everything behind
/// it defaults to closed.
pub fn spawn_if_configured(
    paths: PathReader,
    configured_backend_url: Option<String>,
    capture_gate: CaptureGate,
    scan_report_gate: ScanReportGate,
) -> Option<tokio::task::JoinHandle<()>> {
    let sink = match super::push::HttpWorkUnitSink::from_profile() {
        Some(s) => s,
        None => {
            tracing::warn!(
                env_var = "COORD_HTTP_URL",
                setting = "profiles.<active>.coord_url",
                "plan adapter: no coord base configured; not starting — NO work unit is \
                 pushed to coord from this runner. Arm it by exporting COORD_HTTP_URL or \
                 connecting the active profile to a coord deployment"
            );
            return None;
        }
    };

    // Plan & prompt library body sync: on unless killed, AND needs a resolvable
    // web backend — see `body_sync_sink_if_enabled`. The work-unit reconcile is
    // unaffected either way, exactly as the archive scan is optional. Only the
    // SINK is fixed here; its scan roots follow the path settings tick by tick.
    let body_sync_sink = body_sync_sink_if_enabled(configured_backend_url);

    let interval_secs = std::env::var("QONTINUI_PLAN_ADAPTER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    // Supervised (plan 2026-09-03-…-supervisor Phase 4): a panic mid-cycle
    // used to end plan ingestion for the process's lifetime, silently. The
    // factory rebuilds `run_loop` from the same inputs — its edge-detection
    // maps and the body sync's digest state are per-run, so a rebuilt loop
    // re-seeds from coord exactly as a fresh process does (no transition is
    // replayed: the first cycle reads coord's CURRENT status as its seed).
    let sink = std::sync::Arc::new(sink);
    Some(crate::worker_supervisor::spawn_supervised(
        "plan_workunit_adapter.reconcile_loop",
        move || {
            let paths = paths.clone();
            let body_sync_sink = body_sync_sink.clone();
            let capture_gate = capture_gate.clone();
            let scan_report_gate = scan_report_gate.clone();
            let sink = sink.clone();
            async move {
                run_loop(
                    paths,
                    body_sync_sink,
                    capture_gate,
                    scan_report_gate,
                    &*sink,
                    interval_secs,
                )
                .await;
            }
        },
    ))
}

/// The web sink the library body sync pushes through, or `None`.
///
/// Two things answer `None`, and only ONE of them is the flag: the per-machine
/// kill switch ([`body_sync_enabled`], on by default), and — the guard that
/// actually protects a release build now that the sync is on by default — the
/// backend-resolution guard, `HttpArtifactSink::from_env` answering `None` when
/// no web backend resolves from env or `configured_backend_url`. Only the SINK
/// is decided at spawn; the scan roots follow the path settings every tick
/// ([`LoopState`]), so no sink means no `BodySync` on any tick. Factored out of
/// [`spawn_if_configured`] so that guard is a unit test rather than a claim.
fn body_sync_sink_if_enabled(
    configured_backend_url: Option<String>,
) -> Option<super::body_push::HttpArtifactSink> {
    if !body_sync_enabled() {
        // Not silent: a flag-off arm that logged nothing was indistinguishable
        // from a healthy sync (the plans-dir-absent branch in
        // `spawn_if_configured`, same shape, same reason).
        let observed = std::env::var(PLAN_LIBRARY_SYNC_ENV).ok();
        tracing::info!(
            env_var = PLAN_LIBRARY_SYNC_ENV,
            observed = observed.as_deref().unwrap_or("unset"),
            "{}",
            body_sync_disabled_message(observed.as_deref())
        );
        return None;
    }
    match super::body_push::HttpArtifactSink::from_env(configured_backend_url) {
        Some(sink) => {
            tracing::info!(
                backend = %sink.base(),
                "plan library: body sync enabled (scan roots follow the path settings \
                 every tick; still gated per-cycle on the tenant's plan_capture fleet \
                 dial)"
            );
            Some(sink)
        }
        None => {
            tracing::warn!(
                env_var = PLAN_LIBRARY_SYNC_ENV,
                "plan library: body sync is on (it is on by default; {PLAN_LIBRARY_SYNC_ENV}=0 \
                 kills it) but no qontinui-web backend is configured; not syncing"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::ParsedWorkUnit;
    use super::super::push::{SetDepsOutcome, TransitionBody, UpsertBody, ADAPTER_ACTOR};
    use super::*;
    use anyhow::Result;
    use std::sync::Mutex;

    // ---- scan-source divergence detector (plan 2026-09-10-…-not-a-ref, P1) ----

    /// A [`GitRefReader`] built from canned answers, so every arm of
    /// [`measure_scan_divergence`] — including the ones that need a repo 2153
    /// commits behind its own default branch — is reachable without touching a
    /// filesystem or spawning `git`.
    struct FakeGit {
        root: Result<Option<PathBuf>, String>,
        default_ref: Result<String, String>,
        /// rev -> resolved oid, or an error for that rev.
        revs: HashMap<String, Result<String, String>>,
        counts: Result<(u64, u64), String>,
        /// The canned per-source answers to `ref_refresh_stamps`.
        refresh_stamps: Vec<Result<Option<i64>, String>>,
        /// Phase 2: whether the pre-scan fetch succeeds. `Err` is the arm that
        /// must make a cycle publish NOTHING.
        fetch: Result<(), String>,
        /// Phase 2: the canned depth-1 listing of `<ref>:<dir>`.
        ref_dir: Result<Vec<RefDirEntry>, String>,
        /// Listings keyed by the ref `list_ref_dir` was ASKED for — a name or
        /// an object id. Consulted before `ref_dir`, so a test can make the
        /// listing at a resolved sha differ from the listing at the moving ref
        /// name it resolved from.
        ref_dir_by_rev: HashMap<String, Result<Vec<RefDirEntry>, String>>,
        /// Every rev `list_ref_dir` was called with, in order.
        listed_revs: Mutex<Vec<String>>,
        /// Phase 2: blob id -> its bytes, for `read_blobs`.
        blobs: HashMap<String, Result<String, String>>,
        /// Panic instead of answering `work_tree_root`. The only way to make
        /// a `spawn_blocking` scan task fail to JOIN, which is the arm where
        /// the enumeration did not happen at all.
        panic_on_work_tree_root: bool,
    }

    /// The fixed "now" every pure measurement in this module is taken at.
    const NOW: i64 = 1_789_000_000;

    impl FakeGit {
        /// A healthy clone: in a work tree, `origin/main` resolvable, both
        /// revs resolvable, `(behind, ahead)` as given, and the ref refreshed
        /// a minute before [`NOW`] — fresh.
        fn healthy(behind: u64, ahead: u64) -> Self {
            Self {
                root: Ok(Some(PathBuf::from("/repo"))),
                default_ref: Ok("origin/main".to_string()),
                revs: [
                    ("origin/main".to_string(), Ok("a".repeat(40))),
                    ("HEAD".to_string(), Ok("b".repeat(40))),
                ]
                .into_iter()
                .collect(),
                counts: Ok((behind, ahead)),
                refresh_stamps: vec![Ok(Some(NOW - 60))],
                fetch: Ok(()),
                ref_dir: Ok(Vec::new()),
                ref_dir_by_rev: HashMap::new(),
                listed_revs: Mutex::new(Vec::new()),
                blobs: HashMap::new(),
                panic_on_work_tree_root: false,
            }
        }

        /// [`Self::healthy`] with the ref last refreshed as given.
        fn refreshed(behind: u64, ahead: u64, refreshed_at: Result<Option<i64>, String>) -> Self {
            Self::stamped(behind, ahead, vec![refreshed_at])
        }

        /// [`Self::healthy`] with several sources' refresh records.
        fn stamped(
            behind: u64,
            ahead: u64,
            refresh_stamps: Vec<Result<Option<i64>, String>>,
        ) -> Self {
            Self {
                refresh_stamps,
                ..Self::healthy(behind, ahead)
            }
        }
    }

    impl GitRefReader for FakeGit {
        fn work_tree_root(&self, _dir: &Path) -> Result<Option<PathBuf>, String> {
            assert!(
                !self.panic_on_work_tree_root,
                "FakeGit: deliberately panicking so the blocking task fails to join"
            );
            self.root.clone()
        }
        fn default_ref(&self, _repo_root: &Path) -> Result<String, String> {
            self.default_ref.clone()
        }
        fn rev_parse(&self, _repo_root: &Path, rev: &str) -> Result<String, String> {
            self.revs
                .get(rev)
                .cloned()
                .unwrap_or_else(|| Err(format!("no canned answer for {rev}")))
        }
        fn count_behind_ahead(
            &self,
            _repo_root: &Path,
            _reference: &str,
            _head: &str,
        ) -> Result<(u64, u64), String> {
            self.counts.clone()
        }
        fn ref_refresh_stamps(
            &self,
            _repo_root: &Path,
            _default_ref: &str,
            _ref_sha: &str,
        ) -> Vec<Result<Option<i64>, String>> {
            self.refresh_stamps.clone()
        }

        fn fetch_default(&self, _repo_root: &Path, _default_ref: &str) -> Result<(), String> {
            self.fetch.clone()
        }

        fn list_ref_dir(
            &self,
            _repo_root: &Path,
            ref_name: &str,
            _rel_dir: &str,
        ) -> Result<Vec<RefDirEntry>, String> {
            self.listed_revs.lock().unwrap().push(ref_name.to_string());
            self.ref_dir_by_rev
                .get(ref_name)
                .cloned()
                .unwrap_or_else(|| self.ref_dir.clone())
        }

        fn read_blobs(&self, _repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>> {
            ids.iter()
                .map(|id| {
                    self.blobs
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| Err(format!("fake has no blob {id}")))
                })
                .collect()
        }
    }

    #[test]
    fn ref_scan_falls_back_to_the_tree_only_when_there_is_no_repo() {
        // A plans dir outside any repo is a SUPPORTED layout, not a
        // degradation: there is no ref to read, so the tree is the only source
        // and reading it is correct.
        let git = FakeGit {
            root: Ok(None),
            ..FakeGit::healthy(0, 0)
        };
        assert_eq!(
            super::super::ref_scan::CycleRefPin::default()
                .resolve_source(&git, Path::new("/plans")),
            super::super::ref_scan::ScanSource::WorkTree
        );
    }

    #[test]
    fn a_failed_fetch_publishes_nothing_rather_than_falling_back_to_the_tree() {
        // THE load-bearing arm of Phase 2. Falling back here would reinstate
        // the exact defect the phase removes, and would do it precisely when
        // the ref is least trustworthy.
        let git = FakeGit {
            root: Ok(Some(PathBuf::from("/repo"))),
            fetch: Err("network is unreachable".to_string()),
            ..FakeGit::healthy(0, 0)
        };
        match super::super::ref_scan::CycleRefPin::default()
            .resolve_source(&git, Path::new("/repo/plans"))
        {
            super::super::ref_scan::ScanSource::Unavailable { reason } => {
                assert!(
                    reason.contains("network is unreachable"),
                    "the cause must survive into the reason: {reason}"
                );
            }
            other => panic!("a failed fetch must not yield {other:?}"),
        }
    }

    #[test]
    fn an_unresolvable_default_branch_is_unavailable_not_a_guess_at_origin_main() {
        let git = FakeGit {
            default_ref: Err("no origin/HEAD".to_string()),
            ..FakeGit::healthy(0, 0)
        };
        assert!(matches!(
            super::super::ref_scan::CycleRefPin::default()
                .resolve_source(&git, Path::new("/repo/plans")),
            super::super::ref_scan::ScanSource::Unavailable { .. }
        ));
    }

    #[test]
    fn a_healthy_clone_scans_the_ref_at_the_repo_relative_dir() {
        let git = FakeGit::healthy(0, 0);
        assert_eq!(
            super::super::ref_scan::CycleRefPin::default()
                .resolve_source(&git, Path::new("/repo/plans")),
            super::super::ref_scan::ScanSource::Ref {
                repo_root: PathBuf::from("/repo"),
                ref_name: "origin/main".to_string(),
                rel_dir: "plans".to_string(),
            }
        );
    }

    #[test]
    fn the_ref_walk_is_depth_one_and_md_only_and_survives_one_bad_blob() {
        use super::super::trigger::RefDirEntry;
        let git = FakeGit {
            ref_dir: Ok(vec![
                RefDirEntry {
                    name: "b.md".into(),
                    id: "idb".into(),
                },
                RefDirEntry {
                    name: "a.md".into(),
                    id: "ida".into(),
                },
                // Not markdown — skipped, as the tree walk skips it.
                RefDirEntry {
                    name: "notes.txt".into(),
                    id: "idt".into(),
                },
                // A blob that will not read: skipped with a warning, never
                // fatal — one bad entry must not discard the rest.
                RefDirEntry {
                    name: "bad.md".into(),
                    id: "idx".into(),
                },
            ]),
            blobs: [
                ("ida".to_string(), Ok("# A".to_string())),
                ("idb".to_string(), Ok("# B".to_string())),
                ("idx".to_string(), Err("corrupt".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeGit::healthy(0, 0)
        };
        let got =
            super::super::ref_scan::read_ref_dir(&git, Path::new("/repo"), "origin/main", "plans")
                .expect("listing succeeded");
        let names: Vec<_> = got.files.iter().map(|f| f.name.as_str()).collect();
        // Sorted, markdown only, the unreadable one dropped. `notes.txt` never
        // appears, and NOTHING from a subdirectory can appear because the
        // listing itself is depth 1.
        assert_eq!(names, vec!["a.md", "b.md"]);
        // ...and the skip is REPORTED. `bad.md` is in the ref; the read just
        // could not produce it, so a caller reasoning about absence must not
        // take the short set as "`bad.md` is gone".
        //
        // Neuter check: drop `complete = false` from `read_ref_dir`'s
        // per-blob `Err` arm and this fails.
        assert!(
            !got.complete,
            "a blob the listing named but the read could not produce is a GAP"
        );
        // The CENSUS is the other set: what the listing SAW, `bad.md`
        // included, because a plan whose blob will not read still EXISTS on
        // the ref side and must stay in the denominator. `notes.txt` is not a
        // plan on either.
        assert_eq!(got.names, vec!["a.md", "b.md", "bad.md"]);
        assert_eq!(
            got.ref_sha.as_deref(),
            Some("a".repeat(40)).as_deref(),
            "the sha the stems were listed AT, resolved at the listing"
        );
    }

    #[test]
    fn an_unreadable_listing_publishes_nothing() {
        let git = FakeGit {
            ref_dir: Err("bad object".to_string()),
            ..FakeGit::healthy(0, 0)
        };
        assert!(super::super::ref_scan::read_ref_dir(
            &git,
            Path::new("/repo"),
            "origin/main",
            "plans"
        )
        .is_err());
    }

    /// A batch read that fails WHOLESALE is an error, not an empty corpus.
    ///
    /// Per file an unreadable blob is a skip — one bad object must not discard
    /// the other 1,099, which the test above pins. ALL of them at once is a
    /// different event: a `git` that would not spawn, a batch killed by its
    /// watchdog, a severed pipe. `read_blobs` cannot tell the two apart (it
    /// fans one batch failure across every slot, and must, since per-slot they
    /// are identical), so the whole-batch judgement belongs here — and without
    /// it `Ok(vec![])` hands reconcile an empty corpus and marks every plan in
    /// the fleet disappeared.
    ///
    /// Neuter check: delete the `out.is_empty()` guard in `read_ref_dir` and
    /// this fails.
    #[test]
    fn a_wholesale_blob_failure_is_an_error_not_an_empty_corpus() {
        let git = FakeGit {
            ref_dir: Ok(vec![
                RefDirEntry {
                    name: "a.md".into(),
                    id: "ida".into(),
                },
                RefDirEntry {
                    name: "b.md".into(),
                    id: "idb".into(),
                },
            ]),
            // No canned blobs at all — every slot fails, which is what
            // `read_blobs` produces when the batch process itself never ran.
            blobs: HashMap::new(),
            ..FakeGit::healthy(0, 0)
        };
        let err =
            super::super::ref_scan::read_ref_dir(&git, Path::new("/repo"), "origin/main", "plans")
                .expect_err("every blob failing is ONE broken read, not an empty dir");
        assert!(err.contains("none of the 2"), "got: {err}");
    }

    /// `.md` alone is not a plan on EITHER arm. `read_plan_dir`'s
    /// `extension()` is `None` for it; a bare `ends_with(".md")` here said yes.
    /// The claim this phase makes is that the two arms scan the SAME set, and
    /// an unexamined one-file difference is how that claim stops being true.
    #[test]
    fn a_file_named_only_md_is_not_a_plan_at_the_ref() {
        let git = FakeGit {
            ref_dir: Ok(vec![
                RefDirEntry {
                    name: ".md".into(),
                    id: "idh".into(),
                },
                RefDirEntry {
                    name: "real.md".into(),
                    id: "idr".into(),
                },
            ]),
            blobs: [
                ("idh".to_string(), Ok("# hidden".to_string())),
                ("idr".to_string(), Ok("# real".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeGit::healthy(0, 0)
        };
        let got =
            super::super::ref_scan::read_ref_dir(&git, Path::new("/repo"), "origin/main", "plans")
                .expect("listing succeeded");
        let names: Vec<_> = got.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["real.md"]);
        assert!(
            got.complete,
            "a name that is not a plan is excluded, not skipped — the read is whole"
        );
        assert_eq!(got.names, vec!["real.md"], "and not in the census either");
    }

    /// **The census's stems and its `ref_sha` come from ONE resolution.**
    ///
    /// `rev_parse` runs FIRST and the listing is addressed by the object id it
    /// returned. Resolving after the listing, by ref NAME, made them two reads
    /// of a moving target in two `git` processes: a concurrent `git fetch` in
    /// the same clone — routine on this fleet's shared checkouts — advances
    /// `origin/main` between them, and the census then asserts stems listed at
    /// A under a sha of B. Nothing about the report looks wrong afterwards,
    /// which is why this needs a test rather than a comment.
    ///
    /// The fake answers the sha and the name with DIFFERENT listings, which is
    /// what a fetch landing between the two calls looks like from in here.
    ///
    /// Neuter check: list at `ref_name` again and the stems come back as the
    /// post-fetch set while `ref_sha` still names the pre-fetch commit.
    #[test]
    fn the_ref_listing_is_taken_at_the_resolved_sha_not_at_the_moving_ref_name() {
        let sha = "a".repeat(40);
        let git = FakeGit {
            // What `origin/main` answers AFTER the concurrent fetch.
            ref_dir: Ok(vec![RefDirEntry {
                name: "2026-01-02-landed-since.md".into(),
                id: "id-after".into(),
            }]),
            // What the resolved commit holds — the set actually being reported.
            ref_dir_by_rev: [(
                sha.clone(),
                Ok(vec![RefDirEntry {
                    name: "2026-01-01-at-the-sha.md".into(),
                    id: "id-at".into(),
                }]),
            )]
            .into_iter()
            .collect(),
            blobs: [
                ("id-at".to_string(), Ok("# at the sha".to_string())),
                ("id-after".to_string(), Ok("# landed since".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeGit::healthy(0, 0)
        };

        let got =
            super::super::ref_scan::read_ref_dir(&git, Path::new("/repo"), "origin/main", "plans")
                .expect("listing succeeded");
        assert_eq!(
            *git.listed_revs.lock().unwrap(),
            vec![sha.clone()],
            "the listing is addressed by object id, not by the ref name"
        );
        assert_eq!(got.ref_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(
            got.names,
            vec!["2026-01-01-at-the-sha.md"],
            "the stems are the ones the reported sha holds"
        );
        let files: Vec<_> = got.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(files, vec!["2026-01-01-at-the-sha.md"]);
    }

    /// A rev that will not resolve leaves `ref_sha` UNKNOWN and still LISTS —
    /// at the ref name, which is the only address left. The stems are the
    /// reading; the sha only qualifies it, so losing the qualifier must not
    /// lose the reading.
    #[test]
    fn an_unresolvable_rev_still_lists_at_the_name_with_an_unknown_sha() {
        let git = FakeGit {
            revs: HashMap::new(),
            ref_dir: Ok(vec![RefDirEntry {
                name: "2026-01-01-plan.md".into(),
                id: "id-1".into(),
            }]),
            blobs: [("id-1".to_string(), Ok("# a plan".to_string()))]
                .into_iter()
                .collect(),
            ..FakeGit::healthy(0, 0)
        };
        let got =
            super::super::ref_scan::read_ref_dir(&git, Path::new("/repo"), "origin/main", "plans")
                .expect("an unresolvable rev must not fail the listing");
        assert_eq!(got.ref_sha, None, "UNKNOWN, never a guess");
        assert_eq!(got.names, vec!["2026-01-01-plan.md"]);
        assert_eq!(*git.listed_revs.lock().unwrap(), vec!["origin/main"]);
    }

    /// **The commit's headline claim, and the one nothing asserted before:**
    /// both arms of [`read_plans_for_cycle`] produce the SAME
    /// [`ParsedWorkUnit`] for the same bytes. If the ref arm derived a
    /// different slug or `source_path`, moving the scan source would churn
    /// every coord row in the corpus on the cycle it shipped — a 1,100-row
    /// rewrite dressed as a source change.
    ///
    /// Neuter check: change the ref arm's `dir.join(&f.name)` to
    /// `PathBuf::from(&f.name)` and this fails on `source_path`.
    #[test]
    fn read_plans_for_cycle_arms_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "# A plan\n\n> **Status: VETTED**\n\nBody.\n";
        std::fs::write(tmp.path().join("2026-01-01-a-plan.md"), body).unwrap();

        // Tree arm: not in a repo, so the working tree is the only source.
        let tree = read_plans_for_cycle(
            tmp.path(),
            &PlanConvention::operator_default(),
            &FakeGit {
                root: Ok(None),
                ..FakeGit::healthy(0, 0)
            },
            &CycleRefPin::default(),
        )
        .expect("the tree arm reads");

        // Ref arm: the same dir IS the repo root, and the ref serves the same
        // bytes under the same name.
        let refd = read_plans_for_cycle(
            tmp.path(),
            &PlanConvention::operator_default(),
            &FakeGit {
                root: Ok(Some(tmp.path().to_path_buf())),
                ref_dir: Ok(vec![RefDirEntry {
                    name: "2026-01-01-a-plan.md".into(),
                    id: "ida".into(),
                }]),
                blobs: [("ida".to_string(), Ok(body.to_string()))]
                    .into_iter()
                    .collect(),
                ..FakeGit::healthy(0, 0)
            },
            &CycleRefPin::default(),
        )
        .expect("the ref arm reads");

        assert_eq!(tree.units.len(), 1, "the fixture holds exactly one plan");
        assert!(tree.complete && refd.complete, "both reads were whole");
        assert_eq!(
            tree.units, refd.units,
            "same bytes must parse to the same unit, or the source move churns coord"
        );
        // The CENSUS is where the arms deliberately differ: the tree arm
        // listed no ref, so the ref side is ABSENT — UNKNOWN, never the empty
        // set, which would claim the default branch holds no plans.
        assert_eq!(tree.ref_census, None);
        let census = refd.ref_census.expect("the ref arm listed a ref");
        assert_eq!(census.source, "ref");
        assert_eq!(census.count, 1);
        assert_eq!(
            census.slugs.as_deref(),
            Some(["2026-01-01-a-plan".to_string()].as_slice()),
            "STEMS, not file names — the same identity the corpus is keyed on"
        );
    }

    /// The no-fallback contract at the function that IMPLEMENTS it.
    ///
    /// `a_failed_fetch_publishes_nothing_rather_than_falling_back_to_the_tree`
    /// pins `CycleRefPin::resolve_source`'s VARIANT and would still pass if this arm
    /// were changed back to `read_plan_dir(dir, conv)` — reinstating the whole
    /// defect. This one puts a perfectly readable plan on disk and demands it
    /// NOT come back.
    ///
    /// Neuter check: replace the `Unavailable` arm with
    /// `Ok(read_plan_dir(dir, conv))` and this fails.
    #[test]
    fn an_unavailable_source_publishes_nothing_even_with_a_readable_tree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("2026-01-01-a-plan.md"),
            "# A\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let err = read_plans_for_cycle(
            tmp.path(),
            &PlanConvention::operator_default(),
            &FakeGit {
                root: Ok(Some(tmp.path().to_path_buf())),
                fetch: Err("no route to host".to_string()),
                ..FakeGit::healthy(0, 0)
            },
            &CycleRefPin::default(),
        )
        .expect_err("a failed fetch must not fall back to the tree");
        assert!(err.contains("no route to host"), "got: {err}");
    }

    /// State 1 of 4. A machine with no `paths.plans_dir` scans NOTHING, and
    /// must say so out loud: `NotScanning` with no counts at all. The one
    /// thing it may never be is `0 behind / 0 ahead`, which is what "the scan
    /// is in step" looks like.
    #[test]
    fn scan_divergence_reports_not_scanning_when_no_plans_dir_is_configured() {
        let d = measure_scan_divergence(None, &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.state, ScanDivergenceState::NotScanning);
        assert_eq!(d.state.as_str(), "not_scanning");
        assert_eq!(d.plans_dir, None);
        assert_eq!(d.repo_root, None);
        assert_eq!(d.default_ref, None);
        assert_eq!(
            (d.behind, d.ahead),
            (None, None),
            "a tier-off machine has no divergence to report, and reporting zero would claim it \
             agrees with a ref it never read"
        );
        assert!(!d.is_stale());
    }

    /// State 2 of 4. A plans dir outside any repo is a SUPPORTED
    /// configuration, so it is not an error — but it is not zero divergence
    /// either. There is no ref, so there are no counts.
    #[test]
    fn scan_divergence_reports_not_a_git_work_tree_rather_than_zero() {
        let git = FakeGit {
            root: Ok(None),
            ..FakeGit::healthy(0, 0)
        };
        let d = measure_scan_divergence(Some(Path::new("/plain/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::NotAGitWorkTree);
        assert_eq!(d.state.as_str(), "not_a_git_work_tree");
        assert_eq!(d.plans_dir.as_deref(), Some("/plain/plans"));
        assert_eq!(d.repo_root, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        let detail = d.detail.expect("NotAGitWorkTree must name why");
        assert!(!detail.is_empty());
        assert!(
            detail.contains("/plain/plans"),
            "the detail names the dir it is talking about: {detail}"
        );
    }

    /// State 3 of 4, and the number the plan was written for: a plans dir
    /// parked 2153 behind / 11 ahead of its own default branch reads exactly
    /// that, with both shas and the resolved ref carried.
    #[test]
    fn scan_divergence_measures_a_parked_working_tree() {
        let d = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 11),
            NOW,
        );
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.state.as_str(), "measured");
        assert_eq!(d.plans_dir.as_deref(), Some("/repo/plans"));
        assert_eq!(d.repo_root.as_deref(), Some("/repo"));
        assert_eq!(d.default_ref.as_deref(), Some("origin/main"));
        assert_eq!(d.ref_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(d.head_sha.as_deref(), Some("b".repeat(40).as_str()));
        assert_eq!(d.behind, Some(2153));
        assert_eq!(d.ahead, Some(11));
        assert_eq!(d.detail, None, "a clean measurement explains nothing");
        assert!(d.is_stale());
    }

    /// `Measured` zeros are REAL zeros — the one state allowed to render
    /// `0 behind / 0 ahead`, because it actually compared.
    ///
    /// What it compared is HEAD against the ref, and nothing more: a checkout
    /// sitting exactly on the default branch with uncommitted plan files still
    /// reads `0/0` here while publishing bytes no ref carries. The name says
    /// `head_is_in_step` rather than `is_a_real_zero` so no reader concludes
    /// more than was measured.
    #[test]
    fn scan_divergence_measured_zero_means_head_is_in_step_not_the_working_tree() {
        let d =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!((d.behind, d.ahead), (Some(0), Some(0)));
        assert!(!d.is_stale());
    }

    /// The conflation this phase exists to end, in its sharpest form: a root
    /// probe that FAILED (no `git` on PATH, a nonexistent dir, a 20s timeout
    /// on a stalled mount) must read `Unknown`, never the benign
    /// `NotAGitWorkTree`. Those two differ by whether anything was
    /// established, which is exactly the distinction a shrugging reader loses.
    #[test]
    fn scan_divergence_is_unknown_when_the_work_tree_probe_itself_fails() {
        let git = FakeGit {
            root: Err("`git rev-parse --show-toplevel` did not answer (SpawnError)".to_string()),
            ..FakeGit::healthy(0, 0)
        };
        let d = measure_scan_divergence(Some(Path::new("/maybe/plans")), &git, NOW);
        assert_eq!(
            d.state,
            ScanDivergenceState::Unknown,
            "a probe that could not ask establishes nothing, least of all that the dir is not \
             a repo"
        );
        assert_ne!(d.state, ScanDivergenceState::NotAGitWorkTree);
        assert_eq!(d.plans_dir.as_deref(), Some("/maybe/plans"));
        assert_eq!(d.repo_root, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("SpawnError"));
    }

    /// State 4 of 4, arm A: a work tree whose default branch cannot be
    /// resolved is UNKNOWN with a reason — never a hardcoded `origin/main`,
    /// because a divergence measured against the wrong branch is a
    /// confidently wrong number.
    #[test]
    fn scan_divergence_is_unknown_when_the_default_branch_does_not_resolve() {
        let git = FakeGit {
            default_ref: Err("`origin/HEAD` is not set in this clone".to_string()),
            ..FakeGit::healthy(5, 5)
        };
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.state.as_str(), "unknown");
        assert_eq!(d.repo_root.as_deref(), Some("/repo"));
        assert_eq!(
            d.default_ref, None,
            "nothing resolved, so nothing is claimed — least of all a guess"
        );
        assert_eq!((d.behind, d.ahead), (None, None));
        let detail = d.detail.expect("Unknown must name why");
        assert!(
            detail.contains("origin/HEAD"),
            "the failing probe's own words survive into the detail: {detail}"
        );
    }

    /// State 4 of 4, arm B: the ref and HEAD resolve but the count fails. What
    /// WAS established is kept (repo root, ref name, both shas) and only the
    /// counts stay UNKNOWN.
    #[test]
    fn scan_divergence_is_unknown_when_the_count_fails_but_keeps_what_resolved() {
        let git = FakeGit {
            counts: Err("bad revision".to_string()),
            ..FakeGit::healthy(1, 1)
        };
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.default_ref.as_deref(), Some("origin/main"));
        assert_eq!(d.ref_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("bad revision"));
        assert!(!d.state.as_str().is_empty());
    }

    /// State 4 of 4, arm C: HEAD itself is unresolvable (an unborn branch in a
    /// fresh clone). Still UNKNOWN, still explained.
    #[test]
    fn scan_divergence_is_unknown_when_head_does_not_resolve() {
        let mut git = FakeGit::healthy(1, 1);
        git.revs.insert(
            "HEAD".to_string(),
            Err("ambiguous argument 'HEAD': unknown revision".to_string()),
        );
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.head_sha, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("unknown revision"));
    }

    /// Ahead-only divergence is stale too. This machine, once it catches up,
    /// reads 0 behind / 11 ahead — eleven commits of plans on no ref — and
    /// that must reach the log at WARN, not INFO.
    #[test]
    fn ahead_only_divergence_counts_as_stale() {
        let ahead_only = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(0, 11),
            NOW,
        );
        assert!(
            ahead_only.is_stale(),
            "0 behind / 11 ahead is not agreement"
        );
        let behind_only = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 0),
            NOW,
        );
        assert!(behind_only.is_stale());
        let in_step =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0), NOW);
        assert!(!in_step.is_stale());
        // The three non-Measured states never claim staleness — they have no
        // counts to claim it from.
        assert!(!ScanDivergence::not_scanning().is_stale());
        assert!(!ScanDivergence::unknown(None, "x").is_stale());
    }

    /// The other half of the orientation, and the half a parser test cannot
    /// see: WHICH rev lands on the left of the `...`. Swap the two
    /// interpolations and every reading inverts while
    /// `left_right_count_parses_behind_then_ahead` still passes.
    #[test]
    fn left_right_range_puts_the_reference_on_the_left() {
        assert_eq!(
            left_right_range("origin/main", "HEAD"),
            "origin/main...HEAD"
        );
        assert_eq!(
            left_right_range("origin/trunk", "HEAD"),
            "origin/trunk...HEAD"
        );
    }

    /// The orientation, pinned at the wire. `rev-list --left-right --count
    /// <ref>...HEAD` emits `<left>\t<right>` where LEFT is what the ref has
    /// and HEAD lacks (BEHIND) and RIGHT is the mirror (AHEAD). Swapping them
    /// turns a 2153-commit park into a 2153-commit lead, which reads like a
    /// busy authoring machine instead of a stale one — the failure this whole
    /// detector would then be reporting backwards.
    #[test]
    fn left_right_count_parses_behind_then_ahead() {
        assert_eq!(parse_left_right_count("2153\t11"), Ok((2153, 11)));
        assert_eq!(parse_left_right_count("2153\t11\n"), Ok((2153, 11)));
        assert_eq!(parse_left_right_count("0\t0"), Ok((0, 0)));
        // A space-separated variant parses identically — the split is on
        // whitespace, so no locale or pager setting can flip the meaning.
        assert_eq!(parse_left_right_count("7 2"), Ok((7, 2)));
    }

    /// Anything that is not exactly two numbers is an ERROR, not a defaulted
    /// zero: a parse that silently yields `(0, 0)` would report a parked tree
    /// as in step.
    #[test]
    fn left_right_count_refuses_to_default_on_unparseable_output() {
        for raw in ["", "12", "1\t2\t3", "a\tb", "-1\t2"] {
            assert!(
                parse_left_right_count(raw).is_err(),
                "{raw:?} must not parse to a count"
            );
        }
    }

    /// The STORE overwrites rather than merges: a later reading fully replaces
    /// an earlier one, so a measurement can never outlive the configuration it
    /// was taken under.
    ///
    /// This pins the store alone — it calls [`record_scan_divergence`]
    /// directly, so it says nothing about WHERE in the tick the call sits.
    /// That the idle path reaches it at all is pinned by
    /// `an_idle_tick_records_not_scanning_rather_than_nothing`.
    #[test]
    fn recording_a_divergence_overwrites_the_previous_reading() {
        let metrics = AdapterMetrics::default();
        assert_eq!(
            metrics.snapshot().scan_divergence,
            None,
            "before the first tick the reading is UNKNOWN, not 'no divergence'"
        );

        let measured = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 11),
            NOW,
        );
        record_scan_divergence(measured.clone(), &metrics);
        assert_eq!(metrics.snapshot().scan_divergence, Some(measured));

        // The operator clears `paths.plans_dir`: the idle tick must replace
        // the stale measurement with `NotScanning`, never leave it in place.
        record_scan_divergence(ScanDivergence::not_scanning(), &metrics);
        let after = metrics
            .snapshot()
            .scan_divergence
            .expect("idle still records");
        assert_eq!(after.state, ScanDivergenceState::NotScanning);
        assert_eq!((after.behind, after.ahead), (None, None));
    }

    /// The out-of-band UNKNOWN constructor (a probe task that would not run)
    /// keeps the dir it was measuring and always carries a reason.
    #[test]
    fn unknown_constructor_carries_the_dir_and_a_reason() {
        let d = ScanDivergence::unknown(Some("/repo/plans".to_string()), "task join failed");
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.plans_dir.as_deref(), Some("/repo/plans"));
        assert_eq!(d.detail.as_deref(), Some("task join failed"));
        assert_eq!((d.behind, d.ahead), (None, None));
    }

    // ---- ref freshness + the floor rule (plan 2026-09-11-…-its-own-drift, Revised P1) ----

    const HOUR: i64 = 3600;

    fn measured_with(
        refreshed_at: Result<Option<i64>, String>,
        behind: u64,
        ahead: u64,
    ) -> ScanDivergence {
        measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(behind, ahead, refreshed_at),
            NOW,
        )
    }

    /// A ref refreshed five minutes ago — the census's own cadence — is
    /// current: the counts are real numbers, not floors, and a clean
    /// measurement still explains nothing.
    #[test]
    fn a_fresh_ref_makes_the_counts_current_not_floors() {
        let d = measured_with(Ok(Some(NOW - 300)), 2153, 11);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, Some(300));
        assert_eq!(d.ref_is_fresh(), Some(true));
        assert!(!d.counts_are_floors());
        assert_eq!(d.detail, None);
        assert!(
            d.is_stale(),
            "is_stale keeps its meaning: measured non-zero counts"
        );
    }

    /// A ref seven hours old is past the window: the counts become LOWER
    /// BOUNDS, and the measured numbers themselves are preserved — a floor is
    /// a qualification of the counts, not a reason to drop them.
    #[test]
    fn a_seven_hour_old_ref_makes_the_counts_floors_and_keeps_them() {
        let d = measured_with(Ok(Some(NOW - 7 * HOUR)), 2153, 11);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, Some(7 * 3600));
        assert_eq!(d.ref_is_fresh(), Some(false));
        assert!(d.counts_are_floors());
        assert_eq!((d.behind, d.ahead), (Some(2153), Some(11)));
        assert!(d.is_stale());
    }

    /// The window's edge: exactly six hours is still fresh, one second more
    /// is a floor.
    #[test]
    fn the_freshness_window_is_inclusive_at_six_hours() {
        let window = i64::try_from(SCAN_REF_FRESH_WITHIN.as_secs()).unwrap();
        assert_eq!(window, 6 * HOUR);
        assert!(!measured_with(Ok(Some(NOW - window)), 0, 0).counts_are_floors());
        assert!(measured_with(Ok(Some(NOW - window - 1)), 0, 0).counts_are_floors());
    }

    /// No source records a refresh at all: the age is UNKNOWN, and unknown is
    /// a floor — never taken as fresh. The state stays `Measured` (the counts
    /// are real) and the detail says why the age is missing.
    #[test]
    fn an_unknown_ref_age_is_a_floor_never_fresh() {
        let d = measured_with(Ok(None), 0, 0);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!(d.ref_is_fresh(), None);
        assert!(
            d.counts_are_floors(),
            "a 0/0 against a ref of unknown age proves nothing and must not read as in step"
        );
        assert_eq!((d.behind, d.ahead), (Some(0), Some(0)));
        let detail = d.detail.expect("an absent age is explained");
        assert!(detail.contains("unknown"), "{detail}");
    }

    /// The age PROBE failing is not the measurement failing: `Measured` is
    /// kept with the real counts, `ref_age_secs` is `None`, the counts are
    /// floors, and the probe's own words reach the detail.
    #[test]
    fn a_failed_age_probe_keeps_measured_with_no_age() {
        let d = measured_with(
            Err("`git reflog show` did not answer (Timeout)".to_string()),
            40,
            2,
        );
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!((d.behind, d.ahead), (Some(40), Some(2)));
        assert!(d.counts_are_floors());
        assert!(d.detail.unwrap().contains("Timeout"));
    }

    /// A refresh stamped in the future (clock skew) saturates at age 0 rather
    /// than wrapping into an astronomically old ref.
    #[test]
    fn a_future_refresh_saturates_at_age_zero() {
        let d = measured_with(Ok(Some(NOW + 120)), 0, 0);
        assert_eq!(d.ref_age_secs, Some(0));
        assert_eq!(ref_age_from(NOW, NOW + 120), 0);
        assert_eq!(ref_age_from(NOW, NOW - 5), 5);
        // The tolerance's edge is still ordinary skew.
        let edge = measured_with(Ok(Some(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS)), 0, 0);
        assert_eq!(edge.ref_age_secs, Some(0));
    }

    /// A refresh record dated FAR in the future — a clock corrected backwards
    /// after a fetch, a checkout restored with future mtimes — proves nothing
    /// about when the ref was refreshed. Clamping it to age 0 would be a false
    /// "fresh"; it is an UNKNOWN age instead, so the counts are floors, and
    /// the detail says why.
    #[test]
    fn a_far_future_refresh_is_an_unknown_age_not_fresh() {
        let d = measured_with(Ok(Some(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS + 1)), 0, 0);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!(d.ref_is_fresh(), None);
        assert!(d.counts_are_floors());
        let detail = d.detail.expect("an unknown age is explained");
        assert!(detail.contains("more than 300s in the FUTURE"), "{detail}");

        let a_day_ahead = measured_with(Ok(Some(NOW + 24 * HOUR)), 7, 0);
        assert!(a_day_ahead.counts_are_floors());
        assert_eq!(
            a_day_ahead.behind,
            Some(7),
            "the counts themselves are kept"
        );
    }

    /// The three non-`Measured` states have no counts, so there is nothing
    /// for the floor rule to qualify — and nothing to call fresh either.
    #[test]
    fn non_measured_states_have_no_freshness_and_no_floors() {
        for d in [
            ScanDivergence::not_scanning(),
            ScanDivergence::unknown(None, "x"),
            measure_scan_divergence(
                Some(Path::new("/plain/plans")),
                &FakeGit {
                    root: Ok(None),
                    ..FakeGit::healthy(0, 0)
                },
                NOW,
            ),
        ] {
            assert_eq!(d.ref_is_fresh(), None, "{:?}", d.state);
            assert!(!d.counts_are_floors(), "{:?}", d.state);
            assert_eq!(d.ref_age_secs, None);
        }
    }

    /// The log wording IS the floor rule's read surface. A `0/0` floor must
    /// never produce the benign "reading changed" line a fresh `0/0` produces:
    /// it logs at WARN and says the counts are a lower bound, naming the age —
    /// or naming that the age is unknown.
    #[test]
    fn a_floor_of_zero_never_logs_as_in_step() {
        let (fresh_warn, fresh_text) =
            scan_divergence_message(&measured_with(Ok(Some(NOW - 60)), 0, 0));
        assert!(!fresh_warn, "a proven-current 0/0 is the benign reading");
        assert_eq!(
            fresh_text,
            "plan adapter: scan-source divergence reading changed"
        );

        let (warn, text) = scan_divergence_message(&measured_with(Ok(Some(NOW - 7 * HOUR)), 0, 0));
        assert!(warn);
        assert_ne!(text, fresh_text);
        assert!(text.contains("LOWER BOUND"), "{text}");
        assert!(text.contains("25200s"), "names the age: {text}");

        let (warn, text) = scan_divergence_message(&measured_with(Ok(None), 0, 0));
        assert!(warn);
        assert!(
            text.contains("LOWER BOUND") && text.contains("UNKNOWN age"),
            "{text}"
        );

        // A stale floor says both things: diverged, and at least this much.
        let (warn, text) =
            scan_divergence_message(&measured_with(Ok(Some(NOW - 7 * HOUR)), 2153, 11));
        assert!(warn);
        assert!(
            text.contains("LOWER BOUNDS") && text.contains("WORKING TREE"),
            "{text}"
        );

        // A stale reading against a FRESH ref keeps the parked-tree text and
        // names how old the ref it counted against is.
        let (warn, text) = scan_divergence_message(&measured_with(Ok(Some(NOW - 300)), 2153, 11));
        assert!(warn);
        assert!(!text.contains("LOWER BOUND"), "{text}");
        assert!(text.contains("300s ago"), "{text}");
    }

    /// The ref's age grows every tick; only its CROSSING of the window is a
    /// change. Plain equality would re-log the reading every minute and
    /// re-post it every cycle.
    #[test]
    fn a_reading_is_the_same_while_its_age_advances_inside_the_window() {
        let a = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let b = measured_with(Ok(Some(NOW - 120)), 5, 0);
        assert_ne!(a, b, "the raw ages differ");
        assert!(a.is_same_reading(&b));

        let crossed = measured_with(Ok(Some(NOW - 7 * HOUR)), 5, 0);
        assert!(!a.is_same_reading(&crossed), "fresh -> floors is news");
        let unknown = measured_with(Ok(None), 5, 0);
        assert!(!a.is_same_reading(&unknown), "fresh -> unknown age is news");

        let moved = measured_with(Ok(Some(NOW - 60)), 6, 0);
        assert!(!a.is_same_reading(&moved), "a count change is news");
    }

    /// The store always keeps the LATEST reading, age included, even when the
    /// change detector (and so the log) treats it as the same reading.
    #[test]
    fn recording_keeps_the_latest_age_even_when_the_reading_is_the_same() {
        let metrics = AdapterMetrics::default();
        record_scan_divergence(measured_with(Ok(Some(NOW - 60)), 5, 0), &metrics);
        record_scan_divergence(measured_with(Ok(Some(NOW - 120)), 5, 0), &metrics);
        assert_eq!(
            metrics.snapshot().scan_divergence.unwrap().ref_age_secs,
            Some(120)
        );
    }

    // ---- the two refresh sources, parsed and combined ----

    #[test]
    fn default_branch_name_strips_the_remote() {
        assert_eq!(default_branch_name("origin/main"), "main");
        assert_eq!(default_branch_name("origin/release/2026"), "release/2026");
        assert_eq!(default_branch_name("upstream/trunk"), "trunk");
        assert_eq!(default_branch_name("main"), "main");
    }

    /// `FETCH_HEAD` counts only when it names the default branch AT the sha
    /// the tracking ref now holds.
    #[test]
    fn fetch_head_counts_only_the_default_branch_at_the_current_sha() {
        let sha = "a".repeat(40);
        let other = "c".repeat(40);
        let url = "https://github.com/qontinui/qontinui-dev-notes";
        // A single-branch fetch (the census's refspec shape).
        assert!(fetch_head_names_ref(
            &format!("{sha}\t\tbranch 'main' of {url}\n"),
            "main",
            &sha
        ));
        // A full `git fetch origin`: main marked not-for-merge among others.
        let all = format!(
            "{other}\t\tbranch 'feature' of {url}\n{sha}\tnot-for-merge\tbranch 'main' of {url}\n"
        );
        assert!(fetch_head_names_ref(&all, "main", &sha));
        // Another branch only: refreshes nothing we compare against.
        assert!(!fetch_head_names_ref(
            &format!("{other}\t\tbranch 'feature' of {url}\n"),
            "main",
            &other
        ));
        // A prefix of the name is a different branch.
        assert!(!fetch_head_names_ref(
            &format!("{sha}\t\tbranch 'main-old' of {url}\n"),
            "main",
            &sha
        ));
        // Right name, wrong sha: the fetch did not land in the tracking ref.
        assert!(!fetch_head_names_ref(
            &format!("{other}\t\tbranch 'main' of {url}\n"),
            "main",
            &sha
        ));
        // Tags and garbage.
        assert!(!fetch_head_names_ref(
            &format!("{sha}\tnot-for-merge\ttag 'main' of {url}\n"),
            "main",
            &sha
        ));
        assert!(!fetch_head_names_ref("", "main", &sha));
        assert!(!fetch_head_names_ref("not a fetch head", "main", &sha));
    }

    /// The reflog selector's number is the ENTRY time. Empty is an absent
    /// source; `@{0}` (an index, not a date) must not parse as 1970.
    #[test]
    fn reflog_entry_time_parses_the_selector_and_refuses_an_index() {
        assert_eq!(
            parse_reflog_entry_time("origin/main@{1789129011}"),
            Ok(Some(1_789_129_011))
        );
        assert_eq!(
            parse_reflog_entry_time("refs/remotes/origin/main@{1789129011}\n"),
            Ok(Some(1_789_129_011))
        );
        assert_eq!(parse_reflog_entry_time(""), Ok(None));
        assert_eq!(parse_reflog_entry_time("  \n"), Ok(None));
        for raw in [
            "origin/main@{0}",
            "origin/main@{3}",
            "origin/main",
            "origin/main@{soon}",
        ] {
            assert!(parse_reflog_entry_time(raw).is_err(), "{raw:?}");
        }
    }

    /// The freshest TRUSTWORTHY record wins; a failed probe beside a known
    /// timestamp keeps the timestamp (an overstated age at worst); a failure
    /// beside nothing is a probe failure; nothing at all is "no record".
    #[test]
    fn the_fresher_refresh_source_wins_and_partial_failure_leans_old() {
        let err = || Err::<Option<i64>, String>("probe failed".to_string());
        let combine = |stamps| combine_refresh_stamps(stamps, NOW);
        assert_eq!(
            combine(vec![Ok(Some(10)), Ok(Some(20))]),
            RefreshVerdict::At(20)
        );
        assert_eq!(
            combine(vec![Ok(Some(30)), Ok(Some(20))]),
            RefreshVerdict::At(30)
        );
        assert_eq!(
            combine(vec![Ok(None), Ok(Some(20))]),
            RefreshVerdict::At(20)
        );
        assert_eq!(combine(vec![Ok(None), Ok(None)]), RefreshVerdict::NoRecord);
        assert_eq!(combine(vec![]), RefreshVerdict::NoRecord);
        assert_eq!(combine(vec![Ok(Some(10)), err()]), RefreshVerdict::At(10));
        assert_eq!(combine(vec![err(), Ok(Some(10))]), RefreshVerdict::At(10));
        assert!(matches!(
            combine(vec![Ok(None), err()]),
            RefreshVerdict::ProbeFailed(_)
        ));
        assert!(matches!(
            combine(vec![err(), err()]),
            RefreshVerdict::ProbeFailed(_)
        ));
    }

    /// A future-dated record is dropped BEFORE the fresher is chosen, so it
    /// cannot hide a trustworthy fresh one; only when nothing trustworthy
    /// remains is the age unknown, and then the verdict names the future date.
    #[test]
    fn a_future_dated_source_never_hides_a_trustworthy_one() {
        let future = NOW + SCAN_REF_FUTURE_TOLERANCE_SECS + 1;
        assert_eq!(
            combine_refresh_stamps(vec![Ok(Some(future)), Ok(Some(NOW - 90))], NOW),
            RefreshVerdict::At(NOW - 90),
            "the trustworthy record answers even though the future one is larger"
        );
        assert_eq!(
            combine_refresh_stamps(vec![Ok(Some(future))], NOW),
            RefreshVerdict::OnlyFutureDated
        );
        assert_eq!(
            combine_refresh_stamps(vec![Ok(Some(future)), Ok(None), Err("x".to_string())], NOW),
            RefreshVerdict::OnlyFutureDated,
            "the future date is what the detail names, not the probe failure"
        );
        // Within the tolerance it is ordinary skew and still counts.
        assert_eq!(
            combine_refresh_stamps(vec![Ok(Some(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS))], NOW),
            RefreshVerdict::At(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS)
        );

        // Through the measurement: a fresh FETCH_HEAD beside a future reflog
        // still reads fresh.
        let d = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::stamped(3, 0, vec![Ok(Some(NOW - 90)), Ok(Some(future))]),
            NOW,
        );
        assert_eq!(d.ref_age_secs, Some(90));
        assert!(!d.counts_are_floors());
        assert_eq!(d.detail, None);
    }

    /// One source's probe failing costs only that source: the per-worktree
    /// `FETCH_HEAD` answer survives a failed `--git-common-dir`.
    #[test]
    fn one_failed_source_does_not_discard_another() {
        let d = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::stamped(
                3,
                0,
                vec![
                    Ok(Some(NOW - 30)),
                    Err(
                        "`git rev-parse --git-common-dir` did not answer: it exited non-zero"
                            .to_string(),
                    ),
                    Ok(Some(NOW - 7 * HOUR)),
                ],
            ),
            NOW,
        );
        assert_eq!(d.ref_age_secs, Some(30));
        assert!(!d.counts_are_floors());
    }

    /// The future-stamp detail carries no number that moves with the clock:
    /// two ticks 60 s apart over the same future-dated record are the SAME
    /// reading — no WARN and no POST per minute for the length of the skew.
    #[test]
    fn a_future_stamp_reads_the_same_on_every_tick() {
        let future = NOW + 3 * HOUR;
        let git = FakeGit::refreshed(4, 0, Ok(Some(future)));
        let first = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        let second = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW + 60);
        assert_eq!(first.ref_age_secs, None);
        assert_eq!(first.detail, second.detail);
        assert!(first.is_same_reading(&second));
        assert!(!scan_divergence_changed(Some(&first), &second));
    }

    // ---- ProcessGit against real repos ----

    /// ProcessGit's combined verdict for `origin/main` in `repo`, as of now.
    fn refresh_verdict(repo: &Path, sha: &str) -> RefreshVerdict {
        combine_refresh_stamps(
            ProcessGit.ref_refresh_stamps(repo, "origin/main", sha),
            chrono::Utc::now().timestamp(),
        )
    }

    /// The two `FETCH_HEAD` sources alone, combined.
    fn fetch_head_verdict(repo: &Path, sha: &str) -> RefreshVerdict {
        combine_refresh_stamps(
            ProcessGit::fetch_head_stamps(repo, "origin/main", sha),
            chrono::Utc::now().timestamp(),
        )
    }

    fn assert_refreshed_near_now(verdict: RefreshVerdict, what: &str) {
        let now = chrono::Utc::now().timestamp();
        match verdict {
            RefreshVerdict::At(at) => assert!((now - at).abs() <= 10, "{what}: {at} vs now {now}"),
            other => panic!("{what}: expected a refresh near now, got {other:?}"),
        }
    }

    /// Run `git` in `dir` with an isolated identity and no signing, optionally
    /// pinning the committer date (which is also the REFLOG entry time).
    fn real_git(dir: &Path, args: &[&str], committer_date: Option<&str>) -> String {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=CI",
                "-c",
                "user.email=ci@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args);
        if let Some(date) = committer_date {
            cmd.env("GIT_COMMITTER_DATE", date);
        }
        let out = cmd.output().expect("git spawns");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare `origin` carrying `main` and `feature`, and a `reader` clone
    /// that has fetched `main` with its REFLOG entry pinned to 2020 — so the
    /// reflog alone would read the ref as six years old.
    fn origin_and_old_reflog_reader() -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let writer = tmp.path().join("writer");
        let reader = tmp.path().join("reader");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&writer).unwrap();
        std::fs::create_dir_all(&reader).unwrap();
        real_git(&origin, &["init", "-q", "--bare", "-b", "main"], None);
        real_git(&writer, &["init", "-q", "-b", "main"], None);
        real_git(
            &writer,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &writer,
            &["commit", "-q", "--allow-empty", "-m", "one"],
            None,
        );
        real_git(&writer, &["push", "-q", "origin", "main"], None);
        real_git(&writer, &["checkout", "-q", "-b", "feature"], None);
        real_git(&writer, &["commit", "-q", "--allow-empty", "-m", "f"], None);
        real_git(&writer, &["push", "-q", "origin", "feature"], None);
        real_git(&reader, &["init", "-q", "-b", "main"], None);
        real_git(
            &reader,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &reader,
            &["fetch", "-q", "origin", "main"],
            Some("@1600000000 +0000"),
        );
        let sha = real_git(&reader, &["rev-parse", "refs/remotes/origin/main"], None);
        (tmp, reader, sha)
    }

    // ---- Phase 3: the DOCUMENT layer takes its bytes from the ref too ----

    /// A plans scan root at `dir`, the shape `BodySync` builds in production.
    fn plans_root(dir: &Path) -> super::super::body_push::ScanRoot {
        super::super::body_push::ScanRoot::new(
            dir.to_path_buf(),
            super::super::body_push::ScanRootKind::Plans,
            super::super::body_push::PLANS_ROOT_LABEL,
        )
    }

    /// **The parity test the document layer never had.**
    ///
    /// Phase 2 shipped `read_plans_for_cycle_arms_agree` for the WORK-UNIT
    /// layer and nothing at all for this one, which is exactly why a green CI
    /// could coexist with a corpus that never refreshed: no Phase 2 test ever
    /// executed `body_push`. Both arms must build the SAME `ScannedArtifact`
    /// from the same bytes — same kind, same slug, same `source_repo`, same
    /// recorded `source_path`, same sha.
    ///
    /// NOTE this doc previously ended "or the ref arm mints a second row per
    /// plan … the corpus DOUBLES rather than refreshes". That was FALSE and is
    /// struck here rather than quietly dropped: identity is
    /// `(kind, slug, source_repo)`, the slug is BASENAME-derived and
    /// `source_repo` comes from the root, so a divergent path would update the
    /// SAME row with a different `source_path`. See `scan_one_root_at_ref`,
    /// where the same false claim was retracted — it survived here, in the doc
    /// of the very test that supposedly guarded it, which is how a retraction
    /// leaves a reader worse off than no retraction.
    ///
    /// Neuter check: change `scan_one_root_at_ref`'s `root.dir.join(&f.name)`
    /// to `PathBuf::from(&f.name)` and this fails on the upsert's path.
    #[test]
    fn body_sync_arms_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "# A plan\n\n> **Status: VETTED 2026-09-22.**\n\nBody.\n";
        std::fs::write(tmp.path().join("2026-01-01-a-plan.md"), body).unwrap();
        let root = plans_root(tmp.path());
        let conv = PlanConvention::operator_default();

        // Tree arm: not in a repo, so the tree is the only source there is.
        let (tree, tree_skipped) = scan_roots_at_source(
            std::slice::from_ref(&root),
            &conv,
            &FakeGit {
                root: Ok(None),
                ..FakeGit::healthy(0, 0)
            },
            &CycleRefPin::default(),
        );

        // Ref arm: the same dir IS the repo root, serving the same bytes.
        let (refd, ref_skipped) = scan_roots_at_source(
            std::slice::from_ref(&root),
            &conv,
            &FakeGit {
                root: Ok(Some(tmp.path().to_path_buf())),
                ref_dir: Ok(vec![RefDirEntry {
                    name: "2026-01-01-a-plan.md".into(),
                    id: "ida".into(),
                }]),
                blobs: [("ida".to_string(), Ok(body.to_string()))]
                    .into_iter()
                    .collect(),
                ..FakeGit::healthy(0, 0)
            },
            &CycleRefPin::default(),
        );

        assert_eq!(tree.len(), 1, "the fixture holds exactly one plan");
        assert_eq!(
            tree, refd,
            "same bytes must build the same artifact — same kind, slug, source_repo, \
             source_path and sha"
        );
        // NOT asserted: that the two arms produce the same SKIP records. They
        // do not, and an earlier draft asserted it — vacuously, because this
        // fixture has one file and no subdirectory, so both sides were empty.
        // The tree walk emits `subdirectory_not_scanned` per subdir,
        // `dangling_symlink` per broken `*.md` link, plus `unreadable_entry` /
        // `unreadable_dir`; the ref arm sees none of those
        // because `list_ref_dir` filters trees out before `scan_one_root_at_ref`
        // is reached. Add one subdirectory to this fixture and the old
        // assertion fails. What this test pins is CLASSIFICATION parity, which
        // is what `classify_one` exists to guarantee; listing parity is a
        // different property and is NOT claimed [see `scan_roots_at_source`'s
        // symlink and subdirectory notes].
        let _ = (tree_skipped, ref_skipped);
    }

    /// **The Phase 3 defect, demonstrated.**
    ///
    /// The tree carries the OLD body and the ref carries the NEW one — which
    /// is the steady state on any checkout behind its own default branch, and
    /// was measured at 717 commits behind on merytshost. Before this phase the
    /// document layer published the tree's bytes, so a plan amended on
    /// `origin/main` changed no file here, the digest memory saw no change,
    /// and the artifact's `updated_at` FROZE — two stems were measured frozen
    /// in the same cycle (finding 61b51044).
    ///
    /// Neuter check: point the `Ref` arm of `scan_roots_at_source` at
    /// `scan_one_root` and this fails — it gets the parked body back.
    #[test]
    fn the_body_sync_reads_the_ref_not_the_parked_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let parked = "# A plan\n\n> **Status: DRAFT 2026-09-01.**\n\nThe PARKED body.\n";
        let at_ref = "# A plan\n\n> **Status: SHIPPED 2026-09-22.**\n\nThe REF body.\n";
        std::fs::write(tmp.path().join("2026-01-01-a-plan.md"), parked).unwrap();

        let (got, _) = scan_roots_at_source(
            &[plans_root(tmp.path())],
            &PlanConvention::operator_default(),
            &FakeGit {
                root: Ok(Some(tmp.path().to_path_buf())),
                ref_dir: Ok(vec![RefDirEntry {
                    name: "2026-01-01-a-plan.md".into(),
                    id: "ida".into(),
                }]),
                blobs: [("ida".to_string(), Ok(at_ref.to_string()))]
                    .into_iter()
                    .collect(),
                ..FakeGit::healthy(717, 0)
            },
            &CycleRefPin::default(),
        );

        assert_eq!(got.len(), 1);
        let pushed = &got[0].upsert;
        assert!(
            pushed.body.contains("The REF body"),
            "the ref's bytes must be what is published; got: {:?}",
            pushed.body
        );
        assert!(
            !pushed.body.contains("The PARKED body"),
            "the parked tree's bytes must NOT be published"
        );
        assert_eq!(
            pushed.status, "shipped",
            "the status published is the REF's, which is the whole point: a SUPERSEDED \
             stamp on origin/main has to reach the corpus"
        );
    }

    /// No fallback to the tree, ever — the same contract the work-unit arm
    /// carries. A root whose source will not resolve contributes NOTHING, and
    /// the corpus keeps the bodies it already has.
    ///
    /// Substituting the tree here would republish the parked bytes under the
    /// same identity, which is the defect rather than a degrade
    /// [policy: `unknown-must-not-render-as-a-default`].
    #[test]
    fn an_unavailable_root_contributes_nothing_and_never_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("2026-01-01-a-plan.md"),
            "# A\n\n> **Status: DRAFT 2026-09-01.**\n\nParked.\n",
        )
        .unwrap();

        for (label, reason, git) in [
            (
                "fetch fails",
                "scan_source_unavailable",
                FakeGit {
                    root: Ok(Some(tmp.path().to_path_buf())),
                    fetch: Err("no route to host".into()),
                    ..FakeGit::healthy(0, 0)
                },
            ),
            (
                "no origin/HEAD",
                "scan_source_unavailable",
                FakeGit {
                    root: Ok(Some(tmp.path().to_path_buf())),
                    default_ref: Err("`origin/HEAD` is not set in this clone".into()),
                    ..FakeGit::healthy(0, 0)
                },
            ),
            (
                "listing unreadable",
                "unreadable_ref",
                FakeGit {
                    root: Ok(Some(tmp.path().to_path_buf())),
                    ref_dir: Err("bad object".into()),
                    ..FakeGit::healthy(0, 0)
                },
            ),
        ] {
            let (got, skipped) = scan_roots_at_source(
                &[plans_root(tmp.path())],
                &PlanConvention::operator_default(),
                &git,
                &CycleRefPin::default(),
            );
            assert!(
                got.is_empty(),
                "{label}: an unresolvable source must publish nothing, never the parked tree"
            );
            // ...and the dry-run report SAYS the root contributed nothing,
            // rather than rendering a root with zero plans and zero skips.
            //
            // Neuter check: drop either root-level `skipped.push` in
            // `scan_roots_at_source` and the matching labels fail here.
            assert_eq!(
                skipped,
                vec![super::super::body_push::SkippedFile {
                    path: tmp.path().to_string_lossy().to_string(),
                    reason,
                }],
                "{label}: a dark root must be RECORDED as skipped"
            );
        }
    }

    /// A ref listing that could not read one blob publishes the rest AND
    /// records the missing plan as skipped — the ref-arm twin of the tree
    /// walk's `unreadable_file`. Before this, `RefListing::complete` was
    /// dropped on the document half, so the catch-up dry run reported a
    /// partial root as a whole one: the plan was absent from the count and
    /// from the skipped list alike.
    ///
    /// Neuter check: make `record_ref_listing_gaps` return immediately and
    /// this fails on the skipped list.
    #[test]
    fn a_partial_ref_listing_records_each_unread_plan_as_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "# A plan

> **Status: DRAFT 2026-09-01.**

Body.
";
        let git = FakeGit {
            root: Ok(Some(tmp.path().to_path_buf())),
            ref_dir: Ok(vec![
                RefDirEntry {
                    name: "2026-01-01-good.md".into(),
                    id: "idg".into(),
                },
                RefDirEntry {
                    name: "2026-01-02-bad.md".into(),
                    id: "idb".into(),
                },
            ]),
            blobs: [
                ("idg".to_string(), Ok(body.to_string())),
                ("idb".to_string(), Err("corrupt".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeGit::healthy(0, 0)
        };
        let (got, skipped) = scan_roots_at_source(
            &[plans_root(tmp.path())],
            &PlanConvention::operator_default(),
            &git,
            &CycleRefPin::default(),
        );
        assert_eq!(got.len(), 1, "the readable plan is still published");
        assert_eq!(
            skipped,
            vec![super::super::body_push::SkippedFile {
                path: tmp
                    .path()
                    .join("2026-01-02-bad.md")
                    .to_string_lossy()
                    .to_string(),
                reason: "unreadable_file",
            }],
            "the unread plan is named, at the path the tree walk would record"
        );

        // A WHOLE listing records nothing: the gap record is for gaps only.
        let (whole, none) = scan_roots_at_source(
            &[plans_root(tmp.path())],
            &PlanConvention::operator_default(),
            &FakeGit {
                blobs: [
                    ("idg".to_string(), Ok(body.to_string())),
                    ("idb".to_string(), Ok(body.to_string())),
                ]
                .into_iter()
                .collect(),
                ..git
            },
            &CycleRefPin::default(),
        );
        assert_eq!(whole.len(), 2);
        assert!(
            none.is_empty(),
            "a complete listing skips nothing: {none:?}"
        );
    }

    /// The eviction POLICY, directly — the coverage that was lost when the
    /// first (vacuous) cache test was deleted.
    ///
    /// Those are two different properties and BOTH need pinning. The
    /// through-`read_blobs` test below pins the WIRING (that `read_blobs` does
    /// not evict another caller's ids); this pins the POLICY (that eviction is
    /// by least-recently-used against the byte bound, and that a hit re-stamps).
    /// With only the wiring test, `BLOB_CACHE_MAX_BYTES` is unreachable from
    /// any test — the bound short-circuits — so deleting the
    /// `slot.1 = blob_cache_tick()` re-stamp left the whole suite green.
    ///
    /// Neuter check, VERIFIED: change `retain(|_, (_, n)| *n > cutoff)` to `<`
    /// and this fails.
    ///
    /// An earlier version of this doc ALSO claimed "delete the re-stamp in
    /// `read_blobs`' hit arm and this fails". It does NOT — measured: that
    /// neuter left this test green, because the test stamps its entries by hand
    /// and never executes the hit arm. The claim was written without being run.
    /// The re-stamp is covered by the `touch_blob` assertions below instead,
    /// with the honest bound on that coverage stated there.
    #[test]
    fn eviction_is_least_recently_used_against_the_byte_bound() {
        let mut cache: HashMap<String, (String, u64)> = HashMap::new();
        // 8 entries of 100 bytes each; stamps ascending, so `cold0` is coldest.
        for i in 0..8u64 {
            cache.insert(format!("cold{i}"), ("x".repeat(100), i));
        }

        // Under the bound, NOTHING is evicted — this is the arm that makes a
        // second caller's read safe.
        evict_cold_blobs(&mut cache, 100 * 1024);
        assert_eq!(cache.len(), 8, "under the byte bound nothing is evicted");

        // Over the bound: the COLDEST go, the HOTTEST stay, a quarter at a time.
        evict_cold_blobs(&mut cache, 400);
        assert!(cache.len() < 8, "over the bound something is evicted");
        assert!(
            cache.contains_key("cold7"),
            "the most recently used entry survives"
        );
        assert!(
            !cache.contains_key("cold0"),
            "the least recently used entry is the one evicted"
        );
        assert_eq!(
            cache.len(),
            5,
            "a QUARTER plus the cutoff entry goes (8 -> 5), so eviction is amortised \
             rather than one entry per insert"
        );

        // The bound is BYTES, not entries: two huge entries must evict where
        // two small ones would not. An entry cap could not express this, and
        // that is why the constant changed.
        let mut big: HashMap<String, (String, u64)> = HashMap::new();
        big.insert("a".into(), ("x".repeat(4096), 1));
        big.insert("b".into(), ("x".repeat(4096), 2));
        evict_cold_blobs(&mut big, 4096);
        assert!(
            big.len() < 2,
            "the byte bound is what decides, not the entry count"
        );

        // The RE-STAMP — the half an eviction test cannot reach on its own. A
        // hit must become most-recently-used, or a body only one caller reads
        // ages out while being read every cycle.
        //
        // This pins `touch_blob` itself. The WIRING — that the read path calls
        // it, and evicts after its reads — is pinned separately and
        // hermetically by `read_blobs_into_re_stamps_hits_and_evicts_after`,
        // against a local map and a tiny bound.
        //
        // An earlier version of this comment claimed the wiring COULD NOT be
        // pinned, because "reaching the hit arm under eviction needs a global
        // bound override, and such an override would race". That was false: the
        // route is to PARAMETERISE rather than override, and this crate already
        // does it for `wedge_diagnostics`' lane table for exactly this reason.
        // Left recorded because a wrong reason attached to a real decision
        // argues a closeable gap shut — which is what it did for one review
        // round.
        // Stamps come FROM `blob_cache_tick()`, never hardcoded. The counter
        // is a `fetch_add` returning the PREVIOUS value, so it starts at 0 and
        // every production stamp is drawn from it — meaning the counter is
        // always at or ahead of every stamp in the map. Hardcoding 1 and 2 here
        // violated that invariant and made the touch produce a COLDER stamp
        // than the entries it was compared against; the test then failed for
        // its own reason rather than the code's. Successive `fetch_add`s are
        // ordered even across the parallel tests sharing this counter, so
        // RELATIVE assertions are sound while absolute values are not.
        let mut lru: HashMap<String, (String, u64)> = HashMap::new();
        let stamp_old = blob_cache_tick();
        let stamp_new = blob_cache_tick();
        assert!(stamp_new > stamp_old, "the counter is monotonic");
        lru.insert("old".into(), ("body-old".into(), stamp_old));
        lru.insert("new".into(), ("body-new".into(), stamp_new));
        assert_eq!(
            touch_blob(&mut lru, "old").as_deref(),
            Some("body-old"),
            "a hit serves the cached body"
        );
        let old_stamp = lru["old"].1;
        let new_stamp = lru["new"].1;
        assert!(
            old_stamp > new_stamp,
            "the touched entry must become the HOTTEST ({old_stamp} vs {new_stamp}) — \
             without the re-stamp it keeps its insert stamp and is evicted first"
        );
        assert_eq!(touch_blob(&mut lru, "absent"), None, "a miss is None");
    }

    /// The WIRING of the cache into the read path, hermetically: against a
    /// LOCAL map and a bound small enough to cross, so no global state and no
    /// `git` is involved and nothing races the rest of the test binary.
    ///
    /// This is the coverage my own doc comment claimed was unreachable. It is
    /// reachable because `read_blobs_into` takes the cache and the bound as
    /// parameters — the pattern `wedge_diagnostics::spawn_blocking_tracked_in`
    /// already established here.
    ///
    /// Neuter checks: revert the hit arm to a non-re-stamping `cache.get(id)`
    /// and the re-stamp assertion fails; delete the `evict_cold_blobs` call and
    /// the eviction assertion fails.
    #[test]
    fn read_blobs_into_re_stamps_hits_and_evicts_after() {
        // Pre-populated so EVERY id hits — no `git` is reachable from here, and
        // a miss would try to spawn one against a path that does not exist.
        let mut cache: HashMap<String, (String, u64)> = HashMap::new();
        let cold = blob_cache_tick();
        let warm = blob_cache_tick();
        cache.insert("cold".into(), ("x".repeat(600), cold));
        cache.insert("warm".into(), ("y".repeat(600), warm));

        // Read ONLY the cold one. Its stamp must overtake the other's, or a
        // body that just one caller reads ages out while being read.
        let out = ProcessGit::read_blobs_into(
            &mut cache,
            Path::new("/nonexistent-on-purpose"),
            &["cold".to_string()],
            100 * 1024,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].as_deref(),
            Ok("x".repeat(600).as_str()),
            "the hit is served from the cache, not from git"
        );
        assert!(
            cache["cold"].1 > cache["warm"].1,
            "reading `cold` must make it the HOTTEST ({} vs {}) — this is the \
             re-stamp, wired",
            cache["cold"].1,
            cache["warm"].1
        );

        // And eviction runs AFTER the reads, against the bound passed in.
        let out = ProcessGit::read_blobs_into(
            &mut cache,
            Path::new("/nonexistent-on-purpose"),
            &["cold".to_string()],
            700,
        );
        assert_eq!(out[0].as_deref(), Ok("x".repeat(600).as_str()));
        assert!(
            !cache.contains_key("warm"),
            "1200 bytes held against a 700-byte bound must evict, and the \
             just-read `cold` is not the one to go"
        );
        assert!(cache.contains_key("cold"), "the entry just read survives");
    }

    /// `read_blobs` must stay a PURE DELEGATION to `read_blobs_into`.
    ///
    /// Pinned by source text, exactly as `wedge_diagnostics` pins its lane
    /// wrapper, and for the same reason: a behavioural test against the
    /// process-global cache cannot drive it past a 96 MB bound, so if the body
    /// grew back into the wrapper nothing would cover it — which is the state
    /// this change just left.
    #[test]
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn read_blobs_wrapper_is_a_pure_delegation() {
        let src = include_str!("trigger.rs");
        let at = src
            .find("fn read_blobs(&self, repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>> {\n        if ids.is_empty()")
            .expect("the ProcessGit impl of read_blobs is findable by signature");
        let body: String = src[at..]
            .chars()
            .take(900)
            .collect::<String>()
            .split("\n    }")
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            body.contains("Self::read_blobs_into(&mutcache,repo_root,ids,BLOB_CACHE_MAX_BYTES)"),
            "the wrapper no longer delegates to \
             `read_blobs_into(&mut cache, repo_root, ids, BLOB_CACHE_MAX_BYTES)`. If this \
             is a deliberate signature change and the wrapper is STILL a pure \
             delegation, update this pin in the same change. Body:\n{body}"
        );
        assert!(
            !body.contains("cat_file_blob"),
            "the wrapper has grown its own blob-reading body. Nothing covers that \
             body: the cache tests exercise `read_blobs_into` against a local map, \
             and a behavioural test against the global cache cannot cross a 96 MB \
             bound. Keep the wrapper a delegation and put logic where it is \
             tested. Body:\n{body}"
        );
    }

    /// **The cache regression this phase introduced and then fixed — pinned
    /// THROUGH `read_blobs`, which is the only way it bites.**
    ///
    /// Phase 3 turned one `read_blobs` call per cycle into `1 + roots`, and the
    /// cache's prune was `retain(only this call's ids)`. Two callers with
    /// disjoint id sets therefore evicted each other every cycle — ~1,863
    /// `git cat-file` spawns per minute on a two-root box, each taken while
    /// holding the cache mutex. It was invisible with one caller because the
    /// prune was a no-op.
    ///
    /// The FIRST version of this test called `evict_cold_blobs` directly and
    /// was VACUOUS: neutering `read_blobs`' call site left it passing, because
    /// it never went through `read_blobs` at all. It pinned the helper and not
    /// the thing that regressed. Recorded here because a test that cannot fail
    /// is worse than no test — it reports safety it never measured.
    ///
    /// So: read set A, read a DISJOINT set B, then delete the repository and
    /// re-read A. Only a cache hit can answer once the repo is gone, so the
    /// assertion is exactly "B's read did not evict A".
    ///
    /// Neuter check: restore `retain(|id, _| this_call's_ids.contains(id))` in
    /// `read_blobs` and this fails — verified.
    #[test]
    fn a_second_read_does_not_evict_the_first_reads_blobs() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let entries = ProcessGit
            .list_ref_dir(&clone, "origin/main", "plans")
            .expect("the fixture lists");
        let id_of = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} is in the listing"))
                .id
                .clone()
        };
        let set_a = vec![id_of("2026-01-01-normal.md")];
        let set_b = vec![id_of("2026-01-02-empty.md")];
        assert_ne!(
            set_a, set_b,
            "the two sets must be disjoint for this to test anything"
        );

        // A, then B — two callers, as a cycle now makes.
        let first = ProcessGit.read_blobs(&clone, &set_a);
        assert!(
            first[0].as_deref().unwrap_or("").starts_with("# Normal"),
            "got: {:?}",
            first[0]
        );
        let _ = ProcessGit.read_blobs(&clone, &set_b);

        // The repository is gone; only the cache can answer for A now.
        std::fs::remove_dir_all(&clone).expect("the fixture clone is removable");
        let again = ProcessGit.read_blobs(&clone, &set_a);
        assert_eq!(
            again[0], first[0],
            "the second read's ids must NOT have evicted the first's — under the old \
             prune-to-this-call's-ids policy this is a miss, and a miss cannot be served \
             from a repo that no longer exists"
        );
    }

    /// One unreadable root must not stop the others refreshing — the contract
    /// difference from the work-unit arm, which publishes all-or-nothing
    /// because it has a disappearance concept. The body sync is UPSERT-ONLY,
    /// so a short set costs a deferred refresh and never a deletion.
    ///
    /// BOTH roots take the `Ref` arm, one healthy and one whose listing fails.
    /// An earlier draft paired an `Unavailable` root with a `WorkTree` one,
    /// which proved independence across those two arms and left the branch most
    /// likely to regress untested: an early `return` added to the `Ref`→`Err`
    /// arm would have passed it, because no root in it reached that arm
    /// successfully.
    ///
    /// Neuter check: change the `Ref`→`Err(e)` arm to `return (artifacts,
    /// skipped)` and this fails.
    #[test]
    fn one_dark_ref_root_does_not_starve_a_healthy_ref_root() {
        let good = tempfile::tempdir().unwrap();
        let dark = tempfile::tempdir().unwrap();

        /// Both dirs are repo roots; only `good`'s listing resolves.
        struct TwoRefRoots {
            good: PathBuf,
            body: String,
        }
        impl GitRefReader for TwoRefRoots {
            fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
                Ok(Some(dir.to_path_buf()))
            }
            fn default_ref(&self, _r: &Path) -> Result<String, String> {
                Ok("origin/main".to_string())
            }
            fn rev_parse(&self, _r: &Path, _rev: &str) -> Result<String, String> {
                Ok("a".repeat(40))
            }
            fn count_behind_ahead(
                &self,
                _r: &Path,
                _a: &str,
                _b: &str,
            ) -> Result<(u64, u64), String> {
                Ok((0, 0))
            }
            fn ref_refresh_stamps(
                &self,
                _r: &Path,
                _d: &str,
                _s: &str,
            ) -> Vec<Result<Option<i64>, String>> {
                Vec::new()
            }
            fn fetch_default(&self, _r: &Path, _d: &str) -> Result<(), String> {
                Ok(())
            }
            fn list_ref_dir(
                &self,
                repo_root: &Path,
                _n: &str,
                _d: &str,
            ) -> Result<Vec<RefDirEntry>, String> {
                if repo_root == self.good {
                    Ok(vec![RefDirEntry {
                        name: "2026-01-02-good.md".into(),
                        id: "idgood".into(),
                    }])
                } else {
                    Err("bad object".into())
                }
            }
            fn read_blobs(&self, _r: &Path, ids: &[String]) -> Vec<Result<String, String>> {
                ids.iter()
                    .map(|id| {
                        if id == "idgood" {
                            Ok(self.body.clone())
                        } else {
                            Err("no such blob".to_string())
                        }
                    })
                    .collect()
            }
        }

        let (got, _) = scan_roots_at_source(
            &[plans_root(dark.path()), plans_root(good.path())],
            &PlanConvention::operator_default(),
            &TwoRefRoots {
                good: good.path().to_path_buf(),
                body: "# Good\n\n> **Status: DRAFT 2026-09-01.**\n".to_string(),
            },
            &CycleRefPin::default(),
        );
        assert_eq!(
            got.len(),
            1,
            "the healthy REF root must still publish while the dark one contributes nothing"
        );
        assert!(got[0].upsert.slug.contains("good"));
    }

    /// A real clone with a real `plans/` tree at `origin/main`, for the
    /// [`ProcessGit`] half of the ref scan — the `-z`/TAB/mode parser,
    /// `cat_file_blob` and `fetch_default`. None of it is reachable from
    /// [`FakeGit`], which returns canned answers and so cannot observe a single
    /// one of these properties: `list_ref_dir` ignoring its arguments means a
    /// fake-only suite passes identically if `ls-tree` grows an `-r`.
    ///
    /// Returns the clone root. `plans/` holds, deliberately:
    /// a normal plan, a ZERO-BYTE plan, a SYMLINK named `.md`, a non-markdown
    /// file, a file named exactly `.md`, and a SUBDIRECTORY holding a plan.
    fn ref_scan_fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&clone).unwrap();
        real_git(&origin, &["init", "-q", "--bare", "-b", "main"], None);
        real_git(&clone, &["init", "-q", "-b", "main"], None);
        real_git(
            &clone,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        let plans = clone.join("plans");
        std::fs::create_dir_all(plans.join("archive")).unwrap();
        // NONCED per instance. A git object id is a CONTENT HASH and
        // `blob_cache` is a process-global static, so a fixture with hardcoded
        // bodies mints the SAME ids in every instance — and this fixture has
        // several callers running in parallel in one test binary. A cache test
        // keyed on a shared id can then be rescued by a sibling test's insert
        // of that same id, which makes its neuter check unstable in the
        // FALSE-PASS direction. `a_second_read_does_not_evict_the_first_reads_blobs`
        // asserts on THIS file's id, so this nonce is what makes that test
        // honest; `2026-01-02-empty.md` stays un-nonced because the empty blob
        // is `e69de29b…` in every git repo there is and no assertion rests on
        // it. Also de-races `a_blob_already_read_is_not_read_again`.
        let nonce = tmp.path().display().to_string();
        std::fs::write(
            plans.join("2026-01-01-normal.md"),
            format!("# Normal\n\n> **Status: DRAFT**\n\n<!-- {nonce} -->\n"),
        )
        .unwrap();
        std::fs::write(plans.join("2026-01-02-empty.md"), "").unwrap();
        std::fs::write(plans.join("notes.txt"), "not a plan\n").unwrap();
        std::fs::write(plans.join(".md"), "# not a plan either\n").unwrap();
        std::fs::write(plans.join("archive/2026-01-03-nested.md"), "# Nested\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("2026-01-01-normal.md", plans.join("2026-01-04-link.md"))
            .unwrap();
        real_git(&clone, &["add", "-A"], None);
        real_git(&clone, &["commit", "-q", "-m", "plans"], None);
        real_git(&clone, &["push", "-q", "origin", "main"], None);
        // `fetch_default` is what the scan relies on to create this ref; using
        // it here is the only coverage it gets.
        ProcessGit
            .fetch_default(&clone, "origin/main")
            .expect("the scan's own fetch creates `origin/main`");
        // A checkout built by `init` + `remote add` has NO `origin/HEAD`, and
        // `default_ref` refuses to guess one — so without this the scan is
        // permanently dark here. That is not fixture noise: it is the shape of
        // the `Unavailable` state this phase introduces, and
        // `a_permanent_scan_fault_warns_once_not_every_tick` is its test. A
        // real `git clone` sets the ref; these fixtures have to do it by hand.
        real_git(&clone, &["remote", "set-head", "origin", "-a"], None);
        tmp
    }

    /// The `ls-tree -z` parser against real git output, for the four
    /// properties `FakeGit` structurally cannot observe.
    ///
    /// Neuter checks, each independently: add `-r` to the `ls-tree` args and
    /// `archive/2026-01-03-nested.md` appears; drop the `mode == "120000"` arm
    /// and `2026-01-04-link.md` appears.
    #[test]
    fn process_git_lists_a_real_ref_dir_at_depth_one_skipping_symlinks() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let entries = ProcessGit
            .list_ref_dir(&clone, "origin/main", "plans")
            .expect("a pushed-and-fetched plans dir lists");
        let mut names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();

        // The TREE `archive` is skipped (not an error, and not descended into);
        // the symlink is skipped because its blob is a path, not a plan. Both
        // non-`.md` entries are still listed here — the `.md` filter belongs to
        // `read_ref_dir`, and keeping the two separable is what lets each be
        // tested for what it actually does.
        assert_eq!(
            names,
            vec![
                ".md",
                "2026-01-01-normal.md",
                "2026-01-02-empty.md",
                "notes.txt"
            ],
            "depth 1, no trees, no symlinks"
        );
        assert!(
            entries.iter().all(|e| e.id.len() >= 40),
            "every entry carries a real object id: {entries:?}"
        );

        // The symlink half of this test is VACUOUS off Unix: the fixture only
        // creates the link under `cfg(unix)`, so on Windows the file is simply
        // absent, the assertion above passes unchanged, and dropping the
        // `mode == "120000"` arm would go undetected with no signal that
        // coverage was lost. Saying so here is the honest form — a neuter check
        // that silently stops neutering is worse than no neuter check, because
        // the suite still reports green.
        #[cfg(unix)]
        {
            assert!(
                !names.contains(&"2026-01-04-link.md"),
                "the symlink IS in the fixture's tree at this ref and must not be listed: \
                 {names:?}"
            );
            let listed = real_git(&clone, &["ls-tree", "-z", "origin/main:plans"], None);
            assert!(
                listed.contains("2026-01-04-link.md"),
                "guard: git itself must report the symlink, or this test proves nothing about \
                 the mode filter; got: {listed}"
            );
        }
        #[cfg(not(unix))]
        eprintln!(
            "NOTE: the symlink assertion did not run on this platform; the `120000` mode \
             filter is UNTESTED here"
        );
    }

    /// Blob reads against real git: one slot per id, in the order asked, with
    /// a ZERO-BYTE blob distinguished from a MISSING one.
    ///
    /// The empty case is the one that had a defect: the batch reader
    /// represented both as an empty `Vec`, so an empty `.md` — which
    /// `read_plan_dir` parses without complaint — read back as "empty or
    /// missing" and left the corpus. The two arms must agree, and a plan whose
    /// body is empty is a plan.
    ///
    /// This is also the only coverage `cat_file_blob` has, including its
    /// non-zero-exit arm (the absent object below) — so it now pins the
    /// bounded read that replaced the hand-rolled `--batch` child.
    ///
    /// Neuter check: make `cat_file_blob`'s success arm return
    /// `Err` for an empty stdout and the empty-plan assertion fails.
    #[test]
    fn process_git_reads_blobs_in_order_and_separates_empty_from_missing() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let entries = ProcessGit
            .list_ref_dir(&clone, "origin/main", "plans")
            .expect("lists");
        let id_of = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} is in the listing"))
                .id
                .clone()
        };
        let ids = vec![
            id_of("2026-01-01-normal.md"),
            id_of("2026-01-02-empty.md"),
            // A well-formed object id that is not in this repo.
            "0".repeat(40),
        ];
        let got = ProcessGit.read_blobs(&clone, &ids);

        assert_eq!(got.len(), 3, "one slot per id, in the order asked");
        assert!(
            got[0].as_deref().unwrap_or("").starts_with("# Normal"),
            "got: {:?}",
            got[0]
        );
        assert_eq!(
            got[1].as_deref(),
            Ok(""),
            "a zero-byte plan is a READABLE plan with an empty body, not a failure"
        );
        let missing = got[2].as_ref().expect_err("an absent object cannot read");
        assert!(missing.contains("missing"), "got: {missing}");
    }

    /// The blob cache is keyed on the OBJECT ID, so a second read of the same
    /// id must not touch git at all — which is what buys back the spawn count
    /// the per-blob read costs, and the reason the steady-state scan is now
    /// CHEAPER than the `--batch` process it replaced.
    ///
    /// Proved by deleting the repository out from under the second read: a
    /// cache miss would have to shell out, and `git cat-file` cannot answer
    /// from a directory that no longer exists. Same bytes back = the answer
    /// came from memory.
    ///
    /// Neuter check: delete the `Some(hit) => Ok(hit.clone())` arm and this
    /// fails, because the second read reaches a `git` with no repo.
    #[test]
    fn a_blob_already_read_is_not_read_again() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let entries = ProcessGit
            .list_ref_dir(&clone, "origin/main", "plans")
            .expect("lists");
        let id = entries
            .iter()
            .find(|e| e.name == "2026-01-01-normal.md")
            .expect("the normal plan is listed")
            .id
            .clone();
        let ids = vec![id];

        let first = ProcessGit.read_blobs(&clone, &ids);
        assert!(
            first[0].as_deref().unwrap_or("").starts_with("# Normal"),
            "got: {:?}",
            first[0]
        );

        // The repo is gone; only the cache can answer now.
        std::fs::remove_dir_all(&clone).expect("the fixture clone is removable");
        let second = ProcessGit.read_blobs(&clone, &ids);
        assert_eq!(
            second[0], first[0],
            "a content-addressed hit must not re-shell to a repo that is gone"
        );
    }

    /// `read_ref_dir` over the real reader, end to end: the `.md` filter, the
    /// empty plan surviving, and the sort.
    #[test]
    fn read_ref_dir_over_real_git_keeps_markdown_and_an_empty_plan() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let listing =
            super::super::ref_scan::read_ref_dir(&ProcessGit, &clone, "origin/main", "plans")
                .expect("a real plans dir reads");
        assert!(listing.complete, "every listed plan blob read");
        let names: Vec<_> = listing.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["2026-01-01-normal.md", "2026-01-02-empty.md"],
            "markdown only, sorted; `.md`, `notes.txt`, the symlink and the subdir are all out"
        );
        assert_eq!(
            listing.files[1].body, "",
            "the empty plan survives with an empty body"
        );
        // The CENSUS runs the same filter over the same listing, and names
        // the sha it was taken at.
        assert_eq!(
            listing.names,
            vec!["2026-01-01-normal.md", "2026-01-02-empty.md"]
        );
        assert_eq!(
            listing.ref_sha,
            Some(
                real_git(&clone, &["rev-parse", "origin/main"], None)
                    .trim()
                    .to_string()
            )
        );
    }

    /// A plans dir that is not in the ref is git's own ANSWER, and it is named
    /// as one: a gitignored dir, or one that exists only on a feature branch,
    /// is an unpushed corpus rather than a broken fetch. It still publishes
    /// nothing — it is just not reported as a fault to repair.
    #[test]
    fn a_dir_absent_from_the_ref_says_so_rather_than_reading_as_a_broken_scan() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let err = ProcessGit
            .list_ref_dir(&clone, "origin/main", "no-such-dir")
            .expect_err("a dir that is not in the ref cannot be listed");
        assert!(
            err.contains("does not exist in that ref"),
            "the reason must name the configuration, not a generic probe failure; got: {err}"
        );
    }

    /// The whole ref arm over real git, at [`read_plans_for_cycle`]: the
    /// parked-tree defect's actual fix.
    ///
    /// The working tree is moved OFF `main` and a plan is added there that is
    /// not in the ref. The scan must publish `origin/main`'s plans and not the
    /// checked-out branch's — which is the entire point of Phase 2, and the
    /// one thing no fake can demonstrate.
    ///
    /// Neuter check: change the `Ref` arm to `read_plan_dir(dir, conv)` and the
    /// parked-branch plan appears.
    #[test]
    fn the_scan_reads_the_ref_not_the_branch_the_checkout_is_parked_on() {
        let tmp = ref_scan_fixture();
        let clone = tmp.path().join("clone");
        let plans = clone.join("plans");
        real_git(&clone, &["checkout", "-q", "-b", "parked"], None);
        std::fs::write(
            plans.join("2026-01-09-only-on-the-parked-branch.md"),
            "# Parked\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        real_git(&clone, &["add", "-A"], None);
        real_git(&clone, &["commit", "-q", "-m", "parked"], None);

        let scan = read_plans_for_cycle(
            &plans,
            &PlanConvention::operator_default(),
            &ProcessGit,
            &CycleRefPin::default(),
        )
        .expect("a healthy clone scans");
        let slugs: Vec<_> = scan.units.iter().map(|u| u.slug.as_str()).collect();
        assert!(
            !slugs.contains(&"2026-01-09-only-on-the-parked-branch"),
            "the parked branch's private plan must NOT reach the corpus: {slugs:?}"
        );
        assert!(
            slugs.contains(&"2026-01-01-normal"),
            "the ref's plans must: {slugs:?}"
        );
        // And the census is taken from the same ref, over REAL git: the
        // parked branch's private plan is absent from it too, and the sha it
        // was listed at is the ref's own.
        let census = scan.ref_census.expect("the ref arm listed a ref");
        let listed = census.slugs.clone().expect("a fresh census carries stems");
        assert!(
            !listed.contains(&"2026-01-09-only-on-the-parked-branch".to_string()),
            "the census is of the REF, not of the parked tree: {listed:?}"
        );
        assert!(listed.contains(&"2026-01-01-normal".to_string()));
        assert_eq!(
            census.ref_sha,
            Some(
                real_git(&clone, &["rev-parse", "origin/main"], None)
                    .trim()
                    .to_string()
            ),
            "the census names the sha its stems were listed at"
        );
    }

    /// The plain case on real git output: a clone that has just fetched reads
    /// a refresh within seconds of now — through the reflog AND `FETCH_HEAD`.
    #[test]
    fn process_git_reads_a_just_fetched_ref_as_refreshed_now() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&clone).unwrap();
        real_git(&origin, &["init", "-q", "--bare", "-b", "main"], None);
        real_git(&clone, &["init", "-q", "-b", "main"], None);
        real_git(
            &clone,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &clone,
            &["commit", "-q", "--allow-empty", "-m", "one"],
            None,
        );
        real_git(&clone, &["push", "-q", "origin", "main"], None);
        real_git(&clone, &["fetch", "-q", "origin"], None);
        let sha = real_git(&clone, &["rev-parse", "refs/remotes/origin/main"], None);

        assert_refreshed_near_now(refresh_verdict(&clone, &sha), "a just-fetched ref");
        // Each source on its own agrees.
        assert!(ProcessGit::reflog_refreshed_at(&clone, "origin/main")
            .unwrap()
            .is_some());
        assert_refreshed_near_now(
            fetch_head_verdict(&clone, &sha),
            "FETCH_HEAD names main at the tracking sha",
        );
    }

    /// `FETCH_HEAD` fresher than the reflog WINS: a fetch that found `main`
    /// unchanged writes no reflog entry, so the reflog alone would call a
    /// just-verified ref six years old.
    #[test]
    fn process_git_prefers_a_fresher_fetch_head_over_an_old_reflog() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        assert_eq!(
            ProcessGit::reflog_refreshed_at(&reader, "origin/main"),
            Ok(Some(1_600_000_000)),
            "the reflog entry time is the pinned committer date, not the commit's"
        );
        assert_refreshed_near_now(
            refresh_verdict(&reader, &sha),
            "FETCH_HEAD's mtime should win over the 2020 reflog",
        );
    }

    /// A `FETCH_HEAD` naming only ANOTHER branch refreshes nothing this
    /// reading compares against, so the (old) reflog answers — even though
    /// that `FETCH_HEAD` was written seconds ago.
    #[test]
    fn process_git_ignores_a_fetch_head_that_names_only_another_branch() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        real_git(&reader, &["fetch", "-q", "origin", "feature"], None);
        let fetch_head_path = reader.join(real_git(
            &reader,
            &["rev-parse", "--git-path", "FETCH_HEAD"],
            None,
        ));
        let contents = std::fs::read_to_string(&fetch_head_path).unwrap();
        assert!(contents.contains("branch 'feature' of") && !contents.contains("branch 'main' of"));

        assert_eq!(fetch_head_verdict(&reader, &sha), RefreshVerdict::NoRecord);
        assert_eq!(
            refresh_verdict(&reader, &sha),
            RefreshVerdict::At(1_600_000_000),
            "the reflog answers, not the fresh FETCH_HEAD of another branch"
        );
    }

    /// Neither source: a clone whose tracking ref has no reflog and no
    /// `FETCH_HEAD` answers `Ok(None)` — an unknown age, not an error.
    #[test]
    fn process_git_answers_none_when_neither_source_exists() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        let git_dir = reader.join(real_git(&reader, &["rev-parse", "--git-dir"], None));
        std::fs::remove_file(git_dir.join("FETCH_HEAD")).unwrap();
        std::fs::remove_file(git_dir.join("logs/refs/remotes/origin/main")).unwrap();
        assert_eq!(refresh_verdict(&reader, &sha), RefreshVerdict::NoRecord);
    }

    /// A LINKED worktree whose primary checkout did the fetch — the census's
    /// shape: it fetches in the canonical checkout, while the plans dir may be
    /// a worktree of it. `FETCH_HEAD` is per-worktree, so the linked tree's
    /// own `--git-path FETCH_HEAD` does not exist; the refresh is recorded
    /// only in the common dir's file. Reading the per-worktree file alone
    /// would fall back to the (2020) reflog and call a just-fetched ref six
    /// years old.
    #[test]
    fn process_git_reads_the_primary_fetch_head_from_a_linked_worktree() {
        let (tmp, primary, sha) = origin_and_old_reflog_reader();
        let wt = tmp.path().join("linked");
        real_git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wtb",
                wt.to_str().unwrap(),
                "origin/main",
            ],
            None,
        );
        let own = wt.join(real_git(
            &wt,
            &["rev-parse", "--git-path", "FETCH_HEAD"],
            None,
        ));
        assert!(
            !own.exists(),
            "the linked tree has no FETCH_HEAD of its own: {}",
            own.display()
        );
        assert_eq!(
            fetch_head_file_refreshed_at(&own, "main", &sha),
            Ok(None),
            "the per-worktree file alone establishes nothing"
        );

        assert_refreshed_near_now(
            refresh_verdict(&wt, &sha),
            "the common dir's FETCH_HEAD should win over the 2020 reflog",
        );
    }

    /// A probe that times out every tick is ONE unchanging fault, not a new
    /// reading every tick. The killed child's pid used to reach `detail`
    /// through `DegradeReason`'s `Debug` form, and `detail` is compared
    /// verbatim — so each tick WARNed and re-posted.
    #[test]
    fn a_timed_out_probe_is_the_same_reading_whatever_the_pid() {
        use crate::process_helpers::DegradeReason;
        let args = ["reflog", "show"];
        let first = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 41_873,
                reaped: true,
            },
        );
        let second = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 52_004,
                reaped: true,
            },
        );
        assert_eq!(first, second);
        assert!(
            !first.contains("41873") && !first.contains("pid"),
            "{first}"
        );
        assert!(
            first.contains("TimedOut"),
            "the failure stays named: {first}"
        );

        let a = measured_with(Err(first), 5, 0);
        let b = measured_with(Err(second), 5, 0);
        assert!(a.is_same_reading(&b));
        // An unreaped kill IS a different fault, and still says so.
        let unreaped = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 1,
                reaped: false,
            },
        );
        assert!(unreaped.contains("not reaped"), "{unreaped}");
    }

    /// A measurement task that panics every tick is ONE unchanging fault: the
    /// reading's detail is the same for every panicking task, whatever its
    /// per-run task id — and a cancelled task says so, differently.
    #[tokio::test]
    async fn a_failed_probe_task_has_a_stable_detail() {
        let panicked = || async {
            tokio::task::spawn_blocking(|| panic!("probe blew up"))
                .await
                .unwrap_err()
        };
        let (a, b) = (panicked().await, panicked().await);
        assert_ne!(a.id(), b.id(), "two runs, two task ids");
        assert_ne!(
            a.to_string(),
            b.to_string(),
            "the raw error does vary per run"
        );
        assert_eq!(probe_task_failure_detail(&a), probe_task_failure_detail(&b));
        assert!(probe_task_failure_detail(&a).contains("panicked"));

        let cancelled = {
            let handle = tokio::spawn(std::future::pending::<()>());
            handle.abort();
            handle.await.unwrap_err()
        };
        assert!(cancelled.is_cancelled());
        assert!(probe_task_failure_detail(&cancelled).contains("cancelled"));

        let r1 = ScanDivergence::unknown(Some("/p".into()), probe_task_failure_detail(&a));
        let r2 = ScanDivergence::unknown(Some("/p".into()), probe_task_failure_detail(&b));
        assert!(r1.is_same_reading(&r2));
    }

    /// Every reading the measurement returns carries the time it was taken,
    /// and that time is not part of what the reading SAYS.
    #[test]
    fn a_reading_is_stamped_with_its_measurement_time() {
        let d = measured_with(Ok(Some(NOW - 60)), 5, 0);
        assert_eq!(d.observed_at_unix, Some(NOW));
        for other in [
            measure_scan_divergence(None, &FakeGit::healthy(0, 0), NOW),
            measure_scan_divergence(
                Some(Path::new("/repo/plans")),
                &FakeGit {
                    default_ref: Err("no origin/HEAD".to_string()),
                    ..FakeGit::healthy(0, 0)
                },
                NOW,
            ),
        ] {
            assert_eq!(other.observed_at_unix, Some(NOW), "{:?}", other.state);
        }
        let later = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(5, 0, Ok(Some(NOW - 60))),
            NOW + 60,
        );
        assert_eq!(later.observed_at_unix, Some(NOW + 60));
        assert!(
            d.is_same_reading(&later),
            "a later measurement of the same state is the same reading"
        );
    }

    /// The tick's measurement resolves the reading's `source_repo` — the
    /// filesystem walk runs there, on the blocking pool, so the scan-root
    /// report only copies it.
    #[test]
    fn the_tick_measurement_resolves_source_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("qontinui-dev-notes");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("plans")).unwrap();
        let plans = repo.join("plans");
        let d = measure_scan_source(&plans, &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.source_repo.as_deref(), Some("qontinui-dev-notes/plans"));
        assert_eq!(
            d.source_repo,
            super::super::body_push::derive_source_repo(&plans)
        );
        assert_eq!(d.observed_at_unix, Some(NOW));
    }

    /// Live check against this box's own plans checkout — machine-specific, so
    /// ignored. Run with `--ignored` and compare against
    /// `git reflog show -n1 --date=unix --format=%gd refs/remotes/origin/main`
    /// and `stat -c %Y .git/FETCH_HEAD` in the same repo. The checkout is
    /// named by `QONTINUI_LIVE_PLANS_REPO` (the repo root holding `plans/`);
    /// unset, the test says so and skips.
    #[test]
    #[ignore = "reads a machine-specific checkout named by QONTINUI_LIVE_PLANS_REPO"]
    fn live_ref_refreshed_at_matches_the_shell() {
        let Ok(repo) = std::env::var("QONTINUI_LIVE_PLANS_REPO") else {
            println!(
                "SKIPPED: set QONTINUI_LIVE_PLANS_REPO to a checkout's root (the repo holding \
                 plans/) to compare ProcessGit's refresh read against the shell"
            );
            return;
        };
        let repo = Path::new(&repo);
        let default_ref = ProcessGit.default_ref(repo).unwrap();
        let sha = ProcessGit.rev_parse(repo, &default_ref).unwrap();
        let now = chrono::Utc::now().timestamp();
        let fetch_head = ProcessGit::fetch_head_stamps(repo, &default_ref, &sha);
        let reflog = ProcessGit::reflog_refreshed_at(repo, &default_ref);
        let combined =
            combine_refresh_stamps(ProcessGit.ref_refresh_stamps(repo, &default_ref, &sha), now);
        println!(
            "LIVE repo={} default_ref={default_ref} sha={sha} now={now} fetch_head={fetch_head:?} \
             reflog={reflog:?} combined={combined:?}",
            repo.display()
        );
        let d = measure_scan_divergence(Some(repo.join("plans").as_path()), &ProcessGit, now);
        println!(
            "LIVE reading state={} behind={:?} ahead={:?} ref_age_secs={:?} counts_are_floors={} detail={:?}",
            d.state.as_str(),
            d.behind,
            d.ahead,
            d.ref_age_secs,
            d.counts_are_floors(),
            d.detail
        );
        assert!(matches!(combined, RefreshVerdict::At(_)), "{combined:?}");
    }

    // ---- the scan-root report's posting policy (Revised P2, runner half) ----

    fn instant_plus(base: std::time::Instant, secs: u64) -> std::time::Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn scan_report_is_due_on_first_reading_and_never_before_one() {
        let t0 = std::time::Instant::now();
        let reading = measured_with(Ok(Some(NOW - 60)), 5, 0);
        assert!(
            scan_report_due(None, None, Some(&reading), t0, false),
            "never posted -> due"
        );
        assert!(
            !scan_report_due(None, None, None, t0, false),
            "no reading yet (the loop has not ticked) -> nothing to post"
        );
    }

    #[test]
    fn an_unchanged_reading_is_not_reposted_inside_the_heartbeat() {
        let t0 = std::time::Instant::now();
        let posted = measured_with(Ok(Some(NOW - 60)), 5, 0);
        // Same reading one tick later: its ref age advanced, nothing else.
        let now = measured_with(Ok(Some(NOW - 120)), 5, 0);
        let heartbeat = SCAN_REPORT_HEARTBEAT.as_secs();
        assert!(!scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&now),
            instant_plus(t0, 60),
            false
        ));
        assert!(!scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&now),
            instant_plus(t0, heartbeat - 1),
            false
        ));
        assert!(
            scan_report_due(
                Some((&posted, t0)),
                None,
                Some(&now),
                instant_plus(t0, heartbeat),
                false
            ),
            "the heartbeat re-posts an unchanged reading so observed_at ages honestly"
        );
    }

    #[test]
    fn a_changed_reading_is_posted_at_once() {
        let t0 = std::time::Instant::now();
        let posted = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let moved = measured_with(Ok(Some(NOW - 60)), 9, 0);
        assert!(scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&moved),
            instant_plus(t0, 60),
            false
        ));
        let went_floor = measured_with(Ok(Some(NOW - 7 * HOUR)), 5, 0);
        assert!(
            scan_report_due(
                Some((&posted, t0)),
                None,
                Some(&went_floor),
                instant_plus(t0, 60),
                false
            ),
            "crossing into floors is a change the read side must see"
        );
        assert!(scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&ScanDivergence::not_scanning()),
            instant_plus(t0, 60),
            false
        ));
    }

    #[test]
    fn a_failed_post_backs_off_before_retrying() {
        let t0 = std::time::Instant::now();
        let reading = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let backoff = SCAN_REPORT_RETRY_AFTER_FAILURE.as_secs();
        assert!(!scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, 60),
            false
        ));
        assert!(!scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, backoff - 1),
            false
        ));
        assert!(scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, backoff),
            false
        ));
        assert!(
            backoff < SCAN_REPORT_HEARTBEAT.as_secs(),
            "a recovered backend is caught up faster than a heartbeat"
        );
    }

    // ---- BodySync's ordering rules, over a recording reporter ----

    /// Records every scan-root report it is handed; answers as configured.
    struct FakeReporter {
        sent: Mutex<Vec<super::super::body_push::ScanRootReport>>,
        fail: bool,
        /// The whole ack a delivered report gets. `applied: None` and
        /// `created: None` are the UNKNOWN arms (an older web build, an
        /// unparseable body) and are settable, because how the body sync
        /// treats an UNKNOWN ack is itself a property under test.
        ack: Mutex<super::super::body_push::ScanRootAck>,
    }

    impl Default for FakeReporter {
        /// The ordinary answer: stored, onto a row that already existed.
        fn default() -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                fail: false,
                ack: Mutex::new(super::super::body_push::ScanRootAck {
                    applied: Some(true),
                    created: Some(false),
                }),
            }
        }
    }

    impl FakeReporter {
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }
        fn states(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.state.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl super::super::body_push::ScanRootReporter for FakeReporter {
        async fn report_scan_root(
            &self,
            report: &super::super::body_push::ScanRootReport,
        ) -> Result<super::super::body_push::ScanRootAck, super::super::body_push::ScanRootFailure>
        {
            self.sent.lock().unwrap().push(report.clone());
            if self.fail {
                let n = self.sent.lock().unwrap().len();
                Err(super::super::body_push::ScanRootFailure {
                    kind: "HTTP 422".to_string(),
                    // The body echoes per-attempt input, as a real 422 does.
                    message: format!("report scan root -> 422 {{\"input\": \"attempt {n}\"}}"),
                })
            } else {
                Ok(self.ack.lock().unwrap().clone())
            }
        }
    }

    /// A body sync over NO scan roots (so a cycle's scan is empty and never
    /// reaches the network) whose reports go to `reporter`.
    fn body_sync_reporting_to(reporter: std::sync::Arc<FakeReporter>, gate_open: bool) -> BodySync {
        BodySync::new(
            Vec::new(),
            super::super::body_push::HttpArtifactSink::new("http://127.0.0.1:9"),
            std::sync::Arc::new(move || gate_open) as CaptureGate,
        )
        .with_reporter(reporter)
        .with_scan_report_gate(owns_the_machine())
    }

    /// The scan-report gate of the instance that owns shared root state.
    fn owns_the_machine() -> ScanReportGate {
        std::sync::Arc::new(|| true)
    }

    fn metrics_with(reading: ScanDivergence) -> AdapterMetrics {
        let metrics = AdapterMetrics::default();
        record_scan_divergence(reading, &metrics);
        metrics
    }

    /// A PAUSED body sync still reports its scan root: a paused sync is
    /// exactly when the corpus stops being refreshed, so it is the last cycle
    /// that should go quiet about the scan source.
    ///
    /// Neuter check: move the `report_scan_root_if_due` call in `run_cycle`
    /// below `if self.breaker.should_skip_cycle() { return; }` and this fails.
    #[tokio::test]
    async fn a_paused_body_sync_still_reports_its_scan_root() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        bs.breaker = FailureBreaker {
            consecutive_total_failures: 0,
            pause_cycles_remaining: 3,
        };
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 2153, 11));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;

        assert_eq!(reporter.states(), vec!["measured"]);
        assert_eq!(
            bs.breaker.pause_cycles_remaining, 2,
            "the cycle WAS a paused one — the pause was consumed"
        );
    }

    /// An EMPTY scan still reports: the `artifacts.is_empty()` early return
    /// comes after the report.
    #[tokio::test]
    async fn an_empty_scan_still_reports_its_scan_root() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 0, 0));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(reporter.states(), vec!["measured"]);

        // And the posting policy holds across cycles: the same reading is not
        // re-sent inside the heartbeat.
        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(reporter.states().len(), 1);
    }

    /// A report that fails, cycle after cycle, never feeds the body-push
    /// breaker: a backend that does not serve the scan-roots route yet must
    /// not pause plan-body capture. The breaker starts one failure short of
    /// tripping, so a single counted report failure would pause it.
    #[tokio::test]
    async fn a_failed_scan_root_report_never_touches_the_breaker() {
        let reporter = std::sync::Arc::new(FakeReporter::failing());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let armed = FailureBreaker {
            consecutive_total_failures: TOTAL_FAILURE_CYCLES_BEFORE_PAUSE - 1,
            pause_cycles_remaining: 0,
        };
        bs.breaker = armed;
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE + 2) {
            // Skip the retry backoff so every cycle really attempts a post.
            bs.last_scan_report_failure = None;
            bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
                .await;
        }

        assert_eq!(
            reporter.states().len(),
            usize::try_from(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE + 2).unwrap(),
            "every cycle attempted a report"
        );
        assert_eq!(
            bs.last_scan_report_failure
                .as_ref()
                .map(|(kind, _)| kind.as_str()),
            Some("HTTP 422"),
            "the failure is remembered by its STABLE kind — the 422 bodies differed on \
             every attempt, and keying the WARN on them would re-WARN each time"
        );
        assert!(bs.last_scan_report.is_none(), "nothing was accepted");
        assert_eq!(bs.breaker, armed, "the breaker never saw a report failure");
        assert!(!bs.breaker.is_paused());
    }

    /// Two readings identical except for WHEN they were taken (and the ref
    /// age that grows with it) are the same reading at every change-detection
    /// point — no log transition, not report-due inside the heartbeat — yet
    /// the store keeps the NEWER instant, and the heartbeat re-post carries it.
    /// The web ages a row by when it RECEIVED the report (its own clock) and
    /// keeps the newest reading per device: a report observed BEFORE the stored
    /// one is declined (`applied: false`) and marks the row
    /// `reading_superseded`, while an equal `observed_at` applies. So a
    /// heartbeat that re-sent the first measurement's time would be accepted
    /// but would freeze the stored `observed_at` at that first measurement —
    /// `observed_skew_secs` would grow every heartbeat and the row would
    /// misstate when this device last measured.
    #[tokio::test]
    async fn only_the_measurement_instant_differs_so_nothing_changes_but_the_post_is_current() {
        let first = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let second = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(5, 0, Ok(Some(NOW - 60))),
            NOW + 600,
        );
        assert_eq!(first.observed_at_unix, Some(NOW));
        assert_eq!(second.observed_at_unix, Some(NOW + 600));
        assert_ne!(first, second, "the instant (and the age) did move");
        assert!(first.is_same_reading(&second));
        assert!(
            !scan_divergence_changed(Some(&first), &second),
            "no log transition"
        );
        assert!(
            scan_divergence_changed(None, &second),
            "the first reading always logs"
        );

        let t0 = std::time::Instant::now();
        assert!(
            !scan_report_due(
                Some((&first, t0)),
                None,
                Some(&second),
                instant_plus(t0, 600),
                false
            ),
            "not report-due inside the heartbeat"
        );

        let metrics = AdapterMetrics::default();
        record_scan_divergence(first.clone(), &metrics);
        record_scan_divergence(second.clone(), &metrics);
        assert_eq!(
            metrics.snapshot().scan_divergence.unwrap().observed_at_unix,
            Some(NOW + 600),
            "the store holds the latest tick's instant"
        );

        // Through the body sync: post the first, then let the heartbeat
        // elapse — the re-post carries the SECOND reading's instant.
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        bs.run_cycle(
            &PlanConvention::operator_default(),
            &metrics_with(first),
            None,
        )
        .await;
        let later = metrics_with(second);
        bs.run_cycle(&PlanConvention::operator_default(), &later, None)
            .await;
        assert_eq!(
            reporter.states().len(),
            1,
            "same reading inside the heartbeat"
        );
        let (posted, _) = bs.last_scan_report.take().unwrap();
        bs.last_scan_report = Some((
            posted,
            std::time::Instant::now() - SCAN_REPORT_HEARTBEAT - Duration::from_secs(1),
        ));
        bs.run_cycle(&PlanConvention::operator_default(), &later, None)
            .await;
        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "the heartbeat re-posted");
        let want = chrono::DateTime::<chrono::Utc>::from_timestamp(NOW + 600, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(
            sent[1].observed_at, want,
            "the re-post carries the newer instant"
        );
        assert_ne!(sent[0].observed_at, sent[1].observed_at);
    }

    // ---- the slug census (plan 2026-09-15-…-a-set-difference, Phase 2) ----

    /// A body sync over one real plans root, so its cycles produce a
    /// WORK-TREE census of an actual directory.
    fn body_sync_over(dir: &Path, reporter: std::sync::Arc<FakeReporter>) -> BodySync {
        BodySync::new(
            vec![super::super::body_push::ScanRoot::new(
                dir,
                super::super::body_push::ScanRootKind::Plans,
                super::super::body_push::PLANS_ROOT_LABEL,
            )],
            super::super::body_push::HttpArtifactSink::new("http://127.0.0.1:9"),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_reporter(reporter)
        .with_scan_report_gate(owns_the_machine())
    }

    fn a_ref_census(stems: &[&str]) -> super::super::body_push::PlanSlugCensus {
        super::super::body_push::PlanSlugCensus::new(
            super::super::body_push::SLUG_CENSUS_SOURCE_REF,
            Some("a".repeat(40)),
            stems.iter().map(|s| (*s).to_string()),
        )
    }

    /// One census per SOURCE, and the census of the side that fills the corpus
    /// is of the ACTIVE plans root — the one the reading's `source_repo`
    /// names — so the set difference the web computes stays scoped.
    #[tokio::test]
    async fn a_cycle_reports_both_sides_of_the_set_difference() {
        let dir = one_plan_dir();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_over(dir.path(), reporter.clone());
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        bs.run_cycle(
            &PlanConvention::operator_default(),
            &metrics,
            Some(a_ref_census(&[
                "2026-01-01-one-plan",
                "2026-01-02-only-on-the-ref",
            ])),
        )
        .await;

        let sent = reporter.sent.lock().unwrap().clone();
        let censuses = sent[0]
            .censuses
            .clone()
            .expect("both sides were enumerated");
        assert_eq!(
            censuses
                .iter()
                .map(|c| c.source.as_str())
                .collect::<Vec<_>>(),
            vec!["ref", "work_tree"]
        );
        assert_eq!(
            censuses[0].slugs.as_deref(),
            Some(
                [
                    "2026-01-01-one-plan".to_string(),
                    "2026-01-02-only-on-the-ref".to_string()
                ]
                .as_slice()
            )
        );
        // The difference this exists to make computable: the tree is missing
        // a plan the ref has, and the device says so as two SETS rather than
        // as a ratio.
        assert_eq!(
            censuses[1].slugs.as_deref(),
            Some(["2026-01-01-one-plan".to_string()].as_slice())
        );
        assert_eq!(censuses[1].ref_sha, None, "a tree census names no ref");
    }

    /// **The withheld-set heartbeat.** ~1,800 stems are ~100 KB and this
    /// report goes out every cycle, so an UNCHANGED set travels as
    /// `slugs: null` plus the digest that re-asserts it — and the moment the
    /// set MOVES, the stems are sent again. Without this, "report the census
    /// every cycle" would stop being a heartbeat.
    ///
    /// Neuter check: drop the `last_census_digests` lookup in
    /// `report_scan_root_if_due` and the second report carries its stems.
    #[tokio::test]
    async fn an_unchanged_census_is_re_asserted_by_digest_not_re_sent() {
        let dir = one_plan_dir();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_over(dir.path(), reporter.clone());
        let conv = PlanConvention::operator_default();
        let refc = || Some(a_ref_census(&["2026-01-01-one-plan"]));

        // 1. First report: both sets in full — the web holds neither yet.
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            refc(),
        )
        .await;
        // 2. The READING moved (so the report is due again) but neither set
        //    did: both are withheld, digests unchanged.
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            refc(),
        )
        .await;
        // 3. A plan lands in the tree. The tree digest moves, so those stems
        //    travel again — while the ref set, still unchanged, stays withheld.
        std::fs::write(
            dir.path().join("2026-01-03-new.md"),
            "# New\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 7, 0)),
            refc(),
        )
        .await;

        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 3);
        let census = |i: usize, source: &str| {
            sent[i]
                .censuses
                .as_ref()
                .unwrap()
                .iter()
                .find(|c| c.source == source)
                .cloned()
                .unwrap_or_else(|| panic!("report {i} carries a {source} census"))
        };
        assert!(
            census(0, "ref").slugs.is_some(),
            "first report sends the set"
        );
        assert!(census(0, "work_tree").slugs.is_some());

        assert_eq!(
            census(1, "ref").slugs,
            None,
            "unchanged: withheld, not re-sent"
        );
        assert_eq!(census(1, "work_tree").slugs, None);
        assert_eq!(
            (census(1, "ref").digest, census(1, "ref").count),
            (census(0, "ref").digest.clone(), 1),
            "a withheld set still carries the digest it is re-asserted BY, and its count"
        );

        assert_eq!(
            census(2, "work_tree").slugs.as_deref(),
            Some(
                [
                    "2026-01-01-one-plan".to_string(),
                    "2026-01-03-new".to_string()
                ]
                .as_slice()
            ),
            "the set MOVED, so it travels in full"
        );
        assert_ne!(census(2, "work_tree").digest, census(0, "work_tree").digest);
        assert_eq!(
            census(2, "ref").slugs,
            None,
            "the other side did not move, and is still withheld"
        );
    }

    /// **On the withheld arm, `count` is the EXACT size of the set the digest
    /// names.** The server verifies the digest but cannot verify the
    /// `truncated` flag, so it derives truncation as
    /// `flag or count != <the size of the set it holds>`. A placeholder count
    /// on this arm — a rounded number, a stale one, and worst of all a `0`,
    /// which is `< listed` and so derives truncated too — makes the server read
    /// the census as a FLOOR and take the whole `source_repo`'s coverage block
    /// to UNKNOWN. It would do that SILENTLY, on the steady-state arm that runs
    /// on almost every cycle, and nothing on the wire would complain: the
    /// digest would still verify.
    ///
    /// So this asserts the equality the server derives its verdict from,
    /// against the set the PREVIOUS report actually sent — not against
    /// `count`'s own provenance, which is what a future refactor would break.
    ///
    /// Mutation proof (run 2026-09-17): setting `self.count = 0` inside
    /// `PlanSlugCensus::withheld` fails this test on `count` while every other
    /// assertion here — `slugs: None`, the matching digest, `truncated: false`
    /// — still passes, which is exactly the silence the derived verdict exists
    /// to catch.
    #[tokio::test]
    async fn a_withheld_census_carries_the_exact_size_of_the_set_its_digest_names() {
        let dir = one_plan_dir();
        std::fs::write(
            dir.path().join("2026-01-02-second.md"),
            "# Second\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_over(dir.path(), reporter.clone());
        let conv = PlanConvention::operator_default();
        let refc = || Some(a_ref_census(&["2026-01-01-one-plan", "2026-01-02-second"]));

        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            refc(),
        )
        .await;
        // The reading moves so the heartbeat is due again; neither SET does.
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            refc(),
        )
        .await;

        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        let census = |i: usize, source: &str| {
            sent[i]
                .censuses
                .as_ref()
                .unwrap()
                .iter()
                .find(|c| c.source == source)
                .cloned()
                .unwrap_or_else(|| panic!("report {i} carries a {source} census"))
        };
        for source in ["ref", "work_tree"] {
            let first = census(0, source);
            let held = census(1, source);
            let listed = first
                .slugs
                .as_ref()
                .expect("the first report sent the stems")
                .len() as u64;
            assert_eq!(listed, 2, "{source}: the fixture holds two stems");
            assert_eq!(held.slugs, None, "{source}: the second report withholds");
            assert_eq!(
                held.digest, first.digest,
                "{source}: withheld against the SAME set"
            );
            // The assertion the whole clause is about.
            assert_eq!(
                held.count, listed,
                "{source}: a withheld census must carry the exact cardinality of the set its \
                 digest names — the server derives `truncated` as `flag or count != listed`, so \
                 any other value silently takes this source_repo's coverage to UNKNOWN"
            );
            assert!(
                !held.truncated,
                "{source}: and the flag agrees with the derived verdict"
            );
            // Stated as the server states it, so this fails on ANY divergence
            // rather than only on the `count` field being wrong.
            assert!(
                !(held.truncated || held.count != listed),
                "{source}: the server would derive TRUNCATED from this census"
            );
        }
    }

    /// A report the web did NOT store may not be re-asserted by digest: a
    /// digest MISMATCH clears the stored set to UNKNOWN, so withholding
    /// against a set the web never took would destroy it. Both ways of not
    /// being stored — a failure, and a `applied: false` — re-send in full.
    #[tokio::test]
    async fn a_report_that_did_not_land_re_sends_the_sets_in_full() {
        let dir = one_plan_dir();
        let conv = PlanConvention::operator_default();

        // (a) a delivered report the web DECLINED (it kept a newer reading).
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().applied = Some(false);
        let mut bs = body_sync_over(dir.path(), reporter.clone());
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            None,
        )
        .await;
        assert!(bs.last_census_digests.is_empty(), "nothing was stored");
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            None,
        )
        .await;
        let sent = reporter.sent.lock().unwrap().clone();
        assert!(
            sent[1].censuses.as_ref().unwrap()[0].slugs.is_some(),
            "an unapplied report stored no set, so the next sends it in full"
        );

        // (b) a report that never arrived at all.
        let failing = std::sync::Arc::new(FakeReporter::failing());
        let mut bs = body_sync_over(dir.path(), failing.clone());
        bs.last_census_digests
            .insert("work_tree".to_string(), "stale".to_string());
        bs.run_cycle(
            &conv,
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            None,
        )
        .await;
        assert!(
            bs.last_census_digests.is_empty(),
            "a failure forgets what it thought the web held"
        );
    }

    /// **Idle arm 1 of 3** — `tick`'s no-plans-dir early return (the first
    /// `report_while_idle` call site; `trigger.rs:2614` when the plan was
    /// written). Nothing is configured, so nothing was enumerated on either
    /// side: both censuses ABSENT, never a zero. The web additionally refuses
    /// any census on a `not_scanning` report, which is the same rule from the
    /// other end.
    #[tokio::test]
    async fn idle_arm_no_plans_dir_reports_both_censuses_absent() {
        let (_cell, reader) = switchable_paths();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_git(std::sync::Arc::new(FakeGit::healthy(0, 0)))
        .with_scan_reporter(reporter.clone())
        .with_scan_report_gate(owns_the_machine());

        state
            .tick(&FakeSink::default(), &AdapterMetrics::default())
            .await;

        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].state, "not_scanning");
        assert_eq!(
            sent[0].censuses, None,
            "an enumeration that did not run is ABSENT, never an empty set"
        );
        assert_eq!(
            serde_json::to_value(&sent[0]).unwrap()["censuses"],
            serde_json::Value::Null
        );
    }

    /// **Idle arm 2 of 3** — the `Ok(Err(reason))` publish-nothing arm (the
    /// second `report_while_idle` call site; `trigger.rs:2668` when the plan
    /// was written). The fetch failed, so the REF was never listed.
    ///
    /// The two sides are reported SEPARATELY here, which is the whole point of
    /// carrying them separately. The ref side is ABSENT — nothing listed it.
    /// The work-tree side is READ, because it shares nothing with the failure:
    /// no git, no fetch, one `read_dir` of a directory that is sitting there
    /// readable.
    ///
    /// This is not a fabricated census, which would be inventing a `count`
    /// nobody took; it is a census this cycle actually TAKES. The distinction
    /// is load-bearing because the causes that reach this arm are mostly
    /// STANDING — a clone with no `origin/HEAD`, a plans dir outside its
    /// work-tree root, an unpushed or ignored plans dir. Reporting both sides
    /// absent here took the work-tree census dark on every cycle forever for
    /// such a device, while the report kept going out looking healthy.
    ///
    /// Mutation proof (run 2026-09-20): revert this arm to
    /// `ScanCensusInputs::absent()` and the `work_tree` assertions below fail;
    /// restoring `work_tree_dir: Some(dir.clone())` passes them again.
    #[tokio::test]
    async fn idle_arm_unavailable_scan_source_reports_the_ref_absent_and_reads_the_work_tree() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(owns_the_machine())
        .with_scan_reporter(reporter.clone())
        .with_git(std::sync::Arc::new(FakeGit {
            root: Ok(Some(dir.path().to_path_buf())),
            fetch: Err("could not reach origin".to_string()),
            ..FakeGit::healthy(0, 0)
        }));

        state
            .tick(&FakeSink::default(), &AdapterMetrics::default())
            .await;

        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "the cycle that publishes nothing still reports"
        );
        let censuses = sent[0]
            .censuses
            .as_ref()
            .expect("the work-tree side WAS listed, so the report carries a census");
        assert!(
            censuses
                .iter()
                .all(|c| c.source != super::super::body_push::SLUG_CENSUS_SOURCE_REF),
            "the scan source was unavailable, so the REF side was never listed and must be \
             ABSENT rather than an empty set"
        );
        let work_tree = censuses
            .iter()
            .find(|c| c.source == super::super::body_push::SLUG_CENSUS_SOURCE_WORK_TREE)
            .expect("the work tree needs no git and was read");
        assert_eq!(
            work_tree.count, 1,
            "the fixture holds exactly one plan, and this side was MEASURED rather than guessed"
        );
        assert!(
            !work_tree.truncated,
            "one plan is the whole side, not a floor"
        );
    }

    /// **Idle arm 3 of 3, and the one that matters most** — the `Err(e)`
    /// scan-task-FAILED arm (the third `report_while_idle` call site;
    /// `trigger.rs:2697` when the plan was written).
    ///
    /// Here the enumeration did not happen AT ALL — the blocking task did not
    /// complete — so a `count: 0` would not merely be stale, it would be
    /// FABRICATED: a set nothing on this device ever listed, which the web
    /// would then store as this device's answer for that side and difference
    /// against the other.
    ///
    /// The WORK-TREE side is still read, for the same reason as arm 2: it
    /// shares nothing with the task that failed. A panicking scan task recurs
    /// for as long as its cause stands, so withholding both sides here is the
    /// same permanent darkness from a different arm.
    ///
    /// Mutation proof (run 2026-09-20): making this arm pass a
    /// `PlanSlugCensus::new(SLUG_CENSUS_SOURCE_REF, None, [])` fails the
    /// ref-absent assertion below; reverting the arm to
    /// `ScanCensusInputs::absent()` fails the work-tree assertions; the
    /// shipped form passes both.
    #[tokio::test]
    async fn idle_arm_failed_scan_task_reports_the_ref_absent_and_reads_the_work_tree() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(owns_the_machine())
        .with_scan_reporter(reporter.clone())
        // A git reader that PANICS is how the `spawn_blocking` scan task
        // fails to join — the arm's real-world cause (a panicking scan, a
        // cancelled runtime) rather than a simulated return value.
        .with_git(std::sync::Arc::new(FakeGit {
            panic_on_work_tree_root: true,
            ..FakeGit::healthy(0, 0)
        }));

        state.tick(&FakeSink::default(), &metrics).await;

        assert_eq!(
            metrics.snapshot().cycles_total,
            1,
            "a cycle that publishes nothing is still a cycle"
        );
        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "the failed cycle still reports its scan root"
        );
        assert_eq!(
            sent[0].state, "unknown",
            "the divergence probe panicked too, and says so rather than guessing"
        );
        let censuses = sent[0]
            .censuses
            .as_ref()
            .expect("the work-tree side WAS listed, so the report carries a census");
        assert!(
            censuses
                .iter()
                .all(|c| c.source != super::super::body_push::SLUG_CENSUS_SOURCE_REF),
            "the ref enumeration never RAN — a zero for it would be fabricated, not stale"
        );
        let work_tree = censuses
            .iter()
            .find(|c| c.source == super::super::body_push::SLUG_CENSUS_SOURCE_WORK_TREE)
            .expect("the work tree needs no git and did not depend on the failed task");
        assert_eq!(
            work_tree.count, 1,
            "the fixture holds exactly one plan, measured by a read_dir the panic never touched"
        );
    }

    /// A 2xx that says `applied: false` (the web kept a newer reading) is
    /// still DELIVERED for scheduling — not retried, not a failure — and the
    /// episode is tracked so its WARN fires once, clearing when a report is
    /// applied again.
    #[tokio::test]
    async fn an_unapplied_report_is_delivered_and_tracked_once() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().applied = Some(false);
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert!(
            bs.last_scan_report.is_some(),
            "delivered — the heartbeat clock starts"
        );
        assert!(
            bs.last_scan_report_failure.is_none(),
            "not a failure, so no backoff"
        );
        assert!(bs.last_scan_report_unapplied);

        // Inside the heartbeat nothing is re-sent, unapplied or not.
        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(reporter.states().len(), 1);

        // The next post is applied: the episode ends.
        reporter.ack.lock().unwrap().applied = Some(true);
        let changed = metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0));
        bs.run_cycle(&PlanConvention::operator_default(), &changed, None)
            .await;
        assert_eq!(reporter.states().len(), 2);
        assert!(!bs.last_scan_report_unapplied);
    }

    /// One ref census of a fixed set, for the withhold tests below.
    fn one_ref_census() -> ScanCensusInputs {
        ScanCensusInputs {
            ref_census: Some(super::super::body_push::PlanSlugCensus::new(
                "ref",
                Some("a".repeat(40)),
                ["2026-01-01-one".to_string(), "2026-01-02-two".to_string()],
            )),
            work_tree_dir: None,
        }
    }

    /// The stems of the one census in report `n`, or `None` when it was
    /// WITHHELD (`slugs: null` beside the digest that re-asserts them).
    fn sent_slugs(reporter: &FakeReporter, n: usize) -> Option<Vec<String>> {
        let sent = reporter.sent.lock().unwrap();
        let censuses = sent[n]
            .censuses
            .clone()
            .expect("the report carries censuses");
        assert_eq!(censuses.len(), 1);
        censuses[0].slugs.clone()
    }

    /// **A report that INSERTED the device's row must not be withheld
    /// against.**
    ///
    /// The web carries a withheld census forward only on its UPDATE arm; an
    /// INSERT stores what it was sent, so a `slugs: null` landing on a fresh
    /// row stores the stem set as UNKNOWN. The runner would never re-send it,
    /// because its own digest memory still matches its own re-enumeration —
    /// the set would sit at UNKNOWN until a disk change or a restart moved the
    /// digest. `created` is already on the wire; this reads it.
    ///
    /// Neuter check: stop clearing on `created == Some(true)` and the second
    /// report below goes out withheld, against a row that holds no set.
    #[tokio::test]
    async fn a_created_row_is_never_withheld_against() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().created = Some(true);
        let mut bs = body_sync_reporting_to(reporter.clone(), true);

        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            one_ref_census(),
        )
        .await;
        assert!(
            sent_slugs(&reporter, 0).is_some(),
            "the first report always carries the stems"
        );

        // A changed reading, so the next report is due. The row was CREATED by
        // the first one, so the set it holds is UNKNOWN and the stems travel
        // again.
        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            one_ref_census(),
        )
        .await;
        assert_eq!(
            sent_slugs(&reporter, 1),
            Some(vec![
                "2026-01-01-one".to_string(),
                "2026-01-02-two".to_string()
            ]),
            "an insert stores a withheld census as NULL, so the stems must be re-sent"
        );

        // Once the web is UPDATING the row, the steady state is restored: the
        // set travels only when it moves.
        reporter.ack.lock().unwrap().created = Some(false);
        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 7, 0)),
            one_ref_census(),
        )
        .await;
        assert!(sent_slugs(&reporter, 2).is_some(), "stored, so remembered");
        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 8, 0)),
            one_ref_census(),
        )
        .await;
        assert_eq!(
            sent_slugs(&reporter, 3),
            None,
            "the withhold still works — this fix must not cost the heartbeat its bandwidth"
        );
    }

    /// **An ack that does not say `applied` is UNKNOWN, and UNKNOWN does not
    /// render as "stored".**
    ///
    /// `None` is an older web build or an unparseable body. Withholding
    /// against it bets the stems on a set the web may not hold, and the losing
    /// side of that bet is silent: the row keeps a stale set, or none. The
    /// conservative arm costs one extra full census (~100-200 KB) per
    /// ambiguous ack [policy: `unknown-must-not-render-as-a-default`].
    ///
    /// Neuter check: key the memory off `applied != Some(false)` and the
    /// second report goes out withheld.
    #[tokio::test]
    async fn an_ack_that_does_not_say_applied_does_not_license_a_withhold() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().applied = None;
        reporter.ack.lock().unwrap().created = None;
        let mut bs = body_sync_reporting_to(reporter.clone(), true);

        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            one_ref_census(),
        )
        .await;
        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            one_ref_census(),
        )
        .await;
        assert_eq!(
            sent_slugs(&reporter, 1),
            Some(vec![
                "2026-01-01-one".to_string(),
                "2026-01-02-two".to_string()
            ]),
            "a delivered-but-unconfirmed report is not evidence the web holds the set"
        );
    }

    /// **An ack that does not say `created` is UNKNOWN too — and UNKNOWN is
    /// not "stored" on this axis either.**
    ///
    /// The doc on `ScanRootAck::created` calls `None` "the same UNKNOWN as
    /// `applied`'s, and is treated the same way — as not-stored". The code
    /// spelled it `created != Some(true)`, which KEPT the digest memory on
    /// `None` and withheld the next report's stems against a row that may hold
    /// none: the last non-conservative cell of the truth table, and the same
    /// defect as the `applied` one beside it. Only a positive `created: false`
    /// — "this UPDATED an existing row" — licenses a withhold, because only the
    /// update arm carries a withheld census forward.
    ///
    /// Neuter check: spell it `created != Some(true)` and the second report
    /// below goes out withheld.
    #[tokio::test]
    async fn an_ack_that_does_not_say_created_does_not_license_a_withhold() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().applied = Some(true);
        reporter.ack.lock().unwrap().created = None;
        let mut bs = body_sync_reporting_to(reporter.clone(), true);

        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0)),
            one_ref_census(),
        )
        .await;
        bs.report_while_idle(
            &metrics_with(measured_with(Ok(Some(NOW - 60)), 6, 0)),
            one_ref_census(),
        )
        .await;
        assert_eq!(
            sent_slugs(&reporter, 1),
            Some(vec![
                "2026-01-01-one".to_string(),
                "2026-01-02-two".to_string()
            ]),
            "a report that does not say whether it INSERTED is not evidence the web holds the set"
        );
    }

    /// Only the instance that owns the machine's shared state publishes its
    /// scan-root reading — one web row per device, so a secondary or temp
    /// runner reporting too would flip the machine's row. The gate is checked
    /// on BOTH report paths, defaults CLOSED (a body sync never told it owns
    /// the machine reports nothing), and is read per report.
    #[tokio::test]
    async fn only_the_instance_that_owns_the_machine_reports() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let owns = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = {
            let owns = owns.clone();
            std::sync::Arc::new(move || owns.load(Ordering::SeqCst)) as ScanReportGate
        };
        let mut bs = body_sync_reporting_to(reporter.clone(), true).with_scan_report_gate(gate);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        bs.report_while_idle(&metrics, ScanCensusInputs::absent())
            .await;
        assert!(
            reporter.states().is_empty(),
            "a secondary publishes nothing, on either path"
        );
        assert!(bs.scan_report_gate_announced, "and says why, once");

        owns.store(true, Ordering::SeqCst);
        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(
            reporter.states(),
            vec!["measured"],
            "the gate is read per report"
        );

        // Closed by default: a body sync never handed the predicate.
        let silent = std::sync::Arc::new(FakeReporter::default());
        let mut unconfigured = BodySync::new(
            Vec::new(),
            super::super::body_push::HttpArtifactSink::new("http://127.0.0.1:9"),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_reporter(silent.clone());
        unconfigured
            .run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        unconfigured
            .report_while_idle(&metrics, ScanCensusInputs::absent())
            .await;
        assert!(silent.states().is_empty());
    }

    /// The loop's default is closed too: an idle tick on a loop that was
    /// never handed the ownership predicate publishes nothing.
    #[tokio::test]
    async fn a_loop_without_the_ownership_predicate_reports_nothing() {
        let (_cell, reader) = switchable_paths();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_git(std::sync::Arc::new(FakeGit::healthy(0, 0)))
        .with_scan_reporter(reporter.clone());
        state
            .tick(&FakeSink::default(), &AdapterMetrics::default())
            .await;
        assert!(reporter.states().is_empty());
    }

    /// While the web keeps declining (`applied: false`), an unchanged reading
    /// is re-posted every retry interval (5 min), not every heartbeat (15
    /// min), so the device's current reading lands soon after its clock
    /// catches up.
    #[test]
    fn an_unapplied_reading_is_reposted_on_the_retry_cadence() {
        let t0 = std::time::Instant::now();
        let posted = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let now = measured_with(Ok(Some(NOW - 120)), 5, 0);
        let retry = SCAN_REPORT_RETRY_AFTER_FAILURE.as_secs();
        assert!(retry < SCAN_REPORT_HEARTBEAT.as_secs());
        let due = |unapplied, secs| {
            scan_report_due(
                Some((&posted, t0)),
                None,
                Some(&now),
                instant_plus(t0, secs),
                unapplied,
            )
        };
        assert!(!due(true, retry - 1));
        assert!(
            due(true, retry),
            "unapplied: due again after the retry interval"
        );
        assert!(!due(false, retry), "applied: waits for the full heartbeat");
        assert!(due(false, SCAN_REPORT_HEARTBEAT.as_secs()));
    }

    /// The same, through the body sync: an unapplied report is re-sent once
    /// the retry interval has passed.
    #[tokio::test]
    async fn the_body_sync_reposts_an_unapplied_reading_after_the_retry_interval() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        reporter.ack.lock().unwrap().applied = Some(false);
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(reporter.states().len(), 1);
        let (posted, _) = bs.last_scan_report.take().unwrap();
        bs.last_scan_report = Some((
            posted,
            std::time::Instant::now() - SCAN_REPORT_RETRY_AFTER_FAILURE - Duration::from_secs(1),
        ));
        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        assert_eq!(reporter.states().len(), 2, "re-posted on the 5-min cadence");
    }

    /// A tenant at `plan_capture = off` publishes nothing about its plans dir.
    #[tokio::test]
    async fn a_closed_capture_gate_reports_nothing() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), false);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));
        bs.run_cycle(&PlanConvention::operator_default(), &metrics, None)
            .await;
        bs.report_while_idle(&metrics, ScanCensusInputs::absent())
            .await;
        assert!(reporter.states().is_empty());
    }

    /// A device with NO plans dir still reports `not_scanning` when it has a
    /// body sync — through the idle tick, which never reaches `run_cycle` —
    /// and under the same posting policy: once, not every tick.
    #[tokio::test]
    async fn an_idle_tick_reports_not_scanning_through_the_body_sync() {
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_git(std::sync::Arc::new(FakeGit::healthy(0, 0)))
        .with_scan_reporter(reporter.clone())
        .with_scan_report_gate(owns_the_machine());

        state.tick(&sink, &metrics).await;
        assert_eq!(reporter.states(), vec!["not_scanning"]);
        assert!(
            metrics
                .snapshot()
                .scan_divergence
                .unwrap()
                .observed_at_unix
                .is_some(),
            "the idle reading is stamped with the tick's clock too"
        );
        let sent = reporter.sent.lock().unwrap()[0].clone();
        assert_eq!(
            (sent.behind, sent.ahead, sent.counts_are_floors),
            (None, None, false)
        );

        state.tick(&sink, &metrics).await;
        assert_eq!(
            reporter.states().len(),
            1,
            "the unchanged reading is not re-posted inside the heartbeat"
        );
    }

    // ---- one resolved sha per cycle (follow-up to 2026-09-10-…-not-a-ref) ----

    /// A clone whose `origin/main` MOVES on every read — a peer `git fetch` in
    /// the same shared checkout between any two of this cycle's `git` calls,
    /// which is the routine case on this fleet, not an adversarial one.
    ///
    /// Every resolution of `origin/main` yields a new sha, and the listing at
    /// each sha names a DIFFERENT blob, whose body records which sha it was
    /// read at. So a consumer that resolved the ref for itself publishes a body
    /// saying so, and a test can tell the two halves' commits apart.
    struct MovingRef {
        root: PathBuf,
        reads: std::sync::atomic::AtomicUsize,
        fetches: std::sync::atomic::AtomicUsize,
        listed: Mutex<Vec<String>>,
    }

    impl MovingRef {
        fn at(root: &Path) -> Self {
            Self {
                root: root.to_path_buf(),
                reads: Default::default(),
                fetches: Default::default(),
                listed: Mutex::new(Vec::new()),
            }
        }
        fn fetches(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }
        fn listed(&self) -> Vec<String> {
            self.listed.lock().unwrap().clone()
        }
    }

    impl GitRefReader for MovingRef {
        fn work_tree_root(&self, _dir: &Path) -> Result<Option<PathBuf>, String> {
            Ok(Some(self.root.clone()))
        }
        fn default_ref(&self, _repo_root: &Path) -> Result<String, String> {
            Ok("origin/main".to_string())
        }
        fn rev_parse(&self, _repo_root: &Path, rev: &str) -> Result<String, String> {
            if rev == "origin/main" {
                let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(format!("{n:040x}"))
            } else {
                Ok("b".repeat(40))
            }
        }
        fn count_behind_ahead(&self, _: &Path, _: &str, _: &str) -> Result<(u64, u64), String> {
            Ok((0, 0))
        }
        fn ref_refresh_stamps(
            &self,
            _: &Path,
            _: &str,
            _: &str,
        ) -> Vec<Result<Option<i64>, String>> {
            vec![Ok(Some(NOW - 60))]
        }
        fn fetch_default(&self, _repo_root: &Path, _default_ref: &str) -> Result<(), String> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn list_ref_dir(
            &self,
            _repo_root: &Path,
            ref_name: &str,
            _rel_dir: &str,
        ) -> Result<Vec<RefDirEntry>, String> {
            self.listed.lock().unwrap().push(ref_name.to_string());
            Ok(vec![RefDirEntry {
                name: "2026-01-01-a-plan.md".into(),
                id: format!("blob-at-{ref_name}"),
            }])
        }
        fn read_blobs(&self, _repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>> {
            ids.iter()
                .map(|id| {
                    Ok(format!(
                        "# A plan\n\n> **Status: DRAFT 2026-09-01.**\n\nRead from {id}.\n"
                    ))
                })
                .collect()
        }
    }

    /// **Both halves of one cycle read ONE commit, however the ref moves.**
    ///
    /// The work-unit half lists the ref and reports its census; the document
    /// half then publishes bodies. Before the pin each half fetched and
    /// resolved `origin/main` for itself, so a peer fetch between them made the
    /// census name commit A while the bodies came from commit B — every field
    /// well-formed, the pair incoherent.
    ///
    /// Neuter check: in `CycleRefPin::resolve_ref`, drop the early return on a
    /// memoised answer. The document half then fetches again, resolves the
    /// moved ref, and publishes a body read at the second sha — this fails on
    /// all three assertions.
    #[test]
    fn both_halves_of_one_cycle_read_one_commit_while_the_ref_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let git = MovingRef::at(tmp.path());
        let conv = PlanConvention::operator_default();
        let pin = CycleRefPin::default();

        let scan = read_plans_for_cycle(tmp.path(), &conv, &git, &pin).expect("the ref reads");
        let census_sha = scan
            .ref_census
            .as_ref()
            .and_then(|c| c.ref_sha.clone())
            .expect("the work-unit half resolved the ref");

        let (bodies, _) = scan_roots_at_source(&[plans_root(tmp.path())], &conv, &git, &pin);

        assert_eq!(
            git.fetches(),
            1,
            "one fetch per repo per cycle, not one per half"
        );
        assert_eq!(
            git.listed(),
            vec![census_sha.clone(), census_sha.clone()],
            "both halves list at the object id the census names"
        );
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0]
                .upsert
                .body
                .contains(&format!("blob-at-{census_sha}")),
            "the published body must come from the census's commit: {}",
            bodies[0].upsert.body
        );
    }

    /// A failed fetch is ONE answer for the whole cycle: a second consumer of
    /// the same repo sees the same `Unavailable` rather than retrying its way
    /// into a different ref state from the first.
    ///
    /// Neuter check: as above — without the memoised early return the second
    /// resolution fetches again and this fails on the count.
    #[test]
    fn a_failed_fetch_is_one_answer_for_the_whole_cycle() {
        struct FailsOnce(std::sync::atomic::AtomicUsize, FakeGit);
        impl GitRefReader for FailsOnce {
            fn work_tree_root(&self, d: &Path) -> Result<Option<PathBuf>, String> {
                self.1.work_tree_root(d)
            }
            fn default_ref(&self, r: &Path) -> Result<String, String> {
                self.1.default_ref(r)
            }
            fn rev_parse(&self, r: &Path, rev: &str) -> Result<String, String> {
                self.1.rev_parse(r, rev)
            }
            fn count_behind_ahead(&self, r: &Path, a: &str, b: &str) -> Result<(u64, u64), String> {
                self.1.count_behind_ahead(r, a, b)
            }
            fn ref_refresh_stamps(
                &self,
                r: &Path,
                d: &str,
                s: &str,
            ) -> Vec<Result<Option<i64>, String>> {
                self.1.ref_refresh_stamps(r, d, s)
            }
            fn fetch_default(&self, _: &Path, _: &str) -> Result<(), String> {
                // Fails the first time only — a retry WOULD succeed, which is
                // exactly the second answer a cycle must not get.
                match self.0.fetch_add(1, Ordering::SeqCst) {
                    0 => Err("transient".to_string()),
                    _ => Ok(()),
                }
            }
            fn list_ref_dir(&self, r: &Path, n: &str, d: &str) -> Result<Vec<RefDirEntry>, String> {
                self.1.list_ref_dir(r, n, d)
            }
            fn read_blobs(&self, r: &Path, ids: &[String]) -> Vec<Result<String, String>> {
                self.1.read_blobs(r, ids)
            }
        }
        let git = FailsOnce(Default::default(), FakeGit::healthy(0, 0));
        let pin = CycleRefPin::default();
        for _ in 0..2 {
            assert!(matches!(
                pin.resolve_source(&git, Path::new("/repo/plans")),
                super::super::ref_scan::ScanSource::Unavailable { .. }
            ));
        }
        assert_eq!(
            git.0.load(Ordering::SeqCst),
            1,
            "the failure is not retried within the cycle"
        );
    }

    /// The same property at the TICK — the wiring, not just the functions.
    /// The two tests above hold the pin fixed by hand; this one proves the
    /// reconcile loop actually hands the work-unit half's pin to the body sync.
    ///
    /// Neuter check: in `LoopState::tick`, call `bs.run_cycle(..)` instead of
    /// `bs.run_cycle_pinned(.., pin)`. The body sync then resolves a fresh pin,
    /// fetches a second time and lists at a moved sha — this fails on both.
    #[tokio::test]
    async fn the_tick_hands_the_work_unit_halfs_pin_to_the_body_sync() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let git = std::sync::Arc::new(MovingRef::at(dir.path()));
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(owns_the_machine())
        .with_scan_reporter(std::sync::Arc::new(FakeReporter::default()))
        .with_binding_count(1, Some(1))
        .with_git(git.clone());

        state
            .tick(&FakeSink::default(), &AdapterMetrics::default())
            .await;

        let listed = git.listed();
        assert_eq!(
            listed.len(),
            2,
            "the work-unit half AND the body sync each listed the ref: {listed:?}"
        );
        assert_eq!(
            listed[0], listed[1],
            "both halves listed ONE commit: {listed:?}"
        );
        assert_eq!(git.fetches(), 1, "one fetch for the whole cycle");
    }

    /// The other half of the pin's contract: it must NOT outlive its cycle. A
    /// pin kept across ticks (stored on `LoopState`, say) would never fetch
    /// again and freeze the corpus at one commit — the very staleness this
    /// plan family exists to remove, reached from the opposite direction.
    ///
    /// Neuter check: hoist the pin out of `tick` into a `LoopState` field that
    /// every tick reuses. The second tick then fetches nothing and lists the
    /// first tick's sha — this fails on both.
    #[tokio::test]
    async fn a_pin_does_not_outlive_its_cycle() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let git = std::sync::Arc::new(MovingRef::at(dir.path()));
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(owns_the_machine())
        .with_scan_reporter(std::sync::Arc::new(FakeReporter::default()))
        .with_binding_count(1, Some(1))
        .with_git(git.clone());

        let metrics = AdapterMetrics::default();
        state.tick(&FakeSink::default(), &metrics).await;
        state.tick(&FakeSink::default(), &metrics).await;

        let listed = git.listed();
        assert_eq!(listed.len(), 4, "two halves, two ticks: {listed:?}");
        assert_eq!(git.fetches(), 2, "each cycle fetches afresh");
        assert_ne!(
            listed[0], listed[2],
            "the second cycle must read the moved ref, not the first cycle's pin: {listed:?}"
        );
    }

    // ---- body-sync kill switch (plan 2026-09-03-…-on-by-default Phase 3) ----

    /// Serialized against the other env-touching tests; restores the flag AND the two web
    /// backend env layers, so the "no backend" arm below really has none.
    fn with_body_sync_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            PLAN_LIBRARY_SYNC_ENV,
            super::super::body_push::WEB_BACKEND_URL_ENV,
            super::super::body_push::WEB_BACKEND_URL_ENV_ALT,
        ]);
        std::env::remove_var(super::super::body_push::WEB_BACKEND_URL_ENV);
        std::env::remove_var(super::super::body_push::WEB_BACKEND_URL_ENV_ALT);
        match value {
            Some(v) => std::env::set_var(PLAN_LIBRARY_SYNC_ENV, v),
            None => std::env::remove_var(PLAN_LIBRARY_SYNC_ENV),
        }
        f()
    }

    /// On by default is the whole point: absent reads as on, and so does every
    /// value except the one exact spelling that kills it.
    #[test]
    fn the_body_sync_is_on_unless_the_env_is_exactly_zero() {
        assert!(with_body_sync_env(None, body_sync_enabled), "absent is ON");
        assert!(with_body_sync_env(Some("1"), body_sync_enabled));
        assert!(with_body_sync_env(Some(""), body_sync_enabled));
        assert!(with_body_sync_env(Some("true"), body_sync_enabled));
        assert!(
            with_body_sync_env(Some("off"), body_sync_enabled),
            "only `0` kills"
        );
        assert!(!with_body_sync_env(Some("0"), body_sync_enabled));
        assert!(
            !with_body_sync_env(Some(" 0 "), body_sync_enabled),
            "trimmed"
        );
    }

    // ---- body-sync posture is observable at spawn (plan 2026-08-27-… Phase 1, D6) ----

    /// The killed arm's line names the env var, its observed state, that
    /// `BodySync` was not built, and the consequence — the four things a reader
    /// of a runner log needs to answer "is the body sync on for this device?".
    #[test]
    fn the_killed_arm_names_the_env_var_and_the_consequence() {
        let (enabled, observed) = with_body_sync_env(Some("0"), || {
            (
                body_sync_enabled(),
                std::env::var(PLAN_LIBRARY_SYNC_ENV).ok(),
            )
        });
        assert!(!enabled);
        assert_eq!(observed.as_deref(), Some("0"));
        let msg = body_sync_disabled_message(observed.as_deref());
        assert!(msg.contains(PLAN_LIBRARY_SYNC_ENV), "{msg}");
        assert!(msg.contains("set to \"0\""), "{msg}");
        assert!(msg.contains("BodySync was NOT constructed"), "{msg}");
        assert!(msg.contains("agent.work_artifacts"), "{msg}");
        assert!(
            msg.contains("unset the variable"),
            "the line must say how to re-arm it: {msg}"
        );
        // The unset spelling is honest too, should the predicate ever move.
        assert!(body_sync_disabled_message(None).contains("is unset"));
    }

    /// The dial is announced on the first cycle whichever way it points, on a
    /// flip afterwards, and never on a steady cycle.
    #[test]
    fn the_capture_dial_is_announced_on_the_first_cycle_and_on_flips_only() {
        let first_closed = capture_gate_message(None, false).expect("first cycle, closed");
        assert!(first_closed.contains("CLOSED"), "{first_closed}");
        assert!(first_closed.contains("first cycle"), "{first_closed}");
        let first_open = capture_gate_message(None, true).expect("first cycle, open");
        assert!(first_open.contains("OPEN"), "{first_open}");
        assert!(first_open.contains("first cycle"), "{first_open}");

        assert_eq!(capture_gate_message(Some(false), false), None);
        assert_eq!(capture_gate_message(Some(true), true), None);

        let flipped = capture_gate_message(Some(false), true).expect("flip");
        assert!(flipped.contains("changed"), "{flipped}");
        assert_eq!(capture_gate_message(Some(true), false), Some(flipped));
    }

    /// With the sync on by default, the thing that protects a release build is
    /// the backend-resolution GUARD, not the flag: env unset and no backend ⇒
    /// no sink (so no `BodySync` on any tick); a backend ⇒ one; the kill switch
    /// ⇒ none even with one.
    #[test]
    fn with_no_backend_the_guard_not_the_flag_constructs_no_body_sync() {
        let build = |backend: Option<&str>| body_sync_sink_if_enabled(backend.map(str::to_string));
        assert!(with_body_sync_env(None, || build(None)).is_none());
        assert!(with_body_sync_env(None, || build(Some("http://web.example"))).is_some());
        assert!(with_body_sync_env(Some("0"), || build(Some("http://web.example"))).is_none());
    }

    // ---- path resolution (settings only) ------------------------------------

    /// No per-tenant entries anywhere — the state every runner is in until an
    /// operator keys one.
    fn no_overrides() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// `commands::tenant::get_active_tenant`'s canonical key form: lowercase,
    /// hyphenated `Uuid::to_string()`.
    const TENANT_A: &str = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
    const TENANT_B: &str = "7ac125b6-391b-4d64-8493-27305b25c5b9";

    fn keyed(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The setting is the ONLY source. There is no env rung above it any
    /// more, and this resolver reads nothing but its arguments — so the value
    /// the settings UI shows is, by construction, the value in effect.
    #[test]
    fn the_setting_is_the_only_source_of_the_plans_dir() {
        assert_eq!(
            resolve_plans_dir(Some("/settings/plans".to_string()), &no_overrides(), None)
                .as_deref(),
            Some("/settings/plans")
        );
    }

    /// Nothing configured ⇒ the markdown-plan tier is off. This is the no-op
    /// the adapter's opt-in contract rests on.
    #[test]
    fn nothing_configured_resolves_to_none() {
        assert_eq!(resolve_plans_dir(None, &no_overrides(), None), None);
        assert_eq!(resolve_plans_archive_dir(None, &no_overrides(), None), None);
        assert_eq!(resolve_prompts_dir(None, &no_overrides(), None), None);
    }

    /// A blank setting is unset, not a directory named "" — for all three.
    #[test]
    fn blank_setting_resolves_to_none() {
        assert_eq!(
            resolve_plans_dir(Some("  ".to_string()), &no_overrides(), None),
            None
        );
        assert_eq!(
            resolve_plans_archive_dir(Some("".to_string()), &no_overrides(), None),
            None
        );
        assert_eq!(
            resolve_prompts_dir(Some("\t".to_string()), &no_overrides(), None),
            None
        );
    }

    /// The SCALAR arm trims what it returns, exactly as the per-tenant arm does.
    ///
    /// Pinned separately because nothing else can see it: the product test
    /// `an_empty_map_resolves_exactly_as_the_device_scalar_for_every_input` uses
    /// `non_blank(scalar)` as its own oracle, so it stays green whatever
    /// `non_blank` does, and `a_blank_entry_is_unset_per_entry_…` pins trimming
    /// on the map arm only. Without this, the asymmetry could return unnoticed:
    /// a hand-edited `settings.json` holding `" /x "` would export
    /// `QONTINUI_PLANS_DIR=" /x "` from the scalar and `/x` from a map entry —
    /// one directory, two spellings, through one resolver.
    #[test]
    fn the_scalar_arm_trims_exactly_as_the_per_tenant_arm_does() {
        assert_eq!(
            resolve_plans_dir(Some("  /x \t".to_string()), &no_overrides(), None).as_deref(),
            Some("/x")
        );
        assert_eq!(
            resolve_plans_archive_dir(Some(" /y ".to_string()), &no_overrides(), None).as_deref(),
            Some("/y")
        );
        assert_eq!(
            resolve_prompts_dir(Some("\t/z ".to_string()), &no_overrides(), None).as_deref(),
            Some("/z")
        );
    }

    // ---- per-tenant resolution ---------------------------------------------
    //
    // These pin the RESOLVER's contract (which of the two rungs answers), not
    // the threading of a tenant into it — the threading is pinned by the
    // compiler at every call site plus the session-env tests that assert the
    // admitted spawn tenant is what reaches this function.

    /// GUARANTEE: resolver contract. An empty map is exactly today's answer,
    /// for every combination of scalar and tenant — which is what makes keying
    /// the setting a no-op on every device that has not keyed one.
    #[test]
    fn an_empty_map_resolves_exactly_as_the_device_scalar_for_every_input() {
        for scalar in [None, Some("/settings/plans".to_string())] {
            for tenant in [None, Some(TENANT_A), Some("not-a-uuid")] {
                assert_eq!(
                    resolve_plans_dir(scalar.clone(), &no_overrides(), tenant),
                    non_blank(scalar.clone()),
                    "scalar={scalar:?} tenant={tenant:?}"
                );
                assert_eq!(
                    resolve_plans_archive_dir(scalar.clone(), &no_overrides(), tenant),
                    non_blank(scalar.clone())
                );
                assert_eq!(
                    resolve_prompts_dir(scalar.clone(), &no_overrides(), tenant),
                    non_blank(scalar.clone())
                );
            }
        }
    }

    /// GUARANTEE: resolver contract. The whole point of the plan — a tenant
    /// WITH an entry gets its own directory, a tenant WITHOUT one gets the
    /// device default, and a launch naming no tenant gets the device default.
    /// All three directories, because P5 adopted the shape for all three.
    ///
    /// The tenant-B arm is the load-bearing one: B must get the DEVICE DEFAULT
    /// — not A's directory, and not nothing.
    #[test]
    fn a_keyed_tenant_wins_while_every_other_tenant_gets_the_device_default() {
        let map = keyed(&[(TENANT_A, "/tenant-a/plans")]);
        let scalar = || Some("/device/plans".to_string());

        assert_eq!(
            resolve_plans_dir(scalar(), &map, Some(TENANT_A)).as_deref(),
            Some("/tenant-a/plans"),
            "the keyed tenant's own entry wins over the scalar"
        );
        assert_eq!(
            resolve_plans_dir(scalar(), &map, Some(TENANT_B)).as_deref(),
            Some("/device/plans"),
            "a tenant with no entry falls back to the DEVICE DEFAULT — never \
             another tenant's directory, and never nothing"
        );
        assert_eq!(
            resolve_plans_dir(scalar(), &map, None).as_deref(),
            Some("/device/plans"),
            "a launch with no acting tenant gets the device default"
        );

        // Same three rungs on the archive and prompts twins.
        let archive = keyed(&[(TENANT_A, "/tenant-a/archive")]);
        assert_eq!(
            resolve_plans_archive_dir(
                Some("/device/archive".to_string()),
                &archive,
                Some(TENANT_A)
            )
            .as_deref(),
            Some("/tenant-a/archive")
        );
        assert_eq!(
            resolve_plans_archive_dir(
                Some("/device/archive".to_string()),
                &archive,
                Some(TENANT_B)
            )
            .as_deref(),
            Some("/device/archive")
        );
        let prompts = keyed(&[(TENANT_B, "/tenant-b/prompts")]);
        assert_eq!(
            resolve_prompts_dir(
                Some("/device/prompts".to_string()),
                &prompts,
                Some(TENANT_B)
            )
            .as_deref(),
            Some("/tenant-b/prompts")
        );
        assert_eq!(
            resolve_prompts_dir(
                Some("/device/prompts".to_string()),
                &prompts,
                Some(TENANT_A)
            )
            .as_deref(),
            Some("/device/prompts")
        );
    }

    /// GUARANTEE: resolver contract. A keyed tenant with NO device scalar still
    /// gets its directory — the map is an override, not a decoration on a
    /// configured default. And the unkeyed tenant on that same device gets
    /// `None`, i.e. the tier is off for it, exactly as it is today.
    #[test]
    fn a_keyed_tenant_resolves_even_with_no_device_default() {
        let map = keyed(&[(TENANT_A, "/tenant-a/plans")]);
        assert_eq!(
            resolve_plans_dir(None, &map, Some(TENANT_A)).as_deref(),
            Some("/tenant-a/plans")
        );
        assert_eq!(resolve_plans_dir(None, &map, Some(TENANT_B)), None);
        assert_eq!(resolve_plans_dir(None, &map, None), None);
    }

    /// GUARANTEE: resolver contract. Blank-is-unset applies PER ENTRY, so an
    /// entry configured to whitespace is unset for that tenant — it falls
    /// through to the scalar rather than naming a directory called `""`. And a
    /// value with surrounding whitespace is trimmed, as every other path rung
    /// trims.
    #[test]
    fn a_blank_entry_is_unset_per_entry_not_a_directory_named_empty() {
        let map = keyed(&[
            (TENANT_A, "   "),
            (TENANT_B, "  /tenant-b/plans \t"),
            ("blank-and-no-scalar", ""),
        ]);
        assert_eq!(
            resolve_plans_dir(Some("/device/plans".to_string()), &map, Some(TENANT_A)).as_deref(),
            Some("/device/plans"),
            "a blank entry falls through to the scalar"
        );
        assert_eq!(
            resolve_plans_dir(None, &map, Some(TENANT_A)),
            None,
            "a blank entry with no scalar beneath it is unset, not Some(\"\")"
        );
        assert_eq!(
            resolve_plans_dir(None, &map, Some("blank-and-no-scalar")),
            None
        );
        assert_eq!(
            resolve_plans_dir(None, &map, Some(TENANT_B)).as_deref(),
            Some("/tenant-b/plans"),
            "surrounding whitespace is trimmed off a per-tenant value too"
        );
    }

    /// GUARANTEE: resolver contract, D2. An unparseable or currently-unbound
    /// key is PRESERVED in the map (the operator may be re-pairing) and is
    /// simply inert: nothing looks it up, no lookup panics on it, and it never
    /// leaks into another tenant's resolution or into the device default.
    #[test]
    fn a_garbage_key_never_panics_and_never_resolves() {
        let map = keyed(&[
            ("", "/empty-key"),
            ("   ", "/blank-key"),
            ("not-a-uuid", "/garbage"),
            ("C231D9DA-0CA8-4FE4-BD81-0E3D6C20339A", "/wrong-case"),
            ("c231d9da-0ca8-4fe4-bd81-0e3d6c20339a/../etc", "/traversal"),
            ("🙂", "/emoji"),
        ]);
        let scalar = || Some("/device/plans".to_string());

        // The canonical form of that upper-case key does not resolve through it:
        // lookup is exact, so a non-canonical key is unattributed, not aliased.
        assert_eq!(
            resolve_plans_dir(scalar(), &map, Some(TENANT_A)).as_deref(),
            Some("/device/plans")
        );
        assert_eq!(
            resolve_plans_dir(scalar(), &map, None).as_deref(),
            Some("/device/plans")
        );
        for probe in ["", "   ", "🙂", "not-a-uuid-either", TENANT_B] {
            // Every garbage key is inert for every other probe; only an exact
            // hit answers, which is the point of asserting the exact strings
            // separately below.
            let answered = resolve_plans_dir(None, &map, Some(probe));
            assert!(
                answered.is_none() || map.get(probe).map(String::as_str) == answered.as_deref(),
                "probe {probe:?} resolved to {answered:?}, which is not its own entry"
            );
        }
        // And the keys really are still there — preserved, not dropped.
        assert_eq!(map.len(), 6, "every key survives, however unparseable");
    }

    // ---- the body-sync failure breaker ----------------------------------

    /// THE regression this breaker was rewritten for.
    ///
    /// In steady state the digest memory skips ~everything, so a cycle in which
    /// the operator edited one plan attempts exactly ONE push. The shipped
    /// predicate was `attempted > 0 && errors == attempted`, latched
    /// permanently — so that single push hitting a transient 500, a 30-second
    /// blip or a mid-rotation 401 killed the body sync for the whole process,
    /// and the error message prescribed restarting the runner, which fleet
    /// policy forbids.
    #[test]
    fn a_single_transient_failure_does_not_disable_the_sync() {
        let mut b = FailureBreaker::new();
        assert!(!b.record_cycle(1, 1), "one bad cycle must not trip");
        assert!(!b.is_paused());
        assert!(!b.should_skip_cycle(), "the next cycle still runs");
        // And the very next success clears the count entirely.
        assert!(!b.record_cycle(1, 0));
        assert_eq!(b.consecutive_total_failures(), 0);
        assert!(!b.is_paused());
    }

    /// A failure run BROKEN by one good cycle never trips, however long it is.
    #[test]
    fn non_consecutive_failures_never_trip_the_breaker() {
        let mut b = FailureBreaker::new();
        for _ in 0..20 {
            for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE - 1) {
                assert!(!b.record_cycle(3, 3));
            }
            // One partial success resets the run.
            assert!(!b.record_cycle(3, 1));
        }
        assert!(!b.is_paused());
    }

    /// A genuinely persistent failure DOES pause — the breaker still does its
    /// original job of not flooding the log every 60s forever.
    #[test]
    fn consecutive_total_failures_pause_the_sync() {
        let mut b = FailureBreaker::new();
        for cycle in 1..TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            assert!(!b.record_cycle(5, 5), "cycle {cycle} is too early to trip");
        }
        assert!(
            b.record_cycle(5, 5),
            "the {TOTAL_FAILURE_CYCLES_BEFORE_PAUSE}th consecutive total failure trips it"
        );
        assert!(b.is_paused());
    }

    /// And the pause RE-ARMS. The old latch was one-way, and its own remedy
    /// (restart the runner) is forbidden by served policy
    /// `production-and-cost` `runner-lifecycle` — so a tripped sync was dead
    /// for the process's life. This one resumes on its own.
    #[test]
    fn the_pause_re_arms_rather_than_latching_forever() {
        let mut b = FailureBreaker::new();
        for _ in 0..TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            b.record_cycle(2, 2);
        }
        assert!(b.is_paused());

        // It sits out exactly PAUSE_CYCLES cycles…
        for cycle in 0..PAUSE_CYCLES {
            assert!(b.should_skip_cycle(), "cycle {cycle} is still paused");
        }
        // …then runs again, with a clean failure count.
        assert!(!b.should_skip_cycle(), "the sync must resume by itself");
        assert!(!b.is_paused());
        assert_eq!(b.consecutive_total_failures(), 0);

        // A recovered backend then just works.
        assert!(!b.record_cycle(10, 0));
        assert!(!b.should_skip_cycle());
    }

    /// A cycle that made no network call at all is not a failure — otherwise a
    /// steady state in which every file is locally skipped would look like a
    /// total failure.
    #[test]
    fn a_cycle_with_no_attempts_is_not_a_failure() {
        let mut b = FailureBreaker::new();
        for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE * 3) {
            assert!(!b.record_cycle(0, 0));
        }
        assert!(!b.is_paused());
        assert_eq!(b.consecutive_total_failures(), 0);
    }

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        unit_with_deps(slug, status, vec![])
    }

    /// A unit carrying PROVENANCE — the fields an upsert refreshes and a
    /// skipped push freezes: `title` and `source_path`.
    fn unit_with_provenance(
        slug: &str,
        status: &str,
        title: &str,
        source_path: &str,
    ) -> ParsedWorkUnit {
        ParsedWorkUnit {
            title: Some(title.to_string()),
            source_path: source_path.to_string(),
            ..unit(slug, status)
        }
    }

    fn unit_with_deps(slug: &str, status: &str, depends_on: Vec<String>) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: None,
            status: status.to_string(),
            depends_on,
            area: None,
            area_rejected: None,
            phases: vec![],
            source_path: format!("plans/{slug}.md"),
            content: String::new(),
        }
    }

    /// How the fake sink should answer `set_deps`.
    #[derive(Clone, Copy, Default)]
    enum DepsBehavior {
        #[default]
        Ok,
        TableNotMigrated,
        Error,
        Forbidden,
    }

    #[derive(Default)]
    struct FakeSink {
        statuses: Mutex<HashMap<String, String>>,
        /// Transitions that SUCCEEDED. Deliberately incremented after the
        /// `deny_status` check, because it doubles as "what coord now holds".
        ///
        /// **Never assert on this to prove a request was NOT SENT.** A denied
        /// transition leaves it unmoved, so an `assert_eq!(transitions, N)` is
        /// blind to exactly the request a write-suppression fix exists to
        /// suppress — use [`FakeSink::transition_attempts`].
        transitions: Mutex<u64>,
        /// Every `transition` call that reached the sink, counted BEFORE the
        /// deny check — the door-level twin of `status_upsert_calls`.
        ///
        /// This is the ONLY counter that can see a re-issued DENIED transition,
        /// and `Transition` is the one door a permanent retirement actually
        /// suppresses: for a coord-derived status `settable_status` already
        /// withdraws the word from `UpsertWithStatus`, so the retirement's
        /// forcing of `action = RefreshOnly` is load-bearing here and nowhere
        /// else.
        transition_attempts: Mutex<u64>,
        /// Configured `by_actor` of every unit's latest history row (default
        /// None ⇒ no history ⇒ no owner to defer to).
        last_actor: Option<String>,
        /// Total `last_actor` reads. Paired with `current_status_calls`, this
        /// is what makes "a stable deferral costs TWO reads per cycle per
        /// slug" measurable rather than asserted in prose.
        last_actor_calls: Mutex<u64>,
        deps_behavior: DepsBehavior,
        /// Slug whose `current_status` read should hard-error, so the backfill's
        /// per-unit failure path is reachable without live HTTP.
        fail_status_read_for: Option<String>,
        /// Slug whose `upsert` should hard-error, so the OTHER failure branch
        /// (an `Err` out of `push_work_unit` itself) is reachable too.
        fail_upsert_for: Option<String>,
        /// Statuses the sink silently REWRITES on store, modelling coord's own
        /// normalisation. Idempotence must survive a backend that does not echo
        /// what it was handed.
        normalize: Option<(String, String)>,
        /// When true, EVERY `current_status` read hard-errors regardless of
        /// slug — the UNKNOWN branch the cold-start seed must abstain on
        /// rather than fall through to an overwrite. Distinct from
        /// `fail_status_read_for`, which targets one slug for the backfill's
        /// per-unit failure path.
        fail_current_status: bool,
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
        /// When set, every `upsert` answers with a coord `403` — the shape that
        /// used to be retried, and re-logged, once per slug per cycle forever.
        upsert_forbidden: bool,
        /// When set, every `upsert` answers with an ordinary (retryable) error.
        upsert_errors: bool,
        /// When set, every `upsert` fails with the WRITE-shaped rejection
        /// (`CoordWriteError`, carrying coord's status and body) rather than the
        /// READ-shaped `ForbiddenByCoord`. Both shapes reach the same `Err` arm
        /// and a `403` must retire the slug from EITHER.
        upsert_write_status: Option<u16>,
        /// Body served with `upsert_write_status`. Defaults to the
        /// `self_attestation_forbidden` denial the existing tests assume; a test
        /// exercising coord's `terminality` hint supplies its own.
        upsert_write_body: Option<String>,
        /// `(status, http status, body)`: a write that carries THIS status —
        /// an upsert's `status` field or a transition's `to_status` — fails
        /// with that answer, while every other write succeeds.
        ///
        /// Models what `upsert_write_status` cannot: coord's denials are scoped
        /// to the `(slug, status)` PAIR, so a sink that refuses every write
        /// regardless of what it carries can never show whether the client's
        /// retirement is keyed as narrowly as the server's permanence.
        deny_status: Option<(String, u16, String)>,
        /// Total `upsert` calls received, so a test can prove a retired slug
        /// stops making the HTTP call at all — not merely stops logging.
        upsert_calls: Mutex<u64>,
        /// Of those, the ones that carried a STATUS — counted BEFORE any
        /// simulated failure, so a refused write is counted too.
        ///
        /// `upsert_calls` alone cannot answer "was the refused request
        /// re-issued?": a permanently-retired `(slug, status)` pair still emits
        /// one status-LESS provenance upsert per cycle by design, so the count
        /// that must stay pinned is this one.
        status_upsert_calls: Mutex<u64>,
        /// Every upsert body seen, so the archive scan can be asserted to write
        /// `metadata.archive_path` with no status.
        upserts: Mutex<Vec<UpsertBody>>,
        /// When set, the sink HAS a bulk seed door and answers it with this map.
        /// `None` (the default) models a sink with no bulk door, so the trait's
        /// defaulted `Ok(None)` and the per-slug fallback stay the tested norm.
        bulk: Option<HashMap<String, String>>,
        /// Total `current_status` reads served, so a test can prove the bulk
        /// prime REMOVED the per-slug round-trips rather than merely duplicating
        /// them.
        current_status_calls: Mutex<u64>,
        /// Total `list_statuses` reads served, so "attempted once per corpus"
        /// is asserted rather than assumed.
        list_statuses_calls: Mutex<u64>,
        /// When set, every `list_statuses` read hard-errors — the bulk seed's
        /// retry arm, which leaves the per-slug seed to carry correctness.
        /// Atomic so a test can flip it between ticks through `&sink`.
        fail_list_statuses: std::sync::atomic::AtomicBool,
        /// EVERY sink method, in call order, so a test can assert what a cycle
        /// asked coord FIRST — a count alone cannot tell a re-prime that ran
        /// before the push from a conflict-check read that ran after it.
        calls: Mutex<Vec<&'static str>>,
    }
    impl FakeSink {
        fn record(&self, method: &'static str) {
            self.calls.lock().unwrap().push(method);
        }
        /// The ledger, cloned out so no guard is held across an `assert!`.
        fn ledger(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }
    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, slug: &str) -> Result<Option<String>> {
            self.record("current_status");
            *self.current_status_calls.lock().unwrap() += 1;
            if self.fail_current_status {
                anyhow::bail!("simulated current_status failure");
            }
            if self.fail_status_read_for.as_deref() == Some(slug) {
                anyhow::bail!("simulated work-unit status read failure");
            }
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
        }
        async fn list_statuses(&self) -> Result<Option<HashMap<String, String>>> {
            self.record("list_statuses");
            *self.list_statuses_calls.lock().unwrap() += 1;
            if self.fail_list_statuses.load(Ordering::Relaxed) {
                anyhow::bail!("simulated list_statuses failure");
            }
            Ok(self.bulk.clone())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            self.record("last_actor");
            *self.last_actor_calls.lock().unwrap() += 1;
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            self.record("upsert");
            *self.upsert_calls.lock().unwrap() += 1;
            if body.status.is_some() {
                *self.status_upsert_calls.lock().unwrap() += 1;
            }
            if self.fail_upsert_for.as_deref() == Some(body.slug.as_str()) {
                anyhow::bail!("simulated work-unit upsert failure");
            }
            if self.upsert_forbidden {
                return Err(anyhow::Error::new(
                    crate::plan_workunit_adapter::push::ForbiddenByCoord {
                        route: "POST /coord/work-units/upsert",
                        detail: r#"{"error":"self_attestation_forbidden"}"#.to_string(),
                    },
                ));
            }
            if let Some((denied, http, deny_body)) = &self.deny_status {
                if body.status.as_deref() == Some(denied.as_str()) {
                    return Err(crate::plan_workunit_adapter::push::CoordWriteError {
                        op: "upsert",
                        slug: body.slug.clone(),
                        status: Some(*http),
                        body: deny_body.clone(),
                    }
                    .into());
                }
            }
            if let Some(status) = self.upsert_write_status {
                return Err(crate::plan_workunit_adapter::push::CoordWriteError {
                    op: "upsert",
                    slug: body.slug.clone(),
                    status: Some(status),
                    body: self
                        .upsert_write_body
                        .clone()
                        .unwrap_or_else(|| r#"{"error":"self_attestation_forbidden"}"#.to_string()),
                }
                .into());
            }
            if self.upsert_errors {
                anyhow::bail!("simulated transient upsert failure");
            }
            if let Some(s) = &body.status {
                let stored = match &self.normalize {
                    Some((from, to)) if from == s => to.clone(),
                    _ => s.clone(),
                };
                self.statuses
                    .lock()
                    .unwrap()
                    .insert(body.slug.clone(), stored);
            }
            self.upserts.lock().unwrap().push(body.clone());
            Ok(())
        }
        async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
            self.record("transition");
            // BEFORE the deny check: a refused transition is still a request
            // that went out. See `transition_attempts`.
            *self.transition_attempts.lock().unwrap() += 1;
            if let Some((denied, http, deny_body)) = &self.deny_status {
                if body.to_status == *denied {
                    return Err(crate::plan_workunit_adapter::push::CoordWriteError {
                        op: "transition",
                        slug: slug.to_string(),
                        status: Some(*http),
                        body: deny_body.clone(),
                    }
                    .into());
                }
            }
            *self.transitions.lock().unwrap() += 1;
            self.statuses
                .lock()
                .unwrap()
                .insert(slug.to_string(), body.to_status.clone());
            Ok(())
        }
        async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
            self.record("set_deps");
            self.deps_calls
                .lock()
                .unwrap()
                .push((slug.to_string(), depends_on.to_vec()));
            match self.deps_behavior {
                DepsBehavior::Ok => Ok(SetDepsOutcome::Ok {
                    edges_set: depends_on.len() as u64,
                }),
                DepsBehavior::TableNotMigrated => Ok(SetDepsOutcome::TableNotMigrated),
                DepsBehavior::Error => anyhow::bail!("simulated deps endpoint failure"),
                // The shape `HttpWorkUnitSink::set_deps` ACTUALLY produces for
                // a 403: it builds a `CoordWriteError` for every non-2xx and
                // never a `ForbiddenByCoord` (only `classify_failure`, on the
                // two READ routes, builds that one). This fixture used to
                // synthesise the read shape, which is why the deps arm's
                // downcast could be unreachable in production and still test
                // green.
                DepsBehavior::Forbidden => {
                    Err(crate::plan_workunit_adapter::push::CoordWriteError {
                        op: "set_deps",
                        slug: slug.to_string(),
                        status: Some(403),
                        body: r#"{"error":"self_attestation_forbidden"}"#.to_string(),
                    }
                    .into())
                }
            }
        }
    }

    #[tokio::test]
    async fn reconcile_is_idempotent_across_cycles() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let units = vec![unit("a", "vetted"), unit("b", "draft")];

        // First cycle: both created, no transitions.
        let s1 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s1.scanned, 2);
        assert_eq!(s1.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // Second cycle, unchanged corpus: NO phantom transitions.
        let s2 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.scanned, 2);
        assert_eq!(s2.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    /// A `Write` into a shared buffer, so a test can read what `tracing` logged.
    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A rejected status-block `Area:` is warned by the reconcile — exactly
    /// once per plan per pass — and the push still lands, with `metadata.area`
    /// omitted. A unit with an accepted area is not warned and carries it.
    #[tokio::test]
    async fn reconcile_warns_once_per_pass_for_a_rejected_area() {
        use super::super::parser::AreaRejection;
        let buf = LogBuf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let bad = ParsedWorkUnit {
            area_rejected: Some(AreaRejection::NotKebab("agent_worktree".to_string())),
            ..unit("2026-01-01-bad-area", "draft")
        };
        let good = ParsedWorkUnit {
            area: Some("ci-runners".to_string()),
            ..unit("2026-01-01-good-area", "draft")
        };
        let units = vec![bad, good];
        for _ in 0..2 {
            reconcile_once(
                &units,
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
        }

        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let warned: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("status-block `Area:` rejected"))
            .collect();
        assert_eq!(
            warned.len(),
            2,
            "one warning per pass for the one bad plan: {log}"
        );
        assert!(warned
            .iter()
            .all(|l| l.contains("2026-01-01-bad-area") && l.contains("agent_worktree")));

        let upserts = sink.upserts.lock().unwrap();
        let meta_for = |slug: &str| {
            upserts
                .iter()
                .rev()
                .find(|b| b.slug == slug)
                .and_then(|b| b.metadata.clone())
                .expect("the unit was pushed")
        };
        assert!(!meta_for("2026-01-01-bad-area")
            .as_object()
            .unwrap()
            .contains_key("area"));
        assert_eq!(meta_for("2026-01-01-good-area")["area"], "ci-runners");
    }

    #[tokio::test]
    async fn reconcile_emits_one_transition_on_status_edge() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // Plan edited: vetted -> shipped.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    // --- Cold-start seeding: a runner restart must not re-create coord's rows -
    //
    // `last_applied` is per-PROCESS. Before seeding, the first cycle after every
    // runner start saw `None` for every slug and emitted `UpsertWithStatus` with
    // the FILE's status — bypassing the ownership deferral (Transition-only) and
    // the conflict check (inside `if let Some(prev)`), and landing on coord's
    // `COALESCE($3, status)` which overwrites on a non-NULL `$3`.

    #[tokio::test]
    async fn cold_start_does_not_demote_a_unit_a_real_agent_owns() {
        // coord says `shipped`; the plan file still says `in_progress` (the
        // single commonest divergence on this fleet: 48 of 488 units).
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        // COLD: exactly the state `run_loop` builds on every runner start.
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        // Assert the DEMOTION first, so a regression fails on the behaviour
        // this test exists for rather than on the mechanism that prevents it.
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "coord's terminal status must survive a cold start (without seeding \
             this reads `in_progress` — the measured 2026-09-01 demotion)"
        );
        assert_eq!(s.transitions, 0, "a real agent owns it -> DEFER");
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(s.seeded, 1, "the remote status must be seeded");
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            1,
            "the seed read is handed to the push as `known_remote`, so the \
             deferral's convergence check must NOT pay a second GET"
        );
        // And no upsert may carry a status: that is the overwrite vector.
        assert!(
            sink.upserts
                .lock()
                .unwrap()
                .iter()
                .all(|b| b.status.is_none()),
            "no upsert may carry a status for a unit coord already has"
        );
    }

    #[tokio::test]
    async fn cold_start_with_matching_status_refreshes_without_a_transition() {
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "vetted".to_string());
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 1);
        assert_eq!(s.transitions, 0, "status unchanged -> RefreshOnly");
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("vetted")
        );
    }

    #[tokio::test]
    async fn cold_start_still_creates_a_slug_coord_has_never_seen() {
        // The seed must not break the legitimate create path.
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("new", "draft")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 0, "nothing to seed from — genuinely absent");
        assert_eq!(s.seed_errors, 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("new").map(String::as_str),
            Some("draft"),
            "a brand-new unit is still created WITH its status"
        );
    }

    #[tokio::test]
    async fn seed_read_failure_abstains_rather_than_overwriting() {
        // UNKNOWN must not degrade to "no prior status", which is precisely the
        // fall-through that produced the demotions.
        let sink = FakeSink {
            fail_current_status: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seed_errors, 1);
        assert_eq!(s.seeded, 0);
        assert_eq!(s.errors, 0, "an abstention is not a push error");
        assert!(
            sink.upserts.lock().unwrap().is_empty(),
            "an unreadable remote status must produce NO write at all"
        );
        assert_eq!(metrics.snapshot().seed_errors_total, 1);
        // The unit is not remembered, so the next cycle retries it.
        assert!(!mem.contains_key("a"));
    }

    #[tokio::test]
    async fn bulk_seed_primes_last_applied_and_removes_the_per_slug_reads() {
        // What `run_loop`'s bulk prime does, asserted at the reconcile level:
        // a pre-primed `last_applied` means reconcile_once issues NO per-slug
        // seed at all.
        //
        // `last_actor` is set because priming ALONE does not protect anything —
        // it restores the edge-trigger, and the OWNERSHIP DEFERRAL is what then
        // withholds the transition. A unit with no history legitimately
        // transitions (see `proceeds_when_no_history`), so a default sink here
        // would demote and would be asserting the wrong thing. Every real coord
        // row this adapter has never pushed has a non-adapter actor.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        // Primed exactly as run_loop primes it, from the bulk read.
        let mut mem: HashMap<String, String> = [("a".to_string(), "shipped".to_string())]
            .into_iter()
            .collect();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 0, "the bulk prime already covered this slug");
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            1,
            "ZERO seed reads — the one read is the deferral's convergence check \
             inside push_work_unit, not a seed"
        );
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "priming must protect the terminal status just as the per-slug seed does"
        );
    }

    /// A sink that does NOT implement `list_statuses` at all must inherit
    /// `Ok(None)` from the trait — "I have no bulk door" — and never `Ok(Some(
    /// empty))`, which would read as "coord has no units" and seed nothing
    /// while claiming a successful read.
    ///
    /// `NoBulkSink` exists because `FakeSink` DOES override `list_statuses` (it
    /// counts the calls), so asserting against `FakeSink::default()` would only
    /// prove that its own `bulk` field defaults to `None` — deleting the trait
    /// default entirely would not fail it. This sink implements every
    /// REQUIRED method and nothing else, so the assertion below reaches the
    /// default body or it does not compile.
    #[derive(Default)]
    struct NoBulkSink;

    #[async_trait::async_trait]
    impl WorkUnitSink for NoBulkSink {
        async fn current_status(&self, _slug: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn upsert(&self, _body: &UpsertBody) -> Result<()> {
            Ok(())
        }
        async fn transition(&self, _slug: &str, _body: &TransitionBody) -> Result<()> {
            Ok(())
        }
        async fn set_deps(&self, _slug: &str, _depends_on: &[String]) -> Result<SetDepsOutcome> {
            Ok(SetDepsOutcome::Ok { edges_set: 0 })
        }
    }

    #[tokio::test]
    async fn a_sink_without_a_bulk_door_falls_back_to_the_per_slug_seed() {
        assert!(
            NoBulkSink.list_statuses().await.unwrap().is_none(),
            "the trait's DEFAULT bulk door must be None, not an empty map — an \
             empty map would read as 'coord has no units' and seed nothing"
        );
    }

    #[tokio::test]
    async fn seeding_is_a_first_cycle_cost_only() {
        // Steady state must not pay a current_status read per unit per cycle.
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "vetted".to_string());
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let units = vec![unit("a", "vetted")];

        let s1 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s2 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s1.seeded, 1, "seeded on the cold cycle");
        assert_eq!(s2.seeded, 0, "in-process memory serves the second cycle");
    }

    // --- Graduation-bootstrap P2a: markdown proxy defers to real agents ------

    #[tokio::test]
    async fn defers_transition_when_real_agent_owns_unit() {
        // A real agent last drove the unit (its own agent-scoped actor). The
        // file's status edge (vetted -> in_progress) WOULD transition, but the proxy
        // must DEFER so it doesn't collapse the agent's transition to the system
        // actor: ZERO transitions emitted.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        // Establish last-applied=vetted (create; UpsertWithStatus is never gated).
        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // File edited vetted -> in_progress: transition WOULD fire, but defer.
        // (A SETTABLE target: a transition onto a coord-derived word is not
        // deferred — see
        // `a_derived_transition_on_an_owned_unit_is_retired_not_deferred_forever`.)
        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(metrics.snapshot().transitions_total, 0);
        // The deferral is COUNTED, not silently folded into the refresh tally.
        assert_eq!(s.deferred, 1);
        assert_eq!(metrics.snapshot().deferrals_total, 1);

        // …and a PERSISTENT deferral is counted every cycle, not once. Recording
        // the un-applied status in `last_applied` would make the next cycle
        // answer `RefreshOnly`, so `deferred` would read 0 from here on —
        // indistinguishable from "the divergence went away".
        let s3 = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s3.deferred, 1, "a standing deferral stays visible");
        assert_eq!(s3.transitions, 0);
        assert_eq!(
            s3.conflicts, 0,
            "and it never degrades into a permanent bogus conflict warning"
        );
        assert_eq!(metrics.snapshot().deferrals_total, 2);
    }

    #[tokio::test]
    async fn proceeds_when_last_actor_is_adapter() {
        // The adapter itself last drove the unit ⇒ no real agent owns it ⇒ the
        // proxy proceeds with its transition as normal.
        let sink = FakeSink {
            last_actor: Some(ADAPTER_ACTOR.to_string()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    #[tokio::test]
    async fn proceeds_when_no_history() {
        // No history (last_actor = None) ⇒ nobody owns the unit ⇒ proceed.
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    #[tokio::test]
    async fn unit_with_deps_pushes_dep_edges_with_right_args() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p2".to_string()]);

        let s = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.deps_set, 1);
        assert_eq!(s.errors, 0);
        let calls = sink.deps_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "p4");
        assert_eq!(calls[0].1, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(metrics.snapshot().deps_set_total, 1);
    }

    #[tokio::test]
    async fn empty_deps_unit_makes_no_dep_call_and_no_error() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.deps_set, 0);
        assert_eq!(s.deps_errors, 0);
        assert_eq!(s.errors, 0);
        assert!(sink.deps_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dep_edges_are_edge_triggered_across_cycles() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        // First cycle sends deps.
        reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // Second cycle, unchanged dep set: no re-send (idempotent edge-trigger).
        let s2 = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.deps_set, 0);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 1);

        // Dep set changed -> re-send.
        let u2 = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p3".to_string()]);
        let s3 = reconcile_once(
            &[u2],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s3.deps_set, 1);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn table_not_migrated_does_not_fail_reconcile_and_retries() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::TableNotMigrated,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // 503 is benign: no reconcile error, the unit upsert still succeeded.
        assert_eq!(s.errors, 0);
        assert_eq!(s.deps_errors, 0);
        assert_eq!(s.deps_set, 0);
        assert_eq!(s.deps_skipped_unmigrated, 1);
        assert_eq!(metrics.snapshot().deps_skipped_unmigrated_total, 1);

        // last_deps NOT cached on 503 -> next cycle retries the edge write.
        let s2 = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.deps_skipped_unmigrated, 1);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn dep_edge_hard_error_is_non_fatal() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::Error,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // A dep-edge failure does NOT fail the reconcile (unit upsert landed).
        assert_eq!(s.errors, 0);
        assert_eq!(s.deps_errors, 1);
        assert_eq!(s.deps_set, 0);
        assert_eq!(metrics.snapshot().deps_errors_total, 1);
    }

    /// RT7: a coord `403` retires the slug. Before this, the adapter re-issued
    /// the identical refused request — and emitted a full WARN — once per slug
    /// per cycle, forever; at ~343 plans on a ~68s cycle that was the single
    /// largest consumer of the runner's log budget.
    ///
    /// The assertion that makes this a fix rather than a mute is
    /// `upsert_calls == 1`: the retired slug stops making the HTTP call, not
    /// just the log line. `forbidden_total` incrementing exactly once is the
    /// "<= 1 log line per slug per process" property, since the warn sits on
    /// the same branch as that increment.
    #[tokio::test]
    async fn a_403_retires_the_slug_for_the_life_of_the_process() {
        let sink = FakeSink {
            upsert_forbidden: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s1 = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s1.forbidden, 1);
        assert_eq!(
            s1.errors, 0,
            "a permission verdict is not a retryable error"
        );

        for cycle in 0..5 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.forbidden, 1, "cycle {cycle} still counts the skip");
            assert_eq!(s.errors, 0, "cycle {cycle} must not re-error");
        }

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the refused slug must be asked exactly once, not once per cycle"
        );
        assert_eq!(
            metrics.snapshot().forbidden_total,
            1,
            "one increment per refused slug — and so one WARN per slug per process"
        );
        assert_eq!(metrics.snapshot().errors_total, 0);
    }

    /// A `403` on a WRITE retires the slug exactly like a `403` on a read.
    ///
    /// The two carry the verdict in different types — reads in
    /// `ForbiddenByCoord`, writes in `CoordWriteError` — and this arm reads
    /// both. Honouring only the read shape would leave the write half of the
    /// retry storm running while every test above still passed.
    #[tokio::test]
    async fn a_write_shaped_403_retires_the_slug_too() {
        let sink = FakeSink {
            // No `terminality` in the body: the coord builds that predate the
            // hint, whose 403s are exactly the log-flood this retirement
            // closed. UNKNOWN must not silently re-open it, so this arm still
            // retires — principal-wide.
            upsert_write_status: Some(403),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        for cycle in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.forbidden, 1, "cycle {cycle}");
            assert_eq!(s.errors, 0, "cycle {cycle}");
        }
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the refused slug must be asked exactly once, not once per cycle"
        );
        assert_eq!(metrics.snapshot().forbidden_total, 1);
        assert_eq!(
            forb.retirement_for("a", "any-other-status"),
            Some(RetirementReason::ForbiddenPrincipal),
            "a permission verdict is about the PRINCIPAL, so it retires every status"
        );
    }

    /// A `permanent` refusal of a SETTABLE word, for the tests that exercise
    /// the pair-scoped retirement on the `UpsertWithStatus` door. coord's only
    /// `permanent` today is `status_is_derived`, which it never answers for
    /// `vetted`; this body is deliberately not that one, so the fixture does not
    /// teach a false model of which words coord derives. The mechanism under
    /// test keys on `terminality`, never on the code.
    const SYNTHETIC_PERMANENT: &str =
        r#"{"error":"a_future_permanent_refusal","terminality":"permanent"}"#;

    /// Run `cycles` reconciles of the same one-unit corpus against `sink`,
    /// returning each cycle's summary. The retirement tests below differ only
    /// in the corpus and the sink, and a seven-argument call per cycle buries
    /// what they assert.
    struct Reconciler {
        metrics: AdapterMetrics,
        mem: HashMap<String, String>,
        deps: HashMap<String, Vec<String>>,
        forb: RetiredSlugs,
        forb_deps: HashSet<String>,
    }
    impl Reconciler {
        fn new() -> Self {
            Self {
                metrics: AdapterMetrics::default(),
                mem: HashMap::new(),
                deps: HashMap::new(),
                forb: RetiredSlugs::default(),
                forb_deps: HashSet::new(),
            }
        }
        async fn cycle(&mut self, sink: &FakeSink, units: &[ParsedWorkUnit]) -> ReconcileSummary {
            reconcile_once(
                units,
                &mut self.mem,
                &mut self.deps,
                &mut self.forb,
                &mut self.forb_deps,
                sink,
                &self.metrics,
            )
            .await
        }
    }

    /// **A write-shaped `422` is retired iff coord says `terminality:
    /// "permanent"` — and then only for the `(slug, status)` pair.**
    ///
    /// This replaces `a_write_shaped_422_is_not_retired`, which pinned the
    /// defect: its fixture carried no terminality, and the arm it guarded
    /// retried EVERY 422 forever, including the ones coord had marked
    /// permanent (15,541 `status_is_derived` refusals across 186 slugs on the
    /// operator box, 40.7% of a core). The narrow half of that test is kept
    /// below: a 422 with NO permanent hint is still retried every cycle.
    #[tokio::test]
    async fn a_write_shaped_422_is_retired_only_when_coord_says_permanent() {
        // Permanent: asked once, then the status write is withheld. coord
        // scopes the refusal to the pair, so the status-less metadata upsert
        // that follows is ACCEPTED — modelled with `deny_status`.
        let permanent = FakeSink {
            deny_status: Some(("vetted".to_string(), 422, SYNTHETIC_PERMANENT.to_string())),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        for _ in 0..3 {
            let s = r.cycle(&permanent, &[unit("a", "vetted")]).await;
            assert_eq!(s.errors, 0, "a permanent denial is not a retryable error");
            assert_eq!(s.forbidden, 0, "...nor a permission verdict");
            assert_eq!(s.retired_permanent, 1);
        }
        assert_eq!(
            *permanent.status_upsert_calls.lock().unwrap(),
            1,
            "the permanently-refused status write is sent exactly ONCE"
        );
        assert_eq!(
            r.forb.retirement_for("a", "vetted"),
            Some(RetirementReason::PermanentForStatus)
        );
        assert_eq!(r.forb.retirement_for("a", "draft"), None, "pair, not slug");

        // A coord that refused the status-LESS metadata upsert permanently too
        // (not a shape it produces today): the pair is retired once, and each
        // later refused provenance push is a VISIBLE error, never a silent
        // drop that also stops counting.
        let everything = FakeSink {
            upsert_write_status: Some(422),
            upsert_write_body: Some(r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#.to_string()),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        let s1 = r.cycle(&everything, &[unit("a", "vetted")]).await;
        assert_eq!((s1.retired_permanent, s1.errors), (1, 0));
        for cycle in 1..3 {
            let s = r.cycle(&everything, &[unit("a", "vetted")]).await;
            assert_eq!(s.retired_permanent, 1, "cycle {cycle}");
            assert_eq!(
                s.errors, 1,
                "cycle {cycle}: the refused metadata push is counted"
            );
        }
        assert_eq!(r.metrics.snapshot().retired_permanent_total, 1);

        // No hint (an older coord, or a structural refusal coord gave no retry
        // semantics for): UNKNOWN retries, exactly as before.
        let unknown = FakeSink {
            upsert_write_status: Some(422),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        for _ in 0..3 {
            let s = r.cycle(&unknown, &[unit("a", "vetted")]).await;
            assert_eq!(s.errors, 1);
            assert_eq!(s.forbidden, 0);
            assert_eq!(s.retired_permanent, 0);
        }
        assert_eq!(*unknown.upsert_calls.lock().unwrap(), 3);
        assert!(r.forb.is_empty());
    }

    /// coord ships a `terminality` hint on every work-unit write denial
    /// precisely so a client stops re-issuing a request that can never
    /// succeed. `permanent` retires the `(slug, status)` pair coord scoped it
    /// to: one status write, one WARN, then silence for as long as the file
    /// keeps parsing to that status.
    ///
    /// Adapted from runner#1474 onto main's cold-start seed: the FIRST cycle
    /// pays one `current_status` read (the per-slug seed, since this process
    /// has no memory of the slug), and a retired pair is never seeded again —
    /// which is what keeps the per-cycle read count flat after the denial.
    #[tokio::test]
    async fn a_permanent_terminality_retires_the_pair_and_logs_once() {
        let logs = CapturedLogs::start();
        let sink = FakeSink {
            // Refuses exactly the status write, at the pair scope coord's
            // `permanent` covers; the later status-less upserts are accepted.
            deny_status: Some(("vetted".to_string(), 422, SYNTHETIC_PERMANENT.to_string())),
            ..Default::default()
        };
        let mut r = Reconciler::new();

        let first = r.cycle(&sink, &[unit("a", "vetted")]).await;
        assert_eq!(first.retired_permanent, 1);
        assert_eq!(first.errors, 0);
        assert_eq!(first.forbidden, 0);
        let after_first = logs.text();
        // Assert on the STRUCTURED FIELDS a log consumer filters on; the WARN's
        // prose deliberately does not repeat the token.
        assert!(
            after_first.contains(r#"terminality="permanent""#),
            "the retirement must carry the structured terminality field: {after_first}"
        );
        assert!(
            after_first.contains(r#"retirement_scope="slug+status""#),
            "...and must say how NARROWLY it retired: {after_first}"
        );
        assert!(
            after_first.contains("attempted_status=vetted"),
            "...naming the status the retirement is keyed on: {after_first}"
        );
        let reads_after_first = *sink.current_status_calls.lock().unwrap();
        assert_eq!(
            reads_after_first, 1,
            "cycle 1 pays the cold-start seed only"
        );

        for cycle in 1..4 {
            let s = r.cycle(&sink, &[unit("a", "vetted")]).await;
            assert_eq!(s.retired_permanent, 1, "cycle {cycle} counts it once");
            assert_eq!(s.errors, 0, "cycle {cycle} must not re-error");
        }
        assert_eq!(
            *sink.status_upsert_calls.lock().unwrap(),
            1,
            "the permanently-refused pair must be asked exactly ONCE, not once per cycle"
        );
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            reads_after_first,
            "a retired pair is neither re-seeded nor conflict-checked: no GET per cycle"
        );
        let snap = r.metrics.snapshot();
        assert_eq!(snap.retired_permanent_total, 1, "one increment per pair");
        assert_eq!(
            snap.forbidden_total, 0,
            "a plan-file verdict is not a grant verdict"
        );
        assert_eq!(snap.errors_total, 0);
        assert_eq!(
            logs.text(),
            after_first,
            "cycles 2..4 must emit NO further log output at all"
        );
    }

    /// **The retirement key is the scope coord ASSERTED, and not one word
    /// broader.** `DenialTerminality::Permanent` is scoped to the `(slug,
    /// status)` pair, so the moment the file's parsed status changes the
    /// request is not identical any more and coord's permanence does not
    /// reach it.
    ///
    /// The failure this pins is the ORDINARY path: a plan is stamped `vetted`,
    /// lands, and someone edits the stamp to `shipped`. The transition 422s
    /// `permanent`. Keyed on the SLUG, that would retire the unit for the life
    /// of the process and silently drop every later edit — with no way back
    /// short of a runner restart, which served policy `production-and-cost`
    /// `runner-lifecycle` forbids.
    #[tokio::test]
    async fn a_permanent_retirement_is_keyed_on_the_pair_and_clears_when_the_file_changes() {
        let sink = FakeSink {
            deny_status: Some(("shipped".to_string(), 422, r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#.to_string())),
            ..Default::default()
        };
        let mut r = Reconciler::new();

        // 1. The plan is `vetted`. It lands.
        let s1 = r.cycle(&sink, &[unit("a", "vetted")]).await;
        assert_eq!(s1.errors, 0);
        assert_eq!(r.mem.get("a").map(String::as_str), Some("vetted"));

        // 2. Edited to `shipped`: the transition carries the derived word,
        //    coord refuses it PERMANENTLY, and the pair retires.
        let s2 = r.cycle(&sink, &[unit("a", "shipped")]).await;
        assert_eq!(s2.retired_permanent, 1);
        assert_eq!(
            r.forb.retirement_for("a", "shipped"),
            Some(RetirementReason::PermanentForStatus)
        );
        assert_eq!(
            r.forb.retirement_for("a", "in_progress"),
            None,
            "coord refused `shipped`; it said nothing about any other status"
        );
        assert_eq!(
            r.mem.get("a").map(String::as_str),
            Some("vetted"),
            "a REFUSED transition applied nothing, so the memory must not move"
        );
        let status_writes_after_denial = *sink.status_upsert_calls.lock().unwrap();
        let reads_after_denial = *sink.current_status_calls.lock().unwrap();
        let attempts_after_denial = *sink.transition_attempts.lock().unwrap();
        assert_eq!(attempts_after_denial, 1, "one transition ATTEMPT — refused");
        assert_eq!(
            *sink.transitions.lock().unwrap(),
            0,
            "...and none SUCCEEDED"
        );

        // 3. Still `shipped`: the identical request is not re-issued.
        let s3 = r.cycle(&sink, &[unit("a", "shipped")]).await;
        assert_eq!(s3.retired_permanent, 1);
        assert_eq!(
            *sink.status_upsert_calls.lock().unwrap(),
            status_writes_after_denial,
            "a still-`shipped` file must issue NO further status write"
        );
        assert_eq!(
            *sink.transition_attempts.lock().unwrap(),
            attempts_after_denial,
            "...nor a transition — asserted on the ATTEMPT counter, the only one \
             that can see a re-issued DENIED request"
        );
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            reads_after_denial,
            "...nor the conflict-check GET that a status write would arm"
        );

        // 4. THE POINT. Demoted to `in_progress` — a DIFFERENT request, which
        //    coord never refused. Pushed on the very next cycle, no restart.
        let s4 = r.cycle(&sink, &[unit("a", "in_progress")]).await;
        assert_eq!(
            s4.errors, 0,
            "the edited file must not be treated as retired"
        );
        assert_eq!(s4.retired_permanent, 0);
        assert_eq!(s4.transitions, 1);
        assert_eq!(r.mem.get("a").map(String::as_str), Some("in_progress"));

        // 5. ...and the pair is still remembered, so a file edited BACK to the
        //    refused word costs no status write either.
        let status_writes = *sink.status_upsert_calls.lock().unwrap();
        let attempts = *sink.transition_attempts.lock().unwrap();
        let s5 = r.cycle(&sink, &[unit("a", "shipped")]).await;
        assert_eq!(s5.retired_permanent, 1);
        assert_eq!(*sink.status_upsert_calls.lock().unwrap(), status_writes);
        assert_eq!(
            *sink.transition_attempts.lock().unwrap(),
            attempts,
            "the refused request is not even ATTEMPTED again"
        );
        assert_eq!(r.metrics.snapshot().retired_permanent_total, 1, "one WARN");
    }

    /// **A permanent denial retires the STATUS WRITE, never the unit.** The
    /// status-less metadata upsert is a different request, which coord
    /// accepts, so a retired pair's provenance (`title`, `source_path`) keeps
    /// reaching coord.
    ///
    /// Neuter checks: restore a `continue` in `reconcile_once`'s
    /// `PermanentForStatus` arm and the provenance assertions fail; delete
    /// `if status_retired { action = PushAction::RefreshOnly; }` from
    /// `push_work_unit_with_status_write` and the `transition_attempts`
    /// assertion fails — every other assertion stays green, because a denied
    /// transition still emits the status-less provenance upsert first.
    #[tokio::test]
    async fn a_permanently_retired_pair_still_pushes_its_provenance_every_cycle() {
        let logs = CapturedLogs::start();
        let sink = FakeSink {
            deny_status: Some(("shipped".to_string(), 422, r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#.to_string())),
            ..Default::default()
        };
        let mut r = Reconciler::new();

        r.cycle(
            &sink,
            &[unit_with_provenance(
                "a",
                "vetted",
                "Original",
                "plans/a.md",
            )],
        )
        .await;
        let s2 = r
            .cycle(
                &sink,
                &[unit_with_provenance(
                    "a",
                    "shipped",
                    "Original",
                    "plans/a.md",
                )],
            )
            .await;
        assert_eq!(s2.retired_permanent, 1);
        let status_writes = *sink.status_upsert_calls.lock().unwrap();
        let reads = *sink.current_status_calls.lock().unwrap();
        let attempts = *sink.transition_attempts.lock().unwrap();
        assert_eq!(attempts, 1, "one attempt, refused");
        let log_at_retirement = logs.text();

        // The plan keeps being EDITED while its stamp stays `shipped`.
        for (title, path) in [
            ("Renamed once", "plans/a.md"),
            ("Renamed twice", "plans/archive-candidates/a.md"),
        ] {
            let s = r
                .cycle(&sink, &[unit_with_provenance("a", "shipped", title, path)])
                .await;
            assert_eq!(s.retired_permanent, 1, "counted ONCE per cycle, not twice");
            assert_eq!(s.errors, 0);
        }
        {
            let upserts = sink.upserts.lock().unwrap();
            assert!(
                upserts
                    .iter()
                    .any(|u| u.title.as_deref() == Some("Renamed once")),
                "the retitle must reach coord: {upserts:?}"
            );
            let last = upserts.last().expect("at least one upsert");
            assert_eq!(last.title.as_deref(), Some("Renamed twice"));
            assert_eq!(
                last.metadata.as_ref().expect("metadata")["source_path"],
                "plans/archive-candidates/a.md",
                "a moved plan's provenance must follow it"
            );
            assert_eq!(last.status, None, "...carried by a status-LESS upsert");
        }
        assert_eq!(*sink.status_upsert_calls.lock().unwrap(), status_writes);
        assert_eq!(
            *sink.transition_attempts.lock().unwrap(),
            attempts,
            "the permanently-refused transition must not be RE-ISSUED"
        );
        assert_eq!(*sink.current_status_calls.lock().unwrap(), reads);
        assert_eq!(logs.text(), log_at_retirement, "still ONE WARN per pair");
        assert_eq!(
            r.mem.get("a").map(String::as_str),
            Some("vetted"),
            "nothing was applied, so the memory still names what coord accepted"
        );
    }

    /// **A conflicted `RefreshOnly` must CONVERGE, not warn forever.**
    /// Reporting the OBSERVED REMOTE as applied makes the next cycle a real
    /// edge, which goes through the deferral and the CAS guard. This pins the
    /// UNOWNED branch (no `last_actor`), where the file wins on cycle 3.
    ///
    /// Neuter check: report `Some(u.status)` again from the conflicted
    /// `RefreshOnly` arm — cycle 3 emits no transition and the divergence
    /// WARN count climbs with every cycle.
    #[tokio::test]
    async fn a_conflicted_refresh_converges_when_no_real_actor_owns_the_unit() {
        let logs = CapturedLogs::start();
        let sink = FakeSink::default();
        let mut r = Reconciler::new();
        let file = [unit("a", "vetted")];

        let s1 = r.cycle(&sink, &file).await;
        assert_eq!(s1.conflicts, 0);
        assert_eq!(r.mem.get("a").map(String::as_str), Some("vetted"));

        // Another writer moves the unit in coord, out of band — one whose
        // history row carries no real-agent actor (here: `last_actor` is
        // `None`), i.e. the unowned branch.
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "in_progress".to_string());

        let s2 = r.cycle(&sink, &file).await;
        assert_eq!(s2.conflicts, 1, "the divergence is real and is announced");
        assert_eq!(s2.transitions, 0);
        assert_eq!(
            r.mem.get("a").map(String::as_str),
            Some("in_progress"),
            "the memory must record what coord HOLDS"
        );

        let s3 = r.cycle(&sink, &file).await;
        assert_eq!(s3.transitions, 1, "cycle 3 issues in_progress -> vetted");
        assert_eq!(s3.conflicts, 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("vetted"),
            "the file actually won"
        );
        for cycle in 4..7 {
            let s = r.cycle(&sink, &file).await;
            assert_eq!(s.conflicts, 0, "cycle {cycle}");
            assert_eq!(s.transitions, 0, "cycle {cycle}");
        }
        assert_eq!(
            logs.text().matches("diverged from last-applied").count(),
            1,
            "the override is announced ONCE; got: {}",
            logs.text()
        );
        assert_eq!(r.metrics.snapshot().conflicts_total, 1);
    }

    /// **The OWNED branch of a conflicted refresh is a STABLE DEFERRAL, not
    /// convergence** — and it is the dominant one, because a conflict means a
    /// non-adapter writer moved the unit and `coord::derive_worker` counts as a
    /// real actor. Its steady-state price is pinned: TWO reads per cycle per
    /// slug. What the change bought is that the `file wins (loud override)`
    /// WARN fires ONCE, on the event, instead of once per cycle forever.
    #[tokio::test]
    async fn a_conflicted_refresh_defers_stably_when_a_real_actor_owns_the_unit() {
        let logs = CapturedLogs::start();
        let sink = FakeSink {
            last_actor: Some("coord::derive_worker".to_string()),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        let file = [unit("a", "vetted")];

        let s1 = r.cycle(&sink, &file).await;
        assert_eq!(s1.deferred, 0, "a create is never gated");
        assert_eq!(r.mem.get("a").map(String::as_str), Some("vetted"));

        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let s2 = r.cycle(&sink, &file).await;
        assert_eq!(s2.conflicts, 1);
        assert_eq!(r.mem.get("a").map(String::as_str), Some("shipped"));

        let reads_before = *sink.current_status_calls.lock().unwrap();
        let actor_reads_before = *sink.last_actor_calls.lock().unwrap();
        for cycle in 3..7 {
            let s = r.cycle(&sink, &file).await;
            assert_eq!(s.transitions, 0, "cycle {cycle}: the owner is deferred to");
            assert_eq!(s.deferred, 1, "cycle {cycle}");
            assert_eq!(s.conflicts, 0, "cycle {cycle}");
            assert_eq!(r.mem.get("a").map(String::as_str), Some("shipped"));
        }
        assert_eq!(
            *sink.current_status_calls.lock().unwrap() - reads_before,
            4,
            "one `current_status` per deferred cycle"
        );
        assert_eq!(
            *sink.last_actor_calls.lock().unwrap() - actor_reads_before,
            4,
            "...plus one `last_actor` — TWO reads, not one"
        );
        assert_eq!(*sink.transition_attempts.lock().unwrap(), 0);
        assert_eq!(
            logs.text().matches("diverged from last-applied").count(),
            1,
            "the divergence WARN fires once per EVENT; got: {}",
            logs.text()
        );
        assert_eq!(r.metrics.snapshot().conflicts_total, 1);
    }

    /// **A status the adapter never sent must never be recorded as applied**
    /// — and on main's cold-start seed, the derived-status path converges to
    /// ZERO reads per cycle rather than re-seeding forever.
    ///
    /// coord holds nothing for the slug at first, so cycle 1 is a create whose
    /// `shipped` is WITHDRAWN (metadata-only upsert, nothing applied, nothing
    /// recorded). coord then derives its own status out of band; cycle 2
    /// seeds it, emits the edge onto `shipped`, and coord refuses that
    /// transition `permanent` — retiring the pair. Every later cycle is one
    /// status-less upsert and nothing else.
    ///
    /// Recording the parsed `shipped` anywhere in that sequence would arm the
    /// conflict check (a `current_status` GET per cycle) and make it announce
    /// `file wins (loud override)` forever, for a race this writer withdrew
    /// from.
    #[tokio::test]
    async fn a_withdrawn_derived_status_is_never_recorded_and_arms_no_conflict_check() {
        let logs = CapturedLogs::start();
        let sink = FakeSink {
            deny_status: Some(("shipped".to_string(), 422, r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#.to_string())),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        let file = [unit("a", "shipped")];

        let s1 = r.cycle(&sink, &file).await;
        assert_eq!(s1.errors, 0, "the metadata-only create lands");
        assert!(
            !r.mem.contains_key("a"),
            "the filtered status never went on the wire, so it is not last-applied"
        );

        // coord derives a status of its own for the unit.
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "in_progress".to_string());
        let s2 = r.cycle(&sink, &file).await;
        assert_eq!(
            s2.retired_permanent, 1,
            "the edge onto `shipped` is refused"
        );
        let reads = *sink.current_status_calls.lock().unwrap();
        assert_eq!(
            reads, 2,
            "one cold-start seed per cycle so far, nothing more"
        );

        for cycle in 3..6 {
            let s = r.cycle(&sink, &file).await;
            assert_eq!(s.errors, 0, "cycle {cycle}");
            assert_eq!(s.conflicts, 0, "cycle {cycle}");
        }
        assert!(
            !r.mem.contains_key("a"),
            "still nothing recorded: {:?}",
            r.mem
        );
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            reads,
            "a retired pair costs no GET per cycle"
        );
        assert_eq!(
            *sink.status_upsert_calls.lock().unwrap(),
            0,
            "no cycle ever put the derived word on an upsert"
        );
        assert_eq!(r.metrics.snapshot().conflicts_total, 0);
        assert!(
            !logs.text().contains("diverged from last-applied"),
            "the adapter must not report itself the winner of a race it withdrew from: {}",
            logs.text()
        );
    }

    /// **A transition onto a coord-DERIVED status is not deferred to an
    /// owner.** No identity can set `shipped`, so the transition cannot
    /// overwrite what the owner holds — coord refuses it `permanent` and the
    /// pair retires. Deferring instead re-derived the same edge every cycle and
    /// paid a `last_actor` GET plus a `current_status` GET for it, forever if
    /// coord's predicate never holds.
    ///
    /// Neuter check: drop the `is_coord_derived_status` fall-through in
    /// `push_work_unit_with_status_write`'s deferral — cycles 2..4 defer and
    /// the read counters climb.
    #[tokio::test]
    async fn a_derived_transition_on_an_owned_unit_is_retired_not_deferred_forever() {
        let sink = FakeSink {
            last_actor: Some("coord::derive_worker".to_string()),
            deny_status: Some(("shipped".to_string(), 422, r#"{"error":"status_is_derived","message":"status `shipped` is derived (coord-computed from a predicate), not directly settable","terminality":"permanent"}"#.to_string())),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "in_progress".to_string());
        let mut r = Reconciler::new();
        let file = [unit("a", "shipped")];

        let s1 = r.cycle(&sink, &file).await;
        assert_eq!(s1.deferred, 0, "a write nobody can make protects nothing");
        assert_eq!(
            s1.retired_permanent, 1,
            "coord refused it and the pair retired"
        );
        assert_eq!(*sink.transition_attempts.lock().unwrap(), 1);
        let reads = *sink.current_status_calls.lock().unwrap();
        let actor_reads = *sink.last_actor_calls.lock().unwrap();

        for cycle in 2..5 {
            let s = r.cycle(&sink, &file).await;
            assert_eq!(s.deferred, 0, "cycle {cycle}");
            assert_eq!(s.errors, 0, "cycle {cycle}");
        }
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            reads,
            "no GET per cycle"
        );
        assert_eq!(
            *sink.last_actor_calls.lock().unwrap(),
            actor_reads,
            "...of either kind"
        );
        assert_eq!(
            *sink.transition_attempts.lock().unwrap(),
            1,
            "refused once, never re-sent"
        );
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("in_progress"),
            "the owner's status is untouched"
        );
    }

    /// The retirement stays NARROW: `state_dependent` is terminal only until
    /// the unit's own recorded state changes out of band, so retiring on it
    /// would suppress a denial a later cycle could legitimately clear. Served
    /// at coord's REAL `(status, body)` pair — `owner_unresolved` is a 403.
    #[tokio::test]
    async fn a_state_dependent_terminality_is_not_retired() {
        let sink = FakeSink {
            upsert_write_status: Some(403),
            upsert_write_body: Some(
                r#"{"error":"owner_unresolved","message":"the unit has no recorded owner","terminality":"state_dependent"}"#
                    .to_string(),
            ),
            ..Default::default()
        };
        let mut r = Reconciler::new();
        for cycle in 0..3 {
            let s = r.cycle(&sink, &[unit("a", "vetted")]).await;
            assert_eq!(s.errors, 1, "cycle {cycle}");
            assert_eq!(s.forbidden, 0, "cycle {cycle}");
        }
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 3);
        assert!(
            r.forb.is_empty(),
            "a 403 coord gave a clearing condition for must not retire anything"
        );
    }

    /// `actor_dependent` likewise, and so does a hint this build does not
    /// recognise — a future coord word is UNKNOWN, and UNKNOWN retries. Each
    /// case is served at the `(status, body)` pair coord actually emits.
    #[tokio::test]
    async fn actor_dependent_and_unrecognized_terminalities_are_not_retired() {
        for (http, body) in [
            (
                403,
                r#"{"error":"self_attestation_forbidden","terminality":"actor_dependent"}"#,
            ),
            (
                403,
                r#"{"error":"attester_unresolved","terminality":"actor_dependent"}"#,
            ),
            (
                422,
                r#"{"error":"something_new","terminality":"a_word_coord_adds_in_2027"}"#,
            ),
            (422, r#"{"error":"status_is_derived"}"#),
        ] {
            let sink = FakeSink {
                upsert_write_status: Some(http),
                upsert_write_body: Some(body.to_string()),
                ..Default::default()
            };
            let mut r = Reconciler::new();
            for _ in 0..2 {
                let s = r.cycle(&sink, &[unit("a", "vetted")]).await;
                assert_eq!(s.errors, 1, "{http} body={body}");
                assert_eq!(s.forbidden, 0, "{http} body={body}");
                assert_eq!(s.retired_permanent, 0, "{http} body={body}");
            }
            assert_eq!(*sink.upsert_calls.lock().unwrap(), 2, "{http} body={body}");
            assert!(r.forb.is_empty(), "{http} body={body}");
        }
    }

    /// The retirement is narrow: an ORDINARY failure still retries every cycle.
    /// Widening it would turn a coord blip into a silently frozen work-unit
    /// layer, which is the failure the retry loop exists to prevent.
    #[tokio::test]
    async fn a_non_403_failure_is_still_retried_every_cycle() {
        let sink = FakeSink {
            upsert_errors: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();

        for _ in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.errors, 1);
            assert_eq!(s.forbidden, 0);
        }
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 3);
        assert!(
            forb.is_empty(),
            "a transient failure must not retire a slug"
        );
    }

    /// A 403 on the dep-edge route ALONE must retire only the edge push, not
    /// the whole unit: coord evaluates `POST .../upsert` and
    /// `POST .../:slug/deps` as separate authorization checks (the deps route
    /// mutates a different table), so a unit can be permitted on one and
    /// refused on the other. Before this, the deps error path had no 403
    /// classification at all, so a refused edge push would re-issue the
    /// identical request — and re-WARN — every cycle forever, reproducing the
    /// exact log-flood shape the unit-level fix (`a_403_retires_the_slug...`)
    /// closed for the upsert/transition route.
    #[tokio::test]
    async fn a_403_on_deps_retires_only_the_edge_push() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::Forbidden,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb = RetiredSlugs::default();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        for cycle in 0..5 {
            let s = reconcile_once(
                std::slice::from_ref(&u),
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.deps_forbidden, 1, "cycle {cycle} still counts the skip");
            assert_eq!(s.deps_errors, 0, "cycle {cycle} must not re-error");
            // The unit's own upsert route is unaffected by the deps-route
            // refusal: no `forbidden` (unit-level) skip.
            assert_eq!(
                s.forbidden, 0,
                "cycle {cycle} must not retire the unit itself"
            );
        }

        assert_eq!(
            sink.deps_calls.lock().unwrap().len(),
            1,
            "the refused edge push must be asked exactly once, not once per cycle"
        );
        // Unlike the edge-triggered deps/transition routes, `upsert` is NOT
        // gated on a change (see `UpsertWithStatus is never gated` elsewhere in
        // this file) — it runs every cycle regardless. The point of this
        // assertion is not "once": it is "still 5, not fewer" — proving the
        // deps-route refusal above did not also, incorrectly, retire or skip
        // the unit's own upsert route.
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            5,
            "the unit upsert is unaffected by the deps-route refusal and keeps running every cycle"
        );
        assert_eq!(
            metrics.snapshot().deps_forbidden_total,
            1,
            "one increment per refused slug — and so one WARN per slug per process"
        );
        assert_eq!(metrics.snapshot().deps_errors_total, 0);
        assert_eq!(metrics.snapshot().forbidden_total, 0);
        assert!(
            forb.is_empty(),
            "only the deps route is retired, not the unit"
        );
    }

    #[test]
    fn metrics_snapshot_reads_counters() {
        let m = AdapterMetrics::default();
        m.scanned.store(5, Ordering::Relaxed);
        m.transitions_total.store(2, Ordering::Relaxed);
        let snap = m.snapshot();
        assert_eq!(snap.scanned, 5);
        assert_eq!(snap.transitions_total, 2);
    }

    // ---- Phase 4: metadata-only archive scan (D4) ----

    /// Write `body` to `<dir>/<slug>.md` and return the full path string.
    fn write_plan(dir: &Path, slug: &str, body: &str) -> String {
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().to_string()
    }

    // ---- `source_path` is repo-relative, never the authoring machine's ----

    /// THE REGRESSION TEST. A scan run from an ABSOLUTE root must record a
    /// repo-relative `source_path`. Coord serves this field to every machine
    /// in the fleet as the only pointer from a work unit back to its plan
    /// document, so an authoring-machine absolute path resolves nowhere else
    /// — and resolving nowhere is indistinguishable from the plan not
    /// existing. Measured 2026-09-06: 1177 of 2648 served rows were in that
    /// state.
    ///
    /// The planted `.git` is what makes the expectation deterministic: it
    /// pins which ancestor `derive_source_repo` stops at, so the temp dir's
    /// random name can never leak into the recorded path.
    #[test]
    fn read_plan_dir_records_a_repo_relative_source_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plans = repo.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "s", "# S\n\n> **Status:** draft\n");

        // The loop hands `read_plan_dir` an absolute dir; reproduce that.
        assert!(
            plans.is_absolute(),
            "the scan root under test must be absolute"
        );

        let scanned = read_plan_dir(&plans, &PlanConvention::operator_default());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].source_path, "myrepo/plans/s.md");
        assert_eq!(scanned[0].slug, "s");
    }

    /// The no-`.git` arm — the shape `D:\qontinui-root\plans` has on the
    /// operator box, where the workspace root is not a repository.
    /// `derive_source_repo` falls back to the last two components, so the
    /// recorded path is still relative and still names its scan root.
    #[test]
    fn read_plan_dir_source_path_is_relative_without_a_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        // Premise: the temp dir is not itself inside a git work tree. Assert
        // it, so a machine where that is false says so instead of failing on
        // an expectation that was never the point.
        assert!(
            !tmp.path().ancestors().any(|a| a.join(".git").exists()),
            "temp dir is inside a git work tree; this test's premise does not hold"
        );
        let plans = tmp.path().join("qontinui-root").join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "s", "# S\n");

        let scanned = read_plan_dir(&plans, &PlanConvention::operator_default());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].source_path, "qontinui-root/plans/s.md");
    }

    /// Every scanned entry gets the SAME root prefix and its OWN file name —
    /// the cardinality arm the single-file tests above cannot reach.
    #[test]
    fn read_plan_dir_relative_source_path_holds_for_many_files() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plans = repo.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "a", "# A\n");
        write_plan(&plans, "b", "# B\n");
        write_plan(&plans, "c", "# C\n");

        let mut got: Vec<String> = read_plan_dir(&plans, &PlanConvention::operator_default())
            .into_iter()
            .map(|u| u.source_path)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "myrepo/plans/a.md".to_string(),
                "myrepo/plans/b.md".to_string(),
                "myrepo/plans/c.md".to_string(),
            ]
        );
    }

    /// The load-bearing D4 test: an archive scan of real `*.md` files — one
    /// whose `> **Status:` says the coord-derived `shipped`, one whose status is
    /// the non-vocabulary `archived` (which coord silently classifies `Free` and
    /// would ACCEPT) — produces ZERO status transitions, only a
    /// `metadata.archive_path` stamp per slug pointing at the archived file.
    #[tokio::test]
    async fn archive_scan_stamps_path_and_never_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        // A planted `.git` pins which ancestor `derive_source_repo` stops at,
        // so `archive_path` below is a LITERAL rather than a re-derivation of
        // the temp dir's random name.
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let archive = repo.join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        write_plan(
            &archive,
            "2026-01-01-shipped-plan",
            "# Shipped Plan\n\n> **Status:** shipped 2026-01-01.\n",
        );
        write_plan(
            &archive,
            "2026-01-02-archived-plan",
            "# Archived Plan\n\n> **Status:** archived\n",
        );

        // Reuse the production scan path — its missing-dir-yields-empty-vec
        // behavior is exactly the right unset semantics.
        let conv = PlanConvention::operator_default();
        let scanned = read_plan_dir(&archive, &conv);
        assert_eq!(scanned.len(), 2);

        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let summary = reconcile_archive_once(&scanned, &sink, &metrics).await;

        // ZERO transitions from ANY archive-scanned entry — the only D4 guard,
        // since coord will not reject either `shipped` or `archived` here.
        assert_eq!(
            *sink.transitions.lock().unwrap(),
            0,
            "archive scan must NEVER emit a status transition"
        );
        assert_eq!(summary.stamped, 2);
        assert_eq!(summary.errors, 0);
        assert_eq!(metrics.snapshot().archive_stamped_total, 2);
        assert_eq!(metrics.snapshot().transitions_total, 0);

        // Exactly one metadata-only upsert per slug: no status, archive_path set
        // to the archived file's path.
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 2);
        for up in ups.iter() {
            assert!(up.status.is_none(), "archive upsert carries no status");
        }
        let by_slug: HashMap<&str, &UpsertBody> =
            ups.iter().map(|u| (u.slug.as_str(), u)).collect();
        // The archive writer carries the slug-derived authoring date too, so
        // an archived-only plan is dated (harmless under coord's COALESCE).
        assert_eq!(
            by_slug["2026-01-01-shipped-plan"].authored_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            by_slug["2026-01-02-archived-plan"].authored_at.as_deref(),
            Some("2026-01-02T00:00:00Z")
        );
        // `archive_path` is the same string as `source_path` under another
        // name, so it is repo-relative too. Until 2026-09-06 this assertion
        // compared against the ABSOLUTE path `write_plan` returned — a test
        // pinning the defect, which reddened the moment the defect was fixed
        // [policy: a-test-must-be-able-to-fail, shape 2].
        assert_eq!(
            by_slug["2026-01-01-shipped-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!("myrepo/archive/2026-01-01-shipped-plan.md")
        );
        assert_eq!(
            by_slug["2026-01-02-archived-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!("myrepo/archive/2026-01-02-archived-plan.md")
        );
    }

    /// A missing/unset archive dir yields an empty scan (no writes) — the same
    /// unset semantics as the active dir.
    #[tokio::test]
    async fn archive_scan_of_missing_dir_is_empty_noop() {
        let conv = PlanConvention::operator_default();
        let scanned = read_plan_dir(Path::new("/definitely/not/a/dir/xyz"), &conv);
        assert!(scanned.is_empty());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let summary = reconcile_archive_once(&scanned, &sink, &metrics).await;
        assert_eq!(summary.scanned, 0);
        assert_eq!(summary.stamped, 0);
        assert!(sink.upserts.lock().unwrap().is_empty());
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    /// Disappeared-slug rule (D4): a slug we SAW that is gone from the active
    /// dir AND absent from the archive dir is surfaced ONCE per process, and the
    /// detection never transitions (it only warns — no sink call at all).
    #[test]
    fn disappeared_slug_warns_once_and_never_transitions() {
        let known: HashSet<String> = ["a", "b", "c"].into_iter().map(String::from).collect();

        // `a` still active, `b` moved to archive, `c` vanished from both.
        let active: HashSet<String> = ["a".to_string()].into_iter().collect();
        let archive: HashSet<String> = ["b".to_string()].into_iter().collect();
        let mut warned: HashSet<String> = HashSet::new();

        let first = newly_disappeared_slugs(&known, &active, &archive, &mut warned);
        assert_eq!(first, vec!["c".to_string()], "only c disappeared");

        // Warn-once: a second identical scan yields nothing new.
        let second = newly_disappeared_slugs(&known, &active, &archive, &mut warned);
        assert!(
            second.is_empty(),
            "a disappeared slug is warned at most once per process"
        );
    }

    // ---- one-shot work-unit backfill (`qontinui-pr plan-workunit-backfill`) --

    /// The catch-up path's core promise: a corpus coord has never seen is
    /// CREATED, and running the same backfill again writes no status and emits
    /// no transition. A backfill that started from an empty last-applied memory
    /// (the naive shape) would take the `UpsertWithStatus` arm on every run and
    /// re-stamp a status every time — this pins the seeded-from-coord behaviour
    /// that makes it idempotent.
    ///
    /// Neuter check: seed `push_work_unit` with `None` instead of
    /// `sink.current_status(...)` in `backfill_work_units_once` and the second
    /// run's assertions fail.
    #[tokio::test]
    async fn backfill_creates_missing_units_then_is_idempotent() {
        let sink = FakeSink::default();
        let units = [unit("a", "draft"), unit("b", "in_progress")];

        let first = backfill_work_units_once(&units, &sink).await;
        assert_eq!(first.scanned, 2);
        assert_eq!(first.created, 2);
        assert_eq!(first.refreshed, 0);
        assert_eq!(first.transitioned, 0);
        assert_eq!(first.deferred, 0);
        assert_eq!(first.failed, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("draft")
        );

        let upserts_after_first = sink.upserts.lock().unwrap().len();

        // Re-run over the unchanged corpus.
        let second = backfill_work_units_once(&units, &sink).await;
        assert_eq!(second.created, 0, "nothing is created twice");
        assert_eq!(second.refreshed, 2);
        assert_eq!(second.transitioned, 0);
        assert_eq!(second.failed, 0);
        assert_eq!(
            *sink.transitions.lock().unwrap(),
            0,
            "an unchanged corpus emits NO transition on re-run"
        );
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), upserts_after_first + 2);
        for u in &ups[upserts_after_first..] {
            assert!(
                u.status.is_none(),
                "the idempotent re-run refreshes metadata only, never a status"
            );
        }
    }

    /// The agent-owner deferral (graduation-bootstrap P2a) must survive the
    /// backfill path — it is what stops a bulk catch-up clobbering a status an
    /// agent set, and it is the direction Phase 3's coord -> body reconcile
    /// depends on. Reachable here ONLY because the seed makes the unit take the
    /// `Transition` arm; a `None` seed would route it through
    /// `UpsertWithStatus`, which the deferral never gates.
    #[tokio::test]
    async fn backfill_defers_when_a_real_agent_owns_the_unit() {
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        // coord already holds `shipped` (an agent drove it there); the stale
        // body on disk still says `in_progress`.
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());

        let s = backfill_work_units_once(&[unit("a", "in_progress")], &sink).await;
        assert_eq!(s.deferred, 1);
        assert_eq!(s.transitioned, 0);
        assert_eq!(s.created, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "the agent-set status is left exactly as it was"
        );
    }

    /// The other side of the deferral: when NO real agent owns the unit (no
    /// history), a genuine disk/coord divergence is still corrected — the
    /// deferral narrows the backfill, it does not disable it.
    #[tokio::test]
    async fn backfill_transitions_an_unowned_diverged_unit() {
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "draft".to_string());

        let s = backfill_work_units_once(&[unit("a", "vetted")], &sink).await;
        assert_eq!(s.transitioned, 1);
        assert_eq!(s.deferred, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("vetted")
        );
    }

    /// A per-unit failure is counted and the pass continues — a one-shot
    /// catch-up over ~1,400 files must not abort on one bad row.
    #[tokio::test]
    async fn backfill_counts_a_failed_unit_and_keeps_going() {
        let sink = FakeSink {
            fail_status_read_for: Some("bad".to_string()),
            ..Default::default()
        };
        let s =
            backfill_work_units_once(&[unit("bad", "draft"), unit("good", "draft")], &sink).await;
        assert_eq!(s.scanned, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.created, 1, "the second unit still landed");
    }

    /// The other failure branch: the seed read succeeds and the WRITE fails.
    #[tokio::test]
    async fn backfill_counts_a_failed_push_and_keeps_going() {
        let sink = FakeSink {
            fail_upsert_for: Some("bad".to_string()),
            ..Default::default()
        };
        let s =
            backfill_work_units_once(&[unit("bad", "draft"), unit("good", "draft")], &sink).await;
        assert_eq!(s.failed, 1);
        assert_eq!(s.created, 1);
    }

    /// A status coord refuses `permanent` is NOT a backfill failure: the
    /// metadata upsert landed before the refused status write, and no retry of
    /// the identical request could succeed. Counting it `failed` made the
    /// one-shot CLI exit 1 for a unit with no fault.
    ///
    /// Neuter check: route the permanent denial back into `failed` and this
    /// fails.
    #[tokio::test]
    async fn backfill_counts_a_permanent_status_refusal_separately_from_failure() {
        let sink = FakeSink {
            last_actor: Some("coord::derive_worker".to_string()),
            deny_status: Some((
                "shipped".to_string(),
                422,
                r#"{"error":"status_is_derived","terminality":"permanent"}"#.to_string(),
            )),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "ready".to_string());
        let s = backfill_work_units_once(&[unit("a", "shipped"), unit("b", "draft")], &sink).await;
        assert_eq!(s.refused_permanently, 1, "the derived stamp was refused");
        assert_eq!(s.failed, 0, "...and that is not a failure");
        assert_eq!(s.created, 1, "the next unit still landed");
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("ready"),
            "coord's own status is untouched"
        );
        // The claim the counter rests on: `a`'s status-less metadata upsert
        // LANDED before the refused transition.
        assert!(
            sink.upserts
                .lock()
                .unwrap()
                .iter()
                .any(|b| b.slug == "a" && b.status.is_none()),
            "the refused unit's metadata must have been refreshed"
        );

        // Negative case: a permanent refusal of the UPSERT means nothing
        // landed, so it stays a failure.
        let upsert_refused = FakeSink {
            upsert_write_status: Some(422),
            upsert_write_body: Some(SYNTHETIC_PERMANENT.to_string()),
            ..Default::default()
        };
        let s = backfill_work_units_once(&[unit("c", "vetted")], &upsert_refused).await;
        assert_eq!(s.failed, 1, "a refused upsert landed nothing: a failure");
        assert_eq!(s.refused_permanently, 0);
    }

    /// Idempotence must not rest on the backend echoing the status it was
    /// handed. Coord classifies and can normalise (`push_work_unit`'s own docs
    /// note `archived` lands as Free and `shipped` is derived), so a sink that
    /// stores something OTHER than what was pushed is the realistic case: the
    /// second run must not churn just because the round-trip is lossy.
    ///
    /// It legitimately transitions ONCE — the stored value really does differ
    /// from the file — and then settles, because the transition path writes the
    /// file's word through. What must never happen is an unbounded re-transition
    /// on every subsequent run.
    #[tokio::test]
    async fn backfill_settles_against_a_normalizing_backend() {
        let sink = FakeSink {
            normalize: Some(("in_progress".to_string(), "in-progress".to_string())),
            ..Default::default()
        };
        let units = [unit("a", "in_progress")];

        let r1 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(r1.created, 1);
        let r2 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(
            r2.transitioned, 1,
            "the lossy round-trip costs one correction"
        );
        let r3 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(
            (r3.transitioned, r3.refreshed),
            (0, 1),
            "and then it SETTLES — no unbounded churn"
        );
    }

    /// `deferred` is not just a count: the units are named, so an operator can
    /// act on them without re-running under RUST_LOG=info.
    #[tokio::test]
    async fn backfill_names_the_units_it_deferred() {
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let s = backfill_work_units_once(&[unit("a", "in_progress")], &sink).await;
        assert_eq!(s.deferred as usize, s.deferred_units.len());
        assert_eq!(s.deferred_units[0].slug, "a");
        assert_eq!(s.deferred_units[0].owner, "device:d:agent:a");
        assert_eq!(s.deferred_units[0].wanted, "in_progress");
    }

    // ---- per-tick path resolution + tier visibility -------------------------

    /// Capture everything logged at `info` and above on THIS thread while the
    /// returned guard lives. Thread-local (`set_default`, not the global
    /// subscriber), so it composes with `#[tokio::test]`'s current-thread
    /// runtime and never leaks into sibling tests.
    struct CapturedLogs {
        buf: std::sync::Arc<Mutex<Vec<u8>>>,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl CapturedLogs {
        fn start() -> Self {
            use std::io::Write;
            use std::sync::Arc;

            #[derive(Clone, Default)]
            struct Writer(Arc<Mutex<Vec<u8>>>);
            impl Write for Writer {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(buf);
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
                type Writer = Writer;
                fn make_writer(&'a self) -> Self::Writer {
                    self.clone()
                }
            }

            let writer = Writer::default();
            let buf = writer.0.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                // Deterministic field rendering: with ANSI on, the formatter
                // wraps field NAMES in escape sequences, so an assertion on a
                // structured field (`terminality="permanent"`) would match or
                // not depending on the build's colour support rather than on
                // the field being emitted.
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            let guard = tracing::subscriber::set_default(subscriber);
            Self { buf, _guard: guard }
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.buf.lock().unwrap_or_else(|e| e.into_inner())).to_string()
        }
    }

    /// A [`PathReader`] whose answer a test can change between ticks — the
    /// stand-in for an operator editing the Paths settings section while the
    /// loop runs.
    fn switchable_paths() -> (std::sync::Arc<Mutex<PathInputs>>, PathReader) {
        let cell = std::sync::Arc::new(Mutex::new(PathInputs::default()));
        let reader: PathReader = {
            let cell = cell.clone();
            std::sync::Arc::new(move || cell.lock().unwrap_or_else(|e| e.into_inner()).clone())
        };
        (cell, reader)
    }

    /// A plans dir holding one parseable plan, so a scan is observable as one
    /// `upsert` on the fake sink.
    fn one_plan_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("2026-01-01-one-plan.md"),
            "# One plan\n\n> **Status: DRAFT**\n\nBody.\n",
        )
        .unwrap();
        tmp
    }

    fn plans_dir_input(dir: &Path) -> PathInputs {
        PathInputs {
            plans_dir: Some(dir.to_string_lossy().to_string()),
            ..PathInputs::default()
        }
    }

    /// A [`LoopState`] whose git reader answers "not in a work tree", so the
    /// scan takes the WorkTree arm DETERMINISTICALLY.
    ///
    /// Since Phase 2 the scan resolves its source through `self.git`, so a
    /// tick over a `tempfile::tempdir()` asks a real `git` where that dir
    /// lives. On this box `/tmp` is outside any repo and the answer is the one
    /// these tests want — but on a machine whose `TMPDIR` sits INSIDE a git
    /// repo the Ref arm is taken, `<ref>:tmp/…` does not resolve, and the tick
    /// returns early: a previously hermetic suite would start failing on a
    /// property of the host. Injecting the answer removes the host from it.
    fn tick_state(reader: PathReader) -> LoopState {
        LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate).with_git(
            std::sync::Arc::new(FakeGit {
                root: Ok(None),
                ..FakeGit::healthy(0, 0)
            }),
        )
    }

    /// The phase's headline acceptance criterion, pinned at the TICK — the
    /// only place it can actually be pinned.
    ///
    /// A machine with no plans dir returns early from `tick`, so the
    /// measurement has to be taken BEFORE that return. Testing
    /// [`record_scan_divergence`] directly cannot see the ordering: move the
    /// measurement block below the early return and every store-level test
    /// still passes while a tier-off device silently reports nothing again.
    ///
    /// Neuter check: move the `record_scan_divergence` call in `tick` after
    /// the `let Some(dir) = … else { return }` and this fails.
    #[tokio::test]
    async fn an_idle_tick_records_not_scanning_rather_than_nothing() {
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(std::sync::Arc::new(FakeGit::healthy(2153, 11)));

        assert_eq!(
            metrics.snapshot().scan_divergence,
            None,
            "before the first tick there is no reading at all"
        );

        state.tick(&sink, &metrics).await;

        let d = metrics
            .snapshot()
            .scan_divergence
            .expect("the IDLE tick must record too — that is the whole criterion");
        assert_eq!(d.state, ScanDivergenceState::NotScanning);
        assert_eq!(
            (d.behind, d.ahead),
            (None, None),
            "a machine that scanned nothing may not report agreement with a ref"
        );
        assert_eq!(metrics.snapshot().cycles_total, 0, "still not a reconcile");
    }

    /// The armed half, and the proof the tick reads its INJECTED git rather
    /// than shelling out: a configured plans dir records the fake's numbers.
    #[tokio::test]
    async fn an_armed_tick_records_the_measurement_from_its_git_reader() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(std::sync::Arc::new(FakeGit::healthy(2153, 11)));

        state.tick(&sink, &metrics).await;

        let d = metrics
            .snapshot()
            .scan_divergence
            .expect("an armed tick records a reading");
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!((d.behind, d.ahead), (Some(2153), Some(11)));
        assert!(d.is_stale());
        assert!(
            d.ref_age_secs.is_some(),
            "the tick hands the measurement a clock, so an answered age probe yields an age"
        );
        assert_eq!(
            d.plans_dir.as_deref(),
            Some(dir.path().display().to_string().as_str()),
            "the reading names the dir that was actually scanned"
        );
    }

    /// The observability half: a machine with NO plans dir must SAY the
    /// markdown-plan tier is off, at `info`, naming the setting that arms it
    /// and the restart-free catch-up path. A silent idle is indistinguishable
    /// from a healthy scan — the defect that let a fleet-wide ingestion gap
    /// run unreported.
    ///
    /// Neuter check: drop the `None` arm's log in `apply_resolution` and this
    /// fails.
    #[tokio::test]
    async fn tier_off_machine_says_so_at_info() {
        let logs = CapturedLogs::start();
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            0,
            "an unarmed tier scans nothing"
        );
        assert_eq!(
            metrics.snapshot().cycles_total,
            0,
            "an idle tick is not a reconcile cycle"
        );
        let logged = logs.text();
        assert!(
            logged.contains("markdown-plan tier is OFF"),
            "the tier-off line must be emitted; got: {logged}"
        );
        assert!(
            logged.contains("paths.plans_dir"),
            "it must name the setting that arms the tier; got: {logged}"
        );
        assert!(
            logged.contains("plan-workunit-backfill"),
            "it must name the restart-free catch-up path; got: {logged}"
        );
        assert!(
            logged.contains("INFO"),
            "the line must be `info`, not `debug` — a debug line is invisible \
             at the fleet's default filter; got: {logged}"
        );
    }

    /// THE property Phase 4 exists for: a plans dir configured AFTER the loop
    /// started is scanned on the very next tick — no restart, no re-spawn.
    /// The shipped code baked the dir into `run_loop`'s arguments at spawn, so
    /// an edit looked inert until the next runner start.
    #[tokio::test]
    async fn a_plans_dir_configured_after_spawn_is_scanned_on_the_next_tick() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        // Tick 1: nothing configured — idle, and the gauges say so.
        state.tick(&sink, &metrics).await;
        let snap = metrics.snapshot();
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 0);
        assert_eq!(snap.active_plans_dir, None);
        assert_eq!(snap.scan_roots, 0);
        assert_eq!(
            snap.path_resolutions_total, 1,
            "the first tick resolves once"
        );

        // The operator saves the Paths section…
        *cell.lock().unwrap() = plans_dir_input(dir.path());

        // …and tick 2 scans it.
        state.tick(&sink, &metrics).await;
        let upserts = sink.upserts.lock().unwrap();
        assert_eq!(upserts.len(), 1, "the newly configured dir must be scanned");
        assert_eq!(upserts[0].slug, "2026-01-01-one-plan");
        drop(upserts);
        let snap = metrics.snapshot();
        assert_eq!(
            snap.active_plans_dir.as_deref(),
            Some(dir.path().to_string_lossy().as_ref())
        );
        assert_eq!(snap.scan_roots, 1);
        assert_eq!(snap.cycles_total, 1);
        assert_eq!(
            snap.path_resolutions_total, 2,
            "the change was picked up as one re-resolution"
        );

        // Tick 3, unchanged: no re-resolution, no phantom transition.
        state.tick(&sink, &metrics).await;
        assert_eq!(metrics.snapshot().path_resolutions_total, 2);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        let logged = logs.text();
        assert_eq!(
            logged.matches("markdown-plan tier is OFF").count(),
            1,
            "got: {logged}"
        );
        assert_eq!(
            logged.matches("markdown-plan tier is ON").count(),
            1,
            "got: {logged}"
        );
    }

    /// Plan `2026-09-17-plan-adapter-mints-work-units-under-the-default-binding-of-a-multi-bound-device`
    /// Phase 2, at the TICK: a device coord reports as multi-bound — here the
    /// hazardous shape, ONE local slot and three coord bindings — makes NO coord
    /// work-unit call: no bulk seed, no per-slug read, no upsert, no deps, and no
    /// archive stamp even with an archive dir configured. The cycle is still
    /// counted. The same corpus on a single-bound device pushes exactly as
    /// before. Removing or moving the gate reddens the first half.
    #[tokio::test]
    async fn a_multi_bound_device_withholds_every_work_unit_call() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let archive = tempfile::tempdir().unwrap();
        std::fs::write(
            archive.path().join("2025-01-01-archived-plan.md"),
            "# Archived plan\n\n> **Status: DRAFT**\n\nBody.\n",
        )
        .unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = PathInputs {
            plans_archive_dir: Some(archive.path().to_string_lossy().to_string()),
            ..plans_dir_input(dir.path())
        };
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader.clone()).with_binding_count(1, Some(3));

        state.tick(&sink, &metrics).await;
        state.tick(&sink, &metrics).await;
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            0,
            "no upsert, archive stamp included"
        );
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            0,
            "no per-slug read"
        );
        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            0,
            "no bulk seed read"
        );
        assert!(sink.deps_calls.lock().unwrap().is_empty(), "no deps call");
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        let snap = metrics.snapshot();
        assert_eq!(snap.work_unit_writes_withheld_total, 2);
        assert_eq!(snap.work_unit_writes_withheld_unknown_total, 0);
        assert_eq!(snap.cycles_total, 2, "a withheld cycle is still a cycle");
        let logged = logs.text();
        assert_eq!(
            logged.matches("bound to 3 tenants").count(),
            1,
            "a standing posture logs once: {logged}"
        );

        // Same corpus, single-bound device: today's push, archive stamp included.
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader).with_binding_count(1, Some(1));
        state.tick(&sink, &metrics).await;
        assert_eq!(
            sink.upserts.lock().unwrap().len(),
            2,
            "the active plan and the archive stamp both push when single-bound"
        );
        assert_eq!(metrics.snapshot().work_unit_writes_withheld_total, 0);
    }

    /// **A WITHHELD cycle still LISTS the ref, and still reports both stem
    /// censuses.**
    ///
    /// The posture withholds coord work-unit reads and writes. A `git ls-tree`
    /// is neither, and the cost objection that justifies skipping the SCAN —
    /// ~1,100 blob reads to discard — does not reach a listing that reads no
    /// blob at all.
    ///
    /// Withholding the census with them made it permanently dark on exactly
    /// the device that holds BOTH sides of the coverage question: a posture is
    /// a STANDING property (here the operator box's own shape — two local
    /// bindings, no coord record — which is `WithheldMultiBound` on every tick
    /// forever), so "not this cycle" meant "not ever". Worse, the first such
    /// cycle CLEARED the stems the web already held, because a report whose
    /// `censuses` omits a source stores that source as NULL.
    ///
    /// The fake's `fetch` is `Err`: the census reads the tracking ref this
    /// clone already holds, so a failing fetch must not reach it — which also
    /// proves no fetch sits on this path.
    ///
    /// Neuter check: hand `run_cycle` a `None` ref census on the withheld arm
    /// (main's behaviour) and the REF assertion below fails.
    #[tokio::test]
    async fn a_withheld_cycle_still_lists_the_ref_stem_census() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(owns_the_machine())
        .with_scan_reporter(reporter.clone())
        .with_binding_count(2, None)
        .with_git(std::sync::Arc::new(FakeGit {
            root: Ok(Some(dir.path().to_path_buf())),
            fetch: Err("a census-only listing must never fetch".to_string()),
            ref_dir: Ok(vec![
                // Out of order, and with a non-plan entry, so the census's own
                // predicate and sort are what produce the answer.
                RefDirEntry {
                    name: "2026-01-02-two.md".to_string(),
                    id: "blob-two".to_string(),
                },
                RefDirEntry {
                    name: "2026-01-01-one.md".to_string(),
                    id: "blob-one".to_string(),
                },
                RefDirEntry {
                    name: "README.txt".to_string(),
                    id: "blob-readme".to_string(),
                },
            ]),
            ..FakeGit::healthy(3, 0)
        }));

        state.tick(&sink, &metrics).await;

        // Still withheld: not one coord work-unit call, on any route.
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 0, "no upsert");
        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            0,
            "no bulk seed read"
        );
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            0,
            "no per-slug read"
        );
        assert!(sink.deps_calls.lock().unwrap().is_empty(), "no deps call");
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(metrics.snapshot().work_unit_writes_withheld_total, 1);

        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "the withheld cycle still reports");
        let censuses = sent[0]
            .censuses
            .clone()
            .expect("a withheld cycle enumerated both sides, so it carries censuses");
        let ref_census = censuses
            .iter()
            .find(|c| c.source == super::super::body_push::SLUG_CENSUS_SOURCE_REF)
            .expect("the REF side is a READING on a withheld cycle, not ABSENT");
        assert_eq!(
            ref_census.slugs,
            Some(vec![
                "2026-01-01-one".to_string(),
                "2026-01-02-two".to_string()
            ]),
            "the `*.md` entries at the ref, sorted — and nothing else"
        );
        assert_eq!(
            ref_census.ref_sha,
            Some("a".repeat(40)),
            "listed AT the resolved object id, and saying so"
        );
        assert!(
            censuses
                .iter()
                .any(|c| c.source == super::super::body_push::SLUG_CENSUS_SOURCE_WORK_TREE),
            "and the work-tree side travels with it: {censuses:?}"
        );
    }

    /// An UNKNOWN coord binding set does not fall open to the local slot count
    /// (one, on the device this gate exists for): the tick withholds.
    #[tokio::test]
    async fn an_unknown_coord_binding_set_withholds_rather_than_falling_open() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let logs = CapturedLogs::start();
        let mut state = tick_state(reader).with_binding_count(1, None);
        state.tick(&sink, &metrics).await;
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 0);
        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 0);
        let snap = metrics.snapshot();
        assert_eq!(snap.work_unit_writes_withheld_total, 1);
        assert_eq!(snap.work_unit_writes_withheld_unknown_total, 1);
        assert!(
            !logs.text().contains("is UNKNOWN"),
            "a first unknown cycle is expected on boot and must not warn"
        );
        state.tick(&sink, &metrics).await;
        state.tick(&sink, &metrics).await;
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 0);
        assert_eq!(
            logs.text().matches("is UNKNOWN").count(),
            1,
            "a standing unknown warns exactly once"
        );
    }

    #[test]
    fn work_unit_write_posture_reads_coords_figure_and_never_falls_open() {
        let r = |local, coord| BindingCountReading { local, coord };
        assert_eq!(
            work_unit_write_posture(r(1, Some(1))),
            WorkUnitWritePosture::Write
        );
        assert_eq!(
            work_unit_write_posture(r(1, Some(0))),
            WorkUnitWritePosture::Write
        );
        assert_eq!(
            work_unit_write_posture(r(1, Some(3))),
            WorkUnitWritePosture::WithheldMultiBound(3),
            "one credential slot must not read as single-bound when coord says three"
        );
        assert_eq!(
            work_unit_write_posture(r(2, Some(1))),
            WorkUnitWritePosture::WithheldMultiBound(2),
            "never fewer than the local slots prove"
        );
        assert_eq!(
            work_unit_write_posture(r(2, None)),
            WorkUnitWritePosture::WithheldMultiBound(2)
        );
        assert_eq!(
            work_unit_write_posture(r(1, None)),
            WorkUnitWritePosture::WithheldBindingsUnknown
        );
        // Messages: first-cycle Write is silent; each withheld posture is
        // announced once; recovery is announced.
        let w = WorkUnitWritePosture::Write;
        let h = WorkUnitWritePosture::WithheldMultiBound(3);
        let u = WorkUnitWritePosture::WithheldBindingsUnknown;
        assert!(work_unit_write_posture_message(None, w).is_none());
        assert!(work_unit_write_posture_message(None, h).is_some());
        assert!(work_unit_write_posture_message(None, u)
            .unwrap()
            .contains("UNKNOWN"));
        assert!(work_unit_write_posture_message(Some(h), h).is_none());
        assert!(work_unit_write_posture_message(Some(u), h).is_some());
        assert!(work_unit_write_posture_message(Some(h), w).is_some());
    }

    /// Writes RESUMING after a withheld posture re-prime the edge memory from
    /// coord: the resumed cycle's FIRST coord call is the bulk `list_statuses`
    /// read, and it primes again. Follow-up to #1565 — the withheld cycles
    /// themselves make no coord call of any kind (asserted here through the
    /// call ledger, not per-method counters), but `bulk_seeded` was already
    /// armed before the withhold, so without the re-arm the resumed cycle
    /// would run on memory frozen at the last Write cycle.
    ///
    /// Neuter check: drop `self.bulk_seeded = false;` from the flip arm in
    /// `tick` and the resumed cycle's ledger starts with `current_status`
    /// (or, with `last_applied` still populated, with `upsert`).
    #[tokio::test]
    async fn writes_resuming_after_a_withheld_posture_re_prime_from_the_bulk_seed() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink {
            bulk: Some(
                [("2026-01-01-one-plan".to_string(), "draft".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let bindings = std::sync::Arc::new(std::sync::Mutex::new(BindingCountReading {
            local: 1,
            coord: Some(1),
        }));
        let mut state = tick_state(reader).with_binding_count_cell(bindings.clone());

        // Write cycle: bulk seed primes the one slug, then a refresh.
        state.tick(&sink, &metrics).await;
        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().seeded_total, 1);
        let after_write = sink.ledger();
        assert_eq!(after_write.first().copied(), Some("list_statuses"));

        // Two withheld cycles: coord reports three bindings. No call at all.
        bindings.lock().unwrap().coord = Some(3);
        state.tick(&sink, &metrics).await;
        state.tick(&sink, &metrics).await;
        assert_eq!(
            sink.ledger(),
            after_write,
            "a withheld cycle makes no coord work-unit call of any kind"
        );
        assert_eq!(metrics.snapshot().work_unit_writes_withheld_total, 2);

        // Flip back to single-bound: the resumed cycle re-primes FIRST.
        bindings.lock().unwrap().coord = Some(1);
        state.tick(&sink, &metrics).await;
        let resumed: Vec<&str> = sink.ledger()[after_write.len()..].to_vec();
        assert_eq!(
            resumed.first().copied(),
            Some("list_statuses"),
            "the resumed cycle's first coord call is the bulk seed: {resumed:?}"
        );
        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            2,
            "the bulk seed re-armed on the flip"
        );
        assert_eq!(
            metrics.snapshot().seeded_total,
            2,
            "the re-primed memory came from coord, not from the frozen last Write cycle"
        );
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    /// The discriminating case for the re-prime. While writes were withheld
    /// an agent moved the unit in coord (`draft` -> `vetted`) AND the plan
    /// file was edited to VETTED — the file and coord AGREE. On the flip the
    /// bulk read fails, so the memory must be re-primed per slug: the resumed
    /// cycle reads `current_status` before any push, sees `vetted`, and
    /// refreshes. With the memory frozen at `draft` the same cycle would decide
    /// a `draft -> vetted` transition, read coord's `vetted` as a divergence
    /// from `draft`, warn "file wins (loud override)" and emit a
    /// `transition` with the CAS `from_status` guard dropped — for a status
    /// the file never contradicted.
    ///
    /// Neuter check: drop `self.last_applied.clear();` from the flip arm in
    /// `tick` and this fails with one `transition` recorded.
    #[tokio::test]
    async fn writes_resuming_after_a_withheld_posture_do_not_transition_on_frozen_memory() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        // The bulk door answers (empty) on the Write cycle, so `bulk_seeded`
        // is ARMED before the withhold — the shape the defect needs.
        let sink = FakeSink {
            bulk: Some(HashMap::new()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let bindings = std::sync::Arc::new(std::sync::Mutex::new(BindingCountReading {
            local: 1,
            coord: Some(1),
        }));
        let mut state = tick_state(reader).with_binding_count_cell(bindings.clone());

        // Write cycle: the unit is created as `draft` and remembered as such.
        state.tick(&sink, &metrics).await;
        assert_eq!(
            state
                .last_applied
                .get("2026-01-01-one-plan")
                .map(String::as_str),
            Some("draft")
        );
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // Withheld cycle, during which a session vets the unit in coord and
        // the plan file is edited to match.
        bindings.lock().unwrap().coord = Some(3);
        state.tick(&sink, &metrics).await;
        sink.statuses
            .lock()
            .unwrap()
            .insert("2026-01-01-one-plan".to_string(), "vetted".to_string());
        std::fs::write(
            dir.path().join("2026-01-01-one-plan.md"),
            "# One plan

> **Status: VETTED**

Body.
",
        )
        .unwrap();
        let before_flip = sink.ledger();

        // Flip back with the bulk read FAILING for this cycle.
        bindings.lock().unwrap().coord = Some(1);
        sink.fail_list_statuses.store(true, Ordering::Relaxed);
        state.tick(&sink, &metrics).await;

        let resumed: Vec<&str> = sink.ledger()[before_flip.len()..].to_vec();
        let first_read = resumed.iter().position(|m| *m == "current_status");
        let first_push = resumed
            .iter()
            .position(|m| *m == "upsert" || *m == "transition");
        assert_eq!(
            resumed.first().copied(),
            Some("list_statuses"),
            "the bulk seed is attempted first: {resumed:?}"
        );
        assert!(
            matches!((first_read, first_push), (Some(r), Some(p)) if r < p),
            "the per-slug seed must read coord before any push: {resumed:?}"
        );
        assert!(
            !resumed.contains(&"transition"),
            "file and coord agree on `vetted`; nothing to transition: {resumed:?}"
        );
        assert_eq!(
            state
                .last_applied
                .get("2026-01-01-one-plan")
                .map(String::as_str),
            Some("vetted"),
            "the memory was re-primed from coord"
        );
        assert_eq!(
            metrics.snapshot().seeded_total,
            1,
            "the per-slug seed fired because the memory was empty"
        );
        assert!(
            !logs.text().contains("loud override"),
            "no spurious conflict on a status the file never contradicted: {}",
            logs.text()
        );
    }

    /// **The no-fallback contract at the TICK** — the level the question is
    /// actually asked at: does the RUNNING LOOP publish nothing?
    ///
    /// A readable plan sits in the dir and the fetch fails. Three assertions,
    /// because they are one behaviour:
    ///
    /// - **nothing is published.** `upsert_calls == 0` while a parseable plan
    ///   is right there. This is what `read_plans_for_cycle`'s unit test
    ///   cannot reach, because the call site is where `&ProcessGit` used to be
    ///   hardcoded past the injected reader.
    /// - **the cycle still COUNTS.** A frozen `cycles_total` reads as "the
    ///   loop is dead", which is a different and much louder claim than "the
    ///   loop is failing".
    /// - **the scan-root report still goes out.** A cycle that publishes
    ///   nothing is the one that MOST needs to say so, or the read side keeps
    ///   quoting its last `measured` row until it ages out — the property the
    ///   scan-root report is deliberately sequenced ahead of the breaker pause
    ///   and the `artifacts.is_empty()` return for.
    ///
    /// Neuter check: delete the `report_while_idle` call in the `Ok(Err(..))`
    /// arm and the report assertion fails; delete the `fetch_add` and the
    /// cycle assertion fails.
    #[tokio::test]
    async fn an_unavailable_scan_source_publishes_nothing_but_still_reports() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            // A sink is what makes the loop BUILD a body sync at all; the
            // recording reporter below is what it actually reports through, so
            // this address is never dialled.
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_scan_report_gate(std::sync::Arc::new(|| true) as ScanReportGate)
        .with_scan_reporter(reporter.clone())
        .with_git(std::sync::Arc::new(FakeGit {
            root: Ok(Some(dir.path().to_path_buf())),
            fetch: Err("could not reach origin".to_string()),
            ..FakeGit::healthy(0, 0)
        }));

        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            0,
            "a parseable plan is in the dir and must NOT be published from it"
        );
        assert_eq!(
            metrics.snapshot().cycles_total,
            1,
            "a cycle that publishes nothing is still a cycle"
        );
        assert!(
            !reporter.states().is_empty(),
            "the cycle that refreshes nothing is the one that must still report"
        );
    }

    /// One unchanging fault costs ONE warn, not one per minute.
    ///
    /// A clone with no `origin/HEAD`, or a plans dir absent from the ref, is a
    /// permanent configuration — and this module already strips pids from
    /// probe text specifically so such a fault does not produce a WARN and a
    /// scan-root POST every 60 s. The new publish-nothing arm must hold to the
    /// same rule, and it is also how the ONE line that matters stays findable.
    ///
    /// Neuter check: drop the `last_scan_unavailable` comparison and this
    /// reports 3.
    #[tokio::test]
    async fn a_permanent_scan_fault_warns_once_not_every_tick() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(std::sync::Arc::new(FakeGit {
                root: Ok(Some(dir.path().to_path_buf())),
                default_ref: Err("no `origin/HEAD` in this clone".to_string()),
                ..FakeGit::healthy(0, 0)
            }));

        for _ in 0..3 {
            state.tick(&sink, &metrics).await;
        }

        let logged = logs.text();
        assert_eq!(
            logged.matches("scan source unavailable").count(),
            1,
            "one unchanging fault is one line, not one per tick; got: {logged}"
        );
        assert_eq!(
            metrics.snapshot().cycles_total,
            3,
            "all three cycles are still counted"
        );
    }

    /// The reverse transition: clearing the setting stops the scan on the
    /// next tick, and the OFF line is logged exactly ONCE — per transition,
    /// never per tick, however many idle ticks follow.
    #[tokio::test]
    async fn clearing_the_plans_dir_stops_scanning_and_logs_off_exactly_once() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "armed from the first tick"
        );
        assert_eq!(logs.text().matches("markdown-plan tier is OFF").count(), 0);

        // The operator clears the field.
        *cell.lock().unwrap() = PathInputs::default();
        for _ in 0..3 {
            state.tick(&sink, &metrics).await;
        }

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "no scan may run after the dir is cleared"
        );
        let snap = metrics.snapshot();
        assert_eq!(snap.cycles_total, 1);
        assert_eq!(snap.active_plans_dir, None);
        assert_eq!(snap.scan_roots, 0);
        assert_eq!(
            snap.path_resolutions_total, 2,
            "on→off is one re-resolution, not three"
        );
        let logged = logs.text();
        assert_eq!(
            logged.matches("markdown-plan tier is OFF").count(),
            1,
            "OFF is logged once per transition, not per idle tick; got: {logged}"
        );
    }

    /// The COLD-START BULK SEED, at the loop level where it actually runs.
    ///
    /// `reconcile_once`'s per-slug seed is the correctness path and is tested
    /// above; this asserts the cheap path that removes ~1,200 serialized
    /// round-trips on the first cycle after a runner start — that the loop
    /// primes `last_applied` from ONE `list_statuses` read, that the primed
    /// status is what protects coord's terminal row, and that the read is
    /// attempted once per corpus rather than once per cycle.
    ///
    /// Neuter check: delete the `self.bulk_seed(..)` call in `tick` and the
    /// `list_statuses_calls` assertion fails.
    #[tokio::test]
    async fn the_loop_bulk_seeds_once_and_the_prime_protects_a_terminal_status() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        // coord says `shipped`; the file still says DRAFT — the divergence that
        // demoted 88 terminal units per runner start before the seed existed.
        // `last_actor` is a real agent because priming alone protects nothing:
        // it restores the edge-trigger, and the OWNERSHIP DEFERRAL is what then
        // withholds the transition.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            bulk: Some(
                [("2026-01-01-one-plan".to_string(), "shipped".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("2026-01-01-one-plan".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;

        assert_eq!(
            sink.statuses
                .lock()
                .unwrap()
                .get("2026-01-01-one-plan")
                .map(String::as_str),
            Some("shipped"),
            "the primed status must survive the first cycle after a start"
        );
        assert_eq!(*sink.transitions.lock().unwrap(), 0, "a real agent owns it");
        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            1,
            "one bulk read on the cold cycle"
        );
        assert_eq!(
            metrics.snapshot().seeded_total,
            1,
            "the one slug present in the plans dir was primed"
        );

        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            1,
            "the bulk seed is a cold-start cost, not a per-cycle one"
        );
    }

    /// A sink with NO bulk door must still tick — the trait's defaulted
    /// `Ok(None)` is a documented fallback to the per-slug seed, not an error,
    /// and must not stop the cycle.
    #[tokio::test]
    async fn the_loop_falls_back_to_the_per_slug_seed_when_the_sink_has_no_bulk_door() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default(); // bulk: None
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;

        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 1);
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the cycle still ran and still created the unit"
        );
    }

    /// A corpus switch clears `last_applied`, so the corpus is COLD again and
    /// the bulk seed must re-arm — otherwise the per-slug fallback pays one
    /// round-trip per plan on the very next cycle, which is the cost the bulk
    /// read exists to remove.
    ///
    /// Neuter check: drop `self.bulk_seeded = false;` from `apply_resolution`
    /// and the second `list_statuses_calls` assertion reads 1.
    #[tokio::test]
    async fn changing_the_plans_dir_re_arms_the_bulk_seed() {
        let first = one_plan_dir();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            second.path().join("2026-02-02-another-plan.md"),
            "# Another plan\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(first.path());
        let sink = FakeSink {
            bulk: Some(HashMap::new()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 1);

        *cell.lock().unwrap() = plans_dir_input(second.path());
        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            2,
            "a new corpus is cold again, so the bulk seed re-arms"
        );
    }

    /// Moving the active dir to a different directory re-seeds the edge
    /// memory: the slugs of the old corpus must not be reported as
    /// "disappeared" from the new one.
    #[tokio::test]
    async fn changing_the_plans_dir_reseeds_rather_than_reporting_the_old_corpus_as_gone() {
        let logs = CapturedLogs::start();
        let first = one_plan_dir();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            second.path().join("2026-02-02-another-plan.md"),
            "# Another plan\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(first.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        *cell.lock().unwrap() = plans_dir_input(second.path());
        state.tick(&sink, &metrics).await;

        let slugs: Vec<String> = sink
            .upserts
            .lock()
            .unwrap()
            .iter()
            .map(|u| u.slug.clone())
            .collect();
        assert_eq!(
            slugs,
            vec!["2026-01-01-one-plan", "2026-02-02-another-plan"]
        );
        assert!(
            !logs.text().contains("disappeared from the active dir"),
            "a corpus switch is a re-seed, not a mass disappearance; got: {}",
            logs.text()
        );
        assert_eq!(logs.text().matches("markdown-plan tier is ON").count(), 2);
    }

    /// A slug archived (not vanished) is NOT flagged disappeared — the archive
    /// set suppresses it.
    #[test]
    fn archived_slug_is_not_disappeared() {
        let known: HashSet<String> = ["done".to_string()].into_iter().collect();
        let active: HashSet<String> = HashSet::new();
        let archive: HashSet<String> = ["done".to_string()].into_iter().collect();
        let mut warned: HashSet<String> = HashSet::new();
        assert!(newly_disappeared_slugs(&known, &active, &archive, &mut warned).is_empty());
        assert!(warned.is_empty());
    }

    /// **The disappeared-slug detector must be fed what was SCANNED, not what
    /// was APPLIED.** A coord-DERIVED stamp is withdrawn from the wire, so the
    /// unit never enters the apply-memory — and a `shipped` plan swept up by a
    /// consolidation is the headline case D4 exists for.
    ///
    /// Neuter check: feed `newly_disappeared_slugs` `last_applied`'s keys
    /// again — the WARN never fires and this fails.
    #[tokio::test]
    async fn a_scanned_slug_is_reported_when_its_file_vanishes_even_if_nothing_was_applied() {
        let logs = CapturedLogs::start();
        let dir = tempfile::tempdir().unwrap();
        let plan = dir.path().join("2026-03-03-consolidated-plan.md");
        std::fs::write(&plan, "# Consolidated\n\n> **Status: SHIPPED**\n").unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the plan WAS scanned and pushed (metadata-only)"
        );
        assert!(
            state.last_applied.is_empty(),
            "...and nothing was applied — which is precisely why the apply-memory \
             cannot be the detector's input"
        );

        std::fs::remove_file(&plan).unwrap();
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert!(
            logged.contains("disappeared from the active dir"),
            "the vanished plan must be surfaced; got: {logged}"
        );
        assert!(logged.contains("2026-03-03-consolidated-plan"));

        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            1,
            "a disappeared slug is surfaced at most once per process"
        );
    }

    /// **A scan must say whether it was COMPLETE — an empty or short vector
    /// cannot.** The partial case is the one a cheap `units.is_empty()` guard
    /// would miss entirely.
    #[test]
    fn a_plan_dir_scan_reports_whether_it_was_complete() {
        let conv = PlanConvention::operator_default();

        // 1. Healthy.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("2026-01-01-a.md"),
            "# A\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let scan = scan_plan_dir(dir.path(), &conv);
        assert_eq!(scan.units.len(), 1);
        assert!(scan.complete);

        // 2. A failed `read_dir` — empty AND incomplete.
        let gone = scan_plan_dir(Path::new("/definitely/not/a/dir/xyz"), &conv);
        assert!(gone.units.is_empty());
        assert!(
            !gone.complete,
            "a directory that could not be READ must never read as EMPTY"
        );

        // 3. PARTIAL: listed, but `read_to_string` refuses it (invalid UTF-8 is
        //    the portable spelling of that failure).
        std::fs::write(dir.path().join("2026-01-02-b.md"), [0xff, 0xfe, 0xfd]).unwrap();
        let partial = scan_plan_dir(dir.path(), &conv);
        assert_eq!(
            partial.units.len(),
            1,
            "the readable plan is still returned"
        );
        assert!(!partial.complete, "...but the scan is PARTIAL and says so");

        // 4. A DANGLING SYMLINK: listed, and `stat` refuses it with
        //    `NotFound` — but `lstat` finds the link, so it is a DECIDED
        //    not-a-plan and the scan stays COMPLETE (`is_dangling_symlink`).
        //    It used to be this test's portable spelling of an unstattable
        //    entry, which is how a standing broken link came to hold the scan
        //    PARTIAL — and the disappeared-slug detector disarmed — forever.
        //    The genuinely unstattable arm (`PermissionDenied`, a name gone
        //    mid-scan) is pinned by
        //    `an_entry_that_cannot_be_statted_is_a_gap_not_a_skip`. Its OWN
        //    tempdir: reusing case 3's leaves that case's unreadable file
        //    behind, which clears `complete` by itself and makes this case
        //    prove nothing.
        //
        //    Neuter check: drop the `DanglingLink` arm from
        //    `classify_plan_entry` and the `complete` assertion below fails.
        let dir4 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir4.path().join("2026-01-03-c.md"),
            "# C\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let dangling = dir4.path().join("2026-01-04-d.md");
        match try_symlink(Path::new("no-such-target.md"), &dangling) {
            Ok(()) => {
                assert!(
                    std::fs::metadata(&dangling).is_err(),
                    "precondition: the link must be UNSTATTABLE"
                );
                let dangling_scan = scan_plan_dir(dir4.path(), &conv);
                assert_eq!(dangling_scan.units.len(), 1);
                assert!(
                    dangling_scan.complete,
                    "a dangling symlink is a DECIDED not-a-plan, never a standing gap"
                );
                assert_eq!(
                    dangling_scan.dangling_link_slugs,
                    vec!["2026-01-04-d".to_string()],
                    "...whose NAME is still carried, as presence only"
                );
            }
            Err(e) => {
                // Not a pass: an explicit, printed inability to run this case.
                eprintln!(
                    "SKIPPED case 4 (dangling symlink): this platform refused to create a \
                     symlink ({e})."
                );
            }
        }
    }

    #[cfg(unix)]
    fn try_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn try_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    /// **A `stat` that FAILS is a gap; a `stat` that succeeds on a DIRECTORY is
    /// a skip.** Pinned with no filesystem privilege in the way.
    ///
    /// Neuter check: fold the `Err` arm into `PlanEntry::NotAPlan` (which is
    /// exactly what `is_file()` did) and the third assertion fails.
    #[test]
    fn an_entry_that_cannot_be_statted_is_a_gap_not_a_skip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.md");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            classify_plan_entry(file.metadata(), || file.symlink_metadata()),
            PlanEntry::Read
        ));
        assert!(
            matches!(
                classify_plan_entry(dir.path().metadata(), || dir.path().symlink_metadata()),
                PlanEntry::NotAPlan
            ),
            "a DIRECTORY named `*.md` is resolved and genuinely not a plan"
        );
        assert!(
            matches!(
                classify_plan_entry(
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "EACCES",
                    )),
                    || panic!("lstat must not be consulted for a non-NotFound failure"),
                ),
                PlanEntry::Unstattable(_)
            ),
            "a REFUSED stat is a hole in the scan, never `not a plan`"
        );
        // `NotFound` with NOTHING behind the name either — the entry went
        // away between `read_dir` and `stat`. Uncertain (it may be an
        // unlink-then-create in flight), so still a gap.
        let gone = dir.path().join("gone.md");
        assert!(
            matches!(
                classify_plan_entry(gone.metadata(), || gone.symlink_metadata()),
                PlanEntry::Unstattable(_)
            ),
            "a name that vanished mid-scan is UNCERTAIN, so it is a gap"
        );
        // `NotFound` on a REGULAR file's lstat is the same uncertain shape:
        // only a SYMLINK makes the `NotFound` a decided answer.
        assert!(
            matches!(
                classify_plan_entry(
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                    || file.symlink_metadata(),
                ),
                PlanEntry::Unstattable(_)
            ),
            "NotFound from stat plus a non-link lstat decides nothing"
        );
    }

    /// **A PARTIAL active scan must produce ZERO disappearance warnings, and
    /// must poison nothing.** `warned_disappeared` is warn-once PER PROCESS,
    /// so a false fire burns the slug for the life of the process. Cycle 3 is
    /// the poisoning half: a slug the PARTIAL cycle could not see, and which
    /// then genuinely vanishes, must still be surfaced.
    ///
    /// Neuter check: drop the `active_scan_complete && archive_scan_complete`
    /// guard in `LoopState::tick` — cycle 2's zero-warning assertion fails.
    #[tokio::test]
    async fn a_partial_active_scan_reports_no_disappearance_and_poisons_nothing() {
        let logs = CapturedLogs::start();
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("2026-02-01-a.md");
        let b = dir.path().join("2026-02-02-b.md");
        std::fs::write(&a, "# A\n\n> **Status: DRAFT**\n").unwrap();
        std::fs::write(&b, "# B\n\n> **Status: DRAFT**\n").unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert_eq!(state.seen_slugs.len(), 2, "both plans were scanned");

        // `b` becomes unreadable IN PLACE: nothing disappeared.
        std::fs::write(&b, [0xff, 0xfe, 0xfd]).unwrap();
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "a PARTIAL scan must claim nothing about absence; got: {}",
            logs.text()
        );
        assert!(state.warned_disappeared.is_empty(), "...and poison nothing");
        assert!(
            logs.text().contains("disappeared-slug detection SKIPPED"),
            "the skip is stated out loud; got: {}",
            logs.text()
        );
        // A STANDING fault is said once per streak, not once per cycle.
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared-slug detection SKIPPED")
                .count(),
            1,
            "a second skipped cycle in the same streak must not re-WARN; got: {}",
            logs.text()
        );

        std::fs::remove_file(&b).unwrap();
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert_eq!(
            logged.matches("disappeared from the active dir").count(),
            1,
            "the real disappearance is surfaced exactly once; got: {logged}"
        );
        assert!(logged.contains("2026-02-02-b"));
    }

    /// **A symlinked plan whose TARGET goes missing has not left the active
    /// dir, so it must not burn its warn-once.**
    ///
    /// A dangling link is a DECIDED not-a-plan (`is_dangling_symlink`), which
    /// keeps the scan COMPLETE and so arms the detector — and a non-atomic
    /// rewrite of the target makes a working link dangle for one cycle. Were
    /// the link's stem simply absent, that cycle would warn falsely and insert
    /// the slug into `warned_disappeared`, which is never pruned: the REAL
    /// removal in cycle 3 would then say nothing.
    ///
    /// Neuter check: pass `&active_slugs` instead of `&present_active` to
    /// `newly_disappeared_slugs` — cycle 2 warns and cycle 3 is silent.
    #[tokio::test]
    async fn a_dangling_plan_link_is_present_to_the_disappearance_detector() {
        let logs = CapturedLogs::start();
        let dir = tempfile::tempdir().unwrap();
        let targets = tempfile::tempdir().unwrap();
        let target = targets.path().join("real-plan.md");
        std::fs::write(&target, "# L\n\n> **Status: DRAFT**\n").unwrap();
        let link = dir.path().join("2026-02-03-linked.md");
        if let Err(e) = try_symlink(&target, &link) {
            eprintln!("SKIPPED: this platform refused to create a symlink ({e})");
            return;
        }
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert!(
            state.seen_slugs.contains("2026-02-03-linked"),
            "the working link was read as a plan"
        );

        // The TARGET goes away; the link's name stays.
        std::fs::remove_file(&target).unwrap();
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "a link whose target is missing has not left the dir; got: {}",
            logs.text()
        );
        assert!(
            state.warned_disappeared.is_empty(),
            "...and poisons nothing"
        );

        // The LINK goes away: that is a real disappearance, surfaced once.
        std::fs::remove_file(&link).unwrap();
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert_eq!(
            logged.matches("disappeared from the active dir").count(),
            1,
            "the real disappearance is surfaced exactly once; got: {logged}"
        );
        assert!(logged.contains("2026-02-03-linked"));
    }

    /// **The same protection on the ARCHIVE side.** `archive_scan.dangling_link_slugs`
    /// is chained into `present_archive` exactly as the active arm chains its
    /// own into `present_active` — this pins that the archive chain is wired,
    /// not just present in the diff.
    ///
    /// A slug first seen in the active dir moves to the archive dir as a
    /// working symlink (no warning: it is genuinely present there). Its
    /// TARGET then goes missing while the link's name stays in the archive
    /// dir — the same race as the active-side test, on the other scan.
    ///
    /// Neuter check: drop `.chain(archive_dangling)` from `present_archive`
    /// in `LoopState::tick` — cycle 3 below then warns falsely.
    #[tokio::test]
    async fn a_dangling_archived_link_is_present_to_the_disappearance_detector() {
        let logs = CapturedLogs::start();
        let dir = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir().unwrap();
        let targets = tempfile::tempdir().unwrap();

        let slug_file = dir.path().join("2026-02-04-archived.md");
        std::fs::write(&slug_file, "# A\n\n> **Status: DRAFT**\n").unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = PathInputs {
            plans_archive_dir: Some(archive.path().to_string_lossy().to_string()),
            ..plans_dir_input(dir.path())
        };
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        // Cycle 1: seen in the active dir.
        state.tick(&sink, &metrics).await;
        assert!(
            state.seen_slugs.contains("2026-02-04-archived"),
            "the plan file was read from the active dir"
        );

        // Cycle 2: moved out of active into the archive dir, as a WORKING
        // symlink (target present) — genuinely there, no dangling-link
        // protection needed yet.
        std::fs::remove_file(&slug_file).unwrap();
        let target = targets.path().join("archived-target.md");
        std::fs::write(&target, "# A\n\n> **Status: DRAFT**\n").unwrap();
        let link = archive.path().join("2026-02-04-archived.md");
        if let Err(e) = try_symlink(&target, &link) {
            eprintln!("SKIPPED: this platform refused to create a symlink ({e})");
            return;
        }
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "moved into the archive dir via a working link — genuinely present"
        );

        // Cycle 3: the archived link's TARGET goes missing; the link's name
        // stays in the archive dir. Without `archive_dangling`, this slug
        // would be absent from both `present_active` and `present_archive`
        // and warn falsely.
        std::fs::remove_file(&target).unwrap();
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "a dangling archive link has not left the archive dir; got: {}",
            logs.text()
        );
        assert!(
            state.warned_disappeared.is_empty(),
            "...and poisons nothing"
        );

        // Cycle 4: the archive link itself goes away — a real disappearance,
        // surfaced once.
        std::fs::remove_file(&link).unwrap();
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert_eq!(
            logged.matches("disappeared from the active dir").count(),
            1,
            "the real disappearance is surfaced exactly once; got: {logged}"
        );
        assert!(logged.contains("2026-02-04-archived"));
    }

    /// **The same property on main's REF scan.** A blob the listing names but
    /// the read cannot produce (corrupt, or not UTF-8) comes back `Ok` and
    /// SHORT — `read_ref_dir` refuses only the wholesale failure. Before the
    /// `complete` flag reached the tick, that short read was indistinguishable
    /// from the plan leaving the ref, and burned the slug into the warn-once
    /// set.
    ///
    /// Neuter check: make `read_plans_for_cycle`'s ref arm return
    /// `complete: true` unconditionally — cycle 2 warns falsely and cycle 3
    /// then says nothing.
    #[tokio::test]
    async fn a_partial_ref_read_reports_no_disappearance_and_poisons_nothing() {
        struct SwitchableGit(Mutex<FakeGit>);
        impl GitRefReader for SwitchableGit {
            fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
                self.0.lock().unwrap().work_tree_root(dir)
            }
            fn default_ref(&self, repo_root: &Path) -> Result<String, String> {
                self.0.lock().unwrap().default_ref(repo_root)
            }
            fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<String, String> {
                self.0.lock().unwrap().rev_parse(repo_root, rev)
            }
            fn count_behind_ahead(
                &self,
                repo_root: &Path,
                reference: &str,
                head: &str,
            ) -> Result<(u64, u64), String> {
                self.0
                    .lock()
                    .unwrap()
                    .count_behind_ahead(repo_root, reference, head)
            }
            fn ref_refresh_stamps(
                &self,
                repo_root: &Path,
                default_ref: &str,
                ref_sha: &str,
            ) -> Vec<Result<Option<i64>, String>> {
                self.0
                    .lock()
                    .unwrap()
                    .ref_refresh_stamps(repo_root, default_ref, ref_sha)
            }
            fn fetch_default(&self, repo_root: &Path, default_ref: &str) -> Result<(), String> {
                self.0.lock().unwrap().fetch_default(repo_root, default_ref)
            }
            fn list_ref_dir(
                &self,
                repo_root: &Path,
                ref_name: &str,
                rel_dir: &str,
            ) -> Result<Vec<RefDirEntry>, String> {
                self.0
                    .lock()
                    .unwrap()
                    .list_ref_dir(repo_root, ref_name, rel_dir)
            }
            fn read_blobs(&self, repo_root: &Path, ids: &[String]) -> Vec<Result<String, String>> {
                self.0.lock().unwrap().read_blobs(repo_root, ids)
            }
        }

        let logs = CapturedLogs::start();
        let dir = tempfile::tempdir().unwrap();
        let entry = |name: &str, id: &str| RefDirEntry {
            name: name.into(),
            id: id.into(),
        };
        let git = std::sync::Arc::new(SwitchableGit(Mutex::new(FakeGit {
            root: Ok(Some(dir.path().to_path_buf())),
            ref_dir: Ok(vec![
                entry("2026-05-01-a.md", "ida"),
                entry("2026-05-02-b.md", "idb"),
            ]),
            blobs: [
                (
                    "ida".to_string(),
                    Ok("# A\n\n> **Status: DRAFT**\n".to_string()),
                ),
                (
                    "idb".to_string(),
                    Ok("# B\n\n> **Status: DRAFT**\n".to_string()),
                ),
            ]
            .into_iter()
            .collect(),
            ..FakeGit::healthy(0, 0)
        })));
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(git.clone());

        state.tick(&sink, &metrics).await;
        assert_eq!(
            state.seen_slugs.len(),
            2,
            "both plans were read off the ref"
        );

        // `b` is still LISTED at the ref, but its blob will not read.
        git.0
            .lock()
            .unwrap()
            .blobs
            .insert("idb".to_string(), Err("corrupt object".to_string()));
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "a PARTIAL ref read must claim nothing about absence; got: {}",
            logs.text()
        );
        assert!(state.warned_disappeared.is_empty(), "...and poison nothing");
        assert!(logs.text().contains("disappeared-slug detection SKIPPED"));

        // Now `b` genuinely leaves the ref.
        git.0.lock().unwrap().ref_dir = Ok(vec![entry("2026-05-01-a.md", "ida")]);
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert_eq!(
            logged.matches("disappeared from the active dir").count(),
            1,
            "the real disappearance is surfaced exactly once; got: {logged}"
        );
        assert!(logged.contains("2026-05-02-b"));
    }

    /// **The ARCHIVE scan gets the same guard — and it is the likelier
    /// shape.** The archive set is the only thing that SUPPRESSES a warning,
    /// so an archive scan that fails on its own false-fires every plan that
    /// was scanned active and has since been consolidated into the archive.
    ///
    /// Neuter check: drop `archive_scan_complete` from the guard in
    /// `LoopState::tick` — cycle 2 warns falsely and cycle 4 then says nothing.
    #[tokio::test]
    async fn a_partial_archive_scan_never_false_fires_the_disappearance_detector() {
        let logs = CapturedLogs::start();
        let active = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir().unwrap();
        let plan = active.path().join("2026-04-04-consolidated.md");
        let archived = archive.path().join("2026-04-04-consolidated.md");
        std::fs::write(&plan, "# C\n\n> **Status: DRAFT**\n").unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = PathInputs {
            plans_dir: Some(active.path().to_string_lossy().to_string()),
            plans_archive_dir: Some(archive.path().to_string_lossy().to_string()),
            ..PathInputs::default()
        };
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = tick_state(reader);

        state.tick(&sink, &metrics).await;
        assert_eq!(state.seen_slugs.len(), 1);

        // Moved into the archive — where THIS cycle cannot read it.
        std::fs::remove_file(&plan).unwrap();
        std::fs::write(&archived, [0xff, 0xfe, 0xfd]).unwrap();
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "an unreadable ARCHIVE cannot license a disappearance claim; got: {}",
            logs.text()
        );
        assert!(state.warned_disappeared.is_empty(), "nothing poisoned");

        // The archive copy reads fine now: archived, not lost.
        std::fs::write(&archived, "# C\n\n> **Status: SHIPPED**\n").unwrap();
        state.tick(&sink, &metrics).await;
        assert_eq!(
            logs.text()
                .matches("disappeared from the active dir")
                .count(),
            0,
            "an archived plan is not a disappeared one"
        );

        // Now it really is gone from both dirs.
        std::fs::remove_file(&archived).unwrap();
        state.tick(&sink, &metrics).await;
        let logged = logs.text();
        assert_eq!(
            logged.matches("disappeared from the active dir").count(),
            1,
            "the real disappearance is still detectable; got: {logged}"
        );
        assert!(logged.contains("2026-04-04-consolidated"));
    }
}
