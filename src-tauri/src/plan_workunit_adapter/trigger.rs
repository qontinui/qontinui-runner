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
//! [`spawn_if_configured`] is gated on a **configured plans directory**: the
//! runner's `PathSettings::plans_dir` setting, overridable per-machine by the
//! `QONTINUI_PLAN_ADAPTER_DIR` env var ([`PLAN_ADAPTER_DIR_ENV`]). The
//! markdown-plan carrier is the optional top coordination tier, so a runner
//! with neither configured no-ops entirely (it never scans, never pushes) —
//! claims/intent and coord-native work-units are unaffected.
//!
//! The settings value is passed IN rather than read here: this module lives in
//! the lib crate and the settings store lives in the runner binary's module
//! tree, so the binary resolves `PathSettings` and hands the value to
//! [`spawn_if_configured`]. [`resolve_plans_dir`] owns the precedence so every
//! surface that needs the active plans dir (the adapter here, the session-env
//! injection in the binary) resolves it identically.

use super::parser::{parse_work_unit, slug_from_filename, ParsedWorkUnit, PlanConvention};
use super::push::{
    push_archive_metadata, push_work_unit, PushOutcomeKind, SetDepsOutcome, WorkUnitSink,
};
use std::collections::{HashMap, HashSet};
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
}

/// A point-in-time read of [`AdapterMetrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub scanned: u64,
    pub transitions_total: u64,
    pub cycles_total: u64,
    pub conflicts_total: u64,
    pub errors_total: u64,
    pub deps_set_total: u64,
    pub deps_skipped_unmigrated_total: u64,
    pub deps_errors_total: u64,
    pub archive_stamped_total: u64,
}

impl AdapterMetrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            scanned: self.scanned.load(Ordering::Relaxed),
            transitions_total: self.transitions_total.load(Ordering::Relaxed),
            cycles_total: self.cycles_total.load(Ordering::Relaxed),
            conflicts_total: self.conflicts_total.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
            deps_set_total: self.deps_set_total.load(Ordering::Relaxed),
            deps_skipped_unmigrated_total: self
                .deps_skipped_unmigrated_total
                .load(Ordering::Relaxed),
            deps_errors_total: self.deps_errors_total.load(Ordering::Relaxed),
            archive_stamped_total: self.archive_stamped_total.load(Ordering::Relaxed),
        }
    }
}

/// The shared process-wide adapter metrics.
pub fn adapter_metrics() -> &'static AdapterMetrics {
    static METRICS: OnceLock<AdapterMetrics> = OnceLock::new();
    METRICS.get_or_init(AdapterMetrics::default)
}

/// Outcome of one reconcile cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconcileSummary {
    pub scanned: u64,
    pub transitions: u64,
    pub conflicts: u64,
    pub errors: u64,
    /// Dependency-edge replace-sets applied to coord's edge table this cycle.
    pub deps_set: u64,
    /// Dep-set calls skipped because coord's edge table isn't migrated yet.
    pub deps_skipped_unmigrated: u64,
    /// Dep-set calls that hard-errored (does not count toward `errors`, which
    /// is reserved for the unit upsert/transition path — a dep-edge failure is
    /// non-fatal and additive).
    pub deps_errors: u64,
}

/// Read + parse every `*.md` in `dir` (non-recursive — the plans dir is flat,
/// matching coord's `walk_root`). IO errors on individual files are logged and
/// skipped; a missing dir yields an empty vec.
pub fn read_plan_dir(dir: &Path, conv: &PlanConvention) -> Vec<ParsedWorkUnit> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "plan adapter: cannot read plans dir");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let slug = slug_from_filename(&path_str);
                out.push(parse_work_unit(&slug, &path_str, &body, conv));
            }
            Err(e) => {
                tracing::warn!(path = %path_str, error = %e, "plan adapter: cannot read plan file");
            }
        }
    }
    out
}

/// Push every parsed unit through the edge-trigger + conflict logic, updating
/// the client-side `last_applied` memory and the shared metrics. Pure of IO
/// beyond the sink, so it is unit-tested with a fake sink.
pub async fn reconcile_once<S: WorkUnitSink + ?Sized>(
    parsed_units: &[ParsedWorkUnit],
    last_applied: &mut HashMap<String, String>,
    last_deps: &mut HashMap<String, Vec<String>>,
    sink: &S,
    metrics: &AdapterMetrics,
) -> ReconcileSummary {
    let mut summary = ReconcileSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        let prev = last_applied.get(&u.slug).cloned();
        match push_work_unit(sink, u, prev.as_deref()).await {
            Ok(outcome) => {
                if outcome.conflict {
                    summary.conflicts += 1;
                    metrics.conflicts_total.fetch_add(1, Ordering::Relaxed);
                }
                if matches!(outcome.kind, PushOutcomeKind::Transitioned { .. }) {
                    summary.transitions += 1;
                    metrics.transitions_total.fetch_add(1, Ordering::Relaxed);
                }
                // Record what we just applied so the next cycle is edge-triggered.
                last_applied.insert(u.slug.clone(), u.status.clone());

                // After the unit's upsert/transition succeeded, ALSO push its
                // dependency set to coord's first-class edge table (additive to
                // the metadata.depends_on JSONB fallback the upsert already
                // wrote). Best-effort: a 503 (table not migrated) is benign and
                // a hard error does NOT fail the reconcile — the unit already
                // landed and edges are additive. Edge-triggered: only re-send
                // when the dep set changed since we last applied it (the
                // replace-set is idempotent, so this is purely an optimization).
                if !u.depends_on.is_empty() && last_deps.get(&u.slug) != Some(&u.depends_on) {
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
            Err(e) => {
                summary.errors += 1;
                metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(slug = %u.slug, error = %format!("{e:#}"), "plan adapter: push failed");
            }
        }
    }
    metrics.scanned.store(summary.scanned, Ordering::Relaxed);
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

/// Pure disappeared-slug detection (D4). A slug we have previously applied
/// (present in `known`) that is now absent from BOTH the active scan
/// (`active_slugs`) and the archive scan (`archive_slugs`), and has not already
/// been warned about (`warned`), is "disappeared": the plan file left the
/// active dir without landing in the archive. Returns those newly-disappeared
/// slugs and records them in `warned` so each is surfaced **once per process**.
///
/// The caller only *warns* on the result — the work unit is left untouched.
/// Terminal state is owned by coord's derive engine; the adapter must never
/// push `shipped`/`archived` to fill the gap (a second-writer race).
pub fn newly_disappeared_slugs(
    known: &HashMap<String, String>,
    active_slugs: &HashSet<String>,
    archive_slugs: &HashSet<String>,
    warned: &mut HashSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for slug in known.keys() {
        if !active_slugs.contains(slug) && !archive_slugs.contains(slug) && !warned.contains(slug) {
            warned.insert(slug.clone());
            out.push(slug.clone());
        }
    }
    out
}

/// The periodic reconcile loop. Runs until the task is dropped.
///
/// Each cycle: reconcile the active dir (edge-triggered status transitions),
/// then — when an archive dir is configured — metadata-only stamp every archived
/// plan's `archive_path` (never a transition, D4), then warn once about any slug
/// that vanished from both dirs.
async fn run_loop<S: WorkUnitSink + ?Sized>(
    dir: PathBuf,
    archive_dir: Option<PathBuf>,
    body_sync: Option<BodySync>,
    sink: &S,
    interval_secs: u64,
) {
    let conv = PlanConvention::operator_default();
    let mut body_sync = body_sync;
    let metrics = adapter_metrics();
    let mut last_applied: HashMap<String, String> = HashMap::new();
    let mut last_deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut warned_disappeared: HashSet<String> = HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    tracing::info!(
        dir = %dir.display(),
        archive_dir = archive_dir.as_ref().map(|d| d.display().to_string()),
        interval_secs,
        "plan adapter: reconcile loop started"
    );
    loop {
        tick.tick().await;
        let units = read_plan_dir(&dir, &conv);
        let summary =
            reconcile_once(&units, &mut last_applied, &mut last_deps, sink, metrics).await;
        metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            scanned = summary.scanned,
            transitions = summary.transitions,
            conflicts = summary.conflicts,
            errors = summary.errors,
            deps_set = summary.deps_set,
            deps_skipped_unmigrated = summary.deps_skipped_unmigrated,
            deps_errors = summary.deps_errors,
            "plan adapter: reconcile cycle complete"
        );

        // Archive scan (metadata-only) + disappeared-slug detection. When no
        // archive dir is configured, `read_plan_dir` on `None` is skipped and
        // the archive slug set is empty — a slug that vanishes from the active
        // dir with no archive configured is still surfaced as disappeared.
        let archived = match &archive_dir {
            Some(a) => read_plan_dir(a, &conv),
            None => Vec::new(),
        };
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
        if let Some(bs) = body_sync.as_mut() {
            bs.run_cycle(&conv).await;
        }

        let active_slugs: HashSet<String> = units.iter().map(|u| u.slug.clone()).collect();
        let archive_slugs: HashSet<String> = archived.iter().map(|u| u.slug.clone()).collect();
        for slug in newly_disappeared_slugs(
            &last_applied,
            &active_slugs,
            &archive_slugs,
            &mut warned_disappeared,
        ) {
            tracing::warn!(
                slug = %slug,
                "plan adapter: work-unit slug disappeared from the active dir and is absent \
                 from the archive dir; leaving the unit untouched (terminal state is owned by \
                 coord's derive engine — the adapter never pushes shipped/archived)"
            );
        }
    }
}

/// Per-machine override for the active plans directory. Wins over the
/// runner's `PathSettings::plans_dir` setting when set to a non-empty value.
pub const PLAN_ADAPTER_DIR_ENV: &str = "QONTINUI_PLAN_ADAPTER_DIR";

/// Opt-in switch for the plan & prompt **library body sync** riding along with
/// the reconcile loop (plan `2026-08-10-plan-and-prompt-library-in-web`
/// Phase 2). Enabled only on an exact `"1"`.
///
/// **Why opt-in rather than on-by-default.** The push authenticates with the
/// runner's coord-issued *device* JWT, and the qontinui-web plan-library routes
/// currently depend on `current_active_user`, which is Cognito-only — see
/// [`super::body_push`]'s 401 diagnostic. Until the web side accepts a device
/// bearer, an on-by-default sync would emit ~1,100 failed requests every 60s on
/// every runner in the fleet. The one-shot
/// `qontinui-pr plan-library-backfill` subcommand is the supported path in the
/// meantime, and flipping this to `1` turns the continuous sync on the moment
/// the web side is ready — without a runner rebuild.
pub const PLAN_LIBRARY_SYNC_ENV: &str = "QONTINUI_PLAN_LIBRARY_SYNC";

/// Whether the library body sync is enabled for this process. Read once at
/// spawn (unlike the write-door capability flag, which must be flippable
/// per-request): this one decides whether a long-lived loop *has* a sync at
/// all, and a mid-flight change of that shape has no meaning.
pub fn body_sync_enabled() -> bool {
    std::env::var(PLAN_LIBRARY_SYNC_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// The library body-sync half of a reconcile cycle: re-scan the three roots and
/// push any artifact whose body digest moved.
///
/// Holds its own [`super::body_push::ArtifactSyncState`], so steady state costs
/// one directory walk and zero HTTP calls — the whole point of the digest
/// memory. Kept in the same tick as the work-unit reconcile rather than on its
/// own timer so the two can never observe different filesystem states.
pub struct BodySync {
    roots: Vec<super::body_push::ScanRoot>,
    sink: super::body_push::HttpArtifactSink,
    state: super::body_push::ArtifactSyncState,
    /// Set after a cycle in which EVERY push errored, which in practice means a
    /// systemic problem (backend down, bearer rejected) rather than 1,100
    /// individually broken files. The sync then stops until the runner
    /// restarts, so a misconfiguration costs one noisy cycle, not a permanent
    /// log flood at 60-second intervals.
    disabled_after_total_failure: bool,
}

impl BodySync {
    pub fn new(
        roots: Vec<super::body_push::ScanRoot>,
        sink: super::body_push::HttpArtifactSink,
    ) -> Self {
        Self {
            roots,
            sink,
            state: super::body_push::ArtifactSyncState::new(),
            disabled_after_total_failure: false,
        }
    }

    pub async fn run_cycle(&mut self, conv: &PlanConvention) {
        if self.disabled_after_total_failure {
            return;
        }
        let (artifacts, skipped) = super::body_push::scan_all_roots(&self.roots, conv);
        if artifacts.is_empty() {
            return;
        }
        let summary =
            super::body_push::backfill_once(&self.sink, &artifacts, &mut self.state).await;
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
            "plan library: body sync cycle complete"
        );
        if attempted > 0 && summary.errors == attempted {
            self.disabled_after_total_failure = true;
            tracing::error!(
                errors = summary.errors,
                env_var = PLAN_LIBRARY_SYNC_ENV,
                "plan library: every push in this cycle failed — disabling the body sync for \
                 this process rather than retrying the same failures every tick. Fix the \
                 backend/credential and restart the runner, or use \
                 `qontinui-pr plan-library-backfill` for a one-shot run."
            );
        }
    }
}

/// Where a resolved active plans directory came from — so the caller can log
/// the env override at `info` without duplicating the precedence logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlansDirSource {
    /// [`PLAN_ADAPTER_DIR_ENV`] was set (per-machine override).
    Env,
    /// The runner's `PathSettings::plans_dir` setting.
    Settings,
}

/// Resolve the active plans directory, reporting which source won.
///
/// Precedence: [`PLAN_ADAPTER_DIR_ENV`] → `configured` (the runner's
/// `PathSettings::plans_dir`) → `None` (markdown-plan tier off). Empty strings
/// count as unset at every layer, so an accidentally-blank env var falls
/// through to the setting rather than disabling it.
pub fn resolve_plans_dir_with_source(
    configured: Option<String>,
) -> Option<(String, PlansDirSource)> {
    if let Some(dir) = std::env::var(PLAN_ADAPTER_DIR_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        return Some((dir, PlansDirSource::Env));
    }
    configured
        .filter(|s| !s.trim().is_empty())
        .map(|dir| (dir, PlansDirSource::Settings))
}

/// [`resolve_plans_dir_with_source`] without the provenance — the active plans
/// directory, or `None` when the markdown-plan tier is off.
pub fn resolve_plans_dir(configured: Option<String>) -> Option<String> {
    resolve_plans_dir_with_source(configured).map(|(dir, _)| dir)
}

/// Resolve the plans **archive** directory (D4). Unlike the active dir, the
/// archive has **no env override** — it has no legacy env var to stay
/// compatible with, and it is deliberately not derivable from the active dir
/// (it commonly lives in a different repo). A blank setting counts as unset, so
/// an archive dir configured to `""` disables the archive scan rather than
/// scanning a directory named `""`.
pub fn resolve_plans_archive_dir(configured: Option<String>) -> Option<String> {
    configured.filter(|s| !s.trim().is_empty())
}

/// Resolve the **prompts** directory (plan `2026-08-10-plan-and-prompt-library-in-web`
/// Phase 2): the third scan root, and the value exported to agent sessions as
/// `QONTINUI_PROMPTS_DIR`.
///
/// Like [`resolve_plans_archive_dir`] and unlike [`resolve_plans_dir`], there is
/// deliberately **no env override**. The active plans dir carries one only
/// because `QONTINUI_PLAN_ADAPTER_DIR` predates the setting and machines have it
/// `setx`-persisted; a brand-new directory has no such legacy to stay
/// compatible with, and a second precedence chain is a second thing that can
/// silently disagree with the settings UI. A blank setting counts as unset, so
/// `""` disables the prompts scan rather than scanning a directory named `""`.
///
/// It is also **not derivable from the plans dir**. `/create-plan` currently
/// *guesses* `$QONTINUI_PLANS_DIR/../prompts/*.md`, which is exactly the guess
/// this setting exists to replace — the operator's prompts live in more than one
/// repo and the sibling-of-plans relationship does not hold in general.
pub fn resolve_prompts_dir(configured: Option<String>) -> Option<String> {
    configured.filter(|s| !s.trim().is_empty())
}

/// Spawn the reconcile loop iff the adapter is configured for this runner: a
/// plans directory resolvable via [`resolve_plans_dir`] (the runner's
/// `PathSettings::plans_dir`, or the [`PLAN_ADAPTER_DIR_ENV`] override) AND a
/// coord base resolvable. Returns `None` (no-op) otherwise — a runner with the
/// markdown-plan tier off never scans. Interval overridable via
/// `QONTINUI_PLAN_ADAPTER_INTERVAL_SECS` (default 60s).
///
/// `configured_plans_dir` / `configured_archive_dir` are the caller-supplied
/// `PathSettings::plans_dir` / `PathSettings::plans_archive_dir` (the settings
/// store lives in the runner binary, not this lib crate). The archive dir is
/// optional and gates only the metadata-only archive scan (D4) — the adapter
/// still starts, and still reconciles the active dir, when it is unset.
pub fn spawn_if_configured(
    configured_plans_dir: Option<String>,
    configured_archive_dir: Option<String>,
    configured_prompts_dir: Option<String>,
    configured_backend_url: Option<String>,
) -> Option<tokio::task::JoinHandle<()>> {
    let (dir, source) = resolve_plans_dir_with_source(configured_plans_dir)?;
    if source == PlansDirSource::Env {
        tracing::info!(
            dir = %dir,
            env_var = PLAN_ADAPTER_DIR_ENV,
            "plan adapter: plans dir taken from env override (settings value ignored)"
        );
    }
    let resolved_archive = resolve_plans_archive_dir(configured_archive_dir);
    let resolved_prompts = resolve_prompts_dir(configured_prompts_dir);
    let archive_dir = resolved_archive.clone().map(PathBuf::from);

    // Plan & prompt library body sync: needs the opt-in flag AND a resolvable
    // web backend. Either missing is a silent no-op — the work-unit reconcile
    // below is unaffected, exactly as the archive scan is optional.
    let body_sync = if body_sync_enabled() {
        match super::body_push::HttpArtifactSink::from_env(configured_backend_url) {
            Some(sink) => {
                let roots = super::body_push::scan_roots(
                    Some(dir.clone()),
                    resolved_archive,
                    resolved_prompts,
                );
                tracing::info!(
                    roots = roots.len(),
                    backend = %sink.base(),
                    "plan library: body sync enabled"
                );
                Some(BodySync::new(roots, sink))
            }
            None => {
                tracing::warn!(
                    env_var = PLAN_LIBRARY_SYNC_ENV,
                    "plan library: body sync requested but no qontinui-web backend is \
                     configured; not syncing"
                );
                None
            }
        }
    } else {
        None
    };

    let sink = match super::push::HttpWorkUnitSink::from_profile() {
        Some(s) => s,
        None => {
            tracing::warn!(
                dir = %dir,
                "plan adapter: plans dir configured but no coord base configured; not starting"
            );
            return None;
        }
    };
    let interval_secs = std::env::var("QONTINUI_PLAN_ADAPTER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    Some(tokio::spawn(async move {
        run_loop(
            PathBuf::from(dir),
            archive_dir,
            body_sync,
            &sink,
            interval_secs,
        )
        .await;
    }))
}

#[cfg(test)]
mod tests {
    use super::super::parser::ParsedWorkUnit;
    use super::super::push::{SetDepsOutcome, TransitionBody, UpsertBody, ADAPTER_ACTOR};
    use super::*;
    use anyhow::Result;
    use std::sync::Mutex;

    // ---- active-plans-dir resolution (settings + env override) ----

    /// Serialized against every other env-touching test in this binary, and
    /// restoring `QONTINUI_PLAN_ADAPTER_DIR` on the way out — the operator's
    /// machines have it `setx`-persisted, so a leaked removal would change
    /// what sibling tests observe.
    fn with_plan_dir_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[PLAN_ADAPTER_DIR_ENV]);
        match value {
            Some(v) => std::env::set_var(PLAN_ADAPTER_DIR_ENV, v),
            None => std::env::remove_var(PLAN_ADAPTER_DIR_ENV),
        }
        f()
    }

    #[test]
    fn env_override_wins_over_the_setting() {
        let resolved = with_plan_dir_env(Some("/env/plans"), || {
            resolve_plans_dir_with_source(Some("/settings/plans".to_string()))
        });
        assert_eq!(
            resolved,
            Some(("/env/plans".to_string(), PlansDirSource::Env))
        );
    }

    #[test]
    fn setting_is_used_when_no_env_override() {
        let resolved = with_plan_dir_env(None, || {
            resolve_plans_dir_with_source(Some("/settings/plans".to_string()))
        });
        assert_eq!(
            resolved,
            Some(("/settings/plans".to_string(), PlansDirSource::Settings))
        );
    }

    /// Neither source configured ⇒ the markdown-plan tier is off. This is the
    /// no-op the adapter's opt-in contract rests on.
    #[test]
    fn nothing_configured_resolves_to_none() {
        assert_eq!(with_plan_dir_env(None, || resolve_plans_dir(None)), None);
    }

    /// A blank env var must not silently disable a configured setting.
    #[test]
    fn blank_env_falls_through_to_the_setting() {
        let resolved = with_plan_dir_env(Some("   "), || {
            resolve_plans_dir(Some("/settings/plans".to_string()))
        });
        assert_eq!(resolved.as_deref(), Some("/settings/plans"));
    }

    /// A blank setting is unset, not a directory named "".
    #[test]
    fn blank_setting_resolves_to_none() {
        let resolved = with_plan_dir_env(None, || resolve_plans_dir(Some("  ".to_string())));
        assert_eq!(resolved, None);
    }

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        unit_with_deps(slug, status, vec![])
    }

    fn unit_with_deps(slug: &str, status: &str, depends_on: Vec<String>) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: None,
            status: status.to_string(),
            depends_on,
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
    }

    #[derive(Default)]
    struct FakeSink {
        statuses: Mutex<HashMap<String, String>>,
        transitions: Mutex<u64>,
        /// Configured `by_actor` of every unit's latest history row (default
        /// None ⇒ no history ⇒ no owner to defer to).
        last_actor: Option<String>,
        deps_behavior: DepsBehavior,
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
        /// Every upsert body seen, so the archive scan can be asserted to write
        /// `metadata.archive_path` with no status.
        upserts: Mutex<Vec<UpsertBody>>,
    }
    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, slug: &str) -> Result<Option<String>> {
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            if let Some(s) = &body.status {
                self.statuses
                    .lock()
                    .unwrap()
                    .insert(body.slug.clone(), s.clone());
            }
            self.upserts.lock().unwrap().push(body.clone());
            Ok(())
        }
        async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
            *self.transitions.lock().unwrap() += 1;
            self.statuses
                .lock()
                .unwrap()
                .insert(slug.to_string(), body.to_status.clone());
            Ok(())
        }
        async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
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
            }
        }
    }

    #[tokio::test]
    async fn reconcile_is_idempotent_across_cycles() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let units = vec![unit("a", "vetted"), unit("b", "draft")];

        // First cycle: both created, no transitions.
        let s1 = reconcile_once(&units, &mut mem, &mut deps, &sink, &metrics).await;
        assert_eq!(s1.scanned, 2);
        assert_eq!(s1.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // Second cycle, unchanged corpus: NO phantom transitions.
        let s2 = reconcile_once(&units, &mut mem, &mut deps, &sink, &metrics).await;
        assert_eq!(s2.scanned, 2);
        assert_eq!(s2.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn reconcile_emits_one_transition_on_status_edge() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();

        reconcile_once(&[unit("a", "vetted")], &mut mem, &mut deps, &sink, &metrics).await;
        // Plan edited: vetted -> shipped.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    // --- Graduation-bootstrap P2a: markdown proxy defers to real agents ------

    #[tokio::test]
    async fn defers_transition_when_real_agent_owns_unit() {
        // A real agent last drove the unit (its own agent-scoped actor). The
        // file's status edge (vetted -> shipped) WOULD transition, but the proxy
        // must DEFER so it doesn't collapse the agent's transition to the system
        // actor: ZERO transitions emitted.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();

        // Establish last-applied=vetted (create; UpsertWithStatus is never gated).
        reconcile_once(&[unit("a", "vetted")], &mut mem, &mut deps, &sink, &metrics).await;
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // File edited vetted -> shipped: transition WOULD fire, but defer.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(metrics.snapshot().transitions_total, 0);
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

        reconcile_once(&[unit("a", "vetted")], &mut mem, &mut deps, &sink, &metrics).await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
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

        reconcile_once(&[unit("a", "vetted")], &mut mem, &mut deps, &sink, &metrics).await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
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
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p2".to_string()]);

        let s = reconcile_once(&[u], &mut mem, &mut deps, &sink, &metrics).await;
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

        let s = reconcile_once(&[unit("a", "vetted")], &mut mem, &mut deps, &sink, &metrics).await;
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
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        // First cycle sends deps.
        reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &sink,
            &metrics,
        )
        .await;
        // Second cycle, unchanged dep set: no re-send (idempotent edge-trigger).
        let s2 = reconcile_once(&[u], &mut mem, &mut deps, &sink, &metrics).await;
        assert_eq!(s2.deps_set, 0);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 1);

        // Dep set changed -> re-send.
        let u2 = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p3".to_string()]);
        let s3 = reconcile_once(&[u2], &mut mem, &mut deps, &sink, &metrics).await;
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
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
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
        let s2 = reconcile_once(&[u], &mut mem, &mut deps, &sink, &metrics).await;
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
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(&[u], &mut mem, &mut deps, &sink, &metrics).await;
        // A dep-edge failure does NOT fail the reconcile (unit upsert landed).
        assert_eq!(s.errors, 0);
        assert_eq!(s.deps_errors, 1);
        assert_eq!(s.deps_set, 0);
        assert_eq!(metrics.snapshot().deps_errors_total, 1);
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

    /// The load-bearing D4 test: an archive scan of real `*.md` files — one
    /// whose `> **Status:` says the coord-derived `shipped`, one whose status is
    /// the non-vocabulary `archived` (which coord silently classifies `Free` and
    /// would ACCEPT) — produces ZERO status transitions, only a
    /// `metadata.archive_path` stamp per slug pointing at the archived file.
    #[tokio::test]
    async fn archive_scan_stamps_path_and_never_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let shipped_path = write_plan(
            tmp.path(),
            "2026-01-01-shipped-plan",
            "# Shipped Plan\n\n> **Status:** shipped 2026-01-01.\n",
        );
        let archived_path = write_plan(
            tmp.path(),
            "2026-01-02-archived-plan",
            "# Archived Plan\n\n> **Status:** archived\n",
        );

        // Reuse the production scan path — its missing-dir-yields-empty-vec
        // behavior is exactly the right unset semantics.
        let conv = PlanConvention::operator_default();
        let scanned = read_plan_dir(tmp.path(), &conv);
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
        assert_eq!(
            by_slug["2026-01-01-shipped-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!(shipped_path)
        );
        assert_eq!(
            by_slug["2026-01-02-archived-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!(archived_path)
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

    /// Disappeared-slug rule (D4): a slug we applied that is gone from the active
    /// dir AND absent from the archive dir is surfaced ONCE per process, and the
    /// detection never transitions (it only warns — no sink call at all).
    #[test]
    fn disappeared_slug_warns_once_and_never_transitions() {
        let mut known: HashMap<String, String> = HashMap::new();
        known.insert("a".to_string(), "in_progress".to_string());
        known.insert("b".to_string(), "vetted".to_string());
        known.insert("c".to_string(), "shipped".to_string());

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

    /// A slug archived (not vanished) is NOT flagged disappeared — the archive
    /// set suppresses it.
    #[test]
    fn archived_slug_is_not_disappeared() {
        let mut known: HashMap<String, String> = HashMap::new();
        known.insert("done".to_string(), "shipped".to_string());
        let active: HashSet<String> = HashSet::new();
        let archive: HashSet<String> = ["done".to_string()].into_iter().collect();
        let mut warned: HashSet<String> = HashSet::new();
        assert!(newly_disappeared_slugs(&known, &active, &archive, &mut warned).is_empty());
        assert!(warned.is_empty());
    }
}
