//! `MemoryBridge` — runner-side reference implementation of
//! [`RunnerObservableBridge`] for the `coord.memories` category.
//!
//! Lifecycle (plan `2026-05-22-memories-on-coord-cross-machine.md`):
//!
//! 1. **`pull`** (spawn-time): list coord's tenant pool, materialize
//!    differences into `session_ctx.memory_dir`, push back any local
//!    files coord doesn't know about (first-contact import), delete
//!    tombstoned files, snapshot the resulting state into
//!    `session_ctx.post_pull_snapshot`.
//! 2. **`start_watching`** (during-session, trait method): spawn a
//!    `notify` file watcher on `memory_dir`, debounce per-file events
//!    250ms, route to the inherent `push`.
//! 3. **`push`** (per-event, inherent — NOT on the trait): upsert or
//!    delete to coord. Best-effort — failures are logged, never
//!    propagated. Called only by this bridge's own watch loop.
//! 4. **`reconcile`** (session-end): stop watcher, re-snapshot, diff
//!    against `post_pull_snapshot`, push any deltas the watcher missed,
//!    return aggregate `ReconcileReport`.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use qontinui_types::memory::{MemoryUpsertRequest, MemoryWithHistory};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::memory_client;
use super::{
    DirSnapshot, FileFingerprint, ReconcileReport, RunnerObservableBridge, SessionContext,
};

const DEBOUNCE_MS: u64 = 250;
const MD_EXT: &str = "md";

/// Local-change event surfaced by the memory file watcher to
/// [`MemoryBridge::push`]. Bridge-internal — never crosses the
/// [`RunnerObservableBridge`] trait, because other categories
/// (e.g. git-ops) have a structurally different change shape.
#[derive(Debug, Clone)]
enum ObservableChange {
    Upserted { name: String, content: String },
    Deleted { name: String },
}

/// Reusable bridge for the lifetime of one runner process. `pull`,
/// `push`, `reconcile`, and `start_watching` can all be called multiple
/// times for *concurrent* sessions — watchers are keyed by
/// `session_id`, so a hotmail session and a gmail session can run
/// against their own memory dirs simultaneously without trampling each
/// other's `notify` hooks.
pub struct MemoryBridge {
    http: reqwest::Client,
    /// Async-aware mutex so locks can be held across `.await` points
    /// (e.g., when `start_watching` registers a new handle).
    watchers: tokio::sync::Mutex<HashMap<Uuid, WatcherHandle>>,
}

struct WatcherHandle {
    /// Keep alive — dropping the watcher disconnects the OS-level
    /// notify hook. Underscore-prefixed to document the "hold for side
    /// effect" intent.
    _notify: RecommendedWatcher,
    /// Cancellation token wired into the debounce/dispatch task so a
    /// session-end shutdown unblocks the loop cleanly.
    cancel: CancellationToken,
}

impl MemoryBridge {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: memory_client::build_client()?,
            watchers: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Cancel every active watcher. Called at runner shutdown to
    /// guarantee no `notify` hooks linger past process exit.
    pub async fn shutdown_all(&self) {
        let mut guard = self.watchers.lock().await;
        for (_, handle) in guard.drain() {
            handle.cancel.cancel();
        }
    }

    /// Read the device-token JWT used as `Authorization: Bearer` — the
    /// OWNING SESSION's tenant slot (Phase 8b per-session credential
    /// selection, plan 2026-07-02-session-scoped-multi-tenant-device-binding
    /// §D4): memories are hard tenant-scoped, so a session federating
    /// tenant B's pool must present B's device JWT, not the machine
    /// default's. `ctx.tenant_id` is the session's binding (resolved by
    /// `federation::build_federation_ctx`: spawn input, else the device
    /// default) — when it IS the default, `device_bearer_for` falls back to
    /// the legacy slot, preserving today's behavior byte-for-byte. Returns
    /// `None` if no credential exists for that tenant (slot-miss fail-soft:
    /// never another tenant's token) — every call site short-circuits in
    /// that case (best-effort posture).
    fn device_token(&self, ctx: &SessionContext) -> Option<String> {
        crate::auth::device_bearer_for(Some(&ctx.tenant_id))
    }

    /// Push one local change to coord. Inherent (NOT a trait method):
    /// only this bridge's own watch loop calls it, never the dispatch
    /// site through the trait object. Best-effort — failures are logged,
    /// never propagated.
    async fn push(&self, change: ObservableChange, session_ctx: &SessionContext) -> Result<()> {
        let token = match self.device_token(session_ctx) {
            Some(t) if !t.is_empty() => t,
            _ => {
                debug!("observable_bridge::memory::push: no device token; dropping change");
                return Ok(());
            }
        };
        let base = match memory_client::coord_http_base() {
            Some(b) => b,
            None => {
                debug!("observable_bridge::memory::push: no coord_url; dropping change");
                return Ok(());
            }
        };

        match change {
            ObservableChange::Upserted { name, content } => {
                let req = MemoryUpsertRequest {
                    name: name.clone(),
                    content,
                    description: None,
                    r#type: None,
                    written_by_agent: Some(session_ctx.session_id.to_string()),
                    written_by_device: Some(session_ctx.device_id.to_string()),
                };
                if let Err(e) = memory_client::upsert(&self.http, &base, &token, &req).await {
                    warn!("observable_bridge::memory::push upsert {name} failed: {e}");
                }
            }
            ObservableChange::Deleted { name } => {
                if let Err(e) = memory_client::delete(&self.http, &base, &token, &name).await {
                    warn!("observable_bridge::memory::push delete {name} failed: {e}");
                }
            }
        }
        Ok(())
    }
}

/// Walk `dir` for `*.md` files, returning a deterministic snapshot.
/// `name` is the basename without `.md`. Sort by name so two snapshots
/// of the same directory are byte-equal regardless of OS readdir order.
fn snapshot_dir(dir: &Path) -> Result<DirSnapshot> {
    if !dir.exists() {
        return Ok(DirSnapshot::default());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("observable_bridge::memory: read_dir entry error: {e}");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some(MD_EXT) {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let content = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "observable_bridge::memory: read {} failed: {e}",
                    path.display()
                );
                continue;
            }
        };
        files.push(FileFingerprint {
            name,
            sha256: hex_sha256(&content),
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(DirSnapshot { files })
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn memory_file_path(memory_dir: &Path, name: &str) -> PathBuf {
    memory_dir.join(format!("{name}.{MD_EXT}"))
}

/// Background task that drains notify events, debounces by file path,
/// and forwards to `MemoryBridge::push`.
async fn run_watch_loop(
    bridge: Arc<MemoryBridge>,
    session_ctx: SessionContext,
    mut rx: mpsc::Receiver<notify::Result<Event>>,
    cancel: CancellationToken,
) {
    use std::collections::HashMap;
    use std::time::Instant;
    use tokio::time::{sleep_until, Instant as TokioInstant};

    // Per-path debounce: each event extends the "fire after" timestamp.
    // The task wakes either on the next event OR on the earliest
    // pending deadline.
    let mut pending: HashMap<PathBuf, (Instant, bool)> = HashMap::new();

    loop {
        // Compute the next deadline to wake up at, if any.
        let next_deadline = pending
            .values()
            .map(|(t, _)| *t)
            .min()
            .map(TokioInstant::from_std);

        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("observable_bridge::memory: watch loop cancelled");
                return;
            }
            maybe_event = rx.recv() => {
                let event = match maybe_event {
                    Some(Ok(ev)) => ev,
                    Some(Err(e)) => {
                        warn!("observable_bridge::memory: notify error: {e}");
                        continue;
                    }
                    None => {
                        debug!("observable_bridge::memory: notify channel closed");
                        return;
                    }
                };
                let deleted = matches!(event.kind, EventKind::Remove(_));
                let interesting = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if !interesting {
                    continue;
                }
                let deadline = Instant::now() + std::time::Duration::from_millis(DEBOUNCE_MS);
                for path in event.paths.into_iter() {
                    if path.extension().and_then(|s| s.to_str()) != Some(MD_EXT) {
                        continue;
                    }
                    // Each event overwrites the (deadline, was_deleted)
                    // tuple. Latest signal wins; if a Modify follows a
                    // Remove, the Modify takes over.
                    pending.insert(path, (deadline, deleted));
                }
            }
            _ = async {
                match next_deadline {
                    Some(d) => sleep_until(d).await,
                    None => futures::future::pending::<()>().await,
                }
            } => {
                // Fire any entries whose deadline has passed.
                let now = Instant::now();
                let mut ready: Vec<(PathBuf, bool)> = Vec::new();
                pending.retain(|path, (t, deleted)| {
                    if *t <= now {
                        ready.push((path.clone(), *deleted));
                        false
                    } else {
                        true
                    }
                });
                for (path, was_deleted) in ready {
                    dispatch_change(&bridge, &session_ctx, &path, was_deleted).await;
                }
            }
        }
    }
}

async fn dispatch_change(
    bridge: &MemoryBridge,
    session_ctx: &SessionContext,
    path: &Path,
    was_deleted: bool,
) {
    let name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return,
    };

    // Real ground truth at debounce-fire: does the file exist now?
    // notify can deliver a Remove followed by a Create within the
    // debounce window; trust the filesystem over the event kind.
    let change = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(content) => ObservableChange::Upserted { name, content },
            Err(e) => {
                warn!(
                    "observable_bridge::memory: read {} failed in watcher: {e}",
                    path.display()
                );
                return;
            }
        }
    } else if was_deleted {
        ObservableChange::Deleted { name }
    } else {
        // Race: event fired but file already gone and we never saw a
        // Remove. Best-effort delete.
        ObservableChange::Deleted { name }
    };

    if let Err(e) = bridge.push(change, session_ctx).await {
        warn!("observable_bridge::memory: push from watcher failed: {e}");
    }
}

#[async_trait]
impl RunnerObservableBridge for MemoryBridge {
    fn category(&self) -> &'static str {
        "memory"
    }

    /// Spawn-time materialization. See module docstring for the full
    /// algorithm. Coord-side failures are logged + best-effort (the
    /// watcher + reconcile recover later); `memory_dir` IO failures
    /// propagate as fatal because they indicate misconfiguration.
    async fn pull(&self, session_ctx: &mut SessionContext) -> Result<()> {
        if !session_ctx.memory_dir.exists() {
            std::fs::create_dir_all(&session_ctx.memory_dir).with_context(|| {
                format!("create memory_dir: {}", session_ctx.memory_dir.display())
            })?;
        }

        let local_pre = snapshot_dir(&session_ctx.memory_dir)?;

        let token = match self.device_token(session_ctx) {
            Some(t) if !t.is_empty() => t,
            _ => {
                warn!(
                    "observable_bridge::memory::pull: no device token; skipping coord sync \
                     (memory_dir left untouched, snapshot recorded as post-pull baseline)"
                );
                session_ctx.post_pull_snapshot = Some(local_pre);
                return Ok(());
            }
        };
        let base = match memory_client::coord_http_base() {
            Some(b) => b,
            None => {
                warn!(
                    "observable_bridge::memory::pull: no coord_url in active profile; \
                     skipping coord sync"
                );
                session_ctx.post_pull_snapshot = Some(local_pre);
                return Ok(());
            }
        };

        // Step 1: list coord's tenant pool.
        let listing = match memory_client::list(&self.http, &base, &token).await {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    "observable_bridge::memory::pull: list failed ({e}); \
                     leaving memory_dir untouched, snapshot = local"
                );
                session_ctx.post_pull_snapshot = Some(local_pre);
                return Ok(());
            }
        };

        // Step 2: for each coord entry, GET full payload (the list
        // response omits content + hash to keep the metadata call
        // cheap; comparing requires the full body). Apply tombstones
        // and content updates. Tradeoff documented in plan §Phase 2:
        // GET-per-name is O(N) but typical pools are < 200; if it
        // becomes a bottleneck, gate on `version` deltas tracked
        // across sessions.
        let mut coord_names: HashSet<String> = HashSet::new();
        for summary in &listing.items {
            coord_names.insert(summary.name.clone());
            let full = match memory_client::get(&self.http, &base, &token, &summary.name).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(
                        "observable_bridge::memory::pull: get {} failed: {e}",
                        summary.name
                    );
                    continue;
                }
            };
            if let Err(e) = apply_coord_row(&session_ctx.memory_dir, &full, &local_pre) {
                warn!(
                    "observable_bridge::memory::pull: apply {} failed: {e}",
                    summary.name
                );
            }
        }

        // Step 3: first-contact import — any local file whose name is
        // not in coord's list gets POSTed. Per plan D3 §4 this is how
        // pre-runner content migrates in.
        for fp in &local_pre.files {
            if coord_names.contains(&fp.name) {
                continue;
            }
            let path = memory_file_path(&session_ctx.memory_dir, &fp.name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "observable_bridge::memory::pull: read {} failed: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let req = MemoryUpsertRequest {
                name: fp.name.clone(),
                content,
                description: None,
                r#type: None,
                written_by_agent: Some(session_ctx.session_id.to_string()),
                written_by_device: Some(session_ctx.device_id.to_string()),
            };
            if let Err(e) = memory_client::upsert(&self.http, &base, &token, &req).await {
                warn!(
                    "observable_bridge::memory::pull: first-contact upsert {} failed: {e}",
                    fp.name
                );
            } else {
                info!(
                    "observable_bridge::memory::pull: first-contact imported {}",
                    fp.name
                );
            }
        }

        // Step 4: re-snapshot — the post-pull baseline is what
        // `reconcile` diffs against at session end.
        let post_pull = snapshot_dir(&session_ctx.memory_dir)?;

        // Compute pulled count: files that are new or changed compared
        // to the pre-pull snapshot (i.e., files that coord wrote locally).
        let mut pulled_count: u32 = 0;
        for fp in &post_pull.files {
            match local_pre.get(&fp.name) {
                None => pulled_count = pulled_count.saturating_add(1), // new file
                Some(prev) if prev.sha256 != fp.sha256 => {
                    pulled_count = pulled_count.saturating_add(1); // changed file
                }
                _ => {} // unchanged
            }
        }
        // Files deleted by tombstone also count as pulled changes.
        for fp in &local_pre.files {
            if post_pull.get(&fp.name).is_none() {
                pulled_count = pulled_count.saturating_add(1);
            }
        }
        session_ctx.pulled_count = pulled_count;
        session_ctx.post_pull_snapshot = Some(post_pull);
        Ok(())
    }

    /// Start the per-session file watcher. Must be called AFTER `pull`
    /// (so the post-pull snapshot is the watcher's starting baseline).
    /// Idempotent per `session_id`: re-calling with the same id cleanly
    /// cancels the prior watcher before installing the new one.
    ///
    /// Takes `self: Arc<Self>` so it can `Arc::clone` itself into the
    /// detached `notify` watch-loop task, and clones the borrowed `ctx`
    /// internally before moving it into that task.
    async fn start_watching(self: Arc<Self>, ctx: &SessionContext) -> Result<()> {
        let session_ctx = ctx.clone();
        let session_id = session_ctx.session_id;
        let mut guard = self.watchers.lock().await;
        if let Some(prev) = guard.remove(&session_id) {
            prev.cancel.cancel();
        }

        if !session_ctx.memory_dir.exists() {
            std::fs::create_dir_all(&session_ctx.memory_dir).with_context(|| {
                format!(
                    "create memory_dir for watcher: {}",
                    session_ctx.memory_dir.display()
                )
            })?;
        }

        let (tx, rx) = mpsc::channel::<notify::Result<Event>>(256);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            // The notify callback is a sync std-thread closure; use the
            // blocking_send variant to push into the tokio channel
            // without an executor handle.
            let _ = tx.blocking_send(res);
        })
        .context("observable_bridge::memory: build notify watcher")?;
        watcher
            .watch(&session_ctx.memory_dir, RecursiveMode::NonRecursive)
            .with_context(|| {
                format!(
                    "notify::watch on memory_dir: {}",
                    session_ctx.memory_dir.display()
                )
            })?;

        let cancel = CancellationToken::new();
        let bridge = Arc::clone(&self);
        let task_cancel = cancel.clone();
        tokio::spawn(async move {
            run_watch_loop(bridge, session_ctx, rx, task_cancel).await;
        });

        guard.insert(
            session_id,
            WatcherHandle {
                _notify: watcher,
                cancel,
            },
        );
        Ok(())
    }

    /// Stop the watcher for `session_id` if one is running. Called by
    /// `reconcile` (and the dispatch site) so the final snapshot reflects
    /// a quiesced directory. No-op when the id isn't registered.
    async fn stop_watching(&self, session_id: Uuid) {
        let mut guard = self.watchers.lock().await;
        if let Some(prev) = guard.remove(&session_id) {
            prev.cancel.cancel();
        }
    }

    async fn reconcile(&self, session_ctx: &SessionContext) -> Result<ReconcileReport> {
        self.stop_watching(session_ctx.session_id).await;

        let post_pull = match &session_ctx.post_pull_snapshot {
            Some(s) => s.clone(),
            None => {
                return Err(anyhow!(
                    "observable_bridge::memory::reconcile: no post_pull_snapshot \
                     in session_ctx — was pull() called?"
                ));
            }
        };
        let post_session = snapshot_dir(&session_ctx.memory_dir)?;

        let token = match self.device_token(session_ctx) {
            Some(t) if !t.is_empty() => t,
            _ => {
                warn!(
                    "observable_bridge::memory::reconcile: no device token; \
                     reporting zero counts"
                );
                return Ok(ReconcileReport::default());
            }
        };
        let base = match memory_client::coord_http_base() {
            Some(b) => b,
            None => {
                warn!(
                    "observable_bridge::memory::reconcile: no coord_url; \
                     reporting zero counts"
                );
                return Ok(ReconcileReport::default());
            }
        };

        let mut report = ReconcileReport {
            pulled: session_ctx.pulled_count,
            ..Default::default()
        };
        let pre_names: HashSet<&str> = post_pull.files.iter().map(|f| f.name.as_str()).collect();
        let post_names: HashSet<&str> =
            post_session.files.iter().map(|f| f.name.as_str()).collect();

        // Upserts: name present now AND (absent before OR hash changed).
        for fp in &post_session.files {
            let prev_hash = post_pull.get(&fp.name).map(|p| p.sha256.as_str());
            if prev_hash == Some(fp.sha256.as_str()) {
                report.unchanged = report.unchanged.saturating_add(1);
                continue;
            }
            let path = memory_file_path(&session_ctx.memory_dir, &fp.name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "observable_bridge::memory::reconcile: read {} failed: {e}",
                        path.display()
                    );
                    report.failed = report.failed.saturating_add(1);
                    report.failed_names.push(fp.name.clone());
                    continue;
                }
            };
            let req = MemoryUpsertRequest {
                name: fp.name.clone(),
                content,
                description: None,
                r#type: None,
                written_by_agent: Some(session_ctx.session_id.to_string()),
                written_by_device: Some(session_ctx.device_id.to_string()),
            };
            match memory_client::upsert(&self.http, &base, &token, &req).await {
                Ok(_) => report.pushed = report.pushed.saturating_add(1),
                Err(e) => {
                    warn!(
                        "observable_bridge::memory::reconcile: upsert {} failed: {e}",
                        fp.name
                    );
                    report.failed = report.failed.saturating_add(1);
                    report.failed_names.push(fp.name.clone());
                }
            }
        }

        // Deletes: name in pre_pull but gone now.
        for name in pre_names.difference(&post_names) {
            match memory_client::delete(&self.http, &base, &token, name).await {
                Ok(_) => report.pushed = report.pushed.saturating_add(1),
                Err(e) => {
                    warn!(
                        "observable_bridge::memory::reconcile: delete {} failed: {e}",
                        name
                    );
                    report.failed = report.failed.saturating_add(1);
                    report.failed_names.push(name.to_string());
                }
            }
        }

        info!(
            "observable_bridge::memory::reconcile: pushed={} pulled={} unchanged={} failed={}",
            report.pushed, report.pulled, report.unchanged, report.failed
        );
        Ok(report)
    }
}

/// Materialize one coord row into the local memory dir. Skips no-op
/// writes (hash matches), deletes locally on tombstone.
fn apply_coord_row(
    memory_dir: &Path,
    payload: &MemoryWithHistory,
    local: &DirSnapshot,
) -> Result<()> {
    let name = &payload.latest.name;
    let path = memory_file_path(memory_dir, name);

    if payload.latest.is_tombstone {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove tombstoned {}", path.display()))?;
            info!("observable_bridge::memory: removed tombstoned {name}");
        }
        return Ok(());
    }

    let coord_hash = hex_sha256(payload.latest.content.as_bytes());
    if let Some(local_fp) = local.get(name) {
        if local_fp.sha256 == coord_hash {
            return Ok(());
        }
    }
    std::fs::write(&path, payload.latest.content.as_bytes())
        .with_context(|| format!("write memory file {}", path.display()))?;
    debug!("observable_bridge::memory: materialized {name}");
    Ok(())
}
