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
        match coord_http_base() {
            None => {
                debug!(
                    "worktree_census: no coord_url configured — census chunks will not be POSTed"
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
        let Some((client, url)) = &self.dest else {
            return;
        };
        let body = WorktreeCensusReq {
            device_id: self.device_id,
            tenant_id: self.tenant_id,
            volumes,
            worktrees: chunk.rows,
        };
        match client.post(url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.posted += 1;
                // R3 boot ordering: the first successful chunk POST of this
                // boot releases the reclaim poller's census-before-reclaim
                // gate — coord holds rows from that moment on.
                mark_first_census_posted();
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

/// Free-space report for one volume (drive letter on Windows).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VolumeReport {
    /// Drive letter with trailing colon, e.g. `"D:"`.
    pub volume: String,
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

/// Resolve the coord HTTP base. `COORD_HTTP_URL` env → active profile
/// `coord_url` (ws→http, `/ws` suffix stripped). Returns `None` (the
/// tick cleanly skips) when nothing is configured, matching
/// `fleet::coord_http_base`.
fn coord_http_base() -> Option<String> {
    // Delegates to the shared resolver, preserving `None` (the tick cleanly
    // skips) when nothing is configured. The shared resolver already trims the
    // trailing slash on both the env and ws→http paths; the extra trim here is
    // a belt-and-suspenders no-op kept for byte-identical behavior.
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => {
            Some(base.trim_end_matches('/').to_string())
        }
        _ => None,
    }
}

/// Crate-visible alias of [`coord_http_base`] so the Phase 4 reclaim
/// poller reuses the identical coord-base resolution (env → profile,
/// ws→http, `/ws`-suffix strip).
pub(crate) fn coord_http_base_pub() -> Option<String> {
    coord_http_base()
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
// Volume free space (sysinfo — already a dependency, has the `disk` feature).
// ---------------------------------------------------------------------------

/// Build {volume, total_bytes, free_bytes} for each distinct drive
/// letter among the worktree paths. Uses `sysinfo::Disks` (already a
/// runner dependency with the `disk` feature) rather than a raw
/// `GetDiskFreeSpaceExW` binding — it gives total + available per mount
/// portably. We map each worktree's drive letter to the sysinfo disk
/// whose mount point covers it.
fn collect_volumes(worktree_paths: &[PathBuf]) -> Vec<VolumeReport> {
    use sysinfo::Disks;

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

    let disks = Disks::new_with_refreshed_list();
    let mut out: BTreeMap<String, VolumeReport> = BTreeMap::new();
    for d in disks.list() {
        let mount = d.mount_point();
        if let Some(vol) = drive_letter_of(mount) {
            if wanted.contains(&vol) && !out.contains_key(&vol) {
                out.insert(
                    vol.clone(),
                    VolumeReport {
                        volume: vol,
                        total_bytes: d.total_space(),
                        free_bytes: d.available_space(),
                    },
                );
            }
        }
    }
    out.into_values().collect()
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
    // completed walk regardless of coord reachability. Read-only.
    warn_on_low_disk(&volumes);
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
fn warn_on_low_disk(volumes: &[VolumeReport]) {
    let warn_floor = env_bytes(LOW_DISK_WARN_BYTES_ENV, DEFAULT_LOW_DISK_WARN_BYTES);
    let crit_floor = env_bytes(LOW_DISK_CRIT_BYTES_ENV, DEFAULT_LOW_DISK_CRIT_BYTES);
    const GB: f64 = 1_073_741_824.0;
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
        } else if v.free_bytes < warn_floor {
            warn!(
                "worktree_census: low disk on {} — {:.1} GB free ({:.1}%).",
                v.volume, free_gb, pct
            );
        }
    }
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

    #[test]
    fn collect_volumes_empty_when_no_drive_letters() {
        // POSIX-style paths have no drive letter → no volume rows (so CI
        // on linux gets a deterministic empty result).
        let paths = vec![PathBuf::from("/tmp/x"), PathBuf::from("/home/y")];
        assert!(collect_volumes(&paths).is_empty());
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
