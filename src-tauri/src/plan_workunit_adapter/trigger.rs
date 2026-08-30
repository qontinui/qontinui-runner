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
    push_archive_metadata, push_work_unit, push_work_unit_with_remote, PushOutcomeKind,
    SetDepsOutcome, WorkUnitSink,
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
    /// Slugs coord refused with a `403` and this process has therefore retired
    /// (counter, monotonic — one increment per refused slug, not per cycle).
    /// A non-zero value here with a flat `errors_total` is the healthy shape:
    /// the adapter noticed a permission verdict and stopped re-asking.
    pub forbidden_total: AtomicU64,
}

/// A point-in-time read of [`AdapterMetrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Units skipped or retired this cycle because coord answered `403`
    /// ([`super::push::ForbiddenByCoord`]). Deliberately NOT folded into
    /// `errors`: `errors` means "retryable, and we will retry", which is the
    /// one thing a permission verdict is not.
    pub forbidden: u64,
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
    forbidden: &mut HashSet<String>,
    sink: &S,
    metrics: &AdapterMetrics,
) -> ReconcileSummary {
    let mut summary = ReconcileSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        // A slug coord has already refused (403) is retired for the life of the
        // process: the request would be byte-identical, so the verdict would be
        // too. Skipping here — rather than merely muting the log — is what makes
        // this a fix and not a mute: it also stops the HTTP call.
        if forbidden.contains(&u.slug) {
            summary.forbidden += 1;
            continue;
        }
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
                let deferred = matches!(outcome.kind, PushOutcomeKind::Deferred { .. });
                if deferred {
                    summary.deferred += 1;
                    metrics.deferrals_total.fetch_add(1, Ordering::Relaxed);
                }
                // Record what we just applied so the next cycle is edge-triggered
                // — but ONLY when something was actually applied. A deferral
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
                if !deferred {
                    last_applied.insert(u.slug.clone(), u.status.clone());
                }

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
                // A 403 is a settled permission verdict, not a transient
                // failure. Retire the slug and say so ONCE; every later cycle
                // takes the `forbidden.contains` skip above and logs nothing.
                if let Some(f) =
                    e.downcast_ref::<crate::plan_workunit_adapter::push::ForbiddenByCoord>()
                {
                    forbidden.insert(u.slug.clone());
                    summary.forbidden += 1;
                    metrics.forbidden_total.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        slug = %u.slug,
                        route = %f.route,
                        detail = %f.detail,
                        "plan adapter: coord refused this work unit (403); retiring the \
                         slug for the life of this process — an identical retry \
                         cannot change the verdict. Restart the runner after \
                         fixing the principal's permission."
                    );
                } else {
                    summary.errors += 1;
                    metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(slug = %u.slug, error = %format!("{e:#}"), "plan adapter: push failed");
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
    /// Units whose read or push errored. The pass continues past each one.
    pub failed: u64,
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
                summary.failed += 1;
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan backfill: push failed"
                );
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
    // Slugs coord answered `403` for. Owned by the loop (there is exactly one
    // per process), so "retired" means "for this process's lifetime".
    let mut forbidden: HashSet<String> = HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    // A cycle can legitimately outrun the interval (the first one walks and
    // pushes ~1,100 files). The default `Burst` behaviour then fires every
    // missed tick back to back, so a 5-minute first cycle is followed by four
    // immediate no-gap cycles — the opposite of what a periodic reconcile
    // wants. `Delay` drops the missed ticks and simply restarts the interval
    // from now.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(
        dir = %dir.display(),
        archive_dir = archive_dir.as_ref().map(|d| d.display().to_string()),
        interval_secs,
        "plan adapter: reconcile loop started"
    );
    loop {
        tick.tick().await;
        // RT-P0: `read_plan_dir` is a SYNCHRONOUS walk — one `std::fs::read_dir`
        // plus a `read_to_string` of every `*.md` in the plans dir (~1,100
        // files; the loop's own tick comment above measures the first cycle at
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
        let units = {
            let dir = dir.clone();
            let conv = conv.clone();
            match tokio::task::spawn_blocking(move || read_plan_dir(&dir, &conv)).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "plan adapter: plans-dir scan task failed; skipping this cycle"
                    );
                    Vec::new()
                }
            }
        };
        let summary = reconcile_once(
            &units,
            &mut last_applied,
            &mut last_deps,
            &mut forbidden,
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
            "plan adapter: reconcile cycle complete"
        );

        // Archive scan (metadata-only) + disappeared-slug detection. When no
        // archive dir is configured, `read_plan_dir` on `None` is skipped and
        // the archive slug set is empty — a slug that vanishes from the active
        // dir with no archive configured is still surfaced as disappeared.
        // Same reasoning as the active scan above: off the single worker.
        let archived = match archive_dir.clone() {
            Some(a) => {
                let conv = conv.clone();
                match tokio::task::spawn_blocking(move || read_plan_dir(&a, &conv)).await {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "plan adapter: archive-dir scan task failed; skipping this cycle"
                        );
                        Vec::new()
                    }
                }
            }
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

/// Whether the tenant's fleet dial currently authorizes plan capture.
///
/// A callback rather than a direct read because the dial's cache lives in the
/// runner **binary** (`crate::mcp::fleet_policy_poller`) while this adapter
/// lives in the lib crate, which cannot see it. The binary supplies the reader
/// at spawn time; the lib stays free of the poller.
pub type CaptureGate = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

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
/// `QONTINUI_PLAN_LIBRARY_SYNC=1` would keep pushing the whole corpus at fleet
/// level `off`. Capture is now two independent authorizations in the same
/// direction — a per-machine opt-in env flag AND a tenant-wide dial — so the
/// dial is a real fleet kill switch that does not require touching env on
/// every machine (and cannot, since restarting runners is forbidden).
pub struct BodySync {
    roots: Vec<super::body_push::ScanRoot>,
    sink: super::body_push::HttpArtifactSink,
    state: super::body_push::ArtifactSyncState,
    capture_gate: CaptureGate,
    breaker: FailureBreaker,
    /// Last gate verdict observed, so a flip is logged ONCE rather than every
    /// tick. `None` until the first cycle.
    last_gate_open: Option<bool>,
}

impl BodySync {
    pub fn new(
        roots: Vec<super::body_push::ScanRoot>,
        sink: super::body_push::HttpArtifactSink,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            roots,
            sink,
            state: super::body_push::ArtifactSyncState::new(),
            capture_gate,
            breaker: FailureBreaker::new(),
            last_gate_open: None,
        }
    }

    pub async fn run_cycle(&mut self, conv: &PlanConvention) {
        let gate_open = (self.capture_gate)();
        if self.last_gate_open != Some(gate_open) {
            tracing::info!(
                capture_enabled = gate_open,
                "plan library: tenant plan_capture level changed the body sync's authorization"
            );
            self.last_gate_open = Some(gate_open);
        }
        if !gate_open {
            return;
        }
        if self.breaker.should_skip_cycle() {
            return;
        }

        // `scan_all_roots` does ~1,100 synchronous `read_to_string` calls. On
        // the async path that blocks a tokio worker thread for the whole walk,
        // starving every other task sharing it — so it runs on the blocking
        // pool and the result comes back by value.
        let roots = self.roots.clone();
        let conv = conv.clone();
        let scanned =
            tokio::task::spawn_blocking(move || super::body_push::scan_all_roots(&roots, &conv))
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
pub fn spawn_if_configured(
    configured_plans_dir: Option<String>,
    configured_archive_dir: Option<String>,
    configured_prompts_dir: Option<String>,
    configured_backend_url: Option<String>,
    capture_gate: CaptureGate,
) -> Option<tokio::task::JoinHandle<()>> {
    // A bare `?` here used to be the whole story: no plans dir resolved, return
    // `None`, log NOTHING at any level. On a machine with neither the setting
    // nor the env var that is indistinguishable from a healthy scan — the
    // `silent-empty-is-unknown` shape — and it is exactly how a fleet-wide
    // work-unit ingestion gap went unreported for months. Say it out loud, at
    // `info`, and NAME the two things that arm the tier so the reader does not
    // have to find this function to learn them.
    let (dir, source) = match resolve_plans_dir_with_source(configured_plans_dir) {
        Some(resolved) => resolved,
        None => {
            tracing::info!(
                setting = "paths.plans_dir",
                env_var = PLAN_ADAPTER_DIR_ENV,
                "plan adapter: markdown-plan tier is OFF on this machine — no active plans \
                 dir is configured, so NO plan file is scanned and NO work unit is pushed to \
                 coord from this runner. Arm it by setting `paths.plans_dir` in the runner's \
                 settings, or by exporting QONTINUI_PLAN_ADAPTER_DIR; catch a machine up \
                 without a restart with `qontinui-pr plan-workunit-backfill --plans-dir <dir>`"
            );
            return None;
        }
    };
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
                    "plan library: body sync enabled (still gated per-cycle on the tenant's \
                     plan_capture fleet dial)"
                );
                Some(BodySync::new(roots, sink, capture_gate))
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
                env_var = "COORD_HTTP_URL",
                setting = "profiles.<active>.coord_url",
                "plan adapter: plans dir configured but no coord base configured; not starting \
                 — NO work unit is pushed to coord from this runner. Arm it by exporting \
                 COORD_HTTP_URL or connecting the active profile to a coord deployment"
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
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
        /// When set, every `upsert` answers with a coord `403` — the shape that
        /// used to be retried, and re-logged, once per slug per cycle forever.
        upsert_forbidden: bool,
        /// When set, every `upsert` answers with an ordinary (retryable) error.
        upsert_errors: bool,
        /// Total `upsert` calls received, so a test can prove a retired slug
        /// stops making the HTTP call at all — not merely stops logging.
        upsert_calls: Mutex<u64>,
        /// Every upsert body seen, so the archive scan can be asserted to write
        /// `metadata.archive_path` with no status.
        upserts: Mutex<Vec<UpsertBody>>,
    }
    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, slug: &str) -> Result<Option<String>> {
            if self.fail_status_read_for.as_deref() == Some(slug) {
                anyhow::bail!("simulated work-unit status read failure");
            }
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            *self.upsert_calls.lock().unwrap() += 1;
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
        let mut forb: HashSet<String> = HashSet::new();
        let units = vec![unit("a", "vetted"), unit("b", "draft")];

        // First cycle: both created, no transitions.
        let s1 = reconcile_once(&units, &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
        assert_eq!(s1.scanned, 2);
        assert_eq!(s1.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // Second cycle, unchanged corpus: NO phantom transitions.
        let s2 = reconcile_once(&units, &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
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
        let mut forb: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
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
        let mut forb: HashSet<String> = HashSet::new();

        // Establish last-applied=vetted (create; UpsertWithStatus is never gated).
        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // File edited vetted -> shipped: transition WOULD fire, but defer.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
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
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
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
        let mut forb: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
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
        let mut forb: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
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
        let mut forb: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p2".to_string()]);

        let s = reconcile_once(&[u], &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
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
        let mut forb: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
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
        let mut forb: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        // First cycle sends deps.
        reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
            &sink,
            &metrics,
        )
        .await;
        // Second cycle, unchanged dep set: no re-send (idempotent edge-trigger).
        let s2 = reconcile_once(&[u], &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
        assert_eq!(s2.deps_set, 0);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 1);

        // Dep set changed -> re-send.
        let u2 = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p3".to_string()]);
        let s3 = reconcile_once(&[u2], &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
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
        let mut forb: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
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
        let s2 = reconcile_once(&[u], &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
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
        let mut forb: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(&[u], &mut mem, &mut deps, &mut forb, &sink, &metrics).await;
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
        let mut forb: HashSet<String> = HashSet::new();

        let s1 = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
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
        let mut forb: HashSet<String> = HashSet::new();

        for _ in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
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

    // ---- tier visibility ---------------------------------------------------

    /// The observability half: a machine with NO plans dir must SAY the
    /// markdown-plan tier is off, at `info`, naming both things that arm it.
    /// The shipped code returned `None` from a bare `?` and logged nothing at
    /// any level, which is indistinguishable from a healthy scan — the defect
    /// that let a fleet-wide ingestion gap run unreported.
    ///
    /// Neuter check: restore the bare `?` in `spawn_if_configured` and this
    /// fails.
    #[test]
    fn tier_off_machine_says_so_at_info() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);
        impl Write for Captured {
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
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
            type Writer = Captured;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Captured::default();
        let buf = sink.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink)
            .with_max_level(tracing::Level::INFO)
            .finish();

        // No env override AND no setting ⇒ the tier is off.
        with_plan_dir_env(None, || {
            tracing::subscriber::with_default(subscriber, || {
                let handle = spawn_if_configured(
                    None,
                    None,
                    None,
                    None,
                    std::sync::Arc::new(|| true) as CaptureGate,
                );
                assert!(handle.is_none(), "an unarmed tier spawns nothing");
            });
        });

        let logged = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
        assert!(
            logged.contains("markdown-plan tier is OFF"),
            "the tier-off line must be emitted; got: {logged}"
        );
        assert!(
            logged.contains("paths.plans_dir"),
            "it must name the setting that arms the tier; got: {logged}"
        );
        assert!(
            logged.contains(PLAN_ADAPTER_DIR_ENV),
            "it must name the env var that arms the tier; got: {logged}"
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
