//! Ξ_Worktree census collector (Phase 1, runner side).
//!
//! coord cannot see the operator's Windows disk — it has no host
//! filesystem access. The runner is the only component that can
//! enumerate the on-disk git worktrees, measure their footprint, detect
//! junctioned `node_modules`/`target` dirs (so a 165 GB junctioned
//! `target` costs ~0 to "size" and is attributed to the canonical tree,
//! not the worktree), and read the volume's free space. This module
//! periodically collects that census and POSTs it to coord's
//! `POST /coord/worktree-census/{device_id}` (anonymous, device-keyed — same
//! machine-wide posture as `/coord/trees/upsert`) in bounded CHUNKS as the
//! walk progresses (every `QONTINUI_CENSUS_CHUNK_ROWS` rows or
//! `QONTINUI_CENSUS_CHUNK_SECS` seconds, whichever first) — on a large
//! population a walk takes HOURS, and a single end-of-walk POST left coord
//! with no recent rows (and thus zero actionable reclaim instructions) for
//! the entire walk. Volume reports ride ONLY the final chunk of a walk.
//!
//! ## Free space is DECOUPLED from the walk (disk-monitoring Phase 1)
//!
//! Because volumes rode only the walk's final chunk, "how much disk is left"
//! was sampled once per multi-HOUR walk (one measured at 12.25 h). That is not
//! monitoring. [`spawn_volume_publisher`] therefore samples every mounted
//! volume on its OWN short tick (default 60 s,
//! `QONTINUI_VOLUME_SAMPLE_INTERVAL_SECS`, floored 60 s), publishes it to a
//! process-local cell ([`latest_volume_sample`]) and POSTs a **volumes-only**
//! census body (`worktrees: []`) to the same
//! `POST /coord/worktree-census/{device_id}` endpoint — zero schema change,
//! coord already persists `volumes` and `worktrees` independently.
//!
//! Two invariants govern that publisher (INV-D1 of
//! `plans/2026-08-07-product-disk-monitoring-and-cleanup.md`):
//!
//! 1. **It answers under every condition** — mid-walk, mid-build,
//!    coord-unreachable, cold start, secondary instance. The local sample is
//!    published BEFORE any network attempt, so a telemetry outage costs the
//!    reading nothing, and a failed POST is DROPPED (never retried, never
//!    buffered) exactly like a census chunk.
//! 2. **Absent data is UNKNOWN, never zero.** A probe that returns no volumes
//!    leaves the previous sample in place and logs; it never publishes an
//!    empty "0 bytes free" reading, and a volume reporting `total_bytes == 0`
//!    (no media in the drive) is dropped rather than rendered as full.
//!
//! ## Mirrors the machine-wide pollers, not the per-agent ones
//!
//! Unlike [`crate::dirty_poller`] (per-agent, JWT-gated, one task per
//! allocated agent), the census is **machine-wide**: one task per
//! runner process, keyed by the device's identity from
//! `~/.qontinui/machine.json`. The closest precedent is
//! [`crate::fleet::spawn_tree_publisher`] — same identity source
//! (`device_id` from machine.json), same coord-base resolver
//! (`COORD_HTTP_URL` env → active profile `coord_url`, ws→http), same
//! `tokio::time::interval` + `MissedTickBehavior::Skip` posture, same
//! best-effort "warn and retry next tick, never panic" contract.
//!
//! ## Enumeration
//!
//! For each governed repo root under [`qontinui_root`] (the same parent
//! dir `fleet::tree_publisher` walks), the census finds worktrees three
//! ways and dedups by canonical path:
//!
//! 1. `git -C <canonical> worktree list --porcelain` — git-registered
//!    worktrees (incl. the canonical tree itself).
//! 2. Sibling `<repo>-wt-*` directories in the parent dir — agent /
//!    operator worktrees that may not be registered with the main repo
//!    (e.g. a `git worktree add` from a different checkout, or a manual
//!    clone).
//! 3. Per-repo `.claude/worktrees/*` directories.
//!
//! ## Sizing
//!
//! `node_modules` and the build `target` dir (`target` for cargo repos,
//! `src-tauri/target` for the Tauri runner) are measured with a
//! recursive walk that **skips any reparse point** (junction) — it
//! reports 0 bytes for a junctioned dir and never traverses it. This is
//! the load-bearing safety property: junctioned build dirs (the runner
//! junctions `node_modules`/`dist`/`target` into worktrees to avoid
//! re-downloading/re-compiling) are shared with the canonical tree, so
//! attributing their bytes to the worktree would massively over-count.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The runner's connected-vs-isolated decision, imported (not re-wrapped)
/// from its single definition in `profiles`.
use crate::profiles::connected_coord_base;

use super::canonical_paths::default_canonical_path;

/// Default census cadence — 300s (5 min). The census is a heavy-ish
/// walk (it stats every real file in `node_modules`/`target`), so it
/// runs an order of magnitude slower than the 5s dirty poller and the
/// 30s fleet heartbeat. Override via `QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS`.
const DEFAULT_CENSUS_INTERVAL_SECS: u64 = 300;

/// Default chunk-flush row bound. The walk emits a chunk after this many
/// rows OR after [`DEFAULT_CENSUS_CHUNK_SECS`] seconds, whichever comes
/// first, so coord always holds recent rows for recently-walked paths even
/// while a multi-hour walk is still in flight. Override via
/// `QONTINUI_CENSUS_CHUNK_ROWS`.
const DEFAULT_CENSUS_CHUNK_ROWS: usize = 500;

/// Default chunk-flush time bound (seconds). Override via
/// `QONTINUI_CENSUS_CHUNK_SECS`.
const DEFAULT_CENSUS_CHUNK_SECS: u64 = 60;

/// Default per-chunk POST timeout (seconds). Chunks are bounded (~500 rows)
/// so coord's server-side persist stays well inside this; the old whole-walk
/// POST could exceed its 15s timeout on a 5901-row body. Override via
/// `QONTINUI_CENSUS_POST_TIMEOUT_SECS`.
const DEFAULT_CENSUS_POST_TIMEOUT_SECS: u64 = 30;

fn census_chunk_rows() -> usize {
    std::env::var("QONTINUI_CENSUS_CHUNK_ROWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CENSUS_CHUNK_ROWS)
        .max(1)
}

fn census_chunk_secs() -> u64 {
    std::env::var("QONTINUI_CENSUS_CHUNK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CENSUS_CHUNK_SECS)
        .max(1)
}

fn census_post_timeout_secs() -> u64 {
    std::env::var("QONTINUI_CENSUS_POST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CENSUS_POST_TIMEOUT_SECS)
        .max(1)
}

/// Env override (seconds) for the dedicated volume publisher's cadence.
pub const VOLUME_INTERVAL_SECS_ENV: &str = "QONTINUI_VOLUME_SAMPLE_INTERVAL_SECS";

/// Default volume-sample cadence — 60 s (plan D9 `sample_interval_secs`).
/// Three orders of magnitude faster than the walk it was extracted from.
const DEFAULT_VOLUME_INTERVAL_SECS: u64 = 60;

/// Floor for [`VOLUME_INTERVAL_SECS_ENV`], mirroring the orphan reaper's
/// `QONTINUI_ORPHAN_TARGET_INTERVAL_SECS` floored-60s pattern. It equals the
/// default deliberately: the knob exists to make the publisher QUIETER on a
/// machine that wants less telemetry, and can never be used to hammer coord
/// with sub-minute POSTs.
const MIN_VOLUME_INTERVAL_SECS: u64 = 60;

/// Ceiling for [`VOLUME_INTERVAL_SECS_ENV`] — one hour.
///
/// A floor alone only bounds the knob in the direction that costs coord
/// requests. In the other direction it silently DISABLES the feature:
/// `QONTINUI_VOLUME_SAMPLE_INTERVAL_SECS=999999999` is a well-formed `u64`,
/// so it used to be accepted verbatim — monitoring off for 31 years,
/// announced by a single `info!` at boot, with every disk surface rendering
/// `pending`/`stale` forever and nothing anywhere saying the publisher was
/// configured never to run again. An hour is already three orders of
/// magnitude quieter than the default; past that the honest configuration is
/// "don't spawn the publisher", not "spawn one that never ticks".
const MAX_VOLUME_INTERVAL_SECS: u64 = 3600;

/// Compile-time: the default must sit INSIDE the clamp band, or the ordinary
/// unconfigured boot logs a spurious "CLAMPED" warning at every start.
const _: () = assert!(
    MIN_VOLUME_INTERVAL_SECS <= DEFAULT_VOLUME_INTERVAL_SECS
        && DEFAULT_VOLUME_INTERVAL_SECS <= MAX_VOLUME_INTERVAL_SECS,
    "DEFAULT_VOLUME_INTERVAL_SECS must lie within [MIN_VOLUME_INTERVAL_SECS, MAX_VOLUME_INTERVAL_SECS]"
);

/// Resolved volume-sample cadence: env override, clamped to
/// `[MIN_VOLUME_INTERVAL_SECS, MAX_VOLUME_INTERVAL_SECS]`.
fn volume_sample_interval_secs() -> u64 {
    resolve_volume_interval(std::env::var(VOLUME_INTERVAL_SECS_ENV).ok().as_deref())
}

/// PURE resolution of the cadence from a raw env value — unparseable and
/// absent both fall back to the default, and every value is CLAMPED to the
/// floor/ceiling band. Split out because env-driven assertions flake in this
/// crate's test harness (other threads mutate the process environment).
///
/// Clamping is announced at `warn!`: a knob that was silently rewritten is
/// how an operator ends up debugging "the disk panel never updates" against
/// a value they believe is in force.
fn resolve_volume_interval(raw: Option<&str>) -> u64 {
    let requested = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_VOLUME_INTERVAL_SECS);
    let clamped = requested.clamp(MIN_VOLUME_INTERVAL_SECS, MAX_VOLUME_INTERVAL_SECS);
    if clamped != requested {
        warn!(
            "worktree_census: {VOLUME_INTERVAL_SECS_ENV}={requested}s is outside the supported \
             band [{MIN_VOLUME_INTERVAL_SECS}s, {MAX_VOLUME_INTERVAL_SECS}s] — CLAMPED to \
             {clamped}s. A cadence above the ceiling does not make the publisher quieter, it \
             turns disk monitoring OFF while every surface keeps rendering `pending`."
        );
    }
    clamped
}

/// Minimum spacing between low-disk log emissions from the volume publisher.
/// The publisher ticks every 60 s; the alarm is a LEADING signal, not a
/// heartbeat, and an `error!` per minute for hours during a real disk
/// emergency buries the logs that diagnose it.
const LOW_DISK_LOG_THROTTLE_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Census-before-reclaim boot ordering (the stale-census husk-guard, R3).
//
// The boot-time reclaim pull must not race ahead of the boot-time census:
// coord's view between the two is whatever the LAST census of the previous
// boot reported, so any worktree deleted while the runner was down generates
// stale instructions for the whole window. The reclaim poller therefore
// gates its FIRST pull of each boot on this signal (with a one-interval
// timeout fallback so a census-disabled config never deadlocks reclaim).
// ---------------------------------------------------------------------------

/// Set once the first census POST of this boot has succeeded (2xx).
static FIRST_CENSUS_POSTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Wakes reclaim's boot-ordering wait the moment the flag flips.
static FIRST_CENSUS_NOTIFY: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

fn first_census_notify() -> &'static tokio::sync::Notify {
    FIRST_CENSUS_NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// Record that a census POST landed (called from [`ChunkPoster::post`] on the
/// first 2xx chunk POST of a walk). Chunked emission releases this gate as
/// soon as coord holds SOME rows — the reclaim poller's boot gate only needs
/// "coord has a recent census", not a completed walk.
fn mark_first_census_posted() {
    FIRST_CENSUS_POSTED.store(true, std::sync::atomic::Ordering::Release);
    first_census_notify().notify_waiters();
}

/// Wait up to `timeout` for the first successful census POST of this boot.
/// Returns `true` when a census has been posted (possibly before the call),
/// `false` on timeout (census disabled, no coord configured, or slow walk —
/// the caller proceeds and relies on coord's staleness degrade).
pub(super) async fn wait_first_census_posted(timeout: Duration) -> bool {
    // Register the waiter BEFORE the flag check so a mark between the check
    // and the await can't be missed (Notify::notified buffers a permit only
    // for already-registered waiters via notify_waiters).
    let notified = first_census_notify().notified();
    if FIRST_CENSUS_POSTED.load(std::sync::atomic::Ordering::Acquire) {
        return true;
    }
    tokio::select! {
        _ = notified => true,
        _ = tokio::time::sleep(timeout) => {
            FIRST_CENSUS_POSTED.load(std::sync::atomic::Ordering::Acquire)
        }
    }
}

// ---------------------------------------------------------------------------
// The shared census SNAPSHOT (the on-demand survey's data source).
//
// The census walk ([`build_census_chunked`]) is a MULTI-HOUR walk on a real
// machine: thousands of worktrees under `D:/qontinui-root`, each costing ~7
// `git` subprocess spawns for the worktree plus 3 for its canonical checkout
// (~2s/row measured), plus a recursive file walk of every non-junctioned
// `node_modules`/`target`. Running it INLINE per HTTP request made
// `GET /agent-worktrees/reclaimable` hang for the whole walk — a UI panel's
// primary fetch must never do that.
//
// So the walk publishes here and the on-demand survey READS this snapshot.
// Chunked emission keeps the snapshot PROGRESSIVE: every chunk is upsert-
// merged by `(repo, path)` as the walk advances, and the completed walk then
// REPLACES the whole snapshot (which is what drops rows for paths that
// vanished — chunk merges between completions only ever add/update). A
// snapshot is only ever used for DISPLAY: the survey always reports how old
// it is (`census_age_secs`), and the removal path re-evaluates every runner
// guard against LIVE disk before deleting anything (see
// `on_demand::execute_targets`) — a stale census can never authorize a
// deletion.
// ---------------------------------------------------------------------------

/// The most recent census state. Cheap to clone (the payload is behind an
/// `Arc`) so readers never hold the lock across `await`.
///
/// * `taken_at` — wall-clock time of the NEWEST data merged in: the newest
///   chunk while a walk is in flight, the walk-completion instant otherwise.
///   `census_age_secs` in the survey therefore reflects how recently ANY rows
///   were refreshed, which is the honest freshness signal under chunked
///   emission (individual rows can be older; the removal path re-checks live
///   disk regardless).
/// * `build_ms` — duration of the last COMPLETED walk; `0` when only chunk
///   merges have landed so far this boot (no walk has completed yet).
#[derive(Debug, Clone)]
pub(super) struct CensusSnapshot {
    pub(super) req: Arc<WorktreeCensusReq>,
    pub(super) taken_at: chrono::DateTime<chrono::Utc>,
    pub(super) build_ms: u64,
}

static LATEST_CENSUS: OnceLock<RwLock<Option<CensusSnapshot>>> = OnceLock::new();

/// Wakes survey waiters the moment a new snapshot lands (same idiom as
/// [`FIRST_CENSUS_NOTIFY`], one level up: that one fires on a successful POST,
/// this one on a completed WALK — the survey does not care whether coord
/// accepted the census).
static CENSUS_SNAPSHOT_NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// A census walk is running right now. Guards against piling a second
/// multi-minute disk walk on top of the first (a `?refresh=1` spam, or a
/// periodic tick landing mid-refresh).
static CENSUS_BUILD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// How many census walks this process has STARTED. Purely observational — the
/// regression test asserts the survey does not increment it.
static CENSUS_BUILDS_STARTED: AtomicU64 = AtomicU64::new(0);

fn latest_census_cell() -> &'static RwLock<Option<CensusSnapshot>> {
    LATEST_CENSUS.get_or_init(|| RwLock::new(None))
}

fn snapshot_notify() -> &'static tokio::sync::Notify {
    CENSUS_SNAPSHOT_NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// The most recent snapshot, or `None` before the first walk of this boot
/// completes. NEVER blocks and NEVER triggers a walk.
pub(super) fn latest_census() -> Option<CensusSnapshot> {
    latest_census_cell()
        .read()
        .ok()
        .and_then(|g| g.as_ref().cloned())
}

/// True while a census walk is in flight.
pub(super) fn census_build_active() -> bool {
    CENSUS_BUILD_ACTIVE.load(Ordering::Acquire)
}

/// Count of census walks STARTED in this process (test observability).
pub(super) fn census_builds_started() -> u64 {
    CENSUS_BUILDS_STARTED.load(Ordering::Acquire)
}

/// Replace the snapshot in `cell` with a COMPLETED walk result. This is the
/// only operation that can DROP rows (paths that vanished since the previous
/// walk) — chunk merges only ever add/update. Cell-parametrized so tests can
/// exercise the replace semantics on a local cell without racing the global.
fn publish_census_to_cell(
    cell: &RwLock<Option<CensusSnapshot>>,
    req: WorktreeCensusReq,
    build_ms: u64,
) -> Arc<WorktreeCensusReq> {
    let arc = Arc::new(req);
    let snapshot = CensusSnapshot {
        req: Arc::clone(&arc),
        taken_at: chrono::Utc::now(),
        build_ms,
    };
    if let Ok(mut g) = cell.write() {
        *g = Some(snapshot);
    }
    arc
}

/// Publish a completed walk and wake every waiter.
fn publish_census(req: WorktreeCensusReq, build_ms: u64) -> Arc<WorktreeCensusReq> {
    let arc = publish_census_to_cell(latest_census_cell(), req, build_ms);
    snapshot_notify().notify_waiters();
    arc
}

/// Build the snapshot that results from upsert-merging `chunk_rows` into
/// `prev`, keyed on `(repo, path)`:
///
/// * a row whose `(repo, path)` already exists is REPLACED in place;
/// * a new `(repo, path)` is appended;
/// * rows of `prev` not mentioned by the chunk are KEPT (vanished paths are
///   only dropped by the complete-walk replace, [`publish_census_to_cell`]);
/// * `volumes` and `build_ms` carry over from `prev` (volumes are refreshed
///   only by a completed walk; `build_ms` stays the last completed walk's
///   duration, `0` before the first completion);
/// * `taken_at` becomes `now` — the newest merged chunk defines snapshot
///   freshness (see [`CensusSnapshot`]).
fn merged_snapshot(
    prev: Option<&CensusSnapshot>,
    chunk_rows: &[WorktreeCensus],
    device_id: Uuid,
    tenant_id: Option<Uuid>,
    now: chrono::DateTime<chrono::Utc>,
) -> CensusSnapshot {
    let (mut worktrees, volumes, build_ms) = match prev {
        Some(p) => (p.req.worktrees.clone(), p.req.volumes.clone(), p.build_ms),
        None => (Vec::new(), Vec::new(), 0),
    };
    for row in chunk_rows {
        match worktrees
            .iter_mut()
            .find(|w| w.repo == row.repo && w.path == row.path)
        {
            Some(existing) => *existing = row.clone(),
            None => worktrees.push(row.clone()),
        }
    }
    CensusSnapshot {
        req: Arc::new(WorktreeCensusReq {
            device_id,
            tenant_id,
            volumes,
            worktrees,
        }),
        taken_at: now,
        build_ms,
    }
}

/// Upsert-merge one chunk into `cell` (see [`merged_snapshot`]). No-op on an
/// empty chunk. Cell-parametrized for the same test-isolation reason as
/// [`publish_census_to_cell`].
fn merge_chunk_into_cell(
    cell: &RwLock<Option<CensusSnapshot>>,
    chunk_rows: &[WorktreeCensus],
    device_id: Uuid,
    tenant_id: Option<Uuid>,
) {
    if chunk_rows.is_empty() {
        return;
    }
    if let Ok(mut g) = cell.write() {
        let snap = merged_snapshot(
            g.as_ref(),
            chunk_rows,
            device_id,
            tenant_id,
            chrono::Utc::now(),
        );
        *g = Some(snap);
    }
}

/// Merge one chunk into the shared snapshot and wake survey waiters, so the
/// on-demand survey improves continuously while a walk is in flight instead
/// of being all-or-nothing.
fn merge_chunk_into_latest(
    chunk_rows: &[WorktreeCensus],
    device_id: Uuid,
    tenant_id: Option<Uuid>,
) {
    if chunk_rows.is_empty() {
        return;
    }
    merge_chunk_into_cell(latest_census_cell(), chunk_rows, device_id, tenant_id);
    snapshot_notify().notify_waiters();
}

/// Test-only publisher so the survey's cache path is exercisable without a
/// multi-minute disk walk.
#[cfg(test)]
pub(super) fn publish_census_for_test(req: WorktreeCensusReq) {
    publish_census(req, 0);
}

// ---------------------------------------------------------------------------
// Chunked emission — the walk hands out bounded chunks as it progresses.
//
// The walk is a synchronous multi-hour function on the blocking pool; chunks
// travel over a tokio unbounded mpsc channel (the sync side's `send` never
// blocks and needs no runtime), and the async side of `build_and_publish`
// drains the channel concurrently with the walk: each chunk is upsert-merged
// into `LATEST_CENSUS` and POSTed to coord. Failed POSTs are dropped, not
// retried — the next walk re-observes every path, and per-row freshness on
// the coord side means a dropped chunk only leaves that slice unactionable
// until the next pass. The channel is bounded in practice by the walk's total
// row count (rows are small; the walk itself is the rate limiter).
// ---------------------------------------------------------------------------

/// One bounded slice of a census walk.
struct CensusChunk {
    rows: Vec<WorktreeCensus>,
    /// `Some` ONLY on the final chunk of a walk. Volumes are deliberately
    /// withheld from intermediate chunks: coord's `worktree_volume` table has
    /// no retention prune, so per-chunk volume rows would bloat it (~12k rows
    /// already).
    volumes: Option<Vec<VolumeReport>>,
}

/// Accumulates rows and flushes a chunk after `max_rows` rows OR `max_age`
/// since the oldest buffered row, whichever comes first. Pure bookkeeping —
/// the injected `now` makes the time bound unit-testable.
struct Chunker {
    max_rows: usize,
    max_age: Duration,
    buf: Vec<WorktreeCensus>,
    oldest_buffered_at: Option<Instant>,
}

impl Chunker {
    fn new(max_rows: usize, max_age: Duration) -> Self {
        Self {
            max_rows: max_rows.max(1),
            max_age,
            buf: Vec::new(),
            oldest_buffered_at: None,
        }
    }

    /// Push one row; returns a full chunk when a bound is hit.
    fn push_at(&mut self, row: WorktreeCensus, now: Instant) -> Option<Vec<WorktreeCensus>> {
        if self.buf.is_empty() {
            self.oldest_buffered_at = Some(now);
        }
        self.buf.push(row);
        let age_hit = self
            .oldest_buffered_at
            .is_some_and(|t| now.duration_since(t) >= self.max_age);
        if self.buf.len() >= self.max_rows || age_hit {
            self.oldest_buffered_at = None;
            Some(std::mem::take(&mut self.buf))
        } else {
            None
        }
    }

    fn push(&mut self, row: WorktreeCensus) -> Option<Vec<WorktreeCensus>> {
        self.push_at(row, Instant::now())
    }

    /// Drain whatever is buffered (the final, possibly-empty partial chunk).
    fn take_remainder(&mut self) -> Vec<WorktreeCensus> {
        self.oldest_buffered_at = None;
        std::mem::take(&mut self.buf)
    }
}

/// Sync-side emitter the walk drives: rows in, [`CensusChunk`]s out on the
/// channel. Send failures (receiver dropped) are ignored — the walk finishes
/// and its complete result is still returned to the caller.
struct ChunkEmitter {
    chunker: Chunker,
    tx: tokio::sync::mpsc::UnboundedSender<CensusChunk>,
}

impl ChunkEmitter {
    fn new(chunker: Chunker, tx: tokio::sync::mpsc::UnboundedSender<CensusChunk>) -> Self {
        Self { chunker, tx }
    }

    fn on_row(&mut self, row: WorktreeCensus) {
        if let Some(rows) = self.chunker.push(row) {
            let _ = self.tx.send(CensusChunk {
                rows,
                volumes: None,
            });
        }
    }

    /// Emit the final chunk: the buffered remainder plus the walk's volume
    /// reports (the ONLY chunk that carries volumes).
    fn finish(mut self, volumes: Vec<VolumeReport>) {
        let rows = self.chunker.take_remainder();
        let _ = self.tx.send(CensusChunk {
            rows,
            volumes: Some(volumes),
        });
    }
}

/// Async-side chunk POSTer. Built once per walk; `dest` is `None` when no
/// coord base is configured (chunks then only feed the local snapshot).
struct ChunkPoster {
    dest: Option<(reqwest::Client, String)>,
    device_id: Uuid,
    tenant_id: Option<Uuid>,
    posted: u32,
    failed: u32,
}

impl ChunkPoster {
    fn new(device_id: Uuid, tenant_id: Option<Uuid>) -> Self {
        Self {
            dest: Self::resolve_dest(device_id),
            device_id,
            tenant_id,
            posted: 0,
            failed: 0,
        }
    }

    /// Where this walk's chunks go, or `None` when they only feed the local
    /// snapshot.
    ///
    /// ## The instance guard lives HERE, not only on the periodic spawn
    ///
    /// [`spawn_census`] refuses to start the periodic walk on a secondary,
    /// but that is a COST decision (don't burn a multi-hour machine-wide walk
    /// you cannot contribute), and it does not cover every walk trigger:
    /// [`spawn_census_rebuild`] — the survey endpoint's `?refresh=1` — reaches
    /// [`build_and_publish`] directly, so a secondary asked to refresh would
    /// still POST the machine's whole worktree inventory under the shared
    /// `device_id`. That is the same last-writer-wins clobber the periodic
    /// guard was added to prevent, by another route.
    ///
    /// So the IDENTITY rule is pinned at the single place a census leaves this
    /// process. Every present and future walk trigger routes through here, and
    /// a secondary still gets a local snapshot to serve its own survey from —
    /// it just never speaks for the machine. This is `fleet.rs`'s own stated
    /// lesson applied to the walk: a rule keyed in one place gets missed by the
    /// next mechanism that needs it.
    fn resolve_dest(device_id: Uuid) -> Option<(reqwest::Client, String)> {
        if !crate::fleet::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
            debug!(
                "worktree_census: SECONDARY instance — walking for the local snapshot only, \
                 not POSTing the machine's inventory (see `fleet::machine_state_publish_allowed`)"
            );
            return None;
        }
        match connected_coord_base() {
            None => {
                debug!(
                    "worktree_census: runner is ISOLATED (no coord configured, not a \
                     hosted tier) — census chunks will not be POSTed"
                );
                None
            }
            Some(base) => {
                let url = format!(
                    "{}/coord/worktree-census/{}",
                    base.trim_end_matches('/'),
                    device_id
                );
                match reqwest::Client::builder()
                    .timeout(Duration::from_secs(census_post_timeout_secs()))
                    .build()
                {
                    Ok(client) => Some((client, url)),
                    Err(e) => {
                        warn!(
                            "worktree_census: build census http client: {e} — chunk POSTs disabled for this walk"
                        );
                        None
                    }
                }
            }
        }
    }

    /// POST one chunk to coord. Best-effort: failures are counted, warned,
    /// and DROPPED (never retried, never buffered) — the walk continues and
    /// the next walk re-observes the slice.
    async fn post(&mut self, chunk: CensusChunk) {
        // An intermediate chunk is only ever emitted full, but guard anyway.
        // The FINAL chunk (volumes: Some) is always POSTed even when empty —
        // an empty walk still tells coord "this device censused" and releases
        // the R3 boot gate, matching the pre-chunking behavior.
        if chunk.volumes.is_none() && chunk.rows.is_empty() {
            return;
        }
        let volumes = chunk.volumes.unwrap_or_default();
        let body = WorktreeCensusReq {
            device_id: self.device_id,
            tenant_id: self.tenant_id,
            volumes,
            worktrees: chunk.rows,
        };
        self.send(body, true).await;
    }

    /// POST a **volumes-only** body (`worktrees: []`) — the decoupled
    /// free-space sample, on its own 60 s tick rather than once per
    /// multi-hour walk. Same endpoint, same auth, same drop-on-failure
    /// posture as a census chunk.
    ///
    /// It deliberately does **not** release the R3 census-before-reclaim boot
    /// gate: this body carries no worktree rows, so coord's worktree view is
    /// exactly as stale after it as before, and letting a 60 s volume POST
    /// open the gate would hand the reclaim poller the previous boot's stale
    /// census — precisely the husk-creating race the gate exists to prevent.
    async fn post_volumes(&mut self, volumes: Vec<VolumeReport>) {
        // DEFENSIVE, not a live path: the only caller
        // ([`sample_and_publish_volumes`]) already returns on an empty probe,
        // so this branch is currently unreachable. It is kept because the
        // property it enforces is a wire invariant, not a caller detail —
        // nothing measured ⇒ nothing to say, since an empty `volumes` array is
        // indistinguishable on the wire from "this machine has 0 volumes",
        // which is the fabricated absence invariant 2 forbids. A future second
        // caller inherits the rule instead of re-deriving it.
        if volumes.is_empty() {
            return;
        }
        let body = WorktreeCensusReq {
            device_id: self.device_id,
            tenant_id: self.tenant_id,
            volumes,
            worktrees: Vec::new(),
        };
        self.send(body, false).await;
    }

    /// The single wire send. `release_boot_gate` is `true` only for bodies
    /// that actually refresh coord's WORKTREE view (census chunks) — see
    /// [`ChunkPoster::post_volumes`].
    async fn send(&mut self, body: WorktreeCensusReq, release_boot_gate: bool) {
        let Some((client, url)) = &self.dest else {
            return;
        };
        // Tenant-scoped: the census row this POST carries declares
        // `tenant_id`, so the bearer must come from THAT binding's slot and
        // not the device default. `attach_device_auth_for` degrades to
        // unauthenticated on a slot miss rather than presenting another
        // tenant's credential (`auth::select_device_bearer`), so the worst
        // case is the pre-existing anonymous send — never a cross-tenant
        // attribution, which would be worse than the status quo.
        match crate::auth::attach_device_auth_for(
            client.post(url).json(&body),
            self.tenant_id.as_ref(),
        )
        .send()
        .await
        {
            Ok(resp) if resp.status().is_success() => {
                self.posted += 1;
                // R3 boot ordering: the first successful chunk POST of this
                // boot releases the reclaim poller's census-before-reclaim
                // gate — coord holds rows from that moment on. A volumes-only
                // POST refreshes no worktree row and therefore never opens it.
                if release_boot_gate {
                    mark_first_census_posted();
                }
                debug!(
                    "worktree_census: chunk posted ({} rows, {} volumes)",
                    body.worktrees.len(),
                    body.volumes.len()
                );
            }
            Ok(resp) => {
                self.failed += 1;
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                let excerpt: String = text.chars().take(200).collect();
                warn!(
                    "worktree_census: coord returned {status} for chunk POST {url}: {excerpt} — dropping chunk, walk continues"
                );
            }
            Err(e) => {
                self.failed += 1;
                warn!("worktree_census: chunk POST {url}: {e} — dropping chunk, walk continues");
            }
        }
    }
}

/// What one call to [`build_and_publish`] did.
pub(super) enum BuildOutcome {
    /// A walk ran and its result is now the published snapshot.
    Built(Arc<WorktreeCensusReq>),
    /// Another walk was already in flight — deliberately did NOT start a
    /// second one.
    AlreadyRunning,
    /// Identity / workspace root unresolvable; nothing to census.
    Skipped,
}

/// Run the census walk once (on the blocking pool), stream its chunks to
/// `LATEST_CENSUS` + coord as they are produced, and publish the complete
/// snapshot at the end.
///
/// This is the ONLY place a walk is started, so the "at most one walk at a
/// time" invariant holds for the periodic tick and the explicit refresh
/// alike. Both walk triggers therefore POST chunks: a refresh-triggered walk
/// feeding coord matters on huge populations, where a walk can outlast many
/// periodic intervals (the periodic tick just sees `AlreadyRunning`).
pub(super) async fn build_and_publish() -> Result<BuildOutcome, String> {
    if CENSUS_BUILD_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        debug!("worktree_census: a census walk is already in flight — not starting a second");
        return Ok(BuildOutcome::AlreadyRunning);
    }
    // Resolve identity + root up front (cheap file reads) so a skip releases
    // the guard immediately and never spawns a walk.
    let Some(device_id) = load_device_id() else {
        CENSUS_BUILD_ACTIVE.store(false, Ordering::Release);
        debug!(
            "worktree_census: ~/.qontinui/machine.json missing or device_id unparseable — skipping"
        );
        return Ok(BuildOutcome::Skipped);
    };
    let Some(root) = qontinui_root() else {
        CENSUS_BUILD_ACTIVE.store(false, Ordering::Release);
        debug!("worktree_census: no qontinui-root dir resolved — skipping");
        return Ok(BuildOutcome::Skipped);
    };
    let tenant_id = resolve_tenant_id();

    CENSUS_BUILDS_STARTED.fetch_add(1, Ordering::AcqRel);
    let started = Instant::now();
    // The walk stats real files under every worktree's node_modules/target
    // and shells out to git — a synchronous multi-HOUR disk walk on large
    // populations. Run it on the blocking pool so the shared
    // fleet-publishers runtime's async worker isn't pinned for the duration
    // (the starvation class PR #391 isolated the heartbeat from), and drain
    // its chunks here concurrently.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CensusChunk>();
    let walker =
        tokio::task::spawn_blocking(move || build_census_chunked(&root, device_id, tenant_id, tx));

    let mut poster = ChunkPoster::new(device_id, tenant_id);
    // Ends when the walker drops `tx` (completion or panic).
    while let Some(chunk) = rx.recv().await {
        merge_chunk_into_latest(&chunk.rows, device_id, tenant_id);
        poster.post(chunk).await;
    }

    let built = walker.await;
    CENSUS_BUILD_ACTIVE.store(false, Ordering::Release);
    let build_ms = started.elapsed().as_millis() as u64;
    let req = built.map_err(|e| format!("census walk panicked: {e}"))?;
    info!(
        "worktree_census: census walk completed in {}ms ({} worktrees, {} volumes; {} chunk POSTs ok, {} failed)",
        build_ms,
        req.worktrees.len(),
        req.volumes.len(),
        poster.posted,
        poster.failed
    );
    Ok(BuildOutcome::Built(publish_census(req, build_ms)))
}

/// Kick a census walk in the BACKGROUND (the `?refresh=1` path). Returns
/// `true` when a new walk was started, `false` when one was already running.
/// Never awaits the walk — the caller must stay bounded.
pub(super) fn spawn_census_rebuild() -> bool {
    if census_build_active() {
        return false;
    }
    tokio::spawn(async {
        if let Err(e) = build_and_publish().await {
            warn!("worktree_census: on-demand refresh failed: {e}");
        }
    });
    true
}

/// Wait up to `timeout` for a snapshot strictly newer than `after`
/// (`after: None` ⇒ any snapshot at all). Returns `None` on timeout — the
/// caller then degrades to whatever [`latest_census`] holds, or to an honest
/// "census not ready yet" state. NEVER starts a walk.
pub(super) async fn wait_for_census_after(
    after: Option<chrono::DateTime<chrono::Utc>>,
    timeout: Duration,
) -> Option<CensusSnapshot> {
    let fresh_enough = |s: &CensusSnapshot| after.is_none_or(|prev| s.taken_at > prev);
    let deadline = Instant::now() + timeout;
    loop {
        // Register the waiter BEFORE reading, so a publish between the read
        // and the await cannot be missed.
        let notified = snapshot_notify().notified();
        if let Some(s) = latest_census() {
            if fresh_enough(&s) {
                return Some(s);
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return latest_census().filter(fresh_enough);
        }
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(remaining) => {
                return latest_census().filter(fresh_enough);
            }
        }
    }
}

/// Windows reparse-point attribute bit (`FILE_ATTRIBUTE_REPARSE_POINT`).
/// A junction (and a symlink) sets this in the file attributes returned
/// by `symlink_metadata`. Defined locally so the check needs no winapi
/// binding — `std::os::windows::fs::MetadataExt::file_attributes`
/// surfaces the raw DWORD.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

// ---------------------------------------------------------------------------
// Wire types — coord deserializes these. Field names/shape are the
// contract documented in the Phase 1 plan.
// ---------------------------------------------------------------------------

/// Free-space report for one volume.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VolumeReport {
    /// The volume's identity: the drive letter with a trailing colon (`"D:"`)
    /// when the mount IS a drive root, else the MOUNT POINT itself
    /// (`"D:/data"` for a volume mounted into a folder, `"/mnt/data"` on
    /// POSIX). See [`volume_key`] for why the drive letter alone is not a
    /// usable key.
    pub volume: String,
    /// The drive letter the volume is reachable under, when it has one. A
    /// LABEL, not the key: a folder-mounted volume shares its parent's letter,
    /// so keying on this collapses distinct volumes into one.
    ///
    /// Skipped on the wire when absent so POSIX bodies are byte-identical to
    /// the pre-existing contract; coord's `VolumeItem` ignores unknown fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_letter: Option<String>,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Census row for one worktree.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCensus {
    /// Basename of the main repo (e.g. `qontinui-runner`).
    pub repo: String,
    /// Absolute path to the worktree dir.
    pub path: String,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    /// `now - committer-time-of-HEAD`, in seconds. `None` on an unborn
    /// HEAD or when `git log` fails.
    pub head_age_secs: Option<i64>,
    pub is_dirty: bool,

    pub nm_present: bool,
    pub nm_is_junction: bool,
    /// Real (non-junction) bytes of `node_modules`. 0 when junctioned or
    /// absent.
    pub nm_bytes: u64,

    pub target_present: bool,
    pub target_is_junction: bool,
    /// Real (non-junction) bytes of the build `target` dir. 0 when
    /// junctioned or absent.
    pub target_bytes: u64,

    /// Run-provenance (plan `2026-08-16-plan-corpus-authority-and-run-
    /// provenance`, Phase 1): the **effective** cargo target directory this
    /// worktree's builds resolve to, as an absolute forward-slashed path.
    /// See [`resolve_build_target`] for the exact precedence and for the
    /// cases that deliberately emit `None`.
    ///
    /// `None` = NOT OBSERVED, never "no build target". The consumer is a
    /// deletion engine, so a wrong attribution is strictly worse than a
    /// null and every unresolvable input takes the `None` arm.
    ///
    /// Persisted as `coord.worktree_census.build_target_dir TEXT NULL` (web
    /// migration `runprov_01_worktree_census_build_target`). Optional on the
    /// wire — an old runner omits it and coord reads NULL.
    pub build_target_dir: Option<String>,

    /// Run-provenance: the shared build-pool slot identifier
    /// [`build_target_dir`](Self::build_target_dir) belongs to — the bare slot
    /// name (`"slot-2"`, `"lkg"`), NOT the whole `target-pool/slot-2` path.
    /// `None` whenever the target dir is not inside a `target-pool/` (an
    /// in-tree `target/`, a `target-agent/`, an ad-hoc out-of-tree cache) —
    /// a slot is NEVER synthesised for a non-pool directory.
    ///
    /// Persisted as `coord.worktree_census.build_slot TEXT NULL`. Optional on
    /// the wire.
    pub build_slot: Option<String>,

    /// RFC3339 mtime of the worktree dir itself, `None` if unreadable.
    pub last_access_mtime: Option<String>,
    /// Sum of the non-junction real bytes attributable to this worktree
    /// (`nm_bytes + target_bytes`). A junctioned dir contributes 0.
    pub attributable_bytes: u64,

    /// G2 "work landed" — whether this worktree's HEAD is already
    /// represented on `origin/main`, computed cheaply from local git only:
    ///
    /// * `Some(true)`  — HEAD is an ancestor of `origin/main` (true merge /
    ///   fast-forward), OR every commit unique to HEAD has a patch-id
    ///   equivalent already on `origin/main` (rebase / cherry-pick).
    /// * `Some(false)` — HEAD has commits not represented on `origin/main`.
    /// * `None`        — couldn't determine (no `origin/main` ref, detached
    ///   oddity, git failure). Coord's gate treats `None` as NOT landed.
    ///
    /// **Squash merges are NOT detectable here** — a squash rewrites the
    /// commits into a single new commit with a fresh patch-id, so neither
    /// the ancestry nor the `git cherry` patch-id test sees it. Coord
    /// covers squashes independently via the PR `close_cause='merged'`
    /// signal; this field is only the ancestry/patch-id half of G2.
    pub landed_in_main: Option<bool>,

    /// G6 shadow-mode probe — whether this worktree is currently building
    /// per [`super::reclaim::worktree_is_building`] (cargo `.cargo-lock`
    /// exclusive-open probe + recent-activity mtime window). Reported every
    /// census tick regardless of reclaim arming, so coord can gauge
    /// "instructions that WOULD have been G6-skipped" while arming is still
    /// OFF — the passive prove-out feed for the Q1 rejunction graduation.
    /// `Some(_)` is the live probe result; old runners omit the field and
    /// coord reads NULL (honest unknown).
    pub building: Option<bool>,

    /// Ξ_Worktree Phase 7.3 — canonical-checkout state, the input coord
    /// needs to prove SharedBranch's P1/P2 preconditions safe (§3.2/§3.3).
    ///
    /// These describe the **canonical repo checkout** (`<root>/<repo>/`),
    /// not this worktree row's path — canonical state is per-repo, so every
    /// worktree row of a repo carries the same values (coord reads one row
    /// per repo). All three are `None` when the canonical path can't be
    /// resolved or git fails; coord treats `None` as unsafe → falls through
    /// to an isolated Worktree (the fail-safe staging idiom). Inert until
    /// coord ingests them (7.3 consumer) and Rule 2 reads them (7.2).
    ///
    /// Current branch of the canonical checkout
    /// (`git symbolic-ref --short HEAD`). `None` on detached HEAD or git
    /// failure.
    pub canonical_current_branch: Option<String>,

    /// Whether the canonical checkout has uncommitted changes
    /// (`git status --porcelain` non-empty). `Some(true)` dirty,
    /// `Some(false)` clean, `None` on git failure (coord treats as unsafe).
    /// This is the P1-clean precondition input for SharedBranch.
    pub canonical_is_dirty: Option<bool>,

    /// Advisory base-divergence summary for the canonical checkout, e.g.
    /// `"on:main"` when parked on main, else
    /// `"on:<branch>;<behind>\t<ahead>"` from
    /// `git rev-list --count --left-right origin/main...HEAD`. Best-effort:
    /// tolerates a missing `origin/main` (just the branch name) and never
    /// errors the census. Human-readable context for the P2 base check.
    pub canonical_base_divergence: Option<String>,
}

/// Full census body POSTed to coord.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCensusReq {
    pub device_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub volumes: Vec<VolumeReport>,
    pub worktrees: Vec<WorktreeCensus>,
}

// ---------------------------------------------------------------------------
// Identity + coord-base resolution (mirrors fleet.rs).
// ---------------------------------------------------------------------------

/// `~/.qontinui/machine.json` device identity — `device_id` (serde-
/// aliased to the legacy `machine_id`). Mirrors `fleet::DeviceFile` but
/// kept local so this module is self-contained.
#[derive(Debug, Clone, serde::Deserialize)]
struct DeviceFile {
    #[serde(alias = "machine_id")]
    device_id: String,
}

fn load_device_id() -> Option<Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let device: DeviceFile = serde_json::from_slice(&bytes).ok()?;
    Uuid::parse_str(device.device_id.trim()).ok()
}

/// Crate-visible alias of [`load_device_id`] so sibling modules (the
/// Phase 4 reclaim poller) reuse the SAME identity resolution rather than
/// duplicating the machine.json parse.
pub(crate) fn load_device_id_pub() -> Option<Uuid> {
    load_device_id()
}

/// Read `active_tenant_id` from `~/.qontinui/machine.json` — Phase 8b
/// semantics (plan 2026-07-02-session-scoped-multi-tenant-device-binding
/// §D4): the DEVICE-LEVEL DEFAULT binding, not the-only-tenant. The census
/// is a device-scoped surface (it walks the whole machine), so the default
/// is the correct request-level attribution. Note the census deliberately
/// stamps NO per-worktree tenant: a worktree's tenant is its allocate-time
/// coord stamp (`coord.agent_worktrees.tenant_id`), which coord already
/// holds — stamping the machine default per row here would misattribute
/// another tenant's worktrees (plan Phase 8 item 6). `None` for
/// single-tenant operators, which is fine: coord attributes the census to
/// the device's resolved tenant regardless.
fn resolve_tenant_id() -> Option<Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    Uuid::parse_str(raw.trim()).ok()
}

/// The parent dir under which the runner's canonical checkouts +
/// sibling worktrees live.
///
/// One of four byte-similar copies of this resolution until Phase 2 of
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`; each carried a
/// hardcoded `D:/qontinui-root` Windows arm, which shipped the author's machine
/// layout inside an open-source binary. The resolution now lives once in
/// [`crate::workspace_paths`], over the shared `qontinui_types::paths`.
///
/// Discovery disposition: a miss degrades this sweep (no census rows) and
/// self-corrects the moment the root appears, so it stays `Option`.
///
/// `pub(crate)` so the Phase 5 fs_backstop poller enumerates governed canonical
/// checkouts under the SAME workspace root the census walks — no second
/// root-resolution to drift from.
pub(crate) fn qontinui_root() -> Option<PathBuf> {
    crate::workspace_paths::workspace_root()
}

// ---------------------------------------------------------------------------
// Junction detection + sizing (the cross-platform-safe core).
// ---------------------------------------------------------------------------

/// True iff `path` is a reparse point (junction / symlink) on Windows.
/// Always `false` on non-Windows (the runner ships on Windows; the
/// non-windows arm exists so the crate type-checks + tests run on CI's
/// other targets). Uses `symlink_metadata` so it inspects the link
/// itself, never its target.
pub fn is_junction(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(meta) => meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        // Treat a symlink as the closest analog to a junction so the
        // sizing walk still refuses to traverse it.
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }
}

/// Recursive byte size of `dir`, summing real file sizes. SKIPS any
/// directory that is a reparse point (junction): it contributes 0 and is
/// never traversed. The top-level `dir` is assumed already checked by
/// the caller (we never even call this for a junctioned top-level dir),
/// but nested junctions inside the tree are also skipped defensively so
/// a junction buried under a real dir can't cause a 165 GB traversal.
fn dir_size_skipping_junctions(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    for entry in read.flatten() {
        let path = entry.path();
        // symlink_metadata: never follow a link/junction.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            // A symlink (file or dir) — skip, do not follow.
            continue;
        }
        if file_type.is_dir() {
            if is_junction(&path) {
                // Reparse point — never traverse.
                continue;
            }
            total = total.saturating_add(dir_size_skipping_junctions(&path));
        } else if file_type.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Measure one candidate dir (`node_modules` / `target`):
/// `(present, is_junction, bytes)`. A junction reports
/// `(true, true, 0)` and is never walked.
fn measure_dir(dir: &Path) -> (bool, bool, u64) {
    if !dir.exists() {
        return (false, false, 0);
    }
    if is_junction(dir) {
        return (true, true, 0);
    }
    (true, false, dir_size_skipping_junctions(dir))
}

/// Pick the build `target` dir for a worktree. The Tauri runner's cargo
/// workspace lives under `src-tauri/`, so its target is
/// `src-tauri/target`; everything else uses `target`. We prefer
/// `src-tauri/target` when `src-tauri/` exists, else fall back to the
/// top-level `target`.
fn target_dir_for(worktree: &Path) -> PathBuf {
    let src_tauri = worktree.join("src-tauri");
    if src_tauri.is_dir() {
        let st_target = src_tauri.join("target");
        // Use src-tauri/target if it exists OR if there's no top-level
        // target (the Tauri layout). If the operator happens to have a
        // top-level target too, prefer the one that actually exists.
        if st_target.exists() || !worktree.join("target").exists() {
            return st_target;
        }
    }
    worktree.join("target")
}

// ---------------------------------------------------------------------------
// Run provenance — build_target_dir / build_slot (plan
// `2026-08-16-plan-corpus-authority-and-run-provenance`, Phase 1).
//
// `target_dir_for` above answers "which dir do I MEASURE for target_bytes".
// This section answers the different question "which dir do this worktree's
// builds actually WRITE TO, and is that dir a shared pool slot" — the pair
// that lets a cleanup agent attribute build-target bytes to a worktree.
// ---------------------------------------------------------------------------

/// The effective build target dir + its pool slot for one worktree. Both
/// halves are independently `Option` because the dir can be known while the
/// slot is genuinely inapplicable (an in-tree `target/` belongs to no pool).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BuildTarget {
    pub dir: Option<String>,
    pub slot: Option<String>,
}

/// The directory a `cargo` invocation for this worktree runs FROM, which is
/// the base a **relative** `CARGO_TARGET_DIR` resolves against.
///
/// This fleet's builds go through `cargo-guard.sh`, which `cd`s to
/// `<repo>/src-tauri` before exec'ing cargo and passes slot dirs as
/// `CARGO_TARGET_DIR=../target-pool/slot-N` — so `src-tauri` is the correct
/// base here, not the workspace root. `None` when the worktree is not a cargo
/// project at all (no `Cargo.toml` at either candidate root); that `None` is
/// what makes a Python/TS repo like `qontinui-web` report no build target
/// rather than a fabricated `<worktree>/target`.
fn cargo_invocation_dir_for(worktree: &Path) -> Option<PathBuf> {
    let src_tauri = worktree.join("src-tauri");
    if src_tauri.join("Cargo.toml").is_file() {
        return Some(src_tauri);
    }
    if worktree.join("Cargo.toml").is_file() {
        return Some(worktree.to_path_buf());
    }
    None
}

/// Lexically normalize `.` / `..` components WITHOUT touching the filesystem.
///
/// Deliberately not `Path::canonicalize`: the target dir routinely does not
/// exist yet (a slot that has never been built into), and on Windows
/// `canonicalize` returns a `\\?\` verbatim prefix that would not match the
/// path shape every other census field uses. A `..` that would escape the
/// root is dropped, matching cargo's own path joining.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a real directory name; never pop the prefix/root.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    // Nothing sensible to pop (already at root, or the path
                    // is relative and starts with `..`) — keep it verbatim
                    // so the caller's `is_absolute` check can still reject.
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `raw` against `base` when relative, then lexically normalize.
fn absolutize(raw: &str, base: &Path) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        lexical_normalize(p)
    } else {
        lexical_normalize(&base.join(p))
    }
}

/// The target-dir override configured by a `.cargo/config.toml` inside this
/// worktree — the "repo's configured pool slot" tier.
///
/// Walks from `from` (the cargo invocation dir) up to and INCLUDING
/// `worktree_root`, nearest-first, and takes the first config file that names
/// a target dir. Within one file, `[env] CARGO_TARGET_DIR` outranks
/// `[build] target-dir`, matching cargo's own precedence (the env table feeds
/// the process environment, which beats the build table).
///
/// Path resolution follows cargo: `build.target-dir` is relative to the
/// directory CONTAINING the `.cargo` dir, while an `[env]` value is relative
/// to that same directory only when the entry sets `relative = true` —
/// otherwise it behaves like a shell-set variable and resolves against the
/// invocation dir (`env_base`).
///
/// **The walk STOPS at the worktree root**, so neither `$CARGO_HOME`
/// (`~/.cargo/config.toml`) nor any config above the repo is consulted, even
/// though cargo itself would merge them. That is deliberate on both counts:
///
/// * those files are machine-global, so honouring one would stamp a single
///   directory onto every worktree on the box — the wrong-attribution failure
///   mode this whole field exists to avoid; and
/// * an unbounded walk makes the resolution depend on where the checkout
///   happens to sit. A worktree under the user profile would silently pick up
///   `~/.cargo/config.toml` while one on another volume would not, which is a
///   difference in LOCATION masquerading as a difference in configuration.
///
/// The cost of the bound is that a genuinely machine-global `target-dir`
/// override reports the in-tree fallback instead. Nothing in this fleet sets
/// one (the checked-in `src-tauri/.cargo/config.toml` configures sccache and
/// rustflags only), and the fleet's actual slot routing arrives as an
/// environment variable from `cargo-guard.sh`, which is tier 1 and unaffected.
fn cargo_config_target_dir(from: &Path, worktree_root: &Path, env_base: &Path) -> Option<PathBuf> {
    for dir in from.ancestors() {
        let at_root = dir == worktree_root;
        let cargo_dir = dir.join(".cargo");
        for name in ["config.toml", "config"] {
            let file = cargo_dir.join(name);
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            // `toml::from_str`, NOT `text.parse::<toml::Value>()`. In toml
            // 1.x `FromStr for Value` deserializes a TOML *value expression*
            // (`42`, `{a = 1}`) via `ValueDeserializer`, while `from_str`
            // goes through `Deserializer::parse` and reads a *document*. The
            // `parse` form compiles and type-checks identically, then fails
            // at runtime on the very first `[table]` header — which is every
            // real `.cargo/config.toml`. It silently degraded this whole tier
            // to "no override"; the two `cargo_config_*` tests below are what
            // caught it.
            let Ok(value) = toml::from_str::<toml::Value>(&text) else {
                // A malformed config is UNKNOWN, not "no override" — but the
                // honest degrade here is to keep walking rather than to
                // invent a value; the caller's final fallback stays the
                // in-tree target, which is what cargo would also use if it
                // could not read this file.
                continue;
            };

            // Tier A — `[env] CARGO_TARGET_DIR`, string or table form.
            if let Some(entry) = value.get("env").and_then(|e| e.get("CARGO_TARGET_DIR")) {
                let (raw, relative) = match entry {
                    toml::Value::String(s) => (Some(s.as_str()), false),
                    toml::Value::Table(t) => (
                        t.get("value").and_then(|v| v.as_str()),
                        t.get("relative").and_then(|v| v.as_bool()).unwrap_or(false),
                    ),
                    _ => (None, false),
                };
                if let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) {
                    let base = if relative { dir } else { env_base };
                    return Some(absolutize(raw, base));
                }
            }

            // Tier B — `[build] target-dir`, always relative to `dir`.
            if let Some(raw) = value
                .get("build")
                .and_then(|b| b.get("target-dir"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(absolutize(raw, dir));
            }

            // This config file exists but names no target dir — cargo would
            // keep merging upward, so we do too (within the bound).
        }
        if at_root {
            break;
        }
    }
    None
}

/// Extract the build-pool slot id from an already-resolved target dir.
///
/// Recognises this fleet's pool layout — `<repo>/target-pool/<slot>` where
/// `<slot>` is `slot-<N>` or the `lkg` (last-known-good) slot. Returns the
/// bare slot name, lowercased (Windows paths are case-insensitive, so the
/// same slot must not key two ways in coord).
///
/// Anything else is `None`, including a `target-pool` child that is not a
/// recognised slot name. A slot is NEVER synthesised from a non-pool path:
/// the consumer is a deletion engine and a fabricated slot id would let it
/// charge one worktree's bytes to another.
fn build_slot_for(target_dir: &Path) -> Option<String> {
    use std::path::Component;
    let mut comps = target_dir.components();
    while let Some(c) = comps.next() {
        let Component::Normal(name) = c else { continue };
        if !name.to_string_lossy().eq_ignore_ascii_case("target-pool") {
            continue;
        }
        let Some(Component::Normal(slot)) = comps.next() else {
            return None;
        };
        let slot = slot.to_string_lossy().to_ascii_lowercase();
        let is_numbered = slot
            .strip_prefix("slot-")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if is_numbered || slot == "lkg" {
            return Some(slot);
        }
        return None;
    }
    None
}

/// Resolve `(build_target_dir, build_slot)` for one worktree.
///
/// Precedence for the DIR, highest first:
///
/// 1. `env_target_dir` — the `CARGO_TARGET_DIR` the census process sees.
/// 2. The repo's configured target dir from a `.cargo/config.toml` between
///    the cargo invocation dir and the worktree root inclusive
///    ([`cargo_config_target_dir`], which explains why the walk stops there)
///    — the channel through which a repo is pinned to a pool slot.
/// 3. The in-tree dir [`target_dir_for`] already measures for `target_bytes`,
///    so the two fields describe the SAME directory in the common case.
///
/// Both halves are `None` when the worktree is not a cargo project, and the
/// dir is `None` when resolution somehow yields a non-absolute path — a
/// relative path cannot be attributed to disk, and a guess is worse than a
/// null here.
///
/// **Scope caveat, deliberately disclosed:** tier 1 reads the CENSUS
/// PROCESS's environment, not the environment of the shell that actually ran
/// the build in that worktree — no such per-worktree record exists on disk
/// (`cargo-guard.sh` sets `CARGO_TARGET_DIR` per invocation and leaves no
/// trace). Tiers 2 and 3 are genuinely per-worktree. In the normal fleet
/// configuration the runner is not launched with `CARGO_TARGET_DIR` set, so
/// tier 1 is absent and the value is per-worktree throughout; if an operator
/// DOES export it before launching the runner, every row on that device will
/// report that one dir, which is honest about what the process can observe
/// but is machine-scoped rather than worktree-scoped.
pub(crate) fn resolve_build_target(worktree: &Path, env_target_dir: Option<&str>) -> BuildTarget {
    let Some(invocation_dir) = cargo_invocation_dir_for(worktree) else {
        // Not a cargo project — there is no build target to attribute.
        return BuildTarget::default();
    };

    let dir = env_target_dir
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|raw| absolutize(raw, &invocation_dir))
        .or_else(|| cargo_config_target_dir(&invocation_dir, worktree, &invocation_dir))
        .unwrap_or_else(|| lexical_normalize(&target_dir_for(worktree)));

    if !dir.is_absolute() {
        return BuildTarget::default();
    }

    BuildTarget {
        dir: Some(normalize_path_str(&dir)),
        slot: build_slot_for(&dir),
    }
}

/// [`resolve_build_target`] against the live process environment.
fn resolve_build_target_live(worktree: &Path) -> BuildTarget {
    let env = std::env::var("CARGO_TARGET_DIR").ok();
    resolve_build_target(worktree, env.as_deref())
}

// ---------------------------------------------------------------------------
// Volume free space (sysinfo — already a dependency, has the `disk` feature).
// ---------------------------------------------------------------------------

/// Build {volume, total_bytes, free_bytes} for each distinct drive
/// letter among the worktree paths. Uses `sysinfo::Disks` (already a
/// runner dependency with the `disk` feature) rather than a raw
/// `GetDiskFreeSpaceExW` binding — it gives total + available per mount
/// portably. We map each worktree's drive letter to the sysinfo disk
/// whose mount point covers it.
///
/// This is the CENSUS-WALK collector and stays path-scoped on purpose: a
/// census row is attributed to the volume its worktree sits on, and the
/// low-disk alarm on that path should not fire for an unrelated mount the
/// runner never writes to. The product question — "how much disk is left,
/// anywhere?" — is [`collect_all_volumes`], sampled on its own tick.
fn collect_volumes(worktree_paths: &[PathBuf]) -> Vec<VolumeReport> {
    // Distinct drive letters (uppercased, with colon) among the paths.
    let mut wanted: HashSet<String> = HashSet::new();
    for p in worktree_paths {
        if let Some(vol) = drive_letter_of(p) {
            wanted.insert(vol);
        }
    }
    if wanted.is_empty() {
        return Vec::new();
    }
    volumes_from_mounts(&mounts(), Some(&wanted))
}

/// Build a [`VolumeReport`] for **every mounted volume**, whether or not a
/// worktree lives on it.
///
/// [`collect_volumes`] answers "how much room is left on the volumes these
/// worktrees sit on", which is the right question for attributing a census
/// row and the wrong one for the product requirement — "see available disk
/// space" is not "…on the volumes we happen to use". A volume with no
/// worktree on it (a second data drive, a mounted NAS share) was invisible.
///
/// Cross-platform: keyed on the drive letter where the mount has one and on
/// the mount point itself otherwise, so POSIX mounts (`/`, `/home`) are
/// reported rather than silently dropped.
///
/// An EMPTY return is a blind probe, not an empty machine — see the module
/// doc's invariant 2.
pub(crate) fn collect_all_volumes() -> Vec<VolumeReport> {
    volumes_from_mounts(&mounts(), None)
}

/// Every mounted volume as `(mount_point, total_bytes, available_bytes)`.
///
/// Delegates to [`crate::ci_node::admission::enumerate_mounts`] — the runner's
/// single `sysinfo::Disks` enumeration site — so the free-space number this
/// module publishes and the one the CI-node admission floor trips on are
/// literally the same reading.
fn mounts() -> Vec<(PathBuf, u64, u64)> {
    crate::ci_node::admission::enumerate_mounts()
}

/// PURE mapping of `(mount_point, total_bytes, free_bytes)` mounts into
/// [`VolumeReport`]s, optionally filtered to a set of volume keys. Split out
/// so both collectors are unit-testable without touching a real disk.
///
/// Two entries mapping to the same key keep the FIRST (deterministic via the
/// `BTreeMap`), and a mount reporting `total_bytes == 0` is DROPPED: an empty
/// optical drive or an unmounted card reader is an unreadable volume, and
/// rendering it as "0 of 0 bytes free" would be a fabricated zero.
///
/// "Same key" means the same VOLUME, not the same drive letter — see
/// [`volume_key`]. Windows reports one physical volume twice with differing
/// case (`D:\` and `d:\`), which is a genuine duplicate and still collapses;
/// a volume mounted into a folder (`D:\data`) is a DIFFERENT volume that
/// merely borrows `D:`'s letter, and must survive.
///
/// `wanted` (the census walk's path-scoped filter) matches a mount by its key
/// OR by its drive-letter label, because the filter set is built from
/// worktree paths via [`drive_letter_of`] and a worktree under `D:\data\...`
/// resolves to the letter `D:`. Matching the label keeps the folder-mounted
/// volume that actually holds those worktrees in the census's attribution set
/// instead of silently reporting only the letter's root volume.
fn volumes_from_mounts(
    mounts: &[(PathBuf, u64, u64)],
    wanted: Option<&HashSet<String>>,
) -> Vec<VolumeReport> {
    let mut out: BTreeMap<String, VolumeReport> = BTreeMap::new();
    for (mount, total, free) in mounts {
        if *total == 0 {
            continue;
        }
        let drive_letter = drive_letter_of(mount);
        let key = volume_key(mount);
        if let Some(w) = wanted {
            let matched = w.contains(&key)
                || drive_letter
                    .as_ref()
                    .is_some_and(|letter| w.contains(letter));
            if !matched {
                continue;
            }
        }
        out.entry(volume_dedup_key(&key, drive_letter.is_some()))
            .or_insert_with(|| VolumeReport {
                volume: key,
                drive_letter,
                total_bytes: *total,
                free_bytes: *free,
            });
    }
    out.into_values().collect()
}

/// The stable key a mount is reported under: its Windows drive letter
/// (`"D:"`) when the mount IS that drive's ROOT, else the mount point itself,
/// separator-normalized (`"D:/data"`, `"/"`, `"/mnt/x"`).
///
/// ## Why not the drive letter alone
///
/// It used to be, and that DROPPED real volumes. Mounting a volume into an
/// empty NTFS folder is a normal Windows way to attach a large data disk
/// (`D:\data`), and `sysinfo` enumerates it as its own mount with its own
/// capacity. Collapsing it to `"D:"` collided it with `D:\`, and
/// [`volumes_from_mounts`]'s keep-the-first rule then discarded it — no log,
/// no marker, a whole disk simply absent from a disk-monitoring feature.
/// That is exactly the fabricated absence this module's invariant 2 forbids,
/// so the mount point is the key and the letter rides along as a label.
fn volume_key(mount: &Path) -> String {
    match drive_letter_of(mount) {
        Some(letter) if is_drive_root(mount) => letter,
        _ => normalize_path_str(mount),
    }
}

/// The map key used to collapse genuine duplicates. Windows paths are
/// case-insensitive, so `d:\` and `D:\` are one volume; POSIX mount points are
/// NOT, so `/mnt/Data` and `/mnt/data` stay distinct.
fn volume_dedup_key(key: &str, windows_path: bool) -> String {
    if windows_path {
        key.to_lowercase()
    } else {
        key.to_string()
    }
}

/// True when a drive-lettered path is the drive's ROOT (`D:\`, `D:/`, `D:`)
/// rather than a folder on it (`D:\data`).
fn is_drive_root(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let Some(rest) = s.get(2..) else {
        return false;
    };
    rest.chars().all(|c| c == '\\' || c == '/')
}

/// Extract the `"D:"`-style drive letter from a path. `None` for paths
/// without a Windows drive prefix (e.g. POSIX paths in CI tests).
fn drive_letter_of(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    // Windows: `D:\...` / `D:/...` / `D:`.
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(format!("{}:", (bytes[0] as char).to_ascii_uppercase()));
    }
    None
}

// ---------------------------------------------------------------------------
// Worktree enumeration.
// ---------------------------------------------------------------------------

/// Canonicalize for dedup; fall back to the raw path when canonicalize
/// fails (e.g. the dir was just removed). Lower-cases on Windows so
/// `D:\x` and `d:\x` dedup.
fn dedup_key(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// The emitted `path` field, separator-normalized to forward slashes. Worktrees
/// found via the sibling-dir scan come back as `PathBuf::join` results (Windows
/// `\`), while git-listed ones use `/`; without this a single worktree could be
/// reported with mixed separators (`D:/qontinui-root\qontinui-runner-wt-x`),
/// inflating the coord-side `DISTINCT ON (device, repo, path)` set and confusing
/// path-string matching. Cosmetic on Windows (the APIs accept both) but keeps
/// the twin's data clean + stable.
fn normalize_path_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// True iff `path` holds a real git CLONE — i.e. `.git` is a DIRECTORY.
///
/// This is the STRUCTURAL distinction between a repo root and a linked
/// worktree, and it holds whatever the dir is NAMED: `git worktree add` writes
/// `.git` as a FILE containing a `gitdir:` pointer, while a clone keeps it as a
/// directory holding the object store.
///
/// Naming is not a reliable proxy for it. On 2026-07-31 the census cleared
/// `qontinui-coord-wt-prcreate-fix` — a 26.5 GB real clone — for removal purely
/// because its name matched the worktree pattern. Every place that decides
/// "repo root or worktree?" keys on this function instead.
pub(crate) fn is_git_clone_root(path: &Path) -> bool {
    path.join(".git").is_dir()
}

/// True iff `path` is a CANONICAL repo checkout (a repo root) rather than a
/// worktree of one: a `qontinui-*` dir whose `.git` is a directory.
///
/// The test is structural [`is_git_clone_root`], not name-based. It subsumes
/// the `-wt-` / `-wt` name exclusions this predicate used to carry: a genuine
/// linked worktree has `.git` as a FILE and is excluded on that basis, so a
/// stray worktree dir still cannot be mis-treated as a repo root (the phantom
/// duplicate row under coord's `DISTINCT ON (device, repo, path)` read). What
/// the name test got WRONG, and this one gets right, is the converse: a real
/// clone that merely *looks* like a worktree is a repo root and is reported as
/// one.
pub(crate) fn is_canonical_repo_root(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("qontinui-"))
        && is_git_clone_root(path)
}

/// Run `git -C <canonical> worktree list --porcelain` and return the
/// `worktree <path>` lines as absolute paths. Best-effort: a non-git
/// dir or git failure yields an empty list.
fn git_registered_worktrees(canonical: &Path) -> Vec<PathBuf> {
    let canonical_str = match canonical.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let out = match crate::process_helpers::no_window("git")
        .args(["-C", canonical_str, "worktree", "list", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|p| PathBuf::from(p.trim()))
        .collect()
}

/// Sibling `<repo>-wt-*` dirs in the parent dir, plus per-repo
/// `.claude/worktrees/*` dirs. These catch worktrees not registered with
/// the canonical repo's `git worktree list`.
///
/// Both scans are NAME- and LOCATION-based, so both must reject a real clone
/// structurally: a dir named `<repo>-wt-<slug>` that carries its own object
/// store is a separate repository, never a worktree of `repo`. Without that
/// check the name match alone was enough to enter the reclaim candidate set —
/// which is how `qontinui-coord-wt-prcreate-fix` (a 26.5 GB clone) came to be
/// cleared for removal on 2026-07-31.
fn sibling_and_claude_worktrees(root: &Path, canonical: &Path, repo: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // Sibling `<repo>-wt-*` in the qontinui-root parent dir.
    let prefix = format!("{repo}-wt-");
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || is_git_clone_root(&path) {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(&prefix) {
                    out.push(path);
                }
            }
        }
    }

    // Per-repo `.claude/worktrees/*`.
    let claude_wts = canonical.join(".claude").join("worktrees");
    if let Ok(entries) = std::fs::read_dir(&claude_wts) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !is_git_clone_root(&path) {
                out.push(path);
            }
        }
    }

    out
}

/// Run a git query against a worktree, returning trimmed stdout on
/// success.
fn git_capture(worktree: &Path, args: &[&str]) -> Option<String> {
    let wt = worktree.to_str()?;
    let mut full: Vec<&str> = vec!["-C", wt];
    full.extend_from_slice(args);
    let out = crate::process_helpers::no_window("git")
        .args(&full)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Compute G2 `landed_in_main` for a worktree using local git only.
///
/// 1. `git merge-base --is-ancestor HEAD origin/main` exit 0 → `Some(true)`
///    (HEAD is on origin/main via a true merge / fast-forward).
/// 2. Else `git cherry origin/main HEAD`: if it emits ≥1 line and EVERY
///    line starts with `-`, every HEAD-unique commit has a patch-id
///    equivalent already on origin/main (rebase / cherry-pick) → `Some(true)`.
/// 3. Else `Some(false)` — there is genuinely-unlanded work.
/// 4. Any git failure / missing `origin/main` / detached oddity → `None`
///    (honest unknown; coord's gate treats `None` as not-landed).
///
/// Squash merges are deliberately NOT covered (the patch-id changes) —
/// coord handles those via the PR `close_cause='merged'` signal.
fn compute_landed_in_main(worktree: &Path) -> Option<bool> {
    // Require a resolvable origin/main ref; without it we can't answer.
    git_capture(
        worktree,
        &["rev-parse", "--verify", "--quiet", "origin/main"],
    )?;

    let wt = worktree.to_str()?;

    // (1) Ancestry test — exit 0 means HEAD is an ancestor of origin/main.
    if let Ok(status) = crate::process_helpers::no_window("git")
        .args([
            "-C",
            wt,
            "merge-base",
            "--is-ancestor",
            "HEAD",
            "origin/main",
        ])
        .status()
    {
        if status.success() {
            return Some(true);
        }
    } else {
        // git itself failed to spawn / run — honest unknown.
        return None;
    }

    // (2) Patch-id test via `git cherry`. Lines starting `-` are commits
    // whose patch-id already exists on the upstream; `+` lines are
    // genuinely-unlanded. ALL lines must be `-` (and there must be ≥1).
    let cherry = git_capture(worktree, &["cherry", "origin/main", "HEAD"])?;
    let mut saw_line = false;
    for line in cherry.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        saw_line = true;
        if !line.starts_with('-') {
            // A `+` (unlanded) line → not fully landed.
            return Some(false);
        }
    }
    if saw_line {
        // Every line was `-` → all HEAD-unique commits already upstream.
        return Some(true);
    }

    // (3) No cherry lines + not an ancestor: HEAD == origin/main tip would
    // have been caught by the ancestry test, so this is the no-info case —
    // treat as not landed (there is nothing showing it landed).
    Some(false)
}

/// Ξ_Worktree P7.3 — current branch of the canonical checkout
/// (`git symbolic-ref --short HEAD`). `None` on detached HEAD (the command
/// errors), an empty result, or any git failure.
pub(crate) fn compute_canonical_branch(canonical: &Path) -> Option<String> {
    git_capture(canonical, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty())
}

/// Ξ_Worktree P7.3 — dirty bit of the canonical checkout
/// (`git status --porcelain` non-empty). `Some(true)` when there are
/// uncommitted changes, `Some(false)` when clean, `None` on a git failure
/// (fail-OPEN to `None` is fine: coord reads `None` as unsafe). This is the
/// P1-clean precondition input for SharedBranch.
pub(crate) fn compute_canonical_is_dirty(canonical: &Path) -> Option<bool> {
    git_capture(canonical, &["status", "--porcelain"]).map(|s| !s.trim().is_empty())
}

/// Ξ_Worktree P7.3 — advisory base-divergence summary for the canonical
/// checkout. Never errors the census; falls back gracefully:
///
/// * On `main` → `Some("on:main")`.
/// * Otherwise, with `origin/main` resolvable →
///   `Some("on:<branch>;<behind>\t<ahead>")` from
///   `git rev-list --count --left-right origin/main...HEAD`.
/// * Otherwise (missing `origin/main`, detached HEAD, git failure) → just
///   the branch name `Some("on:<branch>")`, or `None` if even the branch is
///   unresolvable.
fn compute_canonical_base_divergence(canonical: &Path) -> Option<String> {
    let branch = compute_canonical_branch(canonical)?;
    if branch == "main" {
        return Some("on:main".to_string());
    }
    // Best-effort ahead/behind vs origin/main. `--left-right` on the
    // symmetric-difference `A...B` prints `<behind>\t<ahead>`. A missing
    // origin/main makes this fail → fall back to just the branch name.
    match git_capture(
        canonical,
        &["rev-list", "--count", "--left-right", "origin/main...HEAD"],
    ) {
        Some(lr) if !lr.is_empty() => Some(format!("on:{branch};{lr}")),
        _ => Some(format!("on:{branch}")),
    }
}

/// Build the census row for a single worktree dir.
fn capture_worktree(repo: &str, worktree: &Path) -> WorktreeCensus {
    let branch =
        git_capture(worktree, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty());
    let head_sha = git_capture(worktree, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());

    let head_age_secs = git_capture(worktree, &["log", "-1", "--format=%ct"])
        .and_then(|s| s.parse::<i64>().ok())
        .map(|committed| chrono::Utc::now().timestamp().saturating_sub(committed));

    // is_dirty — reclaim-scoped: `git status --porcelain` MINUS the runner's
    // own untracked scaffolding (`.claude/`, `.coord-mcp-status`, `.mcp.json`).
    // Plain non-emptiness marked ~34% of agent worktrees dirty forever purely
    // because the runner provisioned them, which made them permanently
    // unreclaimable. See `super::dirty` for the full rationale and scope.
    let is_dirty = git_capture(worktree, &["status", "--porcelain"])
        .map(|s| super::dirty::porcelain_is_dirty(&s))
        .unwrap_or(false);

    let (nm_present, nm_is_junction, nm_bytes) = measure_dir(&worktree.join("node_modules"));
    let (target_present, target_is_junction, target_bytes) = measure_dir(&target_dir_for(worktree));

    // Run provenance: WHICH dir this worktree's builds write to, and whether
    // that dir is a shared pool slot. Filesystem-only + cheap (a couple of
    // `is_file` probes plus at most one small TOML parse) — no git spawn.
    let build_target = resolve_build_target_live(worktree);

    let last_access_mtime = std::fs::metadata(worktree)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());

    let attributable_bytes = nm_bytes.saturating_add(target_bytes);

    // G2: ancestry/patch-id "landed in origin/main" — local git only,
    // None when undeterminable (no origin/main, git failure).
    let landed_in_main = compute_landed_in_main(worktree);

    // G6 shadow-mode: held-cargo-lock-only build probe for the FLEET census
    // (NOT the reclaim executor's `probe_building`, which also counts recent
    // file activity — editor saves / git ops are not builds and over-reported
    // coord's build-concurrency gauge). Phase 2 of
    // `plans/2026-06-08-coord-build-slot-budget-saturation-fix.md`.
    let building = Some(super::reclaim::probe_building_for_census(worktree));

    // Ξ_Worktree P7.3 — canonical-checkout state (per-REPO, not per-worktree;
    // it's fine that every worktree row of a repo carries the same values —
    // coord reads one row per repo). Resolve the canonical path for this
    // repo; a missing/unresolvable canonical → all three facts are None
    // (`.and_then` off the canonical Option), which coord reads as unsafe.
    let canonical = super::canonical_paths::default_canonical_path(repo).ok();
    let canonical_current_branch = canonical.as_deref().and_then(compute_canonical_branch);
    let canonical_is_dirty = canonical.as_deref().and_then(compute_canonical_is_dirty);
    let canonical_base_divergence = canonical
        .as_deref()
        .and_then(compute_canonical_base_divergence);

    WorktreeCensus {
        repo: repo.to_string(),
        path: normalize_path_str(worktree),
        branch,
        head_sha,
        head_age_secs,
        is_dirty,
        nm_present,
        nm_is_junction,
        nm_bytes,
        target_present,
        target_is_junction,
        target_bytes,
        build_target_dir: build_target.dir,
        build_slot: build_target.slot,
        last_access_mtime,
        attributable_bytes,
        landed_in_main,
        building,
        canonical_current_branch,
        canonical_is_dirty,
        canonical_base_divergence,
    }
}

/// Enumerate every worktree under `root`, dedup by canonical path, build a
/// census row for each, and hand each row to `on_row` as soon as it is
/// captured (the chunked-emission feed — a row costs ~2s of git spawns, so
/// push-per-row is what lets chunks flow DURING the walk).
fn enumerate_worktrees_with(root: &Path, on_row: &mut dyn FnMut(WorktreeCensus)) {
    // Discover the governed repos: every top-level `qontinui-*` dir with
    // a `.git` (matches fleet::tree_publisher's notion of a governed
    // repo, but here it's the canonical checkout we anchor the worktree
    // search on).
    let mut repo_roots: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Canonical checkout: `qontinui-*` whose `.git` is a DIRECTORY.
            // Linked worktrees are excluded structurally (their `.git` is a
            // file) — they're discovered as worktrees OF their repo, not as
            // repos themselves.
            if is_canonical_repo_root(&path) {
                repo_roots.push((name, path));
            }
        }
    }

    let mut seen: HashSet<String> = HashSet::new();

    for (repo, canonical) in &repo_roots {
        // Also try the canonical-path resolver so a repo whose dir name
        // differs from its slug still anchors correctly. (For the flat
        // layout the two agree.)
        let _ = default_canonical_path(repo);

        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(git_registered_worktrees(canonical));
        candidates.extend(sibling_and_claude_worktrees(root, canonical, repo));
        // The canonical tree itself is reported too (it's a worktree of
        // the repo and its node_modules/target footprint matters).
        candidates.push(canonical.clone());

        for wt in candidates {
            if !wt.is_dir() {
                continue;
            }
            let key = dedup_key(&wt);
            if !seen.insert(key) {
                continue;
            }
            on_row(capture_worktree(repo, &wt));
        }
    }
}

// ---------------------------------------------------------------------------
// The walk + spawn.
// ---------------------------------------------------------------------------

/// The census walk, chunked: enumerate every worktree under `root`, streaming
/// each row through a [`ChunkEmitter`] onto `tx` (a chunk every
/// `QONTINUI_CENSUS_CHUNK_ROWS` rows or `QONTINUI_CENSUS_CHUNK_SECS` seconds,
/// whichever first), then compute the volume reports and emit them on the
/// FINAL chunk only. Returns the complete census body for the walk-completion
/// snapshot replace.
///
/// Synchronous — always run on the blocking pool ([`build_and_publish`] is
/// the only caller and the only place a walk starts).
fn build_census_chunked(
    root: &Path,
    device_id: Uuid,
    tenant_id: Option<Uuid>,
    tx: tokio::sync::mpsc::UnboundedSender<CensusChunk>,
) -> WorktreeCensusReq {
    let mut emitter = ChunkEmitter::new(
        Chunker::new(
            census_chunk_rows(),
            Duration::from_secs(census_chunk_secs()),
        ),
        tx,
    );
    let mut worktrees: Vec<WorktreeCensus> = Vec::new();
    enumerate_worktrees_with(root, &mut |row| {
        worktrees.push(row.clone());
        emitter.on_row(row);
    });

    let paths: Vec<PathBuf> = worktrees.iter().map(|w| PathBuf::from(&w.path)).collect();
    let volumes = collect_volumes(&paths);
    // Low-free-space alarm — a LEADING signal so low disk surfaces here rather
    // than as phantom cargo build failures (`os error 112`). Runs on every
    // completed walk regardless of coord reachability. Read-only. The walk is
    // multi-hour, so it is UNTHROTTLED — only the 60 s publisher needs a rate
    // limit — and the "did it emit" answer is therefore unused here.
    let _emitted = warn_on_low_disk(&volumes);
    emitter.finish(volumes.clone());

    WorktreeCensusReq {
        device_id,
        tenant_id,
        volumes,
        worktrees,
    }
}

/// Env override (bytes) for the low-disk WARN floor. Default ~100 GB.
pub const LOW_DISK_WARN_BYTES_ENV: &str = "COORD_LOW_DISK_WARN_BYTES";
/// Env override (bytes) for the critical-disk ERROR floor. Default ~25 GB.
pub const LOW_DISK_CRIT_BYTES_ENV: &str = "COORD_LOW_DISK_CRIT_BYTES";
const DEFAULT_LOW_DISK_WARN_BYTES: u64 = 100 * 1024 * 1024 * 1024;
const DEFAULT_LOW_DISK_CRIT_BYTES: u64 = 25 * 1024 * 1024 * 1024;

fn env_bytes(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Emit a `warn!`/`error!` per volume whose free space is under the floor. A
/// leading signal for the disk-full condition that otherwise only manifests as
/// phantom cargo build failures. Read-only; best-effort logging only.
///
/// Returns whether it ACTUALLY emitted anything. The throttled wrapper needs
/// that answer: stamping the throttle epoch on a healthy (silent) tick lets a
/// routine 60 s sample consume the alarm's budget and suppress the FIRST
/// CRITICAL line of a real emergency — see
/// [`warn_on_low_disk_throttled`].
#[must_use]
fn warn_on_low_disk(volumes: &[VolumeReport]) -> bool {
    let warn_floor = env_bytes(LOW_DISK_WARN_BYTES_ENV, DEFAULT_LOW_DISK_WARN_BYTES);
    let crit_floor = env_bytes(LOW_DISK_CRIT_BYTES_ENV, DEFAULT_LOW_DISK_CRIT_BYTES);
    const GB: f64 = 1_073_741_824.0;
    let mut emitted = false;
    for v in volumes {
        let free_gb = v.free_bytes as f64 / GB;
        let pct = if v.total_bytes > 0 {
            v.free_bytes as f64 / v.total_bytes as f64 * 100.0
        } else {
            100.0
        };
        if v.free_bytes < crit_floor {
            tracing::error!(
                "worktree_census: CRITICAL low disk on {} — {:.1} GB free ({:.1}%). \
                 Builds will manufacture phantom failures (os error 112). Arm \
                 COORD_ORPHAN_TARGET_REAP_ENABLED and/or reclaim target dirs.",
                v.volume,
                free_gb,
                pct
            );
            emitted = true;
        } else if v.free_bytes < warn_floor {
            warn!(
                "worktree_census: low disk on {} — {:.1} GB free ({:.1}%).",
                v.volume, free_gb, pct
            );
            emitted = true;
        }
    }
    emitted
}

// ---------------------------------------------------------------------------
// The decoupled volume publisher (disk-monitoring Phase 1, step 1 + 2).
// ---------------------------------------------------------------------------

/// One free-space reading of every mounted volume, with the instant it was
/// taken. Held process-locally so the runner's own surfaces can answer the
/// disk question with NO dependency on coord (INV-D1).
#[derive(Debug, Clone)]
pub(crate) struct VolumeSample {
    pub(crate) volumes: Vec<VolumeReport>,
    pub(crate) taken_at: chrono::DateTime<chrono::Utc>,
}

/// The most recent volume sample, or `None` before the first one lands.
/// `None` is UNKNOWN — never "no disks" and never "0 bytes free".
static LATEST_VOLUMES: OnceLock<RwLock<Option<VolumeSample>>> = OnceLock::new();

fn latest_volumes_cell() -> &'static RwLock<Option<VolumeSample>> {
    LATEST_VOLUMES.get_or_init(|| RwLock::new(None))
}

/// The latest volume sample. NEVER blocks, never probes, never fabricates —
/// a caller that gets `None` must render UNKNOWN.
pub(crate) fn latest_volume_sample() -> Option<VolumeSample> {
    latest_volumes_cell()
        .read()
        .ok()
        .and_then(|g| g.as_ref().cloned())
}

/// Publish a sample locally. Called BEFORE any network attempt so a coord
/// outage costs the reading nothing.
fn publish_volume_sample(volumes: Vec<VolumeReport>) -> VolumeSample {
    let sample = VolumeSample {
        volumes,
        taken_at: chrono::Utc::now(),
    };
    if let Ok(mut g) = latest_volumes_cell().write() {
        *g = Some(sample.clone());
    }
    sample
}

/// "Nothing has been emitted yet" for the throttle cells below.
///
/// NOT `0`: zero is a perfectly well-formed clock reading (the Unix epoch, and
/// the natural starting point of any injected test clock), so using it as the
/// sentinel makes the first emission at `now == 0` stamp a value that reads
/// back as "never" — and the throttle then lets every subsequent call through.
/// `u64::MAX` is unreachable as a stamp (it is year 584-billion).
const THROTTLE_NEVER: u64 = u64::MAX;

/// Epoch-seconds of the last low-disk log emission from the publisher.
static LAST_LOW_DISK_LOG_EPOCH: AtomicU64 = AtomicU64::new(THROTTLE_NEVER);

/// Epoch-seconds of the last empty-probe warning from the publisher.
static LAST_EMPTY_PROBE_LOG_EPOCH: AtomicU64 = AtomicU64::new(THROTTLE_NEVER);

/// Minimum spacing between empty-probe warnings. The condition it reports —
/// no readable mounts at all — is a STANDING state on a machine that has it
/// (a container with no mounted host volumes), not an event, so an
/// unthrottled multi-line `warn!` every 60 s is pure log flood, forever.
const EMPTY_PROBE_LOG_THROTTLE_SECS: u64 = LOW_DISK_LOG_THROTTLE_SECS;

/// Compile-time: a throttle at or below the publisher's cadence throttles
/// nothing — every tick would clear the window and the flood returns.
const _: () = assert!(
    EMPTY_PROBE_LOG_THROTTLE_SECS > MIN_VOLUME_INTERVAL_SECS
        && LOW_DISK_LOG_THROTTLE_SECS > MIN_VOLUME_INTERVAL_SECS,
    "both log throttles must exceed the publisher's minimum cadence"
);

fn now_epoch_secs() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

/// Rate-limit a log emission to one per `throttle_secs`, stamping the budget
/// ONLY when `emit` reports that it actually wrote something.
///
/// That conditional stamp is the whole point, and its absence was a real
/// suppression bug: the previous code stamped the epoch BEFORE calling
/// [`warn_on_low_disk`], which emits nothing when every volume is above the
/// floor. A healthy boot at t=0 therefore burned the budget, and a disk that
/// crossed the CRITICAL floor at t=301 s hit `now - last = 1 < 300` and had
/// its FIRST `error!` DROPPED — the alarm arrived ~5 minutes late, at exactly
/// the moment it mattered most. A silent (healthy) tick must cost nothing.
///
/// Returns whether the emission happened, for the tests to pin.
fn emit_throttled(
    cell: &AtomicU64,
    now: u64,
    throttle_secs: u64,
    emit: impl FnOnce() -> bool,
) -> bool {
    let last = cell.load(Ordering::Acquire);
    if last != THROTTLE_NEVER && now.saturating_sub(last) < throttle_secs {
        return false;
    }
    if emit() {
        cell.store(now, Ordering::Release);
        true
    } else {
        false
    }
}

/// [`warn_on_low_disk`], rate-limited to [`LOW_DISK_LOG_THROTTLE_SECS`].
/// The alarm is a leading signal, not a heartbeat: at the publisher's 60 s
/// cadence an unthrottled `error!` would bury the very logs used to diagnose
/// the disk emergency it is announcing.
fn warn_on_low_disk_throttled(volumes: &[VolumeReport]) {
    emit_throttled(
        &LAST_LOW_DISK_LOG_EPOCH,
        now_epoch_secs(),
        LOW_DISK_LOG_THROTTLE_SECS,
        || warn_on_low_disk(volumes),
    );
}

/// The empty-probe warning, rate-limited to
/// [`EMPTY_PROBE_LOG_THROTTLE_SECS`]. Always "emits" when the throttle lets
/// it through, so unlike the low-disk alarm it does consume the budget every
/// time it is reached — which is correct: reaching it IS the condition.
fn warn_on_empty_probe_throttled() {
    emit_throttled(
        &LAST_EMPTY_PROBE_LOG_EPOCH,
        now_epoch_secs(),
        EMPTY_PROBE_LOG_THROTTLE_SECS,
        || {
            warn!(
                "worktree_census: volume probe returned no mounted volumes — keeping the \
                 previous sample. This reading is UNKNOWN, not zero free space."
            );
            true
        },
    );
}

/// Take ONE volume sample: probe every mount, publish it locally, log the
/// low-disk alarm (throttled), then POST a volumes-only census body to coord.
///
/// Returns the published sample, or `None` when the probe produced nothing —
/// in which case the previous sample is deliberately LEFT IN PLACE and no
/// empty reading is published (invariant 2: absent is UNKNOWN, not zero).
///
/// Nothing here consults arming, coord reachability, build state or the
/// census walk: this is the measurement half of INV-D1 and it must answer
/// mid-build, mid-emergency and offline.
pub(crate) async fn sample_and_publish_volumes() -> Option<VolumeSample> {
    // sysinfo's enumeration is a syscall per mount and can block on an
    // unresponsive network drive, so it never runs on the shared publishers
    // runtime's single async worker.
    let volumes = match tokio::task::spawn_blocking(collect_all_volumes).await {
        Ok(v) => v,
        Err(e) => {
            warn!("worktree_census: volume probe task failed: {e} — keeping the previous sample");
            return None;
        }
    };
    if volumes.is_empty() {
        // Throttled: on a machine with no readable mounts this is a standing
        // condition, and the publisher ticks every 60 s forever.
        warn_on_empty_probe_throttled();
        return None;
    }
    let sample = publish_volume_sample(volumes);
    warn_on_low_disk_throttled(&sample.volumes);
    post_volumes_to_coord(sample.volumes.clone()).await;
    Some(sample)
}

/// What a cached [`ChunkPoster`] was built against. A change in ANY component
/// (an enrolled device, a tenant switch, an active-profile switch that moves
/// the coord base, or coord being configured/unconfigured) rebuilds the
/// poster; an unchanged key reuses it, TLS connection pool included.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumePosterKey {
    device_id: Uuid,
    tenant_id: Option<Uuid>,
    base: Option<String>,
}

/// A [`ChunkPoster`] plus the key it was built from.
struct CachedVolumePoster {
    key: VolumePosterKey,
    poster: ChunkPoster,
}

/// Outcome of one resolution pass on the blocking pool.
enum VolumePosterResolution {
    /// The cached poster is still valid for the current identity/base.
    Reuse,
    /// Identity or base moved (or there was no cache) — here is a fresh one.
    Rebuilt(Box<CachedVolumePoster>),
    /// No `device_id`: nothing to POST as. The cache is cleared so a later
    /// enrollment is picked up.
    NoIdentity,
}

/// The process-wide volume poster, built at most once per identity/base.
static VOLUME_POSTER: OnceLock<tokio::sync::Mutex<Option<CachedVolumePoster>>> = OnceLock::new();

fn volume_poster_cell() -> &'static tokio::sync::Mutex<Option<CachedVolumePoster>> {
    VOLUME_POSTER.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Resolve identity + destination, reusing `prev` when nothing moved.
///
/// SYNCHRONOUS by design and called only from `spawn_blocking`: every step is
/// blocking work — two `std::fs::read` of `~/.qontinui/machine.json`
/// ([`load_device_id`], [`resolve_tenant_id`]), the active-profile read behind
/// [`connected_coord_base`], the `owns_shared_root_state()` probe, and a
/// `reqwest::Client` build that loads the OS trust store.
fn resolve_volume_poster(prev: Option<VolumePosterKey>) -> VolumePosterResolution {
    let Some(device_id) = load_device_id() else {
        return VolumePosterResolution::NoIdentity;
    };
    let key = VolumePosterKey {
        device_id,
        tenant_id: resolve_tenant_id(),
        base: connected_coord_base(),
    };
    if !volume_poster_needs_rebuild(prev.as_ref(), &key) {
        return VolumePosterResolution::Reuse;
    }
    let poster = ChunkPoster::new(device_id, key.tenant_id);
    VolumePosterResolution::Rebuilt(Box::new(CachedVolumePoster { key, poster }))
}

/// PURE cache decision: rebuild only when there is no cached poster or the
/// identity/base it was built against moved. Split out so the reuse property
/// — the whole point of R3 — is testable without a machine.json, a profile or
/// a coord.
fn volume_poster_needs_rebuild(prev: Option<&VolumePosterKey>, next: &VolumePosterKey) -> bool {
    prev != Some(next)
}

/// Ship one volumes-only body to coord. Best-effort and fully optional: a
/// missing identity, an unconfigured coord, a secondary instance or a failed
/// POST all leave the LOCAL sample untouched — telemetry may be absent, the
/// measurement never is.
///
/// ## Why the poster is CACHED rather than rebuilt per tick
///
/// The per-walk poster is rebuilt once per multi-hour walk, where the cost is
/// noise. This publisher ticks every 60 s on the PUBLISHERS runtime, which is
/// `worker_threads(1)` and shared with the census, reclaim, the orphan reaper,
/// the maintenance executor and the fs backstop. Rebuilding here meant two
/// synchronous `std::fs::read`s, a profile resolution, an instance probe and a
/// full TLS client build on that single async worker every minute — stalling
/// every other poller during exactly the disk/IO emergency this feature
/// exists to report — plus a fresh TCP+TLS handshake to coord every 60 s
/// forever, because the client (and its connection pool) was dropped each
/// tick.
///
/// So: resolve on the BLOCKING pool (matching the mount probe in
/// [`sample_and_publish_volumes`]), and keep the poster alive across ticks,
/// keyed on the resolved identity + coord base so an active-profile switch
/// still rebuilds it and `resolve_dest` stays the one place the
/// secondary-instance identity guard is enforced.
async fn post_volumes_to_coord(volumes: Vec<VolumeReport>) {
    // Held across the resolution await: there is exactly one publisher task,
    // so this serializes nothing that was ever concurrent, and it keeps the
    // cache read and its refresh atomic.
    let mut guard = volume_poster_cell().lock().await;
    let prev = guard.as_ref().map(|c| c.key.clone());
    match tokio::task::spawn_blocking(move || resolve_volume_poster(prev)).await {
        Ok(VolumePosterResolution::Reuse) => {}
        Ok(VolumePosterResolution::Rebuilt(cached)) => *guard = Some(*cached),
        Ok(VolumePosterResolution::NoIdentity) => {
            *guard = None;
            debug!(
                "worktree_census: no device_id — volume sample stays local this tick (not an error)"
            );
            return;
        }
        Err(e) => {
            warn!(
                "worktree_census: volume poster resolution task failed: {e} — the LOCAL sample \
                 is unaffected, retrying next tick"
            );
            return;
        }
    }
    let Some(cached) = guard.as_mut() else {
        return;
    };
    cached.poster.post_volumes(volumes).await;
}

/// Spawn the dedicated volume publisher on the ambient tokio runtime.
///
/// Cadence from [`VOLUME_INTERVAL_SECS_ENV`] (default 60 s, clamped to
/// 60 s..=3600 s — the ceiling exists so the knob cannot silently turn disk
/// monitoring off),
/// `MissedTickBehavior::Skip` like every other poller here. `tokio::interval`
/// fires its first tick immediately, so a boot has a free-space reading
/// within milliseconds instead of waiting out the first multi-hour census
/// walk.
///
/// **Deliberately unguarded by `machine_state_publish_allowed`**, unlike
/// [`spawn_census`]: the walk's guard is a COST decision (don't burn a
/// multi-hour walk you cannot publish), while a volume probe costs
/// microseconds and its LOCAL half must answer on a secondary instance too.
/// The identity half — never speak for the machine from a secondary — is
/// enforced where it belongs, at `ChunkPoster::resolve_dest`.
pub fn spawn_volume_publisher() {
    let secs = volume_sample_interval_secs();
    info!("worktree_census: starting volume publisher, interval={secs}s");
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            // Best-effort by construction: `sample_and_publish_volumes`
            // swallows every failure class into `None` and the loop never
            // panics — a telemetry gap must never stall the runner.
            let _ = sample_and_publish_volumes().await;
        }
    });
}

/// Spawn the periodic census task on the ambient tokio runtime.
///
/// Interval read from `QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS` (default
/// 300s, floored at 30s). `MissedTickBehavior::Skip` matches
/// `fleet::spawn_heartbeat` / `fleet::spawn_tree_publisher` — a system
/// suspend skips catch-up rather than blasting back-to-back ticks.
/// Failures `warn!` and retry on the next tick; the loop never panics.
///
/// Each tick runs [`build_and_publish`], which streams the census to coord in
/// bounded chunks DURING the walk — there is no separate whole-walk POST any
/// more (that single 3.3MB end-of-walk POST is what left coord's census table
/// empty for the whole multi-hour walk, and its server-side persist blew the
/// old 15s client timeout).
pub fn spawn_census() {
    // Device-keyed machine state: a secondary's walk would overwrite the
    // machine's worktree inventory with its own view, and the reclaim/reaper
    // path acts on whichever landed last. Same rule as the fleet publishers —
    // see `fleet::machine_state_publish_allowed`.
    //
    // This guard is the COST half: no periodic multi-hour walk on an instance
    // whose result cannot be published. The IDENTITY half — never POST the
    // machine's inventory from a secondary — is enforced at the chokepoint in
    // `ChunkPoster::resolve_dest`, because this is not the only walk trigger
    // (`spawn_census_rebuild` reaches `build_and_publish` directly) and a guard
    // that only covers the periodic tick can be routed around.
    if !crate::fleet::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
        return;
    }
    let secs: u64 = std::env::var("QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CENSUS_INTERVAL_SECS)
        .max(30);

    info!(
        "worktree_census: starting periodic census task, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match build_and_publish().await {
                Ok(BuildOutcome::Built(req)) => debug!(
                    "worktree_census: periodic walk published {} rows",
                    req.worktrees.len()
                ),
                Ok(BuildOutcome::AlreadyRunning | BuildOutcome::Skipped) => {}
                Err(e) => warn!("worktree_census: {e}"),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The instance guard must sit on the POST CHOKEPOINT, not only on the
    /// periodic spawn.
    ///
    /// `fleet.rs`'s `every_machine_scoped_writer_consults_the_guard` pins that
    /// this module has guards at all; it does not pin WHERE. That distinction is
    /// the whole bug this test exists for: the guard shipped on `spawn_census`
    /// alone, and `spawn_census_rebuild` — the survey's `?refresh=1` — reaches
    /// [`build_and_publish`] without passing it, so a secondary asked to refresh
    /// still POSTed the machine's inventory. Moving both guards back onto spawn
    /// paths would satisfy the count in `fleet.rs` and re-open that hole; this
    /// assertion is what refuses it.
    ///
    /// Source-level for the same reason `fleet.rs`'s pin is: the predicate reads
    /// `QONTINUI_PORT`, which other harness threads mutate, so an env-driven
    /// assertion flakes here (`instance::primary_keeps_the_unscoped_path`).
    #[test]
    fn the_census_post_chokepoint_carries_the_guard() {
        const SRC: &str = include_str!("census.rs");
        const GUARD: &str =
            "machine_state_publish_allowed(crate::instance::owns_shared_root_state())";

        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);

        let dest_fn = prod
            .split_once("fn resolve_dest(")
            .map(|(_, after)| after)
            .expect(
                "ChunkPoster::resolve_dest is the single place a census leaves this process; \
                 if it was renamed, move this pin with it rather than deleting it",
            );
        // Bound the window to the method itself: its closing brace is the first
        // one at the method's own indent (every nested closer is deeper), so a
        // guard sitting in a LATER method cannot satisfy this pin.
        let body = dest_fn
            .split_once("\n    }\n")
            .map(|(b, _)| b)
            .unwrap_or(dest_fn);
        let squeezed: String = body.chars().filter(|c| !c.is_whitespace()).collect();

        assert!(
            squeezed.contains(GUARD),
            "the census POST destination is resolved without consulting \
             `fleet::machine_state_publish_allowed`. Every walk trigger routes through here \
             (the periodic tick AND `spawn_census_rebuild`), so a guard anywhere else can be \
             routed around by a caller that reaches `build_and_publish` directly."
        );
    }

    /// `<root>/<name>` with `.git` as a DIRECTORY — a real clone.
    fn make_clone(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        p
    }

    /// `<root>/<name>` with `.git` as a FILE — a linked worktree.
    fn make_worktree(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join(".git"),
            "gitdir: D:/qontinui-root/x/.git/worktrees/y",
        )
        .unwrap();
        p
    }

    #[test]
    fn clone_root_is_structural_not_name_based() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        assert!(is_git_clone_root(&make_clone(root, "qontinui-runner")));
        assert!(!is_git_clone_root(&make_worktree(
            root,
            "qontinui-runner-wt-pnpm"
        )));
        // No `.git` at all — a plain dir is not a clone.
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        assert!(!is_git_clone_root(&root.join("node_modules")));

        // THE REGRESSION: a real clone whose NAME matches the worktree
        // pattern. The old name-based predicate called this a worktree, which
        // is what put a 26.5 GB clone into a cleared reclaim cohort.
        assert!(is_git_clone_root(&make_clone(
            root,
            "qontinui-coord-wt-prcreate-fix"
        )));
    }

    #[test]
    fn canonical_repo_root_excludes_worktrees_and_includes_wt_named_clones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Real repo roots.
        assert!(is_canonical_repo_root(&make_clone(root, "qontinui-runner")));
        assert!(is_canonical_repo_root(&make_clone(root, "qontinui-coord")));
        // Linked worktrees — `.git` is a file.
        assert!(!is_canonical_repo_root(&make_worktree(
            root,
            "qontinui-runner-wt-pnpm"
        )));
        assert!(!is_canonical_repo_root(&make_worktree(
            root,
            "qontinui-runner-xrepo-wt"
        )));
        // Non-qontinui dirs stay out of the governed set.
        assert!(!is_canonical_repo_root(&make_clone(root, "node_modules")));
        // A `-wt-`-NAMED real clone IS a repo root — the name no longer decides.
        assert!(is_canonical_repo_root(&make_clone(
            root,
            "qontinui-coord-wt-prcreate-fix"
        )));
    }

    #[test]
    fn sibling_scan_skips_real_clones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let canonical = make_clone(root, "qontinui-coord");

        let real_wt = make_worktree(root, "qontinui-coord-wt-verify");
        // Same naming shape, but a separate repository.
        make_clone(root, "qontinui-coord-wt-prcreate-fix");

        let found = sibling_and_claude_worktrees(root, &canonical, "qontinui-coord");
        assert!(
            found.contains(&real_wt),
            "genuine sibling worktree must still be discovered"
        );
        assert!(
            !found
                .iter()
                .any(|p| p.ends_with("qontinui-coord-wt-prcreate-fix")),
            "a real clone must never be reported as a worktree of another repo"
        );
    }

    #[test]
    fn normalize_path_str_forces_forward_slashes() {
        // A sibling-scan PathBuf can carry Windows backslashes; the emitted
        // census path must be separator-stable so coord's DISTINCT ON
        // (device, repo, path) doesn't keep `\` and `/` variants as two rows.
        let p = Path::new(r"D:\qontinui-root\qontinui-runner-wt-verify");
        assert_eq!(
            normalize_path_str(p),
            "D:/qontinui-root/qontinui-runner-wt-verify"
        );
        // Already-forward paths are unchanged.
        let q = Path::new("D:/qontinui-root/qontinui-coord-wt-cpw");
        assert_eq!(
            normalize_path_str(q),
            "D:/qontinui-root/qontinui-coord-wt-cpw"
        );
    }

    #[test]
    fn normal_dir_is_not_a_junction() {
        // A freshly-created plain directory must read as non-junction on
        // every platform. (We can't portably create a junction in a unit
        // test, so this pins the non-junction arm — the common case.)
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_junction(dir.path()));
    }

    #[test]
    fn missing_dir_measures_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("node_modules");
        let (present, is_junction, bytes) = measure_dir(&missing);
        assert!(!present);
        assert!(!is_junction);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn dir_size_sums_real_files_and_skips_nothing_when_no_junctions() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(sub.join("b.bin"), vec![0u8; 250]).unwrap();
        let total = dir_size_skipping_junctions(dir.path());
        assert_eq!(total, 350, "should sum nested real files");
    }

    #[test]
    fn measure_present_real_dir_reports_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("pkg.json"), vec![0u8; 42]).unwrap();
        let (present, is_junction, bytes) = measure_dir(&nm);
        assert!(present);
        assert!(!is_junction);
        assert_eq!(bytes, 42);
    }

    #[test]
    fn drive_letter_parsing() {
        assert_eq!(
            drive_letter_of(Path::new("D:/qontinui-root/x")),
            Some("D:".to_string())
        );
        assert_eq!(
            drive_letter_of(Path::new("c:\\Users\\foo")),
            Some("C:".to_string())
        );
        assert_eq!(drive_letter_of(Path::new("/home/user/x")), None);
        assert_eq!(drive_letter_of(Path::new("relative/path")), None);
    }

    #[test]
    fn target_dir_prefers_src_tauri_when_present() {
        let dir = tempfile::tempdir().unwrap();
        // No src-tauri → top-level target.
        assert_eq!(target_dir_for(dir.path()), dir.path().join("target"));
        // With src-tauri/ → src-tauri/target.
        std::fs::create_dir(dir.path().join("src-tauri")).unwrap();
        assert_eq!(
            target_dir_for(dir.path()),
            dir.path().join("src-tauri").join("target")
        );
    }

    // -----------------------------------------------------------------
    // Run provenance — build_target_dir / build_slot resolution rules.
    // Every test drives `resolve_build_target` with an EXPLICIT
    // `env_target_dir` argument rather than mutating the process
    // environment: `std::env::set_var` is process-global and cargo runs
    // unit tests on a shared thread pool, so an env-mutating test races
    // every other test in the binary.
    // -----------------------------------------------------------------

    /// A worktree that IS a cargo project, laid out like the runner
    /// (`src-tauri/Cargo.toml` under the repo root).
    fn tauri_worktree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src-tauri")).unwrap();
        std::fs::write(dir.path().join("src-tauri").join("Cargo.toml"), "[package]").unwrap();
        dir
    }

    /// Rule 1 — an ABSOLUTE `CARGO_TARGET_DIR` wins over everything, and a
    /// pool path yields the bare slot id.
    #[test]
    fn build_target_honours_absolute_cargo_target_dir_and_extracts_the_slot() {
        let wt = tauri_worktree();
        let pool = wt.path().join("target-pool").join("slot-2");
        let bt = resolve_build_target(wt.path(), Some(&pool.to_string_lossy()));
        assert_eq!(bt.dir.as_deref(), Some(normalize_path_str(&pool).as_str()));
        assert_eq!(bt.slot.as_deref(), Some("slot-2"));
    }

    /// Rule 1, relative form — `CARGO_TARGET_DIR=../target-pool/slot-0` is
    /// exactly what `cargo-guard.sh` passes after `cd`ing to `src-tauri`, so
    /// it must resolve against the invocation dir and normalize the `..`
    /// away (no `\\?\`, no literal `..` left in the emitted path).
    #[test]
    fn build_target_resolves_relative_cargo_target_dir_against_the_invocation_dir() {
        let wt = tauri_worktree();
        let bt = resolve_build_target(wt.path(), Some("../target-pool/slot-0"));
        let expect = normalize_path_str(&wt.path().join("target-pool").join("slot-0"));
        assert_eq!(bt.dir.as_deref(), Some(expect.as_str()));
        assert_eq!(bt.slot.as_deref(), Some("slot-0"));
        assert!(!bt.dir.unwrap().contains(".."));
    }

    /// The `lkg` (last-known-good) slot is a pool slot too.
    #[test]
    fn build_slot_recognises_the_lkg_pool_slot() {
        let wt = tauri_worktree();
        let pool = wt.path().join("target-pool").join("lkg");
        let bt = resolve_build_target(wt.path(), Some(&pool.to_string_lossy()));
        assert_eq!(bt.slot.as_deref(), Some("lkg"));
    }

    /// Rule 2 — a NON-POOL target dir is reported, but `build_slot` stays
    /// `None`. A slot is never synthesised: the consumer is a deletion
    /// engine, so charging `target-agent/` to some slot would be worse than
    /// reporting nothing.
    #[test]
    fn build_slot_is_none_for_a_non_pool_target_dir() {
        let wt = tauri_worktree();
        let agent = wt.path().join("target-agent");
        let bt = resolve_build_target(wt.path(), Some(&agent.to_string_lossy()));
        assert_eq!(bt.dir.as_deref(), Some(normalize_path_str(&agent).as_str()));
        assert_eq!(bt.slot, None, "target-agent/ is not a pool slot");

        // A `target-pool` child that is NOT a recognised slot name is also
        // None — not a guess at the nearest slot.
        let odd = wt.path().join("target-pool").join("scratch");
        let bt = resolve_build_target(wt.path(), Some(&odd.to_string_lossy()));
        assert!(bt.dir.is_some());
        assert_eq!(bt.slot, None, "`scratch` is not slot-<N> and not lkg");
    }

    /// Rule 3 — with no override anywhere, the dir falls back to the SAME
    /// in-tree dir `target_bytes` measures, so the two fields describe one
    /// directory. No pool ⇒ no slot.
    #[test]
    fn build_target_falls_back_to_the_measured_in_tree_target() {
        let wt = tauri_worktree();
        let bt = resolve_build_target(wt.path(), None);
        let expect = normalize_path_str(&target_dir_for(wt.path()));
        assert_eq!(bt.dir.as_deref(), Some(expect.as_str()));
        assert_eq!(bt.slot, None);
    }

    /// Rule 4 (UNRESOLVABLE) — a worktree with no `Cargo.toml` at either
    /// candidate root is not a cargo project, so BOTH fields are `None`.
    /// This is the `qontinui-web` case: emitting `<worktree>/target` there
    /// would attribute a directory that no build will ever write.
    #[test]
    fn build_target_is_none_for_a_non_cargo_worktree() {
        let wt = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_build_target(wt.path(), None),
            BuildTarget::default()
        );
        // …and an env override does NOT rescue it — the worktree still has
        // no cargo build to attribute.
        assert_eq!(
            resolve_build_target(wt.path(), Some("D:/pool/target-pool/slot-1")),
            BuildTarget::default()
        );
        // A `src-tauri/` dir without a manifest is still not a cargo project.
        std::fs::create_dir_all(wt.path().join("src-tauri")).unwrap();
        assert_eq!(
            resolve_build_target(wt.path(), None),
            BuildTarget::default()
        );
    }

    /// An empty / whitespace-only `CARGO_TARGET_DIR` is treated as UNSET
    /// (cargo ignores it too) and must not produce the invocation dir itself
    /// as the target.
    #[test]
    fn empty_cargo_target_dir_is_treated_as_unset() {
        let wt = tauri_worktree();
        let fallback = resolve_build_target(wt.path(), None);
        assert_eq!(resolve_build_target(wt.path(), Some("")), fallback);
        assert_eq!(resolve_build_target(wt.path(), Some("   ")), fallback);
    }

    /// The configured-pool-slot tier: `.cargo/config.toml` `[build]
    /// target-dir`, relative to the dir CONTAINING `.cargo`.
    #[test]
    fn cargo_config_build_target_dir_is_the_middle_tier() {
        let wt = tauri_worktree();
        let cargo_dir = wt.path().join("src-tauri").join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "[build]\ntarget-dir = \"../target-pool/slot-1\"\n",
        )
        .unwrap();

        let bt = resolve_build_target(wt.path(), None);
        let expect = normalize_path_str(&wt.path().join("target-pool").join("slot-1"));
        assert_eq!(bt.dir.as_deref(), Some(expect.as_str()));
        assert_eq!(bt.slot.as_deref(), Some("slot-1"));

        // …and an env var still outranks it.
        let bt = resolve_build_target(wt.path(), Some("../target-pool/slot-2"));
        assert_eq!(bt.slot.as_deref(), Some("slot-2"));
    }

    /// Within one config file, `[env] CARGO_TARGET_DIR` outranks
    /// `[build] target-dir`, and `relative = true` rebases it onto the
    /// config's own directory.
    #[test]
    fn cargo_config_env_table_outranks_build_target_dir() {
        let wt = tauri_worktree();
        let cargo_dir = wt.path().join("src-tauri").join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "[build]\ntarget-dir = \"../target-pool/slot-1\"\n\
             [env]\nCARGO_TARGET_DIR = { value = \"../../target-pool/lkg\", relative = true }\n",
        )
        .unwrap();
        // `relative = true` ⇒ base is the dir holding `.cargo`
        // (`<wt>/src-tauri`), so `../../target-pool/lkg` lands beside the
        // tempdir's parent — assert on the SLOT, which is what a consumer
        // groups by, plus that the build table lost.
        let bt = resolve_build_target(wt.path(), None);
        assert_eq!(bt.slot.as_deref(), Some("lkg"));
        assert!(!bt.dir.unwrap().ends_with("slot-1"));
    }

    /// The real runner `.cargo/config.toml` sets `[env] SCCACHE_SERVER_PORT`
    /// and no target dir at all — a config file that names neither key must
    /// not short-circuit the walk into a bogus value.
    #[test]
    fn cargo_config_without_a_target_dir_falls_through() {
        let wt = tauri_worktree();
        let cargo_dir = wt.path().join("src-tauri").join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "[env]\nSCCACHE_SERVER_PORT = { value = \"4230\", force = false }\n",
        )
        .unwrap();
        let bt = resolve_build_target(wt.path(), None);
        assert_eq!(
            bt.dir.as_deref(),
            Some(normalize_path_str(&target_dir_for(wt.path())).as_str())
        );
        assert_eq!(bt.slot, None);
    }

    /// The config walk STOPS at the worktree root. A `.cargo/config.toml`
    /// ABOVE the repo (the shape `$CARGO_HOME/config.toml` takes when a
    /// checkout happens to live under the user profile) must NOT be picked
    /// up — otherwise resolution would depend on where the checkout sits
    /// rather than on how the repo is configured.
    #[test]
    fn cargo_config_above_the_worktree_root_is_ignored() {
        let outer = tempfile::tempdir().unwrap();
        let wt = outer.path().join("repo");
        std::fs::create_dir_all(wt.join("src-tauri")).unwrap();
        std::fs::write(wt.join("src-tauri").join("Cargo.toml"), "[package]").unwrap();

        // Config one level ABOVE the worktree root — out of bounds.
        let above = outer.path().join(".cargo");
        std::fs::create_dir_all(&above).unwrap();
        std::fs::write(
            above.join("config.toml"),
            "[build]\ntarget-dir = \"/somewhere/target-pool/slot-9\"\n",
        )
        .unwrap();

        let bt = resolve_build_target(&wt, None);
        assert_eq!(
            bt.dir.as_deref(),
            Some(normalize_path_str(&target_dir_for(&wt)).as_str()),
            "an out-of-repo config must not win"
        );
        assert_eq!(bt.slot, None);

        // The SAME file at the worktree root IS in bounds.
        let at_root = wt.join(".cargo");
        std::fs::create_dir_all(&at_root).unwrap();
        std::fs::write(
            at_root.join("config.toml"),
            "[build]\ntarget-dir = \"target-pool/slot-9\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_build_target(&wt, None).slot.as_deref(),
            Some("slot-9")
        );
    }

    /// `build_slot_for` is pure — pin the vocabulary directly, including the
    /// case-insensitivity Windows paths force on us.
    #[test]
    fn build_slot_vocabulary() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "D:/qontinui-root/qontinui-runner/target-pool/slot-2",
                Some("slot-2"),
            ),
            (
                "D:/qontinui-root/qontinui-runner/target-pool/slot-10",
                Some("slot-10"),
            ),
            (
                "D:/qontinui-root/qontinui-runner/target-pool/lkg",
                Some("lkg"),
            ),
            // Windows paths are case-insensitive; one slot must not key twice.
            (
                "D:/qontinui-root/qontinui-runner/Target-Pool/SLOT-2",
                Some("slot-2"),
            ),
            // Not a pool at all.
            ("D:/qontinui-root/qontinui-runner/target", None),
            ("D:/qontinui-root/qontinui-runner/target-agent", None),
            // Pool dir with nothing under it, or a non-slot child.
            ("D:/qontinui-root/qontinui-runner/target-pool", None),
            ("D:/qontinui-root/qontinui-runner/target-pool/scratch", None),
            ("D:/qontinui-root/qontinui-runner/target-pool/slot-", None),
            ("D:/qontinui-root/qontinui-runner/target-pool/slot-x", None),
        ];
        for (path, want) in cases {
            assert_eq!(
                build_slot_for(Path::new(path)).as_deref(),
                *want,
                "build_slot_for({path})"
            );
        }
    }

    #[test]
    fn collect_volumes_empty_when_no_drive_letters() {
        // POSIX-style paths have no drive letter → no volume rows (so CI
        // on linux gets a deterministic empty result).
        let paths = vec![PathBuf::from("/tmp/x"), PathBuf::from("/home/y")];
        assert!(collect_volumes(&paths).is_empty());
    }

    /// Mount list used by the pure volume-mapping tests: two Windows drives,
    /// a case-differing duplicate of one of them, a volume mounted into a
    /// FOLDER on one of them, a POSIX mount, and an empty optical drive.
    fn sample_mounts() -> Vec<(PathBuf, u64, u64)> {
        vec![
            (PathBuf::from(r"C:\"), 500, 100),
            (PathBuf::from(r"D:\"), 4_000, 93),
            // The SAME volume reported twice, differing only in case (Windows
            // paths are case-insensitive) — a genuine duplicate, must collapse.
            (PathBuf::from(r"d:\"), 4_000, 93),
            // A DIFFERENT volume that merely borrows D:'s letter — the normal
            // Windows way to attach a large data disk. Must NOT collapse into
            // `D:\`; keying on the drive letter used to drop it silently.
            (PathBuf::from(r"D:\data"), 8_000, 7_000),
            (PathBuf::from("/mnt/data"), 900, 800),
            // No media in the drive: total 0. NOT a volume with no space.
            (PathBuf::from(r"E:\"), 0, 0),
        ]
    }

    /// THE Phase-1 step-2 property: a volume with no worktree on it is still
    /// reported. The path-filtered collector answers the census's attribution
    /// question and is deliberately kept; the all-volumes collector answers
    /// the product question ("how much disk is left?").
    #[test]
    fn all_volumes_sees_drives_the_worktree_filter_never_would() {
        let mounts = sample_mounts();

        let wanted: HashSet<String> = ["D:".to_string()].into_iter().collect();
        let filtered = volumes_from_mounts(&mounts, Some(&wanted));
        assert_eq!(
            filtered
                .iter()
                .map(|v| v.volume.as_str())
                .collect::<Vec<_>>(),
            vec!["D:", "D:/data"],
            "the path-filtered collector reports the volumes reachable under the \
             wanted drive letter — the letter's root AND anything folder-mounted \
             on it, since a worktree under `D:\\data\\...` resolves to `D:`"
        );

        let all = volumes_from_mounts(&mounts, None);
        // Key-sorted and de-duplicated (`BTreeMap`), so the wire order is
        // deterministic; `/mnt/data` sorts before `C:` on ASCII.
        assert_eq!(
            all.iter().map(|v| v.volume.as_str()).collect::<Vec<_>>(),
            vec!["/mnt/data", "C:", "D:", "D:/data"],
            "every mounted volume must be reported — including ones hosting no \
             worktree (C:), POSIX mounts, which have no drive letter, and a volume \
             mounted into a folder on another volume's letter"
        );
        let d = all
            .iter()
            .find(|v| v.volume == "D:")
            .expect("D: must be reported");
        assert_eq!(d.free_bytes, 93);
        assert_eq!(d.total_bytes, 4_000);
    }

    /// A volume mounted into a FOLDER is a real volume and must survive.
    ///
    /// Keying on the drive letter collapsed `D:\data` into `D:`, and
    /// [`volumes_from_mounts`]'s keep-the-first rule then discarded it with no
    /// log and no marker — a whole 8 TB data disk simply absent from a
    /// disk-monitoring feature, which is the fabricated absence invariant 2
    /// forbids. Genuine duplicates (`d:\` vs `D:\`, one volume Windows reports
    /// twice) must still collapse: the fix distinguishes DIFFERENT MOUNT POINTS
    /// from the SAME mount point spelled differently.
    #[test]
    fn a_folder_mounted_volume_is_reported_and_not_collapsed_into_its_drive_letter() {
        let all = volumes_from_mounts(&sample_mounts(), None);

        let folder = all
            .iter()
            .find(|v| v.volume == "D:/data")
            .expect("a volume mounted into a folder must be reported, not silently dropped");
        assert_eq!(
            (folder.total_bytes, folder.free_bytes),
            (8_000, 7_000),
            "the folder-mounted volume must carry ITS OWN capacity, not the \
             root volume's"
        );
        assert_eq!(
            folder.drive_letter.as_deref(),
            Some("D:"),
            "the drive letter survives as a LABEL — it is just no longer the key"
        );

        let root = all
            .iter()
            .find(|v| v.volume == "D:")
            .expect("the drive root keeps its stable `D:` key (unchanged wire value)");
        assert_eq!((root.total_bytes, root.free_bytes), (4_000, 93));

        // …and the genuine duplicate is still collapsed.
        assert_eq!(
            all.iter().filter(|v| v.volume == "D:").count(),
            1,
            "`d:\\` and `D:\\` are ONE volume (Windows paths are case-insensitive) \
             and must not both be reported: {all:?}"
        );
    }

    /// The keying rule itself, at the unit level: a drive ROOT keys to the
    /// letter (the stable, pre-existing wire value), anything else keys to its
    /// own mount point.
    #[test]
    fn volume_key_is_the_letter_only_for_a_drive_root() {
        assert_eq!(volume_key(Path::new(r"D:\")), "D:");
        assert_eq!(volume_key(Path::new("D:/")), "D:");
        assert_eq!(volume_key(Path::new("d:")), "D:");
        assert_eq!(volume_key(Path::new(r"D:\data")), "D:/data");
        assert_eq!(volume_key(Path::new(r"C:\mnt\big")), "C:/mnt/big");
        assert_eq!(volume_key(Path::new("/mnt/data")), "/mnt/data");
        assert_eq!(volume_key(Path::new("/")), "/");

        // Windows dedup is case-insensitive; POSIX dedup is NOT.
        assert_eq!(
            volume_dedup_key("D:/Data", true),
            volume_dedup_key("d:/data", true)
        );
        assert_ne!(
            volume_dedup_key("/mnt/Data", false),
            volume_dedup_key("/mnt/data", false)
        );
    }

    /// A drive with no media reports `total_space() == 0`. Reporting it would
    /// render as "0 bytes free" — a fabricated zero for a volume that could
    /// not be read at all (INV-D1: absent is UNKNOWN, never zero).
    #[test]
    fn an_unreadable_zero_capacity_mount_is_dropped_not_reported_as_full() {
        let all = volumes_from_mounts(&sample_mounts(), None);
        assert!(
            !all.iter().any(|v| v.volume == "E:"),
            "a zero-capacity mount must never appear as a volume: {all:?}"
        );
        assert!(
            all.iter().all(|v| v.total_bytes > 0),
            "no reported volume may claim zero capacity: {all:?}"
        );
    }

    /// The live probe, on whatever machine the tests run: it may legitimately
    /// return nothing (a container with no readable mounts), but anything it
    /// DOES return must be a usable reading.
    #[test]
    fn the_live_all_volumes_probe_never_emits_a_fabricated_zero() {
        for v in collect_all_volumes() {
            assert!(
                v.total_bytes > 0,
                "live probe emitted a zero-capacity volume: {v:?}"
            );
            assert!(!v.volume.is_empty(), "live probe emitted an unnamed volume");
        }
    }

    /// Before the first sample the reading is ABSENT, and absence is the
    /// UNKNOWN state — there is no zero-filled placeholder to mistake for a
    /// measurement. After a publish it is readable without touching disk,
    /// coord, or the census walk.
    #[test]
    fn a_published_volume_sample_is_readable_and_absence_is_unknown() {
        // Local cell so this never races the process-global one.
        let cell: RwLock<Option<VolumeSample>> = RwLock::new(None);
        assert!(
            cell.read().unwrap().is_none(),
            "an unsampled cell must hold None (UNKNOWN), never an empty reading"
        );

        let published = publish_volume_sample(vec![VolumeReport {
            volume: "D:".to_string(),
            drive_letter: Some("D:".to_string()),
            total_bytes: 4_000,
            free_bytes: 93,
        }]);
        assert_eq!(published.volumes.len(), 1);
        let read_back = latest_volume_sample().expect("a published sample must be readable");
        assert_eq!(read_back.volumes, published.volumes);
        assert_eq!(read_back.taken_at, published.taken_at);
    }

    #[test]
    fn the_volume_cadence_is_floored_and_survives_garbage() {
        assert_eq!(resolve_volume_interval(None), DEFAULT_VOLUME_INTERVAL_SECS);
        assert_eq!(
            resolve_volume_interval(Some("")),
            DEFAULT_VOLUME_INTERVAL_SECS
        );
        assert_eq!(
            resolve_volume_interval(Some("not-a-number")),
            DEFAULT_VOLUME_INTERVAL_SECS
        );
        // The knob may make the publisher QUIETER…
        assert_eq!(resolve_volume_interval(Some("300")), 300);
        assert_eq!(
            resolve_volume_interval(Some("3600")),
            MAX_VOLUME_INTERVAL_SECS,
            "the ceiling itself is a legal value"
        );
        // …never fast enough to hammer coord…
        assert_eq!(resolve_volume_interval(Some("1")), MIN_VOLUME_INTERVAL_SECS);
        assert_eq!(resolve_volume_interval(Some("0")), MIN_VOLUME_INTERVAL_SECS);
        // …and never so quiet that the feature is silently OFF. A well-formed
        // `u64` of 999999999 used to be accepted verbatim: monitoring disabled
        // for 31 years, announced by one `info!` at boot, with every disk
        // surface rendering `pending`/`stale` forever.
        assert_eq!(
            resolve_volume_interval(Some("999999999")),
            MAX_VOLUME_INTERVAL_SECS,
            "an out-of-band cadence must be CLAMPED, never allowed to disable the feature"
        );
        assert_eq!(
            resolve_volume_interval(Some(&u64::MAX.to_string())),
            MAX_VOLUME_INTERVAL_SECS
        );
        assert_eq!(
            resolve_volume_interval(Some("3601")),
            MAX_VOLUME_INTERVAL_SECS
        );
        // (The band itself — default inside [min, max] — is pinned at COMPILE
        // time by the `const _: () = assert!(…)` beside the constants, so a
        // bad edit fails the build rather than this test.)
    }

    /// R2: a HEALTHY tick must not consume the low-disk alarm's throttle
    /// budget.
    ///
    /// The bug this pins: the epoch was stamped UNCONDITIONALLY, before
    /// calling `warn_on_low_disk` — which emits nothing when every volume is
    /// above the floor. So a healthy boot at t=0 stamped the budget, and a
    /// disk crossing the CRITICAL floor at t=301 s saw `now - last = 1 < 300`
    /// and had its FIRST `error!` DROPPED; the alarm surfaced at t≈600 s, five
    /// minutes into the emergency it exists to announce.
    ///
    /// Driven through the pure [`emit_throttled`] seam with a local cell and an
    /// injected clock, so it is deterministic and never touches the process
    /// globals or the real time source.
    #[test]
    fn a_healthy_tick_does_not_consume_the_low_disk_alarm_budget() {
        let cell = AtomicU64::new(THROTTLE_NEVER);

        // t=0..300: healthy ticks. `emit` reports "nothing written".
        for t in [0_u64, 60, 120, 180, 240, 300] {
            assert!(
                !emit_throttled(&cell, t, LOW_DISK_LOG_THROTTLE_SECS, || false),
                "a healthy tick must not report an emission (t={t})"
            );
            assert_eq!(
                cell.load(Ordering::Acquire),
                THROTTLE_NEVER,
                "a healthy tick must leave the throttle budget UNSPENT (t={t})"
            );
        }

        // t=301: the disk crosses the critical floor. This is the line that
        // used to be swallowed.
        assert!(
            emit_throttled(&cell, 301, LOW_DISK_LOG_THROTTLE_SECS, || true),
            "the FIRST real alarm must fire immediately, whatever the healthy \
             ticks before it did"
        );
        assert_eq!(cell.load(Ordering::Acquire), 301);

        // …and from there the throttle does its actual job: the alarm is a
        // leading signal, not a per-minute heartbeat.
        assert!(
            !emit_throttled(&cell, 361, LOW_DISK_LOG_THROTTLE_SECS, || true),
            "a repeat alarm inside the window must be suppressed"
        );
        assert!(
            emit_throttled(&cell, 601, LOW_DISK_LOG_THROTTLE_SECS, || true),
            "once the window elapses the alarm fires again"
        );
        assert_eq!(cell.load(Ordering::Acquire), 601);
    }

    /// R6: the empty-probe warning is throttled too. On a machine with no
    /// readable mounts (a container) the condition is STANDING, so an
    /// unthrottled multi-line `warn!` on a 60 s publisher is log flood forever.
    #[test]
    fn the_empty_probe_warning_is_throttled_like_the_low_disk_alarm() {
        let cell = AtomicU64::new(THROTTLE_NEVER);
        let mut emissions = 0_u32;
        // Ten minutes of 60 s ticks on a mountless machine, starting at the
        // epoch — `THROTTLE_NEVER` is what makes `t=0` a usable stamp rather
        // than one that reads back as "never emitted".
        for t in (0_u64..600).step_by(60) {
            if emit_throttled(&cell, t, EMPTY_PROBE_LOG_THROTTLE_SECS, || true) {
                emissions += 1;
            }
        }
        assert_eq!(
            emissions, 2,
            "ten minutes of empty probes must log twice (t=0, t=300), not ten times"
        );
        // (That the throttle exceeds the publisher's cadence at all — without
        // which it throttles nothing — is pinned at COMPILE time beside the
        // constants.)
    }

    /// The empty-probe path must actually route through the throttle — a
    /// future edit that inlines a bare `warn!` back into
    /// `sample_and_publish_volumes` would restore the per-minute flood while
    /// the unit test above still passed.
    #[test]
    fn the_empty_probe_branch_calls_the_throttled_warner() {
        const SRC: &str = include_str!("census.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let sampler = prod
            .split_once("pub(crate) async fn sample_and_publish_volumes(")
            .map(|(_, after)| after)
            .expect("sample_and_publish_volumes is the publisher's per-tick body");
        let body = sampler
            .split_once("\n}\n")
            .map(|(b, _)| b)
            .unwrap_or(sampler);
        assert!(
            body.contains("warn_on_empty_probe_throttled()"),
            "the empty-probe branch must go through the throttled warner"
        );
    }

    /// A volumes-only POST carries no worktree row, so it must NOT release
    /// the R3 census-before-reclaim boot gate — a 60s telemetry tick opening
    /// that gate would hand the reclaim poller the PREVIOUS boot's census,
    /// which is exactly the husk-creating race the gate exists to prevent.
    ///
    /// Source-level for the same reason the chokepoint pin above is: the
    /// alternative needs a live HTTP server plus a coord-configured profile,
    /// and this asserts the property that would be silently lost by a future
    /// "simplification" that folds `post_volumes` back into `post`.
    #[test]
    fn a_volumes_only_post_never_releases_the_census_boot_gate() {
        const SRC: &str = include_str!("census.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);

        let volumes_fn = prod
            .split_once("async fn post_volumes(")
            .map(|(_, after)| after)
            .expect("ChunkPoster::post_volumes is the volumes-only wire path");
        let body = volumes_fn
            .split_once("\n    }\n")
            .map(|(b, _)| b)
            .unwrap_or(volumes_fn);
        let squeezed: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            squeezed.contains("self.send(body,false)"),
            "post_volumes must send with release_boot_gate = false"
        );
        assert!(
            !squeezed.contains("mark_first_census_posted"),
            "post_volumes must never mark a census as posted"
        );

        // …and the single send site must gate the mark on that flag.
        let send_fn = prod
            .split_once("async fn send(")
            .map(|(_, after)| after)
            .expect("ChunkPoster::send is the single wire send");
        let send_squeezed: String = send_fn
            .split_once("\n    }\n")
            .map(|(b, _)| b)
            .unwrap_or(send_fn)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            send_squeezed.contains("ifrelease_boot_gate{mark_first_census_posted();}"),
            "the R3 boot gate must be released only for bodies that refresh \
             coord's WORKTREE view"
        );
    }

    /// The MIRROR of the test above, and the direction that actually fails
    /// dangerously.
    ///
    /// `a_volumes_only_post_never_releases_the_census_boot_gate` pins that the
    /// volumes path passes `false`. Nothing pinned that the CENSUS path still
    /// passes `true`. A future edit flipping it (say, "make both call sites
    /// consistent") would leave the census-before-reclaim boot gate closed for
    /// the whole process lifetime, permanently blocking the reclaim engine on
    /// every boot — failing SAFE (no deletions) and therefore silent: nothing
    /// alarms, reclaim simply never runs again.
    ///
    /// Source-level for the same reason its mirror is: asserting it live needs
    /// a coord-configured profile and an HTTP server.
    #[test]
    fn the_census_chunk_post_still_releases_the_boot_gate() {
        const SRC: &str = include_str!("census.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);

        let post_fn = prod
            .split_once("async fn post(&mut self, chunk: CensusChunk)")
            .map(|(_, after)| after)
            .expect("ChunkPoster::post is the census-chunk wire path");
        let body = post_fn
            .split_once("\n    }\n")
            .map(|(b, _)| b)
            .unwrap_or(post_fn);
        let squeezed: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            squeezed.contains("self.send(body,true)"),
            "the census chunk POST must send with release_boot_gate = true, or the \
             R3 census-before-reclaim gate never opens and reclaim is disabled for \
             the whole boot"
        );
    }

    /// R3: the volume poster (and its TLS client) is built ONCE and reused
    /// across ticks, rebuilding only when the identity or coord base moves.
    ///
    /// The publisher runs on the `worker_threads(1)` publishers runtime shared
    /// with the census, reclaim, the orphan reaper, the maintenance executor
    /// and the fs backstop. Rebuilding per tick meant two `std::fs::read`s, a
    /// profile resolution, an instance probe and a full `reqwest::Client`
    /// build on that single async worker every 60 s — plus a fresh TCP+TLS
    /// handshake to coord every minute, forever, because the client's
    /// connection pool was dropped with it.
    #[test]
    fn the_volume_poster_is_reused_across_ticks_and_rebuilt_only_on_a_real_change() {
        let device = Uuid::nil();
        let key = VolumePosterKey {
            device_id: device,
            tenant_id: None,
            base: Some("https://coord.example".to_string()),
        };

        assert!(
            volume_poster_needs_rebuild(None, &key),
            "the first tick has nothing cached and must build"
        );
        assert!(
            !volume_poster_needs_rebuild(Some(&key), &key),
            "an unchanged identity + base must REUSE the poster — this is the \
             whole point: no per-tick fs reads, no per-tick TLS handshake"
        );

        // …but a real change still rebuilds, so `resolve_dest` (the one place
        // the secondary-instance identity guard lives) is re-consulted.
        let moved_base = VolumePosterKey {
            base: Some("https://coord.other".to_string()),
            ..key.clone()
        };
        assert!(
            volume_poster_needs_rebuild(Some(&key), &moved_base),
            "an active-profile switch that moves the coord base must rebuild"
        );
        let unconfigured = VolumePosterKey {
            base: None,
            ..key.clone()
        };
        assert!(
            volume_poster_needs_rebuild(Some(&key), &unconfigured),
            "coord becoming unconfigured must rebuild"
        );
        let moved_tenant = VolumePosterKey {
            tenant_id: Some(Uuid::from_u128(7)),
            ..key.clone()
        };
        assert!(
            volume_poster_needs_rebuild(Some(&key), &moved_tenant),
            "a tenant rebinding must rebuild — the bearer is selected per tenant"
        );
        let moved_device = VolumePosterKey {
            device_id: Uuid::from_u128(9),
            ..key.clone()
        };
        assert!(
            volume_poster_needs_rebuild(Some(&key), &moved_device),
            "a re-enrolled device must rebuild — the URL carries the device_id"
        );
    }

    /// …and the per-tick path must actually USE that cache, off the async
    /// worker. A future edit that re-inlines `ChunkPoster::new` into the tick
    /// restores the stall and the per-minute TLS handshake while the pure test
    /// above still passes.
    #[test]
    fn the_volume_tick_resolves_identity_off_the_async_worker_and_reuses_the_cache() {
        const SRC: &str = include_str!("census.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let tick = prod
            .split_once("async fn post_volumes_to_coord(")
            .map(|(_, after)| after)
            .expect("post_volumes_to_coord is the publisher's per-tick coord path");
        let body = tick.split_once("\n}\n").map(|(b, _)| b).unwrap_or(tick);

        assert!(
            body.contains("volume_poster_cell()"),
            "the tick must go through the cached poster, not build a new one"
        );
        assert!(
            body.contains("spawn_blocking"),
            "identity/base resolution is blocking fs work and must never run on \
             the shared single-worker publishers runtime"
        );
        assert!(
            !body.contains("ChunkPoster::new("),
            "the tick must not construct a ChunkPoster directly — that is what \
             rebuilt the TLS client every 60 s"
        );
    }

    #[test]
    fn capture_worktree_on_non_git_dir_is_clean_and_unbranched() {
        // A plain dir (no git) → branch/sha/age None, not dirty, no nm,
        // no target.
        let dir = tempfile::tempdir().unwrap();
        let row = capture_worktree("qontinui-runner", dir.path());
        assert_eq!(row.repo, "qontinui-runner");
        assert!(row.branch.is_none());
        assert!(row.head_sha.is_none());
        assert!(!row.is_dirty);
        assert!(!row.nm_present);
        assert!(!row.target_present);
        assert_eq!(row.attributable_bytes, 0);
        assert!(row.last_access_mtime.is_some(), "dir mtime should read");
        // No git repo → no origin/main ref → landed_in_main is the honest
        // unknown `None` (which coord's gate reads as not-landed).
        assert!(row.landed_in_main.is_none());
        // The census `building` probe is now held-cargo-lock-only
        // (`probe_building_for_census`): a freshly-created tempdir has no
        // `target/*/.cargo-lock` held by a live cargo, so it is NOT building.
        // (The dropped root-mtime-recency heuristic is what over-reported the
        // fleet `count_building` gauge — see this plan's Phase 2.)
        assert_eq!(row.building, Some(false));
    }

    #[test]
    fn landed_in_main_is_none_without_origin_main() {
        // A real git repo but no `origin/main` ref → undeterminable → None.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args([&["-C", path.to_str().unwrap()], args].concat())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} should succeed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "c1"]);
        // No origin/main ref exists → compute returns None.
        assert!(compute_landed_in_main(path).is_none());
    }

    #[test]
    fn landed_in_main_true_when_head_is_ancestor_of_origin_main() {
        // Build a repo, create an `origin/main` ref AT HEAD via a local
        // bare "remote", then verify HEAD-is-ancestor → Some(true).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        // Point a local origin/main remote-tracking ref straight at HEAD.
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        assert_eq!(compute_landed_in_main(path), Some(true));

        // Add an unlanded commit on top → HEAD no longer ancestor and no
        // patch-id match → Some(false).
        std::fs::write(path.join("b.txt"), b"y").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "c2"]);
        assert_eq!(compute_landed_in_main(path), Some(false));
    }

    /// Ξ_Worktree P7.3 — canonical-checkout facts on a real temp git repo.
    /// Mirrors the git-tempdir idiom above. Drives the three compute helpers
    /// directly (a tempdir is not a canonical `<root>/<repo>/` checkout, so
    /// `capture_worktree` would resolve canonical to a real on-disk repo or
    /// None — we exercise the helpers against a controlled repo instead).
    #[test]
    fn canonical_checkout_facts_branch_dirty_and_divergence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        // Name the initial branch deterministically (default may be main or
        // master depending on git config) so the assertions are stable.
        git(&["checkout", "-q", "-b", "feature-x"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);

        // Branch resolves to the current branch.
        assert_eq!(
            compute_canonical_branch(path),
            Some("feature-x".to_string())
        );

        // Clean tree → Some(false); flips to Some(true) after an uncommitted
        // write.
        assert_eq!(compute_canonical_is_dirty(path), Some(false));
        std::fs::write(path.join("b.txt"), b"y").unwrap();
        assert_eq!(compute_canonical_is_dirty(path), Some(true));

        // Divergence string is non-empty (no origin/main here → falls back to
        // just the branch name, which is still a non-empty advisory string).
        let div = compute_canonical_base_divergence(path).expect("divergence Some");
        assert!(!div.is_empty(), "divergence string should be non-empty");
        assert!(div.starts_with("on:feature-x"), "got: {div}");
    }

    /// End-to-end at the census layer: a worktree holding ONLY the runner's
    /// own untracked scaffolding must report `is_dirty: false`, while genuine
    /// uncommitted work must still report `true`.
    ///
    /// The regression this pins: plain porcelain non-emptiness made every
    /// provisioned agent worktree dirty forever, and since neither the census
    /// nor the reclaim executor acts on a dirty worktree, ~34% of the backlog
    /// (1,792 of 5,322 measured 2026-07-28) was permanently unreclaimable.
    #[test]
    fn capture_worktree_ignores_runner_scaffolding_but_not_real_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        assert!(
            !capture_worktree("qontinui-runner", path).is_dirty,
            "a freshly committed tree is clean"
        );

        // Exactly what provisioning an agent worktree leaves behind.
        std::fs::create_dir_all(path.join(".claude").join("agents")).unwrap();
        std::fs::write(path.join(".claude").join("agents").join("r.md"), b"x").unwrap();
        std::fs::write(path.join(".coord-mcp-status"), b"UNREACHABLE").unwrap();
        std::fs::write(path.join(".mcp.json"), b"{}").unwrap();
        assert!(
            !capture_worktree("qontinui-runner", path).is_dirty,
            "runner-written scaffolding alone must NOT read as dirty"
        );

        // A tracked modification still does.
        std::fs::write(path.join("a.txt"), b"edited").unwrap();
        assert!(
            capture_worktree("qontinui-runner", path).is_dirty,
            "a tracked modification must still read as dirty"
        );
    }

    /// Ξ_Worktree P7.3 — `on:main` carve-out + ahead/behind formatting when
    /// `origin/main` resolves.
    #[test]
    fn canonical_checkout_facts_on_main_and_divergence_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["checkout", "-q", "-b", "main"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);

        // On main → exact "on:main" carve-out (no rev-list needed).
        assert_eq!(
            compute_canonical_base_divergence(path),
            Some("on:main".to_string())
        );

        // Point a local origin/main at HEAD, branch off, add one ahead commit
        // → divergence string carries the rev-list left-right counts.
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(path.join("c.txt"), b"z").unwrap();
        git(&["add", "c.txt"]);
        git(&["commit", "-q", "-m", "c2"]);
        let div = compute_canonical_base_divergence(path).expect("divergence Some");
        // origin/main...HEAD: 0 behind, 1 ahead → "on:topic;0\t1".
        assert!(div.starts_with("on:topic;"), "got: {div}");
        assert!(div.contains('1'), "should report the 1 ahead commit: {div}");
    }

    // -----------------------------------------------------------------
    // Chunked-emission tests. All operate on LOCAL state (a Chunker, a
    // channel, a local RwLock cell) — never on the global LATEST_CENSUS,
    // which concurrent tests in other modules also publish to.
    // -----------------------------------------------------------------

    /// Minimal census row for chunk tests.
    fn row(repo: &str, path: &str, bytes: u64) -> WorktreeCensus {
        WorktreeCensus {
            repo: repo.to_string(),
            path: path.to_string(),
            branch: None,
            head_sha: None,
            head_age_secs: None,
            is_dirty: false,
            nm_present: false,
            nm_is_junction: false,
            nm_bytes: 0,
            target_present: false,
            target_is_junction: false,
            target_bytes: 0,
            build_target_dir: None,
            build_slot: None,
            last_access_mtime: None,
            attributable_bytes: bytes,
            landed_in_main: None,
            building: None,
            canonical_current_branch: None,
            canonical_is_dirty: None,
            canonical_base_divergence: None,
        }
    }

    #[test]
    fn chunker_flushes_on_row_boundary() {
        let mut c = Chunker::new(3, Duration::from_secs(3600));
        let mut flushed = Vec::new();
        for i in 0..7 {
            if let Some(chunk) = c.push(row("r", &format!("p{i}"), 0)) {
                flushed.push(chunk);
            }
        }
        assert_eq!(flushed.len(), 2, "3-row bound over 7 rows → two flushes");
        assert_eq!(flushed[0].len(), 3);
        assert_eq!(flushed[1].len(), 3);
        assert_eq!(flushed[0][0].path, "p0");
        assert_eq!(flushed[1][0].path, "p3");
        let rem = c.take_remainder();
        assert_eq!(rem.len(), 1, "the 7th row stays buffered until finish");
        assert_eq!(rem[0].path, "p6");
        assert!(c.take_remainder().is_empty(), "remainder drains the buffer");
    }

    #[test]
    fn chunker_flushes_on_time_boundary_whichever_first() {
        // Row bound is huge; only the 60s age bound can flush.
        let mut c = Chunker::new(1000, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(c.push_at(row("r", "a", 0), t0).is_none());
        assert!(
            c.push_at(row("r", "b", 0), t0 + Duration::from_secs(30))
                .is_none(),
            "age below the bound must not flush"
        );
        let chunk = c
            .push_at(row("r", "c", 0), t0 + Duration::from_secs(61))
            .expect("61s since the oldest buffered row must flush");
        assert_eq!(chunk.len(), 3, "the time flush carries the whole buffer");
        // The next row starts a fresh window anchored at ITS push time.
        assert!(
            c.push_at(row("r", "d", 0), t0 + Duration::from_secs(90))
                .is_none(),
            "new window: 0s old, row bound not hit"
        );
        assert_eq!(c.take_remainder().len(), 1);
    }

    #[test]
    fn chunk_emitter_sends_volumes_only_on_final_chunk() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CensusChunk>();
        let mut emitter = ChunkEmitter::new(Chunker::new(2, Duration::from_secs(3600)), tx);
        for i in 0..5 {
            emitter.on_row(row("r", &format!("p{i}"), 0));
        }
        emitter.finish(vec![VolumeReport {
            volume: "D:".to_string(),
            drive_letter: Some("D:".to_string()),
            total_bytes: 100,
            free_bytes: 40,
        }]);

        let mut chunks = Vec::new();
        while let Ok(c) = rx.try_recv() {
            chunks.push(c);
        }
        assert_eq!(chunks.len(), 3, "2+2 full chunks plus the final remainder");
        assert!(
            chunks[..2].iter().all(|c| c.volumes.is_none()),
            "intermediate chunks must NEVER carry volumes (coord's \
             worktree_volume table has no retention prune)"
        );
        let last = chunks.last().unwrap();
        assert_eq!(last.rows.len(), 1, "the 5th row rides the final chunk");
        assert_eq!(
            last.volumes.as_ref().map(|v| v.len()),
            Some(1),
            "the final chunk carries the walk's volume reports"
        );
    }

    #[test]
    fn merged_snapshot_upserts_by_repo_and_path() {
        let device = Uuid::nil();
        let prev = CensusSnapshot {
            req: Arc::new(WorktreeCensusReq {
                device_id: device,
                tenant_id: None,
                volumes: vec![VolumeReport {
                    volume: "D:".to_string(),
                    drive_letter: Some("D:".to_string()),
                    total_bytes: 100,
                    free_bytes: 50,
                }],
                worktrees: vec![
                    row("qontinui-runner", "D:/x/a", 10),
                    row("qontinui-runner", "D:/x/b", 20),
                    // Same PATH, different repo — must be a distinct key.
                    row("qontinui-coord", "D:/x/a", 30),
                ],
            }),
            taken_at: chrono::Utc::now() - chrono::Duration::hours(2),
            build_ms: 12_345,
        };

        let now = chrono::Utc::now();
        let chunk = vec![
            row("qontinui-runner", "D:/x/a", 99), // update
            row("qontinui-runner", "D:/x/c", 7),  // insert
        ];
        let merged = merged_snapshot(Some(&prev), &chunk, device, None, now);

        assert_eq!(merged.req.worktrees.len(), 4, "1 update + 1 insert over 3");
        let by_key = |repo: &str, path: &str| {
            merged
                .req
                .worktrees
                .iter()
                .find(|w| w.repo == repo && w.path == path)
                .map(|w| w.attributable_bytes)
        };
        assert_eq!(by_key("qontinui-runner", "D:/x/a"), Some(99), "updated");
        assert_eq!(by_key("qontinui-runner", "D:/x/b"), Some(20), "kept");
        assert_eq!(
            by_key("qontinui-coord", "D:/x/a"),
            Some(30),
            "same path in another repo untouched — the key is (repo, path)"
        );
        assert_eq!(by_key("qontinui-runner", "D:/x/c"), Some(7), "inserted");
        assert_eq!(merged.taken_at, now, "taken_at = time of the newest chunk");
        assert_eq!(
            merged.build_ms, 12_345,
            "build_ms stays the last COMPLETED walk's duration"
        );
        assert_eq!(
            merged.req.volumes, prev.req.volumes,
            "chunk merges never touch volumes"
        );
    }

    #[test]
    fn merged_snapshot_cold_start_has_chunk_rows_and_zero_build_ms() {
        let now = chrono::Utc::now();
        let chunk = vec![row("qontinui-runner", "D:/x/a", 1)];
        let merged = merged_snapshot(None, &chunk, Uuid::nil(), None, now);
        assert_eq!(merged.req.worktrees.len(), 1);
        assert!(merged.req.volumes.is_empty());
        assert_eq!(merged.build_ms, 0, "no walk has completed yet");
        assert_eq!(merged.taken_at, now);
    }

    #[test]
    fn chunk_merge_then_complete_walk_replace_drops_vanished_paths() {
        // Local cell — the exact production functions, no global state.
        let cell: RwLock<Option<CensusSnapshot>> = RwLock::new(None);

        // Two chunks of an in-flight walk accumulate…
        merge_chunk_into_cell(
            &cell,
            &[row("qontinui-runner", "D:/x/a", 1)],
            Uuid::nil(),
            None,
        );
        merge_chunk_into_cell(
            &cell,
            &[
                row("qontinui-runner", "D:/x/a", 2), // fresher re-observation
                row("qontinui-runner", "D:/x/b", 3),
            ],
            Uuid::nil(),
            None,
        );
        {
            let g = cell.read().unwrap();
            let snap = g.as_ref().expect("merges created a snapshot");
            assert_eq!(snap.req.worktrees.len(), 2);
            assert_eq!(snap.req.worktrees[0].attributable_bytes, 2, "upserted");
        }

        // Empty chunk (a final chunk can have no remainder) is a no-op.
        merge_chunk_into_cell(&cell, &[], Uuid::nil(), None);
        assert_eq!(
            cell.read().unwrap().as_ref().unwrap().req.worktrees.len(),
            2
        );

        // …then the COMPLETED walk replaces the snapshot wholesale: path b
        // vanished from disk mid-walk and is dropped by the replace (chunk
        // merges alone could never remove it).
        publish_census_to_cell(
            &cell,
            WorktreeCensusReq {
                device_id: Uuid::nil(),
                tenant_id: None,
                volumes: Vec::new(),
                worktrees: vec![row("qontinui-runner", "D:/x/a", 5)],
            },
            777,
        );
        let g = cell.read().unwrap();
        let snap = g.as_ref().unwrap();
        assert_eq!(snap.req.worktrees.len(), 1, "vanished path b dropped");
        assert_eq!(snap.req.worktrees[0].path, "D:/x/a");
        assert_eq!(snap.build_ms, 777, "replace records the walk duration");
    }

    #[test]
    fn attributable_bytes_sums_nm_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("x"), vec![0u8; 10]).unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("y"), vec![0u8; 20]).unwrap();
        let row = capture_worktree("qontinui-coord", dir.path());
        assert_eq!(row.nm_bytes, 10);
        assert_eq!(row.target_bytes, 20);
        assert_eq!(row.attributable_bytes, 30);
    }
}
